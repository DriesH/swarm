//! One hosted terminal and the clients attached to it.
//!
//! The daemon keeps both channel ends of every terminal, so a client that goes away
//! never stops it (ADR 0001). Clients only hold a [`ClientSink`] inside the hub.

use std::collections::HashMap;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use swarm_core::store::{ProcessMeta, Store};
use swarm_protocol::{
    CreateTerminal, Event, GridSize, ReviewSource, TaskId, TerminalId, TerminalInfo, TerminalKind,
    encode,
};
use swarm_term::{ExitStatus, Size, TerminalInput, TerminalOutput, channels};
use swarm_term_vt::{DEFAULT_SCROLLBACK_BYTES, LocalTerminal, SpawnOptions};

use crate::recovery::process_info;

/// One encoded frame, shared by every client that receives it.
pub(crate) type Frame = Arc<Vec<u8>>;

/// Frames a client may have queued before the daemon disconnects it.
pub(crate) const QUEUE_FRAMES: usize = 4096;

/// Scrollback lines in the snapshot a new terminal's creator gets.
pub(crate) const CREATE_HISTORY_LINES: u32 = 1000;

const EXIT_POLL: Duration = Duration::from_millis(25);

/// Sees each output on the hub thread.
pub(crate) type OutputObserver = Box<dyn FnMut(&TerminalOutput) + Send>;

/// What a terminal needs beyond the client's [`CreateTerminal`].
#[derive(Default)]
pub(crate) struct Extras {
    pub(crate) env: Vec<(String, String)>,
    /// Runs once the process runs, before the daemon can notice its exit.
    pub(crate) on_started: Option<Box<dyn FnOnce(TerminalId) + Send>>,
    pub(crate) on_exit: Option<Box<dyn FnOnce(Option<ExitStatus>) + Send>>,
    /// Sees every output on the hub thread before clients get it.
    pub(crate) on_output: Option<OutputObserver>,
    /// Set for a process terminal an agent started.
    pub(crate) process: Option<ProcessMeta>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A grid size as the PTY wants it. The cell size only matters for pixel reports.
pub(crate) fn cell_size(grid: GridSize) -> Size {
    Size {
        cols: grid.cols,
        rows: grid.rows,
        cell_width: 8,
        cell_height: 16,
    }
}

pub(crate) fn encode_event(event: &Event) -> Option<Frame> {
    match encode(event) {
        Ok(frame) => Some(Arc::new(frame)),
        Err(error) => {
            log::error!("cannot encode {event:?}: {error}");
            None
        }
    }
}

/// The daemon's side of one client connection: a bounded queue of frames.
pub(crate) struct ClientSink {
    id: u64,
    frames: flume::Sender<Frame>,
    /// Closed when the queue is full, which ends the connection's reader too.
    stream: Option<UnixStream>,
    changes_running: AtomicBool,
    /// The `TaskDiff` requests of this client that run.
    diffs_running: Mutex<Vec<(TaskId, ReviewSource)>>,
    /// The head of the last diff sent to this client per task and source, so a new
    /// commit with the same changes (an amend, an empty commit) is still sent.
    diff_heads: Mutex<HashMap<(TaskId, ReviewSource), String>>,
}

/// Held while an `AgentChanges` request of one client runs.
pub(crate) struct ChangesGuard(Arc<ClientSink>);

impl Drop for ChangesGuard {
    fn drop(&mut self) {
        self.0.changes_running.store(false, Ordering::Release);
    }
}

/// Held while a `TaskDiff` request of one client runs for one task and source.
pub(crate) struct DiffGuard(Arc<ClientSink>, (TaskId, ReviewSource));

impl Drop for DiffGuard {
    fn drop(&mut self) {
        lock(&self.0.diffs_running).retain(|running| *running != self.1);
    }
}

impl ClientSink {
    pub(crate) fn new(id: u64, frames: flume::Sender<Frame>, stream: Option<UnixStream>) -> Self {
        Self {
            id,
            frames,
            stream,
            changes_running: AtomicBool::new(false),
            diffs_running: Mutex::default(),
            diff_heads: Mutex::default(),
        }
    }

