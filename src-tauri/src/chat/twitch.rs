//! Twitch connector.
//!
//! - Auth: OAuth Device Code Grant for a *Public* client (no client secret),
//!   scopes `user:read:chat user:write:chat`, refresh without secret.
//! - Inbound: EventSub over WebSocket (`channel.chat.message` v1, `stream.offline` v1).
//! - Outbound: Helix `POST /chat/messages`, threaded as replies, paced by a token bucket.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::limiter::TokenBucket;
use super::{ChatMessage, ChatPlatform, ChatRoles, ChatStatus, ConnectionState, Inbound, Outbound};
use crate::error::{AppError, AppResult};
use crate::events::{AppEvent, EventBus, NoticeLevel};
use crate::model::Requester;
use crate::persistence::settings::SettingsStore;
use crate::persistence::Database;
use crate::util::oauth::{excerpt, StoredTokens};

const DEVICE_URL: &str = "https://id.twitch.tv/oauth2/device";
const TOKEN_URL: &str = "https://id.twitch.tv/oauth2/token";
const VALIDATE_URL: &str = "https://id.twitch.tv/oauth2/validate";
const REVOKE_URL: &str = "https://id.twitch.tv/oauth2/revoke";
const HELIX: &str = "https://api.twitch.tv/helix";
const EVENTSUB_URL: &str = "wss://eventsub.wss.twitch.tv/ws?keepalive_timeout_seconds=30";
const SCOPES: &str = "user:read:chat user:write:chat";
const TOKENS_KEY: &str = "twitch.tokens";
const IDENTITY_KEY: &str = "twitch.identity";
const PLATFORM: ChatPlatform = ChatPlatform::Twitch;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TwitchIdentity {
    pub user_id: String,
    pub login: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceLogin {
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
}

#[derive(Deserialize)]
struct DeviceResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
    #[serde(default)]
    scope: Vec<String>,
}

#[derive(Deserialize)]
struct ValidateResponse {
    login: Option<String>,
    user_id: Option<String>,
}

#[derive(Deserialize)]
struct TwitchErrorBody {
    #[serde(default)]
    message: String,
}

/// Raised when no usable credentials exist; the connector waits for a login.
fn needs_login() -> AppError {
    AppError::Invalid("sign in to Twitch first".into())
}

pub struct TwitchClient {
    http: reqwest::Client,
    db: Database,
    settings: Arc<SettingsStore>,
    bus: EventBus,
    tokens: Mutex<Option<StoredTokens>>,
    identity: Mutex<Option<TwitchIdentity>>,
    login_task: std::sync::Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

impl TwitchClient {
    pub async fn new(http: reqwest::Client, db: Database, settings: Arc<SettingsStore>, bus: EventBus) -> AppResult<Self> {
        let tokens = db.kv_get::<StoredTokens>(TOKENS_KEY).await?;
        let identity = db.kv_get::<TwitchIdentity>(IDENTITY_KEY).await?;
        Ok(Self {
            http,
            db,
            settings,
            bus,
            tokens: Mutex::new(tokens),
            identity: Mutex::new(identity),
            login_task: std::sync::Mutex::new(None),
        })
    }

    fn client_id(&self) -> AppResult<String> {
        self.settings
            .get()
            .twitch
            .client_id
            .ok_or_else(|| AppError::Invalid("set your Twitch app Client ID in settings first".into()))
    }

    // ------------------------------------------------------------------ auth

    /// Starts the device-code flow: opens the activation page and polls for
    /// approval in the background. Emits `ChatAuth` when it completes.
    pub async fn begin_login(self: &Arc<Self>) -> AppResult<DeviceLogin> {
        let client_id = self.client_id()?;
        let res = self.http.post(DEVICE_URL).form(&[("client_id", client_id.as_str()), ("scopes", SCOPES)]).send().await?;
        if !res.status().is_success() {
            let status = res.status();
            return Err(AppError::Invalid(format!("Twitch device login failed ({status}): {}", excerpt(&res.text().await.unwrap_or_default()))));
        }
        let device: DeviceResponse = res.json().await?;
        let info = DeviceLogin {
            user_code: device.user_code.clone(),
            verification_uri: device.verification_uri.clone(),
            expires_in: device.expires_in,
        };
        if let Err(err) = tauri_plugin_opener::open_url(&device.verification_uri, None::<&str>) {
            tracing::warn!(%err, "could not open the Twitch activation page");
        }

        let this = self.clone();
        let task = tauri::async_runtime::spawn(async move {
            match this.poll_device(client_id, device).await {
                Ok(identity) => {
                    this.bus.notice(NoticeLevel::Info, format!("Twitch connected as {}", identity.login));
                    this.bus.emit(AppEvent::ChatAuth { platform: PLATFORM, connected: true });
                }
                Err(err) => {
                    this.bus.notice(NoticeLevel::Error, format!("Twitch login failed: {err}"));
                    this.bus.emit(AppEvent::ChatAuth { platform: PLATFORM, connected: false });
                }
            }
        });
        if let Some(previous) = self.login_task.lock().expect("login task lock").replace(task) {
            previous.abort();
        }
        Ok(info)
    }

