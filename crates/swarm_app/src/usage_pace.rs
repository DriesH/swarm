//! Pace: where even use would be by now in a usage window, and whether the use so far
//! reaches the limit before the window resets. Plain numbers in and out, with no clock.

use swarm_protocol::UsageWindow;

/// The window must be a tenth gone before its rate says enough to warn on.
const WARN_AFTER_PERMILLE: u128 = 100;

/// A window with a known length and a reset still to come.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pace {
    /// The elapsed share of the window in thousandths: where even use would be by now.
    pub permille: u16,
    /// Seconds until 100% at the average rate since the window started. Only set when that
    /// is before the reset.
    pub limit_in: Option<u64>,
}

/// `None` without a length or a reset, and once the reset has passed: such a window has no
/// tick and no warning.
pub(crate) fn pace(window: &UsageWindow, now: u64) -> Option<Pace> {
    let length = window.length_secs.filter(|length| *length > 0)?;
    let resets_at = window.resets_at.filter(|at| *at > now)?;

    let start = i128::from(resets_at) - i128::from(length);
    let elapsed = (i128::from(now) - start).clamp(0, i128::from(length));
    let elapsed = u128::try_from(elapsed).unwrap_or_default();
    let length = u128::from(length);

    let permille = elapsed * 1_000 / length;
    let used = u128::from(window.used_percent.min(100));

    let limit_in = if permille >= WARN_AFTER_PERMILLE && used > 0 && used < 100 {
        let until_full = ((100 - used) * elapsed).div_ceil(used);

        Some(until_full).filter(|until_full| *until_full < u128::from(resets_at - now))
    } else {
        None
    };

    Some(Pace {
        permille: u16::try_from(permille).unwrap_or(1_000),
        limit_in: limit_in.and_then(|seconds| u64::try_from(seconds).ok()),
    })
}

/// A span of time as screens and VoiceOver say it: "3d 21h" or "3 days 21 hours", "5h 12m"
/// or "5 hours 12 minutes", "40m" or "40 minutes".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    minutes: u64,
}

#[derive(Clone, Copy)]
enum Unit {
    Day,
    Hour,
    Minute,
}

impl Span {
    /// Time still to come, rounded up to a minute, so it is never "0m" while some is left.
    pub(crate) fn left(seconds: u64) -> Self {
        Self {
            minutes: seconds.div_ceil(60).max(1),
        }
    }

    /// Time gone by, rounded down to a minute.
    pub(crate) fn since(seconds: u64) -> Self {
        Self {
            minutes: seconds / 60,
        }
    }

    /// The two largest units that apply, without a unit that is zero.
    fn parts(self) -> Vec<(u64, Unit)> {
        let days = self.minutes / (24 * 60);
        let hours = self.minutes % (24 * 60) / 60;
        let minutes = self.minutes % 60;

        let parts = if days > 0 {
            [(days, Unit::Day), (hours, Unit::Hour)]
        } else if hours > 0 {
            [(hours, Unit::Hour), (minutes, Unit::Minute)]
        } else {
            [(minutes, Unit::Minute), (0, Unit::Minute)]
        };

        let mut parts: Vec<_> = parts.into_iter().filter(|(count, _)| *count > 0).collect();
        if parts.is_empty() {
            parts.push((0, Unit::Minute));
        }

        parts
    }

    /// For the screen: "3d 21h".
    pub(crate) fn short(self) -> String {
        let parts: Vec<String> = self
            .parts()
            .into_iter()
            .map(|(count, unit)| {
                let letter = match unit {
                    Unit::Day => 'd',
                    Unit::Hour => 'h',
                    Unit::Minute => 'm',
                };

                format!("{count}{letter}")
            })
            .collect();

        parts.join(" ")
    }

