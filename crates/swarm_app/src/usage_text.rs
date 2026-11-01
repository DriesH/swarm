//! The words for usage: window names, reset times, the age of the numbers and why a reading
//! is missing. The sidebar card and the menu bar popover show the same sections.

use swarm_protocol::{SubscriptionUsage, Usage, UsageProblem, UsageWindow};
use swarm_ui::UsageTone;

use crate::usage_pace::{Risk, Span, risk, tick_permille};

const DAY: u64 = 24 * 3_600;

/// Numbers older than this show their age.
const STALE_AFTER: u64 = 10 * 60;

/// Which CLI a subscription belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UsageKind {
    Claude,
    Codex,
}

impl UsageKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
        }
    }

    /// For element ids.
    pub(crate) fn slug(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// One window of a subscription, for example the weekly one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WindowRow {
    /// For example "Session" or "Weekly".
    pub name: String,
    pub percent: u8,
    /// Where even use would be by now, in thousandths of the window.
    pub pace_permille: Option<u16>,
    /// For example "Limit in 3d 21h": the use so far reaches 100% before the reset. Also
    /// "Limit likely reached" and "Limit reached".
    pub risk: Option<String>,
    /// For example "Resets in 2h 6m".
    pub resets: Option<String>,
    /// What VoiceOver reads for the whole window.
    pub spoken: String,
}

impl WindowRow {
    /// How full the bar is, from 0 to 1.
    pub(crate) fn fraction(&self) -> f32 {
        f32::from(self.percent.min(100)) / 100.
    }

    /// A window that is on course to hit the limit before it resets, or has hit it, turns
    /// warn.
    pub(crate) fn tone(&self) -> UsageTone {
        if self.risk.is_some() {
            UsageTone::Warn
        } else {
            UsageTone::Normal
        }
    }

    /// Where the tick sits on the bar, from 0 to 1.
    pub(crate) fn pace(&self) -> Option<f32> {
        self.pace_permille
            .map(|permille| f32::from(permille) / 1_000.)
    }
}

/// The usage of one agent kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageSection {
    pub kind: UsageKind,
    /// For example "Plus". Only set when the CLI reports one.
    pub plan: Option<String>,
    /// "Updated 4m ago" and what VoiceOver reads, once the numbers are old.
    pub age: Option<(String, String)>,
    pub windows: Vec<WindowRow>,
    /// Why the numbers are old or missing: the line to show and what VoiceOver reads.
    pub problem: Option<(String, String)>,
}

impl UsageSection {
    /// What VoiceOver reads for the section, for example "Codex usage, Plus plan".
    pub(crate) fn label(&self) -> String {
        let plan = self.plan.as_ref().map(|plan| format!("{plan} plan"));
        let age = self.age.as_ref().map(|(_, spoken)| spoken.clone());

        let parts = [Some(format!("{} usage", self.kind.name())), plan, age];
        let parts: Vec<String> = parts.into_iter().flatten().collect();

        parts.join(", ")
    }
}

/// Claude first. An agent kind with no windows and no problem to report, or one that is not
/// installed, has no section. `now` is in seconds since the Unix epoch.
pub(crate) fn usage_sections(usage: &Usage, now: u64) -> Vec<UsageSection> {
    [
        (UsageKind::Claude, &usage.claude),
        (UsageKind::Codex, &usage.codex),
    ]
    .into_iter()
    .filter_map(|(kind, reading)| section_of(kind, reading, now))
    .collect()
}

fn section_of(kind: UsageKind, reading: &SubscriptionUsage, now: u64) -> Option<UsageSection> {
    let problem = reading.problem.as_ref().and_then(problem_text);
    if reading.windows.is_empty() && problem.is_none() {
        return None;
    }

    Some(UsageSection {
        kind,
        plan: reading.plan.as_deref().map(capitalized),
        age: reading.read_at.and_then(|read_at| age_text(read_at, now)),
        windows: reading
            .windows
            .iter()
            .map(|window| window_row(kind, window, reading.read_at, now))
            .collect(),
        problem,
    })
}

fn window_row(kind: UsageKind, window: &UsageWindow, read_at: Option<u64>, now: u64) -> WindowRow {
    let name = window_name(window);
    let percent = shown_percent(window, now);

    let risk = risk(window, percent, read_at, now).map(risk_texts);
    let resets_in = window
        .resets_at
        .filter(|at| *at > now)
        .map(|at| Span::left(at - now));

    let spoken = [
        Some(format!(
            "{} {} {percent}% used",
            kind.name(),
            name.to_lowercase()
        )),
        risk.as_ref().map(|(_, spoken)| spoken.clone()),
        resets_in.map(|span| format!("resets in {}", span.spoken())),
    ];
    let spoken: Vec<String> = spoken.into_iter().flatten().collect();

    WindowRow {
        name,
        percent,
        pace_permille: tick_permille(window, now),
        risk: risk.map(|(shown, _)| shown),
        resets: resets_in.map(|span| format!("Resets in {}", span.short())),
        spoken: spoken.join(", "),
    }
}

