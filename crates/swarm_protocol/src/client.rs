//! The app's connection to the daemon: one reader thread, one writer thread, and one
//! forwarder thread per attached terminal. There is no GPUI here.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufReader, BufWriter, Write};
use std::os::unix::fs::DirBuilderExt as _;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use swarm_term::{TerminalChannels, TerminalInput, TerminalOutput};

use crate::{
    AgentChange, AgentId, AgentInfo, AgentKind, CallId, ChecksRun, CloseCheck, CommentId,
    CreateTask, CreateTerminal, Event, FrameError, GH_SECS, GithubStatus, MergeMethod, MessageId,
    MessageInfo, NewComment, PUSH_SECS, Paths, PrDraft, PrPanel, ProjectId, ProjectInfo,
    PullRequest, Request, ReviewComment, ReviewSource, TaskDiff, TaskId, TaskInfo, TerminalId,
    TerminalInfo, VERSION, check_private_dir, read, write,
};

/// Local checks moved on, or the orchestrator sent a pull request draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrEvent {
    Checks(Box<ChecksRun>),
    Draft { task: TaskId, draft: PrDraft },
}

/// A review comment was added, changed or deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentEvent {
    Changed(Box<ReviewComment>),
    Deleted { task: TaskId, id: CommentId },
}

/// A message was sent or delivered, in the order the daemon sent the events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageEvent {
    Sent(MessageInfo),
    Delivered {
        ids: Vec<MessageId>,
        delivered_at: u64,
    },
}

const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// Creating a task runs git and copies files.
const CREATE_TASK_TIMEOUT: Duration = Duration::from_secs(60);
const SPAWN_TIMEOUT: Duration = Duration::from_secs(3);
/// Diffs, approvals and close checks run git over the whole task.
const GIT_TIMEOUT: Duration = Duration::from_secs(60);
/// Closing waits for agents to stop, then runs the teardown script (60 s at most).
const CLOSE_TASK_TIMEOUT: Duration = Duration::from_secs(180);
/// Creating or updating a pull request pushes first, then calls `gh` a few times.
const CREATE_PR_TIMEOUT: Duration = Duration::from_secs(PUSH_SECS + 4 * GH_SECS + 30);
/// Merging calls `gh` a few times and may then close the task.
const MERGE_PR_TIMEOUT: Duration = Duration::from_secs(180 + 4 * GH_SECS + 30);
const SPAWN_POLL: Duration = Duration::from_millis(50);

/// The only variables the daemon inherits from the app.
const DAEMON_ENV: [&str; 7] = [
    "HOME", "USER", "LOGNAME", "PATH", "LANG", "TMPDIR", "RUST_LOG",
];

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("socket error: {0}")]
    Io(#[from] io::Error),
    #[error("protocol error: {0}")]
    Frame(#[from] FrameError),
    #[error(
        "the daemon speaks protocol version {daemon}, this app speaks {VERSION}; stop it with `pkill -x swarm-daemon`"
    )]
    Version { daemon: u32 },
    #[error("the daemon speaks an older protocol; stop it with `pkill -x swarm-daemon`")]
    Incompatible,
    #[error("the socket folder {} is not private: {source}", dir.display())]
    UnsafeSocketDir { dir: PathBuf, source: io::Error },
    #[error("the daemon did not start, see {}", log.display())]
    DaemonDidNotStart { log: PathBuf },
    #[error("the daemon did not answer in time")]
    Timeout,
    #[error("the daemon connection is closed")]
    Disconnected,
    #[error("the daemon refused the request: {0}")]
    Daemon(String),
}

/// Where a terminal's output goes. `generation` tells a re-attach from the old attach.
struct Route {
    output: flume::Sender<TerminalOutput>,
    generation: u64,
}

/// A call that waits for its reply.
struct Waiter {
    reply: flume::Sender<Event>,
    /// The output route of a `Create`. The reader installs it the moment `Created`
    /// arrives, so no output that follows is lost.
    route: Option<Route>,
}

struct Shared {
    routes: Mutex<HashMap<TerminalId, Route>>,
    waiters: Mutex<HashMap<CallId, Waiter>>,
    next_call: AtomicU32,
    connected: AtomicBool,
    /// Set when this side hangs up, so the reader's end of stream is not a warning.
    closing: AtomicBool,
    next_generation: AtomicU64,
    agents: flume::Sender<AgentInfo>,
    tasks: flume::Sender<TaskInfo>,
    terminals: flume::Sender<TerminalInfo>,
    messages: flume::Sender<MessageEvent>,
    comments: flume::Sender<CommentEvent>,
    pull_requests: flume::Sender<PrEvent>,
    /// Terminals whose size an agent owns: their `Resize` input is dropped.
    size_locked: Mutex<HashSet<TerminalId>>,
}

