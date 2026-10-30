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
use swarm_core::prompts::{self, Harness, PromptFile};
use swarm_core::store::{NewAgent, Store};
use swarm_protocol::{
    AgentId, AgentInfo, AgentKind, AgentLifecycle, AgentRole, AgentStatus, CreateTerminal, Event,
    TaskInfo, TerminalKind,
};
use swarm_settings::Settings;
use swarm_term::{ExitStatus, ProgramState, Row, TerminalInput, TerminalOutput};

use crate::hub::{Clients, Extras, Hub, Terminals};
use crate::tasks::{TERMINAL_PALETTE, TERMINAL_SIZE};

/// How long the first message waits for a ready signal before it looks at the agent.
const FIRST_MESSAGE_PATIENCE: Duration = Duration::from_secs(60);

/// After `SessionStart`, how long to wait for an OSC 7501 report before going ahead.
const SESSION_START_GRACE: Duration = Duration::from_secs(2);

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

/// What the settings say about one agent, read when its task is made.
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    kind: AgentKind,
    role: AgentRole,
    model: String,
    command: String,
    skip_permissions: bool,
}

/// What it takes to start a registered agent.
pub(crate) struct Start {
    pub(crate) agent: AgentId,
    pub(crate) task: TaskInfo,
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
    live: Mutex<HashMap<AgentId, Live>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Agents {
    pub(crate) fn new(
        config: AgentConfig,
        store: Arc<Mutex<Store>>,
        terminals: Arc<Terminals>,
        clients: Arc<Clients>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            store,
            terminals,
            clients,
            live: Mutex::default(),
        })
    }

    /// Reads the settings. Call it before anything is created, so a broken settings
    /// file fails the request early.
    pub(crate) fn plan(&self, kind: AgentKind, role: AgentRole) -> Result<Plan> {
        let settings = match &self.config.settings {
            Some(path) => swarm_settings::load(path)?,
            None => Settings::default(),
        };

        Ok(Plan {
            kind,
            role,
            model: settings.agents.model(kind, role).to_owned(),
            command: settings.agents.cli(kind).command.clone(),
            skip_permissions: settings.agents.skip_permissions,
        })
    }

    /// Writes the agent's row and makes it known to the agent socket. Nothing runs yet.
    pub(crate) fn register(&self, task: &TaskInfo, plan: Plan) -> Result<AgentInfo> {
        let session_id = match plan.kind {
            AgentKind::ClaudeCode => Some(new_session_id().context("cannot make a session ID")?),
            AgentKind::Codex => None,
        };
        let token = new_token().context("cannot make a token")?;

        let info = lock(&self.store).add_agent(&NewAgent {
            task: task.id,
            role: plan.role,
            kind: plan.kind,
            model: &plan.model,
            session_id: session_id.as_deref(),
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
            },
        );

        Ok(info)
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

        let home = std::env::var_os("HOME").map(PathBuf::from);
        let command = adapters::find_command(
            &plan.command,
            std::env::var_os("PATH").as_deref(),
            home.as_deref(),
        )
        .ok_or_else(|| CommandNotFound(plan.command.clone()))?;

        let project_root = lock(&self.store)
            .project(start.task.project)?
            .map(|project| project.path)
            .context("the project is gone")?;

        let prompt = prompts::build(
            PromptFile::for_role(plan.role),
            &Harness {
                agent: id,
                task: start.task.id,
                task_name: &start.task.name,
                role: plan.role,
                socket: &self.config.socket,
            },
            self.config.settings.as_deref().and_then(Path::parent),
            &project_root,
        );

        let agent_dir = self.config.data_dir.join("agents").join(id.0.to_string());
        let launch = adapters::build(&LaunchSpec {
            kind: plan.kind,
            command: &command,
            model: &plan.model,
            prompt: &prompt,
            worktree: &start.task.worktree,
            skip_permissions: plan.skip_permissions,
            swarm: &self.config.swarm,
            agent_dir: &agent_dir,
            session: Session::New(session_id.as_deref()),
        });
        write_files(&agent_dir, &launch.files)?;

        let mut env = start.env;
        env.extend([
            (
                "SWARM_SOCKET".to_owned(),
                self.config.socket.to_string_lossy().into_owned(),
            ),
            ("SWARM_AGENT_ID".to_owned(), id.0.to_string()),
            ("SWARM_TASK".to_owned(), start.task.id.0.to_string()),
            ("SWARM_TOKEN".to_owned(), token),
        ]);

        let ready = start.first_message.as_ref().map(|_| flume::unbounded());
        if let Some((sender, _)) = &ready {
            self.update(id, |live| {
                live.ready = Some(sender.clone());

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

                    true
                });
            })),
            on_exit: Some(Box::new(move |exit| exited.exited(id, exit.as_ref()))),
            on_output: Some(Box::new(move |program_output| {
                output.output(id, program_output);
            })),
        };
        let request = CreateTerminal {
            kind: TerminalKind::Agent,
            command: Some(launch.argv),
            cwd: Some(start.task.worktree.clone()),
            size: TERMINAL_SIZE,
            palette: TERMINAL_PALETTE,
            task: Some(start.task.id.terminal_key()),
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

    /// Ends an agent that never ran, or never will.
    pub(crate) fn fail(&self, id: AgentId, message: &str) {
        let Some(mut agent) = lock(&self.live).remove(&id) else {
            return;
        };

        agent.merge.fail(message);
        agent.info.lifecycle = AgentLifecycle::Stopped;
        self.publish(&mut agent);
    }

    /// Applies a hook event from the agent socket.
    pub(crate) fn hook(&self, id: AgentId, token: &str, event: &Value) -> Result<(), Refused> {
        let mut live = lock(&self.live);
        let agent = live
            .get_mut(&id)
            .filter(|agent| same_token(&agent.token, token))
            .ok_or(Refused)?;

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

        Ok(())
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
            });

        if let Err(error) = spawned {
            log::error!("cannot start the first message thread: {error}");
        }
    }

    fn output(&self, id: AgentId, output: &TerminalOutput) {
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
    }

    fn exited(&self, id: AgentId, exit: Option<&ExitStatus>) {
        let Some(mut agent) = lock(&self.live).remove(&id) else {
            return;
        };

        agent.merge.process_exited(exit);
        agent.info.terminal = None;
        agent.info.lifecycle = if agent.session_id.is_some() && agent.session_confirmed {
            AgentLifecycle::Resumable
        } else {
            AgentLifecycle::Stopped
        };
        self.publish(&mut agent);
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

fn row_text(row: &Row) -> String {
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
