//! The agent socket (ADR 0008): the only socket agents may use.
//!
//! One JSON object per line. Every request names the agent and carries its token. The
//! socket never lists or touches user terminals, and it does not understand app requests.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use swarm_protocol::agent::{AgentRequest, Reply};

use crate::calls::{After, Calls, Gone};

/// Longest request line.
const MAX_LINE: u64 = 4 << 20;

/// Most connections served at once; the others are closed.
const MAX_CONNECTIONS: usize = 256;

/// Most connections that may wait at once (`wait`, `task_wait`, `terminal_wait`). The
/// rest of the room is for short calls and hooks.
pub(crate) const MAX_WAITS: usize = 192;

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
pub(crate) struct Slot(Arc<AtomicUsize>);

impl Slot {
    pub(crate) fn take(open: &Arc<AtomicUsize>, limit: usize) -> Option<Self> {
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

/// Lets one warning through each [`WARN_INTERVAL`].
#[derive(Default)]
pub(crate) struct Throttle {
    last: Mutex<Option<Instant>>,
}

impl Throttle {
    /// Returns true when the caller should log a warning.
    pub(crate) fn allow(&self, now: Instant) -> bool {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);

        match *last {
            Some(before) if now.duration_since(before) < WARN_INTERVAL => false,
            _ => {
                *last = Some(now);

                true
            }
        }
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
    let full = Throttle::default();

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
            if full.allow(Instant::now()) {
                log::warn!("agent socket is full: {MAX_CONNECTIONS} connections are open");
            }
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
    let mut writer = &stream;
    let mut reader = BufReader::new(&stream);

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

        let (reply, after) = answer(calls, refusals, line.trim(), &|| peer_gone(&stream));
        let written = writeln!(writer, "{}", encode(&reply)).is_ok();

        if let Some(after) = after {
            after(written, &|| peer_gone(&stream));
        }
        if !written {
            return Ok(());
        }
    }
}

/// True when the other end closed the connection. A peer that sent its next request
/// is still there.
fn peer_gone(stream: &UnixStream) -> bool {
    let descriptor = stream.as_raw_fd();
    let mut poll = libc::pollfd {
        fd: descriptor,
        events: libc::POLLIN,
        revents: 0,
    };

    // SAFETY: `poll` is one valid `pollfd` and the count is 1.
    let ready = unsafe { libc::poll(&raw mut poll, 1, 0) };
    if ready <= 0 {
        return false;
    }
    if poll.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
        return true;
    }

