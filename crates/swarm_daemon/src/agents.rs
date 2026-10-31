//! The agents the daemon runs: their rows, tokens, status and first message.
//!
//! The row is written before the process starts (ADR 0005). Every change goes to
//! SQLite and to every connected app client as [`Event::AgentChanged`].

use std::collections::{BTreeMap, HashMap};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use serde_json::Value;
use swarm_core::adapters::{self, LaunchSpec, Session};
use swarm_core::agents::{StatusMerge, is_trust_prompt, new_session_id, new_token};
use swarm_core::git;
use swarm_core::prompts::{self, Harness, PromptFile};
use swarm_core::store::{NewAgent, NewMessage, Store, unix_millis};
use swarm_protocol::agent::render_inbox;
use swarm_protocol::{
    AgentId, AgentInfo, AgentKind, AgentLifecycle, AgentRole, AgentStatus, CreateTerminal, Event,
    MessageKind, ProjectId, TaskId, TaskInfo, TerminalId, TerminalKind,
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

pub(crate) struct AgentConfig {
    /// `<data>/agents/<id>/` holds each agent's files.
    pub(crate) data_dir: PathBuf,
    /// The agent socket, as `SWARM_SOCKET`.
    pub(crate) socket: PathBuf,
    /// `None` when there is no home folder: the defaults apply.
    pub(crate) settings: Option<PathBuf>,
    /// The `swarm` binary the hooks run.
    pub(crate) swarm: PathBuf,
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
    typing: UserTyping,
}

/// Follows what the user types into a worker's terminal.
#[derive(Default)]
struct UserTyping {
    typed: bool,
    told_at: Option<Instant>,
}

impl UserTyping {
    /// Returns true on the first Enter after typing, unless the orchestrator heard
    /// less than [`USER_INPUT_INTERVAL`] ago.
    fn observe(&mut self, input: &TerminalInput, now: Instant) -> bool {
        let (typed, enter) = match input {
            TerminalInput::Key(key) if matches!(key.name.as_str(), "enter" | "return") => {
                (false, true)
            }
            TerminalInput::Key(key) => (
                key.text.is_some() && !key.modifiers.control && !key.modifiers.command,
                false,
            ),
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
    /// One paste at a time, so two deliveries never interleave in a terminal.
    paste: Mutex<()>,
    /// Told when an agent's process ended.
    on_end: Mutex<Vec<EndObserver>>,
}

type EndObserver = Box<dyn Fn(AgentId) + Send + Sync>;

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
            paste: Mutex::new(()),
            on_end: Mutex::default(),
        })
    }

    /// Calls `observer` with the ID of every agent whose process ends.
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

        lock(&self.live).insert(
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
                typing: UserTyping::default(),
            },
        );

        Ok(info)
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

    /// Shows the agent as blocked with `text` until it works or the app types into it.
    pub(crate) fn notify(&self, id: AgentId, text: &str) {
        self.update(id, |live| live.merge.notify(text));
    }

    /// The app sent input to a terminal: a notice on the agent in it no longer applies,
    /// and the orchestrator hears when the user typed into a worker (ADR 0002).
    pub(crate) fn app_input(self: &Arc<Self>, terminal: TerminalId, input: &TerminalInput) {
        let typed_into = lock(&self.live)
            .values()
            .find(|agent| agent.info.terminal == Some(terminal))
            .map(|agent| (agent.info.id, agent.info.role));
        let Some((id, role)) = typed_into else {
            return;
        };

        self.update(id, |live| live.merge.app_input());

        if role != AgentRole::Worker {
            return;
        }
        let told = lock(&self.live)
            .get_mut(&id)
            .is_some_and(|live| live.typing.observe(input, Instant::now()));
        if told {
            self.user_typed(id);
        }
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

        if let (Some(message), Some(receiver)) = (start.first_message, ready) {
            self.type_when_ready(id, hub, message, receiver);
        }

        Ok(())
    }

    /// `PATH` with the folder of the `swarm` binary first, so agents can run `swarm`.
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

    /// Ends an agent that never ran, or never will.
    pub(crate) fn fail(self: &Arc<Self>, id: AgentId, message: &str) {
        let Some(mut agent) = lock(&self.live).remove(&id) else {
            return;
        };

        agent.merge.fail(message);
        agent.info.lifecycle = AgentLifecycle::Stopped;
        agent.info.ended_at = Some(unix_millis());
        self.publish(&mut agent);

        self.tell_worker_stopped(&agent.info, &format!("failed: {message}"));
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
        drop(live);

        if outcome.stopped {
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

        if matches!(output, TerminalOutput::ProgramStatus(report) if report.state == ProgramState::Idle)
        {
            self.deliver_unread(id);
        }
    }

    /// Pastes the agent's unread messages into its terminal when it is idle and its
    /// first message went in. They count as delivered once the paste reached the terminal.
    pub(crate) fn deliver_unread(self: &Arc<Self>, id: AgentId) {
        let terminal = {
            let live = lock(&self.live);
            let Some(agent) = live.get(&id) else {
                return;
            };

            let idle = matches!(agent.merge.status(), AgentStatus::Idle | AgentStatus::Done);
            if !agent.delivery_open || !idle {
                return;
            }

            agent.info.terminal
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
                let _one_paste = lock(&agents.paste);

                let typed = hub.send_input(TerminalInput::Paste(render_inbox(claim.messages())));
                sleep_for(ENTER_DELAY);

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

    fn exited(self: &Arc<Self>, id: AgentId, exit: Option<&ExitStatus>) {
        let Some(mut agent) = lock(&self.live).remove(&id) else {
            return;
        };

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

        let how = exit.map_or_else(|| "no exit status".to_owned(), exit_text);
        self.tell_worker_stopped(&agent.info, &format!("stopped, {how}"));

        for observer in lock(&self.on_end).iter() {
            observer(id);
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
}
