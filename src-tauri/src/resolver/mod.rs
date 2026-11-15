//! Turns request text (links or "artist - song") into playable [`Track`]s.

pub mod input;
pub mod matcher;
pub mod spotify;
pub mod ytdlp;

use std::sync::Arc;

use crate::error::{AppError, AppResult, Rejection};
use crate::model::{Track, TrackOrigin};
use crate::persistence::settings::{SearchProvider, SettingsStore};
use crate::util::text::clean_video_title;
use input::{parse_input, ParsedInput};
use spotify::{SpotifyClient, SpotifyTrack};
use ytdlp::{thumbnail_url, watch_url, YtDlp, YtSearchEntry, YtVideo};

pub struct Resolver {
    ytdlp: Arc<YtDlp>,
    spotify: Arc<SpotifyClient>,
    settings: Arc<SettingsStore>,
}

impl Resolver {
    pub fn new(ytdlp: Arc<YtDlp>, spotify: Arc<SpotifyClient>, settings: Arc<SettingsStore>) -> Self {
        Self { ytdlp, spotify, settings }
    }

    pub async fn resolve(&self, raw: &str) -> AppResult<Track> {
        match self.parse(raw).await? {
            ParsedInput::YouTube { video_id } => self.resolve_youtube(&video_id).await,
            ParsedInput::Spotify { track_id } => {
                let sp = self.spotify.track(&track_id).await?;
                self.match_spotify(sp).await
            }
            ParsedInput::Search(q) if q.is_empty() => Err(AppError::Invalid("tell me what to play".into())),
            ParsedInput::Search(q) => self.resolve_search(&q).await,
            other => Err(unsupported(&other)),
        }
    }

    /// Block/unblock identities for `raw` without full resolution when the
    /// input is a direct link; falls back to resolving text searches.
    pub async fn identify(&self, raw: &str) -> AppResult<(Vec<String>, Option<Track>)> {
        match self.parse(raw).await? {
            ParsedInput::YouTube { video_id } => {
                let track = self.resolve_youtube(&video_id).await.ok();
                Ok((vec![Track::youtube_key(&video_id)], track))
            }
            ParsedInput::Spotify { track_id } => {
                let key = Track::spotify_key(&track_id);
                // Also block the matched YouTube upload when we can find it.
                let track = match self.spotify.track(&track_id).await {
                    Ok(sp) => self.match_spotify(sp).await.ok(),
                    Err(_) => None,
                };
                let mut keys = track.as_ref().map(Track::block_keys).unwrap_or_default();
                if !keys.contains(&key) {
                    keys.push(key);
                }
                Ok((keys, track))
            }
            ParsedInput::Search(q) if !q.is_empty() => {
                let track = self.resolve_search(&q).await?;
                Ok((track.block_keys(), Some(track)))
            }
            other => Err(unsupported(&other)),
        }
    }

    async fn parse(&self, raw: &str) -> AppResult<ParsedInput> {
        Ok(match parse_input(raw) {
            ParsedInput::SpotifyShortLink(url) => {
                let expanded = self.spotify.expand_short_link(&url).await?;
                match parse_input(expanded.as_str()) {
                    ParsedInput::SpotifyShortLink(_) => ParsedInput::UnsupportedUrl(raw.into()),
                    other => other,
                }
            }
            other => other,
        })
    }

    async fn resolve_youtube(&self, video_id: &str) -> AppResult<Track> {
        let video = self.ytdlp.video(video_id).await.map_err(|e| match &e {
            AppError::YtDlp(msg) if msg.contains("Video unavailable") || msg.contains("Private video") => {
                AppError::Rejected(Rejection::NotFound { query: watch_url(video_id) })
            }
            _ => e,
        })?;
        if video.is_live() {
            return Err(AppError::Rejected(Rejection::LiveStream));
        }
        Ok(track_from_video(video))
    }

    async fn resolve_search(&self, query: &str) -> AppResult<Track> {
        let use_spotify = self.settings.get().search_provider == SearchProvider::Spotify && self.spotify.is_connected().await;
        if use_spotify {
            match self.spotify.search(query).await {
                Ok(Some(sp)) => return self.match_spotify(sp).await,
                Ok(None) => {}
                Err(err) => tracing::warn!(%err, "Spotify search failed; using YouTube search"),
            }
        }
        let hits = self.ytdlp.search(query, 6).await?;
        let hit = matcher::best_search_hit(query, &hits)
            .ok_or_else(|| AppError::Rejected(Rejection::NotFound { query: query.into() }))?;
        Ok(track_from_search_hit(hit))
    }

