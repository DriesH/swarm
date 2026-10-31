//! The Swarm view: the task's agents as a graph and the inspector.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    AnyElement, Bounds, ClickEvent, Context, Entity, FollowMode, FontWeight, Hsla, KeyDownEvent,
    ListAlignment, ListState, MouseButton, MouseMoveEvent, Pixels, Role, SharedString,
    StyleRefinement, Subscription, Window, canvas, div, linear_color_stop, linear_gradient, list,
    point, px, uniform_list,
};
use swarm_protocol::{AgentId as DaemonAgentId, FileStat, MessageId, MessageKind};
use swarm_term_view::TerminalView;
use swarm_ui::fonts::MONO_FONT;
use swarm_ui::prelude::*;
use swarm_ui::styles::focus_ring;
use swarm_ui::{ButtonVariant, Nudge, ThemeColors};

use crate::markdown::{self, Markdown, Parsed};
use crate::model::{Agent, AgentId, AgentKind, Message, TerminalId};
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
const DOT_SIZE: f32 = 1.5;
pub(crate) const INSPECTOR_WIDTH: f32 = 400.;
pub(crate) const INSPECTOR_MIN_WIDTH: f32 = 320.;
pub(crate) const INSPECTOR_MAX_WIDTH: f32 = 640.;
const INSPECTOR_KEY_STEP: f32 = 20.;
/// The rows of a terminal the preview keeps; the box shows as many as fit.
const PREVIEW_ROWS: usize = 60;
/// A folded message is this high.
const FOLDED_HEIGHT: f32 = 214.;
/// Parsed messages kept after the thread changed; more than this are parsed again.
const PARSED_LIMIT: usize = 2_000;
const MIN_ZOOM: f32 = 0.6;
const MAX_ZOOM: f32 = 1.4;
const ZOOM_STEP: f32 = 0.1;
const MINUTE: u64 = 60_000;

/// What the inspector shows under its tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum InspectorTab {
    #[default]
    Messages,
    Changes,
    Terminal,
}

impl InspectorTab {
    fn index(self) -> usize {
        match self {
            Self::Messages => 0,
            Self::Changes => 1,
            Self::Terminal => 2,
        }
    }

    fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Changes,
            2 => Self::Terminal,
            _ => Self::Messages,
        }
    }
}

/// A drag on the inspector's left border.
#[derive(Clone, Copy, Debug)]
pub(crate) struct InspectorDrag {
    start_x: Pixels,
    start_width: f32,
}

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

    /// Hands the inspector's entities what they show. It runs after every notify of the
    /// workspace, and only changed input reaches them, so a scroll or a terminal tick that
    /// notifies one entity does not redo the work here.
    pub(crate) fn sync_inspector(&mut self, cx: &mut Context<Self>) {
        let Some(agent) = self.selected_agent().and_then(|id| self.data.agent(id)) else {
            return;
        };
        let (id, name) = (agent.id, agent.name.clone());
        let fallback = self.fake_preview(agent);
        let live = self
            .live_terminals
            .get(&agent.terminal)
            .map(|live| live.view.clone());

        let revision = self.data.messages_revision;
        let thread = self.thread.clone();
        let messages = (!thread.read(cx).shows(id, revision))
            .then(|| Rc::<[Message]>::from(self.thread_of(id)));
        let names = self.agent_names();
        let now = self.now_ms();

        thread.update(cx, |thread, cx| {
            thread.show(id, revision, messages, names, now, cx);
        });
        self.preview.update(cx, |preview, cx| {
            preview.show(name, live, fallback, cx);
        });
    }

    fn agent_names(&self) -> Rc<HashMap<DaemonAgentId, SharedString>> {
        let task = self.active_task();

        Rc::new(
            task.orchestrator
                .into_iter()
                .chain(task.workers.iter().copied())
                .filter_map(|id| self.data.agent(id))
                .map(|agent| (agent.protocol_id(), agent.name.clone()))
                .collect(),
        )
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

    /// Switches to Review, filtered to the agent and opened at one of its files.
    pub(crate) fn view_file_diff(&mut self, agent: AgentId, path: PathBuf, cx: &mut Context<Self>) {
        self.view_agent_diff(agent, cx);
        self.review_file = Some(path);
    }

    /// Switches to Terminals with a process terminal's tile focused.
    pub(crate) fn focus_process(&mut self, terminal: TerminalId, cx: &mut Context<Self>) {
        self.view = View::Terminals;
        self.focus_terminal(terminal, cx);
    }

    /// The tab is kept for the whole window, not for each agent.
    pub(crate) fn select_inspector_tab(&mut self, tab: InspectorTab, cx: &mut Context<Self>) {
        if self.inspector_tab != tab {
            self.inspector_tab = tab;
            cx.notify();
        }
    }

    pub(crate) fn set_inspector_width(&mut self, width: f32, cx: &mut Context<Self>) {
        let width = width.clamp(INSPECTOR_MIN_WIDTH, INSPECTOR_MAX_WIDTH);

        if (width - self.inspector_width).abs() > f32::EPSILON {
            self.inspector_width = width;
            cx.notify();
        }
    }

    fn start_inspector_drag(&mut self, start_x: Pixels, cx: &mut Context<Self>) {
        self.inspector_drag = Some(InspectorDrag {
            start_x,
            start_width: self.inspector_width,
        });
        cx.notify();
    }

    /// Dragging the border to the left widens the inspector.
    fn drag_inspector(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = self.inspector_drag else {
            return;
        };

        if event.pressed_button != Some(MouseButton::Left) {
            self.end_inspector_drag(cx);
            return;
        }

        let moved = f32::from(drag.start_x - event.position.x);
        self.set_inspector_width(drag.start_width + moved, cx);
    }

    fn end_inspector_drag(&mut self, cx: &mut Context<Self>) {
        if self.inspector_drag.take().is_some() {
            cx.notify();
        }
    }

    /// Left moves the border left, which widens the inspector.
    pub(crate) fn nudge_inspector(&mut self, nudge: Nudge, cx: &mut Context<Self>) {
        let step = match nudge {
            Nudge::Left => INSPECTOR_KEY_STEP,
            Nudge::Right => -INSPECTOR_KEY_STEP,
        };

        self.set_inspector_width(self.inspector_width + step, cx);
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

    /// True when the agent works in the worktree of its task.
    fn in_task_worktree(&self, agent: &Agent) -> bool {
        let task = self.active_task();

        match (&agent.daemon, &task.daemon) {
            (Some(daemon), Some(task_daemon)) => daemon.worktree == task_daemon.worktree,
            _ => task.orchestrator == Some(agent.id),
        }
    }

    /// How many messages the agent sent or got.
    fn thread_len(&self, agent: AgentId) -> usize {
        let Some(id) = self.data.agent(agent).map(Agent::protocol_id) else {
            return 0;
        };

        self.data
            .task_messages(self.active_task)
            .filter(|message| message.from == Some(id) || message.to == id)
            .count()
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

    /// The preview of an agent without a live terminal: the last rows of its fake lines.
    fn fake_preview(&self, agent: &Agent) -> Preview {
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

#[derive(Clone, Default, PartialEq)]
struct Preview {
    lines: Vec<(SharedString, Color)>,
    grid: Option<(u16, u16)>,
    running: bool,
}

/// The last rows of the selected agent's terminal. It reads a live terminal's text only
/// when that terminal changes, not on every frame of the window.
pub(crate) struct TerminalPreview {
    agent_name: SharedString,
    live: Option<(Entity<TerminalView>, Subscription)>,
    fallback: Preview,
    shown: Preview,
}

impl TerminalPreview {
    pub(crate) fn new() -> Self {
        Self {
            agent_name: SharedString::default(),
            live: None,
            fallback: Preview::default(),
            shown: Preview::default(),
        }
    }

    fn show(
        &mut self,
        agent_name: SharedString,
        live: Option<Entity<TerminalView>>,
        fallback: Preview,
        cx: &mut Context<Self>,
    ) {
        let same_live = self.live.as_ref().map(|(view, _)| view.entity_id())
            == live.as_ref().map(Entity::entity_id);
        let changed = !same_live || self.agent_name != agent_name || self.fallback != fallback;

        if !changed {
            return;
        }

        if !same_live {
            self.live = live.map(|view| {
                let subscription = cx.observe(&view, |this, _, cx| this.refresh(cx));

                (view, subscription)
            });
        }
        self.agent_name = agent_name;
        self.fallback = fallback;

        self.refresh(cx);
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some((view, _)) = &self.live else {
            self.shown = self.fallback.clone();
            cx.notify();
            return;
        };

        let view = view.read(cx);
        let text = view.visible_text();
        let rows: Vec<&str> = text.lines().collect();
        let used = rows
            .iter()
            .rposition(|row| !row.trim().is_empty())
            .map_or(0, |last| last + 1);
        let first = used.saturating_sub(PREVIEW_ROWS);

        self.shown = Preview {
            lines: rows[first..used]
                .iter()
                .map(|row| ((*row).to_owned().into(), Color::Default))
                .collect(),
            grid: view.grid_size(),
            running: view.exit_status().is_none(),
        };
        cx.notify();
    }
}

impl Render for TerminalPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors().clone();
        let preview = &self.shown;

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
                    .flex_none()
                    .whitespace_nowrap()
                    .text_color(color.hsla(cx))
                    .child(text)
                    .into_any_element()
            })
            .collect();

        v_flex()
            .size_full()
            .gap(px(6.))
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(6.))
                    .child(div().size(px(6.)).rounded(px(6.)).bg(if preview.running {
                        colors.ok
                    } else {
                        colors.faint
                    }))
                    .child(
                        Label::new(self.agent_name.clone())
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
                v_flex()
                    .id("terminal-preview")
                    .role(Role::Group)
                    .aria_label(format!("{} terminal preview", self.agent_name))
                    .flex_1()
                    .min_h_0()
                    .justify_end()
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
            )
    }
}

