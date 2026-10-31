//! Projects and tasks: git, config, ports and the database rows behind them.
//!
//! No GPUI, no terminal emulator and no socket code: the daemon calls this crate and
//! decides how to start the processes (ADR 0005).

pub mod adapters;
pub mod agents;
pub mod config;
pub mod git;
pub mod prompts;
pub mod store;
pub mod task;

use std::collections::{HashMap, HashSet};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use swarm_protocol::{
    AgentChange, AgentId, AgentInfo, AgentLifecycle, AgentRole, CloseCheck, CreateTask, DIFF_FILES,
    DiffStat, FileChange, ProjectId, ProjectInfo, ReviewSource, TaskDiff, TaskId, TaskInfo,
    TerminalId,
};
use swarm_term::ExitStatus;

use crate::config::{LoadedConfig, ProjectConfig};
use crate::store::{NewTask, Store, StoredProject};
use crate::task::BRANCH_PREFIX;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("git was not found on this computer")]
    GitMissing,
    #[error("{}", git_message(.command, .stderr))]
    Git { command: String, stderr: String },
    #[error("cannot {action}: {source}")]
    Io {
        action: String,
        source: std::io::Error,
    },
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("cannot encode a database value: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("no project with id {0}")]
    UnknownProject(u64),
    #[error("no free ports left in the configured range")]
    NoFreePorts,
}

fn git_message(command: &str, stderr: &str) -> String {
    if stderr.is_empty() {
        format!("git {command} failed")
    } else {
        format!("git {command} failed: {stderr}")
    }
}

/// What the daemon needs to run a task's setup script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedTask {
    pub task: TaskInfo,
    /// The script text, when the caller asked for it and the config has one.
    pub setup: Option<String>,
    /// `SWARM_TASK`, `SWARM_PORT` and `SWARM_PORTS`.
    pub env: Vec<(String, String)>,
}

/// Where a worker works, as [`Projects::create_worker`] made it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerCheckout {
    pub project: ProjectId,
    pub task: TaskId,
    /// The unique worker slug.
    pub name: String,
    pub branch: String,
    pub worktree: PathBuf,
    pub ports: Vec<u16>,
}

/// What the daemon needs to run a worker's setup script and start it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedWorker {
    pub agent: AgentInfo,
    /// The project's setup script, when it has one.
    pub setup: Option<String>,
    /// `SWARM_TASK`, `SWARM_PORT` and `SWARM_PORTS`.
    pub env: Vec<(String, String)>,
}

/// How long the teardown script may run before it is killed.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(60);

/// What closing a task changed, for the daemon to tell its clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosedTask {
    pub task: TaskInfo,
    /// The agents the close ended that were not `Stopped` yet.
    pub stopped: Vec<AgentInfo>,
}

/// Projects and their tasks. Share one value between all clients.
pub struct Projects {
    store: Arc<Mutex<Store>>,
    /// One task is made at a time, so two clients never pick the same name or ports.
    creating: Mutex<()>,
}

impl Projects {
    pub fn new(store: Arc<Mutex<Store>>) -> Self {
        Self {
            store,
            creating: Mutex::new(()),
        }
    }

    pub fn list(&self) -> Result<Vec<ProjectInfo>, Error> {
        let stored = lock(&self.store).list_projects()?;

        stored
            .into_iter()
            .map(|project| self.info(project))
            .collect()
    }

    /// `path` may be any folder inside a repository.
    pub fn add(&self, path: &Path) -> Result<ProjectInfo, Error> {
        let root = git::repository_root(path)?;
        let name = root.file_name().map_or_else(
            || "project".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );

        let project = lock(&self.store).add_project(&root, &name)?;

        self.info(project)
    }

    pub fn set_setup_terminal(
        &self,
        task: TaskId,
        terminal: Option<TerminalId>,
    ) -> Result<(), Error> {
        lock(&self.store).set_setup_terminal(task, terminal)
    }

    /// Records how the setup script ended and clears its terminal.
    pub fn finish_setup(&self, task: TaskId, exit: Option<&ExitStatus>) -> Result<(), Error> {
        lock(&self.store).finish_setup(task, exit)
    }

