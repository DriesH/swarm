//! What the agent socket does with a call (ADR 0008): who may make it, and the work.
//!
//! The caller is found by its agent ID and token. Its role and its task, not anything
//! it sends, decide what it may do. Part of the calls are not built yet and answer
//! "not available yet".

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use swarm_core::git;
use swarm_core::store::NewMessage;
use swarm_protocol::agent::{AgentRequest, AgentSummary, Call, Merged, Reply, Sent};
use swarm_protocol::{
    AgentId, AgentInfo, AgentKind, AgentRole, AgentStatus, Event, MessageKind, TaskInfo,
};

use crate::Services;
use crate::agents::{Place, Refused, lock};
use crate::tasks;

/// `wait` without a timeout.
const DEFAULT_WAIT_SECS: u32 = 120;

/// The longest `wait`.
const MAX_WAIT_SECS: u32 = 600;

const NOT_YET: &str = "not available yet";

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

pub(crate) struct Calls {
    services: Arc<Services>,
}

impl Calls {
    pub(crate) fn new(services: Arc<Services>) -> Self {
        Self { services }
    }

    /// Checks the caller and runs the call. A caller that is not who it says gets
    /// `Err(Refused)` so the socket can log it.
    pub(crate) fn handle(&self, request: AgentRequest) -> Result<Reply, Refused> {
        let caller = self
            .services
            .agents
            .authenticate(request.agent, &request.token)?;

        let reply = match self.run(&caller, request.call) {
            Ok(value) => Reply::success(value),
            Err(Failure(message)) => Reply::failure(message),
        };

        Ok(reply)
    }

    fn run(&self, caller: &AgentInfo, call: Call) -> Outcome {
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
            Call::Read => reply_with(self.services.messages.take(caller.id)?),
            Call::Wait { timeout_secs } => self.wait(caller, timeout_secs),
            Call::Merge { worker } => self.merge(caller, worker),
            Call::Notify { text } => self.notify(caller, &text),
            Call::Agents => self.agents(caller),
            Call::Confirm => self.confirm(caller),

            Call::TaskCreate { .. }
            | Call::TaskList
            | Call::TaskWait { .. }
            | Call::AgentExit
            | Call::TerminalSpawn { .. }
            | Call::TerminalResize { .. }
            | Call::TerminalRead { .. }
            | Call::TerminalWait { .. }
            | Call::TerminalKill { .. }
            | Call::TerminalList => refuse(NOT_YET),
        }
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

        agents.announce(worker.id);
        self.post(caller, &worker, MessageKind::Brief, &brief, true)?;
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

    fn wait(&self, caller: &AgentInfo, timeout_secs: Option<u32>) -> Outcome {
        let seconds = timeout_secs.unwrap_or(DEFAULT_WAIT_SECS).min(MAX_WAIT_SECS);
        let messages = self
            .services
            .messages
            .wait(caller.id, Duration::from_secs(u64::from(seconds)))?;

        reply_with(messages)
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
