//! Sidebar: projects, the open task's agents and the usage card.

use gpui::{Context, FontWeight, Pixels, Role, Window, div, px};
use swarm_protocol::{SubscriptionUsage, Usage, UsageProblem, UsageWindow};
use swarm_ui::prelude::*;

use crate::actions::ToggleCommandPalette;
use crate::model::{Agent, AgentKind, Project, Task, TaskStatus, Terminal};
use crate::window_drag::drag_region;
use crate::workspace::{DataSource, Workspace};

/// Height of the row with the traffic lights and the sidebar toggle.
pub(crate) const WINDOW_ROW_HEIGHT: Pixels = px(30.);

pub(crate) fn render(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement + use<> {
    let projects: Vec<_> = workspace
        .data
        .projects
        .iter()
        .map(|project| project_row(workspace, project, cx))
        .collect();

    v_flex()
        .id("sidebar")
        .role(Role::Navigation)
        .aria_label("Projects and agents")
        .size_full()
        .gap(px(2.))
        .child(window_row(window, cx))
        .child(logo_row(window, cx))
        .child(
            heading("Projects", 14.).h(px(30.)).child(
                IconButton::new("add-project", IconName::Plus, "Add project")
                    .size(IconButtonSize::Medium)
                    .debug_selector(|| "add-project".to_owned())
                    .on_click(cx.listener(|_, _, window, cx| Workspace::pick_project(window, cx))),
            ),
        )
        .children(projects)
        .children(daemon_status(workspace, cx))
        .children(error_message(workspace, cx))
        .when(!workspace.has_no_projects(), |sidebar| {
            sidebar
                .child(heading("Agents", 18.))
                .children(agent_rows(workspace, cx))
        })
        .child(div().flex_1())
        .children(usage_card(workspace))
}

fn daemon_status(workspace: &Workspace, cx: &mut Context<Workspace>) -> Option<gpui::AnyElement> {
    match &workspace.source {
        DataSource::Connecting => Some(
            h_flex()
                .id("daemon-status")
                .role(Role::Status)
                .aria_label("Connecting")
                .h(px(28.))
                .gap(px(8.))
                .px(px(8.))
                .child(StatusRing::new(AgentStatus::Working))
                .child(Label::new("Connecting").color(Color::Muted))
                .into_any_element(),
        ),
        DataSource::Unavailable { reason } => Some(
            h_flex()
                .id("daemon-status")
                .role(Role::Status)
                .aria_label(format!("Daemon not running: {reason}"))
                .min_h(px(28.))
                .gap(px(8.))
                .px(px(8.))
                .child(
                    div()
                        .flex_1()
                        .child(Label::new("Daemon not running").color(Color::Muted)),
                )
                .child(Button::new("retry-daemon", "Retry").on_click(
                    cx.listener(|workspace, _, window, cx| workspace.retry_daemon(window, cx)),
                ))
                .into_any_element(),
        ),
        DataSource::Sample | DataSource::Live => None,
    }
}

fn error_message(workspace: &Workspace, cx: &mut Context<Workspace>) -> Option<impl IntoElement> {
    let message = workspace.sidebar_error.clone()?;
    let color = Color::Del.hsla(cx);

    Some(
        h_flex()
            .id("sidebar-error")
            .role(Role::Alert)
            .aria_label(message.clone())
            .items_start()
            .gap(px(6.))
            .mx(px(8.))
            .mt(px(6.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(12.))
                    .text_color(color)
                    .child(message),
            )
            .child(
                IconButton::new("dismiss-error", IconName::Close, "Dismiss message").on_click(
                    cx.listener(|workspace, _, _, cx| workspace.dismiss_sidebar_error(cx)),
                ),
            ),
    )
}

fn window_row(window: &mut Window, cx: &mut App) -> impl IntoElement {
    h_flex()
        .h(WINDOW_ROW_HEIGHT)
        .pl(px(6.))
        .child(
            drag_region("sidebar-top-drag", window, cx)
                .flex_1()
                .h_full(),
        )
        .child(
            IconButton::new("toggle-sidebar", IconName::Sidebar, "Toggle sidebar")
                .size(IconButtonSize::Medium)
                .debug_selector(|| "toggle-sidebar".to_owned())
                .on_click(|_, _, _| log::info!("Toggle sidebar is not built yet")),
        )
}

fn logo_row(window: &mut Window, cx: &mut App) -> impl IntoElement {
    let fg = cx.theme().colors().fg;

    let brand = drag_region("sidebar-brand-drag", window, cx)
        .flex_1()
        .h_full()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .size(px(20.))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .bg(fg)
                .child(
                    Icon::new(IconName::Bee)
                        .size(IconSize::Custom(px(13.)))
                        .color(Color::Inverse),
                ),
        )
        .child(
            Label::new("Swarm")
                .size(LabelSize::Large)
                .weight(FontWeight::SEMIBOLD),
        );

    h_flex()
        .h(px(32.))
        .pl(px(6.))
        .mt(px(4.))
        .gap(px(8.))
        .child(brand)
        .child(
            IconButton::new("search", IconName::Search, "Search")
                .size(IconButtonSize::Medium)
                .debug_selector(|| "sidebar-search".to_owned())
                .color(Color::Muted)
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(ToggleCommandPalette), cx);
                }),
        )
}