    /// Undoes [`Projects::create_task`] for a task that never started: removes its
    /// worktree, branch and row, which frees its ports.
    pub fn discard_task(&self, task: &TaskInfo) {
        let _creating = lock(&self.creating);

        if let Some(root) = self.project_root(task.project) {
            remove_task_leftovers(&root, &task.branch, &task.worktree);
        }
        if let Err(error) = lock(&self.store).delete_task(task.id) {
            log::error!("cannot delete task {}: {error}", task.id.0);
        }
    }

    /// Undoes [`Projects::create_worker`] for a worker that never started: removes its
    /// worktree, branch and row, which frees its ports.
    /// A worker in the task worktree only loses its row.
    pub fn discard_worker(&self, worker: &AgentInfo) {
        let _creating = lock(&self.creating);

        if !self.shares_task_worktree(worker)
            && let Some(root) = self.project_root(worker.project)
        {
            remove_task_leftovers(&root, &worker.branch, &worker.worktree);
        }
        if let Err(error) = lock(&self.store).delete_agent(worker.id) {
            log::error!("cannot delete agent {}: {error}", worker.id.0);
        }
    }

    /// True for a worker that works in its task's worktree.
    fn shares_task_worktree(&self, worker: &AgentInfo) -> bool {
        let Some(task) = worker.task else {
            return false;
        };

        match lock(&self.store).task(task) {
            Ok(task) => task.is_some_and(|task| task.worktree == worker.worktree),
            Err(error) => {
                log::error!("cannot find task {}: {error}", task.0);

                true
            }
        }
    }

    fn project_root(&self, project: ProjectId) -> Option<PathBuf> {
        match lock(&self.store).project(project) {
            Ok(project) => project.map(|project| project.path),
            Err(error) => {
                log::error!("cannot find project {}: {error}", project.0);

                None
            }
        }
    }

    /// The ports a worker or orchestrator reserved, as `SWARM_PORT`.
    pub fn agent_env(&self, agent: &AgentInfo) -> Result<Vec<(String, String)>, Error> {
        let store = lock(&self.store);

        let ports = match agent.task {
            Some(task) if agent.role == AgentRole::Orchestrator => {
                store.task(task)?.map(|task| task.ports).unwrap_or_default()
            }
            _ => store.agent_ports(agent.id)?,
        };

        Ok(match agent.task {
            Some(task) => ports_env(task, &ports),
            None => Vec::new(),
        })
    }

    /// The project's running Start orchestrator, or the one `register` makes in the
    /// project root on the base branch. The flag is true for a new one. Only one runs
    /// per project, even when two callers ask at once.
    pub fn start_agent_or(
        &self,
        project: ProjectId,
        register: impl FnOnce(&Path, &str) -> Result<AgentInfo, Error>,
    ) -> Result<(AgentInfo, bool), Error> {
        let _creating = lock(&self.creating);

        let stored = lock(&self.store)
            .project(project)?
            .ok_or(Error::UnknownProject(project.0))?;
        if let Some(agent) = lock(&self.store).start_agent(project)? {
            return Ok((agent, false));
        }

        let LoadedConfig { config, .. } = config::load(&stored.path);
        let base = git::default_base_branch(&stored.path, config.worktrees.base_branch.as_deref());

        Ok((register(&stored.path, &base)?, true))
    }

    /// Makes the task branch and task worktree, copies files, reserves ports and
    /// records the task. Nothing is left behind when a step fails.
    pub fn create_task(&self, request: &CreateTask) -> Result<PreparedTask, Error> {
        self.create_task_by(request, None)
    }

