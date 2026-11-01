//! `swarm statusline`: the status line of Swarm's Claude agents. It sends the rate limits
//! in Claude Code's status line input to the daemon while it runs the user's own status
//! line with the same input, so the user keeps it. It never prints an error and always
//! exits 0, because the agent must not see a broken screen.

use std::ffi::OsStr;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use swarm_protocol::agent::Call;
use swarm_usage::status_line_windows;

use crate::session::Session;

/// Longest status line input. Claude Code sends a few KiB.
const MAX_INPUT_BYTES: u64 = 256 << 10;

/// The daemon is on this machine; it answers at once or not at all.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(1);

const USER_COMMAND_TIMEOUT: Duration = Duration::from_secs(3);

const MAX_OUTPUT_BYTES: usize = 64 << 10;

/// Set while the user's command runs. A status line that runs `swarm statusline` again
/// would otherwise start itself over and over.
const CHAINED: &str = "SWARM_STATUSLINE_CHAINED";

pub(crate) fn run() {
    let mut input = Vec::new();
    if std::io::stdin()
        .take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut input)
        .is_err()
        || input.len() as u64 > MAX_INPUT_BYTES
    {
        return;
    }

    let sending = send_rate_limits(&input);

    if let Some(output) = user_output(&input) {
        // A closed stdout is the agent's business, not a failure of ours.
        let mut stdout = std::io::stdout();
        stdout.write_all(&output).and_then(|()| stdout.flush()).ok();
    }

    if let Some(sending) = sending {
        sending.finish();
    }
}

/// What the user's own status line prints for this input. `None` when there is none, or
/// when this process is already part of one.
fn user_output(input: &[u8]) -> Option<Vec<u8>> {
    if std::env::var_os(CHAINED).is_some() {
        return None;
    }

    let command = user_command(
        std::env::var_os("CLAUDE_CONFIG_DIR").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )?;

    run_user_command(&command, input)
}

/// The rate limits call, on its own thread so the user's command does not wait for the
/// daemon.
struct Sending {
    started: Instant,
    done: mpsc::Receiver<()>,
}

impl Sending {
    /// Waits for the call until the time it was given has passed. When the user's command
    /// took longer than that, the call was given enough time already.
    fn finish(self) {
        let left = (self.started + SOCKET_TIMEOUT).saturating_duration_since(Instant::now());

        self.done.recv_timeout(left).ok();
    }
}

fn send_rate_limits(input: &[u8]) -> Option<Sending> {
    let value = serde_json::from_slice::<Value>(input).ok()?;
    value.get("rate_limits")?;
    let session = Session::from_env().ok()?;
    let call = Call::RateLimits {
        windows: status_line_windows(&value),
    };

    let (finished, done) = mpsc::channel();
    thread::Builder::new()
        .name("rate-limits".into())
        .spawn(move || {
            // Whether the daemon took it does not matter to the screen.
            session.call_within(call, SOCKET_TIMEOUT).ok();
            finished.send(()).ok();
        })
        .ok()?;

    Some(Sending {
        started: Instant::now(),
        done,
    })
}

/// The user's own status line command, from `$CLAUDE_CONFIG_DIR/settings.json` or
/// `~/.claude/settings.json`. `None` when there is none, or when it is `swarm statusline`.
fn user_command(config_dir: Option<&OsStr>, home: Option<&OsStr>) -> Option<String> {
    let folder = match config_dir.filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(home?).join(".claude"),
    };

    let text = std::fs::read_to_string(folder.join("settings.json")).ok()?;
    let settings: Value = serde_json::from_str(&text).ok()?;
    let status_line = settings.get("statusLine")?;
    if status_line.get("type")?.as_str()? != "command" {
        return None;
    }

    let command = status_line.get("command")?.as_str()?.trim();

    (!command.is_empty() && !is_swarm_statusline(command)).then(|| command.to_owned())
}

/// Whether the command is a call of Swarm's own `statusline`, with or without a path and
/// quotes. Running it would only send the same numbers again.
fn is_swarm_statusline(command: &str) -> bool {
    let Some(program) = command.trim().strip_suffix("statusline") else {
        return false;
    };

    let program = program.trim().trim_matches(['\'', '"']);

    Path::new(program).file_name() == Some(OsStr::new("swarm"))
}

