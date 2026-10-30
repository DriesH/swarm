use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use anyhow::{Context as _, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// Most bytes the reader buffers before it waits for the UI to catch up.
const MAX_PENDING: usize = 1 << 20;

/// PTY output waiting to be parsed. The reader appends and sends one wake-up;
/// the UI takes everything at once, so many small reads cost one main-thread hop.
#[derive(Default)]
pub struct Output {
    pending: Mutex<Vec<u8>>,
    space: Condvar,
}

impl Output {
    pub fn take(&self) -> Vec<u8> {
        let bytes = std::mem::take(&mut *self.pending.lock().unwrap_or_else(|e| e.into_inner()));
        self.space.notify_one();
        bytes
    }

    fn push(&self, bytes: &[u8]) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        while pending.len() >= MAX_PENDING {
            pending = self.space.wait(pending).unwrap_or_else(|e| e.into_inner());
        }
        pending.extend_from_slice(bytes);
    }
}

/// A shell running in a PTY.
pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    pub input: flume::Sender<Vec<u8>>,
}

impl Pty {
    /// Returns the PTY, its output buffer, and a channel that wakes when output
    /// is pending. The channel closes when the shell exits.
    pub fn spawn(size: PtySize) -> Result<(Self, Arc<Output>, flume::Receiver<()>)> {
        let pair = native_pty_system().openpty(size)?;

        // Default prog = $SHELL (or the passwd shell) started as a login shell.
        let mut cmd = CommandBuilder::new_default_prog();
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        if let Some(home) = std::env::var_os("HOME") {
            cmd.cwd(home);
        }
        let child = pair.slave.spawn_command(cmd)?;
        // Drop our copy of the slave so the reader sees EOF when the shell exits.
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let output = Arc::new(Output::default());
        let (wake_tx, wake_rx) = flume::bounded(1);
        let pending = output.clone();
        thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || {
                let mut buf = vec![0u8; 64 * 1024];
                while let Ok(n @ 1..) = reader.read(&mut buf) {
                    // Blocks while the UI is behind, which throttles fast producers like `cat`.
                    pending.push(&buf[..n]);
                    if let Err(flume::TrySendError::Disconnected(())) = wake_tx.try_send(()) {
                        break;
                    }
                }
            })
            .context("spawn pty reader")?;

        let mut writer = pair.master.take_writer()?;
        let (input_tx, input_rx) = flume::unbounded::<Vec<u8>>();
        thread::Builder::new()
            .name("pty-writer".into())
            .spawn(move || {
                for bytes in input_rx.iter() {
                    if writer
                        .write_all(&bytes)
                        .and_then(|()| writer.flush())
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .context("spawn pty writer")?;

        Ok((
            Self {
                master: pair.master,
                child,
                input: input_tx,
            },
            output,
            wake_rx,
        ))
    }

    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        // Only fails after the writer thread exited, i.e. the shell is gone.
        let _ = self.input.send(bytes.into());
    }

    pub fn resize(&self, size: PtySize) -> Result<()> {
        self.master.resize(size)
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}
