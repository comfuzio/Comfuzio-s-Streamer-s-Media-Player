//! IPC commands for the dashboard window.

use tauri::State;

use crate::audio::device::list_output_devices;
use crate::library::{LibraryStatus, LibraryTrack, ScanSummary};
use crate::playlists::{ExportOutcome, ImportOutcome, Playlist, PlaylistDetail};
use crate::chat::twitch::DeviceLogin;
use crate::chat::youtube::QuotaUsage;
use crate::chat::{ChatPlatform, ChatStatus};
use crate::error::AppResult;
use crate::events::AppEvent;
use crate::model::{BlockEntry, OutputDevice, PlayerSnapshot, QueueItem, QueueSnapshot, Requester};
use crate::persistence::settings::{Settings, OVERLAY_FONTS};
use crate::player::{BlockOutcome, QueueSummary, RequestOutcome};
use crate::resolver::spotify::SpotifyStatus;
use crate::state::AppState;

type S<'a> = State<'a, AppState>;

#[tauri::command]
pub async fn player_snapshot(state: S<'_>) -> AppResult<PlayerSnapshot> {
    Ok(state.player.snapshot().await)
}

#[tauri::command]
pub async fn player_play(state: S<'_>) -> AppResult<PlayerSnapshot> {
    Ok(state.player.play().await)
}

#[tauri::command]
pub async fn player_pause(state: S<'_>) -> AppResult<PlayerSnapshot> {
    Ok(state.player.pause().await)
}

#[tauri::command]
pub async fn player_stop(state: S<'_>) -> AppResult<PlayerSnapshot> {
    Ok(state.player.stop().await)
}

#[tauri::command]
pub async fn player_skip(state: S<'_>) -> AppResult<Option<QueueItem>> {
    Ok(state.player.skip().await)
}

#[tauri::command]
pub async fn player_seek(state: S<'_>, position_ms: u64) -> AppResult<PlayerSnapshot> {
    state.player.seek(position_ms).await
}

#[tauri::command]
pub async fn player_set_volume(state: S<'_>, volume: u8) -> AppResult<u8> {
    state.player.set_volume(volume).await
}

#[tauri::command]
pub async fn queue_get(state: S<'_>) -> AppResult<QueueSnapshot> {
    Ok(state.player.queue().await)
}

#[tauri::command]
pub async fn queue_summary(state: S<'_>) -> AppResult<QueueSummary> {
    Ok(state.player.queue_summary().await)
}

/// Requests from the dashboard are made as the streamer (privileged).
#[tauri::command]
pub async fn queue_request(state: S<'_>, input: String) -> AppResult<RequestOutcome> {
    state.player.request(&input, Requester::local(), true).await
}

#[tauri::command]
pub async fn queue_remove(state: S<'_>, queue_id: String) -> AppResult<Option<QueueItem>> {
    Ok(state.player.remove(&queue_id).await)
}

#[tauri::command]
pub async fn queue_move(state: S<'_>, queue_id: String, to_index: usize) -> AppResult<()> {
    state.player.move_item(&queue_id, to_index).await
}

#[tauri::command]
pub async fn queue_clear(state: S<'_>) -> AppResult<usize> {
    Ok(state.player.clear().await)
}

#[tauri::command]
pub async fn settings_get(state: S<'_>) -> AppResult<Settings> {
    Ok(state.settings.get())
}

#[tauri::command]
pub async fn settings_update(state: S<'_>, settings: Settings) -> AppResult<Settings> {
    state.settings.replace(settings).await
}

/// Accepts `30s`, `5m`, `1h`, ...; returns the stored (clamped) value in seconds.
#[tauri::command]
pub async fn settings_set_max_length(state: S<'_>, value: String) -> AppResult<u64> {
    Ok(state.player.set_max_length(&value).await?.as_secs())
}

#[tauri::command]
pub async fn blocklist_get(state: S<'_>) -> AppResult<Vec<BlockEntry>> {
    state.db.blocklist_all().await
}

