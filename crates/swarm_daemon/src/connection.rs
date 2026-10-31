//! One client: a reader thread (this one) handles requests in order, and a writer
//! thread sends the queued frames, so a slow client never blocks a terminal.

use std::io::{BufReader, BufWriter, ErrorKind, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd as _;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use anyhow::Result;
use swarm_protocol::{
    CallId, CreateTerminal, Event, FrameError, MESSAGES_LISTED, Request, TaskInfo, TerminalId,
    VERSION, read, write,
};
use swarm_term::TerminalInput;

use crate::hub::{CREATE_HISTORY_LINES, ClientSink, Frame, Hub, QUEUE_FRAMES};
use crate::pull_requests::{self, NewRequest};
use crate::tasks;
use crate::{Services, comments};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

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
        Request::Create { call, terminal } => create_terminal(services, sink, call, terminal),
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
            Some(hub) => forward_input(services, sink, &hub, id, input),
            None => send_missing(sink, id),
        },
        Request::StartOrchestrator {
            call,
            project,
            kind,
        } => {
            let services = Arc::clone(services);
            run_slow(
                sink,
                "start-orchestrator",
                call,
                move |sink| match tasks::start_orchestrator(&services, project, kind) {
                    Ok(agent) => {
                        sink.send_event(&Event::AgentStarted { call, agent });
                    }
                    Err(error) => send_error(sink, call, &error),
                },
            );
        }
        Request::ListMessages {
            call,
            project,
            task,
        } => {
            let store = Arc::clone(&services.store);
            run_slow(sink, "list-messages", call, move |sink| {
                let listed = lock(&store).list_messages(project, task, MESSAGES_LISTED);

                match listed {
                    Ok(messages) => {
                        sink.send_event(&Event::Messages { call, messages });
                    }
                    Err(error) => send_error(sink, call, &error),
                }
            });
        }
        Request::AgentChanges { call, agents } => {
            let Some(running) = sink.begin_changes() else {
                send_error(sink, call, &"busy: the previous changes request still runs");

                return;
            };

            let projects = Arc::clone(projects);
            run_slow(sink, "agent-changes", call, move |sink| {
                let changes = projects.agent_changes(&agents);
                drop(running);

                sink.send_event(&Event::AgentChanges { call, changes });
            });
        }
        request @ (Request::TaskDiff { .. }
        | Request::ApproveTask { .. }
        | Request::CheckClose { .. }
        | Request::CloseTask { .. }) => handle_review(services, sink, request),
        request @ (Request::ListComments { .. }
        | Request::AddComment { .. }
        | Request::DeleteComment { .. }
        | Request::ResolveComment { .. }
        | Request::SendComments { .. }) => handle_comments(services, sink, request),
        request @ (Request::GithubStatus { .. }
        | Request::ListBranches { .. }
        | Request::RunChecks { .. }
        | Request::PrPanel { .. }
        | Request::DraftPr { .. }) => handle_github(services, sink, request),
        request @ (Request::CreatePr { .. }
        | Request::UpdatePr { .. }
        | Request::PrStatus { .. }
        | Request::MarkPrReady { .. }
        | Request::AddReviewers { .. }
        | Request::MergePr { .. }
        | Request::SetCloseAfterMerge { .. }) => handle_pull_requests(services, sink, request),
        request @ (Request::ListProjects { .. }
        | Request::AddProject { .. }
        | Request::CreateTask { .. }) => handle_projects(services, sink, request),
    }
}

/// Projects and the tasks made in them.
fn handle_projects(services: &Arc<Services>, sink: &Arc<ClientSink>, request: Request) {
    let projects = &services.projects;

    match request {
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

                if let Err(error) = tasks::create(&services, &task, None, reply) {
                    send_error(sink, call, &error);
                }
            });
        }
        _ => {}
    }
}

fn create_terminal(
    services: &Services,
    sink: &Arc<ClientSink>,
    call: CallId,
    terminal: CreateTerminal,
) {
    if services.agents.is_closing_key(terminal.task.as_deref()) {
        send_error(sink, call, &"the task is closing");

        return;
    }

    match services.terminals.create(terminal) {
        Ok(hub) => {
            sink.send_event(&Event::Created { call, id: hub.id() });
            if !hub.attach(sink, CREATE_HISTORY_LINES) {
                send_missing(sink, hub.id());
            }
        }
        Err(error) => send_error(sink, call, &error),
    }
}