fn heading(title: &'static str, top_margin: f32) -> gpui::Div {
    h_flex().h(px(28.)).pl(px(8.)).mt(px(top_margin)).child(
        div()
            .flex_1()
            .child(Label::new(title).size(LabelSize::Small).color(Color::Faint)),
    )
}

fn project_row(
    workspace: &Workspace,
    project: &Project,
    cx: &mut Context<Workspace>,
) -> impl IntoElement + use<> {
    let id = project.id;
    let tasks: Vec<&Task> = workspace.data.tasks_of(id).collect();
    let needs_you = tasks.iter().any(|task| task.status == TaskStatus::NeedsYou);
    let working = tasks.iter().any(|task| task.status == TaskStatus::Working);

    let summary = if needs_you {
        ", needs you"
    } else if working {
        ", working"
    } else {
        ""
    };
    let plural = if tasks.len() == 1 { "task" } else { "tasks" };
    let config_error = project
        .config_error
        .as_ref()
        .map(|error| format!(", config error: {error}"))
        .unwrap_or_default();
    let accessible = format!(
        "{}, {} {plural}{summary}{config_error}",
        project.name,
        tasks.len()
    );

    let tile = ProjectTile::new(project.letter, project.tile_color);

    let end = h_flex()
        .gap(px(8.))
        .when(project.config_error.is_some(), |end| {
            end.child(StatusPill::new(AgentStatus::Blocked).label("Config error"))
        })
        .when(needs_you, |end| {
            end.child(StatusPill::new(AgentStatus::Blocked).label("Needs you"))
        })
        .when(!needs_you && working, |end| {
            end.child(StatusRing::new(AgentStatus::Working))
        })
        .child(
            div().min_w(px(10.)).flex().justify_end().child(
                Label::new(tasks.len().to_string())
                    .mono()
                    .size(LabelSize::XSmall)
                    .color(Color::Faint),
            ),
        );

    ListRow::new(("project", id.0 as usize), project.name.clone())
        .debug_selector(move || format!("project-row-{}", id.0))
        .accessible_label(accessible)
        .toggle_state(workspace.active_project == id)
        .start_slot(tile)
        .end_slot(end)
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.select_project(id, cx);
        }))
}

