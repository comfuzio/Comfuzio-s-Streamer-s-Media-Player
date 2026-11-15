//! Small text helpers for metadata cleanup and fuzzy matching.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

/// Decodes the handful of HTML entities that appear in `<meta>` / `<title>` tags.
pub fn decode_html_entities(s: &str) -> String {
    static NUMERIC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"&#(x?[0-9a-fA-F]+);").unwrap());
    let s = NUMERIC.replace_all(s, |caps: &regex::Captures| {
        let raw = &caps[1];
        let code = if let Some(hex) = raw.strip_prefix('x') {
            u32::from_str_radix(hex, 16).ok()
        } else {
            raw.parse::<u32>().ok()
        };
        code.and_then(char::from_u32).map(String::from).unwrap_or_default()
    });
    s.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// Strips decorations YouTube uploaders add to titles: "(Official Video)", "[4K]", "| Lyrics" ...
pub fn clean_video_title(title: &str) -> String {
    static NOISE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)\s*[\(\[]\s*(official\s*)?(music\s*video|video|audio|lyrics?|lyric\s*video|visualizer|visualiser|hd|hq|4k|remastered(\s*\d{4})?|explicit|clean)\s*[\)\]]",
        )
        .unwrap()
    });
    static TRAILING: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\s*[|•]\s*(official.*|lyrics?.*|free download.*)$").unwrap());
    let t = NOISE.replace_all(title, "");
    let t = TRAILING.replace_all(&t, "");
    t.trim().to_string()
}

/// Lowercase alphanumeric tokens, used for similarity scoring.
pub fn tokens(s: &str) -> HashSet<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty() && !matches!(*t, "the" | "a" | "an" | "feat" | "ft" | "and" | "x"))
        .map(str::to_owned)
        .collect()
}

/// Fraction of `wanted` tokens present in `candidate` (0.0 ..= 1.0).
pub fn token_coverage(wanted: &HashSet<String>, candidate: &HashSet<String>) -> f64 {
    if wanted.is_empty() {
        return 0.0;
    }
    wanted.intersection(candidate).count() as f64 / wanted.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_entities() {
        assert_eq!(decode_html_entities("Tom &amp; Jerry&#39;s &#x2014; x"), "Tom & Jerry's \u{2014} x");
    }

    #[test]
    fn cleans_titles() {
        assert_eq!(clean_video_title("Rick Astley - Never Gonna Give You Up (Official Music Video)"), "Rick Astley - Never Gonna Give You Up");
        assert_eq!(clean_video_title("Song [4K] | Official Lyrics"), "Song");
    }

    #[test]
    fn coverage() {
        let w = tokens("Rick Astley Never Gonna Give You Up");
        let c = tokens("Never Gonna Give You Up - Rick Astley (Official Video)");
        assert!((token_coverage(&w, &c) - 1.0).abs() < f64::EPSILON);
    }
}
