//! Pace: where even use would be by now in a usage window, and whether the use so far
//! reaches the limit before the window resets. Plain numbers in and out, with no clock.

use swarm_protocol::UsageWindow;

/// The window must be a tenth gone when it was read before its rate says enough to warn on.
const WARN_AFTER_PERMILLE: u128 = 100;

/// What is wrong with a window that is on course to hit the limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Risk {
    /// 100% comes in this many seconds at the average rate, before the reset.
    LimitIn(u64),
    /// The average rate puts 100% in the past, but the last reading is below it.
    LikelyReached,
    /// The window is at 100%.
    Reached,
}

/// A window with a known length and a reset still to come. `start` is seconds since the Unix
/// epoch and may be negative.
struct Timing {
    start: i128,
    length: u64,
    resets_at: u64,
}

impl Timing {
    /// `None` without a length or a reset, and once the reset has passed: such a window has
    /// no tick and no rate.
    fn of(window: &UsageWindow, now: u64) -> Option<Self> {
        let length = window.length_secs.filter(|length| *length > 0)?;
        let resets_at = window.resets_at.filter(|at| *at > now)?;

        Some(Self {
            start: i128::from(resets_at) - i128::from(length),
            length,
            resets_at,
        })
    }

    /// Seconds of the window gone at `time`, from 0 to the length.
    fn elapsed_at(&self, time: u64) -> u64 {
        let elapsed = (i128::from(time) - self.start).clamp(0, i128::from(self.length));

        u64::try_from(elapsed).unwrap_or_default()
    }
}

/// The elapsed share of the window at `now` in thousandths: where even use would be by now.
/// `None` when the window has no length or no reset still to come.
pub(crate) fn tick_permille(window: &UsageWindow, now: u64) -> Option<u16> {
    let timing = Timing::of(window, now)?;
    let permille = u128::from(timing.elapsed_at(now)) * 1_000 / u128::from(timing.length);

    Some(u16::try_from(permille).unwrap_or(1_000))
}

