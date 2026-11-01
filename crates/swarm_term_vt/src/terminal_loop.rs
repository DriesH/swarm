//! The terminal thread: it owns the emulator (`!Send`) and the PTY, and reacts to
//! one command channel fed by the PTY reader and the view.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use swarm_term::{EmulatorChannels, ExitStatus, Size, TerminalInput, TerminalOutput};

use crate::emulator::Emulator;
use crate::pty::{Output, Pty};

/// While output keeps coming, send at most one update per this interval.
const FLUSH_INTERVAL: Duration = Duration::from_millis(8);
/// How long a process gets to react to SIGHUP before SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(3);
/// How often the child is checked for exit. EOF on the PTY is not enough: a
/// background job can keep the PTY open after the shell is gone.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Poll interval once the PTY reported EOF: the child is about to exit.
const EOF_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// How long to wait for the reader to hand over the last output after an exit.
const EXIT_DRAIN: Duration = Duration::from_millis(50);
/// How long history requests are still answered after the last update of an ended
/// process, so a reader can fetch lines that scrolled past in that update.
const EXIT_HISTORY: Duration = Duration::from_millis(100);
/// Scrollback lines in the first snapshot.
const SNAPSHOT_HISTORY_LINES: u32 = 1000;

pub(crate) enum Command {
    Output,
    Eof,
    Input(TerminalInput),
    /// The view dropped its channels, or the owner dropped the terminal.
    Shutdown,
}

pub(crate) struct TerminalLoop {
    emulator: Emulator,
    pty: Pty,
    output: Arc<Output>,
    outputs: flume::Sender<TerminalOutput>,
    dirty: bool,
    eof: bool,
    kill_deadline: Option<Instant>,
    next_flush: Instant,
    next_poll: Instant,
    /// Set while synchronized output holds back a flush.
    hold_until: Option<Instant>,
}

impl TerminalLoop {
    pub(crate) fn new(
        emulator: Emulator,
        pty: Pty,
        output: Arc<Output>,
        channels: &EmulatorChannels,
    ) -> Self {
        let now = Instant::now();

        Self {
            emulator,
            pty,
            output,
            outputs: channels.output.clone(),
            dirty: false,
            eof: false,
            kill_deadline: None,
            next_flush: now,
            next_poll: now + POLL_INTERVAL,
            hold_until: None,
        }
    }

    /// Sends the first snapshot, runs until the process exits, then reports it.
    pub(crate) fn run(mut self, commands: &flume::Receiver<Command>) {
        let result = self
            .send_snapshot(SNAPSHOT_HISTORY_LINES)
            .and_then(|()| self.event_loop(commands));
        let status = match result {
            Ok(status) => status,
            Err(error) => {
                log::error!("terminal failed: {error:#}");
                self.pty.kill_and_reap()
            }
        };

        self.send(TerminalOutput::Exited(status));
        self.output.close();
    }

    fn send_snapshot(&mut self, history: u32) -> Result<()> {
        let (update, snapshot) = self.emulator.snapshot(history)?;
        if let Some(update) = update {
            self.send(TerminalOutput::Update(update));
        }
        self.send(TerminalOutput::Snapshot(snapshot));
        // Reading history moved the viewport; the next flush absorbs it.
        self.dirty = true;

        Ok(())
    }

    fn event_loop(&mut self, commands: &flume::Receiver<Command>) -> Result<ExitStatus> {
        loop {
            match commands.recv_deadline(self.wake_time()) {
                Ok(command) => self.handle(command),
                Err(flume::RecvTimeoutError::Timeout) => {}
                Err(flume::RecvTimeoutError::Disconnected) => return Ok(self.pty.kill_and_reap()),
            }

            let now = Instant::now();
            if now >= self.next_poll {
                self.next_poll = now
                    + if self.eof {
                        EOF_POLL_INTERVAL
                    } else {
                        POLL_INTERVAL
                    };

                if let Some(status) = self.pty.try_wait() {
                    self.finish(commands)?;
                    return Ok(status);
                }
            }

            if self.kill_deadline.is_some_and(|deadline| now >= deadline) {
                self.pty.kill_session();
                self.kill_deadline = None;
                self.next_poll = now;
            }

            // The first change after a quiet spell goes out at once (low echo
            // latency); later ones are batched to one update per interval.
            if self.dirty && now >= self.next_flush {
                if let Some(until) = self.emulator.sync_hold(now)? {
                    self.hold_until = Some(until);
                } else {
                    self.flush()?;
                    self.next_flush = now + FLUSH_INTERVAL;
                }
            }
        }
    }