impl Shared {
    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn route(&self, output: flume::Sender<TerminalOutput>) -> Route {
        Route {
            output,
            generation: self.next_generation.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// Returns whether the route of this attach was still there.
    fn remove_route(&self, id: TerminalId, generation: u64) -> bool {
        let mut routes = Self::lock(&self.routes);

        if routes
            .get(&id)
            .is_some_and(|route| route.generation == generation)
        {
            routes.remove(&id);
            true
        } else {
            false
        }
    }

    fn disconnect(&self) {
        self.connected.store(false, Ordering::Release);
        Self::lock(&self.routes).clear();
        Self::lock(&self.waiters).clear();
    }
}

struct Inner {
    stream: UnixStream,
    writer: flume::Sender<Request>,
    shared: Arc<Shared>,
    daemon_pid: u32,
    agent_events: flume::Receiver<AgentInfo>,
    task_events: flume::Receiver<TaskInfo>,
    terminal_events: flume::Receiver<TerminalInfo>,
    message_events: flume::Receiver<MessageEvent>,
    comment_events: flume::Receiver<CommentEvent>,
    pr_events: flume::Receiver<PrEvent>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.shared.closing.store(true, Ordering::Release);

        if let Err(error) = self.stream.shutdown(std::net::Shutdown::Both) {
            log::debug!("daemon socket was already closed: {error}");
        }
    }
}

/// A handle to the daemon connection. Clones share the connection.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
    reply_timeout: Duration,
}

impl Client {
    /// Connects to the daemon, starting `daemon_binary` first when nothing listens.
    pub fn connect_or_spawn(paths: &Paths, daemon_binary: &Path) -> Result<Self, ClientError> {
        if let Some(folder) = paths.socket.parent() {
            create_private_dir(folder)?;
            check_socket_dir(folder)?;
        }

        match UnixStream::connect(&paths.socket) {
            Ok(stream) => return Self::from_stream(stream),
            Err(error) => log::info!("no daemon on {}: {error}", paths.socket.display()),
        }

        spawn_daemon(paths, daemon_binary)?;

        let stream =
            wait_for_socket(&paths.socket).ok_or_else(|| ClientError::DaemonDidNotStart {
                log: paths.log.clone(),
            })?;

        Self::from_stream(stream)
    }

    /// Connects only; never starts a daemon.
    pub fn connect(socket: &Path) -> Result<Self, ClientError> {
        if let Some(folder) = socket.parent() {
            check_socket_dir(folder)?;
        }

        Self::from_stream(UnixStream::connect(socket)?)
    }

    fn from_stream(stream: UnixStream) -> Result<Self, ClientError> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut writer = BufWriter::new(stream.try_clone()?);

        stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
        write(&mut writer, &Request::Hello { version: VERSION })?;
        writer.flush()?;

        let mut buffer = Vec::new();
        let hello: Event = read(&mut reader, &mut buffer).map_err(|error| match error {
            FrameError::Io(io_error)
                if matches!(
                    io_error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                ClientError::Timeout
            }
            FrameError::Codec(_) => ClientError::Incompatible,
            other => ClientError::Frame(other),
        })?;

        let daemon_pid = match hello {
            Event::Hello { version, pid } if version == VERSION => {
                stream.set_read_timeout(None)?;
                pid
            }
            Event::Hello { version, .. } => return Err(ClientError::Version { daemon: version }),
            Event::Error { message, .. } => return Err(ClientError::Daemon(message)),
            other => {
                return Err(ClientError::Daemon(format!(
                    "unexpected reply to Hello: {other:?}"
                )));
            }
        };

        let (agent_sender, agent_receiver) = flume::unbounded();
        let (task_sender, task_receiver) = flume::unbounded();
        let (terminal_sender, terminal_receiver) = flume::unbounded();
        let (message_sender, message_receiver) = flume::unbounded();
        let (comment_sender, comment_receiver) = flume::unbounded();
        let (pr_sender, pr_receiver) = flume::unbounded();
        let shared = Arc::new(Shared {
            routes: Mutex::new(HashMap::new()),
            waiters: Mutex::new(HashMap::new()),
            next_call: AtomicU32::new(0),
            connected: AtomicBool::new(true),
            closing: AtomicBool::new(false),
            next_generation: AtomicU64::new(0),
            agents: agent_sender,
            tasks: task_sender,
            terminals: terminal_sender,
            messages: message_sender,
            comments: comment_sender,
            pull_requests: pr_sender,
            size_locked: Mutex::new(HashSet::new()),
        });
        let (request_sender, request_receiver) = flume::unbounded();

        let reader_shared = Arc::clone(&shared);
        let reader_requests = request_sender.clone();
        thread::Builder::new()
            .name("daemon-reader".into())
            .spawn(move || {
                read_events(&mut reader, &mut buffer, &reader_shared, &reader_requests);
            })?;

