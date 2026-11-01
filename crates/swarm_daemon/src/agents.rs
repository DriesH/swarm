//! The agents the daemon runs: their rows, tokens, status and first message.
//!
//! The row is written before the process starts (ADR 0005). Every change goes to
//! SQLite and to every connected app client as [`Event::AgentChanged`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use serde_json::Value;
use swarm_core::adapters::{self, LaunchSpec, Session};
use swarm_core::agents::{StatusMerge, exit_failure, is_trust_prompt, new_session_id, new_token};
use swarm_core::git;
use swarm_core::github::Github;
use swarm_core::prompts::{self, Harness, PromptFile};
use swarm_core::store::{NewAgent, NewMessage, Store, unix_millis};
use swarm_protocol::agent::render_inbox;
use swarm_protocol::{
    AgentId, AgentInfo, AgentKind, AgentLifecycle, AgentRole, AgentStatus, CreateTerminal, Event,
    MessageKind, Notice, NoticeKind, ProjectId, TaskId, TaskInfo, TerminalId, TerminalKind,
};
use swarm_settings::Settings;
use swarm_term::{ExitStatus, ProgramState, Row, TerminalInput, TerminalOutput};

use crate::hub::{Clients, Extras, Hub, Terminals};
use crate::messages::Messages;
use crate::processes::exit_text;
use crate::tasks::{TERMINAL_PALETTE, TERMINAL_SIZE};

/// How long the first message waits for a ready signal before it looks at the agent.
const FIRST_MESSAGE_PATIENCE: Duration = Duration::from_secs(60);

/// After `SessionStart`, how long to wait for an OSC 7501 report before going ahead.
const SESSION_START_GRACE: Duration = Duration::from_secs(2);

/// The orchestrator hears at most once in this time that the user typed into a worker.
const USER_INPUT_INTERVAL: Duration = Duration::from_secs(30);

/// Terminal UIs treat an Enter that follows text at once as part of a paste.
const ENTER_DELAY: Duration = Duration::from_millis(150);

/// How often a paste that waits for a prompt to go away looks at the agent.
const BLOCKED_POLL: Duration = Duration::from_millis(200);

/// A line the user began holds messages back until they send it or this long after the
/// last key.
const DRAFT_PATIENCE: Duration = Duration::from_secs(60);

/// Why an agent that the user stopped cannot start more agents.
pub(crate) const STOPPED_BY_USER: &str = "the user stopped you, so nothing new starts";

pub(crate) struct AgentConfig {
    /// `<data>/agents/<id>/` holds each agent's files.
    pub(crate) data_dir: PathBuf,
    /// The agent socket, as `SWARM_SOCKET`.
    pub(crate) socket: PathBuf,
    /// `None` when there is no home folder: the defaults apply.
    pub(crate) settings: Option<PathBuf>,
    /// The `swarm` binary the hooks run.
    pub(crate) swarm: PathBuf,
    /// Where `gh` is: agents get it, and Swarm's own bin folder, on their `PATH`.
    pub(crate) github: Github,
}

/// Where an agent works and what it is called.
pub(crate) struct Place<'a> {
    pub(crate) project: ProjectId,
    /// `None` for a Start orchestrator.
    pub(crate) task: Option<TaskId>,
    pub(crate) name: &'a str,
    pub(crate) worktree: &'a Path,
    pub(crate) branch: &'a str,
    pub(crate) ports: &'a [u16],
}

impl<'a> Place<'a> {
    /// The place of a task's orchestrator: the task worktree and branch.
    pub(crate) fn of_task(task: &'a TaskInfo) -> Self {
        Self {
            project: task.project,
            task: Some(task.id),
            name: "orchestrator",
            worktree: &task.worktree,
            branch: &task.branch,
            ports: &[],
        }
    }
}

/// What the settings say about one agent, read when its task is made.
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    kind: AgentKind,
    role: AgentRole,
    model: String,
    command: String,
    skip_permissions: bool,
}

impl Plan {
    /// Uses `model` instead of the one from the settings.
    pub(crate) fn with_model(mut self, model: Option<String>) -> Self {
        if let Some(model) = model {
            self.model = model;
        }

        self
    }
}

/// What it takes to start a registered agent.
pub(crate) struct Start {
    pub(crate) agent: AgentId,
    pub(crate) task_name: String,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) first_message: Option<String>,
}

/// The agent's CLI is not on the path or in the usual folders.
#[derive(Debug)]
pub(crate) struct CommandNotFound(pub(crate) String);

impl std::fmt::Display for CommandNotFound {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "cannot find {}", self.0)
    }
}

impl std::error::Error for CommandNotFound {}

/// The agent id or token did not match a live agent.
#[derive(Debug)]
pub(crate) struct Refused;

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent flags, not one state"
)]
struct Live {
    info: AgentInfo,
    plan: Plan,
    token: String,
    merge: StatusMerge,
    session_id: Option<String>,
    /// A hook carried the session ID, so the conversation exists.
    session_confirmed: bool,
    /// The screen text, kept only while nothing else has reported (trust dialog check).
    screen: Option<ScreenText>,
    ready: Option<flume::Sender<Ready>>,
    /// Unread messages may be pasted: the first message went in, or there is none.
    delivery_open: bool,
    /// The agent asked to end its session: it stops for good instead of resuming.
    stop_requested: bool,
    /// The user stopped it: it can still resume, and the orchestrator is not told.
    stopped_by_user: bool,
    typing: UserTyping,
    /// One paste at a time, so two deliveries never interleave in the terminal.
    paste: Arc<Mutex<()>>,
}

/// Follows what the user types into an agent's terminal.
#[derive(Default)]
struct UserTyping {
    /// Workers only: the user typed since the orchestrator last heard.
    typed: bool,
    told_at: Option<Instant>,
    /// When the user last typed into a line they have not sent yet.
    draft_at: Option<Instant>,
    /// A thread waits for the draft to end and then delivers again.
    retry_pending: bool,
}

fn types_text(key: &swarm_term::Key) -> bool {
    key.text.is_some() && !key.modifiers.control && !key.modifiers.command
}

impl UserTyping {
    /// Follows the line the user writes. Returns true when this input sent a line
    /// they had begun.
    fn track_draft(&mut self, input: &TerminalInput, now: Instant) -> bool {
        const BREAKS: [char; 2] = ['\r', '\n'];

        let (written, sent) = match input {
            // Shift or Alt with Enter adds a line to the draft.
            TerminalInput::Key(key)
                if matches!(key.name.as_str(), "enter" | "return")
                    && !key.modifiers.shift
                    && !key.modifiers.alt =>
            {
                (false, true)
            }
            TerminalInput::Key(key) => (types_text(key), false),
            TerminalInput::Text(text) | TerminalInput::Paste(text) => (
                text.rsplit(BREAKS)
                    .next()
                    .is_some_and(|last| !last.is_empty()),
                text.contains(BREAKS),
            ),
            _ => return false,
        };

        if written {
            self.draft_at = Some(now);

            return false;
        }

        sent && self.draft_at.take().is_some()
    }

    /// How long the draft still holds messages back.
    fn draft_remaining(&self, now: Instant) -> Option<Duration> {
        let age = now.saturating_duration_since(self.draft_at?);

        DRAFT_PATIENCE
            .checked_sub(age)
            .filter(|left| !left.is_zero())
    }

    fn drafting(&self, now: Instant) -> bool {
        self.draft_remaining(now).is_some()
    }

    /// True once per wait: the caller starts the thread that retries.
    fn begin_retry(&mut self) -> bool {
        !std::mem::replace(&mut self.retry_pending, true)
    }

    /// Returns true on the first Enter after typing, unless the orchestrator heard
    /// less than [`USER_INPUT_INTERVAL`] ago.
    fn observe(&mut self, input: &TerminalInput, now: Instant) -> bool {
        let (typed, enter) = match input {
            TerminalInput::Key(key) if matches!(key.name.as_str(), "enter" | "return") => {
                (false, true)
            }
            TerminalInput::Key(key) => (types_text(key), false),
            TerminalInput::Text(text) | TerminalInput::Paste(text) => (
                !text.trim_end_matches(['\r', '\n']).is_empty(),
                text.ends_with(['\r', '\n']),
            ),
            _ => return false,
        };

        self.typed |= typed;
        if !enter || !std::mem::take(&mut self.typed) {
            return false;
        }

        if self
            .told_at
            .is_some_and(|told| now.duration_since(told) < USER_INPUT_INTERVAL)
        {
            return false;
        }
        self.told_at = Some(now);

        true
    }
}

/// What tells the first message that the agent can take input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ready {
    /// An OSC 7501 `idle` report.
    Idle,
    /// The `SessionStart` hook: only a fallback for a program without OSC 7501.
    SessionStart,
}

/// What the first message needs to know when the agent gives no signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Waiting {
    /// The agent exited.
    Gone,
    /// The agent waits for the user, for example on a trust dialog.
    Blocked,
    Other,
}

impl Live {
    /// Whether unread messages may be typed into the agent now: its state allows it and
    /// the user is not in the middle of a line.
    fn takes_messages(&self, now: Instant) -> bool {
        self.accepts_messages() && !self.typing.drafting(now)
    }

    /// Claude Code queues what is typed during a turn. Codex is not known to, so it
    /// waits for idle. A blocked agent never gets text: its prompt would take it.
    fn accepts_messages(&self) -> bool {
        if !self.delivery_open {
            return false;
        }

        match self.merge.status() {
            AgentStatus::Idle | AgentStatus::Done => true,
            AgentStatus::Working => self.info.kind == AgentKind::ClaudeCode,
            AgentStatus::Unknown | AgentStatus::Blocked(_) | AgentStatus::Error => false,
        }
    }

    fn signal_ready(&self, ready: Ready) {
        if let Some(sender) = &self.ready
            && sender.send(ready).is_err()
        {
            log::debug!("agent {} has no first message waiting", self.info.id.0);
        }
    }

    /// Returns true when the screen showed the trust dialog and the status changed.
    fn watch_screen(&mut self, output: &TerminalOutput) -> bool {
        if !self.merge.is_silent() {
            self.screen = None;

            return false;
        }

        let Some(screen) = &mut self.screen else {
            return false;
        };
        screen.apply(output);

        is_trust_prompt(&screen.text()) && self.merge.trust_prompt_visible()
    }
}

pub(crate) struct Agents {
    config: AgentConfig,
    store: Arc<Mutex<Store>>,
    terminals: Arc<Terminals>,
    clients: Arc<Clients>,
    messages: Arc<Messages>,
    live: Mutex<HashMap<AgentId, Live>>,
    /// Told when an agent's process ended.
    on_end: Mutex<Vec<EndObserver>>,
    /// Tasks being closed: nothing new starts in them, and their agents end quietly.
    closing: Mutex<HashSet<TaskId>>,
    /// When each task last told the user it is ready for review. In memory only.
    reviews: Mutex<HashMap<TaskId, ReviewWatch>>,
    /// The daemon is stopping, so agents that end are not broken.
    shutting_down: AtomicBool,
}

