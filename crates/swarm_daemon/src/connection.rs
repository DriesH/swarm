//! One client: a reader thread (this one) handles requests in order, and a writer
//! thread sends the queued frames, so a slow client never blocks a terminal.

use std::io::{BufReader, BufWriter, ErrorKind, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd as _;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::thread;

use anyhow::Result;
use swarm_protocol::{
    CallId, Event, FrameError, Request, TaskInfo, TerminalId, VERSION, read, write,
};

use crate::Services;
use crate::hub::{CREATE_HISTORY_LINES, ClientSink, Frame, QUEUE_FRAMES};
use crate::tasks;

/// Longest parent chain the peer check follows.
const MAX_ANCESTORS: usize = 256;

pub(crate) fn serve(services: &Arc<Services>, client: u64, mut stream: UnixStream) -> Result<()> {
    if let Some(reason) = refusal(&stream) {
        log::warn!("client {client} refused: {reason}");
        write(
            &mut stream,
            &Event::Error {
                call: None,
                id: None,
                message: reason,
            },
        )?;

        return Ok(());
    }

    let (frames, queue) = flume::bounded::<Frame>(QUEUE_FRAMES);

    let writer_stream = stream.try_clone()?;
    thread::Builder::new()
        .name(format!("client-{client}-writer"))
        .spawn(move || write_frames(&queue, writer_stream))?;

    let sink = Arc::new(ClientSink::new(client, frames, Some(stream.try_clone()?)));
    let mut reader = BufReader::new(stream);
    let mut buffer = Vec::new();

    if handshake(&sink, &mut reader, &mut buffer) {
        services.clients.add(&sink);

        loop {
            match read::<Request>(&mut reader, &mut buffer) {
                Ok(request) => handle(services, &sink, request),
                Err(FrameError::Io(error)) if error.kind() == ErrorKind::UnexpectedEof => break,
                Err(error) => {
                    log::info!("client {client} ended: {error}");
                    break;
                }
            }
        }
    }

    services.clients.remove(client);
    services.terminals.detach_client(client);

    Ok(())
}

/// The first request must be a `Hello` with our version.
fn handshake(sink: &ClientSink, reader: &mut BufReader<UnixStream>, buffer: &mut Vec<u8>) -> bool {
    let reply = match read::<Request>(reader, buffer) {
        Ok(Request::Hello { version }) if version == VERSION => Event::Hello {
            version: VERSION,
            pid: std::process::id(),
        },
        // Answer with our own Hello and close: only `Hello` decodes across versions.
        Ok(Request::Hello { version }) => {
            log::info!(
                "client {} speaks protocol version {version}; closing",
                sink.id()
            );
            sink.send_event(&Event::Hello {
                version: VERSION,
                pid: std::process::id(),
            });
            return false;
        }
        Ok(_) => Event::Error {
            call: None,
            id: None,
            message: "the first request must be Hello".to_owned(),
        },
        Err(error) => {
            log::info!("client {} left before saying hello: {error}", sink.id());
            return false;
        }
    };

    let accepted = matches!(reply, Event::Hello { .. });
    sink.send_event(&reply);

    accepted
}

fn handle(services: &Arc<Services>, sink: &Arc<ClientSink>, request: Request) {
    let (terminals, projects) = (&services.terminals, &services.projects);

    match request {
        Request::Hello { .. } => {
            sink.send_event(&Event::Hello {
                version: VERSION,
                pid: std::process::id(),
            });
        }
        Request::List { call } => {
            sink.send_event(&Event::Terminals {
                call,
                terminals: terminals.list(),
            });
        }
        Request::Create { call, terminal } => match terminals.create(terminal) {
            Ok(hub) => {
                sink.send_event(&Event::Created { call, id: hub.id() });
                if !hub.attach(sink, CREATE_HISTORY_LINES) {
                    send_missing(sink, hub.id());
                }
            }
            Err(error) => send_error(sink, call, &error),
        },
        Request::Attach { id, history } => match terminals.get(id) {
            Some(hub) if hub.attach(sink, history) => {}
            _ => send_missing(sink, id),
        },
        Request::Detach { id } => {
            if let Some(hub) = terminals.get(id) {
                hub.detach(sink.id());
            }
        }
        Request::Input { id, input } => match terminals.get(id) {
            Some(hub) if hub.send_input(input) => services.agents.app_input(id),
            _ => send_missing(sink, id),
        },
        Request::StartOrchestrator { call, .. } => {
            send_error(sink, call, &"not available yet");
        }
        Request::ListProjects { call } => match projects.list() {
            Ok(projects) => {
                sink.send_event(&Event::Projects { call, projects });
            }
            Err(error) => send_error(sink, call, &error),
        },
        Request::AddProject { call, path } => {
            let projects = Arc::clone(projects);
            run_slow(sink, "add-project", call, move |sink| {
                match projects.add(&path) {
                    Ok(project) => {
                        sink.send_event(&Event::ProjectAdded { call, project });
                    }
                    Err(error) => send_error(sink, call, &error),
                }
            });
        }
        Request::CreateTask { call, task } => {
            let services = Arc::clone(services);
            run_slow(sink, "create-task", call, move |sink| {
                let reply = |task: &TaskInfo| {
                    sink.send_event(&Event::TaskCreated {
                        call,
                        task: task.clone(),
                    });
                };

                if let Err(error) = tasks::create(&services, &task, reply) {
                    send_error(sink, call, &error);
                }
            });
        }
    }
}

/// Runs a request that waits on git or the disk on its own thread, so the
/// connection keeps answering other requests.
fn run_slow(
    sink: &Arc<ClientSink>,
    name: &str,
    call: CallId,
    work: impl FnOnce(&ClientSink) + Send + 'static,
) {
    let worker_sink = Arc::clone(sink);
    let spawned = thread::Builder::new()
        .name(format!("{name}-{}", call.0))
        .spawn(move || work(&worker_sink));

    if let Err(error) = spawned {
        send_error(sink, call, &error);
    }
}

fn send_error(sink: &ClientSink, call: CallId, error: &dyn std::fmt::Display) {
    sink.send_event(&Event::Error {
        call: Some(call),
        id: None,
        message: format!("{error:#}"),
    });
}

fn send_missing(sink: &ClientSink, id: TerminalId) {
    sink.send_event(&Event::Error {
        call: None,
        id: Some(id),
        message: format!("no running terminal {}", id.0),
    });
}

fn write_frames(queue: &flume::Receiver<Frame>, stream: UnixStream) {
    let mut writer = BufWriter::new(&stream);

    while let Ok(frame) = queue.recv() {
        // Batch what is queued, then flush once.
        let sent = writer.write_all(&frame).is_ok()
            && queue
                .try_iter()
                .all(|frame| writer.write_all(&frame).is_ok())
            && writer.flush().is_ok();
        if !sent {
            break;
        }
    }

    if let Err(error) = stream.shutdown(Shutdown::Both) {
        log::debug!("client socket already closed: {error}");
    }
}

/// Why the peer must not talk to the daemon, if it must not (ADR 0008).
fn refusal(stream: &UnixStream) -> Option<String> {
    let Some(peer) = peer_pid(stream) else {
        return Some("cannot tell which process connected".to_owned());
    };

    let daemon = std::process::id();
    match descends_from(peer, daemon, parent_pid) {
        Some(false) => None,
        Some(true) => Some("terminals cannot use the terminal socket".to_owned()),
        None => Some("cannot check the connecting process".to_owned()),
    }
}

fn peer_pid(stream: &UnixStream) -> Option<u32> {
    let mut pid: libc::pid_t = 0;
    let mut length = libc::socklen_t::try_from(size_of::<libc::pid_t>()).ok()?;

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
        return None;
    }

    u32::try_from(pid).ok().filter(|pid| *pid > 0)
}

