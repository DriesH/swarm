//! One hosted terminal and the clients attached to it.
//!
//! The daemon keeps both channel ends of every terminal, so a client that goes away
//! never stops it (ADR 0001). Clients only hold a [`ClientSink`] inside the hub.

use std::collections::HashMap;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use swarm_protocol::{CreateTerminal, Event, TerminalId, TerminalInfo, TerminalKind, encode};
use swarm_term::{ExitStatus, TerminalInput, TerminalOutput, channels};
use swarm_term_vt::{DEFAULT_SCROLLBACK_BYTES, LocalTerminal, SpawnOptions};

use crate::recovery::process_info;
use crate::store::Store;

/// One encoded frame, shared by every client that receives it.
pub(crate) type Frame = Arc<Vec<u8>>;

/// Frames a client may have queued before the daemon disconnects it.
pub(crate) const QUEUE_FRAMES: usize = 4096;

/// Scrollback lines in the snapshot a new terminal's creator gets.
pub(crate) const CREATE_HISTORY_LINES: u32 = 1000;

const EXIT_POLL: Duration = Duration::from_millis(25);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
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
}

impl ClientSink {
    pub(crate) fn new(id: u64, frames: flume::Sender<Frame>, stream: Option<UnixStream>) -> Self {
        Self { id, frames, stream }
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// Returns false when the client is gone or too slow.
    fn send(&self, frame: &Frame) -> bool {
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
            TerminalOutput::Update(_) | TerminalOutput::History { .. } | TerminalOutput::Bell => {}
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

    /// Reads the terminal's output until it exits, then ends the hub.
    fn run(
        &self,
        outputs: &flume::Receiver<TerminalOutput>,
        terminals: &Terminals,
        store: &Mutex<Store>,
    ) {
        let mut exit = None;
        while let Ok(output) = outputs.recv() {
            lock(&self.state).observe(&output);

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

        self.finish(exit, terminals, store);
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
        let id = lock(&self.store).insert_starting(&request)?;

        let (terminal_channels, emulator_channels) = channels();
        let options = SpawnOptions {
            command: request.command.clone(),
            cwd: request.cwd.clone(),
            env: Vec::new(),
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

        let hub = Arc::new(Hub {
            id,
            kind: request.kind,
            task: request.task,
            input: terminal_channels.input,
            state: Mutex::new(HubState {
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
                runner.run(&outputs, &terminals, &terminals.store);
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
}
