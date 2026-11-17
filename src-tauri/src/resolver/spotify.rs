//! Spotify Web API client: OAuth 2.0 Authorization Code + PKCE via a loopback
//! redirect, token refresh, track lookup and search. Without a connected
//! account, track links still resolve through the public track page metadata.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use url::Url;

use crate::error::{AppError, AppResult};
use crate::persistence::settings::SettingsStore;
use crate::persistence::Database;
use crate::util::now_ms;
use crate::util::oauth::{accept_callback, pkce_challenge, random_urlsafe, StoredTokens};
use crate::util::text::decode_html_entities;

const AUTH_URL: &str = "https://accounts.spotify.com/authorize";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
const API_BASE: &str = "https://api.spotify.com/v1";
const TOKEN_KV_KEY: &str = "spotify.tokens";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Requested up-front so Premium playback (later phase) needs no re-consent.
const SCOPES: &str = "user-read-private user-read-email streaming user-read-playback-state user-modify-playback-state \
    playlist-read-private playlist-read-collaborative playlist-modify-private playlist-modify-public";

/// Extracts the id from `open.spotify.com/playlist/<id>` or `spotify:playlist:<id>`.
pub fn spotify_playlist_id(raw: &str) -> Option<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:open\.spotify\.com/(?:intl-[a-z-]+/)?playlist/|spotify:playlist:)([A-Za-z0-9]{22})").unwrap()
    });
    RE.captures(raw.trim()).map(|c| c[1].to_owned())
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpotifyTrack {
    pub id: String,
    pub name: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub artwork_url: Option<String>,
    pub duration_ms: u64,
}

impl SpotifyTrack {
    pub fn primary_artist(&self) -> &str {
        self.artists.first().map(String::as_str).unwrap_or_default()
    }

    pub fn artist_line(&self) -> String {
        self.artists.join(", ")
    }

    pub fn url(&self) -> String {
        format!("https://open.spotify.com/track/{}", self.id)
    }
}

#[derive(Deserialize)]
struct ApiImage {
    url: String,
    #[serde(default)]
    width: Option<u32>,
}

#[derive(Deserialize)]
struct ApiNamed {
    name: String,
}

#[derive(Deserialize)]
struct ApiAlbum {
    name: String,
    #[serde(default)]
    images: Vec<ApiImage>,
}

#[derive(Deserialize)]
struct ApiTrack {
    id: String,
    name: String,
    artists: Vec<ApiNamed>,
    album: ApiAlbum,
    duration_ms: u64,
}

impl From<ApiTrack> for SpotifyTrack {
    fn from(t: ApiTrack) -> Self {
        // Prefer a ~300px image: large enough for overlays, light to load.
        let artwork_url = t
            .album
            .images
            .iter()
            .min_by_key(|i| i.width.unwrap_or(640).abs_diff(300))
            .map(|i| i.url.clone());
        Self {
            id: t.id,
            name: t.name,
            artists: t.artists.into_iter().map(|a| a.name).collect(),
            album: Some(t.album.name),
            artwork_url,
            duration_ms: t.duration_ms,
        }
    }
}

#[derive(Deserialize)]
struct SearchResponse {
    tracks: SearchTracks,
}

