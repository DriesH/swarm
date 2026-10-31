//! Review comments: the workspace side. Lists, adds, deletes and resolves comments
//! through the daemon, and applies its comment events.

use std::collections::{HashMap, HashSet};

use gpui::{AppContext as _, Context, SharedString};
use swarm_protocol::{
    AgentId as DaemonAgentId, Client, ClientError, CommentEvent, CommentId, NewComment,
    ReviewComment, ReviewSource, TaskId as DaemonTaskId,
};

use crate::model::TaskId;
use crate::review_model::{
    apply_comment_event, count_comments, fake_add, fake_delete, fake_resolve, fake_send,
};
use crate::review_view::short;
use crate::workspace::{DataSource, Workspace};

/// What a daemon answer is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommentSlot {
    /// A comment box, by the id the view gave it.
    Composer(u64),
    /// The reply field of a thread.
    Reply(CommentId),
    /// A thread: Delete, Resolve or Reopen.
    Card(CommentId),
}

/// The task, source and diff version the comments were listed for.
pub(crate) type Listed = (DaemonTaskId, ReviewSource, Option<u64>);

/// The list requests of one task.
#[derive(Default)]
struct TaskListing {
    /// The number the next request gets.
    next: u64,
    /// The newest request whose answer was taken.
    applied: u64,
    /// The events that came since each unanswered request began.
    running: HashMap<u64, Vec<CommentEvent>>,
}

/// The comment lists asked of the daemon, and the events that came while they were on
/// their way: an answer must not undo a newer event.
#[derive(Default)]
pub(crate) struct CommentSync {
    tasks: HashMap<DaemonTaskId, TaskListing>,
    /// Asked and not answered yet.
    asking: HashSet<Listed>,
    /// The listing that failed. It is asked again when the diff changes or Review opens.
    failed: Option<Listed>,
}

impl CommentSync {
    /// Starts a request and returns its number.
    fn begin(&mut self, wanted: Listed) -> u64 {
        let listing = self.tasks.entry(wanted.0).or_default();

        listing.next += 1;
        listing.running.insert(listing.next, Vec::new());
        self.asking.insert(wanted);

        listing.next
    }

    fn record(&mut self, event: &CommentEvent) {
        let task = match event {
            CommentEvent::Changed(comment) => comment.task,
            CommentEvent::Deleted { task, .. } => *task,
        };

        if let Some(listing) = self.tasks.get_mut(&task) {
            for events in listing.running.values_mut() {
                events.push(event.clone());
            }
        }
    }

    /// The list to keep for an answer: the events that came since its request began are
    /// applied to it. `None` when a newer request already answered.
    fn answered(
        &mut self,
        wanted: Listed,
        number: u64,
        mut list: Vec<ReviewComment>,
    ) -> Option<Vec<ReviewComment>> {
        self.asking.remove(&wanted);
        let listing = self.tasks.get_mut(&wanted.0)?;
        let events = listing.running.remove(&number).unwrap_or_default();

        if number < listing.applied {
            return None;
        }
        listing.applied = number;

        for event in &events {
            apply_comment_event(&mut list, event);
        }

        Some(list)
    }

    fn failed(&mut self, wanted: Listed, number: u64) {
        self.asking.remove(&wanted);
        self.failed = Some(wanted);

        if let Some(listing) = self.tasks.get_mut(&wanted.0) {
            listing.running.remove(&number);
        }
    }

    #[cfg(test)]
    pub(crate) fn has_failed(&self) -> bool {
        self.failed.is_some()
    }

    #[cfg(test)]
    pub(crate) fn is_asking(&self) -> bool {
        !self.asking.is_empty()
    }

    /// Lets every listing be asked again.
    pub(crate) fn forget(&mut self) {
        self.asking.clear();
        self.failed = None;
    }
}

/// The daemon's own words for a refusal, cut to one short line.
pub(crate) fn refusal(error: &ClientError) -> SharedString {
    match error {
        ClientError::Daemon(message) => short(message),
        other => short(&other.to_string()),
    }
}

type Call = Box<dyn FnOnce(&Client) -> Result<(), ClientError> + Send>;

impl Workspace {
    /// Keeps a comment the daemon reports.
    pub(crate) fn comment_event(&mut self, event: &CommentEvent, cx: &mut Context<Self>) {
        self.comment_sync.record(event);
        self.data.apply_comment_event(event);
        cx.notify();
    }

    /// Lets Review ask for the comments again, as when it opens or a connection starts.
    pub(crate) fn forget_comment_listing(&mut self) {
        self.comments_listed = None;
        self.comment_sync.forget();
    }

