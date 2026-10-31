//! Review comments: checking a new one, finding its line again and writing the messages
//! the orchestrator gets (PLAN.md, Review).
//!
//! A change that sends comments has two parts. The plan reads git and the disk and takes
//! no lock. The write runs under the task's lock, checks that the comments are still as
//! the plan saw them, and stores the messages and the new comment states in one
//! transaction. When they changed, the call plans again.

use std::cell::OnceCell;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};
use std::sync::{Condvar, Mutex, PoisonError};

use swarm_protocol::{
    AgentId, AgentInfo, AgentLifecycle, AgentRole, COMMENT_BYTES, CommentAnchor, CommentId,
    CommentState, DIFF_LINE_BYTES, DiffSide, MessageInfo, MessageKind, NewComment, ProjectId,
    ReviewComment, ReviewSource, TaskId, TaskInfo,
};

use crate::store::{MAX_MESSAGE_BYTES, NewMessage, Store};
use crate::{Error, Projects, git, lock, unique_names};

/// How often a call plans again when the comments changed while it planned.
const ATTEMPTS: usize = 3;

/// Room in a message for its count line.
const HEADER_ROOM: usize = 128;

/// The longest path a comment can point at, in bytes.
const PATH_BYTES: usize = 4096;

/// What a comment change wrote.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Written {
    /// The messages posted to the orchestrator, oldest first.
    pub messages: Vec<MessageInfo>,
    /// The comments that were added or sent, oldest first.
    pub comments: Vec<ReviewComment>,
}

/// One lock per task, so a slow task does not hold up the others.
#[derive(Default)]
pub(crate) struct TaskLocks {
    held: Mutex<HashSet<TaskId>>,
    freed: Condvar,
}

pub(crate) struct TaskGuard<'a> {
    locks: &'a TaskLocks,
    task: TaskId,
}

impl TaskLocks {
    /// Waits until nobody holds the task, then holds it.
    pub(crate) fn lock(&self, task: TaskId) -> TaskGuard<'_> {
        let mut held = lock(&self.held);
        while held.contains(&task) {
            held = self
                .freed
                .wait(held)
                .unwrap_or_else(PoisonError::into_inner);
        }
        held.insert(task);

        TaskGuard { locks: self, task }
    }
}

impl Drop for TaskGuard<'_> {
    fn drop(&mut self) {
        lock(&self.locks.held).remove(&self.task);
        self.locks.freed.notify_all();
    }
}

/// Where the messages of a task's comments go.
struct Mail {
    project: ProjectId,
    task: TaskId,
    orchestrator: AgentId,
}

/// A comment ready to send, with the worker that owns its file.
struct Entry {
    comment: ReviewComment,
    /// The owner's name in the message.
    who: String,
    to: Option<AgentId>,
}

/// A comment checked and, when it goes at once, laid out for sending.
struct AddPlan {
    new: NewComment,
    outdated: bool,
    /// The state of the thread's first comment, for a reply.
    parent: Option<CommentState>,
    /// Set when the comment is sent at once.
    send: Option<(Mail, String, Option<AgentId>)>,
}

/// The pending comments of a task, laid out for sending.
struct SendPlan {
    mail: Mail,
    entries: Vec<Entry>,
}

impl Projects {
    /// Checks and stores a comment. A comment that is not held back goes to the
    /// orchestrator in the same transaction and is stored as `Sent`; refused when the
    /// orchestrator is stopped. `announce` runs under the task's lock and the store lock,
    /// so it must not call back into the store.
    pub fn add_comment(
        &self,
        new: &NewComment,
        announce: &dyn Fn(&Written),
    ) -> Result<Written, Error> {
        for _ in 0..ATTEMPTS {
            let plan = self.plan_add(new)?;

            let _guard = self.comments.lock(new.task);
            if let Some(written) = self.write_add(&plan, announce)? {
                return Ok(written);
            }
        }

        Err(changed())
    }

    /// Sends every pending comment of the task in messages of at most
    /// [`MAX_MESSAGE_BYTES`], split between comments. See [`Projects::add_comment`] for
    /// `announce`.
    pub fn send_comments(
        &self,
        task: TaskId,
        announce: &dyn Fn(&Written),
    ) -> Result<Written, Error> {
        for _ in 0..ATTEMPTS {
            let plan = self.plan_send(task)?;

            let _guard = self.comments.lock(task);
            if let Some(written) = self.write_send(&plan, announce)? {
                return Ok(written);
            }
        }

        Err(changed())
    }

