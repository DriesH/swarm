//! The agent socket (ADR 0008): the only socket agents may use.
//!
//! One JSON object per line. Every request names the agent and carries its token. The
//! socket never lists or touches user terminals, and it does not understand app requests.

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
use swarm_protocol::agent::{AgentRequest, Reply};

use crate::calls::Calls;

/// Longest request line.
const MAX_LINE: u64 = 4 << 20;

/// Most connections served at once; the others are closed.
const MAX_CONNECTIONS: usize = 64;

/// A refusal is logged as a warning at most this often per agent.
const WARN_INTERVAL: Duration = Duration::from_secs(60);

/// Most agents the warning limiter remembers.
const MAX_TRACKED_AGENTS: usize = 1024;

/// A connection that sends nothing for this long is closed. The time a call takes to
/// answer does not count: only the wait for the next request does.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Pause after a failed `accept`, so a persistent error does not spin.
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

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

pub(crate) fn serve(listener: &UnixListener, calls: &Arc<Calls>) {
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

        let calls = Arc::clone(calls);
        let refusals = Arc::clone(&refusals);
        let spawned = thread::Builder::new()
            .name("agent-client".into())
            .spawn(move || {
                let _slot = slot;
                if let Err(error) = connection(stream, &calls, &refusals, IDLE_TIMEOUT) {
                    log::debug!("agent connection ended: {error:#}");
                }
            });
        if let Err(error) = spawned {
            log::error!("cannot start a thread for an agent connection: {error}");
        }
    }
}

fn connection(
    stream: UnixStream,
    calls: &Calls,
    refusals: &RefusalLog,
    idle_timeout: Duration,
) -> Result<()> {
    stream.set_read_timeout(Some(idle_timeout))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);

    loop {
        let mut line = String::new();
        let read = reader.by_ref().take(MAX_LINE).read_line(&mut line)?;

        if read == 0 {
            return Ok(());
        }
        if !line.ends_with('\n') {
            writeln!(
                writer,
                "{}",
                encode(&Reply::failure("the request is too long"))
            )?;

            return Ok(());
        }

        writeln!(writer, "{}", encode(&answer(calls, refusals, line.trim())))?;
    }
}

fn answer(calls: &Calls, refusals: &RefusalLog, line: &str) -> Reply {
    let Ok(request) = serde_json::from_str::<AgentRequest>(line) else {
        return Reply::failure("the request is not valid");
    };
    let agent = request.agent;

    if let Ok(reply) = calls.handle(request) {
        return reply;
    }

    if refusals.should_warn(agent.0, Instant::now()) {
        log::warn!("agent socket refused agent {}", agent.0);
    } else {
        log::debug!("agent socket refused agent {}", agent.0);
    }

    Reply::failure("refused")
}

fn encode(reply: &Reply) -> String {
    serde_json::to_string(reply)
        .unwrap_or_else(|_| r#"{"ok":false,"error":"cannot encode the reply"}"#.to_owned())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::{Value, json};

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
    /// Everything the socket needs, with one registered orchestrator.
    fn calls_with_an_agent() -> (Arc<Calls>, u64, String) {
        use std::sync::Mutex;

        use swarm_core::Projects;
        use swarm_core::store::{NewTask, Store};
        use swarm_protocol::{AgentKind, AgentRole};

        use crate::Services;
        use crate::agents::{AgentConfig, Agents, Place};
        use crate::hub::{Clients, Terminals};
        use crate::messages::Messages;

        let store = Store::open_in_memory().expect("store");
        let project = store
            .add_project(Path::new("/repo"), "repo")
            .expect("project");
        let task = store
            .add_task(&NewTask {
                project: project.id,
                name: "Task",
                branch: "swarm/task",
                base: "main",
                worktree: Path::new("/repo/.worktrees/task"),
                ports: &[],
            })
            .expect("task");
        let store = Arc::new(Mutex::new(store));
        let clients = Arc::new(Clients::default());
        let terminals = Terminals::new(Arc::clone(&store));
        let messages = Messages::new(Arc::clone(&store), Arc::clone(&clients));
        let agents = Agents::new(
            AgentConfig {
                data_dir: std::env::temp_dir(),
                socket: PathBuf::from("/tmp/none.sock"),
                settings: None,
                swarm: PathBuf::from("/tmp/swarm"),
            },
            Arc::clone(&store),
            Arc::clone(&terminals),
            Arc::clone(&clients),
            Arc::clone(&messages),
        );
        let plan = agents
            .plan(AgentKind::ClaudeCode, AgentRole::Orchestrator)
            .expect("plan");
        let agent = agents
            .register(&Place::of_task(&task), plan)
            .expect("register");
        let token = agents.token_for_test(agent.id).expect("token");

        let services = Arc::new(Services {
            terminals,
            projects: Arc::new(Projects::new(Arc::clone(&store))),
            agents,
            clients,
            messages,
            store,
        });

        (Arc::new(Calls::new(services)), agent.id.0, token)
    }

    fn ask(stream: &mut UnixStream, reader: &mut BufReader<UnixStream>, line: &str) -> Value {
        writeln!(stream, "{line}").expect("write");
        let mut answer = String::new();
        reader.read_line(&mut answer).expect("answer");

        serde_json::from_str(&answer).expect("json")
    }

    #[test]
    fn a_wait_longer_than_the_idle_time_is_not_cut_off() {
        let (calls, agent, token) = calls_with_an_agent();
        let (server_end, mut client) = UnixStream::pair().expect("pair");
        let serving = thread::spawn(move || {
            connection(
                server_end,
                &calls,
                &RefusalLog::default(),
                Duration::from_millis(300),
            )
        });
        let mut reader = BufReader::new(client.try_clone().expect("clone"));
        let wait = format!(
            r#"{{"agent":{agent},"token":"{token}","method":"wait","params":{{"timeout_secs":1}}}}"#
        );
        let read = format!(r#"{{"agent":{agent},"token":"{token}","method":"read"}}"#);

        let start = Instant::now();
        let waited = ask(&mut client, &mut reader, &wait);
        let waited_for = start.elapsed();
        let after = ask(&mut client, &mut reader, &read);

        assert_eq!(waited, json!({ "ok": true, "result": [] }));
        assert!(waited_for >= Duration::from_millis(900), "{waited_for:?}");
        assert_eq!(after["ok"], true, "the connection stayed open");

        let ended = serving.join().expect("join");
        assert!(ended.is_err(), "an idle connection is closed");
    }

    #[test]
    fn a_request_with_a_wrong_token_is_refused() {
        let (calls, agent, _token) = calls_with_an_agent();
        let reply = answer(
            &calls,
            &RefusalLog::default(),
            &format!(r#"{{"agent":{agent},"token":"wrong","method":"read"}}"#),
        );

        assert!(!reply.ok);
        assert_eq!(reply.error.as_deref(), Some("refused"));
    }

    #[test]
    fn a_garbled_request_is_not_valid() {
        let (calls, _, _) = calls_with_an_agent();

        let reply = answer(&calls, &RefusalLog::default(), "{nope");

        assert_eq!(reply.error.as_deref(), Some("the request is not valid"));
    }
}