        let writer_shared = Arc::clone(&shared);
        let writer_stream = stream.try_clone()?;
        thread::Builder::new()
            .name("daemon-writer".into())
            .spawn(move || {
                write_requests(
                    &mut writer,
                    &request_receiver,
                    &writer_shared,
                    &writer_stream,
                );
            })?;

        Ok(Self {
            inner: Arc::new(Inner {
                stream,
                writer: request_sender,
                shared,
                daemon_pid,
                agent_events: agent_receiver,
                task_events: task_receiver,
                terminal_events: terminal_receiver,
                message_events: message_receiver,
                comment_events: comment_receiver,
                pr_events: pr_receiver,
            }),
            reply_timeout: REPLY_TIMEOUT,
        })
    }

    pub fn daemon_pid(&self) -> u32 {
        self.inner.daemon_pid
    }

    /// Every `AgentChanged` the daemon sends. Clones of the receiver share the events,
    /// so use one consumer.
    pub fn agent_events(&self) -> flume::Receiver<AgentInfo> {
        self.inner.agent_events.clone()
    }

    /// Locks or unlocks the size of a terminal. While locked, the client drops the
    /// `Resize` input of its views, so the size an agent asked for stays.
    pub fn set_size_locked(&self, id: TerminalId, locked: bool) {
        let mut set = Shared::lock(&self.inner.shared.size_locked);

        if locked {
            set.insert(id);
        } else {
            set.remove(&id);
        }
    }

    /// Every `TaskChanged` the daemon sends: a task was added, also by an agent, or
    /// changed. Use one consumer.
    pub fn task_events(&self) -> flume::Receiver<TaskInfo> {
        self.inner.task_events.clone()
    }

    /// Every `TerminalChanged` the daemon sends: an agent started a process terminal
    /// or resized it. Use one consumer.
    pub fn terminal_events(&self) -> flume::Receiver<TerminalInfo> {
        self.inner.terminal_events.clone()
    }

    /// Every `MessageSent` and `MessagesDelivered` the daemon sends. Use one consumer.
    pub fn message_events(&self) -> flume::Receiver<MessageEvent> {
        self.inner.message_events.clone()
    }

    /// Every `CommentChanged` and `CommentDeleted` the daemon sends. Use one consumer.
    pub fn comment_events(&self) -> flume::Receiver<CommentEvent> {
        self.inner.comment_events.clone()
    }

    /// Every `ChecksChanged` and `PrDraftChanged` the daemon sends. Use one consumer.
    pub fn pr_events(&self) -> flume::Receiver<PrEvent> {
        self.inner.pr_events.clone()
    }

    /// Changes how long calls wait (`create_task` waits at least 60 s) for the daemon.
    #[must_use]
    pub fn with_reply_timeout(mut self, timeout: Duration) -> Self {
        self.reply_timeout = timeout;
        self
    }

    /// False once the daemon connection dropped. There is no automatic reconnect.
    pub fn is_connected(&self) -> bool {
        self.inner.shared.connected.load(Ordering::Acquire)
    }

    pub fn list(&self) -> Result<Vec<TerminalInfo>, ClientError> {
        match self.call(|call| Request::List { call }, None)? {
            Event::Terminals { terminals, .. } => Ok(terminals),
            other => Err(unexpected("List", &other)),
        }
    }

    pub fn list_projects(&self) -> Result<Vec<ProjectInfo>, ClientError> {
        match self.call(|call| Request::ListProjects { call }, None)? {
            Event::Projects { projects, .. } => Ok(projects),
            other => Err(unexpected("ListProjects", &other)),
        }
    }

    /// `path` may be any folder inside a git repository.
    pub fn add_project(&self, path: PathBuf) -> Result<ProjectInfo, ClientError> {
        match self.call(|call| Request::AddProject { call, path }, None)? {
            Event::ProjectAdded { project, .. } => Ok(project),
            other => Err(unexpected("AddProject", &other)),
        }
    }

    pub fn create_task(&self, task: CreateTask) -> Result<TaskInfo, ClientError> {
        let timeout = self.reply_timeout.max(CREATE_TASK_TIMEOUT);
        let reply =
            self.call_with_timeout(|call| Request::CreateTask { call, task }, None, timeout)?;

        match reply {
            Event::TaskCreated { task, .. } => Ok(task),
            other => Err(unexpected("CreateTask", &other)),
        }
    }

    /// Starts the project's Start orchestrator, or returns the one that runs. `kind`
    /// `None` uses the default from the daemon's settings.
    pub fn start_orchestrator(
        &self,
        project: ProjectId,
        kind: Option<AgentKind>,
    ) -> Result<AgentInfo, ClientError> {
        let reply = self.call(
            |call| Request::StartOrchestrator {
                call,
                project,
                kind,
            },
            None,
        )?;

        match reply {
            Event::AgentStarted { agent, .. } => Ok(agent),
            other => Err(unexpected("StartOrchestrator", &other)),
        }
    }

    /// The newest messages of a task, or of the project's Start orchestrator when `task`
    /// is `None`, oldest first.
    pub fn list_messages(
        &self,
        project: ProjectId,
        task: Option<TaskId>,
    ) -> Result<Vec<MessageInfo>, ClientError> {
        let reply = self.call(
            |call| Request::ListMessages {
                call,
                project,
                task,
            },
            None,
        )?;

        match reply {
            Event::Messages { messages, .. } => Ok(messages),
            other => Err(unexpected("ListMessages", &other)),
        }
    }

    /// What each agent changed in its worktree. Agents whose worktree cannot be read
    /// are left out.
    pub fn agent_changes(&self, agents: Vec<AgentId>) -> Result<Vec<AgentChange>, ClientError> {
        match self.call(|call| Request::AgentChanges { call, agents }, None)? {
            Event::AgentChanges { changes, .. } => Ok(changes),
            other => Err(unexpected("AgentChanges", &other)),
        }
    }

    /// The changes of a task. Returns `None` when the diff still has the `known`
    /// fingerprint.
    pub fn task_diff(
        &self,
        task: TaskId,
        source: ReviewSource,
        known: Option<u64>,
    ) -> Result<Option<TaskDiff>, ClientError> {
        let timeout = self.reply_timeout.max(GIT_TIMEOUT);
        let reply = self.call_with_timeout(
            |call| Request::TaskDiff {
                call,
                task,
                source,
                known,
            },
            None,
            timeout,
        )?;

        match reply {
            Event::TaskDiff { diff, .. } => Ok(diff),
            other => Err(unexpected("TaskDiff", &other)),
        }
    }

    /// Approves the Merged diff with this fingerprint. The new task arrives through
    /// [`Client::task_events`].
    pub fn approve_task(&self, task: TaskId, fingerprint: u64) -> Result<(), ClientError> {
        let timeout = self.reply_timeout.max(GIT_TIMEOUT);
        let reply = self.call_with_timeout(
            |call| Request::ApproveTask {
                call,
                task,
                fingerprint,
            },
            None,
            timeout,
        )?;

        match reply {
            Event::Done { .. } => Ok(()),
            other => Err(unexpected("ApproveTask", &other)),
        }
    }

    /// The task's comments, oldest first, with anchors checked against the files now.
    pub fn list_comments(&self, task: TaskId) -> Result<Vec<ReviewComment>, ClientError> {
        let timeout = self.reply_timeout.max(GIT_TIMEOUT);

        match self.call_with_timeout(|call| Request::ListComments { call, task }, None, timeout)? {
            Event::Comments { comments, .. } => Ok(comments),
            other => Err(unexpected("ListComments", &other)),
        }
    }

    /// Adds a comment; it arrives through [`Client::comment_events`].
    pub fn add_comment(&self, comment: NewComment) -> Result<(), ClientError> {
        let timeout = self.reply_timeout.max(GIT_TIMEOUT);

        match self.call_with_timeout(|call| Request::AddComment { call, comment }, None, timeout)? {
            Event::Done { .. } => Ok(()),
            other => Err(unexpected("AddComment", &other)),
        }
    }

    pub fn delete_comment(&self, id: CommentId) -> Result<(), ClientError> {
        match self.call(|call| Request::DeleteComment { call, id }, None)? {
            Event::Done { .. } => Ok(()),
            other => Err(unexpected("DeleteComment", &other)),
        }
    }

    pub fn resolve_comment(&self, id: CommentId, resolved: bool) -> Result<(), ClientError> {
        let request = |call| Request::ResolveComment { call, id, resolved };

        match self.call(request, None)? {
            Event::Done { .. } => Ok(()),
            other => Err(unexpected("ResolveComment", &other)),
        }
    }

    /// Sends the task's pending comments to its orchestrator in one message.
    pub fn request_changes(&self, task: TaskId) -> Result<(), ClientError> {
        let timeout = self.reply_timeout.max(GIT_TIMEOUT);

        match self.call_with_timeout(|call| Request::SendComments { call, task }, None, timeout)? {
            Event::Done { .. } => Ok(()),
            other => Err(unexpected("SendComments", &other)),
        }
    }

    pub fn github_status(&self, project: ProjectId) -> Result<GithubStatus, ClientError> {
        let timeout = self.reply_timeout.max(GIT_TIMEOUT);
        let request = |call| Request::GithubStatus { call, project };

        match self.call_with_timeout(request, None, timeout)? {
            Event::GithubStatus { status, .. } => Ok(status),
            other => Err(unexpected("GithubStatus", &other)),
        }
    }

    /// The project's local branches, newest commit first.
    pub fn list_branches(&self, project: ProjectId) -> Result<Vec<String>, ClientError> {
        let timeout = self.reply_timeout.max(GIT_TIMEOUT);
        let request = |call| Request::ListBranches { call, project };

        match self.call_with_timeout(request, None, timeout)? {
            Event::Branches { branches, .. } => Ok(branches),
            other => Err(unexpected("ListBranches", &other)),
        }
    }

    /// Starts the local checks; progress arrives through [`Client::pr_events`].
    pub fn run_checks(&self, task: TaskId) -> Result<(), ClientError> {
        let timeout = self.reply_timeout.max(GIT_TIMEOUT);

        self.done(
            |call| Request::RunChecks { call, task },
            "RunChecks",
            timeout,
        )
    }

    /// The configured checks, the latest run and the latest draft of a task.
    pub fn pr_panel(&self, task: TaskId) -> Result<PrPanel, ClientError> {
        let timeout = self.reply_timeout.max(GIT_TIMEOUT);

        match self.call_with_timeout(|call| Request::PrPanel { call, task }, None, timeout)? {
            Event::PrPanel { panel, .. } => Ok(panel),
            other => Err(unexpected("PrPanel", &other)),
        }
    }

    /// Asks the orchestrator for a draft; it arrives through [`Client::pr_events`].
    pub fn draft_pr(&self, task: TaskId) -> Result<(), ClientError> {
        self.done(
            |call| Request::DraftPr { call, task },
            "DraftPr",
            self.reply_timeout,
        )
    }

    /// Pushes the task branch and opens the pull request. The task with its pull request
    /// arrives through [`Client::task_events`].
    pub fn create_pr(
        &self,
        task: TaskId,
        draft: PrDraft,
        as_draft: bool,
        close_after_merge: bool,
        skip_checks: bool,
    ) -> Result<(), ClientError> {
        let timeout = self.reply_timeout.max(CREATE_PR_TIMEOUT);
        let request = |call| Request::CreatePr {
            call,
            task,
            draft,
            as_draft,
            close_after_merge,
            skip_checks,
        };

        self.done(request, "CreatePr", timeout)
    }

    /// Pushes new commits to the task's open pull request.
    pub fn update_pr(&self, task: TaskId, skip_checks: bool) -> Result<(), ClientError> {
        let timeout = self.reply_timeout.max(CREATE_PR_TIMEOUT);
        let request = |call| Request::UpdatePr {
            call,
            task,
            skip_checks,
        };

        self.done(request, "UpdatePr", timeout)
    }

    pub fn pr_status(&self, task: TaskId) -> Result<PullRequest, ClientError> {
        let timeout = self.reply_timeout.max(GIT_TIMEOUT);

        match self.call_with_timeout(|call| Request::PrStatus { call, task }, None, timeout)? {
            Event::PrStatus { pull_request, .. } => Ok(pull_request),
            other => Err(unexpected("PrStatus", &other)),
        }
    }

    pub fn mark_pr_ready(&self, task: TaskId) -> Result<(), ClientError> {
        let timeout = self.reply_timeout.max(GIT_TIMEOUT);

        self.done(
            |call| Request::MarkPrReady { call, task },
            "MarkPrReady",
            timeout,
        )
    }

    pub fn request_review(&self, task: TaskId, logins: Vec<String>) -> Result<(), ClientError> {
        let timeout = self.reply_timeout.max(GIT_TIMEOUT);
        let request = |call| Request::AddReviewers { call, task, logins };

        self.done(request, "AddReviewers", timeout)
    }

    /// Merges the pull request; with close after merge, the task may close too.
    pub fn merge_pr(&self, task: TaskId, method: MergeMethod) -> Result<(), ClientError> {
        let timeout = self.reply_timeout.max(MERGE_PR_TIMEOUT);
        let request = |call| Request::MergePr { call, task, method };

        self.done(request, "MergePr", timeout)
    }

    pub fn set_close_after_merge(&self, task: TaskId, close: bool) -> Result<(), ClientError> {
        let request = |call| Request::SetCloseAfterMerge { call, task, close };

        self.done(request, "SetCloseAfterMerge", self.reply_timeout)
    }

    fn done(
        &self,
        build: impl FnOnce(CallId) -> Request,
        name: &str,
        timeout: Duration,
    ) -> Result<(), ClientError> {
        match self.call_with_timeout(build, None, timeout)? {
            Event::Done { .. } => Ok(()),
            other => Err(unexpected(name, &other)),
        }
    }

    /// What closing the task would throw away.
    pub fn check_close(&self, task: TaskId) -> Result<CloseCheck, ClientError> {
        let timeout = self.reply_timeout.max(GIT_TIMEOUT);

        match self.call_with_timeout(|call| Request::CheckClose { call, task }, None, timeout)? {
            Event::CloseCheck { check, .. } => Ok(check),
            other => Err(unexpected("CheckClose", &other)),
        }
    }

    /// Closes the task, refused when it would now lose more than `confirmed`. The
    /// closed task arrives through [`Client::task_events`].
    pub fn close_task(&self, task: TaskId, confirmed: CloseCheck) -> Result<(), ClientError> {
        let timeout = self.reply_timeout.max(CLOSE_TASK_TIMEOUT);
        let request = |call| Request::CloseTask {
            call,
            task,
            confirmed,
        };

        match self.call_with_timeout(request, None, timeout)? {
            Event::Done { .. } => Ok(()),
            other => Err(unexpected("CloseTask", &other)),
        }
    }

    /// Creates a terminal and attaches to it.
    pub fn create(
        &self,
        request: CreateTerminal,
    ) -> Result<(TerminalId, TerminalChannels), ClientError> {
        let (view_channels, emulator_channels) = swarm_term::channels();
        let route = self.inner.shared.route(emulator_channels.output);
        let generation = route.generation;

        let created = self.call(
            |call| Request::Create {
                call,
                terminal: request,
            },
            Some(route),
        )?;
        let id = match created {
            Event::Created { id, .. } => id,
            other => return Err(unexpected("Create", &other)),
        };

        self.forward_input(id, generation, emulator_channels.input);

        Ok((id, view_channels))
    }

    /// Returns at once. The snapshot and the output that follows arrive on the channels.
    /// An error from the daemon later just ends the output.
    pub fn attach(&self, id: TerminalId, history: u32) -> Result<TerminalChannels, ClientError> {
        if !self.is_connected() {
            return Err(ClientError::Disconnected);
        }

        let (view_channels, emulator_channels) = swarm_term::channels();
        let route = self.inner.shared.route(emulator_channels.output);
        let generation = route.generation;

        // Installed before the request goes out, so the snapshot cannot arrive first.
        Shared::lock(&self.inner.shared.routes).insert(id, route);

        if let Err(error) = self.send(Request::Attach { id, history }) {
            self.inner.shared.remove_route(id, generation);
            return Err(error);
        }

        self.forward_input(id, generation, emulator_channels.input);

        Ok(view_channels)
    }

    /// Stops the output of a terminal. The terminal keeps running in the daemon.
    pub fn detach(&self, id: TerminalId) {
        Shared::lock(&self.inner.shared.routes).remove(&id);

        if let Err(error) = self.send(Request::Detach { id }) {
            log::debug!("cannot detach terminal {}: {error}", id.0);
        }
    }

    fn send(&self, request: Request) -> Result<(), ClientError> {
        self.inner
            .writer
            .send(request)
            .map_err(|_| ClientError::Disconnected)
    }

    /// Sends a request with a fresh call id and waits for the reply with that id.
    /// `route` is the output route of a `Create`.
    fn call(
        &self,
        build: impl FnOnce(CallId) -> Request,
        route: Option<Route>,
    ) -> Result<Event, ClientError> {
        self.call_with_timeout(build, route, self.reply_timeout)
    }

    fn call_with_timeout(
        &self,
        build: impl FnOnce(CallId) -> Request,
        route: Option<Route>,
        timeout: Duration,
    ) -> Result<Event, ClientError> {
        let shared = &self.inner.shared;
        let call = CallId(shared.next_call.fetch_add(1, Ordering::Relaxed));
        let (reply_sender, replies) = flume::bounded(1);

        Shared::lock(&shared.waiters).insert(
            call,
            Waiter {
                reply: reply_sender,
                route,
            },
        );

        // Checked after the insert: a disconnect from here on clears the waiter.
        if !self.is_connected() {
            Shared::lock(&shared.waiters).remove(&call);
            return Err(ClientError::Disconnected);
        }

        if let Err(error) = self.send(build(call)) {
            Shared::lock(&shared.waiters).remove(&call);
            return Err(error);
        }

        let event = match replies.recv_timeout(timeout) {
            Ok(event) => event,
            Err(flume::RecvTimeoutError::Disconnected) => return Err(ClientError::Disconnected),
            Err(flume::RecvTimeoutError::Timeout) => {
                // The reader sends under this lock, so a missing waiter means the
                // reply is already in the channel.
                let removed = Shared::lock(&shared.waiters).remove(&call);

                match replies.try_recv() {
                    Ok(event) => event,
                    Err(_) if removed.is_some() => return Err(ClientError::Timeout),
                    Err(_) => return Err(ClientError::Disconnected),
                }
            }
        };

        match event {
            Event::Error { message, .. } => Err(ClientError::Daemon(message)),
            event => Ok(event),
        }
    }

    fn forward_input(
        &self,
        id: TerminalId,
        generation: u64,
        input: flume::Receiver<TerminalInput>,
    ) {
        let writer = self.inner.writer.clone();
        let shared = Arc::clone(&self.inner.shared);

        let spawned = thread::Builder::new()
            .name(format!("daemon-input-{}", id.0))
            .spawn(move || {
                while let Ok(input) = input.recv() {
                    let locked = matches!(input, TerminalInput::Resize(_))
                        && Shared::lock(&shared.size_locked).contains(&id);
                    if locked {
                        continue;
                    }

                    if writer.send(Request::Input { id, input }).is_err() {
                        return;
                    }
                }

                // The view dropped its channels.
                if shared.remove_route(id, generation)
                    && let Err(error) = writer.send(Request::Detach { id })
                {
                    log::debug!("cannot detach terminal {}: {error}", id.0);
                }
            });

        if let Err(error) = spawned {
            log::error!(
                "cannot start the input thread of terminal {}: {error}",
                id.0
            );
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "runs on a background thread, never the UI thread"
)]
fn wait_for_socket(socket: &Path) -> Option<UnixStream> {
    let deadline = Instant::now() + SPAWN_TIMEOUT;

    while Instant::now() < deadline {
        if let Ok(stream) = UnixStream::connect(socket) {
            return Some(stream);
        }

        thread::sleep(SPAWN_POLL);
    }

    None
}

