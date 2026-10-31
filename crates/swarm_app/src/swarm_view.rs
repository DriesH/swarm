//! The Swarm view: the task's agents as a graph, a timeline below and the inspector.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    AnyElement, Bounds, ClickEvent, Context, FollowMode, FontWeight, Hsla, KeyDownEvent, Pixels,
    Role, SharedString, Window, canvas, div, fill, list, point, px, relative, size,
};
use swarm_protocol::{AgentId as DaemonAgentId, MessageId, MessageKind};
use swarm_ui::fonts::MONO_FONT;
use swarm_ui::prelude::*;
use swarm_ui::styles::focus_ring;
use swarm_ui::{ButtonVariant, ThemeColors};

use crate::model::{Agent, AgentId, AgentKind, Message};
use crate::workspace::{View, Workspace};

const ORCHESTRATOR_SIZE: (f32, f32) = (210., 80.);
const WORKER_SIZE: (f32, f32) = (160., 92.);
const WORKER_GAP: f32 = 10.;
const GRAPH_PADDING: f32 = 26.;
const ORCHESTRATOR_TOP: f32 = 52.;
const WORKER_TOP: f32 = 300.;
const GRAPH_BOTTOM: f32 = 28.;
/// How far below the orchestrator the curves bend.
const CURVE_BEND: f32 = 88.;
const EDGE_WIDTH: f32 = 1.5;
const FLIGHT_DOT: f32 = 7.;
const MAX_FLIGHT_DOTS: usize = 4;
const DOT_SPACING: f32 = 18.;
const INSPECTOR_WIDTH: f32 = 320.;
const TIMELINE_HEIGHT: f32 = 168.;
const LANE_HEIGHT: f32 = 26.;
const PREVIEW_ROWS: usize = 5;
const MIN_ZOOM: f32 = 0.6;
const MAX_ZOOM: f32 = 1.4;
const ZOOM_STEP: f32 = 0.1;
const MINUTE: u64 = 60_000;

/// A cubic curve from the orchestrator's bottom center to a worker's top center.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Curve {
    from: (f32, f32),
    to: (f32, f32),
    bend: f32,
}

impl Curve {
    fn controls(self) -> ((f32, f32), (f32, f32)) {
        let y = self.from.1 + self.bend;

        ((self.from.0, y), (self.to.0, y))
    }

    fn point_at(self, t: f32) -> (f32, f32) {
        let (a, b) = self.controls();
        let u = 1. - t;
        let weights = [u * u * u, 3. * u * u * t, 3. * u * t * t, t * t * t];
        let points = [self.from, a, b, self.to];

        points
            .iter()
            .zip(weights)
            .fold((0., 0.), |sum, (point, weight)| {
                (sum.0 + point.0 * weight, sum.1 + point.1 * weight)
            })
    }
}

fn kind_color(kind: MessageKind, colors: &ThemeColors) -> Hsla {
    match kind {
        MessageKind::Brief | MessageKind::Task => colors.accent,
        MessageKind::Question => colors.warn,
        MessageKind::Report => colors.ok,
        MessageKind::TerminalExited | MessageKind::UserInput | MessageKind::WorkerStopped => {
            colors.faint
        }
    }
}

/// The color of an edge by its newest message: none is the plain line color.
fn edge_color(kind: Option<MessageKind>, colors: &ThemeColors) -> Hsla {
    match kind {
        Some(MessageKind::Brief | MessageKind::Task) => colors.accent,
        Some(MessageKind::Question) => colors.warn,
        Some(MessageKind::Report) => colors.ok.opacity(0.7),
        _ => colors.line,
    }
}

fn between(message: &Message, a: DaemonAgentId, b: DaemonAgentId) -> bool {
    (message.from == Some(a) && message.to == b) || (message.from == Some(b) && message.to == a)
}

/// The kind of the newest message between two agents, in either direction.
fn newest_kind(messages: &[&Message], a: DaemonAgentId, b: DaemonAgentId) -> Option<MessageKind> {
    messages
        .iter()
        .filter(|message| between(message, a, b))
        .max_by_key(|message| (message.sent_at, message.id))
        .map(|message| message.kind)
}

/// Where the messages in flight between the orchestrator and a worker sit on the curve.
/// The orchestrator's start at 0.35 and move toward the worker, the worker's start at 0.65.
fn flight_dots(
    messages: &[&Message],
    orchestrator: DaemonAgentId,
    worker: DaemonAgentId,
) -> Vec<(f32, MessageKind)> {
    let mut sent_down = 0_u8;
    let mut sent_up = 0_u8;
    let mut dots = Vec::new();

    for message in messages
        .iter()
        .filter(|message| message.delivered_at.is_none() && between(message, orchestrator, worker))
    {
        if message.from == Some(orchestrator) {
            dots.push((0.35 + 0.1 * f32::from(sent_down), message.kind));
            sent_down += 1;
        } else {
            dots.push((0.65 - 0.1 * f32::from(sent_up), message.kind));
            sent_up += 1;
        }

        if dots.len() == MAX_FLIGHT_DOTS {
            break;
        }
    }

    dots
}

/// "45s", "4m" or "2h 5m". `None` while the start is unknown.
pub(crate) fn running_time(since: u64, ended_at: Option<u64>, now: u64) -> Option<String> {
    if since == 0 {
        return None;
    }

    let seconds = ended_at.unwrap_or(now).saturating_sub(since) / 1_000;
    let (minutes, hours) = (seconds / 60 % 60, seconds / 3_600);

    Some(if seconds < 60 {
        format!("{seconds}s")
    } else if hours == 0 {
        format!("{minutes}m")
    } else {
        format!("{hours}h {minutes}m")
    })
}

/// "now", "6m", "2h" or "3d".
fn relative_time(sent_at: u64, now: u64) -> String {
    let minutes = now.saturating_sub(sent_at) / MINUTE;

    if minutes == 0 {
        "now".to_owned()
    } else if minutes < 60 {
        format!("{minutes}m")
    } else if minutes < 24 * 60 {
        format!("{}h", minutes / 60)
    } else {
        format!("{}d", minutes / (24 * 60))
    }
}

/// The messages on an agent's lane: the ones it sent, and the ones Swarm sent to it.
fn lane_marks<'a>(
    messages: &'a [&'a Message],
    agent: DaemonAgentId,
) -> impl Iterator<Item = &'a Message> {
    messages.iter().copied().filter(move |message| {
        message.from == Some(agent) || (message.from.is_none() && message.to == agent)
    })
}

/// Where a time falls between the start and the end of the timeline, from 0 to 1.
fn timeline_fraction(time: u64, start: u64, end: u64) -> f32 {
    let span = end.saturating_sub(start).max(1);
    let offset = time.saturating_sub(start).min(span);

    #[expect(
        clippy::cast_precision_loss,
        reason = "the fraction only places a mark on screen"
    )]
    let fraction = offset as f32 / span as f32;

    fraction
}

