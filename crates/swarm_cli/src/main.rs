//! `swarm`: the command line that agents use to talk to Swarm (ADR 0008).
//!
//! It speaks one JSON object per line to the daemon's agent socket. No async, no
//! terminal socket: the daemon refuses agents on that one.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::time::Duration;

const HELP: &str = "swarm: talk to Swarm from an agent terminal

Usage: swarm <command>

Commands:
  hook    Forward an agent hook event (JSON on stdin) to Swarm
";

/// Longest hook JSON we forward; a bigger event is dropped.
const MAX_HOOK_BYTES: u64 = 2 << 20;

/// A hook must never keep an agent waiting.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(2);

fn main() -> ExitCode {
    let mut arguments = std::env::args().skip(1);

    match arguments.next().as_deref() {
        Some("hook") => {
            hook();

            ExitCode::SUCCESS
        }
        None | Some("--help" | "-h" | "help") => {
            if std::io::stdout().write_all(HELP.as_bytes()).is_err() {
                return ExitCode::FAILURE;
            }

            ExitCode::SUCCESS
        }
        Some(other) => {
            let message = format!("swarm: unknown command {other:?}. Run swarm --help.\n");
            std::io::stderr().write_all(message.as_bytes()).ok();

            ExitCode::from(2)
        }
    }
}

/// Claude Code and Codex must never see an error from a hook, so every failure is
/// dropped here.
fn hook() {
    let mut input = Vec::new();
    if std::io::stdin()
        .take(MAX_HOOK_BYTES + 1)
        .read_to_end(&mut input)
        .is_err()
        || input.len() as u64 > MAX_HOOK_BYTES
    {
        return;
    }

    send_hook(&input).unwrap_or_default();
}

fn send_hook(input: &[u8]) -> Option<()> {
    let socket = std::env::var_os("SWARM_SOCKET")?;
    let agent: u64 = std::env::var("SWARM_AGENT_ID").ok()?.parse().ok()?;
    let token = std::env::var("SWARM_TOKEN").ok()?;
    let event: serde_json::Value = serde_json::from_slice(input).ok()?;

    let request = serde_json::json!({
        "method": "hook",
        "agent": agent,
        "token": token,
        "event": event,
    });

    let mut stream = UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(SOCKET_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT)).ok()?;

    let mut line = request.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).ok()?;
    stream.flush().ok()?;

    // Wait for the answer so the daemon has read the event before this process ends.
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).ok()?;

    Some(())
}