fn parent_pid(pid: u32) -> Option<u32> {
    // SAFETY: proc_bsdinfo is plain old data, so all zeroes is a valid value.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = libc::c_int::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;

    // SAFETY: proc_pidinfo writes at most `size` bytes into `info`.
    let written = unsafe {
        libc::proc_pidinfo(
            libc::c_int::try_from(pid).ok()?,
            libc::PROC_PIDTBSDINFO,
            0,
            (&raw mut info).cast(),
            size,
        )
    };

    (written == size).then_some(info.pbi_ppid)
}

/// Whether `ancestor` is `pid` or one of its parents. `None` when `pid` itself
/// cannot be read, which callers treat as a refusal.
///
/// A later link that cannot be read ends the walk as "outside": the daemon can
/// read every process of its own user, so the unreadable ones belong to others
/// (for example root-owned launchd jobs) and cannot sit between the daemon and a terminal.
fn descends_from(pid: u32, ancestor: u32, parent_of: impl Fn(u32) -> Option<u32>) -> Option<bool> {
    let mut current = pid;

    for step in 0..MAX_ANCESTORS {
        if current == ancestor {
            return Some(true);
        }
        if current <= 1 {
            return Some(false);
        }

        match parent_of(current) {
            Some(parent) => current = parent,
            None if step == 0 => return None,
            None => return Some(false),
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn chain() -> impl Fn(u32) -> Option<u32> {
        let parents = HashMap::from([(30, 20), (20, 10), (10, 1), (50, 40), (40, 1)]);

        move |pid| parents.get(&pid).copied()
    }

    #[test]
    fn a_descendant_is_found_through_its_parents() {
        assert_eq!(descends_from(30, 10, chain()), Some(true));
    }

    #[test]
    fn the_daemon_itself_counts_as_inside() {
        assert_eq!(descends_from(10, 10, chain()), Some(true));
    }

    #[test]
    fn an_unrelated_process_is_outside() {
        assert_eq!(descends_from(50, 10, chain()), Some(false));
    }

    #[test]
    fn an_unreadable_peer_is_unknown() {
        assert_eq!(descends_from(99, 10, chain()), None);
    }

    #[test]
    fn an_unreadable_ancestor_ends_the_walk_outside() {
        assert_eq!(descends_from(30, 5, chain()), Some(false));
    }

    #[test]
    fn a_parent_cycle_is_unknown() {
        let swapped = |pid| Some(if pid == 5 { 6 } else { 5 });

        assert_eq!(descends_from(5, 10, swapped), None);
    }
}
