//! Subscription usage: the numbers the daemon keeps, saves and broadcasts, and the
//! poller that fills them (docs/PLAN.md, "Usage and menu bar").
//!
//! Claude's windows come only from the status line of the Claude agents Swarm runs: with
//! none running, the last numbers stay. Codex is asked every few minutes. Polling runs
//! only while an app is connected.
//!
//! Claude reports no plan: it was read from the Keychain, which Swarm no longer touches.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use serde_json::Value;
use swarm_core::adapters;
use swarm_protocol::{AgentKind, Event, SubscriptionUsage, Usage, UsageProblem, UsageWindow};
use swarm_usage::{Outcome, codex_usage, length_of_label, write_private};

use crate::agents::lock;
use crate::hub::Clients;

/// How often the poller looks at the clients and the schedule.
const POLL_TICK: Duration = Duration::from_secs(5);

/// How often Codex is asked.
const CODEX_INTERVAL: Duration = Duration::from_mins(5);

const CODEX_TIMEOUT: Duration = Duration::from_secs(10);

/// Reset times this close, in seconds, belong to one window. The same window can report
/// a reset time that moves by a few seconds.
const RESET_DRIFT: u64 = 5 * 60;

/// An agent sends two windows; this leaves room and stops a bad caller.
const MAX_WINDOWS: usize = 8;
const MAX_LABEL_CHARS: usize = 16;

/// The file an older daemon kept the Claude endpoint backoff in.
const OLD_BACKOFF_FILE: &str = "claude-usage-backoff.json";

/// Commands and timings that tests replace. Empty in production.
#[derive(Default)]
pub(crate) struct Overrides {
    /// No poller at all.
    pub(crate) no_polling: bool,
    pub(crate) codex: Option<PathBuf>,
    pub(crate) tick: Option<Duration>,
    pub(crate) codex_interval: Option<Duration>,
}

pub(crate) struct UsageService {
    usage: Mutex<Usage>,
    file: PathBuf,
    clients: Arc<Clients>,
    /// Wakes the poller when the first app connects.
    wake: flume::Sender<()>,
}

impl UsageService {
    /// Loads the saved usage from the data folder and starts the poller, which looks up
    /// Codex by the command in the `settings` file.
    pub(crate) fn start(
        data_dir: &Path,
        settings: Option<PathBuf>,
        clients: Arc<Clients>,
        overrides: Overrides,
    ) -> Result<Arc<Self>> {
        remove_old_backoff_file(data_dir);

        let (service, wake) = Self::new(data_dir.join("usage.json"), clients);

        if !overrides.no_polling {
            let poller = Poller::new(Arc::clone(&service), wake, settings, overrides);
            thread::Builder::new()
                .name("usage-poller".into())
                .spawn(move || poller.run())
                .context("cannot start the usage poller")?;
        }

        Ok(service)
    }

    fn new(file: PathBuf, clients: Arc<Clients>) -> (Arc<Self>, flume::Receiver<()>) {
        let (wake, woken) = flume::bounded(1);
        let usage = load(&file);

        let service = Arc::new(Self {
            usage: Mutex::new(usage),
            file,
            clients,
            wake,
        });

        (service, woken)
    }

    pub(crate) fn current(&self) -> Usage {
        lock(&self.usage).clone()
    }

    /// Call after a client was added. The first app makes the poller read at once.
    pub(crate) fn client_connected(&self) {
        if self.clients.count() == 1 {
            // Full means a wake-up is waiting already.
            self.wake.try_send(()).ok();
        }
    }

    /// The rate limits a Claude Code agent got from its status line input.
    pub(crate) fn status_line(&self, windows: Vec<UsageWindow>) {
        self.record_status_line(windows, unix_now());
    }

    fn record_status_line(&self, windows: Vec<UsageWindow>, now: u64) {
        let windows = clean(windows);
        if windows.is_empty() {
            return;
        }

        let mut current = lock(&self.usage);

        let mut usage = current.clone();
        if !merge_windows(&mut usage.claude.windows, windows) {
            return;
        }
        usage.claude.read_at = Some(now);

        self.publish(&mut current, usage);
    }

    fn codex_read(&self, outcome: Outcome, now: u64) {
        let mut current = lock(&self.usage);
        let mut usage = current.clone();

        apply(&mut usage.codex, outcome, now);
        self.publish(&mut current, usage);
    }

    /// Saves and broadcasts a change. The caller holds the usage lock, so two changes
    /// never reach the clients in the wrong order.
    fn publish(&self, current: &mut Usage, usage: Usage) {
        if usage == *current {
            return;
        }
        *current = usage;

        match serde_json::to_vec(current) {
            Ok(bytes) => {
                if let Err(error) = write_private(&self.file, &bytes) {
                    log::warn!("cannot save {}: {error}", self.file.display());
                }
            }
            Err(error) => log::warn!("cannot encode the usage: {error}"),
        }

        self.clients.broadcast(&Event::UsageChanged {
            usage: current.clone(),
        });
    }
}

/// A good read replaces the windows and clears the problem. A failed read keeps the
/// last good windows and sets the problem.
fn apply(subscription: &mut SubscriptionUsage, outcome: Outcome, now: u64) {
    match outcome {
        Ok(reading) => {
            subscription.windows = reading.windows;
            if reading.plan.is_some() {
                subscription.plan = reading.plan;
            }
            subscription.read_at = Some(now);
            subscription.problem = None;
        }
        Err(problem) => subscription.problem = Some(problem),
    }
}