    async fn poll_device(&self, client_id: String, device: DeviceResponse) -> AppResult<TwitchIdentity> {
        let deadline = Instant::now() + Duration::from_secs(device.expires_in);
        let mut interval = Duration::from_secs(device.interval.max(1));
        loop {
            tokio::time::sleep(interval).await;
            if Instant::now() > deadline {
                return Err(AppError::Invalid("the activation code expired, start the login again".into()));
            }
            let res = self
                .http
                .post(TOKEN_URL)
                .form(&[
                    ("client_id", client_id.as_str()),
                    ("scopes", SCOPES),
                    ("device_code", device.device_code.as_str()),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ])
                .send()
                .await?;
            if res.status().is_success() {
                let t: TokenResponse = res.json().await?;
                let tokens = StoredTokens::new(t.access_token, t.refresh_token, t.expires_in, t.scope.join(" "));
                let identity = self.validate(&tokens.access_token).await?;
                self.db.kv_set(TOKENS_KEY, &tokens).await?;
                self.db.kv_set(IDENTITY_KEY, &identity).await?;
                *self.tokens.lock().await = Some(tokens);
                *self.identity.lock().await = Some(identity.clone());
                return Ok(identity);
            }
            let body: TwitchErrorBody = res.json().await.unwrap_or(TwitchErrorBody { message: String::new() });
            match body.message.as_str() {
                "authorization_pending" => {}
                "slow_down" => interval += Duration::from_secs(5),
                other => return Err(AppError::Invalid(format!("Twitch refused the login: {other}"))),
            }
        }
    }

    pub async fn logout(&self) -> AppResult<()> {
        if let Some(task) = self.login_task.lock().expect("login task lock").take() {
            task.abort();
        }
        if let (Some(tokens), Ok(client_id)) = (self.tokens.lock().await.take(), self.client_id()) {
            // Best effort: revoke so the token can't be reused.
            let _ = self
                .http
                .post(REVOKE_URL)
                .form(&[("client_id", client_id.as_str()), ("token", tokens.access_token.as_str())])
                .send()
                .await;
        }
        *self.identity.lock().await = None;
        self.db.kv_delete(TOKENS_KEY).await?;
        self.db.kv_delete(IDENTITY_KEY).await
    }

    async fn validate(&self, access_token: &str) -> AppResult<TwitchIdentity> {
        let res = self.http.get(VALIDATE_URL).header("Authorization", format!("OAuth {access_token}")).send().await?;
        if res.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(needs_login());
        }
        let v: ValidateResponse = res.error_for_status()?.json().await?;
        match (v.user_id, v.login) {
            (Some(user_id), Some(login)) => Ok(TwitchIdentity { user_id, login }),
            _ => Err(AppError::Invalid("Twitch token is not a user token".into())),
        }
    }

