//! Playlists: storage, editing, import (M3U, YouTube, Spotify) and export
//! (M3U, YouTube, Spotify). Imports are one-way snapshots that remember their
//! source, so they can be re-pulled later; exports are explicit actions.

pub mod m3u;

use std::path::Path;
use std::sync::Arc;

use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::chat::youtube::YouTubeClient;
use crate::error::{AppError, AppResult};
use crate::events::{AppEvent, EventBus};
use crate::library::Library;
use crate::model::{Track, TrackOrigin};
use crate::persistence::Database;
use crate::resolver::input::{parse_input, youtube_playlist_id, ParsedInput};
use crate::resolver::spotify::{spotify_playlist_id, SpotifyClient};
use crate::resolver::ytdlp::{watch_url, YtDlp};
use crate::resolver::{split_artist_title, track_from_search_hit, unmatched_spotify_track, Resolver};
use crate::util::now_ms;
use m3u::M3uEntry;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SourceKind {
    YouTube,
    Spotify,
    M3u,
}

impl SourceKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::YouTube => "youtube",
            Self::Spotify => "spotify",
            Self::M3u => "m3u",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "youtube" => Self::YouTube,
            "spotify" => Self::Spotify,
            "m3u" => Self::M3u,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistSource {
    pub kind: SourceKind,
    /// Playlist id (YouTube/Spotify) or file path (M3U).
    pub reference: String,
    pub synced_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub source: Option<PlaylistSource>,
    pub item_count: usize,
    pub total_duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistItem {
    pub id: i64,
    pub position: usize,
    pub track: Track,
    pub added_at_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistDetail {
    pub playlist: Playlist,
    pub items: Vec<PlaylistItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOutcome {
    pub playlist: Playlist,
    pub imported: usize,
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOutcome {
    pub url: Option<String>,
    pub exported: usize,
    pub skipped: usize,
}

const PLAYLIST_SELECT: &str = "SELECT p.id, p.name, p.created_at, p.updated_at, p.source_kind, p.source_ref, p.source_synced_at,
    (SELECT COUNT(*) FROM playlist_items i WHERE i.playlist_id = p.id),
    (SELECT COALESCE(SUM(json_extract(i.track_json, '$.durationMs')), 0) FROM playlist_items i WHERE i.playlist_id = p.id)
    FROM playlists p";

fn row_to_playlist(r: &Row<'_>) -> rusqlite::Result<Playlist> {
    let kind: Option<String> = r.get(4)?;
    let reference: Option<String> = r.get(5)?;
    Ok(Playlist {
        id: r.get(0)?,
        name: r.get(1)?,
        created_at_ms: r.get(2)?,
        updated_at_ms: r.get(3)?,
        source: match (kind.as_deref().and_then(SourceKind::parse), reference) {
            (Some(kind), Some(reference)) => Some(PlaylistSource { kind, reference, synced_at_ms: r.get(6)? }),
            _ => None,
        },
        item_count: r.get::<_, i64>(7)? as usize,
        total_duration_ms: r.get::<_, i64>(8)?.max(0) as u64,
    })
}

fn renumber(tx: &rusqlite::Transaction<'_>, playlist_id: i64) -> rusqlite::Result<()> {
    let ids: Vec<i64> = {
        let mut stmt = tx.prepare("SELECT id FROM playlist_items WHERE playlist_id = ?1 ORDER BY position, id")?;
        let rows = stmt.query_map([playlist_id], |r| r.get(0))?;
        rows.collect::<Result<_, _>>()?
    };
    let mut up = tx.prepare("UPDATE playlist_items SET position = ?1 WHERE id = ?2")?;
    for (i, id) in ids.iter().enumerate() {
        up.execute(params![i as i64, id])?;
    }
    Ok(())
}

fn validate_name(name: &str) -> AppResult<String> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 100 {
        return Err(AppError::Invalid("playlist name must be 1-100 characters".into()));
    }
    Ok(n.to_owned())
}

/// Converts a playlist track to an M3U entry (local path or shareable link).
fn to_m3u_entry(t: &Track) -> M3uEntry {
    let location = match &t.origin {
        TrackOrigin::Local { path, .. } => path.clone(),
        TrackOrigin::YouTube { video_id } => watch_url(video_id),
        TrackOrigin::Spotify { track_id, .. } => format!("https://open.spotify.com/track/{track_id}"),
    };
    M3uEntry {
        location,
        duration_secs: (t.duration_ms > 0).then(|| t.duration_ms.div_ceil(1000)),
        title: Some(t.display_name()),
    }
}

pub struct Playlists {
    db: Database,
    bus: EventBus,
    library: Arc<Library>,
    resolver: Arc<Resolver>,
    ytdlp: Arc<YtDlp>,
    spotify: Arc<SpotifyClient>,
    youtube: Arc<YouTubeClient>,
}

impl Playlists {
    pub fn new(
        db: Database,
        bus: EventBus,
        library: Arc<Library>,
        resolver: Arc<Resolver>,
        ytdlp: Arc<YtDlp>,
        spotify: Arc<SpotifyClient>,
        youtube: Arc<YouTubeClient>,
    ) -> Self {
        Self { db, bus, library, resolver, ytdlp, spotify, youtube }
    }

    fn changed(&self) {
        self.bus.emit(AppEvent::PlaylistsChanged);
    }

    // ------------------------------------------------------------------ storage

    pub async fn list(&self) -> AppResult<Vec<Playlist>> {
        self.db
            .call(|c| {
                let mut stmt = c.prepare(&format!("{PLAYLIST_SELECT} ORDER BY p.name COLLATE NOCASE"))?;
                let rows = stmt.query_map([], row_to_playlist)?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
    }

    pub async fn get(&self, id: i64) -> AppResult<Playlist> {
        self.db
            .call(move |c| Ok(c.query_row(&format!("{PLAYLIST_SELECT} WHERE p.id = ?1"), [id], row_to_playlist).optional()?))
            .await?
            .ok_or_else(|| AppError::NotFound("playlist not found".into()))
    }

    pub async fn tracks(&self, id: i64) -> AppResult<Vec<PlaylistItem>> {
        self.db
            .call(move |c| {
                let mut stmt = c.prepare(
                    "SELECT id, position, track_json, added_at FROM playlist_items WHERE playlist_id = ?1 ORDER BY position, id",
                )?;
                let rows = stmt.query_map([id], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?))
                })?;
                let mut items = Vec::new();
                for row in rows {
                    let (id, position, json, added_at_ms) = row?;
                    match serde_json::from_str::<Track>(&json) {
                        Ok(track) => items.push(PlaylistItem { id, position: position as usize, track, added_at_ms }),
                        Err(err) => tracing::warn!(item = id, %err, "skipping unreadable playlist item"),
                    }
                }
                Ok(items)
            })
            .await
    }

    pub async fn detail(&self, id: i64) -> AppResult<PlaylistDetail> {
        Ok(PlaylistDetail { playlist: self.get(id).await?, items: self.tracks(id).await? })
    }

    pub async fn create(&self, name: &str) -> AppResult<Playlist> {
        let id = self.insert(validate_name(name)?, None).await?;
        self.changed();
        self.get(id).await
    }

    async fn insert(&self, name: String, source: Option<(SourceKind, String)>) -> AppResult<i64> {
        self.db
            .call(move |c| {
                let now = now_ms();
                let (kind, reference) = source.map(|(k, r)| (Some(k.as_str()), Some(r))).unwrap_or((None, None));
                c.execute(
                    "INSERT INTO playlists (name, created_at, updated_at, source_kind, source_ref, source_synced_at)
                     VALUES (?1, ?2, ?2, ?3, ?4, CASE WHEN ?3 IS NULL THEN NULL ELSE ?2 END)",
                    params![name, now, kind, reference],
                )?;
                Ok(c.last_insert_rowid())
            })
            .await
    }

    pub async fn rename(&self, id: i64, name: &str) -> AppResult<Playlist> {
        let name = validate_name(name)?;
        let n = self
            .db
            .call(move |c| Ok(c.execute("UPDATE playlists SET name = ?1, updated_at = ?2 WHERE id = ?3", params![name, now_ms(), id])?))
            .await?;
        if n == 0 {
            return Err(AppError::NotFound("playlist not found".into()));
        }
        self.changed();
        self.get(id).await
    }

    pub async fn delete(&self, id: i64) -> AppResult<()> {
        self.db
            .call(move |c| {
                let tx = c.transaction()?;
                tx.execute("DELETE FROM playlist_items WHERE playlist_id = ?1", [id])?;
                tx.execute("DELETE FROM playlists WHERE id = ?1", [id])?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.changed();
        Ok(())
    }

    /// Inserts tracks at `position` (default: end). Returns the new item count.
    pub async fn add_tracks(&self, id: i64, tracks: Vec<Track>, position: Option<usize>) -> AppResult<usize> {
        self.get(id).await?;
        let count = self
            .db
            .call(move |c| {
                let tx = c.transaction()?;
                let len: i64 = tx.query_row("SELECT COUNT(*) FROM playlist_items WHERE playlist_id = ?1", [id], |r| r.get(0))?;
                let at = position.map(|p| (p as i64).min(len)).unwrap_or(len);
                tx.execute(
                    "UPDATE playlist_items SET position = position + ?1 WHERE playlist_id = ?2 AND position >= ?3",
                    params![tracks.len() as i64, id, at],
                )?;
                {
                    let mut ins = tx.prepare(
                        "INSERT INTO playlist_items (playlist_id, position, track_json, added_at) VALUES (?1, ?2, ?3, ?4)",
                    )?;
                    let now = now_ms();
                    for (i, t) in tracks.iter().enumerate() {
                        ins.execute(params![id, at + i as i64, serde_json::to_string(t)?, now])?;
                    }
                }
                tx.execute("UPDATE playlists SET updated_at = ?1 WHERE id = ?2", params![now_ms(), id])?;
                renumber(&tx, id)?;
                let total: i64 = tx.query_row("SELECT COUNT(*) FROM playlist_items WHERE playlist_id = ?1", [id], |r| r.get(0))?;
                tx.commit()?;
                Ok(total as usize)
            })
            .await?;
        self.changed();
        Ok(count)
    }

    /// Resolves a link or search and appends it.
    pub async fn add_link(&self, id: i64, input: &str) -> AppResult<Track> {
        let track = self.resolver.resolve(input).await?;
        self.add_tracks(id, vec![track.clone()], None).await?;
        Ok(track)
    }

    pub async fn add_library(&self, id: i64, library_ids: &[i64]) -> AppResult<usize> {
        let mut tracks = Vec::with_capacity(library_ids.len());
        for lid in library_ids {
            if let Some(t) = self.library.get(*lid).await? {
                tracks.push(t.to_track());
            }
        }
        self.add_tracks(id, tracks, None).await
    }

    pub async fn remove_item(&self, item_id: i64) -> AppResult<()> {
        self.db
            .call(move |c| {
                let tx = c.transaction()?;
                let pid: Option<i64> =
                    tx.query_row("SELECT playlist_id FROM playlist_items WHERE id = ?1", [item_id], |r| r.get(0)).optional()?;
                if let Some(pid) = pid {
                    tx.execute("DELETE FROM playlist_items WHERE id = ?1", [item_id])?;
                    renumber(&tx, pid)?;
                    tx.execute("UPDATE playlists SET updated_at = ?1 WHERE id = ?2", params![now_ms(), pid])?;
                }
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.changed();
        Ok(())
    }

    pub async fn move_item(&self, item_id: i64, to_index: usize) -> AppResult<()> {
        self.db
            .call(move |c| {
                let tx = c.transaction()?;
                let pid: i64 = tx
                    .query_row("SELECT playlist_id FROM playlist_items WHERE id = ?1", [item_id], |r| r.get(0))
                    .optional()?
                    .ok_or_else(|| AppError::NotFound("playlist item not found".into()))?;
                let mut ids: Vec<i64> = {
                    let mut stmt = tx.prepare("SELECT id FROM playlist_items WHERE playlist_id = ?1 ORDER BY position, id")?;
                    let rows = stmt.query_map([pid], |r| r.get(0))?;
                    rows.collect::<Result<_, _>>()?
                };
                let from = ids.iter().position(|i| *i == item_id).expect("item belongs to playlist");
                let moved = ids.remove(from);
                ids.insert(to_index.min(ids.len()), moved);
                {
                    let mut up = tx.prepare("UPDATE playlist_items SET position = ?1 WHERE id = ?2")?;
                    for (i, id) in ids.iter().enumerate() {
                        up.execute(params![i as i64, id])?;
                    }
                }
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.changed();
        Ok(())
    }

    async fn replace_items(&self, id: i64, tracks: Vec<Track>) -> AppResult<()> {
        self.db
            .call(move |c| {
                let tx = c.transaction()?;
                tx.execute("DELETE FROM playlist_items WHERE playlist_id = ?1", [id])?;
                {
                    let mut ins = tx.prepare(
                        "INSERT INTO playlist_items (playlist_id, position, track_json, added_at) VALUES (?1, ?2, ?3, ?4)",
                    )?;
                    let now = now_ms();
                    for (i, t) in tracks.iter().enumerate() {
                        ins.execute(params![id, i as i64, serde_json::to_string(t)?, now])?;
                    }
                }
                let now = now_ms();
                tx.execute("UPDATE playlists SET updated_at = ?1, source_synced_at = ?1 WHERE id = ?2", params![now, id])?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.changed();
        Ok(())
    }

    // ------------------------------------------------------------------ import

    /// Fetches tracks from a playlist source (shared by import and re-sync).
    async fn fetch_source(&self, kind: SourceKind, reference: &str) -> AppResult<(String, Vec<Track>, Vec<String>)> {
        match kind {
            SourceKind::YouTube => {
                let (title, entries) = self.ytdlp.playlist(reference).await?;
                let tracks = entries.iter().filter(|e| !e.is_live()).map(track_from_search_hit).collect();
                Ok((title, tracks, Vec::new()))
            }
            SourceKind::Spotify => {
                let (name, items) = self.spotify.playlist(reference).await?;
                Ok((name, items.into_iter().map(unmatched_spotify_track).collect(), Vec::new()))
            }
            SourceKind::M3u => self.read_m3u(Path::new(reference)).await,
        }
    }

    async fn read_m3u(&self, path: &Path) -> AppResult<(String, Vec<Track>, Vec<String>)> {
        let text = tokio::fs::read(path).await.map_err(|e| AppError::Invalid(format!("cannot read {}: {e}", path.display())))?;
        let text = String::from_utf8_lossy(&text);
        let base = path.parent().unwrap_or(Path::new("."));
        let name = text
            .lines()
            .find_map(|l| l.trim().strip_prefix("#PLAYLIST:").map(|n| n.trim().to_owned()))
            .filter(|n| !n.is_empty())
            .or_else(|| path.file_stem().and_then(|s| s.to_str()).map(str::to_owned))
            .unwrap_or_else(|| "Imported playlist".into());

        let mut tracks = Vec::new();
        let mut skipped = Vec::new();
        for entry in m3u::parse(&text) {
            match self.m3u_track(&entry, base).await {
                Ok(t) => tracks.push(t),
                Err(err) => skipped.push(format!("{} ({err})", entry.location)),
            }
        }
        Ok((name, tracks, skipped))
    }

    async fn m3u_track(&self, entry: &M3uEntry, base: &Path) -> AppResult<Track> {
        if let Some(path) = entry.file_path(base) {
            return self.library.track_for_path(&path).await;
        }
        match parse_input(&entry.location) {
            // Trust #EXTINF metadata when present: avoids one yt-dlp call per line.
            ParsedInput::YouTube { video_id } if entry.title.is_some() => {
                let (artist, title) = split_artist_title(entry.title.as_deref().unwrap_or_default(), "");
                Ok(Track {
                    key: Track::youtube_key(&video_id),
                    origin: TrackOrigin::YouTube { video_id: video_id.clone() },
                    title,
                    artist,
                    album: None,
                    artwork_url: Some(crate::resolver::ytdlp::thumbnail_url(&video_id)),
                    duration_ms: entry.duration_secs.unwrap_or(0) * 1000,
                    source_url: watch_url(&video_id),
                })
            }
            ParsedInput::Spotify { track_id } => Ok(unmatched_spotify_track(self.spotify.track(&track_id).await?)),
            _ => self.resolver.resolve(&entry.location).await,
        }
    }

    async fn import_into_new(&self, kind: SourceKind, reference: String) -> AppResult<ImportOutcome> {
        let (name, tracks, skipped) = self.fetch_source(kind, &reference).await?;
        if tracks.is_empty() {
            return Err(AppError::Invalid("that playlist has no playable tracks".into()));
        }
        let imported = tracks.len();
        let id = self.insert(validate_name(&name).unwrap_or_else(|_| "Imported playlist".into()), Some((kind, reference))).await?;
        self.replace_items(id, tracks).await?;
        Ok(ImportOutcome { playlist: self.get(id).await?, imported, skipped })
    }

    pub async fn import_m3u(&self, path: &str) -> AppResult<ImportOutcome> {
        self.import_into_new(SourceKind::M3u, path.to_owned()).await
    }

    /// Imports a YouTube or Spotify playlist link.
    pub async fn import_url(&self, url: &str) -> AppResult<ImportOutcome> {
        if let Some(list) = youtube_playlist_id(url) {
            return self.import_into_new(SourceKind::YouTube, list).await;
        }
        if let Some(id) = spotify_playlist_id(url) {
            return self.import_into_new(SourceKind::Spotify, id).await;
        }
        Err(AppError::Invalid("paste a YouTube playlist link (…list=…) or a Spotify playlist link".into()))
    }

    /// Re-pulls an imported playlist from its source, replacing its items.
    pub async fn sync(&self, id: i64) -> AppResult<ImportOutcome> {
        let pl = self.get(id).await?;
        let source = pl.source.ok_or_else(|| AppError::Invalid("this playlist wasn't imported, nothing to sync".into()))?;
        let (_, tracks, skipped) = self.fetch_source(source.kind, &source.reference).await?;
        let imported = tracks.len();
        self.replace_items(id, tracks).await?;
        Ok(ImportOutcome { playlist: self.get(id).await?, imported, skipped })
    }

    // ------------------------------------------------------------------ export

    pub async fn export_m3u(&self, id: i64, dest: &str) -> AppResult<ExportOutcome> {
        let pl = self.get(id).await?;
        let entries: Vec<M3uEntry> = self.tracks(id).await?.iter().map(|i| to_m3u_entry(&i.track)).collect();
        tokio::fs::write(dest, m3u::write(&pl.name, &entries)).await?;
        Ok(ExportOutcome { url: None, exported: entries.len(), skipped: 0 })
    }

    /// Creates a private YouTube playlist (needs the YouTube chat sign-in; ~50 quota units per song).
    pub async fn export_youtube(&self, id: i64) -> AppResult<ExportOutcome> {
        let pl = self.get(id).await?;
        let items = self.tracks(id).await?;
        let video_ids: Vec<String> = items
            .iter()
            .filter_map(|i| match &i.track.origin {
                TrackOrigin::YouTube { video_id } => Some(video_id.clone()),
                TrackOrigin::Spotify { matched_video_id: Some(v), .. } => Some(v.clone()),
                _ => None,
            })
            .collect();
        if video_ids.is_empty() {
            return Err(AppError::Invalid("no YouTube songs in this playlist (local and unmatched Spotify songs can't be exported)".into()));
        }
        let skipped = items.len() - video_ids.len();
        let (url, exported) = self.youtube.export_playlist(&pl.name, &video_ids).await?;
        Ok(ExportOutcome { url: Some(url), exported, skipped })
    }

    /// Creates a private Spotify playlist, matching non-Spotify songs by search.
    pub async fn export_spotify(&self, id: i64) -> AppResult<ExportOutcome> {
        let pl = self.get(id).await?;
        let items = self.tracks(id).await?;
        let mut uris = Vec::new();
        let mut skipped = 0;
        for item in &items {
            let t = &item.track;
            let found = match &t.origin {
                TrackOrigin::Spotify { track_id, .. } => Some(track_id.clone()),
                _ => self.spotify.search(&format!("{} {}", t.artist, t.title)).await.ok().flatten().map(|s| s.id),
            };
            match found {
                Some(id) => uris.push(format!("spotify:track:{id}")),
                None => skipped += 1,
            }
        }
        if uris.is_empty() {
            return Err(AppError::Invalid("none of these songs could be found on Spotify".into()));
        }
        let url = self.spotify.create_playlist(&pl.name, &uris).await?;
        Ok(ExportOutcome { url: Some(url), exported: uris.len(), skipped })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m3u_entries_from_tracks() {
        let yt = Track {
            key: "yt:dQw4w9WgXcQ".into(),
            origin: TrackOrigin::YouTube { video_id: "dQw4w9WgXcQ".into() },
            title: "Never Gonna Give You Up".into(),
            artist: "Rick Astley".into(),
            album: None,
            artwork_url: None,
            duration_ms: 212_500,
            source_url: String::new(),
        };
        let e = to_m3u_entry(&yt);
        assert_eq!(e.location, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert_eq!(e.duration_secs, Some(213));
        assert_eq!(e.title.as_deref(), Some("Rick Astley - Never Gonna Give You Up"));
    }
}
