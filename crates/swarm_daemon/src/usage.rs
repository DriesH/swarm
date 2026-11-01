//! Subscription usage: the numbers the daemon keeps, saves and broadcasts, and the
//! poller that fills them (docs/PLAN.md, "Usage and menu bar").
//!
//! Claude's windows come from the status line of the Claude agents Swarm runs. The
//! endpoint is the fallback when none has sent them lately. Codex is asked every few
//! minutes. Polling runs only while an app is connected.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use swarm_core::adapters;
use swarm_protocol::{AgentKind, Event, SubscriptionUsage, Usage, UsageWindow};
use swarm_usage::{ClaudeEndpoint, Commands, Outcome, Reading, codex_usage, write_private};

use crate::agents::lock;
use crate::hub::Clients;

/// How often the poller looks at the clients and the schedule.
const POLL_TICK: Duration = Duration::from_secs(5);

/// How often Codex is asked.
const CODEX_INTERVAL: Duration = Duration::from_mins(5);

const CODEX_TIMEOUT: Duration = Duration::from_secs(10);

/// The endpoint is not used while a status line sent windows this recently.
const STATUS_LINE_FRESH: Duration = Duration::from_mins(10);

/// Status lines run often. With the same windows, `read_at` moves on at this pace only.
const READ_AT_STEP: u64 = 60;

/// An agent sends two windows; this leaves room and stops a bad caller.
const MAX_WINDOWS: usize = 8;
const MAX_LABEL_CHARS: usize = 16;

/// Commands and timings that tests replace. Empty in production.
#[derive(Default)]
pub(crate) struct Overrides {
    /// No poller at all.
    pub(crate) no_polling: bool,
    pub(crate) security: Option<PathBuf>,
    pub(crate) curl: Option<PathBuf>,
    pub(crate) codex: Option<PathBuf>,
    pub(crate) claude: Option<PathBuf>,
    pub(crate) tick: Option<Duration>,
    pub(crate) codex_interval: Option<Duration>,
}

struct State {
    usage: Usage,
    /// When a status line last sent windows, in Unix seconds. Not saved: it only keeps the
    /// endpoint quiet. It is not an `Instant`, which stops while the Mac sleeps.
    status_line_at: Option<u64>,
}

pub(crate) struct UsageService {
    state: Mutex<State>,
    file: PathBuf,
    clients: Arc<Clients>,
    /// Wakes the poller when the first app connects.
    wake: flume::Sender<()>,
}

impl UsageService {
    /// Loads the saved usage from the data folder and starts the poller, which looks up
    /// the agent CLIs by the commands in the `settings` file.
    pub(crate) fn start(
        data_dir: &Path,
        settings: Option<PathBuf>,
        clients: Arc<Clients>,
        overrides: Overrides,
    ) -> Result<Arc<Self>> {
        let (service, wake) = Self::new(data_dir.join("usage.json"), clients);

        if !overrides.no_polling {
            let poller = Poller::new(Arc::clone(&service), wake, data_dir, settings, overrides);
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
            state: Mutex::new(State {
                usage,
                status_line_at: None,
            }),
            file,
            clients,
            wake,
        });

