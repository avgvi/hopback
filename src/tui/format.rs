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

pub fn truncate_left(s: &str, width: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= width {
        return s.to_string();
    }
    if width <= 3 {
        return chars[chars.len() - width..].iter().collect();
    }
    let tail: String = chars[chars.len() - (width - 3)..].iter().collect();
    format!("...{tail}")
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
            format_directory("/Users/x/code/aiz", "/Users/x"),
            "~/code/aiz"
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
        assert_eq!(truncate_left("~/code/aiz", 100), "~/code/aiz");
        assert_eq!(
            truncate_left("~/code/very/deep/path/project", 12),
            "...h/project"
        );

        assert!(
            truncate_left("~/code/very/deep/path/project", 12)
                .chars()
                .count()
                <= 12
        );
    }

    #[test]
    fn truncate_left_unicode_safe() {
        let t = truncate_left("ααα/βββ/γγγ/δδδ", 8);
        assert!(t.chars().count() <= 8);
    }
}
