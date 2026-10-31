//! Fake data for the app shell. Plain data, no gpui entities.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{Hsla, SharedString, rgb};
use swarm_protocol::{
    AgentId as DaemonAgentId, AgentInfo, AgentLifecycle, AgentRole,
    AgentStatus as DaemonAgentStatus, DiffStat, FileStat, MessageId, MessageInfo, MessageKind,
    ProjectInfo, ReviewSource, TaskDiff, TaskInfo, TerminalId as DaemonTerminalId, TerminalInfo,
};
use swarm_term::{BlockedKind, ExitStatus};
use swarm_ui::AgentStatus;
use swarm_ui::prelude::Color;

/// The clock of fake data: snapshots and tests show times relative to it.
pub(crate) const SAMPLE_NOW: u64 = 1_800_000_000_000;

pub(crate) const START_TASK: &str = "Start";
pub(crate) const PLAN_TASK: &str = "Plan";

/// The key for a fake task: `<project name>/<task name>`, for example
/// `swarm/terminal-renderer`. Real tasks use `TaskId::terminal_key`.
pub(crate) fn task_key(project: &str, task: &str) -> String {
    format!("{project}/{task}")
}

/// The key for the Start tab of a real project. Unique per project, unlike [`task_key`].
pub(crate) fn start_key(project: swarm_protocol::ProjectId) -> String {
    format!("project:{}/start", project.0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ProjectId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TaskId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct AgentId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TerminalId(pub u32);

#[derive(Clone, Debug)]
pub(crate) struct Project {
    pub id: ProjectId,
    pub name: SharedString,
    pub letter: char,
    pub tile_color: Hsla,
    pub base_branch: SharedString,
    /// Set for a project the daemon knows.
    pub daemon: Option<swarm_protocol::ProjectId>,
    /// The repository root of a project the daemon knows.
    pub root: Option<PathBuf>,
    /// Why the project's config could not be read.
    pub config_error: Option<SharedString>,
}

/// The newest messages kept for each task.
pub(crate) const MESSAGES_PER_TASK: usize = 500;

/// How many delivery times are kept for messages that have not been listed yet.
const EARLY_DELIVERIES: usize = 1_000;

/// A message as the app keeps it: the text is cheap to clone for rendering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Message {
    pub id: MessageId,
    pub from: Option<DaemonAgentId>,
    pub to: DaemonAgentId,
    pub kind: MessageKind,
    pub text: SharedString,
    pub sent_at: u64,
    /// `None` while the message is in flight.
    pub delivered_at: Option<u64>,
}

impl From<&MessageInfo> for Message {
    fn from(info: &MessageInfo) -> Self {
        Self {
            id: info.id,
            from: info.from,
            to: info.to,
            kind: info.kind,
            text: SharedString::from(info.text.clone()),
            sent_at: info.sent_at,
            delivered_at: info.delivered_at,
        }
    }
}

/// What links a task to the daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DaemonTask {
    pub id: swarm_protocol::TaskId,
    /// The Start orchestrator that made the task, if one did.
    pub created_by: Option<DaemonAgentId>,
    /// The task's orchestrator confirmed its brief.
    pub briefed: bool,
    pub setup_terminal: Option<DaemonTerminalId>,
    pub setup_exit: Option<ExitStatus>,
    pub worktree: PathBuf,
    /// The branch the task started from.
    pub base: SharedString,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TaskStatus {
    Working,
    NeedsYou,
    Idle,
}

#[derive(Clone, Debug)]
pub(crate) struct Task {
    pub id: TaskId,
    pub project: ProjectId,
    pub name: SharedString,
    pub branch: SharedString,
    pub status: TaskStatus,
    /// A real task has one once the daemon reports its orchestrator.
    pub orchestrator: Option<AgentId>,
    pub workers: Vec<AgentId>,
    pub processes: Vec<TerminalId>,
    pub user_terminals: Vec<TerminalId>,
    /// The fingerprint of the Merged diff the user approved.
    pub approved: Option<u64>,
    /// A Start or Plan task: its orchestrator splits a request into tasks. It has no worktree.
    pub is_start: bool,
    /// Set for a task the daemon knows.
    pub daemon: Option<DaemonTask>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentKind {
    ClaudeCode,
    Codex,
}

impl AgentKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
        }
    }
}

impl From<swarm_protocol::AgentKind> for AgentKind {
    fn from(kind: swarm_protocol::AgentKind) -> Self {
        match kind {
            swarm_protocol::AgentKind::ClaudeCode => Self::ClaudeCode,
            swarm_protocol::AgentKind::Codex => Self::Codex,
        }
    }
}

impl From<AgentKind> for swarm_protocol::AgentKind {
    fn from(kind: AgentKind) -> Self {
        match kind {
            AgentKind::ClaudeCode => Self::ClaudeCode,
            AgentKind::Codex => Self::Codex,
        }
    }
}

/// What links an agent to the daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DaemonAgent {
    pub id: DaemonAgentId,
    pub project: swarm_protocol::ProjectId,
    /// `None` for a Start orchestrator.
    pub task: Option<swarm_protocol::TaskId>,
    /// The agent's terminal while it runs.
    pub terminal: Option<DaemonTerminalId>,
    pub status: DaemonAgentStatus,
    pub message: Option<SharedString>,
    pub lifecycle: AgentLifecycle,
    /// Of the daemon's last report. An older report is dropped.
    pub revision: u64,
    pub worktree: PathBuf,
    pub branch: SharedString,
}

/// The sidebar and tile visuals for a daemon status.
pub(crate) fn ui_status(status: DaemonAgentStatus) -> AgentStatus {
    match status {
        DaemonAgentStatus::Unknown => AgentStatus::Unknown,
        DaemonAgentStatus::Idle => AgentStatus::Idle,
        DaemonAgentStatus::Working => AgentStatus::Working,
        DaemonAgentStatus::Blocked(_) => AgentStatus::Blocked,
        DaemonAgentStatus::Done => AgentStatus::Done,
        DaemonAgentStatus::Error => AgentStatus::Error,
    }
}