fn step_zoom(zoom: f32, steps: i8) -> f32 {
    let next = zoom + ZOOM_STEP * f32::from(steps);

    ((next * 10.).round() / 10.).clamp(MIN_ZOOM, MAX_ZOOM)
}

/// "pty-io, Codex, Blocked, 1 message in flight".
fn card_label(agent: &Agent, in_flight: u32) -> String {
    let label = format!(
        "{}, {}, {}",
        agent.name,
        agent.kind.label(),
        agent.status.label()
    );

    if in_flight == 0 {
        label
    } else {
        format!(
            "{label}, {} in flight",
            plural(in_flight, "message", "messages")
        )
    }
}

fn plural(count: u32, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

impl Workspace {
    /// Now in milliseconds since the Unix epoch, or the fixed clock of fake data.
    pub(crate) fn now_ms(&self) -> u64 {
        self.fixed_now.unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
                .unwrap_or(0)
        })
    }

    /// The agent the Swarm view shows in the inspector: the one picked, else the orchestrator.
    pub(crate) fn selected_agent(&self) -> Option<AgentId> {
        let task = self.active_task();

        self.swarm_selection
            .get(&task.id)
            .copied()
            .filter(|id| task.orchestrator == Some(*id) || task.workers.contains(id))
            .or(task.orchestrator)
    }

    pub(crate) fn select_swarm_agent(&mut self, agent: AgentId, cx: &mut Context<Self>) {
        self.swarm_selection.insert(self.active_task, agent);
        cx.notify();
    }

    pub(crate) fn swarm_handle(
        &mut self,
        agent: AgentId,
        cx: &mut Context<Self>,
    ) -> gpui::FocusHandle {
        self.swarm_focus
            .entry(agent)
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone()
    }

    pub(crate) fn ensure_swarm_handles(&mut self, cx: &mut Context<Self>) {
        let task = self.active_task();
        let agents: Vec<AgentId> = task
            .orchestrator
            .into_iter()
            .chain(task.workers.iter().copied())
            .collect();

        for agent in agents {
            self.swarm_handle(agent, cx);
        }
    }

    /// Fits the thread's list to the messages. The list follows the newest message when the
    /// agent changed, or when it was already at the bottom, so reading older ones is safe.
    fn sync_messages_list(&self, agent: AgentId, thread: &[Message]) {
        let key = scroll_key(agent, thread);
        let previous = self.messages_scroll_key.get();
        let known = self.messages_list.item_count();

        if previous == Some(key) && known == thread.len() {
            return;
        }

        let same_agent = previous.is_some_and(|(previous_agent, _)| previous_agent == agent);
        let following = !same_agent || self.messages_list.is_following_tail();
        let appended = same_agent
            && thread.len() > known
            && (known == 0
                || previous.is_some_and(|(_, newest)| {
                    thread.get(known - 1).map(|message| message.id) == newest
                }));

        if appended {
            self.messages_list
                .splice(known..known, thread.len() - known);
        } else {
            self.messages_list.reset(thread.len());
        }

        if following {
            self.messages_list.set_follow_mode(FollowMode::Tail);
        }

        self.messages_scroll_key.set(Some(key));
    }

    pub(crate) fn zoom_swarm(&mut self, steps: i8, cx: &mut Context<Self>) {
        self.swarm_zoom = step_zoom(self.swarm_zoom, steps);
        cx.notify();
    }

    /// Switches to Terminals with the agent's tile focused, restored from the tray.
    pub(crate) fn focus_swarm_agent(&mut self, agent: AgentId, cx: &mut Context<Self>) {
        let Some(terminal) = self.data.agent(agent).map(|agent| agent.terminal) else {
            return;
        };

        self.view = View::Terminals;
        self.focus_terminal(terminal, cx);
    }

    /// Switches to Review, filtered to the agent.
    pub(crate) fn view_agent_diff(&mut self, agent: AgentId, cx: &mut Context<Self>) {
        self.set_view(View::Review, cx);
        self.review_agent = Some(agent);
    }

    /// Moves the selection and the keyboard focus with an arrow key. Enter focuses the tile.
    fn swarm_key(&mut self, from: AgentId, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let task = self.active_task();
        let workers = task.workers.clone();
        let orchestrator = task.orchestrator;
        let position = workers.iter().position(|id| *id == from);

        let target = match (key, position) {
            ("enter", _) => {
                self.select_swarm_agent(from, cx);
                self.focus_swarm_agent(from, cx);
                return;
            }
            ("up", Some(_)) => orchestrator,
            ("down", None) => workers.first().copied(),
            ("left", Some(index)) => workers.get(index.saturating_sub(1)).copied(),
            ("right", Some(index)) => workers.get(index + 1).copied().or(Some(from)),
            _ => None,
        };

        let Some(target) = target else {
            return;
        };

        self.select_swarm_agent(target, cx);
        let handle = self.swarm_handle(target, cx);
        window.focus(&handle, cx);
    }

    /// Messages where the agent is the sender or the recipient, oldest first.
    fn thread_of(&self, agent: AgentId) -> Vec<Message> {
        let Some(id) = self.data.agent(agent).map(Agent::protocol_id) else {
            return Vec::new();
        };

        self.data
            .task_messages(self.active_task)
            .filter(|message| message.from == Some(id) || message.to == id)
            .cloned()
            .collect()
    }

    /// The agent's worktree relative to the project root.
    fn worktree_label(&self, agent: &Agent) -> String {
        let task = self.active_task();

        let Some(daemon) = &agent.daemon else {
            let name = if task.orchestrator == Some(agent.id) {
                task.name.to_string()
            } else {
                agent.name.to_string()
            };

            return format!(".worktrees/{name}");
        };

        let root = self
            .data
            .project(task.project)
            .and_then(|project| project.root.as_deref());

        match root.and_then(|root| daemon.worktree.strip_prefix(root).ok()) {
            Some(relative) if relative.as_os_str().is_empty() => ".".to_owned(),
            Some(relative) => relative.display().to_string(),
            None => daemon.worktree.display().to_string(),
        }
    }

    fn branch_label(&self, agent: &Agent) -> SharedString {
        match &agent.daemon {
            Some(daemon) => daemon.branch.clone(),
            None if self.active_task().orchestrator == Some(agent.id) => {
                self.active_task().branch.clone()
            }
            None => SharedString::from(format!("swarm/{}", agent.name)),
        }
    }

    /// The last rows of the agent's terminal, its grid size and whether it runs.
    fn terminal_preview(&self, agent: &Agent, cx: &App) -> Preview {
        let live = self.live_terminals.get(&agent.terminal);

        if let Some(live) = live {
            let view = live.view.read(cx);
            let text = view.visible_text();
            let rows: Vec<&str> = text.lines().collect();
            let used = rows
                .iter()
                .rposition(|row| !row.trim().is_empty())
                .map_or(0, |last| last + 1);
            let first = used.saturating_sub(PREVIEW_ROWS);

            return Preview {
                lines: rows[first..used]
                    .iter()
                    .map(|row| ((*row).to_owned().into(), Color::Default))
                    .collect(),
                grid: view.grid_size(),
                running: view.exit_status().is_none(),
            };
        }

        let terminal = self.data.terminal(agent.terminal);
        let lines = terminal.map_or_else(Vec::new, |terminal| {
            let first = terminal.lines.len().saturating_sub(PREVIEW_ROWS);

            terminal.lines[first..]
                .iter()
                .map(|line| (line.text.clone(), line.color))
                .collect()
        });

        Preview {
            lines,
            grid: Some(
                terminal
                    .and_then(|terminal| terminal.requested_size)
                    .unwrap_or((80, 24)),
            ),
            running: agent.status != AgentStatus::Done,
        }
    }
}

