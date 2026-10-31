//! Runs a program with a deadline and captures what it prints.

use std::io::{Read, Write as _};
use std::os::unix::process::CommandExt as _;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

/// Output beyond this is dropped from the front, so the tail is what is kept.
const KEPT_BYTES: usize = 4 << 20;

/// How long to wait for the output after the program ended. A program that left a
/// background process behind keeps its pipes open.
const OUTPUT_GRACE: Duration = Duration::from_secs(2);

const POLL: Duration = Duration::from_millis(20);

pub(crate) struct Captured {
    /// `None` when the deadline killed the program.
    pub(crate) status: Option<ExitStatus>,
    pub(crate) stdout: Vec<u8>,
    /// Empty when the output was merged into `stdout`.
    pub(crate) stderr: Vec<u8>,
}

impl Captured {
    pub(crate) fn success(&self) -> bool {
        self.status.is_some_and(|status| status.success())
    }
}

type Buffer = Arc<Mutex<Vec<u8>>>;

/// Starts `command` in its own process group, feeds it `input` and reads its output.
/// With `merge_output`, stderr goes to the same pipe as stdout, in the order printed.
/// After `timeout` the whole process group is killed.
pub(crate) fn capture(
    mut command: Command,
    input: Option<&[u8]>,
    timeout: Duration,
    merge_output: bool,
) -> std::io::Result<Captured> {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .process_group(0);

    let merged = if merge_output {
        let (reader, writer) = std::io::pipe()?;
        command.stdout(writer.try_clone()?).stderr(writer);

        Some(reader)
    } else {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());

        None
    };

    let mut child = command.spawn()?;
    // The command holds the write end of a merged pipe; the reader sees the end only
    // when it is gone.
    drop(command);

    let (done, finished) = mpsc::channel();
    let (stdout, stderr) = (Buffer::default(), Buffer::default());
    let mut readers = 0;

    if let Some(reader) = merged {
        read_in_background(reader, &stdout, &done);
        readers += 1;
    } else {
        if let Some(pipe) = child.stdout.take() {
            read_in_background(pipe, &stdout, &done);
            readers += 1;
        }
        if let Some(pipe) = child.stderr.take() {
            read_in_background(pipe, &stderr, &done);
            readers += 1;
        }
    }
    drop(done);

    if let (Some(bytes), Some(mut pipe)) = (input, child.stdin.take()) {
        let bytes = bytes.to_vec();
        thread::spawn(move || {
            if let Err(error) = pipe.write_all(&bytes) {
                log::debug!("the program stopped reading its input: {error}");
            }
        });
    }

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }

        if Instant::now() >= deadline {
            kill_group(child.id());
            child.wait()?;

            break None;
        }

        thread::park_timeout(POLL);
    };

    let grace = Instant::now() + OUTPUT_GRACE;
    for _ in 0..readers {
        if finished
            .recv_timeout(grace.saturating_duration_since(Instant::now()))
            .is_err()
        {
            break;
        }
    }

    Ok(Captured {
        status,
        stdout: take(&stdout),
        stderr: take(&stderr),
    })
}

fn read_in_background(mut pipe: impl Read + Send + 'static, buffer: &Buffer, done: &Sender<()>) {
    let (buffer, done) = (Arc::clone(buffer), done.clone());

    thread::spawn(move || {
        let mut chunk = [0_u8; 16 << 10];

        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    let mut kept = buffer.lock().unwrap_or_else(PoisonError::into_inner);
                    kept.extend_from_slice(&chunk[..count]);

                    if kept.len() > KEPT_BYTES {
                        let surplus = kept.len() - KEPT_BYTES;
                        kept.drain(..surplus);
                    }
                }
            }
        }

        if done.send(()).is_err() {
            log::debug!("nobody waits for the output any more");
        }
    });
}

fn take(buffer: &Buffer) -> Vec<u8> {
    std::mem::take(&mut *buffer.lock().unwrap_or_else(PoisonError::into_inner))
}

fn kill_group(pid: u32) {
    let Ok(group) = libc::pid_t::try_from(pid) else {
        return;
    };

    // SAFETY: killpg takes plain integers and has no memory effects.
    unsafe {
        libc::killpg(group, libc::SIGKILL);
    }
}

/// The last `count` lines of `text`.
pub(crate) fn last_lines(text: &str, count: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let skip = lines.len().saturating_sub(count);

    lines[skip..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);

        command
    }

    #[test]
    fn stdout_and_stderr_are_kept_apart() {
        let captured = capture(
            shell("echo out; echo err >&2"),
            None,
            Duration::from_secs(10),
            false,
        )
        .expect("run");

        assert!(captured.success());
        assert_eq!(captured.stdout, b"out\n");
        assert_eq!(captured.stderr, b"err\n");
    }

    #[test]
    fn merged_output_keeps_the_order_printed() {
        let captured = capture(
            shell("echo one; echo two >&2; echo three"),
            None,
            Duration::from_secs(10),
            true,
        )
        .expect("run");

        assert_eq!(captured.stdout, b"one\ntwo\nthree\n");
        assert!(captured.stderr.is_empty());
    }

    #[test]
    fn the_input_reaches_stdin() {
        let captured = capture(
            shell("cat"),
            Some(b"hello body"),
            Duration::from_secs(10),
            false,
        )
        .expect("run");

        assert_eq!(captured.stdout, b"hello body");
    }

    #[test]
    fn input_larger_than_a_pipe_does_not_block() {
        let big = vec![b'x'; 1 << 20];

        let captured =
            capture(shell("cat"), Some(&big), Duration::from_secs(20), false).expect("run");

        assert_eq!(captured.stdout.len(), big.len());
    }

    #[test]
    fn a_failing_program_is_not_a_success() {
        let captured = capture(shell("exit 3"), None, Duration::from_secs(10), false).expect("run");

        assert!(!captured.success());
        assert_eq!(captured.status.and_then(|status| status.code()), Some(3));
    }

    #[test]
    fn the_deadline_kills_the_whole_group() {
        let start = Instant::now();

        let captured = capture(
            shell("echo started; sleep 30 & wait"),
            None,
            Duration::from_millis(300),
            true,
        )
        .expect("run");

        assert_eq!(captured.status, None);
        assert!(!captured.success());
        assert!(start.elapsed() < Duration::from_secs(10));
        assert_eq!(captured.stdout, b"started\n");
    }

    #[test]
    fn the_last_lines_are_cut_from_the_end() {
        assert_eq!(last_lines("a\nb\nc\nd", 2), "c\nd");
        assert_eq!(last_lines("a\nb", 5), "a\nb");
        assert_eq!(last_lines("", 3), "");
    }
}