/// Follows one task to tell when it is ready for review.
#[derive(Default)]
struct ReviewWatch {
    /// The orchestrator is idle or done.
    resting: bool,
    /// The orchestrator began to rest while it asked the user for a decision.
    asked: bool,
    /// The notice went out and no worker has worked since.
    sent: bool,
}

impl ReviewWatch {
    /// An orchestrator that asked for a decision is not ready: it is asked again the
    /// next time it begins to rest.
    fn orchestrator_changed(&mut self, rests: bool, asking: bool) {
        if rests && !self.resting {
            self.asked = asking;
        }
        if !rests {
            self.asked = false;
        }

        self.resting = rests;
    }

    /// The orchestrator rests and the user has not heard yet, so only a worker can still
    /// hold the notice back.
    fn may_be_ready(&self) -> bool {
        self.resting && !self.asked && !self.sent
    }

    /// A worker works, so the next time the swarm rests is news again.
    fn worker_worked(&mut self) {
        self.sent = false;
    }
}

/// The task has workers and every one of them is done or ended.
fn workers_done(agents: &[AgentInfo]) -> bool {
    let mut workers = agents
        .iter()
        .filter(|agent| agent.role == AgentRole::Worker)
        .peekable();

    workers.peek().is_some()
        && workers.all(|worker| match worker.lifecycle {
            AgentLifecycle::Starting => false,
            AgentLifecycle::Running => !matches!(
                worker.status,
                AgentStatus::Unknown | AgentStatus::Working | AgentStatus::Blocked(_)
            ),
            AgentLifecycle::Resumable | AgentLifecycle::Stopped => true,
        })
}