struct Preview {
    lines: Vec<(SharedString, Color)>,
    grid: Option<(u16, u16)>,
    running: bool,
}

/// Everything the view shows, read once per frame.
struct SwarmData<'a> {
    agents: Vec<&'a Agent>,
    messages: Vec<&'a Message>,
    names: Rc<HashMap<DaemonAgentId, SharedString>>,
    in_flight: HashMap<DaemonAgentId, u32>,
    now: u64,
    selected: Option<AgentId>,
}

pub(crate) fn render(
    workspace: &Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let task = workspace.active_task();
    let agents: Vec<&Agent> = task
        .orchestrator
        .into_iter()
        .chain(task.workers.iter().copied())
        .filter_map(|id| workspace.data.agent(id))
        .collect();
    let messages: Vec<&Message> = workspace.data.task_messages(task.id).collect();

    let names = agents
        .iter()
        .map(|agent| (agent.protocol_id(), agent.name.clone()))
        .collect();
    let mut in_flight: HashMap<DaemonAgentId, u32> = HashMap::new();
    for message in messages
        .iter()
        .filter(|message| message.delivered_at.is_none())
    {
        for agent in message.from.into_iter().chain([message.to]) {
            *in_flight.entry(agent).or_default() += 1;
        }
    }

    let data = SwarmData {
        agents,
        messages,
        names: Rc::new(names),
        in_flight,
        now: workspace.now_ms(),
        selected: workspace.selected_agent(),
    };

    h_flex()
        .id("swarm-view")
        .size_full()
        .items_stretch()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(graph(workspace, &data, cx))
                .child(timeline(&data, cx)),
        )
        .child(inspector(workspace, &data, cx))
        .into_any_element()
}

fn scaled(content: impl Into<SharedString>, size: f32, zoom: f32, color: Hsla) -> gpui::Div {
    div()
        .text_size(px(size * zoom))
        .line_height(px(size * zoom * 1.4))
        .text_color(color)
        .child(content.into())
}

fn mono(content: impl Into<SharedString>, size: f32, color: Hsla) -> gpui::Div {
    div()
        .font_family(MONO_FONT)
        .text_size(px(size))
        .text_color(color)
        .child(content.into())
}

fn kind_icon(kind: AgentKind) -> IconName {
    match kind {
        AgentKind::ClaudeCode => IconName::ClaudeCode,
        AgentKind::Codex => IconName::Codex,
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one flat element tree reads best in one place"
)]
fn graph(workspace: &Workspace, data: &SwarmData, cx: &mut Context<Workspace>) -> AnyElement {
    let colors = cx.theme().colors().clone();
    let zoom = workspace.swarm_zoom;
    let scale = |value: f32| px(value * zoom);

    let orchestrator = data
        .agents
        .first()
        .filter(|agent| workspace.active_task().orchestrator == Some(agent.id));
    let workers: Vec<&Agent> = data
        .agents
        .iter()
        .copied()
        .filter(|agent| workspace.active_task().workers.contains(&agent.id))
        .collect();

    let row_width = if workers.is_empty() {
        0.
    } else {
        #[expect(clippy::cast_precision_loss, reason = "a handful of workers")]
        let count = workers.len() as f32;

        count * WORKER_SIZE.0 + (count - 1.) * WORKER_GAP
    };
    let content_width =
        (row_width + 2. * GRAPH_PADDING).max(ORCHESTRATOR_SIZE.0 + 2. * GRAPH_PADDING);
    let content_height = if workers.is_empty() {
        ORCHESTRATOR_TOP + ORCHESTRATOR_SIZE.1 + GRAPH_BOTTOM
    } else {
        WORKER_TOP + WORKER_SIZE.1 + GRAPH_BOTTOM
    };
    let row_left = (content_width - row_width) / 2.;
    let center = content_width / 2.;

    let mut cards = Vec::new();
    let mut edges = Vec::new();
    let mut extras = Vec::new();

    if let Some(agent) = orchestrator {
        cards.push(card(
            workspace,
            agent,
            data,
            (center - ORCHESTRATOR_SIZE.0 / 2., ORCHESTRATOR_TOP),
            ORCHESTRATOR_SIZE,
            true,
            cx,
        ));
    }

    for (index, agent) in workers.iter().enumerate() {
        #[expect(clippy::cast_precision_loss, reason = "a handful of workers")]
        let left = row_left + index as f32 * (WORKER_SIZE.0 + WORKER_GAP);

        cards.push(card(
            workspace,
            agent,
            data,
            (left, WORKER_TOP),
            WORKER_SIZE,
            false,
            cx,
        ));

        let Some(orchestrator) = orchestrator else {
            continue;
        };
        let curve = Curve {
            from: (center, ORCHESTRATOR_TOP + ORCHESTRATOR_SIZE.1),
            to: (left + WORKER_SIZE.0 / 2., WORKER_TOP),
            bend: CURVE_BEND,
        };
        let (one, other) = (orchestrator.protocol_id(), agent.protocol_id());
        let kind = newest_kind(&data.messages, one, other);

        edges.push((
            curve,
            edge_color(kind, &colors),
            kind == Some(MessageKind::Question),
        ));

        if let Some(kind) = kind {
            extras.push(edge_pill(curve, kind, zoom, &colors));
        }
        for (t, kind) in flight_dots(&data.messages, one, other) {
            extras.push(flight_dot(curve, t, kind, zoom, &colors));
        }
    }

    let paint_edges: Vec<(Curve, Hsla, bool)> = edges;
    let dot_color = colors.dot;

    let zoom_buttons = h_flex()
        .absolute()
        .right(px(12.))
        .top(px(10.))
        .gap(px(4.))
        .child(zoom_button(
            "zoom-out",
            IconName::Minus,
            "Zoom out",
            zoom <= MIN_ZOOM,
            -1,
            cx,
        ))
        .child(zoom_button(
            "zoom-in",
            IconName::Plus,
            "Zoom in",
            zoom >= MAX_ZOOM,
            1,
            cx,
        ));

    let content = div()
        .relative()
        .flex_none()
        .mx_auto()
        .w(scale(content_width))
        .h(scale(content_height))
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, (), window, _| {
                    for (curve, color, dashed) in &paint_edges {
                        paint_edge(bounds, *curve, *color, *dashed, zoom, window);
                    }
                },
            )
            .absolute()
            .size_full(),
        )
        .children(cards)
        .children(extras);

    div()
        .id("agent-graph")
        .role(Role::Region)
        .aria_label("Agent graph")
        .relative()
        .flex_1()
        .min_h_0()
        .bg(colors.term)
        .overflow_hidden()
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, (), window, _| paint_dot_grid(bounds, dot_color, window),
            )
            .absolute()
            .size_full(),
        )
        .child(
            div()
                .id("graph-scroll")
                .size_full()
                .flex()
                .overflow_scroll()
                .child(content),
        )
        .child(zoom_buttons)
        .into_any_element()
}