    /// Like [`Projects::create_task`]; the row records `created_by`, the Start
    /// orchestrator that asked for the task.
    pub fn create_task_by(
        &self,
        request: &CreateTask,
        created_by: Option<AgentId>,
    ) -> Result<PreparedTask, Error> {
        let _creating = lock(&self.creating);

        let project = lock(&self.store)
            .project(request.project)?
            .ok_or(Error::UnknownProject(request.project.0))?;
        let root = project.path;

        let LoadedConfig { config, error } = config::load(&root);
        if let Some(error) = error {
            return Err(Error::Invalid(format!(
                "the project config has a problem: {error}"
            )));
        }

        let name = request.name.trim();
        let slug = task::slug(name)?;
        let base = match &request.from {
            Some(from) => from.clone(),
            None => git::default_base_branch(&root, config.worktrees.base_branch.as_deref()),
        };
        git::verify_start_point(&root, &base)?;

        let folder = root.join(&config.worktrees.folder);
        task::create_folder_inside(&root, &folder)?;

        let slug = task::first_free_slug(&slug, |candidate| {
            let worktree = folder.join(candidate);

            Ok(worktree.symlink_metadata().is_ok()
                || git::branch_exists(&root, &format!("{BRANCH_PREFIX}{candidate}"))?)
        })?;
        let branch = format!("{BRANCH_PREFIX}{slug}");
        let worktree = folder.join(&slug);

        let ports = self.reserve_ports(&config)?;

        let folder_line = format!("/{}/", config.worktrees.folder.trim_matches('/'));
        git::add_exclude_line(&root, &folder_line)?;
        git::worktree_add(&root, &branch, &worktree, &base)?;

        let recorded = self.record(
            &root, &config, request, name, &branch, &base, &worktree, &ports, created_by,
        );
        let task = match recorded {
            Ok(task) => task,
            Err(error) => {
                remove_task_leftovers(&root, &branch, &worktree);

                return Err(error);
            }
        };

        let setup = (request.run_setup && !config.worktrees.setup.trim().is_empty())
            .then(|| config.worktrees.setup.clone());
        let env = task_env(&task);

        Ok(PreparedTask { task, setup, env })
    }

    /// Adds a worker that works in the task worktree on the task branch. Nothing is
    /// made on disk and no setup script runs: the task worktree ran it already.
    /// `register` records the worker's agent row.
    pub fn create_shared_worker(
        &self,
        task: &TaskInfo,
        name: &str,
        register: impl FnOnce(&WorkerCheckout) -> Result<AgentInfo, Error>,
    ) -> Result<PreparedWorker, Error> {
        let _creating = lock(&self.creating);

        let worker_slug = self.free_worker_slug(task, name)?;
        let base_commit = git::commit_of(&task.worktree, &task.branch)?;

        let checkout = WorkerCheckout {
            project: task.project,
            task: task.id,
            name: worker_slug,
            branch: task.branch.clone(),
            worktree: task.worktree.clone(),
            ports: task.ports.clone(),
        };
        let agent = register(&checkout)?;

        if let Err(error) = lock(&self.store).set_base_commit(agent.id, &base_commit) {
            log::warn!("cannot record where worker {} started: {error}", agent.id.0);
        }

        let env = ports_env(task.id, &checkout.ports);

        Ok(PreparedWorker {
            agent,
            setup: None,
            env,
        })
    }

    /// The slug of `name`, when no worker of the task has it yet.
    fn free_worker_slug(&self, task: &TaskInfo, name: &str) -> Result<String, Error> {
        let worker_slug = task::slug(name).map_err(|_| {
            Error::Invalid(format!("the worker name {name:?} has no letters or digits"))
        })?;
        let taken = lock(&self.store)
            .list_agents(task.id)?
            .iter()
            .any(|agent| agent.role == AgentRole::Worker && agent.name == worker_slug);
        if taken {
            return Err(Error::Invalid(format!(
                "this task already has a worker named {worker_slug}"
            )));
        }

        Ok(worker_slug)
    }

