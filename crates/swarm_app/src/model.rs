//! Fake data for the app shell. Plain data, no gpui entities.

use std::path::PathBuf;

use gpui::{Hsla, SharedString, rgb};
use swarm_protocol::{
    AgentId as DaemonAgentId, AgentInfo, AgentLifecycle, AgentRole,
    AgentStatus as DaemonAgentStatus, ProjectInfo, TaskInfo, TerminalId as DaemonTerminalId,
};
use swarm_term::{BlockedKind, ExitStatus};
use swarm_ui::AgentStatus;
use swarm_ui::prelude::Color;

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

/// What links a task to the daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DaemonTask {
    pub id: swarm_protocol::TaskId,
    pub setup_terminal: Option<DaemonTerminalId>,
    pub setup_exit: Option<ExitStatus>,
    pub worktree: PathBuf,
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
    pub additions: u32,
    pub deletions: u32,
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
    pub task: swarm_protocol::TaskId,
    /// The agent's terminal while it runs.
    pub terminal: Option<DaemonTerminalId>,
    pub status: DaemonAgentStatus,
    pub message: Option<SharedString>,
    pub lifecycle: AgentLifecycle,
    /// Of the daemon's last report. An older report is dropped.
    pub revision: u64,
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
    /// Set for an agent the daemon runs.
    pub daemon: Option<DaemonAgent>,
}

impl Agent {
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
    pub link: Option<SharedString>,
    /// Columns and rows.
    pub requested_size: Option<(u16, u16)>,
    pub lines: Vec<FakeLine>,
}

#[derive(Clone, Debug)]
pub(crate) struct ChangedFile {
    pub task: TaskId,
    pub path: SharedString,
    pub worker: AgentId,
    pub additions: u32,
    pub deletions: u32,
    pub editing: bool,
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
    pub changes: Vec<ChangedFile>,
    pub usage: Vec<(SharedString, f32)>,
    pub file_tree: Vec<TreeEntry>,
    pub pull_request: Option<FakePullRequest>,
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
            additions: 0,
            deletions: 0,
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
            lines: output,
        });

        if let Some(entry) = data.tasks.iter_mut().find(|entry| entry.id == task) {
            entry.processes.push(id);
        }
    }

    fn change(
        &mut self,
        task: TaskId,
        worker: AgentId,
        path: &str,
        (additions, deletions): (u32, u32),
        editing: bool,
    ) {
        self.data().changes.push(ChangedFile {
            task,
            path: SharedString::new(path),
            worker,
            additions,
            deletions,
            editing,
        });
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
                model: "codex",
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
        builder.worker(
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

        let src = "crates/swarm-term/src";
        builder.change(
            renderer,
            term_render,
            &format!("{src}/element.rs"),
            (48, 12),
            true,
        );
        builder.change(
            renderer,
            term_render,
            &format!("{src}/grid.rs"),
            (22, 9),
            false,
        );
        builder.change(
            renderer,
            term_render,
            &format!("{src}/font.rs"),
            (18, 0),
            false,
        );
        builder.change(renderer, term_render, "benches/grid.rs", (6, 0), false);
        builder.change(renderer, pty_io, &format!("{src}/pty.rs"), (31, 6), false);
        builder.change(
            renderer,
            pty_io,
            "crates/swarm-term/Cargo.toml",
            (2, 0),
            false,
        );
        let ui = "crates/swarm-ui/src";
        builder.change(
            renderer,
            ui_tokens,
            &format!("{ui}/tokens.rs"),
            (142, 0),
            false,
        );
        builder.change(
            renderer,
            ui_tokens,
            &format!("{ui}/theme.rs"),
            (38, 12),
            false,
        );
        builder.change(renderer, ui_tokens, &format!("{ui}/lib.rs"), (6, 0), false);

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

        let mut data = builder.data;
        data.file_tree = sample_file_tree();
        data.usage = sample_usage();

        if let Some(task) = data.tasks.iter_mut().find(|task| task.id == renderer) {
            task.additions = 312;
            task.deletions = 48;
        }

        data
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

        id
    }

    /// Adds or updates a task the daemon runs, with its agents.
    pub(crate) fn sync_task(&mut self, project: ProjectId, info: &TaskInfo) -> TaskId {
        let daemon = DaemonTask {
            id: info.id,
            setup_terminal: info.setup_terminal,
            setup_exit: info.setup_exit.clone(),
            worktree: info.worktree.clone(),
        };
        let existing = self.tasks.iter_mut().find(|task| {
            task.daemon
                .as_ref()
                .is_some_and(|other| other.id == info.id)
        });

        if let Some(task) = existing {
            task.name = SharedString::new(&info.name);
            task.branch = SharedString::new(&info.branch);
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
            additions: 0,
            deletions: 0,
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
    /// or `None` when its task is unknown.
    pub(crate) fn sync_agent(&mut self, info: &AgentInfo) -> Option<(AgentId, TerminalId)> {
        let task = self
            .tasks
            .iter()
            .find(|task| {
                task.daemon
                    .as_ref()
                    .is_some_and(|daemon| daemon.id == info.task)
            })
            .map(|task| task.id)?;

        let daemon = DaemonAgent {
            id: info.id,
            task: info.task,
            terminal: info.terminal,
            status: info.status,
            message: info.message.as_deref().map(SharedString::new),
            lifecycle: info.lifecycle,
            revision: info.revision,
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
        let (name, kind) = match info.role {
            AgentRole::Orchestrator => (
                SharedString::new("orchestrator"),
                TerminalKind::Orchestrator,
            ),
            AgentRole::Worker => (
                SharedString::from(format!("worker {}", info.id.0)),
                TerminalKind::Worker,
            ),
        };

        self.terminals.push(Terminal {
            id: terminal,
            kind,
            title: name.clone(),
            meta: None,
            owner: None,
            link: None,
            requested_size: None,
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
            daemon: Some(daemon),
        });

        if let Some(entry) = self.tasks.iter_mut().find(|entry| entry.id == task) {
            match info.role {
                AgentRole::Orchestrator => entry.orchestrator = Some(id),
                AgentRole::Worker => entry.workers.push(id),
            }
        }

        (id, terminal)
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
        let lines = vec![FakeLine {
            text: "\u{203a} ready".into(),
            color: Color::Muted,
        }];
        let task = self.add_task(project, name, AgentKind::ClaudeCode, lines);

        if let Some(entry) = self.tasks.iter_mut().find(|entry| entry.id == task) {
            entry.is_start = true;
            entry.branch = SharedString::default();
        }

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
            additions: 0,
            deletions: 0,
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
    fn terminal_renderer_has_four_workers_and_two_processes() {
        let data = FakeData::sample();
        let task = data.tasks.first();

        assert_eq!(task.map(|task| task.workers.len()), Some(4));
        assert_eq!(task.map(|task| task.processes.len()), Some(2));
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

        assert_eq!(
            data.task(start).map(|task| task.name.as_ref()),
            Some(START_TASK)
        );
        assert_eq!(
            data.task(plan).map(|task| task.name.as_ref()),
            Some(PLAN_TASK)
        );
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
        }
    }

    fn agent_with(status: DaemonAgentStatus, revision: u64) -> AgentInfo {
        AgentInfo {
            id: DaemonAgentId(1),
            task: swarm_protocol::TaskId(7),
            role: AgentRole::Orchestrator,
            kind: swarm_protocol::AgentKind::ClaudeCode,
            model: "opus".into(),
            terminal: (status != DaemonAgentStatus::Unknown).then_some(DaemonTerminalId(3)),
            status,
            message: None,
            lifecycle: AgentLifecycle::Running,
            revision,
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
}