fn paint_dot_grid(bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let step = px(DOT_SPACING);
    let mut y = bounds.origin.y + step / 2.;

    while y < bounds.bottom() {
        let mut x = bounds.origin.x + step / 2.;

        while x < bounds.right() {
            window.paint_quad(fill(
                Bounds::new(point(x, y), size(px(1.5), px(1.5))),
                color,
            ));
            x += step;
        }

        y += step;
    }
}

fn paint_edge(
    bounds: Bounds<Pixels>,
    curve: Curve,
    color: Hsla,
    dashed: bool,
    zoom: f32,
    window: &mut Window,
) {
    let at = |(x, y): (f32, f32)| bounds.origin + point(px(x * zoom), px(y * zoom));
    let (first, second) = curve.controls();

    let mut builder = gpui::PathBuilder::stroke(px(EDGE_WIDTH));
    if dashed {
        builder = builder.dash_array(&[px(4.), px(4.)]);
    }
    builder.move_to(at(curve.from));
    builder.cubic_bezier_to(at(curve.to), at(first), at(second));

    match builder.build() {
        Ok(path) => window.paint_path(path, color),
        Err(error) => log::warn!("cannot draw an edge: {error}"),
    }
}

fn edge_pill(curve: Curve, kind: MessageKind, zoom: f32, colors: &ThemeColors) -> AnyElement {
    let (x, y) = curve.point_at(0.5);
    let width = 120.;

    div()
        .absolute()
        .left(px((x - width / 2.) * zoom))
        .top(px(y * zoom))
        .w(px(width * zoom))
        .flex()
        .justify_center()
        .child(
            div()
                .px(px(8. * zoom))
                .rounded(px(999.))
                .border_1()
                .border_color(colors.line)
                .bg(colors.panel)
                .child(scaled(kind.label(), 11., zoom, kind_color(kind, colors))),
        )
        .into_any_element()
}

fn flight_dot(
    curve: Curve,
    t: f32,
    kind: MessageKind,
    zoom: f32,
    colors: &ThemeColors,
) -> AnyElement {
    let (x, y) = curve.point_at(t);
    let diameter = FLIGHT_DOT * zoom;

    div()
        .absolute()
        .left(px(x * zoom - diameter / 2.))
        .top(px(y * zoom - diameter / 2.))
        .size(px(diameter))
        .rounded(px(diameter))
        .bg(kind_color(kind, colors))
        .into_any_element()
}