fn blocked_label(kind: BlockedKind) -> &'static str {
    match kind {
        BlockedKind::Permission => "permission",
        BlockedKind::Question => "question",
        BlockedKind::Auth => "login",
        BlockedKind::Other => "blocked",
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Agent {
    pub id: AgentId,
    pub task: TaskId,
    pub name: SharedString,
    pub kind: AgentKind,
    pub model: SharedString,
    pub status: AgentStatus,
    pub terminal: TerminalId,
    /// Milliseconds since the Unix epoch; 0 when unknown.
    pub created_at: u64,
    /// When its process last started, if it did.
    pub started_at: Option<u64>,
    /// When its process last ended, if it did.
    pub ended_at: Option<u64>,
    /// What a fake agent says it does, where a real one shows the program's status text.
    pub note: Option<SharedString>,
    /// Set for an agent the daemon runs.
    pub daemon: Option<DaemonAgent>,
}

impl Agent {
    /// When the running time starts: the last process start, else the creation.
    pub(crate) fn running_since(&self) -> u64 {
        self.started_at.unwrap_or(self.created_at)
    }

    /// The id messages use: the daemon's, or one made from the fake id.
    pub(crate) fn protocol_id(&self) -> DaemonAgentId {
        self.daemon
            .as_ref()
            .map_or(DaemonAgentId(u64::from(self.id.0)), |daemon| daemon.id)
    }

    /// What a real agent shows next to its name: kind, model, then why it is blocked
    /// or has stopped. A fake agent shows its model only.
    pub(crate) fn meta(&self) -> SharedString {
        let Some(daemon) = &self.daemon else {
            return self.model.clone();
        };

        let mut parts = vec![self.kind.label().to_owned(), self.model.to_string()];

        match daemon.lifecycle {
            AgentLifecycle::Stopped => parts.push("stopped".to_owned()),
            AgentLifecycle::Resumable => parts.push("resumable".to_owned()),
            AgentLifecycle::Starting | AgentLifecycle::Running => {}
        }
        if let DaemonAgentStatus::Blocked(kind) = daemon.status {
            parts.push(blocked_label(kind).to_owned());
        }
        if matches!(
            daemon.status,
            DaemonAgentStatus::Blocked(_) | DaemonAgentStatus::Error
        ) && let Some(message) = &daemon.message
        {
            parts.push(message.to_string());
        }

        SharedString::from(parts.join(" \u{b7} "))
    }

    /// True while the agent's process runs and it works or waits for the user.
    pub(crate) fn is_working(&self) -> bool {
        match &self.daemon {
            Some(daemon) => {
                daemon.lifecycle == AgentLifecycle::Running
                    && matches!(
                        daemon.status,
                        DaemonAgentStatus::Working | DaemonAgentStatus::Blocked(_)
                    )
            }
            None => matches!(self.status, AgentStatus::Working | AgentStatus::Blocked),
        }
    }

    /// The status word for screen readers, for example "needs you, permission".
    pub(crate) fn status_text(&self) -> String {
        let Some(daemon) = &self.daemon else {
            return self.status.label().to_lowercase();
        };

        match daemon.status {
            DaemonAgentStatus::Blocked(kind) => format!("needs you, {}", blocked_label(kind)),
            _ => self.status.label().to_lowercase(),
        }
    }

    /// The name a screen reader announces, for example
    /// "orchestrator, Claude Code, opus, needs you, permission".
    pub(crate) fn accessible_label(&self) -> String {
        let mut parts = vec![self.name.to_string(), self.kind.label().to_owned()];

        if self.daemon.is_some() {
            parts.push(self.model.to_string());
        }
        parts.push(self.status_text());

        if let Some(daemon) = &self.daemon {
            match daemon.lifecycle {
                AgentLifecycle::Stopped => parts.push("stopped".to_owned()),
                AgentLifecycle::Resumable => parts.push("resumable".to_owned()),
                AgentLifecycle::Starting | AgentLifecycle::Running => {}
            }
            parts.extend(daemon.message.iter().map(ToString::to_string));
        }

        parts.join(", ")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TerminalKind {
    Orchestrator,
    Worker,
    Process,
    User,
}

#[derive(Clone, Debug)]
pub(crate) struct FakeLine {
    pub text: SharedString,
    pub color: Color,
}

#[derive(Clone, Debug)]
pub(crate) struct Terminal {
    pub id: TerminalId,
    pub kind: TerminalKind,
    pub title: SharedString,
    pub meta: Option<SharedString>,
    pub owner: Option<AgentId>,
    /// The daemon agent that started a process terminal. `owner` follows once the app
    /// knows that agent.
    pub daemon_owner: Option<DaemonAgentId>,
    pub link: Option<SharedString>,
    /// Columns and rows.
    pub requested_size: Option<(u16, u16)>,
    /// Columns and rows an agent fixed. The app never resizes such a terminal.
    pub fixed_size: Option<(u16, u16)>,
    /// How a process ended. `None` while it runs. A live terminal's view knows it too.
    pub exit: Option<ExitStatus>,
    pub lines: Vec<FakeLine>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileMark {
    Modified,
    Added,
}

#[derive(Clone, Debug)]
pub(crate) struct TreeEntry {
    pub path: SharedString,
    pub depth: u8,
    pub is_dir: bool,
    pub expanded: bool,
    pub mark: Option<FileMark>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckState {
    Running,
    Passed,
}

#[derive(Clone, Debug)]
pub(crate) struct FakePullRequest {
    pub number: u32,
    pub title: SharedString,
    pub base: SharedString,
    pub head: SharedString,
    /// Name, state and duration.
    pub checks: Vec<(SharedString, CheckState, SharedString)>,
}

pub(crate) const PULL_REQUEST_TITLE: &str =
    "feat(term): render terminals with libghostty-vt in GPUI";

pub(crate) const PULL_REQUEST_DESCRIPTION: &str = "## What

- added a GPUI terminal element on libghostty-vt
- added PTY I/O with portable-pty
- added 38 theme tokens for light and dark

## Why

- agents need a fast, native terminal

## Notes for reviewers

- check the frame bench in crates/swarm-term/benches";

const PTY_BRIEF: &str = "Wire PTY read and write on a background thread in `crates/swarm-term`. Report the API back to me.

## Scope

- Spawn the shell with `portable-pty` or raw `openpty`. Ask me before you add a dependency.
- Read output on its own thread and send it over a `flume` channel.
- Resize with `TIOCSWINSZ` when the grid changes.
- Add tests for spawn, resize and exit.";

/// The `PRStatus` mockup's pull request.
pub(crate) fn sample_pull_request() -> FakePullRequest {
    let check = |name: &str, state: CheckState, duration: &str| {
        (SharedString::new(name), state, SharedString::new(duration))
    };

    FakePullRequest {
        number: 42,
        title: PULL_REQUEST_TITLE.into(),
        base: "main".into(),
        head: "swarm/terminal-renderer".into(),
        checks: vec![
            check("build-macos", CheckState::Running, "2m 10s"),
            check("cargo test --workspace", CheckState::Running, "1m 02s"),
            check("cargo clippy", CheckState::Passed, "41s"),
            check("cargo fmt --check", CheckState::Passed, "12s"),
        ],
    }
}

fn sample_file_tree() -> Vec<TreeEntry> {
    let dir = |path: &str, depth: u8, expanded: bool| TreeEntry {
        path: SharedString::new(path),
        depth,
        is_dir: true,
        expanded,
        mark: None,
    };
    let file = |path: &str, depth: u8, mark: Option<FileMark>| TreeEntry {
        path: SharedString::new(path),
        depth,
        is_dir: false,
        expanded: false,
        mark,
    };
    let (modified, added) = (Some(FileMark::Modified), Some(FileMark::Added));

    vec![
        dir("crates", 0, true),
        dir("swarm-term", 1, true),
        dir("src", 2, true),
        file("element.rs", 3, modified),
        file("font.rs", 3, added),
        file("grid.rs", 3, modified),
        file("lib.rs", 3, None),
        file("pty.rs", 3, modified),
        file("Cargo.toml", 2, modified),
        dir("swarm-ui", 1, true),
        dir("src", 2, true),
        file("lib.rs", 3, modified),
        file("theme.rs", 3, modified),
        file("tokens.rs", 3, added),
        dir("swarm-core", 1, false),
        dir("docs", 0, false),
        dir("spikes", 0, false),
        file("CLAUDE.md", 1, None),
        file("Cargo.toml", 1, None),
    ]
}

#[derive(Clone, Debug, Default)]
pub(crate) struct FakeData {
    pub projects: Vec<Project>,
    pub tasks: Vec<Task>,
    pub agents: Vec<Agent>,
    pub terminals: Vec<Terminal>,
    pub usage: Vec<(SharedString, f32)>,
    pub file_tree: Vec<TreeEntry>,
    pub pull_request: Option<FakePullRequest>,
    /// The last [`MESSAGES_PER_TASK`] messages of each task, oldest first. A Start
    /// orchestrator's messages sit under its project with no task.
    pub messages:
        HashMap<(swarm_protocol::ProjectId, Option<swarm_protocol::TaskId>), VecDeque<Message>>,
    /// Counts every change to `messages`, so a view can tell when its copy is stale.
    pub messages_revision: u64,
    /// Delivery times that came before their message, newest last.
    pub early_deliveries: VecDeque<(MessageId, u64)>,
    /// The latest changes of each agent's worktree. Missing while unknown.
    pub agent_changes: HashMap<AgentId, AgentChanges>,
    /// The latest diff of each task and source, see [`FakeData::diff_id`].
    pub diffs: HashMap<(swarm_protocol::TaskId, ReviewSource), Rc<TaskDiff>>,
}

/// What an agent changed: the totals and the files, most changed lines first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentChanges {
    pub stat: DiffStat,
    pub files: Rc<[FileStat]>,
}

fn sample_usage() -> Vec<(SharedString, f32)> {
    vec![("Claude · 5h".into(), 0.42), ("Codex · 5h".into(), 0.18)]
}

const PROJECT_TILES: [u32; 4] = [0x004f_46e5, 0x000d_7d70, 0x00c2_410c, 0x0015_803d];

fn lines(items: &[(&str, Color)]) -> Vec<FakeLine> {
    items
        .iter()
        .map(|(text, color)| FakeLine {
            text: SharedString::new(*text),
            color: *color,
        })
        .collect()
}

#[derive(Default)]
struct Builder {
    data: FakeData,
    next_agent: u32,
    next_terminal: u32,
    next_task: u32,
}

struct SampleSwarm {
    task: TaskId,
    orchestrator: AgentId,
    term_render: AgentId,
    pty_io: AgentId,
    ui_tokens: AgentId,
    bench: AgentId,
}

struct AgentSpec<'a> {
    name: &'a str,
    kind: AgentKind,
    model: &'a str,
    status: AgentStatus,
    lines: Vec<FakeLine>,
}

impl Builder {
    fn data(&mut self) -> &mut FakeData {
        &mut self.data
    }

    fn project(&mut self, id: u32, name: &str, tile: u32) -> ProjectId {
        let id = ProjectId(id);
        let letter = name.chars().next().map_or('?', |c| c.to_ascii_uppercase());

        self.data().projects.push(Project {
            id,
            name: SharedString::new(name),
            letter,
            tile_color: rgb(tile).into(),
            base_branch: "main".into(),
            daemon: None,
            root: None,
            config_error: None,
        });

        id
    }

    fn task(
        &mut self,
        project: ProjectId,
        name: &str,
        status: TaskStatus,
        orchestrator: AgentSpec<'_>,
    ) -> TaskId {
        let id = TaskId(self.next_task);
        self.next_task += 1;

        let orchestrator = self.agent(id, orchestrator, TerminalKind::Orchestrator);

        self.data().tasks.push(Task {
            id,
            project,
            name: SharedString::new(name),
            branch: SharedString::from(format!("swarm/{name}")),
            status,
            orchestrator: Some(orchestrator),
            workers: Vec::new(),
            processes: Vec::new(),
            user_terminals: Vec::new(),
            approved: None,
            is_start: false,
            daemon: None,
        });

        id
    }

    fn agent(&mut self, task: TaskId, spec: AgentSpec<'_>, kind: TerminalKind) -> AgentId {
        let id = AgentId(self.next_agent);
        self.next_agent += 1;
        let terminal = TerminalId(self.next_terminal);
        self.next_terminal += 1;

        let meta = (kind == TerminalKind::Worker).then(|| SharedString::new(spec.model));

        let data = self.data();
        data.terminals.push(Terminal {
            id: terminal,
            kind,
            title: SharedString::new(spec.name),
            meta,
            owner: None,
            link: None,
            requested_size: None,
            daemon_owner: None,
            fixed_size: None,
            exit: None,
            lines: spec.lines,
        });
        data.agents.push(Agent {
            id,
            task,
            name: SharedString::new(spec.name),
            kind: spec.kind,
            model: SharedString::new(spec.model),
            status: spec.status,
            terminal,
            created_at: 0,
            started_at: None,
            ended_at: None,
            note: None,
            daemon: None,
        });

        id
    }

    fn worker(&mut self, task: TaskId, spec: AgentSpec<'_>) -> AgentId {
        let id = self.agent(task, spec, TerminalKind::Worker);

        if let Some(entry) = self.data().tasks.iter_mut().find(|entry| entry.id == task) {
            entry.workers.push(id);
        }

        id
    }

    fn process(
        &mut self,
        task: TaskId,
        owner: AgentId,
        title: &str,
        size: (u16, u16),
        link: Option<&str>,
        output: Vec<FakeLine>,
    ) {
        let id = TerminalId(self.next_terminal);
        self.next_terminal += 1;

        let data = self.data();
        let owner_name = data
            .agents
            .iter()
            .find(|agent| agent.id == owner)
            .map_or_else(String::new, |agent| agent.name.to_string());

        data.terminals.push(Terminal {
            id,
            kind: TerminalKind::Process,
            title: SharedString::new(title),
            meta: Some(SharedString::from(format!(
                "{owner_name} · {}×{}",
                size.0, size.1
            ))),
            owner: Some(owner),
            link: link.map(SharedString::new),
            requested_size: Some(size),
            daemon_owner: None,
            fixed_size: None,
            exit: None,
            lines: output,
        });

        if let Some(entry) = data.tasks.iter_mut().find(|entry| entry.id == task) {
            entry.processes.push(id);
        }
    }

    /// Marks the process terminal added last as ended with `code`.
    fn exited(&mut self, code: u32) {
        if let Some(terminal) = self.data.terminals.last_mut() {
            terminal.exit = Some(ExitStatus::Code(code));
        }
    }
}

fn user_terminal_lines() -> Vec<FakeLine> {
    let prompt = "swarm terminal-renderer $ ";

    lines(&[
        (&format!("{prompt}git log --oneline -3"), Color::Default),
        (
            "6cfe305 task(spikes): added terminal spike with libghostty-vt",
            Color::Muted,
        ),
        (
            "c7d3f19 task(docs): added zed architecture research",
            Color::Muted,
        ),
        (
            "cccf36a task(docs): added review and notification plans",
            Color::Muted,
        ),
        (&format!("{prompt}pnpm --dir docs dev"), Color::Default),
        ("  VITE  ready in 398 ms", Color::Ok),
        ("  ➜  Local:   http://localhost:5173/", Color::Default),
        ("^C", Color::Faint),
        (
            &format!("{prompt}cargo clippy -p swarm-term"),
            Color::Default,
        ),
        ("    Checking swarm-term v0.1.0", Color::Muted),
        ("    Finished `dev` profile in 3.42s", Color::Muted),
        (prompt, Color::Default),
    ])
}

fn minimal_orchestrator(model: &str) -> AgentSpec<'_> {
    AgentSpec {
        name: "orchestrator",
        kind: AgentKind::ClaudeCode,
        model,
        status: AgentStatus::Idle,
        lines: lines(&[("› ready", Color::Muted)]),
    }
}

impl FakeData {
    /// The Workspace mockup's data.
    #[expect(
        clippy::too_many_lines,
        reason = "one flat list of fake data reads best in one place"
    )]
    pub(crate) fn sample() -> Self {
        let mut builder = Builder::default();

        let swarm = builder.project(0, "swarm", 0x004f_46e5);
        let website = builder.project(1, "website", 0x000d_7d70);
        let billing = builder.project(2, "billing-api", 0x00c2_410c);
        builder.project(3, "extensions", 0x0015_803d);

        let renderer = builder.task(
            swarm,
            "terminal-renderer",
            TaskStatus::Working,
            AgentSpec {
                name: "orchestrator",
                kind: AgentKind::ClaudeCode,
                model: "opus 5.5",
                status: AgentStatus::Working,
                lines: lines(&[
                    ("› split the renderer work across the swarm", Color::Default),
                    ("● swarm spawn term-render --agent claude", Color::Default),
                    ("  └ worktree ready · setup ✓ 8.2s", Color::Muted),
                    ("● swarm spawn pty-io --agent codex", Color::Default),
                    ("  └ worktree ready · setup ✓ 7.9s", Color::Muted),
                    ("● swarm wait --any", Color::Default),
                    ("  └ ui-tokens done · +186 −12", Color::Muted),
                    ("● pty-io asks: portable-pty or openpty?", Color::Default),
                    ("  portable-pty. Replying now.", Color::Muted),
                    ("● swarm send pty-io \"Use portable-pty.\"", Color::Default),
                    ("◆ term-render started cargo test · 90×12", Color::Muted),
                    ("◆ ui-tokens started pnpm dev · 80×10", Color::Muted),
                    (
                        "✻ Orchestrating… (2m 14s · esc to interrupt)",
                        Color::Claude,
                    ),
                ]),
            },
        );

        let term_render = builder.worker(
            renderer,
            AgentSpec {
                name: "term-render",
                kind: AgentKind::ClaudeCode,
                model: "sonnet 5.5",
                status: AgentStatus::Working,
                lines: lines(&[
                    ("● Update(src/element.rs)", Color::Default),
                    ("  88 + shape_line(row, &font, cx)", Color::Add),
                    ("  88 - paint_cell(cell, cx)", Color::Del),
                    ("● swarm terminal spawn", Color::Default),
                    ("  \"cargo test -p swarm-term\" 90x14", Color::Muted),
                    ("● swarm terminal wait --exit", Color::Default),
                    ("✻ Working… 4m 02s", Color::Claude),
                ]),
            },
        );
        let pty_io = builder.worker(
            renderer,
            AgentSpec {
                name: "pty-io",
                kind: AgentKind::Codex,
                model: "gpt-6-luna",
                status: AgentStatus::Blocked,
                lines: lines(&[
                    ("• Ran cargo test -p swarm-term", Color::Default),
                    ("  └ 14 passed · 0 failed", Color::Ok),
                    ("• swarm ask orchestrator", Color::Default),
                    ("  └ portable-pty or openpty?", Color::Muted),
                    ("◦ Waiting for orchestrator", Color::Warn),
                ]),
            },
        );
        let ui_tokens = builder.worker(
            renderer,
            AgentSpec {
                name: "ui-tokens",
                kind: AgentKind::ClaudeCode,
                model: "sonnet 5.5",
                status: AgentStatus::Done,
                lines: lines(&[
                    ("● Update(src/tokens.rs)", Color::Default),
                    ("✓ Done · +186 −12", Color::Ok),
                ]),
            },
        );
        let bench = builder.worker(
            renderer,
            AgentSpec {
                name: "bench",
                kind: AgentKind::Codex,
                model: "codex",
                status: AgentStatus::Idle,
                lines: lines(&[("◦ Idle", Color::Faint)]),
            },
        );

        builder.process(
            renderer,
            term_render,
            "cargo test",
            (90, 12),
            None,
            lines(&[
                ("running 22 tests", Color::Default),
                ("test pty::spawn ... ok", Color::Muted),
                ("test pty::resize ... ok", Color::Muted),
                ("test grid::shape_line ... ok", Color::Muted),
                ("test grid::scrollback ... ok", Color::Muted),
                ("test element::paint_cursor ...", Color::Default),
            ]),
        );
        builder.process(
            renderer,
            ui_tokens,
            "pnpm dev",
            (80, 10),
            Some("localhost:5173"),
            lines(&[
                ("  VITE  ready in 412 ms", Color::Ok),
                ("", Color::Muted),
                ("  ➜  Local:   http://localhost:5173/", Color::Default),
                ("  ➜  Network: use --host to expose", Color::Muted),
                ("10:41:07 [vite] hmr update /tokens.css", Color::Faint),
            ]),
        );

        builder.process(
            renderer,
            pty_io,
            "cargo test -p swarm-term --watch",
            (90, 12),
            None,
            lines(&[
                ("running 14 tests", Color::Default),
                ("test pty::spawn ... ok", Color::Muted),
                ("test pty::resize ... ok", Color::Muted),
            ]),
        );
        builder.process(
            renderer,
            pty_io,
            "cargo build -p swarm-term",
            (80, 12),
            None,
            lines(&[
                ("   Compiling swarm-term v0.1.0", Color::Muted),
                ("    Finished `dev` profile in 3.42s", Color::Muted),
            ]),
        );
        builder.exited(0);

        builder.task(
            swarm,
            "menu-bar-usage",
            TaskStatus::NeedsYou,
            minimal_orchestrator("opus 5.5"),
        );
        builder.task(
            swarm,
            "worktree-scripts",
            TaskStatus::Idle,
            minimal_orchestrator("opus 5.5"),
        );
        builder.task(
            website,
            "landing-page",
            TaskStatus::NeedsYou,
            minimal_orchestrator("opus 5.5"),
        );
        builder.task(
            billing,
            "invoice-export",
            TaskStatus::Working,
            minimal_orchestrator("opus 5.5"),
        );
        builder.task(
            billing,
            "tax-rates",
            TaskStatus::Working,
            minimal_orchestrator("opus 5.5"),
        );

        let orchestrator = builder
            .data
            .task(renderer)
            .and_then(|task| task.orchestrator)
            .unwrap_or(AgentId(0));
        let team = SampleSwarm {
            task: renderer,
            orchestrator,
            term_render,
            pty_io,
            ui_tokens,
            bench,
        };

        let mut data = builder.data;
        data.add_sample_swarm(&team);
        data.file_tree = sample_file_tree();
        data.usage = sample_usage();

        data
    }

    /// The Live and Merged diff of the sample task, with the files its workers changed.
    fn add_sample_diffs(&mut self, team: &SampleSwarm) {
        let agents = [
            (team.term_render, "term-render"),
            (team.pty_io, "pty-io"),
            (team.ui_tokens, "ui-tokens"),
        ]
        .map(|(agent, _)| DaemonAgentId(u64::from(agent.0)));
        let task = swarm_protocol::TaskId(u64::from(team.task.0));

        for source in [ReviewSource::Live, ReviewSource::Merged] {
            let diff = crate::review_model::sample_diff(task, source, agents);

            self.diffs.insert((task, source), Rc::new(diff));
        }
    }

    /// What each worker of the sample task changed.
    fn add_sample_changes(&mut self, team: &SampleSwarm) {
        let changes = |files: &[(&str, u32, u32)]| {
            let (additions, deletions) = files.iter().fold((0, 0), |sum, (_, added, removed)| {
                (sum.0 + added, sum.1 + removed)
            });
            let listed: Vec<FileStat> = files
                .iter()
                .map(|(path, additions, deletions)| FileStat {
                    path: PathBuf::from(path),
                    additions: *additions,
                    deletions: *deletions,
                })
                .collect();

            AgentChanges {
                stat: DiffStat {
                    files: u32::try_from(listed.len()).unwrap_or(u32::MAX),
                    additions,
                    deletions,
                },
                files: listed.into(),
            }
        };
        self.agent_changes.insert(
            team.ui_tokens,
            changes(&[
                ("crates/swarm-ui/src/tokens.rs", 142, 0),
                ("crates/swarm-ui/src/theme.rs", 38, 12),
                ("crates/swarm-ui/src/lib.rs", 6, 0),
            ]),
        );
        self.agent_changes.insert(
            team.term_render,
            changes(&[
                ("crates/swarm-term/src/element.rs", 48, 12),
                ("crates/swarm-term/src/grid.rs", 22, 9),
                ("crates/swarm-term/src/font.rs", 18, 0),
                ("benches/grid.rs", 6, 0),
            ]),
        );
        self.agent_changes.insert(
            team.pty_io,
            changes(&[
                ("crates/swarm-term/src/pty.rs", 39, 8),
                ("crates/swarm-term/Cargo.toml", 2, 0),
            ]),
        );
    }

    /// Times, status notes, changes and messages for the Swarm view of the sample task.
    fn add_sample_swarm(&mut self, team: &SampleSwarm) {
        const SECOND: u64 = 1_000;

        let ago = |seconds: u64| SAMPLE_NOW - seconds * SECOND;
        let times = [
            (team.orchestrator, 840, None, "Answering pty-io"),
            (team.ui_tokens, 790, Some(ago(70)), "Done · ready to merge"),
            (team.term_render, 780, None, "Running cargo bench"),
            (team.pty_io, 400, None, "Asked a question"),
            (team.bench, 750, None, "Waiting for a task"),
        ];

        for (id, started, ended, note) in times {
            if let Some(agent) = self.agents.iter_mut().find(|agent| agent.id == id) {
                agent.created_at = ago(started);
                agent.ended_at = ended;
                agent.note = Some(SharedString::new(note));
            }
        }

        self.add_sample_changes(team);
        self.add_sample_diffs(team);

        let Some(key) = self.message_key(team.task) else {
            return;
        };
        let id_of = |agent: AgentId| DaemonAgentId(u64::from(agent.0));
        let sample = [
            (
                1,
                team.orchestrator,
                team.ui_tokens,
                MessageKind::Brief,
                785,
                true,
            ),
            (
                2,
                team.orchestrator,
                team.term_render,
                MessageKind::Brief,
                775,
                true,
            ),
            (
                4,
                team.orchestrator,
                team.pty_io,
                MessageKind::Brief,
                395,
                true,
            ),
            (
                5,
                team.ui_tokens,
                team.orchestrator,
                MessageKind::Report,
                70,
                true,
            ),
            (
                6,
                team.pty_io,
                team.orchestrator,
                MessageKind::Question,
                62,
                true,
            ),
            (
                7,
                team.orchestrator,
                team.term_render,
                MessageKind::Task,
                8,
                false,
            ),
            (
                8,
                team.orchestrator,
                team.pty_io,
                MessageKind::Task,
                6,
                false,
            ),
        ];
        let texts = [
            "Build the color tokens for the terminal. Report when the theme compiles.",
            "Render the terminal grid with shaped lines. Report the frame time.",
            PTY_BRIEF,
            "Tokens done: 38 tokens, light and dark. Ready to merge.",
            "`portable-pty` or raw `openpty`? `portable-pty` adds 4 dependencies.",
            "Run cargo bench on the grid and report the numbers.",
            "`portable-pty`. It handles resize and gives us Windows later.",
        ];

        let log = self.messages.entry(key).or_default();
        for ((id, from, to, kind, seconds, delivered), text) in sample.into_iter().zip(texts) {
            log.push_back(Message {
                id: MessageId(id),
                from: Some(id_of(from)),
                to: id_of(to),
                kind,
                text: SharedString::new(text),
                sent_at: ago(seconds),
                delivered_at: delivered.then(|| ago(seconds) + SECOND),
            });
        }
    }

    /// No projects yet. Usage and the file tree stay empty until they are real.
    pub(crate) fn live() -> Self {
        Self::default()
    }

    /// Adds or updates the projects and their tasks. Known ones keep their ids.
    pub(crate) fn sync_projects(&mut self, projects: &[ProjectInfo]) {
        for info in projects {
            self.sync_project(info);
        }
    }

    pub(crate) fn sync_project(&mut self, info: &ProjectInfo) -> ProjectId {
        let existing = self
            .projects
            .iter()
            .find(|project| project.daemon == Some(info.id))
            .map(|project| project.id);

        let id = existing.unwrap_or_else(|| {
            let id = ProjectId(
                self.projects
                    .iter()
                    .map(|project| project.id.0 + 1)
                    .max()
                    .unwrap_or(0),
            );
            let tile = PROJECT_TILES[self.projects.len() % PROJECT_TILES.len()];

            self.projects.push(Project {
                id,
                name: SharedString::new(&info.name),
                letter: info
                    .name
                    .chars()
                    .next()
                    .map_or('?', |letter| letter.to_ascii_uppercase()),
                tile_color: rgb(tile).into(),
                base_branch: SharedString::new(&info.base_branch),
                daemon: Some(info.id),
                root: Some(info.path.clone()),
                config_error: None,
            });

            id
        });

        if let Some(project) = self.projects.iter_mut().find(|project| project.id == id) {
            project.base_branch = SharedString::new(&info.base_branch);
            project.root = Some(info.path.clone());
            project.config_error = info.config_error.as_deref().map(SharedString::new);
        }

        for task in &info.tasks {
            self.sync_task(id, task);
        }

        if let Some(start) = &info.start {
            self.sync_agent(start);
        }

        id
    }

    /// Adds or updates a task the daemon runs, with its agents.
    pub(crate) fn sync_task(&mut self, project: ProjectId, info: &TaskInfo) -> TaskId {
        let daemon = DaemonTask {
            id: info.id,
            created_by: info.created_by,
            briefed: info.briefed,
            setup_terminal: info.setup_terminal,
            setup_exit: info.setup_exit.clone(),
            worktree: info.worktree.clone(),
            base: SharedString::new(&info.base),
        };
        let existing = self.tasks.iter_mut().find(|task| {
            task.daemon
                .as_ref()
                .is_some_and(|other| other.id == info.id)
        });

        if let Some(task) = existing {
            task.name = SharedString::new(&info.name);
            task.branch = SharedString::new(&info.branch);
            task.approved = info.approved;
            let known = task.daemon.as_ref().and_then(|old| old.setup_terminal);
            task.daemon = Some(DaemonTask {
                setup_terminal: info.setup_terminal.or(known),
                ..daemon
            });
            let id = task.id;

            self.sync_agents(info);

            return id;
        }

        let id = TaskId(
            self.tasks
                .iter()
                .map(|task| task.id.0 + 1)
                .max()
                .unwrap_or(0),
        );

        self.tasks.push(Task {
            id,
            project,
            name: SharedString::new(&info.name),
            branch: SharedString::new(&info.branch),
            status: TaskStatus::Idle,
            orchestrator: None,
            workers: Vec::new(),
            processes: Vec::new(),
            user_terminals: Vec::new(),
            approved: info.approved,
            is_start: false,
            daemon: Some(daemon),
        });

        self.sync_agents(info);

        id
    }

    fn sync_agents(&mut self, info: &TaskInfo) {
        for agent in &info.agents {
            self.sync_agent(agent);
        }
    }

    /// Adds or updates an agent the daemon runs. Returns the agent and its tile's terminal,
    /// or `None` when its task or project is unknown. A copy with a lower revision than
    /// the one known is dropped, whether it comes from a listing or an event.
    pub(crate) fn sync_agent(&mut self, info: &AgentInfo) -> Option<(AgentId, TerminalId)> {
        let task = match info.task {
            Some(id) => self.task_with_daemon_id(id)?,
            None => self.start_tab_for(info)?,
        };

        let daemon = DaemonAgent {
            id: info.id,
            project: info.project,
            task: info.task,
            terminal: info.terminal,
            status: info.status,
            message: info.message.as_deref().map(SharedString::new),
            lifecycle: info.lifecycle,
            revision: info.revision,
            worktree: info.worktree.clone(),
            branch: SharedString::new(&info.branch),
        };

        let existing = self.agents.iter_mut().find(|agent| {
            agent
                .daemon
                .as_ref()
                .is_some_and(|other| other.id == info.id)
        });

        let ids = if let Some(agent) = existing {
            let stale = agent
                .daemon
                .as_ref()
                .is_some_and(|known| known.revision >= info.revision);
            if stale {
                return Some((agent.id, agent.terminal));
            }

            agent.model = SharedString::new(&info.model);
            agent.status = ui_status(info.status);
            agent.created_at = info.created_at;
            agent.started_at = info.started_at;
            agent.ended_at = info.ended_at;
            agent.daemon = Some(daemon);

            (agent.id, agent.terminal)
        } else {
            self.add_daemon_agent(task, info, daemon)
        };

        self.refresh_task_status(task);

        Some(ids)
    }

    fn add_daemon_agent(
        &mut self,
        task: TaskId,
        info: &AgentInfo,
        daemon: DaemonAgent,
    ) -> (AgentId, TerminalId) {
        let id = AgentId(
            self.agents
                .iter()
                .map(|agent| agent.id.0 + 1)
                .max()
                .unwrap_or(0),
        );
        let terminal = self.next_terminal_id();
        let name = SharedString::new(&info.name);
        let kind = match info.role {
            AgentRole::Orchestrator | AgentRole::StartOrchestrator => TerminalKind::Orchestrator,
            AgentRole::Worker => TerminalKind::Worker,
        };

        self.terminals.push(Terminal {
            id: terminal,
            kind,
            title: name.clone(),
            meta: None,
            owner: None,
            link: None,
            requested_size: None,
            daemon_owner: None,
            fixed_size: None,
            exit: None,
            lines: Vec::new(),
        });
        self.agents.push(Agent {
            id,
            task,
            name,
            kind: info.kind.into(),
            model: SharedString::new(&info.model),
            status: ui_status(info.status),
            terminal,
            created_at: info.created_at,
            started_at: info.started_at,
            ended_at: info.ended_at,
            note: None,
            daemon: Some(daemon),
        });

        if let Some(entry) = self.tasks.iter_mut().find(|entry| entry.id == task) {
            match info.role {
                AgentRole::Orchestrator | AgentRole::StartOrchestrator => {
                    entry.orchestrator = Some(id);
                }
                AgentRole::Worker => entry.workers.push(id),
            }
        }

        for entry in &mut self.terminals {
            if entry.daemon_owner == Some(info.id) {
                entry.owner = Some(id);
            }
        }

        (id, terminal)
    }

    /// The id Review calls and diffs use for a tab: the daemon's, or one made from the
    /// fake id. A Start or Plan tab has none.
    pub(crate) fn diff_id(&self, task: TaskId) -> Option<swarm_protocol::TaskId> {
        let task = self.task(task).filter(|task| !task.is_start)?;

        Some(
            task.daemon
                .as_ref()
                .map_or(swarm_protocol::TaskId(u64::from(task.id.0)), |daemon| {
                    daemon.id
                }),
        )
    }

    /// The latest diff of a tab from one source.
    pub(crate) fn task_diff(&self, task: TaskId, source: ReviewSource) -> Option<&Rc<TaskDiff>> {
        self.diffs.get(&(self.diff_id(task)?, source))
    }

    /// The totals of a tab's Live diff, zero while it is unknown.
    pub(crate) fn live_stat(&self, task: TaskId) -> DiffStat {
        self.task_diff(task, ReviewSource::Live)
            .map(|diff| diff.stat)
            .unwrap_or_default()
    }

    /// Keeps a diff the daemon sent. Returns false when it is the one already kept.
    pub(crate) fn store_diff(&mut self, diff: TaskDiff) -> bool {
        let key = (diff.task, diff.source);

        if self
            .diffs
            .get(&key)
            .is_some_and(|known| known.fingerprint == diff.fingerprint)
        {
            return false;
        }

        self.diffs.insert(key, Rc::new(diff));

        true
    }

    /// How many workers of a tab work or wait for the user.
    pub(crate) fn working_workers(&self, task: TaskId) -> usize {
        self.task(task).map_or(0, |task| {
            task.workers
                .iter()
                .filter_map(|id| self.agent(*id))
                .filter(|agent| agent.is_working())
                .count()
        })
    }

    pub(crate) fn has_daemon_task(&self, id: swarm_protocol::TaskId) -> bool {
        self.task_with_daemon_id(id).is_some()
    }

    fn task_with_daemon_id(&self, id: swarm_protocol::TaskId) -> Option<TaskId> {
        self.tasks
            .iter()
            .find(|task| task.daemon.as_ref().is_some_and(|daemon| daemon.id == id))
            .map(|task| task.id)
    }

    pub(crate) fn project_with_daemon_id(
        &self,
        id: swarm_protocol::ProjectId,
    ) -> Option<ProjectId> {
        self.projects
            .iter()
            .find(|project| project.daemon == Some(id))
            .map(|project| project.id)
    }

    pub(crate) fn agent_with_daemon_id(&self, id: DaemonAgentId) -> Option<&Agent> {
        self.agents
            .iter()
            .find(|agent| agent.daemon.as_ref().is_some_and(|daemon| daemon.id == id))
    }

    /// The tab of a Start orchestrator: its project's Start tab, made when missing. An
    /// orchestrator that already ended gets no tab of its own.
    fn start_tab_for(&mut self, info: &AgentInfo) -> Option<TaskId> {
        let project = self.project_with_daemon_id(info.project)?;
        let ended = matches!(
            info.lifecycle,
            AgentLifecycle::Stopped | AgentLifecycle::Resumable
        );

        match self.start_task(project) {
            Some(task) => {
                let known = self.start_agent_of(task);

                (!ended || known == Some(info.id)).then_some(task)
            }
            None if ended => None,
            None => Some(self.ensure_start_task(project)),
        }
    }

    /// The project's Start or Plan tab.
    pub(crate) fn start_task(&self, project: ProjectId) -> Option<TaskId> {
        self.tasks_of(project)
            .find(|task| task.is_start)
            .map(|task| task.id)
    }

    /// The daemon id of the Start orchestrator on a Start tab.
    pub(crate) fn start_agent_of(&self, task: TaskId) -> Option<DaemonAgentId> {
        self.task(task)?
            .orchestrator
            .and_then(|id| self.agent(id))
            .and_then(|agent| agent.daemon.as_ref())
            .map(|daemon| daemon.id)
    }

    /// True while the Start orchestrator of the project's Start tab runs.
    pub(crate) fn start_runs(&self, project: ProjectId) -> bool {
        self.start_task(project)
            .and_then(|task| self.task(task)?.orchestrator)
            .and_then(|id| self.agent(id)?.daemon.as_ref())
            .is_some_and(|daemon| {
                matches!(
                    daemon.lifecycle,
                    AgentLifecycle::Starting | AgentLifecycle::Running
                )
            })
    }

    /// What a tab shows as its name. A Start tab is "Start" while every task of the
    /// project came from its orchestrator, else "Plan".
    pub(crate) fn tab_label(&self, task: &Task) -> SharedString {
        if !task.is_start {
            return task.name.clone();
        }

        let start = self.start_agent_of(task.id);
        let only_its_tasks =
            self.tasks_of(task.project)
                .filter(|other| !other.is_start)
                .all(|other| {
                    other.daemon.as_ref().is_some_and(|daemon| {
                        daemon.created_by.is_some() && daemon.created_by == start
                    })
                });

        SharedString::new_static(if only_its_tasks {
            START_TASK
        } else {
            PLAN_TASK
        })
    }

    /// The daemon task with the lowest id that a Start orchestrator made.
    pub(crate) fn first_task_made_by(
        &self,
        project: ProjectId,
        agent: DaemonAgentId,
    ) -> Option<TaskId> {
        self.tasks_of(project)
            .filter_map(|task| {
                let daemon = task.daemon.as_ref()?;

                (daemon.created_by == Some(agent)).then_some((daemon.id, task.id))
            })
            .min_by_key(|(daemon_id, _)| *daemon_id)
            .map(|(_, task)| task)
    }

    /// Removes the agents of a tab and their terminals. Returns the terminals.
    pub(crate) fn remove_agents_of(&mut self, task: TaskId) -> Vec<TerminalId> {
        let gone: Vec<TerminalId> = self
            .agents
            .iter()
            .filter(|agent| agent.task == task)
            .map(|agent| agent.terminal)
            .collect();

        self.agents.retain(|agent| agent.task != task);
        self.terminals
            .retain(|terminal| !gone.contains(&terminal.id));

        if let Some(entry) = self.tasks.iter_mut().find(|entry| entry.id == task) {
            entry.orchestrator = None;
            entry.workers.clear();
        }

        gone
    }

    /// Removes a tab with its agents and terminals. Returns the terminals.
    pub(crate) fn remove_task(&mut self, task: TaskId) -> Vec<TerminalId> {
        if let Some(id) = self.diff_id(task) {
            self.diffs.retain(|(other, _), _| *other != id);
        }

        let mut gone = self.remove_agents_of(task);

        if let Some(entry) = self.task(task) {
            let others: Vec<TerminalId> = entry
                .processes
                .iter()
                .chain(&entry.user_terminals)
                .copied()
                .collect();

            self.terminals
                .retain(|terminal| !others.contains(&terminal.id));
            gone.extend(others);
        }

        self.tasks.retain(|entry| entry.id != task);

        gone
    }

    /// Keeps a message, up to the last [`MESSAGES_PER_TASK`] of its task, in id order.
    /// A copy of a kept message only fills in `delivered_at`.
    pub(crate) fn record_message(&mut self, info: &MessageInfo) {
        self.messages_revision += 1;
        let early = self.take_early_delivery(info.id);
        let delivered_at = info.delivered_at.or(early);

        let log = self.messages.entry((info.project, info.task)).or_default();

        if let Some(known) = log.iter_mut().find(|known| known.id == info.id) {
            if known.delivered_at.is_none() {
                known.delivered_at = delivered_at;
            }

            return;
        }

        let mut message = Message::from(info);
        message.delivered_at = delivered_at;

        let at = log.partition_point(|known| known.id < message.id);
        log.insert(at, message);

        while log.len() > MESSAGES_PER_TASK {
            log.pop_front();
        }
    }

    /// Keeps the messages a listing returned.
    pub(crate) fn merge_messages(&mut self, messages: &[MessageInfo]) {
        for message in messages {
            self.record_message(message);
        }
    }

    /// Sets `delivered_at` on the kept messages with these ids. Ids not kept yet are
    /// remembered, so a message that arrives later is not left in flight.
    pub(crate) fn mark_delivered(&mut self, ids: &[MessageId], delivered_at: u64) {
        self.messages_revision += 1;
        let mut missing: Vec<MessageId> = ids.to_vec();

        for message in self.messages.values_mut().flatten() {
            if !ids.contains(&message.id) {
                continue;
            }

            missing.retain(|id| *id != message.id);

            if message.delivered_at.is_none() {
                message.delivered_at = Some(delivered_at);
            }
        }

        for id in missing {
            self.early_deliveries.push_back((id, delivered_at));
        }
        while self.early_deliveries.len() > EARLY_DELIVERIES {
            self.early_deliveries.pop_front();
        }
    }

    fn take_early_delivery(&mut self, id: MessageId) -> Option<u64> {
        let at = self
            .early_deliveries
            .iter()
            .position(|(early, _)| *early == id)?;

        self.early_deliveries.remove(at).map(|(_, time)| time)
    }

    /// The kept messages of a task, oldest first. `None` is a Start orchestrator's.
    pub(crate) fn messages_of(
        &self,
        project: swarm_protocol::ProjectId,
        task: Option<swarm_protocol::TaskId>,
    ) -> impl Iterator<Item = &Message> {
        self.messages.get(&(project, task)).into_iter().flatten()
    }

    /// Where the messages of a tab are kept. Fake tabs use ids made from their own.
    pub(crate) fn message_key(
        &self,
        task: TaskId,
    ) -> Option<(swarm_protocol::ProjectId, Option<swarm_protocol::TaskId>)> {
        let task = self.task(task)?;
        let project = self.project(task.project)?;

        let project_key = project
            .daemon
            .unwrap_or(swarm_protocol::ProjectId(u64::from(project.id.0)));
        let task_key = (!task.is_start).then(|| {
            task.daemon
                .as_ref()
                .map_or(swarm_protocol::TaskId(u64::from(task.id.0)), |daemon| {
                    daemon.id
                })
        });

        Some((project_key, task_key))
    }

    /// The messages of a tab, oldest first.
    pub(crate) fn task_messages(&self, task: TaskId) -> impl Iterator<Item = &Message> {
        let (project, task) = self.message_key(task).unzip();

        project
            .into_iter()
            .flat_map(move |project| self.messages_of(project, task.flatten()))
    }

    /// Applies what the daemon says about a process terminal an agent started.
    pub(crate) fn update_process(
        &mut self,
        terminal: TerminalId,
        title: &str,
        info: &TerminalInfo,
    ) {
        let owner = info
            .owner
            .and_then(|owner| self.agent_with_daemon_id(owner))
            .map(|agent| agent.id);

        if let Some(entry) = self.terminals.iter_mut().find(|entry| entry.id == terminal) {
            entry.title = SharedString::new(title);
            entry.daemon_owner = info.owner;
            entry.owner = owner;
            entry.fixed_size = info.fixed_size.map(|size| (size.cols, size.rows));
            entry.exit.clone_from(&info.exit);
        }
    }

    /// A real task needs you when any agent is blocked or in error, and works when any agent works.
    fn refresh_task_status(&mut self, task: TaskId) {
        let statuses: Vec<DaemonAgentStatus> = self
            .agents_of(task)
            .filter_map(|agent| agent.daemon.as_ref().map(|daemon| daemon.status))
            .collect();

        let status = if statuses.iter().any(|status| {
            matches!(
                status,
                DaemonAgentStatus::Blocked(_) | DaemonAgentStatus::Error
            )
        }) {
            TaskStatus::NeedsYou
        } else if statuses.contains(&DaemonAgentStatus::Working) {
            TaskStatus::Working
        } else {
            TaskStatus::Idle
        };

        if let Some(entry) = self.tasks.iter_mut().find(|entry| entry.id == task) {
            entry.status = status;
        }
    }

    pub(crate) fn project(&self, id: ProjectId) -> Option<&Project> {
        self.projects.iter().find(|project| project.id == id)
    }

    pub(crate) fn task(&self, id: TaskId) -> Option<&Task> {
        self.tasks.iter().find(|task| task.id == id)
    }

    pub(crate) fn agent(&self, id: AgentId) -> Option<&Agent> {
        self.agents.iter().find(|agent| agent.id == id)
    }

    pub(crate) fn terminal(&self, id: TerminalId) -> Option<&Terminal> {
        self.terminals.iter().find(|terminal| terminal.id == id)
    }

    pub(crate) fn tasks_of(&self, project: ProjectId) -> impl Iterator<Item = &Task> {
        self.tasks
            .iter()
            .filter(move |task| task.project == project)
    }

    pub(crate) fn agents_of(&self, task: TaskId) -> impl Iterator<Item = &Agent> {
        self.agents.iter().filter(move |agent| agent.task == task)
    }

    /// Adds a fake user terminal to the task. It starts with the Island mockup's lines.
    pub(crate) fn add_user_terminal(&mut self, task: TaskId) -> TerminalId {
        self.push_user_terminal(
            task,
            "zsh".into(),
            Some("~/personal/swarm".into()),
            user_terminal_lines(),
        )
    }

    /// Adds a user terminal that the daemon runs. The tile shows the live view, so it
    /// has no fake lines.
    pub(crate) fn add_live_user_terminal(
        &mut self,
        task: TaskId,
        title: Option<&str>,
    ) -> TerminalId {
        self.push_user_terminal(
            task,
            "Terminal".into(),
            title.map(SharedString::new),
            Vec::new(),
        )
    }

    /// Adds a process terminal that the daemon runs.
    pub(crate) fn add_live_process(
        &mut self,
        task: TaskId,
        title: &str,
        meta: Option<SharedString>,
    ) -> TerminalId {
        let id = self.next_terminal_id();

        self.terminals.push(Terminal {
            id,
            kind: TerminalKind::Process,
            title: SharedString::new(title),
            meta,
            owner: None,
            link: None,
            requested_size: None,
            daemon_owner: None,
            fixed_size: None,
            exit: None,
            lines: Vec::new(),
        });

        if let Some(entry) = self.tasks.iter_mut().find(|entry| entry.id == task) {
            entry.processes.push(id);
        }

        id
    }

    fn next_terminal_id(&self) -> TerminalId {
        TerminalId(
            self.terminals
                .iter()
                .map(|terminal| terminal.id.0)
                .max()
                .map_or(0, |id| id + 1),
        )
    }

    fn push_user_terminal(
        &mut self,
        task: TaskId,
        title: SharedString,
        meta: Option<SharedString>,
        lines: Vec<FakeLine>,
    ) -> TerminalId {
        let id = self.next_terminal_id();

        self.terminals.push(Terminal {
            id,
            kind: TerminalKind::User,
            title,
            meta,
            owner: None,
            link: None,
            requested_size: None,
            daemon_owner: None,
            fixed_size: None,
            exit: None,
            lines,
        });

        if let Some(entry) = self.tasks.iter_mut().find(|entry| entry.id == task) {
            entry.user_terminals.push(id);
        }

        id
    }

    pub(crate) fn set_terminal_meta(&mut self, terminal: TerminalId, meta: Option<SharedString>) {
        if let Some(entry) = self.terminals.iter_mut().find(|entry| entry.id == terminal) {
            entry.meta = meta;
        }
    }

    /// The task worktree of a real task, or the repository root for the Start tab of
    /// a real project.
    pub(crate) fn worktree_of(&self, task: TaskId) -> Option<PathBuf> {
        let task = self.task(task)?;

        if let Some(daemon) = &task.daemon {
            return Some(daemon.worktree.clone());
        }

        if task.is_start {
            return self.project(task.project)?.root.clone();
        }

        None
    }

    /// The key the daemon stores with the task's terminals, see [`task_key`].
    pub(crate) fn key_of_task(&self, task: TaskId) -> Option<String> {
        let task = self.task(task)?;

        if let Some(daemon) = &task.daemon {
            return Some(daemon.id.terminal_key());
        }

        let project = self.project(task.project)?;

        if task.is_start
            && let Some(daemon) = project.daemon
        {
            return Some(start_key(daemon));
        }

        Some(task_key(&project.name, &task.name))
    }

    pub(crate) fn task_with_key(&self, key: &str) -> Option<TaskId> {
        self.tasks
            .iter()
            .find(|task| self.key_of_task(task.id).is_some_and(|other| other == key))
            .map(|task| task.id)
    }

    /// Removes a user terminal from the task.
    pub(crate) fn close_user_terminal(&mut self, task: TaskId, terminal: TerminalId) {
        if let Some(entry) = self.tasks.iter_mut().find(|entry| entry.id == task) {
            entry.user_terminals.retain(|id| *id != terminal);
        }
    }

    /// Stops a process terminal and removes it from the task.
    pub(crate) fn stop_process(&mut self, task: TaskId, terminal: TerminalId) {
        if let Some(entry) = self.tasks.iter_mut().find(|entry| entry.id == task) {
            entry.processes.retain(|id| *id != terminal);
        }
    }

    /// The project's first task. A project without tasks gets a Start task.
    pub(crate) fn first_or_start_task(&mut self, project: ProjectId) -> TaskId {
        let first = self.tasks_of(project).next().map(|task| task.id);

        first.unwrap_or_else(|| self.ensure_start_task(project))
    }

    /// The project's Start task, or Plan task when it has other tasks. Makes it when missing.
    pub(crate) fn ensure_start_task(&mut self, project: ProjectId) -> TaskId {
        let existing = self
            .tasks_of(project)
            .find(|task| task.is_start)
            .map(|task| task.id);

        if let Some(task) = existing {
            return task;
        }

        let name = if self.tasks_of(project).next().is_some() {
            PLAN_TASK
        } else {
            START_TASK
        };
        let live = self
            .project(project)
            .is_some_and(|project| project.daemon.is_some());
        let task = if live {
            self.add_empty_task(project, name)
        } else {
            let lines = vec![FakeLine {
                text: "\u{203a} ready".into(),
                color: Color::Muted,
            }];

            self.add_task(project, name, AgentKind::ClaudeCode, lines)
        };

        if let Some(entry) = self.tasks.iter_mut().find(|entry| entry.id == task) {
            entry.is_start = true;
            entry.branch = SharedString::default();
        }

        task
    }

    /// Adds a task with no agent yet, for a Start tab whose orchestrator the daemon runs.
    fn add_empty_task(&mut self, project: ProjectId, name: &str) -> TaskId {
        let task = TaskId(
            self.tasks
                .iter()
                .map(|task| task.id.0 + 1)
                .max()
                .unwrap_or(0),
        );

        self.tasks.push(Task {
            id: task,
            project,
            name: SharedString::new(name),
            branch: SharedString::from(format!("swarm/{name}")),
            status: TaskStatus::Idle,
            orchestrator: None,
            workers: Vec::new(),
            processes: Vec::new(),
            user_terminals: Vec::new(),
            approved: None,
            is_start: false,
            daemon: None,
        });

        task
    }

    /// Adds a working task with one orchestrator and its terminal.
    pub(crate) fn add_task(
        &mut self,
        project: ProjectId,
        name: &str,
        kind: AgentKind,
        terminal_lines: Vec<FakeLine>,
    ) -> TaskId {
        let next = |ids: &mut dyn Iterator<Item = u32>| ids.max().map_or(0, |id| id + 1);

        let task = TaskId(next(&mut self.tasks.iter().map(|task| task.id.0)));
        let agent = AgentId(next(&mut self.agents.iter().map(|agent| agent.id.0)));
        let terminal = TerminalId(next(&mut self.terminals.iter().map(|entry| entry.id.0)));

        let model = match kind {
            AgentKind::ClaudeCode => "opus 5.5",
            AgentKind::Codex => "codex",
        };

        self.terminals.push(Terminal {
            id: terminal,
            kind: TerminalKind::Orchestrator,
            title: "orchestrator".into(),
            meta: None,
            owner: None,
            link: None,
            requested_size: None,
            daemon_owner: None,
            fixed_size: None,
            exit: None,
            lines: terminal_lines,
        });
        self.agents.push(Agent {
            id: agent,
            task,
            name: "orchestrator".into(),
            kind,
            model: model.into(),
            status: AgentStatus::Working,
            terminal,
            created_at: 0,
            started_at: None,
            ended_at: None,
            note: None,
            daemon: None,
        });
        self.tasks.push(Task {
            id: task,
            project,
            name: SharedString::new(name),
            branch: SharedString::from(format!("swarm/{name}")),
            status: TaskStatus::Working,
            orchestrator: Some(agent),
            workers: Vec::new(),
            processes: Vec::new(),
            user_terminals: Vec::new(),
            approved: None,
            is_start: false,
            daemon: None,
        });

        task
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn every_agent_terminal_exists() {
        let data = FakeData::sample();

        assert!(
            data.agents
                .iter()
                .all(|agent| data.terminal(agent.terminal).is_some())
        );
    }

    #[test]
    fn every_orchestrator_belongs_to_its_task() {
        let data = FakeData::sample();

        assert!(data.tasks.iter().all(|task| {
            task.orchestrator
                .and_then(|id| data.agent(id))
                .is_some_and(|agent| agent.task == task.id)
        }));
    }

    #[test]
    fn ids_are_unique() {
        let data = FakeData::sample();

        let agents: HashSet<_> = data.agents.iter().map(|agent| agent.id).collect();
        let terminals: HashSet<_> = data.terminals.iter().map(|terminal| terminal.id).collect();
        let tasks: HashSet<_> = data.tasks.iter().map(|task| task.id).collect();
        let projects: HashSet<_> = data.projects.iter().map(|project| project.id).collect();

        assert_eq!(agents.len(), data.agents.len());
        assert_eq!(terminals.len(), data.terminals.len());
        assert_eq!(tasks.len(), data.tasks.len());
        assert_eq!(projects.len(), data.projects.len());
    }

    #[test]
    fn terminal_renderer_has_four_workers_and_four_processes() {
        let data = FakeData::sample();
        let task = data.tasks.first();

        assert_eq!(task.map(|task| task.workers.len()), Some(4));
        assert_eq!(task.map(|task| task.processes.len()), Some(4));
    }

    #[test]
    fn file_tree_depths_match_the_mockup() {
        let data = FakeData::sample();
        let depths: Vec<u8> = data.file_tree.iter().map(|entry| entry.depth).collect();

        assert_eq!(
            depths,
            [0, 1, 2, 3, 3, 3, 3, 3, 2, 1, 2, 3, 3, 3, 1, 0, 0, 1, 1]
        );
    }

    #[test]
    fn there_is_no_pull_request_at_first() {
        assert!(FakeData::sample().pull_request.is_none());
    }

    #[test]
    fn ensure_start_task_is_idempotent() {
        let mut data = FakeData::sample();

        let first = data.ensure_start_task(ProjectId(3));
        let second = data.ensure_start_task(ProjectId(3));

        assert_eq!(first, second);
        assert_eq!(data.tasks_of(ProjectId(3)).count(), 1);
    }

    #[test]
    fn a_start_task_is_named_after_whether_the_project_has_tasks() {
        let mut data = FakeData::sample();

        let start = data.ensure_start_task(ProjectId(3));
        let plan = data.ensure_start_task(ProjectId(0));

        let label = |task: TaskId| data.task(task).map(|task| data.tab_label(task));
        assert_eq!(label(start).as_deref(), Some(START_TASK));
        assert_eq!(label(plan).as_deref(), Some(PLAN_TASK));
        assert!(data.task(plan).is_some_and(|task| task.branch.is_empty()));
    }

    #[test]
    fn the_sample_has_no_start_task() {
        assert!(FakeData::sample().tasks.iter().all(|task| !task.is_start));
    }

    #[test]
    fn a_task_key_is_the_project_and_task_name() {
        assert_eq!(
            task_key("swarm", "terminal-renderer"),
            "swarm/terminal-renderer"
        );
    }

    #[test]
    fn a_task_is_found_again_by_its_key() {
        let data = FakeData::sample();

        let found = data.task_with_key("billing-api/tax-rates");

        assert_eq!(
            found
                .and_then(|task| data.task(task))
                .map(|task| task.name.as_ref()),
            Some("tax-rates")
        );
    }

    #[test]
    fn an_unknown_key_finds_no_task() {
        assert_eq!(FakeData::sample().task_with_key("swarm/nope"), None);
    }

    #[test]
    fn process_owners_exist() {
        let data = FakeData::sample();

        assert!(
            data.terminals
                .iter()
                .filter_map(|terminal| terminal.owner)
                .all(|owner| data.agent(owner).is_some())
        );
    }

    fn project_info(id: u64, config_error: Option<&str>) -> ProjectInfo {
        ProjectInfo {
            id: swarm_protocol::ProjectId(id),
            name: format!("demo{id}"),
            path: PathBuf::from(format!("/repo/demo{id}")),
            base_branch: "main".into(),
            config_error: config_error.map(str::to_owned),
            tasks: Vec::new(),
            start: None,
        }
    }

    #[test]
    fn a_config_error_is_kept_and_cleared_on_sync() {
        let mut data = FakeData::live();

        data.sync_project(&project_info(1, Some("bad toml")));
        let shown = data.projects[0].config_error.clone();
        data.sync_project(&project_info(1, None));

        assert_eq!(shown.as_deref(), Some("bad toml"));
        assert!(data.projects[0].config_error.is_none());
    }

    #[test]
    fn the_start_tab_of_a_real_project_has_its_own_key_and_root() {
        let mut data = FakeData::live();
        let first = data.sync_project(&project_info(1, None));
        let second = data.sync_project(&project_info(2, None));
        let (start_one, start_two) = (
            data.ensure_start_task(first),
            data.ensure_start_task(second),
        );

        assert_eq!(
            data.key_of_task(start_one).as_deref(),
            Some("project:1/start")
        );
        assert_eq!(
            data.key_of_task(start_two).as_deref(),
            Some("project:2/start")
        );
        assert_eq!(
            data.worktree_of(start_one),
            Some(PathBuf::from("/repo/demo1"))
        );
    }

    fn task_with(agents: Vec<AgentInfo>) -> TaskInfo {
        TaskInfo {
            id: swarm_protocol::TaskId(7),
            project: swarm_protocol::ProjectId(1),
            name: "login".into(),
            branch: "swarm/login".into(),
            base: "main".into(),
            worktree: PathBuf::from("/tmp/worktree"),
            ports: Vec::new(),
            setup_terminal: None,
            setup_exit: None,
            agents,
            created_by: None,
            briefed: false,
            approved: None,
            closed: false,
        }
    }

    fn agent_with(status: DaemonAgentStatus, revision: u64) -> AgentInfo {
        AgentInfo {
            id: DaemonAgentId(1),
            project: swarm_protocol::ProjectId(1),
            task: Some(swarm_protocol::TaskId(7)),
            role: AgentRole::Orchestrator,
            kind: swarm_protocol::AgentKind::ClaudeCode,
            model: "opus".into(),
            name: "orchestrator".into(),
            worktree: PathBuf::from("/tmp/worktree"),
            branch: "swarm/login".into(),
            terminal: (status != DaemonAgentStatus::Unknown).then_some(DaemonTerminalId(3)),
            status,
            message: None,
            lifecycle: AgentLifecycle::Running,
            revision,
            created_at: 0,
            started_at: None,
            ended_at: None,
        }
    }

    fn live_task(data: &mut FakeData, agents: Vec<AgentInfo>) -> TaskId {
        let project = data.sync_project(&project_info(1, None));

        data.sync_task(project, &task_with(agents))
    }

    fn orchestrator(data: &FakeData, task: TaskId) -> &Agent {
        let id = data.task(task).and_then(|task| task.orchestrator);

        id.and_then(|id| data.agent(id)).expect("an orchestrator")
    }

    #[test]
    fn an_older_listing_does_not_replace_a_newer_event() {
        let mut data = FakeData::live();
        let task = live_task(&mut data, Vec::new());
        data.sync_agent(&agent_with(DaemonAgentStatus::Working, 5));

        data.sync_task(
            data.task(task).expect("task").project,
            &task_with(vec![agent_with(DaemonAgentStatus::Unknown, 3)]),
        );

        let agent = orchestrator(&data, task);
        let daemon = agent.daemon.as_ref().expect("daemon agent");
        assert_eq!(daemon.status, DaemonAgentStatus::Working);
        assert_eq!(daemon.terminal, Some(DaemonTerminalId(3)));
        assert_eq!(data.task(task).expect("task").status, TaskStatus::Working);
    }

    #[test]
    fn a_newer_listing_replaces_an_older_event() {
        let mut data = FakeData::live();
        let task = live_task(&mut data, Vec::new());
        data.sync_agent(&agent_with(DaemonAgentStatus::Unknown, 3));

        data.sync_task(
            data.task(task).expect("task").project,
            &task_with(vec![agent_with(DaemonAgentStatus::Working, 5)]),
        );

        let daemon = orchestrator(&data, task).daemon.as_ref().expect("daemon");
        assert_eq!(daemon.status, DaemonAgentStatus::Working);
        assert_eq!(daemon.revision, 5);
    }

    #[test]
    fn an_older_event_does_not_replace_a_newer_one() {
        let mut data = FakeData::live();
        let task = live_task(&mut data, Vec::new());
        data.sync_agent(&agent_with(DaemonAgentStatus::Done, 6));

        data.sync_agent(&agent_with(DaemonAgentStatus::Working, 6));
        data.sync_agent(&agent_with(DaemonAgentStatus::Working, 4));

        let agent = orchestrator(&data, task);
        assert_eq!(agent.status, AgentStatus::Done);
    }

    #[test]
    fn an_agent_in_error_makes_the_task_need_you() {
        let mut data = FakeData::live();
        let task = live_task(&mut data, vec![agent_with(DaemonAgentStatus::Error, 1)]);

        assert_eq!(data.task(task).expect("task").status, TaskStatus::NeedsYou);
    }

    #[test]
    fn an_unknown_status_has_its_own_look_and_word() {
        let mut data = FakeData::live();
        let task = live_task(&mut data, vec![agent_with(DaemonAgentStatus::Unknown, 1)]);

        let agent = orchestrator(&data, task);
        assert_eq!(agent.status, AgentStatus::Unknown);
        assert_eq!(agent.status_text(), "unknown");
    }

    fn start_with(status: DaemonAgentStatus, revision: u64) -> AgentInfo {
        AgentInfo {
            id: DaemonAgentId(9),
            task: None,
            role: AgentRole::StartOrchestrator,
            name: "start".into(),
            ..agent_with(status, revision)
        }
    }

    #[test]
    fn a_start_orchestrator_belongs_to_the_project_start_tab_not_a_task() {
        let mut data = FakeData::live();
        let project = data.sync_project(&project_info(1, None));
        let task = data.sync_task(project, &task_with(Vec::new()));

        data.sync_agent(&start_with(DaemonAgentStatus::Idle, 1));

        let start = data.start_task(project).expect("a Start tab");
        assert_ne!(start, task);
        assert!(data.task(start).is_some_and(|tab| tab.is_start));
        assert_eq!(data.agents_of(start).count(), 1);
        assert_eq!(data.agents_of(task).count(), 0);
        assert!(data.start_runs(project));
    }

    #[test]
    fn the_start_tab_of_a_live_project_has_no_fake_orchestrator() {
        let mut data = FakeData::live();
        let project = data.sync_project(&project_info(1, None));

        let start = data.ensure_start_task(project);

        assert_eq!(data.agents_of(start).count(), 0);
        assert!(!data.start_runs(project));
    }

    #[test]
    fn an_older_start_listing_does_not_replace_a_newer_event() {
        let mut data = FakeData::live();
        let project = data.sync_project(&project_info(1, None));
        data.sync_agent(&start_with(DaemonAgentStatus::Working, 5));

        let listed = ProjectInfo {
            start: Some(start_with(DaemonAgentStatus::Idle, 3)),
            ..project_info(1, None)
        };
        data.sync_project(&listed);

        let tab = data.start_task(project).expect("a Start tab");
        let agent = data.agent_with_daemon_id(DaemonAgentId(9)).expect("agent");
        assert_eq!(agent.status, AgentStatus::Working);
        assert_eq!(data.agents_of(tab).count(), 1);
    }

    #[test]
    fn an_ended_start_orchestrator_makes_no_tab() {
        let mut data = FakeData::live();
        let project = data.sync_project(&project_info(1, None));
        let mut ended = start_with(DaemonAgentStatus::Done, 2);
        ended.lifecycle = AgentLifecycle::Resumable;

        let synced = data.sync_agent(&ended);

        assert!(synced.is_none());
        assert!(data.start_task(project).is_none());
    }

    #[test]
    fn a_worker_takes_its_name_from_the_daemon() {
        let mut data = FakeData::live();
        let task = live_task(&mut data, Vec::new());
        let worker = AgentInfo {
            id: DaemonAgentId(4),
            role: AgentRole::Worker,
            name: "api".into(),
            ..agent_with(DaemonAgentStatus::Working, 1)
        };

        data.sync_agent(&worker);

        let entry = data.task(task).expect("task");
        let agent = entry.workers.first().and_then(|id| data.agent(*id));
        assert_eq!(
            agent.map(|agent| agent.name.to_string()).as_deref(),
            Some("api")
        );
    }

    fn message(id: u64) -> MessageInfo {
        MessageInfo {
            id: MessageId(id),
            project: swarm_protocol::ProjectId(1),
            task: Some(swarm_protocol::TaskId(7)),
            from: None,
            to: DaemonAgentId(1),
            kind: MessageKind::Task,
            text: format!("message {id}"),
            sent_at: id,
            delivered_at: None,
        }
    }

    #[test]
    fn merging_a_history_drops_known_messages_and_keeps_the_order() {
        let mut data = FakeData::live();
        let delivered = MessageInfo {
            delivered_at: Some(9),
            ..message(2)
        };

        data.record_message(&message(3));
        data.record_message(&message(2));
        data.merge_messages(&[message(1), delivered, message(3)]);

        let kept: Vec<(u64, Option<u64>)> = data
            .messages_of(
                swarm_protocol::ProjectId(1),
                Some(swarm_protocol::TaskId(7)),
            )
            .map(|message| (message.id.0, message.delivered_at))
            .collect();
        assert_eq!(kept, [(1, None), (2, Some(9)), (3, None)]);
    }

    fn delivery_times(data: &FakeData) -> Vec<(u64, Option<u64>)> {
        data.messages_of(
            swarm_protocol::ProjectId(1),
            Some(swarm_protocol::TaskId(7)),
        )
        .map(|message| (message.id.0, message.delivered_at))
        .collect()
    }

    #[test]
    fn a_delivery_that_comes_before_its_message_is_applied_when_it_arrives() {
        let mut data = FakeData::live();

        data.mark_delivered(&[MessageId(2)], 9);
        data.merge_messages(&[message(1), message(2)]);

        assert_eq!(delivery_times(&data), [(1, None), (2, Some(9))]);
        assert!(data.early_deliveries.is_empty());
    }

    #[test]
    fn early_deliveries_are_bounded() {
        let mut data = FakeData::live();
        let ids: Vec<MessageId> = (0..1_500).map(MessageId).collect();

        data.mark_delivered(&ids, 9);

        assert_eq!(data.early_deliveries.len(), EARLY_DELIVERIES);
        assert_eq!(data.early_deliveries.front(), Some(&(MessageId(500), 9)));
    }

    #[test]
    fn a_delivered_copy_wins_over_a_kept_one_in_flight_and_never_the_reverse() {
        let mut data = FakeData::live();
        let delivered = MessageInfo {
            delivered_at: Some(9),
            ..message(1)
        };

        data.record_message(&message(1));
        data.record_message(&delivered);
        assert_eq!(delivery_times(&data), [(1, Some(9))]);

        data.record_message(&message(1));
        assert_eq!(delivery_times(&data), [(1, Some(9))]);
    }

    #[test]
    fn a_resumed_agent_runs_since_its_last_start() {
        let mut data = FakeData::live();
        let info = |started_at| AgentInfo {
            created_at: 100,
            started_at,
            ..agent_with(DaemonAgentStatus::Working, 1)
        };

        live_task(&mut data, vec![info(None)]);
        let never_started = data.agents.last().map(Agent::running_since);

        data.sync_agent(&AgentInfo {
            revision: 2,
            ..info(Some(500))
        });
        let resumed = data.agents.last().map(Agent::running_since);

        assert_eq!((never_started, resumed), (Some(100), Some(500)));
    }

    #[test]
    fn the_last_500_messages_of_a_task_are_kept() {
        let mut data = FakeData::live();

        for id in 0..520 {
            data.record_message(&message(id));
        }
        data.record_message(&message(519));

        let kept: Vec<u64> = data
            .messages_of(
                swarm_protocol::ProjectId(1),
                Some(swarm_protocol::TaskId(7)),
            )
            .map(|message| message.id.0)
            .collect();
        assert_eq!(kept.len(), MESSAGES_PER_TASK);
        assert_eq!((kept.first(), kept.last()), (Some(&20), Some(&519)));
    }

    #[test]
    fn messages_are_kept_per_task() {
        let mut data = FakeData::live();
        data.record_message(&message(1));
        data.record_message(&MessageInfo {
            task: None,
            ..message(2)
        });

        let count = |task| data.messages_of(swarm_protocol::ProjectId(1), task).count();
        assert_eq!(count(Some(swarm_protocol::TaskId(7))), 1);
        assert_eq!(count(None), 1);
        assert_eq!(count(Some(swarm_protocol::TaskId(8))), 0);
    }

    #[test]
    fn a_tab_is_start_only_while_the_start_orchestrator_made_every_task() {
        let mut data = FakeData::live();
        let project = data.sync_project(&project_info(1, None));
        data.sync_agent(&start_with(DaemonAgentStatus::Idle, 1));
        let start = data.start_task(project).expect("a Start tab");
        let label = |data: &FakeData| {
            data.task(start)
                .map(|tab| data.tab_label(tab).to_string())
                .unwrap_or_default()
        };
        let none = label(&data);

        let made = TaskInfo {
            created_by: Some(DaemonAgentId(9)),
            ..task_with(Vec::new())
        };
        data.sync_task(project, &made);
        let one = label(&data);

        let other = TaskInfo {
            id: swarm_protocol::TaskId(8),
            created_by: None,
            ..task_with(Vec::new())
        };
        data.sync_task(project, &other);

        assert_eq!((none.as_str(), one.as_str()), (START_TASK, START_TASK));
        assert_eq!(label(&data), PLAN_TASK);
    }

    #[test]
    fn an_agent_process_is_owned_by_the_agent_that_started_it() {
        let mut data = FakeData::live();
        let task = live_task(&mut data, vec![agent_with(DaemonAgentStatus::Idle, 1)]);
        let terminal = data.add_live_process(task, "Process", None);
        let info = TerminalInfo {
            id: DaemonTerminalId(30),
            kind: swarm_protocol::TerminalKind::Process,
            task: Some("task:7".into()),
            owner: Some(DaemonAgentId(1)),
            name: Some("dev server".into()),
            fixed_size: Some(swarm_protocol::GridSize { cols: 80, rows: 12 }),
            title: None,
            cwd: None,
            exit: None,
        };

        data.update_process(terminal, "dev server", &info);

        let entry = data.terminal(terminal).expect("terminal");
        let orchestrator = data.task(task).and_then(|task| task.orchestrator);
        assert_eq!(entry.title.as_ref(), "dev server");
        assert_eq!(entry.owner, orchestrator);
        assert_eq!(entry.fixed_size, Some((80, 12)));
    }
}
