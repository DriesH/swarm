//! Review comments: stored by `swarm_core`, sent to the task orchestrator and announced
//! to every client.

use anyhow::Result;
use swarm_core::comments::Written;
use swarm_protocol::{CommentId, Event, NewComment, TaskId};

use crate::Services;

/// Stores the comment. A comment that is not held back goes to the orchestrator now.
pub(crate) fn add(services: &Services, new: &NewComment) -> Result<()> {
    let written = services
        .projects
        .add_comment(new, &|written| announce(services, written))?;

    deliver(services, &written);

    Ok(())
}

/// Sends every pending comment of the task.
pub(crate) fn send_pending(services: &Services, task: TaskId) -> Result<()> {
    let written = services
        .projects
        .send_comments(task, &|written| announce(services, written))?;

    deliver(services, &written);

    Ok(())
}

pub(crate) fn delete(services: &Services, id: CommentId) -> Result<()> {
    services.projects.delete_comment(id, |task, deleted| {
        for id in deleted {
            services
                .clients
                .broadcast(&Event::CommentDeleted { task, id: *id });
        }
    })?;

    Ok(())
}

pub(crate) fn resolve(services: &Services, id: CommentId, resolved: bool) -> Result<()> {
    services.projects.resolve_comment(id, resolved, |comment| {
        services.clients.broadcast(&Event::CommentChanged {
            comment: comment.clone(),
        });
    })?;

    Ok(())
}

/// The messages first, so no client sees a comment sent before its message.
fn announce(services: &Services, written: &Written) {
    for message in &written.messages {
        services.clients.broadcast(&Event::MessageSent {
            message: message.clone(),
        });
    }

    for comment in &written.comments {
        services.clients.broadcast(&Event::CommentChanged {
            comment: comment.clone(),
        });
    }
}

/// Wakes the orchestrator if it waits, and pastes the messages if it is idle.
fn deliver(services: &Services, written: &Written) {
    let Some(first) = written.messages.first() else {
        return;
    };

    services.messages.wake();
    services.agents.deliver_unread(first.to);
}