    /// None while an earlier `AgentChanges` request of this client still runs.
    pub(crate) fn begin_changes(self: &Arc<Self>) -> Option<ChangesGuard> {
        self.changes_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            .then(|| ChangesGuard(Arc::clone(self)))
    }

    /// None while an earlier `TaskDiff` request of this client for the same task and
    /// source still runs. Other tasks and sources run side by side.
    pub(crate) fn begin_diff(
        self: &Arc<Self>,
        task: TaskId,
        source: ReviewSource,
    ) -> Option<DiffGuard> {
        let key = (task, source);

        let mut running = lock(&self.diffs_running);
        if running.contains(&key) {
            return None;
        }
        running.push(key);

        Some(DiffGuard(Arc::clone(self), key))
    }

    /// Whether a diff with `fingerprint` at `head` must go to this client, given the
    /// fingerprint it says it has. Records `head` when it must.
    pub(crate) fn needs_diff(
        &self,
        task: TaskId,
        source: ReviewSource,
        known: Option<u64>,
        fingerprint: u64,
        head: &str,
    ) -> bool {
        let mut heads = lock(&self.diff_heads);
        let same_head = heads.get(&(task, source)).is_some_and(|sent| sent == head);
        if known == Some(fingerprint) && same_head {
            return false;
        }

        heads.insert((task, source), head.to_owned());

        true
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// Returns false when the client is gone or too slow.
    pub(crate) fn send(&self, frame: &Frame) -> bool {
        match self.frames.try_send(Frame::clone(frame)) {
            Ok(()) => true,
            Err(flume::TrySendError::Full(_)) => {
                log::warn!("client {} is too slow, disconnecting it", self.id);
                if let Some(stream) = &self.stream
                    && let Err(error) = stream.shutdown(Shutdown::Both)
                {
                    log::debug!("client {} already closed: {error}", self.id);
                }

                false
            }
            Err(flume::TrySendError::Disconnected(_)) => false,
        }
    }

    pub(crate) fn send_event(&self, event: &Event) -> bool {
        encode_event(event).is_some_and(|frame| self.send(&frame))
    }
}

/// Every connected app client, for events that are not about one terminal.
#[derive(Default)]
pub(crate) struct Clients {
    sinks: Mutex<Vec<Arc<ClientSink>>>,
}

impl Clients {
    pub(crate) fn add(&self, sink: &Arc<ClientSink>) {
        lock(&self.sinks).push(Arc::clone(sink));
    }

    pub(crate) fn remove(&self, client: u64) {
        lock(&self.sinks).retain(|sink| sink.id() != client);
    }

    /// Sends the event to every client; clients that are gone or too slow are dropped.
    pub(crate) fn broadcast(&self, event: &Event) {
        let Some(frame) = encode_event(event) else {
            return;
        };

        lock(&self.sinks).retain(|sink| sink.send(&frame));
    }

    /// Stalls every broadcast until the guard drops.
    #[cfg(test)]
    pub(crate) fn stall(&self) -> MutexGuard<'_, Vec<Arc<ClientSink>>> {
        lock(&self.sinks)
    }
}

/// Which clients a frame goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Audience {
    /// Clients waiting for an attach; they count as attached afterwards.
    Waiting,
    Attached,
    /// Every client, for the end of the terminal.
    Everyone,
}

struct Subscriber {
    sink: Arc<ClientSink>,
    attached: bool,
}

#[derive(Default)]
struct HubState {
    process: Option<ProcessMeta>,
    title: Option<String>,
    cwd: Option<PathBuf>,
    exit: Option<ExitStatus>,
    /// The terminal ended: nobody can attach any more.
    closed: bool,
    subscribers: Vec<Subscriber>,
}