#[derive(Deserialize)]
struct SearchTracks {
    items: Vec<ApiTrack>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpotifyStatus {
    pub configured: bool,
    pub connected: bool,
    pub redirect_uri: String,
}

pub struct SpotifyClient {
    http: reqwest::Client,
    db: Database,
    settings: Arc<SettingsStore>,
    tokens: Mutex<Option<StoredTokens>>,
    login_lock: Mutex<()>,
}

impl SpotifyClient {
    pub async fn new(http: reqwest::Client, db: Database, settings: Arc<SettingsStore>) -> AppResult<Self> {
        let tokens = db.kv_get::<StoredTokens>(TOKEN_KV_KEY).await?;
        Ok(Self { http, db, settings, tokens: Mutex::new(tokens), login_lock: Mutex::new(()) })
    }

    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.settings.get().spotify.redirect_port)
    }

    fn client_id(&self) -> AppResult<String> {
        self.settings
            .get()
            .spotify
            .client_id
            .ok_or_else(|| AppError::Spotify("set your Spotify app Client ID in settings first".into()))
    }

    pub async fn status(&self) -> SpotifyStatus {
        SpotifyStatus {
            configured: self.settings.get().spotify.client_id.is_some(),
            connected: self.tokens.lock().await.is_some(),
            redirect_uri: self.redirect_uri(),
        }
    }

    pub async fn is_connected(&self) -> bool {
        self.tokens.lock().await.is_some()
    }

    /// Runs the full PKCE flow: opens the consent page in the system browser and
    /// waits for the loopback redirect.
    pub async fn login(&self) -> AppResult<()> {
        let _guard = self
            .login_lock
            .try_lock()
            .map_err(|_| AppError::Spotify("a login is already in progress".into()))?;
        let client_id = self.client_id()?;
        let redirect_uri = self.redirect_uri();
        let port = self.settings.get().spotify.redirect_port;
        let verifier = random_urlsafe(64)?;
        let state = random_urlsafe(24)?;

        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| AppError::Spotify(format!("cannot listen on 127.0.0.1:{port}: {e}")))?;

        let mut auth = Url::parse(AUTH_URL).expect("static URL");
        auth.query_pairs_mut()
            .append_pair("client_id", &client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", &pkce_challenge(&verifier))
            .append_pair("state", &state)
            .append_pair("scope", SCOPES);
        tauri_plugin_opener::open_url(auth.as_str(), None::<&str>)
            .map_err(|e| AppError::Spotify(format!("could not open the browser: {e}")))?;

        let code = tokio::time::timeout(LOGIN_TIMEOUT, accept_callback(&listener, &state, "Spotify"))
            .await
            .map_err(|_| AppError::Spotify("login timed out".into()))??;
        drop(listener);

        let res: TokenResponse = self
            .token_request(&[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("redirect_uri", &redirect_uri),
                ("client_id", &client_id),
                ("code_verifier", &verifier),
            ])
            .await?;
        let refresh_token = res
            .refresh_token
            .clone()
            .ok_or_else(|| AppError::Spotify("token response had no refresh token".into()))?;
        self.store(StoredTokens {
            access_token: res.access_token,
            refresh_token,
            expires_at_ms: now_ms() + res.expires_in * 1000,
            scope: res.scope.unwrap_or_default(),
        })
        .await
    }

    pub async fn logout(&self) -> AppResult<()> {
        *self.tokens.lock().await = None;
        self.db.kv_delete(TOKEN_KV_KEY).await
    }

    async fn store(&self, tokens: StoredTokens) -> AppResult<()> {
        self.db.kv_set(TOKEN_KV_KEY, &tokens).await?;
        *self.tokens.lock().await = Some(tokens);
        Ok(())
    }

    async fn token_request(&self, form: &[(&str, &str)]) -> AppResult<TokenResponse> {
        let res = self.http.post(TOKEN_URL).form(form).send().await?;
        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            return Err(AppError::Spotify(format!("token endpoint returned {status}: {}", body.chars().take(200).collect::<String>())));
        }
        Ok(res.json().await?)
    }

    /// A valid access token, refreshed if it expires within a minute.
    /// `Ok(None)` when no account is connected.
    async fn access_token(&self) -> AppResult<Option<String>> {
        let mut guard = self.tokens.lock().await;
        let Some(tokens) = guard.as_ref() else { return Ok(None) };
        if tokens.expires_at_ms - now_ms() > 60_000 {
            return Ok(Some(tokens.access_token.clone()));
        }
        let client_id = self.client_id()?;
        let refresh = tokens.refresh_token.clone();
        let res = match self
            .token_request(&[("grant_type", "refresh_token"), ("refresh_token", &refresh), ("client_id", &client_id)])
            .await
        {
            Ok(r) => r,
            Err(err) => {
                // A revoked/invalid refresh token is unrecoverable: drop it.
                if err.to_string().contains("invalid_grant") {
                    *guard = None;
                    drop(guard);
                    self.db.kv_delete(TOKEN_KV_KEY).await?;
                }
                return Err(err);
            }
        };
        let updated = StoredTokens {
            access_token: res.access_token.clone(),
            refresh_token: res.refresh_token.unwrap_or(refresh),
            expires_at_ms: now_ms() + res.expires_in * 1000,
            scope: res.scope.unwrap_or_else(|| tokens.scope.clone()),
        };
        self.db.kv_set(TOKEN_KV_KEY, &updated).await?;
        *guard = Some(updated);
        Ok(Some(res.access_token))
    }

    async fn api_get<T: for<'de> Deserialize<'de>>(&self, token: &str, url: Url) -> AppResult<T> {
        let res = self.http.get(url).bearer_auth(token).send().await?;
        match res.status() {
            s if s.is_success() => Ok(res.json().await?),
            reqwest::StatusCode::NOT_FOUND => Err(AppError::NotFound("Spotify track not found".into())),
            reqwest::StatusCode::TOO_MANY_REQUESTS => Err(AppError::Spotify("rate limited, try again shortly".into())),
            s => Err(AppError::Spotify(format!("API returned {s}"))),
        }
    }

    pub async fn track(&self, track_id: &str) -> AppResult<SpotifyTrack> {
        match self.access_token().await {
            Ok(Some(token)) => {
                let mut url = Url::parse(&format!("{API_BASE}/tracks/{track_id}")).expect("valid URL");
                if let Some(m) = self.settings.get().spotify.market {
                    url.query_pairs_mut().append_pair("market", &m);
                }
                self.api_get::<ApiTrack>(&token, url).await.map(Into::into)
            }
            Ok(None) => self.public_track(track_id).await,
            Err(err) => {
                tracing::warn!(%err, "Spotify API unavailable; falling back to public metadata");
                self.public_track(track_id).await
            }
        }
    }

    /// Text search; requires a connected account.
    pub async fn search(&self, query: &str) -> AppResult<Option<SpotifyTrack>> {
        let token = self
            .access_token()
            .await?
            .ok_or_else(|| AppError::Spotify("connect a Spotify account to search Spotify".into()))?;
        let mut url = Url::parse(&format!("{API_BASE}/search")).expect("valid URL");
        url.query_pairs_mut().append_pair("q", query).append_pair("type", "track").append_pair("limit", "1");
        if let Some(m) = self.settings.get().spotify.market {
            url.query_pairs_mut().append_pair("market", &m);
        }
        let res: SearchResponse = self.api_get(&token, url).await?;
        Ok(res.tracks.items.into_iter().next().map(Into::into))
    }

    async fn api_send(&self, req: reqwest::RequestBuilder) -> AppResult<serde_json::Value> {
        let res = req.send().await?;
        let status = res.status();
        let body: serde_json::Value = res.json().await.unwrap_or(serde_json::Value::Null);
        if status.is_success() {
            return Ok(body);
        }
        let msg = body["error"]["message"].as_str().unwrap_or("").to_owned();
        let detail = if msg.is_empty() { String::new() } else { format!(": {msg}") };
        Err(AppError::Spotify(match status {
            reqwest::StatusCode::FORBIDDEN | reqwest::StatusCode::UNAUTHORIZED => format!(
                "Spotify refused access ({status}{detail}). Reconnect Spotify in Settings to grant playlist permissions. \
                 Spotify also blocks some playlist access for apps in Development Mode"
            ),
            reqwest::StatusCode::NOT_FOUND => "Spotify playlist not found (it must be yours or one you collaborate on)".into(),
            _ => format!("Spotify API returned {status}{detail}"),
        }))
    }

    async fn required_token(&self) -> AppResult<String> {
        self.access_token()
            .await?
            .ok_or_else(|| AppError::Spotify("connect your Spotify account in Settings first".into()))
    }

    /// Name and tracks of a playlist (paged, 50 per request). Local files and podcast episodes are skipped.
    pub async fn playlist(&self, playlist_id: &str) -> AppResult<(String, Vec<SpotifyTrack>)> {
        let token = self.required_token().await?;
        let meta = self
            .api_send(
                self.http
                    .get(format!("{API_BASE}/playlists/{playlist_id}"))
                    .query(&[("fields", "name")])
                    .bearer_auth(&token),
            )
            .await?;
        let name = meta["name"].as_str().unwrap_or("Spotify playlist").to_owned();
        let market = self.settings.get().spotify.market;
        let mut tracks = Vec::new();
        let mut offset = 0usize;
        loop {
            let mut req = self
                .http
                .get(format!("{API_BASE}/playlists/{playlist_id}/items"))
                .query(&[("limit", "50"), ("additional_types", "track")])
                .query(&[("offset", offset)])
                .bearer_auth(&token);
            if let Some(m) = &market {
                req = req.query(&[("market", m)]);
            }
            let page = self.api_send(req).await?;
            let items = page["items"].as_array().cloned().unwrap_or_default();
            for el in &items {
                // Feb 2026 API: the content moved from `track` to `item`.
                let item = if el["item"].is_object() { &el["item"] } else { &el["track"] };
                if el["is_local"].as_bool() == Some(true) || item["type"].as_str() != Some("track") {
                    continue;
                }
                if let Ok(t) = serde_json::from_value::<ApiTrack>(item.clone()) {
                    tracks.push(t.into());
                }
            }
            offset += items.len();
            if items.is_empty() || page["next"].is_null() || offset >= 10_000 {
                break;
            }
        }
        Ok((name, tracks))
    }

    /// Creates a private playlist with `uris` and returns its URL.
    pub async fn create_playlist(&self, name: &str, uris: &[String]) -> AppResult<String> {
        let token = self.required_token().await?;
        let created = self
            .api_send(self.http.post(format!("{API_BASE}/me/playlists")).bearer_auth(&token).json(&serde_json::json!({
                "name": name,
                "public": false,
                "description": "Exported from Comfuzio Streamer's Media Player",
            })))
            .await?;
        let id = created["id"]
            .as_str()
            .ok_or_else(|| AppError::Spotify("playlist creation returned no id".into()))?
            .to_owned();
        for chunk in uris.chunks(100) {
            self.api_send(
                self.http
                    .post(format!("{API_BASE}/playlists/{id}/items"))
                    .bearer_auth(&token)
                    .json(&serde_json::json!({ "uris": chunk })),
            )
            .await?;
        }
        Ok(created["external_urls"]["spotify"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("https://open.spotify.com/playlist/{id}")))
    }

    /// Expands `spotify.link` short links to their `open.spotify.com` target.
    pub async fn expand_short_link(&self, url: &Url) -> AppResult<Url> {
        let res = self.http.get(url.clone()).send().await?;
        let final_url = res.url().clone();
        if final_url.host_str() == Some("open.spotify.com") {
            return Ok(final_url);
        }
        // Some short links land on an interstitial page that embeds the target.
        static TARGET: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"https://open\.spotify\.com/[^"'\s<>]+"#).unwrap());
        let body = res.text().await?;
        TARGET
            .find(&body)
            .and_then(|m| Url::parse(&decode_html_entities(m.as_str())).ok())
            .ok_or_else(|| AppError::Spotify("could not expand Spotify short link".into()))
    }

    /// Metadata from the public track page (`<title>` and OpenGraph tags).
    async fn public_track(&self, track_id: &str) -> AppResult<SpotifyTrack> {
        static TITLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<title>(.*?)</title>").unwrap());
        static TITLE_PARTS: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^(.*) - song(?: and lyrics)? by (.*) \| Spotify$").unwrap());
        static OG_IMAGE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"<meta\s+property="og:image"\s+content="([^"]+)""#).unwrap());
        static DURATION: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"<meta\s+name="music:duration"\s+content="(\d+)""#).unwrap());

        let url = format!("https://open.spotify.com/track/{track_id}");
        let res = self.http.get(&url).header("Accept-Language", "en").send().await?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(AppError::NotFound("Spotify track not found".into()));
        }
        let html = res.error_for_status()?.text().await?;
        let title = TITLE
            .captures(&html)
            .map(|c| decode_html_entities(c[1].trim()))
            .ok_or_else(|| AppError::Spotify("track page had no title".into()))?;
        let (name, artists) = match TITLE_PARTS.captures(&title) {
            Some(c) => (c[1].to_owned(), c[2].split(", ").map(str::to_owned).collect()),
            None => (title.trim_end_matches(" | Spotify").to_owned(), Vec::new()),
        };
        Ok(SpotifyTrack {
            id: track_id.to_owned(),
            name,
            artists,
            album: None,
            artwork_url: OG_IMAGE.captures(&html).map(|c| decode_html_entities(&c[1])),
            duration_ms: DURATION.captures(&html).and_then(|c| c[1].parse::<u64>().ok()).unwrap_or(0) * 1000,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_ids() {
        let id = "37i9dQZF1DXcBWIGoYBM5M";
        assert_eq!(spotify_playlist_id(&format!("https://open.spotify.com/playlist/{id}?si=x")).as_deref(), Some(id));
        assert_eq!(spotify_playlist_id(&format!("https://open.spotify.com/intl-de/playlist/{id}")).as_deref(), Some(id));
        assert_eq!(spotify_playlist_id(&format!("spotify:playlist:{id}")).as_deref(), Some(id));
        assert_eq!(spotify_playlist_id("https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT"), None);
    }
}