        (service, woken)
    }

    pub(crate) fn current(&self) -> Usage {
        lock(&self.state).usage.clone()
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

        let mut state = lock(&self.state);
        state.status_line_at = Some(now);

        let claude = &state.usage.claude;
        let same = claude.windows == windows
            && claude.problem.is_none()
            && claude.read_at.is_some_and(|at| now < at + READ_AT_STEP);
        if same {
            return;
        }

        let mut usage = state.usage.clone();
        apply(
            &mut usage.claude,
            Ok(Reading {
                windows,
                plan: None,
            }),
            now,
        );
        self.publish(&mut state, usage);
    }

    /// A status line from the future, after the clock was set back, is stale too.
    fn status_line_is_stale(&self, now: u64) -> bool {
        let fresh = |at: u64| at <= now && now < at.saturating_add(STATUS_LINE_FRESH.as_secs());

        !lock(&self.state).status_line_at.is_some_and(fresh)
    }

    fn claude_read(&self, outcome: Outcome, now: u64) {
        let mut state = lock(&self.state);
        let mut usage = state.usage.clone();

        apply(&mut usage.claude, outcome, now);
        self.publish(&mut state, usage);
    }

    fn codex_read(&self, outcome: Outcome, now: u64) {
        let mut state = lock(&self.state);
        let mut usage = state.usage.clone();

        apply(&mut usage.codex, outcome, now);
        self.publish(&mut state, usage);
    }

    /// Saves and broadcasts a change. It holds the state lock, so two changes never
    /// reach the clients in the wrong order.
    fn publish(&self, state: &mut State, usage: Usage) {
        if usage == state.usage {
            return;
        }
        state.usage = usage;

        match serde_json::to_vec(&state.usage) {
            Ok(bytes) => {
                if let Err(error) = write_private(&self.file, &bytes) {
                    log::warn!("cannot save {}: {error}", self.file.display());
                }
            }
            Err(error) => log::warn!("cannot encode the usage: {error}"),
        }

        self.clients.broadcast(&Event::UsageChanged {
            usage: state.usage.clone(),
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

/// An agent sent these, so they are cut to a sane size.
fn clean(windows: Vec<UsageWindow>) -> Vec<UsageWindow> {
    windows
        .into_iter()
        .take(MAX_WINDOWS)
        .map(|window| UsageWindow {
            label: window.label.chars().take(MAX_LABEL_CHARS).collect(),
            used_percent: window.used_percent.min(100),
            resets_at: window.resets_at,
        })
        .collect()
}

fn load(file: &Path) -> Usage {
    match std::fs::read(file) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
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

/// The account name of Claude Code's Keychain item.
fn account() -> Option<String> {
    ["USER", "LOGNAME"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|name| !name.is_empty())
}

struct Poller {
    service: Arc<UsageService>,
    woken: flume::Receiver<()>,
    endpoint: ClaudeEndpoint,
    /// The settings file that names the agent CLIs.
    settings: Option<PathBuf>,
    /// A path a test gave. Without one, the command comes from the settings.
    claude: Option<String>,
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
        data_dir: &Path,
        settings: Option<PathBuf>,
        overrides: Overrides,
    ) -> Self {
        let mut commands = Commands::system();
        if let Some(security) = overrides.security {
            commands.security = security;
        }
        if let Some(curl) = overrides.curl {
            commands.curl = curl;
        }

        let name = |path: Option<PathBuf>| path.map(|path| path.to_string_lossy().into_owned());

        Self {
            service,
            woken,
            endpoint: ClaudeEndpoint::new(
                commands,
                account(),
                data_dir.join("claude-usage-backoff.json"),
            ),
            settings,
            claude: name(overrides.claude),
            codex: name(overrides.codex),
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

            self.poll();
        }
    }

    fn poll(&mut self) {
        self.poll_codex(unix_time());
        self.poll_claude(unix_now());
    }

    fn poll_codex(&mut self, time: Duration) {
        if self.codex_due.is_some_and(|due| time < due) {
            return;
        }

        let outcome = codex_usage(self.find_cli(AgentKind::Codex).as_deref(), CODEX_TIMEOUT);
        self.service.codex_read(outcome, time.as_secs());
        self.codex_due = Some(time + self.codex_interval);
    }

    fn poll_claude(&mut self, now: u64) {
        if !self.service.status_line_is_stale(now) {
            return;
        }

        let installed = self.find_cli(AgentKind::ClaudeCode).is_some();

        if let Some(outcome) = self.endpoint.poll(now, installed) {
            self.service.claude_read(outcome, now);
        }
    }

    /// The CLI of an agent kind: the path a test gave, or the command from the settings
    /// that `Agents::plan` uses too.
    fn find_cli(&self, kind: AgentKind) -> Option<PathBuf> {
        let given = match kind {
            AgentKind::ClaudeCode => &self.claude,
            AgentKind::Codex => &self.codex,
        };
        let command = given
            .clone()
            .unwrap_or_else(|| configured_command(self.settings.as_deref(), kind));

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
    use swarm_protocol::{UsageProblem, read};

    use crate::hub::{ClientSink, Frame};
    use crate::temp_folder::TempFolder;

    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn window(label: &str, percent: u8, resets_at: u64) -> UsageWindow {
        UsageWindow {
            label: label.to_owned(),
            used_percent: percent,
            resets_at: Some(resets_at),
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
            Err(UsageProblem::RateLimited { retry_at: NOW + 60 }),
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
            Some(UsageProblem::RateLimited { retry_at: NOW + 60 })
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
        service.claude_read(Ok(reading(vec![window("7d", 33, 9)], Some("pro"))), NOW);

        let (reloaded, _woken) =
            UsageService::new(service.file.clone(), Arc::new(Clients::default()));

        assert_eq!(reloaded.current(), service.current());
        assert_eq!(reloaded.current().claude.plan.as_deref(), Some("pro"));
    }

    #[test]
    fn a_broken_file_is_ignored() {
        let folder = TempFolder::new("broken");
        let file = folder.path().join("usage.json");
        std::fs::write(&file, "{ nope").expect("write");

        let (service, _woken) = UsageService::new(file, Arc::new(Clients::default()));

        assert_eq!(service.current(), Usage::default());
    }

    #[test]
    fn a_status_line_sets_the_claude_windows_and_the_time() {
        let (service, received, _folder) = service_with_client("statusline");
        assert!(service.status_line_is_stale(NOW));

        service.record_status_line(vec![window("5h", 20, 7), window("7d", 30, 8)], NOW);

        let sent = changes(&received);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].claude.windows.len(), 2);
        assert_eq!(sent[0].claude.read_at, Some(NOW));
        assert!(!service.status_line_is_stale(NOW + 1));
    }

    #[test]
    fn a_status_line_clears_a_claude_problem() {
        let (service, received, _folder) = service_with_client("clears");
        service.claude_read(Err(UsageProblem::Unauthorized), NOW);
        changes(&received);

        service.record_status_line(vec![window("5h", 20, 7)], NOW + 5);

        let sent = changes(&received);
        assert_eq!(sent[0].claude.problem, None);
    }

    #[test]
    fn the_same_windows_again_are_not_broadcast_before_the_read_time_step() {
        let (service, received, _folder) = service_with_client("step");
        service.record_status_line(vec![window("5h", 20, 7)], NOW);
        changes(&received);

        service.record_status_line(vec![window("5h", 20, 7)], NOW + 1);
        service.record_status_line(vec![window("5h", 20, 7)], NOW + READ_AT_STEP - 1);
        assert!(changes(&received).is_empty());

        service.record_status_line(vec![window("5h", 20, 7)], NOW + READ_AT_STEP);
        let sent = changes(&received);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].claude.read_at, Some(NOW + READ_AT_STEP));
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
    fn an_empty_status_line_changes_nothing() {
        let (service, received, _folder) = service_with_client("empty");

        service.record_status_line(Vec::new(), NOW);

        assert!(changes(&received).is_empty());
        assert!(service.status_line_is_stale(NOW));
    }

    #[test]
    fn a_status_line_stays_fresh_for_ten_minutes_by_the_clock_on_the_wall() {
        let (service, _received, _folder) = service_with_client("fresh");
        service.record_status_line(vec![window("5h", 20, 7)], NOW);
        let fresh_for = STATUS_LINE_FRESH.as_secs();

        assert!(!service.status_line_is_stale(NOW));
        assert!(!service.status_line_is_stale(NOW + fresh_for - 1));
        assert!(service.status_line_is_stale(NOW + fresh_for));
    }

    #[test]
    fn a_status_line_from_the_future_is_stale() {
        let (service, _received, _folder) = service_with_client("future");
        service.record_status_line(vec![window("5h", 20, 7)], NOW);

        assert!(service.status_line_is_stale(NOW - 1));
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

    // The poller, with fake commands.

    /// What the fake `codex` does: it answers a read of the rate limits.
    const CODEX_BODY: &str = "while IFS= read -r line; do case \"$line\" in *account/rateLimits/read*) cat \"$DIR/answer\";; esac; done";

    /// Fake `security`, `curl`, `codex` and `claude` in a folder of their own.
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

            let far = (NOW + 86_400) * 1000;
            fakes.write(
                "credentials",
                &format!(
                    r#"{{"claudeAiOauth":{{"accessToken":"sk-fake","expiresAt":{far}.0,"subscriptionType":"max"}}}}"#
                ),
            );
            fakes.write(
                "response",
                "HTTP/2 200 \r\n\r\n{\"five_hour\":{\"utilization\":42,\"resets_at\":\"2026-10-30T18:00:00Z\"}}",
            );
            fakes.write(
                "answer",
                "{\"id\":2,\"result\":{\"rateLimits\":{\"planType\":\"plus\",\"primary\":{\"usedPercent\":12,\"windowDurationMins\":300,\"resetsAt\":99}}}}\n",
            );
            fakes.script("security", "cat \"$DIR/credentials\"");
            fakes.script("curl", "cat > /dev/null\ncat \"$DIR/response\"");
            fakes.script("codex", CODEX_BODY);
            fakes.script("claude", "exit 0");

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

        fn overrides(&self) -> Overrides {
            Overrides {
                security: Some(self.dir.join("security")),
                curl: Some(self.dir.join("curl")),
                codex: Some(self.dir.join("codex")),
                claude: Some(self.dir.join("claude")),
                ..Overrides::default()
            }
        }

        fn poller(&self, service: &Arc<UsageService>, woken: flume::Receiver<()>) -> Poller {
            Poller::new(
                Arc::clone(service),
                woken,
                &self.dir,
                None,
                self.overrides(),
            )
        }

        /// A poller that finds `claude` and `codex` as production does, by the commands in
        /// a settings file with this text. Only `security` and `curl` are fakes.
        fn poller_by_settings(
            &self,
            service: &Arc<UsageService>,
            woken: flume::Receiver<()>,
            settings: &str,
        ) -> Poller {
            self.write("settings.toml", settings);
            let overrides = Overrides {
                claude: None,
                codex: None,
                ..self.overrides()
            };

            Poller::new(
                Arc::clone(service),
                woken,
                &self.dir,
                Some(self.dir.join("settings.toml")),
                overrides,
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

    #[test]
    fn without_a_status_line_the_endpoint_and_codex_fill_the_usage() {
        let (fakes, service, mut poller) = poller_setup("both");

        poller.poll();

        let usage = service.current();
        assert_eq!(usage.claude.windows.len(), 1);
        assert_eq!(usage.claude.windows[0].label, "5h");
        assert_eq!(usage.claude.windows[0].used_percent, 42);
        assert_eq!(usage.claude.plan.as_deref(), Some("max"));
        assert_eq!(usage.codex.windows, [window("5h", 12, 99)]);
        assert_eq!(usage.codex.plan.as_deref(), Some("plus"));
        assert_eq!(
            (
                fakes.calls("security"),
                fakes.calls("curl"),
                fakes.calls("codex")
            ),
            (1, 1, 1)
        );
    }

    #[test]
    fn a_recent_status_line_keeps_the_endpoint_quiet() {
        let (fakes, service, mut poller) = poller_setup("quiet");
        service.record_status_line(vec![window("5h", 7, 1), window("7d", 8, 2)], unix_now());

        poller.poll();

        let usage = service.current();
        assert_eq!(
            usage.claude.windows[0].used_percent, 7,
            "the status line stays"
        );
        assert_eq!(usage.claude.problem, None);
        assert_eq!((fakes.calls("security"), fakes.calls("curl")), (0, 0));
        assert_eq!(fakes.calls("codex"), 1, "Codex has no status line");
    }

    #[test]
    fn an_old_status_line_lets_the_endpoint_run() {
        let (fakes, service, mut poller) = poller_setup("old");
        let long_ago = unix_now() - STATUS_LINE_FRESH.as_secs() - 1;
        service.record_status_line(vec![window("5h", 7, 1)], long_ago);

        poller.poll();

        assert_eq!(fakes.calls("curl"), 1);
        assert_eq!(service.current().claude.windows[0].used_percent, 42);
    }

    #[test]
    fn codex_is_asked_once_per_interval_and_at_once_after_a_wake() {
        let (fakes, _service, mut poller) = poller_setup("interval");
        poller.codex_interval = Duration::from_secs(300);

        poller.poll();
        poller.poll();
        assert_eq!(fakes.calls("codex"), 1);

        poller.codex_due = None;
        poller.poll();
        assert_eq!(fakes.calls("codex"), 2);
    }

    #[test]
    fn codex_is_due_by_the_wall_clock_so_a_sleeping_mac_counts() {
        let (fakes, _service, mut poller) = poller_setup("wall-clock");
        poller.codex_interval = Duration::from_secs(300);
        let start = Duration::from_secs(NOW);

        poller.poll_codex(start);
        poller.poll_codex(start + Duration::from_secs(299));
        assert_eq!(fakes.calls("codex"), 1);

        poller.poll_codex(start + Duration::from_secs(300));
        assert_eq!(fakes.calls("codex"), 2);
    }

    #[test]
    fn the_cli_commands_come_from_the_settings() {
        let fakes = Fakes::new("settings");
        let (service, woken) =
            UsageService::new(fakes.dir.join("usage.json"), Arc::new(Clients::default()));
        fakes.script("my-claude", "exit 0");
        fakes.script("my-codex", CODEX_BODY);
        let dir = fakes.dir.display();
        let mut poller = fakes.poller_by_settings(
            &service,
            woken,
            &format!(
                "[agents.claude_code]\ncommand = \"{dir}/my-claude\"\n\n[agents.codex]\ncommand = \"{dir}/my-codex\"\n"
            ),
        );

        poller.poll();

        let usage = service.current();
        assert_eq!(usage.codex.windows, [window("5h", 12, 99)]);
        assert_eq!(fakes.calls("my-codex"), 1);
        assert_eq!(fakes.calls("codex"), 0);
        assert_eq!(usage.claude.problem, None, "claude is installed");
        assert_eq!(usage.claude.windows[0].used_percent, 42);
    }

    #[test]
    fn a_command_in_the_settings_that_does_not_exist_is_not_installed() {
        let fakes = Fakes::new("settings-missing");
        let (service, woken) =
            UsageService::new(fakes.dir.join("usage.json"), Arc::new(Clients::default()));
        let mut poller = fakes.poller_by_settings(
            &service,
            woken,
            "[agents.claude_code]\ncommand = \"/nonexistent/claude\"\n\n[agents.codex]\ncommand = \"/nonexistent/codex\"\n",
        );

        poller.poll();

        let usage = service.current();
        assert_eq!(usage.claude.problem, Some(UsageProblem::NotInstalled));
        assert_eq!(usage.codex.problem, Some(UsageProblem::NotInstalled));
        assert_eq!(fakes.calls("security"), 0);
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
    fn a_missing_claude_and_codex_are_not_installed() {
        let (_fakes, service, mut poller) = poller_setup("missing");
        poller.claude = Some("/nonexistent/claude".to_owned());
        poller.codex = Some("/nonexistent/codex".to_owned());

        poller.poll();

        let usage = service.current();
        assert_eq!(usage.claude.problem, Some(UsageProblem::NotInstalled));
        assert_eq!(usage.codex.problem, Some(UsageProblem::NotInstalled));
    }

    #[test]
    fn a_failed_codex_read_keeps_the_last_windows() {
        let (fakes, service, mut poller) = poller_setup("keeps");
        poller.poll();
        fakes.write(
            "answer",
            "{\"id\":2,\"error\":{\"message\":\"codex account authentication required\"}}\n",
        );
        poller.codex_due = None;

        poller.poll();

        let codex = service.current().codex;
        assert_eq!(codex.windows, [window("5h", 12, 99)]);
        assert_eq!(codex.problem, Some(UsageProblem::NotLoggedIn));
    }
}
