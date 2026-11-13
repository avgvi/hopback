use crate::config::Rgb;

pub fn now_unix_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn time_ago(now_secs: i64, ts_secs: i64) -> String {
    let delta = now_secs - ts_secs;
    if delta <= 1 {
        return "now".to_string();
    }
    const MIN: i64 = 60;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;

    let scales: [(i64, &str, &str); 5] = [
        (YEAR, "a year ago", "years"),
        (MONTH, "a month ago", "months"),
        (DAY, "a day ago", "days"),
        (HOUR, "an hour ago", "hours"),
        (MIN, "a minute ago", "minutes"),
    ];
    for (unit, singular, plural) in scales {
        if delta >= unit {
            let n = delta / unit;
            return if n == 1 {
                singular.to_string()
            } else {
                format!("{n} {plural} ago")
            };
        }
    }

    format!("{delta} seconds ago")
}

pub fn age_color(age_hours: f64) -> Rgb {
    let decay_rate = -(1.0_f64 - 0.3).ln() / 24.0;
    let age = age_hours.max(0.0);
    let t = 1.0 - (-decay_rate * age).exp();

    let (r, g, b) = if t < 0.3 {
        let s = t / 0.3;
        (100.0 + s * 100.0, 200.0 - s * 20.0, 50.0 - s * 50.0)
    } else if t < 0.6 {
        let s = (t - 0.3) / 0.3;
        (200.0, 180.0 - s * 80.0, 0.0 + s * 50.0)
    } else {
        let s = ((t - 0.6) / 0.4).min(1.0);
        (200.0 - s * 100.0, 100.0, 50.0 + s * 50.0)
    };
    Rgb(r as u8, g as u8, b as u8)
}

pub fn format_directory(path: &str, home: &str) -> String {
    if path.is_empty() {
        return "n/a".to_string();
    }

    let home = home.strip_suffix('/').unwrap_or(home);
    if !home.is_empty() {
        if path == home {
            return "~".to_string();
        }
        if let Some(rest) = path.strip_prefix(home) {
            if rest.starts_with('/') {
                return format!("~{rest}");
            }
        }
    }
    path.to_string()
}

/// Keeps the end of `s`, which for a path is the informative part, and marks
/// the cut with `…`.
pub fn truncate_left(s: &str, width: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= width {
        return s.to_string();
    }
    if width <= 1 {
        return chars[chars.len() - width..].iter().collect();
    }
    let tail: String = chars[chars.len() - (width - 1)..].iter().collect();
    format!("…{tail}")
}

/// Keeps the start of `s` and marks the cut with `…`.
pub fn truncate_end(s: &str, width: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let head: String = chars[..width - 1].iter().collect();
    format!("{}…", head.trim_end())
}

/// A compact age for a narrow table column: `now`, `5m`, `3h`, `2d`, `6w`,
/// `4mo`, `2y`.
pub fn age_short(now_secs: i64, ts_secs: i64) -> String {
    const MIN: i64 = 60;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;
    let delta = now_secs - ts_secs;
    if delta < MIN {
        "now".to_string()
    } else if delta < HOUR {
        format!("{}m", delta / MIN)
    } else if delta < DAY {
        format!("{}h", delta / HOUR)
    } else if delta < 14 * DAY {
        format!("{}d", delta / DAY)
    } else if delta < 60 * DAY {
        format!("{}w", delta / (7 * DAY))
    } else if delta < 365 * DAY {
        format!("{}mo", delta / (30 * DAY))
    } else {
        format!("{}y", delta / (365 * DAY))
    }
}

/// `1123` as `1,123`.
pub fn group_thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Seconds east of UTC for the local time zone at `ts_secs`, or 0 when it
/// cannot be found.
#[cfg(unix)]
pub fn local_offset_secs(ts_secs: i64) -> i64 {
    let t: libc::time_t = ts_secs as libc::time_t;
    // SAFETY: `tm` is plain data that `localtime_r` fills in; both pointers
    // are valid for the duration of the call.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return 0;
        }
        tm.tm_gmtoff as i64
    }
}

#[cfg(not(unix))]
pub fn local_offset_secs(_ts_secs: i64) -> i64 {
    0
}

/// `2026-11-11 20:00` for a Unix time shifted by `offset_secs`.
pub fn date_time(ts_secs: i64, offset_secs: i64) -> String {
    let stamp = crate::output::rfc3339_utc(ts_secs + offset_secs);
    // `YYYY-MM-DDTHH:MM:SSZ` -> `YYYY-MM-DD HH:MM`
    format!("{} {}", &stamp[..10], &stamp[11..16])
}

