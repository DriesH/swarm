//! What the agent socket does with a call (ADR 0008): who may make it, and the work.
//!
//! The caller is found by its agent ID and token. Its role and its task, not anything
//! it sends, decide what it may do.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use swarm_core::git;
use swarm_core::store::NewMessage;
use swarm_protocol::agent::{
    AgentRequest, AgentSummary, Call, Merged, Reply, Sent, TaskSummary, TaskUntil, TerminalSize,
    TerminalUntil,
};
use swarm_protocol::{
    AgentId, AgentInfo, AgentKind, AgentLifecycle, AgentRole, AgentStatus, CreateTask, Event,
    MessageKind, TaskId, TaskInfo, TerminalId,
};

use crate::Services;
use crate::agent_socket::{MAX_WAITS, Slot, Throttle};
use crate::agents::{Place, Refused, lock};
use crate::messages::Claim;
use crate::processes::Spawn;
use crate::tasks;

/// `wait` without a timeout.
const DEFAULT_WAIT_SECS: u32 = 100;

/// The longest `wait`.
const MAX_WAIT_SECS: u32 = 600;

/// How often `task_wait` looks at the tasks again.
const TASK_POLL: Duration = Duration::from_millis(100);

/// Why a call did not return a value, in the words the agent sees.
struct Failure(String);

impl<E: Into<anyhow::Error>> From<E> for Failure {
    fn from(error: E) -> Self {
        Self(format!("{:#}", error.into()))
    }
}

fn refuse<T>(message: impl Into<String>) -> Result<T, Failure> {
    Err(Failure(message.into()))
}

type Outcome = Result<Value, Failure>;

fn reply_with(result: impl Serialize) -> Outcome {
    Ok(serde_json::to_value(result)?)
}

/// Work that runs after the reply was written. The flag says whether the write worked.
pub(crate) type After = Box<dyn FnOnce(bool) + Send>;

/// Tells whether the caller hung up.
pub(crate) type Gone<'a> = &'a dyn Fn() -> bool;

pub(crate) struct Calls {
    services: Arc<Services>,
    /// Calls that wait (`wait`, `task_wait`, `terminal_wait`) right now.
    waits: Arc<AtomicUsize>,
    wait_warning: Throttle,
}