/// Gets the agent and whether the user stopped it.
type EndObserver = Box<dyn Fn(AgentId, bool) + Send + Sync>;

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Agents {
    pub(crate) fn new(
        config: AgentConfig,
        store: Arc<Mutex<Store>>,
        terminals: Arc<Terminals>,
        clients: Arc<Clients>,
        messages: Arc<Messages>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            store,
            terminals,
            clients,
            messages,
            live: Mutex::default(),
            on_end: Mutex::default(),
            closing: Mutex::default(),
            reviews: Mutex::default(),
            shutting_down: AtomicBool::new(false),
        })
    }

    /// Starts closing the task. False when it is closing already.
    pub(crate) fn begin_close(&self, task: TaskId) -> bool {
        lock(&self.reviews).remove(&task);

        lock(&self.closing).insert(task)
    }

    /// The daemon is about to stop its terminals: no agent that ends then is broken.
    pub(crate) fn begin_shutdown(&self) {
        self.shutting_down
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// The close failed; the task takes new agents again.
    pub(crate) fn abort_close(&self, task: TaskId) {
        lock(&self.closing).remove(&task);
    }

    /// Like [`Self::is_closing`], for the `task` key of a terminal.
    pub(crate) fn is_closing_key(&self, key: Option<&str>) -> bool {
        key.is_some_and(|key| {
            lock(&self.closing)
                .iter()
                .any(|task| task.terminal_key() == key)
        })
    }

    pub(crate) fn is_closing(&self, task: Option<TaskId>) -> bool {
        task.is_some_and(|task| lock(&self.closing).contains(&task))
    }

    /// Marks the live agents of the task as stopped on purpose, so their end counts as
    /// `Stopped` and tells nobody. Returns the terminals of those that run. An agent
    /// that never started ends here.
    pub(crate) fn stop_task_agents(&self, task: TaskId) -> Vec<TerminalId> {
        self.mark_stopped(
            |agent| agent.info.task == Some(task),
            |agent| agent.stop_requested = true,
        )
    }

    /// Stops every live agent as the user's choice: one with a confirmed session ends
    /// `Resumable`, and no orchestrator is told. Returns once the kills are sent.
    pub(crate) fn stop_all(&self) {
        let terminals = self.mark_stopped(|_| true, |agent| agent.stopped_by_user = true);

        for hub in terminals
            .into_iter()
            .filter_map(|terminal| self.terminals.get(terminal))
        {
            hub.send_input(TerminalInput::Kill);
        }
    }

    /// Runs `mark` on the live agents `pick` chooses. Returns the terminals of those
    /// that run. An agent that never started ends here, as `Stopped`.
    fn mark_stopped(
        &self,
        pick: impl Fn(&Live) -> bool,
        mark: impl Fn(&mut Live),
    ) -> Vec<TerminalId> {
        let mut terminals = Vec::new();
        let mut unstarted = Vec::new();

        {
            let mut live = lock(&self.live);
            for agent in live.values_mut().filter(|agent| pick(agent)) {
                mark(agent);
                match agent.info.terminal {
                    Some(terminal) => terminals.push(terminal),
                    None => unstarted.push(agent.info.id),
                }
            }
        }

        for id in unstarted {
            let Some(mut agent) = lock(&self.live).remove(&id) else {
                continue;
            };

            agent.info.lifecycle = AgentLifecycle::Stopped;
            agent.info.ended_at = Some(unix_millis());
            self.publish(&mut agent);
        }

        terminals
    }

    /// How many agents start or run, the same ones the app counts as running.
    pub(crate) fn running_count(&self) -> u32 {
        let running = lock(&self.live)
            .values()
            .filter(|agent| {
                matches!(
                    agent.info.lifecycle,
                    AgentLifecycle::Starting | AgentLifecycle::Running
                )
            })
            .count();

        u32::try_from(running).unwrap_or(u32::MAX)
    }

    /// True while an agent of the task still has a live entry.
    pub(crate) fn has_live(&self, task: TaskId) -> bool {
        lock(&self.live)
            .values()
            .any(|agent| agent.info.task == Some(task))
    }

    /// Calls `observer` for every agent whose process ends.
    pub(crate) fn on_end(&self, observer: EndObserver) {
        lock(&self.on_end).push(observer);
    }

    /// Reads the settings. Call it before anything is created, so a broken settings
    /// file fails the request early.
    pub(crate) fn plan(&self, kind: AgentKind, role: AgentRole) -> Result<Plan> {
        let settings = match &self.config.settings {
            Some(path) => swarm_settings::load(path)?,
            None => Settings::default(),
        };

        let command = settings.agents.cli(kind).command.clone();
        Self::find_command(&command)?;

        Ok(Plan {
            kind,
            role,
            model: settings.agents.model(kind, role).to_owned(),
            command,
            skip_permissions: settings.agents.skip_permissions,
        })
    }

    /// Looks the CLI up as a shell would.
    fn find_command(command: &str) -> Result<PathBuf, CommandNotFound> {
        let home = std::env::var_os("HOME").map(PathBuf::from);

        adapters::find_command(
            command,
            std::env::var_os("PATH").as_deref(),
            home.as_deref(),
        )
        .ok_or_else(|| CommandNotFound(command.to_owned()))
    }

    /// The agent kind the settings pick for orchestrators.
    pub(crate) fn default_kind(&self) -> Result<AgentKind> {
        let settings = match &self.config.settings {
            Some(path) => swarm_settings::load(path)?,
            None => Settings::default(),
        };

        Ok(settings.agents.orchestrator.into())
    }

    /// Writes the agent's row and makes it known to the agent socket. Nothing runs yet.
    pub(crate) fn register(&self, place: &Place<'_>, plan: Plan) -> Result<AgentInfo> {
        self.register_asked_by(None, place, plan)
    }

    /// Like [`Self::register`], for an agent that `asker` asked for, if one did. It
    /// refuses when the user stopped the asker. The check and the new entry are one
    /// step, so a Stop all either stops the new agent or finds the asker marked.
    pub(crate) fn register_asked_by(
        &self,
        asker: Option<AgentId>,
        place: &Place<'_>,
        plan: Plan,
    ) -> Result<AgentInfo> {
        if self.is_closing(place.task) {
            bail!("the task is closing");
        }
        if asker.is_some_and(|asker| self.is_stopped(asker)) {
            bail!(STOPPED_BY_USER);
        }

        let session_id = match plan.kind {
            AgentKind::ClaudeCode => Some(new_session_id().context("cannot make a session ID")?),
            AgentKind::Codex => None,
        };
        let token = new_token().context("cannot make a token")?;

        let info = lock(&self.store).add_agent(&NewAgent {
            project: place.project,
            task: place.task,
            role: plan.role,
            kind: plan.kind,
            model: &plan.model,
            session_id: session_id.as_deref(),
            name: place.name,
            worktree: place.worktree,
            branch: place.branch,
            ports: place.ports,
        })?;

        let mut live = lock(&self.live);
        if asker.is_some_and(|asker| live.get(&asker).is_none_or(|agent| agent.stopped_by_user)) {
            drop(live);
            if let Err(error) = lock(&self.store).delete_agent(info.id) {
                log::error!("cannot delete agent {}: {error}", info.id.0);
            }

            bail!(STOPPED_BY_USER);
        }

        live.insert(
            info.id,
            Live {
                info: info.clone(),
                plan,
                token,
                merge: StatusMerge::default(),
                session_id,
                session_confirmed: false,
                screen: Some(ScreenText::default()),
                ready: None,
                delivery_open: true,
                stop_requested: false,
                stopped_by_user: false,
                typing: UserTyping::default(),
                paste: Arc::default(),
            },
        );

        Ok(info)
    }

    /// True when the user stopped the agent, or its process ended since. A call from
    /// such an agent must not start anything new.
    pub(crate) fn is_stopped(&self, id: AgentId) -> bool {
        lock(&self.live)
            .get(&id)
            .is_none_or(|agent| agent.stopped_by_user)
    }

    /// Drops an agent that never ran, without telling anyone.
    pub(crate) fn forget(&self, id: AgentId) {
        lock(&self.live).remove(&id);
    }

    /// Tells every client about an agent that was just added.
    pub(crate) fn announce(&self, id: AgentId) {
        if let Some(info) = self.info(id) {
            self.clients.broadcast(&Event::AgentChanged { agent: info });
        }
    }

    pub(crate) fn info(&self, id: AgentId) -> Option<AgentInfo> {
        lock(&self.live).get(&id).map(|agent| agent.info.clone())
    }

    /// The live agent that the id and token name.
    pub(crate) fn authenticate(&self, id: AgentId, token: &str) -> Result<AgentInfo, Refused> {
        lock(&self.live)
            .get(&id)
            .filter(|agent| same_token(&agent.token, token))
            .map(|agent| agent.info.clone())
            .ok_or(Refused)
    }

    #[cfg(test)]
    pub(crate) fn token_for_test(&self, id: AgentId) -> Option<String> {
        lock(&self.live).get(&id).map(|agent| agent.token.clone())
    }

    /// Shows the agent as blocked with `text` until it works or the app types into it,
    /// and tells every client the user is needed.
    pub(crate) fn notify(&self, id: AgentId, text: &str) {
        self.update(id, |live| live.merge.notify(text));

        if let Some(info) = self.info(id) {
            self.send_notice(NoticeKind::NeedsDecision, &info, text.to_owned());
        }
    }

    /// The user typed or pasted into a terminal: a notice on the agent in it no longer
    /// applies, and the orchestrator hears when the user typed into a worker (ADR 0002).
    /// Resizes, focus and scrollback requests only show the terminal; they change nothing.
    ///
    /// Returns the agent whose unsent line this input sent. Call
    /// [`Self::deliver_unread`] for it once the input reached the terminal, so the
    /// messages come after the line.
    pub(crate) fn app_input(
        self: &Arc<Self>,
        terminal: TerminalId,
        input: &TerminalInput,
    ) -> Option<AgentId> {
        if !matches!(
            input,
            TerminalInput::Key(_) | TerminalInput::Text(_) | TerminalInput::Paste(_)
        ) {
            return None;
        }

        let (id, role) = lock(&self.live)
            .values()
            .find(|agent| agent.info.terminal == Some(terminal))
            .map(|agent| (agent.info.id, agent.info.role))?;

        self.update(id, |live| live.merge.app_input());

        let (told, sent) = {
            let mut live = lock(&self.live);
            let agent = live.get_mut(&id)?;
            let now = Instant::now();

            let sent = agent.typing.track_draft(input, now);
            let told = role == AgentRole::Worker && agent.typing.observe(input, now);

            (told, sent)
        };
        if told {
            self.user_typed(id);
        }

        sent.then_some(id)
    }

    fn user_typed(self: &Arc<Self>, worker: AgentId) {
        let Some(info) = self.info(worker) else {
            return;
        };
        let Some(task) = info.task else {
            return;
        };

        let orchestrator = lock(&self.store).list_agents(task).map(|agents| {
            agents
                .into_iter()
                .find(|agent| agent.role == AgentRole::Orchestrator)
        });
        let orchestrator = match orchestrator {
            Ok(Some(orchestrator)) => orchestrator,
            Ok(None) => return,
            Err(error) => {
                log::error!("cannot find the orchestrator of task {}: {error}", task.0);

                return;
            }
        };

        let text = format!("The user typed into {}'s terminal.", info.name);
        let posted = self.messages.post(&NewMessage {
            project: info.project,
            task: Some(task),
            from: None,
            to: orchestrator.id,
            kind: MessageKind::UserInput,
            text: &text,
            delivered: false,
        });
        match posted {
            Ok(_) => self.deliver_unread(orchestrator.id),
            Err(error) => log::error!("cannot tell the orchestrator about the user: {error:#}"),
        }
    }

    /// Ends the agent's process for good (`swarm exit`): the lifecycle becomes
    /// `Stopped`, not `Resumable`.
    pub(crate) fn exit(&self, id: AgentId) {
        let terminal = {
            let mut live = lock(&self.live);
            let Some(agent) = live.get_mut(&id) else {
                return;
            };
            agent.stop_requested = true;

            agent.info.terminal
        };

        if let Some(hub) = terminal.and_then(|terminal| self.terminals.get(terminal))
            && !hub.send_input(TerminalInput::Kill)
        {
            log::debug!("agent {} already ended", id.0);
        }
    }

    /// Starts the agent's process in the task worktree.
    pub(crate) fn start(self: &Arc<Self>, start: Start) -> Result<()> {
        let id = start.agent;
        let task = self.info(id).and_then(|info| info.task);
        if self.is_closing(task) {
            bail!("the task is closing");
        }

        let (plan, token, session_id) = {
            let live = lock(&self.live);
            let entry = live.get(&id).context("the agent is not registered")?;

            (
                entry.plan.clone(),
                entry.token.clone(),
                entry.session_id.clone(),
            )
        };

        let command = Self::find_command(&plan.command)?;

        let info = self.info(id).context("the agent is not registered")?;
        let project_root = lock(&self.store)
            .project(info.project)?
            .map(|project| project.path)
            .context("the project is gone")?;

        let prompt = prompts::build(
            PromptFile::for_role(plan.role),
            &Harness {
                agent: id,
                task: info.task,
                task_name: &start.task_name,
                role: plan.role,
                kind: plan.kind,
                skip_permissions: plan.skip_permissions,
                socket: &self.config.socket,
            },
            self.config.settings.as_deref().and_then(Path::parent),
            &project_root,
        );

        let harness_env = harness_env(&info, &self.config.socket, &token);
        let writable_roots = Self::writable_roots(&plan, &info.worktree);

        let agent_dir = self.config.data_dir.join("agents").join(id.0.to_string());
        let launch = adapters::build(&LaunchSpec {
            kind: plan.kind,
            command: &command,
            model: &plan.model,
            prompt: &prompt,
            worktree: &info.worktree,
            skip_permissions: plan.skip_permissions,
            swarm: &self.config.swarm,
            agent_dir: &agent_dir,
            session: Session::New(session_id.as_deref()),
            harness_env: &harness_env,
            writable_roots: &writable_roots,
        });
        write_files(&agent_dir, &launch.files)?;

        let mut env = start.env;
        env.retain(|(name, _)| harness_env.iter().all(|(own, _)| own != name));
        env.extend(harness_env);
        env.push(("PATH".to_owned(), self.agent_path()));

        let ready = start.first_message.as_ref().map(|_| flume::unbounded());
        if let Some((sender, _)) = &ready {
            self.update(id, |live| {
                live.ready = Some(sender.clone());
                live.delivery_open = false;

                false
            });
        }
        let ready = ready.map(|(_, receiver)| receiver);

        let (started, output, exited) = (Arc::clone(self), Arc::clone(self), Arc::clone(self));
        let extras = Extras {
            env,
            on_started: Some(Box::new(move |terminal| {
                started.update(id, |live| {
                    live.info.terminal = Some(terminal);
                    live.info.lifecycle = AgentLifecycle::Running;
                    live.info.ended_at = None;
                    live.info.started_at = Some(unix_millis());

                    true
                });
            })),
            on_exit: Some(Box::new(move |exit| exited.exited(id, exit.as_ref()))),
            on_output: Some(Box::new(move |program_output| {
                output.output(id, program_output);
            })),
            process: None,
        };
        let request = CreateTerminal {
            kind: TerminalKind::Agent,
            command: Some(launch.argv),
            cwd: Some(info.worktree.clone()),
            size: TERMINAL_SIZE,
            palette: TERMINAL_PALETTE,
            task: Some(terminal_key(&info)),
        };
        let hub = self
            .terminals
            .create_with(request, extras)
            .context("cannot start the agent")?;

        if self.ended_while_starting(id, info.task) {
            hub.send_input(TerminalInput::Kill);
        }

        if let (Some(message), Some(receiver)) = (start.first_message, ready) {
            self.type_when_ready(id, hub, message, receiver);
        }

        Ok(())
    }

    /// True when a stop, a close or `swarm exit` ran while the process started. Their
    /// kill found no terminal to send to: the hub is not known until the process runs.
    fn ended_while_starting(&self, id: AgentId, task: Option<TaskId>) -> bool {
        let closing = self.is_closing(task);

        let mut live = lock(&self.live);
        let Some(agent) = live.get_mut(&id) else {
            // A stop ended it as an agent that never started.
            return true;
        };

        if closing {
            agent.stop_requested = true;
        }

        agent.stopped_by_user || agent.stop_requested
    }

    /// `PATH` with the folder of the `swarm` binary first, so agents can run `swarm`. The
    /// folder of `gh` and Swarm's own bin folder come last.
    fn agent_path(&self) -> String {
        let mut folders: Vec<PathBuf> = self
            .config
            .swarm
            .parent()
            .map(Path::to_path_buf)
            .into_iter()
            .collect();
        folders.extend(
            std::env::var_os("PATH")
                .iter()
                .flat_map(std::env::split_paths),
        );

        let github = &self.config.github;
        let gh_folder = github
            .find()
            .and_then(|gh| gh.parent().map(Path::to_path_buf));
        for folder in gh_folder
            .into_iter()
            .chain([github.bin_dir().to_path_buf()])
        {
            if !folders.contains(&folder) {
                folders.push(folder);
            }
        }

        std::env::join_paths(folders).map_or_else(
            |_| std::env::var("PATH").unwrap_or_default(),
            |path| path.to_string_lossy().into_owned(),
        )
    }

    /// The git folders a Codex sandbox must write for `git commit` in a worktree.
    fn writable_roots(plan: &Plan, worktree: &Path) -> Vec<PathBuf> {
        if plan.kind != AgentKind::Codex || plan.skip_permissions {
            return Vec::new();
        }

        git::writable_git_dirs(worktree).unwrap_or_else(|error| {
            log::warn!(
                "cannot find the git folders of {}: {error}",
                worktree.display()
            );

            Vec::new()
        })
    }

    /// Ends an agent that never ran, or never will, and tells every client it broke
    /// unless it ended on purpose.
    pub(crate) fn fail(self: &Arc<Self>, id: AgentId, message: &str) {
        let Some(mut agent) = lock(&self.live).remove(&id) else {
            return;
        };

        self.messages.clear_queued(id);

        agent.merge.fail(message);
        agent.info.lifecycle = AgentLifecycle::Stopped;
        agent.info.ended_at = Some(unix_millis());
        self.publish(&mut agent);

        if !self.is_closing(agent.info.task) {
            self.tell_worker_stopped(&agent.info, &format!("failed: {message}"));
        }

        if !self.is_ending(&agent) {
            let text = format!("Failed to start: {message}");
            self.send_notice(NoticeKind::Broke, &agent.info, text);
        }

        self.follow_review(&agent);
    }

    /// The orchestrator hears that one of its workers failed to start or ended.
    fn tell_worker_stopped(self: &Arc<Self>, worker: &AgentInfo, what: &str) {
        let (AgentRole::Worker, Some(task)) = (worker.role, worker.task) else {
            return;
        };

        let orchestrator = lock(&self.store).list_agents(task).map(|agents| {
            agents
                .into_iter()
                .find(|agent| agent.role == AgentRole::Orchestrator)
        });
        let orchestrator = match orchestrator {
            Ok(Some(orchestrator)) => orchestrator,
            Ok(None) => return,
            Err(error) => {
                log::error!("cannot find the orchestrator of task {}: {error}", task.0);

                return;
            }
        };

        let text = format!("worker {} {what}", worker.name);
        let posted = self.messages.post(&NewMessage {
            project: worker.project,
            task: Some(task),
            from: None,
            to: orchestrator.id,
            kind: MessageKind::WorkerStopped,
            text: &text,
            delivered: false,
        });
        match posted {
            Ok(_) => self.deliver_unread(orchestrator.id),
            Err(error) => log::error!("cannot tell the orchestrator about a worker: {error:#}"),
        }
    }

    /// Applies a hook event from an agent that the socket has already identified.
    pub(crate) fn hook(self: &Arc<Self>, id: AgentId, event: &Value) {
        let mut live = lock(&self.live);
        let Some(agent) = live.get_mut(&id) else {
            return;
        };

        let outcome = agent.merge.hook(event);
        agent.screen = None;
        self.end_queue_if_stopped(agent);

        if let Some(session_id) = outcome.session_id {
            if agent.session_id.as_deref() != Some(session_id.as_str()) {
                if let Err(error) = lock(&self.store).set_agent_session(id, &session_id) {
                    log::error!("cannot save the session of agent {}: {error}", id.0);
                }
                agent.session_id = Some(session_id);
            }

            if !agent.session_confirmed {
                if let Err(error) = lock(&self.store).confirm_agent_session(id) {
                    log::error!("cannot confirm the session of agent {}: {error}", id.0);
                }
                agent.session_confirmed = true;
            }
        }
        if outcome.session_started {
            agent.signal_ready(Ready::SessionStart);
        }
        if outcome.changed {
            self.publish(agent);
        }
        self.send_notices(agent);
        drop(live);

        if outcome.stopped || outcome.changed {
            self.deliver_unread(id);
        }
    }

    /// What the agent is doing, for a first message that got no ready signal.
    fn waiting(&self, id: AgentId) -> Waiting {
        match lock(&self.live).get(&id).map(|agent| agent.merge.status()) {
            None => Waiting::Gone,
            Some(AgentStatus::Blocked(_)) => Waiting::Blocked,
            Some(_) => Waiting::Other,
        }
    }

    /// Types the message as a paste once the agent is ready: startup dialogs would eat
    /// it before. It is never dropped while the agent lives.
    fn type_when_ready(
        self: &Arc<Self>,
        id: AgentId,
        hub: Arc<Hub>,
        message: String,
        ready: flume::Receiver<Ready>,
    ) {
        let agents = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name(format!("first-message-{}", id.0))
            .spawn(move || {
                let waited = wait_until_ready(
                    &ready,
                    || agents.waiting(id),
                    FIRST_MESSAGE_PATIENCE,
                    SESSION_START_GRACE,
                );
                if !waited {
                    log::debug!("agent {} ended before its first message", id.0);

                    return;
                }

                let typed = hub.send_input(TerminalInput::Paste(message));
                sleep_for(ENTER_DELAY);

                if !typed || !hub.send_input(TerminalInput::Text("\r".to_owned())) {
                    log::warn!("agent {} ended before its first message", id.0);
                }

                agents.update(id, |live| {
                    live.delivery_open = true;

                    false
                });
            });

        if let Err(error) = spawned {
            log::error!("cannot start the first message thread: {error}");
        }
    }

    fn output(self: &Arc<Self>, id: AgentId, output: &TerminalOutput) {
        self.update(id, |live| match output {
            TerminalOutput::ProgramStatus(report) => {
                let changed = live.merge.program_status(report);
                live.screen = None;
                if report.state == ProgramState::Idle {
                    live.signal_ready(Ready::Idle);
                }

                changed
            }
            TerminalOutput::Snapshot(_) | TerminalOutput::Update(_) => live.watch_screen(output),
            _ => false,
        });

        if matches!(
            output,
            TerminalOutput::ProgramStatus(report)
                if matches!(report.state, ProgramState::Idle | ProgramState::Working)
        ) {
            self.deliver_unread(id);
        }
    }

    /// Pastes the agent's unread messages into its terminal when it can take them (see
    /// [`Live::takes_messages`]). They count as delivered once the paste reached the
    /// terminal.
    pub(crate) fn deliver_unread(self: &Arc<Self>, id: AgentId) {
        let (terminal, paste) = {
            let mut live = lock(&self.live);
            let Some(agent) = live.get_mut(&id) else {
                return;
            };

            if !agent.accepts_messages() || self.messages.is_waiting(id) {
                return;
            }

            if agent.typing.drafting(Instant::now()) {
                let retry = agent.typing.begin_retry();
                drop(live);

                if retry {
                    self.retry_after_draft(id);
                }

                return;
            }

            (agent.info.terminal, Arc::clone(&agent.paste))
        };
        let Some(hub) = terminal.and_then(|terminal| self.terminals.get(terminal)) else {
            return;
        };

        let claim = match self.messages.claim(id) {
            Ok(claim) if !claim.is_empty() => claim,
            Ok(_) => return,
            Err(error) => {
                log::error!("cannot take the messages of agent {}: {error:#}", id.0);

                return;
            }
        };

        let agents = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name(format!("deliver-{}", id.0))
            .spawn(move || {
                let _one_paste = lock(&paste);

                // Waiting for the paste lock took time: the agent may be blocked by now, the
                // user may have begun a line, or a `wait` may take the messages.
                if !agents.begin_paste(id) {
                    claim.release();
                    // A line the user began meanwhile needs its retry timer.
                    agents.deliver_unread(id);

                    return;
                }

                let typed = hub.send_input(TerminalInput::Paste(render_inbox(claim.messages())));
                sleep_for(ENTER_DELAY);

                // A prompt that opened since the paste would take this Enter as its answer.
                // The text stays in the input box: keep the claim and press Enter once the
                // prompt is gone, so the messages are not pasted a second time.
                if typed && !wait_out_block(|| agents.waiting(id), BLOCKED_POLL) {
                    log::warn!("agent {} ended before its messages were sent", id.0);
                    claim.release();

                    return;
                }

                if !typed || !hub.send_input(TerminalInput::Text("\r".to_owned())) {
                    log::warn!("agent {} ended before its messages arrived", id.0);
                }
                if typed {
                    claim.commit();
                }
            });

        if let Err(error) = spawned {
            log::error!("cannot start the delivery thread: {error}");
        }
    }

    /// Run with the paste lock held. False when the agent no longer takes messages or
    /// sits in a `wait`. A paste into a working agent lands in its input queue; the
    /// agent's next `wait` then ends at once (see [`Messages::begin_paste`]).
    fn begin_paste(&self, id: AgentId) -> bool {
        let live = lock(&self.live);
        let Some(agent) = live.get(&id) else {
            return false;
        };

        if !agent.takes_messages(Instant::now()) {
            return false;
        }

        let working = agent.merge.status() == AgentStatus::Working;

        self.messages.begin_paste(id, working)
    }

    /// Delivers again once the user's line no longer holds messages back. One thread
    /// per agent: later calls while it runs do nothing.
    fn retry_after_draft(self: &Arc<Self>, id: AgentId) {
        let agents = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name(format!("draft-{}", id.0))
            .spawn(move || {
                while let Some(left) = agents.draft_remaining(id) {
                    sleep_for(left);
                }

                agents.deliver_unread(id);
            });

        if let Err(error) = spawned {
            log::error!("cannot start the draft thread: {error}");
            self.update(id, |live| {
                live.typing.retry_pending = false;

                false
            });
        }
    }

    /// How long the agent's draft still holds messages back. When there is none, the
    /// retry thread is done.
    fn draft_remaining(&self, id: AgentId) -> Option<Duration> {
        let mut live = lock(&self.live);
        let agent = live.get_mut(&id)?;

        let left = agent.typing.draft_remaining(Instant::now());
        if left.is_none() {
            agent.typing.retry_pending = false;
        }

        left
    }

    /// The paste queue of an agent that reads its input again is empty.
    fn end_queue_if_stopped(&self, agent: &Live) {
        if matches!(agent.merge.status(), AgentStatus::Idle | AgentStatus::Done) {
            self.messages.clear_queued(agent.info.id);
        }
    }

    fn exited(self: &Arc<Self>, id: AgentId, exit: Option<&ExitStatus>) {
        let Some(mut agent) = lock(&self.live).remove(&id) else {
            return;
        };

        self.messages.clear_queued(id);

        let failure_told = agent.merge.failure_told();
        agent.merge.process_exited(exit);
        agent.info.terminal = None;
        agent.info.lifecycle =
            if !agent.stop_requested && agent.session_id.is_some() && agent.session_confirmed {
                AgentLifecycle::Resumable
            } else {
                AgentLifecycle::Stopped
            };
        agent.info.ended_at = Some(unix_millis());
        self.publish(&mut agent);

        if !agent.stopped_by_user && !self.is_closing(agent.info.task) {
            let how = exit.map_or_else(|| "no exit status".to_owned(), exit_text);
            self.tell_worker_stopped(&agent.info, &format!("stopped, {how}"));
        }

        if let Some(reason) = exit_failure(exit)
            && !failure_told
            && !self.is_ending(&agent)
        {
            self.send_notice(NoticeKind::Broke, &agent.info, reason);
        }

        self.follow_review(&agent);

        for observer in lock(&self.on_end).iter() {
            observer(id, agent.stopped_by_user);
        }
    }

    /// Runs `change` on a live agent and publishes when it returns true.
    fn update(&self, id: AgentId, change: impl FnOnce(&mut Live) -> bool) {
        let mut live = lock(&self.live);
        let Some(agent) = live.get_mut(&id) else {
            return;
        };

        if change(agent) {
            self.publish(agent);
        }

        self.send_notices(agent);
        self.end_queue_if_stopped(agent);
    }

    fn publish(&self, agent: &mut Live) {
        agent.info.revision += 1;
        agent.info.status = agent.merge.status();
        agent.info.message = agent.merge.message().map(str::to_owned);

        if let Err(error) = lock(&self.store).save_agent(&agent.info) {
            log::error!("cannot save agent {}: {error}", agent.info.id.0);
        }

        self.clients.broadcast(&Event::AgentChanged {
            agent: agent.info.clone(),
        });
    }

    /// Tells every client what a change of the agent means for the user. Call it after
    /// every change, published or not: a notice hides a status change from the app.
    fn send_notices(&self, agent: &mut Live) {
        if let Some(text) = agent.merge.take_prompt() {
            self.send_notice(NoticeKind::Prompt, &agent.info, text);
        }

        if let Some(reason) = agent.merge.take_failure()
            && !self.is_ending(agent)
        {
            self.send_notice(NoticeKind::Broke, &agent.info, reason);
        }

        self.follow_review(agent);
    }

    /// Follows the swarm of the agent's task, to tell when it is ready for review.
    fn follow_review(&self, agent: &Live) {
        let Some(task) = agent.info.task else {
            return;
        };

        match agent.info.role {
            AgentRole::Worker if agent.merge.status() == AgentStatus::Working => {
                lock(&self.reviews).entry(task).or_default().worker_worked();
            }
            AgentRole::Worker if !self.is_halted(agent) => self.send_review_if_ready(task),
            AgentRole::Orchestrator => self.check_review(agent, task),
            AgentRole::Worker | AgentRole::StartOrchestrator => {}
        }
    }

    /// Follows the orchestrator of the task.
    fn check_review(&self, orchestrator: &Live, task: TaskId) {
        if self.is_ending(orchestrator) {
            return;
        }

        let rests = orchestrator.merge.is_resting()
            && orchestrator.info.lifecycle == AgentLifecycle::Running;

        lock(&self.reviews)
            .entry(task)
            .or_default()
            .orchestrator_changed(rests, orchestrator.merge.has_notice());

        self.send_review_if_ready(task);
    }

    /// Sends "ready for review" when the orchestrator rests and every worker is done,
    /// once per round of work. It happens when the last of the two happens, so a worker
    /// that finishes after the orchestrator rested counts too.
    fn send_review_if_ready(&self, task: TaskId) {
        let mut reviews = lock(&self.reviews);
        let Some(watch) = reviews.get_mut(&task).filter(|watch| watch.may_be_ready()) else {
            return;
        };

        let Some((orchestrator, name)) = self.orchestrator_when_workers_are_done(task) else {
            return;
        };

        watch.sent = true;
        drop(reviews);

        self.send_notice(NoticeKind::ReadyForReview, &orchestrator, name);
    }

    /// The running orchestrator and the task's name, when the task has workers and none
    /// of them is busy.
    fn orchestrator_when_workers_are_done(&self, task: TaskId) -> Option<(AgentInfo, String)> {
        let store = lock(&self.store);
        let found = store
            .list_agents(task)
            .and_then(|agents| Ok((agents, store.task(task)?)));

        match found {
            Ok((agents, Some(task))) if workers_done(&agents) => agents
                .into_iter()
                .find(|agent| {
                    agent.role == AgentRole::Orchestrator
                        && agent.lifecycle == AgentLifecycle::Running
                })
                .map(|orchestrator| (orchestrator, task.name)),
            Ok(_) => None,
            Err(error) => {
                log::error!("cannot check whether task {} is ready: {error}", task.0);

                None
            }
        }
    }

    /// The agent ends on purpose or with the daemon, so a signal or a failure is no news.
    fn is_ending(&self, agent: &Live) -> bool {
        agent.stop_requested || self.is_halted(agent)
    }

    /// The user, a close or the daemon stops the agent, not the agent itself.
    fn is_halted(&self, agent: &Live) -> bool {
        agent.stopped_by_user
            || self
                .shutting_down
                .load(std::sync::atomic::Ordering::Relaxed)
            || self.is_closing(agent.info.task)
    }

    fn send_notice(&self, kind: NoticeKind, agent: &AgentInfo, text: String) {
        self.clients.broadcast(&Event::Notice {
            notice: Notice {
                kind,
                project: agent.project,
                task: agent.task,
                agent: agent.id,
                text,
                at: unix_millis(),
            },
        });
    }
}

