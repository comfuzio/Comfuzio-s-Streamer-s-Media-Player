//! YouTube Live Chat connector (YouTube Data API v3).
//!
//! - Auth: Google OAuth for installed apps (loopback redirect + PKCE), scope
//!   `youtube.force-ssl`, offline access for refresh tokens.
//! - Inbound: `liveBroadcasts.list` finds the active broadcast, then
//!   `liveChatMessages.list` is paged at the server-requested interval
//!   (never faster than `minPollSecs`).
//! - Outbound: `liveChatMessages.insert`, opt-in because each costs 20 units.
//! - Every call is charged against a daily quota budget (resets at midnight
//!   Pacific) so a long stream can't silently exhaust the project's quota.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};
use url::Url;

use super::{ChatMessage, ChatPlatform, ChatRoles, ChatStatus, ConnectionState, Inbound, Outbound};
use crate::error::{AppError, AppResult};
use crate::events::{AppEvent, EventBus, NoticeLevel};
use crate::model::Requester;
use crate::persistence::settings::SettingsStore;
use crate::persistence::Database;
use crate::util::now_ms;
use crate::util::oauth::{accept_callback, excerpt, pkce_challenge, random_urlsafe, StoredTokens};

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";
const API: &str = "https://www.googleapis.com/youtube/v3";
const SCOPE: &str = "https://www.googleapis.com/auth/youtube.force-ssl";
const TOKENS_KEY: &str = "youtube.tokens";
const IDENTITY_KEY: &str = "youtube.identity";
const QUOTA_KEY: &str = "youtube.quota";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const PLATFORM: ChatPlatform = ChatPlatform::YouTube;

