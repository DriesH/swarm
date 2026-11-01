//! Process terminals that agents start: who owns them, what they printed, and who
//! stopped them (ADR 0006). User and agent terminals are never in here, so no call can
//! reach them.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use swarm_core::store::{NewMessage, ProcessMeta, Store};
use swarm_protocol::agent::TerminalWaited;
use swarm_protocol::agent::{TerminalSummary, TerminalText, TerminalUntil};
use swarm_protocol::{
    AgentId, AgentInfo, AgentRole, CreateTerminal, Event, GridSize, MessageKind, ProjectId, TaskId,
    TerminalId, TerminalKind,
};
use swarm_term::{ExitStatus, TerminalInput, TerminalOutput};

use crate::agents::{Agents, row_text, terminal_key};
use crate::hub::{Clients, Extras, Terminals, cell_size};
use crate::messages::Messages;
use crate::tasks::TERMINAL_PALETTE;

/// The most lines `read` returns.
const MAX_READ_LINES: u32 = 2000;

/// Scrollback lines kept in memory for each terminal.
const KEPT_LINES: usize = 5000;

/// Ended terminals kept for `list` and `read` until the oldest ones go.
const KEPT_ENDED: usize = 32;

/// How long `kill` waits to see the process end before it answers.
const KILL_PATIENCE: Duration = Duration::from_millis(500);

/// How often a wait looks at its peer.
const PEER_CHECK: Duration = Duration::from_secs(1);

/// How long a process gets to end on SIGTERM when its owner ended.
const OWNER_ENDED_GRACE: Duration = Duration::from_secs(5);

const MAX_NAME_CHARS: usize = 60;

/// The size a process terminal starts with, until the app fits it to its tile.
const START_SIZE: GridSize = GridSize { cols: 80, rows: 12 };

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Who ended a process terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stopper {
    /// The process ended by itself.
    Process,
    /// An agent killed it: "orchestrator" or "worker api".
    Agent(String),
    User,
    /// Its owner's process ended, so Swarm stopped it.
    OwnerEnded,
    /// The user stopped its owner, so Swarm stopped it. Nobody is told.
    OwnerStopped,
}

/// The text of a terminal: scrollback and the screen, one string for each line.
#[derive(Default)]
struct Transcript {
    /// Scrollback by absolute line number. A burst can leave a hole until the lines
    /// are fetched.
    scrollback: BTreeMap<u64, String>,
    rows: Vec<String>,
    /// Number of the line after the last scrollback line.
    history_end: u64,
    /// Scrollback the emulator still has to send: `(first line, count)`.
    wanted: Vec<(u64, u32)>,
}

impl Transcript {
    fn apply(&mut self, output: &TerminalOutput) {
        match output {
            TerminalOutput::Snapshot(snapshot) => {
                self.history_end = snapshot.screen.history.end();
                let first = self
                    .history_end
                    .saturating_sub(snapshot.history_rows.len() as u64);
                self.scrollback = (first..)
                    .zip(snapshot.history_rows.iter().map(row_text))
                    .collect();
                self.rows = snapshot.rows.iter().map(row_text).collect();
                self.wanted.clear();
            }
            TerminalOutput::Update(update) => {
                let end = update.screen.history.end();

                if update.history_reset {
                    self.scrollback.clear();
                    self.wanted.clear();
                } else if end > self.history_end {
                    self.scroll(end);
                }
                self.history_end = end;

                self.rows
                    .resize(usize::from(update.screen.rows), String::new());
                for (index, row) in &update.rows {
                    if let Some(line) = self.rows.get_mut(usize::from(*index)) {
                        *line = row_text(row);
                    }
                }
            }
            TerminalOutput::History { from, rows } => {
                for (number, row) in (*from..).zip(rows) {
                    self.scrollback.insert(number, row_text(row));
                }
            }
            _ => {}
        }

        while self.scrollback.len() > KEPT_LINES {
            self.scrollback.pop_first();
        }
    }