fn zoom_button(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    disabled: bool,
    steps: i8,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let colors = cx.theme().colors();

    div()
        .size(px(26.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .border_1()
        .border_color(colors.line)
        .bg(colors.panel)
        .child(
            IconButton::new(id, icon, label)
                .color(Color::Muted)
                .disabled(disabled)
                .on_click(cx.listener(move |workspace, _: &ClickEvent, _, cx| {
                    workspace.zoom_swarm(steps, cx);
                })),
        )
        .into_any_element()
}

fn status_note(agent: &Agent) -> SharedString {
    agent
        .daemon
        .as_ref()
        .and_then(|daemon| daemon.message.clone())
        .or_else(|| agent.note.clone())
        .unwrap_or_else(|| SharedString::new_static(agent.status.label()))
}

fn stats_line(workspace: &Workspace, agent: &Agent, zoom: f32, cx: &App) -> gpui::Div {
    let colors = cx.theme().colors();
    let (add, del) = (colors.add, colors.del);

    match workspace.data.diff_stats.get(&agent.id) {
        Some(stat) => h_flex()
            .gap(px(4. * zoom))
            .child(mono(format!("+{}", stat.additions), 11. * zoom, add))
            .child(mono(format!("\u{2212}{}", stat.deletions), 11. * zoom, del)),
        None => h_flex().child(mono("\u{2014}", 11. * zoom, colors.faint)),
    }
}

fn card(
    workspace: &Workspace,
    agent: &Agent,
    data: &SwarmData,
    (left, top): (f32, f32),
    (width, height): (f32, f32),
    is_orchestrator: bool,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let colors = cx.theme().colors().clone();
    let zoom = workspace.swarm_zoom;
    let id = agent.id;
    let selected = data.selected == Some(id);
    let in_flight = data
        .in_flight
        .get(&agent.protocol_id())
        .copied()
        .unwrap_or(0);
    let label = card_label(agent, in_flight);

    let time = running_time(agent.running_since(), agent.ended_at, data.now);
    let time_text = time.unwrap_or_else(|| "\u{2014}".to_owned());
    let second_line = if is_orchestrator {
        h_flex().child(mono(
            format!("{} \u{b7} {time_text}", agent.model),
            11. * zoom,
            colors.faint,
        ))
    } else {
        h_flex()
            .gap(px(4. * zoom))
            .child(mono(
                format!("{time_text} \u{b7}"),
                11. * zoom,
                colors.faint,
            ))
            .child(stats_line(workspace, agent, zoom, cx))
    };

    let mut card = div()
        .id(("agent-card", id.0 as usize))
        .role(Role::Button)
        .aria_label(label)
        .aria_selected(selected)
        .absolute()
        .left(px(left * zoom))
        .top(px(top * zoom))
        .w(px(width * zoom))
        .h(px(height * zoom))
        .flex()
        .flex_col()
        .gap(px(4. * zoom))
        .px(px(12. * zoom))
        .py(px(10. * zoom))
        .rounded(px(10. * zoom))
        .border_1()
        .border_color(if selected { colors.fg } else { colors.line })
        .bg(colors.panel)
        .cursor_pointer()
        .focus_visible(move |style| focus_ring(style, colors.accent))
        .on_click(cx.listener(move |workspace, _: &ClickEvent, _, cx| {
            workspace.select_swarm_agent(id, cx);
        }))
        .on_key_down(
            cx.listener(move |workspace, event: &KeyDownEvent, window, cx| {
                workspace.swarm_key(id, event.keystroke.key.as_str(), window, cx);
            }),
        );

    if let Some(handle) = workspace.swarm_focus.get(&id) {
        card = card.track_focus(handle);
    }

    card.child(
        h_flex()
            .gap(px(8. * zoom))
            .child(Icon::new(kind_icon(agent.kind)).size(IconSize::Custom(px(16. * zoom))))
            .child(
                scaled(agent.name.clone(), 13., zoom, colors.fg)
                    .font_weight(if is_orchestrator {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::MEDIUM
                    })
                    .flex_1()
                    .min_w_0()
                    .truncate(),
            )
            .child(StatusDot::new(agent.status)),
    )
    .child(second_line)
    .child(scaled(status_note(agent), 12., zoom, colors.muted).truncate())
    .into_any_element()
}

fn timeline(data: &SwarmData, cx: &mut Context<Workspace>) -> AnyElement {
    let colors = cx.theme().colors().clone();

    let start = data
        .agents
        .iter()
        .map(|agent| agent.running_since())
        .filter(|since| *since > 0)
        .min()
        .unwrap_or(data.now);
    let minutes = data.now.saturating_sub(start) / MINUTE;

    let lanes: Vec<AnyElement> = data
        .agents
        .iter()
        .map(|agent| lane(agent, data, start, cx))
        .collect();

    v_flex()
        .id("timeline")
        .role(Role::Region)
        .aria_label("Timeline")
        .h(px(TIMELINE_HEIGHT))
        .flex_none()
        .border_t_1()
        .border_color(colors.line)
        .child(
            h_flex()
                .h(px(30.))
                .flex_none()
                .gap(px(10.))
                .px(px(14.))
                .child(
                    div().w(px(96.)).flex_none().child(
                        Label::new("Timeline")
                            .size(LabelSize::Small)
                            .weight(FontWeight::MEDIUM),
                    ),
                )
                .child(
                    Label::new("0m")
                        .mono()
                        .size(LabelSize::XSmall)
                        .color(Color::Faint),
                )
                .child(div().flex_1())
                .child(
                    Label::new(format!("{minutes}m"))
                        .mono()
                        .size(LabelSize::XSmall)
                        .color(Color::Faint),
                ),
        )
        .child(
            v_flex()
                .id("timeline-lanes")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(lanes),
        )
        .into_any_element()
}

fn lane(agent: &Agent, data: &SwarmData, start: u64, cx: &mut Context<Workspace>) -> AnyElement {
    let colors = cx.theme().colors().clone();
    let marks: Vec<&Message> = lane_marks(&data.messages, agent.protocol_id()).collect();

    let from = timeline_fraction(agent.running_since(), start, data.now);
    let to = timeline_fraction(agent.ended_at.unwrap_or(data.now), start, data.now);

    let rings: Vec<AnyElement> = marks
        .iter()
        .map(|message| {
            let at = timeline_fraction(message.sent_at, start, data.now);

            div()
                .absolute()
                .top(px(2.))
                .left(relative(at))
                .ml(px(-5.))
                .size(px(10.))
                .rounded(px(10.))
                .border_2()
                .border_color(kind_color(message.kind, &colors))
                .bg(colors.panel)
                .into_any_element()
        })
        .collect();

    h_flex()
        .id(("lane", agent.id.0 as usize))
        .role(Role::Group)
        .aria_label(format!(
            "{}, {}",
            agent.name,
            plural(
                u32::try_from(marks.len()).unwrap_or(u32::MAX),
                "message",
                "messages"
            )
        ))
        .h(px(LANE_HEIGHT))
        .flex_none()
        .px(px(14.))
        .gap(px(10.))
        .child(
            div().w(px(96.)).flex_none().child(
                Label::new(agent.name.clone())
                    .mono()
                    .size(LabelSize::XSmall)
                    .color(Color::Muted)
                    .truncate(),
            ),
        )
        .child(
            div()
                .flex_1()
                .relative()
                .h(px(14.))
                .when(agent.running_since() > 0, |track| {
                    track.child(
                        div()
                            .absolute()
                            .top(px(6.))
                            .h(px(2.))
                            .rounded(px(1.))
                            .left(relative(from))
                            .w(relative((to - from).max(0.)))
                            .bg(agent.status.background(cx)),
                    )
                })
                .children(rings),
        )
        .into_any_element()
}

fn inspector(workspace: &Workspace, data: &SwarmData, cx: &mut Context<Workspace>) -> AnyElement {
    let colors = cx.theme().colors().clone();

    let selected = data
        .selected
        .and_then(|id| data.agents.iter().copied().find(|agent| agent.id == id));

    let body = match selected {
        Some(agent) => inspector_body(workspace, agent, data, cx),
        None => div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .child(Label::new("No agent").color(Color::Faint))
            .into_any_element(),
    };

    v_flex()
        .id("inspector")
        .role(Role::Complementary)
        .aria_label("Selected agent")
        .w(px(INSPECTOR_WIDTH))
        .flex_none()
        .h_full()
        .min_h_0()
        .border_l_1()
        .border_color(colors.line)
        .child(body)
        .into_any_element()
}

fn definition(label: &'static str, value: impl IntoElement, colors: &ThemeColors) -> AnyElement {
    h_flex()
        .items_start()
        .child(
            div()
                .w(px(84.))
                .flex_none()
                .text_size(px(12.))
                .text_color(colors.faint)
                .child(label),
        )
        .child(div().flex_1().min_w_0().text_size(px(12.)).child(value))
        .into_any_element()
}

#[expect(
    clippy::too_many_lines,
    reason = "one flat element tree reads best in one place"
)]
fn inspector_body(
    workspace: &Workspace,
    agent: &Agent,
    data: &SwarmData,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let colors = cx.theme().colors().clone();
    let id = agent.id;

    let changes = match workspace.data.diff_stats.get(&id) {
        Some(stat) => h_flex()
            .gap(px(4.))
            .child(
                div()
                    .text_color(colors.add)
                    .child(format!("+{}", stat.additions)),
            )
            .child(
                div()
                    .text_color(colors.del)
                    .child(format!("\u{2212}{}", stat.deletions)),
            )
            .child(
                div()
                    .text_color(colors.muted)
                    .child(format!("\u{b7} {}", plural(stat.files, "file", "files"))),
            )
            .into_any_element(),
        None => div().child("\u{2014}").into_any_element(),
    };
    let running = running_time(agent.running_since(), agent.ended_at, data.now)
        .unwrap_or_else(|| "\u{2014}".to_owned());

    let header = h_flex()
        .p(px(14.))
        .gap(px(10.))
        .flex_none()
        .border_b_1()
        .border_color(colors.line)
        .child(Icon::new(kind_icon(agent.kind)).size(IconSize::Custom(px(20.))))
        .child(
            Label::new(agent.name.clone())
                .size(LabelSize::Large)
                .weight(FontWeight::SEMIBOLD),
        )
        .child(div().flex_1())
        .child(StatusPill::new(agent.status));

    let definitions = v_flex()
        .flex_none()
        .gap(px(8.))
        .px(px(14.))
        .py(px(12.))
        .border_b_1()
        .border_color(colors.line)
        .child(definition(
            "Agent",
            format!("{} \u{b7} {}", agent.kind.label(), agent.model),
            &colors,
        ))
        .child(definition(
            "Worktree",
            mono(workspace.worktree_label(agent), 11., colors.fg),
            &colors,
        ))
        .child(definition(
            "Branch",
            mono(workspace.branch_label(agent), 11., colors.fg),
            &colors,
        ))
        .child(definition("Running", running, &colors))
        .child(definition("Changes", changes, &colors));

    let thread = workspace.thread_of(id);
    workspace.sync_messages_list(id, &thread);

    let names = Rc::clone(&data.names);
    let (now, row_colors) = (data.now, colors.clone());
    let rows = list(workspace.messages_list.clone(), move |index, _, _| {
        thread.get(index).map_or_else(
            || div().into_any_element(),
            |message| message_row(index, message, now, &names, &row_colors),
        )
    })
    .size_full();

    let messages = v_flex()
        .flex_1()
        .min_h_0()
        .px(px(14.))
        .pt(px(12.))
        .gap(px(12.))
        .child(
            Label::new("Messages")
                .size(LabelSize::Small)
                .color(Color::Faint),
        )
        .child(
            div()
                .id("message-thread")
                .role(Role::List)
                .aria_label("Messages")
                .flex_1()
                .min_h_0()
                .child(rows),
        );

    let preview = workspace.terminal_preview(agent, cx);
    let rows: Vec<AnyElement> = preview
        .lines
        .iter()
        .map(|(text, color)| {
            let text = if text.is_empty() {
                SharedString::new_static(" ")
            } else {
                text.clone()
            };

            div()
                .whitespace_nowrap()
                .text_color(color.hsla(cx))
                .child(text)
                .into_any_element()
        })
        .collect();
    let terminal = v_flex()
        .flex_none()
        .gap(px(6.))
        .px(px(14.))
        .pt(px(12.))
        .child(
            h_flex()
                .gap(px(6.))
                .child(div().size(px(6.)).rounded(px(6.)).bg(if preview.running {
                    colors.ok
                } else {
                    colors.faint
                }))
                .child(
                    Label::new("Live terminal")
                        .size(LabelSize::Small)
                        .color(Color::Faint),
                )
                .child(div().flex_1())
                .children(preview.grid.map(|(columns, rows)| {
                    Label::new(format!("{columns}\u{d7}{rows}"))
                        .mono()
                        .size(LabelSize::XSmall)
                        .color(Color::Faint)
                })),
        )
        .child(
            div()
                .id("terminal-preview")
                .role(Role::Group)
                .aria_label(format!("{} terminal preview", agent.name))
                .h(px(118.))
                .px(px(10.))
                .py(px(8.))
                .rounded(px(8.))
                .border_1()
                .border_color(colors.line)
                .bg(colors.term)
                .overflow_hidden()
                .font_family(MONO_FONT)
                .text_size(px(11.))
                .line_height(px(17.6))
                .children(rows),
        );

    let buttons = h_flex()
        .flex_none()
        .gap(px(8.))
        .px(px(14.))
        .py(px(12.))
        .child(
            div().flex_1().child(
                Button::new("inspector-focus", "Focus")
                    .variant(ButtonVariant::Primary)
                    .full_width()
                    .on_click(cx.listener(move |workspace, _: &ClickEvent, _, cx| {
                        workspace.focus_swarm_agent(id, cx);
                    })),
            ),
        )
        .child(
            div().flex_1().child(
                Button::new("inspector-diff", "View diff")
                    .full_width()
                    .on_click(cx.listener(move |workspace, _: &ClickEvent, _, cx| {
                        workspace.view_agent_diff(id, cx);
                    })),
            ),
        );

    v_flex()
        .size_full()
        .min_h_0()
        .child(header)
        .child(definitions)
        .child(messages)
        .child(terminal)
        .child(buttons)
        .into_any_element()
}