fn unexpected(request: &str, event: &Event) -> ClientError {
    ClientError::Daemon(format!("unexpected reply to {request}: {event:?}"))
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per event reads best in one match"
)]
fn read_events(
    reader: &mut BufReader<UnixStream>,
    buffer: &mut Vec<u8>,
    shared: &Shared,
    requests: &flume::Sender<Request>,
) {
    loop {
        let event: Event = match read(reader, buffer) {
            Ok(event) => event,
            Err(error) if shared.closing.load(Ordering::Acquire) => {
                log::debug!("daemon connection closed by this app: {error}");
                break;
            }
            Err(error) => {
                log::warn!("daemon connection closed: {error}");
                break;
            }
        };

        match event {
            Event::Output { id, output } => {
                let routes = Shared::lock(&shared.routes);
                if let Some(route) = routes.get(&id)
                    && route.output.send(output).is_err()
                {
                    log::debug!("output of terminal {} has no listener", id.0);
                }
            }
            Event::Closed { id } => {
                Shared::lock(&shared.routes).remove(&id);
            }
            Event::Error {
                call: None,
                id,
                message,
            } => {
                log::warn!("daemon error: {message}");

                if let Some(id) = id {
                    Shared::lock(&shared.routes).remove(&id);
                }
            }
            Event::Created { call, id } => deliver_created(shared, requests, call, id),
            Event::Terminals { call, terminals } => {
                deliver(shared, call, Event::Terminals { call, terminals });
            }
            Event::Projects { call, projects } => {
                deliver(shared, call, Event::Projects { call, projects });
            }
            Event::ProjectAdded { call, project } => {
                deliver(shared, call, Event::ProjectAdded { call, project });
            }
            Event::TaskCreated { call, task } => {
                deliver(shared, call, Event::TaskCreated { call, task });
            }
            Event::Error {
                call: Some(call),
                id,
                message,
            } => deliver(
                shared,
                call,
                Event::Error {
                    call: Some(call),
                    id,
                    message,
                },
            ),
            Event::Hello { .. } => log::debug!("ignoring a repeated Hello"),
            Event::AgentChanged { agent } => {
                if shared.agents.send(agent).is_err() {
                    log::debug!("agent event has no listener");
                }
            }
            Event::AgentStarted { call, agent } => {
                deliver(shared, call, Event::AgentStarted { call, agent });
            }
            Event::TaskChanged { task } => {
                if shared.tasks.send(task).is_err() {
                    log::debug!("task event has no listener");
                }
            }
            Event::TerminalChanged { terminal } => {
                if shared.terminals.send(terminal).is_err() {
                    log::debug!("terminal event has no listener");
                }
            }
            Event::MessageSent { message } => {
                if shared.messages.send(MessageEvent::Sent(message)).is_err() {
                    log::debug!("message event has no listener");
                }
            }
            Event::MessagesDelivered { ids, delivered_at } => {
                let event = MessageEvent::Delivered { ids, delivered_at };
                if shared.messages.send(event).is_err() {
                    log::debug!("message event has no listener");
                }
            }
            Event::Messages { call, messages } => {
                deliver(shared, call, Event::Messages { call, messages });
            }
            Event::AgentChanges { call, changes } => {
                deliver(shared, call, Event::AgentChanges { call, changes });
            }
            Event::TaskDiff { call, diff } => {
                deliver(shared, call, Event::TaskDiff { call, diff });
            }
            Event::CloseCheck { call, check } => {
                deliver(shared, call, Event::CloseCheck { call, check });
            }
            Event::Done { call } => deliver(shared, call, Event::Done { call }),
            Event::Comments { call, comments } => {
                deliver(shared, call, Event::Comments { call, comments });
            }
            Event::CommentChanged { comment } => {
                if shared
                    .comments
                    .send(CommentEvent::Changed(Box::new(comment)))
                    .is_err()
                {
                    log::debug!("comment event has no listener");
                }
            }
            Event::GithubStatus { call, status } => {
                deliver(shared, call, Event::GithubStatus { call, status });
            }
            Event::Branches { call, branches } => {
                deliver(shared, call, Event::Branches { call, branches });
            }
            Event::PrPanel { call, panel } => deliver(shared, call, Event::PrPanel { call, panel }),
            Event::PrStatus { call, pull_request } => {
                deliver(shared, call, Event::PrStatus { call, pull_request });
            }
            Event::ChecksChanged { run } => {
                if shared
                    .pull_requests
                    .send(PrEvent::Checks(Box::new(run)))
                    .is_err()
                {
                    log::debug!("checks event has no listener");
                }
            }
            Event::PrDraftChanged { task, draft } => {
                if shared
                    .pull_requests
                    .send(PrEvent::Draft { task, draft })
                    .is_err()
                {
                    log::debug!("draft event has no listener");
                }
            }
            Event::CommentDeleted { task, id } => {
                if shared
                    .comments
                    .send(CommentEvent::Deleted { task, id })
                    .is_err()
                {
                    log::debug!("comment event has no listener");
                }
            }
        }
    }

    shared.disconnect();
}

