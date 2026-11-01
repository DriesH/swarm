//! The socket protocol between `swarm-daemon` and the app, plus the app's client.
//!
//! A frame is a little-endian `u32` length followed by one postcard-encoded
//! [`Request`] (client → daemon) or [`Event`] (daemon → client). Terminal input and
//! output reuse the `swarm_term` types, tagged with a [`TerminalId`].
//!
//! No libghostty here: the app links this crate (ADR 0007).

use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use swarm_term::{Palette, Size, TerminalInput, TerminalOutput};

pub mod agent;
mod client;

pub use client::{Client, ClientError, CommentEvent, MessageEvent, PrEvent};

/// Bumped on every incompatible change; the daemon refuses other versions.
pub const VERSION: u32 = 13;

/// The most messages [`Request::ListMessages`] returns.
pub const MESSAGES_LISTED: usize = 500;

/// The most files one [`AgentChange`] lists.
pub const FILES_LISTED: usize = 500;

/// The longest comment body, in bytes. A quarter of a message, so the orchestrator can
/// pass a comment on with its header and quoted line.
pub const COMMENT_BYTES: usize = 16 << 10;

/// The most files one [`TaskDiff`] lists. Its totals still count every file.
pub const DIFF_FILES: usize = 1_000;

/// The most lines one [`FileDiff`] carries; the rest is left out and the file is
/// marked truncated.
pub const DIFF_LINES_PER_FILE: usize = 5_000;

/// The most lines one [`TaskDiff`] carries over all its files.
pub const DIFF_LINES: usize = 50_000;

/// Longer lines are cut to this many bytes, on a character boundary.
pub const DIFF_LINE_BYTES: usize = 1_000;

/// A file with more changed lines comes folded, see [`Fold::Large`].
pub const FOLD_LINES: u32 = 500;

/// The longest a push may run in the daemon, pre-push hooks included.
pub const PUSH_SECS: u64 = 600;

/// The longest one `gh` call may run in the daemon.
pub const GH_SECS: u64 = 60;

/// Guards against a corrupt length prefix allocating gigabytes.
const MAX_FRAME: usize = 256 << 20;

/// macOS limit for a unix socket path (`sun_path`), including the trailing NUL.
const MAX_SOCKET_PATH: usize = 104;

/// Stable for the daemon's lifetime and stored in its database.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TerminalId(pub u64);

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalKind {
    /// Opened by the user. Agents can never see it (ADR 0006).
    User,
    Process,
    Agent,
}

/// A terminal size in cells.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CreateTerminal {
    pub kind: TerminalKind,
    /// `None` runs the user's login shell.
    pub command: Option<Vec<String>>,
    pub cwd: Option<PathBuf>,
    pub size: Size,
    pub palette: Palette,
    /// Opaque key the app uses to find the terminal's task again after a restart.
    pub task: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectId(pub u64);

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(pub u64);

impl TaskId {
    /// The `task` key of the terminals that belong to this task.
    pub fn terminal_key(self) -> String {
        format!("task:{}", self.0)
    }
}

/// A git repository the user added to Swarm.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ProjectInfo {
    pub id: ProjectId,
    /// The repository folder's name.
    pub name: String,
    /// The repository root.
    pub path: PathBuf,
    pub base_branch: String,
    /// Why `.swarm/config.toml` could not be read; defaults apply meanwhile.
    pub config_error: Option<String>,
    pub tasks: Vec<TaskInfo>,
    /// The project's Start orchestrator while it has not stopped.
    pub start: Option<AgentInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AgentId(pub u64);

/// Which CLI an agent runs.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentKind {
    ClaudeCode,
    Codex,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentRole {
    Orchestrator,
    Worker,
    /// Splits a request into tasks in the project root, then exits. Has no task.
    StartOrchestrator,
}

/// What an agent is doing, merged from OSC 7501, hooks and the process exit.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Unknown,
    Idle,
    Working,
    Done,
    Blocked(swarm_term::BlockedKind),
    Error,
}

