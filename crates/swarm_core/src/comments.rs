//! Review comments: checking a new one, finding its line again and writing the message
//! the orchestrator gets (PLAN.md, Review).

use std::cell::OnceCell;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};
use std::sync::MutexGuard;

use swarm_protocol::{
    AgentId, AgentRole, COMMENT_BYTES, CommentAnchor, CommentId, CommentState, DIFF_LINE_BYTES,
    DiffSide, NewComment, ProjectId, ReviewComment, ReviewSource, TaskId, TaskInfo,
};

use crate::{Error, Projects, git, lock, unique_names};

/// A comment that was just stored as `Pending`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedComment {
    pub comment: ReviewComment,
    /// The comment is not held back: its caller sends it now.
    pub send_now: bool,
}

/// What the daemon posts for some comments, and who each one is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub project: ProjectId,
    pub task: TaskId,
    pub orchestrator: AgentId,
    pub text: String,
    /// The worker that owns each comment's file; `None` is the orchestrator.
    pub owners: Vec<(CommentId, Option<AgentId>)>,
}

impl Projects {
    /// Hold it while a comment is added, sent, deleted or resolved.
    pub fn comment_guard(&self) -> MutexGuard<'_, ()> {
        lock(&self.comments)
    }

    /// Checks and stores a comment as `Pending`. The caller sends it when `send_now`.
    pub fn add_comment(&self, new: &NewComment) -> Result<AddedComment, Error> {
        let (task, parent) = {
            let store = lock(&self.store);
            let task = store
                .task(new.task)?
                .ok_or_else(|| Error::Invalid(format!("no open task with id {}", new.task.0)))?;
            let parent = new
                .parent
                .map(|id| store.comment(id))
                .transpose()?
                .flatten();

            (task, parent)
        };

        if new.body.trim().is_empty() {
            return Err(invalid("the comment is empty"));
        }
        if new.body.len() > COMMENT_BYTES {
            return Err(invalid(format!(
                "the comment is {} KiB; the limit is {} KiB",
                new.body.len().div_ceil(1024),
                COMMENT_BYTES >> 10,
            )));
        }

        let (comment, send_now) = match (new.parent, parent) {
            (None, _) => {
                check_anchor(&new.anchor, &task)?;

                (new.clone(), new.source == ReviewSource::Live)
            }
            (Some(id), None) => return Err(invalid(format!("no comment with id {}", id.0))),
            (Some(_), Some(parent)) => {
                if parent.task != new.task {
                    return Err(invalid("the comment is in another task"));
                }
                if parent.parent.is_some() {
                    return Err(invalid("a reply goes to a thread's first comment"));
                }

                let reply = NewComment {
                    source: parent.source,
                    anchor: parent.anchor,
                    ..new.clone()
                };

                (reply, parent.state != CommentState::Pending)
            }
        };

        let comment = lock(&self.store).add_comment(&comment, CommentState::Pending)?;

        Ok(AddedComment { comment, send_now })
    }

    /// Undoes [`Projects::add_comment`] when it could not be sent.
    pub fn discard_comment(&self, id: CommentId) -> Result<(), Error> {
        lock(&self.store).delete_comment(id)?;

        Ok(())
    }

    /// The ids of the task's pending comments, oldest first.
    pub fn pending_comments(&self, task: TaskId) -> Result<Vec<CommentId>, Error> {
        Ok(lock(&self.store)
            .task_comments(task)?
            .into_iter()
            .filter(|comment| comment.state == CommentState::Pending)
            .map(|comment| comment.id)
            .collect())
    }

    /// The message for these pending comments of an open task: `batch` adds the count
    /// line. Finds each anchor again and the worker behind each file first.
    pub fn outgoing(
        &self,
        task: TaskId,
        ids: &[CommentId],
        batch: bool,
    ) -> Result<Outgoing, Error> {
        let (task, comments) = {
            let store = lock(&self.store);
            let task = store
                .task(task)?
                .ok_or_else(|| Error::Invalid(format!("no open task with id {}", task.0)))?;

            let mut comments = Vec::new();
            for id in ids {
                let comment = store
                    .comment(*id)?
                    .filter(|comment| comment.task == task.id)
                    .filter(|comment| comment.state == CommentState::Pending)
                    .ok_or_else(|| invalid(format!("comment {} is not pending", id.0)))?;
                comments.push(comment);
            }

            (task, comments)
        };

        let orchestrator = task
            .agents
            .iter()
            .find(|agent| agent.role == AgentRole::Orchestrator)
            .ok_or_else(|| invalid("the task has no orchestrator"))?;
        let comments = self.anchored(&task, comments)?;

        let owners = Owners::new(&task);
        let names: HashMap<AgentId, &str> = task
            .agents
            .iter()
            .map(|agent| (agent.id, agent.name.as_str()))
            .collect();

        let mut blocks = Vec::new();
        let mut recipients = Vec::new();
        for comment in &comments {
            let to = owners.of(&comment.anchor);
            let who = to.and_then(|id| names.get(&id).copied()).unwrap_or("you");

            blocks.push(block(comment, who));
            recipients.push((comment.id, to));
        }

        let mut text = blocks.join("\n\n");
        if batch {
            let count = comments.len();
            let noun = if count == 1 { "comment" } else { "comments" };

            text = format!("{count} {noun}:\n\n{text}");
        }

        Ok(Outgoing {
            project: task.project,
            task: task.id,
            orchestrator: orchestrator.id,
            text,
            owners: recipients,
        })
    }

    /// Marks the comments as sent to their owners.
    pub fn mark_comments_sent(
        &self,
        owners: &[(CommentId, Option<AgentId>)],
    ) -> Result<Vec<ReviewComment>, Error> {
        let store = lock(&self.store);

        owners
            .iter()
            .map(|(id, to)| store.mark_comment_sent(*id, *to))
            .collect()
    }

    /// Deletes a pending comment and its replies. Returns the task and every deleted id.
    pub fn delete_comment(&self, id: CommentId) -> Result<(TaskId, Vec<CommentId>), Error> {
        let store = lock(&self.store);
        let comment = open_comment(&store, id)?;

        if comment.state != CommentState::Pending {
            return Err(invalid("only a pending comment can be deleted"));
        }

        Ok((comment.task, store.delete_comment(id)?))
    }

    /// Resolves a sent thread, or sets it back to sent.
    pub fn resolve_comment(&self, id: CommentId, resolved: bool) -> Result<ReviewComment, Error> {
        let store = lock(&self.store);
        let comment = open_comment(&store, id)?;

        if comment.parent.is_some() {
            return Err(invalid("only a thread's first comment can be resolved"));
        }
        if comment.state == CommentState::Pending {
            return Err(invalid("a pending comment is not sent yet"));
        }

        let state = if resolved {
            CommentState::Resolved
        } else {
            CommentState::Sent
        };
        store.set_comment_state(id, state)?;

        store
            .comment(id)?
            .ok_or_else(|| invalid(format!("no comment with id {}", id.0)))
    }

    /// The comments of the task, oldest first, with each anchor found again in the files
    /// as they are now. A closed task has none.
    pub fn list_comments(&self, id: TaskId) -> Result<Vec<ReviewComment>, Error> {
        let (task, comments) = {
            let store = lock(&self.store);
            let task = store
                .task_including_closed(id)?
                .ok_or_else(|| invalid(format!("no task with id {}", id.0)))?;
            if task.closed {
                return Ok(Vec::new());
            }

            let comments = store.task_comments(id)?;

            (task, comments)
        };

        self.anchored(&task, comments)
    }

    /// Finds each comment's line again and saves what changed. Git and the disk are
    /// read without the store lock.
    fn anchored(
        &self,
        task: &TaskInfo,
        mut comments: Vec<ReviewComment>,
    ) -> Result<Vec<ReviewComment>, Error> {
        let mut files = Files::new(task);
        let mut changed = Vec::new();

        for comment in &mut comments {
            let found = files
                .lines(&comment.anchor, comment.source)
                .and_then(|lines| find_line(&comment.anchor, lines));

            let (line, outdated) = match found {
                Some(line) => (line, false),
                None => (comment.anchor.line, true),
            };
            if line != comment.anchor.line || outdated != comment.outdated {
                comment.anchor.line = line;
                comment.outdated = outdated;
                changed.push((comment.id, line, outdated));
            }
        }

        if !changed.is_empty() {
            let store = lock(&self.store);

            for (id, line, outdated) in changed {
                store.set_comment_anchor(id, line, outdated)?;
            }
        }

        Ok(comments)
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

/// A comment whose task is open.
fn open_comment(store: &crate::store::Store, id: CommentId) -> Result<ReviewComment, Error> {
    let comment = store
        .comment(id)?
        .ok_or_else(|| invalid(format!("no comment with id {}", id.0)))?;

    if store.task(comment.task)?.is_none() {
        return Err(invalid(format!("no open task with id {}", comment.task.0)));
    }

    Ok(comment)
}

/// The anchor sits in the task worktree or a worker's own worktree, on a relative path
/// inside it, and its texts are no longer than a diff line.
fn check_anchor(anchor: &CommentAnchor, task: &TaskInfo) -> Result<(), Error> {
    let own = task
        .agents
        .iter()
        .any(|agent| agent.role == AgentRole::Worker && agent.worktree == anchor.worktree);
    if anchor.worktree != task.worktree && !own {
        return Err(invalid("the comment is not in a worktree of this task"));
    }

    let mut names = anchor.path.components();
    let first = names.next();
    let relative = matches!(first, Some(Component::Normal(_)))
        && names.all(|component| matches!(component, Component::Normal(_)));
    if !relative {
        return Err(invalid("the path must be relative, without `..`"));
    }
    if first == Some(Component::Normal(".git".as_ref())) {
        return Err(invalid("the path is inside .git"));
    }

    if anchor.line == 0 {
        return Err(invalid("lines count from 1"));
    }

    let texts = [
        Some(&anchor.text),
        anchor.before.as_ref(),
        anchor.after.as_ref(),
    ];
    if texts
        .into_iter()
        .flatten()
        .any(|text| text.len() > DIFF_LINE_BYTES)
    {
        return Err(invalid("the anchored line is too long"));
    }

    Ok(())
}

/// One comment as the orchestrator reads it.
fn block(comment: &ReviewComment, who: &str) -> String {
    let anchor = &comment.anchor;
    let reply = comment
        .parent
        .map(|parent| format!(" reply to #{}", parent.0))
        .unwrap_or_default();
    let side = match anchor.side {
        DiffSide::Old => " (old)",
        DiffSide::New => "",
    };

    format!(
        "#{}{reply} for {who} · {}:{}{side}\n> {}\n{}",
        comment.id.0,
        anchor.path.display(),
        anchor.line,
        anchor.text,
        comment.body.trim_end(),
    )
}

/// Who owns a file: the worker whose own worktree it is in, else the agent of the
/// newest `swarm commit` of the path.
struct Owners<'a> {
    task: &'a TaskInfo,
    names: HashMap<&'a str, Option<AgentId>>,
    trailers: OnceCell<HashMap<Vec<u8>, String>>,
}