    fn wake_time(&self) -> Instant {
        let mut wake = self.next_poll;
        if let Some(deadline) = self.kill_deadline {
            wake = wake.min(deadline);
        }
        if self.dirty {
            wake = wake.min(self.hold_until.unwrap_or(self.next_flush));
        }

        wake
    }

    /// Takes the output that is still buffered after the child exited.
    fn finish(&mut self, commands: &flume::Receiver<Command>) -> Result<()> {
        let deadline = Instant::now() + EXIT_DRAIN;
        while !self.eof
            && let Ok(command) = commands.recv_deadline(deadline)
        {
            match command {
                Command::Output => self.take_output(),
                Command::Eof => {
                    self.eof = true;
                    self.take_output();
                }
                Command::Input(_) | Command::Shutdown => {}
            }
        }

        self.take_output();
        let changed = self.dirty;
        self.flush()?;

        if changed {
            self.answer_history(commands);
        }

        Ok(())
    }

    fn answer_history(&mut self, commands: &flume::Receiver<Command>) {
        let deadline = Instant::now() + EXIT_HISTORY;
        while let Ok(command) = commands.recv_deadline(deadline) {
            if let Command::Input(input @ TerminalInput::FetchHistory { .. }) = command
                && let Err(error) = self.handle_input(input)
            {
                log::warn!("history after exit failed: {error:#}");
            }
        }
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Output => self.take_output(),
            Command::Eof => {
                self.eof = true;
                self.take_output();
                self.next_poll = Instant::now();
            }
            Command::Input(input) => {
                if let Err(error) = self.handle_input(input) {
                    log::warn!("terminal input failed: {error:#}");
                }
            }
            Command::Shutdown => self.terminate(),
        }
    }

    fn take_output(&mut self) {
        let bytes = self.output.take();

        let statuses = self.emulator.scan_status(&bytes);
        if !statuses.is_empty() {
            // The screen the earlier output drew goes out before the status.
            if let Err(error) = self.flush() {
                log::warn!("flush before a status failed: {error:#}");
            }
            for status in statuses {
                self.send(TerminalOutput::ProgramStatus(status));
            }
        }

        self.emulator.write(&bytes);
        self.dirty = true;
    }

    fn handle_input(&mut self, input: TerminalInput) -> Result<()> {
        match input {
            TerminalInput::Key(key) => {
                let bytes = self.emulator.encode_key(&key);
                if !bytes.is_empty() {
                    self.pty.write(bytes);
                }
            }
            TerminalInput::Text(text) => self.pty.write(text.into_bytes()),
            TerminalInput::Paste(text) => {
                let bytes = self.emulator.encode_paste(&text)?;
                self.pty.write(bytes);
            }
            TerminalInput::Resize(size) => {
                let size = Size {
                    cols: size.cols.max(1),
                    rows: size.rows.max(1),
                    ..size
                };
                if size != self.emulator.size() {
                    self.pty.resize(size)?;
                    self.emulator.resize(size)?;
                    self.dirty = true;
                }
            }
            TerminalInput::Palette(palette) => {
                self.emulator.set_palette(&palette)?;
                self.dirty = true;
            }
            TerminalInput::FetchHistory { from, count } => {
                // Updates first, so the view sees them before the history answer.
                self.flush()?;
                let (from, rows) = self.emulator.history_rows(from, count)?;
                self.send(TerminalOutput::History { from, rows });
                self.dirty = true;
            }
            TerminalInput::Scroll {
                lines,
                column,
                row,
                keys,
            } => {
                let bytes = self.emulator.encode_scroll(lines, column, row, keys)?;

                if !bytes.is_empty() {
                    self.pty.write(bytes);
                }
            }
            TerminalInput::Focus(focused) => {
                if let Some(bytes) = self.emulator.focus_report(focused)? {
                    self.pty.write(bytes);
                }
            }
            TerminalInput::Snapshot { history } => self.send_snapshot(history)?,
            TerminalInput::Kill => self.terminate(),
        }

        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.dirty = false;
        self.hold_until = None;
        if let Some(update) = self.emulator.flush()? {
            self.send(TerminalOutput::Update(update));
        }

        Ok(())
    }

    fn send(&mut self, output: TerminalOutput) {
        if self.outputs.send(output).is_err() {
            self.terminate();
        }
    }

    /// SIGHUP now, SIGKILL for the whole session after the grace period.
    fn terminate(&mut self) {
        if self.kill_deadline.is_some() {
            return;
        }

        self.pty.hangup();
        self.kill_deadline = Some(Instant::now() + KILL_GRACE);
        // The child may already be gone.
        self.next_poll = Instant::now();
    }
}