    /// Lists the task's comments again when Review opened, or its diff changed: the
    /// daemon checks the anchors against the files as they are now.
    pub(crate) fn list_comments_if_needed(&mut self, wanted: Listed, cx: &mut Context<Self>) {
        if self.comments_listed == Some(wanted)
            || self.comment_sync.failed == Some(wanted)
            || self.comment_sync.asking.contains(&wanted)
        {
            return;
        }
        let Some(client) = self.daemon.clone() else {
            return;
        };

        let task = wanted.0;
        let number = self.comment_sync.begin(wanted);
        let listing = cx.background_spawn(async move { client.list_comments(task) });

        cx.spawn(async move |this, cx| {
            let result = listing.await;
            let updated = this.update(cx, |workspace, cx| {
                workspace.listed_comments(wanted, number, result, cx);
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the comments came: {error}");
            }
        })
        .detach();
    }

    fn listed_comments(
        &mut self,
        wanted: Listed,
        number: u64,
        result: Result<Vec<ReviewComment>, ClientError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(list) => {
                if let Some(list) = self.comment_sync.answered(wanted, number, list) {
                    self.data.set_comments(wanted.0, list);
                    self.comments_listed = Some(wanted);
                }
            }
            Err(error) => {
                log::debug!("cannot list the comments: {error}");
                self.comment_sync.failed(wanted, number);
            }
        }

        cx.notify();
    }

    /// The worker that owns the file a comment is on.
    fn comment_owner(&self, comment: &ReviewComment) -> Option<DaemonAgentId> {
        self.data
            .diffs
            .get(&(comment.task, comment.source))?
            .files
            .iter()
            .find(|file| {
                file.worktree == comment.anchor.worktree && file.path == comment.anchor.path
            })
            .and_then(|file| file.agent)
    }

    /// Adds a comment or a reply.
    pub(crate) fn add_review_comment(
        &mut self,
        comment: NewComment,
        slot: CommentSlot,
        cx: &mut Context<Self>,
    ) {
        let task = comment.task;
        let probe = ReviewComment {
            id: CommentId(0),
            task,
            parent: comment.parent,
            source: comment.source,
            anchor: comment.anchor.clone(),
            body: String::new(),
            state: swarm_protocol::CommentState::Pending,
            to: None,
            outdated: false,
            created_at: 0,
            sent_at: None,
        };
        let owner = self.comment_owner(&probe);
        let now = self.now_ms();
        let new = comment.clone();

        self.comment_call(
            slot,
            Box::new(move |client| client.add_comment(comment)),
            move |data| {
                let mut list = data.task_list(task).to_vec();

                fake_add(&mut list, new, owner, now);
                data.set_comments(task, list);
            },
            cx,
        );
    }

    /// Deletes a pending comment and its replies.
    pub(crate) fn delete_review_comment(
        &mut self,
        task: DaemonTaskId,
        id: CommentId,
        cx: &mut Context<Self>,
    ) {
        self.comment_call(
            CommentSlot::Card(id),
            Box::new(move |client| client.delete_comment(id)),
            move |data| {
                let mut list = data.task_list(task).to_vec();

                fake_delete(&mut list, id);
                data.set_comments(task, list);
            },
            cx,
        );
    }

    /// Resolves a sent thread, or opens it again.
    pub(crate) fn resolve_review_comment(
        &mut self,
        task: DaemonTaskId,
        id: CommentId,
        resolved: bool,
        cx: &mut Context<Self>,
    ) {
        self.comment_call(
            CommentSlot::Card(id),
            Box::new(move |client| client.resolve_comment(id, resolved)),
            move |data| {
                let mut list = data.task_list(task).to_vec();

                fake_resolve(&mut list, id, resolved);
                data.set_comments(task, list);
            },
            cx,
        );
    }

    /// Runs one comment call on the daemon, or on the sample data without one. The
    /// answer reaches the view later, never inside the view's own update.
    fn comment_call(
        &mut self,
        slot: CommentSlot,
        call: Call,
        sample: impl FnOnce(&mut crate::model::FakeData),
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.daemon.clone() else {
            let result = if self.source == DataSource::Sample {
                sample(&mut self.data);
                Ok(())
            } else {
                Err("Not connected".into())
            };

            Self::report_comment_later(slot, result, cx);
            cx.notify();
            return;
        };

        let running = cx.background_spawn(async move { call(&client) });

        cx.spawn(async move |this, cx| {
            let result = running.await.map_err(|error| {
                log::warn!("comment call failed: {error}");
                short(&error.to_string())
            });
            let updated = this.update(cx, |workspace, cx| {
                workspace.report_comment(slot, result, cx);
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the comment answered: {error}");
            }
        })
        .detach();
    }

    fn report_comment_later(
        slot: CommentSlot,
        result: Result<(), SharedString>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let updated = this.update(cx, |workspace, cx| {
                workspace.report_comment(slot, result, cx);
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the comment answered: {error}");
            }
        })
        .detach();
    }

    fn report_comment(
        &mut self,
        slot: CommentSlot,
        result: Result<(), SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.review
            .update(cx, |review, cx| review.comment_result(slot, result, cx));
    }