impl<'a> Owners<'a> {
    fn new(task: &'a TaskInfo) -> Self {
        Self {
            task,
            names: unique_names(&task.agents),
            trailers: OnceCell::new(),
        }
    }

    fn of(&self, anchor: &CommentAnchor) -> Option<AgentId> {
        let owner = if anchor.worktree == self.task.worktree {
            let trailers = self.trailers.get_or_init(|| self.attribution());

            trailers
                .get(anchor.path.as_os_str().as_bytes())
                .and_then(|name| self.names.get(name.as_str()).copied().flatten())
        } else {
            self.task
                .agents
                .iter()
                .find(|agent| agent.worktree == anchor.worktree)
                .map(|agent| agent.id)
        };

        owner.filter(|owner| {
            self.task
                .agents
                .iter()
                .any(|agent| agent.id == *owner && agent.role == AgentRole::Worker)
        })
    }

    fn attribution(&self) -> HashMap<Vec<u8>, String> {
        let task = self.task;
        let found = git::merge_base(&task.worktree, &task.base)
            .and_then(|base| git::attribution(&task.worktree, &base));

        found.unwrap_or_else(|error| {
            log::warn!(
                "cannot tell who owns the files of task {}: {error}",
                task.id.0
            );

            HashMap::new()
        })
    }
}