/// Whether the agent's process runs. Separate from [`AgentStatus`].
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentLifecycle {
    Starting,
    Running,
    /// Ended; it can start again from its session.
    Resumable,
    Stopped,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AgentInfo {
    pub id: AgentId,
    pub project: ProjectId,
    /// `None` for a Start orchestrator.
    pub task: Option<TaskId>,
    pub role: AgentRole,
    pub kind: AgentKind,
    pub model: String,
    /// "orchestrator", "start" or the worker's name; unique within its task.
    pub name: String,
    /// Where it works: a worker worktree, the task worktree or the project root.
    pub worktree: PathBuf,
    pub branch: String,
    /// The agent's terminal while it runs.
    pub terminal: Option<TerminalId>,
    pub status: AgentStatus,
    /// The program's own status text, or an error reason.
    pub message: Option<String>,
    pub lifecycle: AgentLifecycle,
    /// Goes up with every change, so a client can drop a copy older than the one it has.
    pub revision: u64,
    /// Milliseconds since the Unix epoch.
    pub created_at: u64,
    /// When its process last started, in milliseconds since the Unix epoch. `None`
    /// before the first start.
    pub started_at: Option<u64>,
    /// When its process last ended, in milliseconds since the Unix epoch. `None` while
    /// it runs or before it starts.
    pub ended_at: Option<u64>,
}

/// One task: its own branch and task worktree.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TaskInfo {
    pub id: TaskId,
    pub project: ProjectId,
    pub name: String,
    pub branch: String,
    /// The branch it started from.
    pub base: String,
    pub worktree: PathBuf,
    /// Ports reserved for this task worktree, from `[worktrees] ports`.
    pub ports: Vec<u16>,
    /// The process terminal running the setup script, while it runs.
    pub setup_terminal: Option<TerminalId>,
    /// How the setup script ended, so a fast or failed setup is never missed.
    pub setup_exit: Option<swarm_term::ExitStatus>,
    pub agents: Vec<AgentInfo>,
    /// The Start orchestrator that made the task, if one did.
    pub created_by: Option<AgentId>,
    /// The task's orchestrator confirmed the brief it started with.
    pub briefed: bool,
    /// The [`TaskDiff::fingerprint`] of the Merged diff the user approved. The approval
    /// holds while the Merged diff still has this fingerprint.
    pub approved: Option<u64>,
    /// The task was closed: its agents stopped and its worktrees are gone. Sent once in
    /// a `TaskChanged`; closed tasks are never listed.
    pub closed: bool,
    /// The branch the pull request merges into: the branch the task started from when
    /// `origin` has it, else the project's base branch.
    pub pr_base: String,
    /// The pull request made from the task branch, once it exists.
    pub pull_request: Option<PrLink>,
    /// Close the task (as [`Request::CloseTask`] does) once its pull request is merged.
    pub close_after_merge: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CreateTask {
    pub project: ProjectId,
    pub name: String,
    /// Branch to start from; `None` uses the project's base branch.
    pub from: Option<String>,
    pub run_setup: bool,
    /// Starts an orchestrator of this kind once setup is done; `None` starts none.
    pub orchestrator: Option<AgentKind>,
    /// Typed into the orchestrator once it reports that it is ready.
    pub first_message: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TerminalInfo {
    pub id: TerminalId,
    pub kind: TerminalKind,
    pub task: Option<String>,
    /// The agent that started this process terminal.
    pub owner: Option<AgentId>,
    /// A label the owner gave, such as "dev server".
    pub name: Option<String>,
    /// Unused: a process terminal follows its tile. The daemon sends `None`.
    pub fixed_size: Option<GridSize>,
    pub title: Option<String>,
    pub cwd: Option<PathBuf>,
    /// `None` while the process runs.
    pub exit: Option<swarm_term::ExitStatus>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MessageId(pub u64);

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageKind {
    /// The first message of a worker or a task orchestrator.
    Brief,
    /// From the orchestrator to a worker after the brief.
    Task,
    Question,
    Report,
    /// From Swarm: a process terminal the recipient owns or oversees ended.
    TerminalExited,
    /// From Swarm: the user typed into a worker's terminal (ADR 0002).
    UserInput,
    /// From Swarm: a worker failed to start, or its process ended.
    WorkerStopped,
    /// From Swarm: the user's review comments, to the task orchestrator, which passes
    /// each one on to the worker it names.
    Comment,
    /// From Swarm: asks the task orchestrator for a pull request title and description,
    /// which it sends back with `swarm pr draft`.
    PrDraft,
}

impl MessageKind {
    /// The words people and agents read: "brief", "question", "user input".
    pub fn label(self) -> &'static str {
        match self {
            Self::Brief => "brief",
            Self::Task => "task",
            Self::Question => "question",
            Self::Report => "report",
            Self::TerminalExited => "terminal exited",
            Self::UserInput => "user input",
            Self::WorkerStopped => "worker stopped",
            Self::Comment => "comment",
            Self::PrDraft => "pr draft",
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MessageInfo {
    pub id: MessageId,
    pub project: ProjectId,
    pub task: Option<TaskId>,
    /// `None` when Swarm itself sends it.
    pub from: Option<AgentId>,
    pub to: AgentId,
    pub kind: MessageKind,
    pub text: String,
    /// Milliseconds since the Unix epoch.
    pub sent_at: u64,
    /// When the recipient got it. `None` while it is in flight.
    pub delivered_at: Option<u64>,
}

/// What an agent changed in its worktree since it branched off: committed and
/// uncommitted work, untracked files included.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiffStat {
    pub files: u32,
    pub additions: u32,
    pub deletions: u32,
}

/// One changed file of an agent. `path` is relative to the agent's worktree.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    pub path: PathBuf,
    pub additions: u32,
    pub deletions: u32,
}

/// What one agent changed: the totals, and up to [`FILES_LISTED`] files, most changed
/// lines first.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AgentChange {
    pub agent: AgentId,
    pub stat: DiffStat,
    pub files: Vec<FileStat>,
}

/// Where Review reads the changes of a task from.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReviewSource {
    /// The task worktree against the base branch, plus each worker with its own
    /// worktree that is not merged yet, against the commit it started from.
    Live,
    /// The commits of the task branch since it left the base branch: what the pull
    /// request would hold. Uncommitted work is left out.
    Merged,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChange {
    Added,
    Modified,
    Deleted,
}

/// Why a file of a diff comes without its lines. [`Request::FileHunks`] loads them.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    /// The lock file of a package manager, like `Cargo.lock` or `pnpm-lock.yaml`.
    Lock,
    /// Marked `linguist-generated` in `.gitattributes`, minified, or a source map.
    Generated,
    /// More than [`FOLD_LINES`] changed lines.
    Large,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Added,
    Deleted,
    /// The line before has no line end (git's `\ No newline at end of file`). Its
    /// text is empty.
    NoNewline,
}

/// One line of a hunk, without the leading `+`, `-` or space and without the line end.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

/// One `@@ -old_start,old_lines +new_start,new_lines @@ section` block.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    /// The text git prints after the second `@@`, usually the enclosing function.
    pub section: String,
    pub lines: Vec<DiffLine>,
}

