//! Review comments: stored by `swarm_core`, sent to the task orchestrator and announced
//! to every client.

use anyhow::Result;
use swarm_core::comments::Written;
use swarm_protocol::{AgentRole, CommentId, Event, NewComment, TaskId};

use crate::Services;
use crate::agents::lock;

/// Stores the comment. A comment that is not held back goes to the orchestrator now.
pub(crate) fn add(services: &Services, new: &NewComment) -> Result<()> {
    let written = services
        .projects
        .add_comment(new, &|written| announce(services, written))?;

    deliver(services, &written)
}

/// Sends every pending comment of the task. A `Resumable` orchestrator resumes first;
/// a refused resume fails the call and the comments stay pending.
pub(crate) fn send_pending(services: &Services, task: TaskId) -> Result<()> {
    if !services.projects.pending_comments(task)?.is_empty() {
        let orchestrator = lock(&services.store)
            .list_agents(task)?
            .into_iter()
            .find(|agent| agent.role == AgentRole::Orchestrator);

        if let Some(orchestrator) = orchestrator {
            services.agents.resume_for_message(orchestrator.id)?;
        }
    }

    let written = services
        .projects
        .send_comments(task, &|written| announce(services, written))?;

    deliver(services, &written)
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

/// Wakes the orchestrator if it waits, and pastes the messages if it is idle. A
/// `Resumable` orchestrator resumes first; a refused resume is the error.
fn deliver(services: &Services, written: &Written) -> Result<()> {
    let Some(first) = written.messages.first() else {
        return Ok(());
    };

    services.agents.resume_for_message(first.to)?;
    services.messages.wake();
    services.agents.deliver_unread(first.to);

    Ok(())
}