/// Hands a reply to the call that waits for it. A reply for a call that timed out is
/// dropped.
fn deliver(shared: &Shared, call: CallId, event: Event) {
    let waiter = Shared::lock(&shared.waiters).remove(&call);

    match waiter {
        Some(waiter) => {
            if waiter.reply.send(event).is_err() {
                log::debug!("call {} was dropped before its reply", call.0);
            }
        }
        None => log::debug!("dropping a late daemon reply: {event:?}"),
    }
}

/// Installs the output route before the reply goes out, so the snapshot that follows
/// finds it. A terminal nobody waits for any more is killed.
fn deliver_created(
    shared: &Shared,
    requests: &flume::Sender<Request>,
    call: CallId,
    id: TerminalId,
) {
    // Held until the reply is sent: a timing-out call then sees a consistent state.
    let mut waiters = Shared::lock(&shared.waiters);

    let Some(waiter) = waiters.remove(&call) else {
        drop(waiters);
        log::warn!(
            "terminal {} was created after its call gave up, killing it",
            id.0
        );

        if requests
            .send(Request::Input {
                id,
                input: TerminalInput::Kill,
            })
            .is_err()
        {
            log::debug!("cannot kill terminal {}: the connection is closed", id.0);
        }

        return;
    };

    if let Some(route) = waiter.route {
        Shared::lock(&shared.routes).insert(id, route);
    }

    if waiter.reply.send(Event::Created { call, id }).is_err() {
        Shared::lock(&shared.routes).remove(&id);
    }
}