/// The warning and what VoiceOver reads for it.
fn risk_texts(risk: Risk) -> (String, String) {
    match risk {
        Risk::LimitIn(seconds) => {
            let span = Span::left(seconds);

            (
                format!("Limit in {}", span.short()),
                format!("limit in {}", span.spoken()),
            )
        }
        Risk::LikelyReached => (
            "Limit likely reached".to_owned(),
            "limit likely reached".to_owned(),
        ),
        Risk::Reached => ("Limit reached".to_owned(), "limit reached".to_owned()),
    }
}

/// A window is named by its length: under a day a Session, a week Weekly, thirty days
/// Monthly. Any other length, or none, uses the label.
pub(crate) fn window_name(window: &UsageWindow) -> String {
    let known = match window.length_secs {
        Some(length) if (1..DAY).contains(&length) => Some("Session"),
        Some(length) if length == 7 * DAY => Some("Weekly"),
        Some(length) if length == 30 * DAY => Some("Monthly"),
        _ => None,
    };

    known.map_or_else(|| format!("{} window", window.label), str::to_owned)
}

/// A window whose reset time has passed starts again at 0%, whatever was last read.
pub(crate) fn shown_percent(window: &UsageWindow, now: u64) -> u8 {
    if window.resets_at.is_some_and(|at| at <= now) {
        0
    } else {
        window.used_percent
    }
}

/// "Updated 4m ago" and its spoken form, once the numbers are more than ten minutes old.
fn age_text(read_at: u64, now: u64) -> Option<(String, String)> {
    let age = now.saturating_sub(read_at);
    if age <= STALE_AFTER {
        return None;
    }

    let span = Span::since(age);

    Some((
        format!("Updated {} ago", span.short()),
        format!("updated {} ago", span.spoken()),
    ))
}

/// The line under a section and what VoiceOver reads for a problem. A CLI that is not
/// installed has none.
pub(crate) fn problem_text(problem: &UsageProblem) -> Option<(String, String)> {
    match problem {
        UsageProblem::NotInstalled => None,
        UsageProblem::NotLoggedIn => Some(("Not logged in".to_owned(), "Not logged in".to_owned())),
        UsageProblem::Failed(reason) => Some((
            "Usage unavailable".to_owned(),
            format!("Usage unavailable: {reason}"),
        )),
    }
}

/// True when the sections show something that changes with the clock: a pace, a reset time
/// or the age of the numbers.
pub(crate) fn usage_shows_time(usage: &Usage) -> bool {
    [&usage.claude, &usage.codex].into_iter().any(|reading| {
        reading.read_at.is_some()
            || reading
                .windows
                .iter()
                .any(|window| window.resets_at.is_some())
    })
}

