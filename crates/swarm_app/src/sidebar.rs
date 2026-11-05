//! Sidebar: projects, the open task's agents and the usage card.

use std::path::Path;
use std::rc::Rc;

use gpui::{
    Context, ElementId, FocusHandle, FontWeight, Global, Hsla, KeyDownEvent, Pixels, Role,
    SharedString, WeakEntity, Window, div, px,
};
use swarm_ui::a11y::accessible;
use swarm_ui::prelude::*;
use swarm_ui::roving::RovingFocus;
use swarm_ui::styles::focus_ring;

use crate::actions::{OpenFieldGuide, OpenSettings, ReportIssue, ToggleCommandPalette};
use crate::daemon::is_resumable;
use crate::model::{Agent, AgentKind, Project, ProjectId, TaskStatus, Terminal};
use crate::settings::Page;
use crate::usage_text::{UsageKind, UsageSection, WindowRow, usage_sections};
use crate::views::process_status;
use crate::window_drag::drag_region;
use crate::workspace::{DataSource, Workspace};

/// The expanded sidebar's width. The title bar keeps its leading part this wide, so the
/// tabs line up with the card.
pub(crate) const SIDEBAR_WIDTH: Pixels = px(248.);

/// Room for a 36 pt `RailButton` with 6 pt on each side.
pub(crate) const RAIL_WIDTH: Pixels = px(48.);

pub(crate) fn render(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement + use<> {
    let projects: Vec<_> = workspace
        .data
        .projects
        .iter()
        .enumerate()
        .map(|(index, project)| project_row(workspace, project, index, cx))
        .collect();

    let agents = if workspace.has_no_projects() {
        Vec::new()
    } else {
        agent_rows(workspace, cx)
    };

    let scrolling = v_flex()
        .gap(px(2.))
        .child(
            heading("projects-heading", "Projects", 14.)
                .h(px(30.))
                .child(
                    IconButton::new("add-project", IconName::Plus, "Add project")
                        .size(IconButtonSize::Medium)
                        .debug_selector(|| "add-project".to_owned())
                        .on_click(
                            cx.listener(|_, _, window, cx| Workspace::pick_project(window, cx)),
                        ),
                ),
        )
        .child(
            v_flex()
                .id("project-list")
                .role(Role::List)
                .aria_label("Projects")
                .on_key_down(cx.listener(|workspace, event: &KeyDownEvent, window, cx| {
                    if workspace
                        .project_roving
                        .handle_key(&event.keystroke, window, cx)
                        .is_some()
                    {
                        cx.stop_propagation();
                    }
                }))
                .children(projects),
        )
        .children(daemon_status(workspace, cx))
        .children(error_message(workspace, cx))
        .when(!agents.is_empty(), |sidebar| {
            sidebar
                .child(
                    heading("agents-heading", "Agents", 18.)
                        .debug_selector(|| "agents-heading".to_owned()),
                )
                .child(
                    v_flex()
                        .id("agent-list")
                        .role(Role::List)
                        .aria_label("Agents")
                        .on_key_down(cx.listener(|workspace, event: &KeyDownEvent, window, cx| {
                            if workspace
                                .agent_roving
                                .handle_key(&event.keystroke, window, cx)
                                .is_some()
                            {
                                cx.stop_propagation();
                            }
                        }))
                        .children(agents),
                )
        });

    v_flex()
        .id("sidebar")
        .track_focus(&workspace.sidebar_focus)
        .role(Role::Navigation)
        .aria_label("Projects and agents")
        .size_full()
        .gap(px(2.))
        .child(logo_row(window, cx))
        .child(
            div()
                .id("sidebar-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(scrolling),
        )
        .child(footer(workspace, window, cx))
}

/// The sidebar collapsed to a column of buttons: search, the projects and the open task's
/// terminals. It tracks the sidebar's focus when `track_focus` is set. While the sidebar and
/// the rail show together, only the sidebar does.
pub(crate) fn render_rail(
    workspace: &Workspace,
    track_focus: bool,
    cx: &mut Context<Workspace>,
) -> impl IntoElement + use<> {
    let line = cx.theme().colors().line;

    let projects: Vec<RailButton> = workspace
        .data
        .projects
        .iter()
        .enumerate()
        .map(|(index, project)| {
            project_button(workspace, project, track_focus.then_some(index), cx)
        })
        .collect();

    let terminals: Vec<RailButton> = if workspace.has_no_projects() {
        Vec::new()
    } else {
        rows(workspace)
            .into_iter()
            .enumerate()
            .map(|(index, row)| terminal_button(workspace, row, track_focus.then_some(index), cx))
            .collect()
    };

    v_flex()
        .id("sidebar-rail")
        .when(track_focus, |rail| {
            rail.track_focus(&workspace.sidebar_focus)
        })
        .role(Role::Navigation)
        .aria_label("Projects and agents")
        .debug_selector(|| "sidebar-rail".to_owned())
        .size_full()
        .items_center()
        .gap(px(4.))
        .pt(px(4.))
        .pb(px(10.))
        .child(
            IconButton::new("rail-search", IconName::Search, "Search")
                .size(IconButtonSize::Medium)
                .color(Color::Muted)
                .debug_selector(|| "rail-search".to_owned())
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(ToggleCommandPalette), cx);
                }),
        )
        .child(rail_divider(line))
        .child(
            div()
                .id("rail-items")
                .on_key_down(cx.listener(|workspace, event: &KeyDownEvent, window, cx| {
                    let roving = if workspace.project_roving.focused(window, cx).is_some() {
                        &mut workspace.project_roving
                    } else {
                        &mut workspace.agent_roving
                    };

                    if roving.focused(window, cx).is_some()
                        && roving.handle_key(&event.keystroke, window, cx).is_some()
                    {
                        cx.stop_propagation();
                    }
                }))
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_y_scroll()
                .child(
                    v_flex()
                        .items_center()
                        .gap(px(4.))
                        .children(projects)
                        .child(
                            IconButton::new("rail-add-project", IconName::Plus, "Add project")
                                .size(IconButtonSize::Medium)
                                .debug_selector(|| "rail-add-project".to_owned())
                                .on_click(cx.listener(|_, _, window, cx| {
                                    Workspace::pick_project(window, cx);
                                })),
                        )
                        .when(!terminals.is_empty(), |list| {
                            list.child(rail_divider(line)).children(terminals)
                        }),
                ),
        )
        .child(rail_footer(workspace, cx))
}

/// The usage ring of the highest window, then a divider and the three footer buttons.
fn rail_footer(workspace: &Workspace, cx: &mut Context<Workspace>) -> impl IntoElement + use<> {
    let line = cx.theme().colors().line;
    let sections = usage_sections(&workspace.data.usage, workspace.now_ms() / 1_000);

    let ring = highest_window(&sections).map(|(kind, window)| {
        UsageRing::new(
            "rail-usage",
            window.fraction(),
            ring_label(kind, window, &sections),
        )
        .tooltip(shown_usage(&sections))
        .tone(window.tone())
        .debug_selector(|| "rail-usage".to_owned())
    });

    let caught = workspace.data.caught_creatures.len();
    let buttons = FooterLink::ALL.map(|link| {
        let label = link.label();
        let tooltip = match link {
            FooterLink::FieldGuide => format!("{label}, {}", caught_text(caught)),
            _ => label.to_owned(),
        };

        RailButton::new(
            ("rail-footer", link.index()),
            link.spoken(caught),
            Icon::new(link.icon())
                .size(IconSize::Large)
                .color(Color::Muted),
        )
        .tooltip(tooltip)
        .debug_selector(move || format!("rail-{}", link.slug()))
        .on_click(move |_, window, cx| link.dispatch(window, cx))
    });

    v_flex()
        .id("rail-footer")
        .role(Role::Group)
        .aria_label("App")
        .flex_none()
        .items_center()
        .gap(px(4.))
        .children(ring)
        .child(rail_divider(line))
        .children(buttons)
}

fn rail_divider(line: Hsla) -> gpui::Div {
    div().w(px(32.)).h(px(1.)).flex_none().bg(line).my(px(4.))
}

/// `focus_index` is the row's place in the project list when the rail holds the focus.
fn project_button(
    workspace: &Workspace,
    project: &Project,
    focus_index: Option<usize>,
    cx: &mut Context<Workspace>,
) -> RailButton {
    let id = project.id;
    let summary = ProjectSummary::of(workspace, id);
    let label = SharedString::from(project_label(project, &summary));

    let badge = if summary.needs_you || project.config_error.is_some() {
        Some(AgentStatus::Blocked)
    } else if summary.working {
        Some(AgentStatus::Working)
    } else {
        None
    };

    let button = RailButton::new(
        ("rail-project", id.0 as usize),
        label.clone(),
        ProjectTile::new(project.letter, project.tile_color).size(px(22.)),
    );
    let button = match focus_index.and_then(|index| handle_at(&workspace.project_roving, index)) {
        Some(handle) => button.track_focus(&handle),
        None => button,
    };

    button
        .tooltip(label)
        .toggle_state(workspace.active_project == id)
        .badge(badge)
        .debug_selector(move || format!("rail-project-{}", id.0))
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.select_project(id, cx);
        }))
}

/// `focus_index` is the row's place in the agent list when the rail holds the focus.
fn terminal_button(
    workspace: &Workspace,
    row: Row<'_>,
    focus_index: Option<usize>,
    cx: &mut Context<Workspace>,
) -> RailButton {
    let terminal_icon = || {
        Icon::new(IconName::Terminal)
            .size(IconSize::Large)
            .color(Color::Muted)
    };

    let (button, terminal) = match row {
        Row::Agent(agent, _) => {
            let (icon, _) = agent_kind(agent.kind);
            let id = agent.id;
            let label = SharedString::from(agent_label(agent));

            let button = RailButton::new(
                ("rail-agent", id.0 as usize),
                label.clone(),
                Icon::new(icon).size(IconSize::Large),
            )
            .tooltip(label)
            .badge(Some(agent.status))
            .debug_selector(move || format!("rail-agent-{}", id.0));

            (button, agent.terminal)
        }
        Row::Process(terminal, owner, _) => {
            let id = terminal.id;
            let label = SharedString::from(process_label(terminal, owner));

            let button = RailButton::new(
                ("rail-process", id.0 as usize),
                label.clone(),
                terminal_icon(),
            )
            .tooltip(label)
            .badge(Some(process_status(terminal)))
            .debug_selector(move || format!("rail-process-{}", id.0));

            (button, id)
        }
        Row::User(terminal) => {
            let id = terminal.id;

            let button = RailButton::new(
                ("rail-user", id.0 as usize),
                user_label(workspace, terminal),
                LockedTerminalIcon::new().cutout(if workspace.focused_terminal == Some(id) {
                    cx.theme().colors().active
                } else {
                    cx.theme().colors().ground
                }),
            )
            .tooltip(terminal.title.clone())
            .debug_selector(move || format!("rail-user-{}", id.0));

            (button, id)
        }
    };

    let button = match focus_index.and_then(|index| handle_at(&workspace.agent_roving, index)) {
        Some(handle) => button.track_focus(&handle),
        None => button,
    };

    button
        .toggle_state(workspace.focused_terminal == Some(terminal))
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.focus_terminal(terminal, cx);
        }))
}

