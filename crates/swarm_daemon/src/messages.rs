//! Messages between agents: stored in SQLite, announced to the app, and waited for.

use std::collections::{HashMap, HashSet};
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

    /// Gives the messages back and wakes the `wait` calls that could not take them while
    /// the claim held them.
    pub(crate) fn release(self) {
        let owner = Arc::clone(&self.owner);
        drop(self);

        owner.wake();
    }

    /// The messages reached the agent: mark them delivered.
    pub(crate) fn commit(self) {
        let ids = self.ids();

        // Broadcasting under the store lock keeps "sent" ahead of "delivered".
        let store = lock(&self.owner.store);
        match store.mark_delivered(&ids) {
            Ok(delivery) if !delivery.ids.is_empty() => {
                let comments = store.comments_in_messages(&delivery.ids);

                self.owner.clients.broadcast(&Event::MessagesDelivered {
                    ids: delivery.ids,
                    delivered_at: delivery.delivered_at,
                });

                match comments {
                    Ok(comments) => {
                        for comment in comments {
                            self.owner
                                .clients
                                .broadcast(&Event::CommentChanged { comment });
                        }
                    }
                    Err(error) => log::error!("cannot read the delivered comments: {error}"),
                }
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
    waits: Mutex<Waits>,
}

/// What the `wait` calls and the pastes of each agent need to agree on, behind one lock.
#[derive(Default)]
struct Waits {
    /// How many `wait` calls each agent has open.
    open: HashMap<AgentId, usize>,
    /// Agents with a paste in their input queue that the next `wait` must not outlast.
    queued: HashSet<AgentId>,
}

/// Counts an open `wait` call until it drops.
struct Waiter<'a> {
    owner: &'a Messages,
    agent: AgentId,
}

impl Drop for Waiter<'_> {
    fn drop(&mut self) {
        let mut waits = lock(&self.owner.waits);

        if let Some(count) = waits.open.get_mut(&self.agent) {
            *count -= 1;
            if *count == 0 {
                waits.open.remove(&self.agent);
            }
        }
    }
}

impl Messages {
    pub(crate) fn new(store: Arc<Mutex<Store>>, clients: Arc<Clients>) -> Arc<Self> {
        Arc::new(Self {
            store,
            clients,
            wake: Mutex::new(()),
            arrived: Condvar::new(),
            claimed: Mutex::default(),
            waits: Mutex::default(),
        })
    }

    /// The agent sits in a `wait` call, which takes its next message the moment it
    /// arrives.
    pub(crate) fn is_waiting(&self, agent: AgentId) -> bool {
        lock(&self.waits).open.contains_key(&agent)
    }

    /// A paste for the agent is about to go out. False when the agent sits in a `wait`:
    /// the wait takes the messages itself.
    ///
    /// With `queued`, the paste lands in the input queue of a working agent. The agent
    /// only reads it after its current tool call ends, so its next `wait` ends at once
    /// (see [`Self::wait`]). This and the start of a `wait` share one lock: either the
    /// wait starts first and takes the messages, or it sees the mark.
    pub(crate) fn begin_paste(&self, agent: AgentId, queued: bool) -> bool {
        let mut waits = lock(&self.waits);
        if waits.open.contains_key(&agent) {
            return false;
        }

        if queued {
            waits.queued.insert(agent);
        }

        true
    }

    /// The agent reads its input again: nothing waits in its queue anymore.
    pub(crate) fn clear_queued(&self, agent: AgentId) {
        lock(&self.waits).queued.remove(&agent);
    }

    /// Stores the message, tells the app and wakes the recipient if it waits.
    pub(crate) fn post(&self, message: &NewMessage<'_>) -> Result<MessageInfo> {
        let store = lock(&self.store);
        let info = store.add_message(message)?;

        // Under the store lock, so a claim cannot announce "delivered" first.
        self.clients.broadcast(&Event::MessageSent {
            message: info.clone(),
        });
        drop(store);

        self.wake();

        Ok(info)
    }