/// Documented quota costs (units).
const COST_LIST: u32 = 1;
const COST_CHAT_LIST: u32 = 5;
const COST_CHAT_INSERT: u32 = 20;
const COST_PLAYLIST_INSERT: u32 = 50;
const COST_PLAYLIST_ITEM: u32 = 50;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct YouTubeIdentity {
    pub channel_id: String,
    pub title: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct QuotaUsage {
    /// Pacific "quota day" number.
    pub day: i64,
    pub used: u32,
}

/// Google resets quotas at midnight Pacific Time. We use UTC-8 year-round, so
/// during daylight saving the local day rolls over one hour *later* than
/// Google's, which can only under-use the budget, never overspend it.
fn quota_day(now_ms: i64) -> i64 {
    (now_ms - 8 * 3_600_000).div_euclid(86_400_000)
}

fn ms_until_next_quota_day(now_ms: i64) -> u64 {
    let next = (quota_day(now_ms) + 1) * 86_400_000 + 8 * 3_600_000;
    (next - now_ms).max(0) as u64
}

impl QuotaUsage {
    /// Charges `units` if the budget allows it.
    fn try_spend(&mut self, units: u32, budget: u32, now_ms: i64) -> bool {
        let today = quota_day(now_ms);
        if self.day != today {
            *self = Self { day: today, used: 0 };
        }
        if self.used + units > budget {
            return false;
        }
        self.used += units;
        true
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: i64,
    #[serde(default)]
    scope: Option<String>,
}

enum ApiError {
    /// The chat ended or disappeared: the stream is over.
    ChatGone,
    QuotaExceeded,
    Other(AppError),
}

impl From<AppError> for ApiError {
    fn from(e: AppError) -> Self {
        Self::Other(e)
    }
}

impl From<reqwest::Error> for ApiError {
    fn from(e: reqwest::Error) -> Self {
        Self::Other(e.into())
    }
}

pub struct YouTubeClient {
    http: reqwest::Client,
    db: Database,
    settings: Arc<SettingsStore>,
    bus: EventBus,
    tokens: Mutex<Option<StoredTokens>>,
    identity: Mutex<Option<YouTubeIdentity>>,
    quota: Mutex<QuotaUsage>,
    login_lock: Mutex<()>,
}

impl YouTubeClient {
    pub async fn new(http: reqwest::Client, db: Database, settings: Arc<SettingsStore>, bus: EventBus) -> AppResult<Self> {
        let tokens = db.kv_get::<StoredTokens>(TOKENS_KEY).await?;
        let identity = db.kv_get::<YouTubeIdentity>(IDENTITY_KEY).await?;
        let quota = db.kv_get::<QuotaUsage>(QUOTA_KEY).await?.unwrap_or_default();
        Ok(Self {
            http,
            db,
            settings,
            bus,
            tokens: Mutex::new(tokens),
            identity: Mutex::new(identity),
            quota: Mutex::new(quota),
            login_lock: Mutex::new(()),
        })
    }

    fn credentials(&self) -> AppResult<(String, String)> {
        let s = self.settings.get().youtube_chat;
        match (s.client_id, s.client_secret) {
            (Some(id), Some(secret)) => Ok((id, secret)),
            _ => Err(AppError::Invalid("set your Google OAuth client ID and secret in settings first".into())),
        }
    }

    pub async fn quota_usage(&self) -> QuotaUsage {
        let mut q = *self.quota.lock().await;
        if q.day != quota_day(now_ms()) {
            q = QuotaUsage { day: quota_day(now_ms()), used: 0 };
        }
        q
    }

    async fn spend(&self, units: u32) -> bool {
        let budget = self.settings.get().youtube_chat.daily_quota;
        let mut q = self.quota.lock().await;
        let ok = q.try_spend(units, budget, now_ms());
        let snapshot = *q;
        drop(q);
        if ok {
            if let Err(err) = self.db.kv_set(QUOTA_KEY, &snapshot).await {
                tracing::warn!(%err, "failed to persist YouTube quota usage");
            }
        }
        ok
    }

    // ------------------------------------------------------------------ auth

    /// Full loopback + PKCE login in the system browser.
    pub async fn login(&self) -> AppResult<YouTubeIdentity> {
        let _guard = self.login_lock.try_lock().map_err(|_| AppError::Invalid("a YouTube login is already in progress".into()))?;
        let (client_id, client_secret) = self.credentials()?;
        // Google accepts any loopback port for desktop clients (RFC 8252).
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let redirect_uri = format!("http://127.0.0.1:{}/callback", listener.local_addr()?.port());
        let verifier = random_urlsafe(64)?;
        let state = random_urlsafe(24)?;

        let mut auth = Url::parse(AUTH_URL).expect("static URL");
        auth.query_pairs_mut()
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", SCOPE)
            .append_pair("code_challenge", &pkce_challenge(&verifier))
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent");
        tauri_plugin_opener::open_url(auth.as_str(), None::<&str>)
            .map_err(|e| AppError::Invalid(format!("could not open the browser: {e}")))?;

        let code = tokio::time::timeout(LOGIN_TIMEOUT, accept_callback(&listener, &state, "YouTube"))
            .await
            .map_err(|_| AppError::Invalid("YouTube login timed out".into()))??;
        drop(listener);

        let res = self
            .http
            .post(TOKEN_URL)
            .form(&[
                ("code", code.as_str()),
                ("client_id", client_id.as_str()),
                ("client_secret", client_secret.as_str()),
                ("redirect_uri", redirect_uri.as_str()),
                ("grant_type", "authorization_code"),
                ("code_verifier", verifier.as_str()),
            ])
            .send()
            .await?;
        if !res.status().is_success() {
            let status = res.status();
            return Err(AppError::Invalid(format!("Google token exchange failed ({status}): {}", excerpt(&res.text().await.unwrap_or_default()))));
        }
        let t: TokenResponse = res.json().await?;
        let refresh = t.refresh_token.ok_or_else(|| AppError::Invalid("Google returned no refresh token".into()))?;
        let tokens = StoredTokens::new(t.access_token, refresh, t.expires_in, t.scope.unwrap_or_default());
        self.db.kv_set(TOKENS_KEY, &tokens).await?;
        *self.tokens.lock().await = Some(tokens);

        let identity = self.fetch_identity().await.map_err(|e| match e {
            ApiError::Other(e) => e,
            ApiError::QuotaExceeded => AppError::Invalid("YouTube API quota exhausted for today".into()),
            ApiError::ChatGone => AppError::Internal("unexpected chat error".into()),
        })?;
        self.db.kv_set(IDENTITY_KEY, &identity).await?;
        *self.identity.lock().await = Some(identity.clone());
        self.bus.notice(NoticeLevel::Info, format!("YouTube connected as {}", identity.title));
        self.bus.emit(AppEvent::ChatAuth { platform: PLATFORM, connected: true });
        Ok(identity)
    }

    pub async fn logout(&self) -> AppResult<()> {
        if let Some(tokens) = self.tokens.lock().await.take() {
            let _ = self.http.post(REVOKE_URL).form(&[("token", tokens.refresh_token.as_str())]).send().await;
        }
        *self.identity.lock().await = None;
        self.db.kv_delete(TOKENS_KEY).await?;
        self.db.kv_delete(IDENTITY_KEY).await?;
        self.bus.emit(AppEvent::ChatAuth { platform: PLATFORM, connected: false });
        Ok(())
    }

    async fn access_token(&self, force_refresh: bool) -> AppResult<String> {
        let mut guard = self.tokens.lock().await;
        let tokens = guard.as_ref().ok_or_else(|| AppError::Invalid("sign in to YouTube first".into()))?;
        if !force_refresh && !tokens.expires_within(2 * 60 * 1000) {
            return Ok(tokens.access_token.clone());
        }
        let (client_id, client_secret) = self.credentials()?;
        let res = self
            .http
            .post(TOKEN_URL)
            .form(&[
                ("client_id", client_id.as_str()),
                ("client_secret", client_secret.as_str()),
                ("refresh_token", tokens.refresh_token.as_str()),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .await?;
        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            if body.contains("invalid_grant") {
                *guard = None;
                drop(guard);
                self.db.kv_delete(TOKENS_KEY).await?;
                self.bus.emit(AppEvent::ChatAuth { platform: PLATFORM, connected: false });
                return Err(AppError::Invalid("YouTube session expired, sign in again".into()));
            }
            return Err(AppError::Invalid(format!("Google token refresh failed ({status}): {}", excerpt(&body))));
        }
        let t: TokenResponse = res.json().await?;
        let fresh = StoredTokens::new(
            t.access_token,
            t.refresh_token.unwrap_or_else(|| tokens.refresh_token.clone()),
            t.expires_in,
            t.scope.unwrap_or_else(|| tokens.scope.clone()),
        );
        self.db.kv_set(TOKENS_KEY, &fresh).await?;
        let access = fresh.access_token.clone();
        *guard = Some(fresh);
        Ok(access)
    }

    /// Authorized API call charged `cost` units, with one refresh on 401.
    async fn api(&self, method: reqwest::Method, path: &str, query: &[(&str, &str)], body: Option<&Value>, cost: u32) -> Result<Value, ApiError> {
        if !self.spend(cost).await {
            return Err(ApiError::QuotaExceeded);
        }
        for attempt in 0..2 {
            let token = self.access_token(attempt == 1).await?;
            let mut req = self.http.request(method.clone(), format!("{API}{path}")).query(query).bearer_auth(token);
            if let Some(b) = body {
                req = req.json(b);
            }
            let res = req.send().await?;
            let status = res.status();
            if status == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
                continue;
            }
            let v: Value = res.json().await.unwrap_or(Value::Null);
            if status.is_success() {
                return Ok(v);
            }
            let reason = v["error"]["errors"][0]["reason"].as_str().unwrap_or_default();
            return Err(match reason {
                "liveChatEnded" | "liveChatNotFound" | "liveChatDisabled" => ApiError::ChatGone,
                "quotaExceeded" | "dailyLimitExceeded" => ApiError::QuotaExceeded,
                _ => ApiError::Other(AppError::Invalid(format!(
                    "YouTube API {path} failed ({status}): {}",
                    v["error"]["message"].as_str().unwrap_or(reason)
                ))),
            });
        }
        unreachable!("loop returns on the second attempt")
    }

    async fn fetch_identity(&self) -> Result<YouTubeIdentity, ApiError> {
        let v = self.api(reqwest::Method::GET, "/channels", &[("part", "snippet"), ("mine", "true")], None, COST_LIST).await?;
        let ch = &v["items"][0];
        match (ch["id"].as_str(), ch["snippet"]["title"].as_str()) {
            (Some(id), Some(title)) => Ok(YouTubeIdentity { channel_id: id.into(), title: title.into() }),
            _ => Err(ApiError::Other(AppError::NotFound("this Google account has no YouTube channel".into()))),
        }
    }

    /// The active broadcast's (liveChatId, title), if the channel is live.
    async fn active_chat(&self) -> Result<Option<(String, String)>, ApiError> {
        let v = self
            .api(reqwest::Method::GET, "/liveBroadcasts", &[("part", "snippet"), ("broadcastStatus", "active"), ("broadcastType", "all")], None, COST_LIST)
            .await?;
        Ok(v["items"].as_array().into_iter().flatten().find_map(|b| {
            let chat = b["snippet"]["liveChatId"].as_str()?;
            Some((chat.to_owned(), b["snippet"]["title"].as_str().unwrap_or("live").to_owned()))
        }))
    }

    // ------------------------------------------------------------------ playlists

    /// Creates a private YouTube playlist with `video_ids`. Each insert costs
    /// 50 quota units, so the whole export is checked against today's budget first.
    pub async fn export_playlist(&self, title: &str, video_ids: &[String]) -> AppResult<(String, usize)> {
        if self.tokens.lock().await.is_none() {
            return Err(AppError::Invalid("sign in with Google on the Chat bot tab first".into()));
        }
        let needed = COST_PLAYLIST_INSERT + COST_PLAYLIST_ITEM * video_ids.len() as u32;
        let budget = self.settings.get().youtube_chat.daily_quota;
        let used = self.quota_usage().await.used;
        if used + needed > budget {
            return Err(AppError::Invalid(format!(
                "exporting {} songs needs {needed} YouTube API units, but only {} of today's {budget} are left",
                video_ids.len(),
                budget.saturating_sub(used)
            )));
        }
        let to_err = |e: ApiError| match e {
            ApiError::Other(e) => e,
            ApiError::QuotaExceeded => AppError::Invalid("YouTube API quota used up for today".into()),
            ApiError::ChatGone => AppError::Internal("unexpected chat error".into()),
        };
        let body = json!({
            "snippet": { "title": title, "description": "Exported from Comfuzio Streamer's Media Player" },
            "status": { "privacyStatus": "private" },
        });
        let created = self
            .api(reqwest::Method::POST, "/playlists", &[("part", "snippet,status")], Some(&body), COST_PLAYLIST_INSERT)
            .await
            .map_err(to_err)?;
        let playlist_id = created["id"]
            .as_str()
            .ok_or_else(|| AppError::Internal("YouTube returned no playlist id".into()))?
            .to_owned();
        let mut added = 0;
        for video_id in video_ids {
            let item = json!({
                "snippet": {
                    "playlistId": playlist_id,
                    "resourceId": { "kind": "youtube#video", "videoId": video_id },
                }
            });
            match self.api(reqwest::Method::POST, "/playlistItems", &[("part", "snippet")], Some(&item), COST_PLAYLIST_ITEM).await {
                Ok(_) => added += 1,
                Err(ApiError::QuotaExceeded) => break,
                // A single unavailable video shouldn't abort the export.
                Err(err) => tracing::warn!(video_id, error = %to_err(err), "could not add video to YouTube playlist"),
            }
        }
        Ok((format!("https://www.youtube.com/playlist?list={playlist_id}"), added))
    }

    // ------------------------------------------------------------------ connection

    pub async fn run(self: Arc<Self>, inbound: mpsc::Sender<Inbound>, mut outbound: mpsc::Receiver<Outbound>) {
        let status = |state, detail: Option<String>| {
            let mut s = ChatStatus::new(PLATFORM, state);
            s.detail = detail;
            Inbound::Status(s)
        };
        let mut backoff = Duration::from_secs(5);
        loop {
            if self.tokens.lock().await.is_none() {
                let _ = inbound.send(status(ConnectionState::NotConfigured, Some("sign in to YouTube".into()))).await;
                std::future::pending::<()>().await;
            }
            let _ = inbound.send(status(ConnectionState::Connecting, None)).await;
            match self.active_chat().await {
                Ok(Some((chat_id, title))) => {
                    backoff = Duration::from_secs(5);
                    let mut s = ChatStatus::new(PLATFORM, ConnectionState::Connected);
                    s.channel = Some(title);
                    s.account = self.identity.lock().await.as_ref().map(|i| i.title.clone());
                    let _ = inbound.send(Inbound::Status(s)).await;
                    match self.chat_loop(&chat_id, &inbound, &mut outbound).await {
                        Ok(()) | Err(ApiError::ChatGone) => {
                            let _ = inbound.send(Inbound::StreamOffline(PLATFORM)).await;
                        }
                        Err(ApiError::QuotaExceeded) => self.wait_for_quota(&inbound).await,
                        Err(ApiError::Other(err)) => {
                            tracing::warn!(%err, "YouTube chat error");
                            let _ = inbound.send(status(ConnectionState::Error, Some(err.to_string()))).await;
                            tokio::time::sleep(backoff).await;
                            backoff = (backoff * 2).min(Duration::from_secs(120));
                        }
                    }
                }
                Ok(None) => {
                    let _ = inbound.send(status(ConnectionState::WaitingForStream, Some("no active broadcast".into()))).await;
                    let wait = Duration::from_secs(u64::from(self.settings.get().youtube_chat.offline_check_secs));
                    drain_outbound_for(&mut outbound, wait).await;
                }
                Err(ApiError::QuotaExceeded) => self.wait_for_quota(&inbound).await,
                Err(ApiError::ChatGone) => tokio::time::sleep(backoff).await,
                Err(ApiError::Other(err)) => {
                    let _ = inbound.send(status(ConnectionState::Error, Some(err.to_string()))).await;
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(300));
                }
            }
        }
    }

    async fn wait_for_quota(&self, inbound: &mpsc::Sender<Inbound>) {
        let wait = ms_until_next_quota_day(now_ms());
        let msg = format!("daily API quota used up; resuming in ~{} min", wait / 60_000 + 1);
        self.bus.notice(NoticeLevel::Warning, format!("YouTube chat: {msg}"));
        let _ = inbound.send(Inbound::Status(ChatStatus::new(PLATFORM, ConnectionState::Error).detail(msg))).await;
        tokio::time::sleep(Duration::from_millis(wait + 60_000)).await;
    }

    async fn chat_loop(&self, chat_id: &str, inbound: &mpsc::Sender<Inbound>, outbound: &mut mpsc::Receiver<Outbound>) -> Result<(), ApiError> {
        let mut page_token: Option<String> = None;
        let mut first_page = true;
        loop {
            let mut query = vec![("liveChatId", chat_id), ("part", "snippet,authorDetails"), ("maxResults", "2000")];
            if let Some(t) = page_token.as_deref() {
                query.push(("pageToken", t));
            }
            let v = self.api(reqwest::Method::GET, "/liveChat/messages", &query, None, COST_CHAT_LIST).await?;

            // The first page is backlog from before we connected: skip it.
            if !first_page {
                for item in v["items"].as_array().into_iter().flatten() {
                    match item["snippet"]["type"].as_str() {
                        Some("textMessageEvent") => {
                            if let Some(msg) = to_chat_message(item) {
                                let _ = inbound.send(Inbound::Message(msg)).await;
                            }
                        }
                        Some("chatEndedEvent") => return Err(ApiError::ChatGone),
                        _ => {}
                    }
                }
            }
            first_page = false;
            if v["offlineAt"].is_string() {
                return Err(ApiError::ChatGone);
            }
            page_token = v["nextPageToken"].as_str().map(str::to_owned);

            let server_ms = v["pollingIntervalMillis"].as_u64().unwrap_or(5_000);
            let floor_ms = u64::from(self.settings.get().youtube_chat.min_poll_secs) * 1000;
            let deadline = Instant::now() + Duration::from_millis(server_ms.max(floor_ms));
            // Serve replies while waiting for the next poll.
            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline.into()) => break,
                    out = outbound.recv() => match out {
                        Some(out) => self.send(chat_id, &out).await,
                        None => { tokio::time::sleep_until(deadline.into()).await; break; }
                    }
                }
            }
        }
    }

    async fn send(&self, chat_id: &str, out: &Outbound) {
        let body = json!({
            "snippet": {
                "liveChatId": chat_id,
                "type": "textMessageEvent",
                "textMessageDetails": { "messageText": out.text },
            }
        });
        match self.api(reqwest::Method::POST, "/liveChat/messages", &[("part", "snippet")], Some(&body), COST_CHAT_INSERT).await {
            Ok(_) => {}
            Err(ApiError::QuotaExceeded) => tracing::warn!("YouTube reply skipped: quota budget reached"),
            Err(ApiError::ChatGone) => {}
            Err(ApiError::Other(err)) => tracing::warn!(%err, "YouTube reply failed"),
        }
    }
}