/// Runs the command through the shell with the status line input and returns what it
/// printed. `None` when it does not start or takes too long.
fn run_user_command(command: &str, input: &[u8]) -> Option<Vec<u8>> {
    let mut shell = Command::new("/bin/sh");
    shell.arg("-c").arg(command).env(CHAINED, "1");

    swarm_usage::run(&mut shell, input, USER_COMMAND_TIMEOUT, MAX_OUTPUT_BYTES)
        .ok()
        .map(|finished| finished.stdout)
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStrExt as _;
    use std::process::Stdio;

    use super::*;

    fn settings_dir(name: &str, settings: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("swc-line-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("folder");
        std::fs::write(dir.join("settings.json"), settings).expect("settings");

        dir
    }

    fn command_in(dir: &Path) -> Option<String> {
        user_command(Some(dir.as_os_str()), None)
    }

    #[test]
    fn the_command_comes_from_the_config_dir() {
        let dir = settings_dir(
            "config",
            r#"{"statusLine":{"type":"command","command":"  ~/line.sh --fast ","padding":2}}"#,
        );

        assert_eq!(command_in(&dir).as_deref(), Some("~/line.sh --fast"));
    }

    #[test]
    fn without_a_config_dir_the_home_folder_is_used() {
        let home = std::env::temp_dir().join(format!("swc-home-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".claude")).expect("folder");
        std::fs::write(
            home.join(".claude/settings.json"),
            r#"{"statusLine":{"type":"command","command":"echo hi"}}"#,
        )
        .expect("settings");

        assert_eq!(
            user_command(None, Some(home.as_os_str())).as_deref(),
            Some("echo hi")
        );
        assert_eq!(
            user_command(Some(OsStr::new("")), Some(home.as_os_str())).as_deref(),
            Some("echo hi")
        );
        assert_eq!(user_command(None, None), None);
    }

    #[test]
    fn no_usable_status_line_gives_none() {
        for (name, settings) in [
            ("empty", "{}"),
            ("broken", "{ nope"),
            ("string", r#"{"statusLine":"echo hi"}"#),
            (
                "other-type",
                r#"{"statusLine":{"type":"static","command":"echo hi"}}"#,
            ),
            ("no-command", r#"{"statusLine":{"type":"command"}}"#),
            (
                "blank",
                r#"{"statusLine":{"type":"command","command":"  "}}"#,
            ),
            (
                "not-text",
                r#"{"statusLine":{"type":"command","command":7}}"#,
            ),
        ] {
            assert_eq!(command_in(&settings_dir(name, settings)), None, "{name}");
        }
        assert_eq!(command_in(Path::new("/nonexistent/claude")), None);
    }

    #[test]
    fn swarm_statusline_is_not_chained_to_itself() {
        for command in [
            "swarm statusline",
            "/usr/local/bin/swarm statusline",
            "  '/Users/me/My Apps/swarm' statusline ",
            "\"/opt/swarm\" statusline",
        ] {
            assert!(is_swarm_statusline(command), "{command}");
        }
        for command in [
            "swarm status",
            "my-swarm statusline",
            "swarm statusline --extra",
            "echo statusline",
            "statusline",
            "~/bin/statusline",
        ] {
            assert!(!is_swarm_statusline(command), "{command}");
        }
    }

    #[test]
    fn a_swarm_statusline_command_is_dropped() {
        let dir = settings_dir(
            "self",
            r#"{"statusLine":{"type":"command","command":"/bin/swarm statusline"}}"#,
        );

        assert_eq!(command_in(&dir), None);
    }

    #[test]
    fn the_user_command_gets_the_input_and_its_output_comes_back() {
        let output = run_user_command("cat; printf '!'", b"in").expect("output");

        assert_eq!(output, b"in!");
    }

    #[test]
    fn a_user_command_that_does_not_end_gives_nothing() {
        let started = Instant::now();

        let output = run_user_command("exec sleep 30", b"");

        assert_eq!(output, None);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_user_command_that_times_out_takes_the_programs_it_started_with_it() {
        let dir = std::env::temp_dir().join(format!("swc-tree-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("folder");
        let pid_file = dir.join("pid");
        // No `exec` on the outside: the shell stays and the pipeline runs under it.
        let command = format!(
            "sh -c 'echo $$ > \"$0\"; exec sleep 30' '{}' | cat",
            pid_file.display()
        );

        let output = run_user_command(&command, b"");

        let pid = std::fs::read_to_string(&pid_file).expect("pid");
        let gone = process_gone(pid.trim());
        std::fs::remove_dir_all(&dir).expect("cleanup");
        assert_eq!(output, None);
        assert!(gone, "the program the shell started still runs");
    }

    /// Waits up to 3 seconds for a process to go.
    fn process_gone(pid: &str) -> bool {
        let exists = || {
            Command::new("/bin/kill")
                .args(["-0", pid])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        };
        let deadline = Instant::now() + Duration::from_secs(3);

        while exists() {
            if Instant::now() >= deadline {
                return false;
            }
            thread::park_timeout(Duration::from_millis(20));
        }

        true
    }

    #[test]
    fn the_user_command_is_told_it_is_chained() {
        let output = run_user_command(&format!("printf %s \"${CHAINED}\""), b"").expect("output");

        assert_eq!(output.as_slice(), OsStr::new("1").as_bytes());
    }
}
