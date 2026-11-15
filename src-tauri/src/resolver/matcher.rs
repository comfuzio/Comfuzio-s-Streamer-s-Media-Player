//! Picks the YouTube upload that best matches a Spotify track (or a text query).

use crate::resolver::spotify::SpotifyTrack;
use crate::resolver::ytdlp::YtSearchEntry;
use crate::util::text::{token_coverage, tokens};

/// Words that mark an alternate version; penalized unless the target has them too.
const VERSION_MARKERS: &[&str] = &[
    "live", "cover", "remix", "karaoke", "instrumental", "acoustic", "sped", "slowed", "nightcore", "reverb",
    "8d", "reaction", "tutorial", "lesson", "mashup", "extended", "loop", "hour", "edit", "bootleg",
];

fn version_penalty(target: &str, candidate: &str) -> f64 {
    let target_tokens = tokens(target);
    let cand_tokens = tokens(candidate);
    VERSION_MARKERS
        .iter()
        .filter(|m| cand_tokens.contains(**m) && !target_tokens.contains(**m))
        .count() as f64
        * 25.0
}

/// Higher is better. Returns `None` for unusable candidates (live, wildly wrong length).
pub fn score_spotify_candidate(target: &SpotifyTrack, cand: &YtSearchEntry) -> Option<f64> {
    if cand.is_live() {
        return None;
    }
    let title = cand.title.as_deref().unwrap_or_default();
    let channel = cand.channel_name();
    let mut score = 0.0;

    if target.duration_ms > 0 {
        let dur = cand.duration? * 1000.0;
        let diff_s = (dur - target.duration_ms as f64).abs() / 1000.0;
        if diff_s > 45.0 {
            return None;
        }
        score += (40.0 - diff_s * 2.0).max(-20.0);
    }

    let haystack = tokens(&format!("{title} {channel}"));
    score += 40.0 * token_coverage(&tokens(&target.name), &haystack);
    score += 20.0 * token_coverage(&tokens(&target.artist_line()), &haystack);

    let channel_lc = channel.to_lowercase();
    let artist_lc = target.primary_artist().to_lowercase();
    if channel_lc.ends_with(" - topic") {
        score += 15.0; // auto-generated "Artist - Topic" uploads are the studio audio
    }
    if !artist_lc.is_empty() && channel_lc.trim_end_matches(" - topic").replace(' ', "").contains(&artist_lc.replace(' ', "")) {
        score += 10.0;
    }
    let title_lc = title.to_lowercase();
    if title_lc.contains("official audio") {
        score += 5.0;
    } else if title_lc.contains("music video") || title_lc.contains("official video") {
        score -= 3.0; // often has intros/outros
    }

    score -= version_penalty(&format!("{} {}", target.name, target.artist_line()), title);
    Some(score)
}

pub fn best_spotify_match<'a>(target: &SpotifyTrack, candidates: &'a [YtSearchEntry]) -> Option<&'a YtSearchEntry> {
    candidates
        .iter()
        .filter_map(|c| score_spotify_candidate(target, c).map(|s| (c, s)))
        .filter(|(_, s)| *s >= 20.0)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(c, _)| c)
}

/// For raw text searches: keep YouTube's ranking but skip live streams, entries
/// without a duration and alternate versions the viewer didn't ask for.
pub fn best_search_hit<'a>(query: &str, candidates: &'a [YtSearchEntry]) -> Option<&'a YtSearchEntry> {
    let usable = || candidates.iter().filter(|c| !c.is_live() && c.duration.is_some_and(|d| d > 0.0));
    usable()
        .find(|c| version_penalty(query, c.title.as_deref().unwrap_or_default()) == 0.0)
        .or_else(|| usable().next())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, title: &str, channel: &str, dur: f64) -> YtSearchEntry {
        YtSearchEntry {
            id: id.into(),
            title: Some(title.into()),
            duration: Some(dur),
            channel: Some(channel.into()),
            uploader: None,
            live_status: None,
        }
    }

    fn target() -> SpotifyTrack {
        SpotifyTrack {
            id: "x".into(),
            name: "One More Time".into(),
            artists: vec!["Daft Punk".into()],
            album: None,
            artwork_url: None,
            duration_ms: 320_000,
        }
    }

    #[test]
    fn prefers_topic_audio_over_live_and_remix() {
        let cands = vec![
            entry("live0000000", "Daft Punk - One More Time (Live 2007)", "DaftPunkFan", 330.0),
            entry("remix000000", "One More Time (Remix)", "Someone", 321.0),
            entry("topic000000", "One More Time", "Daft Punk - Topic", 320.0),
            entry("video000000", "Daft Punk - One More Time (Official Video)", "Daft Punk", 335.0),
        ];
        assert_eq!(best_spotify_match(&target(), &cands).unwrap().id, "topic000000");
    }

    #[test]
    fn rejects_wrong_lengths() {
        let cands = vec![entry("tenhours000", "One More Time 10 hours", "Loops", 36_000.0)];
        assert!(best_spotify_match(&target(), &cands).is_none());
    }

    #[test]
    fn search_skips_unrequested_versions() {
        let cands = vec![
            entry("cover000000", "Song (Cover)", "c", 200.0),
            entry("orig0000000", "Song", "o", 200.0),
        ];
        assert_eq!(best_search_hit("artist song", &cands).unwrap().id, "orig0000000");
        assert_eq!(best_search_hit("artist song cover", &cands).unwrap().id, "cover000000");
    }
}
