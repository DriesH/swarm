//! The main area: terminals, swarm and review views.

use gpui::{
    AnyElement, AppContext as _, Bounds, Context, Entity, FontWeight, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, Pixels, Role, SharedString, Size, Subscription, Window, div,
    px,
};
use swarm_layout::{Activity, Layout, LayoutStyle, Rect, TileId, TileKind, TileSpec};
use swarm_term_view::TerminalView;
use swarm_ui::fonts::MONO_FONT;
use swarm_ui::prelude::*;
use swarm_ui::styles::focus_ring;
use swarm_ui::{FocusStyle, TextFieldSize, TileRing};

use crate::daemon::{CHROME_HEIGHT, CHROME_WIDTH, exit_failed};
use crate::model::{Agent, AgentKind, FakeLine, Terminal, TerminalId, TerminalKind};
use crate::workspace::{DataSource, View, Workspace};

/// The padding a terminal view leaves around its grid.
const VIEW_PADDING: f32 = 8.;
const CHAR_WIDTH: f32 = 7.2;
const ROW_HEIGHT: f32 = 17.;
const HANDLE_HEIGHT: f32 = 6.;
/// The rows of the terminal a small tile shows.
const SMALL_TILE_ROWS: u16 = 12;
/// The rows of the terminal a process tile shows.
const PROCESS_TILE_ROWS: u16 = 8;
const KEY_STEP: f32 = 20.;
const MIN_TILE: Size<Pixels> = Size {
    width: px(260.),
    height: px(160.),
};
const FIRST_FRAME_AREA: Size<Pixels> = Size {
    width: px(1030.),
    height: px(790.),
};
const START_MAX_WIDTH: f32 = 880.;
const START_PADDING: (f32, f32) = (24., 28.);
const DEFAULT_USER_SIZE: Size<Pixels> = Size {
    width: px(650.),
    height: px(470.),
};

/// A drag on a user terminal's bottom edge.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Resizing {
    terminal: TerminalId,
    start_y: Pixels,
    start_height: Pixels,
}

pub(crate) fn render(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    if workspace.has_no_projects() {
        return empty_state(workspace, cx).into_any_element();
    }

    match workspace.view {
        View::Terminals => terminals(workspace, cx).into_any_element(),
        View::Swarm => crate::swarm_view::render(workspace, window, cx),
        View::Review => workspace
            .review
            .clone()
            .cached(crate::swarm_view::full_size())
            .into_any_element(),
    }
}

fn empty_state(workspace: &Workspace, cx: &mut Context<Workspace>) -> impl IntoElement + use<> {
    let live = workspace.source == DataSource::Live;

    v_flex()
        .id("empty-state")
        .role(Role::Group)
        .aria_label("No projects")
        .relative()
        .size_full()
        .items_center()
        .justify_center()
        .gap(px(12.))
        .when(crate::settings::easter_eggs(cx), |state| {
            state.child(honeycomb_behind(workspace))
        })
        .child(Label::new("No projects").color(Color::Faint))
        .child(
            Button::new("empty-add-project", "Add project")
                .debug_selector(|| "empty-add-project".to_owned())
                .icon(IconName::Plus)
                .disabled(!live)
                .on_click(cx.listener(|_, _, window, cx| Workspace::pick_project(window, cx))),
        )
}

/// The faint honeycomb of the empty states: it fills the parent and sits under its content.
fn honeycomb_behind(workspace: &Workspace) -> impl IntoElement + use<> {
    div()
        .debug_selector(|| "honeycomb".to_owned())
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .child(workspace.honeycomb.clone().cached(Honeycomb::cache_style()))
}

pub(crate) fn orchestrator_input(cx: &mut Context<Workspace>) -> (Entity<TextInput>, Subscription) {
    let input = cx.new(|cx| {
        let mut input = TextInput::new(cx);
        input.set_label("Message the orchestrator", cx);
        input.set_size(TextFieldSize::Large, cx);
        input.set_prefix(Some("\u{203a}".into()), cx);
        input.set_focus_style(FocusStyle::Border, cx);
        input.set_mono(true, cx);
        input
    });

    let subscription = cx.subscribe(&input, |workspace, _, event, cx| {
        if let TextInputEvent::Submitted(text) = event {
            workspace.submit_orchestrator(text, cx);
        }
    });

    (input, subscription)
}

fn to_layout_size(size: Size<Pixels>) -> swarm_layout::Size {
    swarm_layout::Size::new(f32::from(size.width), f32::from(size.height))
}

fn to_rect(size: Size<Pixels>) -> Rect {
    Rect::new(0., 0., f32::from(size.width), f32::from(size.height))
}

/// Height of a tile that shows `rows` rows of the terminal font: the rows, the padding of
/// the terminal view and the tile's header and border.
fn rows_height(cell: Option<(f32, f32)>, rows: u16) -> f32 {
    let (_, cell_height) = cell.unwrap_or((CHAR_WIDTH, ROW_HEIGHT));

    f32::from(rows) * cell_height + 2. * VIEW_PADDING + CHROME_HEIGHT
}

/// The layout style with a small tile that shows [`SMALL_TILE_ROWS`] rows and a process
/// tile that shows [`PROCESS_TILE_ROWS`] rows of the default font. A tile keeps its pixels
/// when the font size changes, and the terminal inside gets fewer rows.
pub(crate) fn layout_style(workspace: &Workspace) -> LayoutStyle {
    LayoutStyle {
        small_tile_height: rows_height(workspace.default_cell_size, SMALL_TILE_ROWS),
        process_tile_height: rows_height(workspace.default_cell_size, PROCESS_TILE_ROWS),
        ..LayoutStyle::default()
    }
}

/// The columns and rows of the terminal in a tile of this size, with the measured size of
/// one cell: what the terminal view fits in the tile's body, and so what the daemon is told.
pub(crate) fn tile_grid(size: Size<Pixels>, cell: Option<(f32, f32)>) -> (u16, u16) {
    let (cell_width, cell_height) = cell.unwrap_or((CHAR_WIDTH, ROW_HEIGHT));
    let body = Size {
        width: size.width - px(CHROME_WIDTH),
        height: size.height - px(CHROME_HEIGHT),
    };
    let cell = Size {
        width: px(cell_width),
        height: px(cell_height),
    };
    let grid = TerminalView::grid_in(body, cell);

    (grid.cols, grid.rows)
}

fn activity_of(status: AgentStatus) -> Activity {
    match status {
        AgentStatus::Blocked | AgentStatus::Error => Activity::Attention,
        AgentStatus::Working => Activity::Working,
        AgentStatus::Done => Activity::Done,
        AgentStatus::Idle | AgentStatus::Unknown => Activity::Idle,
    }
}

/// A process that ended with a failure needs attention, so its tile grows.
fn process_activity(terminal: &Terminal) -> Activity {
    if terminal.exit.as_ref().is_some_and(exit_failed) {
        Activity::Attention
    } else {
        Activity::Running
    }
}

fn tile_kind(kind: TerminalKind) -> TileKind {
    match kind {
        TerminalKind::Orchestrator => TileKind::Orchestrator,
        TerminalKind::Worker => TileKind::Worker,
        TerminalKind::Process => TileKind::Process,
        TerminalKind::User => TileKind::User,
    }
}

