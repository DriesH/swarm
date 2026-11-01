//! The words for usage: waits, reset times and why a reading is old or missing. The
//! sidebar card and the menu bar popover share them.

use swarm_protocol::{UsageProblem, UsageWindow};

/// The line under a row and what VoiceOver reads for a problem. A CLI that is not
/// installed has none.
pub(crate) fn problem_text(problem: &UsageProblem, now: u64) -> Option<(String, String)> {
    let text = match problem {
        UsageProblem::NotInstalled => return None,
        UsageProblem::NotLoggedIn => ("Not logged in".to_owned(), "Not logged in".to_owned()),
        UsageProblem::Unauthorized => ("Login refused".to_owned(), "Login refused".to_owned()),
        UsageProblem::RateLimited { retry_at } => match retry_at.saturating_sub(now) {
            0 => ("Rate limited".to_owned(), "Rate limited".to_owned()),
            left => (
                format!("Rate limited · {}", Wait::new(left).short()),
                format!("Rate limited, retry in {}", Wait::new(left).spoken()),
            ),
        },
        UsageProblem::Failed(reason) => (
            "Usage unavailable".to_owned(),
            format!("Usage unavailable: {reason}"),
        ),
    };

    Some(text)
}

/// A window whose reset time has passed starts again at 0%, whatever was last read.
pub(crate) fn shown_percent(window: &UsageWindow, now: u64) -> u8 {
    if window.resets_at.is_some_and(|at| at <= now) {
        0
    } else {
        window.used_percent
    }
}

/// For example "5 hour window 42%, resets in 2 hours". A window that has reset has no
/// reset note.
pub(crate) fn spoken_window(window: &UsageWindow, now: u64) -> String {
    let name = window_name(&window.label);
    let percent = shown_percent(window, now);

    match window.resets_at {
        Some(at) if at > now => format!(
            "{name} {percent}%, resets in {}",
            Wait::new(at - now).spoken()
        ),
        _ => format!("{name} {percent}%"),
    }
}

/// "5h" reads as "5 hour window". A label in another form is read as it is.
fn window_name(label: &str) -> String {
    for (suffix, unit) in [('m', "minute"), ('h', "hour"), ('d', "day")] {
        let count = label
            .strip_suffix(suffix)
            .filter(|count| !count.is_empty() && count.bytes().all(|byte| byte.is_ascii_digit()));

        if let Some(count) = count {
            return format!("{count} {unit} window");
        }
    }

    format!("{label} window")
}

/// Time left, rounded up: in minutes up to 90 of them, then in hours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wait {
    Minutes(u64),
    Hours(u64),
}

impl Wait {
    pub(crate) fn new(seconds: u64) -> Self {
        let minutes = seconds.div_ceil(60);

        if minutes > 90 {
            Self::Hours(minutes.div_ceil(60))
        } else {
            Self::Minutes(minutes)
        }
    }

    /// For the screen: "24m", "2h" or, past two days, "3d".
    pub(crate) fn short(self) -> String {
        match self {
            Self::Minutes(count) => format!("{count}m"),
            Self::Hours(count) if count > 48 => format!("{}d", count.div_ceil(24)),
            Self::Hours(count) => format!("{count}h"),
        }
    }

    /// For VoiceOver: "24 minutes", "2 hours" or, past two days, "3 days".
    pub(crate) fn spoken(self) -> String {
        let counted = |count: u64, unit: &str| {
            if count == 1 {
                format!("1 {unit}")
            } else {
                format!("{count} {unit}s")
            }
        };

        match self {
            Self::Minutes(count) => counted(count, "minute"),
            Self::Hours(count) if count > 48 => counted(count.div_ceil(24), "day"),
            Self::Hours(count) => counted(count, "hour"),
        }
    }
}

/// What the screen shows under a window's percent, for example "resets in 2h". A window
/// that has reset has none.
pub(crate) fn resets_note(window: &UsageWindow, now: u64) -> Option<String> {
    let at = window.resets_at.filter(|at| *at > now)?;

    Some(format!("resets in {}", Wait::new(at - now).short()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn rate_limited_in(seconds: u64) -> UsageProblem {
        UsageProblem::RateLimited {
            retry_at: NOW + seconds,
        }
    }

    fn note_of(problem: &UsageProblem) -> Option<String> {
        problem_text(problem, NOW).map(|(shown, _)| shown)
    }

    fn window_ending_in(seconds: Option<u64>) -> UsageWindow {
        UsageWindow {
            label: "5h".to_owned(),
            used_percent: 42,
            resets_at: seconds.map(|seconds| NOW + seconds),
        }
    }

    #[test]
    fn a_rate_limit_shows_the_wait_in_minutes_then_hours() {
        let notes: Vec<_> = [24 * 60, 5_400, 5_460, 3 * 3_600]
            .map(|seconds| note_of(&rate_limited_in(seconds)))
            .into();

        assert_eq!(
            notes,
            [
                Some("Rate limited · 24m".to_owned()),
                Some("Rate limited · 90m".to_owned()),
                Some("Rate limited · 2h".to_owned()),
                Some("Rate limited · 3h".to_owned()),
            ]
        );
    }

    #[test]
    fn waits_round_up() {
        let short: Vec<_> = [1, 60, 61, 5_400, 5_401, 7_200, 7_201]
            .map(|seconds| Wait::new(seconds).short())
            .into();
        let spoken: Vec<_> = [1, 61, 7_200, 48 * 3_600, 48 * 3_600 + 1]
            .map(|seconds| Wait::new(seconds).spoken())
            .into();

        assert_eq!(short, ["1m", "1m", "2m", "90m", "2h", "2h", "3h"]);
        assert_eq!(Wait::new(48 * 3_600).short(), "48h");
        assert_eq!(Wait::new(48 * 3_600 + 1).short(), "3d");
        assert_eq!(Wait::new(74 * 3_600).short(), "4d");
        assert_eq!(
            spoken,
            ["1 minute", "2 minutes", "2 hours", "48 hours", "3 days"]
        );
    }

    #[test]
    fn a_rate_limit_that_is_over_shows_no_time() {
        assert_eq!(
            note_of(&UsageProblem::RateLimited { retry_at: NOW - 5 }),
            Some("Rate limited".to_owned())
        );
    }

    #[test]
    fn the_other_problems_have_short_texts() {
        let notes: Vec<_> = [
            UsageProblem::NotLoggedIn,
            UsageProblem::Unauthorized,
            UsageProblem::Failed("HTTP 500: upstream broke".to_owned()),
            UsageProblem::NotInstalled,
        ]
        .iter()
        .map(note_of)
        .collect();

        assert_eq!(
            notes,
            [
                Some("Not logged in".to_owned()),
                Some("Login refused".to_owned()),
                Some("Usage unavailable".to_owned()),
                None,
            ]
        );
    }

    #[test]
    fn windows_with_other_labels_are_read_as_they_are() {
        assert_eq!(window_name("30d"), "30 day window");
        assert_eq!(window_name("90m"), "90 minute window");
        assert_eq!(window_name("weekly"), "weekly window");
        assert_eq!(window_name("h"), "h window");
    }

    #[test]
    fn the_reset_note_counts_the_time_left() {
        let notes: Vec<_> = [Some(2 * 3_600), Some(600), Some(0), None]
            .map(|seconds| resets_note(&window_ending_in(seconds), NOW))
            .into();

        assert_eq!(
            notes,
            [
                Some("resets in 2h".to_owned()),
                Some("resets in 10m".to_owned()),
                None,
                None,
            ]
        );
    }
}