/// Waits for the agent's ready signal. Returns false when the agent ended first.
/// Every `patience` without a signal, a blocked agent (for example on a trust dialog)
/// keeps the message waiting, and any other agent gets it anyway.
fn wait_until_ready(
    ready: &flume::Receiver<Ready>,
    waiting: impl Fn() -> Waiting,
    patience: Duration,
    grace: Duration,
) -> bool {
    loop {
        match ready.recv_timeout(patience) {
            Ok(Ready::Idle) => return true,
            Ok(Ready::SessionStart) => return wait_for_idle_report(ready, grace),
            Err(flume::RecvTimeoutError::Timeout) => match waiting() {
                Waiting::Gone => return false,
                Waiting::Blocked => {}
                Waiting::Other => return true,
            },
            Err(flume::RecvTimeoutError::Disconnected) => return false,
        }
    }
}

/// Waits while the agent is blocked, looking every `poll`. Returns false when the agent
/// ended first. There is no time limit: a permission prompt can wait for the user.
fn wait_out_block(waiting: impl Fn() -> Waiting, poll: Duration) -> bool {
    loop {
        match waiting() {
            Waiting::Gone => return false,
            Waiting::Blocked => sleep_for(poll),
            Waiting::Other => return true,
        }
    }
}

/// After `SessionStart`: an OSC 7501 report within `grace` wins, else go ahead.
fn wait_for_idle_report(ready: &flume::Receiver<Ready>, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());

        match ready.recv_timeout(remaining) {
            Ok(Ready::Idle) | Err(flume::RecvTimeoutError::Timeout) => return true,
            Ok(Ready::SessionStart) => {}
            Err(flume::RecvTimeoutError::Disconnected) => return false,
        }
    }
}