fn tile_spec(
    workspace: &Workspace,
    terminal: &Terminal,
    activity: Activity,
    large: bool,
) -> TileSpec {
    let mut spec = TileSpec::new(TileId(terminal.id.0), tile_kind(terminal.kind), activity);
    spec.minimized = workspace.minimized.contains(&terminal.id);
    spec.large = large;

    if terminal.kind == TerminalKind::User {
        spec.fixed = workspace
            .user_sizes
            .get(&terminal.id)
            .copied()
            .map(to_layout_size);
    }

    spec
}

/// The open task's tiles in layout order: orchestrator, workers, processes, user terminals.
/// A Start task has only its orchestrator.
pub(crate) fn tile_specs(workspace: &Workspace) -> Vec<TileSpec> {
    let data = &workspace.data;
    let task = workspace.active_task();

    if task.is_start {
        return task
            .orchestrator
            .and_then(|id| data.agent(id))
            .and_then(|agent| data.terminal(agent.terminal))
            .map(|terminal| tile_spec(workspace, terminal, Activity::Working, false))
            .into_iter()
            .collect();
    }

    let agents = task
        .orchestrator
        .into_iter()
        .chain(task.workers.iter().copied())
        .filter_map(|id| data.agent(id))
        .filter_map(|agent| {
            let terminal = data.terminal(agent.terminal)?;
            Some((terminal, activity_of(agent.status), agent.tile_large))
        });
    let others = task
        .processes
        .iter()
        .chain(&task.user_terminals)
        .filter_map(|id| data.terminal(*id))
        .map(|terminal| (terminal, process_activity(terminal), false));

    agents
        .chain(others)
        .map(|(terminal, activity, large)| tile_spec(workspace, terminal, activity, large))
        .collect()
}

pub(crate) fn compute_layout(workspace: &Workspace) -> Layout {
    let area = workspace.terminal_area_size().unwrap_or(FIRST_FRAME_AREA);

    let specs = tile_specs(workspace);

    if workspace.active_task().is_start {
        return start_layout(area, &specs);
    }

    swarm_layout::layout(to_rect(area), &specs, &layout_style(workspace))
}

/// One centered tile, at most 880px wide and as tall as the area allows.
fn start_layout(area: Size<Pixels>, specs: &[TileSpec]) -> Layout {
    let (pad_x, pad_y) = START_PADDING;
    let width = (f32::from(area.width) - 2. * pad_x).clamp(0., START_MAX_WIDTH);
    let height = (f32::from(area.height) - 2. * pad_y).max(0.);
    let x = (f32::from(area.width) - width) / 2.;

    Layout {
        placed: specs
            .iter()
            .map(|spec| (spec.id, Rect::new(x, pad_y, width, height)))
            .collect(),
        tray: Vec::new(),
    }
}

impl Workspace {
    /// Applies a worker's `tile.grow` or `tile.shrink`. A tile the user minimized ignores it
    /// until the user restores the tile. Other agents and unknown ones change nothing.
    pub(crate) fn tile_requested(
        &mut self,
        agent: swarm_protocol::AgentId,
        large: bool,
        cx: &mut Context<Self>,
    ) {
        let minimized = &self.minimized;

        let Some(worker) = self.data.worker_with_daemon_id_mut(agent) else {
            return;
        };

        if minimized.contains(&worker.terminal) || worker.tile_large == large {
            return;
        }

        worker.tile_large = large;
        cx.notify();
    }

    pub(crate) fn stop_process(&mut self, terminal: TerminalId, cx: &mut Context<Self>) {
        self.close_process(terminal, cx);
    }

    /// Removes a process tile. A live process that still runs is killed.
    pub(crate) fn close_process(&mut self, terminal: TerminalId, cx: &mut Context<Self>) {
        let owner = self
            .data
            .tasks
            .iter()
            .find(|task| task.processes.contains(&terminal))
            .map(|task| task.id);
        let Some(task) = owner else {
            return;
        };

        if let Some(live) = self.live_terminals.remove(&terminal) {
            live.kill_if_running(cx);
            self.dismissed_terminals.insert(live.daemon_id());
        }

        self.data.stop_process(task, terminal);
        self.disconnected.remove(&terminal);
        self.terminal_focus.remove(&terminal);
        self.minimized.remove(&terminal);
        if self.focused_terminal == Some(terminal) {
            self.focused_terminal = None;
        }

        cx.notify();
    }

    /// Removes the tile of a user or process terminal.
    pub(crate) fn close_tile(&mut self, terminal: TerminalId, cx: &mut Context<Self>) {
        self.close_user_terminal(terminal, cx);
        self.close_process(terminal, cx);
    }

    /// The size a new user terminal tile starts with.
    pub(crate) fn planned_user_size(&self) -> Size<Pixels> {
        let size = self
            .terminal_area_size()
            .map_or(DEFAULT_USER_SIZE, |area| Size {
                width: DEFAULT_USER_SIZE.width.min(area.width - px(350.)),
                height: DEFAULT_USER_SIZE.height.min(area.height - px(20.)),
            });

        size.max(&MIN_TILE)
    }

    /// Opens a real terminal once the app has reached a daemon, else a fake one.
    pub(crate) fn new_user_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_work() {
            log::debug!("no task is open, cannot open a terminal");
            return;
        }

        let task = self.active_task().id;

        if let Some(client) = self.daemon.clone() {
            if client.is_connected() {
                self.create_live_terminal(client, task, None, None, window, cx);
            } else {
                log::warn!("the daemon is disconnected, cannot open a terminal");
            }

            return;
        }

        if self.source != DataSource::Sample {
            log::warn!("the daemon is not connected, cannot open a terminal");
            return;
        }

        let terminal = self.data.add_user_terminal(task);
        self.user_sizes.insert(terminal, self.planned_user_size());
        self.focused_terminal = Some(terminal);