/// One changed file in Review.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    /// The worktree the file is in: the task worktree, or a worker's own worktree.
    pub worktree: PathBuf,
    /// Relative to `worktree`. A rename shows as a deleted and an added file.
    pub path: PathBuf,
    /// The agent whose `swarm commit` last touched the file, or the worker that owns
    /// the worktree. `None` when no agent commit touched it.
    pub agent: Option<AgentId>,
    pub change: FileChange,
    /// The old and new file mode (for example `0o100644` and `0o100755`) when the mode
    /// changed.
    pub mode_change: Option<(u32, u32)>,
    pub additions: u32,
    pub deletions: u32,
    pub binary: bool,
    pub hunks: Vec<DiffHunk>,
    /// Lines were left out because of [`DIFF_LINES_PER_FILE`] or [`DIFF_LINES`], or
    /// the whole file because it is too large to read. A file left out whole has no
    /// hunks and is not binary.
    pub truncated: bool,
    /// A hash of the whole change of the file, also of lines left out. Equal digests
    /// mean equal changes.
    pub digest: u64,
    /// Set when the file comes without its lines: `hunks` is empty, and `additions`
    /// and `deletions` still count every line. Kept in a [`Event::FileHunks`] answer.
    pub fold: Option<Fold>,
}

/// The changes of a task from one [`ReviewSource`].
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TaskDiff {
    pub task: TaskId,
    pub source: ReviewSource,
    /// A hash of every changed line, file mode and blob, also of lines left out by
    /// the caps. Equal fingerprints mean equal changes. Stable across daemon restarts.
    pub fingerprint: u64,
    /// The commit at HEAD of the task worktree when the diff was read.
    pub head: String,
    /// Totals over every changed file, also files past [`DIFF_FILES`].
    pub stat: DiffStat,
    /// Up to [`DIFF_FILES`] files, sorted by worktree, then path.
    pub files: Vec<FileDiff>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommentId(pub u64);

/// The side of a diff a line is on: the base version, or the changed version.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiffSide {
    Old,
    New,
}

/// Where a comment sits: a line found by its text, not its number, so it follows the
/// code while agents edit around it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CommentAnchor {
    /// The worktree of the [`FileDiff`] the comment was made on.
    pub worktree: PathBuf,
    /// Relative to `worktree`.
    pub path: PathBuf,
    pub side: DiffSide,
    /// 1-based line number on `side`. The daemon moves it when the text moves.
    pub line: u32,
    /// The text of the line, as in [`DiffLine::text`].
    pub text: String,
    /// The lines just before and after, used to pick between equal lines.
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentState {
    /// Made on Merged and waiting for [`Request::SendComments`].
    Pending,
    /// Sent to the task orchestrator.
    Sent,
    /// The user marked the thread done. Only a thread's first comment is resolved.
    Resolved,
}