fn daemon_status(workspace: &Workspace, cx: &mut Context<Workspace>) -> Option<LiveRegion> {
    match &workspace.source {
        DataSource::Connecting => Some(
            LiveRegion::status("daemon-status", "Connecting").child(
                h_flex()
                    .debug_selector(|| "daemon-status".to_owned())
                    .h(px(28.))
                    .gap(px(8.))
                    .px(px(8.))
                    .child(StatusRing::new(AgentStatus::Working))
                    .child(Label::new("Connecting").color(Color::Muted)),
            ),
        ),
        DataSource::Unavailable { reason } => {
            let action = if workspace.stale_daemon.is_some() {
                restart_daemon_button(cx)
            } else {
                Button::new("retry-daemon", "Retry")
                    .debug_selector(|| "retry-daemon".to_owned())
                    .on_click(cx.listener(|workspace, _, _, cx| workspace.retry_daemon(cx)))
            };

            let muted = Color::Muted.hsla(cx);

            Some(
                LiveRegion::alert("daemon-status", format!("Daemon not running: {reason}")).child(
                    v_flex()
                        .gap(px(2.))
                        .child(
                            h_flex()
                                .debug_selector(|| "daemon-status".to_owned())
                                .min_h(px(28.))
                                .gap(px(8.))
                                .px(px(8.))
                                .child(
                                    div().flex_1().child(
                                        Label::new("Daemon not running").color(Color::Muted),
                                    ),
                                )
                                .child(action),
                        )
                        .child(
                            div()
                                .debug_selector(|| "daemon-reason".to_owned())
                                .mx(px(8.))
                                .text_size(px(12.))
                                .text_color(muted)
                                .child(reason.clone()),
                        ),
                ),
            )
        }
        DataSource::Live => {
            let text = workspace.stale_daemon.as_ref()?.status()?;

            Some(
                LiveRegion::status("daemon-status", text.clone()).child(
                    h_flex()
                        .debug_selector(|| "daemon-status".to_owned())
                        .min_h(px(28.))
                        .gap(px(8.))
                        .px(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Label::new(text).color(Color::Muted)),
                        )
                        .child(restart_daemon_button(cx)),
                ),
            )
        }
        DataSource::Sample => None,
    }
}

fn restart_daemon_button(cx: &mut Context<Workspace>) -> Button {
    Button::new("restart-daemon", "Restart daemon")
        .debug_selector(|| "restart-daemon".to_owned())
        .on_click(cx.listener(|workspace, _, window, cx| workspace.ask_restart_daemon(window, cx)))
}

/// The error text as an alert, so VoiceOver speaks it when it appears.
fn error_region(message: &SharedString) -> LiveRegion {
    LiveRegion::alert("sidebar-error", message.clone()).child(message.clone())
}

fn error_message(workspace: &Workspace, cx: &mut Context<Workspace>) -> Option<impl IntoElement> {
    let message = workspace.sidebar_error.clone()?;
    let color = Color::Del.hsla(cx);

    Some(
        h_flex()
            .debug_selector(|| "sidebar-error".to_owned())
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
                    .child(error_region(&message)),
            )
            .child(
                IconButton::new("dismiss-error", IconName::Close, "Dismiss message")
                    .debug_selector(|| "dismiss-error".to_owned())
                    .on_click(
                        cx.listener(|workspace, _, _, cx| workspace.dismiss_sidebar_error(cx)),
                    ),
            ),
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

fn heading(id: &'static str, title: &'static str, top_margin: f32) -> gpui::Div {
    h_flex()
        .h(px(28.))
        .pl(px(8.))
        .mt(px(top_margin))
        .child(div().flex_1().child(heading_label(id, title)))
}

fn heading_label(id: &'static str, title: &'static str) -> Label {
    Label::new(title)
        .heading(id, 2)
        .size(LabelSize::Small)
        .color(Color::Faint)
}

fn project_row(
    workspace: &Workspace,
    project: &Project,
    index: usize,
    cx: &mut Context<Workspace>,
) -> impl IntoElement + use<> {
    let id = project.id;
    let summary = ProjectSummary::of(workspace, id);
    let (needs_you, working) = (summary.needs_you, summary.working);

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
                Label::new(summary.task_count.to_string())
                    .mono()
                    .size(LabelSize::XSmall)
                    .color(Color::Faint),
            ),
        );

    let mut row = ListRow::new(("project", id.0 as usize), project.name.clone())
        .debug_selector(move || format!("project-row-{}", id.0))
        .accessible_label(project_label(project, &summary))
        .toggle_state(workspace.active_project == id)
        .start_slot(tile)
        .end_slot(end)
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.project_roving.set_active(index);
            workspace.select_project(id, cx);
        }));

    if let Some(focus) = workspace.project_focus.get(&id) {
        row = row.track_focus(focus);
    }

    project_menu(id, project, row, cx)
}

/// What the tasks of a project add up to. Counted once per project.
struct ProjectSummary {
    task_count: usize,
    needs_you: bool,
    working: bool,
}

impl ProjectSummary {
    fn of(workspace: &Workspace, project: ProjectId) -> Self {
        let mut summary = Self {
            task_count: 0,
            needs_you: false,
            working: false,
        };

        for task in workspace.data.tasks_of(project) {
            summary.task_count += 1;
            summary.needs_you |= task.status == TaskStatus::NeedsYou;
            summary.working |= task.status == TaskStatus::Working;
        }

        summary
    }
}

/// What VoiceOver reads for a project: its name, task count and status.
fn project_label(project: &Project, summary: &ProjectSummary) -> String {
    let status = if summary.needs_you {
        ", needs you"
    } else if summary.working {
        ", working"
    } else {
        ""
    };
    let plural = if summary.task_count == 1 {
        "task"
    } else {
        "tasks"
    };
    let config_error = project
        .config_error
        .as_ref()
        .map(|error| format!(", config error: {error}"))
        .unwrap_or_default();

    format!(
        "{}, {} {plural}{status}{config_error}",
        project.name, summary.task_count
    )
}

/// The row with its context menu: Show in Finder, Project settings and Remove project.
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
                    MenuItem::new("show-in-finder", "Show in Finder")
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
            .project(project)
            .map(|entry| Page::Project(entry.id));

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

/// The handle of row `index`, or none when the roving focus has not synced to this many rows.
fn handle_at(roving: &RovingFocus, index: usize) -> Option<FocusHandle> {
    (index < roving.len()).then(|| roving.handle(index))
}

/// The key of each row of the agent list, in order. The roving focus keeps a handle per key.
pub(crate) fn row_keys(workspace: &Workspace) -> Vec<ElementId> {
    if workspace.has_no_projects() {
        return Vec::new();
    }

    rows(workspace)
        .iter()
        .map(|row| match row {
            Row::Agent(agent, _) => ElementId::from(("agent", agent.id.0 as usize)),
            Row::Process(terminal, ..) => ElementId::from(("process", terminal.id.0 as usize)),
            Row::User(terminal) => ElementId::from(("user-terminal", terminal.id.0 as usize)),
        })
        .collect()
}

fn agent_rows(workspace: &Workspace, cx: &mut Context<Workspace>) -> Vec<gpui::AnyElement> {
    rows(workspace)
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            let list_row = match row {
                Row::Agent(agent, indent) => agent_list_row(workspace, agent, indent, cx),
                Row::Process(terminal, owner, indent) => {
                    process_row(workspace, terminal, owner, indent, cx)
                }
                Row::User(terminal) => user_row(workspace, terminal, cx),
            };

            match handle_at(&workspace.agent_roving, index) {
                Some(handle) => list_row.track_focus(&handle),
                None => list_row,
            }
            .into_any_element()
        })
        .collect()
}

fn process_row(
    workspace: &Workspace,
    terminal: &Terminal,
    owner: &str,
    indent: usize,
    cx: &mut Context<Workspace>,
) -> ListRow {
    let id = terminal.id;

    ListRow::new(("process", id.0 as usize), terminal.title.clone())
        .debug_selector(move || format!("process-row-{}", id.0))
        .accessible_label(process_label(terminal, owner))
        .compact(true)
        .indent(indent)
        .toggle_state(workspace.focused_terminal == Some(id))
        .start_slot(
            Icon::new(IconName::Terminal)
                .size(IconSize::Large)
                .color(Color::Muted),
        )
        .end_slot(StatusDot::new(process_status(terminal)))
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.focus_terminal(id, cx);
        }))
}

/// What VoiceOver reads for a process terminal, the agent that started it and how it is
/// doing.
fn process_label(terminal: &Terminal, owner: &str) -> String {
    let status = process_status(terminal).label().to_lowercase();

    if owner.is_empty() {
        format!("{}, process terminal, {status}", terminal.title)
    } else {
        format!("{}, started by {owner}, {status}", terminal.title)
    }
}

fn user_label(workspace: &Workspace, terminal: &Terminal) -> String {
    format!(
        "{}, your terminal {}, mine, agents have no access",
        terminal.title,
        workspace.user_terminal_number(terminal.id)
    )
}