fn agent_name(
    names: &HashMap<DaemonAgentId, SharedString>,
    id: Option<DaemonAgentId>,
) -> SharedString {
    let Some(id) = id else {
        return "swarm".into();
    };

    names
        .get(&id)
        .cloned()
        .unwrap_or_else(|| SharedString::new_static("agent"))
}

fn message_row(
    index: usize,
    message: &Message,
    now: u64,
    names: &HashMap<DaemonAgentId, SharedString>,
    colors: &ThemeColors,
) -> AnyElement {
    let when = if message.delivered_at.is_none() {
        "in flight".to_owned()
    } else {
        relative_time(message.sent_at, now)
    };

    v_flex()
        .id(("message", index))
        .role(Role::ListItem)
        .pb(px(12.))
        .child(
            v_flex()
                .gap(px(4.))
                .child(
                    h_flex()
                        .gap(px(6.))
                        .child(
                            mono(agent_name(names, message.from), 11., colors.fg)
                                .font_weight(FontWeight::MEDIUM),
                        )
                        .child(mono("\u{2192}", 11., colors.faint))
                        .child(mono(agent_name(names, Some(message.to)), 11., colors.faint))
                        .child(div().flex_1())
                        .child(mono(when, 11., colors.faint)),
                )
                .child(
                    div()
                        .text_size(px(12.5))
                        .line_height(px(18.75))
                        .text_color(colors.muted)
                        .child(message.text.clone()),
                ),
        )
        .into_any_element()
}

