//! The agent socket (ADR 0008): the only socket agents may use.
//!
//! One JSON object per line. Every request names the agent and carries its token. The
//! socket never lists or touches terminals, and it does not understand app requests.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use swarm_protocol::AgentId;

use crate::agents::Agents;

/// Longest request line.
const MAX_LINE: u64 = 4 << 20;

/// Most connections served at once; the others are closed.
const MAX_CONNECTIONS: usize = 64;

/// A refusal is logged as a warning at most this often per agent.
const WARN_INTERVAL: Duration = Duration::from_secs(60);

/// Most agents the warning limiter remembers.
const MAX_TRACKED_AGENTS: usize = 1024;

/// A connection that sends nothing for this long is closed.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Pause after a failed `accept`, so a persistent error does not spin.
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

#[derive(Deserialize)]
struct Request {
    method: String,
    agent: u64,
    token: String,
    #[serde(default)]
    event: Value,
}

/// Replaces a stale socket file and makes the new one private (mode 0600).
pub(crate) fn bind(path: &Path) -> Result<UnixListener> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("cannot remove the stale agent socket"),
    }

    let listener = UnixListener::bind(path).context("cannot bind the agent socket")?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;

    Ok(listener)
}

/// Counts the open connections; the count goes down when the guard drops.
struct Slot(Arc<AtomicUsize>);

impl Slot {
    fn take(open: &Arc<AtomicUsize>, limit: usize) -> Option<Self> {
        if open.fetch_add(1, Ordering::AcqRel) >= limit {
            open.fetch_sub(1, Ordering::AcqRel);

            return None;
        }

        Some(Self(Arc::clone(open)))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Lets one refusal warning per agent through each [`WARN_INTERVAL`].
#[derive(Default)]
struct RefusalLog {
    seen: Mutex<HashMap<u64, Instant>>,
}

impl RefusalLog {
    /// Returns true when the caller should log a warning.
    fn should_warn(&self, agent: u64, now: Instant) -> bool {
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        if seen.len() >= MAX_TRACKED_AGENTS {
            seen.retain(|_, last| now.duration_since(*last) < WARN_INTERVAL);
        }
        if seen.len() >= MAX_TRACKED_AGENTS {
            return false;
        }

        match seen.get(&agent) {
            Some(last) if now.duration_since(*last) < WARN_INTERVAL => false,
            _ => {
                seen.insert(agent, now);

                true
            }
        }
    }
}

pub(crate) fn serve(listener: &UnixListener, agents: &Arc<Agents>) {
    let open = Arc::new(AtomicUsize::new(0));
    let refusals = Arc::new(RefusalLog::default());

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                log::warn!("agent socket accept failed: {error}");
                thread::park_timeout(ACCEPT_RETRY);
                continue;
            }
        };

        let Some(slot) = Slot::take(&open, MAX_CONNECTIONS) else {
            log::debug!("agent socket is full: connection closed");
            drop(stream);
            continue;
        };

        let agents = Arc::clone(agents);
        let refusals = Arc::clone(&refusals);
        let spawned = thread::Builder::new()
            .name("agent-client".into())
            .spawn(move || {
                let _slot = slot;
                if let Err(error) = connection(stream, &agents, &refusals) {
                    log::debug!("agent connection ended: {error:#}");
                }
            });
        if let Err(error) = spawned {
            log::error!("cannot start a thread for an agent connection: {error}");
        }
    }
}

fn connection(stream: UnixStream, agents: &Agents, refusals: &RefusalLog) -> Result<()> {
    stream.set_read_timeout(Some(IDLE_TIMEOUT))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);

    loop {
        let mut line = String::new();
        let read = reader.by_ref().take(MAX_LINE).read_line(&mut line)?;

        if read == 0 {
            return Ok(());
        }
        if !line.ends_with('\n') {
            writeln!(writer, "{}", failure("the request is too long"))?;

            return Ok(());
        }

        writeln!(writer, "{}", answer(agents, refusals, line.trim()))?;
    }
}

fn answer(agents: &Agents, refusals: &RefusalLog, line: &str) -> Value {
    let request: Request = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(_) => return failure("the request is not valid"),
    };

    match request.method.as_str() {
        "hook" => {
            let result = agents.hook(AgentId(request.agent), &request.token, &request.event);
            if result.is_ok() {
                return json!({ "ok": true });
            }

            if refusals.should_warn(request.agent, Instant::now()) {
                log::warn!("agent socket refused agent {}", request.agent);
            } else {
                log::debug!("agent socket refused agent {}", request.agent);
            }

            failure("refused")
        }
        _ => failure("unknown method"),
    }
}

fn failure(message: &str) -> Value {
    json!({ "ok": false, "error": message })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_stop_at_the_limit_and_come_back() {
        let open = Arc::new(AtomicUsize::new(0));

        let first = Slot::take(&open, 2);
        let second = Slot::take(&open, 2);
        let third = Slot::take(&open, 2);

        assert!(first.is_some() && second.is_some());
        assert!(third.is_none());
        assert_eq!(open.load(Ordering::Acquire), 2);

        drop(first);

        assert!(Slot::take(&open, 2).is_some());
    }

    #[test]
    fn a_refusal_warns_once_a_minute_per_agent() {
        let log = RefusalLog::default();
        let start = Instant::now();

        assert!(log.should_warn(1, start));
        assert!(!log.should_warn(1, start + Duration::from_secs(30)));
        assert!(log.should_warn(2, start + Duration::from_secs(30)));
        assert!(log.should_warn(1, start + WARN_INTERVAL));
    }
}
