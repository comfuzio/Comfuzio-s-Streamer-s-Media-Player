//! User settings: schema, validation and a live, persisted store.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::Database;
use crate::error::{AppError, AppResult};
use crate::events::{AppEvent, EventBus};
use crate::util::duration::clamp_max_length;

pub const DEFAULT_NAMESPACE: &str = "mb";
pub const DEFAULT_OVERLAY_PORT: u16 = 3847;
pub const DEFAULT_SPOTIFY_REDIRECT_PORT: u16 = 3848;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum SearchProvider {
    /// Text requests are searched on YouTube directly.
    #[default]
    YouTube,
    /// Text requests are searched on Spotify (requires a connected account) for
    /// cleaner metadata, then matched to a YouTube audio stream.
    Spotify,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct YouTubeSettings {
    /// Override for the yt-dlp executable; defaults to the bundled sidecar.
    pub yt_dlp_path: Option<String>,
    /// Netscape-format cookies.txt (e.g. exported from a Premium session).
    pub cookies_file: Option<String>,
    /// Alternatively read cookies from a local browser profile: `firefox`, `chrome`, `edge`, ...
    pub cookies_from_browser: Option<String>,
    /// Passed to yt-dlp `--js-runtimes` (e.g. `deno`, `node:/path/to/node`).
    pub js_runtimes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct SpotifySettings {
    /// Client ID of the streamer's own Spotify developer app (PKCE, no secret).
    pub client_id: Option<String>,
    /// Loopback port for the OAuth redirect: `http://127.0.0.1:<port>/callback`.
    pub redirect_port: u16,
    /// ISO 3166-1 alpha-2 market for search, e.g. `US`.
    pub market: Option<String>,
}

impl Default for SpotifySettings {
    fn default() -> Self {
        Self { client_id: None, redirect_port: DEFAULT_SPOTIFY_REDIRECT_PORT, market: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct QueueSettings {
    /// Max songs one viewer may have queued at once (mods/broadcaster exempt).
    pub max_per_user: u32,
    pub max_size: u32,
    pub allow_duplicates: bool,
}

impl Default for QueueSettings {
    fn default() -> Self {
        Self { max_per_user: 3, max_size: 200, allow_duplicates: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct ChatSettings {
    /// Per-viewer cooldown between song requests (and `wrongsong`).
    pub request_cooldown_secs: u32,
    /// Per-channel cooldown for `song` / `queue` info replies.
    pub info_cooldown_secs: u32,
}

impl Default for ChatSettings {
    fn default() -> Self {
        Self { request_cooldown_secs: 5, info_cooldown_secs: 5 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct TwitchSettings {
    pub enabled: bool,
    /// Client ID of the streamer's Twitch app (client type "Public", Device Code flow).
    pub client_id: Option<String>,
    /// Channel login to listen to; `None` = the connected account's own channel
    /// (set this when the connected account is a separate bot account).
    pub channel: Option<String>,
    pub reply_in_chat: bool,
}

impl Default for TwitchSettings {
    fn default() -> Self {
        Self { enabled: false, client_id: None, channel: None, reply_in_chat: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct YouTubeChatSettings {
    pub enabled: bool,
    /// OAuth client ("Desktop app") from the streamer's Google Cloud project.
    pub client_id: Option<String>,
    /// Desktop-app client secrets are not confidential, but Google requires them.
    pub client_secret: Option<String>,
    /// Lower bound for the chat poll interval (the API may ask for longer).
    pub min_poll_secs: u32,
    /// Seconds between checks for an active broadcast while offline.
    pub offline_check_secs: u32,
    /// Daily API quota of the Google Cloud project (default allocation: 10 000).
    pub daily_quota: u32,
    /// Replies cost 20 quota units each, so they are opt-in.
    pub reply_in_chat: bool,
}

impl Default for YouTubeChatSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            client_id: None,
            client_secret: None,
            min_poll_secs: 6,
            offline_check_secs: 90,
            daily_quota: 10_000,
            reply_in_chat: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum OverlayTheme {
    Minimal,
    Neon,
    #[default]
    Glass,
    Vinyl,
}

/// Fonts offered in the builder: "system" plus Google Fonts families.
pub const OVERLAY_FONTS: &[&str] = &[
    "system", "Inter", "Poppins", "Montserrat", "Space Grotesk", "Outfit", "Bebas Neue", "Orbitron", "Rajdhani",
    "Press Start 2P", "Playfair Display", "JetBrains Mono",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct OverlaySettings {
    pub theme: OverlayTheme,
    pub accent_color: String,
    pub text_color: String,
    /// `#rrggbb` or `#rrggbbaa`.
    pub background_color: String,
    pub font: String,
    /// Percent, 50..=200.
    pub scale: u16,
    /// Card width in px at 100 % scale, 240..=1200.
    pub width: u16,
    /// 0..=48 px.
    pub corner_radius: u8,
    pub show_artwork: bool,
    pub show_requester: bool,
    pub show_progress: bool,
    pub show_up_next: bool,
    pub hide_when_idle: bool,
    pub animate: bool,
}

impl Default for OverlaySettings {
    fn default() -> Self {
        Self {
            theme: OverlayTheme::Glass,
            accent_color: "#9b6bff".into(),
            text_color: "#ffffff".into(),
            background_color: "#14121fb3".into(),
            font: "Inter".into(),
            scale: 100,
            width: 520,
            corner_radius: 18,
            show_artwork: true,
            show_requester: true,
            show_progress: true,
            show_up_next: false,
            hide_when_idle: true,
            animate: true,
        }
    }
}

fn is_hex_color(s: &str) -> bool {
    s.strip_prefix('#').is_some_and(|h| (h.len() == 6 || h.len() == 8) && h.chars().all(|c| c.is_ascii_hexdigit()))
}

impl OverlaySettings {
    fn validated(mut self) -> AppResult<Self> {
        for (name, value) in [
            ("accent", &mut self.accent_color),
            ("text", &mut self.text_color),
            ("background", &mut self.background_color),
        ] {
            *value = value.trim().to_ascii_lowercase();
            if !is_hex_color(value) {
                return Err(AppError::Invalid(format!("overlay {name} color must look like #rrggbb or #rrggbbaa")));
            }
        }
        if !OVERLAY_FONTS.contains(&self.font.as_str()) {
            return Err(AppError::Invalid(format!("unknown overlay font \"{}\"", self.font)));
        }
        self.scale = self.scale.clamp(50, 200);
        self.width = self.width.clamp(240, 1200);
        self.corner_radius = self.corner_radius.min(48);
        Ok(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct LibrarySettings {
    /// Folders scanned (recursively) for local music.
    pub folders: Vec<String>,
    /// Let text requests (`!sr artist - song`) match the local library before searching online.
    pub match_requests: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct AutoDjSettings {
    /// Play `playlist_id` whenever the request queue is empty.
    pub enabled: bool,
    pub playlist_id: Option<i64>,
    pub shuffle: bool,
}

impl Default for AutoDjSettings {
    fn default() -> Self {
        Self { enabled: false, playlist_id: None, shuffle: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Chat command namespace: `!<namespace>sr`, `!<namespace>skip`, ...
    pub command_namespace: String,
    /// 0..=100
    pub volume: u8,
    /// Max requested song length in seconds, clamped to 30..=3600.
    pub max_length_secs: u32,
    pub auto_stop_on_offline: bool,
    /// Output device name; `None` follows the system default.
    pub output_device: Option<String>,
    pub overlay_port: u16,
    pub search_provider: SearchProvider,
    /// Audio cache budget in MiB.
    pub cache_max_mib: u32,
    pub youtube: YouTubeSettings,
    pub spotify: SpotifySettings,
    pub queue: QueueSettings,
    pub chat: ChatSettings,
    pub twitch: TwitchSettings,
    pub youtube_chat: YouTubeChatSettings,
    pub overlay: OverlaySettings,
    /// Closing the window keeps the app running in the tray (and frees the dashboard's memory).
    pub close_to_tray: bool,
    /// Launch straight into the tray without opening the dashboard.
    pub start_in_tray: bool,
    pub library: LibrarySettings,
    pub auto_dj: AutoDjSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            command_namespace: DEFAULT_NAMESPACE.into(),
            volume: 50,
            max_length_secs: 10 * 60,
            auto_stop_on_offline: true,
            output_device: None,
            overlay_port: DEFAULT_OVERLAY_PORT,
            search_provider: SearchProvider::default(),
            cache_max_mib: 1024,
            youtube: YouTubeSettings::default(),
            spotify: SpotifySettings::default(),
            queue: QueueSettings::default(),
            chat: ChatSettings::default(),
            twitch: TwitchSettings::default(),
            youtube_chat: YouTubeChatSettings::default(),
            overlay: OverlaySettings::default(),
            close_to_tray: true,
            start_in_tray: false,
            library: LibrarySettings::default(),
            auto_dj: AutoDjSettings::default(),
        }
    }
}

fn trim_opt(v: &mut Option<String>) {
    *v = v.take().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
}

impl Settings {
    /// Normalizes user input and rejects values that can't be fixed up.
    pub fn validated(mut self) -> AppResult<Self> {
        let ns = self.command_namespace.trim().trim_start_matches('!').to_ascii_lowercase();
        if ns.is_empty() || ns.len() > 16 || !ns.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(AppError::Invalid(
                "command namespace must be 1-16 letters or digits (e.g. \"mb\", \"music\")".into(),
            ));
        }
        self.command_namespace = ns;
        self.volume = self.volume.min(100);
        self.max_length_secs =
            clamp_max_length(Duration::from_secs(u64::from(self.max_length_secs))).as_secs() as u32;
        if self.overlay_port < 1024 || self.spotify.redirect_port < 1024 {
            return Err(AppError::Invalid("ports must be 1024 or higher".into()));
        }
        if self.overlay_port == self.spotify.redirect_port {
            return Err(AppError::Invalid("overlay and Spotify redirect ports must differ".into()));
        }
        self.cache_max_mib = self.cache_max_mib.clamp(64, 32 * 1024);
        self.queue.max_per_user = self.queue.max_per_user.max(1);
        self.queue.max_size = self.queue.max_size.clamp(1, 5000);
        trim_opt(&mut self.output_device);
        trim_opt(&mut self.youtube.yt_dlp_path);
        trim_opt(&mut self.youtube.cookies_file);
        trim_opt(&mut self.youtube.cookies_from_browser);
        trim_opt(&mut self.youtube.js_runtimes);
        trim_opt(&mut self.spotify.client_id);
        trim_opt(&mut self.spotify.market);
        trim_opt(&mut self.twitch.client_id);
        trim_opt(&mut self.twitch.channel);
        if let Some(ch) = &mut self.twitch.channel {
            *ch = ch.trim_start_matches('#').trim_start_matches('@').to_ascii_lowercase();
            if !ch.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || ch.len() > 25 {
                return Err(AppError::Invalid("Twitch channel must be a channel login name".into()));
            }
        }
        trim_opt(&mut self.youtube_chat.client_id);
        trim_opt(&mut self.youtube_chat.client_secret);
        self.chat.request_cooldown_secs = self.chat.request_cooldown_secs.min(600);
        self.chat.info_cooldown_secs = self.chat.info_cooldown_secs.min(600);
        self.youtube_chat.min_poll_secs = self.youtube_chat.min_poll_secs.clamp(2, 120);
        self.youtube_chat.offline_check_secs = self.youtube_chat.offline_check_secs.clamp(30, 1800);
        self.youtube_chat.daily_quota = self.youtube_chat.daily_quota.max(100);
        self.overlay = self.overlay.validated()?;
        let mut folders: Vec<String> = Vec::new();
        for f in self.library.folders.drain(..) {
            let f = f.trim().to_owned();
            if !f.is_empty() && !folders.contains(&f) {
                folders.push(f);
            }
        }
        self.library.folders = folders;
        if self.auto_dj.playlist_id.is_none() {
            self.auto_dj.enabled = false;
        }
        if let Some(m) = &mut self.spotify.market {
            m.make_ascii_uppercase();
            if m.len() != 2 || !m.chars().all(|c| c.is_ascii_alphabetic()) {
                return Err(AppError::Invalid("Spotify market must be a 2-letter country code".into()));
            }
        }
        Ok(self)
    }

    pub fn max_length(&self) -> Duration {
        Duration::from_secs(u64::from(self.max_length_secs))
    }
}

/// In-memory settings mirror with write-through persistence.
pub struct SettingsStore {
    current: RwLock<Settings>,
    db: Database,
    bus: EventBus,
}

impl SettingsStore {
    pub async fn load(db: Database, bus: EventBus) -> AppResult<Arc<Self>> {
        let raw: Option<String> = db
            .call(|c| Ok(c.query_row("SELECT json FROM settings WHERE id = 1", [], |r| r.get(0)).optional()?))
            .await?;
        let settings = match raw {
            Some(json) => match serde_json::from_str::<Settings>(&json).map_err(AppError::from).and_then(Settings::validated) {
                Ok(s) => s,
                Err(err) => {
                    tracing::warn!(%err, "stored settings unreadable; falling back to defaults");
                    Settings::default()
                }
            },
            None => Settings::default(),
        };
        let store = Arc::new(Self { current: RwLock::new(settings.clone()), db, bus });
        store.persist(&settings).await?;
        Ok(store)
    }

    pub fn get(&self) -> Settings {
        self.current.read().expect("settings lock poisoned").clone()
    }

    /// Applies `f` to a copy, validates, persists, then publishes.
    pub async fn update<F>(&self, f: F) -> AppResult<Settings>
    where
        F: FnOnce(&mut Settings),
    {
        let mut next = self.get();
        f(&mut next);
        self.replace(next).await
    }

    pub async fn replace(&self, next: Settings) -> AppResult<Settings> {
        let next = next.validated()?;
        if next == self.get() {
            return Ok(next);
        }
        self.persist(&next).await?;
        *self.current.write().expect("settings lock poisoned") = next.clone();
        self.bus.emit(AppEvent::Settings(Box::new(next.clone())));
        Ok(next)
    }

    async fn persist(&self, s: &Settings) -> AppResult<()> {
        let json = serde_json::to_string(s)?;
        self.db
            .call(move |c| {
                c.execute(
                    "INSERT INTO settings (id, json) VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET json = excluded.json",
                    params![json],
                )?;
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_is_normalized() {
        let s = Settings { command_namespace: " !Music ".into(), ..Default::default() }.validated().unwrap();
        assert_eq!(s.command_namespace, "music");
    }

    #[test]
    fn namespace_rejects_symbols() {
        for bad in ["", "m b", "music!", "averyveryverylongnamespace"] {
            assert!(Settings { command_namespace: bad.into(), ..Default::default() }.validated().is_err());
        }
    }

    #[test]
    fn max_length_is_clamped() {
        let s = Settings { max_length_secs: 5, ..Default::default() }.validated().unwrap();
        assert_eq!(s.max_length_secs, 30);
        let s = Settings { max_length_secs: 99_999, ..Default::default() }.validated().unwrap();
        assert_eq!(s.max_length_secs, 3600);
    }

    #[test]
    fn overlay_colors_and_fonts_are_validated() {
        let mut s = Settings::default();
        s.overlay.accent_color = " #FFAA00 ".into();
        assert_eq!(s.clone().validated().unwrap().overlay.accent_color, "#ffaa00");
        s.overlay.accent_color = "red; background:url(x)".into();
        assert!(s.clone().validated().is_err());
        s.overlay.accent_color = "#ffaa00".into();
        s.overlay.font = "Comic</style>".into();
        assert!(s.validated().is_err());
    }

    #[test]
    fn unknown_and_missing_fields_use_defaults() {
        let s: Settings = serde_json::from_str(r#"{"commandNamespace":"radio","futureField":1}"#).unwrap();
        assert_eq!(s.command_namespace, "radio");
        assert_eq!(s.volume, 50);
    }
}