/// The message thread of the selected agent. It owns its list, so a scroll notifies only
/// this entity, and it is cached, so a frame that leaves it alone reuses its last paint.
pub(crate) struct MessageThread {
    list: ListState,
    agent: Option<AgentId>,
    revision: u64,
    messages: Rc<[Message]>,
    /// The parsed text of each message, in the order of `messages`.
    parsed: Parsed,
    /// Parsed text by message, so a message is parsed once however often the thread changes.
    parse_cache: HashMap<MessageId, Rc<Markdown>>,
    /// Messages the user unfolded.
    expanded: Rc<HashSet<MessageId>>,
    names: Rc<HashMap<DaemonAgentId, SharedString>>,
    now: u64,
    scroll_key: Option<(AgentId, Option<MessageId>)>,
}

impl MessageThread {
    pub(crate) fn new() -> Self {
        Self {
            list: ListState::new(0, ListAlignment::Top, px(200.)),
            agent: None,
            revision: 0,
            messages: Rc::from([]),
            parsed: Rc::from([]),
            parse_cache: HashMap::new(),
            expanded: Rc::default(),
            names: Rc::default(),
            now: 0,
            scroll_key: None,
        }
    }

    /// True when the thread already holds this agent's messages at this revision.
    fn shows(&self, agent: AgentId, revision: u64) -> bool {
        self.agent == Some(agent) && self.revision == revision
    }

    /// Takes the new messages, if any, and what the rows need to read. The list follows the
    /// newest message when the agent changed, or when it was already at the bottom, so
    /// reading older ones is safe.
    fn show(
        &mut self,
        agent: AgentId,
        revision: u64,
        messages: Option<Rc<[Message]>>,
        names: Rc<HashMap<DaemonAgentId, SharedString>>,
        now: u64,
        cx: &mut Context<Self>,
    ) {
        let mut changed = false;

        if let Some(messages) = messages {
            self.fit_list(agent, &messages);
            self.parse(&messages);
            self.messages = messages;
            changed = true;
        }
        if self.agent != Some(agent) || self.revision != revision {
            self.agent = Some(agent);
            self.revision = revision;
            changed = true;
        }
        if *self.names != *names {
            self.names = names;
            changed = true;
        }
        if self.now / MINUTE != now / MINUTE {
            changed = true;
        }
        self.now = now;

        if changed {
            cx.notify();
        }
    }