/// Merges the windows of a status line into the stored ones, label by label. A stored
/// window the status line lacks stays: Claude Code drops a window once it has reset.
/// Returns whether any window got newer data.
fn merge_windows(stored: &mut Vec<UsageWindow>, incoming: Vec<UsageWindow>) -> bool {
    let mut newer = false;

    for window in incoming {
        let position = stored.iter().position(|known| known.label == window.label);

        match position {
            Some(index) if is_newer(&stored[index], &window) => {
                stored[index] = window;
                newer = true;
            }
            None if stored.len() < MAX_WINDOWS => {
                stored.push(window);
                newer = true;
            }
            _ => {}
        }
    }

    // The shortest first; a window of unknown length goes last.
    stored.sort_by_key(|window| window.length_secs.unwrap_or(u64::MAX));

    newer
}

/// Whether `incoming` is newer than the `stored` window of the same label. An idle agent
/// sends the numbers of its last reply, so a lower value never replaces a higher one
/// of the same window. Only a later reset time makes a lower value valid.
fn is_newer(stored: &UsageWindow, incoming: &UsageWindow) -> bool {
    let (Some(stored_reset), Some(incoming_reset)) = (stored.resets_at, incoming.resets_at) else {
        // Without both reset times the order is unknown: the latest to arrive wins.
        return incoming != stored;
    };

    if incoming_reset > stored_reset.saturating_add(RESET_DRIFT) {
        return true;
    }
    if incoming_reset.saturating_add(RESET_DRIFT) < stored_reset {
        return false;
    }

    incoming.used_percent > stored.used_percent
}

/// An agent sent these, so they are cut to a sane size.
fn clean(windows: Vec<UsageWindow>) -> Vec<UsageWindow> {
    windows
        .into_iter()
        .take(MAX_WINDOWS)
        .map(|window| UsageWindow {
            label: window.label.chars().take(MAX_LABEL_CHARS).collect(),
            used_percent: window.used_percent.min(100),
            resets_at: window.resets_at,
            length_secs: window.length_secs,
        })
        .collect()
}