type FileKey = (PathBuf, PathBuf, DiffSide, ReviewSource);

/// The files comments point at, each read once.
struct Files<'a> {
    task: &'a TaskInfo,
    read: HashMap<FileKey, Option<Vec<String>>>,
    bases: HashMap<PathBuf, Option<String>>,
}

impl<'a> Files<'a> {
    fn new(task: &'a TaskInfo) -> Self {
        Self {
            task,
            read: HashMap::new(),
            bases: HashMap::new(),
        }
    }

    /// The lines of the commented side, or `None` when the file or worktree is gone.
    fn lines(&mut self, anchor: &CommentAnchor, source: ReviewSource) -> Option<&[String]> {
        let key = (
            anchor.worktree.clone(),
            anchor.path.clone(),
            anchor.side,
            source,
        );
        if !self.read.contains_key(&key) {
            let lines = self.load(anchor, source);
            self.read.insert(key.clone(), lines);
        }

        self.read.get(&key)?.as_deref()
    }

    fn load(&mut self, anchor: &CommentAnchor, source: ReviewSource) -> Option<Vec<String>> {
        if !anchor.worktree.is_dir() {
            return None;
        }

        let read = match (anchor.side, source) {
            (DiffSide::New, ReviewSource::Live) => {
                Ok(git::working_file_lines(&anchor.worktree, &anchor.path))
            }
            (DiffSide::New, ReviewSource::Merged) => {
                git::blob_lines(&self.task.worktree, "HEAD", &anchor.path)
            }
            (DiffSide::Old, _) => match self.base(&anchor.worktree) {
                Some(base) => git::blob_lines(&anchor.worktree, &base, &anchor.path),
                None => Ok(None),
            },
        };

        read.unwrap_or_else(|error| {
            log::warn!("cannot read {}: {error}", anchor.path.display());

            None
        })
    }