/// `park_timeout` can wake early, so wait until the whole time has passed.
fn sleep_for(duration: Duration) {
    let deadline = Instant::now() + duration;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }

        thread::park_timeout(remaining);
    }
}

/// The folder is private (0700) and so are its files (0600): they hold the prompt.
fn write_files(folder: &Path, files: &[adapters::LaunchFile]) -> Result<()> {
    std::fs::create_dir_all(folder)
        .with_context(|| format!("cannot create {}", folder.display()))?;
    std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("cannot protect {}", folder.display()))?;

    for file in files {
        let write = || -> std::io::Result<()> {
            let mut opened = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&file.path)?;
            opened.write_all(file.contents.as_bytes())?;

            std::fs::set_permissions(&file.path, std::fs::Permissions::from_mode(0o600))
        };

        write().with_context(|| format!("cannot write {}", file.path.display()))?;
    }

    Ok(())
}

/// The words for a role in `SWARM_ROLE`.
pub(crate) fn role_name(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Orchestrator => "orchestrator",
        AgentRole::Worker => "worker",
        AgentRole::StartOrchestrator => "start_orchestrator",
    }
}

/// The `SWARM_*` variables of an agent.
fn harness_env(info: &AgentInfo, socket: &Path, token: &str) -> Vec<(String, String)> {
    let mut env = vec![
        (
            "SWARM_SOCKET".to_owned(),
            socket.to_string_lossy().into_owned(),
        ),
        ("SWARM_AGENT_ID".to_owned(), info.id.0.to_string()),
        ("SWARM_AGENT_NAME".to_owned(), info.name.clone()),
    ];
    if let Some(task) = info.task {
        env.push(("SWARM_TASK".to_owned(), task.0.to_string()));
    }
    env.push(("SWARM_TOKEN".to_owned(), token.to_owned()));
    env.push(("SWARM_ROLE".to_owned(), role_name(info.role).to_owned()));

    env
}

/// Compares without stopping at the first difference.
fn same_token(expected: &str, given: &str) -> bool {
    expected.len() == given.len()
        && expected
            .bytes()
            .zip(given.bytes())
            .fold(0, |difference, (left, right)| difference | (left ^ right))
            == 0
}

/// The text of the screen, row by row.
#[derive(Default)]
struct ScreenText {
    rows: BTreeMap<u16, String>,
}

impl ScreenText {
    fn apply(&mut self, output: &TerminalOutput) {
        match output {
            TerminalOutput::Snapshot(snapshot) => {
                self.rows = snapshot
                    .rows
                    .iter()
                    .enumerate()
                    .map(|(index, row)| (u16::try_from(index).unwrap_or(u16::MAX), row_text(row)))
                    .collect();
            }
            TerminalOutput::Update(update) => {
                for (index, row) in &update.rows {
                    self.rows.insert(*index, row_text(row));
                }
            }
            _ => {}
        }
    }