fn load(file: &Path) -> Usage {
    match std::fs::read(file) {
        Ok(bytes) => parse_saved(&bytes).unwrap_or_else(|error| {
            log::warn!("ignoring {}: {error}", file.display());
            Usage::default()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Usage::default(),
        Err(error) => {
            log::warn!("cannot read {}: {error}", file.display());
            Usage::default()
        }
    }
}

/// Reads a file an older daemon may have written: a problem that no longer exists is
/// dropped, and a window without a length gets the one its label stands for.
fn parse_saved(bytes: &[u8]) -> serde_json::Result<Usage> {
    let mut saved: Value = serde_json::from_slice(bytes)?;

    for name in ["claude", "codex"] {
        if let Some(subscription) = saved.get_mut(name) {
            upgrade(subscription);
        }
    }

    let mut usage: Usage = serde_json::from_value(saved)?;
    // Nothing reads Claude's plan or fails a read of Claude now, so what an older daemon
    // saved would stay for good.
    usage.claude.problem = None;
    usage.claude.plan = None;

    Ok(usage)
}

fn upgrade(subscription: &mut Value) {
    let Some(subscription) = subscription.as_object_mut() else {
        return;
    };

    let known = subscription
        .get("problem")
        .is_none_or(|problem| serde_json::from_value::<UsageProblem>(problem.clone()).is_ok());
    if !known {
        subscription.remove("problem");
    }

    let windows = subscription
        .get_mut("windows")
        .and_then(Value::as_array_mut);
    for window in windows
        .into_iter()
        .flatten()
        .filter_map(Value::as_object_mut)
    {
        if window
            .get("length_secs")
            .is_some_and(|length| !length.is_null())
        {
            continue;
        }

        let length = window
            .get("label")
            .and_then(Value::as_str)
            .and_then(length_of_label);
        if let Some(length) = length {
            window.insert("length_secs".to_owned(), length.into());
        }
    }
}

/// An older daemon kept the backoff of the Claude endpoint in its data folder. Nothing
/// reads it now.
fn remove_old_backoff_file(data_dir: &Path) {
    let file = data_dir.join(OLD_BACKOFF_FILE);

    match std::fs::remove_file(&file) {
        Ok(()) => log::info!("removed {}", file.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => log::warn!("cannot remove {}: {error}", file.display()),
    }
}

/// Time since the Unix epoch. Deadlines use it, not `Instant`, which stops while the Mac
/// sleeps.
fn unix_time() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

fn unix_now() -> u64 {
    unix_time().as_secs()
}

/// Looks the CLI up as the agents do: `PATH`, then the usual install folders. The
/// daemon can run with launchd's small `PATH`.
fn find(command: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);

    adapters::find_command(
        command,
        std::env::var_os("PATH").as_deref(),
        home.as_deref(),
    )
}

struct Poller {
    service: Arc<UsageService>,
    woken: flume::Receiver<()>,
    /// The settings file that names the agent CLIs.
    settings: Option<PathBuf>,
    /// A path a test gave. Without one, the command comes from the settings.
    codex: Option<String>,
    tick: Duration,
    codex_interval: Duration,
    /// When Codex is read next, as Unix time. `None` reads Codex on the next tick.
    codex_due: Option<Duration>,
}

impl Poller {
    fn new(
        service: Arc<UsageService>,
        woken: flume::Receiver<()>,
        settings: Option<PathBuf>,
        overrides: Overrides,
    ) -> Self {
        Self {
            service,
            woken,
            settings,
            codex: overrides
                .codex
                .map(|path| path.to_string_lossy().into_owned()),
            tick: overrides.tick.unwrap_or(POLL_TICK),
            codex_interval: overrides.codex_interval.unwrap_or(CODEX_INTERVAL),
            codex_due: None,
        }
    }

    fn run(mut self) {
        loop {
            match self.woken.recv_timeout(self.tick) {
                // The first app connected: read now, whatever the schedule says.
                Ok(()) => self.codex_due = None,
                Err(flume::RecvTimeoutError::Timeout) => {}
                Err(flume::RecvTimeoutError::Disconnected) => return,
            }

            if self.service.clients.count() == 0 {
                continue;
            }

            self.poll(unix_time());
        }
    }

    fn poll(&mut self, time: Duration) {
        if self.codex_due.is_some_and(|due| time < due) {
            return;
        }

        let outcome = codex_usage(self.find_codex().as_deref(), CODEX_TIMEOUT);
        self.service.codex_read(outcome, time.as_secs());
        self.codex_due = Some(time + self.codex_interval);
    }

    /// The path a test gave, or the command from the settings that `Agents::plan` uses
    /// too.
    fn find_codex(&self) -> Option<PathBuf> {
        let command = self
            .codex
            .clone()
            .unwrap_or_else(|| configured_command(self.settings.as_deref(), AgentKind::Codex));

        find(&command)
    }
}

/// The `agents.<kind>.command` of the settings file. A file that is missing or broken
/// gives the default: a failed usage read is no reason to stop the poller.
pub(crate) fn configured_command(settings: Option<&Path>, kind: AgentKind) -> String {
    let settings = settings
        .map(swarm_settings::load)
        .transpose()
        .unwrap_or_else(|error| {
            log::debug!("cannot read the settings for the usage poller: {error}");
            None
        })
        .unwrap_or_default();

    settings.agents.cli(kind).command.clone()
}

#[cfg(test)]
mod tests {
    use swarm_protocol::read;
    use swarm_usage::Reading;

    use crate::hub::{ClientSink, Frame};
    use crate::temp_folder::TempFolder;

    use super::*;

    const NOW: u64 = 1_800_000_000;

    /// A reset time of the weekly window.
    const WEEK_RESET: u64 = 1_900_000_000;

    /// The length comes from the label, as it does for the windows of both CLIs.
    fn window(label: &str, percent: u8, resets_at: u64) -> UsageWindow {
        UsageWindow {
            label: label.to_owned(),
            used_percent: percent,
            resets_at: Some(resets_at),
            length_secs: length_of_label(label),
        }
    }

    fn reading(windows: Vec<UsageWindow>, plan: Option<&str>) -> Reading {
        Reading {
            windows,
            plan: plan.map(str::to_owned),
        }
    }

    /// A service with one connected client; `changes` holds what the client was sent. The
    /// folder holds the saved usage and goes away with the last one who holds it.
    fn service_with_client(name: &str) -> (Arc<UsageService>, flume::Receiver<Frame>, TempFolder) {
        let folder = TempFolder::new(name);
        let clients = Arc::new(Clients::default());
        let (frames, received) = flume::bounded(64);
        clients.add(&Arc::new(ClientSink::new(1, frames, None)));
        let (service, _woken) = UsageService::new(folder.path().join("usage.json"), clients);

        (service, received, folder)
    }

    fn changes(received: &flume::Receiver<Frame>) -> Vec<Usage> {
        received
            .try_iter()
            .map(|frame| {
                let event: Event = read(&mut &frame[..], &mut Vec::new()).expect("event");
                let Event::UsageChanged { usage } = event else {
                    panic!("expected UsageChanged, got {event:?}");
                };

                usage
            })
            .collect()
    }

    #[test]
    fn a_good_read_sets_the_windows_and_clears_the_problem() {
        let mut subscription = SubscriptionUsage {
            problem: Some(UsageProblem::NotLoggedIn),
            ..SubscriptionUsage::default()
        };

        apply(
            &mut subscription,
            Ok(reading(vec![window("5h", 10, 5)], Some("max"))),
            NOW,
        );

        assert_eq!(subscription.windows, [window("5h", 10, 5)]);
        assert_eq!(subscription.plan.as_deref(), Some("max"));
        assert_eq!(subscription.read_at, Some(NOW));
        assert_eq!(subscription.problem, None);
    }

    #[test]
    fn a_failed_read_keeps_the_last_windows_and_sets_the_problem() {
        let mut subscription = SubscriptionUsage::default();
        apply(
            &mut subscription,
            Ok(reading(vec![window("5h", 10, 5)], Some("max"))),
            NOW,
        );

        apply(
            &mut subscription,
            Err(UsageProblem::Failed("boom".to_owned())),
            NOW + 30,
        );

        assert_eq!(subscription.windows, [window("5h", 10, 5)]);
        assert_eq!(
            subscription.read_at,
            Some(NOW),
            "the failed read is not a read"
        );
        assert_eq!(
            subscription.problem,
            Some(UsageProblem::Failed("boom".to_owned()))
        );
    }

    #[test]
    fn a_read_without_a_plan_keeps_the_known_plan() {
        let mut subscription = SubscriptionUsage::default();
        apply(&mut subscription, Ok(reading(Vec::new(), Some("max"))), NOW);

        apply(
            &mut subscription,
            Ok(reading(vec![window("5h", 1, 1)], None)),
            NOW + 1,
        );

        assert_eq!(subscription.plan.as_deref(), Some("max"));
    }

    #[test]
    fn a_change_is_broadcast_and_saved() {
        let (service, received, _folder) = service_with_client("change");

        service.codex_read(Ok(reading(vec![window("5h", 10, 5)], Some("plus"))), NOW);

        let sent = changes(&received);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].codex.windows, [window("5h", 10, 5)]);
        let saved = load(&service.file);
        assert_eq!(saved, service.current());
    }

    #[test]
    fn no_change_means_no_broadcast() {
        let (service, received, _folder) = service_with_client("same");
        service.codex_read(Err(UsageProblem::NotInstalled), NOW);
        changes(&received);

        service.codex_read(Err(UsageProblem::NotInstalled), NOW + 300);

        assert!(changes(&received).is_empty());
    }

    #[test]
    fn the_saved_usage_is_loaded_at_start() {
        let (service, _received, _folder) = service_with_client("reload");
        service.codex_read(Ok(reading(vec![window("7d", 33, 9)], Some("pro"))), NOW);
        service.record_status_line(vec![window("5h", 20, 7)], NOW);

        let (reloaded, _woken) =
            UsageService::new(service.file.clone(), Arc::new(Clients::default()));

        assert_eq!(reloaded.current(), service.current());
        assert_eq!(reloaded.current().codex.plan.as_deref(), Some("pro"));
        assert_eq!(reloaded.current().claude.windows, [window("5h", 20, 7)]);
    }

    #[test]
    fn a_broken_file_is_ignored() {
        let folder = TempFolder::new("broken");
        let file = folder.path().join("usage.json");
        std::fs::write(&file, "{ nope").expect("write");

        let (service, _woken) = UsageService::new(file, Arc::new(Clients::default()));

        assert_eq!(service.current(), Usage::default());
    }

    // Files an older daemon saved.

    const OLD_USAGE: &str = r#"{
        "claude": {
            "windows": [
                {"label": "5h", "used_percent": 42, "resets_at": 1793383200},
                {"label": "7d", "used_percent": 13, "resets_at": null}
            ],
            "plan": "max",
            "read_at": 1793380000,
            "problem": {"RateLimited": {"retry_at": 1793381000}}
        },
        "codex": {
            "windows": [
                {"label": "30d", "used_percent": 3, "resets_at": 9},
                {"label": "5h", "used_percent": 4, "resets_at": 8, "length_secs": 123}
            ],
            "plan": "plus",
            "read_at": 1793380001,
            "problem": "Unauthorized"
        }
    }"#;

    fn load_text(name: &str, text: &str) -> Usage {
        let folder = TempFolder::new(name);
        let file = folder.path().join("usage.json");
        std::fs::write(&file, text).expect("write");

        load(&file)
    }

    #[test]
    fn a_problem_that_no_longer_exists_is_dropped_and_the_rest_loads() {
        let usage = load_text("old-problems", OLD_USAGE);

        assert_eq!(usage.claude.problem, None);
        assert_eq!(usage.codex.problem, None);
        assert_eq!(usage.claude.read_at, Some(1_793_380_000));
        assert_eq!(usage.claude.windows.len(), 2);
        assert_eq!(usage.codex.plan.as_deref(), Some("plus"));
        assert_eq!(usage.codex.windows.len(), 2);
    }

    #[test]
    fn a_problem_that_still_exists_is_kept_for_codex_and_dropped_for_claude() {
        for problem in [
            r#""NotInstalled""#,
            r#""NotLoggedIn""#,
            r#"{"Failed": "boom"}"#,
        ] {
            let text = format!(
                r#"{{"claude": {{"windows": [], "problem": {problem}}}, "codex": {{"windows": [], "problem": {problem}}}}}"#
            );

            let usage = load_text("known-problem", &text);

            assert!(usage.codex.problem.is_some(), "{problem}");
            assert_eq!(usage.claude.problem, None, "{problem}: Claude is not read");
        }
    }

    #[test]
    fn a_window_saved_without_a_length_gets_the_one_of_its_label() {
        let usage = load_text("old-lengths", OLD_USAGE);

        let lengths = |subscription: &SubscriptionUsage| -> Vec<Option<u64>> {
            subscription
                .windows
                .iter()
                .map(|window| window.length_secs)
                .collect()
        };
        assert_eq!(lengths(&usage.claude), [Some(18_000), Some(604_800)]);
        assert_eq!(
            lengths(&usage.codex),
            [None, Some(123)],
            "a label of another length stays without one, and a saved length stays"
        );
    }

    #[test]
    fn a_window_saved_with_a_null_length_gets_the_one_of_its_label() {
        let usage = load_text(
            "null-length",
            r#"{"claude": {"windows": [{"label": "7d", "used_percent": 1, "resets_at": null, "length_secs": null}]}, "codex": {"windows": []}}"#,
        );

        assert_eq!(usage.claude.windows[0].length_secs, Some(604_800));
    }

    #[test]
    fn a_file_that_is_not_the_usage_is_ignored() {
        for text in [
            "[]",
            "5",
            r#"{"claude": 5}"#,
            r#"{"claude": {"windows": "many"}}"#,
        ] {
            let usage = load_text("not-usage", text);

            assert_eq!(usage, Usage::default(), "{text}");
        }
    }

    #[test]
    fn the_backoff_file_of_the_old_daemon_is_removed_at_start() {
        let folder = TempFolder::new("old-backoff");
        let backoff = folder.path().join(OLD_BACKOFF_FILE);
        std::fs::write(&backoff, r#"{"backoff_until": 1}"#).expect("backoff");
        std::fs::write(folder.path().join("usage.json"), OLD_USAGE).expect("usage");
        std::fs::write(folder.path().join("claude-backoff.json"), "{}").expect("other");

        let started = UsageService::start(
            folder.path(),
            None,
            Arc::new(Clients::default()),
            Overrides {
                no_polling: true,
                ..Overrides::default()
            },
        )
        .expect("start");

        assert!(!backoff.exists());
        assert!(folder.path().join("usage.json").is_file());
        assert!(folder.path().join("claude-backoff.json").is_file());
        assert_eq!(started.current().claude.windows.len(), 2);
    }

    #[test]
    fn a_start_without_a_backoff_file_works() {
        let folder = TempFolder::new("no-backoff");

        let started = UsageService::start(
            folder.path(),
            None,
            Arc::new(Clients::default()),
            Overrides {
                no_polling: true,
                ..Overrides::default()
            },
        )
        .expect("start");

        assert_eq!(started.current(), Usage::default());
    }

    // The status line.

    #[test]
    fn a_status_line_sets_the_claude_windows_with_their_length_and_the_time() {
        let (service, received, _folder) = service_with_client("statusline");

        service.record_status_line(vec![window("5h", 20, 7), window("7d", 30, 8)], NOW);

        let sent = changes(&received);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].claude.windows.len(), 2);
        assert_eq!(sent[0].claude.windows[0].length_secs, Some(18_000));
        assert_eq!(sent[0].claude.windows[1].length_secs, Some(604_800));
        assert_eq!(sent[0].claude.read_at, Some(NOW));
    }

    #[test]
    fn the_same_windows_again_change_nothing() {
        let (service, received, _folder) = service_with_client("step");
        service.record_status_line(vec![window("5h", 20, 7)], NOW);
        changes(&received);

        for later in [1, 59, 60, 3_600] {
            service.record_status_line(vec![window("5h", 20, 7)], NOW + later);
        }

        assert!(changes(&received).is_empty());
        assert_eq!(service.current().claude.read_at, Some(NOW));
    }

    #[test]
    fn other_windows_are_broadcast_at_once() {
        let (service, received, _folder) = service_with_client("other");
        service.record_status_line(vec![window("5h", 20, 7)], NOW);
        changes(&received);

        service.record_status_line(vec![window("5h", 21, 7)], NOW + 1);

        assert_eq!(changes(&received).len(), 1);
    }

    #[test]
    fn a_window_the_status_line_lacks_is_kept() {
        let (service, received, _folder) = service_with_client("lacks");
        service.record_status_line(
            vec![window("5h", 40, 100), window("7d", 30, WEEK_RESET)],
            NOW,
        );
        changes(&received);

        service.record_status_line(vec![window("7d", 31, WEEK_RESET)], NOW + 10);

        let claude = service.current().claude;
        assert_eq!(
            claude.windows,
            [window("5h", 40, 100), window("7d", 31, WEEK_RESET)]
        );
        assert_eq!(claude.read_at, Some(NOW + 10));
        assert_eq!(changes(&received).len(), 1);
    }

    #[test]
    fn a_window_seen_for_the_first_time_is_added_shortest_first() {
        let (service, _received, _folder) = service_with_client("added");
        service.record_status_line(vec![window("7d", 30, WEEK_RESET)], NOW);

        service.record_status_line(vec![window("5h", 40, 100)], NOW + 10);

        assert_eq!(
            service.current().claude.windows,
            [window("5h", 40, 100), window("7d", 30, WEEK_RESET)]
        );
    }

    #[test]
    fn an_idle_agents_older_numbers_do_not_overwrite_newer_ones() {
        let (service, received, _folder) = service_with_client("idle");
        service.record_status_line(vec![window("5h", 50, 100)], NOW);
        changes(&received);

        service.record_status_line(vec![window("5h", 30, 100)], NOW + 120);

        let claude = service.current().claude;
        assert_eq!(claude.windows, [window("5h", 50, 100)]);
        assert_eq!(claude.read_at, Some(NOW));
        assert!(changes(&received).is_empty());
    }

    #[test]
    fn reset_times_within_the_drift_are_one_window() {
        let (service, _received, _folder) = service_with_client("drift");
        service.record_status_line(vec![window("5h", 50, 1_000)], NOW);

        service.record_status_line(vec![window("5h", 30, 1_000 + RESET_DRIFT)], NOW + 1);
        assert_eq!(service.current().claude.windows, [window("5h", 50, 1_000)]);

        service.record_status_line(vec![window("5h", 60, 1_000 + RESET_DRIFT)], NOW + 2);
        assert_eq!(
            service.current().claude.windows,
            [window("5h", 60, 1_000 + RESET_DRIFT)]
        );
    }

    #[test]
    fn a_later_reset_time_is_a_new_window_and_may_take_a_lower_value() {
        let (service, _received, _folder) = service_with_client("new-window");
        service.record_status_line(vec![window("5h", 90, 1_000)], NOW);

        service.record_status_line(vec![window("5h", 2, 1_000 + RESET_DRIFT + 1)], NOW + 5);

        let claude = service.current().claude;
        assert_eq!(claude.windows, [window("5h", 2, 1_000 + RESET_DRIFT + 1)]);
        assert_eq!(claude.read_at, Some(NOW + 5));
    }

    #[test]
    fn an_earlier_reset_time_is_an_old_window_and_is_ignored() {
        let (service, received, _folder) = service_with_client("old-window");
        service.record_status_line(vec![window("5h", 2, 1_000 + 18_000)], NOW);
        changes(&received);

        service.record_status_line(vec![window("5h", 90, 1_000)], NOW + 5);

        assert_eq!(
            service.current().claude.windows,
            [window("5h", 2, 1_000 + 18_000)]
        );
        assert!(changes(&received).is_empty());
    }

    #[test]
    fn the_read_time_moves_only_when_a_window_got_newer() {
        let (service, _received, _folder) = service_with_client("read-time");
        service.record_status_line(
            vec![window("5h", 50, 100), window("7d", 40, WEEK_RESET)],
            NOW,
        );

        service.record_status_line(
            vec![window("5h", 30, 100), window("7d", 41, WEEK_RESET)],
            NOW + 10,
        );
        let claude = service.current().claude;
        assert_eq!(
            claude.windows,
            [window("5h", 50, 100), window("7d", 41, WEEK_RESET)]
        );
        assert_eq!(claude.read_at, Some(NOW + 10));

        service.record_status_line(
            vec![window("5h", 30, 100), window("7d", 41, WEEK_RESET)],
            NOW + 20,
        );
        assert_eq!(service.current().claude.read_at, Some(NOW + 10));
    }

    #[test]
    fn a_window_without_a_reset_time_takes_the_latest_numbers() {
        let (service, _received, _folder) = service_with_client("no-reset");
        let without_reset = |percent| UsageWindow {
            resets_at: None,
            ..window("5h", percent, 0)
        };
        service.record_status_line(vec![without_reset(50)], NOW);

        service.record_status_line(vec![without_reset(30)], NOW + 1);
        assert_eq!(service.current().claude.windows, [without_reset(30)]);

        service.record_status_line(vec![window("5h", 20, 500)], NOW + 2);
        assert_eq!(service.current().claude.windows, [window("5h", 20, 500)]);
    }

    #[test]
    fn a_saved_claude_plan_is_dropped_and_a_status_line_does_not_bring_it_back() {
        let folder = TempFolder::new("claude-plan");
        let file = folder.path().join("usage.json");
        std::fs::write(&file, OLD_USAGE).expect("write");

        let (service, _woken) = UsageService::new(file.clone(), Arc::new(Clients::default()));
        assert_eq!(service.current().claude.plan, None);
        assert_eq!(service.current().codex.plan.as_deref(), Some("plus"));

        service.record_status_line(vec![window("5h", 50, 1_793_383_200)], NOW);
        assert_eq!(service.current().claude.plan, None);
        assert_eq!(load(&file).claude.plan, None);
    }

    #[test]
    fn an_empty_status_line_changes_nothing() {
        let (service, received, _folder) = service_with_client("empty");

        service.record_status_line(Vec::new(), NOW);

        assert!(changes(&received).is_empty());
        assert_eq!(service.current(), Usage::default());
    }

    #[test]
    fn windows_from_an_agent_are_cut_to_size() {
        let many: Vec<UsageWindow> = (0..20).map(|n| window(&"x".repeat(40), 250, n)).collect();

        let cleaned = clean(many);

        assert_eq!(cleaned.len(), MAX_WINDOWS);
        assert_eq!(cleaned[0].label.chars().count(), MAX_LABEL_CHARS);
        assert_eq!(cleaned[0].used_percent, 100);
    }

    #[test]
    fn a_window_from_an_agent_keeps_its_length() {
        let sent = UsageWindow {
            length_secs: Some(18_000),
            ..window("5h", 1, 1)
        };

        assert_eq!(clean(vec![sent.clone()]), [sent]);
    }

    #[test]
    fn only_the_first_client_wakes_the_poller() {
        let folder = TempFolder::new("wake");
        let clients = Arc::new(Clients::default());
        let (frames, _received) = flume::bounded(8);
        let (service, woken) =
            UsageService::new(folder.path().join("usage.json"), Arc::clone(&clients));

        clients.add(&Arc::new(ClientSink::new(1, frames.clone(), None)));
        service.client_connected();
        let first = woken.try_iter().count();

        clients.add(&Arc::new(ClientSink::new(2, frames, None)));
        service.client_connected();
        let second = woken.try_iter().count();

        assert_eq!((first, second), (1, 0));
    }

    // The poller, with a fake `codex`.

    /// What the fake `codex` does: it answers a read of the rate limits.
    const CODEX_BODY: &str = "while IFS= read -r line; do case \"$line\" in *account/rateLimits/read*) cat \"$DIR/answer\";; esac; done";

    /// A fake `codex` in a folder of its own.
    struct Fakes {
        dir: PathBuf,
        _folder: TempFolder,
    }

    impl Fakes {
        fn new(name: &str) -> Self {
            let folder = TempFolder::new(name);
            let fakes = Self {
                dir: folder.path().to_owned(),
                _folder: folder,
            };

            fakes.write(
                "answer",
                "{\"id\":2,\"result\":{\"rateLimits\":{\"planType\":\"plus\",\"primary\":{\"usedPercent\":12,\"windowDurationMins\":300,\"resetsAt\":99}}}}\n",
            );
            fakes.script("codex", CODEX_BODY);

            fakes
        }

        fn write(&self, name: &str, text: &str) {
            std::fs::write(self.dir.join(name), text).expect("file");
        }

        /// A script that counts its starts in `<name>-calls`.
        fn script(&self, name: &str, body: &str) {
            use std::os::unix::fs::PermissionsExt as _;

            let path = self.dir.join(name);
            std::fs::write(
                &path,
                format!(
                    "#!/bin/sh\nDIR='{}'\necho started >> \"$DIR/{name}-calls\"\n{body}\n",
                    self.dir.display()
                ),
            )
            .expect("script");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("mode");
        }

        fn calls(&self, name: &str) -> usize {
            std::fs::read_to_string(self.dir.join(format!("{name}-calls")))
                .map_or(0, |text| text.lines().count())
        }

        fn poller(&self, service: &Arc<UsageService>, woken: flume::Receiver<()>) -> Poller {
            let overrides = Overrides {
                codex: Some(self.dir.join("codex")),
                ..Overrides::default()
            };

            Poller::new(Arc::clone(service), woken, None, overrides)
        }

        /// A poller that finds `codex` as production does, by the command in a settings
        /// file with this text.
        fn poller_by_settings(
            &self,
            service: &Arc<UsageService>,
            woken: flume::Receiver<()>,
            settings: &str,
        ) -> Poller {
            self.write("settings.toml", settings);

            Poller::new(
                Arc::clone(service),
                woken,
                Some(self.dir.join("settings.toml")),
                Overrides::default(),
            )
        }
    }

    fn poller_setup(name: &str) -> (Fakes, Arc<UsageService>, Poller) {
        let fakes = Fakes::new(name);
        let (service, woken) =
            UsageService::new(fakes.dir.join("usage.json"), Arc::new(Clients::default()));
        let poller = fakes.poller(&service, woken);

        (fakes, service, poller)
    }

    fn now() -> Duration {
        Duration::from_secs(NOW)
    }

    #[test]
    fn a_poll_fills_the_codex_usage_with_the_length_of_its_windows() {
        let (fakes, service, mut poller) = poller_setup("codex");

        poller.poll(now());

        let usage = service.current();
        assert_eq!(usage.codex.windows, [window("5h", 12, 99)]);
        assert_eq!(usage.codex.windows[0].length_secs, Some(18_000));
        assert_eq!(usage.codex.plan.as_deref(), Some("plus"));
        assert_eq!(fakes.calls("codex"), 1);
    }

    #[test]
    fn a_poll_leaves_the_claude_numbers_as_they_are() {
        let (_fakes, service, mut poller) = poller_setup("claude-kept");
        let long_ago = NOW - 86_400;
        service.record_status_line(vec![window("5h", 7, 1), window("7d", 8, 2)], long_ago);
        let before = service.current().claude;

        poller.poll(now());
        poller.codex_due = None;
        poller.poll(now() + Duration::from_secs(3600));

        let claude = service.current().claude;
        assert_eq!(claude, before);
        assert_eq!(claude.read_at, Some(long_ago));
        assert_eq!(claude.problem, None);
    }

    #[test]
    fn codex_is_asked_once_per_interval_and_at_once_after_a_wake() {
        let (fakes, _service, mut poller) = poller_setup("interval");
        poller.codex_interval = Duration::from_secs(300);

        poller.poll(now());
        poller.poll(now());
        assert_eq!(fakes.calls("codex"), 1);

        poller.codex_due = None;
        poller.poll(now());
        assert_eq!(fakes.calls("codex"), 2);
    }

    #[test]
    fn codex_is_due_by_the_wall_clock_so_a_sleeping_mac_counts() {
        let (fakes, _service, mut poller) = poller_setup("wall-clock");
        poller.codex_interval = Duration::from_secs(300);

        poller.poll(now());
        poller.poll(now() + Duration::from_secs(299));
        assert_eq!(fakes.calls("codex"), 1);

        poller.poll(now() + Duration::from_secs(300));
        assert_eq!(fakes.calls("codex"), 2);
    }

    #[test]
    fn the_codex_command_comes_from_the_settings() {
        let fakes = Fakes::new("settings");
        let (service, woken) =
            UsageService::new(fakes.dir.join("usage.json"), Arc::new(Clients::default()));
        fakes.script("my-codex", CODEX_BODY);
        let dir = fakes.dir.display();
        let mut poller = fakes.poller_by_settings(
            &service,
            woken,
            &format!("[agents.codex]\ncommand = \"{dir}/my-codex\"\n"),
        );

        poller.poll(now());

        let usage = service.current();
        assert_eq!(usage.codex.windows, [window("5h", 12, 99)]);
        assert_eq!(fakes.calls("my-codex"), 1);
        assert_eq!(fakes.calls("codex"), 0);
    }

    #[test]
    fn a_command_in_the_settings_that_does_not_exist_is_not_installed() {
        let fakes = Fakes::new("settings-missing");
        let (service, woken) =
            UsageService::new(fakes.dir.join("usage.json"), Arc::new(Clients::default()));
        let mut poller = fakes.poller_by_settings(
            &service,
            woken,
            "[agents.codex]\ncommand = \"/nonexistent/codex\"\n",
        );

        poller.poll(now());

        let usage = service.current();
        assert_eq!(usage.codex.problem, Some(UsageProblem::NotInstalled));
        assert_eq!(usage.claude, SubscriptionUsage::default());
    }

    #[test]
    fn without_usable_settings_the_default_commands_are_looked_up() {
        let fakes = Fakes::new("settings-default");
        fakes.write("broken.toml", "agents = [");
        let broken = fakes.dir.join("broken.toml");
        let missing = fakes.dir.join("missing.toml");

        for settings in [None, Some(missing.as_path()), Some(broken.as_path())] {
            assert_eq!(
                configured_command(settings, AgentKind::ClaudeCode),
                "claude"
            );
            assert_eq!(configured_command(settings, AgentKind::Codex), "codex");
        }
    }

    #[test]
    fn a_failed_codex_read_keeps_the_last_windows() {
        let (fakes, service, mut poller) = poller_setup("keeps");
        poller.poll(now());
        fakes.write(
            "answer",
            "{\"id\":2,\"error\":{\"message\":\"codex account authentication required\"}}\n",
        );
        poller.codex_due = None;

        poller.poll(now());

        let codex = service.current().codex;
        assert_eq!(codex.windows, [window("5h", 12, 99)]);
        assert_eq!(codex.problem, Some(UsageProblem::NotLoggedIn));
    }

    // Swarm never reads Claude Code's sign-in token and never calls Anthropic's usage
    // endpoint. The test in `tests/daemon.rs` traps `security` and `curl` on `PATH`; this
    // one also catches a call by absolute path.

    /// Every source file of `swarm_usage`, whole.
    const USAGE_CRATE_SOURCES: [(&str, &str); 5] = [
        ("claude.rs", include_str!("../../swarm_usage/src/claude.rs")),
        ("codex.rs", include_str!("../../swarm_usage/src/codex.rs")),
        (
            "fixtures.rs",
            include_str!("../../swarm_usage/src/fixtures.rs"),
        ),
        (
            "process.rs",
            include_str!("../../swarm_usage/src/process.rs"),
        ),
        (
            "swarm_usage.rs",
            include_str!("../../swarm_usage/src/swarm_usage.rs"),
        ),
    ];

    const FORBIDDEN: [&str; 5] = [
        "/usr/bin/curl",
        "/usr/bin/security",
        "\"curl\"",
        "\"security\"",
        "oauth",
    ];

    /// The forbidden words in the part of a source file above its tests, which may name
    /// them. Case does not matter.
    fn forbidden_in(name: &str, source: &str) -> Vec<String> {
        let code = source
            .split("#[cfg(test)]")
            .next()
            .unwrap_or(source)
            .to_lowercase();

        FORBIDDEN
            .iter()
            .filter(|word| code.contains(*word))
            .map(|word| format!("{name}: {word}"))
            .collect()
    }

    #[test]
    fn the_usage_sources_never_name_the_keychain_or_the_usage_endpoint_programs() {
        let sources = USAGE_CRATE_SOURCES
            .iter()
            .copied()
            .chain([("swarm_daemon usage.rs", include_str!("usage.rs"))]);

        let hits: Vec<String> = sources
            .flat_map(|(name, source)| forbidden_in(name, source))
            .collect();

        assert!(hits.is_empty(), "forbidden words in the sources: {hits:?}");
    }

    #[test]
    fn the_source_scan_covers_every_file_of_the_usage_crate() {
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("../swarm_usage/src");
        let mut on_disk: Vec<String> = std::fs::read_dir(&folder)
            .expect("the swarm_usage sources")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        on_disk.sort();

        let scanned: Vec<&str> = USAGE_CRATE_SOURCES.iter().map(|(name, _)| *name).collect();

        assert_eq!(on_disk, scanned, "add the new file to USAGE_CRATE_SOURCES");
    }

    #[test]
    fn the_source_scan_reports_a_forbidden_word_above_the_tests_only() {
        let source = "fn run() { Command::new(\"/usr/bin/curl\"); Command::new(\"security\"); }\n\
                      // OAuth\n#[cfg(test)]\nmod tests { \"curl\" /usr/bin/security }";

        let hits = forbidden_in("source", source);

        assert_eq!(
            hits,
            [
                "source: /usr/bin/curl",
                "source: \"security\"",
                "source: oauth"
            ]
        );
    }
}