/// Discards replies while no chat is attached (e.g. waiting for a broadcast).
async fn drain_outbound_for(outbound: &mut mpsc::Receiver<Outbound>, wait: Duration) {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return,
            out = outbound.recv() => if out.is_none() { tokio::time::sleep_until(deadline).await; return; },
        }
    }
}

fn to_chat_message(item: &Value) -> Option<ChatMessage> {
    let author = &item["authorDetails"];
    let snippet = &item["snippet"];
    let text = snippet["textMessageDetails"]["messageText"].as_str().or(snippet["displayMessage"].as_str())?;
    let channel_id = author["channelId"].as_str().or(snippet["authorChannelId"].as_str())?;
    Some(ChatMessage {
        platform: PLATFORM,
        message_id: item["id"].as_str().unwrap_or_default().to_owned(),
        user: Requester {
            platform: PLATFORM.platform(),
            user_id: channel_id.to_owned(),
            display_name: author["displayName"].as_str().unwrap_or("viewer").trim_start_matches('@').to_owned(),
        },
        roles: ChatRoles {
            broadcaster: author["isChatOwner"].as_bool().unwrap_or(false),
            moderator: author["isChatModerator"].as_bool().unwrap_or(false),
            vip: false,
            subscriber: author["isChatSponsor"].as_bool().unwrap_or(false),
        },
        text: text.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_resets_on_pacific_day_boundary() {
        // 2026-11-15 07:59:59 UTC == 23:59:59 on the 14th at UTC-8.
        let before = 1_794_729_599_000i64;
        let after = before + 2_000;
        assert_ne!(quota_day(before), quota_day(after));
        let mut q = QuotaUsage::default();
        assert!(q.try_spend(9_995, 10_000, before));
        assert!(!q.try_spend(20, 10_000, before));
        assert!(q.try_spend(20, 10_000, after), "new day restores the budget");
        assert_eq!(q.used, 20);
    }

    #[test]
    fn next_reset_is_within_a_day() {
        let wait = ms_until_next_quota_day(1_794_729_599_000);
        assert_eq!(wait, 1_000);
    }

    #[test]
    fn maps_chat_items() {
        let item = json!({
            "id": "m1",
            "snippet": { "type": "textMessageEvent", "textMessageDetails": { "messageText": "!mbsong" } },
            "authorDetails": { "channelId": "UC1", "displayName": "@Viewer", "isChatModerator": true, "isChatOwner": false }
        });
        let msg = to_chat_message(&item).unwrap();
        assert_eq!(msg.text, "!mbsong");
        assert_eq!(msg.user.display_name, "Viewer");
        assert!(msg.roles.privileged());
    }
}