    /// Makes a worker's branch (from the task branch) and worktree, copies files and
    /// reserves ports. `register` records the worker's agent row while ports are still
    /// held; nothing is left behind when a step or `register` fails.
    pub fn create_worker(
        &self,
        task: &TaskInfo,
        name: &str,
        register: impl FnOnce(&WorkerCheckout) -> Result<AgentInfo, Error>,
    ) -> Result<PreparedWorker, Error> {
        let _creating = lock(&self.creating);

        let project = lock(&self.store)
            .project(task.project)?
            .ok_or(Error::UnknownProject(task.project.0))?;
        let root = project.path;

        let LoadedConfig { config, error } = config::load(&root);
        if let Some(error) = error {
            return Err(Error::Invalid(format!(
                "the project config has a problem: {error}"
            )));
        }

        let worker_slug = self.free_worker_slug(task, name)?;

        let task_slug = task
            .worktree
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .ok_or_else(|| Error::Invalid("the task worktree has no name".to_owned()))?;
        let folder = root.join(&config.worktrees.folder);
        task::create_folder_inside(&root, &folder)?;

        let branch = format!("{}--{worker_slug}", task.branch);
        let worktree = folder.join(format!("{task_slug}--{worker_slug}"));
        if worktree.symlink_metadata().is_ok() || git::branch_exists(&root, &branch)? {
            return Err(Error::Invalid(format!(
                "the branch or folder for worker {worker_slug} exists already"
            )));
        }
        git::verify_start_point(&root, &task.branch)?;
        let base_commit = git::commit_of(&root, &task.branch)?;

        let ports = self.reserve_ports(&config)?;

        git::worktree_add(&root, &branch, &worktree, &base_commit)?;

        let checkout = WorkerCheckout {
            project: task.project,
            task: task.id,
            name: worker_slug,
            branch,
            worktree,
            ports,
        };
        let recorded = task::copy_files(&root, &checkout.worktree, &config.worktrees.copy)
            .and_then(|()| register(&checkout));
        let agent = match recorded {
            Ok(agent) => agent,
            Err(error) => {
                remove_task_leftovers(&root, &checkout.branch, &checkout.worktree);

                return Err(error);
            }
        };

        if let Err(error) = lock(&self.store).set_base_commit(agent.id, &base_commit) {
            log::warn!(
                "cannot record where worker {} branched: {error}",
                agent.id.0
            );
        }

        let setup =
            (!config.worktrees.setup.trim().is_empty()).then(|| config.worktrees.setup.clone());
        let env = ports_env(task.id, &checkout.ports);

        Ok(PreparedWorker { agent, setup, env })
    }

    /// What each agent changed in its worktree since it branched off. An agent whose
    /// changes cannot be read is left out: a Start orchestrator, an unknown agent, a
    /// missing worktree or a git failure.
    pub fn agent_changes(&self, agents: &[AgentId]) -> Vec<AgentChange> {
        agents
            .iter()
            .filter_map(|id| match self.agent_change(*id) {
                Ok(change) => change,
                Err(error) => {
                    log::debug!("cannot read the changes of agent {}: {error}", id.0);

                    None
                }
            })
            .collect()
    }

    fn agent_change(&self, id: AgentId) -> Result<Option<AgentChange>, Error> {
        // The store lock is gone before git runs.
        let (agent, worker_base, task_base, task_branch, task_worktree) = {
            let store = lock(&self.store);
            let Some(agent) = store.agent(id)?.map(|stored| stored.info) else {
                return Ok(None);
            };
            let worker_base = store.base_commit(id)?;
            let task = match agent.task {
                Some(task) => store.task(task)?,
                None => None,
            };
            let task_base = task.as_ref().map(|task| task.base.clone());
            let task_worktree = task.as_ref().map(|task| task.worktree.clone());
            let task_branch = task.map(|task| task.branch);

            (agent, worker_base, task_base, task_branch, task_worktree)
        };

        if !agent.worktree.is_dir() {
            return Ok(None);
        }

        let shared = agent.role == AgentRole::Worker
            && task_worktree.as_deref() == Some(agent.worktree.as_path());
        if shared {
            // The worktree holds other agents' work too, so only its commits count.
            let Some(base) = worker_base else {
                return Ok(None);
            };

            let (stat, files) = git::trailer_stat(&agent.worktree, &base, &agent.name)?;

            return Ok(Some(AgentChange {
                agent: id,
                stat,
                files,
            }));
        }

        let base = match agent.role {
            AgentRole::Worker => match (worker_base, task_branch) {
                (Some(base), _) => Some(base),
                // Made before the base commit was recorded.
                (None, Some(branch)) => Some(git::merge_base(&agent.worktree, &branch)?),
                (None, None) => None,
            },
            AgentRole::Orchestrator => match task_base {
                Some(base) => Some(git::merge_base(&agent.worktree, &base)?),
                None => None,
            },
            AgentRole::StartOrchestrator => None,
        };
        let Some(base) = base else {
            return Ok(None);
        };

        let (stat, files) = git::diff_files(&agent.worktree, &base)?;

        Ok(Some(AgentChange {
            agent: id,
            stat,
            files,
        }))
    }

