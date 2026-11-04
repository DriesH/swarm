//! Stops a running daemon from outside, so a daemon that speaks another protocol version
//! can be replaced. The process is found through the socket, not through a request.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::AsRawFd as _;
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

const DAEMON_NAME: &str = "swarm-daemon";
const POLL: Duration = Duration::from_millis(50);
/// SIGTERM gets 10 s: the daemon stops its terminals first.
const TERM_POLLS: u32 = 200;
const KILL_POLLS: u32 = 40;

#[derive(Debug, thiserror::Error)]
pub enum StopError {
    #[error("cannot find the daemon process: {0}")]
    Socket(io::Error),
    #[error("the process behind the socket is not {DAEMON_NAME}")]
    NotDaemon,
    #[error("cannot signal the daemon: {0}")]
    Signal(io::Error),
    #[error("the daemon did not stop")]
    Alive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Terminate,
    Kill,
}

/// What stopping a daemon needs from the system. Tests replace it.
pub trait Host: Send + Sync {
    /// The process on the other end of the socket.
    fn socket_owner(&self, socket: &Path) -> io::Result<u32>;

    fn executable(&self, pid: u32) -> Option<PathBuf>;

    fn is_running(&self, pid: u32) -> bool;

    fn signal(&self, pid: u32, signal: Signal) -> io::Result<()>;

    /// Waits one poll step.
    fn pause(&self);
}

/// The real system.
#[derive(Debug, Clone, Copy)]
pub struct System;

impl Host for System {
    fn socket_owner(&self, socket: &Path) -> io::Result<u32> {
        peer_pid(&UnixStream::connect(socket)?)
    }

    fn executable(&self, pid: u32) -> Option<PathBuf> {
        let mut buffer = vec![0_u8; usize::try_from(libc::PROC_PIDPATHINFO_MAXSIZE).ok()?];
        let size = u32::try_from(buffer.len()).ok()?;

        // SAFETY: proc_pidpath writes at most `size` bytes into `buffer`.
        let written = unsafe {
            libc::proc_pidpath(
                libc::c_int::try_from(pid).ok()?,
                buffer.as_mut_ptr().cast(),
                size,
            )
        };
        let written = usize::try_from(written)
            .ok()
            .filter(|written| *written > 0)?;

        buffer.truncate(written);

        Some(PathBuf::from(OsString::from_vec(buffer)))
    }

    fn is_running(&self, pid: u32) -> bool {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };

        // SAFETY: signal 0 only checks that the process exists.
        let result = unsafe { libc::kill(pid, 0) };

        result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    fn signal(&self, pid: u32, signal: Signal) -> io::Result<()> {
        let pid = libc::pid_t::try_from(pid)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let number = match signal {
            Signal::Terminate => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        };

        // SAFETY: kill takes plain integers.
        let result = unsafe { libc::kill(pid, number) };
        if result == 0 {
            return Ok(());
        }

        let error = io::Error::last_os_error();

        // Gone already is what we wanted.
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "runs on a background thread, never the UI thread"
    )]
    fn pause(&self) {
        thread::sleep(POLL);
    }
}

fn peer_pid(stream: &UnixStream) -> io::Result<u32> {
    let mut pid: libc::pid_t = 0;
    let mut length = libc::socklen_t::try_from(size_of::<libc::pid_t>())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;

    // SAFETY: the value pointer is valid for `length` bytes and both outlive the call.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&raw mut pid).cast(),
            &raw mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }

    u32::try_from(pid)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the socket has no peer"))
}

/// True when `path` is the daemon's program.
fn is_daemon_program(path: &Path) -> bool {
    path.file_name() == Some(OsStr::new(DAEMON_NAME))
}

/// Stops the daemon that listens on `socket`: SIGTERM, then SIGKILL after 10 s. Returns
/// once the process is gone. A socket with no daemon behind it is nothing to stop.
/// Refuses a process that is not `swarm-daemon`.
pub fn stop_daemon(socket: &Path, host: &dyn Host) -> Result<(), StopError> {
    let pid = match host.socket_owner(socket) {
        Ok(pid) => pid,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(());
        }
        Err(error) => return Err(StopError::Socket(error)),
    };

    let is_daemon = host
        .executable(pid)
        .is_some_and(|program| is_daemon_program(&program));
    if !is_daemon {
        return Err(StopError::NotDaemon);
    }

    host.signal(pid, Signal::Terminate)
        .map_err(StopError::Signal)?;
    if wait_until_gone(pid, host, TERM_POLLS) {
        return Ok(());
    }

    log::warn!("the daemon (pid {pid}) ignored SIGTERM, killing it");
    host.signal(pid, Signal::Kill).map_err(StopError::Signal)?;

    if wait_until_gone(pid, host, KILL_POLLS) {
        Ok(())
    } else {
        Err(StopError::Alive)
    }
}

