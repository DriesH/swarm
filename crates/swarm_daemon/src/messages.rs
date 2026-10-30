//! Messages between agents: stored in SQLite, announced to the app, and waited for.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::Result;
use swarm_core::store::{NewMessage, Store};
use swarm_protocol::agent::InboxMessage;
use swarm_protocol::{AgentId, Event, MessageInfo};

use crate::hub::Clients;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) struct Messages {
    store: Arc<Mutex<Store>>,
    clients: Arc<Clients>,
    /// Held while a waiter checks for messages, so a message cannot slip in between the
    /// check and the sleep.
    wake: Mutex<()>,
    arrived: Condvar,
}

impl Messages {
    pub(crate) fn new(store: Arc<Mutex<Store>>, clients: Arc<Clients>) -> Arc<Self> {
        Arc::new(Self {
            store,
            clients,
            wake: Mutex::new(()),
            arrived: Condvar::new(),
        })
    }

    /// Stores the message, tells the app and wakes the recipient if it waits.
    pub(crate) fn post(&self, message: &NewMessage<'_>) -> Result<MessageInfo> {
        let info = lock(&self.store).add_message(message)?;

        self.clients.broadcast(&Event::MessageSent {
            message: info.clone(),
        });
        drop(lock(&self.wake));
        self.arrived.notify_all();

        Ok(info)
    }

    /// Takes the agent's unread messages and marks them delivered.
    pub(crate) fn take(&self, agent: AgentId) -> Result<Vec<InboxMessage>> {
        Ok(lock(&self.store).take_unread(agent)?)
    }

    pub(crate) fn has_unread(&self, agent: AgentId) -> bool {
        lock(&self.store).has_unread(agent).unwrap_or_else(|error| {
            log::error!("cannot check the messages of agent {}: {error}", agent.0);

            false
        })
    }

    /// Waits until the agent has an unread message or `timeout` passes, then takes them.
    pub(crate) fn wait(&self, agent: AgentId, timeout: Duration) -> Result<Vec<InboxMessage>> {
        let deadline = Instant::now() + timeout;
        let mut guard = lock(&self.wake);

        loop {
            let messages = self.take(agent)?;
            if !messages.is_empty() {
                return Ok(messages);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(messages);
            }

            guard = self
                .arrived
                .wait_timeout(guard, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use swarm_protocol::{MessageKind, ProjectId};

    use super::*;

    fn messages() -> Arc<Messages> {
        let store = Store::open_in_memory().expect("store");
        let project = store
            .add_project(std::path::Path::new("/repo"), "repo")
            .expect("project");
        assert_eq!(project.id, ProjectId(1));

        Messages::new(Arc::new(Mutex::new(store)), Arc::new(Clients::default()))
    }

    fn new_message(to: u64, text: &str) -> NewMessage<'_> {
        NewMessage {
            project: ProjectId(1),
            task: None,
            from: None,
            to: AgentId(to),
            kind: MessageKind::Report,
            text,
            delivered: false,
        }
    }

    #[test]
    fn a_wait_returns_empty_after_the_timeout() {
        let messages = messages();
        let start = Instant::now();

        let taken = messages
            .wait(AgentId(1), Duration::from_millis(80))
            .expect("wait");

        assert!(taken.is_empty());
        assert!(start.elapsed() >= Duration::from_millis(80));
    }

    #[test]
    fn a_wait_returns_at_once_when_a_message_is_waiting() {
        let messages = messages();
        messages.post(&new_message(1, "ready")).expect("post");

        let taken = messages
            .wait(AgentId(1), Duration::from_secs(30))
            .expect("wait");

        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].text, "ready");
    }

    #[test]
    fn a_wait_wakes_when_a_message_arrives() {
        let messages = messages();
        let sender = Arc::clone(&messages);
        let start = Instant::now();
        let poster = thread::spawn(move || {
            thread::park_timeout(Duration::from_millis(100));
            sender.post(&new_message(1, "late")).expect("post");
        });

        let taken = messages
            .wait(AgentId(1), Duration::from_secs(30))
            .expect("wait");
        poster.join().expect("join");

        assert_eq!(taken[0].text, "late");
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_message_for_another_agent_does_not_end_a_wait() {
        let messages = messages();
        messages.post(&new_message(2, "not yours")).expect("post");

        let taken = messages
            .wait(AgentId(1), Duration::from_millis(50))
            .expect("wait");

        assert!(taken.is_empty());
        assert!(messages.has_unread(AgentId(2)));
    }

    #[test]
    fn a_message_is_broadcast_to_the_app() {
        let store = Store::open_in_memory().expect("store");
        store
            .add_project(std::path::Path::new("/repo"), "repo")
            .expect("project");
        let clients = Arc::new(Clients::default());
        let (frames, receiver) = flume::bounded(8);
        clients.add(&Arc::new(crate::hub::ClientSink::new(1, frames, None)));
        let messages = Messages::new(Arc::new(Mutex::new(store)), clients);

        messages.post(&new_message(1, "hello")).expect("post");

        assert_eq!(receiver.try_iter().count(), 1);
    }
}