    /// For VoiceOver: "3 days 21 hours".
    pub(crate) fn spoken(self) -> String {
        let parts: Vec<String> = self
            .parts()
            .into_iter()
            .map(|(count, unit)| {
                let name = match unit {
                    Unit::Day => "day",
                    Unit::Hour => "hour",
                    Unit::Minute => "minute",
                };
                let plural = if count == 1 { "" } else { "s" };

                format!("{count} {name}{plural}")
            })
            .collect();

        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;
    const HOUR: u64 = 3_600;
    const DAY: u64 = 24 * HOUR;

    /// A window of `length` that resets in `resets_in` seconds.
    fn window(used_percent: u8, length: Option<u64>, resets_in: Option<u64>) -> UsageWindow {
        UsageWindow {
            label: "5h".to_owned(),
            used_percent,
            resets_at: resets_in.map(|seconds| NOW + seconds),
            length_secs: length,
        }
    }

    fn pace_of(window: &UsageWindow) -> Option<Pace> {
        pace(window, NOW)
    }

    #[test]
    fn the_marker_is_the_elapsed_share_of_the_window_in_thousandths() {
        let session = window(42, Some(5 * HOUR), Some(126 * 60));
        let weekly = window(61, Some(7 * DAY), Some(7 * DAY - 70 * HOUR));

        assert_eq!(pace_of(&session).map(|pace| pace.permille), Some(580));
        assert_eq!(pace_of(&weekly).map(|pace| pace.permille), Some(416));
    }

    #[test]
    fn a_window_that_just_started_has_its_marker_at_the_start() {
        let started = window(0, Some(5 * HOUR), Some(5 * HOUR));

        assert_eq!(pace_of(&started).map(|pace| pace.permille), Some(0));
    }

    #[test]
    fn a_reset_further_away_than_the_window_is_long_keeps_the_marker_at_the_start() {
        let early = window(10, Some(5 * HOUR), Some(6 * HOUR));

        assert_eq!(pace_of(&early).map(|pace| pace.permille), Some(0));
        assert_eq!(pace_of(&early).and_then(|pace| pace.limit_in), None);
    }

    #[test]
    fn use_that_reaches_the_limit_before_the_reset_is_at_risk() {
        // 61% in 75 hours of 168: 100% comes 47h 57m from now, with 93 hours left.
        let weekly = window(61, Some(7 * DAY), Some(93 * HOUR));

        let limit_in = pace_of(&weekly).and_then(|pace| pace.limit_in);

        assert_eq!(limit_in, Some(47 * HOUR + 57 * 60 + 3));
        assert_eq!(
            limit_in.map(|seconds| Span::left(seconds).short()),
            Some("1d 23h".to_owned())
        );
    }

    #[test]
    fn use_that_lasts_until_the_reset_is_not_at_risk() {
        // 42% in 174 minutes of 300: 100% comes in 240 minutes, with 126 left.
        let session = window(42, Some(5 * HOUR), Some(126 * 60));

        assert_eq!(pace_of(&session).and_then(|pace| pace.limit_in), None);
    }

    #[test]
    fn reaching_the_limit_exactly_at_the_reset_is_not_at_risk() {
        // 50% in 150 minutes of 300: 100% comes in 150 minutes, the minute of the reset.
        let even = window(50, Some(5 * HOUR), Some(150 * 60));

        assert_eq!(pace_of(&even).and_then(|pace| pace.limit_in), None);

        let one_percent_more = window(51, Some(5 * HOUR), Some(150 * 60));

        assert!(pace_of(&one_percent_more).is_some_and(|pace| pace.limit_in.is_some()));
    }

    #[test]
    fn the_rate_counts_from_a_tenth_of_the_window() {
        let length = 5 * HOUR;

        // 29 and 30 minutes of 300 have passed; 99% used reaches the limit soon either way.
        let just_before = window(99, Some(length), Some(length - 29 * 60));
        let at_the_tenth = window(99, Some(length), Some(length - 30 * 60));

        assert_eq!(pace_of(&just_before).and_then(|pace| pace.limit_in), None);
        assert!(pace_of(&at_the_tenth).is_some_and(|pace| pace.limit_in.is_some()));
        assert!(pace_of(&just_before).is_some());
    }

    #[test]
    fn a_window_without_a_length_has_no_pace() {
        let unknown = window(90, None, Some(HOUR));
        let zero = window(90, Some(0), Some(HOUR));

        assert_eq!(pace_of(&unknown), None);
        assert_eq!(pace_of(&zero), None);
    }

    #[test]
    fn a_window_without_a_reset_has_no_pace() {
        assert_eq!(pace_of(&window(90, Some(5 * HOUR), None)), None);
    }

    #[test]
    fn a_window_that_has_reset_has_no_pace() {
        let at_reset = window(90, Some(5 * HOUR), Some(0));
        let passed = UsageWindow {
            resets_at: Some(NOW - 60),
            ..window(90, Some(5 * HOUR), None)
        };

        assert_eq!(pace_of(&at_reset), None);
        assert_eq!(pace_of(&passed), None);
    }

    #[test]
    fn nothing_used_or_everything_used_is_not_at_risk() {
        let none = window(0, Some(5 * HOUR), Some(HOUR));
        let all = window(100, Some(5 * HOUR), Some(HOUR));

        assert_eq!(pace_of(&none).and_then(|pace| pace.limit_in), None);
        assert_eq!(pace_of(&all).and_then(|pace| pace.limit_in), None);
        assert_eq!(pace_of(&all).map(|pace| pace.permille), Some(800));
    }

    #[test]
    fn a_reset_sooner_than_the_length_in_unix_seconds_does_not_underflow() {
        let odd = UsageWindow {
            resets_at: Some(60),
            ..window(50, Some(5 * HOUR), None)
        };

        let pace = pace(&odd, 0);

        assert_eq!(pace.map(|pace| pace.permille), Some(996));
        assert_eq!(pace.and_then(|pace| pace.limit_in), None);
    }

    #[test]
    fn spans_round_time_left_up_to_the_minute() {
        let spans: Vec<_> = [1, 60, 61, 40 * 60, 5 * HOUR + 12 * 60]
            .map(|seconds| Span::left(seconds).short())
            .into();

        assert_eq!(spans, ["1m", "1m", "2m", "40m", "5h 12m"]);
    }

    #[test]
    fn spans_show_the_two_largest_units() {
        let short: Vec<_> = [
            3 * DAY + 21 * HOUR,
            3 * DAY + 21 * HOUR + 59 * 60,
            6 * DAY + 11 * HOUR,
            2 * HOUR + 6 * 60,
            HOUR,
            59 * 60,
        ]
        .map(|seconds| Span::left(seconds).short())
        .into();

        assert_eq!(short, ["3d 21h", "3d 21h", "6d 11h", "2h 6m", "1h", "59m"]);
    }

    #[test]
    fn a_zero_unit_is_left_out() {
        let short: Vec<_> = [2 * DAY, 3 * HOUR, DAY + 5 * 60]
            .map(|seconds| Span::left(seconds).short())
            .into();

        assert_eq!(short, ["2d", "3h", "1d"]);
    }

    #[test]
    fn spans_are_spoken_in_full_words() {
        let spoken: Vec<_> = [
            3 * DAY + 21 * HOUR,
            5 * HOUR + 12 * 60,
            40 * 60,
            DAY + HOUR + 60,
            HOUR + 60,
            30,
        ]
        .map(|seconds| Span::left(seconds).spoken())
        .into();

        assert_eq!(
            spoken,
            [
                "3 days 21 hours",
                "5 hours 12 minutes",
                "40 minutes",
                "1 day 1 hour",
                "1 hour 1 minute",
                "1 minute",
            ]
        );
    }

    #[test]
    fn time_gone_by_rounds_down() {
        assert_eq!(Span::since(4 * 60 + 59).short(), "4m");
        assert_eq!(Span::since(14 * 60).short(), "14m");
        assert_eq!(Span::since(2 * HOUR + 5 * 60).short(), "2h 5m");
    }
}