/// The session's date and time in the local time zone.
pub fn local_date_time(ts_secs: i64) -> String {
    date_time(ts_secs, local_offset_secs(ts_secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_ago_buckets() {
        let now = 1_000_000_000;
        assert_eq!(time_ago(now, now), "now");
        assert_eq!(time_ago(now, now - 1), "now");
        assert_eq!(time_ago(now, now - 11), "11 seconds ago");
        assert_eq!(time_ago(now, now - 60), "a minute ago");
        assert_eq!(time_ago(now, now - 180), "3 minutes ago");
        assert_eq!(time_ago(now, now - 3600), "an hour ago");
        assert_eq!(time_ago(now, now - 2 * 3600), "2 hours ago");
        assert_eq!(time_ago(now, now - 24 * 3600), "a day ago");
        assert_eq!(time_ago(now, now - 2 * 24 * 3600), "2 days ago");
        assert_eq!(time_ago(now, now - 31 * 24 * 3600), "a month ago");
        assert_eq!(time_ago(now, now - 400 * 24 * 3600), "a year ago");
    }

    #[test]
    fn future_timestamp_reads_as_now() {
        let now = 1_000_000_000;
        assert_eq!(time_ago(now, now + 500), "now");
    }

    #[test]
    fn age_color_fresh_is_greenish_old_is_grayish() {
        let fresh = age_color(0.0);

        assert!(fresh.1 > fresh.0 && fresh.1 > fresh.2);
        let old = age_color(10_000.0);

        assert!(old.0 <= 110 && old.1 <= 110 && old.2 <= 110);
    }

    #[test]
    fn format_directory_collapses_home() {
        assert_eq!(
            format_directory("/Users/x/code/hopback", "/Users/x"),
            "~/code/hopback"
        );
        assert_eq!(format_directory("/var/log", "/Users/x"), "/var/log");
        assert_eq!(format_directory("", "/Users/x"), "n/a");

        assert_eq!(format_directory("/Users/x", "/Users/x"), "~");

        assert_eq!(
            format_directory("/Users/xavier/proj", "/Users/x"),
            "/Users/xavier/proj"
        );

        assert_eq!(format_directory("/Users/x/code", "/Users/x/"), "~/code");
    }

    #[test]
    fn truncate_left_keeps_tail() {
        assert_eq!(truncate_left("~/code/hopback", 100), "~/code/hopback");
        assert_eq!(
            truncate_left("~/code/very/deep/path/project", 12),
            "…ath/project"
        );

        assert!(
            truncate_left("~/code/very/deep/path/project", 12)
                .chars()
                .count()
                <= 12
        );
    }

    #[test]
    fn truncate_end_marks_the_cut() {
        assert_eq!(truncate_end("short", 10), "short");
        assert_eq!(truncate_end("a long session title", 10), "a long se…");
        assert_eq!(truncate_end("word then more", 6), "word…");
        assert_eq!(truncate_end("abc", 0), "");
        assert!(truncate_end("ααααααααα", 4).chars().count() <= 4);
    }

    #[test]
    fn age_short_buckets() {
        let now = 1_000_000_000;
        let day = 86_400;
        assert_eq!(age_short(now, now), "now");
        assert_eq!(age_short(now, now + 50), "now");
        assert_eq!(age_short(now, now - 59), "now");
        assert_eq!(age_short(now, now - 60), "1m");
        assert_eq!(age_short(now, now - 3 * 3600), "3h");
        assert_eq!(age_short(now, now - 2 * day), "2d");
        assert_eq!(age_short(now, now - 13 * day), "13d");
        assert_eq!(age_short(now, now - 21 * day), "3w");
        assert_eq!(age_short(now, now - 90 * day), "3mo");
        assert_eq!(age_short(now, now - 800 * day), "2y");
    }

    #[test]
    fn thousands_are_grouped() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1_123), "1,123");
        assert_eq!(group_thousands(1_000_000), "1,000,000");
    }

    #[test]
    fn date_time_applies_the_offset() {
        assert_eq!(date_time(0, 0), "1970-01-01 00:00");
        assert_eq!(date_time(1_700_000_000, 0), "2023-11-14 22:13");
        assert_eq!(date_time(1_700_000_000, 2 * 3600), "2023-11-15 00:13");
        assert_eq!(date_time(1_700_000_000, -8 * 3600), "2023-11-14 14:13");
        // Whatever the zone, the local form has the same shape.
        assert_eq!(local_date_time(1_700_000_000).len(), 16);
    }

    #[test]
    fn truncate_left_unicode_safe() {
        let t = truncate_left("ααα/βββ/γγγ/δδδ", 8);
        assert!(t.chars().count() <= 8);
    }
}
