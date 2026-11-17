//! Domain types shared by the resolver, queue, persistence and IPC layers.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Where a track came from. Online tracks play from a YouTube audio stream
/// (Spotify tracks via their matched video); local tracks play from disk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TrackOrigin {
    #[serde(rename_all = "camelCase")]
    YouTube { video_id: String },
    /// `matched_video_id` is filled when the track is first played, so large
    /// imported playlists don't need hundreds of YouTube searches up front.
    #[serde(rename_all = "camelCase")]
    Spotify {
        track_id: String,
        #[serde(default)]
        matched_video_id: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Local { path: String, library_id: Option<i64> },
}

/// How to obtain the audio for a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackSource<'a> {
    YouTube(&'a str),
    File(&'a Path),
    /// A Spotify track that still needs a YouTube match.
    Unresolved,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    /// Canonical identity, e.g. `yt:dQw4w9WgXcQ` or `sp:4cOdK2wGLETKBW3PvgPWqT`.
    pub key: String,
    pub origin: TrackOrigin,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub artwork_url: Option<String>,
    pub duration_ms: u64,
    /// The link a viewer would open to find this track.
    pub source_url: String,
}

impl Track {
    pub fn youtube_key(video_id: &str) -> String {
        format!("yt:{video_id}")
    }

    pub fn spotify_key(track_id: &str) -> String {
        format!("sp:{track_id}")
    }

    pub fn file_key(path: &str) -> String {
        format!("file:{path}")
    }

    pub fn playback(&self) -> PlaybackSource<'_> {
        match &self.origin {
            TrackOrigin::YouTube { video_id } => PlaybackSource::YouTube(video_id),
            TrackOrigin::Spotify { matched_video_id: Some(v), .. } => PlaybackSource::YouTube(v),
            TrackOrigin::Spotify { matched_video_id: None, .. } => PlaybackSource::Unresolved,
            TrackOrigin::Local { path, .. } => PlaybackSource::File(Path::new(path)),
        }
    }

    /// The YouTube video played for this track, if it is (already) one.
    pub fn youtube_video_id(&self) -> Option<&str> {
        match self.playback() {
            PlaybackSource::YouTube(v) => Some(v),
            _ => None,
        }
    }

    /// Every identity under which this track can be blocked.
    pub fn block_keys(&self) -> Vec<String> {
        match &self.origin {
            TrackOrigin::YouTube { video_id } => vec![Self::youtube_key(video_id)],
            TrackOrigin::Spotify { track_id, matched_video_id } => {
                let mut keys = vec![Self::spotify_key(track_id)];
                keys.extend(matched_video_id.as_deref().map(Self::youtube_key));
                keys
            }
            TrackOrigin::Local { path, .. } => vec![Self::file_key(path)],
        }
    }

    /// "Artist - Title" for chat replies.
    pub fn display_name(&self) -> String {
        if self.artist.is_empty() {
            self.title.clone()
        } else {
            format!("{} - {}", self.artist, self.title)
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum Platform {
    Twitch,
    YouTube,
    /// Requests made from the desktop dashboard.
    Local,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Requester {
    pub platform: Platform,
    /// Stable platform user id (Twitch user-id, YouTube channel id).
    pub user_id: String,
    pub display_name: String,
}

impl Requester {
    pub fn local() -> Self {
        Self { platform: Platform::Local, user_id: "local".into(), display_name: "Streamer".into() }
    }

    pub fn is_same_user(&self, other: &Requester) -> bool {
        self.platform == other.platform && self.user_id == other.user_id
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    /// Unique per queue entry (the same track can be queued twice if allowed).
    pub queue_id: String,
    pub track: Track,
    pub requester: Requester,
    pub added_at_ms: i64,
    /// Picked by Auto-DJ from a playlist (requester = the playlist) rather than requested.
    #[serde(default)]
    pub autoplay: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PlayerStatus {
    /// Halted by the streamer; requests queue up but nothing plays. Initial state.
    Stopped,
    /// Started, but the queue ran dry; the next request plays immediately.
    Idle,
    /// Fetching/decoding the next track.
    Loading,
    Playing,
    Paused,
}

/// Full player state pushed to the dashboard and overlay on every change.
/// Consumers interpolate progress locally from `positionMs` + `updatedAtMs`
/// while `status == playing`, so no polling is needed.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlayerSnapshot {
    pub status: PlayerStatus,
    pub current: Option<QueueItem>,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub volume: u8,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QueueSnapshot {
    pub items: Vec<QueueItem>,
    /// Sum of queued durations (excluding the current track).
    pub total_duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BlockEntry {
    pub key: String,
    /// Entries created by one block action share a group, so unblocking a
    /// Spotify link also lifts the matched YouTube video.
    pub group_id: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub source_url: Option<String>,
    pub blocked_by: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputDevice {
    pub name: String,
    pub is_default: bool,
}
