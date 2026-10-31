//! Messages between agents: stored in SQLite, announced to the app, and waited for.

use std::collections::HashSet;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::Result;
use swarm_core::store::{NewMessage, Store};
use swarm_protocol::agent::InboxMessage;
use swarm_protocol::{AgentId, Event, MessageId, MessageInfo};

use crate::hub::Clients;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How often a waiting call looks at its peer.
const PEER_CHECK: Duration = Duration::from_secs(1);

/// Unread messages that someone is about to deliver. They count as delivered only after
/// [`Claim::commit`]; a claim that drops without it gives them back.
pub(crate) struct Claim {
    messages: Vec<InboxMessage>,
    owner: Arc<Messages>,
}

impl Claim {
    pub(crate) fn messages(&self) -> &[InboxMessage] {
        &self.messages
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// The messages reached the agent: mark them delivered.
    pub(crate) fn commit(self) {
        let ids = self.ids();

        let delivery = lock(&self.owner.store).mark_delivered(&ids);
        match delivery {
            Ok(delivery) if !delivery.ids.is_empty() => {
                self.owner.clients.broadcast(&Event::MessagesDelivered {
                    ids: delivery.ids,
                    delivered_at: delivery.delivered_at,
                });
            }
            Ok(_) => {}
            Err(error) => log::error!("cannot mark messages as delivered: {error}"),
        }
    }

    fn ids(&self) -> Vec<MessageId> {
        self.messages.iter().map(|message| message.id).collect()
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        let mut claimed = lock(&self.owner.claimed);
        for message in &self.messages {
            claimed.remove(&message.id);
        }
    }
}

pub(crate) struct Messages {
    store: Arc<Mutex<Store>>,
    clients: Arc<Clients>,
    /// Held while a waiter checks for messages, so a message cannot slip in between the
    /// check and the sleep.
    wake: Mutex<()>,
    arrived: Condvar,
    /// Messages a [`Claim`] holds right now.
    claimed: Mutex<HashSet<MessageId>>,
}

impl Messages {
    pub(crate) fn new(store: Arc<Mutex<Store>>, clients: Arc<Clients>) -> Arc<Self> {
        Arc::new(Self {
            store,
            clients,
            wake: Mutex::new(()),
            arrived: Condvar::new(),
            claimed: Mutex::default(),
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

    /// Claims the agent's unread messages that nobody else holds.
    pub(crate) fn claim(self: &Arc<Self>, agent: AgentId) -> Result<Claim> {
        let unread = lock(&self.store).unread(agent)?;

        let mut claimed = lock(&self.claimed);
        let messages: Vec<InboxMessage> = unread
            .into_iter()
            .filter(|message| claimed.insert(message.id))
            .collect();

        Ok(Claim {
            messages,
            owner: Arc::clone(self),
        })
    }

    /// Waits until the agent has an unread message, `timeout` passes or `gone` says the
    /// caller hung up, then claims the messages. A caller that hung up gets none.
    pub(crate) fn wait(
        self: &Arc<Self>,
        agent: AgentId,
        timeout: Duration,
        gone: &dyn Fn() -> bool,
    ) -> Result<Claim> {
        let deadline = Instant::now() + timeout;
        let mut guard = lock(&self.wake);

        loop {
            if gone() {
                return Ok(Claim {
                    messages: Vec::new(),
                    owner: Arc::clone(self),
                });
            }

            let claim = self.claim(agent)?;
            if !claim.is_empty() {
                return Ok(claim);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(claim);
            }

            guard = self
                .arrived
                .wait_timeout(guard, remaining.min(PEER_CHECK))
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
            .wait(AgentId(1), Duration::from_millis(80), &|| false)
            .expect("wait");

        assert!(taken.is_empty());
        assert!(start.elapsed() >= Duration::from_millis(80));
    }

    #[test]
    fn a_wait_returns_at_once_when_a_message_is_waiting() {
        let messages = messages();
        messages.post(&new_message(1, "ready")).expect("post");

        let taken = messages
            .wait(AgentId(1), Duration::from_secs(30), &|| false)
            .expect("wait");

        assert_eq!(taken.messages().len(), 1);
        assert_eq!(taken.messages()[0].text, "ready");
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
            .wait(AgentId(1), Duration::from_secs(30), &|| false)
            .expect("wait");
        poster.join().expect("join");

        assert_eq!(taken.messages()[0].text, "late");
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_message_for_another_agent_does_not_end_a_wait() {
        let messages = messages();
        messages.post(&new_message(2, "not yours")).expect("post");

        let taken = messages
            .wait(AgentId(1), Duration::from_millis(50), &|| false)
            .expect("wait");

        assert!(taken.is_empty());
        assert!(!messages.claim(AgentId(2)).expect("claim").is_empty());
    }

    #[test]
    fn a_claim_that_is_dropped_gives_the_messages_back() {
        let messages = messages();
        messages.post(&new_message(1, "keep")).expect("post");

        let claim = messages.claim(AgentId(1)).expect("claim");
        assert_eq!(claim.messages().len(), 1);
        assert!(messages.claim(AgentId(1)).expect("again").is_empty());
        drop(claim);

        assert_eq!(
            messages.claim(AgentId(1)).expect("later").messages().len(),
            1
        );
    }

    #[test]
    fn a_committed_claim_is_delivered_for_good() {
        let messages = messages();
        messages.post(&new_message(1, "once")).expect("post");

        messages.claim(AgentId(1)).expect("claim").commit();

        assert!(messages.claim(AgentId(1)).expect("later").is_empty());
    }

    #[test]
    fn a_wait_for_a_caller_that_hung_up_claims_nothing() {
        let messages = messages();
        messages.post(&new_message(1, "mine")).expect("post");

        let taken = messages
            .wait(AgentId(1), Duration::from_secs(30), &|| true)
            .expect("wait");

        assert!(taken.is_empty());
        assert_eq!(
            messages.claim(AgentId(1)).expect("claim").messages().len(),
            1
        );
    }

    #[test]
    fn a_wait_stops_when_the_caller_hangs_up() {
        let messages = messages();
        let hung_up = std::sync::atomic::AtomicBool::new(false);
        let start = Instant::now();

        let taken = thread::scope(|scope| {
            scope.spawn(|| {
                thread::park_timeout(Duration::from_millis(100));
                hung_up.store(true, std::sync::atomic::Ordering::Release);
            });

            messages.wait(AgentId(1), Duration::from_secs(30), &|| {
                hung_up.load(std::sync::atomic::Ordering::Acquire)
            })
        })
        .expect("wait");

        assert!(taken.is_empty());
        assert!(start.elapsed() < Duration::from_secs(5));
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
