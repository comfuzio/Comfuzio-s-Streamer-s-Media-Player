//! Local music library: scans configured folders, reads tags and cover art,
//! and turns files into playable [`Track`]s.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use lofty::file::{AudioFile, TaggedFileExt};
use lofty::tag::Accessor;
use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::events::{AppEvent, EventBus, NoticeLevel};
use crate::model::{Track, TrackOrigin};
use crate::persistence::settings::SettingsStore;
use crate::persistence::Database;
use crate::util::text::{clean_video_title, tokens};

/// Extensions the decoder can play (Symphonia features in Cargo.toml).
pub const AUDIO_EXTENSIONS: &[&str] = &["mp3", "flac", "m4a", "mp4", "aac", "ogg", "oga", "wav", "wave"];
const COVER_NAMES: &[&str] = &["cover", "folder", "front", "album", "albumart"];
const COVER_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp"];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LibraryTrack {
    pub id: i64,
    pub path: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_ms: u64,
    pub has_art: bool,
}

impl LibraryTrack {
    pub fn to_track(&self) -> Track {
        Track {
            key: Track::file_key(&self.path),
            origin: TrackOrigin::Local { path: self.path.clone(), library_id: Some(self.id) },
            title: self.title.clone(),
            artist: self.artist.clone(),
            album: self.album.clone(),
            artwork_url: self.has_art.then(|| artwork_path(self.id)),
            duration_ms: self.duration_ms,
            source_url: self.path.clone(),
        }
    }
}

