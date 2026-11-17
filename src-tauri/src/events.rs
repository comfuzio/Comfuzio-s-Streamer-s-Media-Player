//! Application-wide event bus. Every state change is published once here and
//! fanned out to the Tauri frontend (`app-event`) and to in-process
//! subscribers (the overlay WebSocket server and chat bots in later phases).

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::broadcast;

use crate::chat::{ChatPlatform, ChatStatus};
use crate::model::{PlayerSnapshot, QueueSnapshot};
use crate::persistence::settings::Settings;

pub const FRONTEND_EVENT: &str = "app-event";

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    pub level: NoticeLevel,
    pub message: String,
}

// `Player` snapshots are by far the most frequent event; boxing them would add an
// allocation per update for no benefit, since events are short-lived clones.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "payload", rename_all = "camelCase")]
pub enum AppEvent {
    Player(PlayerSnapshot),
    Queue(QueueSnapshot),
    Settings(Box<Settings>),
    BlocklistChanged,
    LibraryChanged,
    PlaylistsChanged,
    SpotifyAuth { connected: bool },
    ChatStatus(ChatStatus),
    ChatAuth { platform: ChatPlatform, connected: bool },
    Notice(Notice),
}

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<AppEvent>,
    app: AppHandle,
}

impl EventBus {
    pub fn new(app: AppHandle) -> Self {
        let (tx, _) = broadcast::channel(256);
        Self { tx, app }
    }

    pub fn emit(&self, event: AppEvent) {
        if let Err(err) = self.app.emit(FRONTEND_EVENT, &event) {
            tracing::warn!(%err, "failed to forward event to frontend");
        }
        // No subscribers is fine.
        let _ = self.tx.send(event);
    }

    pub fn notice(&self, level: NoticeLevel, message: impl Into<String>) {
        let message = message.into();
        match level {
            NoticeLevel::Error => tracing::error!("{message}"),
            NoticeLevel::Warning => tracing::warn!("{message}"),
            NoticeLevel::Info => tracing::info!("{message}"),
        }
        self.emit(AppEvent::Notice(Notice { level, message }));
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.tx.subscribe()
    }
}
