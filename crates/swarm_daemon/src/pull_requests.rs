//! Pull requests: the draft the orchestrator writes, the push and `gh pr create`, the
//! status read back from GitHub, and the actions on it (PLAN.md, Pull requests).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use swarm_core::git;
use swarm_core::github::{self, Github, NewPullRequest, Target};
use swarm_core::prompts::{self, PromptFile};
use swarm_core::store::{MAX_MESSAGE_BYTES, NewMessage};
use swarm_protocol::{
    AgentLifecycle, AgentRole, CheckState, Event, MergeMethod, MessageKind, PrDraft, PullRequest,
    TaskId, TaskInfo,
};

use crate::{Services, tasks};

/// A push may run a pre-push hook, which can run the whole test suite.
const PUSH_TIMEOUT: Duration = Duration::from_secs(600);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a client asks for when it creates the pull request.
pub(crate) struct NewRequest {
    pub(crate) draft: PrDraft,
    pub(crate) as_draft: bool,
    pub(crate) close_after_merge: bool,
    pub(crate) skip_checks: bool,
}

/// The daemon's memory about pull requests.
pub(crate) struct PullRequests {
    github: Github,
    /// `~/.config/swarm`, where the user's `pr.md` lives.
    config_dir: Option<PathBuf>,
    /// The latest draft the orchestrator sent for each task. Lost on a daemon restart.
    drafts: Mutex<HashMap<TaskId, PrDraft>>,
    /// Tasks with a create, merge or other change under way.
    busy: Mutex<HashSet<TaskId>>,
}

/// Held while a change to a task's pull request runs.
pub(crate) struct Busy<'a> {
    owner: &'a PullRequests,
    task: TaskId,
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        lock(&self.owner.busy).remove(&self.task);
    }
}

impl PullRequests {
    pub(crate) fn new(github: Github, config_dir: Option<PathBuf>) -> Self {
        Self {
            github,
            config_dir,
            drafts: Mutex::default(),
            busy: Mutex::default(),
        }
    }

    pub(crate) fn github(&self) -> &Github {
        &self.github
    }

    /// Keeps the draft as the latest one of the task.
    fn store(&self, task: TaskId, draft: PrDraft) {
        lock(&self.drafts).insert(task, draft);
    }

    #[cfg(test)]
    fn draft(&self, task: TaskId) -> Option<PrDraft> {
        lock(&self.drafts).get(&task).cloned()
    }

    /// Forgets the draft of a task that closed.
    pub(crate) fn forget(&self, task: TaskId) {
        lock(&self.drafts).remove(&task);
    }

    fn begin(&self, task: TaskId) -> Result<Busy<'_>> {
        if !lock(&self.busy).insert(task) {
            bail!("another change to this pull request is running");
        }

        Ok(Busy { owner: self, task })
    }
}

/// Stores the draft the task's orchestrator sent and tells every client.
pub(crate) fn store_draft(services: &Services, task: TaskId, draft: PrDraft) {
    services.pull_requests.store(task, draft.clone());

    services
        .clients
        .broadcast(&Event::PrDraftChanged { task, draft });
}

/// Asks the task's orchestrator for a title and a description. Refused unless the task is
/// approved as it is now and the orchestrator can take a message.
pub(crate) fn request_draft(services: &Services, id: TaskId) -> Result<()> {
    let task = services.projects.check_approved(id)?;
    if services.agents.is_closing(Some(id)) {
        bail!("the task is closing");
    }

    let orchestrator = task
        .agents
        .iter()
        .find(|agent| agent.role == AgentRole::Orchestrator)
        .context("the task has no orchestrator")?;
    let orchestrator = services
        .agents
        .info(orchestrator.id)
        .unwrap_or_else(|| orchestrator.clone());
    if orchestrator.lifecycle == AgentLifecycle::Stopped {
        bail!("the orchestrator is stopped");
    }

    let root = services.projects.project_path(task.project)?;
    let format = prompts::layered(
        PromptFile::Pr,
        services.pull_requests.config_dir.as_deref(),
        &root,
    );
    let text = draft_request(&task, &format);
    if text.len() > MAX_MESSAGE_BYTES {
        bail!(
            "the pull request prompt is longer than {} KiB",
            MAX_MESSAGE_BYTES >> 10
        );
    }

    services.messages.post(&NewMessage {
        project: task.project,
        task: Some(id),
        from: None,
        to: orchestrator.id,
        kind: MessageKind::PrDraft,
        text: &text,
        delivered: false,
    })?;
    services.agents.deliver_unread(orchestrator.id);

    Ok(())
}