fn capitalized(text: &str) -> String {
    let mut characters = text.chars();

    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;
    const HOUR: u64 = 3_600;

    fn window(
        label: &str,
        used_percent: u8,
        length: Option<u64>,
        resets_in: Option<u64>,
    ) -> UsageWindow {
        UsageWindow {
            label: label.to_owned(),
            used_percent,
            resets_at: resets_in.map(|seconds| NOW + seconds),
            length_secs: length,
        }
    }

    fn reading(plan: Option<&str>, windows: Vec<UsageWindow>) -> SubscriptionUsage {
        SubscriptionUsage {
            windows,
            plan: plan.map(str::to_owned),
            ..SubscriptionUsage::default()
        }
    }

    /// A row for a window that was read just now.
    fn claude_row(window: &UsageWindow) -> WindowRow {
        window_row(UsageKind::Claude, window, Some(NOW), NOW)
    }

    #[test]
    fn windows_are_named_by_their_length() {
        let name = |label: &str, length: Option<u64>| window_name(&window(label, 1, length, None));

        assert_eq!(name("5h", Some(5 * HOUR)), "Session");
        assert_eq!(name("1m", Some(60)), "Session");
        assert_eq!(name("24h", Some(DAY - 1)), "Session");
        assert_eq!(name("7d", Some(7 * DAY)), "Weekly");
        assert_eq!(name("30d", Some(30 * DAY)), "Monthly");
    }

    #[test]
    fn the_length_names_a_window_whatever_its_label_says() {
        let name = window_name(&window("5h", 1, Some(30 * DAY), None));

        assert_eq!(name, "Monthly");
    }

    #[test]
    fn windows_of_any_other_length_or_none_use_their_label() {
        let name = |label: &str, length: Option<u64>| window_name(&window(label, 1, length, None));

        assert_eq!(name("1d", Some(DAY)), "1d window");
        assert_eq!(name("3d", Some(3 * DAY)), "3d window");
        assert_eq!(name("limit", None), "limit window");
        assert_eq!(name("5h", None), "5h window");
        assert_eq!(name("7d", None), "7d window");
        assert_eq!(name("limit", Some(0)), "limit window");
    }

    #[test]
    fn a_window_has_its_percent_pace_risk_and_reset_text() {
        // 61% after 75 of 168 hours, 93 hours before the reset.
        let weekly = window("7d", 61, Some(7 * DAY), Some(93 * HOUR));

        let row = claude_row(&weekly);

        assert_eq!(row.name, "Weekly");
        assert_eq!(row.percent, 61);
        assert_eq!(row.pace_permille, Some(446));
        assert_eq!(row.pace(), Some(0.446));
        assert_eq!(row.risk.as_deref(), Some("Limit in 1d 23h"));
        assert_eq!(row.resets.as_deref(), Some("Resets in 3d 21h"));
    }

    #[test]
    fn a_window_on_pace_has_a_tick_and_no_warning() {
        let session = window("5h", 42, Some(5 * HOUR), Some(126 * 60));

        let row = claude_row(&session);

        assert_eq!(row.pace(), Some(0.58));
        assert_eq!(row.risk, None);
        assert_eq!(row.resets.as_deref(), Some("Resets in 2h 6m"));
    }

    #[test]
    fn a_window_without_a_length_has_no_tick_and_no_warning() {
        let row = claude_row(&window("7d", 90, None, Some(HOUR)));

        assert_eq!(row.pace_permille, None);
        assert_eq!(row.risk, None);
        assert_eq!(row.resets.as_deref(), Some("Resets in 1h"));
    }

    #[test]
    fn a_window_that_has_reset_shows_zero_and_nothing_else() {
        let passed = UsageWindow {
            resets_at: Some(NOW - 5),
            ..window("5h", 97, Some(5 * HOUR), None)
        };

        let row = claude_row(&passed);

        assert_eq!(row.percent, 0);
        assert_eq!(row.pace_permille, None);
        assert_eq!(row.risk, None);
        assert_eq!(row.resets, None);
        assert_eq!(row.spoken, "Claude session 0% used");
    }

    #[test]
    fn a_window_that_has_not_reset_keeps_its_percent() {
        let row = claude_row(&window("5h", 42, None, Some(60)));

        assert_eq!(row.percent, 42);
        assert_eq!(row.resets.as_deref(), Some("Resets in 1m"));
    }

    #[test]
    fn the_spoken_label_has_the_percent_the_risk_and_the_reset() {
        let weekly = window("7d", 61, Some(7 * DAY), Some(93 * HOUR));
        let session = window("5h", 42, Some(5 * HOUR), Some(126 * 60));
        let plain = window("7d", 9, Some(7 * DAY), None);

        assert_eq!(
            claude_row(&weekly).spoken,
            "Claude weekly 61% used, limit in 1 day 23 hours, resets in 3 days 21 hours"
        );
        assert_eq!(
            claude_row(&session).spoken,
            "Claude session 42% used, resets in 2 hours 6 minutes"
        );
        assert_eq!(claude_row(&plain).spoken, "Claude weekly 9% used");
    }

    #[test]
    fn a_weekly_window_warns_once_a_tenth_of_it_has_passed() {
        // 13 hours of 168 are gone in the first case, 19 in the second.
        let young = window("7d", 61, Some(7 * DAY), Some(6 * DAY + 11 * HOUR));
        let older = window("7d", 61, Some(7 * DAY), Some(6 * DAY + 5 * HOUR));

        assert_eq!(
            claude_row(&young).spoken,
            "Claude weekly 61% used, resets in 6 days 11 hours"
        );
        assert_eq!(
            claude_row(&older).spoken,
            "Claude weekly 61% used, limit in 12 hours 9 minutes, resets in 6 days 5 hours"
        );
    }

    #[test]
    fn a_full_window_warns_that_the_limit_is_reached() {
        let session = window("5h", 100, Some(5 * HOUR), Some(HOUR));

        let row = claude_row(&session);

        assert_eq!(row.risk.as_deref(), Some("Limit reached"));
        assert_eq!(row.tone(), UsageTone::Warn);
        assert_eq!(row.resets.as_deref(), Some("Resets in 1h"));
        assert_eq!(
            row.spoken,
            "Claude session 100% used, limit reached, resets in 1 hour"
        );
    }

    #[test]
    fn a_full_window_without_a_length_still_warns() {
        let row = claude_row(&window("5h", 100, None, None));

        assert_eq!(row.risk.as_deref(), Some("Limit reached"));
        assert_eq!(row.pace_permille, None);
    }

    #[test]
    fn a_window_that_was_full_and_has_reset_does_not_warn() {
        let passed = UsageWindow {
            resets_at: Some(NOW - 5),
            ..window("5h", 100, Some(5 * HOUR), None)
        };

        let row = claude_row(&passed);

        assert_eq!(row.risk, None);
        assert_eq!(row.tone(), UsageTone::Normal);
    }

    #[test]
    fn the_limit_is_worked_out_at_the_time_of_the_reading() {
        // 168 hours long, 75 gone. 61% was read 20 hours ago, at hour 55.
        let weekly = window("7d", 61, Some(7 * DAY), Some(93 * HOUR));

        let current = window_row(UsageKind::Claude, &weekly, Some(NOW), NOW);
        let old = window_row(UsageKind::Claude, &weekly, Some(NOW - 20 * HOUR), NOW);

        assert_eq!(current.risk.as_deref(), Some("Limit in 1d 23h"));
        assert_eq!(old.risk.as_deref(), Some("Limit in 15h 10m"));
        assert_eq!(
            old.spoken,
            "Claude weekly 61% used, limit in 15 hours 10 minutes, resets in 3 days 21 hours"
        );
    }

    #[test]
    fn an_old_reading_that_has_likely_hit_the_limit_says_so() {
        // 5 hours long, 4 gone. 30% was read 3 hours ago, at hour 1: 100% came at hour 3h 20m.
        let session = window("5h", 30, Some(5 * HOUR), Some(HOUR));

        let row = window_row(UsageKind::Claude, &session, Some(NOW - 3 * HOUR), NOW);

        assert_eq!(row.risk.as_deref(), Some("Limit likely reached"));
        assert_eq!(row.tone(), UsageTone::Warn);
        assert_eq!(
            row.spoken,
            "Claude session 30% used, limit likely reached, resets in 1 hour"
        );
    }

    #[test]
    fn the_tick_follows_the_clock_not_the_reading() {
        let session = window("5h", 30, Some(5 * HOUR), Some(HOUR));

        let row = window_row(UsageKind::Claude, &session, Some(NOW - 3 * HOUR), NOW);

        assert_eq!(row.pace_permille, Some(800));
    }

    #[test]
    fn a_section_passes_the_time_of_its_reading_to_each_window() {
        let usage = Usage {
            codex: SubscriptionUsage {
                read_at: Some(NOW - 3 * HOUR),
                ..reading(None, vec![window("5h", 30, Some(5 * HOUR), Some(HOUR))])
            },
            ..Usage::default()
        };

        let sections = usage_sections(&usage, NOW);

        assert_eq!(
            sections[0].windows[0].risk.as_deref(),
            Some("Limit likely reached")
        );
    }

    #[test]
    fn the_bar_is_full_at_most() {
        let row = WindowRow {
            percent: 140,
            ..claude_row(&window("5h", 1, None, None))
        };

        assert!((row.fraction() - 1.).abs() < f32::EPSILON);
    }

    #[test]
    fn numbers_older_than_ten_minutes_show_their_age() {
        let age = |seconds: u64| age_text(NOW - seconds, NOW);

        assert_eq!(age(0), None);
        assert_eq!(age(10 * 60), None);
        assert_eq!(
            age(10 * 60 + 1),
            Some((
                "Updated 10m ago".to_owned(),
                "updated 10 minutes ago".to_owned()
            ))
        );
        assert_eq!(
            age(4 * 3_600 + 125).map(|age| age.0),
            Some("Updated 4h 2m ago".to_owned())
        );
    }

    #[test]
    fn a_reading_from_the_future_has_no_age() {
        assert_eq!(age_text(NOW + 500, NOW), None);
    }

    #[test]
    fn a_section_has_the_kind_with_its_plan_and_a_row_for_each_window() {
        let usage = Usage {
            claude: SubscriptionUsage {
                read_at: Some(NOW - 60),
                ..reading(
                    Some("max"),
                    vec![
                        window("5h", 42, Some(5 * HOUR), Some(2 * HOUR)),
                        window("7d", 18, Some(7 * DAY), None),
                    ],
                )
            },
            codex: SubscriptionUsage {
                read_at: Some(NOW - 14 * 60),
                ..reading(None, vec![window("5h", 7, Some(5 * HOUR), Some(600))])
            },
        };

        let sections = usage_sections(&usage, NOW);

        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].kind, UsageKind::Claude);
        assert_eq!(sections[0].plan.as_deref(), Some("Max"));
        assert_eq!(sections[0].age, None);
        assert_eq!(sections[0].windows.len(), 2);
        assert_eq!(sections[0].windows[0].name, "Session");
        assert_eq!(sections[0].windows[1].name, "Weekly");
        assert_eq!(sections[1].kind, UsageKind::Codex);
        assert_eq!(sections[1].plan, None);
        assert_eq!(
            sections[1].age.as_ref().map(|age| age.0.as_str()),
            Some("Updated 14m ago")
        );
    }

    #[test]
    fn the_section_label_has_the_plan_and_the_age() {
        let usage = Usage {
            claude: SubscriptionUsage {
                read_at: Some(NOW - 14 * 60),
                ..reading(Some("max"), vec![window("5h", 42, None, None)])
            },
            codex: reading(None, vec![window("5h", 7, None, None)]),
        };

        let sections = usage_sections(&usage, NOW);

        assert_eq!(
            sections[0].label(),
            "Claude usage, Max plan, updated 14 minutes ago"
        );
        assert_eq!(sections[1].label(), "Codex usage");
    }

    #[test]
    fn a_failed_read_keeps_its_reason_for_the_screen_reader_only() {
        let usage = Usage {
            codex: SubscriptionUsage {
                problem: Some(UsageProblem::Failed("HTTP 500: upstream broke".to_owned())),
                ..reading(None, vec![window("5h", 7, None, None)])
            },
            ..Usage::default()
        };

        let sections = usage_sections(&usage, NOW);

        assert_eq!(sections.len(), 1);
        assert_eq!(
            sections[0].problem,
            Some((
                "Usage unavailable".to_owned(),
                "Usage unavailable: HTTP 500: upstream broke".to_owned()
            ))
        );
        assert_eq!(sections[0].windows.len(), 1);
    }

    #[test]
    fn the_other_problems_have_short_texts() {
        let texts: Vec<_> = [UsageProblem::NotLoggedIn, UsageProblem::NotInstalled]
            .iter()
            .map(|problem| problem_text(problem).map(|text| text.0))
            .collect();

        assert_eq!(texts, [Some("Not logged in".to_owned()), None]);
    }

    #[test]
    fn an_agent_kind_with_nothing_to_show_or_not_installed_has_no_section() {
        let usage = Usage {
            claude: reading(None, Vec::new()),
            codex: SubscriptionUsage {
                problem: Some(UsageProblem::NotInstalled),
                ..SubscriptionUsage::default()
            },
        };

        assert!(usage_sections(&usage, NOW).is_empty());
        assert!(usage_sections(&Usage::default(), NOW).is_empty());
    }

    #[test]
    fn a_problem_before_any_numbers_is_a_section_without_windows() {
        let usage = Usage {
            codex: SubscriptionUsage {
                problem: Some(UsageProblem::NotLoggedIn),
                ..SubscriptionUsage::default()
            },
            ..Usage::default()
        };

        let sections = usage_sections(&usage, NOW);

        assert_eq!(sections.len(), 1);
        assert!(sections[0].windows.is_empty());
        assert_eq!(
            sections[0]
                .problem
                .as_ref()
                .map(|problem| problem.0.as_str()),
            Some("Not logged in")
        );
    }

    #[test]
    fn only_a_reset_time_or_a_reading_time_makes_the_sections_depend_on_the_clock() {
        let timed = Usage {
            claude: reading(None, vec![window("5h", 42, None, Some(60))]),
            ..Usage::default()
        };
        let read = Usage {
            codex: SubscriptionUsage {
                read_at: Some(NOW),
                ..reading(None, vec![window("5h", 42, None, None)])
            },
            ..Usage::default()
        };
        let still = Usage {
            claude: reading(None, vec![window("5h", 42, None, None)]),
            codex: SubscriptionUsage {
                problem: Some(UsageProblem::NotLoggedIn),
                ..SubscriptionUsage::default()
            },
        };

        assert!(usage_shows_time(&timed));
        assert!(usage_shows_time(&read));
        assert!(!usage_shows_time(&still));
        assert!(!usage_shows_time(&Usage::default()));
    }
}
