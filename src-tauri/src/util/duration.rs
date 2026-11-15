//! Human duration parsing/formatting used by `!<ns>maxlength` and chat replies.

use std::time::Duration;

use crate::error::{AppError, AppResult};

pub const MAX_LENGTH_FLOOR: Duration = Duration::from_secs(30);
pub const MAX_LENGTH_CEILING: Duration = Duration::from_secs(60 * 60);

/// Parses durations such as `30s`, `5m`, `1h`, `1h30m`, `2m 15s`, `4:30`, `1:02:03`.
/// A bare number is interpreted as **minutes** (`!mbmaxlength 8` => 8 minutes).
pub fn parse_human_duration(input: &str) -> AppResult<Duration> {
    let s: String = input.trim().to_ascii_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    if s.is_empty() {
        return Err(AppError::Invalid("duration is empty".into()));
    }
    let invalid = || AppError::Invalid(format!("invalid duration \"{}\" (try 30s, 5m, 1h)", input.trim()));

    if s.contains(':') {
        let parts: Vec<u64> = s
            .split(':')
            .map(|p| p.parse::<u64>().map_err(|_| invalid()))
            .collect::<AppResult<_>>()?;
        let secs = match parts.as_slice() {
            [m, sec] if *sec < 60 => m * 60 + sec,
            [h, m, sec] if *m < 60 && *sec < 60 => h * 3600 + m * 60 + sec,
            _ => return Err(invalid()),
        };
        return Ok(Duration::from_secs(secs));
    }

    if s.chars().all(|c| c.is_ascii_digit()) {
        let minutes: u64 = s.parse().map_err(|_| invalid())?;
        return Ok(Duration::from_secs(minutes.saturating_mul(60)));
    }

    let mut total: u64 = 0;
    let mut rest = s.as_str();
    while !rest.is_empty() {
        let digits = rest.find(|c: char| !c.is_ascii_digit()).ok_or_else(invalid)?;
        if digits == 0 {
            return Err(invalid());
        }
        let value: u64 = rest[..digits].parse().map_err(|_| invalid())?;
        rest = &rest[digits..];
        let unit_len = rest.find(|c: char| c.is_ascii_digit()).unwrap_or(rest.len());
        let multiplier = match &rest[..unit_len] {
            "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            "s" | "sec" | "secs" | "second" | "seconds" => 1,
            _ => return Err(invalid()),
        };
        total = total.saturating_add(value.saturating_mul(multiplier));
        rest = &rest[unit_len..];
    }
    Ok(Duration::from_secs(total))
}

/// Clamps a requested max song length to [30 s, 1 h].
pub fn clamp_max_length(d: Duration) -> Duration {
    d.clamp(MAX_LENGTH_FLOOR, MAX_LENGTH_CEILING)
}

/// `m:ss` or `h:mm:ss`.
pub fn format_clock_ms(ms: u64) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// `1h 5m`, `4m 30s`, `45s`.
pub fn format_human_ms(ms: u64) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    match (h, m, s) {
        (0, 0, s) => format!("{s}s"),
        (0, m, 0) => format!("{m}m"),
        (0, m, s) => format!("{m}m {s}s"),
        (h, 0, _) => format!("{h}h"),
        (h, m, _) => format!("{h}h {m}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: &str) -> u64 {
        parse_human_duration(s).unwrap().as_secs()
    }

    #[test]
    fn parses_units() {
        assert_eq!(secs("30s"), 30);
        assert_eq!(secs("5m"), 300);
        assert_eq!(secs("1h"), 3600);
        assert_eq!(secs("1h30m"), 5400);
        assert_eq!(secs("2m 15s"), 135);
        assert_eq!(secs("10min"), 600);
        assert_eq!(secs(" 90 SEC "), 90);
    }

    #[test]
    fn parses_clock_and_bare_minutes() {
        assert_eq!(secs("4:30"), 270);
        assert_eq!(secs("1:02:03"), 3723);
        assert_eq!(secs("8"), 480);
    }

    #[test]
    fn rejects_garbage() {
        for bad in ["", "abc", "5x", "m5", "1:75", "1:2:3:4", "-5m"] {
            assert!(parse_human_duration(bad).is_err(), "{bad} should fail");
        }
    }

    #[test]
    fn clamps() {
        assert_eq!(clamp_max_length(Duration::from_secs(5)).as_secs(), 30);
        assert_eq!(clamp_max_length(Duration::from_secs(7200)).as_secs(), 3600);
        assert_eq!(clamp_max_length(Duration::from_secs(600)).as_secs(), 600);
    }

    #[test]
    fn formats() {
        assert_eq!(format_clock_ms(65_000), "1:05");
        assert_eq!(format_clock_ms(3_723_000), "1:02:03");
        assert_eq!(format_human_ms(270_000), "4m 30s");
        assert_eq!(format_human_ms(3_900_000), "1h 5m");
        assert_eq!(format_human_ms(45_000), "45s");
    }
}
