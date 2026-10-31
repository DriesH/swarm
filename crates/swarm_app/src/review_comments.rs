//! Review comments: the workspace side. Lists, adds, deletes and resolves comments
//! through the daemon, and applies its comment events.

use gpui::{AppContext as _, Context, SharedString};
use swarm_protocol::{
    AgentId as DaemonAgentId, Client, ClientError, CommentEvent, CommentId, NewComment,
    ReviewComment, ReviewSource, TaskId as DaemonTaskId,
};

use crate::model::TaskId;
use crate::review_model::{count_comments, fake_add, fake_delete, fake_resolve, fake_send};
use crate::review_view::short;
use crate::workspace::{DataSource, Workspace};

/// What a daemon answer is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommentSlot {
    /// The box for a new comment.
    Composer,
    /// A thread: a reply, Delete, Resolve or Reopen.
    Card(CommentId),
}

/// The task, source and diff version the comments were listed for.
pub(crate) type Listed = (DaemonTaskId, ReviewSource, Option<u64>);

type Call = Box<dyn FnOnce(&Client) -> Result<(), ClientError> + Send>;

impl Workspace {
    /// Keeps a comment the daemon reports.
    pub(crate) fn comment_event(&mut self, event: &CommentEvent, cx: &mut Context<Self>) {
        self.data.apply_comment_event(event);
        cx.notify();
    }

    /// Lists the task's comments again when Review opened, or its diff changed: the
    /// daemon checks the anchors against the files as they are now.
    pub(crate) fn list_comments_if_needed(&mut self, wanted: Listed, cx: &mut Context<Self>) {
        if self.comments_listed == Some(wanted) {
            return;
        }
        let Some(client) = self.daemon.clone() else {
            return;
        };

        self.comments_listed = Some(wanted);
        let task = wanted.0;
        let listing = cx.background_spawn(async move { client.list_comments(task) });

        cx.spawn(async move |this, cx| match listing.await {
            Ok(list) => {
                let updated = this.update(cx, |workspace, cx| {
                    workspace.data.set_comments(task, list);
                    cx.notify();
                });

                if let Err(error) = updated {
                    log::debug!("the window closed before the comments came: {error}");
                }
            }
            Err(error) => log::debug!("cannot list the comments: {error}"),
        })
        .detach();
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

        if pending == 0 || self.review_requesting.is_some() {
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

        self.review_requesting = Some(task);
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
        self.review_requesting = None;

        if let Err(error) = result {
            log::warn!("cannot request changes: {error}");
            self.review_error = Some((task, short(&error.to_string())));
        }

        cx.notify();
    }
}