/// Review and close: the requests that read a diff, approve or close a task.
fn handle_review(services: &Arc<Services>, sink: &Arc<ClientSink>, request: Request) {
    let projects = &services.projects;

    match request {
        Request::TaskDiff {
            call,
            task,
            source,
            known,
        } => {
            let Some(running) = sink.begin_diff(task, source) else {
                send_error(sink, call, &"busy: the previous diff request still runs");

                return;
            };

            let projects = Arc::clone(projects);
            run_slow(sink, "task-diff", call, move |sink| {
                let diff = projects.task_diff(task, source);
                drop(running);

                match diff {
                    Ok(diff) => {
                        let diff = (known != Some(diff.fingerprint)).then_some(diff);

                        sink.send_event(&Event::TaskDiff { call, diff });
                    }
                    Err(error) => send_error(sink, call, &error),
                }
            });
        }
        Request::ApproveTask {
            call,
            task,
            fingerprint,
        } => {
            let services = Arc::clone(services);
            run_slow(sink, "approve-task", call, move |sink| {
                match services.projects.approve(task, fingerprint) {
                    Ok(task) => {
                        services.clients.broadcast(&Event::TaskChanged { task });
                        sink.send_event(&Event::Done { call });
                    }
                    Err(error) => send_error(sink, call, &error),
                }
            });
        }
        Request::CheckClose { call, task } => {
            let services = Arc::clone(services);
            run_slow(
                sink,
                "check-close",
                call,
                move |sink| match tasks::close_check(&services, task) {
                    Ok(check) => {
                        sink.send_event(&Event::CloseCheck { call, check });
                    }
                    Err(error) => send_error(sink, call, &error),
                },
            );
        }
        Request::CloseTask {
            call,
            task,
            confirmed,
        } => {
            let services = Arc::clone(services);
            run_slow(sink, "close-task", call, move |sink| {
                // A push or merge of the pull request keeps the worktree in use.
                let closed = services
                    .pull_requests
                    .begin(task)
                    .and_then(|_busy| tasks::close(&services, task, &confirmed));

                done_or_error(sink, call, closed);
            });
        }
        _ => {}
    }
}

/// GitHub status, branches, local checks and the pull request panel.
fn handle_github(services: &Arc<Services>, sink: &Arc<ClientSink>, request: Request) {
    let services = Arc::clone(services);

    match request {
        Request::GithubStatus { call, project } => {
            run_slow(sink, "github-status", call, move |sink| {
                let status = services
                    .projects
                    .github_status(services.pull_requests.github(), project);

                match status {
                    Ok(status) => {
                        sink.send_event(&Event::GithubStatus { call, status });
                    }
                    Err(error) => send_error(sink, call, &error),
                }
            });
        }
        Request::ListBranches { call, project } => {
            run_slow(sink, "list-branches", call, move |sink| {
                match services.projects.local_branches(project) {
                    Ok(branches) => {
                        sink.send_event(&Event::Branches { call, branches });
                    }
                    Err(error) => send_error(sink, call, &error),
                }
            });
        }
        Request::RunChecks { call, task } => {
            run_slow(sink, "run-checks", call, move |sink| {
                done_or_error(sink, call, services.checks.start(task));
            });
        }
        Request::PrPanel { call, task } => {
            run_slow(
                sink,
                "pr-panel",
                call,
                move |sink| match pull_requests::panel(&services, task) {
                    Ok(panel) => {
                        sink.send_event(&Event::PrPanel { call, panel });
                    }
                    Err(error) => send_error(sink, call, &error),
                },
            );
        }
        Request::DraftPr { call, task } => {
            run_slow(sink, "draft-pr", call, move |sink| {
                done_or_error(sink, call, pull_requests::request_draft(&services, task));
            });
        }
        _ => {}
    }
}

