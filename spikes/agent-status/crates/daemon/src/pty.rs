use std::ffi::OsString;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use anyhow::{Context as _, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// Most bytes the reader buffers before it waits for the parser to catch up.
const MAX_PENDING: usize = 1 << 20;

/// Variables an agent may inherit from the daemon. Everything else is dropped,
/// so markers like `CLAUDE_CODE_*` from whoever started the daemon never leak.
const KEEP_ENV: &[&str] = &["HOME", "USER", "LOGNAME", "PATH", "LANG", "TMPDIR"];

/// PTY output waiting to be parsed. The reader appends and sends one wake-up;
/// the terminal thread takes everything at once.
#[derive(Default)]
pub struct Output {
    pending: Mutex<Vec<u8>>,
    space: Condvar,
    woken: AtomicBool,
}

impl Output {
    pub fn take(&self) -> Vec<u8> {
        self.woken.store(false, Ordering::Release);
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

    /// True if the caller should send a wake-up (none is in flight).
    fn needs_wake(&self) -> bool {
        !self.woken.swap(true, Ordering::AcqRel)
    }
}

pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    input: flume::Sender<Vec<u8>>,
}

pub struct Spawned {
    pub pty: Pty,
    pub output: Arc<Output>,
    pub pid: Option<u32>,
    pub cmd: String,
}

impl Pty {
    /// Starts `cmd` (or the login shell) with a clean environment. `on_output`
    /// runs on the reader thread when output is pending and no wake-up is in
    /// flight, and once more with `eof = true` when the PTY closes.
    pub fn spawn(
        cmd: Option<&[String]>,
        cwd: Option<&str>,
        env: &[(String, String)],
        size: PtySize,
        on_output: impl Fn(bool) + Send + 'static,
    ) -> Result<Spawned> {
        let pair = native_pty_system().openpty(size)?;

        let shell = login_shell();
        let mut builder = match cmd {
            Some([program, args @ ..]) => {
                let mut b = CommandBuilder::new(program);
                b.args(args);
                b
            }
            // Default prog: `$SHELL` from the builder's env, started as a login shell.
            _ => CommandBuilder::new_default_prog(),
        };
        builder.env_clear();
        for key in KEEP_ENV {
            if let Some(value) = std::env::var_os(key) {
                builder.env(key, value);
            }
        }
        builder.env("SHELL", &shell);
        builder.env("TERM", "xterm-256color");
        builder.env("COLORTERM", "truecolor");
        builder.env("TERM_PROGRAM", "swarm");
        for (key, value) in env {
            builder.env(key, value);
        }
        let home = std::env::var_os("HOME");
        match (cwd, home) {
            (Some(dir), _) => builder.cwd(dir),
            (None, Some(home)) => builder.cwd(home),
            (None, None) => {}
        }
        let cmd_label = match cmd {
            Some(argv) => argv.join(" "),
            None => format!("{} (login)", shell.to_string_lossy()),
        };

        let child = pair.slave.spawn_command(builder)?;
        let pid = child.process_id();
        // Drop our copy of the slave so the reader sees EOF when the child exits.
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let output = Arc::new(Output::default());
        let pending = output.clone();
        thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || {
                let mut buf = vec![0u8; 64 * 1024];
                while let Ok(n @ 1..) = reader.read(&mut buf) {
                    // Blocks while the parser is behind, which throttles fast producers.
                    pending.push(&buf[..n]);
                    if pending.needs_wake() {
                        on_output(false);
                    }
                }
                on_output(true);
            })
            .context("spawn pty reader")?;

        let mut writer = pair.master.take_writer()?;
        let (input, input_rx) = flume::unbounded::<Vec<u8>>();
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

        Ok(Spawned {
            pty: Self {
                master: pair.master,
                child,
                input,
            },
            output,
            pid,
            cmd: cmd_label,
        })
    }

    pub fn writer(&self) -> flume::Sender<Vec<u8>> {
        self.input.clone()
    }

    pub fn write(&self, bytes: Vec<u8>) {
        // Only fails after the writer thread exited, i.e. the child is gone.
        let _ = self.input.send(bytes);
    }

    pub fn resize(&self, size: PtySize) -> Result<()> {
        self.master.resize(size)
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }

    /// Exit code, or `None` when killed by a signal.
    pub fn wait(&mut self) -> Option<u32> {
        let status = self.child.wait().ok()?;
        status.signal().is_none().then(|| status.exit_code())
    }
}

/// The passwd login shell, not `$SHELL`: the daemon's `$SHELL` is whatever
/// the process that started it had.
fn login_shell() -> OsString {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: getpwuid returns null or a pointer to static storage; the string
    // is copied out before any other passwd call.
    let shell = unsafe {
        let ent = libc::getpwuid(libc::getuid());
        if ent.is_null() || (*ent).pw_shell.is_null() {
            None
        } else {
            let bytes = std::ffi::CStr::from_ptr((*ent).pw_shell).to_bytes();
            Some(std::ffi::OsStr::from_bytes(bytes).to_owned())
        }
    };
    shell
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/zsh".into())
}