    /// The ids of the task's pending comments, oldest first.
    pub fn pending_comments(&self, task: TaskId) -> Result<Vec<CommentId>, Error> {
        pending_ids(&lock(&self.store), task)
    }

    /// Deletes a pending comment and its replies. `announce` gets the task and every
    /// deleted id, under the task's lock.
    pub fn delete_comment(
        &self,
        id: CommentId,
        announce: impl FnOnce(TaskId, &[CommentId]),
    ) -> Result<(), Error> {
        let task = self.comment_task(id)?;
        let _guard = self.comments.lock(task);

        let deleted = {
            let store = lock(&self.store);
            let comment = open_comment(&store, id)?;

            if comment.state != CommentState::Pending {
                return Err(invalid("only a pending comment can be deleted"));
            }

            store.delete_comment(id)?
        };
        announce(task, &deleted);

        Ok(())
    }

    /// Resolves a sent thread, or sets it back to sent. `announce` gets the comment,
    /// under the task's lock.
    pub fn resolve_comment(
        &self,
        id: CommentId,
        resolved: bool,
        announce: impl FnOnce(&ReviewComment),
    ) -> Result<(), Error> {
        let task = self.comment_task(id)?;
        let _guard = self.comments.lock(task);

        let comment = {
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
                .ok_or_else(|| invalid(format!("no comment with id {}", id.0)))?
        };
        announce(&comment);

        Ok(())
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

    fn comment_task(&self, id: CommentId) -> Result<TaskId, Error> {
        lock(&self.store)
            .comment(id)?
            .map(|comment| comment.task)
            .ok_or_else(|| invalid(format!("no comment with id {}", id.0)))
    }

    /// Checks the comment. When it goes at once, finds its line and its owner too: this
    /// reads git and the disk, so call it without the task's lock.
    fn plan_add(&self, new: &NewComment) -> Result<AddPlan, Error> {
        let (task, parent) = {
            let store = lock(&self.store);
            let task = open_task(&store, new.task)?;
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

        let parent_state = parent.as_ref().map(|parent| parent.state);
        let (mut comment, send_now) = match (new.parent, parent) {
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

        if !send_now {
            return Ok(AddPlan {
                new: comment,
                outdated: false,
                parent: parent_state,
                send: None,
            });
        }

        let orchestrator = live_orchestrator(&task)?;
        let (line, outdated) = locate(&mut Files::new(&task), &comment.anchor, comment.source);
        comment.anchor.line = line;

        let to = Owners::new(&task).of(&comment.anchor);
        let mail = Mail {
            project: task.project,
            task: task.id,
            orchestrator: orchestrator.id,
        };

        Ok(AddPlan {
            send: Some((mail, owner_name(&task, to), to)),
            new: comment,
            outdated,
            parent: parent_state,
        })
    }

    /// Stores the planned comment, and its message when it goes at once. `None` when the
    /// thread or the task changed since the plan.
    fn write_add(
        &self,
        plan: &AddPlan,
        announce: &dyn Fn(&Written),
    ) -> Result<Option<Written>, Error> {
        let store = lock(&self.store);

        let written = store.atomically(|store| {
            let parent = plan
                .new
                .parent
                .map(|id| store.comment(id))
                .transpose()?
                .flatten()
                .map(|parent| parent.state);
            if parent != plan.parent || store.task(plan.new.task)?.is_none() {
                return Ok(None);
            }

            let comment = store.add_comment(&plan.new, CommentState::Pending)?;
            if plan.outdated {
                store.set_comment_anchor(comment.id, comment.anchor.line, true)?;
            }

            let Some((mail, who, to)) = &plan.send else {
                return Ok(Some(Written {
                    messages: Vec::new(),
                    comments: vec![comment],
                }));
            };
            let entry = Entry {
                comment,
                who: who.clone(),
                to: *to,
            };

            post(store, mail, &[entry], false).map(Some)
        })?;

        if let Some(written) = &written {
            announce(written);
        }

        Ok(written)
    }

    /// Lays out the task's pending comments. This reads git and the disk, so call it
    /// without the task's lock.
    fn plan_send(&self, id: TaskId) -> Result<SendPlan, Error> {
        let (task, comments) = {
            let store = lock(&self.store);
            let task = open_task(&store, id)?;

            let comments: Vec<ReviewComment> = store
                .task_comments(id)?
                .into_iter()
                .filter(|comment| comment.state == CommentState::Pending)
                .collect();

            (task, comments)
        };

        if comments.is_empty() {
            return Err(invalid("no comments are waiting"));
        }
        let orchestrator = live_orchestrator(&task)?.id;

        let comments = self.anchored(&task, comments)?;
        let owners = Owners::new(&task);
        let entries = comments
            .into_iter()
            .map(|comment| {
                let to = owners.of(&comment.anchor);

                Entry {
                    who: owner_name(&task, to),
                    comment,
                    to,
                }
            })
            .collect();

        Ok(SendPlan {
            mail: Mail {
                project: task.project,
                task: task.id,
                orchestrator,
            },
            entries,
        })
    }

    /// Stores the messages and marks the comments sent. `None` when the pending comments
    /// are not the planned ones any more.
    fn write_send(
        &self,
        plan: &SendPlan,
        announce: &dyn Fn(&Written),
    ) -> Result<Option<Written>, Error> {
        let store = lock(&self.store);

        let written = store.atomically(|store| {
            let waiting = pending_ids(store, plan.mail.task)?;
            let planned = plan.entries.iter().map(|entry| entry.comment.id);
            if !waiting.iter().copied().eq(planned) {
                return Ok(None);
            }

            post(store, &plan.mail, &plan.entries, true).map(Some)
        })?;

        if let Some(written) = &written {
            announce(written);
        }

        Ok(written)
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
            let (line, outdated) = locate(&mut files, &comment.anchor, comment.source);

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

fn changed() -> Error {
    invalid("the comments changed; try again")
}

fn open_task(store: &Store, id: TaskId) -> Result<TaskInfo, Error> {
    store
        .task(id)?
        .ok_or_else(|| invalid(format!("no open task with id {}", id.0)))
}

fn pending_ids(store: &Store, task: TaskId) -> Result<Vec<CommentId>, Error> {
    Ok(store
        .task_comments(task)?
        .into_iter()
        .filter(|comment| comment.state == CommentState::Pending)
        .map(|comment| comment.id)
        .collect())
}

/// The task's orchestrator, when it can still get a message.
fn live_orchestrator(task: &TaskInfo) -> Result<&AgentInfo, Error> {
    let orchestrator = task
        .agents
        .iter()
        .find(|agent| agent.role == AgentRole::Orchestrator)
        .ok_or_else(|| invalid("the task has no orchestrator"))?;

    if orchestrator.lifecycle == AgentLifecycle::Stopped {
        return Err(invalid("the orchestrator is stopped"));
    }

    Ok(orchestrator)
}

fn owner_name(task: &TaskInfo, owner: Option<AgentId>) -> String {
    owner
        .and_then(|id| task.agents.iter().find(|agent| agent.id == id))
        .map_or("you", |agent| agent.name.as_str())
        .to_owned()
}

/// A comment whose task is open.
fn open_comment(store: &Store, id: CommentId) -> Result<ReviewComment, Error> {
    let comment = store
        .comment(id)?
        .ok_or_else(|| invalid(format!("no comment with id {}", id.0)))?;

    if store.task(comment.task)?.is_none() {
        return Err(invalid(format!("no open task with id {}", comment.task.0)));
    }

    Ok(comment)
}

/// The message texts for the entries, then posts them and marks the comments sent.
/// Everything is written in the caller's transaction.
fn post(store: &Store, mail: &Mail, entries: &[Entry], batch: bool) -> Result<Written, Error> {
    let blocks: Vec<String> = entries
        .iter()
        .map(|entry| block(&entry.comment, &entry.who))
        .collect();

    let mut messages = Vec::new();
    for text in message_texts(&blocks, batch) {
        messages.push(store.add_message(&NewMessage {
            project: mail.project,
            task: Some(mail.task),
            from: None,
            to: mail.orchestrator,
            kind: MessageKind::Comment,
            text: &text,
            delivered: false,
        })?);
    }

    let comments = entries
        .iter()
        .map(|entry| store.mark_comment_sent(entry.comment.id, entry.to))
        .collect::<Result<_, _>>()?;

    Ok(Written { messages, comments })
}

/// Packs the blocks, in order, into messages of at most [`MAX_MESSAGE_BYTES`]. A `batch`
/// message starts with its count line, and with its place when there is more than one.
fn message_texts(blocks: &[String], batch: bool) -> Vec<String> {
    if !batch {
        return vec![blocks.join("\n\n")];
    }

    let room = MAX_MESSAGE_BYTES - HEADER_ROOM;
    let mut groups: Vec<Vec<&str>> = Vec::new();
    let mut size = 0;
    for block in blocks {
        match groups.last_mut() {
            Some(group) if size + 2 + block.len() <= room => {
                size += 2 + block.len();
                group.push(block);
            }
            _ => {
                size = block.len();
                groups.push(vec![block]);
            }
        }
    }

    let total = groups.len();
    groups
        .iter()
        .enumerate()
        .map(|(index, group)| {
            let count = group.len();
            let noun = if count == 1 { "comment" } else { "comments" };
            let place = if total > 1 {
                format!(" ({} of {total})", index + 1)
            } else {
                String::new()
            };

            format!("{count} {noun}{place}:\n\n{}", group.join("\n\n"))
        })
        .collect()
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

    if anchor.path.as_os_str().len() > PATH_BYTES {
        return Err(invalid("the path is too long"));
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

/// One comment as the orchestrator reads it. A message is read as text, so the path and
/// the quoted line show their control characters, and no body line can pass for a
/// message header.
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
        quoted(&anchor.path),
        anchor.line,
        escape_controls(&anchor.text),
        neutralize_headers(comment.body.trim_end()),
    )
}

/// The path in quotes, with control characters, quotes and backslashes escaped.
fn quoted(path: &Path) -> String {
    let mut quoted = String::from("\"");

    for character in path.to_string_lossy().chars() {
        if matches!(character, '"' | '\\') {
            quoted.push('\\');
        }
        push_visible(&mut quoted, character);
    }
    quoted.push('"');

    quoted
}

/// The text with every control character shown as an escape like `\n` or `\u{1b}`.
fn escape_controls(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());

    for character in text.chars() {
        push_visible(&mut escaped, character);
    }

    escaped
}

fn push_visible(text: &mut String, character: char) {
    if character.is_control() || matches!(character, '\u{2028}' | '\u{2029}') {
        text.extend(character.escape_debug());
    } else {
        text.push(character);
    }
}

const HEADER_START: &str = "[swarm]";

/// The text with a space before each line that starts like a message header.
fn neutralize_headers(body: &str) -> String {
    let mut text = String::with_capacity(body.len());

    for line in body.split_inclusive(is_line_break) {
        let header = line
            .get(..HEADER_START.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(HEADER_START));
        if header {
            text.push(' ');
        }
        text.push_str(line);
    }

    text
}

fn is_line_break(character: char) -> bool {
    matches!(
        character,
        '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

/// The 1-based line that holds the anchor now, and whether the text is gone. A missing
/// text keeps the line where it was.
fn locate(files: &mut Files<'_>, anchor: &CommentAnchor, source: ReviewSource) -> (u32, bool) {
    match files
        .lines(anchor, source)
        .and_then(|lines| find_line(anchor, lines))
    {
        Some(line) => (line, false),
        None => (anchor.line, true),
    }
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

    #[test]
    fn a_path_has_a_length_limit() {
        let task = task_with_worktree("/w");
        let mut long = anchor(1, "x", None, None);

        long.path = PathBuf::from("a".repeat(PATH_BYTES));
        assert!(check_anchor(&long, &task).is_ok());
        long.path = PathBuf::from("a".repeat(PATH_BYTES + 1));
        assert!(check_anchor(&long, &task).is_err());
    }

    fn comment(path: &str, text: &str, body: &str) -> ReviewComment {
        let mut anchor = anchor(1, text, None, None);
        anchor.path = PathBuf::from(path);

        ReviewComment {
            id: CommentId(1),
            task: TaskId(1),
            parent: None,
            source: ReviewSource::Merged,
            anchor,
            body: body.to_owned(),
            state: CommentState::Pending,
            to: None,
            outdated: false,
            created_at: 0,
            sent_at: None,
        }
    }

    #[test]
    fn the_longest_comment_still_fits_in_a_message() {
        let path = "\u{1}".repeat(PATH_BYTES);
        let text = "\u{1}".repeat(DIFF_LINE_BYTES);
        let body = "[swarm]\n".repeat(COMMENT_BYTES / 8);

        let longest = block(&comment(&path, &text, &body), "orchestrator");

        assert!(
            longest.len() + HEADER_ROOM <= MAX_MESSAGE_BYTES,
            "{}",
            longest.len()
        );
    }

    #[test]
    fn blocks_are_packed_in_order_without_passing_the_limit() {
        let room = MAX_MESSAGE_BYTES - HEADER_ROOM;
        let blocks = vec![
            "a".repeat(room),
            "b".repeat(room),
            "c".to_owned(),
            "d".to_owned(),
        ];

        let texts = message_texts(&blocks, true);

        assert_eq!(texts.len(), 3);
        assert!(texts[0].starts_with("1 comment (1 of 3):\n\na"));
        assert!(texts[1].starts_with("1 comment (2 of 3):\n\nb"));
        assert!(texts[2].ends_with(":\n\nc\n\nd"));
        assert!(texts[2].starts_with("2 comments (3 of 3):"));
        assert!(texts.iter().all(|text| text.len() <= MAX_MESSAGE_BYTES));
        assert_eq!(message_texts(&blocks[2..], true), ["2 comments:\n\nc\n\nd"]);
        assert_eq!(message_texts(&blocks[2..], false), ["c\n\nd"]);
    }

    #[test]
    fn only_control_characters_are_escaped() {
        assert_eq!(
            escape_controls("a\r\n\t\0\u{1b}\u{85}\u{2028}é\\\"'"),
            "a\\r\\n\\t\\0\\u{1b}\\u{85}\\u{2028}é\\\"'"
        );
        assert_eq!(quoted(Path::new("a\"b\\c\nd é")), "\"a\\\"b\\\\c\\nd é\"");
    }

    #[test]
    fn a_header_at_the_start_of_a_line_gets_a_space() {
        assert_eq!(neutralize_headers("[swarm] x"), " [swarm] x");
        assert_eq!(
            neutralize_headers("a\n[Swarm] b\r[SWARM]"),
            "a\n [Swarm] b\r [SWARM]"
        );
        assert_eq!(
            neutralize_headers("a\u{2028}[swarm]\u{85}[swarm]"),
            "a\u{2028} [swarm]\u{85} [swarm]"
        );
        assert_eq!(
            neutralize_headers("not [swarm]\n [swarm] "),
            "not [swarm]\n [swarm] "
        );
        assert_eq!(neutralize_headers("[swar"), "[swar");
        assert_eq!(neutralize_headers(""), "");
    }

    #[test]
    fn a_task_lock_does_not_hold_up_another_task() {
        let locks = std::sync::Arc::new(TaskLocks::default());
        let _first = locks.lock(TaskId(1));
        let (sender, receiver) = std::sync::mpsc::channel();

        let other = std::sync::Arc::clone(&locks);
        std::thread::spawn(move || {
            let _second = other.lock(TaskId(2));
            sender.send(()).ok();
        });

        receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("task 2 waited for task 1");
    }

    #[test]
    fn a_task_lock_waits_for_the_same_task() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let locks = std::sync::Arc::new(TaskLocks::default());
        let released = std::sync::Arc::new(AtomicBool::new(false));
        let first = locks.lock(TaskId(1));

        let (waiter_locks, waiter_released) = (
            std::sync::Arc::clone(&locks),
            std::sync::Arc::clone(&released),
        );
        let waiter = std::thread::spawn(move || {
            let _second = waiter_locks.lock(TaskId(1));

            waiter_released.load(Ordering::SeqCst)
        });

        released.store(true, Ordering::SeqCst);
        drop(first);

        assert!(waiter.join().expect("thread"), "it took the lock too early");
        drop(locks.lock(TaskId(1)));
        assert!(lock(&locks.held).is_empty(), "no entry is left behind");
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