/// A review comment by the user. A reply has a `parent` and the parent's anchor.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ReviewComment {
    pub id: CommentId,
    pub task: TaskId,
    pub parent: Option<CommentId>,
    /// The source it was made on. Live comments are sent at once; Merged ones wait.
    pub source: ReviewSource,
    pub anchor: CommentAnchor,
    pub body: String,
    pub state: CommentState,
    /// The worker it was meant for when sent: the agent that owns the file. `None`
    /// before it is sent, or when no worker owns the file and the orchestrator has it.
    pub to: Option<AgentId>,
    /// The anchored line is gone from the file; `anchor.line` is where it last was.
    pub outdated: bool,
    /// Milliseconds since the Unix epoch.
    pub created_at: u64,
    pub sent_at: Option<u64>,
    /// When the message carrying it reached the orchestrator's terminal. `None` while
    /// it waits.
    pub delivered_at: Option<u64>,
}

/// A new comment. `parent` makes it a reply, which takes the parent's anchor.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct NewComment {
    pub task: TaskId,
    pub parent: Option<CommentId>,
    pub source: ReviewSource,
    pub anchor: CommentAnchor,
    pub body: String,
}

/// GitHub setup of a project: the `gh` CLI, its sign-in and the `origin` remote.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct GithubStatus {
    /// The `gh` the daemon uses, `None` when it is not installed.
    pub gh: Option<PathBuf>,
    pub gh_version: Option<String>,
    /// `gh auth status` for github.com succeeded.
    pub signed_in: bool,
    /// `owner/name` when `origin` is on github.com. Pull request features only show
    /// when this is set.
    pub repository: Option<String>,
    /// The command that installs `gh` (Homebrew when it exists, else the official
    /// release into Swarm's own bin folder). The app runs it in a user terminal.
    pub install: Vec<String>,
    /// The command that signs `gh` in to github.com in the browser. The app runs it in a
    /// user terminal.
    pub sign_in: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Waiting,
    Running,
    Passed,
    Failed,
    /// Not run, because an earlier check failed.
    Skipped,
    /// Its terminal was closed or the run was replaced before it ended.
    Stopped,
}

/// One command of the project's `[checks] before_pr`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct LocalCheck {
    pub command: String,
    pub state: CheckState,
    /// The process terminal it runs in. It stays after a failure so its output can be
    /// read.
    pub terminal: Option<TerminalId>,
    pub exit: Option<swarm_term::ExitStatus>,
    pub duration_ms: Option<u64>,
}

/// One run of the local checks in the task worktree, one command after the other.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ChecksRun {
    pub task: TaskId,
    /// HEAD of the task worktree when the run started. A newer commit makes the result
    /// stale.
    pub commit: String,
    /// The task worktree had no uncommitted changes and the same HEAD when the run
    /// started and when it ended, so the checks tested exactly `commit`. Only a clean
    /// run can let a pull request through.
    pub clean: bool,
    /// `[checks] required`: a pull request needs every check passed on its commit.
    pub required: bool,
    pub checks: Vec<LocalCheck>,
    /// The pre-push hook git runs on push, by its tool when known ("husky",
    /// "lefthook", "pre-push"). Shown only; Swarm does not run it itself.
    pub pre_push_hook: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PrDraft {
    pub title: String,
    pub body: String,
}

/// A pull request made from a task branch.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PrLink {
    pub number: u64,
    pub url: String,
    /// The commit Swarm last pushed to it. Merging only merges this commit.
    pub head: String,
}

