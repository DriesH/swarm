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

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use swarm_protocol::{CreateTask, ProjectInfo, TaskId, TaskInfo, TerminalId};
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

    /// Makes the task branch and task worktree, copies files, reserves ports and
    /// records the task. Nothing is left behind when a step fails.
    pub fn create_task(&self, request: &CreateTask) -> Result<PreparedTask, Error> {
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
            &root, &config, request, name, &branch, &base, &worktree, &ports,
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
    ) -> Result<TaskInfo, Error> {
        task::copy_files(root, worktree, &config.worktrees.copy)?;

        lock(&self.store).add_task(&NewTask {
            project: request.project,
            name,
            branch,
            base,
            worktree,
            ports,
        })
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
        })
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
    let mut env = vec![("SWARM_TASK".to_owned(), task.id.0.to_string())];

    if let Some(first) = task.ports.first() {
        let list: Vec<String> = task.ports.iter().map(u16::to_string).collect();

        env.push(("SWARM_PORT".to_owned(), first.to_string()));
        env.push(("SWARM_PORTS".to_owned(), list.join(",")));
    }

    env
}
