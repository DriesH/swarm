//! The emulator side of one terminal: a PTY and a libghostty terminal on their own
//! threads, talking to a view only through the channels of `swarm_term`.

mod emulator;
mod keys;
mod mouse;
mod osc7501;
mod pty;
mod terminal_loop;

use std::path::PathBuf;
use std::thread;

use swarm_term::{EmulatorChannels, Palette, Size};

use crate::emulator::{Emulator, Sinks};
use crate::terminal_loop::{Command, TerminalLoop};

/// About 10,000 lines of 200 columns.
pub const DEFAULT_SCROLLBACK_BYTES: usize = 32 << 20;

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("command not found: {0}")]
    CommandNotFound(String),
    #[error("cannot open the pty or start the process: {0}")]
    Pty(String),
    #[error("cannot start the emulator: {0}")]
    Emulator(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// The passwd login shell that runs when `SpawnOptions::command` is `None`.
pub fn login_shell() -> Option<PathBuf> {
    pty::login_shell()
}

pub struct SpawnOptions {
    /// `None` runs the user's login shell (passwd shell, not `$SHELL`) as a login shell.
    pub command: Option<Vec<String>>,
    pub cwd: Option<PathBuf>,
    /// Added on top of the allow-listed environment.
    pub env: Vec<(String, String)>,
    pub size: Size,
    pub palette: Palette,
    /// Ghostty's limit is in bytes, not lines.
    pub scrollback_bytes: usize,
}

/// Owns the threads of one running terminal. Dropping it kills the process.
pub struct LocalTerminal {
    pid: Option<u32>,
    commands: flume::Sender<Command>,
}

impl LocalTerminal {
    pub fn spawn(options: SpawnOptions, channels: EmulatorChannels) -> Result<Self, SpawnError> {
        let (commands, command_receiver) = flume::unbounded();

        let wake = commands.clone();
        let spawned = pty::Pty::spawn(&options, move |eof| {
            let command = if eof { Command::Eof } else { Command::Output };
            if wake.send(command).is_err() {
                log::debug!("terminal thread is gone, dropping a pty wake-up");
            }
        })?;
        let pid = spawned.pid;

        let (ready_sender, ready_receiver) = flume::bounded(1);
        let input = channels.input.clone();
        thread::Builder::new()
            .name("terminal".into())
            .spawn(move || {
                let pty::Spawned {
                    mut pty, output, ..
                } = spawned;
                let sinks = Sinks {
                    replies: pty.writer(),
                    events: channels.output.clone(),
                };
                let emulator = Emulator::new(
                    options.size,
                    &options.palette,
                    options.scrollback_bytes,
                    sinks,
                );
                match emulator {
                    Ok(emulator) => {
                        if ready_sender.send(Ok(())).is_err() {
                            log::debug!("spawn gave up before the terminal was ready");
                        }
                        TerminalLoop::new(emulator, pty, output, &channels).run(&command_receiver);
                    }
                    Err(error) => {
                        pty.kill_and_reap();
                        output.close();
                        let error = SpawnError::Emulator(format!("{error:#}"));
                        if ready_sender.send(Err(error)).is_err() {
                            log::debug!("spawn gave up before the terminal was ready");
                        }
                    }
                }
            })?;
        ready_receiver.recv().map_err(|_| {
            SpawnError::Emulator("the terminal thread ended before it was ready".into())
        })??;

        // Dropping it on an error shuts the terminal down.
        let terminal = Self { pid, commands };

        let forward = terminal.commands.clone();
        thread::Builder::new()
            .name("terminal-input".into())
            .spawn(move || {
                for message in &input {
                    if forward.send(Command::Input(message)).is_err() {
                        return;
                    }
                }
                if forward.send(Command::Shutdown).is_err() {
                    log::debug!("terminal already ended");
                }
            })?;

        Ok(terminal)
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }
}

impl Drop for LocalTerminal {
    fn drop(&mut self) {
        if self.commands.send(Command::Shutdown).is_err() {
            log::debug!("terminal already ended");
        }
    }
}