/// What the pull request panel needs from the daemon for one task.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PrPanel {
    /// `[checks] before_pr` as configured now. Empty means no local checks.
    pub commands: Vec<String>,
    /// `[checks] required`.
    pub required: bool,
    /// The pre-push hook git runs on push, as in [`ChecksRun::pre_push_hook`].
    pub pre_push_hook: Option<String>,
    /// The latest run, if any. It does not survive a daemon restart.
    pub run: Option<ChecksRun>,
    /// The latest draft from the orchestrator, if any.
    pub draft: Option<PrDraft>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMethod {
    Squash,
    Merge,
    Rebase,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum GithubCheckState {
    Queued,
    Running,
    Passed,
    Failed,
    Skipped,
    Cancelled,
}

/// One check run or status on the pull request's head commit, as GitHub reports it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct GithubCheck {
    pub name: String,
    pub state: GithubCheckState,
    pub duration_secs: Option<u32>,
    pub url: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    Requested,
    Approved,
    ChangesRequested,
    Commented,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Reviewer {
    /// The GitHub login, without `@`.
    pub login: String,
    pub state: ReviewState,
}

/// The pull request as GitHub reports it now.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
    pub title: String,
    pub state: PrState,
    pub draft: bool,
    pub checks: Vec<GithubCheck>,
    pub reviewers: Vec<Reviewer>,
    /// GitHub would merge it now: no conflicts, required checks and reviews passed.
    pub mergeable: bool,
    /// Milliseconds since the Unix epoch.
    pub read_at: u64,
}

/// One limit of a subscription, like Claude's 5-hour window.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct UsageWindow {
    /// Short, like `5h`, `7d` or `30d`.
    pub label: String,
    /// 0 to 100, rounded.
    pub used_percent: u8,
    /// Unix seconds.
    pub resets_at: Option<u64>,
}

/// Why the last usage read failed.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum UsageProblem {
    /// The CLI is not installed.
    NotInstalled,
    NotLoggedIn,
    /// The account refused the stored login.
    Unauthorized,
    /// Asked again too soon; the next read is at `retry_at` (Unix seconds).
    RateLimited {
        retry_at: u64,
    },
    Failed(String),
}

/// The usage of one CLI's subscription. A failed read keeps the last good windows.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct SubscriptionUsage {
    /// Shortest window first.
    pub windows: Vec<UsageWindow>,
    /// The plan name of the subscription, like `max` or `plus`.
    pub plan: Option<String>,
    /// When `windows` was read, in Unix seconds. `None` before the first read.
    pub read_at: Option<u64>,
    pub problem: Option<UsageProblem>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Usage {
    pub claude: SubscriptionUsage,
    pub codex: SubscriptionUsage,
}

/// What closing a task would throw away, so the app can ask first.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct CloseCheck {
    /// Agents of the task whose process runs.
    pub running_agents: u32,
    /// The user's own terminals in the task. Closing stops them.
    pub user_terminals: u32,
    /// Files with changes that are not committed, in the task worktree and in workers'
    /// own worktrees. Closing deletes them.
    pub uncommitted_files: u32,
    /// Names of workers with their own worktree whose commits are not merged into the
    /// task branch. Their branches stay.
    pub unmerged_workers: Vec<String>,
    /// The task branch and workers' own branches that are merged into the base branch.
    /// Closing deletes them; every other branch stays.
    pub merged_branches: Vec<String>,
}

/// Matches a reply to its request, so a late reply never answers a newer call.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallId(pub u32);