impl HubState {
    fn observe(&mut self, output: &TerminalOutput) {
        match output {
            TerminalOutput::Title(title) => self.title = Some(title.clone()),
            TerminalOutput::WorkingDirectory(directory) => {
                self.cwd = Some(path_from_osc7(directory));
            }
            TerminalOutput::Snapshot(snapshot) => {
                if let Some(title) = &snapshot.title {
                    self.title = Some(title.clone());
                }
                if let Some(directory) = &snapshot.working_directory {
                    self.cwd = Some(path_from_osc7(directory));
                }
            }
            TerminalOutput::Exited(status) => self.exit = Some(status.clone()),
            TerminalOutput::Update(_)
            | TerminalOutput::History { .. }
            | TerminalOutput::Bell
            | TerminalOutput::ProgramStatus(_) => {}
        }
    }

    fn deliver(&mut self, frame: &Frame, audience: Audience) {
        self.subscribers.retain_mut(|subscriber| {
            let wanted = match audience {
                Audience::Waiting => !subscriber.attached,
                Audience::Attached => subscriber.attached,
                Audience::Everyone => true,
            };
            if !wanted {
                return true;
            }

            if audience == Audience::Waiting {
                subscriber.attached = true;
            }

            subscriber.sink.send(frame)
        });
    }

    /// Returns false when the terminal already ended.
    fn wait_for_attach(&mut self, sink: &Arc<ClientSink>) -> bool {
        if self.closed {
            return false;
        }

        self.subscribers
            .retain(|subscriber| subscriber.sink.id() != sink.id());
        self.subscribers.push(Subscriber {
            sink: Arc::clone(sink),
            attached: false,
        });

        true
    }

    fn detach(&mut self, client: u64) {
        self.subscribers
            .retain(|subscriber| subscriber.sink.id() != client);
    }
}

/// `file://host/path%20x` or a plain path, as a path.
fn path_from_osc7(value: &str) -> PathBuf {
    let Some(rest) = value.strip_prefix("file://") else {
        return PathBuf::from(value);
    };
    let path = rest.find('/').map_or("", |slash| &rest[slash..]);

    let mut bytes = Vec::with_capacity(path.len());
    let mut input = path.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let hex: Vec<u8> = input.clone().take(2).collect();
            let decoded = std::str::from_utf8(&hex)
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok());
            if let Some(decoded) = decoded {
                bytes.push(decoded);
                input.nth(1);
                continue;
            }
        }
        bytes.push(byte);
    }

    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
}

pub(crate) struct Hub {
    id: TerminalId,
    kind: TerminalKind,
    task: Option<String>,
    pid: Option<u32>,
    input: flume::Sender<TerminalInput>,
    state: Mutex<HubState>,
}

impl Hub {
    pub(crate) fn id(&self) -> TerminalId {
        self.id
    }

    pub(crate) fn info(&self) -> TerminalInfo {
        let state = lock(&self.state);

        TerminalInfo {
            id: self.id,
            kind: self.kind,
            task: self.task.clone(),
            owner: state.process.as_ref().map(|process| process.owner),
            name: state
                .process
                .as_ref()
                .and_then(|process| process.name.clone()),
            // Frozen protocol field: a process terminal follows its tile.
            fixed_size: None,
            title: state.title.clone(),
            cwd: state.cwd.clone(),
            exit: state.exit.clone(),
        }
    }

    /// Marks the client as waiting and asks the terminal for a snapshot, which goes
    /// to every waiting client. Returns false when the terminal is gone.
    pub(crate) fn attach(&self, sink: &Arc<ClientSink>, history: u32) -> bool {
        if !lock(&self.state).wait_for_attach(sink) {
            return false;
        }

        self.input.send(TerminalInput::Snapshot { history }).is_ok()
    }

    pub(crate) fn detach(&self, client: u64) {
        lock(&self.state).detach(client);
    }

    pub(crate) fn send_input(&self, input: TerminalInput) -> bool {
        self.input.send(input).is_ok()
    }

