use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// The current time as an RFC 3339 UTC string.
pub fn now() -> String {
    fmt_utc(unix_now())
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Formats seconds since the Unix epoch as `2026-11-26T08:30:00Z`.
pub fn fmt_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

/// Parses `2026-11-26T08:30:00Z` back into seconds since the Unix epoch.
pub fn parse_utc(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[19] != b'Z' {
        return None;
    }
    let num = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hh, mm, ss) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400 + hh * 3_600 + mm * 60 + ss)
}

/// A duration the way a person says it: `45s`, `3m10s`, `2h05m`, `4d`.
pub fn ago(secs: i64) -> String {
    let s = secs.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3_600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else if s < 172_800 {
        format!("{}h{:02}m", s / 3_600, s % 3_600 / 60)
    } else {
        format!("{}d", s / 86_400)
    }
}

/// A size in kilobytes, as `14.1 GB` or `320 MB`.
pub fn size(kb: i64) -> String {
    if kb >= 1_048_576 {
        format!("{:.1} GB", kb as f64 / 1_048_576.0)
    } else {
        format!("{} MB", (kb / 1024).max(1))
    }
}

pub fn user_home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        user_home()
    } else if let Some(rest) = path.strip_prefix("~/") {
        user_home().join(rest)
    } else {
        PathBuf::from(path)
    }
}

/// The first seven characters of a commit id.
pub fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_utc() {
        assert_eq!(fmt_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(fmt_utc(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(fmt_utc(951_782_400), "2000-02-29T00:00:00Z");
        for secs in [0, 1_700_000_000, 951_782_400, 1_795_723_400, 4_102_444_799] {
            assert_eq!(parse_utc(&fmt_utc(secs)), Some(secs));
        }
        assert_eq!(parse_utc("yesterday"), None);
        assert_eq!(ago(45), "45s");
        assert_eq!(ago(190), "3m10s");
        assert_eq!(ago(7_500), "2h05m");
        assert_eq!(ago(400_000), "4d");
        assert_eq!(size(14_780_000), "14.1 GB");
        assert_eq!(size(330_000), "322 MB");
    }
}