fn wait_until_gone(pid: u32, host: &dyn Host, polls: u32) -> bool {
    for _ in 0..polls {
        if !host.is_running(pid) {
            return true;
        }

        host.pause();
    }

    !host.is_running(pid)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// A host with one process. SIGKILL ends it; SIGTERM only when `ends_on_term`.
    struct Fake {
        program: &'static str,
        refused: Option<io::ErrorKind>,
        ends_on_term: bool,
        signals: Mutex<Vec<(u32, Signal)>>,
        gone: AtomicBool,
    }

    impl Fake {
        fn new(program: &'static str, ends_on_term: bool) -> Self {
            Self {
                program,
                refused: None,
                ends_on_term,
                signals: Mutex::new(Vec::new()),
                gone: AtomicBool::new(false),
            }
        }

        fn sent(&self) -> Vec<(u32, Signal)> {
            self.signals.lock().expect("lock").clone()
        }
    }

    impl Host for Fake {
        fn socket_owner(&self, _: &Path) -> io::Result<u32> {
            match self.refused {
                Some(kind) => Err(kind.into()),
                None => Ok(4242),
            }
        }

        fn executable(&self, _: u32) -> Option<PathBuf> {
            Some(PathBuf::from(self.program))
        }

        fn is_running(&self, _: u32) -> bool {
            !self.gone.load(Ordering::Relaxed)
        }

        fn signal(&self, pid: u32, signal: Signal) -> io::Result<()> {
            self.signals.lock().expect("lock").push((pid, signal));

            if signal == Signal::Kill || self.ends_on_term {
                self.gone.store(true, Ordering::Relaxed);
            }

            Ok(())
        }

        fn pause(&self) {}
    }

    fn socket() -> &'static Path {
        Path::new("/nowhere/daemon.sock")
    }

    #[test]
    fn a_daemon_gets_sigterm_and_nothing_else_when_it_leaves() {
        let host = Fake::new("/Applications/Swarm.app/Contents/MacOS/swarm-daemon", true);

        let result = stop_daemon(socket(), &host);

        assert!(result.is_ok());
        assert_eq!(host.sent(), [(4242, Signal::Terminate)]);
    }

    #[test]
    fn a_daemon_that_ignores_sigterm_gets_sigkill() {
        let host = Fake::new("/x/swarm-daemon", false);

        let result = stop_daemon(socket(), &host);

        assert!(result.is_ok());
        assert_eq!(
            host.sent(),
            [(4242, Signal::Terminate), (4242, Signal::Kill)]
        );
    }

    #[test]
    fn a_process_that_is_not_the_daemon_gets_no_signal() {
        let host = Fake::new("/bin/zsh", true);

        let result = stop_daemon(socket(), &host);

        assert!(matches!(result, Err(StopError::NotDaemon)));
        assert!(host.sent().is_empty());
    }

    #[test]
    fn a_socket_nobody_listens_on_is_nothing_to_stop() {
        let mut host = Fake::new("/x/swarm-daemon", true);
        host.refused = Some(io::ErrorKind::ConnectionRefused);

        let result = stop_daemon(socket(), &host);

        assert!(result.is_ok());
        assert!(host.sent().is_empty());
    }

    #[test]
    fn the_real_system_says_this_test_is_not_the_daemon() {
        let program = System.executable(std::process::id());

        assert!(program.is_some_and(|program| !is_daemon_program(&program)));
    }

    #[test]
    fn the_real_system_sees_this_process_run() {
        assert!(System.is_running(std::process::id()));
    }

    #[test]
    fn a_listening_socket_names_this_process_as_its_peer() {
        let folder = std::env::temp_dir().join(format!("sdstop-{}", std::process::id()));
        std::fs::create_dir_all(&folder).expect("make the folder");
        let path = folder.join("s.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");

        let owner = System.socket_owner(&path);

        std::fs::remove_dir_all(&folder).ok();
        assert_eq!(owner.ok(), Some(std::process::id()));
    }
}