    async fn match_spotify(&self, sp: SpotifyTrack) -> AppResult<Track> {
        let query = if sp.artists.is_empty() { sp.name.clone() } else { format!("{} - {}", sp.artist_line(), sp.name) };
        let hits = self.ytdlp.search(&query, 8).await?;
        let best = matcher::best_spotify_match(&sp, &hits)
            .ok_or_else(|| AppError::Rejected(Rejection::NotFound { query: query.clone() }))?;
        let duration_ms = if sp.duration_ms > 0 {
            sp.duration_ms
        } else {
            best.duration.map(|d| (d * 1000.0) as u64).unwrap_or(0)
        };
        Ok(Track {
            key: Track::spotify_key(&sp.id),
            origin: TrackOrigin::Spotify { track_id: sp.id.clone(), matched_video_id: best.id.clone() },
            title: sp.name.clone(),
            artist: sp.artist_line(),
            album: sp.album.clone(),
            artwork_url: sp.artwork_url.clone().or_else(|| Some(thumbnail_url(&best.id))),
            duration_ms,
            source_url: sp.url(),
        })
    }
}

fn unsupported(input: &ParsedInput) -> AppError {
    let msg = match input {
        ParsedInput::SpotifyNonTrack(kind) => format!("Spotify {kind} links aren't supported, send a single track"),
        ParsedInput::YouTubePlaylist => "playlists aren't supported, send a single video".into(),
        _ => "only YouTube and Spotify links are supported".into(),
    };
    AppError::Rejected(Rejection::Unsupported(msg))
}

/// Best-effort artist/title split for a YouTube upload.
fn split_artist_title(title: &str, channel: &str) -> (String, String) {
    let cleaned = clean_video_title(title);
    for sep in [" - ", " – ", " — ", " | "] {
        if let Some((artist, song)) = cleaned.split_once(sep) {
            if !artist.trim().is_empty() && !song.trim().is_empty() {
                return (artist.trim().to_owned(), song.trim().to_owned());
            }
        }
    }
    let channel = channel.trim_end_matches(" - Topic").trim_end_matches("VEVO").trim();
    (channel.to_owned(), cleaned)
}

fn track_from_video(v: YtVideo) -> Track {
    let channel = v.channel.clone().or(v.uploader.clone()).unwrap_or_default();
    let (artist, title) = match (v.artists.as_ref().filter(|a| !a.is_empty()).map(|a| a.join(", ")).or(v.artist.clone()), v.track.clone()) {
        (Some(artist), Some(track)) => (artist, track),
        _ => split_artist_title(&v.title, &channel),
    };
    Track {
        key: Track::youtube_key(&v.id),
        origin: TrackOrigin::YouTube { video_id: v.id.clone() },
        title,
        artist,
        album: v.album,
        artwork_url: v.thumbnail.or_else(|| Some(thumbnail_url(&v.id))),
        duration_ms: v.duration.map(|d| (d * 1000.0) as u64).unwrap_or(0),
        source_url: watch_url(&v.id),
    }
}

fn track_from_search_hit(hit: &YtSearchEntry) -> Track {
    let (artist, title) = split_artist_title(hit.title.as_deref().unwrap_or("Unknown"), hit.channel_name());
    Track {
        key: Track::youtube_key(&hit.id),
        origin: TrackOrigin::YouTube { video_id: hit.id.clone() },
        title,
        artist,
        album: None,
        artwork_url: Some(thumbnail_url(&hit.id)),
        duration_ms: hit.duration.map(|d| (d * 1000.0) as u64).unwrap_or(0),
        source_url: watch_url(&hit.id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_titles() {
        assert_eq!(
            split_artist_title("Rick Astley - Never Gonna Give You Up (Official Music Video)", "Rick Astley"),
            ("Rick Astley".into(), "Never Gonna Give You Up".into())
        );
        assert_eq!(split_artist_title("Blinding Lights", "The Weeknd - Topic"), ("The Weeknd".into(), "Blinding Lights".into()));
    }
}