    /// A fresh access token; refreshes when it expires within 5 minutes.
    async fn access_token(&self, force_refresh: bool) -> AppResult<String> {
        let mut guard = self.tokens.lock().await;
        let tokens = guard.as_ref().ok_or_else(needs_login)?;
        if !force_refresh && !tokens.expires_within(5 * 60 * 1000) {
            return Ok(tokens.access_token.clone());
        }
        let client_id = self.client_id()?;
        let res = self
            .http
            .post(TOKEN_URL)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", tokens.refresh_token.as_str()),
                ("client_id", client_id.as_str()),
            ])
            .send()
            .await?;
        if !res.status().is_success() {
            let status = res.status();
            if status == reqwest::StatusCode::BAD_REQUEST || status == reqwest::StatusCode::UNAUTHORIZED {
                // Refresh tokens are single-use and expire; the user has to sign in again.
                *guard = None;
                drop(guard);
                self.db.kv_delete(TOKENS_KEY).await?;
                self.bus.emit(AppEvent::ChatAuth { platform: PLATFORM, connected: false });
                return Err(AppError::Invalid("Twitch session expired, sign in again".into()));
            }
            return Err(AppError::Invalid(format!("Twitch token refresh failed ({status})")));
        }
        let t: TokenResponse = res.json().await?;
        let fresh = StoredTokens::new(t.access_token, t.refresh_token, t.expires_in, t.scope.join(" "));
        self.db.kv_set(TOKENS_KEY, &fresh).await?;
        let access = fresh.access_token.clone();
        *guard = Some(fresh);
        Ok(access)
    }

    /// Helix call with one transparent refresh on 401.
    async fn helix(&self, method: reqwest::Method, path: &str, query: &[(&str, &str)], body: Option<&Value>) -> AppResult<(reqwest::StatusCode, Value)> {
        let client_id = self.client_id()?;
        for attempt in 0..2 {
            let token = self.access_token(attempt == 1).await?;
            let mut req = self
                .http
                .request(method.clone(), format!("{HELIX}{path}"))
                .query(query)
                .bearer_auth(&token)
                .header("Client-Id", &client_id);
            if let Some(b) = body {
                req = req.json(b);
            }
            let res = req.send().await?;
            let status = res.status();
            if status == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
                continue;
            }
            let text = res.text().await.unwrap_or_default();
            let value = if text.is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::String(text)) };
            return Ok((status, value));
        }
        unreachable!("loop returns on the second attempt")
    }

    async fn user_by_login(&self, login: &str) -> AppResult<TwitchIdentity> {
        let (status, v) = self.helix(reqwest::Method::GET, "/users", &[("login", login)], None).await?;
        if !status.is_success() {
            return Err(AppError::Invalid(format!("Twitch user lookup failed ({status})")));
        }
        let user = &v["data"][0];
        match (user["id"].as_str(), user["login"].as_str()) {
            (Some(id), Some(login)) => Ok(TwitchIdentity { user_id: id.into(), login: login.into() }),
            _ => Err(AppError::NotFound(format!("Twitch channel \"{login}\" not found"))),
        }
    }

    async fn subscribe(&self, kind: &str, condition: Value, session_id: &str) -> AppResult<()> {
        let body = json!({
            "type": kind,
            "version": "1",
            "condition": condition,
            "transport": { "method": "websocket", "session_id": session_id },
        });
        let (status, v) = self.helix(reqwest::Method::POST, "/eventsub/subscriptions", &[], Some(&body)).await?;
        if status.is_success() || status == reqwest::StatusCode::CONFLICT {
            return Ok(());
        }
        Err(AppError::Invalid(format!("EventSub {kind} subscription failed ({status}): {}", excerpt(&v.to_string()))))
    }

    async fn send_chat(&self, broadcaster_id: &str, sender_id: &str, out: &Outbound) -> AppResult<Option<String>> {
        let mut body = json!({ "broadcaster_id": broadcaster_id, "sender_id": sender_id, "message": out.text });
        if let Some(parent) = &out.reply_to {
            body["reply_parent_message_id"] = json!(parent);
        }
        let (status, v) = self.helix(reqwest::Method::POST, "/chat/messages", &[], Some(&body)).await?;
        if !status.is_success() {
            return Err(AppError::Invalid(format!("Twitch send failed ({status}): {}", excerpt(&v.to_string()))));
        }
        let data = &v["data"][0];
        if data["is_sent"].as_bool() == Some(false) {
            let reason = data["drop_reason"]["message"].as_str().unwrap_or("unknown reason");
            tracing::warn!(reason, "Twitch dropped our chat message");
            return Ok(None);
        }
        Ok(data["message_id"].as_str().map(str::to_owned))
    }

    // ------------------------------------------------------------------ connection

    /// Connector main loop; runs until the task is aborted.
    pub async fn run(self: Arc<Self>, inbound: mpsc::Sender<Inbound>, mut outbound: mpsc::Receiver<Outbound>) {
        let mut backoff = Duration::from_secs(1);
        loop {
            let started = Instant::now();
            let result = self.session(&inbound, &mut outbound).await;
            if started.elapsed() > Duration::from_secs(120) {
                backoff = Duration::from_secs(1);
            }
            match result {
                Ok(()) => tracing::info!("Twitch EventSub closed; reconnecting"),
                Err(err) => {
                    let needs_auth = self.tokens.lock().await.is_none();
                    let state = if needs_auth { ConnectionState::NotConfigured } else { ConnectionState::Error };
                    let _ = inbound.send(Inbound::Status(ChatStatus::new(PLATFORM, state).detail(err.to_string()))).await;
                    if needs_auth {
                        // Wait for a login; the hub restarts this task when one completes.
                        std::future::pending::<()>().await;
                    }
                    tracing::warn!(%err, "Twitch connection error");
                }
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
    }

    async fn session(&self, inbound: &mpsc::Sender<Inbound>, outbound: &mut mpsc::Receiver<Outbound>) -> AppResult<()> {
        let token = self.access_token(false).await?;
        let me = self.validate(&token).await?;
        if self.identity.lock().await.as_ref() != Some(&me) {
            self.db.kv_set(IDENTITY_KEY, &me).await?;
            *self.identity.lock().await = Some(me.clone());
        }
        let channel = match self.settings.get().twitch.channel {
            Some(login) if login != me.login => self.user_by_login(&login).await?,
            _ => me.clone(),
        };

        let mut connecting = ChatStatus::new(PLATFORM, ConnectionState::Connecting);
        connecting.channel = Some(channel.login.clone());
        let _ = inbound.send(Inbound::Status(connecting)).await;

        let (mut ws, welcome) = connect(EVENTSUB_URL).await?;
        let session_id = welcome["session"]["id"].as_str().ok_or_else(|| AppError::Invalid("EventSub welcome had no session id".into()))?.to_owned();
        let mut keepalive = keepalive_window(&welcome);

        self.subscribe("channel.chat.message", json!({ "broadcaster_user_id": channel.user_id, "user_id": me.user_id }), &session_id).await?;
        self.subscribe("stream.offline", json!({ "broadcaster_user_id": channel.user_id }), &session_id).await?;

        let mut connected = ChatStatus::new(PLATFORM, ConnectionState::Connected);
        connected.channel = Some(channel.login.clone());
        connected.account = Some(me.login.clone());
        let _ = inbound.send(Inbound::Status(connected)).await;

        let sent_ids: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        // Twitch allows 20 messages / 30 s for regular accounts.
        let mut bucket = TokenBucket::new(20, Duration::from_secs(30));
        let mut last_validate = Instant::now();

        loop {
            tokio::select! {
                frame = tokio::time::timeout(keepalive, ws.next()) => {
                    let frame = match frame {
                        Err(_) => return Err(AppError::Invalid("EventSub keepalive timed out".into())),
                        Ok(None) => return Ok(()),
                        Ok(Some(Err(e))) => return Err(AppError::Invalid(format!("EventSub socket error: {e}"))),
                        Ok(Some(Ok(f))) => f,
                    };
                    let text = match frame {
                        Message::Text(t) => t,
                        Message::Close(c) => {
                            let code = c.map(|c| u16::from(c.code)).unwrap_or(0);
                            return Err(AppError::Invalid(format!("EventSub closed the connection (code {code})")));
                        }
                        _ => continue,
                    };
                    let msg: Value = serde_json::from_str(text.as_str())?;
                    let kind = msg["metadata"]["message_type"].as_str().unwrap_or_default();
                    match kind {
                        "session_keepalive" => {}
                        "notification" => {
                            let sub_type = msg["metadata"]["subscription_type"].as_str().unwrap_or_default();
                            let event = &msg["payload"]["event"];
                            match sub_type {
                                "channel.chat.message" => {
                                    if let Some(chat) = self.chat_message(event, &channel, &me, &sent_ids).await {
                                        let _ = inbound.send(Inbound::Message(chat)).await;
                                    }
                                }
                                "stream.offline" => {
                                    let _ = inbound.send(Inbound::StreamOffline(PLATFORM)).await;
                                }
                                other => tracing::debug!(other, "ignoring EventSub notification"),
                            }
                        }
                        "session_reconnect" => {
                            let url = msg["payload"]["session"]["reconnect_url"].as_str().ok_or_else(|| AppError::Invalid("reconnect without url".into()))?;
                            // Subscriptions carry over; the old socket closes when we drop it.
                            let (new_ws, new_welcome) = connect(url).await?;
                            keepalive = keepalive_window(&new_welcome);
                            let _ = ws.close(None).await;
                            ws = new_ws;
                            tracing::info!("EventSub session migrated");
                        }
                        "revocation" => {
                            let status = msg["payload"]["subscription"]["status"].as_str().unwrap_or("revoked");
                            return Err(AppError::Invalid(format!("Twitch revoked the chat subscription ({status})")));
                        }
                        other => tracing::debug!(other, "ignoring EventSub message"),
                    }
                    // Twitch requires periodic token validation for long-running apps.
                    if last_validate.elapsed() > Duration::from_secs(3600) {
                        last_validate = Instant::now();
                        let token = self.access_token(false).await?;
                        self.validate(&token).await?;
                    }
                }
                Some(out) = outbound.recv() => {
                    let wait = bucket.reserve(Instant::now());
                    if wait > Duration::from_secs(15) {
                        tracing::warn!("dropping Twitch reply: rate limit backlog");
                        continue;
                    }
                    tokio::time::sleep(wait).await;
                    match self.send_chat(&channel.user_id, &me.user_id, &out).await {
                        Ok(Some(id)) => {
                            let mut ids = sent_ids.lock().await;
                            ids.push_back(id);
                            if ids.len() > 64 {
                                ids.pop_front();
                            }
                        }
                        Ok(None) => {}
                        Err(err) => tracing::warn!(%err, "Twitch reply failed"),
                    }
                }
            }
        }
    }

    async fn chat_message(&self, event: &Value, channel: &TwitchIdentity, me: &TwitchIdentity, sent_ids: &Mutex<VecDeque<String>>) -> Option<ChatMessage> {
        // Shared Chat: messages from other channels carry their own badges; ignore them.
        if let Some(source) = event["source_broadcaster_user_id"].as_str() {
            if source != channel.user_id {
                return None;
            }
        }
        let message_id = event["message_id"].as_str()?.to_owned();
        let chatter_id = event["chatter_user_id"].as_str()?.to_owned();
        // Never react to our own replies.
        if sent_ids.lock().await.contains(&message_id) || (chatter_id == me.user_id && me.user_id != channel.user_id) {
            return None;
        }
        let mut roles = ChatRoles::default();
        for badge in event["badges"].as_array().into_iter().flatten() {
            match badge["set_id"].as_str().unwrap_or_default() {
                "broadcaster" => roles.broadcaster = true,
                "moderator" | "lead_moderator" => roles.moderator = true,
                "vip" => roles.vip = true,
                "subscriber" | "founder" => roles.subscriber = true,
                _ => {}
            }
        }
        if chatter_id == channel.user_id {
            roles.broadcaster = true;
        }
        let display = event["chatter_user_name"].as_str().or(event["chatter_user_login"].as_str()).unwrap_or("viewer");
        Some(ChatMessage {
            platform: PLATFORM,
            message_id,
            user: Requester { platform: PLATFORM.platform(), user_id: chatter_id, display_name: display.to_owned() },
            roles,
            text: event["message"]["text"].as_str().unwrap_or_default().to_owned(),
        })
    }
}

fn keepalive_window(welcome_payload: &Value) -> Duration {
    let secs = welcome_payload["session"]["keepalive_timeout_seconds"].as_u64().unwrap_or(30);
    Duration::from_secs(secs + 10)
}

/// Connects and waits for `session_welcome`; returns the socket and the welcome payload.
async fn connect(url: &str) -> AppResult<(Ws, Value)> {
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|e| AppError::Invalid(format!("cannot reach Twitch EventSub: {e}")))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let frame = tokio::time::timeout(remaining, ws.next())
            .await
            .map_err(|_| AppError::Invalid("no EventSub welcome within 10s".into()))?;
        match frame {
            Some(Ok(Message::Text(t))) => {
                let v: Value = serde_json::from_str(t.as_str())?;
                if v["metadata"]["message_type"] == "session_welcome" {
                    return Ok((ws, v["payload"].clone()));
                }
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(AppError::Invalid(format!("EventSub handshake failed: {e}"))),
            None => return Err(AppError::Invalid("EventSub closed during handshake".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in (network): `cargo test eventsub_handshake -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn eventsub_handshake_against_twitch() {
        let (_ws, welcome) = connect(EVENTSUB_URL).await.expect("handshake");
        let id = welcome["session"]["id"].as_str().expect("session id");
        println!("session {id}, keepalive window {:?}", keepalive_window(&welcome));
        assert_eq!(keepalive_window(&welcome), Duration::from_secs(40));
    }

    #[test]
    fn keepalive_window_adds_grace() {
        let welcome = json!({ "session": { "id": "x", "keepalive_timeout_seconds": 30 } });
        assert_eq!(keepalive_window(&welcome), Duration::from_secs(40));
    }
}