    /// An open task. Unknown and closed tasks are errors.
    pub fn task(&self, id: TaskId) -> Result<TaskInfo, Error> {
        lock(&self.store)
            .task(id)?
            .ok_or_else(|| Error::Invalid(format!("no open task with id {}", id.0)))
    }

    /// The changes of a task. `Merged` is the commits of the task branch since it left
    /// its base. `Live` is the task worktree as it is now, and adds each worker with its
    /// own worktree that is not merged yet.
    pub fn task_diff(&self, id: TaskId, source: ReviewSource) -> Result<TaskDiff, Error> {
        // The store lock is gone before git runs.
        let (task, agents) = {
            let store = lock(&self.store);
            let task = store
                .task(id)?
                .ok_or_else(|| Error::Invalid(format!("no open task with id {}", id.0)))?;
            let agents = store.list_agents(id)?;

            (task, agents)
        };

        if !task.worktree.is_dir() {
            return Err(Error::Invalid(format!(
                "the task worktree {} is missing",
                task.worktree.display()
            )));
        }

        let base = git::merge_base(&task.worktree, &task.base)?;
        let mut sections = vec![Section {
            worktree: task.worktree.clone(),
            base,
            owner: None,
        }];

        if source == ReviewSource::Live {
            for worker in agents
                .iter()
                .filter(|agent| agent.role == AgentRole::Worker)
                .filter(|worker| worker.worktree != task.worktree && worker.worktree.is_dir())
            {
                match unmerged_section(&task, worker) {
                    Ok(section) => sections.extend(section),
                    Err(error) => {
                        log::warn!("cannot review worker {}: {error}", worker.name);
                    }
                }
            }
        }
        sections.sort_by(|one, other| {
            one.worktree
                .as_os_str()
                .as_bytes()
                .cmp(other.worktree.as_os_str().as_bytes())
        });

        let scope = match source {
            ReviewSource::Live => git::DiffScope::WorkingTree,
            ReviewSource::Merged => git::DiffScope::Committed,
        };
        let names = unique_names(&agents);
        let mut budget = git::DiffBudget::default();
        let mut fingerprint = git::Fingerprint::default();
        fingerprint.write(&[match source {
            ReviewSource::Live => 1,
            ReviewSource::Merged => 2,
        }]);

        let (mut stat, mut files) = (DiffStat::default(), Vec::new());
        for section in &sections {
            let changed =
                match git::worktree_diff(&section.worktree, &section.base, scope, &mut budget) {
                    Ok(changed) => changed,
                    Err(error) if section.owner.is_some() => {
                        log::warn!("cannot read {}: {error}", section.worktree.display());

                        continue;
                    }
                    Err(error) => return Err(error),
                };
            let (trailers, uncommitted) = match section.owner {
                Some(_) => (HashMap::new(), HashSet::new()),
                None => (
                    git::attribution(&section.worktree, &section.base)?,
                    match source {
                        ReviewSource::Live => git::uncommitted_paths(&section.worktree)?,
                        ReviewSource::Merged => HashSet::new(),
                    },
                ),
            };

            for mut file in changed {
                fingerprint.write_text(section.worktree.as_os_str().as_bytes());
                fingerprint.write_text(&file.raw_path);
                fingerprint.write(&[match file.diff.change {
                    FileChange::Added => 1,
                    FileChange::Modified => 2,
                    FileChange::Deleted => 3,
                }]);
                fingerprint.write_u64(file.digest);

                stat.files = stat.files.saturating_add(1);
                stat.additions = stat.additions.saturating_add(file.diff.additions);
                stat.deletions = stat.deletions.saturating_add(file.diff.deletions);

                // Uncommitted work has no trailer, so it belongs to nobody.
                file.diff.agent = section.owner.or_else(|| {
                    if uncommitted.contains(&file.raw_path) {
                        return None;
                    }

                    trailers
                        .get(&file.raw_path)
                        .and_then(|name| names.get(name.as_str()).copied().flatten())
                });
                if files.len() < DIFF_FILES {
                    files.push(file.diff);
                }
            }
        }

        Ok(TaskDiff {
            task: id,
            source,
            fingerprint: fingerprint.finish(),
            stat,
            files,
        })
    }

