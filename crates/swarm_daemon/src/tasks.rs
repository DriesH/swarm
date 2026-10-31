//! Starting a task or a worker: the work in `swarm_core`, then the setup script as a
//! terminal, then the agent.

use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use swarm_core::{PreparedTask, PreparedWorker, Projects};
use swarm_protocol::{
    AgentId, AgentInfo, AgentKind, AgentRole, CreateTask, CreateTerminal, Event, ProjectId, TaskId,
    TaskInfo, TerminalId, TerminalKind,
};
use swarm_term::TerminalInput;
use swarm_term::{ExitStatus, Palette, Rgb, Size};

use crate::Services;
use crate::agents::{CommandNotFound, Place, Start};
use crate::hub::{Extras, Terminals};

/// Default size and colors of a setup or agent terminal; the app resizes it when it
/// attaches.
pub(crate) const TERMINAL_SIZE: Size = Size {
    cols: 100,
    rows: 30,
    cell_width: 8,
    cell_height: 16,
};

pub(crate) const TERMINAL_PALETTE: Palette = Palette {
    fg: Rgb(230, 230, 230),
    bg: Rgb(10, 10, 12),
    cursor: Rgb(255, 255, 255),
    ansi: [
        Rgb(0, 0, 0),
        Rgb(205, 49, 49),
        Rgb(13, 188, 121),
        Rgb(229, 229, 16),
        Rgb(36, 114, 200),
        Rgb(188, 63, 188),
        Rgb(17, 168, 205),
        Rgb(229, 229, 229),
        Rgb(102, 102, 102),
        Rgb(241, 76, 76),
        Rgb(35, 209, 139),
        Rgb(245, 245, 67),
        Rgb(59, 142, 234),
        Rgb(214, 112, 214),
        Rgb(41, 184, 219),
        Rgb(255, 255, 255),
    ],
};

/// How long the processes of a closing task get to end on SIGTERM before SIGKILL.
const CLOSE_GRACE: Duration = Duration::from_secs(5);

/// How long a close waits for processes after SIGKILL.
const CLOSE_KILL_WAIT: Duration = Duration::from_secs(5);

const CLOSE_POLL: Duration = Duration::from_millis(25);

/// The name of a Start orchestrator.
const START_NAME: &str = "start";

/// How the setup script went, as the orchestrator needs to know it.
enum Setup {
    None,
    /// The script could not even start.
    Failed,
    Running(flume::Receiver<Option<ExitStatus>>),
}

/// Makes the task and starts its setup script without waiting for it. `reply` gets
/// the task first; the orchestrator starts after it, once the setup has ended.
/// `created_by` is the Start orchestrator that asked for the task, if one did.
pub(crate) fn create(
    services: &Services,
    request: &CreateTask,
    created_by: Option<AgentId>,
    reply: impl FnOnce(&TaskInfo),
) -> Result<()> {
    let plan = request
        .orchestrator
        .map(|kind| services.agents.plan(kind, AgentRole::Orchestrator))
        .transpose()?;

    let PreparedTask {
        mut task,
        setup,
        env,
    } = services.projects.create_task_by(request, created_by)?;

    let agent = match plan {
        Some(plan) => match services.agents.register(&Place::of_task(&task), plan) {
            Ok(info) => {
                task.agents.push(info.clone());

                Some(info.id)
            }
            Err(error) => {
                services.projects.discard_task(&task);

                return Err(error.context("cannot record the orchestrator"));
            }
        },
        None => None,
    };

    let setup = match setup {
        None => Setup::None,
        Some(script) => match start_setup(
            &services.terminals,
            &services.projects,
            &SetupSite {
                task: task.id,
                cwd: &task.worktree,
                record: true,
            },
            script,
            env.clone(),
        ) {
            Ok((terminal, done)) => {
                task.setup_terminal = Some(terminal);

                Setup::Running(done)
            }
            Err(error) => {
                log::error!("cannot start the setup of task {}: {error:#}", task.id.0);

                Setup::Failed
            }
        },
    };

    reply(&task);

    if let Some(agent) = agent {
        let start = Start {
            agent,
            task_name: task.name.clone(),
            env,
            first_message: request.first_message.clone(),
        };
        start_agent(services, start, setup);
    }

    Ok(())
}