    fn text(&self) -> String {
        self.rows
            .values()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The key the app finds an agent's task by: the task, or the project's Start tab.
pub(crate) fn terminal_key(info: &AgentInfo) -> String {
    match info.task {
        Some(task) => task.terminal_key(),
        None => start_key(info.project),
    }
}

/// Keeps in step with `start_key` in the app's model.
pub(crate) fn start_key(project: ProjectId) -> String {
    format!("project:{}/start", project.0)
}

pub(crate) fn row_text(row: &Row) -> String {
    let mut line = String::new();
    for span in &row.texts {
        while line.chars().count() < usize::from(span.col) {
            line.push(' ');
        }
        line.push_str(&span.text);
    }

    line
}

#[cfg(test)]
mod tests {
    use swarm_protocol::read;
    use swarm_term::ProgramStatus;

    use crate::hub::{ClientSink, Frame};

    use super::*;

    const SHORT: Duration = Duration::from_millis(40);

    fn channel() -> (flume::Sender<Ready>, flume::Receiver<Ready>) {
        flume::unbounded()
    }

    #[test]
    fn an_idle_report_is_ready() {
        let (sender, receiver) = channel();
        sender.send(Ready::Idle).expect("send");

        assert!(wait_until_ready(&receiver, || Waiting::Other, SHORT, SHORT));
    }

    #[test]
    fn session_start_alone_is_ready_after_the_grace_time() {
        let (sender, receiver) = channel();
        sender.send(Ready::SessionStart).expect("send");
        let start = Instant::now();

        let ready = wait_until_ready(&receiver, || Waiting::Other, SHORT, SHORT * 3);

        assert!(ready);
        assert!(start.elapsed() >= SHORT * 3);
    }

    #[test]
    fn an_idle_report_after_session_start_does_not_wait_for_the_grace_time() {
        let (sender, receiver) = channel();
        sender.send(Ready::SessionStart).expect("send");
        sender.send(Ready::Idle).expect("send");
        let start = Instant::now();

        assert!(wait_until_ready(
            &receiver,
            || Waiting::Other,
            SHORT,
            Duration::from_secs(5)
        ));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_blocked_agent_keeps_the_message_waiting() {
        let (sender, receiver) = channel();
        let polls = std::sync::atomic::AtomicU32::new(0);

        let ready = wait_until_ready(
            &receiver,
            || {
                if polls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 3 {
                    Waiting::Blocked
                } else {
                    sender.send(Ready::Idle).ok();
                    Waiting::Blocked
                }
            },
            SHORT,
            SHORT,
        );

        assert!(ready);
        assert!(polls.load(std::sync::atomic::Ordering::Relaxed) >= 4);
    }

    #[test]
    fn an_agent_that_is_not_blocked_gets_the_message_anyway() {
        let (_sender, receiver) = channel();

        assert!(wait_until_ready(&receiver, || Waiting::Other, SHORT, SHORT));
    }

    #[test]
    fn an_agent_that_exited_drops_the_message() {
        let (_sender, receiver) = channel();
        assert!(!wait_until_ready(&receiver, || Waiting::Gone, SHORT, SHORT));

        let (sender, receiver) = channel();
        drop(sender);
        assert!(!wait_until_ready(
            &receiver,
            || Waiting::Other,
            SHORT,
            SHORT
        ));
    }

    fn live(kind: AgentKind, state: ProgramState) -> Live {
        let mut merge = StatusMerge::default();
        merge.program_status(&ProgramStatus {
            state,
            message: None,
        });

        Live {
            info: AgentInfo {
                id: AgentId(1),
                project: ProjectId(1),
                task: Some(TaskId(1)),
                role: AgentRole::Orchestrator,
                kind,
                model: "model".to_owned(),
                name: "orchestrator".to_owned(),
                worktree: PathBuf::from("/repo"),
                branch: "main".to_owned(),
                terminal: Some(TerminalId(1)),
                status: AgentStatus::Unknown,
                message: None,
                lifecycle: AgentLifecycle::Running,
                revision: 0,
                created_at: 0,
                started_at: None,
                ended_at: None,
            },
            plan: Plan {
                kind,
                role: AgentRole::Orchestrator,
                model: "model".to_owned(),
                command: "agent".to_owned(),
                skip_permissions: false,
            },
            token: String::new(),
            merge,
            session_id: None,
            session_confirmed: false,
            screen: None,
            ready: None,
            delivery_open: true,
            stop_requested: false,
            stopped_by_user: false,
            typing: UserTyping::default(),
            paste: Arc::default(),
        }
    }

    #[test]
    fn claude_code_takes_messages_while_idle_done_or_working() {
        for state in [
            ProgramState::Idle,
            ProgramState::Done,
            ProgramState::Working,
        ] {
            assert!(
                live(AgentKind::ClaudeCode, state).takes_messages(Instant::now()),
                "{state:?}"
            );
        }
    }

    #[test]
    fn codex_takes_messages_only_while_idle_or_done() {
        let now = Instant::now();

        assert!(live(AgentKind::Codex, ProgramState::Idle).takes_messages(now));
        assert!(live(AgentKind::Codex, ProgramState::Done).takes_messages(now));
        assert!(!live(AgentKind::Codex, ProgramState::Working).takes_messages(now));
    }

    #[test]
    fn no_agent_takes_messages_while_blocked_in_error_or_unknown() {
        let blocked = [
            ProgramState::Blocked(swarm_term::BlockedKind::Permission),
            ProgramState::Blocked(swarm_term::BlockedKind::Question),
            ProgramState::Blocked(swarm_term::BlockedKind::Auth),
            ProgramState::Blocked(swarm_term::BlockedKind::Other),
            ProgramState::Error,
            ProgramState::Clear,
        ];

        for kind in [AgentKind::ClaudeCode, AgentKind::Codex] {
            for state in blocked {
                assert!(
                    !live(kind, state).takes_messages(Instant::now()),
                    "{kind:?} {state:?}"
                );
            }

            let mut silent = live(kind, ProgramState::Idle);
            silent.merge = StatusMerge::default();
            assert!(
                !silent.takes_messages(Instant::now()),
                "{kind:?} before any report"
            );
        }
    }

    #[test]
    fn nothing_is_typed_before_the_first_message_went_in() {
        let mut agent = live(AgentKind::ClaudeCode, ProgramState::Idle);
        agent.delivery_open = false;

        assert!(!agent.takes_messages(Instant::now()));
    }

    #[test]
    fn a_notice_blocks_delivery_until_the_agent_works_again() {
        let mut agent = live(AgentKind::ClaudeCode, ProgramState::Working);
        agent.merge.notify("look here");
        assert!(!agent.takes_messages(Instant::now()));

        agent.merge.program_status(&ProgramStatus {
            state: ProgramState::Working,
            message: None,
        });
        assert!(agent.takes_messages(Instant::now()));
    }

    #[test]
    fn sleep_for_waits_the_whole_time() {
        let start = Instant::now();

        sleep_for(SHORT);

        assert!(start.elapsed() >= SHORT);
    }

    #[test]
    fn agent_files_are_private() {
        let folder = PathBuf::from(format!("/tmp/swd-unit-{}", std::process::id()));
        let file = adapters::LaunchFile {
            path: folder.join("prompt.md"),
            contents: "secret".to_owned(),
        };
        std::fs::create_dir_all(&folder).expect("folder");
        std::fs::write(&file.path, "old").expect("old file");

        write_files(&folder, std::slice::from_ref(&file)).expect("write");

        let mode = |path: &Path| {
            std::fs::metadata(path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode(&folder), 0o700);
        assert_eq!(mode(&file.path), 0o600);
        assert_eq!(std::fs::read_to_string(&file.path).expect("read"), "secret");
        std::fs::remove_dir_all(&folder).expect("cleanup");
    }

    fn typed(text: &str) -> TerminalInput {
        TerminalInput::Text(text.to_owned())
    }

    fn key(name: &str, text: Option<&str>) -> TerminalInput {
        TerminalInput::Key(swarm_term::Key {
            name: name.to_owned(),
            modifiers: swarm_term::Modifiers::default(),
            text: text.map(str::to_owned),
            repeat: false,
        })
    }

    #[test]
    fn the_first_enter_after_typing_tells_the_orchestrator() {
        let mut typing = UserTyping::default();
        let now = Instant::now();

        assert!(!typing.observe(&key("a", Some("a")), now));
        assert!(!typing.observe(&typed("bc"), now));
        assert!(typing.observe(&key("enter", None), now));
    }

    #[test]
    fn an_enter_without_typing_is_not_news() {
        let mut typing = UserTyping::default();

        assert!(!typing.observe(&key("enter", None), Instant::now()));
        assert!(!typing.observe(&typed("\r"), Instant::now()));
    }

    #[test]
    fn text_or_a_paste_that_ends_in_a_line_break_counts_as_typing_and_enter() {
        let now = Instant::now();

        assert!(UserTyping::default().observe(&typed("ls\r"), now));
        assert!(UserTyping::default().observe(&TerminalInput::Paste("a\nb\n".to_owned()), now));
        assert!(!UserTyping::default().observe(&TerminalInput::Paste("no break".to_owned()), now));
    }

    #[test]
    fn keys_that_type_nothing_do_not_count() {
        let mut typing = UserTyping::default();
        let now = Instant::now();

        assert!(!typing.observe(&key("up", None), now));
        assert!(!typing.observe(&TerminalInput::Focus(true), now));
        assert!(!typing.observe(&key("enter", None), now));
    }

    #[test]
    fn the_orchestrator_hears_at_most_once_in_thirty_seconds() {
        let mut typing = UserTyping::default();
        let start = Instant::now();

        assert!(typing.observe(&typed("a\r"), start));
        assert!(!typing.observe(&typed("b\r"), start + Duration::from_secs(29)));
        assert!(typing.observe(&typed("c\r"), start + Duration::from_secs(30)));
    }

    #[test]
    fn typing_starts_a_draft_and_enter_ends_it() {
        let mut typing = UserTyping::default();
        let now = Instant::now();

        assert!(!typing.track_draft(&key("a", Some("a")), now));
        assert!(typing.drafting(now));

        assert!(typing.track_draft(&key("enter", None), now));
        assert!(!typing.drafting(now));
    }

    #[test]
    fn an_enter_without_a_draft_sends_nothing() {
        let mut typing = UserTyping::default();

        assert!(!typing.track_draft(&key("enter", None), Instant::now()));
        assert!(!typing.track_draft(&typed("\r"), Instant::now()));
    }

    #[test]
    fn a_line_break_in_text_ends_the_draft_unless_more_text_follows() {
        let now = Instant::now();

        for (text, ends) in [("ls\r", true), ("a\nb\n", true), ("a\nb", false)] {
            let mut typing = UserTyping::default();
            typing.track_draft(&typed("half"), now);

            assert_eq!(typing.track_draft(&typed(text), now), ends, "{text:?}");
            assert_eq!(typing.drafting(now), !ends, "{text:?}");
        }

        let mut typing = UserTyping::default();
        typing.track_draft(&TerminalInput::Paste("first\nsecond".to_owned()), now);
        assert!(typing.drafting(now));
    }

    #[test]
    fn shift_or_alt_with_enter_keeps_the_draft() {
        let now = Instant::now();

        for (shift, alt) in [(true, false), (false, true)] {
            let mut typing = UserTyping::default();
            typing.track_draft(&typed("half"), now);

            let enter = TerminalInput::Key(swarm_term::Key {
                name: "enter".to_owned(),
                modifiers: swarm_term::Modifiers {
                    shift,
                    alt,
                    ..swarm_term::Modifiers::default()
                },
                text: None,
                repeat: false,
            });

            assert!(!typing.track_draft(&enter, now), "{shift} {alt}");
            assert!(typing.drafting(now), "{shift} {alt}");
        }
    }

    #[test]
    fn keys_that_type_nothing_do_not_start_a_draft() {
        let mut typing = UserTyping::default();
        let now = Instant::now();

        typing.track_draft(&key("up", None), now);
        typing.track_draft(&TerminalInput::Focus(true), now);

        assert!(!typing.drafting(now));
    }

    #[test]
    fn a_draft_holds_messages_back_for_sixty_seconds_after_the_last_key() {
        let mut typing = UserTyping::default();
        let start = Instant::now();

        typing.track_draft(&typed("a"), start);
        typing.track_draft(&typed("b"), start + Duration::from_secs(30));

        assert!(typing.drafting(start + Duration::from_secs(89)));
        assert!(!typing.drafting(start + Duration::from_secs(90)));
        assert_eq!(
            typing.draft_remaining(start + Duration::from_secs(80)),
            Some(Duration::from_secs(10))
        );
    }

    #[test]
    fn nothing_is_typed_into_a_line_the_user_is_writing() {
        let mut agent = live(AgentKind::ClaudeCode, ProgramState::Idle);
        let start = Instant::now();
        agent.typing.track_draft(&typed("half"), start);

        assert!(!agent.takes_messages(start + Duration::from_secs(59)));
        assert!(agent.takes_messages(start + DRAFT_PATIENCE));

        agent.typing.track_draft(&key("enter", None), start);
        assert!(agent.takes_messages(start + Duration::from_secs(1)));
    }

    #[test]
    fn the_draft_retry_starts_once() {
        let mut typing = UserTyping::default();

        assert!(typing.begin_retry());
        assert!(!typing.begin_retry());

        typing.retry_pending = false;
        assert!(typing.begin_retry());
    }

    #[test]
    fn a_blocked_agent_is_waited_out_without_a_time_limit() {
        let polls = std::sync::atomic::AtomicU32::new(0);

        let ended = wait_out_block(
            || {
                if polls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 5 {
                    Waiting::Blocked
                } else {
                    Waiting::Other
                }
            },
            Duration::from_millis(1),
        );

        assert!(ended);
        assert_eq!(polls.load(std::sync::atomic::Ordering::Relaxed), 6);
    }

    #[test]
    fn an_agent_that_ends_while_blocked_ends_the_wait_with_false() {
        let polls = std::sync::atomic::AtomicU32::new(0);

        let ended = wait_out_block(
            || {
                if polls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 2 {
                    Waiting::Blocked
                } else {
                    Waiting::Gone
                }
            },
            Duration::from_millis(1),
        );

        assert!(!ended);
    }

    #[test]
    fn an_agent_that_is_not_blocked_is_not_waited_for() {
        let start = Instant::now();

        assert!(wait_out_block(|| Waiting::Other, Duration::from_secs(5)));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    fn member(role: AgentRole, lifecycle: AgentLifecycle, status: AgentStatus) -> AgentInfo {
        AgentInfo {
            role,
            lifecycle,
            status,
            ..live(AgentKind::ClaudeCode, ProgramState::Idle).info
        }
    }

    fn worker(lifecycle: AgentLifecycle, status: AgentStatus) -> AgentInfo {
        member(AgentRole::Worker, lifecycle, status)
    }

    #[test]
    fn a_task_without_workers_is_not_done() {
        assert!(!workers_done(&[]));
        assert!(!workers_done(&[member(
            AgentRole::Orchestrator,
            AgentLifecycle::Running,
            AgentStatus::Idle
        )]));
    }

    #[test]
    fn a_working_blocked_or_starting_worker_is_not_done() {
        let running = |status| [worker(AgentLifecycle::Running, status)];

        assert!(!workers_done(&running(AgentStatus::Working)));
        assert!(!workers_done(&running(AgentStatus::Blocked(
            swarm_term::BlockedKind::Permission
        ))));
        assert!(!workers_done(&running(AgentStatus::Unknown)));
        assert!(!workers_done(&[worker(
            AgentLifecycle::Starting,
            AgentStatus::Unknown
        )]));
    }

    #[test]
    fn idle_done_and_failed_workers_are_done() {
        for status in [AgentStatus::Idle, AgentStatus::Done, AgentStatus::Error] {
            assert!(
                workers_done(&[worker(AgentLifecycle::Running, status)]),
                "{status:?}"
            );
        }
    }

    #[test]
    fn a_worker_that_ended_is_done_even_with_a_stale_status() {
        for lifecycle in [AgentLifecycle::Resumable, AgentLifecycle::Stopped] {
            assert!(workers_done(&[worker(lifecycle, AgentStatus::Working)]));
        }
    }

    #[test]
    fn one_busy_worker_among_done_ones_is_not_done() {
        assert!(!workers_done(&[
            worker(AgentLifecycle::Running, AgentStatus::Done),
            worker(AgentLifecycle::Running, AgentStatus::Working),
        ]));
    }

    #[test]
    fn a_watch_is_ready_only_while_the_orchestrator_rests_and_nothing_was_sent() {
        let mut watch = ReviewWatch::default();
        assert!(!watch.may_be_ready());

        watch.orchestrator_changed(true, false);
        assert!(watch.may_be_ready());

        watch.sent = true;
        assert!(!watch.may_be_ready());

        watch.sent = false;
        watch.orchestrator_changed(false, false);
        assert!(!watch.may_be_ready());
    }

    #[test]
    fn an_orchestrator_that_began_to_rest_with_a_question_stays_not_ready_until_it_works() {
        let mut watch = ReviewWatch::default();

        watch.orchestrator_changed(true, true);
        watch.orchestrator_changed(true, false);
        assert!(!watch.may_be_ready());

        watch.orchestrator_changed(false, false);
        watch.orchestrator_changed(true, false);
        assert!(watch.may_be_ready());
    }

    #[test]
    fn a_worker_working_makes_the_next_rest_news_again() {
        let mut watch = ReviewWatch {
            sent: true,
            ..ReviewWatch::default()
        };

        watch.worker_worked();

        assert!(!watch.sent);
    }

    #[test]
    fn equal_tokens_match() {
        assert!(same_token("abc123", "abc123"));
    }

    #[test]
    fn a_different_token_is_refused() {
        assert!(!same_token("abc123", "abc124"));
    }

    #[test]
    fn a_token_of_another_length_is_refused() {
        assert!(!same_token("abc123", "abc12"));
    }

    #[test]
    fn an_empty_token_does_not_match_a_real_one() {
        assert!(!same_token("abc123", ""));
    }

    fn agents_with_a_task() -> (Arc<Agents>, TaskInfo) {
        let (agents, task, _frames) = agents_with_a_client();

        (agents, task)
    }

    /// Like [`agents_with_a_task`], with a connected client; `frames` holds what the
    /// client was sent.
    fn agents_with_a_client() -> (Arc<Agents>, TaskInfo, flume::Receiver<Frame>) {
        use swarm_core::store::NewTask;

        let store = Store::open_in_memory().expect("store");
        let project = store
            .add_project(Path::new("/repo"), "repo")
            .expect("project");
        let task = store
            .add_task(&NewTask {
                project: project.id,
                name: "Task",
                branch: "swarm/task",
                base: "main",
                pr_base: "main",
                worktree: Path::new("/repo/.worktrees/task"),
                ports: &[],
            })
            .expect("task");
        let store = Arc::new(Mutex::new(store));
        let clients = Arc::new(Clients::default());
        let (sender, frames) = flume::unbounded();
        clients.add(&Arc::new(ClientSink::new(1, sender, None)));
        let terminals = Terminals::new(Arc::clone(&store));
        let messages = Messages::new(Arc::clone(&store), Arc::clone(&clients));

        let agents = Agents::new(
            AgentConfig {
                data_dir: PathBuf::from("/tmp/none-data"),
                socket: PathBuf::from("/tmp/none.sock"),
                settings: None,
                swarm: PathBuf::from("/tmp/swarm"),
                github: Github::with_folders(PathBuf::from("/tmp/none-bin"), None, Vec::new()),
            },
            store,
            terminals,
            clients,
            messages,
        );

        (agents, task, frames)
    }

    fn register_in(agents: &Agents, task: &TaskInfo, asker: Option<AgentId>) -> Result<AgentInfo> {
        let plan = Plan {
            kind: AgentKind::ClaudeCode,
            role: AgentRole::Orchestrator,
            model: "model".to_owned(),
            command: "agent".to_owned(),
            skip_permissions: false,
        };

        agents.register_asked_by(asker, &Place::of_task(task), plan)
    }

    /// What `on_started` does before the hub is known: the agent has a terminal that
    /// no stop can reach.
    fn start_without_a_hub(agents: &Agents, id: AgentId) {
        agents.update(id, |live| {
            live.info.terminal = Some(TerminalId(99));

            true
        });
    }

    #[test]
    fn a_stop_all_that_found_no_hub_is_seen_when_the_process_has_started() {
        let (agents, task) = agents_with_a_task();
        let agent = register_in(&agents, &task, None).expect("register");
        start_without_a_hub(&agents, agent.id);
        assert!(!agents.ended_while_starting(agent.id, agent.task));

        agents.stop_all();

        assert!(agents.ended_while_starting(agent.id, agent.task));
    }

    #[test]
    fn an_exit_that_found_no_hub_is_seen_when_the_process_has_started() {
        let (agents, task) = agents_with_a_task();
        let agent = register_in(&agents, &task, None).expect("register");
        start_without_a_hub(&agents, agent.id);

        agents.exit(agent.id);

        assert!(agents.ended_while_starting(agent.id, agent.task));
    }

    #[test]
    fn a_close_that_began_is_seen_when_the_process_has_started() {
        let (agents, task) = agents_with_a_task();
        let agent = register_in(&agents, &task, None).expect("register");
        start_without_a_hub(&agents, agent.id);
        assert!(!agents.ended_while_starting(agent.id, agent.task));

        assert!(agents.begin_close(task.id));

        assert!(agents.ended_while_starting(agent.id, agent.task));
        assert!(
            lock(&agents.live)[&agent.id].stop_requested,
            "it ends as stopped, not resumable"
        );
    }

    #[test]
    fn an_agent_that_was_removed_is_seen_when_the_process_has_started() {
        let (agents, task) = agents_with_a_task();
        let agent = register_in(&agents, &task, None).expect("register");

        agents.stop_all();

        assert!(agents.ended_while_starting(agent.id, agent.task));
    }

    #[test]
    fn an_agent_the_user_stopped_cannot_register_another() {
        let (agents, task) = agents_with_a_task();
        let asker = register_in(&agents, &task, None).expect("register");
        start_without_a_hub(&agents, asker.id);
        assert!(!agents.is_stopped(asker.id));
        let before = lock(&agents.store).list_agents(task.id).expect("list");

        agents.stop_all();
        let refused = register_in(&agents, &task, Some(asker.id));

        assert!(agents.is_stopped(asker.id));
        let error = refused.expect_err("refused");
        assert_eq!(error.to_string(), STOPPED_BY_USER);
        assert_eq!(
            lock(&agents.store).list_agents(task.id).expect("list"),
            before,
            "no row is left"
        );
        assert_eq!(lock(&agents.live).len(), 1);
    }

    #[test]
    fn an_agent_that_is_gone_cannot_register_another() {
        let (agents, task) = agents_with_a_task();

        assert!(agents.is_stopped(AgentId(404)));
        assert!(register_in(&agents, &task, Some(AgentId(404))).is_err());
    }

    #[test]
    fn a_running_agent_registers_another() {
        let (agents, task) = agents_with_a_task();
        let asker = register_in(&agents, &task, None).expect("register");

        let another = register_in(&agents, &task, Some(asker.id));

        assert!(another.is_ok());
        assert_eq!(lock(&agents.live).len(), 2);
    }

    fn notices(frames: &flume::Receiver<Frame>) -> Vec<Notice> {
        frames
            .try_iter()
            .filter_map(|frame| {
                match read::<Event>(&mut &frame[..], &mut Vec::new()).expect("event") {
                    Event::Notice { notice } => Some(notice),
                    _ => None,
                }
            })
            .collect()
    }

    fn texts(notices: &[Notice]) -> Vec<(NoticeKind, &str)> {
        notices
            .iter()
            .map(|notice| (notice.kind, notice.text.as_str()))
            .collect()
    }

    fn report(agents: &Arc<Agents>, id: AgentId, state: ProgramState, message: Option<&str>) {
        let status = ProgramStatus {
            state,
            message: message.map(str::to_owned),
        };

        agents.output(id, &TerminalOutput::ProgramStatus(status));
    }

    fn stop_failure(agents: &Arc<Agents>, id: AgentId, error: &str) {
        agents.hook(
            id,
            &serde_json::json!({"hook_event_name": "StopFailure", "error": error}),
        );
    }

    #[test]
    fn an_agent_that_fails_to_start_is_one_broke_notice() {
        let (agents, task, frames) = agents_with_a_client();
        let agent = register_in(&agents, &task, None).expect("register");

        agents.fail(agent.id, "Command not found");

        let notices = notices(&frames);
        assert_eq!(
            texts(&notices),
            [(NoticeKind::Broke, "Failed to start: Command not found")]
        );
        assert_eq!(notices[0].agent, agent.id);
        assert_eq!(notices[0].task, Some(task.id));
        assert_eq!(notices[0].project, task.project);
    }

    #[test]
    fn an_agent_that_failed_already_fails_to_start_only_once() {
        let (agents, task, frames) = agents_with_a_client();
        let agent = register_in(&agents, &task, None).expect("register");

        agents.fail(agent.id, "Cannot start");
        agents.fail(agent.id, "Cannot start");

        assert_eq!(notices(&frames).len(), 1);
    }

    #[test]
    fn a_start_failure_that_ended_on_purpose_is_no_broke() {
        type Ending = (&'static str, fn(&Agents, &TaskInfo, AgentId));
        let endings: [Ending; 4] = [
            ("the user stopped it", |agents, _, id| {
                lock(&agents.live)
                    .get_mut(&id)
                    .expect("live")
                    .stopped_by_user = true;
            }),
            ("a stop was requested", |agents, _, id| {
                lock(&agents.live)
                    .get_mut(&id)
                    .expect("live")
                    .stop_requested = true;
            }),
            ("the task is closing", |agents, task, _| {
                assert!(agents.begin_close(task.id));
            }),
            ("the daemon stops", |agents, _, _| agents.begin_shutdown()),
        ];

        for (name, end) in endings {
            let (agents, task, frames) = agents_with_a_client();
            let agent = register_in(&agents, &task, None).expect("register");
            end(&agents, &task, agent.id);

            agents.fail(agent.id, "Cannot start");

            assert!(notices(&frames).is_empty(), "{name}");
        }
    }

    #[test]
    fn an_osc_error_is_a_broke_notice_with_the_status_text() {
        let (agents, task, frames) = agents_with_a_client();
        let agent = register_in(&agents, &task, None).expect("register");

        report(
            &agents,
            agent.id,
            ProgramState::Error,
            Some("usage limit reached"),
        );

        let notices = notices(&frames);
        assert_eq!(
            texts(&notices),
            [(NoticeKind::Broke, "usage limit reached")]
        );
        assert_eq!(notices[0].agent, agent.id);
        assert_eq!(notices[0].task, Some(task.id));
    }

    #[test]
    fn an_osc_error_without_text_is_a_broke_notice_with_a_short_reason() {
        let (agents, task, frames) = agents_with_a_client();
        let agent = register_in(&agents, &task, None).expect("register");

        report(&agents, agent.id, ProgramState::Error, None);

        assert_eq!(texts(&notices(&frames)), [(NoticeKind::Broke, "Failed")]);
    }

    #[test]
    fn an_osc_error_is_one_broke_notice_per_error_spell() {
        let (agents, task, frames) = agents_with_a_client();
        let agent = register_in(&agents, &task, None).expect("register");

        report(&agents, agent.id, ProgramState::Error, Some("first"));
        report(&agents, agent.id, ProgramState::Error, Some("first"));
        report(&agents, agent.id, ProgramState::Error, Some("changed"));
        assert_eq!(texts(&notices(&frames)), [(NoticeKind::Broke, "first")]);

        report(&agents, agent.id, ProgramState::Working, None);
        report(&agents, agent.id, ProgramState::Error, Some("second"));
        assert_eq!(texts(&notices(&frames)), [(NoticeKind::Broke, "second")]);
    }

    #[test]
    fn an_osc_error_and_a_stop_failure_hook_are_one_broke_notice_in_either_order() {
        let (agents, task, frames) = agents_with_a_client();
        let agent = register_in(&agents, &task, None).expect("register");
        report(&agents, agent.id, ProgramState::Working, None);

        report(&agents, agent.id, ProgramState::Error, Some("from osc"));
        stop_failure(&agents, agent.id, "from hook");
        assert_eq!(texts(&notices(&frames)), [(NoticeKind::Broke, "from osc")]);

        report(&agents, agent.id, ProgramState::Working, None);
        stop_failure(&agents, agent.id, "from hook");
        report(&agents, agent.id, ProgramState::Error, Some("from osc"));
        assert_eq!(texts(&notices(&frames)), [(NoticeKind::Broke, "from hook")]);
    }

    #[test]
    fn an_osc_error_of_an_agent_that_ends_on_purpose_is_no_broke() {
        let (agents, task, frames) = agents_with_a_client();
        let agent = register_in(&agents, &task, None).expect("register");
        assert!(agents.begin_close(task.id));

        report(&agents, agent.id, ProgramState::Error, Some("closing"));

        assert!(notices(&frames).is_empty());
    }

    fn crash(agents: &Arc<Agents>, id: AgentId, code: u32) {
        agents.exited(id, Some(&ExitStatus::Code(code)));
    }

    #[test]
    fn a_plain_crash_is_one_broke_notice() {
        let (agents, task, frames) = agents_with_a_client();
        let agent = register_in(&agents, &task, None).expect("register");
        report(&agents, agent.id, ProgramState::Working, None);

        crash(&agents, agent.id, 1);

        let notices = notices(&frames);
        assert_eq!(texts(&notices), [(NoticeKind::Broke, "Exited with code 1")]);
        assert_eq!(notices[0].agent, agent.id);
    }

    #[test]
    fn an_error_spell_that_ends_in_a_crash_is_one_broke_notice() {
        let (agents, task, frames) = agents_with_a_client();

        let osc_agent = register_in(&agents, &task, None).expect("register");
        report(&agents, osc_agent.id, ProgramState::Working, None);
        report(
            &agents,
            osc_agent.id,
            ProgramState::Error,
            Some("usage limit reached"),
        );
        crash(&agents, osc_agent.id, 1);
        assert_eq!(
            texts(&notices(&frames)),
            [(NoticeKind::Broke, "usage limit reached")]
        );

        let hook_agent = register_in(&agents, &task, None).expect("register");
        report(&agents, hook_agent.id, ProgramState::Working, None);
        stop_failure(&agents, hook_agent.id, "rate_limit");
        agents.exited(
            hook_agent.id,
            Some(&ExitStatus::Signal("Terminated".to_owned())),
        );
        assert_eq!(
            texts(&notices(&frames)),
            [(NoticeKind::Broke, "rate_limit")]
        );
    }

    #[test]
    fn a_crash_after_an_error_spell_ended_is_a_new_broke_notice() {
        let (agents, task, frames) = agents_with_a_client();
        let agent = register_in(&agents, &task, None).expect("register");
        report(&agents, agent.id, ProgramState::Error, Some("first"));

        report(&agents, agent.id, ProgramState::Working, None);
        crash(&agents, agent.id, 1);

        assert_eq!(
            texts(&notices(&frames)),
            [
                (NoticeKind::Broke, "first"),
                (NoticeKind::Broke, "Exited with code 1")
            ]
        );
    }

    fn register_worker(agents: &Agents, task: &TaskInfo) -> AgentInfo {
        let plan = Plan {
            kind: AgentKind::ClaudeCode,
            role: AgentRole::Worker,
            model: "model".to_owned(),
            command: "agent".to_owned(),
            skip_permissions: false,
        };

        agents
            .register_asked_by(None, &Place::of_task(task), plan)
            .expect("register")
    }

    /// An orchestrator and a worker whose processes run.
    fn a_swarm(agents: &Agents, task: &TaskInfo) -> (AgentInfo, AgentInfo) {
        let orchestrator = register_in(agents, task, None).expect("register");
        let worker = register_worker(agents, task);

        for id in [orchestrator.id, worker.id] {
            agents.update(id, |live| {
                live.info.lifecycle = AgentLifecycle::Running;

                true
            });
        }

        (orchestrator, worker)
    }

    fn ready_texts(notices: &[Notice]) -> Vec<(NoticeKind, &str)> {
        texts(notices)
            .into_iter()
            .filter(|(kind, _)| *kind == NoticeKind::ReadyForReview)
            .collect()
    }

    #[test]
    fn a_worker_that_finishes_after_the_orchestrator_rests_gives_ready_for_review() {
        let (agents, task, frames) = agents_with_a_client();
        let (orchestrator, worker) = a_swarm(&agents, &task);
        report(&agents, worker.id, ProgramState::Working, None);
        report(&agents, orchestrator.id, ProgramState::Idle, None);
        assert!(notices(&frames).is_empty(), "the worker is busy");

        report(&agents, worker.id, ProgramState::Done, None);

        let sent = notices(&frames);
        assert_eq!(texts(&sent), [(NoticeKind::ReadyForReview, "Task")]);
        assert_eq!(sent[0].agent, orchestrator.id);

        report(&agents, worker.id, ProgramState::Idle, None);
        report(&agents, orchestrator.id, ProgramState::Done, None);
        assert!(notices(&frames).is_empty(), "once per round of work");
    }

    #[test]
    fn a_worker_that_ends_after_the_orchestrator_rests_gives_ready_for_review() {
        let (agents, task, frames) = agents_with_a_client();
        let (orchestrator, worker) = a_swarm(&agents, &task);
        report(&agents, worker.id, ProgramState::Working, None);
        report(&agents, orchestrator.id, ProgramState::Idle, None);
        assert!(notices(&frames).is_empty(), "the worker is busy");

        crash(&agents, worker.id, 0);

        assert_eq!(
            ready_texts(&notices(&frames)),
            [(NoticeKind::ReadyForReview, "Task")]
        );
    }

    #[test]
    fn a_worker_that_leaves_a_prompt_after_the_orchestrator_rests_gives_ready_for_review() {
        let (agents, task, frames) = agents_with_a_client();
        let (orchestrator, worker) = a_swarm(&agents, &task);
        report(
            &agents,
            worker.id,
            ProgramState::Blocked(swarm_term::BlockedKind::Permission),
            Some("approve Write"),
        );
        report(&agents, orchestrator.id, ProgramState::Idle, None);
        notices(&frames);

        report(&agents, worker.id, ProgramState::Idle, None);

        assert_eq!(
            texts(&notices(&frames)),
            [(NoticeKind::ReadyForReview, "Task")]
        );
    }

    #[test]
    fn the_last_of_the_orchestrator_and_the_workers_to_rest_gives_one_ready_for_review() {
        let (agents, task, frames) = agents_with_a_client();
        let (orchestrator, worker) = a_swarm(&agents, &task);
        report(&agents, worker.id, ProgramState::Working, None);
        report(&agents, orchestrator.id, ProgramState::Working, None);
        report(&agents, worker.id, ProgramState::Done, None);
        assert!(notices(&frames).is_empty(), "the orchestrator is busy");

        report(&agents, orchestrator.id, ProgramState::Idle, None);
        report(&agents, orchestrator.id, ProgramState::Done, None);

        assert_eq!(
            texts(&notices(&frames)),
            [(NoticeKind::ReadyForReview, "Task")]
        );
    }

    #[test]
    fn an_orchestrator_that_asked_for_a_decision_is_not_ready_until_it_rests_again() {
        let (agents, task, frames) = agents_with_a_client();
        let (orchestrator, worker) = a_swarm(&agents, &task);
        report(&agents, worker.id, ProgramState::Working, None);
        report(&agents, orchestrator.id, ProgramState::Working, None);
        agents.notify(orchestrator.id, "pick one");
        report(&agents, orchestrator.id, ProgramState::Idle, None);

        report(&agents, worker.id, ProgramState::Done, None);
        assert_eq!(
            texts(&notices(&frames)),
            [(NoticeKind::NeedsDecision, "pick one")]
        );

        report(&agents, orchestrator.id, ProgramState::Working, None);
        report(&agents, orchestrator.id, ProgramState::Idle, None);
        assert_eq!(
            texts(&notices(&frames)),
            [(NoticeKind::ReadyForReview, "Task")]
        );
    }

    #[test]
    fn a_worker_that_finishes_after_the_orchestrator_ended_gives_no_ready_for_review() {
        let (agents, task, frames) = agents_with_a_client();
        let (orchestrator, worker) = a_swarm(&agents, &task);
        report(&agents, worker.id, ProgramState::Working, None);
        report(&agents, orchestrator.id, ProgramState::Idle, None);
        agents.exit(orchestrator.id);
        crash(&agents, orchestrator.id, 0);
        notices(&frames);

        report(&agents, worker.id, ProgramState::Done, None);

        assert!(ready_texts(&notices(&frames)).is_empty());
    }

    #[test]
    fn a_status_change_during_a_close_gives_no_ready_for_review() {
        let (agents, task, frames) = agents_with_a_client();
        let (orchestrator, worker) = a_swarm(&agents, &task);
        report(&agents, worker.id, ProgramState::Done, None);
        assert!(agents.begin_close(task.id));

        report(&agents, orchestrator.id, ProgramState::Idle, None);

        assert!(notices(&frames).is_empty());
    }

    #[test]
    fn a_worker_that_ends_because_the_user_or_the_daemon_stopped_it_gives_no_ready_for_review() {
        type Ending = (&'static str, fn(&Agents, AgentId));
        let endings: [Ending; 2] = [
            ("the user stopped it", |agents, worker| {
                lock(&agents.live)
                    .get_mut(&worker)
                    .expect("live")
                    .stopped_by_user = true;
            }),
            ("the daemon stops", |agents, _| agents.begin_shutdown()),
        ];

        for (name, end) in endings {
            let (agents, task, frames) = agents_with_a_client();
            let (orchestrator, worker) = a_swarm(&agents, &task);
            report(&agents, worker.id, ProgramState::Working, None);
            report(&agents, orchestrator.id, ProgramState::Idle, None);
            end(&agents, worker.id);

            crash(&agents, worker.id, 0);

            assert!(ready_texts(&notices(&frames)).is_empty(), "{name}");
        }
    }
}