/// Client → daemon. `Hello` must stay the first variant with the same fields in every
/// version: it is the only message both sides can read after a version change.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Hello {
        version: u32,
    },
    List {
        call: CallId,
    },
    /// Answered with [`Event::Created`]; the client is attached to the new terminal.
    Create {
        call: CallId,
        terminal: CreateTerminal,
    },
    /// Answered with a snapshot holding up to `history` scrollback lines, then the
    /// output stream until [`Request::Detach`] or disconnect.
    Attach {
        id: TerminalId,
        history: u32,
    },
    Detach {
        id: TerminalId,
    },
    Input {
        id: TerminalId,
        input: TerminalInput,
    },
    /// Answered with [`Event::Projects`].
    ListProjects {
        call: CallId,
    },
    /// `path` may be any folder inside a git repository. Answered with
    /// [`Event::ProjectAdded`]; adding a project twice returns the existing one.
    AddProject {
        call: CallId,
        path: PathBuf,
    },
    /// Creates the task branch and task worktree, copies files, reserves ports and
    /// starts the setup script in a process terminal. Answered with [`Event::TaskCreated`].
    CreateTask {
        call: CallId,
        task: CreateTask,
    },
    /// Starts the project's Start orchestrator in the project root, or returns the
    /// one that runs. Answered with [`Event::AgentStarted`].
    StartOrchestrator {
        call: CallId,
        project: ProjectId,
        /// `None` uses the default from the settings.
        kind: Option<AgentKind>,
    },
    /// The newest messages of a task, or of a project's Start orchestrator when `task`
    /// is `None`. Answered with [`Event::Messages`].
    ListMessages {
        call: CallId,
        project: ProjectId,
        task: Option<TaskId>,
    },
    /// Answered with [`Event::AgentChanges`].
    AgentChanges {
        call: CallId,
        agents: Vec<AgentId>,
    },
    /// Answered with [`Event::TaskDiff`]. With `known` set to the fingerprint the
    /// client already has, an unchanged diff is answered with `diff: None`.
    TaskDiff {
        call: CallId,
        task: TaskId,
        source: ReviewSource,
        known: Option<u64>,
    },
    /// The lines of one file of the diff, folded or not, under the same caps as a
    /// [`TaskDiff`]. Answered with [`Event::FileHunks`].
    FileHunks {
        call: CallId,
        task: TaskId,
        source: ReviewSource,
        worktree: PathBuf,
        path: PathBuf,
    },
    /// Approves the Merged diff with this fingerprint. Refused when the Merged diff
    /// changed since, or while comments of the task are pending. Answered with [`Event::TaskChanged`] to every client, then
    /// [`Event::Done`].
    ApproveTask {
        call: CallId,
        task: TaskId,
        fingerprint: u64,
    },
    /// Answered with [`Event::CloseCheck`].
    CheckClose {
        call: CallId,
        task: TaskId,
    },
    /// The last known subscription usage. Answered with [`Event::Usage`].
    Usage {
        call: CallId,
    },
    /// The task's comments, oldest first, with each anchor checked against the files
    /// as they are now. Answered with [`Event::Comments`].
    ListComments {
        call: CallId,
        task: TaskId,
    },
    /// Adds a comment. On Live it goes to the task orchestrator at once as a `Comment`
    /// message; on Merged it waits as `Pending`. A reply follows its parent: it waits
    /// while the parent waits, else it is sent at once. Answered with
    /// [`Event::CommentChanged`] to every client, then [`Event::Done`].
    AddComment {
        call: CallId,
        comment: NewComment,
    },
    /// Deletes a `Pending` comment and its replies. Answered with
    /// [`Event::CommentDeleted`] to every client, then [`Event::Done`].
    DeleteComment {
        call: CallId,
        id: CommentId,
    },
    /// Marks a sent thread resolved, or sent again. Answered with
    /// [`Event::CommentChanged`] to every client, then [`Event::Done`].
    ResolveComment {
        call: CallId,
        id: CommentId,
        resolved: bool,
    },
    /// Sends every `Pending` comment of the task to its orchestrator in `Comment`
    /// messages of at most 64 KiB each. Refused when none waits, when the orchestrator
    /// is stopped, or when the task has no orchestrator. Answered with a [`Event::CommentChanged`] per
    /// comment to every client, then [`Event::Done`].
    SendComments {
        call: CallId,
        task: TaskId,
    },
    /// Answered with [`Event::GithubStatus`].
    GithubStatus {
        call: CallId,
        project: ProjectId,
    },
    /// The project's local branches, newest commit first. Answered with
    /// [`Event::Branches`].
    ListBranches {
        call: CallId,
        project: ProjectId,
    },
    /// Starts the project's local checks in the task worktree, one process terminal per
    /// command, replacing a run that is still going. Answered with [`Event::Done`];
    /// progress arrives as [`Event::ChecksChanged`].
    RunChecks {
        call: CallId,
        task: TaskId,
    },
    /// The configured checks, the latest run and the latest draft. Answered with
    /// [`Event::PrPanel`].
    PrPanel {
        call: CallId,
        task: TaskId,
    },
    /// Asks the task orchestrator for a pull request draft. Refused unless the task is
    /// approved and its orchestrator can take messages. Answered with [`Event::Done`];
    /// the draft arrives as [`Event::PrDraftChanged`].
    DraftPr {
        call: CallId,
        task: TaskId,
    },
    /// Reads HEAD of the task worktree once, then pushes exactly that commit to the task
    /// branch and opens the pull request into [`TaskInfo::pr_base`] with `gh`. Refused
    /// unless HEAD is the task branch, the Merged diff at that commit is approved, no
    /// comment is pending, and a clean checks run passed on that commit (when
    /// `[checks] required` is false, `skip_checks` allows failed or missing checks). An
    /// open pull request from the branch that Swarm did not record is taken over
    /// instead. An error carries the push or `gh` output. Answered with a `TaskChanged`
    /// to every client, then [`Event::Done`].
    CreatePr {
        call: CallId,
        task: TaskId,
        draft: PrDraft,
        as_draft: bool,
        close_after_merge: bool,
        skip_checks: bool,
    },
    /// Pushes new commits to the task's open pull request, under the same rules as
    /// [`Request::CreatePr`]. Answered with a `TaskChanged` to every client, then
    /// [`Event::Done`].
    UpdatePr {
        call: CallId,
        task: TaskId,
        skip_checks: bool,
    },
    /// Answered with [`Event::PrStatus`].
    PrStatus {
        call: CallId,
        task: TaskId,
    },
    /// Marks a draft pull request ready for review. Answered with [`Event::Done`].
    MarkPrReady {
        call: CallId,
        task: TaskId,
    },
    /// Asks these GitHub logins for a review. Answered with [`Event::Done`].
    AddReviewers {
        call: CallId,
        task: TaskId,
        logins: Vec<String>,
    },
    /// Merges the pull request on GitHub, but only when its head is still
    /// [`PrLink::head`], confirms the merge, and deletes the remote branch. With
    /// `close_after_merge`, the task then closes only when nothing would be lost: no
    /// uncommitted work and no local commit that is not in the pull request. A merge
    /// queue leaves the pull request open. Answered with a `TaskChanged` to every
    /// client, then [`Event::Done`].
    MergePr {
        call: CallId,
        task: TaskId,
        method: MergeMethod,
    },
    /// Answered with a `TaskChanged` to every client, then [`Event::Done`].
    SetCloseAfterMerge {
        call: CallId,
        task: TaskId,
        close: bool,
    },
    /// Stops the task's agents and all its terminals, runs the teardown script, and
    /// removes the task worktree and workers' own worktrees, uncommitted changes
    /// included. Branches merged into the base branch are deleted; the others stay. Once the agents stopped, the daemon checks again and
    /// refuses when closing would lose more than `confirmed`, the check the user saw.
    /// Answered with a `TaskChanged` with `closed: true` to every client, then
    /// [`Event::Done`].
    CloseTask {
        call: CallId,
        task: TaskId,
        confirmed: CloseCheck,
    },
}