    /// Sends the signal to the process group. Returns false when the terminal is gone
    /// or the signal cannot be sent.
    pub(crate) fn signal(&self, signal: libc::c_int) -> bool {
        if lock(&self.state).closed {
            return false;
        }
        let Some(group) = self.pid.and_then(|pid| libc::pid_t::try_from(pid).ok()) else {
            return false;
        };

        // SAFETY: killpg takes plain integers and has no memory effects.
        unsafe { libc::killpg(group, signal) == 0 }
    }

    /// Reads the terminal's output until it exits, then ends the hub.
    fn run(
        &self,
        outputs: &flume::Receiver<TerminalOutput>,
        terminals: &Terminals,
        store: &Mutex<Store>,
        on_exit: Option<Box<dyn FnOnce(Option<ExitStatus>) + Send>>,
        mut on_output: Option<OutputObserver>,
    ) {
        let mut exit = None;
        while let Ok(output) = outputs.recv() {
            lock(&self.state).observe(&output);
            if let Some(on_output) = &mut on_output {
                on_output(&output);
            }

            if let TerminalOutput::Exited(status) = output {
                exit = Some(status);
                break;
            }

            let audience = if matches!(output, TerminalOutput::Snapshot(_)) {
                Audience::Waiting
            } else {
                Audience::Attached
            };
            let event = Event::Output {
                id: self.id,
                output,
            };
            if let Some(frame) = encode_event(&event) {
                lock(&self.state).deliver(&frame, audience);
            }
        }

        self.finish(exit.clone(), terminals, store);

        if let Some(on_exit) = on_exit {
            on_exit(exit);
        }
    }

    fn finish(&self, exit: Option<ExitStatus>, terminals: &Terminals, store: &Mutex<Store>) {
        if let Err(error) = lock(store).mark_exited(self.id, exit.as_ref()) {
            log::error!("cannot mark terminal {} exited: {error:#}", self.id.0);
        }

        terminals.forget(self.id);

        let exited = exit.map(|status| Event::Output {
            id: self.id,
            output: TerminalOutput::Exited(status),
        });
        let closed = Event::Closed { id: self.id };

        let mut state = lock(&self.state);
        state.closed = true;
        for event in exited.iter().chain([&closed]) {
            if let Some(frame) = encode_event(event) {
                state.deliver(&frame, Audience::Everyone);
            }
        }
        state.subscribers.clear();

        log::info!("terminal {} ended", self.id.0);
    }
}

/// Every terminal this daemon run hosts.
pub(crate) struct Terminals {
    store: Arc<Mutex<Store>>,
    hubs: Mutex<HashMap<TerminalId, Arc<Hub>>>,
}

impl Terminals {
    pub(crate) fn new(store: Arc<Mutex<Store>>) -> Arc<Self> {
        Arc::new(Self {
            store,
            hubs: Mutex::default(),
        })
    }

    pub(crate) fn get(&self, id: TerminalId) -> Option<Arc<Hub>> {
        lock(&self.hubs).get(&id).cloned()
    }

    pub(crate) fn list(&self) -> Vec<TerminalInfo> {
        let mut terminals: Vec<TerminalInfo> =
            lock(&self.hubs).values().map(|hub| hub.info()).collect();
        terminals.sort_by_key(|info| info.id);

        terminals
    }

    fn forget(&self, id: TerminalId) {
        lock(&self.hubs).remove(&id);
    }

    pub(crate) fn detach_client(&self, client: u64) {
        let hubs: Vec<Arc<Hub>> = lock(&self.hubs).values().cloned().collect();
        for hub in hubs {
            hub.detach(client);
        }
    }

    /// Writes the row, starts the process and its hub thread. The row exists before
    /// the process does (ADR 0005).
    pub(crate) fn create(self: &Arc<Self>, request: CreateTerminal) -> Result<Arc<Hub>> {
        self.create_with(request, Extras::default())
    }