    /// Stores the approval when the Merged diff still has this fingerprint.
    pub fn approve(&self, id: TaskId, fingerprint: u64) -> Result<TaskInfo, Error> {
        let current = self.task_diff(id, ReviewSource::Merged)?;
        if current.fingerprint != fingerprint {
            return Err(Error::Invalid(
                "the changes changed since you reviewed them".to_owned(),
            ));
        }

        lock(&self.store).set_approved(id, fingerprint)?;

        self.task(id)
    }

    /// What closing the task would throw away.
    pub fn close_check(&self, id: TaskId) -> Result<CloseCheck, Error> {
        let task = self.task(id)?;

        let mut check = CloseCheck::default();
        let mut worktrees = Vec::new();
        if task.worktree.is_dir() {
            worktrees.push(task.worktree.clone());
        }

        for agent in &task.agents {
            if matches!(
                agent.lifecycle,
                AgentLifecycle::Starting | AgentLifecycle::Running
            ) {
                check.running_agents += 1;
            }

            let own = agent.role == AgentRole::Worker && agent.worktree != task.worktree;
            if !own {
                continue;
            }
            if agent.worktree.is_dir() && !worktrees.contains(&agent.worktree) {
                worktrees.push(agent.worktree.clone());
            }
            if task.worktree.is_dir() && !merged(&task, &agent.branch)? {
                check.unmerged_workers.push(agent.name.clone());
            }
        }

        for worktree in worktrees {
            check.uncommitted_files = check
                .uncommitted_files
                .saturating_add(git::uncommitted_files(&worktree)?);
        }

        if let Some(root) = self.project_root(task.project) {
            check.merged_branches = merged_branches(&root, &task)?;
        }

        Ok(check)
    }

    /// Runs the teardown script, removes the task worktree and the workers' own
    /// worktrees (uncommitted changes go with them), deletes the branches merged into
    /// the base branch and marks the task closed. The caller stopped the task's processes first. A failure leaves the
    /// task marked as closing, so a retry or the next daemon start finishes it.
    pub fn close_task(&self, id: TaskId) -> Result<ClosedTask, Error> {
        self.finish_close(id, true)
    }

    /// Finishes every close that did not end, for example after a crash. No teardown
    /// script runs, because the task's processes are gone.
    pub fn recover_closing(&self) {
        let tasks = match lock(&self.store).closing_tasks() {
            Ok(tasks) => tasks,
            Err(error) => {
                log::error!("cannot list the tasks that were closing: {error}");

                return;
            }
        };

        for task in tasks {
            match self.finish_close(task, false) {
                Ok(_) => log::info!("finished closing task {}", task.0),
                Err(error) => log::error!("cannot finish closing task {}: {error}", task.0),
            }
        }
    }