impl Calls {
    pub(crate) fn new(services: Arc<Services>) -> Self {
        Self {
            services,
            waits: Arc::new(AtomicUsize::new(0)),
            wait_warning: Throttle::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn hold_waits_for_test(&self, count: usize) -> Vec<Slot> {
        (0..count)
            .filter_map(|_| Slot::take(&self.waits, usize::MAX))
            .collect()
    }

    /// Room for one more waiting call, so short calls and hooks always find a place.
    fn wait_slot(&self) -> Result<Slot, Failure> {
        Slot::take(&self.waits, MAX_WAITS).ok_or_else(|| {
            if self.wait_warning.allow(Instant::now()) {
                log::warn!("agent socket: {MAX_WAITS} calls wait already, a wait was refused");
            }

            Failure("too many waits".to_owned())
        })
    }

    /// Checks the caller and runs the call. A caller that is not who it says gets
    /// `Err(Refused)` so the socket can log it. The second value is work for after the
    /// reply was sent.
    pub(crate) fn handle(
        &self,
        request: AgentRequest,
        gone: Gone<'_>,
    ) -> Result<(Reply, Option<After>), Refused> {
        let caller = self
            .services
            .agents
            .authenticate(request.agent, &request.token)?;

        if request.call == Call::AgentExit {
            let agents = Arc::clone(&self.services.agents);
            let id = caller.id;
            let after: After = Box::new(move |written| {
                if written {
                    agents.exit(id);
                }
            });

            return Ok((Reply::success(Value::Null), Some(after)));
        }

        let mut claim = None;
        let reply = match self.run(&caller, request.call, gone, &mut claim) {
            Ok(value) => Reply::success(value),
            Err(Failure(message)) => Reply::failure(message),
        };

        let after = claim.filter(|_| reply.ok).map(|claim| -> After {
            Box::new(move |written| {
                if written {
                    claim.commit();
                }
            })
        });

        Ok((reply, after))
    }

    /// `claim` gets the messages a reply carries. They count as delivered once the
    /// socket wrote the reply.
    fn run(
        &self,
        caller: &AgentInfo,
        call: Call,
        gone: Gone<'_>,
        claim: &mut Option<Claim>,
    ) -> Outcome {
        match call {
            Call::Hook { event } => {
                self.services.agents.hook(caller.id, &event);

                Ok(Value::Null)
            }
            Call::Spawn {
                name,
                kind,
                model,
                brief,
            } => self.spawn(caller, &name, kind, model, brief),
            Call::Send { to, text } => self.send(caller, to, &text),
            Call::Ask { question } => {
                self.to_orchestrator(caller, MessageKind::Question, &question)
            }
            Call::Report { text } => self.to_orchestrator(caller, MessageKind::Report, &text),
            Call::Read => {
                let taken = self.services.messages.claim(caller.id)?;

                inbox(taken, claim)
            }
            Call::Wait { timeout_secs } => {
                let _slot = self.wait_slot()?;

                self.wait(caller, timeout_secs, gone, claim)
            }
            Call::Merge { worker } => self.merge(caller, worker),
            Call::Notify { text } => self.notify(caller, &text),
            Call::Agents => self.agents(caller),
            Call::Confirm => self.confirm(caller),

            Call::TaskCreate {
                name,
                from,
                brief,
                kind,
            } => self.task_create(caller, name, from, brief, kind),
            Call::TaskList => self.task_list(caller),
            Call::TaskWait {
                ids,
                until,
                timeout_secs,
            } => {
                let _slot = self.wait_slot()?;

                self.task_wait(caller, &ids, until, timeout_secs, gone)
            }
            // `handle` answers it before it gets here.
            Call::AgentExit => Ok(Value::Null),

            Call::TerminalSpawn {
                command,
                cwd,
                size,
                name,
            } => self.terminal_spawn(caller, command, cwd, size, name),
            Call::TerminalResize { id, size } => {
                reply_with(self.services.processes.resize(caller, id, size)?)
            }
            Call::TerminalRead { id, lines } => {
                reply_with(self.services.processes.read(caller, id, lines)?)
            }
            Call::TerminalWait {
                id,
                until,
                timeout_secs,
            } => {
                let _slot = self.wait_slot()?;

                self.terminal_wait(caller, id, &until, timeout_secs, gone)
            }
            Call::TerminalKill { id, signal } => reply_with(self.services.processes.kill(
                caller,
                id,
                signal.as_deref(),
            )?),
            Call::TerminalList => reply_with(self.services.processes.list(caller)),
        }
    }

    fn started_orchestrator(caller: &AgentInfo, action: &str) -> Result<(), Failure> {
        if caller.role == AgentRole::StartOrchestrator {
            return Ok(());
        }

        refuse(format!("only the Start orchestrator can {action}"))
    }

    /// The caller's task, when the caller is a task's orchestrator.
    fn orchestrated_task(&self, caller: &AgentInfo, action: &str) -> Result<TaskInfo, Failure> {
        let (AgentRole::Orchestrator, Some(task)) = (caller.role, caller.task) else {
            return refuse(format!("only a task's orchestrator can {action}"));
        };

        lock(&self.services.store)
            .task(task)?
            .ok_or_else(|| Failure("the task is gone".to_owned()))
    }

    /// A worker of the caller's task, by its agent ID.
    fn worker_of(&self, caller: &AgentInfo, worker: AgentId) -> Result<AgentInfo, Failure> {
        let stored = lock(&self.services.store).agent(worker)?;

        match stored {
            Some(stored)
                if stored.info.role == AgentRole::Worker && stored.info.task == caller.task =>
            {
                Ok(stored.info)
            }
            _ => refuse(format!("agent {} is not a worker of your task", worker.0)),
        }
    }

    fn post(
        &self,
        from: &AgentInfo,
        to: &AgentInfo,
        kind: MessageKind,
        text: &str,
        delivered: bool,
    ) -> Result<Sent, Failure> {
        let message = self.services.messages.post(&NewMessage {
            project: from.project,
            task: from.task,
            from: Some(from.id),
            to: to.id,
            kind,
            text,
            delivered,
        })?;

        if !delivered {
            self.services.agents.deliver_unread(to.id);
        }

        Ok(Sent {
            message: message.id,
        })
    }

    fn spawn(
        &self,
        caller: &AgentInfo,
        name: &str,
        kind: Option<AgentKind>,
        model: Option<String>,
        brief: String,
    ) -> Outcome {
        let task = self.orchestrated_task(caller, "spawn workers")?;

        if brief.trim().is_empty() {
            return refuse("a worker needs a brief");
        }
        let model = model.map(|model| model.trim().to_owned());
        if model
            .as_deref()
            .is_some_and(|model| model.is_empty() || model.starts_with('-'))
        {
            return refuse("the model is not valid");
        }

        let kind = kind.unwrap_or(caller.kind);
        let plan = self
            .services
            .agents
            .plan(kind, AgentRole::Worker)?
            .with_model(model);

        let agents = &self.services.agents;
        let prepared = self
            .services
            .projects
            .create_worker(&task, name, |checkout| {
                agents
                    .register(
                        &Place {
                            project: checkout.project,
                            task: Some(checkout.task),
                            name: &checkout.name,
                            worktree: &checkout.worktree,
                            branch: &checkout.branch,
                            ports: &checkout.ports,
                        },
                        plan,
                    )
                    .map_err(|error| swarm_core::Error::Invalid(format!("{error:#}")))
            })?;
        let worker = prepared.agent.clone();

        if let Err(failure) = self.post(caller, &worker, MessageKind::Brief, &brief, true) {
            agents.forget(worker.id);
            self.services.projects.discard_worker(&worker);

            return Err(failure);
        }
        agents.announce(worker.id);
        tasks::start_worker(&self.services, prepared, &task, brief);

        let worker = agents.info(worker.id).unwrap_or(worker);

        reply_with(summary(&worker))
    }

    fn send(&self, caller: &AgentInfo, to: AgentId, text: &str) -> Outcome {
        match caller.role {
            AgentRole::Orchestrator => {}
            AgentRole::Worker => {
                return refuse("workers can only message their orchestrator (use ask or report)");
            }
            AgentRole::StartOrchestrator => {
                return refuse("only a task's orchestrator can send messages to workers");
            }
        }
        if text.trim().is_empty() {
            return refuse("the message is empty");
        }

        let worker = self.worker_of(caller, to)?;

        reply_with(self.post(caller, &worker, MessageKind::Task, text, false)?)
    }

    fn to_orchestrator(&self, caller: &AgentInfo, kind: MessageKind, text: &str) -> Outcome {
        let Some(task) = caller.task.filter(|_| caller.role == AgentRole::Worker) else {
            return refuse("only workers can ask or report; an orchestrator messages its workers");
        };
        if text.trim().is_empty() {
            return refuse("the message is empty");
        }

        let orchestrator = lock(&self.services.store)
            .list_agents(task)?
            .into_iter()
            .find(|agent| agent.role == AgentRole::Orchestrator)
            .ok_or_else(|| Failure("the task has no orchestrator".to_owned()))?;

        reply_with(self.post(caller, &orchestrator, kind, text, false)?)
    }

    fn wait(
        &self,
        caller: &AgentInfo,
        timeout_secs: Option<u32>,
        gone: Gone<'_>,
        claim: &mut Option<Claim>,
    ) -> Outcome {
        let seconds = timeout_secs.unwrap_or(DEFAULT_WAIT_SECS).min(MAX_WAIT_SECS);
        let taken = self.services.messages.wait(
            caller.id,
            Duration::from_secs(u64::from(seconds)),
            gone,
        )?;

        inbox(taken, claim)
    }

    fn merge(&self, caller: &AgentInfo, worker: AgentId) -> Outcome {
        let task = self.orchestrated_task(caller, "merge a worker")?;
        let worker = self.worker_of(caller, worker)?;

        let outcome = git::merge_branch(&task.worktree, &worker.branch)?;

        reply_with(Merged {
            merged: outcome.merged,
            conflicts: outcome.conflicts,
            output: outcome.output,
        })
    }

    fn notify(&self, caller: &AgentInfo, text: &str) -> Outcome {
        self.orchestrated_task(caller, "notify the user")?;
        if text.trim().is_empty() {
            return refuse("the notice is empty");
        }

        self.services.agents.notify(caller.id, text.trim());

        Ok(Value::Null)
    }

    fn agents(&self, caller: &AgentInfo) -> Outcome {
        let task = self.orchestrated_task(caller, "list its workers")?;

        let workers: Vec<AgentSummary> = task
            .agents
            .iter()
            .filter(|agent| agent.role == AgentRole::Worker)
            .map(summary)
            .collect();

        reply_with(workers)
    }

    fn task_create(
        &self,
        caller: &AgentInfo,
        name: String,
        from: Option<String>,
        brief: String,
        kind: Option<AgentKind>,
    ) -> Outcome {
        Self::started_orchestrator(caller, "create tasks")?;

        if name.trim().is_empty() {
            return refuse("a task needs a name");
        }
        if brief.trim().is_empty() {
            return refuse("a task needs a brief");
        }

        let kind = match kind {
            Some(kind) => kind,
            None => self.services.agents.default_kind()?,
        };
        let request = CreateTask {
            project: caller.project,
            name,
            from,
            run_setup: true,
            orchestrator: Some(kind),
            first_message: Some(brief.clone()),
        };

        let services = &self.services;
        let mut created = None;
        tasks::create(services, &request, Some(caller.id), |task| {
            if let Some(orchestrator) = task
                .agents
                .iter()
                .find(|agent| agent.role == AgentRole::Orchestrator)
                && let Err(error) = services.messages.post(&NewMessage {
                    project: task.project,
                    task: Some(task.id),
                    from: Some(caller.id),
                    to: orchestrator.id,
                    kind: MessageKind::Brief,
                    text: &brief,
                    delivered: true,
                })
            {
                log::error!("cannot store the brief of task {}: {error:#}", task.id.0);
            }

            services
                .clients
                .broadcast(&Event::TaskChanged { task: task.clone() });
            created = Some(task.clone());
        })?;
        let task = created.ok_or_else(|| Failure("the task was not made".to_owned()))?;

        reply_with(self.task_summary(&task))
    }

    fn task_list(&self, caller: &AgentInfo) -> Outcome {
        Self::started_orchestrator(caller, "list the tasks")?;

        let tasks = lock(&self.services.store).list_tasks(caller.project)?;
        let summaries: Vec<TaskSummary> =
            tasks.iter().map(|task| self.task_summary(task)).collect();

        reply_with(summaries)
    }

    fn task_wait(
        &self,
        caller: &AgentInfo,
        ids: &[TaskId],
        until: TaskUntil,
        timeout_secs: Option<u32>,
        gone: Gone<'_>,
    ) -> Outcome {
        Self::started_orchestrator(caller, "wait for tasks")?;

        if ids.is_empty() {
            return refuse("name at least one task");
        }
        let seconds = timeout_secs.unwrap_or(DEFAULT_WAIT_SECS).min(MAX_WAIT_SECS);
        let deadline = Instant::now() + Duration::from_secs(u64::from(seconds));

        loop {
            let mut tasks = Vec::new();
            for id in ids {
                let task = lock(&self.services.store)
                    .task(*id)?
                    .filter(|task| task.project == caller.project)
                    .ok_or_else(|| Failure(format!("task {} is not in your project", id.0)))?;
                tasks.push(task);
            }

            let reached = tasks.iter().all(|task| match until {
                TaskUntil::Briefed => task.briefed,
                TaskUntil::Done => task_is_done(task),
            });
            if reached || Instant::now() >= deadline || gone() {
                let summaries: Vec<TaskSummary> =
                    tasks.iter().map(|task| self.task_summary(task)).collect();

                return reply_with(summaries);
            }

            std::thread::park_timeout(TASK_POLL);
        }
    }

    /// The task as agents see it, with its orchestrator as it is now.
    fn task_summary(&self, task: &TaskInfo) -> TaskSummary {
        let orchestrator = task
            .agents
            .iter()
            .find(|agent| agent.role == AgentRole::Orchestrator)
            .map(|agent| {
                self.services
                    .agents
                    .info(agent.id)
                    .unwrap_or_else(|| agent.clone())
            });

        TaskSummary {
            id: task.id,
            name: task.name.clone(),
            branch: task.branch.clone(),
            briefed: task.briefed,
            orchestrator: orchestrator.as_ref().map(summary),
        }
    }

    fn terminal_spawn(
        &self,
        caller: &AgentInfo,
        command: Vec<String>,
        cwd: Option<PathBuf>,
        size: Option<TerminalSize>,
        name: Option<String>,
    ) -> Outcome {
        let env = self.services.projects.agent_env(caller)?;

        let summary = self.services.processes.spawn(
            caller,
            Spawn {
                command,
                cwd,
                size,
                name,
                env,
            },
        )?;

        reply_with(summary)
    }

    fn terminal_wait(
        &self,
        caller: &AgentInfo,
        id: TerminalId,
        until: &TerminalUntil,
        timeout_secs: Option<u32>,
        gone: Gone<'_>,
    ) -> Outcome {
        let seconds = timeout_secs.unwrap_or(DEFAULT_WAIT_SECS).min(MAX_WAIT_SECS);

        reply_with(self.services.processes.wait(
            caller,
            id,
            until,
            Duration::from_secs(u64::from(seconds)),
            gone,
        )?)
    }

    fn confirm(&self, caller: &AgentInfo) -> Outcome {
        let task = self.orchestrated_task(caller, "confirm a brief")?;

        let task = {
            let store = lock(&self.services.store);
            store.confirm_task(task.id)?;

            store
                .task(task.id)?
                .ok_or_else(|| Failure("the task is gone".to_owned()))?
        };
        self.services
            .clients
            .broadcast(&Event::TaskChanged { task });

        Ok(Value::Null)
    }
}

/// The task's orchestrator stopped. Its `Done` status only marks the end of a turn.
fn task_is_done(task: &TaskInfo) -> bool {
    task.agents
        .iter()
        .find(|agent| agent.role == AgentRole::Orchestrator)
        .is_some_and(|orchestrator| {
            matches!(
                orchestrator.lifecycle,
                AgentLifecycle::Stopped | AgentLifecycle::Resumable
            )
        })
}

/// The reply for a claim of messages; the claim waits for the write.
fn inbox(taken: Claim, claim: &mut Option<Claim>) -> Outcome {
    let value = reply_with(taken.messages())?;
    *claim = Some(taken);

    Ok(value)
}

/// The agent as other agents see it.
pub(crate) fn summary(info: &AgentInfo) -> AgentSummary {
    AgentSummary {
        id: info.id,
        name: info.name.clone(),
        role: info.role,
        kind: info.kind,
        model: info.model.clone(),
        status: status_word(info.status).to_owned(),
        message: info.message.clone(),
        lifecycle: info.lifecycle,
        branch: info.branch.clone(),
        worktree: info.worktree.clone(),
    }
}

fn status_word(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Unknown => "unknown",
        AgentStatus::Idle => "idle",
        AgentStatus::Working => "working",
        AgentStatus::Done => "done",
        AgentStatus::Blocked(_) => "blocked",
        AgentStatus::Error => "error",
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn task_with_orchestrator(status: AgentStatus, lifecycle: AgentLifecycle) -> TaskInfo {
        let orchestrator = AgentInfo {
            id: AgentId(1),
            project: swarm_protocol::ProjectId(1),
            task: Some(TaskId(1)),
            role: AgentRole::Orchestrator,
            kind: AgentKind::ClaudeCode,
            model: "opus".to_owned(),
            name: "orchestrator".to_owned(),
            worktree: PathBuf::from("/repo/.worktrees/task"),
            branch: "swarm/task".to_owned(),
            terminal: None,
            status,
            message: None,
            lifecycle,
            revision: 1,
            created_at: 0,
            ended_at: None,
        };

        TaskInfo {
            id: TaskId(1),
            project: swarm_protocol::ProjectId(1),
            name: "Task".to_owned(),
            branch: "swarm/task".to_owned(),
            base: "main".to_owned(),
            worktree: PathBuf::from("/repo/.worktrees/task"),
            ports: Vec::new(),
            setup_terminal: None,
            setup_exit: None,
            agents: vec![orchestrator],
            created_by: None,
            briefed: true,
        }
    }

    #[test]
    fn the_end_of_a_turn_is_not_done() {
        let task = task_with_orchestrator(AgentStatus::Done, AgentLifecycle::Running);

        assert!(!task_is_done(&task));
    }

    #[test]
    fn a_task_is_done_when_its_orchestrator_stopped() {
        for lifecycle in [AgentLifecycle::Stopped, AgentLifecycle::Resumable] {
            let task = task_with_orchestrator(AgentStatus::Idle, lifecycle);

            assert!(task_is_done(&task), "{lifecycle:?}");
        }
    }
}