/// Closes a task: stops its agents and process terminals, then removes its worktrees
/// and tells every client. Nothing new starts in the task from the first step on.
pub(crate) fn close(services: &Services, id: TaskId) -> Result<()> {
    services.projects.task(id)?;
    if !services.agents.begin_close(id) {
        bail!("task {} is closing already", id.0);
    }

    let result = stop_and_close(services, id);
    if result.is_err() {
        services.agents.abort_close(id);
    }

    result
}

fn stop_and_close(services: &Services, id: TaskId) -> Result<()> {
    stop_processes(services, id);

    let closed = services.projects.close_task(id)?;

    for agent in closed.stopped {
        services.clients.broadcast(&Event::AgentChanged { agent });
    }
    services
        .clients
        .broadcast(&Event::TaskChanged { task: closed.task });

    Ok(())
}

/// Ends every agent and process terminal of the task: SIGTERM, then SIGKILL after
/// [`CLOSE_GRACE`]. Repeats while new ones show up from a request that was under way.
fn stop_processes(services: &Services, task: TaskId) {
    let key = task.terminal_key();

    for _ in 0..3 {
        let mut running = services.agents.stop_task_agents(task);
        running.extend(
            services
                .terminals
                .list()
                .into_iter()
                .filter(|info| info.task.as_deref() == Some(key.as_str()))
                .filter(|info| info.kind != TerminalKind::User)
                .map(|info| info.id),
        );
        running.sort_unstable();
        running.dedup();

        if running.is_empty() && !services.agents.has_live(task) {
            return;
        }

        for terminal in &running {
            let Some(hub) = services.terminals.get(*terminal) else {
                continue;
            };
            if !hub.signal(libc::SIGTERM) {
                hub.send_input(TerminalInput::Kill);
            }
        }
        if wait_until_ended(services, task, &running, CLOSE_GRACE) {
            continue;
        }

        for terminal in &running {
            let Some(hub) = services.terminals.get(*terminal) else {
                continue;
            };
            if !hub.signal(libc::SIGKILL) {
                hub.send_input(TerminalInput::Kill);
            }
        }
        if !wait_until_ended(services, task, &running, CLOSE_KILL_WAIT) {
            log::warn!("task {} still has processes after SIGKILL", task.0);

            return;
        }
    }
}

/// True when the terminals are gone and their agents have ended, within `timeout`.
fn wait_until_ended(
    services: &Services,
    task: TaskId,
    terminals: &[TerminalId],
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;

    loop {
        let gone = terminals
            .iter()
            .all(|terminal| services.terminals.get(*terminal).is_none());
        if gone && !services.agents.has_live(task) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }

        thread::park_timeout(CLOSE_POLL);
    }
}

/// Starts the project's Start orchestrator in the project root, or returns the one that
/// runs. It has no setup script and no first message: the user speaks first.
pub(crate) fn start_orchestrator(
    services: &Services,
    project: ProjectId,
    kind: Option<AgentKind>,
) -> Result<AgentInfo> {
    let kind = match kind {
        Some(kind) => kind,
        None => services.agents.default_kind()?,
    };
    let plan = services.agents.plan(kind, AgentRole::StartOrchestrator)?;

    let (agent, created) = services.projects.start_agent_or(project, |root, base| {
        services
            .agents
            .register(
                &Place {
                    project,
                    task: None,
                    name: START_NAME,
                    worktree: root,
                    branch: base,
                    ports: &[],
                },
                plan,
            )
            .map_err(|error| swarm_core::Error::Invalid(format!("{error:#}")))
    })?;
    if !created {
        return Ok(services.agents.info(agent.id).unwrap_or(agent));
    }

    services.agents.announce(agent.id);
    let start = Start {
        agent: agent.id,
        task_name: String::new(),
        env: Vec::new(),
        first_message: None,
    };
    start_agent(services, start, Setup::None);

    Ok(services.agents.info(agent.id).unwrap_or(agent))
}