/// What the thread was last scrolled for: the agent and its newest message.
fn scroll_key(agent: AgentId, thread: &[Message]) -> (AgentId, Option<MessageId>) {
    (agent, thread.last().map(|message| message.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions;
    use crate::model::{SAMPLE_NOW, TaskId, TerminalId};
    use crate::workspace::RightPanel;
    use gpui::{TestAppContext, VisualTestContext};

    fn setup(cx: &mut TestAppContext) -> (gpui::Entity<Workspace>, &mut VisualTestContext) {
        cx.update(|cx| {
            swarm_ui::init(cx);
            actions::bind_keys(cx);
        });

        let (workspace, cx) = cx.add_window_view(Workspace::with_sample);
        workspace.update(cx, |workspace, cx| workspace.set_view(View::Swarm, cx));
        redraw(cx);

        (workspace, cx)
    }

    fn redraw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
    }

    fn agents(workspace: &Workspace) -> Vec<(AgentId, SharedString, DaemonAgentId)> {
        let task = workspace.active_task();

        task.orchestrator
            .into_iter()
            .chain(task.workers.iter().copied())
            .filter_map(|id| workspace.data.agent(id))
            .map(|agent| (agent.id, agent.name.clone(), agent.protocol_id()))
            .collect()
    }

    fn id_of(workspace: &Workspace, name: &str) -> DaemonAgentId {
        agents(workspace)
            .into_iter()
            .find(|(_, agent, _)| agent == name)
            .map_or(DaemonAgentId(u64::MAX), |(_, _, id)| id)
    }

    #[gpui::test]
    fn edges_take_the_kind_of_the_newest_message(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let kinds = workspace.read_with(cx, |workspace, _| {
            let messages: Vec<&Message> = workspace
                .data
                .task_messages(workspace.active_task)
                .collect();
            let orchestrator = id_of(workspace, "orchestrator");

            ["term-render", "pty-io", "ui-tokens", "bench"]
                .map(|name| newest_kind(&messages, orchestrator, id_of(workspace, name)))
        });

        assert_eq!(
            kinds,
            [
                Some(MessageKind::Task),
                Some(MessageKind::Question),
                Some(MessageKind::Report),
                None
            ]
        );
    }

    #[gpui::test]
    fn messages_in_flight_become_dots_by_sender(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let (down, up, none) = workspace.read_with(cx, |workspace, _| {
            let messages: Vec<&Message> = workspace
                .data
                .task_messages(workspace.active_task)
                .collect();
            let orchestrator = id_of(workspace, "orchestrator");
            let dots = |name| flight_dots(&messages, orchestrator, id_of(workspace, name));

            (dots("term-render"), dots("pty-io"), dots("ui-tokens"))
        });

        assert_eq!(down, [(0.35, MessageKind::Task)]);
        assert_eq!(up, [(0.65, MessageKind::Question)]);
        assert!(none.is_empty());
    }

    #[test]
    fn dots_are_capped_and_spread_along_the_curve() {
        let (orchestrator, worker) = (DaemonAgentId(1), DaemonAgentId(2));
        let messages: Vec<Message> = (0..6)
            .map(|id| Message {
                id: MessageId(id),
                from: Some(orchestrator),
                to: worker,
                kind: MessageKind::Task,
                text: SharedString::default(),
                sent_at: id,
                delivered_at: None,
            })
            .collect();
        let refs: Vec<&Message> = messages.iter().collect();

        let dots = flight_dots(&refs, orchestrator, worker);

        assert_eq!(dots.len(), 4);
        assert!((dots[1].0 - 0.45).abs() < 1e-6);
    }

    #[test]
    fn running_time_is_short() {
        let start = 1_000_000;

        assert_eq!(
            running_time(start, None, start + 45_000).as_deref(),
            Some("45s")
        );
        assert_eq!(
            running_time(start, None, start + 4 * 60_000).as_deref(),
            Some("4m")
        );
        assert_eq!(
            running_time(start, None, start + (2 * 60 + 5) * 60_000).as_deref(),
            Some("2h 5m")
        );
        assert_eq!(
            running_time(start, Some(start + 60_000), start + 9_000_000).as_deref(),
            Some("1m")
        );
        assert_eq!(running_time(0, None, start), None);
        assert_eq!(relative_time(start, start + 6 * 60_000), "6m");
        assert_eq!(relative_time(start, start + 30_000), "now");
        assert_eq!(relative_time(start, start + 2 * 3_600_000), "2h");
    }

    #[test]
    fn timeline_marks_sit_between_start_and_end() {
        assert!((timeline_fraction(150, 100, 200) - 0.5).abs() < 1e-6);
        assert!(timeline_fraction(50, 100, 200).abs() < 1e-6);
        assert!((timeline_fraction(500, 100, 200) - 1.).abs() < 1e-6);
    }

    #[gpui::test]
    fn a_lane_holds_what_the_agent_sent_and_what_swarm_sent_it(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let (orchestrator, pty_io, mark) = workspace.read_with(cx, |workspace, _| {
            let messages: Vec<&Message> = workspace
                .data
                .task_messages(workspace.active_task)
                .collect();
            let count = |name| lane_marks(&messages, id_of(workspace, name)).count();
            let first = lane_marks(&messages, id_of(workspace, "orchestrator")).next();
            let start = SAMPLE_NOW - 840_000;

            (
                count("orchestrator"),
                count("pty-io"),
                first.map(|message| timeline_fraction(message.sent_at, start, SAMPLE_NOW)),
            )
        });

        assert_eq!((orchestrator, pty_io), (4, 1));
        assert!(mark.is_some_and(|fraction| fraction > 0.05 && fraction < 0.07));
    }

    #[gpui::test]
    fn the_orchestrator_is_selected_and_arrow_keys_move_the_selection(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let ids: Vec<AgentId> = workspace.read_with(cx, |workspace, _| {
            agents(workspace).iter().map(|a| a.0).collect()
        });
        assert_eq!(
            workspace.read_with(cx, |workspace, _| workspace.selected_agent()),
            ids.first().copied()
        );

        let focus = |agent: AgentId, cx: &mut VisualTestContext| {
            workspace.update_in(cx, |workspace, window, cx| {
                let handle = workspace.swarm_handle(agent, cx);
                window.focus(&handle, cx);
            });
        };
        let selected = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, _| workspace.selected_agent())
        };

        focus(ids[0], cx);
        cx.simulate_keystrokes("down");
        assert_eq!(selected(cx), Some(ids[1]));

        cx.simulate_keystrokes("right");
        assert_eq!(selected(cx), Some(ids[2]));

        cx.simulate_keystrokes("left left");
        assert_eq!(selected(cx), Some(ids[1]));

        cx.simulate_keystrokes("up");
        assert_eq!(selected(cx), Some(ids[0]));
    }

    #[gpui::test]
    fn focus_leaves_the_swarm_view_for_the_agents_tile(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let (worker, terminal) = workspace.read_with(cx, |workspace, _| {
            let id = workspace.active_task().workers[1];
            (id, workspace.data.agent(id).map(|agent| agent.terminal))
        });
        let terminal = terminal.unwrap_or(TerminalId(u32::MAX));
        workspace.update(cx, |workspace, cx| workspace.minimize(terminal, cx));

        workspace.update(cx, |workspace, cx| workspace.focus_swarm_agent(worker, cx));

        let (view, focused, minimized) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.view,
                workspace.focused_terminal,
                workspace.minimized.contains(&terminal),
            )
        });
        assert_eq!(view, View::Terminals);
        assert_eq!(focused, Some(terminal));
        assert!(!minimized);
    }

    #[gpui::test]
    fn enter_on_a_card_focuses_its_tile(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let worker = workspace.read_with(cx, |workspace, _| workspace.active_task().workers[0]);
        workspace.update_in(cx, |workspace, window, cx| {
            let handle = workspace.swarm_handle(worker, cx);
            window.focus(&handle, cx);
        });

        cx.simulate_keystrokes("enter");

        let (view, focused, expected) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.view,
                workspace.focused_terminal,
                workspace.data.agent(worker).map(|agent| agent.terminal),
            )
        });
        assert_eq!(view, View::Terminals);
        assert_eq!(focused, expected);
    }

    #[gpui::test]
    fn view_diff_opens_review_for_the_agent(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let worker = workspace.read_with(cx, |workspace, _| workspace.active_task().workers[0]);

        workspace.update(cx, |workspace, cx| workspace.view_agent_diff(worker, cx));

        let (view, agent) =
            workspace.read_with(cx, |workspace, _| (workspace.view, workspace.review_agent));
        assert_eq!(view, View::Review);
        assert_eq!(agent, Some(worker));
    }

    #[gpui::test]
    fn delivering_a_message_ends_its_flight(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let in_flight = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, _| {
                let messages: Vec<&Message> = workspace
                    .data
                    .task_messages(workspace.active_task)
                    .collect();
                let orchestrator = id_of(workspace, "orchestrator");

                ["term-render", "pty-io"]
                    .map(|name| flight_dots(&messages, orchestrator, id_of(workspace, name)).len())
            })
        };
        assert_eq!(in_flight(cx), [1, 1]);

        workspace.update(cx, |workspace, cx| {
            workspace.message_event(
                swarm_protocol::MessageEvent::Delivered {
                    ids: vec![MessageId(6), MessageId(7)],
                    delivered_at: SAMPLE_NOW,
                },
                cx,
            );
        });

        assert_eq!(in_flight(cx), [0, 0]);
    }

    #[gpui::test]
    fn the_swarm_view_hides_the_right_panel_but_keeps_the_choice(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let (panel, choice) = workspace.read_with(cx, |workspace, _| {
            (workspace.visible_panel(), workspace.right_panel)
        });
        workspace.update(cx, |workspace, cx| workspace.set_view(View::Terminals, cx));
        let back = workspace.read_with(cx, |workspace, _| workspace.visible_panel());

        assert_eq!(panel, None);
        assert_eq!(choice, Some(RightPanel::Changes));
        assert_eq!(back, Some(RightPanel::Changes));
    }

    #[gpui::test]
    fn a_card_label_counts_the_messages_in_flight(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let (plain, one, two) = workspace.read_with(cx, |workspace, _| {
            let worker = workspace.active_task().workers[1];
            let agent = workspace.data.agent(worker).expect("a worker");

            (
                card_label(agent, 0),
                card_label(agent, 1),
                card_label(agent, 2),
            )
        });

        assert_eq!(plain, "pty-io, Codex, Blocked");
        assert_eq!(one, "pty-io, Codex, Blocked, 1 message in flight");
        assert_eq!(two, "pty-io, Codex, Blocked, 2 messages in flight");
    }

    #[test]
    fn the_scroll_key_follows_the_newest_message_not_the_count() {
        let message = |id: u64| Message {
            id: MessageId(id),
            from: None,
            to: DaemonAgentId(1),
            kind: MessageKind::Task,
            text: SharedString::default(),
            sent_at: id,
            delivered_at: None,
        };
        let agent = AgentId(1);
        let before = [message(1), message(2)];
        let after = [message(2), message(3)];

        assert_eq!(before.len(), after.len());
        assert_ne!(scroll_key(agent, &before), scroll_key(agent, &after));
        assert_eq!(scroll_key(agent, &[]), (agent, None));
    }

    fn add_thread_messages(
        workspace: &gpui::Entity<Workspace>,
        range: std::ops::Range<u64>,
        cx: &mut VisualTestContext,
    ) {
        workspace.update(cx, |workspace, _| {
            let orchestrator = id_of(workspace, "orchestrator");
            let Some((project, task)) = workspace.data.message_key(workspace.active_task) else {
                return;
            };

            for id in range {
                workspace.data.record_message(&swarm_protocol::MessageInfo {
                    id: MessageId(id),
                    project,
                    task,
                    from: Some(orchestrator),
                    to: id_of(workspace, "bench"),
                    kind: MessageKind::Task,
                    text: format!("a long enough message to take some room {id}"),
                    sent_at: SAMPLE_NOW,
                    delivered_at: Some(SAMPLE_NOW),
                });
            }
        });
    }

    fn following(workspace: &gpui::Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
        workspace.read_with(cx, |workspace, _| {
            workspace.messages_list.is_following_tail()
        })
    }

    fn scroll_position(
        workspace: &gpui::Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> gpui::ListOffset {
        workspace.read_with(cx, |workspace, _| {
            workspace.messages_list.logical_scroll_top()
        })
    }

    #[gpui::test]
    fn a_new_message_scrolls_only_a_thread_that_was_at_the_bottom(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        add_thread_messages(&workspace, 100..140, cx);
        redraw(cx);
        redraw(cx);
        assert!(following(&workspace, cx));

        add_thread_messages(&workspace, 140..141, cx);
        redraw(cx);
        redraw(cx);
        assert!(following(&workspace, cx));

        workspace.read_with(cx, |workspace, _| {
            workspace.messages_list.scroll_by(px(-600.));
        });
        redraw(cx);
        let reading = scroll_position(&workspace, cx);
        assert!(!following(&workspace, cx));

        add_thread_messages(&workspace, 141..142, cx);
        redraw(cx);
        redraw(cx);
        assert!(!following(&workspace, cx));
        assert_eq!(scroll_position(&workspace, cx).item_ix, reading.item_ix);
    }

    #[gpui::test]
    fn only_view_diff_filters_review_to_an_agent(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let worker = workspace.read_with(cx, |workspace, _| workspace.active_task().workers[0]);
        let filter = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, _| workspace.review_agent)
        };

        workspace.update(cx, |workspace, cx| workspace.view_agent_diff(worker, cx));
        assert_eq!(filter(cx), Some(worker));

        workspace.update_in(cx, |_, window, cx| {
            window.dispatch_action(Box::new(actions::ShowReview), cx);
        });
        assert_eq!(filter(cx), None);

        workspace.update(cx, |workspace, cx| workspace.view_agent_diff(worker, cx));
        workspace.update(cx, |workspace, cx| workspace.set_view(View::Review, cx));
        assert_eq!(filter(cx), None);
    }

    #[gpui::test]
    fn zoom_stays_between_the_limits(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        for _ in 0..10 {
            workspace.update(cx, |workspace, cx| workspace.zoom_swarm(1, cx));
        }
        let most = workspace.read_with(cx, |workspace, _| workspace.swarm_zoom);
        for _ in 0..10 {
            workspace.update(cx, |workspace, cx| workspace.zoom_swarm(-1, cx));
        }
        let least = workspace.read_with(cx, |workspace, _| workspace.swarm_zoom);

        assert!((most - MAX_ZOOM).abs() < 1e-6);
        assert!((least - MIN_ZOOM).abs() < 1e-6);
    }

    #[gpui::test]
    fn a_start_task_shows_only_its_orchestrator(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        workspace.update(cx, |workspace, cx| workspace.select_task(TaskId(1), cx));
        redraw(cx);

        let count = workspace.read_with(cx, |workspace, _| agents(workspace).len());
        assert_eq!(count, 1);
    }
}