    pub(crate) fn create_with(
        self: &Arc<Self>,
        request: CreateTerminal,
        extras: Extras,
    ) -> Result<Arc<Hub>> {
        let id = lock(&self.store).insert_starting(&request)?;
        if let Some(process) = &extras.process
            && let Err(error) = lock(&self.store).set_process(id, process)
        {
            log::error!("cannot record the owner of terminal {}: {error}", id.0);
        }

        let (terminal_channels, emulator_channels) = channels();
        let options = SpawnOptions {
            command: request.command.clone(),
            cwd: request.cwd.clone(),
            env: extras.env,
            size: request.size,
            palette: request.palette,
            scrollback_bytes: DEFAULT_SCROLLBACK_BYTES,
        };

        let terminal = match LocalTerminal::spawn(options, emulator_channels) {
            Ok(terminal) => terminal,
            Err(error) => {
                if let Err(store_error) = lock(&self.store).mark_exited(id, None) {
                    log::error!("cannot mark terminal {} failed: {store_error:#}", id.0);
                }

                return Err(error).context("cannot start the terminal");
            }
        };

        let pid = terminal.pid();
        let started_us = pid
            .and_then(|pid| i32::try_from(pid).ok())
            .and_then(process_info)
            .map(|info| info.started_us);
        lock(&self.store).mark_running(id, pid, started_us)?;

        if let Some(on_started) = extras.on_started {
            on_started(id);
        }

        let hub = Arc::new(Hub {
            id,
            kind: request.kind,
            task: request.task,
            input: terminal_channels.input,
            pid,
            state: Mutex::new(HubState {
                process: extras.process,
                cwd: request.cwd,
                ..HubState::default()
            }),
        });
        lock(&self.hubs).insert(id, Arc::clone(&hub));

        let outputs = terminal_channels.output;
        let runner = Arc::clone(&hub);
        let terminals = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name(format!("hub-{}", id.0))
            .spawn(move || {
                runner.run(
                    &outputs,
                    &terminals,
                    &terminals.store,
                    extras.on_exit,
                    extras.on_output,
                );
                drop(terminal);
            });
        if let Err(error) = spawned {
            self.forget(id);

            return Err(error).context("cannot start the hub thread");
        }

        log::info!("terminal {} started, pid {pid:?}", id.0);

        Ok(hub)
    }

    pub(crate) fn kill_all(&self) {
        let hubs: Vec<Arc<Hub>> = lock(&self.hubs).values().cloned().collect();
        for hub in hubs {
            if !hub.send_input(TerminalInput::Kill) {
                log::debug!("terminal {} is already gone", hub.id().0);
            }
        }
    }

    /// Returns true when every terminal ended within `timeout`.
    pub(crate) fn wait_until_empty(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if lock(&self.hubs).is_empty() {
                return true;
            }
            thread::park_timeout(EXIT_POLL);
        }

        lock(&self.hubs).is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink(id: u64, capacity: usize) -> (Arc<ClientSink>, flume::Receiver<Frame>) {
        let (sender, receiver) = flume::bounded(capacity);

        (Arc::new(ClientSink::new(id, sender, None)), receiver)
    }

    fn frame(byte: u8) -> Frame {
        Arc::new(vec![byte])
    }

    #[test]
    fn a_waiting_client_gets_the_snapshot_and_becomes_attached() {
        let mut state = HubState::default();
        let (client, frames) = sink(1, 8);
        state.wait_for_attach(&client);

        state.deliver(&frame(1), Audience::Waiting);
        state.deliver(&frame(2), Audience::Attached);

        assert_eq!(frames.try_iter().count(), 2);
    }

    #[test]
    fn only_one_changes_request_runs_per_client() {
        let (client, _frames) = sink(1, 8);

        let first = client.begin_changes();
        assert!(first.is_some());
        assert!(client.begin_changes().is_none());

        drop(first);
        assert!(client.begin_changes().is_some());
    }