        cx.notify();
    }

    /// Closes the tile. A live terminal that still runs is killed.
    pub(crate) fn close_user_terminal(&mut self, terminal: TerminalId, cx: &mut Context<Self>) {
        let owner = self
            .data
            .tasks
            .iter()
            .find(|task| task.user_terminals.contains(&terminal))
            .map(|task| task.id);
        let Some(task) = owner else {
            return;
        };

        if let Some(live) = self.live_terminals.remove(&terminal) {
            live.kill_if_running(cx);
        }

        self.github_terminal_closed(terminal, cx);
        self.data.close_user_terminal(task, terminal);
        self.disconnected.remove(&terminal);
        self.terminal_focus.remove(&terminal);
        self.user_sizes.remove(&terminal);
        self.minimized.remove(&terminal);
        if self.focused_terminal == Some(terminal) {
            self.focused_terminal = None;
        }

        cx.notify();
    }

    pub(crate) fn resize_user_terminal(
        &mut self,
        terminal: TerminalId,
        size: Size<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let largest = self
            .terminal_area_size()
            .map_or(FIRST_FRAME_AREA, |area| Size {
                width: area.width - px(20.),
                height: area.height - px(20.),
            });
        let size = size.min(&largest).max(&MIN_TILE);

        if self.user_sizes.get(&terminal) != Some(&size) {
            self.user_sizes.insert(terminal, size);
            cx.notify();
        }
    }

    pub(crate) fn submit_orchestrator(&mut self, text: &SharedString, cx: &mut Context<Self>) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }

        let orchestrator = self
            .active_task()
            .orchestrator
            .and_then(|id| self.data.agent(id))
            .map(|agent| agent.terminal);
        let terminal =
            orchestrator.and_then(|id| self.data.terminals.iter_mut().find(|entry| entry.id == id));

        if let Some(terminal) = terminal {
            terminal.lines.push(FakeLine {
                text: format!("› {text}").into(),
                color: Color::Default,
            });
        }

        self.orchestrator_input
            .update(cx, |input, cx| input.set_text("", cx));
        cx.notify();
    }

    fn set_terminal_area(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let resized = self.terminal_area_size() != Some(bounds.size);
        self.terminal_area = Some(bounds);
        self.terminal_area_had_panel = self.visible_panel().is_some();

        // A notify during prepaint can wait for the next input; this draws right away.
        if resized {
            cx.on_next_frame(window, |_, _, cx| cx.notify());
        }
    }

    fn start_resize(&mut self, terminal: TerminalId, start_y: Pixels, cx: &mut Context<Self>) {
        let Some(rect) = self.layout.rect(TileId(terminal.0)) else {
            return;
        };

        self.resizing = Some(Resizing {
            terminal,
            start_y,
            start_height: px(rect.size.height),
        });
        cx.notify();
    }

    fn drag_resize(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(resizing) = self.resizing else {
            return;
        };

        if event.pressed_button != Some(MouseButton::Left) {
            self.end_resize(cx);
            return;
        }

        let Some(rect) = self.layout.rect(TileId(resizing.terminal.0)) else {
            return;
        };

        let size = Size {
            width: px(rect.size.width),
            height: resizing.start_height + (event.position.y - resizing.start_y),
        };
        self.resize_user_terminal(resizing.terminal, size, cx);
    }

    fn end_resize(&mut self, cx: &mut Context<Self>) {
        if self.resizing.take().is_some() {
            cx.notify();
        }
    }

    fn nudge_resize(&mut self, terminal: TerminalId, delta: f32, cx: &mut Context<Self>) {
        let Some(rect) = self.layout.rect(TileId(terminal.0)) else {
            return;
        };

        let size = Size {
            width: px(rect.size.width),
            height: px(rect.size.height + delta),
        };
        self.resize_user_terminal(terminal, size, cx);
    }
}

fn terminals(workspace: &Workspace, cx: &mut Context<Workspace>) -> impl IntoElement + use<> {
    let area = cx.entity();
    let term = cx.theme().colors().term;
    let is_start = workspace.active_task().is_start;
    let tiles: Vec<AnyElement> = workspace
        .layout
        .placed
        .iter()
        .filter_map(|(id, rect)| tile(workspace, TerminalId(id.0), *rect, cx))
        .collect();

    div()
        .on_children_prepainted(move |bounds, window, cx| {
            if let Some(first) = bounds.first() {
                area.update(cx, |workspace, cx| {
                    workspace.set_terminal_area(*first, window, cx);
                });
            }
        })
        .id("terminals")
        .size_full()
        .relative()
        .when(is_start, |area| area.bg(term))
        .on_mouse_move(cx.listener(|workspace, event: &MouseMoveEvent, _, cx| {
            workspace.drag_resize(event, cx);
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|workspace, _, _, cx| workspace.end_resize(cx)),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|workspace, _, _, cx| workspace.end_resize(cx)),
        )
        // Fills the area so its bounds are the layout area.
        .child(div().absolute().size_full())
        .when(is_start && crate::settings::easter_eggs(cx), |area| {
            area.child(honeycomb_behind(workspace))
        })
        .children(tiles)
}

fn tile(
    workspace: &Workspace,
    id: TerminalId,
    rect: Rect,
    cx: &mut Context<Workspace>,
) -> Option<AnyElement> {
    let terminal = workspace.data.terminal(id)?;
    let size = Size {
        width: px(rect.size.width),
        height: px(rect.size.height),
    };

    let frame = match terminal.kind {
        TerminalKind::User => user_frame(workspace, terminal, cx),
        TerminalKind::Process if workspace.live_terminals.contains_key(&id) => {
            live_process_frame(workspace, terminal, cx)
        }
        _ => agent_frame(workspace, terminal, cx),
    };

    let resizing = workspace
        .resizing
        .filter(|resizing| resizing.terminal == id);
    let handle =
        (terminal.kind == TerminalKind::User).then(|| resize_handle(id, resizing.is_some(), cx));
    let badge = resizing.map(|_| size_badge(size, workspace.cell_size, cx));

    Some(
        div()
            .id(("tile-slot", id.0))
            .debug_selector(move || format!("tile-slot-{}", id.0))
            .absolute()
            .left(px(rect.origin.x))
            .top(px(rect.origin.y))
            .w(size.width)
            .h(size.height)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |workspace, _, _, cx| workspace.focus_terminal(id, cx)),
            )
            .child(frame)
            .children(handle)
            .children(badge)
            .into_any_element(),
    )
}

fn lines(terminal: &Terminal, cx: &Context<Workspace>) -> Vec<AnyElement> {
    let soft = cx.theme().colors().soft;

    terminal
        .lines
        .iter()
        .map(|line| {
            let text = if line.text.is_empty() {
                SharedString::from(" ")
            } else {
                line.text.clone()
            };
            let typed = text.starts_with('›');

            div()
                .whitespace_nowrap()
                .text_color(line.color.hsla(cx))
                .when(typed, |row| {
                    row.bg(soft)
                        .rounded(px(4.))
                        .px(px(6.))
                        .mx(px(-6.))
                        .my(px(4.))
                })
                .child(text)
                .into_any_element()
        })
        .collect()
}

fn agent_frame(
    workspace: &Workspace,
    terminal: &Terminal,
    cx: &mut Context<Workspace>,
) -> TileFrame {
    let id = terminal.id;
    let agent = workspace
        .data
        .agents
        .iter()
        .find(|agent| agent.terminal == id);
    let focused = workspace.focused_terminal == Some(id);
    let name = terminal.title.clone();

    let icon = match (terminal.kind, agent.map(|agent| agent.kind)) {
        (TerminalKind::Process, _) => IconName::Terminal,
        (_, Some(AgentKind::Codex)) => IconName::Codex,
        _ => IconName::ClaudeCode,
    };
    let meta = agent.map(Agent::meta).or_else(|| terminal.meta.clone());
    let live = workspace.live_terminals.get(&id);
    let is_live_agent = agent.is_some_and(|agent| agent.daemon.is_some());

    let mut frame = TileFrame::new(("tile", id.0), icon, name.clone())
        .focused(focused)
        .attention(agent.is_some_and(|agent| {
            matches!(agent.status, AgentStatus::Blocked | AgentStatus::Error)
        }));

    // A live view tracks its own focus handle.
    if terminal.kind != TerminalKind::Orchestrator
        && live.is_none()
        && let Some(handle) = workspace.terminal_focus.get(&id)
    {
        frame = frame.track_focus(handle);
    }

    if is_live_agent && let Some(agent) = agent {
        frame = frame.accessible_label(agent.accessible_label());
    }

    if terminal.kind == TerminalKind::Process {
        frame = frame.accessible_label(format!("{name} process terminal"));
    }
    if let Some(meta) = meta {
        frame = frame.meta(meta);
    }
    if let Some(link) = terminal.link.clone() {
        frame = frame.action(
            Label::new(link)
                .mono()
                .size(LabelSize::XSmall)
                .color(Color::Accent),
        );
    }

    frame = match (terminal.kind, agent) {
        (TerminalKind::Orchestrator, Some(agent)) => frame.status_ring(agent.status),
        (_, Some(agent)) => frame.status(agent.status),
        (_, None) if terminal.link.is_none() => frame.status(AgentStatus::Working),
        (_, None) => frame,
    };

    if terminal.kind != TerminalKind::Orchestrator {
        frame = frame.action(
            IconButton::new(
                ("minimize", id.0),
                IconName::Minimize,
                format!("Move {name} to tray"),
            )
            .on_click(cx.listener(move |workspace, _, _, cx| workspace.minimize(id, cx))),
        );
    }
    if terminal.kind == TerminalKind::Process {
        frame = frame.action(
            IconButton::new(("stop", id.0), IconName::Close, format!("Stop {name}"))
                .on_click(cx.listener(move |workspace, _, _, cx| workspace.stop_process(id, cx))),
        );
    }

    if let Some(live) = live {
        return frame.child(div().size_full().child(live.view.clone()));
    }

    frame = frame.children(lines(terminal, cx));

    if terminal.kind == TerminalKind::Orchestrator && !is_live_agent {
        frame = frame.footer(div().m(px(10.)).child(workspace.orchestrator_input.clone()));
    }

    frame
}