/// Whether `used` percent of the window is at risk. `read_at` is when the number was read: the
/// rate is the one at that time, so a number that is old does not look current. Without it
/// the number is taken as current.
pub(crate) fn risk(window: &UsageWindow, used: u8, read_at: Option<u64>, now: u64) -> Option<Risk> {
    if used >= 100 {
        return Some(Risk::Reached);
    }
    if used == 0 {
        return None;
    }

    let timing = Timing::of(window, now)?;
    let read_at = read_at.map_or(now, |read_at| read_at.min(now));
    let elapsed = u128::from(timing.elapsed_at(read_at));

    if elapsed * 1_000 / u128::from(timing.length) < WARN_AFTER_PERMILLE {
        return None;
    }

    let full_after = i128::try_from((elapsed * 100).div_ceil(u128::from(used))).ok()?;
    let limit_at = timing.start + full_after;

    if limit_at >= i128::from(timing.resets_at) {
        None
    } else if limit_at > i128::from(now) {
        let limit_in = u64::try_from(limit_at - i128::from(now)).ok()?;

        Some(Risk::LimitIn(limit_in))
    } else {
        Some(Risk::LikelyReached)
    }
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
    fn window(length: Option<u64>, resets_in: Option<u64>) -> UsageWindow {
        UsageWindow {
            label: "5h".to_owned(),
            used_percent: 0,
            resets_at: resets_in.map(|seconds| NOW + seconds),
            length_secs: length,
        }
    }

    /// The risk of a window read just now.
    fn risk_now(used: u8, window: &UsageWindow) -> Option<Risk> {
        risk(window, used, Some(NOW), NOW)
    }

    fn limit_in(risk: Option<Risk>) -> Option<u64> {
        match risk {
            Some(Risk::LimitIn(seconds)) => Some(seconds),
            _ => None,
        }
    }

    #[test]
    fn the_tick_is_the_elapsed_share_of_the_window_in_thousandths() {
        let session = window(Some(5 * HOUR), Some(126 * 60));
        let weekly = window(Some(7 * DAY), Some(7 * DAY - 70 * HOUR));

        assert_eq!(tick_permille(&session, NOW), Some(580));
        assert_eq!(tick_permille(&weekly, NOW), Some(416));
    }

    #[test]
    fn a_window_that_just_started_has_its_tick_at_the_start() {
        let started = window(Some(5 * HOUR), Some(5 * HOUR));

        assert_eq!(tick_permille(&started, NOW), Some(0));
    }

    #[test]
    fn a_reset_further_away_than_the_window_is_long_keeps_the_tick_at_the_start() {
        let early = window(Some(5 * HOUR), Some(6 * HOUR));

        assert_eq!(tick_permille(&early, NOW), Some(0));
        assert_eq!(risk_now(10, &early), None);
    }

    #[test]
    fn use_that_reaches_the_limit_before_the_reset_is_at_risk() {
        // 61% in 75 hours of 168: 100% comes 47h 57m from now, with 93 hours left.
        let weekly = window(Some(7 * DAY), Some(93 * HOUR));

        let seconds = limit_in(risk_now(61, &weekly));

        assert_eq!(seconds, Some(47 * HOUR + 57 * 60 + 3));
        assert_eq!(
            seconds.map(|seconds| Span::left(seconds).short()),
            Some("1d 23h".to_owned())
        );
    }

    #[test]
    fn use_that_lasts_until_the_reset_is_not_at_risk() {
        // 42% in 174 minutes of 300: 100% comes in 240 minutes, with 126 left.
        let session = window(Some(5 * HOUR), Some(126 * 60));

        assert_eq!(risk_now(42, &session), None);
    }

    #[test]
    fn reaching_the_limit_exactly_at_the_reset_is_not_at_risk() {
        // 50% in 150 minutes of 300: 100% comes in 150 minutes, the minute of the reset.
        let even = window(Some(5 * HOUR), Some(150 * 60));

        assert_eq!(risk_now(50, &even), None);
        assert!(limit_in(risk_now(51, &even)).is_some());
    }

    #[test]
    fn the_rate_counts_from_a_tenth_of_the_window() {
        let length = 5 * HOUR;

        // 29 and 30 minutes of 300 have passed; 99% used reaches the limit soon either way.
        let just_before = window(Some(length), Some(length - 29 * 60));
        let at_the_tenth = window(Some(length), Some(length - 30 * 60));

        assert_eq!(risk_now(99, &just_before), None);
        assert!(limit_in(risk_now(99, &at_the_tenth)).is_some());
        assert!(tick_permille(&just_before, NOW).is_some());
    }

    #[test]
    fn nothing_used_is_not_at_risk() {
        let none = window(Some(5 * HOUR), Some(HOUR));

        assert_eq!(risk_now(0, &none), None);
    }

    #[test]
    fn a_full_window_has_reached_the_limit() {
        let all = window(Some(5 * HOUR), Some(HOUR));

        assert_eq!(risk_now(100, &all), Some(Risk::Reached));
        assert_eq!(tick_permille(&all, NOW), Some(800));
    }

    #[test]
    fn a_full_window_has_reached_the_limit_whatever_else_is_known() {
        let without_length = window(None, Some(HOUR));
        let without_reset = window(Some(5 * HOUR), None);
        let young = window(Some(5 * HOUR), Some(5 * HOUR - 60));

        assert_eq!(risk_now(100, &without_length), Some(Risk::Reached));
        assert_eq!(risk_now(100, &without_reset), Some(Risk::Reached));
        assert_eq!(risk_now(100, &young), Some(Risk::Reached));
    }

    #[test]
    fn a_window_without_a_length_has_no_tick_and_no_rate() {
        let unknown = window(None, Some(HOUR));
        let zero = window(Some(0), Some(HOUR));

        assert_eq!(tick_permille(&unknown, NOW), None);
        assert_eq!(tick_permille(&zero, NOW), None);
        assert_eq!(risk_now(90, &unknown), None);
        assert_eq!(risk_now(90, &zero), None);
    }

    #[test]
    fn a_window_without_a_reset_has_no_tick_and_no_rate() {
        let open = window(Some(5 * HOUR), None);

        assert_eq!(tick_permille(&open, NOW), None);
        assert_eq!(risk_now(90, &open), None);
    }

    #[test]
    fn a_window_that_has_reset_has_no_tick_and_no_rate() {
        let at_reset = window(Some(5 * HOUR), Some(0));
        let passed = UsageWindow {
            resets_at: Some(NOW - 60),
            ..window(Some(5 * HOUR), None)
        };

        for window in [&at_reset, &passed] {
            assert_eq!(tick_permille(window, NOW), None);
            assert_eq!(risk_now(90, window), None);
        }
    }

    #[test]
    fn a_reset_sooner_than_the_length_in_unix_seconds_does_not_underflow() {
        let odd = UsageWindow {
            resets_at: Some(60),
            ..window(Some(5 * HOUR), None)
        };

        assert_eq!(tick_permille(&odd, 0), Some(996));
        assert_eq!(risk(&odd, 50, Some(0), 0), None);
    }

    #[test]
    fn an_old_reading_gives_the_rate_it_had_when_it_was_read() {
        // 168 hours long, 75 gone, 93 left. 61% at hour 55 is a faster rate than 61% at hour
        // 75: 100% came at hour 90.2 on that rate, which is 15h 9m from now.
        let weekly = window(Some(7 * DAY), Some(93 * HOUR));

        let old = risk(&weekly, 61, Some(NOW - 20 * HOUR), NOW);
        let current = risk(&weekly, 61, Some(NOW), NOW);

        assert_eq!(limit_in(old), Some(15 * HOUR + 9 * 60 + 51));
        assert_eq!(limit_in(current), Some(47 * HOUR + 57 * 60 + 3));
    }

    #[test]
    fn an_old_reading_that_would_have_reached_the_limit_by_now_likely_has() {
        // 5 hours long, 4 gone. 30% at hour 1 puts 100% at hour 3h 20m, which has passed.
        let session = window(Some(5 * HOUR), Some(HOUR));

        let old = risk(&session, 30, Some(NOW - 3 * HOUR), NOW);

        assert_eq!(old, Some(Risk::LikelyReached));
    }

    #[test]
    fn an_old_reading_that_lasts_until_the_reset_is_not_at_risk() {
        // 20% at hour 2 of 5 gives 100% at hour 10, long after the reset.
        let session = window(Some(5 * HOUR), Some(HOUR));

        assert_eq!(risk(&session, 20, Some(NOW - 2 * HOUR), NOW), None);
    }

    #[test]
    fn a_reading_taken_in_the_first_tenth_of_the_window_does_not_warn_however_old() {
        // 5 hours long, 2 gone, read at 24 minutes (8%): 60% then is a rate too noisy to trust.
        let session = window(Some(5 * HOUR), Some(3 * HOUR));
        let early = risk(&session, 60, Some(NOW - 2 * HOUR + 24 * 60), NOW);

        assert_eq!(early, None);
        assert!(risk(&session, 60, Some(NOW), NOW).is_some());
    }

    #[test]
    fn a_reading_from_the_future_counts_as_current() {
        let weekly = window(Some(7 * DAY), Some(93 * HOUR));

        assert_eq!(
            risk(&weekly, 61, Some(NOW + HOUR), NOW),
            risk(&weekly, 61, Some(NOW), NOW)
        );
    }

    #[test]
    fn a_reading_without_a_time_counts_as_current() {
        let weekly = window(Some(7 * DAY), Some(93 * HOUR));

        assert_eq!(
            risk(&weekly, 61, None, NOW),
            risk(&weekly, 61, Some(NOW), NOW)
        );
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
