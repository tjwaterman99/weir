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
    }
}