fn user_row(workspace: &Workspace, terminal: &Terminal, cx: &mut Context<Workspace>) -> ListRow {
    let id = terminal.id;

    let colors = cx.theme().colors();
    let selected = workspace.focused_terminal == Some(id);
    let cutout = if selected {
        colors.active
    } else {
        colors.ground
    };

    let close = IconButton::new(
        ("close-user-terminal", id.0 as usize),
        IconName::Close,
        format!("Close {}", terminal.title),
    )
    .debug_selector(move || format!("sidebar-close-terminal-{}", id.0))
    .skip_tab_stop()
    .on_click(cx.listener(move |workspace, _, window, cx| {
        // The row under the button must not select the terminal as well.
        cx.stop_propagation();
        workspace.request_close_terminal(id, window, cx);
    }));

    ListRow::new(("user-terminal", id.0 as usize), terminal.title.clone())
        .debug_selector(move || format!("user-row-{}", id.0))
        .accessible_label(user_label(workspace, terminal))
        .compact(true)
        .toggle_state(selected)
        .start_slot(LockedTerminalIcon::new().cutout(cutout))
        .name_suffix(
            div()
                .debug_selector(move || format!("user-mine-{}", id.0))
                .child(Tag::new("Mine")),
        )
        .end_slot(close)
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.focus_terminal(id, cx);
        }))
}

/// The icon and the name of an agent kind.
fn agent_kind(kind: AgentKind) -> (IconName, &'static str) {
    match kind {
        AgentKind::ClaudeCode => (IconName::ClaudeCode, "Claude Code"),
        AgentKind::Codex => (IconName::Codex, "Codex"),
    }
}

/// What VoiceOver reads for an agent: its name, kind and status.
fn agent_label(agent: &Agent) -> String {
    let (_, kind) = agent_kind(agent.kind);

    let mut label = format!(
        "{}, {kind}, {}",
        agent.name,
        agent.status.label().to_lowercase()
    );
    if is_resumable(agent) {
        label.push_str(", resumable");
    }

    label
}

/// The row of an agent in the sidebar.
pub(crate) fn agent_list_row(
    workspace: &Workspace,
    agent: &Agent,
    indent: usize,
    cx: &mut Context<Workspace>,
) -> ListRow {
    let terminal = agent.terminal;
    let (icon, _) = agent_kind(agent.kind);

    let queen = workspace.is_queen(agent, cx);

    ListRow::new(("agent", agent.id.0 as usize), agent.name.clone())
        .accessible_label(agent_label(agent))
        .compact(true)
        .indent(indent)
        .toggle_state(workspace.focused_terminal == Some(terminal))
        .start_slot(Icon::new(icon).size(IconSize::Large))
        .end_slot(
            h_flex()
                .gap(px(6.))
                .when(queen, |end| end.child(queen_crown(agent)))
                .when(is_resumable(agent), |end| {
                    end.child(
                        Label::new("resumable")
                            .size(LabelSize::XSmall)
                            .color(Color::Faint),
                    )
                })
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

/// Below this window height the usage card folds to one line.
const FOLD_BELOW: Pixels = px(700.);

/// How many creatures the Field guide holds.
const FIELD_GUIDE_TOTAL: usize = 20;

/// True when a window of this height folds the usage card unless the user opened it.
fn usage_can_fold(height: Pixels) -> bool {
    height < FOLD_BELOW
}

impl Workspace {
    /// Opens or folds the usage card of a short window. The choice stays while the window
    /// lives.
    pub(crate) fn toggle_usage_card(&mut self, cx: &mut Context<Self>) {
        self.usage_expanded = !self.usage_expanded;
        cx.notify();
    }
}

/// The places the footer of the sidebar and the rail lead to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FooterLink {
    Settings,
    FieldGuide,
    ReportIssue,
}

impl FooterLink {
    /// In the order they show.
    const ALL: [Self; 3] = [Self::Settings, Self::FieldGuide, Self::ReportIssue];

    fn label(self) -> &'static str {
        match self {
            Self::Settings => "Settings",
            Self::FieldGuide => "Field guide",
            Self::ReportIssue => "Report an issue",
        }
    }

    /// For element ids and test selectors.
    fn slug(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::FieldGuide => "field-guide",
            Self::ReportIssue => "report-issue",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Settings => 0,
            Self::FieldGuide => 1,
            Self::ReportIssue => 2,
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Settings => IconName::Settings,
            Self::FieldGuide => IconName::Book,
            Self::ReportIssue => IconName::Feedback,
        }
    }

    /// What VoiceOver reads.
    fn spoken(self, caught: usize) -> String {
        match self {
            Self::FieldGuide => format!("Field guide, {caught} of {FIELD_GUIDE_TOTAL} caught"),
            _ => self.label().to_owned(),
        }
    }

    fn dispatch(self, window: &mut Window, cx: &mut App) {
        match self {
            Self::Settings => window.dispatch_action(Box::new(OpenSettings), cx),
            Self::FieldGuide => window.dispatch_action(Box::new(OpenFieldGuide), cx),
            Self::ReportIssue => window.dispatch_action(Box::new(ReportIssue), cx),
        }
    }
}

fn caught_text(caught: usize) -> String {
    format!("{caught}/{FIELD_GUIDE_TOTAL}")
}

/// The window with the highest used share of every kind, and the kind it belongs to.
fn highest_window(sections: &[UsageSection]) -> Option<(UsageKind, &WindowRow)> {
    sections
        .iter()
        .flat_map(|section| section.windows.iter().map(|window| (section.kind, window)))
        .max_by_key(|(_, window)| window.percent)
}

/// The highest window of each kind that has one.
fn highest_per_kind(sections: &[UsageSection]) -> Vec<(UsageKind, &WindowRow)> {
    sections
        .iter()
        .filter_map(|section| {
            let window = section.windows.iter().max_by_key(|window| window.percent)?;

            Some((section.kind, window))
        })
        .collect()
}

/// What VoiceOver reads for the usage of every window, folded or not.
fn spoken_usage(sections: &[UsageSection]) -> String {
    let windows: Vec<&str> = sections
        .iter()
        .flat_map(|section| section.windows.iter())
        .map(|window| window.spoken.as_str())
        .collect();

    windows.join(". ")
}

/// What VoiceOver reads for the rail's ring: the highest window, then every window.
fn ring_label(kind: UsageKind, highest: &WindowRow, sections: &[UsageSection]) -> String {
    format!(
        "Usage, highest window {} {} percent. {}",
        kind.name(),
        highest.percent,
        spoken_usage(sections)
    )
}

/// Every window on one line, for a tooltip.
fn shown_usage(sections: &[UsageSection]) -> String {
    let kinds: Vec<String> = sections
        .iter()
        .filter(|section| !section.windows.is_empty())
        .map(|section| {
            let windows: Vec<String> = section
                .windows
                .iter()
                .map(|window| format!("{} {}%", window.name, window.percent))
                .collect();

            format!("{} {}", section.kind.name(), windows.join(", "))
        })
        .collect();

    kinds.join(" \u{b7} ")
}

