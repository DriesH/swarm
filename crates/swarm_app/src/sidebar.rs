//! Sidebar: projects, the open task's agents and the usage card.

use std::path::Path;
use std::rc::Rc;

use gpui::{Context, FontWeight, Global, Pixels, Role, WeakEntity, Window, div, px};
use swarm_protocol::{SubscriptionUsage, Usage, UsageProblem};
use swarm_ui::prelude::*;

use crate::actions::{ToggleCommandPalette, ToggleSidebar};
use crate::model::{Agent, AgentKind, Project, ProjectId, Task, TaskStatus, Terminal};
use crate::settings::Page;
use crate::usage_text::{problem_text, shown_percent, spoken_window};
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
                .child(
                    Button::new("retry-daemon", "Retry")
                        .on_click(cx.listener(|workspace, _, _, cx| workspace.retry_daemon(cx))),
                )
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
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(ToggleSidebar), cx);
                }),
        )
}

fn logo_row(window: &mut Window, cx: &mut App) -> impl IntoElement {
    let brand = drag_region("sidebar-brand-drag", window, cx)
        .flex_1()
        .h_full()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(BrandMark::new())
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

    let mut row = ListRow::new(("project", id.0 as usize), project.name.clone())
        .debug_selector(move || format!("project-row-{}", id.0))
        .accessible_label(accessible)
        .toggle_state(workspace.active_project == id)
        .start_slot(tile)
        .end_slot(end)
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.select_project(id, cx);
        }));

    if let Some(focus) = workspace.project_focus.get(&id) {
        row = row.track_focus(focus);
    }

    project_menu(id, project, row, cx)
}

/// The row with its context menu: Open in Finder, Project settings and Remove project.
fn project_menu(
    id: ProjectId,
    project: &Project,
    row: ListRow,
    cx: &Context<Workspace>,
) -> ContextMenu {
    let workspace = cx.weak_entity();
    let root = project.root.clone();

    ContextMenu::new(
        ("project-menu", id.0 as usize),
        format!("Project actions, {}", project.name),
        row,
        move |menu, _, _| {
            let (settings, remove) = (workspace.clone(), workspace.clone());
            let folder = root.clone();

            menu.width(px(200.))
                .item(
                    MenuItem::new("open-in-finder", "Open in Finder")
                        .icon(IconName::Folder)
                        .disabled(folder.is_none())
                        .on_select(move |_, cx| {
                            if let Some(folder) = &folder {
                                reveal_in_finder(folder, cx);
                            }
                        }),
                )
                .item(
                    MenuItem::new("project-settings", "Project settings")
                        .icon(IconName::Settings)
                        .on_select(move |_, cx| {
                            update_workspace(&settings, cx, |workspace, cx| {
                                workspace.open_project_settings(id, cx);
                            });
                        }),
                )
                .separator()
                .item(
                    MenuItem::new("remove-project", "Remove project\u{2026}")
                        .icon(IconName::Close)
                        .on_select(move |window, cx| {
                            update_workspace(&remove, cx, |workspace, cx| {
                                workspace.open_remove_project(id, window, cx);
                            });
                        }),
                )
        },
    )
}

pub(crate) fn update_workspace(
    workspace: &WeakEntity<Workspace>,
    cx: &mut App,
    update: impl FnOnce(&mut Workspace, &mut Context<Workspace>),
) {
    if let Err(error) = workspace.update(cx, update) {
        log::debug!("the window closed before the menu item ran: {error}");
    }
}

impl Workspace {
    /// Opens Settings on the page of the project.
    pub(crate) fn open_project_settings(&self, project: ProjectId, cx: &mut Context<Self>) {
        let page = self
            .data
            .projects
            .iter()
            .position(|entry| entry.id == project)
            .map(Page::Project);

        self.open_settings(page, cx);
    }
}

type Revealer = Rc<dyn Fn(&Path, &mut App)>;

struct RevealWith(Revealer);

impl Global for RevealWith {}

/// Shows the folder in Finder. The one place that does, so a test can see which folder.
pub(crate) fn reveal_in_finder(folder: &Path, cx: &mut App) {
    let revealer = cx
        .try_global::<RevealWith>()
        .map(|revealer| Rc::clone(&revealer.0));

    match revealer {
        Some(reveal) => reveal(folder, cx),
        None => cx.reveal_path(folder),
    }
}