    fn finish_close(&self, id: TaskId, teardown: bool) -> Result<ClosedTask, Error> {
        let (task, root) = {
            let store = lock(&self.store);
            let task = store
                .task_including_closed(id)?
                .ok_or_else(|| Error::Invalid(format!("no task with id {}", id.0)))?;
            if task.closed {
                return Err(Error::Invalid(format!("task {} is closed already", id.0)));
            }
            let root = store
                .project(task.project)?
                .ok_or(Error::UnknownProject(task.project.0))?
                .path;

            (task, root)
        };

        lock(&self.store).begin_closing(id)?;

        let LoadedConfig { config, .. } = config::load(&root);
        if teardown
            && task.worktree.is_dir()
            && !config.worktrees.teardown.trim().is_empty()
            && let Err(error) = run_teardown(&config.worktrees.teardown, &task)
        {
            log::error!("the teardown of task {} failed: {error}", id.0);
        }

        let _creating = lock(&self.creating);

        let own = task
            .agents
            .iter()
            .filter(|agent| agent.role == AgentRole::Worker && agent.worktree != task.worktree)
            .map(|agent| agent.worktree.as_path());
        for worktree in own.chain([task.worktree.as_path()]) {
            if worktree.symlink_metadata().is_ok() {
                git::worktree_remove(&root, worktree)?;
            }
        }
        if let Err(error) = git::worktree_prune(&root) {
            log::warn!("cannot prune the worktrees of {}: {error}", root.display());
        }

        // Checked again now: a branch merged since the check loses nothing either.
        match merged_branches(&root, &task) {
            Ok(branches) => {
                for branch in branches {
                    if let Err(error) = git::branch_delete(&root, &branch) {
                        log::warn!("cannot delete the merged branch {branch}: {error}");
                    }
                }
            }
            Err(error) => log::warn!("cannot find the merged branches of {}: {error}", id.0),
        }

        let stopped = lock(&self.store).close_task(id)?;
        let task = lock(&self.store)
            .task_including_closed(id)?
            .ok_or_else(|| Error::Invalid(format!("no task with id {}", id.0)))?;

        Ok(ClosedTask { task, stopped })
    }

    /// Ports no task of any project uses, so two projects never share a port.
    fn reserve_ports(&self, config: &ProjectConfig) -> Result<Vec<u16>, Error> {
        let Some(range) = &config.worktrees.ports else {
            return Ok(Vec::new());
        };

        let used = lock(&self.store).all_task_ports()?;

        task::allocate_ports(range, &used)
    }

    /// The steps after the worktree exists; any failure here is rolled back.
    #[expect(
        clippy::too_many_arguments,
        reason = "each value is a different part of one task"
    )]
    fn record(
        &self,
        root: &Path,
        config: &ProjectConfig,
        request: &CreateTask,
        name: &str,
        branch: &str,
        base: &str,
        worktree: &Path,
        ports: &[u16],
        created_by: Option<AgentId>,
    ) -> Result<TaskInfo, Error> {
        task::copy_files(root, worktree, &config.worktrees.copy)?;

        lock(&self.store).add_task_by(
            &NewTask {
                project: request.project,
                name,
                branch,
                base,
                worktree,
                ports,
            },
            created_by,
        )
    }

    fn info(&self, project: StoredProject) -> Result<ProjectInfo, Error> {
        let LoadedConfig { config, error } = config::load(&project.path);
        let base_branch =
            git::default_base_branch(&project.path, config.worktrees.base_branch.as_deref());
        let tasks = lock(&self.store).list_tasks(project.id)?;

        Ok(ProjectInfo {
            id: project.id,
            name: project.name,
            path: project.path,
            base_branch,
            config_error: error,
            tasks,
            start: lock(&self.store).start_agent(project.id)?,
        })
    }
}

/// One worktree of a task diff and who owns its files.
struct Section {
    worktree: PathBuf,
    base: String,
    /// The worker that owns the worktree; `None` for the task worktree.
    owner: Option<AgentId>,
}

/// The section of a worker with its own worktree, unless the task branch has all of its
/// work already.
fn unmerged_section(task: &TaskInfo, worker: &AgentInfo) -> Result<Option<Section>, Error> {
    let worker_head = git::commit_of(&worker.worktree, "HEAD")?;
    let task_head = git::commit_of(&task.worktree, "HEAD")?;
    if git::is_ancestor(&task.worktree, &worker_head, &task_head)?
        && git::uncommitted_files(&worker.worktree)? == 0
    {
        return Ok(None);
    }

    // Not the recorded start commit: what the task branch took already must not show twice.
    let base = git::merge_base(&worker.worktree, &task_head)?;

    Ok(Some(Section {
        worktree: worker.worktree.clone(),
        base,
        owner: Some(worker.id),
    }))
}

