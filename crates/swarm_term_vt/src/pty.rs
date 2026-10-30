use std::ffi::{CStr, OsStr};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use swarm_term::{ExitStatus, Size};

use crate::{SpawnError, SpawnOptions};

const FALLBACK_SHELL: &str = "/bin/zsh";

/// Most bytes the reader buffers before it waits for the parser to catch up.
const MAX_PENDING: usize = 1 << 20;

/// Variables a terminal may inherit. Everything else is dropped, so markers like
/// `CLAUDE_CODE_*` from whoever started Swarm never leak into agents.
const KEEP_ENV: &[&str] = &["HOME", "USER", "LOGNAME", "PATH", "LANG", "TMPDIR"];

#[derive(Default)]
struct Pending {
    bytes: Vec<u8>,
    closed: bool,
}

/// PTY output waiting to be parsed. The reader appends and sends one wake-up;
/// the terminal thread takes everything at once.
#[derive(Default)]
pub(crate) struct Output {
    pending: Mutex<Pending>,
    space: Condvar,
    woken: AtomicBool,
}

impl Output {
    pub(crate) fn take(&self) -> Vec<u8> {
        self.woken.store(false, Ordering::Release);

        let bytes = std::mem::take(&mut self.lock().bytes);
        self.space.notify_one();

        bytes
    }

    /// Unblocks the reader for good once the terminal thread is done.
    pub(crate) fn close(&self) {
        self.lock().closed = true;
        self.space.notify_all();
    }

    /// Returns false when the output was closed.
    fn push(&self, bytes: &[u8]) -> bool {
        let mut pending = self.lock();
        while pending.bytes.len() >= MAX_PENDING && !pending.closed {
            pending = self
                .space
                .wait(pending)
                .unwrap_or_else(PoisonError::into_inner);
        }
        if pending.closed {
            return false;
        }

        pending.bytes.extend_from_slice(bytes);
        true
    }

    /// True if the caller should send a wake-up (none is in flight).
    fn needs_wake(&self) -> bool {
        !self.woken.swap(true, Ordering::AcqRel)
    }

    fn lock(&self) -> MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub(crate) struct Pty {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    input: flume::Sender<Vec<u8>>,
    /// The child is a session leader, so its pid is also its session and process group.
    pid: Option<libc::pid_t>,
    /// Once set, the pid may be reused, so nothing is signalled anymore.
    reaped: Option<ExitStatus>,
}

pub(crate) struct Spawned {
    pub(crate) pty: Pty,
    pub(crate) output: Arc<Output>,
    pub(crate) pid: Option<u32>,
}

impl Pty {
    /// Starts the command (or the login shell) with a clean environment. `on_output`
    /// runs on the reader thread when output is pending and no wake-up is in
    /// flight, and once more with `eof = true` when the PTY closes.
    pub(crate) fn spawn(
        options: &SpawnOptions,
        on_output: impl Fn(bool) + Send + 'static,
    ) -> Result<Spawned, SpawnError> {
        if let Some([program, ..]) = options.command.as_deref()
            && !command_exists(program, options.cwd.as_deref())
        {
            return Err(SpawnError::CommandNotFound(program.clone()));
        }

        let pair = native_pty_system()
            .openpty(pty_size(options.size))
            .map_err(pty_error)?;

        let shell = login_shell().unwrap_or_else(|| FALLBACK_SHELL.into());
        let mut builder = match options.command.as_deref() {
            Some([program, args @ ..]) => {
                let mut builder = CommandBuilder::new(program);
                builder.args(args);
                builder
            }
            // Default prog starts the `SHELL` from this env as a login shell.
            _ => CommandBuilder::new_default_prog(),
        };
        builder.env_clear();
        for key in KEEP_ENV {
            if let Some(value) = std::env::var_os(key) {
                builder.env(key, value);
            }
        }
        if std::env::var_os("LANG").is_none() {
            builder.env("LANG", "en_US.UTF-8");
        }
        builder.env("SHELL", &shell);
        builder.env("TERM", "xterm-256color");
        builder.env("COLORTERM", "truecolor");
        builder.env("TERM_PROGRAM", "swarm");
        for (key, value) in &options.env {
            builder.env(key, value);
        }

        match (&options.cwd, std::env::var_os("HOME")) {
            (Some(directory), _) => builder.cwd(directory),
            (None, Some(home)) => builder.cwd(home),
            (None, None) => {}
        }

        let child = pair.slave.spawn_command(builder).map_err(pty_error)?;
        let pid = child.process_id();
        // Drop our copy of the slave so the reader sees EOF when the child exits.
        drop(pair.slave);

        let (input, input_receiver) = flume::unbounded::<Vec<u8>>();
        let mut pty = Self {
            master: pair.master,
            child,
            input,
            pid: pid.and_then(|pid| libc::pid_t::try_from(pid).ok()),
            reaped: None,
        };

        match pty.start_threads(input_receiver, on_output) {
            Ok(output) => Ok(Spawned { pty, output, pid }),
            Err(error) => {
                pty.kill_and_reap();
                Err(error)
            }
        }
    }