#[tauri::command]
pub async fn blocklist_add(state: S<'_>, target: Option<String>) -> AppResult<BlockOutcome> {
    state.player.block(target.as_deref(), "Streamer").await
}

#[tauri::command]
pub async fn blocklist_remove(state: S<'_>, key: String) -> AppResult<Vec<BlockEntry>> {
    state.player.unblock_key(&key).await
}

#[tauri::command]
pub async fn audio_devices() -> AppResult<Vec<OutputDevice>> {
    tauri::async_runtime::spawn_blocking(list_output_devices)
        .await
        .map_err(|e| crate::error::AppError::Internal(e.to_string()))?
}

#[tauri::command]
pub async fn spotify_status(state: S<'_>) -> AppResult<SpotifyStatus> {
    Ok(state.spotify.status().await)
}

#[tauri::command]
pub async fn spotify_login(state: S<'_>) -> AppResult<SpotifyStatus> {
    state.spotify.login().await?;
    state.bus.emit(AppEvent::SpotifyAuth { connected: true });
    Ok(state.spotify.status().await)
}

#[tauri::command]
pub async fn spotify_logout(state: S<'_>) -> AppResult<SpotifyStatus> {
    state.spotify.logout().await?;
    state.bus.emit(AppEvent::SpotifyAuth { connected: false });
    Ok(state.spotify.status().await)
}

#[tauri::command]
pub async fn ytdlp_version(state: S<'_>) -> AppResult<String> {
    state.ytdlp.version().await
}

#[tauri::command]
pub async fn chat_statuses(state: S<'_>) -> AppResult<Vec<ChatStatus>> {
    Ok(state.chat.statuses())
}

#[tauri::command]
pub async fn chat_reconnect(state: S<'_>, platform: ChatPlatform) -> AppResult<()> {
    state.chat.restart(platform);
    Ok(())
}

/// Starts the Twitch device-code login; returns the code to show the streamer.
#[tauri::command]
pub async fn twitch_login(state: S<'_>) -> AppResult<DeviceLogin> {
    state.chat.twitch.begin_login().await
}

#[tauri::command]
pub async fn twitch_logout(state: S<'_>) -> AppResult<()> {
    state.chat.twitch.logout().await?;
    state.bus.emit(AppEvent::ChatAuth { platform: ChatPlatform::Twitch, connected: false });
    Ok(())
}

/// Runs the Google loopback login; resolves with the channel title.
#[tauri::command]
pub async fn youtube_login(state: S<'_>) -> AppResult<String> {
    Ok(state.chat.youtube.login().await?.title)
}

#[tauri::command]
pub async fn youtube_logout(state: S<'_>) -> AppResult<()> {
    state.chat.youtube.logout().await
}

#[tauri::command]
pub async fn youtube_quota(state: S<'_>) -> AppResult<QuotaUsage> {
    Ok(state.chat.youtube.quota_usage().await)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayInfo {
    pub url: String,
    pub running: bool,
    pub fonts: Vec<&'static str>,
}

#[tauri::command]
pub async fn overlay_info(state: S<'_>) -> AppResult<OverlayInfo> {
    Ok(OverlayInfo { url: state.overlay.url(), running: state.overlay.is_running(), fonts: OVERLAY_FONTS.to_vec() })
}

// ---------------------------------------------------------------- library

#[tauri::command]
pub async fn library_status(state: S<'_>) -> AppResult<LibraryStatus> {
    state.library.status().await
}

#[tauri::command]
pub async fn library_scan(state: S<'_>) -> AppResult<ScanSummary> {
    state.library.scan().await
}

#[tauri::command]
pub async fn library_search(state: S<'_>, query: String) -> AppResult<Vec<LibraryTrack>> {
    state.library.search(&query, 500).await
}

/// Queues library tracks as the streamer (`next` = play right after the current song).
#[tauri::command]
pub async fn library_enqueue(state: S<'_>, ids: Vec<i64>, next: bool) -> AppResult<usize> {
    let mut tracks = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(t) = state.library.get(id).await? {
            tracks.push(t.to_track());
        }
    }
    state.player.enqueue_many(tracks, next).await
}

// ---------------------------------------------------------------- playlists