    /// Lines up to `end` scrolled off the screen. The old rows are a first guess: an
    /// update carries no scrollback, and a row may have changed before it scrolled. The
    /// emulator is asked for the real lines.
    fn scroll(&mut self, end: u64) {
        let scrolled = end - self.history_end;
        let known =
            usize::try_from(scrolled).map_or(self.rows.len(), |lines| lines.min(self.rows.len()));

        for (number, row) in (self.history_end..).zip(self.rows.drain(..known)) {
            self.scrollback.insert(number, row);
        }

        let from = self.history_end.max(end.saturating_sub(KEPT_LINES as u64));
        let count = u32::try_from(end - from).unwrap_or(u32::MAX);
        self.wanted.push((from, count));
    }

    fn wants_history(&self) -> bool {
        !self.wanted.is_empty()
    }

    /// The scrollback requests to send to the emulator.
    fn take_wanted(&mut self) -> Vec<(u64, u32)> {
        std::mem::take(&mut self.wanted)
    }

    fn lines(&self) -> impl Iterator<Item = &str> {
        self.scrollback
            .values()
            .chain(&self.rows)
            .map(|line| line.trim_end())
    }

    /// The last `count` lines, or the screen, without trailing blank lines.
    fn last(&self, count: Option<u32>) -> String {
        let mut lines: Vec<&str> = match count {
            None => self.rows.iter().map(|line| line.trim_end()).collect(),
            Some(_) => self.lines().collect(),
        };
        while lines.last().is_some_and(|line| line.is_empty()) {
            lines.pop();
        }

        if let Some(count) = count {
            let keep = usize::try_from(count).unwrap_or(usize::MAX);
            let skip = lines.len().saturating_sub(keep);
            lines.drain(..skip);
        }

        lines.join("\n")
    }

    fn find(&self, pattern: &str) -> Option<String> {
        self.lines()
            .find(|line| line.contains(pattern))
            .map(str::to_owned)
    }
}

struct State {
    id: TerminalId,
    /// What the emulator reports, so it follows the tile.
    size: GridSize,
    text: Transcript,
    exit: Option<ExitStatus>,
    ended: bool,
    stopper: Stopper,
}

/// One process terminal an agent started.
struct Entry {
    owner: AgentId,
    project: ProjectId,
    task: Option<TaskId>,
    name: Option<String>,
    command: Vec<String>,
    state: Mutex<State>,
    changed: Condvar,
}

impl Entry {
    fn id(&self) -> TerminalId {
        lock(&self.state).id
    }

    fn in_scope(&self, caller: &AgentInfo) -> bool {
        self.project == caller.project && self.task == caller.task
    }

    fn is_oversight(&self, caller: &AgentInfo) -> bool {
        caller.role == AgentRole::Orchestrator && caller.task.is_some() && caller.task == self.task
    }

    /// What people and agents call it: the name, or the command.
    fn label(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.command.join(" "))
    }

    fn summary(&self, state: &State) -> TerminalSummary {
        TerminalSummary {
            id: state.id,
            name: self.name.clone(),
            command: self.command.clone(),
            owner: Some(self.owner),
            size: state.size,
            running: !state.ended,
            exit: state.exit.as_ref().map(exit_text),
        }
    }
}

/// How a message names the agent that did something: "orchestrator" or "worker api".
fn describe(agent: &AgentInfo) -> String {
    match agent.role {
        AgentRole::Orchestrator => "orchestrator".to_owned(),
        AgentRole::Worker => format!("worker {}", agent.name),
        AgentRole::StartOrchestrator => "the Start orchestrator".to_owned(),
    }
}

pub(crate) fn exit_text(exit: &ExitStatus) -> String {
    match exit {
        ExitStatus::Code(code) => format!("code {code}"),
        ExitStatus::Signal(name) => format!("signal {name}"),
    }
}

/// What `terminal_spawn` asks for.
pub(crate) struct Spawn {
    pub(crate) command: Vec<String>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) name: Option<String>,
    /// The task env (`SWARM_TASK`, `SWARM_PORT`); never the caller's token.
    pub(crate) env: Vec<(String, String)>,
}

pub(crate) struct Processes {
    store: Arc<Mutex<Store>>,
    terminals: Arc<Terminals>,
    clients: Arc<Clients>,
    messages: Arc<Messages>,
    agents: Arc<Agents>,
    entries: Mutex<HashMap<TerminalId, Arc<Entry>>>,
}