/// Served by the overlay server; the dashboard prefixes it with the server origin.
pub fn artwork_path(id: i64) -> String {
    format!("/art/local/{id}")
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummary {
    pub total: usize,
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub failed: usize,
    pub missing_folders: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryStatus {
    pub total: usize,
    pub scanning: bool,
}

/// Metadata read from a file.
#[derive(Debug, Clone, PartialEq)]
pub struct FileMeta {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_ms: u64,
    pub has_art: bool,
}

fn non_empty(s: Option<std::borrow::Cow<'_, str>>) -> Option<String> {
    s.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// "Artist - Title.mp3" → (artist, title); otherwise ("", stem).
fn guess_from_filename(path: &Path) -> (String, String) {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Unknown").replace('_', " ");
    // Drop leading track numbers like "01 - ", "01. ", "1 ".
    let trimmed = stem.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start_matches(['.', '-', ' ']);
    let base = if trimmed.is_empty() { stem.as_str() } else { trimmed };
    match base.split_once(" - ") {
        Some((a, t)) if !a.trim().is_empty() && !t.trim().is_empty() => (a.trim().to_owned(), clean_video_title(t)),
        _ => (String::new(), clean_video_title(base)),
    }
}

fn folder_cover(path: &Path) -> Option<PathBuf> {
    let dir = path.parent()?;
    for name in COVER_NAMES {
        for ext in COVER_EXTS {
            for candidate in [format!("{name}.{ext}"), format!("{}.{ext}", name.to_uppercase())] {
                let p = dir.join(candidate);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// Reads tags/properties; falls back to the file name when untagged.
pub fn read_meta(path: &Path) -> AppResult<FileMeta> {
    let tagged = lofty::read_from_path(path).map_err(|e| AppError::Invalid(format!("unreadable audio file: {e}")))?;
    let duration_ms = tagged.properties().duration().as_millis() as u64;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let (guess_artist, guess_title) = guess_from_filename(path);
    let title = tag.and_then(|t| non_empty(t.title())).unwrap_or(guess_title);
    let artist = tag.and_then(|t| non_empty(t.artist())).unwrap_or(guess_artist);
    let album = tag.and_then(|t| non_empty(t.album()));
    let has_art = tag.is_some_and(|t| !t.pictures().is_empty()) || folder_cover(path).is_some();
    Ok(FileMeta { title, artist, album, duration_ms, has_art })
}

/// Embedded cover (first picture) or a cover image next to the file.
pub fn read_artwork(path: &Path) -> Option<(Vec<u8>, String)> {
    if let Ok(tagged) = lofty::read_from_path(path) {
        let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
        if let Some(pic) = tag.and_then(|t| t.pictures().first()) {
            let mime = pic.mime_type().map(|m| m.as_str().to_owned()).unwrap_or_else(|| "image/jpeg".into());
            return Some((pic.data().to_vec(), mime));
        }
    }
    let cover = folder_cover(path)?;
    let mime = match cover.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        _ => "image/jpeg",
    };
    Some((std::fs::read(cover).ok()?, mime.to_owned()))
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

struct FoundFile {
    path: String,
    size: i64,
    modified_ms: i64,
}

/// Iterative recursive walk (no symlink following, so loops are impossible).
fn walk(root: &Path, out: &mut Vec<FoundFile>) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            let path = entry.path();
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() && is_audio(&path) {
                let Ok(meta) = entry.metadata() else { continue };
                let modified_ms = meta
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                if let Some(p) = path.to_str() {
                    out.push(FoundFile { path: p.to_owned(), size: meta.len() as i64, modified_ms });
                }
            }
        }
    }
}

fn search_blob(meta: &FileMeta, path: &str) -> String {
    let file = Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or("");
    format!("{} {} {} {}", meta.artist, meta.title, meta.album.as_deref().unwrap_or(""), file).to_lowercase()
}

const COLUMNS: &str = "id, path, title, artist, album, duration_ms, has_art";

fn row_to_track(r: &Row<'_>) -> rusqlite::Result<LibraryTrack> {
    Ok(LibraryTrack {
        id: r.get(0)?,
        path: r.get(1)?,
        title: r.get(2)?,
        artist: r.get(3)?,
        album: r.get(4)?,
        duration_ms: r.get::<_, i64>(5)? as u64,
        has_art: r.get::<_, i64>(6)? != 0,
    })
}

pub struct Library {
    db: Database,
    settings: Arc<SettingsStore>,
    bus: EventBus,
    scanning: AtomicBool,
}

impl Library {
    pub fn new(db: Database, settings: Arc<SettingsStore>, bus: EventBus) -> Self {
        Self { db, settings, bus, scanning: AtomicBool::new(false) }
    }

    pub async fn status(&self) -> AppResult<LibraryStatus> {
        let total: i64 = self.db.call(|c| Ok(c.query_row("SELECT COUNT(*) FROM library_tracks", [], |r| r.get(0))?)).await?;
        Ok(LibraryStatus { total: total as usize, scanning: self.scanning.load(Ordering::Acquire) })
    }

    /// Rescans all configured folders, reading tags only for new or changed files.
    pub async fn scan(&self) -> AppResult<ScanSummary> {
        if self.scanning.swap(true, Ordering::AcqRel) {
            return Err(AppError::Invalid("a library scan is already running".into()));
        }
        self.bus.emit(AppEvent::LibraryChanged);
        let folders = self.settings.get().library.folders;
        let db = self.db.clone();
        let result = tokio::task::spawn_blocking(move || scan_blocking(&db, &folders))
            .await
            .map_err(|e| AppError::Internal(format!("library scan failed: {e}")))
            .and_then(|r| r);
        self.scanning.store(false, Ordering::Release);
        self.bus.emit(AppEvent::LibraryChanged);
        match &result {
            Ok(s) => {
                let mut msg = format!(
                    "Library scan: {} tracks ({} new, {} updated, {} removed)",
                    s.total, s.added, s.updated, s.removed
                );
                if s.failed > 0 {
                    msg.push_str(&format!(", {} unreadable", s.failed));
                }
                if !s.missing_folders.is_empty() {
                    msg.push_str(&format!(". Missing folder(s): {}", s.missing_folders.join(", ")));
                }
                self.bus.notice(NoticeLevel::Info, msg);
            }
            Err(e) => self.bus.notice(NoticeLevel::Error, format!("Library scan failed: {e}")),
        }
        result
    }

    /// Every query word must appear in artist/title/album/file name.
    pub async fn search(&self, query: &str, limit: usize) -> AppResult<Vec<LibraryTrack>> {
        let words: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .map(|w| format!("%{}%", w.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")))
            .collect();
        self.db
            .call(move |c| {
                let mut sql = format!("SELECT {COLUMNS} FROM library_tracks");
                for (i, _) in words.iter().enumerate() {
                    sql.push_str(if i == 0 { " WHERE " } else { " AND " });
                    sql.push_str(&format!("search LIKE ?{} ESCAPE '\\'", i + 1));
                }
                sql.push_str(&format!(" ORDER BY artist COLLATE NOCASE, album COLLATE NOCASE, title COLLATE NOCASE LIMIT {limit}"));
                let mut stmt = c.prepare(&sql)?;
                let rows = stmt.query_map(rusqlite::params_from_iter(words.iter()), row_to_track)?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
    }

    pub async fn get(&self, id: i64) -> AppResult<Option<LibraryTrack>> {
        self.db
            .call(move |c| {
                Ok(c.query_row(&format!("SELECT {COLUMNS} FROM library_tracks WHERE id = ?1"), [id], row_to_track)
                    .optional()?)
            })
            .await
    }

    pub async fn by_path(&self, path: &str) -> AppResult<Option<LibraryTrack>> {
        let path = path.to_owned();
        self.db
            .call(move |c| {
                Ok(c.query_row(&format!("SELECT {COLUMNS} FROM library_tracks WHERE path = ?1"), [path], row_to_track)
                    .optional()?)
            })
            .await
    }

    /// Track for any audio file: from the library if indexed, otherwise read directly.
    pub async fn track_for_path(&self, path: &Path) -> AppResult<Track> {
        let p = path.to_str().ok_or_else(|| AppError::Invalid("file path is not valid UTF-8".into()))?.to_owned();
        if let Some(t) = self.by_path(&p).await? {
            return Ok(t.to_track());
        }
        if !path.is_file() || !is_audio(path) {
            return Err(AppError::NotFound(format!("not a playable audio file: {p}")));
        }
        let owned = path.to_path_buf();
        let meta = tokio::task::spawn_blocking(move || read_meta(&owned))
            .await
            .map_err(|e| AppError::Internal(e.to_string()))??;
        Ok(Track {
            key: Track::file_key(&p),
            origin: TrackOrigin::Local { path: p.clone(), library_id: None },
            title: meta.title,
            artist: meta.artist,
            album: meta.album,
            artwork_url: None,
            duration_ms: meta.duration_ms,
            source_url: p,
        })
    }

    /// Best library hit for a chat request, if the match is convincing.
    pub async fn best_match(&self, query: &str) -> AppResult<Option<LibraryTrack>> {
        let wanted = tokens(query);
        if wanted.is_empty() {
            return Ok(None);
        }
        let hits = self.search(&wanted.iter().cloned().collect::<Vec<_>>().join(" "), 10).await?;
        // All words matched (guaranteed by search); prefer the tightest title.
        Ok(hits.into_iter().min_by_key(|t| {
            let have = tokens(&format!("{} {}", t.artist, t.title));
            have.len().abs_diff(wanted.len())
        }))
    }

    pub async fn artwork(&self, id: i64) -> AppResult<Option<(Vec<u8>, String)>> {
        let Some(t) = self.get(id).await? else { return Ok(None) };
        tokio::task::spawn_blocking(move || read_artwork(Path::new(&t.path)))
            .await
            .map_err(|e| AppError::Internal(e.to_string()))
    }
}

fn scan_blocking(db: &Database, folders: &[String]) -> AppResult<ScanSummary> {
    let mut summary = ScanSummary::default();
    let mut found = Vec::new();
    for folder in folders {
        // Re-joining the components normalizes separators ("D:/Music" -> "D:\Music" on Windows).
        let p: PathBuf = Path::new(folder).components().collect();
        if p.is_dir() {
            walk(&p, &mut found);
        } else {
            summary.missing_folders.push(folder.clone());
        }
    }

    let existing: HashMap<String, (i64, i64)> = db.call_blocking(|c| {
        let mut stmt = c.prepare("SELECT path, file_size, modified_ms FROM library_tracks")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))))?;
        Ok(rows.collect::<Result<HashMap<_, _>, _>>()?)
    })?;

    // Read tags outside the DB lock: this is the slow part.
    let mut upserts: Vec<(FoundFile, FileMeta, bool)> = Vec::new();
    for file in &found {
        let is_new = !existing.contains_key(&file.path);
        if !is_new && existing.get(&file.path) == Some(&(file.size, file.modified_ms)) {
            continue;
        }
        match read_meta(Path::new(&file.path)) {
            Ok(meta) => upserts.push((FoundFile { path: file.path.clone(), size: file.size, modified_ms: file.modified_ms }, meta, is_new)),
            Err(err) => {
                summary.failed += 1;
                tracing::debug!(path = %file.path, %err, "skipping unreadable file");
            }
        }
    }
    let present: std::collections::HashSet<&str> = found.iter().map(|f| f.path.as_str()).collect();
    let removed: Vec<String> = existing.keys().filter(|p| !present.contains(p.as_str())).cloned().collect();

    db.call_blocking(|c| {
        let tx = c.transaction()?;
        {
            let mut up = tx.prepare(
                "INSERT INTO library_tracks (path, title, artist, album, duration_ms, has_art, file_size, modified_ms, search)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(path) DO UPDATE SET title = excluded.title, artist = excluded.artist, album = excluded.album,
                   duration_ms = excluded.duration_ms, has_art = excluded.has_art, file_size = excluded.file_size,
                   modified_ms = excluded.modified_ms, search = excluded.search",
            )?;
            for (f, m, is_new) in &upserts {
                up.execute(params![
                    f.path,
                    m.title,
                    m.artist,
                    m.album,
                    m.duration_ms as i64,
                    m.has_art as i64,
                    f.size,
                    f.modified_ms,
                    search_blob(m, &f.path)
                ])?;
                if *is_new {
                    summary.added += 1;
                } else {
                    summary.updated += 1;
                }
            }
            let mut del = tx.prepare("DELETE FROM library_tracks WHERE path = ?1")?;
            for p in &removed {
                del.execute([p])?;
            }
            summary.removed = removed.len();
        }
        tx.commit()?;
        summary.total = c.query_row("SELECT COUNT(*) FROM library_tracks", [], |r| r.get::<_, i64>(0))? as usize;
        Ok(())
    })?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal 16-bit PCM WAV with `secs` of silence.
    pub fn write_wav(path: &Path, secs: u32) {
        let rate = 8000u32;
        let data_len = rate * 2 * secs;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes()); // PCM
        b.extend_from_slice(&1u16.to_le_bytes()); // mono
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 2).to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        b.resize(b.len() + data_len as usize, 0);
        std::fs::write(path, b).unwrap();
    }

    #[test]
    fn filename_guessing() {
        assert_eq!(guess_from_filename(Path::new("/m/01 - Daft Punk - One More Time.mp3")), ("Daft Punk".into(), "One More Time".into()));
        assert_eq!(guess_from_filename(Path::new("/m/Intro.flac")), (String::new(), "Intro".into()));
        assert_eq!(guess_from_filename(Path::new("/m/2049.flac")), (String::new(), "2049".into()));
    }

    #[test]
    fn scans_adds_updates_and_removes() {
        let dir = std::env::temp_dir().join(format!("comfuzio-lib-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        write_wav(&dir.join("Artist A - First Song.wav"), 2);
        write_wav(&dir.join("sub").join("02 Second.wav"), 3);
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        let db = Database::open_in_memory().unwrap();
        let folders = vec![dir.to_string_lossy().into_owned(), "Z:/does/not/exist".into()];

        let s = scan_blocking(&db, &folders).unwrap();
        assert_eq!((s.total, s.added, s.removed), (2, 2, 0));
        assert_eq!(s.missing_folders.len(), 1);

        let again = scan_blocking(&db, &folders).unwrap();
        assert_eq!((again.added, again.updated), (0, 0), "unchanged files are skipped");

        std::fs::remove_file(dir.join("sub").join("02 Second.wav")).unwrap();
        let after = scan_blocking(&db, &folders).unwrap();
        assert_eq!((after.total, after.removed), (1, 1));

        let row: (String, String, i64) = db
            .call_blocking(|c| Ok(c.query_row("SELECT artist, title, duration_ms FROM library_tracks", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?))
            .unwrap();
        assert_eq!(row.0, "Artist A");
        assert_eq!(row.1, "First Song");
        assert!((1900..=2100).contains(&row.2), "duration {}", row.2);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
