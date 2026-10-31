//! Review comments: stored by `swarm_core`, sent to the task orchestrator as one
//! message, and announced to every client.

use anyhow::{Result, bail};
use swarm_core::comments::Outgoing;
use swarm_core::store::NewMessage;
use swarm_protocol::{CommentId, Event, MessageKind, NewComment, TaskId};

use crate::Services;

/// Stores the comment. A comment that is not held back goes to the orchestrator now.
pub(crate) fn add(services: &Services, new: &NewComment) -> Result<()> {
    let projects = &services.projects;
    let _guard = projects.comment_guard();

    let added = projects.add_comment(new)?;
    if !added.send_now {
        services.clients.broadcast(&Event::CommentChanged {
            comment: added.comment,
        });

        return Ok(());
    }

    let id = added.comment.id;
    let posted = projects
        .outgoing(new.task, &[id], false)
        .map_err(anyhow::Error::from)
        .and_then(|outgoing| post(services, &outgoing).map(|()| outgoing));

    match posted {
        Ok(outgoing) => finish(services, &outgoing),
        Err(error) => {
            if let Err(discard) = projects.discard_comment(id) {
                log::error!("cannot drop comment {}: {discard}", id.0);
            }

            Err(error)
        }
    }
}

/// Sends every pending comment of the task in one message.
pub(crate) fn send_pending(services: &Services, task: TaskId) -> Result<()> {
    let projects = &services.projects;
    let _guard = projects.comment_guard();

    let pending = projects.pending_comments(task)?;
    if pending.is_empty() {
        bail!("no comments are waiting");
    }

    let outgoing = projects.outgoing(task, &pending, true)?;
    post(services, &outgoing)?;

    finish(services, &outgoing)
}

pub(crate) fn delete(services: &Services, id: CommentId) -> Result<()> {
    let _guard = services.projects.comment_guard();

    let (task, deleted) = services.projects.delete_comment(id)?;
    for id in deleted {
        services
            .clients
            .broadcast(&Event::CommentDeleted { task, id });
    }

    Ok(())
}

pub(crate) fn resolve(services: &Services, id: CommentId, resolved: bool) -> Result<()> {
    let _guard = services.projects.comment_guard();

    let comment = services.projects.resolve_comment(id, resolved)?;
    services
        .clients
        .broadcast(&Event::CommentChanged { comment });

    Ok(())
}

fn post(services: &Services, outgoing: &Outgoing) -> Result<()> {
    services.messages.post(&NewMessage {
        project: outgoing.project,
        task: Some(outgoing.task),
        from: None,
        to: outgoing.orchestrator,
        kind: MessageKind::Comment,
        text: &outgoing.text,
        delivered: false,
    })?;

    Ok(())
}

/// The message is posted: mark the comments sent, tell the clients and nudge the
/// orchestrator.
fn finish(services: &Services, outgoing: &Outgoing) -> Result<()> {
    let sent = services.projects.mark_comments_sent(&outgoing.owners)?;
    services.agents.deliver_unread(outgoing.orchestrator);

    for comment in sent {
        services
            .clients
            .broadcast(&Event::CommentChanged { comment });
    }

    Ok(())
}