    fn parse(&mut self, messages: &[Message]) {
        if self.parse_cache.len() > PARSED_LIMIT {
            self.parse_cache.clear();
        }

        let parsed: Vec<Rc<Markdown>> = messages
            .iter()
            .map(|message| {
                let markdown = self
                    .parse_cache
                    .entry(message.id)
                    .or_insert_with(|| Rc::new(markdown::parse(&message.text)));

                Rc::clone(markdown)
            })
            .collect();

        self.parsed = parsed.into();
    }

    /// Folds or unfolds a message. The list measures that one row again.
    fn toggle(&mut self, message: MessageId, cx: &mut Context<Self>) {
        let expanded = Rc::make_mut(&mut self.expanded);
        if !expanded.remove(&message) {
            expanded.insert(message);
        }

        if let Some(index) = self.messages.iter().position(|entry| entry.id == message) {
            self.list.splice(index..index + 1, 1);
        }

        cx.notify();
    }

    fn fit_list(&mut self, agent: AgentId, thread: &[Message]) {
        let key = scroll_key(agent, thread);
        let previous = self.scroll_key;
        let known = self.list.item_count();

        if previous == Some(key) && known == thread.len() {
            return;
        }

        let same_agent = previous.is_some_and(|(previous_agent, _)| previous_agent == agent);
        let following = !same_agent || self.list.is_following_tail();
        let appended = same_agent
            && thread.len() > known
            && (known == 0
                || previous.is_some_and(|(_, newest)| {
                    thread.get(known - 1).map(|message| message.id) == newest
                }));

        if appended {
            self.list.splice(known..known, thread.len() - known);
        } else {
            self.list.reset(thread.len());
        }

        if following {
            self.list.set_follow_mode(FollowMode::Tail);
        }

        self.scroll_key = Some(key);
    }
}

impl Render for MessageThread {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors().clone();
        let thread = cx.entity();
        let (messages, parsed) = (Rc::clone(&self.messages), Rc::clone(&self.parsed));
        let (expanded, names) = (Rc::clone(&self.expanded), Rc::clone(&self.names));
        let now = self.now;

        list(self.list.clone(), move |index, _, _| {
            let (Some(message), Some(markdown)) = (messages.get(index), parsed.get(index)) else {
                return div().into_any_element();
            };
            let unfolded = expanded.contains(&message.id);
            let thread = thread.clone();
            let id = message.id;

            let row = MessageRow {
                index,
                message,
                markdown,
                folded: markdown.tall && !unfolded,
                names: &names,
                now,
                on_toggle: Rc::new(move |cx: &mut App| {
                    thread.update(cx, |thread, cx| thread.toggle(id, cx));
                }),
            };

            message_row(&row, &colors)
        })
        .size_full()
    }
}

/// Paints the dots behind the graph. It is cached, so only a resize paints them again.
pub(crate) struct DotGrid;

impl Render for DotGrid {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let color = cx.theme().colors().dot;

        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| paint_dot_grid(bounds, color, window),
        )
        .size_full()
    }
}

fn full_size() -> StyleRefinement {
    StyleRefinement::default().size_full()
}