    let mut byte = 0_u8;
    // SAFETY: the buffer is one valid byte, and a peek takes nothing off the socket.
    let peeked = unsafe {
        libc::recv(
            descriptor,
            (&raw mut byte).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };

    peeked == 0
}

fn answer(
    calls: &Calls,
    refusals: &RefusalLog,
    line: &str,
    gone: Gone<'_>,
) -> (Reply, Option<After>) {
    let Ok(request) = serde_json::from_str::<AgentRequest>(line) else {
        return (Reply::failure("the request is not valid"), None);
    };
    let agent = request.agent;

    if let Ok(answer) = calls.handle(request, gone) {
        return answer;
    }

    if refusals.should_warn(agent.0, Instant::now()) {
        log::warn!("agent socket refused agent {}", agent.0);
    } else {
        log::debug!("agent socket refused agent {}", agent.0);
    }

    (Reply::failure("refused"), None)
}

fn encode(reply: &Reply) -> String {
    serde_json::to_string(reply)
        .unwrap_or_else(|_| r#"{"ok":false,"error":"cannot encode the reply"}"#.to_owned())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::{Value, json};
    use swarm_core::store::NewMessage;
    use swarm_protocol::{AgentId, MessageKind, ProjectId};

    use crate::messages::Messages;
    use crate::temp_folder::TempFolder;

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
    fn calls_with_an_agent() -> (Arc<Calls>, u64, String, Arc<Messages>, TempFolder) {
        use std::sync::Mutex;

        use swarm_core::Projects;
        use swarm_core::github::Github;
        use swarm_core::store::{NewTask, Store};
        use swarm_core::system_check::Tools;
        use swarm_protocol::{AgentKind, AgentRole};

        use crate::Services;
        use crate::agents::{AgentConfig, Agents, Place};
        use crate::checks::Checks;
        use crate::connection::SystemChecks;
        use crate::hub::{Clients, Terminals};
        use crate::messages::Messages;
        use crate::processes::Processes;
        use crate::pull_requests::PullRequests;
        use crate::usage::{Overrides, UsageService};

        let folder = TempFolder::new("agent-socket");
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
                pr_base: "main",
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
                data_dir: folder.path().to_owned(),
                socket: PathBuf::from("/tmp/none.sock"),
                settings: None,
                swarm: PathBuf::from("/tmp/swarm"),
                github: Github::with_folders(PathBuf::from("/tmp/none-bin"), None, Vec::new()),
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

        let projects = Arc::new(Projects::new(Arc::clone(&store)));
        let github = Github::with_folders(PathBuf::from("/tmp/none-bin"), None, Vec::new());
        let services = Arc::new(Services {
            processes: Processes::new(
                Arc::clone(&store),
                Arc::clone(&terminals),
                Arc::clone(&clients),
                Arc::clone(&messages),
                Arc::clone(&agents),
            ),
            checks: Checks::new(
                Arc::clone(&projects),
                Arc::clone(&terminals),
                Arc::clone(&clients),
                Arc::clone(&agents),
            ),
            pull_requests: PullRequests::new(github.clone(), None),
            system_check: SystemChecks::new(Tools::system(), None, github),
            usage: UsageService::start(
                folder.path(),
                None,
                Arc::clone(&clients),
                Overrides {
                    no_polling: true,
                    ..Overrides::default()
                },
            )
            .expect("usage"),
            terminals,
            projects,
            agents,
            clients,
            messages: Arc::clone(&messages),
            store,
            config_dir: None,
        });

        (
            Arc::new(Calls::new(services)),
            agent.id.0,
            token,
            messages,
            folder,
        )
    }

    fn ask(stream: &mut UnixStream, reader: &mut BufReader<UnixStream>, line: &str) -> Value {
        writeln!(stream, "{line}").expect("write");
        let mut answer = String::new();
        reader.read_line(&mut answer).expect("answer");

        serde_json::from_str(&answer).expect("json")
    }

    #[test]
    fn a_wait_longer_than_the_idle_time_is_not_cut_off() {
        let (calls, agent, token, _, _folder) = calls_with_an_agent();
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
    fn a_client_that_hangs_up_mid_wait_loses_no_message() {
        let (calls, agent, token, messages, _folder) = calls_with_an_agent();
        let (server_end, mut client) = UnixStream::pair().expect("pair");
        let serving = thread::spawn(move || {
            connection(
                server_end,
                &calls,
                &RefusalLog::default(),
                Duration::from_secs(30),
            )
        });
        writeln!(
            client,
            r#"{{"agent":{agent},"token":"{token}","method":"wait","params":{{"timeout_secs":60}}}}"#
        )
        .expect("write");
        thread::park_timeout(Duration::from_millis(300));

        drop(client);
        messages
            .post(&NewMessage {
                project: ProjectId(1),
                task: None,
                from: None,
                to: AgentId(agent),
                kind: MessageKind::Report,
                text: "after the hang-up",
                delivered: false,
            })
            .expect("post");
        let start = Instant::now();
        serving.join().expect("join").expect("a clean end");

        assert!(start.elapsed() < Duration::from_secs(5), "the wait went on");
        let kept = messages.claim(AgentId(agent)).expect("claim");
        assert_eq!(kept.messages().len(), 1);
        assert_eq!(kept.messages()[0].text, "after the hang-up");
    }

    #[test]
    fn a_message_is_delivered_only_after_the_reply_was_written() {
        let (calls, agent, token, messages, _folder) = calls_with_an_agent();
        messages
            .post(&NewMessage {
                project: ProjectId(1),
                task: None,
                from: None,
                to: AgentId(agent),
                kind: MessageKind::Report,
                text: "hello",
                delivered: false,
            })
            .expect("post");
        let request = format!(r#"{{"agent":{agent},"token":"{token}","method":"read"}}"#);

        let (reply, after) = answer(&calls, &RefusalLog::default(), &request, &|| false);
        assert!(reply.ok);
        assert!(
            messages.claim(AgentId(agent)).expect("claim").is_empty(),
            "held until the write"
        );

        after.expect("work after the write")(false, &|| false);
        assert_eq!(
            messages
                .claim(AgentId(agent))
                .expect("claim")
                .messages()
                .len(),
            1
        );

        let (_, after) = answer(&calls, &RefusalLog::default(), &request, &|| false);
        after.expect("work after the write")(true, &|| false);
        assert!(messages.claim(AgentId(agent)).expect("claim").is_empty());
    }

    #[test]
    fn waits_beyond_the_cap_are_refused_but_short_calls_are_not() {
        let (calls, agent, token, _, _folder) = calls_with_an_agent();
        let held = calls.hold_waits_for_test(MAX_WAITS);
        let wait = format!(
            r#"{{"agent":{agent},"token":"{token}","method":"wait","params":{{"timeout_secs":1}}}}"#
        );
        let read = format!(r#"{{"agent":{agent},"token":"{token}","method":"read"}}"#);

        let (waited, _) = answer(&calls, &RefusalLog::default(), &wait, &|| false);
        let (short, _) = answer(&calls, &RefusalLog::default(), &read, &|| false);

        assert_eq!(waited.error.as_deref(), Some("too many waits"));
        assert!(short.ok);
        drop(held);
        let (again, _) = answer(&calls, &RefusalLog::default(), &wait, &|| false);
        assert!(again.ok);
    }

    #[test]
    fn the_full_warning_comes_once_a_minute() {
        let throttle = Throttle::default();
        let start = Instant::now();

        assert!(throttle.allow(start));
        assert!(!throttle.allow(start + Duration::from_secs(59)));
        assert!(throttle.allow(start + WARN_INTERVAL));
    }

    #[test]
    fn a_closed_peer_is_gone_and_an_open_one_is_not() {
        let (server_end, mut client) = UnixStream::pair().expect("pair");
        assert!(!peer_gone(&server_end));

        writeln!(client, "next request").expect("write");
        assert!(
            !peer_gone(&server_end),
            "a pending request is not a hang-up"
        );

        drop(client);
        let mut sink = String::new();
        BufReader::new(&server_end)
            .read_line(&mut sink)
            .expect("read");
        assert!(peer_gone(&server_end));
    }

    #[test]
    fn a_request_with_a_wrong_token_is_refused() {
        let (calls, agent, _token, _, _folder) = calls_with_an_agent();
        let (reply, _) = answer(
            &calls,
            &RefusalLog::default(),
            &format!(r#"{{"agent":{agent},"token":"wrong","method":"read"}}"#),
            &|| false,
        );

        assert!(!reply.ok);
        assert_eq!(reply.error.as_deref(), Some("refused"));
    }

    #[test]
    fn a_garbled_request_is_not_valid() {
        let (calls, _, _, _, _folder) = calls_with_an_agent();

        let (reply, _) = answer(&calls, &RefusalLog::default(), "{nope", &|| false);

        assert_eq!(reply.error.as_deref(), Some("the request is not valid"));
    }
}
