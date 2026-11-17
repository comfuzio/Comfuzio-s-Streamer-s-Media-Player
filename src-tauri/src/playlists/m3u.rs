//! M3U / M3U8 reading and writing (extended `#EXTINF` format).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct M3uEntry {
    /// URL or file path exactly as written (relative paths are resolved by the caller).
    pub location: String,
    /// From `#EXTINF:<secs>,<title>`; `None` when absent or negative ("unknown").
    pub duration_secs: Option<u64>,
    pub title: Option<String>,
}

impl M3uEntry {
    pub fn is_url(&self) -> bool {
        let l = self.location.to_ascii_lowercase();
        l.starts_with("http://") || l.starts_with("https://") || l.starts_with("spotify:")
    }

    /// Resolves a file entry against the playlist's directory; handles `file://` URIs.
    pub fn file_path(&self, base_dir: &Path) -> Option<PathBuf> {
        if self.is_url() {
            return None;
        }
        let raw = match self.location.strip_prefix("file://") {
            Some(rest) => url::Url::parse(&format!("file://{rest}")).ok().and_then(|u| u.to_file_path().ok())?,
            None => PathBuf::from(self.location.replace('\\', std::path::MAIN_SEPARATOR_STR)),
        };
        Some(if raw.is_absolute() { raw } else { base_dir.join(raw) })
    }
}

/// Parses M3U text. Unknown directives are ignored; plain (non-extended) M3U works too.
pub fn parse(text: &str) -> Vec<M3uEntry> {
    let mut entries = Vec::new();
    let mut pending: (Option<u64>, Option<String>) = (None, None);
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(info) = line.strip_prefix("#EXTINF:") {
            let (dur, title) = info.split_once(',').unwrap_or((info, ""));
            // Duration may be followed by attributes: `#EXTINF:123 tvg-id="x",Title`.
            let dur = dur.split_whitespace().next().unwrap_or("").parse::<f64>().ok();
            pending = (
                dur.filter(|d| *d > 0.0).map(|d| d.round() as u64),
                Some(title.trim().to_owned()).filter(|t| !t.is_empty()),
            );
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let (duration_secs, title) = std::mem::take(&mut pending);
        entries.push(M3uEntry { location: line.to_owned(), duration_secs, title });
    }
    entries
}

/// Serializes entries as extended M3U8 (UTF-8).
pub fn write(name: &str, entries: &[M3uEntry]) -> String {
    let mut out = String::from("#EXTM3U\n");
    out.push_str(&format!("#PLAYLIST:{}\n", name.replace(['\r', '\n'], " ")));
    for e in entries {
        let dur = e.duration_secs.map(|d| d as i64).unwrap_or(-1);
        let title = e.title.as_deref().unwrap_or("").replace(['\r', '\n'], " ");
        out.push_str(&format!("#EXTINF:{dur},{title}\n{}\n", e.location));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_extended_and_plain_lines() {
        let text = "\u{feff}#EXTM3U\n#PLAYLIST:Mix\n#EXTINF:213,Rick Astley - Never Gonna Give You Up\nhttps://www.youtube.com/watch?v=dQw4w9WgXcQ\n\n#EXTINF:-1,\nmusic\\song.flac\nC:\\abs\\x.mp3\n#EXTINF:12.6 tvg-id=\"a\",Live\nhttp://x/y\n";
        let e = parse(text);
        assert_eq!(e.len(), 4);
        assert_eq!(e[0].duration_secs, Some(213));
        assert_eq!(e[0].title.as_deref(), Some("Rick Astley - Never Gonna Give You Up"));
        assert!(e[0].is_url());
        assert_eq!(e[1].duration_secs, None);
        assert_eq!(e[1].title, None);
        assert_eq!(e[2].title, None, "EXTINF applies to the next entry only");
        assert_eq!(e[3].duration_secs, Some(13));
        assert_eq!(e[3].title.as_deref(), Some("Live"));
    }

    #[test]
    fn resolves_relative_paths() {
        let base = Path::new("/music/lists");
        let e = M3uEntry { location: "../a/b.mp3".into(), duration_secs: None, title: None };
        assert_eq!(e.file_path(base).unwrap(), base.join("../a/b.mp3".replace('/', std::path::MAIN_SEPARATOR_STR)));
        let url = M3uEntry { location: "https://youtu.be/x".into(), duration_secs: None, title: None };
        assert!(url.file_path(base).is_none());
    }

    #[test]
    fn round_trips() {
        let entries = vec![
            M3uEntry { location: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".into(), duration_secs: Some(213), title: Some("A - B".into()) },
            M3uEntry { location: "C:/m/x.flac".into(), duration_secs: None, title: Some("X".into()) },
        ];
        assert_eq!(parse(&write("Mix", &entries)), entries);
    }
}