/// Daemon → client. `Hello` stays first, as in [`Request`].
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Hello {
        version: u32,
        pid: u32,
    },
    Terminals {
        call: CallId,
        terminals: Vec<TerminalInfo>,
    },
    Created {
        call: CallId,
        id: TerminalId,
    },
    Output {
        id: TerminalId,
        output: TerminalOutput,
    },
    /// The terminal ended and the daemon forgot it; no more output follows.
    Closed {
        id: TerminalId,
    },
    /// `call` is set when the error answers a request, `id` when it is about a terminal.
    Error {
        call: Option<CallId>,
        id: Option<TerminalId>,
        message: String,
    },
    Projects {
        call: CallId,
        projects: Vec<ProjectInfo>,
    },
    ProjectAdded {
        call: CallId,
        project: ProjectInfo,
    },
    TaskCreated {
        call: CallId,
        task: TaskInfo,
    },
    /// Sent to every client whenever an agent is added, starts, ends or changes status.
    AgentChanged {
        agent: AgentInfo,
    },
    AgentStarted {
        call: CallId,
        agent: AgentInfo,
    },
    /// Sent to every client when a task is added (also by an agent) or changes.
    TaskChanged {
        task: TaskInfo,
    },
    /// Sent to every client when an agent starts a process terminal or resizes it.
    TerminalChanged {
        terminal: TerminalInfo,
    },
    /// Sent to every client for each message between agents.
    MessageSent {
        message: MessageInfo,
    },
    /// Up to [`MESSAGES_LISTED`] messages, oldest first.
    Messages {
        call: CallId,
        messages: Vec<MessageInfo>,
    },
    /// Sent to every client when recipients got messages.
    MessagesDelivered {
        ids: Vec<MessageId>,
        delivered_at: u64,
    },
    /// One entry per asked agent whose worktree could be read.
    AgentChanges {
        call: CallId,
        changes: Vec<AgentChange>,
    },
    /// `None` when the diff still has the fingerprint the request named.
    TaskDiff {
        call: CallId,
        diff: Option<TaskDiff>,
    },
    /// `None` when the diff no longer holds the file.
    FileHunks {
        call: CallId,
        file: Option<FileDiff>,
    },
    CloseCheck {
        call: CallId,
        check: CloseCheck,
    },
    /// A request that has no other answer succeeded.
    Done {
        call: CallId,
    },
    Comments {
        call: CallId,
        comments: Vec<ReviewComment>,
    },
    /// Sent to every client when a comment is added, sent or resolved.
    CommentChanged {
        comment: ReviewComment,
    },
    /// Sent to every client when a comment is deleted.
    CommentDeleted {
        task: TaskId,
        id: CommentId,
    },
    Usage {
        call: CallId,
        usage: Usage,
    },
    /// Sent to every client when the usage of a subscription changed.
    UsageChanged {
        usage: Usage,
    },
    GithubStatus {
        call: CallId,
        status: GithubStatus,
    },
    Branches {
        call: CallId,
        branches: Vec<String>,
    },
    PrPanel {
        call: CallId,
        panel: PrPanel,
    },
    /// Sent to every client whenever a check of a run starts or ends.
    ChecksChanged {
        run: ChecksRun,
    },
    /// Sent to every client when the task orchestrator sends a pull request draft.
    PrDraftChanged {
        task: TaskId,
        draft: PrDraft,
    },
    PrStatus {
        call: CallId,
        pull_request: PullRequest,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("socket error: {0}")]
    Io(#[from] io::Error),
    #[error("cannot encode or decode a frame: {0}")]
    Codec(#[from] postcard::Error),
    #[error("frame of {0} bytes is over the limit")]
    TooLarge(usize),
}

/// Encodes one length-prefixed frame, ready to write to any number of clients.
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, FrameError> {
    let mut frame = postcard::to_extend(message, vec![0; 4])?;
    let length = frame.len() - 4;
    let prefix = u32::try_from(length).map_err(|_| FrameError::TooLarge(length))?;

    frame[..4].copy_from_slice(&prefix.to_le_bytes());

    Ok(frame)
}

pub fn write<T: Serialize>(writer: &mut impl Write, message: &T) -> Result<(), FrameError> {
    let frame = encode(message)?;
    writer.write_all(&frame)?;

    Ok(())
}

/// Reads one frame into `buffer` and decodes it. `buffer` is reused between calls.
pub fn read<T: DeserializeOwned>(
    reader: &mut impl Read,
    buffer: &mut Vec<u8>,
) -> Result<T, FrameError> {
    let mut prefix = [0; 4];
    reader.read_exact(&mut prefix)?;

    let length = u32::from_le_bytes(prefix) as usize;
    if length > MAX_FRAME {
        return Err(FrameError::TooLarge(length));
    }

    buffer.resize(length, 0);
    reader.read_exact(buffer)?;

    Ok(postcard::from_bytes(buffer)?)
}

/// Where the daemon keeps its socket, database and log.
#[derive(Debug, Clone)]
pub struct Paths {
    pub data: PathBuf,
    pub socket: PathBuf,
    pub database: PathBuf,
    pub log: PathBuf,
}

impl Paths {
    /// `~/Library/Application Support/Swarm`.
    pub fn new() -> io::Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;

        Ok(Self::in_dir(home.join("Library/Application Support/Swarm")))
    }

    /// Everything inside `data`. The socket moves to the user's private temp folder
    /// when `data` makes it too long for macOS.
    pub fn in_dir(data: PathBuf) -> Self {
        let socket = data.join("daemon.sock");
        let socket = if socket.as_os_str().len() < MAX_SOCKET_PATH {
            socket
        } else {
            user_temp_dir().join("swarm").join("daemon.sock")
        };

        Self {
            database: data.join("swarm.db"),
            log: data.join("daemon.log"),
            socket,
            data,
        }
    }
}