/// Runs the worker's setup script without waiting, then starts the worker with `brief`
/// as its first message. The caller has the worker already; a failure shows as its status.
pub(crate) fn start_worker(
    services: &Services,
    prepared: PreparedWorker,
    task: &TaskInfo,
    brief: String,
) {
    let agent = prepared.agent.id;

    let setup = match prepared.setup {
        None => Setup::None,
        Some(script) => match start_setup(
            &services.terminals,
            &services.projects,
            &SetupSite {
                task: task.id,
                cwd: &prepared.agent.worktree,
                record: false,
            },
            script,
            prepared.env.clone(),
        ) {
            Ok((_, done)) => Setup::Running(done),
            Err(error) => {
                log::error!("cannot start the setup of worker {}: {error:#}", agent.0);

                Setup::Failed
            }
        },
    };

    let start = Start {
        agent,
        task_name: task.name.clone(),
        env: prepared.env,
        first_message: Some(brief),
    };
    start_agent(services, start, setup);
}

/// Waits for the setup on its own thread, then starts the agent or marks it failed.
fn start_agent(services: &Services, start: Start, setup: Setup) {
    let agents = Arc::clone(&services.agents);
    let agent = start.agent;

    let spawned = thread::Builder::new()
        .name(format!("start-agent-{}", agent.0))
        .spawn(move || {
            let setup_worked = match setup {
                Setup::None => true,
                Setup::Failed => false,
                Setup::Running(done) => {
                    matches!(done.recv(), Ok(Some(ExitStatus::Code(0))))
                }
            };
            if !setup_worked {
                agents.fail(agent, "Setup failed");

                return;
            }

            if let Err(error) = agents.start(start) {
                log::error!("cannot start agent {}: {error:#}", agent.0);
                agents.fail(agent, start_failure(&error));
            }
        });

    if let Err(error) = spawned {
        log::error!("cannot start a thread for agent {}: {error}", agent.0);
        services.agents.fail(agent, "Cannot start the agent");
    }
}

/// The short text the status shows; the full error goes to the log.
fn start_failure(error: &anyhow::Error) -> &'static str {
    if error.downcast_ref::<CommandNotFound>().is_some() {
        "Command not found"
    } else {
        "Cannot start"
    }
}

/// Where a setup script runs and whether the task row keeps its terminal and exit.
struct SetupSite<'a> {
    task: TaskId,
    cwd: &'a Path,
    /// True for a task's own setup; a worker's setup leaves the task row alone.
    record: bool,
}

fn start_setup(
    terminals: &Arc<Terminals>,
    projects: &Arc<Projects>,
    site: &SetupSite<'_>,
    script: String,
    env: Vec<(String, String)>,
) -> Result<(TerminalId, flume::Receiver<Option<ExitStatus>>)> {
    let request = CreateTerminal {
        kind: TerminalKind::Process,
        command: Some(vec!["/bin/sh".to_owned(), "-lc".to_owned(), script]),
        cwd: Some(site.cwd.to_owned()),
        size: TERMINAL_SIZE,
        palette: TERMINAL_PALETTE,
        task: Some(site.task.terminal_key()),
    };

    let (started, finished) = (Arc::clone(projects), Arc::clone(projects));
    let (done, ended) = flume::bounded(1);
    let (task_id, record) = (site.task, site.record);
    let extras = Extras {
        env,
        on_started: Some(Box::new(move |terminal| {
            if record && let Err(error) = started.set_setup_terminal(task_id, Some(terminal)) {
                log::error!("cannot record the setup terminal: {error}");
            }
        })),
        on_exit: Some(Box::new(move |exit| {
            if record && let Err(error) = finished.finish_setup(task_id, exit.as_ref()) {
                log::error!("cannot record the end of the setup: {error}");
            }
            if done.send(exit).is_err() {
                log::debug!("nobody waits for the setup of task {}", task_id.0);
            }
        })),
        on_output: None,
        process: None,
    };

    let hub = terminals
        .create_with(request, extras)
        .context("cannot start the setup terminal")?;

    Ok((hub.id(), ended))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_command_gets_a_short_message() {
        let error = anyhow::Error::new(CommandNotFound("claude".to_owned())).context("outer");

        assert_eq!(start_failure(&error), "Command not found");
    }

    #[test]
    fn another_error_gets_the_generic_message() {
        let error = anyhow::anyhow!("cannot write /some/long/path").context("more");

        assert_eq!(start_failure(&error), "Cannot start");
    }
}