impl Processes {
    pub(crate) fn new(
        store: Arc<Mutex<Store>>,
        terminals: Arc<Terminals>,
        clients: Arc<Clients>,
        messages: Arc<Messages>,
        agents: Arc<Agents>,
    ) -> Arc<Self> {
        let processes = Arc::new(Self {
            store,
            terminals,
            clients,
            messages,
            agents: Arc::clone(&agents),
            entries: Mutex::default(),
        });

        let owner_gone = Arc::downgrade(&processes);
        agents.on_end(Box::new(move |agent, by_user| {
            if let Some(processes) = owner_gone.upgrade() {
                processes.owner_ended(agent, by_user);
            }
        }));

        processes
    }

    pub(crate) fn spawn(
        self: &Arc<Self>,
        caller: &AgentInfo,
        request: Spawn,
    ) -> Result<TerminalSummary> {
        if request
            .command
            .first()
            .is_none_or(|program| program.trim().is_empty())
        {
            bail!("the command is empty");
        }
        let name = request
            .name
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty());
        if name
            .as_deref()
            .is_some_and(|name| name.chars().count() > MAX_NAME_CHARS)
        {
            bail!("the name is longer than {MAX_NAME_CHARS} characters");
        }

        if self.agents.is_closing(caller.task) {
            bail!("the task is closing");
        }
        if self.agents.is_removing(caller.project) {
            bail!("the project is being removed");
        }

        let cwd = inside_worktree(caller, request.cwd)?;
        let entry = Arc::new(Entry {
            owner: caller.id,
            project: caller.project,
            task: caller.task,
            name: name.clone(),
            command: request.command.clone(),
            state: Mutex::new(State {
                id: TerminalId(0),
                size: START_SIZE,
                text: Transcript::default(),
                exit: None,
                ended: false,
                stopper: Stopper::Process,
            }),
            changed: Condvar::new(),
        });

        let (started, watched, finished) =
            (Arc::clone(&entry), Arc::clone(&entry), Arc::clone(&entry));
        let processes = Arc::clone(self);
        let terminals = Arc::clone(&self.terminals);
        let extras = Extras {
            env: request.env,
            on_started: Some(Box::new(move |id| lock(&started.state).id = id)),
            on_exit: Some(Box::new(move |exit| processes.ended(&finished, exit))),
            on_output: Some(Box::new(move |output| {
                let (id, wanted) = {
                    let mut state = lock(&watched.state);
                    state.text.apply(output);

                    if let Some(size) = screen_size(output) {
                        state.size = size;
                    }

                    (state.id, state.text.wants_history())
                };
                if wanted {
                    ask_for_history(&terminals, &watched, id);
                }
                watched.changed.notify_all();
            })),
            process: Some(ProcessMeta {
                owner: caller.id,
                name,
            }),
        };
        let terminal = CreateTerminal {
            kind: TerminalKind::Process,
            command: Some(request.command),
            cwd: Some(cwd),
            size: cell_size(START_SIZE),
            palette: TERMINAL_PALETTE,
            task: Some(terminal_key(caller)),
        };
        let hub = self
            .terminals
            .create_with(terminal, extras)
            .context("cannot start the terminal")?;

        {
            let mut entries = lock(&self.entries);
            entries.insert(hub.id(), Arc::clone(&entry));
            forget_oldest_ended(&mut entries);
        }
        self.clients.broadcast(&Event::TerminalChanged {
            terminal: hub.info(),
        });

        let state = lock(&entry.state);