/// The message the orchestrator reads: what to do, then the `pr.md` format.
fn draft_request(task: &TaskInfo, format: &str) -> String {
    format!(
        "The user approved the task \"{name}\". Write the pull request for branch `{branch}` \
         into `{base}`.\n\n\
         1. Read the changes in the task worktree: `git log {base}..HEAD` and `git diff {base}...HEAD`.\n\
         2. Write a title and a description in the format below.\n\
         3. Send them with `swarm pr draft --title \"<title>\" --body-file -` and the description \
         on stdin, or with the `pr_draft` tool.\n\n\
         {format}",
        name = task.name,
        branch = task.branch,
        base = task.base,
    )
}

/// Pushes the task branch and opens the pull request, then records it on the task.
pub(crate) fn create(services: &Services, id: TaskId, request: &NewRequest) -> Result<()> {
    let _busy = services.pull_requests.begin(id)?;

    let task = services.projects.task(id)?;
    if services.agents.is_closing(Some(id)) {
        bail!("the task is closing");
    }
    if let Some(link) = &task.pull_request {
        bail!("the task has pull request #{} already", link.number);
    }

    let title = github::check_title(&request.draft.title)?;
    github::check_body(&request.draft.body)?;

    let repository = repository_of(services, &task)?;
    let root = services.projects.project_path(task.project)?;
    services.pull_requests.github.require_ready(&root)?;

    let task = services.projects.check_approved(id)?;
    let pending = services.projects.pending_comments(id)?.len();
    if pending > 0 {
        bail!(
            "{pending} {} pending: send or delete {} first",
            if pending == 1 {
                "comment is"
            } else {
                "comments are"
            },
            if pending == 1 { "it" } else { "them" },
        );
    }
    check_local_checks(services, &task, request.skip_checks)?;

    git::push_branch(&task.worktree, &task.branch, PUSH_TIMEOUT)?;

    let link = services.pull_requests.github.create_pull_request(
        &Target {
            worktree: &task.worktree,
            repository: &repository,
        },
        &NewPullRequest {
            base: &task.base,
            head: &task.branch,
            title: &title,
            body: &request.draft.body,
            draft: request.as_draft,
        },
    )?;

    let task = services
        .projects
        .set_pull_request(id, &link, request.close_after_merge)
        .with_context(|| {
            format!(
                "pull request #{} is open, but Swarm could not record it",
                link.number
            )
        })?;
    services.clients.broadcast(&Event::TaskChanged { task });

    Ok(())
}

/// The local checks must have passed on the current commit, unless they are not required
/// and the user chose to skip them.
fn check_local_checks(services: &Services, task: &TaskInfo, skip: bool) -> Result<()> {
    let config = services.projects.checks_config(task.project)?;
    let commands: Vec<String> = config
        .before_pr
        .iter()
        .map(|command| command.trim().to_owned())
        .filter(|command| !command.is_empty())
        .collect();

    if commands.is_empty() || (skip && !config.required) {
        return Ok(());
    }

    let Some(run) = services.checks.latest(task.id) else {
        bail!("run the local checks first");
    };
    if run.commit != git::commit_of(&task.worktree, "HEAD")? {
        bail!("the local checks ran on an older commit: run them again");
    }
    if run
        .checks
        .iter()
        .map(|check| check.command.as_str())
        .ne(commands.iter().map(String::as_str))
    {
        bail!("the checks in the config changed since the last run: run them again");
    }
    if let Some(check) = run
        .checks
        .iter()
        .find(|check| check.state == CheckState::Failed)
    {
        bail!("the local check `{}` failed", check.command);
    }
    if run
        .checks
        .iter()
        .any(|check| check.state != CheckState::Passed)
    {
        bail!("the local checks have not all passed");
    }

    Ok(())
}

/// `owner/name` of the project's `origin`, which must be on GitHub.
fn repository_of(services: &Services, task: &TaskInfo) -> Result<String> {
    services
        .projects
        .repository(task.project)?
        .context("the project's origin is not on GitHub")
}

/// The task with its pull request number and the place `gh` runs.
fn open_pull_request(services: &Services, id: TaskId) -> Result<(TaskInfo, u64, String)> {
    let task = services.projects.task(id)?;
    let number = task
        .pull_request
        .as_ref()
        .context("the task has no pull request")?
        .number;
    let repository = repository_of(services, &task)?;

    Ok((task, number, repository))
}

/// The pull request as GitHub reports it now.
pub(crate) fn status(services: &Services, id: TaskId) -> Result<PullRequest> {
    let (task, number, repository) = open_pull_request(services, id)?;

    Ok(services.pull_requests.github.pull_request(
        &Target {
            worktree: &task.worktree,
            repository: &repository,
        },
        number,
    )?)
}