/// The task branch has every commit of `branch`.
fn merged(task: &TaskInfo, branch: &str) -> Result<bool, Error> {
    git::is_ancestor(&task.worktree, branch, "HEAD")
}

/// Agent names that belong to one agent of the task. A name two agents share (a worker
/// called "orchestrator") names nobody, because a trailer cannot tell them apart.
fn unique_names(agents: &[AgentInfo]) -> HashMap<&str, Option<AgentId>> {
    let mut names: HashMap<&str, Option<AgentId>> = HashMap::new();

    for agent in agents {
        names
            .entry(agent.name.as_str())
            .and_modify(|owner| *owner = None)
            .or_insert(Some(agent.id));
    }

    names
}

/// Runs the script with `sh -c` in the task worktree, with the setup script's
/// environment. Kills it after [`TEARDOWN_TIMEOUT`].
/// The task branch and workers' own branches whose tip is in the base branch, so
/// deleting them loses no commit.
fn merged_branches(root: &Path, task: &TaskInfo) -> Result<Vec<String>, Error> {
    let base = format!("refs/heads/{}", task.base);
    if !git::branch_exists(root, &task.base)? {
        return Ok(Vec::new());
    }

    let own = task
        .agents
        .iter()
        .filter(|agent| agent.role == AgentRole::Worker && agent.worktree != task.worktree)
        .map(|agent| agent.branch.as_str());

    let mut merged = Vec::new();
    for branch in [task.branch.as_str()].into_iter().chain(own) {
        if merged.iter().any(|known| known == branch) || !git::branch_exists(root, branch)? {
            continue;
        }
        if git::is_ancestor(root, &format!("refs/heads/{branch}"), &base)? {
            merged.push(branch.to_owned());
        }
    }

    Ok(merged)
}

fn run_teardown(script: &str, task: &TaskInfo) -> Result<(), Error> {
    let io_error = |action: &str| {
        let action = action.to_owned();
        move |source| Error::Io { action, source }
    };

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(script)
        .current_dir(&task.worktree)
        .envs(task_env(task))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(io_error("start the teardown script"))?;

    let deadline = Instant::now() + TEARDOWN_TIMEOUT;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(io_error("wait for the teardown"))?
        {
            return if status.success() {
                Ok(())
            } else {
                Err(Error::Invalid(format!("the script ended with {status}")))
            };
        }

        if Instant::now() >= deadline {
            if let Ok(group) = libc::pid_t::try_from(child.id()) {
                // SAFETY: killpg takes plain integers and has no memory effects.
                unsafe {
                    libc::killpg(group, libc::SIGKILL);
                }
            }
            child.wait().map_err(io_error("wait for the teardown"))?;

            return Err(Error::Invalid(format!(
                "the script ran longer than {} seconds",
                TEARDOWN_TIMEOUT.as_secs()
            )));
        }

        std::thread::park_timeout(Duration::from_millis(50));
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn remove_task_leftovers(root: &Path, branch: &str, worktree: &Path) {
    if let Err(error) = git::worktree_remove(root, worktree) {
        log::error!("cannot remove the worktree {}: {error}", worktree.display());
    }
    if let Err(error) = git::branch_delete(root, branch) {
        log::error!("cannot delete the branch {branch}: {error}");
    }
}

fn task_env(task: &TaskInfo) -> Vec<(String, String)> {
    ports_env(task.id, &task.ports)
}

fn ports_env(task: TaskId, ports: &[u16]) -> Vec<(String, String)> {
    let mut env = vec![("SWARM_TASK".to_owned(), task.0.to_string())];

    if let Some(first) = ports.first() {
        let list: Vec<String> = ports.iter().map(u16::to_string).collect();

        env.push(("SWARM_PORT".to_owned(), first.to_string()));
        env.push(("SWARM_PORTS".to_owned(), list.join(",")));
    }

    env
}