/// Everything the view shows, read once per frame.
struct SwarmData<'a> {
    agents: Vec<&'a Agent>,
    messages: Vec<&'a Message>,
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
        in_flight,
        now: workspace.now_ms(),
        selected: workspace.selected_agent(),
    };

    h_flex()
        .id("swarm-view")
        .size_full()
        .items_stretch()
        .on_mouse_move(cx.listener(|workspace, event: &MouseMoveEvent, _, cx| {
            workspace.drag_inspector(event, cx);
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|workspace, _, _, cx| workspace.end_inspector_drag(cx)),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|workspace, _, _, cx| workspace.end_inspector_drag(cx)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(graph(workspace, &data, cx)),
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
        .child(workspace.dot_grid.clone().cached(full_size().absolute()))
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

/// Paints every dot as two triangles of one path. Thousands of quads would each cost a
/// scene insert every frame, even when the cached view replays them.
fn paint_dot_grid(bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let step = px(DOT_SPACING);
    let dot = px(DOT_SIZE);
    let solid = point(0., 1.);

    let mut path = gpui::Path::new(bounds.origin + point(step / 2., step / 2.));
    let mut top = bounds.origin.y + step / 2.;

    while top < bounds.bottom() {
        let mut left = bounds.origin.x + step / 2.;

        while left < bounds.right() {
            let (right, bottom) = (left + dot, top + dot);
            let corners = [
                point(left, top),
                point(right, top),
                point(right, bottom),
                point(left, bottom),
            ];

            path.push_triangle((corners[0], corners[1], corners[2]), (solid, solid, solid));
            path.push_triangle((corners[0], corners[2], corners[3]), (solid, solid, solid));
            left += step;
        }

        top += step;
    }

    window.paint_path(path, color);
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

    match workspace
        .data
        .agent_changes
        .get(&agent.id)
        .map(|changes| changes.stat)
    {
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

fn inspector(workspace: &Workspace, data: &SwarmData, cx: &mut Context<Workspace>) -> AnyElement {
    let colors = cx.theme().colors().clone();
    let this = cx.entity();
    let (start, nudge) = (this.clone(), this);

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

    let handle = ResizeHandle::new(
        "inspector-resize",
        "Resize inspector",
        &workspace.inspector_handle,
    )
    .value(
        workspace.inspector_width,
        INSPECTOR_MIN_WIDTH,
        INSPECTOR_MAX_WIDTH,
    )
    .dragging(workspace.inspector_drag.is_some())
    .on_drag_start(move |event, _, cx| {
        start.update(cx, |workspace, cx| {
            workspace.start_inspector_drag(event.position.x, cx);
        });
    })
    .on_nudge(move |direction, _, cx| {
        nudge.update(cx, |workspace, cx| workspace.nudge_inspector(direction, cx));
    });

    v_flex()
        .id("inspector")
        .debug_selector(|| "inspector".to_owned())
        .role(Role::Complementary)
        .aria_label("Selected agent")
        .relative()
        .w(px(workspace.inspector_width))
        .flex_none()
        .h_full()
        .min_h_0()
        .border_l_1()
        .border_color(colors.line)
        .child(body)
        .child(handle)
        .into_any_element()
}

fn inspector_body(
    workspace: &Workspace,
    agent: &Agent,
    data: &SwarmData,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let colors = cx.theme().colors().clone();
    let id = agent.id;
    let tab = workspace.inspector_tab;
    let this = cx.entity();

    let title = h_flex()
        .gap(px(10.))
        .child(Icon::new(kind_icon(agent.kind)).size(IconSize::Custom(px(20.))))
        .child(
            div().min_w_0().child(
                Label::new(agent.name.clone())
                    .size(LabelSize::Large)
                    .weight(FontWeight::SEMIBOLD)
                    .truncate(),
            ),
        )
        .child(StatusPill::new(agent.status))
        .child(div().flex_1())
        .child(
            Button::new("inspector-diff", "View diff").on_click(cx.listener(
                move |workspace, _: &ClickEvent, _, cx| {
                    workspace.view_agent_diff(id, cx);
                },
            )),
        )
        .child(
            Button::new("inspector-focus", "Focus")
                .variant(ButtonVariant::Primary)
                .on_click(cx.listener(move |workspace, _: &ClickEvent, _, cx| {
                    workspace.focus_swarm_agent(id, cx);
                })),
        );

    let mut facts = vec![format!("{} \u{b7} {}", agent.kind.label(), agent.model)];
    facts.extend(running_time(
        agent.running_since(),
        agent.ended_at,
        data.now,
    ));
    facts.push(
        if workspace.in_task_worktree(agent) {
            "Task worktree"
        } else {
            "Own worktree"
        }
        .to_owned(),
    );

    let details = v_flex()
        .gap(px(4.))
        .mt(px(-4.))
        .child(
            h_flex()
                .flex_wrap()
                .gap_x(px(12.))
                .gap_y(px(4.))
                .text_size(px(12.))
                .text_color(colors.muted)
                .children(facts.into_iter().map(|fact| div().child(fact))),
        )
        .child(
            mono(workspace.branch_label(agent), 11., colors.faint)
                .whitespace_nowrap()
                .truncate(),
        );

    let mut tabs = TabStrip::new("inspector-tabs", "Inspector")
        .tab_with_count("Messages", workspace.thread_len(id).to_string());
    tabs = match workspace.data.agent_changes.get(&id) {
        Some(changes) => tabs.tab_with_count("Changes", changes.stat.files.to_string()),
        None => tabs.tab("Changes"),
    };
    let tabs = tabs
        .tab("Terminal")
        .selected(tab.index())
        .debug_selector(|| "inspector-tab".to_owned())
        .on_select(move |index, _, cx| {
            this.update(cx, |workspace, cx| {
                workspace.select_inspector_tab(InspectorTab::from_index(index), cx);
            });
        });

    let content = match tab {
        InspectorTab::Messages => div()
            .id("message-thread")
            .role(Role::List)
            .aria_label("Messages")
            .flex_1()
            .min_h_0()
            .px(px(16.))
            .pt(px(2.))
            .child(workspace.thread.clone().cached(full_size()))
            .into_any_element(),
        InspectorTab::Changes => changes_tab(workspace, agent, cx),
        InspectorTab::Terminal => terminal_tab(workspace, agent, cx),
    };

    v_flex()
        .size_full()
        .min_h_0()
        .child(
            v_flex()
                .flex_none()
                .gap(px(12.))
                .px(px(16.))
                .pt(px(14.))
                .child(title)
                .child(details)
                .child(tabs),
        )
        .child(content)
        .into_any_element()
}

fn signed(additions: u32, deletions: u32, colors: &ThemeColors) -> gpui::Div {
    h_flex()
        .flex_none()
        .gap(px(4.))
        .child(mono(format!("+{additions}"), 11., colors.add))
        .child(mono(format!("\u{2212}{deletions}"), 11., colors.del))
}

fn changes_tab(workspace: &Workspace, agent: &Agent, cx: &mut Context<Workspace>) -> AnyElement {
    let colors = cx.theme().colors().clone();
    let id = agent.id;

    let Some(changes) = workspace
        .data
        .agent_changes
        .get(&id)
        .filter(|changes| changes.stat.files > 0)
    else {
        return div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .child(Label::new("No changes").color(Color::Faint))
            .into_any_element();
    };
    let stat = changes.stat;

    let files = uniform_list(
        "changed-files",
        changes.files.len(),
        cx.processor(move |workspace, range: Range<usize>, _, cx| {
            let colors = cx.theme().colors().clone();
            let Some(changes) = workspace.data.agent_changes.get(&id) else {
                return Vec::new();
            };
            let listed = Rc::clone(&changes.files);

            range
                .filter_map(|index| listed.get(index).map(|file| (index, file)))
                .map(|(index, file)| file_row(index, file, id, &colors, cx))
                .collect()
        }),
    )
    .size_full();

    v_flex()
        .flex_1()
        .min_h_0()
        .pt(px(12.))
        .gap(px(10.))
        .child(
            h_flex()
                .flex_none()
                .gap(px(8.))
                .px(px(16.))
                .child(signed(stat.additions, stat.deletions, &colors))
                .child(
                    Label::new(plural(stat.files, "file", "files"))
                        .size(LabelSize::Small)
                        .color(Color::Faint),
                ),
        )
        .child(
            div()
                .id("changed-files-list")
                .role(Role::List)
                .aria_label("Changed files")
                .flex_1()
                .min_h_0()
                .px(px(8.))
                .child(files),
        )
        .into_any_element()
}

fn file_label(path: &Path, additions: u32, deletions: u32) -> String {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let mut parts = vec![name];

    if additions > 0 {
        parts.push(plural(additions, "addition", "additions"));
    }
    if deletions > 0 {
        parts.push(plural(deletions, "deletion", "deletions"));
    }
    parts.push("open in Review".to_owned());

    parts.join(", ")
}

fn file_row(
    index: usize,
    file: &FileStat,
    agent: AgentId,
    colors: &ThemeColors,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let name = file.path.file_name().map_or_else(
        || file.path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let folder = file
        .path
        .parent()
        .map(|folder| folder.display().to_string())
        .unwrap_or_default();
    let (path, accent, active) = (file.path.clone(), colors.accent, colors.active);

    div()
        .h(px(36.))
        .child(
            h_flex()
                .id(("changed-file", index))
                .debug_selector(|| format!("changed-file-{index}"))
                .role(Role::Button)
                .aria_label(file_label(&file.path, file.additions, file.deletions))
                .focusable()
                .tab_stop(true)
                .cursor_pointer()
                .h(px(34.))
                .gap(px(10.))
                .px(px(8.))
                .rounded(px(7.))
                .hover(move |style| style.bg(active))
                .focus_visible(move |style| focus_ring(style, accent))
                .on_click(cx.listener(move |workspace, _: &ClickEvent, _, cx| {
                    workspace.view_file_diff(agent, path.clone(), cx);
                }))
                .child(
                    Icon::new(IconName::File)
                        .size(IconSize::Custom(px(14.)))
                        .color(Color::Faint),
                )
                .child(Label::new(name).weight(FontWeight::MEDIUM))
                .child(
                    div().flex_1().min_w_0().child(
                        mono(folder, 11., colors.faint)
                            .whitespace_nowrap()
                            .truncate(),
                    ),
                )
                .child(signed(file.additions, file.deletions, colors)),
        )
        .into_any_element()
}

fn terminal_tab(workspace: &Workspace, agent: &Agent, cx: &mut Context<Workspace>) -> AnyElement {
    let rows: Vec<AnyElement> = workspace
        .active_task()
        .processes
        .iter()
        .filter_map(|id| workspace.data.terminal(*id))
        .filter(|terminal| terminal.owner == Some(agent.id))
        .map(|terminal| process_row(workspace, terminal, cx))
        .collect();

    v_flex()
        .flex_1()
        .min_h_0()
        .px(px(16.))
        .pt(px(12.))
        .pb(px(16.))
        .gap(px(6.))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .child(workspace.preview.clone().cached(full_size())),
        )
        .when(!rows.is_empty(), |tab| {
            tab.child(
                div().flex_none().mt(px(8.)).child(
                    Label::new("Process terminals")
                        .size(LabelSize::Small)
                        .color(Color::Faint),
                ),
            )
            .child(
                v_flex()
                    .id("process-terminals")
                    .role(Role::List)
                    .aria_label("Process terminals")
                    .flex_none()
                    .gap(px(2.))
                    .children(rows),
            )
        })
        .into_any_element()
}

fn exit_label(exit: Option<&swarm_term::ExitStatus>) -> String {
    match exit {
        None => "Running".to_owned(),
        Some(swarm_term::ExitStatus::Code(code)) => format!("Exited {code}"),
        Some(swarm_term::ExitStatus::Signal(signal)) => format!("Exited {signal}"),
    }
}

fn process_row(
    workspace: &Workspace,
    terminal: &crate::model::Terminal,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let colors = cx.theme().colors().clone();
    let id = terminal.id;

    let live = workspace
        .live_terminals
        .get(&id)
        .map(|live| live.view.read(cx));
    let exit = live.map_or(terminal.exit.as_ref(), TerminalView::exit_status);
    let size = live
        .and_then(TerminalView::grid_size)
        .or(terminal.requested_size)
        .or(terminal.fixed_size);
    let state = exit_label(exit);
    let running = exit.is_none();

    let label = format!(
        "{}, {}, focus in Terminals",
        terminal.title,
        state.to_lowercase()
    );
    let (accent, active) = (colors.accent, colors.active);

    let dot = div().flex_none().size(px(6.)).rounded(px(6.));
    let dot = if running {
        dot.bg(colors.ok)
    } else {
        dot.border_1().border_color(colors.faint)
    };

    h_flex()
        .id(("process", id.0 as usize))
        .debug_selector(|| format!("process-{}", id.0))
        .role(Role::Button)
        .aria_label(label)
        .focusable()
        .tab_stop(true)
        .cursor_pointer()
        .h(px(32.))
        .gap(px(10.))
        .mx(px(-8.))
        .px(px(8.))
        .rounded(px(7.))
        .hover(move |style| style.bg(active))
        .focus_visible(move |style| focus_ring(style, accent))
        .on_click(cx.listener(move |workspace, _: &ClickEvent, _, cx| {
            workspace.focus_process(id, cx);
        }))
        .child(dot)
        .child(
            div().flex_1().min_w_0().child(
                mono(terminal.title.clone(), 11.5, colors.fg)
                    .whitespace_nowrap()
                    .truncate(),
            ),
        )
        .children(size.map(|(columns, rows)| {
            mono(format!("{columns}\u{d7}{rows}"), 11., colors.faint).flex_none()
        }))
        .child(Label::new(state).size(LabelSize::Small).color(Color::Muted))
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

/// What one row of the thread needs to draw itself.
struct MessageRow<'a> {
    index: usize,
    message: &'a Message,
    markdown: &'a Markdown,
    folded: bool,
    names: &'a HashMap<DaemonAgentId, SharedString>,
    now: u64,
    on_toggle: Rc<dyn Fn(&mut App)>,
}

fn capitalised(word: &str) -> String {
    let mut letters = word.chars();

    letters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(letters).collect()
    })
}

/// The text and fill of a message kind's tag.
fn kind_tag_colors(kind: MessageKind, colors: &ThemeColors) -> (Hsla, Hsla) {
    match kind {
        MessageKind::Brief | MessageKind::Task => (colors.accent, colors.accent_bg),
        MessageKind::Question => (colors.warn, colors.warn_bg),
        MessageKind::Report => (colors.ok, colors.ok_bg),
        MessageKind::TerminalExited | MessageKind::UserInput | MessageKind::WorkerStopped => {
            (colors.muted, colors.soft)
        }
    }
}

fn message_row(row: &MessageRow<'_>, colors: &ThemeColors) -> AnyElement {
    let message = row.message;
    let in_flight = message.delivered_at.is_none();
    let when = if in_flight {
        "In flight".to_owned()
    } else {
        relative_time(message.sent_at, row.now)
    };
    let (tag_color, tag_fill) = kind_tag_colors(message.kind, colors);

    let header = h_flex()
        .gap(px(6.))
        .text_size(px(12.))
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(agent_name(row.names, message.from)),
        )
        .child(div().text_color(colors.faint).child(format!(
            "\u{2192} {}",
            agent_name(row.names, Some(message.to))
        )))
        .child(
            div()
                .px(px(6.))
                .rounded(px(4.))
                .bg(tag_fill)
                .text_color(tag_color)
                .text_size(px(11.))
                .line_height(px(18.))
                .font_weight(FontWeight::MEDIUM)
                .child(capitalised(message.kind.label())),
        )
        .child(div().flex_1())
        .child(mono(
            when,
            11.,
            if in_flight { colors.ok } else { colors.faint },
        ));

    let body = div()
        .relative()
        .when(row.folded, |body| {
            body.max_h(px(FOLDED_HEIGHT)).overflow_hidden()
        })
        .child(markdown::render(row.markdown, colors))
        .when(row.folded, |body| {
            body.child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(56.))
                    .bg(linear_gradient(
                        180.,
                        linear_color_stop(colors.panel.opacity(0.), 0.),
                        linear_color_stop(colors.panel, 1.),
                    )),
            )
        });

    let toggle = row.markdown.tall.then(|| {
        let label = if row.folded { "Show more" } else { "Show less" };
        let on_toggle = Rc::clone(&row.on_toggle);
        let accent = colors.accent;

        h_flex().child(
            div()
                .id(("message-fold", row.index))
                .debug_selector(|| format!("message-fold-{}", row.index))
                .role(Role::Button)
                .aria_label(label)
                .focusable()
                .tab_stop(true)
                .cursor_pointer()
                .rounded(px(4.))
                .focus_visible(move |style| focus_ring(style, accent))
                .on_click(move |_, _, cx| on_toggle(cx))
                .child(
                    Label::new(label)
                        .size(LabelSize::Small)
                        .weight(FontWeight::MEDIUM)
                        .color(Color::Accent),
                ),
        )
    });

    v_flex()
        .id(("message", row.index))
        .role(Role::ListItem)
        .py(px(12.))
        .gap(px(8.))
        .border_b_1()
        .border_color(colors.line)
        .child(header)
        .child(body)
        .children(toggle)
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
    use std::cell::Cell;

    fn setup(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
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
                Some(MessageKind::Task),
                Some(MessageKind::Report),
                None
            ]
        );
    }

    #[gpui::test]
    fn messages_in_flight_become_dots_by_sender(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let (term_render, pty_io, none) = workspace.read_with(cx, |workspace, _| {
            let messages: Vec<&Message> = workspace
                .data
                .task_messages(workspace.active_task)
                .collect();
            let orchestrator = id_of(workspace, "orchestrator");
            let dots = |name| flight_dots(&messages, orchestrator, id_of(workspace, name));

            (dots("term-render"), dots("pty-io"), dots("ui-tokens"))
        });

        assert_eq!(term_render, [(0.35, MessageKind::Task)]);
        assert_eq!(pty_io, [(0.35, MessageKind::Task)]);
        assert!(none.is_empty());
    }

    #[test]
    fn a_message_from_the_worker_starts_on_its_side_of_the_curve() {
        let (orchestrator, worker) = (DaemonAgentId(1), DaemonAgentId(2));
        let question = Message {
            id: MessageId(1),
            from: Some(worker),
            to: orchestrator,
            kind: MessageKind::Question,
            text: SharedString::default(),
            sent_at: 1,
            delivered_at: None,
        };

        let dots = flight_dots(&[&question], orchestrator, worker);

        assert_eq!(dots, [(0.65, MessageKind::Question)]);
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
                    ids: vec![MessageId(7), MessageId(8)],
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
        workspace: &Entity<Workspace>,
        range: Range<u64>,
        cx: &mut VisualTestContext,
    ) {
        workspace.update(cx, |workspace, cx| {
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

            cx.notify();
        });
    }

    fn following(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
        workspace.read_with(cx, |workspace, cx| {
            workspace.thread.read(cx).list.is_following_tail()
        })
    }

    fn scroll_position(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> gpui::ListOffset {
        workspace.read_with(cx, |workspace, cx| {
            workspace.thread.read(cx).list.logical_scroll_top()
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

        workspace.read_with(cx, |workspace, cx| {
            workspace.thread.read(cx).list.scroll_by(px(-600.));
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
    fn scrolling_the_thread_notifies_the_thread_and_not_the_workspace(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        add_thread_messages(&workspace, 100..140, cx);
        redraw(cx);
        redraw(cx);

        let thread = workspace.read_with(cx, |workspace, _| workspace.thread.clone());
        let counts = Rc::new(Cell::new((0_u32, 0_u32)));
        let seen = Rc::clone(&counts);
        let _workspace_watch = cx.update(|_, cx| {
            cx.observe(&workspace, move |_, _| {
                seen.set((seen.get().0 + 1, seen.get().1));
            })
        });
        let seen = Rc::clone(&counts);
        let _thread_watch = cx.update(|_, cx| {
            cx.observe(&thread, move |_, _| {
                seen.set((seen.get().0, seen.get().1 + 1));
            })
        });

        let list = thread.read_with(cx, |thread, _| thread.list.clone());
        let bounds = list.viewport_bounds();
        assert!(bounds.size.height > px(0.));
        cx.simulate_event(gpui::ScrollEvent {
            position: bounds.center(),
            delta: gpui::ScrollDelta::Pixels(point(px(0.), px(120.))),
            ..Default::default()
        });
        cx.run_until_parked();

        let (workspace_notifies, thread_notifies) = counts.get();
        assert_eq!(workspace_notifies, 0);
        assert!(thread_notifies > 0);
        assert!(!list.is_following_tail());
    }

    #[gpui::test]
    fn the_thread_follows_the_selected_agent(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let worker = workspace.read_with(cx, |workspace, _| workspace.active_task().workers[0]);

        workspace.update(cx, |workspace, cx| workspace.select_swarm_agent(worker, cx));

        let (agent, count, expected) = workspace.read_with(cx, |workspace, cx| {
            let thread = workspace.thread.read(cx);

            (
                thread.agent,
                thread.messages.len(),
                workspace.thread_of(worker).len(),
            )
        });
        assert_eq!(agent, Some(worker));
        assert_eq!(count, expected);
        assert!(count > 0);
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

    fn worker_named(
        workspace: &Entity<Workspace>,
        name: &str,
        cx: &mut VisualTestContext,
    ) -> AgentId {
        workspace.read_with(cx, |workspace, _| {
            agents(workspace)
                .into_iter()
                .find(|(_, agent, _)| agent == name)
                .map_or(AgentId(u32::MAX), |(id, _, _)| id)
        })
    }

    fn show_agent(
        workspace: &Entity<Workspace>,
        name: &str,
        cx: &mut VisualTestContext,
    ) -> AgentId {
        let agent = worker_named(workspace, name, cx);

        workspace.update(cx, |workspace, cx| workspace.select_swarm_agent(agent, cx));
        redraw(cx);

        agent
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx.debug_bounds(selector);
        assert!(bounds.is_some(), "{selector} is not on screen");

        cx.simulate_click(
            bounds.map_or_else(Default::default, |bounds| bounds.center()),
            gpui::Modifiers::none(),
        );
        redraw(cx);
    }

    fn tab(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> InspectorTab {
        workspace.read_with(cx, |workspace, _| workspace.inspector_tab)
    }

    #[gpui::test]
    fn the_tabs_switch_and_keep_their_choice_for_every_agent(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        assert_eq!(tab(&workspace, cx), InspectorTab::Messages);

        click("inspector-tab-1", cx);
        assert_eq!(tab(&workspace, cx), InspectorTab::Changes);

        show_agent(&workspace, "pty-io", cx);
        assert_eq!(tab(&workspace, cx), InspectorTab::Changes);

        click("inspector-tab-2", cx);
        show_agent(&workspace, "bench", cx);
        assert_eq!(tab(&workspace, cx), InspectorTab::Terminal);

        click("inspector-tab-0", cx);
        assert_eq!(tab(&workspace, cx), InspectorTab::Messages);
    }

    #[gpui::test]
    fn the_arrow_keys_move_between_the_tabs_of_the_strip(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        click("inspector-tab-0", cx);

        cx.simulate_keystrokes("right");
        assert_eq!(tab(&workspace, cx), InspectorTab::Changes);

        cx.simulate_keystrokes("right");
        assert_eq!(tab(&workspace, cx), InspectorTab::Terminal);

        cx.simulate_keystrokes("left");
        assert_eq!(tab(&workspace, cx), InspectorTab::Changes);

        redraw(cx);
        cx.simulate_keystrokes("left left");
        assert_eq!(tab(&workspace, cx), InspectorTab::Terminal);
    }

    #[gpui::test]
    fn tab_reaches_the_strip_and_the_arrows_work_from_there(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let orchestrator = worker_named(&workspace, "orchestrator", cx);

        let mut reached = None;
        for presses in 1..=12 {
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.select_inspector_tab(InspectorTab::Messages, cx);
                let handle = workspace.swarm_handle(orchestrator, cx);
                window.focus(&handle, cx);
            });
            redraw(cx);

            for _ in 0..presses {
                cx.simulate_keystrokes("tab");
            }
            cx.simulate_keystrokes("right");

            if tab(&workspace, cx) == InspectorTab::Changes {
                reached = Some(presses);
                break;
            }
        }

        assert!(reached.is_some(), "Tab never reached the tab strip");
    }

    #[gpui::test]
    fn the_thread_parses_each_message_once_into_blocks(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        show_agent(&workspace, "pty-io", cx);

        let brief = workspace.read_with(cx, |workspace, cx| {
            workspace.thread.read(cx).parsed.first().cloned()
        });
        let brief = brief.expect("the brief");
        let kinds: Vec<&str> = brief
            .blocks
            .iter()
            .map(|block| match block {
                markdown::Block::Paragraph(_) => "paragraph",
                markdown::Block::Heading(_) => "heading",
                markdown::Block::Item { .. } => "item",
                markdown::Block::Code(_) => "code",
            })
            .collect();
        assert_eq!(
            kinds,
            ["paragraph", "heading", "item", "item", "item", "item"]
        );
        assert!(brief.tall);

        add_thread_messages(&workspace, 100..101, cx);
        workspace.update(cx, |workspace, cx| {
            let bench = workspace.active_task().workers[0];
            workspace.select_swarm_agent(bench, cx);
        });
        show_agent(&workspace, "pty-io", cx);

        let again = workspace.read_with(cx, |workspace, cx| {
            workspace.thread.read(cx).parsed.first().cloned()
        });
        assert!(again.is_some_and(|again| Rc::ptr_eq(&again, &brief)));
    }

    fn brief_height(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Pixels {
        workspace.read_with(cx, |workspace, cx| {
            workspace
                .thread
                .read(cx)
                .list
                .bounds_for_item(0)
                .map_or(px(0.), |bounds| bounds.size.height)
        })
    }

    #[gpui::test]
    fn a_long_message_folds_and_show_more_and_less_change_its_height(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        show_agent(&workspace, "pty-io", cx);
        redraw(cx);

        let folded = brief_height(&workspace, cx);
        assert!(folded > px(0.));
        assert!(folded < px(FOLDED_HEIGHT + 100.));

        click("message-fold-0", cx);
        redraw(cx);
        let expanded = brief_height(&workspace, cx);
        assert!(
            expanded > folded,
            "{expanded:?} is not taller than {folded:?}"
        );

        click("message-fold-0", cx);
        redraw(cx);
        assert_eq!(brief_height(&workspace, cx), folded);
    }

    #[gpui::test]
    fn a_short_message_has_no_fold_button(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        show_agent(&workspace, "pty-io", cx);

        assert!(cx.debug_bounds("message-fold-0").is_some());
        assert!(cx.debug_bounds("message-fold-1").is_none());
        assert!(cx.debug_bounds("message-fold-2").is_none());
        drop(workspace);
    }

    #[gpui::test]
    fn a_changed_file_opens_review_at_that_file(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let pty_io = show_agent(&workspace, "pty-io", cx);
        click("inspector-tab-1", cx);

        click("changed-file-0", cx);

        let (view, agent, file) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.view,
                workspace.review_agent,
                workspace.review_file.clone(),
            )
        });
        assert_eq!(view, View::Review);
        assert_eq!(agent, Some(pty_io));
        assert_eq!(file, Some(PathBuf::from("crates/swarm-term/src/pty.rs")));
    }

    #[gpui::test]
    fn view_diff_clears_the_file_a_row_set(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let pty_io = show_agent(&workspace, "pty-io", cx);
        workspace.update(cx, |workspace, cx| {
            workspace.view_file_diff(pty_io, PathBuf::from("a.rs"), cx);
            workspace.set_view(View::Swarm, cx);
        });

        let file = workspace.read_with(cx, |workspace, _| workspace.review_file.clone());

        assert_eq!(file, None);
    }

    #[test]
    fn a_file_row_names_its_counts_and_where_it_goes() {
        let label = file_label(Path::new("crates/swarm-term/src/pty.rs"), 39, 8);
        let added_only = file_label(Path::new("Cargo.toml"), 1, 0);

        assert_eq!(label, "pty.rs, 39 additions, 8 deletions, open in Review");
        assert_eq!(added_only, "Cargo.toml, 1 addition, open in Review");
    }

    #[gpui::test]
    fn an_agent_without_changes_shows_none(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        show_agent(&workspace, "bench", cx);
        click("inspector-tab-2", cx);
        click("inspector-tab-1", cx);

        assert!(cx.debug_bounds("changed-file-0").is_none());
        let known = workspace.read_with(cx, |workspace, _| {
            let bench = workspace.active_task().workers[3];
            workspace.data.agent_changes.contains_key(&bench)
        });
        assert!(!known);
    }

    #[gpui::test]
    fn a_process_row_focuses_its_tile_in_the_terminals_view(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let pty_io = show_agent(&workspace, "pty-io", cx);
        click("inspector-tab-2", cx);

        let processes: Vec<TerminalId> = workspace.read_with(cx, |workspace, _| {
            workspace
                .active_task()
                .processes
                .iter()
                .copied()
                .filter(|id| {
                    workspace
                        .data
                        .terminal(*id)
                        .is_some_and(|terminal| terminal.owner == Some(pty_io))
                })
                .collect()
        });
        assert_eq!(processes.len(), 2);

        workspace.update(cx, |workspace, cx| workspace.minimize(processes[1], cx));
        click("process-8", cx);

        let (view, focused, minimized) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.view,
                workspace.focused_terminal,
                workspace.minimized.contains(&processes[1]),
            )
        });
        assert_eq!(processes[1], TerminalId(8));
        assert_eq!(view, View::Terminals);
        assert_eq!(focused, Some(processes[1]));
        assert!(!minimized);
    }

    #[test]
    fn a_process_that_ended_says_how() {
        assert_eq!(exit_label(None), "Running");
        assert_eq!(
            exit_label(Some(&swarm_term::ExitStatus::Code(0))),
            "Exited 0"
        );
        assert_eq!(
            exit_label(Some(&swarm_term::ExitStatus::Code(2))),
            "Exited 2"
        );
    }

    #[gpui::test]
    fn the_orchestrator_is_in_the_task_worktree_and_a_worker_in_its_own(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let (orchestrator, worker) = workspace.read_with(cx, |workspace, _| {
            let task = workspace.active_task();
            let agent = |id: Option<AgentId>| id.and_then(|id| workspace.data.agent(id));

            (
                agent(task.orchestrator).map(|agent| workspace.in_task_worktree(agent)),
                agent(task.workers.first().copied()).map(|agent| workspace.in_task_worktree(agent)),
            )
        });

        assert_eq!(orchestrator, Some(true));
        assert_eq!(worker, Some(false));
    }

    fn width(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> f32 {
        workspace.read_with(cx, |workspace, _| workspace.inspector_width)
    }

    #[gpui::test]
    fn the_inspector_width_stays_between_the_limits(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        assert!((width(&workspace, cx) - INSPECTOR_WIDTH).abs() < 1e-6);

        workspace.update(cx, |workspace, cx| workspace.set_inspector_width(100., cx));
        assert!((width(&workspace, cx) - 320.).abs() < 1e-6);

        workspace.update(cx, |workspace, cx| workspace.set_inspector_width(900., cx));
        assert!((width(&workspace, cx) - 640.).abs() < 1e-6);
    }

    #[gpui::test]
    fn the_arrow_keys_resize_the_inspector_by_twenty_pixels(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update_in(cx, |workspace, window, cx| {
            let handle = workspace.inspector_handle.clone();
            window.focus(&handle, cx);
        });
        redraw(cx);

        cx.simulate_keystrokes("left");
        assert!((width(&workspace, cx) - 420.).abs() < 1e-6);

        cx.simulate_keystrokes("right right");
        assert!((width(&workspace, cx) - 380.).abs() < 1e-6);

        workspace.update(cx, |workspace, cx| workspace.set_inspector_width(630., cx));
        cx.simulate_keystrokes("left left");
        assert!((width(&workspace, cx) - 640.).abs() < 1e-6);
    }

    #[gpui::test]
    fn dragging_the_left_border_resizes_the_inspector_within_the_limits(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let bounds = cx.debug_bounds("inspector").expect("the inspector");
        let border = point(bounds.origin.x, bounds.center().y);
        let none = gpui::Modifiers::none();

        cx.simulate_mouse_down(border, MouseButton::Left, none);
        cx.simulate_mouse_move(border - point(px(60.), px(0.)), MouseButton::Left, none);
        assert!((width(&workspace, cx) - 460.).abs() < 1.0);
        assert!(workspace.read_with(cx, |workspace, _| workspace.inspector_drag.is_some()));

        cx.simulate_mouse_move(border - point(px(900.), px(0.)), MouseButton::Left, none);
        assert!((width(&workspace, cx) - 640.).abs() < 1e-6);

        cx.simulate_mouse_move(border + point(px(300.), px(0.)), MouseButton::Left, none);
        assert!((width(&workspace, cx) - 320.).abs() < 1e-6);

        cx.simulate_mouse_up(border, MouseButton::Left, none);
        assert!(workspace.read_with(cx, |workspace, _| workspace.inspector_drag.is_none()));
    }

    #[gpui::test]
    fn the_inspector_width_does_not_change_the_terminal_area(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| workspace.set_view(View::Terminals, cx));
        redraw(cx);
        redraw(cx);
        let before = workspace.read_with(cx, |workspace, _| workspace.terminal_area);

        workspace.update(cx, |workspace, cx| {
            workspace.set_view(View::Swarm, cx);
            workspace.set_inspector_width(600., cx);
            workspace.set_view(View::Terminals, cx);
        });
        redraw(cx);
        redraw(cx);

        let after = workspace.read_with(cx, |workspace, _| workspace.terminal_area);
        assert!(before.is_some());
        assert_eq!(before, after);
    }
}
