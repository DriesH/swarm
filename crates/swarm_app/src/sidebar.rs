//! Sidebar: projects, the open task's agents and the usage card.

use gpui::{App, Context, FontWeight, Role, SharedString, Window, div, px, relative};
use swarm_ui::prelude::*;

use crate::actions::ToggleCommandPalette;
use crate::model::{Agent, AgentKind, Project, Task, TaskStatus, Terminal};
use crate::window_drag::drag_region;
use crate::workspace::{DataSource, Workspace};

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
            heading("Projects", 14.).child(
                IconButton::new("add-project", IconName::Plus, "Add project")
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
        .when(!workspace.data.usage.is_empty(), |sidebar| {
            sidebar.child(usage_card(workspace, cx))
        })
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
        .h(px(24.))
        .px(px(6.))
        .child(
            drag_region("sidebar-top-drag", window, cx)
                .flex_1()
                .h_full(),
        )
        .child(
            IconButton::new("toggle-sidebar", IconName::Sidebar, "Toggle sidebar")
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
        .px(px(6.))
        .mt(px(12.))
        .gap(px(8.))
        .child(brand)
        .child(
            IconButton::new("search", IconName::Search, "Search")
                .color(Color::Muted)
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(ToggleCommandPalette), cx);
                }),
        )
}

fn heading(title: &'static str, top_margin: f32) -> gpui::Div {
    h_flex().h(px(28.)).px(px(8.)).mt(px(top_margin)).child(
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

fn usage_card(workspace: &Workspace, cx: &App) -> impl IntoElement + use<> {
    let colors = cx.theme().colors();
    let (line, panel, soft, fg) = (colors.line, colors.panel, colors.soft, colors.fg);

    let entries: Vec<_> = workspace
        .data
        .usage
        .iter()
        .map(|(name, fraction)| usage_entry(name.clone(), *fraction, soft, fg))
        .collect();

    v_flex()
        .gap(px(9.))
        .px(px(12.))
        .py(px(10.))
        .rounded(px(10.))
        .border_1()
        .border_color(line)
        .bg(panel)
        .children(entries)
}

fn usage_entry(
    name: SharedString,
    fraction: f32,
    track: gpui::Hsla,
    fill: gpui::Hsla,
) -> impl IntoElement {
    let fraction = fraction.clamp(0., 1.);
    let percent = format!("{:.0}%", fraction * 100.);

    v_flex()
        .gap(px(5.))
        .child(
            h_flex()
                .child(
                    div()
                        .flex_1()
                        .child(Label::new(name).size(LabelSize::Small).color(Color::Muted)),
                )
                .child(Label::new(percent).mono().size(LabelSize::XSmall)),
        )
        .child(
            div().h(px(4.)).rounded(px(2.)).bg(track).child(
                div()
                    .h(px(4.))
                    .w(relative(fraction))
                    .rounded(px(2.))
                    .bg(fill),
            ),
        )
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext};

    use super::*;
    use crate::model::ProjectId;

    fn setup(cx: &mut TestAppContext) -> (gpui::Entity<Workspace>, &mut VisualTestContext) {
        cx.update(swarm_ui::theme::init);

        cx.add_window_view(Workspace::with_sample)
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
}
