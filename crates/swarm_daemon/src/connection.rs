//! One client: a reader thread (this one) handles requests in order, and a writer
//! thread sends the queued frames, so a slow client never blocks a terminal.

use std::fmt::Write as _;
use std::io::{BufReader, BufWriter, ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd as _;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use anyhow::Result;
use swarm_core::Projects;
use swarm_core::github::Github;
use swarm_core::prompts;
use swarm_core::system_check::{self, AgentCommands, Tools};
use swarm_protocol::{
    AgentKind, CallId, CreateTerminal, Event, FrameError, MESSAGES_LISTED, Request, SystemCheck,
    TaskInfo, TerminalId, VERSION, read, write,
};
use swarm_term::TerminalInput;

use crate::hub::{CREATE_HISTORY_LINES, ClientSink, Frame, Hub, QUEUE_FRAMES};
use crate::peer_signature::{self, AuditToken};
use crate::pull_requests::{self, NewRequest};
use crate::usage::configured_command;
use crate::{Services, comments, stats, task_files, tasks};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Longest parent chain the peer check follows.
const MAX_ANCESTORS: usize = 256;

pub(crate) fn serve(services: &Arc<Services>, client: u64, mut stream: UnixStream) -> Result<()> {
    let pinned = match admit(&stream) {
        Ok(token) => token,
        Err(reason) => {
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
    };

    let probe = stream.try_clone()?;
    let token_now = || peer_signature::peer_audit_token(&probe);

    let (frames, queue) = flume::bounded::<Frame>(QUEUE_FRAMES);

    let writer_stream = stream.try_clone()?;
    thread::Builder::new()
        .name(format!("client-{client}-writer"))
        .spawn(move || write_frames(&queue, writer_stream))?;

    let sink = Arc::new(ClientSink::new(client, frames, Some(stream.try_clone()?)));
    let mut reader = BufReader::new(stream);
    let mut buffer = Vec::new();

    if handshake(&sink, &mut reader, &mut buffer, &pinned, &token_now) {
        services.clients.add(&sink);
        services.usage.client_connected();

        loop {
            match read_pinned(&mut reader, &mut buffer, &pinned, &token_now) {
                Ok(request) => handle(services, &sink, request),
                Err(error) => {
                    log_end(client, &error);
                    break;
                }
            }
        }
    }

    services.clients.remove(client);
    services.terminals.detach_client(client);

    Ok(())
}

/// Why a read from a pinned client ended.
#[derive(Debug)]
enum PinnedError {
    Frame(FrameError),
    /// A different process holds the client's end of the socket now.
    TokenChanged,
}

/// Reads one request, then checks the peer's audit token is still the pinned one.
/// `exec` and a handover of the socket both change the token.
fn read_pinned(
    reader: &mut impl Read,
    buffer: &mut Vec<u8>,
    pinned: &AuditToken,
    token_now: &impl Fn() -> Option<AuditToken>,
) -> Result<Request, PinnedError> {
    let request = read::<Request>(reader, buffer).map_err(PinnedError::Frame)?;

    match token_now() {
        Some(token) if token == *pinned => Ok(request),
        _ => Err(PinnedError::TokenChanged),
    }
}

fn log_end(client: u64, error: &PinnedError) {
    match error {
        PinnedError::Frame(FrameError::Io(error)) if error.kind() == ErrorKind::UnexpectedEof => {}
        PinnedError::Frame(error) => log::info!("client {client} ended: {error}"),
        PinnedError::TokenChanged => {
            log::warn!("client {client} closed: the connecting process changed");
        }
    }
}

/// The daemon's version, protocol and build. Every `Hello` it sends says the same.
fn hello() -> Event {
    Event::Hello {
        version: VERSION,
        pid: std::process::id(),
        build: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

/// The client must send `Hello` with our version, then answer our challenge with `Prove`.
fn handshake(
    sink: &ClientSink,
    reader: &mut impl Read,
    buffer: &mut Vec<u8>,
    pinned: &AuditToken,
    token_now: &impl Fn() -> Option<AuditToken>,
) -> bool {
    match read_pinned(reader, buffer, pinned, token_now) {
        Ok(Request::Hello { version }) if version == VERSION => {}
        // Answer with our own Hello and close: only `Hello` decodes across versions.
        Ok(Request::Hello { version }) => {
            log::info!(
                "client {} speaks protocol version {version}; closing",
                sink.id()
            );
            sink.send_event(&hello());

            return false;
        }
        Ok(_) => {
            send_handshake_error(sink, "the first request must be Hello");

            return false;
        }
        Err(error) => {
            log_end(sink.id(), &error);

            return false;
        }
    }

    sink.send_event(&hello());

    let Some(nonce) = new_nonce() else {
        log::error!("client {}: cannot make a challenge nonce", sink.id());
        send_handshake_error(sink, "cannot make a challenge");

        return false;
    };
    sink.send_event(&Event::Challenge {
        nonce: nonce.clone(),
    });

    match read_pinned(reader, buffer, pinned, token_now) {
        Ok(Request::Prove { nonce: answer }) if answer == nonce => true,
        Ok(_) => {
            send_handshake_error(sink, "the request after Hello must be Prove with the nonce");

            false
        }
        Err(error) => {
            log_end(sink.id(), &error);

            false
        }
    }
}

fn send_handshake_error(sink: &ClientSink, message: &str) {
    sink.send_event(&Event::Error {
        call: None,
        id: None,
        message: message.to_owned(),
    });
}

/// 32 random bytes as hex.
fn new_nonce() -> Option<String> {
    let mut bytes = [0_u8; 32];

    // SAFETY: the pointer is valid for `bytes.len()` writable bytes, at most 256.
    let result = unsafe { libc::getentropy(bytes.as_mut_ptr().cast(), bytes.len()) };
    if result != 0 {
        return None;
    }

    let mut nonce = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(nonce, "{byte:02x}").ok()?;
    }

    Some(nonce)
}

fn handle(services: &Arc<Services>, sink: &Arc<ClientSink>, request: Request) {
    let (terminals, projects) = (&services.terminals, &services.projects);

    match request {
        Request::Hello { .. } => {
            sink.send_event(&hello());
        }
        Request::Prove { .. } => {
            sink.send_event(&Event::Error {
                call: None,
                id: None,
                message: "Prove is only valid right after Hello".to_owned(),
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
        Request::SystemCheck { call } => system_check(services, sink, call),
        Request::Stats { call } => send_stats(services, sink, call),
        Request::Catches { call } => send_catches(services, sink, call),
        Request::RecordCatch { call, creature } => record_catch(services, sink, call, &creature),
        Request::Usage { call } => {
            sink.send_event(&Event::Usage {
                call,
                usage: services.usage.current(),
            });
        }
        Request::TaskFiles { call, task } => send_task_files(services, sink, call, task),
        Request::StopAgents { call } => {
            services.agents.stop_all();
            sink.send_event(&Event::Done { call });
        }
        request @ (Request::ProjectConfig { .. }
        | Request::SaveProjectConfig { .. }
        | Request::Prompts { .. }
        | Request::CustomizePrompt { .. }
        | Request::ResetPrompt { .. }) => handle_settings(services, sink, request),
        request @ (Request::TaskDiff { .. }
        | Request::FileHunks { .. }
        | Request::FileTexts { .. }
        | Request::ApproveTask { .. }
        | Request::ReviewMarks { .. }
        | Request::SetReviewMarks { .. }
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
        | Request::CheckRemoveProject { .. }
        | Request::RemoveProject { .. }
        | Request::StartOrchestrator { .. }
        | Request::ResumeAgent { .. }
        | Request::PlanDraft { .. }
        | Request::SavePlanDraft { .. }
        | Request::CreateTask { .. }) => handle_projects(services, sink, request),
    }
}

fn send_task_files(
    services: &Arc<Services>,
    sink: &Arc<ClientSink>,
    call: CallId,
    task: swarm_protocol::TaskId,
) {
    let services = Arc::clone(services);

    run_slow(
        sink,
        "task-files",
        call,
        move |sink| match task_files::list(&services, task) {
            Ok((files, omitted)) => {
                sink.send_event(&Event::TaskFiles {
                    call,
                    files,
                    omitted,
                });
            }
            Err(error) => send_error(sink, call, &error),
        },
    );
}

fn send_stats(services: &Services, sink: &ClientSink, call: CallId) {
    match stats::current(services) {
        Ok(stats) => {
            sink.send_event(&Event::Stats { call, stats });
        }
        Err(error) => send_error(sink, call, &error),
    }
}

fn send_catches(services: &Services, sink: &ClientSink, call: CallId) {
    match stats::catches(services) {
        Ok(catches) => {
            sink.send_event(&Event::Catches { call, catches });
        }
        Err(error) => send_error(sink, call, &error),
    }
}

fn record_catch(services: &Services, sink: &ClientSink, call: CallId, creature: &str) {
    match stats::record_catch(services, creature) {
        Ok(caught) => {
            sink.send_event(&Event::CatchRecorded { call, caught });
        }
        Err(error) => send_error(sink, call, &error),
    }
}

/// Checks the tools Swarm needs on its own thread: the commands may take seconds.
fn system_check(services: &Arc<Services>, sink: &Arc<ClientSink>, call: CallId) {
    let services = Arc::clone(services);

    run_slow(sink, "system-check", call, move |sink| {
        sink.send_event(&Event::SystemCheck {
            call,
            check: services.system_check.run(),
        });
    });
}

/// The tools Swarm needs, as the daemon and its agents see them (PLAN.md, System check).
pub(crate) struct SystemChecks {
    tools: Tools,
    /// The settings file that names the agent commands.
    settings: Option<PathBuf>,
    github: Github,
}

impl SystemChecks {
    pub(crate) fn new(tools: Tools, settings: Option<PathBuf>, github: Github) -> Self {
        Self {
            tools,
            settings,
            github,
        }
    }

    /// Checks every tool now. The settings are read again, so a command the user changed
    /// counts at once.
    fn run(&self) -> SystemCheck {
        let claude = configured_command(self.settings.as_deref(), AgentKind::ClaudeCode);
        let codex = configured_command(self.settings.as_deref(), AgentKind::Codex);

        system_check::check(
            &self.tools,
            &AgentCommands {
                claude: &claude,
                codex: &codex,
            },
            &self.github,
        )
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
        Request::CheckRemoveProject { call, project } => {
            let services = Arc::clone(services);
            run_slow(
                sink,
                "check-remove-project",
                call,
                move |sink| match tasks::remove_check(&services, project) {
                    Ok(check) => {
                        sink.send_event(&Event::RemoveCheck { call, check });
                    }
                    Err(error) => send_error(sink, call, &error),
                },
            );
        }
        Request::RemoveProject {
            call,
            project,
            confirmed,
        } => {
            let services = Arc::clone(services);
            run_slow(sink, "remove-project", call, move |sink| {
                let removed = tasks::remove_project(&services, project, &confirmed);

                done_or_error(sink, call, removed);
            });
        }
        Request::StartOrchestrator {
            call,
            project,
            kind,
            plan,
        } => {
            let services = Arc::clone(services);
            run_slow(
                sink,
                "start-orchestrator",
                call,
                move |sink| match tasks::start_orchestrator(&services, project, kind, plan) {
                    Ok(agent) => {
                        sink.send_event(&Event::AgentStarted { call, agent });
                    }
                    Err(error) => send_error(sink, call, &error),
                },
            );
        }
        Request::ResumeAgent { call, agent } => {
            let services = Arc::clone(services);
            run_slow(sink, "resume-agent", call, move |sink| {
                match services.agents.resume(agent) {
                    Ok(agent) => {
                        sink.send_event(&Event::AgentStarted { call, agent });
                    }
                    Err(error) => send_error(sink, call, &error),
                }
            });
        }
        Request::PlanDraft { call, project } => {
            let services = Arc::clone(services);
            run_slow(
                sink,
                "plan-draft",
                call,
                move |sink| match tasks::plan_draft(&services, project) {
                    Ok(draft) => {
                        sink.send_event(&Event::PlanDraft { call, draft });
                    }
                    Err(error) => send_error(sink, call, &error),
                },
            );
        }
        Request::SavePlanDraft {
            call,
            project,
            text,
        } => {
            let services = Arc::clone(services);
            run_slow(sink, "save-plan-draft", call, move |sink| {
                let saved = tasks::save_plan_draft(&services, project, text.as_deref());

                done_or_error(sink, call, saved);
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
    if services.agents.is_removing_key(terminal.task.as_deref()) {
        send_error(sink, call, &"the project is being removed");

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

/// Settings pages: a project's config file and the prompt layers.
fn handle_settings(services: &Arc<Services>, sink: &Arc<ClientSink>, request: Request) {
    let services = Arc::clone(services);

    match request {
        Request::ProjectConfig { call, project } => {
            run_slow(sink, "project-config", call, move |sink| {
                match services.projects.project_config(project) {
                    Ok((fields, error)) => {
                        let pre_push_hook = services
                            .projects
                            .project_path(project)
                            .ok()
                            .and_then(|root| swarm_core::git::pre_push_hook(&root));

                        sink.send_event(&Event::ProjectConfig {
                            call,
                            fields,
                            error,
                            pre_push_hook,
                        });
                    }
                    Err(error) => send_error(sink, call, &error),
                }
            });
        }
        Request::SaveProjectConfig {
            call,
            project,
            base,
            fields,
        } => {
            run_slow(sink, "save-project-config", call, move |sink| {
                let saved = services
                    .projects
                    .save_project_config(project, &base, &fields);

                done_or_error(sink, call, saved.map_err(Into::into));
            });
        }
        Request::Prompts { call, project } => {
            run_slow(sink, "prompts", call, move |sink| {
                let listed = project
                    .map(|project| services.projects.project_path(project))
                    .transpose()
                    .and_then(|root| {
                        prompts::list(services.config_dir.as_deref(), root.as_deref())
                    });

                match listed {
                    Ok(prompts) => {
                        sink.send_event(&Event::Prompts { call, prompts });
                    }
                    Err(error) => send_error(sink, call, &error),
                }
            });
        }
        Request::CustomizePrompt { call, name } => {
            run_slow(
                sink,
                "customize-prompt",
                call,
                move |sink| match prompts::customize(&name, services.config_dir.as_deref()) {
                    Ok(path) => {
                        sink.send_event(&Event::PromptFile { call, path });
                    }
                    Err(error) => send_error(sink, call, &error),
                },
            );
        }
        Request::ResetPrompt { call, name } => {
            run_slow(sink, "reset-prompt", call, move |sink| {
                let reset = prompts::reset(&name, services.config_dir.as_deref());

                done_or_error(sink, call, reset.map_err(Into::into));
            });
        }
        _ => {}
    }
}

/// Sends both sides of a file for syntax highlighting, off the connection thread.
fn send_file_texts(
    projects: &Arc<Projects>,
    sink: &Arc<ClientSink>,
    call: CallId,
    (task, source, worktree, path): (
        swarm_protocol::TaskId,
        swarm_protocol::ReviewSource,
        PathBuf,
        PathBuf,
    ),
) {
    let projects = Arc::clone(projects);

    run_slow(sink, "file-texts", call, move |sink| {
        match projects.file_texts(task, source, &worktree, &path) {
            Ok(texts) => {
                sink.send_event(&Event::FileTexts {
                    call,
                    old: texts.old,
                    new: texts.new,
                });
            }
            Err(error) => send_error(sink, call, &error),
        }
    });
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
                        let send =
                            sink.needs_diff(task, source, known, diff.fingerprint, &diff.head);
                        let diff = send.then_some(diff);

                        sink.send_event(&Event::TaskDiff { call, diff });
                    }
                    Err(error) => send_error(sink, call, &error),
                }
            });
        }
        Request::FileHunks {
            call,
            task,
            source,
            worktree,
            path,
        } => {
            // Not behind `begin_diff`: loading a file never waits for a diff refresh.
            let projects = Arc::clone(projects);
            run_slow(sink, "file-hunks", call, move |sink| {
                match projects.file_hunks(task, source, &worktree, &path) {
                    Ok(file) => {
                        sink.send_event(&Event::FileHunks { call, file });
                    }
                    Err(error) => send_error(sink, call, &error),
                }
            });
        }
        Request::FileTexts {
            call,
            task,
            source,
            worktree,
            path,
        } => send_file_texts(projects, sink, call, (task, source, worktree, path)),
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
        Request::ReviewMarks { call, task } => {
            let projects = Arc::clone(projects);
            run_slow(sink, "review-marks", call, move |sink| {
                match projects.review_marks(task) {
                    Ok(marks) => {
                        sink.send_event(&Event::ReviewMarks { call, marks });
                    }
                    Err(error) => send_error(sink, call, &error),
                }
            });
        }
        Request::SetReviewMarks { call, task, marks } => {
            let projects = Arc::clone(projects);
            run_slow(sink, "set-review-marks", call, move |sink| {
                let saved = projects.set_review_marks(task, &marks);

                done_or_error(sink, call, saved.map_err(Into::into));
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
                let closed = tasks::close_asked(&services, task, &confirmed);

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
    let killed = matches!(input, TerminalInput::Kill);
    let sent_line = services.agents.app_input(id, &input);
    if killed {
        services.processes.stopped_by_user(id);
        services.checks.user_closed(id);
    }

    let reached = hub.send_input(input);

    // After the input, so a message never lands before the line the user just sent.
    if let Some(agent) = sent_line {
        services.agents.deliver_unread(agent);
    }

    if !reached {
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

/// The peer's audit token if it may talk to the daemon, else why not (ADR 0008).
/// The caller pins the token for the life of the connection.
fn admit(stream: &UnixStream) -> Result<AuditToken, String> {
    let token = peer_signature::peer_audit_token(stream)
        .ok_or_else(|| "cannot tell which process connected".to_owned())?;

    match first_refusal(ancestry_refusal(stream), || peer_signature::refusal(&token)) {
        Some(reason) => Err(reason),
        None => Ok(token),
    }
}

/// The ancestry refusal wins; the signature is only checked for a peer that passed it.
fn first_refusal(
    ancestry: Option<String>,
    signature: impl FnOnce() -> Option<String>,
) -> Option<String> {
    ancestry.or_else(signature)
}

fn ancestry_refusal(stream: &UnixStream) -> Option<String> {
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
    use std::cell::Cell;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

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

    #[test]
    fn the_ancestry_refusal_wins_over_the_signature() {
        let checked = Cell::new(false);

        let refusal = first_refusal(Some("ancestry".to_owned()), || {
            checked.set(true);
            Some("signature".to_owned())
        });

        assert_eq!(
            (refusal.as_deref(), checked.get()),
            (Some("ancestry"), false)
        );
    }

    #[test]
    fn the_signature_refusal_applies_when_ancestry_passes() {
        let refusal = first_refusal(None, || Some("signature".to_owned()));

        assert_eq!(refusal.as_deref(), Some("signature"));
    }

    const PINNED: AuditToken = [1; 32];
    const OTHER: AuditToken = [2; 32];
    const WAIT: Duration = Duration::from_secs(5);

    /// A handshake running on a thread against a client end the test holds.
    struct Handshake {
        client: UnixStream,
        events: flume::Receiver<Frame>,
        done: thread::JoinHandle<bool>,
    }

    impl Handshake {
        /// `token_now` is called once per frame the daemon reads.
        fn start(token_now: impl Fn() -> Option<AuditToken> + Send + 'static) -> Self {
            let (client, server) = UnixStream::pair().expect("pair");
            let (frames, events) = flume::bounded::<Frame>(16);

            let done = thread::spawn(move || {
                let sink = ClientSink::new(1, frames, None);
                let mut reader = BufReader::new(server);

                handshake(&sink, &mut reader, &mut Vec::new(), &PINNED, &token_now)
            });

            Self {
                client,
                events,
                done,
            }
        }

        fn send(&mut self, request: &Request) {
            write(&mut self.client, request).expect("send");
        }

        fn next_event(&self) -> Event {
            let frame = self.events.recv_timeout(WAIT).expect("an event");

            read(&mut &frame[..], &mut Vec::new()).expect("decode")
        }

        fn nonce(&self) -> String {
            assert!(matches!(self.next_event(), Event::Hello { .. }));

            match self.next_event() {
                Event::Challenge { nonce } => nonce,
                other => panic!("expected a challenge, got {other:?}"),
            }
        }

        fn finish(self) -> bool {
            self.done.join().expect("handshake thread")
        }
    }

    const SAME_TOKEN: fn() -> Option<AuditToken> = || Some(PINNED);

    #[test]
    fn the_right_nonce_is_served() {
        let mut client = Handshake::start(SAME_TOKEN);
        client.send(&Request::Hello { version: VERSION });

        let nonce = client.nonce();
        client.send(&Request::Prove { nonce });

        assert!(client.finish());
    }

    #[test]
    fn a_request_instead_of_prove_is_closed_with_an_error() {
        let mut client = Handshake::start(SAME_TOKEN);
        client.send(&Request::Hello { version: VERSION });
        client.nonce();

        client.send(&Request::List { call: CallId(1) });

        assert!(matches!(client.next_event(), Event::Error { .. }));
        assert!(!client.finish());
    }

    #[test]
    fn a_wrong_nonce_is_closed_with_an_error() {
        let mut client = Handshake::start(SAME_TOKEN);
        client.send(&Request::Hello { version: VERSION });
        client.nonce();

        client.send(&Request::Prove {
            nonce: "wrong".to_owned(),
        });

        assert!(matches!(client.next_event(), Event::Error { .. }));
        assert!(!client.finish());
    }

    #[test]
    fn a_token_that_changes_after_hello_closes_the_connection() {
        let reads = AtomicUsize::new(0);
        let mut client = Handshake::start(move || {
            Some(if reads.fetch_add(1, Ordering::SeqCst) == 0 {
                OTHER
            } else {
                PINNED
            })
        });

        client.send(&Request::Hello { version: VERSION });

        assert!(!client.finish());
    }

    #[test]
    fn a_token_that_changes_after_the_proof_closes_the_connection() {
        let reads = AtomicUsize::new(0);
        let mut client = Handshake::start(move || {
            Some(if reads.fetch_add(1, Ordering::SeqCst) < 1 {
                PINNED
            } else {
                OTHER
            })
        });
        client.send(&Request::Hello { version: VERSION });

        let nonce = client.nonce();
        client.send(&Request::Prove { nonce });

        assert!(!client.finish());
    }

    #[test]
    fn a_token_that_cannot_be_read_closes_the_connection() {
        let mut client = Handshake::start(|| None);

        client.send(&Request::Hello { version: VERSION });

        assert!(!client.finish());
    }

    #[test]
    fn a_pinned_read_returns_the_request_while_the_token_holds() {
        let mut bytes = Vec::new();
        write(&mut bytes, &Request::List { call: CallId(7) }).expect("encode");

        let request = read_pinned(&mut &bytes[..], &mut Vec::new(), &PINNED, &SAME_TOKEN);

        assert!(matches!(request, Ok(Request::List { call: CallId(7) })));
    }

    #[test]
    fn a_pinned_read_fails_when_the_token_changed() {
        let mut bytes = Vec::new();
        write(&mut bytes, &Request::List { call: CallId(7) }).expect("encode");

        let request = read_pinned(&mut &bytes[..], &mut Vec::new(), &PINNED, &|| Some(OTHER));

        assert!(matches!(request, Err(PinnedError::TokenChanged)));
    }

    #[test]
    fn a_pinned_read_fails_when_the_token_cannot_be_read() {
        let mut bytes = Vec::new();
        write(&mut bytes, &Request::List { call: CallId(7) }).expect("encode");

        let request = read_pinned(&mut &bytes[..], &mut Vec::new(), &PINNED, &|| None);

        assert!(matches!(request, Err(PinnedError::TokenChanged)));
    }

    #[test]
    fn a_nonce_is_64_hex_characters_and_never_repeats() {
        let first = new_nonce().expect("nonce");
        let second = new_nonce().expect("nonce");

        assert!(first.len() == 64 && first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }
}