    #[test]
    fn one_diff_request_runs_per_client_task_and_source() {
        let (client, _frames) = sink(1, 8);
        let (task, other) = (TaskId(1), TaskId(2));

        let first = client.begin_diff(task, ReviewSource::Live);
        assert!(first.is_some());
        assert!(client.begin_diff(task, ReviewSource::Live).is_none());
        assert!(
            client.begin_diff(task, ReviewSource::Merged).is_some(),
            "another source"
        );
        assert!(client.begin_diff(other, ReviewSource::Live).is_some());
        assert!(client.begin_changes().is_some(), "another kind of request");

        drop(first);
        assert!(client.begin_diff(task, ReviewSource::Live).is_some());
    }

    #[test]
    fn a_waiting_client_gets_no_stream_output() {
        let mut state = HubState::default();
        let (client, frames) = sink(1, 8);
        state.wait_for_attach(&client);

        state.deliver(&frame(2), Audience::Attached);

        assert_eq!(frames.try_iter().count(), 0);
    }

    #[test]
    fn an_attached_client_gets_no_second_snapshot() {
        let mut state = HubState::default();
        let (client, frames) = sink(1, 8);
        state.wait_for_attach(&client);
        state.deliver(&frame(1), Audience::Waiting);

        state.deliver(&frame(1), Audience::Waiting);

        assert_eq!(frames.try_iter().count(), 1);
    }

    #[test]
    fn a_detached_client_gets_nothing() {
        let mut state = HubState::default();
        let (client, frames) = sink(1, 8);
        state.wait_for_attach(&client);
        state.deliver(&frame(1), Audience::Waiting);

        state.detach(1);
        state.deliver(&frame(2), Audience::Everyone);

        assert_eq!(frames.try_iter().count(), 1);
    }

    #[test]
    fn a_full_queue_removes_the_client() {
        let mut state = HubState::default();
        let (client, _frames) = sink(1, 1);
        state.wait_for_attach(&client);
        state.deliver(&frame(1), Audience::Waiting);

        state.deliver(&frame(2), Audience::Attached);
        state.deliver(&frame(3), Audience::Attached);

        assert!(state.subscribers.is_empty());
    }

    #[test]
    fn nobody_can_attach_after_the_end() {
        let mut state = HubState {
            closed: true,
            ..HubState::default()
        };
        let (client, _frames) = sink(1, 8);

        assert!(!state.wait_for_attach(&client));
    }

    #[test]
    fn title_and_directory_are_kept_for_the_list() {
        let mut state = HubState::default();

        state.observe(&TerminalOutput::Title("vim".into()));
        state.observe(&TerminalOutput::WorkingDirectory(
            "file://host/Users/me/My%20Project".into(),
        ));

        assert_eq!(state.title.as_deref(), Some("vim"));
        assert_eq!(state.cwd, Some(PathBuf::from("/Users/me/My Project")));
    }

    #[test]
    fn a_plain_path_is_kept_as_it_is() {
        assert_eq!(path_from_osc7("/tmp/a b"), PathBuf::from("/tmp/a b"));
    }

    #[test]
    fn a_broadcast_reaches_every_client() {
        let clients = Clients::default();
        let (first, first_frames) = sink(1, 8);
        let (second, second_frames) = sink(2, 8);
        clients.add(&first);
        clients.add(&second);

        clients.broadcast(&Event::Closed { id: TerminalId(1) });

        assert_eq!(first_frames.try_iter().count(), 1);
        assert_eq!(second_frames.try_iter().count(), 1);
    }

    #[test]
    fn a_removed_client_gets_no_broadcast() {
        let clients = Clients::default();
        let (client, frames) = sink(1, 8);
        clients.add(&client);

        clients.remove(1);
        clients.broadcast(&Event::Closed { id: TerminalId(1) });

        assert_eq!(frames.try_iter().count(), 0);
    }

    #[test]
    fn a_broadcast_drops_a_client_that_is_gone() {
        let clients = Clients::default();
        let (client, frames) = sink(1, 8);
        clients.add(&client);
        drop(frames);

        clients.broadcast(&Event::Closed { id: TerminalId(1) });

        assert!(lock(&clients.sinks).is_empty());
    }
}