    /// Where the worktree left its base, like the diff finds it.
    fn base(&mut self, worktree: &Path) -> Option<String> {
        self.bases
            .entry(worktree.to_owned())
            .or_insert_with(|| {
                let task = self.task;
                let base = if worktree == task.worktree {
                    git::merge_base(worktree, &task.base)
                } else {
                    git::commit_of(&task.worktree, "HEAD")
                        .and_then(|head| git::merge_base(worktree, &head))
                };

                base.map_err(|error| log::warn!("cannot find the base of a comment: {error}"))
                    .ok()
            })
            .clone()
    }
}

/// The 1-based line that holds the anchored text: a line with the same neighbors wins,
/// then the one nearest to where the comment was.
fn find_line(anchor: &CommentAnchor, lines: &[String]) -> Option<u32> {
    let stored = i64::from(anchor.line);

    let (index, _) = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| **line == anchor.text)
        .map(|(index, _)| {
            let before = index > 0 && anchor.before.as_ref() == lines.get(index - 1);
            let after = anchor.after.is_some() && anchor.after.as_ref() == lines.get(index + 1);
            let score = u8::from(before) + u8::from(after);
            let distance = (i64::try_from(index).unwrap_or(i64::MAX) + 1 - stored).unsigned_abs();

            (index, (score, Reverse(distance), Reverse(index)))
        })
        .max_by_key(|(_, rank)| *rank)?;

    u32::try_from(index + 1).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor(line: u32, text: &str, before: Option<&str>, after: Option<&str>) -> CommentAnchor {
        CommentAnchor {
            worktree: PathBuf::from("/w"),
            path: PathBuf::from("a.rs"),
            side: DiffSide::New,
            line,
            text: text.to_owned(),
            before: before.map(str::to_owned),
            after: after.map(str::to_owned),
        }
    }

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|line| (*line).to_owned()).collect()
    }

    #[test]
    fn the_nearest_equal_line_wins_without_neighbors() {
        let file = lines(&["x", "a", "x", "b", "x"]);

        assert_eq!(find_line(&anchor(4, "x", None, None), &file), Some(3));
        assert_eq!(find_line(&anchor(2, "x", None, None), &file), Some(1));
        assert_eq!(find_line(&anchor(5, "x", None, None), &file), Some(5));
    }

    #[test]
    fn matching_neighbors_beat_distance() {
        let file = lines(&["p", "x", "q", "a", "b", "x", "c"]);

        assert_eq!(
            find_line(&anchor(2, "x", Some("b"), Some("c")), &file),
            Some(6)
        );
    }

    #[test]
    fn a_missing_line_is_not_found() {
        assert_eq!(find_line(&anchor(1, "x", None, None), &lines(&["y"])), None);
    }

    #[test]
    fn paths_must_stay_inside_the_worktree() {
        let task = task_with_worktree("/w");

        for bad in ["", "/etc/passwd", "../x", "a/../../x", "./a", ".git/config"] {
            let mut bad_anchor = anchor(1, "x", None, None);
            bad_anchor.path = PathBuf::from(bad);

            assert!(check_anchor(&bad_anchor, &task).is_err(), "{bad:?}");
        }
        assert!(check_anchor(&anchor(1, "x", None, None), &task).is_ok());
    }

    fn task_with_worktree(worktree: &str) -> TaskInfo {
        TaskInfo {
            id: TaskId(1),
            project: ProjectId(1),
            name: "t".to_owned(),
            branch: "swarm/t".to_owned(),
            base: "main".to_owned(),
            worktree: PathBuf::from(worktree),
            ports: Vec::new(),
            setup_terminal: None,
            setup_exit: None,
            briefed: false,
            agents: Vec::new(),
            approved: None,
            closed: false,
            created_by: None,
        }
    }
}
