//! `swarm statusline`: the status line of Swarm's Claude agents. It sends the rate limits
//! in Claude Code's status line input to the daemon, then runs the user's own status line
//! with the same input, so the user keeps it. It never prints an error and always exits 0,
//! because the agent must not see a broken screen.

use std::ffi::OsStr;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

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

    send_rate_limits(&input);

    if std::env::var_os(CHAINED).is_some() {
        return;
    }

    let command = user_command(
        std::env::var_os("CLAUDE_CONFIG_DIR").as_deref(),
        std::env::var_os("HOME").as_deref(),
    );
    let Some(output) = command.and_then(|command| run_user_command(&command, &input)) else {
        return;
    };

    // A closed stdout is the agent's business, not a failure of ours.
    let mut stdout = std::io::stdout();
    stdout.write_all(&output).and_then(|()| stdout.flush()).ok();
}

fn send_rate_limits(input: &[u8]) {
    let Ok(value) = serde_json::from_slice::<Value>(input) else {
        return;
    };
    if value.get("rate_limits").is_none() {
        return;
    }
    let Ok(session) = Session::from_env() else {
        return;
    };

    // Whether the daemon took it does not matter to the screen.
    session
        .call_within(
            Call::RateLimits {
                windows: status_line_windows(&value),
            },
            SOCKET_TIMEOUT,
        )
        .ok();
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
        let started = std::time::Instant::now();

        let output = run_user_command("exec sleep 30", b"");

        assert_eq!(output, None);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn the_user_command_is_told_it_is_chained() {
        let output = run_user_command(&format!("printf %s \"${CHAINED}\""), b"").expect("output");

        assert_eq!(output.as_slice(), OsStr::new("1").as_bytes());
    }
}