/// One row of the agent list.
enum Row<'a> {
    Agent(&'a Agent, usize),
    Process(&'a Terminal, &'a str, usize),
    User(&'a Terminal),
}

/// Rows in order: each agent, the processes it started, then user terminals.
fn rows(workspace: &Workspace) -> Vec<Row<'_>> {
    let task = workspace.active_task();
    let data = &workspace.data;
    let mut rows = Vec::new();

    let agents = task
        .orchestrator
        .into_iter()
        .chain(task.workers.iter().copied())
        .filter_map(|id| data.agent(id));

    for (index, agent) in agents.enumerate() {
        let indent = usize::from(index > 0);
        rows.push(Row::Agent(agent, indent));

        let owned = task
            .processes
            .iter()
            .filter_map(|id| data.terminal(*id))
            .filter(|terminal| terminal.owner.or(task.orchestrator) == Some(agent.id));
        rows.extend(owned.map(|terminal| Row::Process(terminal, agent.name.as_ref(), indent + 1)));
    }

    rows.extend(
        task.processes
            .iter()
            .filter_map(|id| data.terminal(*id))
            .filter(|terminal| terminal.owner.or(task.orchestrator).is_none())
            .map(|terminal| Row::Process(terminal, "", 0)),
    );

    rows.extend(
        task.user_terminals
            .iter()
            .filter_map(|id| data.terminal(*id))
            .map(Row::User),
    );

    rows
}

fn agent_rows(workspace: &Workspace, cx: &mut Context<Workspace>) -> Vec<gpui::AnyElement> {
    rows(workspace)
        .into_iter()
        .map(|row| match row {
            Row::Agent(agent, indent) => agent_row(workspace, agent, indent, cx),
            Row::Process(terminal, owner, indent) => {
                process_row(workspace, terminal, owner, indent, cx)
            }
            Row::User(terminal) => user_row(workspace, terminal, cx),
        })
        .collect()
}

fn process_row(
    workspace: &Workspace,
    terminal: &Terminal,
    owner: &str,
    indent: usize,
    cx: &mut Context<Workspace>,
) -> gpui::AnyElement {
    let id = terminal.id;
    let accessible = if owner.is_empty() {
        format!("{}, process terminal", terminal.title)
    } else {
        format!("{}, started by {owner}, running", terminal.title)
    };

    ListRow::new(("process", id.0 as usize), terminal.title.clone())
        .accessible_label(accessible)
        .compact(true)
        .indent(indent)
        .toggle_state(workspace.focused_terminal == Some(id))
        .start_slot(
            Icon::new(IconName::Terminal)
                .size(IconSize::Large)
                .color(Color::Muted),
        )
        .end_slot(StatusDot::new(AgentStatus::Working))
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.focus_terminal(id, cx);
        }))
        .into_any_element()
}

fn user_row(
    workspace: &Workspace,
    terminal: &Terminal,
    cx: &mut Context<Workspace>,
) -> gpui::AnyElement {
    let id = terminal.id;

    ListRow::new(("user-terminal", id.0 as usize), terminal.title.clone())
        .accessible_label(format!("{}, your terminal", terminal.title))
        .compact(true)
        .toggle_state(workspace.focused_terminal == Some(id))
        .start_slot(
            Icon::new(IconName::User)
                .size(IconSize::Large)
                .color(Color::Muted),
        )
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.focus_terminal(id, cx);
        }))
        .into_any_element()
}

fn agent_row(
    workspace: &Workspace,
    agent: &Agent,
    indent: usize,
    cx: &mut Context<Workspace>,
) -> gpui::AnyElement {
    let terminal = agent.terminal;
    let (icon, kind) = match agent.kind {
        AgentKind::ClaudeCode => (IconName::ClaudeCode, "Claude Code"),
        AgentKind::Codex => (IconName::Codex, "Codex"),
    };

    ListRow::new(("agent", agent.id.0 as usize), agent.name.clone())
        .accessible_label(format!(
            "{}, {kind}, {}",
            agent.name,
            agent.status.label().to_lowercase()
        ))
        .compact(true)
        .indent(indent)
        .toggle_state(workspace.focused_terminal == Some(terminal))
        .start_slot(Icon::new(icon).size(IconSize::Large))
        .end_slot(StatusDot::new(agent.status))
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.focus_terminal(terminal, cx);
        }))
        .into_any_element()
}

/// One row of the usage card.
#[derive(Debug, PartialEq, Eq)]
struct UsageRow {
    id: &'static str,
    /// For example "Claude · 5h", from the shortest window.
    label: String,
    /// The used share of the shortest window.
    percent: Option<u8>,
    /// Why the numbers are old or missing.
    note: Option<String>,
    /// Every window with its reset time, for VoiceOver.
    spoken: String,
}

