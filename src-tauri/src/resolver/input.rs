//! Classifies raw request text (`!mbsr <this>`) without touching the network.

use std::sync::LazyLock;

use regex::Regex;
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedInput {
    YouTube { video_id: String },
    Spotify { track_id: String },
    /// `spotify.link` / `spoti.fi` short links that must be expanded over HTTP.
    SpotifyShortLink(Url),
    /// A Spotify album/playlist/artist/episode: only single tracks are requestable.
    SpotifyNonTrack(String),
    /// A YouTube playlist link without a specific video.
    YouTubePlaylist,
    UnsupportedUrl(String),
    Search(String),
}

static YT_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{11}$").unwrap());
static SPOTIFY_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9]{22}$").unwrap());

pub fn is_youtube_id(s: &str) -> bool {
    YT_ID.is_match(s)
}

/// Extracts the `list=` id from a YouTube / YouTube Music playlist link.
pub fn youtube_playlist_id(raw: &str) -> Option<String> {
    let input = raw.trim();
    let normalized = if input.starts_with("http") { input.to_owned() } else { format!("https://{input}") };
    let url = Url::parse(&normalized).ok()?;
    let host = url.host_str()?.trim_start_matches("www.").to_ascii_lowercase();
    if !matches!(host.as_str(), "youtube.com" | "m.youtube.com" | "music.youtube.com" | "youtu.be") {
        return None;
    }
    url.query_pairs()
        .find(|(k, _)| k == "list")
        .map(|(_, v)| v.into_owned())
        .filter(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
}

pub fn parse_input(raw: &str) -> ParsedInput {
    let input = raw.trim().trim_matches(|c| c == '<' || c == '>');

    if let Some(rest) = input.strip_prefix("spotify:") {
        let mut parts = rest.split(':');
        return match (parts.next(), parts.next()) {
            (Some("track"), Some(id)) if SPOTIFY_ID.is_match(id) => ParsedInput::Spotify { track_id: id.into() },
            (Some(kind), _) => ParsedInput::SpotifyNonTrack(kind.into()),
            _ => ParsedInput::UnsupportedUrl(input.into()),
        };
    }

    let looks_like_url = input.starts_with("http://") || input.starts_with("https://") || input.starts_with("www.")
        || input.starts_with("youtu.be/") || input.starts_with("youtube.com/") || input.starts_with("m.youtube.com/")
        || input.starts_with("music.youtube.com/") || input.starts_with("open.spotify.com/");
    if !looks_like_url {
        return ParsedInput::Search(input.split_whitespace().collect::<Vec<_>>().join(" "));
    }

    let normalized = if input.starts_with("http") { input.to_owned() } else { format!("https://{input}") };
    let Ok(url) = Url::parse(&normalized) else {
        return ParsedInput::UnsupportedUrl(input.into());
    };
    let host = url.host_str().unwrap_or_default().trim_start_matches("www.").to_ascii_lowercase();
    let segments: Vec<&str> = url.path_segments().map(|s| s.filter(|p| !p.is_empty()).collect()).unwrap_or_default();

    match host.as_str() {
        "youtube.com" | "m.youtube.com" | "music.youtube.com" | "youtube-nocookie.com" => {
            let id = match segments.as_slice() {
                ["watch", ..] => url.query_pairs().find(|(k, _)| k == "v").map(|(_, v)| v.into_owned()),
                ["shorts" | "live" | "embed" | "v" | "e", id, ..] => Some((*id).to_owned()),
                ["playlist", ..] => return ParsedInput::YouTubePlaylist,
                _ => None,
            };
            match id {
                Some(id) if is_youtube_id(&id) => ParsedInput::YouTube { video_id: id },
                _ => ParsedInput::UnsupportedUrl(input.into()),
            }
        }
        "youtu.be" => match segments.first() {
            Some(id) if is_youtube_id(id) => ParsedInput::YouTube { video_id: (*id).to_owned() },
            _ => ParsedInput::UnsupportedUrl(input.into()),
        },
        "open.spotify.com" | "play.spotify.com" => {
            // Strip locale prefixes like /intl-de/ and embed prefixes.
            let segs: Vec<&str> =
                segments.iter().copied().filter(|s| !s.starts_with("intl-") && *s != "embed").collect();
            match segs.as_slice() {
                ["track", id, ..] if SPOTIFY_ID.is_match(id) => ParsedInput::Spotify { track_id: (*id).to_owned() },
                [kind, ..] => ParsedInput::SpotifyNonTrack((*kind).to_owned()),
                [] => ParsedInput::UnsupportedUrl(input.into()),
            }
        }
        "spotify.link" | "spoti.fi" => ParsedInput::SpotifyShortLink(url),
        _ => ParsedInput::UnsupportedUrl(input.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yt(id: &str) -> ParsedInput {
        ParsedInput::YouTube { video_id: id.into() }
    }

    #[test]
    fn youtube_variants() {
        let id = "dQw4w9WgXcQ";
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtube.com/watch?feature=share&v=dQw4w9WgXcQ&t=42",
            "youtube.com/watch?v=dQw4w9WgXcQ",
            "https://m.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ&list=RDAMVM",
            "https://youtu.be/dQw4w9WgXcQ?si=abc",
            "youtu.be/dQw4w9WgXcQ",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
            "https://www.youtube.com/embed/dQw4w9WgXcQ",
            "<https://youtu.be/dQw4w9WgXcQ>",
        ] {
            assert_eq!(parse_input(url), yt(id), "{url}");
        }
        assert_eq!(parse_input("https://www.youtube.com/playlist?list=PL123"), ParsedInput::YouTubePlaylist);
        assert!(matches!(parse_input("https://youtube.com/watch?v=short"), ParsedInput::UnsupportedUrl(_)));
    }

    #[test]
    fn spotify_variants() {
        let id = "4cOdK2wGLETKBW3PvgPWqT";
        let sp = ParsedInput::Spotify { track_id: id.into() };
        assert_eq!(parse_input("https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT?si=x"), sp);
        assert_eq!(parse_input("https://open.spotify.com/intl-de/track/4cOdK2wGLETKBW3PvgPWqT"), sp);
        assert_eq!(parse_input("spotify:track:4cOdK2wGLETKBW3PvgPWqT"), sp);
        assert_eq!(
            parse_input("https://open.spotify.com/album/1DFixLWuPkv3KT3TnV35m3"),
            ParsedInput::SpotifyNonTrack("album".into())
        );
        assert!(matches!(parse_input("https://spotify.link/abc"), ParsedInput::SpotifyShortLink(_)));
    }

    #[test]
    fn playlist_ids() {
        assert_eq!(youtube_playlist_id("https://www.youtube.com/playlist?list=PLabc_123-x").as_deref(), Some("PLabc_123-x"));
        assert_eq!(youtube_playlist_id("https://music.youtube.com/playlist?list=OLAK5uy_x").as_deref(), Some("OLAK5uy_x"));
        assert_eq!(youtube_playlist_id("youtube.com/watch?v=dQw4w9WgXcQ&list=PL1").as_deref(), Some("PL1"));
        assert_eq!(youtube_playlist_id("https://youtube.com/watch?v=dQw4w9WgXcQ"), None);
        assert_eq!(youtube_playlist_id("https://evil.com/playlist?list=PL1"), None);
    }

    #[test]
    fn text_and_other_urls() {
        assert_eq!(parse_input("  daft   punk - one more time "), ParsedInput::Search("daft punk - one more time".into()));
        assert!(matches!(parse_input("https://soundcloud.com/x/y"), ParsedInput::UnsupportedUrl(_)));
    }
}