/// The per-user temp folder macOS creates with mode 0700, unlike `/tmp`.
fn user_temp_dir() -> PathBuf {
    let mut buffer = vec![0_u8; 1024];

    // SAFETY: the buffer is valid for its full length, which is passed as the size.
    let length = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
        )
    };

    if length == 0 || length > buffer.len() {
        return std::env::temp_dir();
    }

    buffer.truncate(length - 1);
    PathBuf::from(std::ffi::OsString::from_vec(buffer))
}

/// Refuses a folder that another user owns or others can open, so nobody else can
/// plant a socket there. Both the daemon and the client check the socket's folder.
pub fn check_private_dir(path: &Path) -> io::Result<()> {
    let metadata = std::fs::metadata(path)?;

    // SAFETY: getuid has no preconditions and cannot fail.
    let user = unsafe { libc::getuid() };

    if metadata.uid() != user || metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} must be owned by you with mode 0700", path.display()),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_round_trips() {
        let message = Request::Attach {
            id: TerminalId(7),
            history: 1_000,
        };

        let frame = encode(&message).unwrap();
        let back: Request = read(&mut frame.as_slice(), &mut Vec::new()).unwrap();

        assert_eq!(back, message);
    }

    #[test]
    fn a_huge_length_prefix_is_refused() {
        let frame = u32::MAX.to_le_bytes();

        let result: Result<Request, _> = read(&mut frame.as_slice(), &mut Vec::new());

        assert!(matches!(result, Err(FrameError::TooLarge(_))));
    }
}