/// Replaces the system call, so a test can see which folder would show.
#[cfg(test)]
pub(crate) fn use_revealer(revealer: Revealer, cx: &mut App) {
    cx.set_global(RevealWith(revealer));
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
    agent_list_row(workspace, agent, indent, cx).into_any_element()
}

/// The row of an agent in the sidebar.
pub(crate) fn agent_list_row(
    workspace: &Workspace,
    agent: &Agent,
    indent: usize,
    cx: &mut Context<Workspace>,
) -> ListRow {
    let terminal = agent.terminal;
    let (icon, kind) = match agent.kind {
        AgentKind::ClaudeCode => (IconName::ClaudeCode, "Claude Code"),
        AgentKind::Codex => (IconName::Codex, "Codex"),
    };

    let queen = workspace.is_queen(agent, cx);

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
        .end_slot(
            h_flex()
                .gap(px(6.))
                .when(queen, |end| end.child(queen_crown(agent)))
                .child(StatusDot::new(agent.status)),
        )
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.focus_terminal(terminal, cx);
        }))
}

/// The queen's crown. A decoration: it has no role and no label, so VoiceOver skips it.
pub(crate) fn queen_crown(agent: &Agent) -> gpui::Div {
    div().debug_selector(|| "queen-bee".to_owned()).child(
        PixelBee::new(("queen-bee", agent.id.0 as usize))
            .scale(1)
            .crown(true),
    )
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

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext};

    use swarm_protocol::UsageWindow;

    use super::*;
    use crate::model::SAMPLE_NOW;

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

    use std::cell::RefCell;
    use std::path::PathBuf;

    use gpui::{Bounds, MouseButton, Point, point};

    use crate::daemon::tests::redraw as settle;
    use crate::palette::Overlay;

    /// The sample workspace with a folder on each project, and the keys bound.
    fn menu_setup(cx: &mut TestAppContext) -> (gpui::Entity<Workspace>, &mut VisualTestContext) {
        let (workspace, cx) = crate::daemon::tests::setup(cx);

        workspace.update(cx, |workspace, _| {
            for project in &mut workspace.data.projects {
                project.root = Some(PathBuf::from(format!("/tmp/{}", project.name)));
            }
        });
        settle(cx);

        (workspace, cx)
    }

    fn bounds_of(selector: &'static str, cx: &mut VisualTestContext) -> Bounds<Pixels> {
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"))
    }

    fn right_click(position: Point<Pixels>, cx: &mut VisualTestContext) {
        cx.simulate_mouse_down(position, MouseButton::Right, gpui::Modifiers::none());
        cx.simulate_mouse_up(position, MouseButton::Right, gpui::Modifiers::none());
        settle(cx);
    }

    /// Right-clicks the row of project 1, "website", and returns where.
    fn open_menu(cx: &mut VisualTestContext) -> Point<Pixels> {
        let pointer = bounds_of("project-row-1", cx).center();

        right_click(pointer, cx);

        pointer
    }

    fn menu_is_open(cx: &mut VisualTestContext) -> bool {
        cx.debug_bounds("project-menu-1-menu").is_some()
    }

    fn row_is_focused(workspace: &gpui::Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
        let row = workspace.read_with(cx, |workspace, _| {
            workspace.project_focus.get(&ProjectId(1)).cloned()
        });

        row.is_some_and(|row| cx.update(|window, _| row.is_focused(window)))
    }

    #[gpui::test]
    fn a_right_click_on_a_project_row_opens_its_menu_at_the_pointer(cx: &mut TestAppContext) {
        let (_, cx) = menu_setup(cx);
        assert!(!menu_is_open(cx));

        let pointer = open_menu(cx);

        let menu = bounds_of("project-menu-1-menu", cx);
        assert_eq!(menu.origin, pointer);
    }

    #[gpui::test]
    fn a_control_click_on_a_project_row_opens_its_menu(cx: &mut TestAppContext) {
        let (_, cx) = menu_setup(cx);
        let pointer = bounds_of("project-row-1", cx).center();

        cx.simulate_click(pointer, gpui::Modifiers::control());
        settle(cx);

        assert!(menu_is_open(cx));
    }

    #[gpui::test]
    fn shift_f10_on_the_focused_project_row_opens_its_menu_at_the_row(cx: &mut TestAppContext) {
        let (workspace, cx) = menu_setup(cx);
        let row = workspace.read_with(cx, |workspace, _| {
            workspace.project_focus.get(&ProjectId(1)).cloned()
        });
        cx.update(|window, cx| {
            if let Some(row) = &row {
                row.focus(window, cx);
            }
        });
        settle(cx);

        cx.simulate_keystrokes("shift-f10");

        let (row, menu) = (
            bounds_of("project-row-1", cx),
            bounds_of("project-menu-1-menu", cx),
        );
        assert_eq!(menu.left(), row.left());
        assert!(menu.top() >= row.bottom() && menu.top() - row.bottom() <= px(8.));
    }

    #[gpui::test]
    fn escape_closes_the_project_menu_and_focus_returns_to_the_row(cx: &mut TestAppContext) {
        let (workspace, cx) = menu_setup(cx);
        open_menu(cx);
        assert!(!row_is_focused(&workspace, cx));

        cx.simulate_keystrokes("escape");

        assert!(!menu_is_open(cx));
        assert!(row_is_focused(&workspace, cx));
    }

    #[gpui::test]
    fn a_click_outside_closes_the_project_menu(cx: &mut TestAppContext) {
        let (_, cx) = menu_setup(cx);
        open_menu(cx);

        cx.simulate_click(point(px(900.), px(600.)), gpui::Modifiers::none());
        settle(cx);

        assert!(!menu_is_open(cx));
    }

    #[gpui::test]
    fn open_in_finder_reveals_the_folder_of_the_project(cx: &mut TestAppContext) {
        let (_, cx) = menu_setup(cx);
        let revealed: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
        cx.update(|_, cx| {
            let seen = Rc::clone(&revealed);

            use_revealer(
                Rc::new(move |folder, _| seen.borrow_mut().push(folder.to_path_buf())),
                cx,
            );
        });
        open_menu(cx);

        cx.simulate_keystrokes("enter");

        assert_eq!(*revealed.borrow(), [PathBuf::from("/tmp/website")]);
        assert!(!menu_is_open(cx));
    }

    #[gpui::test]
    fn open_in_finder_is_off_for_a_project_without_a_folder(cx: &mut TestAppContext) {
        let (workspace, cx) = menu_setup(cx);
        workspace.update(cx, |workspace, _| {
            if let Some(project) = workspace.data.projects.get_mut(1) {
                project.root = None;
            }
        });
        let revealed: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
        cx.update(|_, cx| {
            let seen = Rc::clone(&revealed);

            use_revealer(
                Rc::new(move |folder, _| seen.borrow_mut().push(folder.to_path_buf())),
                cx,
            );
        });
        settle(cx);
        open_menu(cx);
        let menu = bounds_of("project-menu-1-menu", cx);

        // Border and padding take 6 px; Open in Finder is the first item, 34 px high.
        cx.simulate_click(
            menu.origin + point(px(40.), px(6. + 17.)),
            gpui::Modifiers::none(),
        );
        settle(cx);

        assert!(revealed.borrow().is_empty());
        assert!(menu_is_open(cx));
    }

    #[gpui::test]
    fn project_settings_opens_settings_on_the_page_of_the_project(cx: &mut TestAppContext) {
        let (_, cx) = menu_setup(cx);
        open_menu(cx);

        cx.simulate_keystrokes("down enter");
        settle(cx);

        assert_eq!(
            cx.update(|_, cx| crate::settings::shown_page(cx)),
            Some(Page::Project(1))
        );
        assert!(!menu_is_open(cx));
    }

    #[gpui::test]
    fn remove_project_opens_the_dialog_for_the_project(cx: &mut TestAppContext) {
        let (workspace, cx) = menu_setup(cx);
        open_menu(cx);

        cx.simulate_keystrokes("down down enter");
        settle(cx);

        let asked = workspace.read_with(cx, |workspace, cx| match &workspace.overlay {
            Some(Overlay::RemoveProject { view, .. }) => Some(view.read(cx).project),
            _ => None,
        });
        assert_eq!(asked, Some(ProjectId(1)));
        assert!(!menu_is_open(cx));
        assert!(cx.debug_bounds("remove-project-cancel").is_some());
    }

    #[gpui::test]
    fn the_dialog_opened_from_the_menu_keeps_the_focus(cx: &mut TestAppContext) {
        let (workspace, cx) = menu_setup(cx);
        open_menu(cx);
        cx.simulate_keystrokes("down down enter");
        settle(cx);

        cx.simulate_keystrokes("escape");

        let closed = workspace.read_with(cx, |workspace, _| workspace.overlay.is_none());
        assert!(closed, "Escape did not reach the dialog");
        assert!(row_is_focused(&workspace, cx));
    }
}