    fn start_threads(
        &self,
        input_receiver: flume::Receiver<Vec<u8>>,
        on_output: impl Fn(bool) + Send + 'static,
    ) -> Result<Arc<Output>, SpawnError> {
        let mut reader = self.master.try_clone_reader().map_err(pty_error)?;
        let mut writer = self.master.take_writer().map_err(pty_error)?;

        let output = Arc::new(Output::default());
        let pending = Arc::clone(&output);
        thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || {
                let mut buffer = vec![0u8; 64 * 1024];
                while let Ok(read @ 1..) = reader.read(&mut buffer) {
                    // Blocks while the parser is behind, which throttles fast producers.
                    if !pending.push(&buffer[..read]) {
                        return;
                    }
                    if pending.needs_wake() {
                        on_output(false);
                    }
                }
                on_output(true);
            })?;

        thread::Builder::new()
            .name("pty-writer".into())
            .spawn(move || {
                for bytes in &input_receiver {
                    if let Err(error) = writer.write_all(&bytes).and_then(|()| writer.flush()) {
                        log::debug!("pty write failed: {error}");
                        break;
                    }
                }
            })?;

        Ok(output)
    }

    pub(crate) fn writer(&self) -> flume::Sender<Vec<u8>> {
        self.input.clone()
    }

    pub(crate) fn write(&self, bytes: Vec<u8>) {
        if self.input.send(bytes).is_err() {
            log::debug!("pty writer is gone, dropping input");
        }
    }

    pub(crate) fn resize(&self, size: Size) -> anyhow::Result<()> {
        self.master.resize(pty_size(size))
    }

    /// SIGHUP to the shell's process group and to the foreground process group.
    pub(crate) fn hangup(&self) {
        let Some(pid) = self.signalable_pid() else {
            return;
        };

        signal_group(pid, libc::SIGHUP);
        if let Some(foreground) = self.master.process_group_leader()
            && foreground != pid
        {
            signal_group(foreground, libc::SIGHUP);
        }
    }

    /// SIGKILL to every process in the child's session, also ones that left the
    /// process group or ignore SIGHUP.
    pub(crate) fn kill_session(&self) {
        let Some(pid) = self.signalable_pid() else {
            return;
        };

        for member in session_members(pid) {
            // SAFETY: kill takes plain integers and has no memory effects.
            if unsafe { libc::kill(member, libc::SIGKILL) } != 0 {
                log::debug!("kill({member}) failed: {}", std::io::Error::last_os_error());
            }
        }
        signal_process(pid, libc::SIGKILL);
    }

    fn signalable_pid(&self) -> Option<libc::pid_t> {
        self.pid.filter(|_| self.reaped.is_none())
    }

