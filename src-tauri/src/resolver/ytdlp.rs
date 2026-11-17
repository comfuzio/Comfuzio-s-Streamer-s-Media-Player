//! Thin async wrapper around the bundled `yt-dlp` sidecar.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use tokio::process::Command;

use crate::error::{AppError, AppResult};
use crate::persistence::settings::SettingsStore;

const METADATA_TIMEOUT: Duration = Duration::from_secs(45);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Audio-only formats our decoder (Symphonia) understands, best first.
/// YouTube's AAC (`m4a`, itag 140 / 141 for Premium) is the reliable choice.
pub const AUDIO_FORMAT: &str =
    "bestaudio[ext=m4a]/bestaudio[acodec^=mp4a]/bestaudio[ext=mp3]/bestaudio[acodec=flac]/bestaudio[acodec=vorbis]";

#[derive(Debug, Clone, Deserialize)]
pub struct YtVideo {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub uploader: Option<String>,
    /// Present on YouTube Music / auto-generated "Topic" uploads.
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub artists: Option<Vec<String>>,
    #[serde(default)]
    pub track: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub thumbnail: Option<String>,
    #[serde(default)]
    pub is_live: Option<bool>,
    #[serde(default)]
    pub live_status: Option<String>,
}

impl YtVideo {
    pub fn is_live(&self) -> bool {
        self.is_live == Some(true) || matches!(self.live_status.as_deref(), Some("is_live" | "is_upcoming"))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct YtSearchEntry {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub uploader: Option<String>,
    #[serde(default)]
    pub live_status: Option<String>,
}

impl YtSearchEntry {
    pub fn is_live(&self) -> bool {
        matches!(self.live_status.as_deref(), Some("is_live" | "is_upcoming"))
    }

    pub fn channel_name(&self) -> &str {
        self.channel.as_deref().or(self.uploader.as_deref()).unwrap_or_default()
    }
}

#[derive(Deserialize)]
struct SearchResult {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    entries: Vec<YtSearchEntry>,
}

/// Placeholder titles YouTube uses for playlist entries that cannot be played.
fn is_unavailable(e: &YtSearchEntry) -> bool {
    matches!(e.title.as_deref(), Some("[Private video]" | "[Deleted video]" | "[Unavailable video]"))
}

pub fn watch_url(video_id: &str) -> String {
    format!("https://www.youtube.com/watch?v={video_id}")
}

pub fn thumbnail_url(video_id: &str) -> String {
    format!("https://i.ytimg.com/vi/{video_id}/hqdefault.jpg")
}

pub struct YtDlp {
    settings: Arc<SettingsStore>,
}

impl YtDlp {
    pub fn new(settings: Arc<SettingsStore>) -> Self {
        Self { settings }
    }

    /// Resolution order: explicit setting -> sidecar next to our executable -> PATH.
    pub fn binary(&self) -> AppResult<PathBuf> {
        if let Some(custom) = self.settings.get().youtube.yt_dlp_path {
            let p = PathBuf::from(custom);
            return if p.is_file() {
                Ok(p)
            } else {
                Err(AppError::YtDlp(format!("configured yt-dlp path does not exist: {}", p.display())))
            };
        }
        let exe_name = if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" };
        if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
            let sidecar = dir.join(exe_name);
            if sidecar.is_file() {
                return Ok(sidecar);
            }
        }
        std::env::var_os("PATH")
            .and_then(|paths| std::env::split_paths(&paths).map(|d| d.join(exe_name)).find(|p| p.is_file()))
            .ok_or_else(|| AppError::YtDlp("yt-dlp executable not found (sidecar missing and not on PATH)".into()))
    }

    fn command(&self) -> AppResult<Command> {
        let mut cmd = Command::new(self.binary()?);
        cmd.kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .args(["--ignore-config", "--no-warnings", "--no-color"]);
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let yt = self.settings.get().youtube;
        if let Some(file) = &yt.cookies_file {
            cmd.arg("--cookies").arg(file);
        } else if let Some(browser) = &yt.cookies_from_browser {
            cmd.arg("--cookies-from-browser").arg(browser);
        }
        if let Some(rt) = &yt.js_runtimes {
            cmd.arg("--js-runtimes").arg(rt);
        }
        Ok(cmd)
    }

    async fn run(&self, mut cmd: Command, timeout: Duration) -> AppResult<Vec<u8>> {
        let child = cmd.spawn().map_err(|e| AppError::YtDlp(format!("failed to start yt-dlp: {e}")))?;
        let output = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| AppError::YtDlp(format!("timed out after {}s", timeout.as_secs())))??;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(AppError::YtDlp(summarize_stderr(&output.stderr)))
        }
    }

    async fn run_json<T: DeserializeOwned>(&self, args: &[&str], timeout: Duration) -> AppResult<T> {
        let mut cmd = self.command()?;
        cmd.args(args);
        let stdout = self.run(cmd, timeout).await?;
        serde_json::from_slice(&stdout).map_err(|e| AppError::YtDlp(format!("unexpected output: {e}")))
    }

    pub async fn version(&self) -> AppResult<String> {
        let mut cmd = self.command()?;
        cmd.arg("--version");
        let out = self.run(cmd, Duration::from_secs(20)).await?;
        Ok(String::from_utf8_lossy(&out).trim().to_owned())
    }

    pub async fn video(&self, video_id: &str) -> AppResult<YtVideo> {
        let url = watch_url(video_id);
        self.run_json(&["-J", "--no-playlist", "--skip-download", "--", &url], METADATA_TIMEOUT).await
    }

    /// Fast flat search (no per-video extraction).
    pub async fn search(&self, query: &str, limit: u8) -> AppResult<Vec<YtSearchEntry>> {
        let q = format!("ytsearch{}:{}", limit.clamp(1, 20), query);
        let res: SearchResult = self.run_json(&["-J", "--flat-playlist", "--", &q], METADATA_TIMEOUT).await?;
        Ok(res.entries.into_iter().filter(|e| crate::resolver::input::is_youtube_id(&e.id)).collect())
    }

    /// Lists a playlist's videos (flat: no per-video extraction). Returns (title, entries).
    pub async fn playlist(&self, list_id: &str) -> AppResult<(String, Vec<YtSearchEntry>)> {
        // Auto-generated mixes ("RD" + seed video id) are only listable through a watch URL.
        let url = match list_id.strip_prefix("RD").filter(|seed| crate::resolver::input::is_youtube_id(seed)) {
            Some(seed) => format!("https://www.youtube.com/watch?v={seed}&list={list_id}"),
            None => format!("https://www.youtube.com/playlist?list={list_id}"),
        };
        let res: SearchResult = self.run_json(&["-J", "--flat-playlist", "--", &url], Duration::from_secs(180)).await?;
        let entries = res
            .entries
            .into_iter()
            .filter(|e| crate::resolver::input::is_youtube_id(&e.id) && !is_unavailable(e))
            .collect();
        Ok((res.title.unwrap_or_else(|| "YouTube playlist".into()), entries))
    }

    /// Downloads the best decodable audio-only stream into `dir` and returns the file path.
    pub async fn download_audio(&self, video_id: &str, dir: &Path) -> AppResult<PathBuf> {
        let template = dir.join("%(id)s.%(ext)s");
        let url = watch_url(video_id);
        let mut cmd = self.command()?;
        cmd.args(["-f", AUDIO_FORMAT, "--no-playlist", "--no-mtime", "--no-progress", "--no-simulate"])
            .args(["--print", "after_move:filepath", "-o"])
            .arg(&template)
            .args(["--", &url]);
        let stdout = self.run(cmd, DOWNLOAD_TIMEOUT).await?;
        let path = String::from_utf8_lossy(&stdout)
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| AppError::YtDlp("download finished without reporting a file".into()))?;
        if path.is_file() {
            Ok(path)
        } else {
            Err(AppError::YtDlp(format!("downloaded file missing: {}", path.display())))
        }
    }
}

/// Reduces yt-dlp stderr to its most relevant line.
fn summarize_stderr(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text
        .lines()
        .rev()
        .find(|l| l.contains("ERROR"))
        .or_else(|| text.lines().rev().find(|l| !l.trim().is_empty()))
        .unwrap_or("unknown error");
    line.trim().trim_start_matches("ERROR:").trim().chars().take(300).collect()
}