fn usage_card(workspace: &Workspace) -> Option<impl IntoElement + use<>> {
    let rows = usage_rows(&workspace.data.usage, workspace.now_ms() / 1_000);
    if rows.is_empty() {
        return None;
    }

    let meters = rows.into_iter().map(|row| {
        let meter = UsageMeter::new(row.id, row.label)
            .accessible_label(row.spoken)
            .debug_selector(move || row.id.to_owned());
        let meter = match row.percent {
            Some(percent) => meter
                .value(format!("{percent}%"))
                .fraction(f32::from(percent) / 100.),
            None => meter,
        };

        match row.note {
            Some(note) => meter.note(note),
            None => meter,
        }
    });

    Some(
        div().debug_selector(|| "usage-card".to_owned()).child(
            Panel::new().accessible("usage", "Subscription usage").row(
                v_flex()
                    .gap(px(9.))
                    .px(px(12.))
                    .py(px(10.))
                    .children(meters),
            ),
        ),
    )
}

/// The rows to show, Claude first. An agent kind with no windows and no problem to
/// report, or one that is not installed, has no row.
fn usage_rows(usage: &Usage, now: u64) -> Vec<UsageRow> {
    [
        ("usage-claude", "Claude", &usage.claude),
        ("usage-codex", "Codex", &usage.codex),
    ]
    .into_iter()
    .filter_map(|(id, name, reading)| usage_row(id, name, reading, now))
    .collect()
}

fn usage_row(
    id: &'static str,
    name: &str,
    reading: &SubscriptionUsage,
    now: u64,
) -> Option<UsageRow> {
    let problem = reading
        .problem
        .as_ref()
        .and_then(|problem| problem_text(problem, now));
    if reading.windows.is_empty() && problem.is_none() {
        return None;
    }

    let first = reading.windows.first();
    let label = match first {
        Some(window) => format!("{name} · {}", window.label),
        None => name.to_owned(),
    };

    let mut spoken = format!("{name} usage: ");
    let windows: Vec<String> = reading
        .windows
        .iter()
        .map(|window| spoken_window(window, now))
        .collect();
    spoken.push_str(&windows.join("; "));

    let note = problem.map(|(shown, said)| {
        if !windows.is_empty() {
            spoken.push_str(". ");
        }
        spoken.push_str(&said);

        shown
    });

    Some(UsageRow {
        id,
        label,
        percent: first.map(|window| shown_percent(window, now)),
        note,
        spoken,
    })
}

/// True when the card shows a time: a reset time that VoiceOver reads, or the wait of a
/// rate limit.
pub(crate) fn usage_shows_time(usage: &Usage) -> bool {
    [&usage.claude, &usage.codex].into_iter().any(|reading| {
        matches!(reading.problem, Some(UsageProblem::RateLimited { .. }))
            || reading
                .windows
                .iter()
                .any(|window| window.resets_at.is_some())
    })
}