/// Creating, updating, reading and merging the pull request.
fn handle_pull_requests(services: &Arc<Services>, sink: &Arc<ClientSink>, request: Request) {
    let services = Arc::clone(services);

    match request {
        Request::CreatePr {
            call,
            task,
            draft,
            as_draft,
            close_after_merge,
            skip_checks,
        } => {
            run_slow(sink, "create-pr", call, move |sink| {
                let request = NewRequest {
                    draft,
                    as_draft,
                    close_after_merge,
                    skip_checks,
                };

                done_or_error(sink, call, pull_requests::create(&services, task, &request));
            });
        }
        Request::UpdatePr {
            call,
            task,
            skip_checks,
        } => {
            run_slow(sink, "update-pr", call, move |sink| {
                done_or_error(
                    sink,
                    call,
                    pull_requests::update(&services, task, skip_checks),
                );
            });
        }
        Request::PrStatus { call, task } => {
            run_slow(
                sink,
                "pr-status",
                call,
                move |sink| match pull_requests::status(&services, task) {
                    Ok(pull_request) => {
                        sink.send_event(&Event::PrStatus { call, pull_request });
                    }
                    Err(error) => send_error(sink, call, &error),
                },
            );
        }
        Request::MarkPrReady { call, task } => {
            run_slow(sink, "mark-pr-ready", call, move |sink| {
                done_or_error(sink, call, pull_requests::mark_ready(&services, task));
            });
        }
        Request::AddReviewers { call, task, logins } => {
            run_slow(sink, "add-reviewers", call, move |sink| {
                done_or_error(
                    sink,
                    call,
                    pull_requests::add_reviewers(&services, task, &logins),
                );
            });
        }
        Request::MergePr { call, task, method } => {
            run_slow(sink, "merge-pr", call, move |sink| {
                done_or_error(sink, call, pull_requests::merge(&services, task, method));
            });
        }
        Request::SetCloseAfterMerge { call, task, close } => {
            run_slow(sink, "set-close-after-merge", call, move |sink| {
                done_or_error(
                    sink,
                    call,
                    pull_requests::set_close_after_merge(&services, task, close),
                );
            });
        }
        _ => {}
    }
}

/// Review comments: list, add, delete, resolve and send.
fn handle_comments(services: &Arc<Services>, sink: &Arc<ClientSink>, request: Request) {
    let services = Arc::clone(services);

    match request {
        Request::ListComments { call, task } => {
            run_slow(sink, "list-comments", call, move |sink| {
                match services.projects.list_comments(task) {
                    Ok(comments) => {
                        sink.send_event(&Event::Comments { call, comments });
                    }
                    Err(error) => send_error(sink, call, &error),
                }
            });
        }
        Request::AddComment { call, comment } => {
            run_slow(sink, "add-comment", call, move |sink| {
                done_or_error(sink, call, comments::add(&services, &comment));
            });
        }
        Request::DeleteComment { call, id } => {
            run_slow(sink, "delete-comment", call, move |sink| {
                done_or_error(sink, call, comments::delete(&services, id));
            });
        }
        Request::ResolveComment { call, id, resolved } => {
            run_slow(sink, "resolve-comment", call, move |sink| {
                done_or_error(sink, call, comments::resolve(&services, id, resolved));
            });
        }
        Request::SendComments { call, task } => {
            run_slow(sink, "send-comments", call, move |sink| {
                done_or_error(sink, call, comments::send_pending(&services, task));
            });
        }
        _ => {}
    }
}

fn done_or_error(sink: &ClientSink, call: CallId, result: Result<()>) {
    match result {
        Ok(()) => {
            sink.send_event(&Event::Done { call });
        }
        Err(error) => send_error(sink, call, &error),
    }
}

/// The app typed into, resized or killed a terminal.
fn forward_input(
    services: &Services,
    sink: &ClientSink,
    hub: &Hub,
    id: TerminalId,
    input: TerminalInput,
) {
    // An agent chose the size of its process terminal; the app keeps out of it.
    if matches!(input, TerminalInput::Resize(_)) && hub.fixed_size().is_some() {
        return;
    }

    let killed = matches!(input, TerminalInput::Kill);
    services.agents.app_input(id, &input);
    if killed {
        services.processes.stopped_by_user(id);
        services.checks.user_closed(id);
    }

    if !hub.send_input(input) {
        send_missing(sink, id);
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
