use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Manager};

use crate::audio::cache::AudioCache;
use crate::chat::hub::ChatHub;
use crate::chat::twitch::TwitchClient;
use crate::chat::youtube::YouTubeClient;
use crate::error::{AppError, AppResult};
use crate::events::EventBus;
use crate::library::Library;
use crate::overlay::OverlayServer;
use crate::persistence::settings::SettingsStore;
use crate::persistence::Database;
use crate::player::PlaybackController;
use crate::playlists::Playlists;
use crate::resolver::spotify::SpotifyClient;
use crate::resolver::ytdlp::YtDlp;
use crate::resolver::Resolver;

/// Everything the IPC layer (and later the chat bots / overlay server) needs.
pub struct AppState {
    pub bus: EventBus,
    pub db: Database,
    pub settings: Arc<SettingsStore>,
    pub ytdlp: Arc<YtDlp>,
    pub spotify: Arc<SpotifyClient>,
    pub player: Arc<PlaybackController>,
    pub library: Arc<Library>,
    pub playlists: Arc<Playlists>,
    pub chat: Arc<ChatHub>,
    pub overlay: Arc<OverlayServer>,
}

impl AppState {
    pub async fn initialize(app: &AppHandle) -> AppResult<Self> {
        let paths = app.path();
        let data_dir = paths.app_data_dir()?;
        let cache_dir = paths.app_cache_dir()?.join("audio");
        std::fs::create_dir_all(&data_dir)?;
        tracing::info!(data = %data_dir.display(), cache = %cache_dir.display(), "storage locations");

        let bus = EventBus::new(app.clone());
        let db = Database::open(&data_dir.join("player.sqlite3"))?;
        let settings = SettingsStore::load(db.clone(), bus.clone()).await?;

        let http = reqwest::Client::builder()
            .user_agent(concat!("ComfuzioStreamerPlayer/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| AppError::Internal(format!("http client: {e}")))?;

        let ytdlp = Arc::new(YtDlp::new(settings.clone()));
        let spotify = Arc::new(SpotifyClient::new(http.clone(), db.clone(), settings.clone()).await?);
        let twitch = Arc::new(TwitchClient::new(http.clone(), db.clone(), settings.clone(), bus.clone()).await?);
        let youtube = Arc::new(YouTubeClient::new(http, db.clone(), settings.clone(), bus.clone()).await?);
        let library = Arc::new(Library::new(db.clone(), settings.clone(), bus.clone()));
        let resolver = Arc::new(Resolver::new(ytdlp.clone(), spotify.clone(), settings.clone(), library.clone()));
        let playlists = Arc::new(Playlists::new(
            db.clone(),
            bus.clone(),
            library.clone(),
            resolver.clone(),
            ytdlp.clone(),
            spotify.clone(),
            youtube.clone(),
        ));
        let cache = Arc::new(AudioCache::new(cache_dir, ytdlp.clone(), settings.clone())?);
        let player =
            PlaybackController::start(db.clone(), settings.clone(), resolver, cache, playlists.clone(), bus.clone()).await?;

        let chat = ChatHub::start(player.clone(), settings.clone(), bus.clone(), twitch, youtube);
        let overlay = OverlayServer::start(player.clone(), settings.clone(), bus.clone(), library.clone());

        Ok(Self { bus, db, settings, ytdlp, spotify, player, library, playlists, chat, overlay })
    }
}
