//! Typical hook durations, measured from pacman.log history.

use std::collections::HashMap;

use chrono::{DateTime, FixedOffset};

use super::log_line::{LogEvent, classify_log};

/// Fewer samples than this and a median is noise, so the hook gets none.
const MIN_RUNS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Median {
    pub secs: u64,
    pub runs: usize,
}

/// Median duration per hook file name, over every run in `log`.
///
/// A sample runs from the hook's `running` line to the next timestamped line
/// that is not `[ALPM-SCRIPTLET]`: scriptlet output belongs to the hook
/// (`90-mkinitcpio-install` prints at once and runs ~23 s). A hook with no
/// line after it gives no sample, and neither does the last hook of a run:
/// its next line is the next run's `[PACMAN] Running`, which can be hours
/// later. A sample that goes back in time is dropped too. Only
/// hooks with at least `MIN_RUNS` samples are kept. With an even count the
/// median is the mean of the two middle samples, rounded down.
pub fn hook_medians(log: &str) -> HashMap<String, Median> {
    let mut samples: HashMap<String, Vec<u64>> = HashMap::new();
    let mut open: Option<(String, DateTime<FixedOffset>)> = None;
    for (at, event) in log.lines().filter_map(classify_log) {
        if matches!(event, LogEvent::Scriptlet(_)) {
            continue;
        }
        if let Some((hook, start)) = open.take()
            && !matches!(event, LogEvent::RunStart(_))
            && let Ok(secs) = u64::try_from((at - start).num_seconds())
        {
            samples.entry(hook).or_default().push(secs);
        }
        if let LogEvent::HookRunning(hook) = event {
            open = Some((hook, at));
        }
    }
    samples
        .into_iter()
        .filter(|(_, s)| s.len() >= MIN_RUNS)
        .map(|(hook, mut s)| {
            s.sort_unstable();
            let n = s.len();
            let secs = if n % 2 == 1 {
                s[n / 2]
            } else {
                (s[n / 2 - 1] + s[n / 2]) / 2
            };
            (hook, Median { secs, runs: n })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(t: &str, name: &str) -> String {
        format!("[2026-10-01T10:{t}+0200] [ALPM] running '{name}'...\n")
    }

    fn line(t: &str, rest: &str) -> String {
        format!("[2026-10-01T10:{t}+0200] {rest}\n")
    }

    #[test]
    fn last_hook_of_run_is_not_timed_to_next_run() {
        let mut log = String::new();
        for m in ["00", "10", "20"] {
            log += &hook(&format!("{m}:00"), "zz.hook");
            log += &line(&format!("{m}:01"), "[ALPM-SCRIPTLET] ==> root: 1008");
            log += &line(&format!("{m}:50"), "[PACMAN] Running 'pacman -Syu'");
        }
        assert!(hook_medians(&log).is_empty());
    }

    #[test]
    fn three_runs_give_middle_value() {
        let log = [
            hook("00:00", "x.hook"),
            line("00:10", "[ALPM] transaction completed"),
            hook("01:00", "x.hook"),
            line("01:20", "[ALPM] transaction completed"),
            hook("02:00", "x.hook"),
            line("02:30", "[ALPM] transaction completed"),
        ]
        .concat();
        let m = hook_medians(&log);
        assert_eq!(m.get("x.hook"), Some(&Median { secs: 20, runs: 3 }));
    }

    #[test]
    fn hook_seen_twice_is_absent() {
        let log = [
            hook("00:00", "x.hook"),
            line("00:10", "[ALPM] transaction completed"),
            hook("01:00", "x.hook"),
            line("01:20", "[ALPM] transaction completed"),
        ]
        .concat();
        assert!(hook_medians(&log).is_empty());
    }

    #[test]
    fn scriptlet_output_belongs_to_hook() {
        let mut log = String::new();
        for m in ["00", "01", "02"] {
            log += &hook(&format!("{m}:00"), "x.hook");
            log += &line(&format!("{m}:00"), "[ALPM-SCRIPTLET] ==> Building image");
            log += &line(&format!("{m}:05"), "[ALPM-SCRIPTLET] ==> done");
            log += &hook(&format!("{m}:20"), "y.hook");
        }
        let m = hook_medians(&log);
        assert_eq!(m.get("x.hook"), Some(&Median { secs: 20, runs: 3 }));
    }

    #[test]
    fn hook_on_last_line_is_ignored() {
        let log = [
            hook("00:00", "x.hook"),
            line("00:10", "[ALPM] transaction completed"),
            hook("01:00", "x.hook"),
            line("01:10", "[ALPM] transaction completed"),
            hook("02:00", "x.hook"),
            line("02:10", "[ALPM] transaction completed"),
            hook("03:00", "x.hook"),
        ]
        .concat();
        let m = hook_medians(&log);
        assert_eq!(m.get("x.hook"), Some(&Median { secs: 10, runs: 3 }));
    }

    #[test]
    fn even_count_takes_mean_of_middles_rounded_down() {
        let mut log = String::new();
        for (m, end) in [("00", "10"), ("01", "11"), ("02", "20"), ("03", "30")] {
            log += &hook(&format!("{m}:00"), "x.hook");
            log += &line(&format!("{m}:{end}"), "[ALPM] transaction completed");
        }
        let m = hook_medians(&log);
        assert_eq!(m.get("x.hook"), Some(&Median { secs: 15, runs: 4 }));
    }
}