    /// Does not block. Returns the status once the child has exited.
    pub(crate) fn try_wait(&mut self) -> Option<ExitStatus> {
        if self.reaped.is_some() {
            return self.reaped.clone();
        }

        let status = match self.child.try_wait() {
            Ok(Some(status)) => exit_status(&status),
            Ok(None) => return None,
            Err(error) => {
                log::warn!("polling the child failed: {error}");
                ExitStatus::Code(1)
            }
        };
        self.reaped = Some(status.clone());

        Some(status)
    }

    pub(crate) fn wait(&mut self) -> ExitStatus {
        if let Some(status) = &self.reaped {
            return status.clone();
        }

        let status = match self.child.wait() {
            Ok(status) => exit_status(&status),
            Err(error) => {
                log::warn!("waiting for the child failed: {error}");
                ExitStatus::Code(1)
            }
        };
        self.reaped = Some(status.clone());

        status
    }

    pub(crate) fn kill_and_reap(&mut self) -> ExitStatus {
        self.kill_session();
        self.wait()
    }
}

fn exit_status(status: &portable_pty::ExitStatus) -> ExitStatus {
    match status.signal() {
        Some(name) => ExitStatus::Signal(name.to_owned()),
        None => ExitStatus::Code(status.exit_code()),
    }
}

fn pty_error(error: anyhow::Error) -> SpawnError {
    SpawnError::Pty(format!("{error:#}"))
}

fn signal_group(group: libc::pid_t, signal: libc::c_int) {
    // SAFETY: killpg takes plain integers and has no memory effects.
    if unsafe { libc::killpg(group, signal) } != 0 {
        log::debug!(
            "killpg({group}, {signal}) failed: {}",
            std::io::Error::last_os_error()
        );
    }
}

fn signal_process(pid: libc::pid_t, signal: libc::c_int) {
    // SAFETY: kill takes plain integers and has no memory effects.
    if unsafe { libc::kill(pid, signal) } != 0 {
        log::debug!(
            "kill({pid}, {signal}) failed: {}",
            std::io::Error::last_os_error()
        );
    }
}

/// Pids of all processes whose session id is `session`.
fn session_members(session: libc::pid_t) -> Vec<libc::pid_t> {
    // SAFETY: a null buffer with size 0 only asks for the number of processes.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let Ok(count) = usize::try_from(count) else {
        return Vec::new();
    };

    // Room for processes that start between the two calls.
    let mut pids: Vec<libc::pid_t> = vec![0; count + 64];
    let Ok(bytes) = libc::c_int::try_from(pids.len() * size_of::<libc::pid_t>()) else {
        return Vec::new();
    };
    // SAFETY: the buffer holds `bytes` bytes and lives for the whole call.
    let filled = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(usize::try_from(filled).unwrap_or(0));

    pids.retain(|pid| {
        // SAFETY: getsid takes a plain integer and has no memory effects.
        *pid > 0 && unsafe { libc::getsid(*pid) } == session
    });

    pids
}

/// The same lookup portable-pty does, so a missing program gets its own error.
fn command_exists(program: &str, cwd: Option<&Path>) -> bool {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return cwd
            .map_or_else(|| path.to_path_buf(), |cwd| cwd.join(path))
            .exists();
    }

    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| directory.join(program).is_file())
    })
}

fn pty_size(size: Size) -> PtySize {
    PtySize {
        cols: size.cols,
        rows: size.rows,
        pixel_width: size.cols.saturating_mul(size.cell_width),
        pixel_height: size.rows.saturating_mul(size.cell_height),
    }
}

/// The passwd login shell, not `$SHELL`: that one is whatever the process that
/// started Swarm had.
pub(crate) fn login_shell() -> Option<PathBuf> {
    // SAFETY: getpwuid returns null or a pointer to static storage; the string
    // is copied out before any other passwd call.
    let shell = unsafe {
        let entry = libc::getpwuid(libc::getuid());
        if entry.is_null() || (*entry).pw_shell.is_null() {
            None
        } else {
            let bytes = CStr::from_ptr((*entry).pw_shell).to_bytes();
            Some(PathBuf::from(OsStr::from_bytes(bytes)))
        }
    };

    shell.filter(|shell| !shell.as_os_str().is_empty())
}