/// The line under a row and what VoiceOver reads for a problem. A CLI that is not
/// installed has none.
fn problem_text(problem: &UsageProblem, now: u64) -> Option<(String, String)> {
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
fn shown_percent(window: &UsageWindow, now: u64) -> u8 {
    if window.resets_at.is_some_and(|at| at <= now) {
        0
    } else {
        window.used_percent
    }
}

/// For example "5 hour window 42%, resets in 2 hours". A window that has reset has no
/// reset note.
fn spoken_window(window: &UsageWindow, now: u64) -> String {
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
enum Wait {
    Minutes(u64),
    Hours(u64),
}

impl Wait {
    fn new(seconds: u64) -> Self {
        let minutes = seconds.div_ceil(60);

        if minutes > 90 {
            Self::Hours(minutes.div_ceil(60))
        } else {
            Self::Minutes(minutes)
        }
    }

    /// For the screen: "24m" or "2h".
    fn short(self) -> String {
        match self {
            Self::Minutes(count) => format!("{count}m"),
            Self::Hours(count) => format!("{count}h"),
        }
    }

    /// For VoiceOver: "24 minutes", "2 hours" or, past two days, "3 days".
    fn spoken(self) -> String {
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

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext};

    use super::*;
    use crate::model::{ProjectId, SAMPLE_NOW};

    fn setup(cx: &mut TestAppContext) -> (gpui::Entity<Workspace>, &mut VisualTestContext) {
        cx.update(swarm_ui::theme::init);

        cx.add_window_view(Workspace::with_sample)
    }

    #[gpui::test]
    fn the_sidebar_buttons_end_where_the_project_rows_end(cx: &mut TestAppContext) {
        let (_, cx) = setup(cx);
        let mut right =
            |selector: &'static str| cx.debug_bounds(selector).map(|bounds| bounds.right());

        let row = right("project-row-1");

        assert!(row.is_some());
        for selector in ["toggle-sidebar", "sidebar-search", "add-project"] {
            assert_eq!(right(selector), row, "{selector}");
        }
    }

    #[gpui::test]
    fn clicking_a_project_row_selects_the_project(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let before = workspace.read_with(cx, |workspace, _| workspace.active_project);

        let bounds = cx.debug_bounds("project-row-1");
        assert!(bounds.is_some());
        cx.simulate_click(
            bounds.map_or_else(Default::default, |bounds| bounds.center()),
            gpui::Modifiers::none(),
        );

        let active = workspace.read_with(cx, |workspace, _| workspace.active_project);
        assert_ne!(before, ProjectId(1));
        assert_eq!(active, ProjectId(1));
    }

    #[gpui::test]
    fn processes_nest_under_the_agent_that_started_them(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update_in(cx, Workspace::new_user_terminal);

        let outline: Vec<(String, usize)> = workspace.read_with(cx, |workspace, _| {
            rows(workspace)
                .iter()
                .map(|row| match row {
                    Row::Agent(agent, indent) => (agent.name.to_string(), *indent),
                    Row::Process(terminal, _, indent) => (terminal.title.to_string(), *indent),
                    Row::User(terminal) => (terminal.title.to_string(), 0),
                })
                .collect()
        });

        let expected = [
            ("orchestrator", 0),
            ("term-render", 1),
            ("cargo test", 2),
            ("pty-io", 1),
            ("cargo test -p swarm-term --watch", 2),
            ("cargo build -p swarm-term", 2),
            ("ui-tokens", 1),
            ("pnpm dev", 2),
            ("bench", 1),
            ("zsh", 0),
        ];
        let expected: Vec<(String, usize)> = expected
            .iter()
            .map(|(name, indent)| ((*name).to_owned(), *indent))
            .collect();
        assert_eq!(outline, expected);
    }

    #[gpui::test]
    fn a_process_without_an_owner_nests_under_the_orchestrator(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, _| {
            let task = workspace.active_task();
            let process = task.processes.first().copied();
            if let Some(terminal) = workspace
                .data
                .terminals
                .iter_mut()
                .find(|terminal| Some(terminal.id) == process)
            {
                terminal.owner = None;
            }
        });

        let second = workspace.read_with(cx, |workspace, _| match rows(workspace).get(1) {
            Some(Row::Process(terminal, owner, indent)) => {
                Some((terminal.title.to_string(), (*owner).to_owned(), *indent))
            }
            _ => None,
        });

        assert_eq!(
            second,
            Some(("cargo test".to_owned(), "orchestrator".to_owned(), 1))
        );
    }

    const NOW: u64 = 1_800_000_000;

    fn window(label: &str, used_percent: u8, resets_at: Option<u64>) -> UsageWindow {
        UsageWindow {
            label: label.to_owned(),
            used_percent,
            resets_at,
        }
    }

    fn reading(windows: Vec<UsageWindow>, problem: Option<UsageProblem>) -> SubscriptionUsage {
        SubscriptionUsage {
            windows,
            problem,
            ..Default::default()
        }
    }

    fn rate_limited_in(seconds: u64) -> UsageProblem {
        UsageProblem::RateLimited {
            retry_at: NOW + seconds,
        }
    }

    fn note_of(problem: &UsageProblem) -> Option<String> {
        problem_text(problem, NOW).map(|(shown, _)| shown)
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
    fn a_failed_read_keeps_its_reason_for_the_screen_reader_only() {
        let problem = UsageProblem::Failed("HTTP 500: upstream broke".to_owned());
        let usage = Usage {
            codex: reading(Vec::new(), Some(problem)),
            ..Default::default()
        };

        let rows = usage_rows(&usage, NOW);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].note.as_deref(), Some("Usage unavailable"));
        assert_eq!(
            rows[0].spoken,
            "Codex usage: Usage unavailable: HTTP 500: upstream broke"
        );
    }

    #[test]
    fn the_card_has_no_rows_when_there_is_nothing_to_show() {
        assert!(usage_rows(&Usage::default(), NOW).is_empty());
    }

    #[test]
    fn not_installed_hides_an_agent_kind() {
        let usage = Usage {
            claude: reading(vec![window("5h", 42, None)], None),
            codex: reading(Vec::new(), Some(UsageProblem::NotInstalled)),
        };

        let ids: Vec<_> = usage_rows(&usage, NOW).iter().map(|row| row.id).collect();

        assert_eq!(ids, ["usage-claude"]);
    }

    #[test]
    fn a_row_shows_the_shortest_window_and_its_percent() {
        let usage = Usage {
            claude: reading(vec![window("5h", 42, None), window("7d", 18, None)], None),
            ..Default::default()
        };

        let rows = usage_rows(&usage, NOW);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "Claude · 5h");
        assert_eq!(rows[0].percent, Some(42));
        assert_eq!(rows[0].note, None);
    }

    #[test]
    fn a_problem_before_any_numbers_shows_a_row_without_a_bar() {
        let usage = Usage {
            codex: reading(Vec::new(), Some(UsageProblem::NotLoggedIn)),
            ..Default::default()
        };

        let rows = usage_rows(&usage, NOW);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "Codex");
        assert_eq!(rows[0].percent, None);
        assert_eq!(rows[0].note.as_deref(), Some("Not logged in"));
    }

    #[test]
    fn a_problem_keeps_the_old_numbers_on_screen() {
        let usage = Usage {
            claude: reading(vec![window("5h", 42, None)], Some(rate_limited_in(24 * 60))),
            ..Default::default()
        };

        let rows = usage_rows(&usage, NOW);

        assert_eq!(rows[0].percent, Some(42));
        assert_eq!(rows[0].note.as_deref(), Some("Rate limited · 24m"));
    }

    #[test]
    fn the_screen_reader_label_lists_every_window_with_its_reset_time() {
        let usage = Usage {
            claude: reading(
                vec![
                    window("5h", 42, Some(NOW + 2 * 3_600)),
                    window("7d", 18, None),
                ],
                None,
            ),
            ..Default::default()
        };

        let rows = usage_rows(&usage, NOW);

        assert_eq!(
            rows[0].spoken,
            "Claude usage: 5 hour window 42%, resets in 2 hours; 7 day window 18%"
        );
    }

    #[test]
    fn the_screen_reader_label_says_what_went_wrong_after_the_windows() {
        let usage = Usage {
            claude: reading(
                vec![window("5h", 42, Some(NOW + 600))],
                Some(rate_limited_in(24 * 60)),
            ),
            ..Default::default()
        };

        let rows = usage_rows(&usage, NOW);

        assert_eq!(
            rows[0].spoken,
            "Claude usage: 5 hour window 42%, resets in 10 minutes. Rate limited, retry in 24 minutes"
        );
    }

    #[test]
    fn a_window_that_has_reset_shows_zero_and_no_reset_note() {
        let usage = Usage {
            claude: reading(
                vec![
                    window("5h", 42, Some(NOW - 1)),
                    window("7d", 18, Some(NOW + 3_600)),
                ],
                None,
            ),
            codex: reading(vec![window("5h", 90, Some(NOW))], None),
        };

        let rows = usage_rows(&usage, NOW);

        assert_eq!(rows[0].percent, Some(0));
        assert_eq!(
            rows[0].spoken,
            "Claude usage: 5 hour window 0%; 7 day window 18%, resets in 60 minutes"
        );
        assert_eq!(
            rows[1].percent,
            Some(0),
            "a reset at this very second counts"
        );
        assert_eq!(rows[1].spoken, "Codex usage: 5 hour window 0%");
    }

    #[test]
    fn a_window_that_has_not_reset_keeps_its_percent() {
        let usage = Usage {
            claude: reading(vec![window("5h", 42, Some(NOW + 1))], None),
            ..Default::default()
        };

        let rows = usage_rows(&usage, NOW);

        assert_eq!(rows[0].percent, Some(42));
        assert_eq!(
            rows[0].spoken,
            "Claude usage: 5 hour window 42%, resets in 1 minute"
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
    fn only_a_reset_time_or_a_rate_limit_makes_the_card_depend_on_the_clock() {
        let timed = Usage {
            claude: reading(vec![window("5h", 42, Some(NOW))], None),
            ..Default::default()
        };
        let limited = Usage {
            codex: reading(Vec::new(), Some(rate_limited_in(60))),
            ..Default::default()
        };
        let still = Usage {
            claude: reading(
                vec![window("5h", 42, None)],
                Some(UsageProblem::NotLoggedIn),
            ),
            ..Default::default()
        };

        assert!(usage_shows_time(&timed));
        assert!(usage_shows_time(&limited));
        assert!(!usage_shows_time(&still));
        assert!(!usage_shows_time(&Usage::default()));
    }

    fn redraw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn card_rows(
        workspace: &gpui::Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> (bool, bool, bool) {
        workspace.update(cx, |_, cx| cx.notify());
        redraw(cx);

        (
            cx.debug_bounds("usage-card").is_some(),
            cx.debug_bounds("usage-claude").is_some(),
            cx.debug_bounds("usage-codex").is_some(),
        )
    }

    #[gpui::test]
    fn the_sample_card_shows_both_agent_kinds(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        assert_eq!(card_rows(&workspace, cx), (true, true, true));

        let rows = workspace.read_with(cx, |workspace, _| {
            usage_rows(&workspace.data.usage, SAMPLE_NOW / 1_000)
                .into_iter()
                .map(|row| (row.label, row.percent))
                .collect::<Vec<_>>()
        });
        assert_eq!(
            rows,
            [
                ("Claude · 5h".to_owned(), Some(42)),
                ("Codex · 5h".to_owned(), Some(18)),
            ]
        );
    }

    #[gpui::test]
    fn the_card_hides_when_nothing_is_left_to_show(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        workspace.update(cx, |workspace, cx| {
            workspace.usage_changed(Usage::default(), cx);
        });

        assert_eq!(card_rows(&workspace, cx), (false, false, false));
    }

    #[gpui::test]
    fn not_installed_hides_only_that_row_of_the_card(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        workspace.update(cx, |workspace, cx| {
            let mut usage = workspace.data.usage.clone();
            usage.codex = reading(Vec::new(), Some(UsageProblem::NotInstalled));
            workspace.usage_changed(usage, cx);
        });

        assert_eq!(card_rows(&workspace, cx), (true, true, false));
    }

    #[gpui::test]
    fn an_event_replaces_the_shown_numbers(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        workspace.update(cx, |workspace, cx| {
            let usage = Usage {
                claude: reading(vec![window("5h", 64, None)], Some(rate_limited_in(600))),
                codex: reading(vec![window("5h", 7, None)], None),
            };
            workspace.usage_changed(usage, cx);
        });

        let rows = workspace.read_with(cx, |workspace, _| {
            usage_rows(&workspace.data.usage, SAMPLE_NOW / 1_000)
                .into_iter()
                .map(|row| (row.percent, row.note))
                .collect::<Vec<_>>()
        });
        assert_eq!(
            rows,
            [
                (Some(64), Some("Rate limited · 10m".to_owned())),
                (Some(7), None),
            ]
        );
    }
}