    /// Sends the task's pending comments to its orchestrator in one message.
    pub(crate) fn request_review_changes(&mut self, cx: &mut Context<Self>) {
        let task = self.active_task;
        let Some(id) = self.data.diff_id(task) else {
            return;
        };
        let pending = count_comments(&self.data.task_comments(task)).pending;

        if pending == 0 || self.review_requesting.contains(&task) {
            return;
        }

        self.review_error = None;

        let Some(client) = self.daemon.clone() else {
            if self.source == DataSource::Sample {
                let now = self.now_ms();
                let mut list = self.data.task_list(id).to_vec();

                fake_send(&mut list, |comment| self.comment_owner(comment), now);
                self.data.set_comments(id, list);
            } else {
                self.review_error = Some((task, "Not connected".into()));
            }

            cx.notify();
            return;
        };

        self.review_requesting.insert(task);
        let sending = cx.background_spawn(async move { client.request_changes(id) });

        cx.spawn(async move |this, cx| {
            let result = sending.await;

            let updated = this.update(cx, |workspace, cx| {
                workspace.finish_request(task, result, cx);
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the request answered: {error}");
            }
        })
        .detach();
    }

    fn finish_request(
        &mut self,
        task: TaskId,
        result: Result<(), ClientError>,
        cx: &mut Context<Self>,
    ) {
        self.review_requesting.remove(&task);

        if let Err(error) = result {
            log::warn!("cannot request changes: {error}");
            self.review_error = Some((task, refusal(&error)));
        }

        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use swarm_protocol::{CommentAnchor, CommentState, DiffSide};

    use super::*;

    fn comment(id: u64, body: &str) -> ReviewComment {
        ReviewComment {
            id: CommentId(id),
            task: DaemonTaskId(1),
            parent: None,
            source: ReviewSource::Live,
            anchor: CommentAnchor {
                worktree: PathBuf::from("/wt"),
                path: PathBuf::from("a.rs"),
                side: DiffSide::New,
                line: 1,
                text: String::new(),
                before: None,
                after: None,
            },
            body: body.to_owned(),
            state: CommentState::Pending,
            to: None,
            outdated: false,
            created_at: 0,
            sent_at: None,
        }
    }

    fn changed(id: u64, body: &str) -> CommentEvent {
        CommentEvent::Changed(Box::new(comment(id, body)))
    }

    fn wanted(fingerprint: u64) -> Listed {
        (DaemonTaskId(1), ReviewSource::Live, Some(fingerprint))
    }

    fn bodies(list: &[ReviewComment]) -> Vec<(u64, &str)> {
        list.iter()
            .map(|comment| (comment.id.0, comment.body.as_str()))
            .collect()
    }

    #[test]
    fn an_answer_keeps_what_events_changed_while_it_was_on_its_way() {
        let mut sync = CommentSync::default();
        let number = sync.begin(wanted(1));

        sync.record(&changed(1, "newer"));
        sync.record(&changed(2, "added"));
        sync.record(&CommentEvent::Deleted {
            task: DaemonTaskId(1),
            id: CommentId(3),
        });
        let list = vec![comment(1, "older"), comment(3, "gone")];

        let kept = sync.answered(wanted(1), number, list);

        assert_eq!(
            kept.as_deref().map(bodies),
            Some(vec![(1, "newer"), (2, "added")])
        );
    }

    #[test]
    fn an_event_before_the_request_does_not_touch_its_answer() {
        let mut sync = CommentSync::default();

        sync.record(&changed(1, "from an event"));
        let number = sync.begin(wanted(1));
        let kept = sync.answered(wanted(1), number, vec![comment(1, "from the list")]);

        assert_eq!(
            kept.as_deref().map(bodies),
            Some(vec![(1, "from the list")])
        );
    }

    #[test]
    fn events_of_another_task_are_left_out() {
        let mut sync = CommentSync::default();
        let number = sync.begin(wanted(1));
        let mut other = comment(9, "other task");

        other.task = DaemonTaskId(2);
        sync.record(&CommentEvent::Changed(Box::new(other)));
        let kept = sync.answered(wanted(1), number, vec![comment(1, "mine")]);

        assert_eq!(kept.as_deref().map(bodies), Some(vec![(1, "mine")]));
    }

    #[test]
    fn an_older_answer_after_a_newer_one_is_dropped() {
        let mut sync = CommentSync::default();
        let old = sync.begin(wanted(1));
        let new = sync.begin(wanted(2));

        let newer = sync.answered(wanted(2), new, vec![comment(1, "new")]);
        let older = sync.answered(wanted(1), old, vec![comment(1, "old")]);

        assert!(newer.is_some());
        assert!(older.is_none());
    }

    #[test]
    fn a_failed_listing_waits_for_a_new_diff_or_a_new_open() {
        let mut sync = CommentSync::default();
        let number = sync.begin(wanted(1));

        assert!(sync.asking.contains(&wanted(1)));
        sync.failed(wanted(1), number);

        assert!(sync.asking.is_empty());
        assert_eq!(sync.failed, Some(wanted(1)));

        sync.forget();

        assert_eq!(sync.failed, None);
    }

    #[test]
    fn a_refusal_shows_the_daemons_words_without_the_prefix() {
        let refused = ClientError::Daemon("the orchestrator is stopped".into());
        let lost = ClientError::Disconnected;

        assert_eq!(refusal(&refused), "the orchestrator is stopped");
        assert_eq!(refusal(&lost), "the daemon connection is closed");
        assert!(
            refusal(&ClientError::Daemon("x".repeat(200)))
                .chars()
                .count()
                <= 81
        );
    }
}