pub(crate) fn mark_ready(services: &Services, id: TaskId) -> Result<()> {
    let _busy = services.pull_requests.begin(id)?;
    let (task, number, repository) = open_pull_request(services, id)?;

    services.pull_requests.github.mark_ready(
        &Target {
            worktree: &task.worktree,
            repository: &repository,
        },
        number,
    )?;

    Ok(())
}

pub(crate) fn add_reviewers(services: &Services, id: TaskId, logins: &[String]) -> Result<()> {
    let _busy = services.pull_requests.begin(id)?;
    let (task, number, repository) = open_pull_request(services, id)?;

    services.pull_requests.github.add_reviewers(
        &Target {
            worktree: &task.worktree,
            repository: &repository,
        },
        number,
        logins,
    )?;

    Ok(())
}

/// Merges on GitHub. With "close after merge" the task then closes as Close task does,
/// unless uncommitted work would be lost; its merged local branch goes too.
pub(crate) fn merge(services: &Services, id: TaskId, method: MergeMethod) -> Result<()> {
    let _busy = services.pull_requests.begin(id)?;
    let (task, number, repository) = open_pull_request(services, id)?;

    services.pull_requests.github.merge(
        &Target {
            worktree: &task.worktree,
            repository: &repository,
        },
        number,
        method,
        &task.branch,
    )?;

    if task.close_after_merge {
        let check = tasks::close_check(services, id)?;

        if check.uncommitted_files == 0 {
            tasks::close(services, id, &check)
                .context("the pull request is merged, but the task did not close")?;

            if let Err(error) = services
                .projects
                .delete_merged_branch(task.project, &task.branch)
            {
                log::warn!("cannot delete the merged branch {}: {error}", task.branch);
            }

            return Ok(());
        }
    }

    let task = services.projects.task(id)?;
    services.clients.broadcast(&Event::TaskChanged { task });

    Ok(())
}

pub(crate) fn set_close_after_merge(services: &Services, id: TaskId, close: bool) -> Result<()> {
    let task = services.projects.set_close_after_merge(id, close)?;
    services.clients.broadcast(&Event::TaskChanged { task });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pull_requests() -> PullRequests {
        PullRequests::new(
            Github::with_folders(PathBuf::from("/tmp/none-bin"), None, Vec::new()),
            None,
        )
    }

    fn draft(title: &str) -> PrDraft {
        PrDraft {
            title: title.to_owned(),
            body: "body".to_owned(),
        }
    }

    #[test]
    fn the_latest_draft_of_a_task_is_kept_until_it_closes() {
        let pull_requests = pull_requests();

        pull_requests.store(TaskId(1), draft("first"));
        pull_requests.store(TaskId(1), draft("second"));
        pull_requests.store(TaskId(2), draft("other"));

        assert_eq!(pull_requests.draft(TaskId(1)), Some(draft("second")));
        pull_requests.forget(TaskId(1));
        assert_eq!(pull_requests.draft(TaskId(1)), None);
        assert_eq!(pull_requests.draft(TaskId(2)), Some(draft("other")));
    }

    #[test]
    fn one_change_per_task_runs_at_a_time() {
        let pull_requests = pull_requests();

        let first = pull_requests.begin(TaskId(1)).expect("first");
        assert!(pull_requests.begin(TaskId(1)).is_err());
        assert!(pull_requests.begin(TaskId(2)).is_ok());
        drop(first);

        assert!(pull_requests.begin(TaskId(1)).is_ok());
    }

    #[test]
    fn the_draft_request_names_the_branch_and_ends_with_the_format() {
        let task = TaskInfo {
            id: TaskId(1),
            project: swarm_protocol::ProjectId(1),
            name: "Login".to_owned(),
            branch: "swarm/login".to_owned(),
            base: "main".to_owned(),
            worktree: PathBuf::from("/repo/.worktrees/login"),
            ports: Vec::new(),
            setup_terminal: None,
            setup_exit: None,
            agents: Vec::new(),
            created_by: None,
            briefed: true,
            approved: Some(1),
            closed: false,
            pull_request: None,
            close_after_merge: false,
        };

        let text = draft_request(&task, "## What\n\n- the format");

        assert!(text.contains("branch `swarm/login` into `main`"));
        assert!(text.contains("git diff main...HEAD"));
        assert!(text.contains("swarm pr draft --title"));
        assert!(text.ends_with("## What\n\n- the format"));
    }
}
