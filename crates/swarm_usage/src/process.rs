//! Runs a command with a time limit. The child is always killed and reaped, with every
//! process it started.

use std::fmt;
use std::io::{ErrorKind, Write as _};
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

/// How often a running child is checked.
const POLL: Duration = Duration::from_millis(10);

/// How long the output may take to end after the command ended. A grandchild that
/// holds the pipe open must not keep the caller waiting.
const OUTPUT_GRACE: Duration = Duration::from_millis(250);

static SPAWNING: Mutex<()> = Mutex::new(());

/// Starts the command, one at a time. A child that another thread forks while the pipes
/// of this one are being made can inherit them and hold them open.
pub(crate) fn spawn(command: &mut Command) -> std::io::Result<Child> {
    let _one_at_a_time = SPAWNING.lock().unwrap_or_else(PoisonError::into_inner);

    command.spawn()
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("cannot start the command: {0}")]
    Spawn(std::io::Error),
    #[error("the command took too long")]
    Timeout,
    #[error("cannot talk to the command: {0}")]
    Io(std::io::Error),
}

/// A command that ended in time.
pub struct Finished {
    pub status: ExitStatus,
    /// At most `max_output` bytes.
    pub stdout: Vec<u8>,
}

/// The output can hold a secret, like the Keychain item, so `Debug` shows its size only.
impl fmt::Debug for Finished {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Finished")
            .field("status", &self.status)
            .field("stdout", &format_args!("{} bytes", self.stdout.len()))
            .finish()
    }
}

/// Kills and reaps the child when it goes out of scope, also on an early return. The
/// child leads its own process group, and the whole group is killed.
struct Reaper {
    child: Child,
    /// The child ended and was reaped already.
    reaped: bool,
}

impl Drop for Reaper {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }

        kill_group(self.child.id());
        if let Err(error) = self.child.kill() {
            log::debug!("cannot kill the command: {error}");
        }
        if let Err(error) = self.child.wait() {
            log::debug!("cannot reap the command: {error}");
        }
    }
}

/// Sends SIGKILL to the group that `leader` started, so a shell and the programs it
/// started die together. Call it before the leader is reaped: until then its ID cannot
/// belong to another process.
fn kill_group(leader: u32) {
    let Ok(group) = libc::pid_t::try_from(leader) else {
        return;
    };

    // SAFETY: `killpg` takes two integers and touches no memory.
    if unsafe { libc::killpg(group, libc::SIGKILL) } != 0 {
        log::debug!(
            "cannot kill the process group of the command: {}",
            std::io::Error::last_os_error()
        );
    }
}

