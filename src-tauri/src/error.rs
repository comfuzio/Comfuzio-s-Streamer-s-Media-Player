use std::fmt;

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

use crate::util::duration::format_human_ms;

/// Reasons a song request is refused. `Display` is phrased so it can be sent
/// straight back into chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    Blocked { title: String },
    TooLong { title: String, duration_ms: u64, max_ms: u64 },
    LiveStream,
    Duplicate { title: String },
    UserLimit { limit: u32 },
    QueueFull { limit: u32 },
    NotFound { query: String },
    Unsupported(String),
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blocked { title } => write!(f, "\"{title}\" is on the blocklist"),
            Self::TooLong { title, duration_ms, max_ms } => write!(
                f,
                "\"{title}\" is too long ({} > max {})",
                format_human_ms(*duration_ms),
                format_human_ms(*max_ms)
            ),
            Self::LiveStream => f.write_str("live streams can't be requested"),
            Self::Duplicate { title } => write!(f, "\"{title}\" is already in the queue"),
            Self::UserLimit { limit } => {
                write!(f, "you already have {limit} song(s) in the queue")
            }
            Self::QueueFull { limit } => write!(f, "the queue is full ({limit} songs)"),
            Self::NotFound { query } => write!(f, "no playable result for \"{query}\""),
            Self::Unsupported(what) => f.write_str(what),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("tauri error: {0}")]
    Tauri(#[from] tauri::Error),
    #[error("yt-dlp: {0}")]
    YtDlp(String),
    #[error("spotify: {0}")]
    Spotify(String),
    #[error("audio: {0}")]
    Audio(String),
    #[error("{0}")]
    Rejected(Rejection),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    NotFound(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl AppError {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Database(_) => "database",
            Self::Io(_) => "io",
            Self::Http(_) => "network",
            Self::Json(_) => "serialization",
            Self::Tauri(_) => "tauri",
            Self::YtDlp(_) => "ytDlp",
            Self::Spotify(_) => "spotify",
            Self::Audio(_) => "audio",
            Self::Rejected(_) => "rejected",
            Self::Invalid(_) => "invalid",
            Self::NotFound(_) => "notFound",
            Self::Internal(_) => "internal",
        }
    }

    pub fn rejected(r: Rejection) -> Self {
        Self::Rejected(r)
    }
}

/// Errors cross the IPC boundary as `{ kind, message }`.
impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("AppError", 2)?;
        s.serialize_field("kind", self.kind())?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}

pub type AppResult<T> = Result<T, AppError>;