fn live_process_frame(
    workspace: &Workspace,
    terminal: &Terminal,
    cx: &mut Context<Workspace>,
) -> TileFrame {
    let id = terminal.id;
    let name = terminal.title.clone();

    let mut frame = TileFrame::new(("tile", id.0), IconName::Terminal, name.clone())
        .accessible_label(format!("{name} process terminal"))
        .focused(workspace.focused_terminal == Some(id))
        .status(AgentStatus::Working);

    if let Some(meta) = terminal.meta.clone() {
        frame = frame.meta(meta);
    }

    frame = frame
        .action(
            IconButton::new(
                ("minimize", id.0),
                IconName::Minimize,
                format!("Move {name} to tray"),
            )
            .on_click(cx.listener(move |workspace, _, _, cx| workspace.minimize(id, cx))),
        )
        .action(
            IconButton::new(("stop", id.0), IconName::Close, format!("Stop {name}"))
                .on_click(cx.listener(move |workspace, _, _, cx| workspace.stop_process(id, cx))),
        );

    match workspace.live_terminals.get(&id) {
        Some(live) => frame.child(div().size_full().child(live.view.clone())),
        None => frame,
    }
}

fn user_frame(
    workspace: &Workspace,
    terminal: &Terminal,
    cx: &mut Context<Workspace>,
) -> TileFrame {
    let id = terminal.id;
    let colors = cx.theme().colors();
    let active = colors.active;
    let focused = workspace.focused_terminal == Some(id);

    let you = h_flex()
        .gap(px(6.))
        .child(
            div()
                .px(px(6.))
                .py(px(1.))
                .rounded(px(5.))
                .bg(active)
                .child(
                    Label::new("You")
                        .size(LabelSize::XSmall)
                        .weight(FontWeight::MEDIUM),
                ),
        )
        .child(
            Icon::new(IconName::Lock)
                .size(IconSize::Custom(px(13.)))
                .color(Color::Muted)
                .accessible(("lock", id.0), "Agents have no access"),
        );

    let mut frame = TileFrame::new(("tile", id.0), IconName::User, terminal.title.clone())
        .accessible_label(format!("Your terminal, {}", terminal.title))
        .ring(TileRing::Fg)
        .focused(focused)
        .name_suffix(you);

    let live = workspace.live_terminals.get(&id);

    // A live tile's focus handle belongs to its view, which tracks it itself.
    if live.is_none()
        && let Some(handle) = workspace.terminal_focus.get(&id)
    {
        frame = frame.track_focus(handle);
    }

    if workspace.disconnected.contains(&id) {
        frame = frame.action(
            Label::new("disconnected")
                .mono()
                .size(LabelSize::XSmall)
                .color(Color::Warn),
        );
    } else if let Some(path) = terminal.meta.clone() {
        frame = frame.action(
            Label::new(path)
                .mono()
                .size(LabelSize::XSmall)
                .color(Color::Faint),
        );
    }

    let frame = frame.action(
        IconButton::new(("close", id.0), IconName::Close, "Close your terminal").on_click(
            cx.listener(move |workspace, _, _, cx| {
                workspace.close_user_terminal(id, cx);
            }),
        ),
    );

    match live {
        Some(live) => frame.child(div().size_full().child(live.view.clone())),
        None => frame.children(lines(terminal, cx)),
    }
}

fn resize_handle(id: TerminalId, dragging: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let colors = cx.theme().colors();
    let (accent, ground) = (colors.accent, colors.ground);

    div()
        .id(("resize", id.0))
        .role(Role::Splitter)
        .aria_label("Resize your terminal")
        .focusable()
        .tab_stop(true)
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .h(px(HANDLE_HEIGHT))
        .flex()
        .items_center()
        .justify_center()
        .cursor_row_resize()
        .when(dragging, |handle| handle.bg(accent))
        .focus_visible(move |style| focus_ring(style.bg(accent), accent))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |workspace, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                workspace.start_resize(id, event.position.y, cx);
            }),
        )
        .on_key_down(cx.listener(move |workspace, event: &KeyDownEvent, _, cx| {
            match event.keystroke.key.as_str() {
                "up" => workspace.nudge_resize(id, -KEY_STEP, cx),
                "down" => workspace.nudge_resize(id, KEY_STEP, cx),
                _ => {}
            }
        }))
        .when(dragging, |handle| {
            handle.child(div().w(px(36.)).h(px(4.)).rounded(px(2.)).bg(ground))
        })
        .into_any_element()
}

