//! On-disk audio cache. Tracks are downloaded once (deduplicating concurrent
//! requests), reused on replays, and evicted LRU-first beyond the size budget.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::{Mutex, OnceCell};

use crate::error::{AppError, AppResult};
use crate::persistence::settings::SettingsStore;
use crate::resolver::ytdlp::YtDlp;

/// Containers our decoder can open (matches `ytdlp::AUDIO_FORMAT`).
const AUDIO_EXTENSIONS: &[&str] = &["m4a", "mp4", "mp3", "flac", "ogg", "oga", "wav"];
const STALE_PART_AGE: Duration = Duration::from_secs(60 * 60);

type Download = Arc<OnceCell<Result<PathBuf, String>>>;

pub struct AudioCache {
    dir: PathBuf,
    ytdlp: Arc<YtDlp>,
    settings: Arc<SettingsStore>,
    inflight: Mutex<HashMap<String, Download>>,
}

impl AudioCache {
    pub fn new(dir: PathBuf, ytdlp: Arc<YtDlp>, settings: Arc<SettingsStore>) -> AppResult<Self> {
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir, ytdlp, settings, inflight: Mutex::new(HashMap::new()) })
    }

    /// Path to a decodable local copy of `video_id`, downloading if needed.
    /// `protect` lists video ids that pruning must keep (current/next tracks).
    pub async fn fetch(&self, video_id: &str, protect: &[String]) -> AppResult<PathBuf> {
        if let Some(path) = find_cached(&self.dir, video_id) {
            touch(&path);
            return Ok(path);
        }
        let cell = {
            let mut map = self.inflight.lock().await;
            map.entry(video_id.to_owned()).or_default().clone()
        };
        let result = cell
            .get_or_init(|| async {
                self.ytdlp.download_audio(video_id, &self.dir).await.map_err(|e| match e {
                    AppError::YtDlp(msg) => msg,
                    other => other.to_string(),
                })
            })
            .await
            .clone();
        self.inflight.lock().await.remove(video_id);
        let path = result.map_err(AppError::YtDlp)?;

        let budget = u64::from(self.settings.get().cache_max_mib) * 1024 * 1024;
        let dir = self.dir.clone();
        let mut keep: Vec<String> = protect.to_vec();
        keep.push(video_id.to_owned());
        if let Err(err) = tokio::task::spawn_blocking(move || prune(&dir, budget, &keep)).await {
            tracing::warn!(%err, "cache prune task failed");
        }
        Ok(path)
    }

    /// Removes cached files for `video_id` (e.g. after a decode failure).
    pub fn evict(&self, video_id: &str) {
        while let Some(path) = find_cached(&self.dir, video_id) {
            if let Err(err) = std::fs::remove_file(&path) {
                tracing::warn!(%err, path = %path.display(), "failed to evict cached audio");
                break;
            }
        }
    }
}

fn split_name(path: &Path) -> Option<(&str, &str)> {
    Some((path.file_stem()?.to_str()?, path.extension()?.to_str()?))
}

fn find_cached(dir: &Path, video_id: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| {
        p.is_file()
            && split_name(p).is_some_and(|(stem, ext)| stem == video_id && AUDIO_EXTENSIONS.contains(&ext))
    })
}

fn touch(path: &Path) {
    let result = std::fs::File::options().write(true).open(path).and_then(|f| f.set_modified(SystemTime::now()));
    if let Err(err) = result {
        tracing::debug!(%err, "could not refresh cache mtime");
    }
}

fn prune(dir: &Path, budget_bytes: u64, keep: &[String]) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let now = SystemTime::now();
    let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(now);
        let is_audio = split_name(&path).is_some_and(|(_, ext)| AUDIO_EXTENSIONS.contains(&ext));
        if !is_audio {
            // Leftover partial downloads from crashed/killed yt-dlp runs.
            if now.duration_since(modified).unwrap_or_default() > STALE_PART_AGE {
                let _ = std::fs::remove_file(&path);
            }
            continue;
        }
        files.push((path, meta.len(), modified));
    }
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total <= budget_bytes {
        return;
    }
    files.sort_by_key(|f| f.2);
    for (path, size, _) in files {
        if total <= budget_bytes {
            break;
        }
        let protected = split_name(&path).is_some_and(|(stem, _)| keep.iter().any(|k| k == stem));
        if protected {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => total = total.saturating_sub(size),
            Err(err) => tracing::debug!(%err, path = %path.display(), "could not evict cache file"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_evicts_oldest_unprotected_first() {
        let dir = std::env::temp_dir().join(format!("comfuzio-cache-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, age_secs: u64| {
            let p = dir.join(name);
            std::fs::write(&p, vec![0u8; 1000]).unwrap();
            let f = std::fs::File::options().write(true).open(&p).unwrap();
            f.set_modified(SystemTime::now() - Duration::from_secs(age_secs)).unwrap();
        };
        write("oldest00000.m4a", 300);
        write("protected00.m4a", 200);
        write("newest00000.m4a", 100);

        prune(&dir, 2000, &["protected00".to_owned()]);
        assert!(find_cached(&dir, "oldest00000").is_none());
        assert!(find_cached(&dir, "protected00").is_some());
        assert!(find_cached(&dir, "newest00000").is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