    /// Wakes the recipients that wait.
    pub(crate) fn wake(&self) {
        drop(lock(&self.wake));
        self.arrived.notify_all();
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

    /// Counts a new `wait` call of the agent. The flag is true when a paste waits in the
    /// agent's input queue; it is taken.
    fn open_wait(&self, agent: AgentId) -> (Waiter<'_>, bool) {
        let mut waits = lock(&self.waits);
        *waits.open.entry(agent).or_default() += 1;
        let queued = waits.queued.remove(&agent);

        (Waiter { owner: self, agent }, queued)
    }

    /// Waits until the agent has an unread message, `timeout` passes or `gone` says the
    /// caller hung up, then claims the messages. A caller that hung up gets none.
    ///
    /// A paste in the agent's input queue (see [`Self::begin_paste`]) ends the first wait
    /// after it at once, once: the agent reads the paste when this call ends.
    pub(crate) fn wait(
        self: &Arc<Self>,
        agent: AgentId,
        timeout: Duration,
        gone: &dyn Fn() -> bool,
    ) -> Result<Claim> {
        let (_waiter, queued) = self.open_wait(agent);

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
            if !claim.is_empty() || queued {
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
    fn an_agent_in_a_wait_is_waiting_until_the_wait_ends() {
        let messages = messages();
        let peer = Arc::clone(&messages);
        let ended_empty = thread::spawn(move || {
            peer.wait(AgentId(1), Duration::from_millis(400), &|| false)
                .expect("wait")
                .is_empty()
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        while !messages.is_waiting(AgentId(1)) {
            assert!(Instant::now() < deadline, "the wait never started");
            thread::park_timeout(Duration::from_millis(10));
        }
        assert!(!messages.is_waiting(AgentId(2)));

        assert!(ended_empty.join().expect("join"));
        assert!(!messages.is_waiting(AgentId(1)));
    }

    #[test]
    fn a_paste_in_the_queue_ends_the_next_wait_at_once_and_only_that_one() {
        let messages = messages();
        assert!(messages.begin_paste(AgentId(1), true));

        let start = Instant::now();
        let first = messages
            .wait(AgentId(1), Duration::from_secs(30), &|| false)
            .expect("wait");
        assert!(first.is_empty());
        assert!(start.elapsed() < Duration::from_secs(5));

        let start = Instant::now();
        messages
            .wait(AgentId(1), Duration::from_millis(80), &|| false)
            .expect("wait");
        assert!(start.elapsed() >= Duration::from_millis(80));
    }

    #[test]
    fn a_queue_mark_belongs_to_its_agent() {
        let messages = messages();
        assert!(messages.begin_paste(AgentId(2), true));

        let start = Instant::now();
        messages
            .wait(AgentId(1), Duration::from_millis(80), &|| false)
            .expect("wait");

        assert!(start.elapsed() >= Duration::from_millis(80));
    }

    #[test]
    fn a_paste_that_is_not_queued_leaves_the_wait_alone() {
        let messages = messages();
        assert!(messages.begin_paste(AgentId(1), false));

        let start = Instant::now();
        messages
            .wait(AgentId(1), Duration::from_millis(80), &|| false)
            .expect("wait");

        assert!(start.elapsed() >= Duration::from_millis(80));
    }

    #[test]
    fn a_cleared_queue_mark_does_not_end_a_wait() {
        let messages = messages();
        assert!(messages.begin_paste(AgentId(1), true));
        messages.clear_queued(AgentId(1));

        let start = Instant::now();
        messages
            .wait(AgentId(1), Duration::from_millis(80), &|| false)
            .expect("wait");

        assert!(start.elapsed() >= Duration::from_millis(80));
    }

    #[test]
    fn a_wait_that_ends_at_once_still_returns_unread_messages() {
        let messages = messages();
        messages.post(&new_message(1, "unread")).expect("post");
        assert!(messages.begin_paste(AgentId(1), true));

        let taken = messages
            .wait(AgentId(1), Duration::from_secs(30), &|| false)
            .expect("wait");

        assert_eq!(taken.messages().len(), 1);
    }

    #[test]
    fn no_paste_goes_out_while_the_agent_sits_in_a_wait() {
        let messages = messages();
        let peer = Arc::clone(&messages);
        let waiting = thread::spawn(move || {
            peer.wait(AgentId(1), Duration::from_millis(400), &|| false)
                .expect("wait")
                .is_empty()
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        while !messages.is_waiting(AgentId(1)) {
            assert!(Instant::now() < deadline, "the wait never started");
            thread::park_timeout(Duration::from_millis(10));
        }

        assert!(!messages.begin_paste(AgentId(1), true));
        assert!(!messages.begin_paste(AgentId(1), false));
        assert!(messages.begin_paste(AgentId(2), true));
        assert!(waiting.join().expect("join"));

        let start = Instant::now();
        messages
            .wait(AgentId(1), Duration::from_millis(80), &|| false)
            .expect("wait");
        assert!(
            start.elapsed() >= Duration::from_millis(80),
            "a refused paste left a mark"
        );
    }

    #[test]
    fn a_released_claim_wakes_the_wait_that_could_not_take_it() {
        let messages = messages();
        messages.post(&new_message(1, "held")).expect("post");
        let claim = messages.claim(AgentId(1)).expect("claim");

        let peer = Arc::clone(&messages);
        let start = Instant::now();
        let waiting = thread::spawn(move || {
            peer.wait(AgentId(1), Duration::from_secs(30), &|| false)
                .expect("wait")
                .messages()
                .len()
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !messages.is_waiting(AgentId(1)) {
            assert!(Instant::now() < deadline, "the wait never started");
            thread::park_timeout(Duration::from_millis(10));
        }
        thread::park_timeout(Duration::from_millis(100));

        claim.release();

        assert_eq!(waiting.join().expect("join"), 1);
        assert!(start.elapsed() < PEER_CHECK, "the wait slept on");
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

    /// The store stays locked while a broadcast waits, so no other thread can announce
    /// "delivered" ahead of "sent".
    fn store_is_locked_while_broadcast_stalls(
        action: impl FnOnce(&Arc<Messages>) + Send + 'static,
    ) {
        let store = Store::open_in_memory().expect("store");
        store
            .add_project(std::path::Path::new("/repo"), "repo")
            .expect("project");
        let clients = Arc::new(Clients::default());
        let messages = Messages::new(Arc::new(Mutex::new(store)), Arc::clone(&clients));

        let stalled = clients.stall();
        let worker = Arc::clone(&messages);
        let running = thread::spawn(move || action(&worker));
        thread::park_timeout(Duration::from_millis(200));

        assert!(
            messages.store.try_lock().is_err(),
            "the store lock was released early"
        );

        drop(stalled);
        running.join().expect("action");
    }

    #[test]
    fn a_post_broadcasts_under_the_store_lock() {
        store_is_locked_while_broadcast_stalls(|messages| {
            messages.post(&new_message(1, "x")).expect("post");
        });
    }

    #[test]
    fn a_commit_broadcasts_under_the_store_lock() {
        let store = Store::open_in_memory().expect("store");
        store
            .add_project(std::path::Path::new("/repo"), "repo")
            .expect("project");
        let clients = Arc::new(Clients::default());
        let messages = Messages::new(Arc::new(Mutex::new(store)), Arc::clone(&clients));
        messages.post(&new_message(1, "x")).expect("post");
        let claim = messages.claim(AgentId(1)).expect("claim");

        let stalled = clients.stall();
        let running = thread::spawn(move || claim.commit());
        thread::park_timeout(Duration::from_millis(200));

        assert!(
            messages.store.try_lock().is_err(),
            "the store lock was released early"
        );

        drop(stalled);
        running.join().expect("commit");
    }
}