/// Starts `command` with `input` on its stdin and waits at most `timeout` for it to end.
/// Its stderr is dropped, and only the first `max_output` bytes of stdout are kept.
///
/// The command leads a process group of its own. When it takes too long, everything in
/// that group is killed, not only the command.
pub fn run(
    command: &mut Command,
    input: &[u8],
    timeout: Duration,
    max_output: usize,
) -> Result<Finished, RunError> {
    let deadline = Instant::now() + timeout;

    let mut running = Reaper {
        child: spawn(
            command
                .process_group(0)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .map_err(RunError::Spawn)?,
        reaped: false,
    };
    let stdin = running.child.stdin.take();
    let stdout = running.child.stdout.take();

    // Both pipes get their own thread, so a command that reads nothing or never
    // finishes writing cannot hold this one past the deadline.
    if let Some(mut stdin) = stdin {
        let input = input.to_vec();
        thread::Builder::new()
            .name("run-stdin".into())
            .spawn(move || {
                if let Err(error) = stdin.write_all(&input) {
                    log::debug!("the command did not read its input: {error}");
                }
            })
            .map_err(RunError::Io)?;
    }

    let output = Arc::new(Mutex::new(Vec::new()));
    let (ended, output_ended) = mpsc::channel();
    if let Some(stdout) = stdout {
        let output = Arc::clone(&output);
        thread::Builder::new()
            .name("run-stdout".into())
            .spawn(move || {
                collect(stdout, &output, max_output);
                ended.send(()).ok();
            })
            .map_err(RunError::Io)?;
    }

    let status = loop {
        match running.child.try_wait() {
            Ok(Some(status)) => {
                running.reaped = true;

                break status;
            }
            Ok(None) if Instant::now() < deadline => thread::park_timeout(POLL),
            Ok(None) => return Err(RunError::Timeout),
            Err(error) => return Err(RunError::Io(error)),
        }
    };

    // Whatever has arrived counts, also when a grandchild keeps the pipe open.
    output_ended.recv_timeout(OUTPUT_GRACE).ok();
    let stdout = std::mem::take(&mut *output.lock().unwrap_or_else(PoisonError::into_inner));

    Ok(Finished { status, stdout })
}

/// Reads the pipe into `output`, at most `max` bytes.
fn collect(mut pipe: impl std::io::Read, output: &Mutex<Vec<u8>>, max: usize) {
    let mut chunk = [0_u8; 8192];

    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => return,
            Ok(count) => {
                let mut output = output.lock().unwrap_or_else(PoisonError::into_inner);
                let room = max.saturating_sub(output.len());
                output.extend_from_slice(&chunk[..count.min(room)]);

                if output.len() >= max {
                    return;
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => {
                log::debug!("cannot read the output of the command: {error}");

                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::fixtures::Scripts;

    fn sh(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(script);

        command
    }

    #[test]
    fn the_output_and_the_status_come_back() {
        let finished = run(
            &mut sh("cat; exit 3"),
            b"hello",
            Duration::from_secs(5),
            1024,
        )
        .expect("run");

        assert_eq!(finished.stdout, b"hello");
        assert_eq!(finished.status.code(), Some(3));
    }

    #[test]
    fn a_command_that_never_ends_times_out_and_is_reaped() {
        let scripts = Scripts::new();
        let pid_file = scripts.dir().join("pid");
        let script = format!("echo $$ > '{}'; exec sleep 30", pid_file.display());

        let started = Instant::now();
        let result = run(&mut sh(&script), b"", Duration::from_millis(300), 1024);

        assert!(matches!(result, Err(RunError::Timeout)));
        assert!(started.elapsed() < Duration::from_secs(5));
        let pid = std::fs::read_to_string(&pid_file).expect("pid");
        assert!(!Scripts::process_exists(pid.trim()), "the child still runs");
    }

    #[test]
    fn a_timeout_also_kills_the_programs_the_command_started() {
        let scripts = Scripts::new();
        let pid_file = scripts.dir().join("pid");
        // No `exec` on the outside: the shell stays and the pipeline runs under it.
        let script = format!(
            "sh -c 'echo $$ > \"$0\"; exec sleep 30' '{}' | cat",
            pid_file.display()
        );

        let result = run(&mut sh(&script), b"", Duration::from_secs(1), 1024);

        assert!(matches!(result, Err(RunError::Timeout)));
        let pid = std::fs::read_to_string(&pid_file).expect("pid");
        assert!(
            Scripts::wait_until_gone(pid.trim()),
            "the program the shell started still runs"
        );
    }

    #[test]
    fn the_debug_text_shows_the_size_of_the_output_and_not_the_output() {
        let finished = run(&mut sh("printf AB"), b"", Duration::from_secs(5), 1024).expect("run");

        let shown = format!("{finished:?}");

        assert!(shown.contains("stdout: 2 bytes"), "{shown}");
        assert!(!shown.contains("65"), "{shown}");
        assert!(!shown.contains("AB"), "{shown}");
    }

    #[test]
    fn a_command_that_reads_nothing_cannot_block_the_caller() {
        let big = vec![b'x'; 1 << 20];

        let result = run(
            &mut sh("exec sleep 30"),
            &big,
            Duration::from_millis(300),
            1024,
        );

        assert!(matches!(result, Err(RunError::Timeout)));
    }

    #[test]
    fn the_output_is_cut_at_the_limit() {
        let finished = run(
            &mut sh("yes | head -c 100000"),
            b"",
            Duration::from_secs(5),
            64,
        )
        .expect("run");

        assert_eq!(finished.stdout.len(), 64);
    }

    #[test]
    fn a_missing_program_is_a_spawn_error() {
        let mut command = Command::new(Path::new("/nonexistent/program"));

        let result = run(&mut command, b"", Duration::from_secs(1), 16);

        assert!(matches!(result, Err(RunError::Spawn(_))));
    }
}
