//! The Files panel: the paths in a task worktree.

use anyhow::Result;
use swarm_core::git;
use swarm_protocol::{TASK_FILES_LISTED, TaskId};

use crate::Services;

/// The sorted paths in the task worktree, and how many the cap cut.
pub(crate) fn list(services: &Services, task: TaskId) -> Result<(Vec<String>, usize)> {
    let task = services.projects.task(task)?;
    let listed = git::worktree_files(&task.worktree, TASK_FILES_LISTED)?;

    Ok(listed)
}