fn size_badge(size: Size<Pixels>, cell: Option<(f32, f32)>, cx: &Context<Workspace>) -> AnyElement {
    let colors = cx.theme().colors();
    let (columns, rows) = tile_grid(size, cell);

    div()
        .absolute()
        .right(px(12.))
        .bottom(px(12.))
        .px(px(8.))
        .py(px(2.))
        .rounded(px(5.))
        .bg(colors.accent)
        .text_color(colors.ground)
        .font_family(MONO_FONT)
        .text_size(px(11.))
        .child(format!("{columns}×{rows}"))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext};
    use swarm_term::ExitStatus;

    use super::*;
    use crate::actions::{self, NewTerminal, PlanWithOrchestrator};
    use crate::model::{AgentId, TaskId};

    fn setup(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
        cx.update(|cx| {
            swarm_ui::theme::init(cx);
            actions::bind_keys(cx);
        });

        let (workspace, cx) = cx.add_window_view(Workspace::with_sample);
        cx.run_until_parked();

        (workspace, cx)
    }

    fn terminal_of(workspace: &Workspace, agent: usize) -> TerminalId {
        let id = workspace.active_task().workers.get(agent).copied();
        let agent = workspace.data.agent(id.unwrap_or(AgentId(u32::MAX)));

        agent.map_or(TerminalId(u32::MAX), |agent| agent.terminal)
    }

    fn is_placed(workspace: &Workspace, terminal: TerminalId) -> bool {
        workspace.layout.rect(TileId(terminal.0)).is_some()
    }

    fn in_tray(workspace: &Workspace, terminal: TerminalId) -> bool {
        workspace.layout.tray.contains(&TileId(terminal.0))
    }

    fn redraw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
    }

    #[test]
    fn blocked_agents_need_attention() {
        assert_eq!(activity_of(AgentStatus::Blocked), Activity::Attention);
    }

    #[test]
    fn working_agents_are_working() {
        assert_eq!(activity_of(AgentStatus::Working), Activity::Working);
    }

    #[test]
    fn done_and_idle_agents_map_to_their_own_activity() {
        assert_eq!(activity_of(AgentStatus::Done), Activity::Done);
        assert_eq!(activity_of(AgentStatus::Idle), Activity::Idle);
    }

    #[test]
    fn the_grid_of_a_tile_leaves_out_its_frame_and_the_padding_of_the_view() {
        let size = Size {
            width: px(648.),
            height: px(272.),
        };

        // (648 - 26 - 16) / 7.2 columns and (272 - 56 - 16) / 17 rows.
        assert_eq!(tile_grid(size, None), (84, 11));
    }

    #[test]
    fn a_bigger_cell_fits_fewer_cells_in_the_same_tile() {
        let size = Size {
            width: px(648.),
            height: px(272.),
        };

        assert_eq!(tile_grid(size, Some((9., 22.))), (67, 9));
    }

    #[gpui::test]
    fn the_size_badge_counts_the_grid_the_view_is_given(cx: &mut TestAppContext) {
        let (_, cx) = setup(cx);
        let tile = Size {
            width: px(651.),
            height: px(473.),
        };

        cx.update(|window, cx| {
            let cell = TerminalView::cell_size(15., window, cx);
            let body = Size {
                width: tile.width - px(CHROME_WIDTH),
                height: tile.height - px(CHROME_HEIGHT),
            };
            let given = TerminalView::size_for(body, 15., window, cx);

            let shown = tile_grid(tile, Some((f32::from(cell.width), f32::from(cell.height))));

            assert_eq!(shown, (given.cols, given.rows));
        });
    }

    #[test]
    fn a_small_tile_shows_twelve_rows_of_the_terminal_font() {
        assert!((rows_height(Some((7.2, 17.)), SMALL_TILE_ROWS) - 276.).abs() < 0.01);
        assert!((rows_height(Some((8., 20.)), SMALL_TILE_ROWS) - 312.).abs() < 0.01);
        assert!((rows_height(None, SMALL_TILE_ROWS) - 276.).abs() < 0.01);
    }

    #[gpui::test]
    fn the_layout_style_takes_the_small_tile_from_the_cell_of_the_default_font(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);

        let measured = workspace.read_with(cx, |workspace, _| {
            (
                workspace.default_cell_size,
                layout_style(workspace).small_tile_height,
            )
        });
        workspace.update(cx, |workspace, _| {
            workspace.default_cell_size = Some((9., 22.));
        });
        let larger =
            workspace.read_with(cx, |workspace, _| layout_style(workspace).small_tile_height);

        let (cell, height) = measured;
        assert_eq!(cell.map(|(_, height)| height), Some(17.));
        assert!((height - 276.).abs() < 0.01);
        assert!((larger - 336.).abs() < 0.01);
    }

    #[gpui::test]
    fn the_layout_style_ignores_the_cell_of_the_current_font(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let style = |workspace: &Workspace| {
            let style = layout_style(workspace);

            (style.small_tile_height, style.process_tile_height)
        };

        let before = workspace.read_with(cx, |workspace, _| style(workspace));
        workspace.update(cx, |workspace, _| workspace.cell_size = Some((9., 22.)));
        let after = workspace.read_with(cx, |workspace, _| style(workspace));

        assert_eq!(before, after);
    }

    #[gpui::test]
    fn a_process_tile_shows_eight_rows_of_the_terminal_font(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let height = workspace.read_with(cx, |workspace, _| {
            layout_style(workspace).process_tile_height
        });

        cx.update(|window, cx| {
            let body = Size {
                width: px(300.),
                height: px(height - CHROME_HEIGHT),
            };
            let grid = TerminalView::size_for(body, 13., window, cx);

            assert_eq!(grid.rows, PROCESS_TILE_ROWS);
        });
    }

    fn rect_of(workspace: &Workspace, terminal: TerminalId) -> Rect {
        workspace
            .layout
            .rect(TileId(terminal.0))
            .unwrap_or_default()
    }

    #[gpui::test]
    fn the_orchestrator_takes_most_of_the_width_and_workers_are_small_boxes(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        redraw(cx);

        workspace.read_with(cx, |workspace, _| {
            let area = workspace
                .terminal_area_size()
                .expect("the area is measured");
            let content = f32::from(area.width) - 20.;
            let small = layout_style(workspace).small_tile_height;
            let orchestrator = workspace
                .active_task()
                .orchestrator
                .and_then(|id| workspace.data.agent(id))
                .map(|agent| rect_of(workspace, agent.terminal))
                .expect("the sample has an orchestrator");

            assert!((orchestrator.size.width - 0.65 * content).abs() < 1.);
            assert!((orchestrator.size.height - (f32::from(area.height) - 20.)).abs() < 1.);

            let working = workspace
                .active_task()
                .workers
                .iter()
                .filter_map(|id| workspace.data.agent(*id))
                .find(|agent| agent.status == AgentStatus::Working)
                .map(|agent| rect_of(workspace, agent.terminal))
                .expect("the sample has a working worker");
            assert!((working.size.height - small).abs() < 0.5);
            assert!(working.origin.x >= orchestrator.right());
            assert!(working.size.height < f32::from(area.height) / 2.);
        });
    }

    #[gpui::test]
    fn a_blocked_worker_grows_and_shrinks_back_when_it_works_again(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let blocked = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .agents
                .iter()
                .find(|agent| agent.status == AgentStatus::Blocked)
                .map(|agent| (agent.id, agent.terminal))
        });
        let (agent, terminal) = blocked.expect("the sample has a blocked worker");
        redraw(cx);
        let height = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, _| rect_of(workspace, terminal).size.height)
        };

        let grown = height(cx);
        workspace.update(cx, |workspace, cx| {
            if let Some(entry) = workspace
                .data
                .agents
                .iter_mut()
                .find(|entry| entry.id == agent)
            {
                entry.status = AgentStatus::Working;
            }
            cx.notify();
        });
        redraw(cx);
        let small =
            workspace.read_with(cx, |workspace, _| layout_style(workspace).small_tile_height);

        assert!(grown > small + 50.);
        assert!((height(cx) - small).abs() < 0.5);
    }

    #[gpui::test]
    fn a_tall_window_shows_the_tall_box_with_process_terminals_below_it(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.simulate_resize(gpui::size(px(1600.), px(1300.)));
        redraw(cx);
        redraw(cx);

        workspace.read_with(cx, |workspace, _| {
            let blocked = workspace
                .data
                .agents
                .iter()
                .find(|agent| agent.status == AgentStatus::Blocked)
                .map(|agent| rect_of(workspace, agent.terminal))
                .expect("the sample has a blocked worker");
            let style = layout_style(workspace);
            let tall = 2. * style.small_tile_height + style.gap;
            assert!((blocked.size.height - tall).abs() < 0.5);

            let below: Vec<Rect> = workspace
                .active_task()
                .processes
                .iter()
                .map(|id| rect_of(workspace, *id))
                .filter(|rect| rect.size.width > 0. && rect.origin.y >= blocked.bottom())
                .collect();
            assert!(!below.is_empty(), "no process terminal below {blocked:?}");
            assert!(
                below
                    .iter()
                    .all(|rect| (rect.origin.x - blocked.origin.x).abs() < 0.5)
            );
        });
    }

    #[gpui::test]
    fn specs_start_with_the_orchestrator_then_workers_then_processes(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let kinds: Vec<TileKind> = workspace.read_with(cx, |workspace, _| {
            tile_specs(workspace).iter().map(|spec| spec.kind).collect()
        });

        assert_eq!(
            kinds,
            [
                TileKind::Orchestrator,
                TileKind::Worker,
                TileKind::Worker,
                TileKind::Worker,
                TileKind::Worker,
                TileKind::Process,
                TileKind::Process,
                TileKind::Process,
                TileKind::Process,
            ]
        );
    }

    fn process_activity_after_exit(exit: Option<ExitStatus>, cx: &mut TestAppContext) -> Activity {
        let (workspace, cx) = setup(cx);
        let process = workspace.update(cx, |workspace, _| {
            let process = workspace.active_task().processes.first().copied();
            let process = process.unwrap_or(TerminalId(u32::MAX));

            if let Some(exit) = exit {
                workspace.data.set_terminal_exit(process, exit);
            }

            process
        });

        workspace.read_with(cx, |workspace, _| {
            tile_specs(workspace)
                .iter()
                .find(|spec| spec.id == TileId(process.0))
                .map_or(Activity::Idle, |spec| spec.activity)
        })
    }

    #[gpui::test]
    fn a_process_that_failed_needs_attention(cx: &mut TestAppContext) {
        let failed = Some(ExitStatus::Code(2));
        let killed = Some(ExitStatus::Signal("SIGKILL".to_owned()));

        assert_eq!(process_activity_after_exit(failed, cx), Activity::Attention);
        assert_eq!(process_activity_after_exit(killed, cx), Activity::Attention);
    }

    #[gpui::test]
    fn a_process_that_runs_or_exited_well_stays_running(cx: &mut TestAppContext) {
        let well = Some(ExitStatus::Code(0));

        assert_eq!(process_activity_after_exit(None, cx), Activity::Running);
        assert_eq!(process_activity_after_exit(well, cx), Activity::Running);
    }

    #[gpui::test]
    fn the_terminal_area_is_measured(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let area = workspace.read_with(cx, |workspace, _| workspace.terminal_area);

        assert!(area.is_some());
    }

    #[gpui::test]
    fn a_tab_switch_that_shows_the_panel_lays_out_for_the_new_area(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.dispatch_action(PlanWithOrchestrator);
        redraw(cx);

        let before_measure = workspace.update(cx, |workspace, cx| {
            workspace.select_task(TaskId(1), cx);
            compute_layout(workspace)
        });
        redraw(cx);
        let measured = workspace.read_with(cx, |workspace, _| workspace.layout.clone());

        assert!(!measured.placed.is_empty());
        assert_eq!(before_measure.placed, measured.placed);
    }

    #[gpui::test]
    fn minimizing_a_worker_moves_it_to_the_tray(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let worker = workspace.read_with(cx, |workspace, _| terminal_of(workspace, 0));

        workspace.update(cx, |workspace, cx| workspace.minimize(worker, cx));
        redraw(cx);

        assert!(workspace.read_with(cx, |workspace, _| in_tray(workspace, worker)));
    }

    #[gpui::test]
    fn restoring_a_tray_chip_places_the_tile_again(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let worker = workspace.read_with(cx, |workspace, _| terminal_of(workspace, 0));
        workspace.update(cx, |workspace, cx| workspace.minimize(worker, cx));
        redraw(cx);

        workspace.update(cx, |workspace, cx| {
            workspace.restore(worker, cx);
            workspace.focus_terminal(worker, cx);
        });
        redraw(cx);

        assert!(workspace.read_with(cx, |workspace, _| is_placed(workspace, worker)));
    }

    #[gpui::test]
    fn stopping_a_process_removes_its_tile(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let process = workspace.read_with(cx, |workspace, _| {
            workspace.active_task().processes.first().copied()
        });
        let process = process.unwrap_or(TerminalId(u32::MAX));

        workspace.update(cx, |workspace, cx| workspace.stop_process(process, cx));
        redraw(cx);

        let (placed, in_tray) = workspace.read_with(cx, |workspace, _| {
            (is_placed(workspace, process), in_tray(workspace, process))
        });
        assert!(!placed);
        assert!(!in_tray);
    }

    #[gpui::test]
    fn the_new_terminal_action_adds_a_placed_user_tile(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        cx.dispatch_action(NewTerminal);
        redraw(cx);

        let user = workspace.read_with(cx, |workspace, _| {
            workspace.active_task().user_terminals.first().copied()
        });
        let user = user.unwrap_or(TerminalId(u32::MAX));
        assert!(workspace.read_with(cx, |workspace, _| is_placed(workspace, user)));
    }

    #[gpui::test]
    fn resizing_a_user_terminal_changes_its_height(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.dispatch_action(NewTerminal);
        redraw(cx);
        let user = workspace.read_with(cx, |workspace, _| {
            workspace.active_task().user_terminals.first().copied()
        });
        let user = user.unwrap_or(TerminalId(u32::MAX));

        workspace.update(cx, |workspace, cx| {
            let size = Size {
                width: px(500.),
                height: px(300.),
            };
            workspace.resize_user_terminal(user, size, cx);
        });
        redraw(cx);

        let height = workspace.read_with(cx, |workspace, _| {
            workspace
                .layout
                .rect(TileId(user.0))
                .map(|rect| rect.size.height)
        });
        assert_eq!(height, Some(300.));
    }

    #[gpui::test]
    fn submitting_the_orchestrator_input_appends_a_line(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let count = |workspace: &Workspace| {
            let id = workspace.active_task().orchestrator;
            let terminal = id
                .and_then(|id| workspace.data.agent(id))
                .map(|agent| agent.terminal);
            terminal
                .and_then(|id| workspace.data.terminal(id))
                .map(|terminal| terminal.lines.clone())
                .unwrap_or_default()
        };
        let before = workspace.read_with(cx, |workspace, _| count(workspace).len());

        workspace.update(cx, |workspace, cx| {
            workspace.submit_orchestrator(&"hello".into(), cx);
        });

        let lines = workspace.read_with(cx, |workspace, _| count(workspace));
        assert_eq!(lines.len(), before + 1);
        assert_eq!(lines.last().map(|line| line.text.as_ref()), Some("› hello"));
    }

    #[gpui::test]
    fn the_start_view_places_exactly_one_tile(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        workspace.update(cx, |workspace, cx| {
            workspace.select_project(crate::model::ProjectId(3), cx);
        });
        redraw(cx);

        let (placed, tray) = workspace.read_with(cx, |workspace, _| {
            (workspace.layout.placed.len(), workspace.layout.tray.len())
        });
        assert_eq!(placed, 1);
        assert_eq!(tray, 0);
    }

    #[test]
    fn the_start_tile_is_centered_and_at_most_880_wide() {
        let area = Size {
            width: px(1200.),
            height: px(700.),
        };
        let spec = TileSpec::new(TileId(0), TileKind::Orchestrator, Activity::Working);

        let layout = start_layout(area, &[spec]);

        let rect = layout.rect(TileId(0));
        assert_eq!(rect, Some(Rect::new(160., 28., 880., 644.)));
    }

    mod tile_requests {
        use std::path::PathBuf;

        use swarm_protocol::{
            AgentId as DaemonAgentId, AgentInfo, AgentRole, AgentStatus as DaemonStatus, Client,
            Event, Request, TerminalId as DaemonTerminalId, TerminalInfo,
            TerminalKind as DaemonKind,
        };
        use swarm_term::BlockedKind;

        use super::*;
        use crate::daemon::tests::{
            agent_info, fake_daemon_replying, info, real_task_workspace, wait_until,
        };

        const ORCHESTRATOR: u64 = 1;
        const API: u64 = 2;
        const UI: u64 = 3;
        /// The terminal a test attaches to make the fake daemon send request number 0.
        const FIRST_TRIGGER: u64 = 100;

        #[derive(Debug, PartialEq)]
        enum Height {
            Small,
            Tall,
            Process,
            Other(f32),
        }

        struct Rig {
            workspace: Entity<Workspace>,
            client: Client,
        }

        fn worker_info(id: u64, name: &str, status: DaemonStatus) -> AgentInfo {
            AgentInfo {
                role: AgentRole::Worker,
                name: name.into(),
                ..agent_info(id, 7, status)
            }
        }

        /// A daemon that sends `requests[n]` as a `TileRequest` when the client attaches
        /// terminal `FIRST_TRIGGER + n`.
        fn fake_daemon_requesting(requests: Vec<(u64, bool)>) -> PathBuf {
            fake_daemon_replying(move |request| match request {
                Request::Attach { id, .. } => {
                    id.0.checked_sub(FIRST_TRIGGER)
                        .and_then(|number| usize::try_from(number).ok())
                        .and_then(|number| requests.get(number))
                        .map(|&(agent, large)| Event::TileRequest {
                            agent: DaemonAgentId(agent),
                            large,
                        })
                }
                _ => None,
            })
        }

        /// Makes the fake daemon send request number `number`.
        fn send_request(rig: &Rig, number: u64) {
            let channels = rig
                .client
                .attach(DaemonTerminalId(FIRST_TRIGGER + number), 0)
                .expect("send the attach");

            drop(channels);
        }

        /// The task "login" with its orchestrator and two working workers, in a tall window.
        fn rig(
            requests: Vec<(u64, bool)>,
            cx: &mut TestAppContext,
        ) -> (Rig, &mut VisualTestContext) {
            let socket = fake_daemon_requesting(requests);
            let (workspace, cx, client) = real_task_workspace(&socket, cx);
            let agents = [
                agent_info(ORCHESTRATOR, 7, DaemonStatus::Working),
                worker_info(API, "api", DaemonStatus::Working),
                worker_info(UI, "ui", DaemonStatus::Working),
            ];
            for agent in &agents {
                change_agent(&workspace, agent, cx);
            }

            cx.simulate_resize(gpui::size(px(1600.), px(1300.)));
            redraw(cx);
            redraw(cx);

            (Rig { workspace, client }, cx)
        }

        fn change_agent(
            workspace: &Entity<Workspace>,
            agent: &AgentInfo,
            cx: &mut VisualTestContext,
        ) {
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.agent_changed(agent, Some(window), cx);
            });
        }

        fn terminal_of(workspace: &Workspace, agent: u64) -> TerminalId {
            workspace
                .data
                .agent_with_daemon_id(DaemonAgentId(agent))
                .map_or(TerminalId(u32::MAX), |agent| agent.terminal)
        }

        fn ask(rig: &Rig, agent: u64, large: bool, cx: &mut VisualTestContext) {
            rig.workspace.update(cx, |workspace, cx| {
                workspace.tile_requested(DaemonAgentId(agent), large, cx);
            });
        }

        fn height_of(
            rig: &Rig,
            terminal: impl Fn(&Workspace) -> TerminalId,
            cx: &mut VisualTestContext,
        ) -> Height {
            redraw(cx);

            rig.workspace.read_with(cx, |workspace, _| {
                let style = layout_style(workspace);
                let height = rect_of(workspace, terminal(workspace)).size.height;
                let tall = 2. * style.small_tile_height + style.gap;
                let is = |expected: f32| (height - expected).abs() < 0.5;

                if is(style.small_tile_height) {
                    Height::Small
                } else if is(tall) {
                    Height::Tall
                } else if is(style.process_tile_height) {
                    Height::Process
                } else {
                    Height::Other(height)
                }
            })
        }

        fn tile(rig: &Rig, agent: u64, cx: &mut VisualTestContext) -> Height {
            height_of(rig, |workspace| terminal_of(workspace, agent), cx)
        }

        fn asked_large(rig: &Rig, agent: u64, cx: &VisualTestContext) -> bool {
            rig.workspace.read_with(cx, |workspace, _| {
                workspace
                    .data
                    .agent_with_daemon_id(DaemonAgentId(agent))
                    .is_some_and(|agent| agent.tile_large)
            })
        }

        fn nobody_asked(rig: &Rig, cx: &VisualTestContext) -> bool {
            rig.workspace.read_with(cx, |workspace, _| {
                workspace.data.agents.iter().all(|agent| !agent.tile_large)
            })
        }

        fn minimize(rig: &Rig, agent: u64, cx: &mut VisualTestContext) {
            rig.workspace.update(cx, |workspace, cx| {
                let terminal = terminal_of(workspace, agent);
                workspace.minimize(terminal, cx);
            });
        }

        fn restore(rig: &Rig, agent: u64, cx: &mut VisualTestContext) {
            rig.workspace.update(cx, |workspace, cx| {
                let terminal = terminal_of(workspace, agent);
                workspace.restore(terminal, cx);
            });
        }

        fn orchestrator_rect(rig: &Rig, cx: &mut VisualTestContext) -> Rect {
            redraw(cx);

            rig.workspace.read_with(cx, |workspace, _| {
                rect_of(workspace, terminal_of(workspace, ORCHESTRATOR))
            })
        }

        #[gpui::test]
        fn a_grow_request_makes_the_worker_tile_large_and_a_shrink_makes_it_small_again(
            cx: &mut TestAppContext,
        ) {
            let (rig, cx) = rig(Vec::new(), cx);
            assert_eq!(tile(&rig, API, cx), Height::Small);

            ask(&rig, API, true, cx);
            let grown = (tile(&rig, API, cx), tile(&rig, UI, cx));
            ask(&rig, API, false, cx);
            let shrunk = tile(&rig, API, cx);

            assert_eq!(grown, (Height::Tall, Height::Small));
            assert_eq!(shrunk, Height::Small);
        }

        #[gpui::test]
        fn a_blocked_worker_stays_large_after_a_shrink(cx: &mut TestAppContext) {
            let (rig, cx) = rig(Vec::new(), cx);
            let blocked = DaemonStatus::Blocked(BlockedKind::Permission);
            change_agent(&rig.workspace, &worker_info(API, "api", blocked), cx);

            let alone = tile(&rig, API, cx);
            ask(&rig, API, false, cx);
            let shrunk = tile(&rig, API, cx);
            change_agent(
                &rig.workspace,
                &worker_info(API, "api", DaemonStatus::Working),
                cx,
            );
            let working = tile(&rig, API, cx);

            assert_eq!(alone, Height::Tall);
            assert_eq!(shrunk, Height::Tall);
            assert_eq!(working, Height::Small);
        }

        #[gpui::test]
        fn a_minimized_worker_ignores_a_grow_until_the_user_restores_it(cx: &mut TestAppContext) {
            let (rig, cx) = rig(Vec::new(), cx);
            minimize(&rig, API, cx);

            ask(&rig, API, true, cx);
            restore(&rig, API, cx);
            let ignored = tile(&rig, API, cx);
            ask(&rig, API, true, cx);
            let heeded = tile(&rig, API, cx);

            assert_eq!(ignored, Height::Small);
            assert_eq!(heeded, Height::Tall);
        }

        #[gpui::test]
        fn a_minimized_worker_ignores_a_shrink_until_the_user_restores_it(cx: &mut TestAppContext) {
            let (rig, cx) = rig(Vec::new(), cx);
            ask(&rig, API, true, cx);
            minimize(&rig, API, cx);

            ask(&rig, API, false, cx);
            restore(&rig, API, cx);
            let ignored = tile(&rig, API, cx);
            ask(&rig, API, false, cx);
            let heeded = tile(&rig, API, cx);

            assert_eq!(ignored, Height::Tall);
            assert_eq!(heeded, Height::Small);
        }

        #[gpui::test]
        fn a_request_for_an_unknown_agent_changes_nothing(cx: &mut TestAppContext) {
            let (rig, cx) = rig(Vec::new(), cx);
            let before = (tile(&rig, API, cx), tile(&rig, UI, cx));

            ask(&rig, 99, true, cx);
            let after_a_grow = (tile(&rig, API, cx), tile(&rig, UI, cx));
            let kept_a_grow = !nobody_asked(&rig, cx);
            ask(&rig, API, true, cx);
            ask(&rig, 99, false, cx);
            let after_a_shrink = (tile(&rig, API, cx), tile(&rig, UI, cx));

            assert_eq!(before, (Height::Small, Height::Small));
            assert_eq!(after_a_grow, before);
            assert!(!kept_a_grow);
            assert_eq!(after_a_shrink, (Height::Tall, Height::Small));
        }

        #[gpui::test]
        fn a_process_terminal_ignores_requests(cx: &mut TestAppContext) {
            let (rig, cx) = rig(Vec::new(), cx);
            let process = TerminalInfo {
                owner: Some(DaemonAgentId(API)),
                name: Some("dev server".to_owned()),
                ..info(30, DaemonKind::Process, "task:7")
            };
            rig.workspace.update_in(cx, |workspace, window, cx| {
                workspace.terminal_changed(&process, window, cx);
            });
            let process_tile = |workspace: &Workspace| {
                let first = workspace.active_task().processes.first().copied();

                first.unwrap_or(TerminalId(u32::MAX))
            };
            let before = height_of(&rig, process_tile, cx);

            ask(&rig, 30, true, cx);
            let named_like_it = height_of(&rig, process_tile, cx);
            ask(&rig, API, true, cx);
            let owner_grown = height_of(&rig, process_tile, cx);

            assert_eq!(before, Height::Process);
            assert_eq!(named_like_it, Height::Process);
            assert_eq!(owner_grown, Height::Process);
            assert_eq!(tile(&rig, API, cx), Height::Tall);
        }

        #[gpui::test]
        fn the_orchestrator_ignores_requests_and_keeps_its_share(cx: &mut TestAppContext) {
            let (rig, cx) = rig(Vec::new(), cx);
            let before = orchestrator_rect(&rig, cx);

            ask(&rig, ORCHESTRATOR, true, cx);
            let asked = orchestrator_rect(&rig, cx);
            let nothing_kept = nobody_asked(&rig, cx);
            ask(&rig, API, true, cx);
            let beside_a_large_worker = orchestrator_rect(&rig, cx);

            let (area, share) = rig.workspace.read_with(cx, |workspace, _| {
                let area = workspace
                    .terminal_area_size()
                    .expect("the area is measured");

                (area, 0.65 * (f32::from(area.width) - 20.))
            });
            assert_eq!(asked, before);
            assert!(nothing_kept);
            assert_eq!(beside_a_large_worker, before);
            assert!((before.size.width - share).abs() < 1.);
            assert!((before.size.height - (f32::from(area.height) - 20.)).abs() < 1.);
        }

        #[gpui::test]
        fn a_request_arrives_through_the_daemon(cx: &mut TestAppContext) {
            let (rig, cx) = rig(vec![(99, true), (API, true), (API, false)], cx);

            send_request(&rig, 0);
            send_request(&rig, 1);
            let arrived = wait_until(cx, |cx| asked_large(&rig, API, cx));
            let grown = tile(&rig, API, cx);
            let unknown_left_alone = rig.workspace.read_with(cx, |workspace, _| {
                workspace
                    .data
                    .agents
                    .iter()
                    .filter(|agent| agent.tile_large)
                    .count()
                    == 1
            });
            send_request(&rig, 2);
            let withdrawn = wait_until(cx, |cx| !asked_large(&rig, API, cx));
            let shrunk = tile(&rig, API, cx);

            assert!(arrived, "the grow request never arrived");
            assert_eq!(grown, Height::Tall);
            assert!(unknown_left_alone);
            assert!(withdrawn, "the shrink request never arrived");
            assert_eq!(shrunk, Height::Small);
        }

        #[gpui::test]
        fn a_request_through_the_daemon_skips_a_minimized_worker(cx: &mut TestAppContext) {
            let (rig, cx) = rig(vec![(API, true), (UI, true)], cx);
            minimize(&rig, API, cx);

            send_request(&rig, 0);
            send_request(&rig, 1);
            let later_one = wait_until(cx, |cx| asked_large(&rig, UI, cx));
            restore(&rig, API, cx);

            assert!(later_one, "the second request never arrived");
            assert!(!asked_large(&rig, API, cx));
            assert_eq!(tile(&rig, API, cx), Height::Small);
            assert_eq!(tile(&rig, UI, cx), Height::Tall);
        }

        #[gpui::test]
        fn a_request_is_applied_with_no_window_open(cx: &mut TestAppContext) {
            let (rig, cx) = rig(vec![(API, true)], cx);
            cx.update(|window, _| window.remove_window());
            cx.run_until_parked();
            rig.workspace.update(cx, Workspace::window_closed);

            send_request(&rig, 0);
            let arrived = wait_until(cx, |cx| asked_large(&rig, API, cx));

            assert!(arrived, "the request was lost while no window was open");
            assert!(cx.windows().is_empty());
            assert!(
                rig.workspace
                    .read_with(cx, |workspace, _| workspace.window.is_none())
            );
        }
    }
}