#[tauri::command]
pub async fn playlists_list(state: S<'_>) -> AppResult<Vec<Playlist>> {
    state.playlists.list().await
}

#[tauri::command]
pub async fn playlist_get(state: S<'_>, id: i64) -> AppResult<PlaylistDetail> {
    state.playlists.detail(id).await
}

#[tauri::command]
pub async fn playlist_create(state: S<'_>, name: String) -> AppResult<Playlist> {
    state.playlists.create(&name).await
}

#[tauri::command]
pub async fn playlist_rename(state: S<'_>, id: i64, name: String) -> AppResult<Playlist> {
    state.playlists.rename(id, &name).await
}

#[tauri::command]
pub async fn playlist_delete(state: S<'_>, id: i64) -> AppResult<()> {
    state.playlists.delete(id).await?;
    // Deleting the Auto-DJ playlist turns Auto-DJ off.
    if state.settings.get().auto_dj.playlist_id == Some(id) {
        state
            .settings
            .update(|s| {
                s.auto_dj.playlist_id = None;
                s.auto_dj.enabled = false;
            })
            .await?;
    }
    Ok(())
}

#[tauri::command]
pub async fn playlist_add_library(state: S<'_>, id: i64, library_ids: Vec<i64>) -> AppResult<usize> {
    state.playlists.add_library(id, &library_ids).await
}

#[tauri::command]
pub async fn playlist_add_link(state: S<'_>, id: i64, input: String) -> AppResult<()> {
    state.playlists.add_link(id, &input).await.map(|_| ())
}

/// Appends the current song (unless it's an Auto-DJ pick) and the whole queue.
#[tauri::command]
pub async fn playlist_add_queue(state: S<'_>, id: i64) -> AppResult<usize> {
    let snap = state.player.snapshot().await;
    let queue = state.player.queue().await;
    let tracks: Vec<_> = snap
        .current
        .into_iter()
        .filter(|c| !c.autoplay)
        .chain(queue.items)
        .map(|i| i.track)
        .collect();
    if tracks.is_empty() {
        return Err(crate::error::AppError::Invalid("the queue is empty".into()));
    }
    state.playlists.add_tracks(id, tracks, None).await
}

#[tauri::command]
pub async fn playlist_remove_item(state: S<'_>, item_id: i64) -> AppResult<()> {
    state.playlists.remove_item(item_id).await
}

#[tauri::command]
pub async fn playlist_move_item(state: S<'_>, item_id: i64, to_index: usize) -> AppResult<()> {
    state.playlists.move_item(item_id, to_index).await
}

#[tauri::command]
pub async fn playlist_enqueue(state: S<'_>, id: i64, shuffle: bool) -> AppResult<usize> {
    let mut tracks: Vec<_> = state.playlists.tracks(id).await?.into_iter().map(|i| i.track).collect();
    if shuffle {
        crate::player::shuffle(&mut tracks);
    }
    state.player.enqueue_many(tracks, false).await
}

#[tauri::command]
pub async fn playlist_import_m3u(state: S<'_>, path: String) -> AppResult<ImportOutcome> {
    state.playlists.import_m3u(&path).await
}

#[tauri::command]
pub async fn playlist_import_url(state: S<'_>, url: String) -> AppResult<ImportOutcome> {
    state.playlists.import_url(&url).await
}

#[tauri::command]
pub async fn playlist_sync(state: S<'_>, id: i64) -> AppResult<ImportOutcome> {
    state.playlists.sync(id).await
}

#[tauri::command]
pub async fn playlist_export_m3u(state: S<'_>, id: i64, path: String) -> AppResult<ExportOutcome> {
    state.playlists.export_m3u(id, &path).await
}

#[tauri::command]
pub async fn playlist_export_youtube(state: S<'_>, id: i64) -> AppResult<ExportOutcome> {
    state.playlists.export_youtube(id).await
}

#[tauri::command]
pub async fn playlist_export_spotify(state: S<'_>, id: i64) -> AppResult<ExportOutcome> {
    state.playlists.export_spotify(id).await
}