fn write_requests(
    writer: &mut BufWriter<UnixStream>,
    requests: &flume::Receiver<Request>,
    shared: &Shared,
    stream: &UnixStream,
) {
    while let Ok(first) = requests.recv() {
        let mut result = write(writer, &first);

        for request in requests.try_iter() {
            if result.is_ok() {
                result = write(writer, &request);
            }
        }

        let result = result.and_then(|()| writer.flush().map_err(FrameError::from));

        if let Err(error) = result {
            log::warn!("cannot write to the daemon: {error}");
            break;
        }
    }

    if let Err(error) = stream.shutdown(std::net::Shutdown::Both) {
        log::debug!("daemon socket was already closed: {error}");
    }
    shared.disconnect();
}

fn create_private_dir(path: &Path) -> Result<(), ClientError> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;

    Ok(())
}

fn check_socket_dir(folder: &Path) -> Result<(), ClientError> {
    check_private_dir(folder).map_err(|error| {
        if error.kind() == io::ErrorKind::PermissionDenied {
            ClientError::UnsafeSocketDir {
                dir: folder.to_path_buf(),
                source: error,
            }
        } else {
            ClientError::Io(error)
        }
    })
}

/// Starts the daemon in its own session with a clean environment, so quitting the app
/// does not signal it.
fn spawn_daemon(paths: &Paths, daemon_binary: &Path) -> Result<(), ClientError> {
    create_private_dir(&paths.data)?;

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.log)?;

    let mut command = Command::new(daemon_binary);
    command
        .arg("--data-dir")
        .arg(&paths.data)
        .stdin(Stdio::null())
        .stdout(log_file.try_clone()?)
        .stderr(log_file)
        .env_clear();

    for name in DAEMON_ENV {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }

    // SAFETY: only `setsid`, which is async-signal-safe, runs between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }

            Ok(())
        });
    }

    let mut child = command.spawn()?;
    let pid = child.id();

    log::info!("started the daemon, pid {pid}");

    let reaper = thread::Builder::new()
        .name("daemon-reaper".into())
        .spawn(move || match child.wait() {
            Ok(status) => log::info!("the daemon (pid {pid}) exited: {status}"),
            Err(error) => log::warn!("cannot wait for the daemon (pid {pid}): {error}"),
        });

    if let Err(error) = reaper {
        log::warn!("cannot start the daemon reaper: {error}");
    }

    Ok(())
}