        Ok(entry.summary(&state))
    }

    /// The process terminals of the caller's task.
    pub(crate) fn list(&self, caller: &AgentInfo) -> Vec<TerminalSummary> {
        let mut entries: Vec<Arc<Entry>> = lock(&self.entries)
            .values()
            .filter(|entry| entry.in_scope(caller))
            .cloned()
            .collect();
        entries.sort_by_key(|entry| entry.id());

        entries
            .iter()
            .map(|entry| entry.summary(&lock(&entry.state)))
            .collect()
    }

    /// The layout decides the size of a process terminal, so a request changes nothing:
    /// the answer is the current size.
    pub(crate) fn resize(&self, caller: &AgentInfo, id: TerminalId) -> Result<TerminalSummary> {
        let entry = self.visible(caller, id)?;
        if entry.owner != caller.id {
            bail!(
                "only the agent that started terminal {} can resize it",
                id.0
            );
        }

        let state = lock(&entry.state);

        Ok(entry.summary(&state))
    }

    pub(crate) fn read(
        &self,
        caller: &AgentInfo,
        id: TerminalId,
        lines: Option<u32>,
    ) -> Result<TerminalText> {
        let entry = self.watched(caller, id, "read")?;
        let lines = lines.map(|lines| lines.clamp(1, MAX_READ_LINES));

        let text = lock(&entry.state).text.last(lines);

        Ok(TerminalText { text })
    }

    pub(crate) fn wait(
        &self,
        caller: &AgentInfo,
        id: TerminalId,
        until: &TerminalUntil,
        timeout: Duration,
        gone: &dyn Fn() -> bool,
    ) -> Result<TerminalWaited> {
        let entry = self.watched(caller, id, "wait for")?;
        if matches!(until, TerminalUntil::Output(pattern) if pattern.is_empty()) {
            bail!("the text to wait for is empty");
        }

        let deadline = Instant::now() + timeout;
        let mut state = lock(&entry.state);

        loop {
            let (reached, line) = match until {
                TerminalUntil::Exit => (state.ended, None),
                TerminalUntil::Output(pattern) => {
                    let line = state.text.find(pattern);

                    (line.is_some(), line)
                }
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            if reached || state.ended || remaining.is_zero() || gone() {
                return Ok(TerminalWaited {
                    reached,
                    terminal: entry.summary(&state),
                    line,
                });
            }

            state = entry
                .changed
                .wait_timeout(state, remaining.min(PEER_CHECK))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    pub(crate) fn kill(
        &self,
        caller: &AgentInfo,
        id: TerminalId,
        signal: Option<&str>,
    ) -> Result<TerminalSummary> {
        let entry = self.watched(caller, id, "kill")?;
        let signal = parse_signal(signal.unwrap_or("TERM"))?;

        let mut state = lock(&entry.state);
        if state.ended {
            return Ok(entry.summary(&state));
        }

        if state.stopper == Stopper::Process {
            state.stopper = Stopper::Agent(describe(caller));
        }
        drop(state);

        let stopped = self
            .terminals
            .get(id)
            .is_some_and(|hub| hub.signal(signal) || hub.send_input(TerminalInput::Kill));
        if !stopped {
            log::debug!("terminal {} is already gone", id.0);
        }

        let deadline = Instant::now() + KILL_PATIENCE;
        let mut state = lock(&entry.state);
        while !state.ended {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }

            state = entry
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }

        Ok(entry.summary(&state))
    }

    /// The owner's process ended: stop the terminals it started. When the user stopped
    /// the owner, nobody hears of it.
    pub(crate) fn owner_ended(&self, owner: AgentId, by_user: bool) {
        let mine: Vec<Arc<Entry>> = lock(&self.entries)
            .values()
            .filter(|entry| entry.owner == owner)
            .cloned()
            .collect();

        for entry in mine {
            let id = {
                let mut state = lock(&entry.state);
                if state.ended {
                    continue;
                }
                if state.stopper == Stopper::Process {
                    state.stopper = if by_user {
                        Stopper::OwnerStopped
                    } else {
                        Stopper::OwnerEnded
                    };
                }

                state.id
            };

            let Some(hub) = self.terminals.get(id) else {
                continue;
            };
            if !hub.signal(libc::SIGTERM) {
                hub.send_input(TerminalInput::Kill);

                continue;
            }

            let spawned = std::thread::Builder::new()
                .name(format!("stop-terminal-{}", id.0))
                .spawn(move || {
                    let deadline = Instant::now() + OWNER_ENDED_GRACE;
                    let mut state = lock(&entry.state);
                    while !state.ended {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        if remaining.is_zero() {
                            hub.send_input(TerminalInput::Kill);

                            return;
                        }

                        state = entry
                            .changed
                            .wait_timeout(state, remaining)
                            .unwrap_or_else(PoisonError::into_inner)
                            .0;
                    }
                });
            if let Err(error) = spawned {
                log::error!("cannot watch terminal {} stop: {error}", id.0);
            }
        }
    }

    /// Drops the entries of a removed project, ended ones too.
    pub(crate) fn forget_project(&self, project: ProjectId) {
        lock(&self.entries).retain(|_, entry| entry.project != project);
    }

    /// The user closed the terminal in the app.
    pub(crate) fn stopped_by_user(&self, id: TerminalId) {
        let entry = lock(&self.entries).get(&id).cloned();

        if let Some(entry) = entry {
            let mut state = lock(&entry.state);
            if state.stopper == Stopper::Process {
                state.stopper = Stopper::User;
            }
        }
    }

    /// An entry the caller may see at all. A terminal of someone else, of another task
    /// or of another kind looks the same as one that does not exist.
    fn visible(&self, caller: &AgentInfo, id: TerminalId) -> Result<Arc<Entry>> {
        lock(&self.entries)
            .get(&id)
            .filter(|entry| entry.in_scope(caller))
            .cloned()
            .with_context(|| format!("no process terminal {} in your task", id.0))
    }

    /// An entry the caller may read, wait for and kill: its owner and the orchestrator.
    fn watched(&self, caller: &AgentInfo, id: TerminalId, action: &str) -> Result<Arc<Entry>> {
        let entry = self.visible(caller, id)?;
        if entry.owner != caller.id && !entry.is_oversight(caller) {
            bail!(
                "only the agent that started terminal {} or the task's orchestrator can {action} it",
                id.0
            );
        }

        Ok(entry)
    }

    fn ended(&self, entry: &Entry, exit: Option<ExitStatus>) {
        let (id, stopper) = {
            let mut state = lock(&entry.state);
            state.exit.clone_from(&exit);
            state.ended = true;

            (state.id, state.stopper.clone())
        };
        entry.changed.notify_all();

        if self.agents.is_closing(entry.task) || self.agents.is_removing(entry.project) {
            return;
        }

        let how = match &stopper {
            Stopper::Process => "exited".to_owned(),
            Stopper::Agent(who) => format!("stopped by {who}"),
            Stopper::User => "stopped by the user".to_owned(),
            Stopper::OwnerEnded | Stopper::OwnerStopped => "stopped, its owner ended".to_owned(),
        };
        let result = exit
            .as_ref()
            .map_or_else(|| "no exit status".to_owned(), exit_text);
        let text = format!("{} (terminal {}) {how}, {result}", entry.label(), id.0);

        for agent in self.recipients(entry, &stopper) {
            let posted = self.messages.post(&NewMessage {
                project: entry.project,
                task: entry.task,
                from: None,
                to: agent,
                kind: MessageKind::TerminalExited,
                text: &text,
                delivered: false,
            });
            match posted {
                Ok(_) => self.agents.deliver_unread(agent),
                Err(error) => {
                    log::error!(
                        "cannot tell agent {} that terminal {} ended: {error:#}",
                        agent.0,
                        id.0
                    );
                }
            }
        }
    }

    /// The owner, and the task's orchestrator when that is another agent. When the
    /// owner ended, only the orchestrator hears of it.
    fn recipients(&self, entry: &Entry, stopper: &Stopper) -> Vec<AgentId> {
        let mut recipients = match stopper {
            Stopper::OwnerStopped => return Vec::new(),
            Stopper::OwnerEnded => Vec::new(),
            _ => vec![entry.owner],
        };

        let Some(task) = entry.task else {
            return recipients;
        };
        match lock(&self.store).list_agents(task) {
            Ok(agents) => {
                let orchestrator = agents
                    .into_iter()
                    .find(|agent| agent.role == AgentRole::Orchestrator);
                if let Some(orchestrator) = orchestrator
                    && orchestrator.id != entry.owner
                {
                    recipients.push(orchestrator.id);
                }
            }
            Err(error) => {
                log::error!("cannot find the orchestrator of task {}: {error}", task.0);
            }
        }

        recipients
    }
}

/// Asks the emulator for the scrollback lines that scrolled past inside one update. The
/// answer arrives as an output and fills the transcript. Without the terminal yet, the
/// request stays for the next output.
fn ask_for_history(terminals: &Terminals, entry: &Entry, id: TerminalId) {
    let Some(hub) = terminals.get(id) else {
        return;
    };

    let wanted = lock(&entry.state).text.take_wanted();
    for (from, count) in wanted {
        if !hub.send_input(TerminalInput::FetchHistory { from, count }) {
            log::debug!("terminal {} ended before its history was fetched", id.0);
        }
    }
}

fn forget_oldest_ended(entries: &mut HashMap<TerminalId, Arc<Entry>>) {
    let mut ended: Vec<TerminalId> = entries
        .iter()
        .filter(|(_, entry)| lock(&entry.state).ended)
        .map(|(id, _)| *id)
        .collect();
    ended.sort();

    let surplus = ended.len().saturating_sub(KEPT_ENDED);
    for id in ended.into_iter().take(surplus) {
        entries.remove(&id);
    }
}

/// The grid of the screen an emulator output carries.
fn screen_size(output: &TerminalOutput) -> Option<GridSize> {
    let screen = match output {
        TerminalOutput::Snapshot(snapshot) => &snapshot.screen,
        TerminalOutput::Update(update) => &update.screen,
        _ => return None,
    };

    Some(GridSize {
        cols: screen.cols,
        rows: screen.rows,
    })
}

/// The folder to run in: the worktree, or a folder inside it. A link that leads out
/// of the worktree is refused, because the path is resolved first.
fn inside_worktree(caller: &AgentInfo, cwd: Option<PathBuf>) -> Result<PathBuf> {
    let worktree = caller
        .worktree
        .canonicalize()
        .with_context(|| format!("cannot use your worktree {}", caller.worktree.display()))?;

    let Some(cwd) = cwd else {
        return Ok(worktree);
    };
    let wanted = if cwd.is_absolute() {
        cwd
    } else {
        worktree.join(cwd)
    };
    let resolved = wanted
        .canonicalize()
        .with_context(|| format!("cannot use the folder {}", wanted.display()))?;

    if !resolved.starts_with(&worktree) {
        bail!(
            "the folder {} is outside your worktree {}",
            resolved.display(),
            worktree.display()
        );
    }

    Ok(resolved)
}

fn parse_signal(name: &str) -> Result<libc::c_int> {
    let upper = name.trim().to_ascii_uppercase();

    match upper.strip_prefix("SIG").unwrap_or(&upper) {
        "TERM" => Ok(libc::SIGTERM),
        "INT" => Ok(libc::SIGINT),
        "KILL" => Ok(libc::SIGKILL),
        "HUP" => Ok(libc::SIGHUP),
        _ => bail!("unknown signal {name:?}: use TERM, INT, KILL or HUP"),
    }
}

#[cfg(test)]
mod tests {
    use swarm_term::{
        Colors, Cursor, CursorStyle, History, Rgb, Row, Screen, Snapshot, TextFlags, TextSpan,
        Update,
    };

    use super::*;

    fn row(text: &str) -> Row {
        Row {
            bgs: Vec::new(),
            texts: vec![TextSpan {
                col: 0,
                cells: 0,
                text: text.to_owned(),
                fg: Rgb(0, 0, 0),
                flags: TextFlags::default(),
            }],
        }
    }

    fn screen(rows: u16, history_end: u64) -> Screen {
        Screen {
            cols: 80,
            rows,
            colors: Colors {
                fg: Rgb(0, 0, 0),
                bg: Rgb(0, 0, 0),
                cursor: None,
            },
            cursor: Cursor {
                col: 0,
                row: 0,
                style: CursorStyle::Block,
                visible: true,
            },
            alternate: false,
            history: History {
                start: 0,
                len: history_end,
            },
        }
    }

    fn snapshot(rows: &[&str], history: &[&str]) -> TerminalOutput {
        TerminalOutput::Snapshot(Snapshot {
            screen: screen(
                u16::try_from(rows.len()).expect("rows"),
                history.len() as u64,
            ),
            rows: rows.iter().map(|text| row(text)).collect(),
            history_rows: history.iter().map(|text| row(text)).collect(),
            title: None,
            working_directory: None,
        })
    }

    fn update(rows: u16, history_end: u64, changed: &[(u16, &str)]) -> TerminalOutput {
        TerminalOutput::Update(Update {
            screen: screen(rows, history_end),
            rows: changed
                .iter()
                .map(|(index, text)| (*index, row(text)))
                .collect(),
            history_reset: false,
        })
    }

    #[test]
    fn the_screen_is_the_default_and_trailing_blank_lines_are_trimmed() {
        let mut transcript = Transcript::default();
        transcript.apply(&snapshot(&["ready in 5ms", "", ""], &["old line"]));

        assert_eq!(transcript.last(None), "ready in 5ms");
        assert_eq!(transcript.last(Some(5)), "old line\nready in 5ms");
        assert_eq!(transcript.last(Some(1)), "ready in 5ms");
    }

    #[test]
    fn rows_that_scroll_off_move_into_the_scrollback() {
        let mut transcript = Transcript::default();
        transcript.apply(&snapshot(&["one", "two", "three"], &[]));

        transcript.apply(&update(3, 1, &[(1, "three"), (2, "four")]));

        assert_eq!(transcript.last(None), "two\nthree\nfour");
        assert_eq!(transcript.last(Some(10)), "one\ntwo\nthree\nfour");
    }

    #[test]
    fn a_burst_past_the_screen_asks_for_the_missed_lines() {
        let mut transcript = Transcript::default();
        transcript.apply(&snapshot(&["a", "b", "c"], &[]));

        transcript.apply(&update(3, 10, &[(0, "x"), (1, "y"), (2, "z")]));

        assert_eq!(transcript.take_wanted(), [(0, 10)]);
        let missed: Vec<Row> = (0..10)
            .map(|number| row(&format!("line {number}")))
            .collect();
        transcript.apply(&TerminalOutput::History {
            from: 0,
            rows: missed,
        });
        let text = transcript.last(Some(20));
        assert_eq!(
            text,
            "line 0\nline 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9\nx\ny\nz"
        );
        assert!(transcript.take_wanted().is_empty());
    }

    #[test]
    fn a_small_scroll_keeps_the_old_rows_until_the_real_lines_come() {
        let mut transcript = Transcript::default();
        transcript.apply(&snapshot(&["one", "two", "three"], &[]));

        transcript.apply(&update(3, 2, &[(0, "three"), (1, "four"), (2, "five")]));

        assert_eq!(transcript.last(Some(10)), "one\ntwo\nthree\nfour\nfive");
        assert_eq!(transcript.take_wanted(), [(0, 2)]);
    }

    #[test]
    fn a_snapshot_numbers_its_history_by_the_end_of_the_history() {
        let mut transcript = Transcript::default();
        transcript.apply(&snapshot(&["now"], &["old one", "old two"]));
        transcript.apply(&update(1, 3, &[(0, "next")]));

        assert_eq!(transcript.last(Some(10)), "old one\nold two\nnow\nnext");
    }

    #[test]
    fn a_wait_finds_a_line_on_screen_and_in_the_scrollback() {
        let mut transcript = Transcript::default();
        transcript.apply(&snapshot(&["now", ""], &["ready in 5ms"]));

        assert_eq!(transcript.find("ready in").as_deref(), Some("ready in 5ms"));
        assert_eq!(transcript.find("now").as_deref(), Some("now"));
        assert_eq!(transcript.find("missing"), None);
    }

    #[test]
    fn the_scrollback_is_bounded() {
        let mut transcript = Transcript::default();
        transcript.apply(&snapshot(&["a"], &[]));

        for line in 1..=(KEPT_LINES as u64 + 10) {
            transcript.apply(&update(1, line, &[(0, "x")]));
        }

        assert_eq!(transcript.scrollback.len(), KEPT_LINES);
    }

    #[test]
    fn the_size_follows_the_screen_of_the_last_output() {
        let grid = |output: &TerminalOutput| screen_size(output).map(|size| (size.cols, size.rows));

        assert_eq!(grid(&snapshot(&["a", "b"], &[])), Some((80, 2)));
        assert_eq!(grid(&update(9, 0, &[])), Some((80, 9)));
        assert_eq!(grid(&TerminalOutput::Bell), None);
    }

    #[test]
    fn signals_have_short_names() {
        assert_eq!(parse_signal("term").expect("TERM"), libc::SIGTERM);
        assert_eq!(parse_signal("SIGKILL").expect("KILL"), libc::SIGKILL);
        assert_eq!(parse_signal("INT").expect("INT"), libc::SIGINT);
        assert_eq!(parse_signal("hup").expect("HUP"), libc::SIGHUP);
        assert!(parse_signal("USR1").is_err());
    }

    #[test]
    fn exits_are_worded_for_people() {
        assert_eq!(exit_text(&ExitStatus::Code(1)), "code 1");
        assert_eq!(exit_text(&ExitStatus::Signal("15".to_owned())), "signal 15");
    }
}
