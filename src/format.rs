//! Small human-facing formatters shared by the CLI (`paclens status`) and the
//! TUI dashboard. Pure and directly unit-tested, so neither caller re-implements
//! byte/time formatting (principle P5: one source of truth).

use chrono::{DateTime, Utc};

/// Format a byte count as a human-readable size (binary units).
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// A coarse "N minutes ago" rendering of a past timestamp.
pub fn relative_time(when: DateTime<Utc>) -> String {
    let secs = Utc::now().signed_duration_since(when).num_seconds().max(0);
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} minutes ago", secs / 60),
        3600..=86399 => format!("{} hours ago", secs / 3600),
        _ => format!("{} days ago", secs / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn human_bytes_picks_the_right_unit() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.00 KiB");
        assert_eq!(human_bytes(1536), "1.50 KiB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.00 MiB");
        assert_eq!(human_bytes(1024u64.pow(3)), "1.00 GiB");
    }

    #[test]
    fn relative_time_buckets_by_magnitude() {
        let now = Utc::now();
        assert_eq!(relative_time(now), "just now");
        assert_eq!(relative_time(now - Duration::minutes(5)), "5 minutes ago");
        assert_eq!(relative_time(now - Duration::hours(3)), "3 hours ago");
        assert_eq!(relative_time(now - Duration::days(2)), "2 days ago");
    }
}