/// The usage, a divider and the three places to go. A short window folds the usage to one
/// line until the user opens it.
fn footer(
    workspace: &Workspace,
    window: &Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement + use<> {
    let line = cx.theme().colors().line;
    let sections = usage_sections(&workspace.data.usage, workspace.now_ms() / 1_000);

    let foldable = usage_can_fold(window.viewport_size().height);
    let open = !foldable || workspace.usage_expanded;

    let summary = (foldable && !sections.is_empty()).then(|| usage_summary(&sections, open, cx));
    let card = if open { usage_card(&sections) } else { None };

    let caught = workspace.data.caught_creatures.len();
    let links = v_flex()
        .id("sidebar-links")
        .role(Role::List)
        .aria_label("App")
        .gap(px(2.))
        .children(FooterLink::ALL.map(|link| footer_row(link, caught)));

    v_flex()
        .id("sidebar-footer")
        .flex_none()
        .gap(px(8.))
        .pt(px(8.))
        .children(summary)
        .children(card)
        .child(
            div()
                .debug_selector(|| "sidebar-divider".to_owned())
                .h(px(1.))
                .mx(px(2.))
                .bg(line),
        )
        .child(links)
}

fn footer_row(link: FooterLink, caught: usize) -> ListRow {
    let slug = link.slug();

    ListRow::new(("footer", link.index()), link.label())
        .debug_selector(move || format!("footer-{slug}"))
        .accessible_label(link.spoken(caught))
        .start_slot(
            Icon::new(link.icon())
                .size(IconSize::Medium)
                .color(Color::Muted),
        )
        .when(link == FooterLink::FieldGuide, |row| {
            row.end_slot(
                div()
                    .debug_selector(|| "footer-field-guide-count".to_owned())
                    .child(
                        Label::new(caught_text(caught))
                            .mono()
                            .size(LabelSize::XSmall)
                            .color(Color::Faint),
                    ),
            )
        })
        .on_click(move |_, window, cx| link.dispatch(window, cx))
}

/// The one-line usage: the highest window of each agent kind. A button that folds and opens
/// the card.
fn usage_summary(
    sections: &[UsageSection],
    open: bool,
    cx: &mut Context<Workspace>,
) -> impl IntoElement + use<> {
    let colors = cx.theme().colors();
    let (line, raised, active, accent) = (colors.line, colors.raised, colors.active, colors.accent);

    let kinds = highest_per_kind(sections)
        .into_iter()
        .map(|(kind, window)| {
            let dot = match kind {
                UsageKind::Claude => colors.claude,
                UsageKind::Codex => colors.fg,
            };

            h_flex()
                .gap(px(5.))
                .child(div().size(px(7.)).rounded_full().bg(dot))
                .child(Label::new(kind.name()).size(LabelSize::Small))
                .child(
                    Label::new(format!("{}%", window.percent))
                        .mono()
                        .size(LabelSize::Small),
                )
        });

    let chevron = if open {
        IconName::ChevronDown
    } else {
        IconName::ChevronUp
    };

    accessible(
        div().id("usage-fold"),
        Role::Button,
        format!("Usage: {}", spoken_usage(sections)),
    )
    .aria_expanded(open)
    .debug_selector(|| "usage-fold".to_owned())
    .focusable()
    .tab_stop(true)
    .cursor_pointer()
    .flex()
    .items_center()
    .gap(px(10.))
    .h(px(34.))
    .px(px(10.))
    .rounded(px(10.))
    .border_1()
    .border_color(line)
    .bg(raised)
    .hover(|style| style.bg(active))
    .focus_visible(|style| focus_ring(style, accent))
    .children(kinds)
    .child(div().flex_1())
    .child(
        Icon::new(chevron)
            .size(IconSize::XSmall)
            .color(Color::Faint),
    )
    .on_click(cx.listener(|workspace, _, _, cx| workspace.toggle_usage_card(cx)))
}

fn usage_card(sections: &[UsageSection]) -> Option<impl IntoElement + use<>> {
    if sections.is_empty() {
        return None;
    }

    Some(
        div().debug_selector(|| "usage-card".to_owned()).child(
            Panel::new().accessible("usage", "Subscription usage").row(
                v_flex()
                    .gap(px(12.))
                    .px(px(12.))
                    .py(px(10.))
                    .children(sections.iter().map(usage_group)),
            ),
        ),
    )
}

/// An agent kind with its plan, then a meter for each window.
fn usage_group(section: &UsageSection) -> impl IntoElement + use<> {
    let id = SharedString::from(format!("usage-{}", section.kind.slug()));

    let header = h_flex()
        .items_baseline()
        .gap(px(6.))
        .child(
            Label::new(section.kind.name())
                .size(LabelSize::Small)
                .weight(FontWeight::MEDIUM),
        )
        .children(section.plan.clone().map(|plan| {
            let selector = format!("{id}-plan");

            div()
                .debug_selector(move || selector)
                .child(Label::new(plan).size(LabelSize::XSmall).color(Color::Faint))
        }))
        .child(div().flex_1())
        .children(section.age.as_ref().map(|(shown, _)| {
            let selector = format!("{id}-age");

            div().debug_selector(move || selector).child(
                Label::new(shown.clone())
                    .size(LabelSize::XSmall)
                    .color(Color::Faint),
            )
        }));

    let problem = section.problem.as_ref().map(|(shown, spoken)| {
        let problem_id = format!("{id}-problem");

        div()
            .debug_selector({
                let selector = problem_id.clone();
                move || selector
            })
            .child(problem_region(problem_id, shown, spoken))
    });

    v_flex()
        .id(id.clone())
        .debug_selector({
            let selector = id.to_string();
            move || selector
        })
        .role(Role::Group)
        .aria_label(section.label())
        .gap(px(7.))
        .child(header)
        .children(
            section
                .windows
                .iter()
                .enumerate()
                .map(|(index, window)| usage_meter(&id, index, window)),
        )
        .children(problem)
}

/// Why a usage figure is missing, spoken when it appears.
fn problem_region(id: String, shown: &str, spoken: &str) -> LiveRegion {
    LiveRegion::status(SharedString::from(id), spoken.to_owned()).child(
        Label::new(shown.to_owned())
            .size(LabelSize::XSmall)
            .color(Color::Faint),
    )
}

fn usage_meter(group: &SharedString, index: usize, row: &WindowRow) -> UsageMeter {
    let selector = format!("{group}-{index}");

    UsageMeter::new((group.clone(), index), row.name.clone())
        .value(format!("{}%", row.percent))
        .fraction(row.fraction())
        .tone(row.tone())
        .surface(UsageSurface::Panel)
        .accessible_label(row.spoken.clone())
        .debug_selector(move || selector)
        .when_some(row.pace(), UsageMeter::pace)
}

#[cfg(test)]
mod tests {
    use gpui::{Hsla, TestAppContext, VisualTestContext};

    use swarm_protocol::{SubscriptionUsage, Usage, UsageProblem, UsageWindow};
    use swarm_ui::UsageTone;

    use super::*;
    use crate::daemon::tests::drawn_color;
    use crate::model::SAMPLE_NOW;

    fn setup(cx: &mut TestAppContext) -> (gpui::Entity<Workspace>, &mut VisualTestContext) {
        cx.update(|cx| {
            swarm_ui::theme::init(cx);
            crate::workspace::follow_test_reduce_motion(true, cx);
        });

        cx.add_window_view(Workspace::with_sample)
    }

    #[gpui::test]
    fn the_sidebar_names_a_resumable_agent_as_resumable(cx: &mut TestAppContext) {
        let (workspace, cx, _resumed) = crate::daemon::tests::resume_workspace(None, cx);

        let label = workspace.read_with(cx, |workspace, _| {
            agent_label(workspace.data.agents.first().expect("one agent"))
        });

        assert_eq!(label, "orchestrator, Claude Code, idle, resumable");
    }

    #[gpui::test]
    fn the_sidebar_does_not_call_a_running_agent_resumable(cx: &mut TestAppContext) {
        let (workspace, cx, _resumed) = crate::daemon::tests::resume_workspace(None, cx);
        let running = crate::daemon::tests::agent_info(1, 7, swarm_protocol::AgentStatus::Idle);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.agent_changed(&running, Some(window), cx);
        });

        let label = workspace.read_with(cx, |workspace, _| {
            agent_label(workspace.data.agents.first().expect("one agent"))
        });

        assert_eq!(label, "orchestrator, Claude Code, idle");
    }

    #[gpui::test]
    fn the_sidebar_buttons_end_where_the_project_rows_end(cx: &mut TestAppContext) {
        let (_, cx) = setup(cx);
        let mut right =
            |selector: &'static str| cx.debug_bounds(selector).map(|bounds| bounds.right());

        let row = right("project-row-1");

        assert!(row.is_some());
        for selector in ["sidebar-search", "add-project"] {
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

    /// A user terminal in the open task, with the focus on the orchestrator.
    fn with_user_terminal(
        workspace: &gpui::Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> crate::model::TerminalId {
        workspace.update_in(cx, Workspace::new_user_terminal);

        let (user, orchestrator) = workspace.read_with(cx, |workspace, _| {
            let task = workspace.active_task();
            let orchestrator = task
                .orchestrator
                .and_then(|id| workspace.data.agent(id))
                .map(|agent| agent.terminal);

            (task.user_terminals.last().copied(), orchestrator)
        });
        let (user, orchestrator) = (
            user.expect("a user terminal"),
            orchestrator.expect("an orchestrator"),
        );

        workspace.update(cx, |workspace, cx| {
            workspace.focus_terminal(orchestrator, cx);
        });
        redraw(cx);

        user
    }

    fn click_at(selector: &'static str, offset: Point<Pixels>, cx: &mut VisualTestContext) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"));

        cx.simulate_click(bounds.origin + offset, gpui::Modifiers::none());
        redraw(cx);
    }

    fn has_close_dialog(workspace: &gpui::Entity<Workspace>, cx: &VisualTestContext) -> bool {
        workspace.read_with(cx, |workspace, _| {
            matches!(workspace.overlay, Some(Overlay::CloseTerminal { .. }))
        })
    }

    #[gpui::test]
    fn the_user_terminal_row_has_a_close_button_that_asks_and_does_not_select_the_row(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        let user = with_user_terminal(&workspace, cx);
        let focused = workspace.read_with(cx, |workspace, _| workspace.focused_terminal);

        assert_eq!(user, crate::model::TerminalId(14));
        click_at("sidebar-close-terminal-14", point(px(12.), px(12.)), cx);

        assert!(has_close_dialog(&workspace, cx));
        assert_eq!(
            workspace.read_with(cx, |workspace, _| workspace.focused_terminal),
            focused
        );
    }

    #[gpui::test]
    fn clicking_the_user_terminal_row_selects_it_and_opens_no_dialog(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let user = with_user_terminal(&workspace, cx);

        click_at("user-row-14", point(px(20.), px(14.)), cx);

        assert_eq!(
            workspace.read_with(cx, |workspace, _| workspace.focused_terminal),
            Some(user)
        );
        assert!(!has_close_dialog(&workspace, cx));
    }

    #[gpui::test]
    fn a_process_row_has_no_close_button(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let process = workspace.read_with(cx, |workspace, _| {
            workspace.active_task().processes.first().copied()
        });

        assert_eq!(process, Some(crate::model::TerminalId(5)));
        assert!(cx.debug_bounds("process-row-5").is_some());
        assert!(cx.debug_bounds("sidebar-close-terminal-5").is_none());
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

    fn collapse(workspace: &gpui::Entity<Workspace>, cx: &mut VisualTestContext) {
        workspace.update_in(cx, Workspace::toggle_sidebar);
        redraw(cx);
    }

    #[gpui::test]
    fn the_agents_heading_shows_only_when_the_open_tab_has_rows(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        redraw(cx);
        let with_rows = shown("agents-heading", cx);

        workspace.update(cx, |workspace, cx| {
            workspace.select_project(ProjectId(3), cx);
        });
        redraw(cx);
        let rows = workspace.read_with(cx, |workspace, _| rows(workspace).len());

        assert!(with_rows);
        assert_eq!(rows, 0);
        assert!(!shown("agents-heading", cx));
    }

    #[gpui::test]
    fn the_rail_is_48_points_wide_and_the_card_starts_right_after_it(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        collapse(&workspace, cx);

        let rail = cx.debug_bounds("sidebar-rail");
        let card = cx.debug_bounds("main-card");

        assert_eq!(rail.map(|bounds| bounds.size.width), Some(px(48.)));
        assert!(card.is_some());
        assert_eq!(
            card.map(|bounds| bounds.left()),
            rail.map(|bounds| bounds.right())
        );
    }

    #[gpui::test]
    fn an_error_opens_the_collapsed_sidebar_and_shows_the_alert(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        collapse(&workspace, cx);
        let hidden = !shown("sidebar-error", cx);

        workspace.update(cx, |workspace, cx| {
            workspace.report_sidebar_error("not a git repository".to_owned(), cx);
        });
        redraw(cx);

        assert!(hidden);
        assert!(shown("sidebar-error", cx));
    }

    fn process_with_exit(exit: Option<swarm_term::ExitStatus>) -> Terminal {
        Terminal {
            id: crate::model::TerminalId(1),
            kind: crate::model::TerminalKind::Process,
            title: "cargo watch".into(),
            meta: None,
            owner: None,
            daemon_owner: None,
            link: None,
            exit,
            lines: Vec::new(),
        }
    }

    #[test]
    fn a_process_row_says_how_the_process_is_doing() {
        let label = |exit| process_label(&process_with_exit(exit), "pty-io");

        assert_eq!(label(None), "cargo watch, started by pty-io, working");
        assert_eq!(
            label(Some(swarm_term::ExitStatus::Code(0))),
            "cargo watch, started by pty-io, done"
        );
        assert_eq!(
            label(Some(swarm_term::ExitStatus::Code(1))),
            "cargo watch, started by pty-io, error"
        );
    }

    /// True when the row of the open project has the keyboard focus.
    fn open_project_row_has_focus(
        workspace: &gpui::Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> bool {
        let row = workspace.read_with(cx, |workspace, _| {
            workspace
                .project_focus
                .get(&workspace.active_project)
                .cloned()
        });

        row.is_some_and(|row| cx.update(|window, _| row.is_focused(window)))
    }

    fn click_center(selector: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx.debug_bounds(selector).expect("the control is on screen");

        cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    }

    #[gpui::test]
    fn dismissing_the_error_moves_the_focus_to_the_row_of_the_open_project(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.report_sidebar_error("not a git repository".to_owned(), cx);
        });
        redraw(cx);

        click_center("dismiss-error", cx);
        redraw(cx);
        redraw(cx);

        assert!(!shown("sidebar-error", cx));
        assert!(open_project_row_has_focus(&workspace, cx));
    }

    #[gpui::test]
    fn retrying_the_daemon_moves_the_focus_to_the_row_of_the_open_project(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.daemon_launch = Some(crate::daemon::tests::unreachable_launch());
            workspace.daemon_unavailable("no daemon".to_owned(), cx);
        });
        redraw(cx);

        click_center("retry-daemon", cx);
        workspace.update(cx, |workspace, cx| {
            workspace.source = DataSource::Connecting;
            cx.notify();
        });
        redraw(cx);
        redraw(cx);

        assert!(!shown("retry-daemon", cx));
        assert!(open_project_row_has_focus(&workspace, cx));
    }

    #[gpui::test]
    fn a_missing_daemon_shows_its_reason_as_visible_text(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.daemon_unavailable("the daemon did not stop".to_owned(), cx);
        });

        redraw(cx);

        assert!(shown("daemon-reason", cx));
    }

    #[gpui::test]
    fn a_running_daemon_shows_no_reason(cx: &mut TestAppContext) {
        let (_workspace, cx) = setup(cx);

        redraw(cx);

        assert!(!shown("daemon-reason", cx));
    }

    #[gpui::test]
    fn a_missing_daemon_opens_the_collapsed_sidebar_with_its_status(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        collapse(&workspace, cx);
        let hidden = !shown("daemon-status", cx);

        workspace.update(cx, |workspace, cx| {
            workspace.daemon_unavailable("no daemon".to_owned(), cx);
        });
        redraw(cx);

        assert!(hidden);
        assert!(shown("daemon-status", cx));
    }

    #[gpui::test]
    fn the_rail_lists_each_project_and_selects_one(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        collapse(&workspace, cx);
        let projects = workspace.read_with(cx, |workspace, _| workspace.data.projects.len());
        let before = workspace.read_with(cx, |workspace, _| workspace.active_project);

        let buttons = [
            "rail-project-0",
            "rail-project-1",
            "rail-project-2",
            "rail-project-3",
        ];
        let shown_all = buttons.iter().all(|selector| shown(selector, cx));
        click_at("rail-project-2", point(px(18.), px(18.)), cx);

        assert_eq!(projects, buttons.len());
        assert!(shown_all);
        assert_ne!(before, ProjectId(2));
        assert_eq!(
            workspace.read_with(cx, |workspace, _| workspace.active_project),
            ProjectId(2)
        );
    }

    #[gpui::test]
    fn the_rail_focuses_an_agent_terminal(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        collapse(&workspace, cx);
        let agent = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .agents
                .iter()
                .find(|agent| agent.name == "pty-io")
                .map(|agent| (agent.id, agent.terminal))
        });
        let (id, terminal) = agent.expect("the sample has a pty-io agent");
        let before = workspace.read_with(cx, |workspace, _| workspace.focused_terminal);

        assert_eq!(id, crate::model::AgentId(2));
        click_at("rail-agent-2", point(px(18.), px(18.)), cx);

        assert_ne!(before, Some(terminal));
        assert_eq!(
            workspace.read_with(cx, |workspace, _| workspace.focused_terminal),
            Some(terminal)
        );
    }

    #[gpui::test]
    fn the_rail_shows_user_terminals(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let user = with_user_terminal(&workspace, cx);
        collapse(&workspace, cx);
        let before = workspace.read_with(cx, |workspace, _| workspace.focused_terminal);

        assert_eq!(user, crate::model::TerminalId(14));
        assert!(shown("rail-user-14", cx));
        click_at("rail-user-14", point(px(18.), px(18.)), cx);

        assert_ne!(before, Some(user));
        assert_eq!(
            workspace.read_with(cx, |workspace, _| workspace.focused_terminal),
            Some(user)
        );
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

    fn shown(selector: &'static str, cx: &mut VisualTestContext) -> bool {
        cx.debug_bounds(selector).is_some()
    }

    /// The meters of the sample card, in order, with the selectors of their bar and tick.
    const METERS: [(&str, &str, &str); 4] = [
        (
            "usage-claude-0",
            "usage-claude-0-bar",
            "usage-claude-0-tick",
        ),
        (
            "usage-claude-1",
            "usage-claude-1-bar",
            "usage-claude-1-tick",
        ),
        ("usage-codex-0", "usage-codex-0-bar", "usage-codex-0-tick"),
        ("usage-codex-1", "usage-codex-1-bar", "usage-codex-1-tick"),
    ];

    /// How far along the bar the middle of the tick sits, from 0 to 1.
    fn tick_position(
        bar: &'static str,
        tick: &'static str,
        cx: &mut VisualTestContext,
    ) -> Option<f32> {
        let bar = cx.debug_bounds(bar)?;
        let tick = cx.debug_bounds(tick)?;

        Some(f32::from(tick.center().x - bar.origin.x) / f32::from(bar.size.width))
    }

    fn sample_sections(
        workspace: &gpui::Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Vec<UsageSection> {
        workspace.read_with(cx, |workspace, _| {
            usage_sections(&workspace.data.usage, workspace.now_ms() / 1_000)
        })
    }

    #[gpui::test]
    fn the_sample_card_shows_session_and_weekly_for_both_agent_kinds(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        assert_eq!(card_rows(&workspace, cx), (true, true, true));

        let windows: Vec<_> = sample_sections(&workspace, cx)
            .iter()
            .map(|section| {
                let names: Vec<_> = section
                    .windows
                    .iter()
                    .map(|window| window.name.as_str())
                    .collect();

                (section.kind.name(), section.plan.clone(), names.join("/"))
            })
            .collect();
        assert_eq!(
            windows,
            [
                ("Claude", None, "Session/Weekly".to_owned()),
                (
                    "Codex",
                    Some("Plus".to_owned()),
                    "Session/Weekly".to_owned()
                ),
            ]
        );
        for meter in [
            "usage-claude-0",
            "usage-claude-1",
            "usage-codex-0",
            "usage-codex-1",
        ] {
            assert!(shown(meter, cx), "{meter}");
        }
        assert!(!shown("usage-claude-2", cx));
        assert!(!shown("usage-codex-2", cx));
    }

    #[gpui::test]
    fn each_window_has_a_tick_where_even_use_would_be(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        card_rows(&workspace, cx);

        let paces: Vec<_> = sample_sections(&workspace, cx)
            .iter()
            .flat_map(|section| section.windows.iter().map(WindowRow::pace))
            .collect();

        assert_eq!(paces.len(), METERS.len());
        for ((name, bar, tick), pace) in METERS.into_iter().zip(paces) {
            let position = tick_position(bar, tick, cx);

            assert!(pace.is_some(), "{name} has a pace");
            assert!(
                position
                    .zip(pace)
                    .is_some_and(|(position, pace)| (position - pace).abs() < 0.01),
                "{name}: the tick is at {position:?}, not {pace:?}"
            );
        }
    }

    #[gpui::test]
    fn the_tick_moves_when_the_clock_does(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        card_rows(&workspace, cx);
        let (_, bar, tick) = METERS[0];
        let before = tick_position(bar, tick, cx);

        workspace.update(cx, |workspace, _| {
            workspace.fixed_now = Some(SAMPLE_NOW + 3_600_000);
        });
        card_rows(&workspace, cx);
        let after = tick_position(bar, tick, cx);

        // The session lasts five hours: one more hour is a fifth further.
        assert!(
            before
                .zip(after)
                .is_some_and(|(before, after)| (after - before - 0.2).abs() < 0.01),
            "{before:?} -> {after:?}"
        );
    }

    #[gpui::test]
    fn only_the_window_that_would_hit_the_limit_early_turns_warn(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let tones: Vec<_> = sample_sections(&workspace, cx)
            .iter()
            .flat_map(|section| {
                section
                    .windows
                    .iter()
                    .map(|window| (section.kind.slug(), window.name.clone(), window.tone()))
                    .collect::<Vec<_>>()
            })
            .collect();

        assert_eq!(
            tones,
            [
                ("claude", "Session".to_owned(), UsageTone::Normal),
                ("claude", "Weekly".to_owned(), UsageTone::Warn),
                ("codex", "Session".to_owned(), UsageTone::Normal),
                ("codex", "Weekly".to_owned(), UsageTone::Normal),
            ]
        );
    }

    /// The fill color of a meter, for the normal and the warn tone.
    fn fill_colors(cx: &mut VisualTestContext) -> (Hsla, Hsla) {
        cx.update(|_, cx| {
            let colors = cx.theme().colors();

            (colors.fg, colors.warn)
        })
    }

    #[gpui::test]
    fn only_the_window_that_warns_draws_a_warn_fill_and_an_alert_icon(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        card_rows(&workspace, cx);
        let (normal, warn) = fill_colors(cx);

        let fills = [
            "usage-claude-0-fill",
            "usage-claude-1-fill",
            "usage-codex-0-fill",
            "usage-codex-1-fill",
        ]
        .map(|selector| drawn_color(selector, cx));
        let alerts = [
            "usage-claude-0-alert",
            "usage-claude-1-alert",
            "usage-codex-0-alert",
            "usage-codex-1-alert",
        ]
        .map(|selector| shown(selector, cx));

        assert_eq!(
            fills,
            [Some(normal), Some(warn), Some(normal), Some(normal)]
        );
        assert_eq!(alerts, [false, true, false, false]);
    }

    #[gpui::test]
    fn a_full_window_draws_a_warn_fill_and_an_alert_icon(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| {
            let mut usage = workspace.data.usage.clone();
            usage.codex.windows[0].used_percent = 100;
            workspace.usage_changed(usage, cx);
        });
        card_rows(&workspace, cx);
        let (_, warn) = fill_colors(cx);

        assert_eq!(drawn_color("usage-codex-0-fill", cx), Some(warn));
        assert!(shown("usage-codex-0-alert", cx));
        assert_eq!(
            sample_sections(&workspace, cx)[1].windows[0]
                .risk
                .as_deref(),
            Some("Limit reached")
        );
    }

    #[gpui::test]
    fn the_tick_is_cut_out_of_a_warn_fill_in_the_color_of_the_card(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        card_rows(&workspace, cx);
        let panel = cx.update(|_, cx| cx.theme().colors().panel);

        assert_eq!(drawn_color("usage-claude-1-notch", cx), Some(panel));
        assert!(!shown("usage-claude-0-notch", cx));
    }

    #[gpui::test]
    fn the_age_of_old_numbers_shows_next_to_the_name_of_the_kind(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        card_rows(&workspace, cx);

        // Codex was read 14 minutes before the sample time, Claude one minute.
        assert!(shown("usage-codex-age", cx));
        assert!(!shown("usage-claude-age", cx));

        workspace.update(cx, |workspace, _| {
            workspace.fixed_now = Some(SAMPLE_NOW + 10 * 60_000);
        });
        card_rows(&workspace, cx);

        assert!(shown("usage-claude-age", cx));
    }

    #[gpui::test]
    fn the_plan_shows_only_when_the_agent_kind_has_one(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        card_rows(&workspace, cx);

        assert!(!shown("usage-claude-plan", cx));
        assert!(shown("usage-codex-plan", cx));

        workspace.update(cx, |workspace, cx| {
            let mut usage = workspace.data.usage.clone();
            usage.claude.plan = Some("max".to_owned());
            usage.codex.plan = None;
            workspace.usage_changed(usage, cx);
        });
        card_rows(&workspace, cx);

        assert!(shown("usage-claude-plan", cx));
        assert!(!shown("usage-codex-plan", cx));
    }

    #[gpui::test]
    fn a_window_without_a_length_has_a_bar_and_no_tick(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| {
            let mut usage = workspace.data.usage.clone();
            for window in &mut usage.claude.windows {
                window.length_secs = None;
            }
            workspace.usage_changed(usage, cx);
        });
        card_rows(&workspace, cx);

        assert!(shown("usage-claude-1-bar", cx));
        assert!(!shown("usage-claude-1-tick", cx));
        assert!(shown("usage-codex-1-tick", cx));
        assert!(
            sample_sections(&workspace, cx)[0]
                .windows
                .iter()
                .all(|window| window.tone() == UsageTone::Normal)
        );
    }

    fn label_of(meter: UsageMeter, cx: &mut VisualTestContext) -> Option<String> {
        use gpui::accesskit::Node;
        use gpui::{Element as _, IntoElement as _, RenderOnce as _};

        cx.update(|window, cx| {
            let element = meter.render(window, cx).into_element();
            let mut node = Node::new(Role::Unknown);
            element.write_a11y_info(&mut node);

            node.label().map(str::to_owned)
        })
    }

    #[gpui::test]
    fn each_window_has_one_label_with_its_percent_risk_and_reset(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let sections = sample_sections(&workspace, cx);
        let group = SharedString::from("usage-claude");

        let labels: Vec<_> = sections[0]
            .windows
            .iter()
            .enumerate()
            .map(|(index, window)| label_of(usage_meter(&group, index, window), cx))
            .collect();

        assert_eq!(
            labels,
            [
                Some("Claude session 42% used, resets in 2 hours 6 minutes".to_owned()),
                Some(
                    "Claude weekly 61% used, limit in 1 day 23 hours, resets in 3 days 21 hours"
                        .to_owned()
                ),
            ]
        );
    }

    #[gpui::test]
    fn a_group_is_labelled_with_its_kind_and_plan(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let labels: Vec<_> = sample_sections(&workspace, cx)
            .iter()
            .map(UsageSection::label)
            .collect();

        assert_eq!(
            labels,
            [
                "Claude usage",
                "Codex usage, Plus plan, updated 14 minutes ago"
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

    fn counting<A: gpui::Action>(cx: &mut VisualTestContext) -> Rc<std::cell::Cell<usize>> {
        let count = Rc::new(std::cell::Cell::new(0));
        let seen = Rc::clone(&count);

        cx.update(|_, cx| {
            cx.on_action(move |_: &A, _| seen.set(seen.get() + 1));
        });

        count
    }

    fn resize_to_height(
        height: f32,
        workspace: &gpui::Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) {
        cx.simulate_resize(gpui::size(px(1_000.), px(height)));
        workspace.update(cx, |_, cx| cx.notify());
        redraw(cx);
    }

    fn top_of(selector: &'static str, cx: &mut VisualTestContext) -> Pixels {
        cx.debug_bounds(selector)
            .map_or(Pixels::MAX, |bounds| bounds.top())
    }

    #[gpui::test]
    fn the_user_terminal_row_shows_the_lock_icon_and_the_mine_pill(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        with_user_terminal(&workspace, cx);
        redraw(cx);

        let row = cx.debug_bounds("user-row-14").expect("the row is drawn");
        let pill = cx.debug_bounds("user-mine-14").expect("the pill is drawn");

        assert!(row.contains(&pill.center()));
    }

    #[gpui::test]
    fn the_mine_pill_comes_after_the_name_and_before_the_close_button(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        with_user_terminal(&workspace, cx);
        redraw(cx);

        let pill = cx.debug_bounds("user-mine-14").expect("the pill is drawn");
        let close = cx
            .debug_bounds("sidebar-close-terminal-14")
            .expect("the close button is drawn");

        assert!(pill.right() < close.left());
    }

    #[gpui::test]
    fn the_user_terminal_row_is_labelled_as_mine_with_no_access_for_agents(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        let user = with_user_terminal(&workspace, cx);

        let label = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .terminal(user)
                .map(|terminal| (terminal.title.to_string(), user_label(workspace, terminal)))
        });

        let (title, label) = label.expect("the user terminal exists");
        assert_eq!(
            label,
            format!("{title}, your terminal 1, mine, agents have no access")
        );
    }

    #[gpui::test]
    fn the_footer_shows_the_usage_card_a_divider_then_the_three_places_in_order(
        cx: &mut TestAppContext,
    ) {
        let (_, cx) = setup(cx);
        redraw(cx);

        let card = cx.debug_bounds("usage-card").expect("the card is drawn");
        let tops: Vec<Pixels> = [
            "sidebar-divider",
            "footer-settings",
            "footer-field-guide",
            "footer-report-issue",
        ]
        .map(|selector| top_of(selector, cx))
        .to_vec();

        assert!(card.bottom() <= tops[0]);
        assert!(tops.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[gpui::test]
    fn the_footer_stays_at_the_bottom_when_the_list_is_short(cx: &mut TestAppContext) {
        let (_, cx) = setup(cx);
        redraw(cx);

        let sidebar = cx.debug_bounds("sidebar-column").expect("the sidebar");
        let last = cx
            .debug_bounds("footer-report-issue")
            .expect("the last row");

        assert!(sidebar.bottom() - last.bottom() < px(24.));
    }

    #[test]
    fn the_field_guide_row_says_how_many_creatures_were_caught() {
        assert_eq!(caught_text(7), "7/20");
        assert_eq!(
            FooterLink::FieldGuide.spoken(7),
            "Field guide, 7 of 20 caught"
        );
        assert_eq!(FooterLink::Settings.spoken(7), "Settings");
    }

    #[gpui::test]
    fn the_field_guide_row_shows_the_count_of_the_caught_creatures(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.data.caught_creatures = vec!["a".to_owned(), "b".to_owned()];
            cx.notify();
        });
        redraw(cx);

        let spoken = workspace.read_with(cx, |workspace, _| {
            FooterLink::FieldGuide.spoken(workspace.data.caught_creatures.len())
        });

        assert!(shown("footer-field-guide-count", cx));
        assert_eq!(spoken, "Field guide, 2 of 20 caught");
    }

    #[gpui::test]
    fn the_field_guide_row_dispatches_open_field_guide(cx: &mut TestAppContext) {
        let (_, cx) = setup(cx);
        redraw(cx);
        let opened = counting::<OpenFieldGuide>(cx);

        click_center("footer-field-guide", cx);

        assert_eq!(opened.get(), 1);
    }

    #[gpui::test]
    fn the_report_an_issue_row_dispatches_report_issue(cx: &mut TestAppContext) {
        let (_, cx) = setup(cx);
        redraw(cx);
        let reported = counting::<ReportIssue>(cx);

        click_center("footer-report-issue", cx);

        assert_eq!(reported.get(), 1);
    }

    #[gpui::test]
    fn the_settings_row_opens_settings(cx: &mut TestAppContext) {
        let (_, cx) = crate::daemon::tests::setup(cx);
        redraw(cx);

        click_center("footer-settings", cx);
        redraw(cx);

        assert!(cx.update(|_, cx| crate::settings::shown_page(cx)).is_some());
    }

    #[gpui::test]
    fn enter_on_a_focused_footer_row_does_what_a_click_does(cx: &mut TestAppContext) {
        let (workspace, cx) = keyed_setup(cx);
        redraw(cx);
        let reported = counting::<ReportIssue>(cx);

        tab_to("ListItem Report an issue", &workspace, cx);
        press("enter", cx);

        assert_eq!(reported.get(), 1);
    }

    /// A key press with its release, which is what activates a focused control.
    fn press(key: &'static str, cx: &mut VisualTestContext) {
        cx.simulate_keystrokes(key);

        if let Ok(keystroke) = gpui::Keystroke::parse(key) {
            cx.simulate_event(gpui::KeyUpEvent { keystroke });
        }
    }

    /// A workspace whose key bindings are in place, so Tab and Space work.
    fn keyed_setup(cx: &mut TestAppContext) -> (gpui::Entity<Workspace>, &mut VisualTestContext) {
        crate::daemon::tests::setup(cx)
    }

    /// Presses Tab from the start of the window until the stop is called `name`, or starts
    /// with it.
    fn tab_to(name: &str, workspace: &gpui::Entity<Workspace>, cx: &mut VisualTestContext) {
        let root = workspace.read_with(cx, |workspace, _| workspace.sidebar_focus.clone());
        cx.update(|window, cx| window.focus(&root, cx));

        for _ in 0..60 {
            cx.simulate_keystrokes("tab");

            if swarm_ui::test_support::focused_stop(cx, &root).starts_with(name) {
                return;
            }
        }

        panic!("Tab never reached {name}");
    }

    #[test]
    fn a_window_below_700_points_folds_the_usage_card() {
        assert!(usage_can_fold(px(699.)));
        assert!(!usage_can_fold(px(700.)));
    }

    #[gpui::test]
    fn a_tall_window_shows_the_card_and_no_fold_button(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        resize_to_height(800., &workspace, cx);

        assert!(shown("usage-card", cx));
        assert!(!shown("usage-fold", cx));
    }

    #[gpui::test]
    fn a_short_window_folds_the_card_to_one_line(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        resize_to_height(600., &workspace, cx);

        assert!(!shown("usage-card", cx));
        assert!(shown("usage-fold", cx));
        assert!(shown("footer-settings", cx));
    }

    #[gpui::test]
    fn clicking_the_fold_line_opens_the_card_and_clicking_again_folds_it(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        resize_to_height(600., &workspace, cx);

        click_center("usage-fold", cx);
        redraw(cx);
        let opened = shown("usage-card", cx);

        click_center("usage-fold", cx);
        redraw(cx);

        assert!(opened);
        assert!(!shown("usage-card", cx));
    }

    #[gpui::test]
    fn space_on_the_focused_fold_line_toggles_the_card(cx: &mut TestAppContext) {
        let (workspace, cx) = keyed_setup(cx);
        resize_to_height(600., &workspace, cx);
        tab_to("Button Usage", &workspace, cx);

        press("space", cx);
        redraw(cx);
        let opened = shown("usage-card", cx);

        press("space", cx);
        redraw(cx);

        assert!(opened);
        assert!(!shown("usage-card", cx));
    }

    #[gpui::test]
    fn the_choice_to_open_the_card_stays_when_the_window_grows_and_shrinks(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        resize_to_height(600., &workspace, cx);
        click_center("usage-fold", cx);

        resize_to_height(900., &workspace, cx);
        resize_to_height(600., &workspace, cx);

        assert!(shown("usage-card", cx));
    }

    #[gpui::test]
    fn the_fold_line_shows_the_highest_window_of_each_agent_kind(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let line = workspace.read_with(cx, |workspace, _| {
            let sections = usage_sections(&workspace.data.usage, workspace.now_ms() / 1_000);

            highest_per_kind(&sections)
                .into_iter()
                .map(|(kind, window)| format!("{} {}%", kind.name(), window.percent))
                .collect::<Vec<_>>()
        });

        assert_eq!(line, ["Claude 61%", "Codex 18%"]);
    }

    fn fold_line_a11y(
        workspace: &gpui::Entity<Workspace>,
        open: bool,
        cx: &mut VisualTestContext,
    ) -> (Option<Role>, Option<String>, Option<bool>) {
        use gpui::accesskit::Node;
        use gpui::{Element as _, IntoElement as _};

        let sections = workspace.read_with(cx, |workspace, _| {
            usage_sections(&workspace.data.usage, workspace.now_ms() / 1_000)
        });

        workspace.update(cx, |_, cx| {
            let element = usage_summary(&sections, open, cx).into_element();
            let mut node = Node::new(Role::Unknown);
            element.write_a11y_info(&mut node);

            (
                element.a11y_role(),
                node.label().map(str::to_owned),
                node.is_expanded(),
            )
        })
    }

    #[gpui::test]
    fn the_fold_line_is_a_button_that_says_whether_the_card_is_open(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let (role, _, folded) = fold_line_a11y(&workspace, false, cx);
        let (_, _, opened) = fold_line_a11y(&workspace, true, cx);

        assert_eq!(role, Some(Role::Button));
        assert_eq!(folded, Some(false));
        assert_eq!(opened, Some(true));
    }

    #[gpui::test]
    fn the_fold_line_is_read_with_every_window_in_both_states(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let (_, folded, _) = fold_line_a11y(&workspace, false, cx);
        let (_, opened, _) = fold_line_a11y(&workspace, true, cx);
        let folded = folded.unwrap_or_default();

        for window in [
            "Claude session",
            "Claude weekly",
            "Codex session",
            "Codex weekly",
        ] {
            assert!(folded.contains(window), "{window}: {folded}");
        }
        assert_eq!(Some(folded), opened);
    }

    #[gpui::test]
    fn the_rail_shows_a_ring_for_the_highest_window_and_the_three_footer_buttons(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        collapse(&workspace, cx);

        for selector in [
            "rail-usage",
            "rail-settings",
            "rail-field-guide",
            "rail-report-issue",
        ] {
            assert!(shown(selector, cx), "{selector}");
        }
    }

    #[gpui::test]
    fn the_rail_has_the_ring_above_the_footer_buttons(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        collapse(&workspace, cx);

        let tops: Vec<Pixels> = [
            "rail-usage",
            "rail-settings",
            "rail-field-guide",
            "rail-report-issue",
        ]
        .map(|selector| top_of(selector, cx))
        .to_vec();

        assert!(tops.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[gpui::test]
    fn the_rail_leaves_out_the_ring_without_usage_numbers_and_keeps_the_buttons(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.usage_changed(Usage::default(), cx);
        });
        collapse(&workspace, cx);

        assert!(!shown("rail-usage", cx));
        assert!(shown("rail-settings", cx));
    }

    #[gpui::test]
    fn without_usage_numbers_the_sidebar_leaves_out_the_card_and_keeps_the_footer(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.usage_changed(Usage::default(), cx);
        });
        resize_to_height(600., &workspace, cx);

        assert!(!shown("usage-card", cx));
        assert!(!shown("usage-fold", cx));
        assert!(shown("sidebar-divider", cx));
        assert!(shown("footer-settings", cx));
    }

    #[gpui::test]
    fn the_rail_footer_buttons_dispatch_their_actions(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        collapse(&workspace, cx);
        let guide = counting::<OpenFieldGuide>(cx);
        let report = counting::<ReportIssue>(cx);

        click_center("rail-field-guide", cx);
        click_center("rail-report-issue", cx);

        assert_eq!((guide.get(), report.get()), (1, 1));
    }

    #[gpui::test]
    fn the_rail_ring_is_read_with_the_highest_window_and_every_window(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let label = workspace.read_with(cx, |workspace, _| {
            let sections = usage_sections(&workspace.data.usage, workspace.now_ms() / 1_000);

            highest_window(&sections)
                .map(|(kind, window)| ring_label(kind, window, &sections))
                .unwrap_or_default()
        });

        assert!(label.starts_with("Usage, highest window Claude 61 percent. "));
        assert!(label.contains("Codex weekly"));
    }

    #[gpui::test]
    fn the_rail_ring_tooltip_lists_every_window(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let tooltip = workspace.read_with(cx, |workspace, _| {
            shown_usage(&usage_sections(
                &workspace.data.usage,
                workspace.now_ms() / 1_000,
            ))
        });

        assert_eq!(
            tooltip,
            "Claude Session 42%, Weekly 61% \u{b7} Codex Session 18%, Weekly 9%"
        );
    }

    #[gpui::test]
    fn the_sidebar_keeps_the_footer_in_view_while_the_projects_scroll(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        resize_to_height(400., &workspace, cx);

        let footer = cx.debug_bounds("footer-report-issue");

        assert!(footer.is_some_and(|footer| footer.bottom() <= px(400.)));
    }

    #[gpui::test]
    fn not_installed_hides_only_that_group_of_the_card(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        workspace.update(cx, |workspace, cx| {
            let mut usage = workspace.data.usage.clone();
            usage.codex = SubscriptionUsage {
                problem: Some(UsageProblem::NotInstalled),
                ..SubscriptionUsage::default()
            };
            workspace.usage_changed(usage, cx);
        });

        assert_eq!(card_rows(&workspace, cx), (true, true, false));
    }

    #[gpui::test]
    fn a_problem_shows_under_the_group_and_keeps_the_old_numbers(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        workspace.update(cx, |workspace, cx| {
            let mut usage = workspace.data.usage.clone();
            usage.claude.problem = Some(UsageProblem::NotLoggedIn);
            usage.codex = SubscriptionUsage {
                problem: Some(UsageProblem::NotLoggedIn),
                ..SubscriptionUsage::default()
            };
            workspace.usage_changed(usage, cx);
        });
        card_rows(&workspace, cx);

        assert!(shown("usage-claude-problem", cx));
        assert!(shown("usage-claude-1", cx));
        assert!(shown("usage-codex-problem", cx));
        assert!(!shown("usage-codex-0", cx));
    }

    #[gpui::test]
    fn an_event_replaces_the_shown_numbers(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        workspace.update(cx, |workspace, cx| {
            let usage = Usage {
                claude: SubscriptionUsage {
                    windows: vec![UsageWindow {
                        label: "5h".to_owned(),
                        used_percent: 64,
                        resets_at: None,
                        length_secs: None,
                    }],
                    ..SubscriptionUsage::default()
                },
                ..Usage::default()
            };
            workspace.usage_changed(usage, cx);
        });
        card_rows(&workspace, cx);

        let percents: Vec<_> = sample_sections(&workspace, cx)
            .iter()
            .flat_map(|section| section.windows.iter().map(|window| window.percent))
            .collect();
        assert_eq!(percents, [64]);
        assert!(shown("usage-claude-0", cx));
        assert!(!shown("usage-claude-1", cx));
        assert!(!shown("usage-codex", cx));
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
    fn show_in_finder_reveals_the_folder_of_the_project(cx: &mut TestAppContext) {
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
    fn show_in_finder_is_off_for_a_project_without_a_folder(cx: &mut TestAppContext) {
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

        // Border and padding take 6 px; Show in Finder is the first item, 34 px high.
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
            Some(Page::Project(ProjectId(1)))
        );
        assert!(!menu_is_open(cx));
    }

    #[gpui::test]
    fn project_settings_of_a_project_added_while_settings_is_open_shows_its_page(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = menu_setup(cx);
        workspace.update(cx, |workspace, cx| workspace.open_settings(None, cx));
        settle(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.data.projects.push(Project {
                id: ProjectId(4),
                name: "added".into(),
                letter: 'A',
                tile_color: gpui::rgb(0x0058_90e0).into(),
                base_branch: "main".into(),
                daemon: None,
                root: Some(PathBuf::from("/tmp/added")),
                config_error: None,
            });
            cx.notify();
        });
        settle(cx);

        let pointer = bounds_of("project-row-4", cx).center();
        right_click(pointer, cx);
        cx.simulate_keystrokes("down enter");
        settle(cx);

        assert_eq!(
            cx.update(|_, cx| crate::settings::shown_page(cx)),
            Some(Page::Project(ProjectId(4)))
        );
        let window = cx
            .windows()
            .into_iter()
            .find_map(|window| window.downcast::<crate::settings::SettingsWindow>())
            .expect("the settings window is open");
        let mut settings = VisualTestContext::from_window(window.into(), cx);
        settle(&mut settings);
        assert!(
            settings.debug_bounds("remove-project").is_some(),
            "the page of the new project is empty"
        );
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

    mod a11y {
        use gpui::Stateful;
        use gpui::accesskit::Role as NodeRole;

        use super::*;
        use crate::live_probe::{Seen, see_in};

        fn heading_of(
            label: Label,
            cx: &mut VisualTestContext,
        ) -> (Option<NodeRole>, Option<usize>) {
            cx.update(|window, cx| {
                let mut element = label.render(window, cx).into_any_element();
                let node = element
                    .downcast_mut::<Stateful<gpui::Div>>()
                    .expect("a heading is a node");

                (node.a11y_role(), {
                    let mut data = gpui::accesskit::Node::new(NodeRole::Unknown);
                    node.write_a11y_info(&mut data);
                    data.level()
                })
            })
        }

        fn seen(role: NodeRole, live: Live, value: &str) -> Seen {
            Seen {
                role: Some(role),
                label: None,
                value: Some(value.to_owned()),
                live: Some(live),
                heading_level: None,
                read_only: false,
            }
        }

        #[gpui::test]
        fn the_projects_and_agents_headings_are_level_two_headings(cx: &mut TestAppContext) {
            let (_, cx) = setup(cx);

            let projects = heading_of(heading_label("projects-heading", "Projects"), cx);
            let agents = heading_of(heading_label("agents-heading", "Agents"), cx);

            assert_eq!(projects, (Some(NodeRole::Heading), Some(2)));
            assert_eq!(agents, (Some(NodeRole::Heading), Some(2)));
        }

        #[gpui::test]
        fn connecting_is_a_polite_status(cx: &mut TestAppContext) {
            let (workspace, cx) = setup(cx);
            workspace.update(cx, |workspace, _| workspace.source = DataSource::Connecting);

            let region = workspace.update(cx, |workspace, cx| daemon_status(workspace, cx));
            let region = region.expect("a status while connecting");

            assert_eq!(
                see_in(region, cx),
                seen(NodeRole::Status, Live::Polite, "Connecting")
            );
        }

        #[gpui::test]
        fn a_missing_daemon_is_an_assertive_alert_with_the_reason(cx: &mut TestAppContext) {
            let (workspace, cx) = setup(cx);
            workspace.update(cx, |workspace, _| {
                workspace.source = DataSource::Unavailable {
                    reason: "no daemon".into(),
                };
            });

            let region = workspace.update(cx, |workspace, cx| daemon_status(workspace, cx));
            let region = region.expect("an alert without a daemon");

            assert_eq!(
                see_in(region, cx),
                seen(
                    NodeRole::Alert,
                    Live::Assertive,
                    "Daemon not running: no daemon"
                )
            );
        }

        #[gpui::test]
        fn the_sidebar_error_is_an_assertive_alert(cx: &mut TestAppContext) {
            let (_, cx) = setup(cx);

            let region = error_region(&SharedString::from("not a git repository"));

            assert_eq!(
                see_in(region, cx),
                seen(NodeRole::Alert, Live::Assertive, "not a git repository")
            );
        }

        #[gpui::test]
        fn a_usage_problem_is_a_polite_status_with_the_spoken_text(cx: &mut TestAppContext) {
            let (_, cx) = setup(cx);

            let region = problem_region(
                "usage-claude-problem".to_owned(),
                "Not signed in",
                "Claude Code is not signed in",
            );

            assert_eq!(
                see_in(region, cx),
                seen(
                    NodeRole::Status,
                    Live::Polite,
                    "Claude Code is not signed in"
                )
            );
        }

        /// The workspace with the list focus handles synced, as after a frame.
        fn list_counts(
            workspace: &gpui::Entity<Workspace>,
            cx: &mut VisualTestContext,
        ) -> ((usize, usize), (usize, usize)) {
            redraw(cx);

            workspace.read_with(cx, |workspace, _| {
                let stops = |roving: &RovingFocus| {
                    (0..roving.len())
                        .filter(|index| roving.handle(*index).tab_stop)
                        .count()
                };

                (
                    (
                        workspace.project_roving.len(),
                        stops(&workspace.project_roving),
                    ),
                    (workspace.agent_roving.len(), stops(&workspace.agent_roving)),
                )
            })
        }

        #[gpui::test]
        fn focus_that_lands_on_the_collapsed_sidebar_goes_to_a_rail_button(
            cx: &mut TestAppContext,
        ) {
            let (workspace, cx) = setup(cx);
            collapse(&workspace, cx);
            redraw(cx);

            let landing = workspace.read_with(cx, |workspace, _| {
                workspace.sidebar_landing(workspace.sidebar_focus.clone())
            });
            cx.update(|window, cx| landing.focus(window, cx));
            let root = workspace.read_with(cx, |workspace, _| workspace.sidebar_focus.clone());

            let stop = swarm_ui::test_support::focused_stop(cx, &root);

            assert!(stop.starts_with("Button swarm"), "{stop}");
        }

        #[gpui::test]
        fn each_list_has_one_tab_stop(cx: &mut TestAppContext) {
            let (workspace, cx) = setup(cx);

            let ((projects, project_stops), (agents, agent_stops)) = list_counts(&workspace, cx);

            assert!(projects > 1);
            assert!(agents > 1);
            assert_eq!((project_stops, agent_stops), (1, 1));
        }

        #[gpui::test]
        fn a_user_terminal_adds_no_tab_stop_to_the_agent_list(cx: &mut TestAppContext) {
            cx.update(|cx| {
                swarm_ui::init(cx);
                crate::actions::bind_keys(cx);
            });
            let (workspace, cx) = setup(cx);
            with_user_terminal(&workspace, cx);
            let first = workspace.read_with(cx, |workspace, _| workspace.agent_roving.handle(0));
            cx.update(|window, cx| first.focus(window, cx));
            redraw(cx);
            let root = workspace.read_with(cx, |workspace, _| workspace.sidebar_focus.clone());

            let ((_, _), (agents, agent_stops)) = list_counts(&workspace, cx);
            let walk = swarm_ui::test_support::tab_walk(cx, &root, "tab");

            assert!(agents > 1);
            assert_eq!(agent_stops, 1);
            assert!(
                walk.iter().all(|stop| !stop.contains("Close")),
                "a close button is a Tab stop: {walk:?}"
            );
        }

        fn focus_project(
            workspace: &gpui::Entity<Workspace>,
            index: usize,
            cx: &mut VisualTestContext,
        ) {
            let handle =
                workspace.read_with(cx, |workspace, _| workspace.project_roving.handle(index));

            cx.update(|window, cx| handle.focus(window, cx));
            redraw(cx);
        }

        fn focused_project(
            workspace: &gpui::Entity<Workspace>,
            cx: &mut VisualTestContext,
        ) -> Option<usize> {
            cx.update(|window, cx| {
                workspace.read_with(cx, |workspace, cx| {
                    workspace.project_roving.focused(window, cx)
                })
            })
        }

        #[gpui::test]
        fn the_arrows_home_and_end_move_through_the_projects(cx: &mut TestAppContext) {
            let (workspace, cx) = setup(cx);
            redraw(cx);
            focus_project(&workspace, 0, cx);

            cx.simulate_keystrokes("down");
            let after_down = focused_project(&workspace, cx);
            cx.simulate_keystrokes("end");
            let after_end = focused_project(&workspace, cx);
            cx.simulate_keystrokes("up");
            let after_up = focused_project(&workspace, cx);
            cx.simulate_keystrokes("home");
            let after_home = focused_project(&workspace, cx);

            let last = workspace.read_with(cx, |workspace, _| workspace.data.projects.len() - 1);
            assert_eq!(after_down, Some(1));
            assert_eq!(after_end, Some(last));
            assert_eq!(after_up, Some(last - 1));
            assert_eq!(after_home, Some(0));
        }

        #[gpui::test]
        fn the_arrows_move_through_the_agents(cx: &mut TestAppContext) {
            let (workspace, cx) = setup(cx);
            redraw(cx);
            let first = workspace.read_with(cx, |workspace, _| workspace.agent_roving.handle(0));
            cx.update(|window, cx| first.focus(window, cx));
            redraw(cx);

            cx.simulate_keystrokes("down down");

            let focused = cx.update(|window, cx| {
                workspace.read_with(cx, |workspace, cx| {
                    workspace.agent_roving.focused(window, cx)
                })
            });
            assert_eq!(focused, Some(2));
        }

        #[gpui::test]
        fn clicking_a_project_makes_it_the_tab_stop_of_the_list(cx: &mut TestAppContext) {
            let (workspace, cx) = setup(cx);
            redraw(cx);

            click_center("project-row-2", cx);
            redraw(cx);

            let (active, stop) = workspace.read_with(cx, |workspace, _| {
                (
                    workspace.project_roving.active(),
                    workspace.project_roving.handle(2).tab_stop,
                )
            });
            assert_eq!(active, 2);
            assert!(stop);
        }

        #[gpui::test]
        fn removing_the_focused_project_keeps_focus_on_a_drawn_row(cx: &mut TestAppContext) {
            let (workspace, cx) = setup(cx);
            redraw(cx);
            focus_project(&workspace, 1, cx);

            workspace.update(cx, |workspace, cx| {
                workspace.data.projects.remove(1);
                cx.notify();
            });
            redraw(cx);
            redraw(cx);

            let on_row = cx.update(|window, cx| {
                workspace.read_with(cx, |workspace, cx| {
                    workspace.project_roving.focused(window, cx).is_some()
                        || workspace.agent_roving.focused(window, cx).is_some()
                })
            });
            assert!(on_row);
        }
    }
}
