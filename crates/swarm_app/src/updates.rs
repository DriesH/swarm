//! The updater in the app (PLAN.md, Updates): when to check, what the status says, and the
//! restart that installs a prepared update.
//!
//! The network, the signature check and the install helper sit behind [`UpdateSource`] and
//! [`Host`], so tests drive the state machine with fakes. The timers are GPUI's, which tests
//! move with `advance_clock`.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, AppContext as _, Context, Entity, Global, SharedString, Subscription, Task};
use swarm_update::{
    DeveloperIdVerifier, Fetcher, MANIFEST_URL, Manifest, Offer, Prepared, REPO_PREFIX,
    RunningBuild, SignatureVerifier, SystemFetcher, Version, evaluate, macos_version,
    parse_manifest, user_agent,
};

use crate::workspace::Workspace;

/// The first check comes this long after launch.
const FIRST_CHECK: Duration = Duration::from_secs(30);

/// The time between two scheduled checks.
const CHECK_EVERY: Duration = Duration::from_hours(6);

/// The longest reason the status shows.
const REASON_LIMIT: usize = 80;

/// Where the status says that this build does not update.
const UNSIGNED: &str = "Updates need a signed build";

/// Reads the manifest and prepares the new app. Both block, so the updater calls them from
/// the background executor.
pub(crate) trait UpdateSource: Send + Sync + 'static {
    /// The newest release.
    fn latest(&self) -> swarm_update::Result<Manifest>;

    /// Downloads, checks and unpacks the release of `manifest`.
    fn prepare(&self, manifest: &Manifest) -> swarm_update::Result<Prepared>;

    /// The macOS this process runs on.
    fn macos(&self) -> Version;
}

/// The real source: GitHub Releases, checked with the Developer ID requirement.
struct SystemSource<F, V> {
    fetcher: F,
    verifier: V,
    cache_root: PathBuf,
}

impl<F, V> UpdateSource for SystemSource<F, V>
where
    F: Fetcher + Send + Sync + 'static,
    V: SignatureVerifier + Send + Sync + 'static,
{
    fn latest(&self) -> swarm_update::Result<Manifest> {
        let body = self.fetcher.get(MANIFEST_URL, &user_agent())?;

        parse_manifest(&body, REPO_PREFIX)
    }

    fn prepare(&self, manifest: &Manifest) -> swarm_update::Result<Prepared> {
        swarm_update::prepare(manifest, &self.cache_root, &self.fetcher, &self.verifier)
    }

    fn macos(&self) -> Version {
        macos_version()
    }
}

/// What the updater asks of the machine it runs on: the folder check, the install helper
/// and the quit.
pub(crate) trait Host {
    fn can_replace(&self, bundle: &Path) -> bool;

    fn launch_helper(
        &self,
        prepared: &Prepared,
        bundle: &Path,
        pid: u32,
        reopen: bool,
    ) -> swarm_update::Result<()>;

    fn quit(&self, cx: &mut App);
}

struct SystemHost;

impl Host for SystemHost {
    fn can_replace(&self, bundle: &Path) -> bool {
        swarm_update::can_replace(bundle)
    }

    fn launch_helper(
        &self,
        prepared: &Prepared,
        bundle: &Path,
        pid: u32,
        reopen: bool,
    ) -> swarm_update::Result<()> {
        swarm_update::launch_helper(prepared, bundle, pid, reopen)
    }

    fn quit(&self, cx: &mut App) {
        cx.quit();
    }
}

/// Where the updater stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UpdateState {
    /// The last check found nothing newer.
    Idle,
    Checking,
    Downloading(Version),
    Ready(Prepared),
    Failed(SharedString),
    NeedsMacos {
        version: Version,
        needs: Version,
    },
    /// This build is not a signed bundle, so it never checks.
    Unsigned,
}

impl UpdateState {
    /// The one status line of Settings › General.
    pub(crate) fn status(&self) -> SharedString {
        match self {
            Self::Idle => "Up to date".into(),
            Self::Checking => "Checking\u{2026}".into(),
            Self::Downloading(version) => format!("Downloading {version}\u{2026}").into(),
            Self::Ready(prepared) => format!("Update ready: {}", prepared.version).into(),
            Self::Failed(reason) => format!("Check failed: {reason}").into(),
            Self::NeedsMacos { version, needs } => {
                format!("Swarm {version} needs macOS {}", macos_text(*needs)).into()
            }
            Self::Unsigned => UNSIGNED.into(),
        }
    }
}

/// `14.0`, or `14.0.1` when the patch number matters.
fn macos_text(version: Version) -> String {
    if version.patch == 0 {
        format!("{}.{}", version.major, version.minor)
    } else {
        version.to_string()
    }
}

/// The first line of the error, cut short.
fn short_reason(error: &swarm_update::Error) -> SharedString {
    let text = error.to_string();
    let line = text.lines().next().unwrap_or_default();

    if line.chars().count() <= REASON_LIMIT {
        return line.to_owned().into();
    }

    let cut: String = line.chars().take(REASON_LIMIT).collect();

    format!("{}\u{2026}", cut.trim_end()).into()
}

/// What the check found, before the updater acts on it.
enum Found {
    UpToDate,
    NeedsMacos { version: Version, needs: Version },
    Available(Manifest),
}

pub(crate) struct Updater {
    source: Arc<dyn UpdateSource>,
    host: Rc<dyn Host>,
    build: RunningBuild,
    state: UpdateState,
    /// The folder of the app lets the helper replace it. Read when the update is ready.
    replaceable: bool,
    /// The helper runs already, so a quit must not start a second one.
    helper_launched: bool,
    check: Option<Task<()>>,
    schedule: Option<Task<()>>,
    _quit: Subscription,
}

impl Updater {
    fn new(
        source: Arc<dyn UpdateSource>,
        host: Rc<dyn Host>,
        build: RunningBuild,
        cx: &mut Context<Self>,
    ) -> Self {
        let state = if build.signed_by_team && build.bundle.is_some() {
            UpdateState::Idle
        } else {
            UpdateState::Unsigned
        };

        let quit = cx.on_app_quit(|this, _| {
            this.launch_on_quit();

            std::future::ready(())
        });

        Self {
            source,
            host,
            build,
            state,
            replaceable: false,
            helper_launched: false,
            check: None,
            schedule: None,
            _quit: quit,
        }
    }

    /// Checks 30 seconds from now, then every 6 hours, while the setting is on.
    fn start(&mut self, cx: &mut Context<Self>) {
        self.schedule = Some(cx.spawn(async move |this, cx| {
            let mut wait = FIRST_CHECK;

            loop {
                cx.background_executor().timer(wait).await;
                wait = CHECK_EVERY;

                if this.update(cx, Self::scheduled_check).is_err() {
                    break;
                }
            }
        }));
    }

    fn scheduled_check(&mut self, cx: &mut Context<Self>) {
        if crate::settings::auto_check_updates(cx) {
            self.check(cx);
        }
    }

    /// Reads the manifest, and downloads the update when there is one. Does nothing while a
    /// check runs or an update is ready, and never on a build that cannot update.
    pub(crate) fn check(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.state,
            UpdateState::Unsigned
                | UpdateState::Checking
                | UpdateState::Downloading(_)
                | UpdateState::Ready(_)
        ) {
            return;
        }

        self.state = UpdateState::Checking;
        cx.notify();

        let source = Arc::clone(&self.source);
        let running = self.build.version;

        self.check = Some(cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn({
                    let source = Arc::clone(&source);

                    async move {
                        let manifest = source.latest()?;

                        Ok(match evaluate(&manifest, &running, &source.macos()) {
                            Offer::UpToDate => Found::UpToDate,
                            Offer::NeedsNewerMacos { needs } => Found::NeedsMacos {
                                version: manifest.version,
                                needs,
                            },
                            Offer::Available(manifest) => Found::Available(manifest),
                        })
                    }
                })
                .await;

            let Ok(Some(manifest)) = this.update(cx, |this, cx| this.checked(found, cx)) else {
                return;
            };

            let prepared = cx
                .background_executor()
                .spawn(async move { source.prepare(&manifest) })
                .await;

            this.update(cx, |this, cx| this.prepared(prepared, cx)).ok();
        }));
    }

    /// Takes the answer of the check. Returns the release to download, when there is one.
    fn checked(
        &mut self,
        found: swarm_update::Result<Found>,
        cx: &mut Context<Self>,
    ) -> Option<Manifest> {
        let mut download = None;

        self.state = match found {
            Ok(Found::UpToDate) => UpdateState::Idle,
            Ok(Found::NeedsMacos { version, needs }) => UpdateState::NeedsMacos { version, needs },
            Ok(Found::Available(manifest)) => {
                let state = UpdateState::Downloading(manifest.version);
                download = Some(manifest);

                state
            }
            Err(error) => Self::failed(&error),
        };
        cx.notify();

        download
    }

    fn prepared(&mut self, prepared: swarm_update::Result<Prepared>, cx: &mut Context<Self>) {
        self.state = match prepared {
            Ok(prepared) => {
                self.replaceable = self
                    .build
                    .bundle
                    .as_deref()
                    .is_some_and(|bundle| self.host.can_replace(bundle));

                UpdateState::Ready(prepared)
            }
            Err(error) => Self::failed(&error),
        };
        cx.notify();
    }

    fn failed(error: &swarm_update::Error) -> UpdateState {
        log::warn!("update check failed: {error}");

        UpdateState::Failed(short_reason(error))
    }

    /// Starts the helper that swaps the app, then quits. An app whose folder is read-only
    /// opens the release notes instead, where the download is.
    pub(crate) fn restart_to_update(&mut self, cx: &mut Context<Self>) {
        let UpdateState::Ready(prepared) = self.state.clone() else {
            return;
        };

        if !self.replaceable {
            cx.open_url(&prepared.notes_url);
            return;
        }

        let Some(bundle) = self.build.bundle.clone() else {
            return;
        };

        match self
            .host
            .launch_helper(&prepared, &bundle, std::process::id(), true)
        {
            Ok(()) => {
                self.helper_launched = true;
                self.host.quit(cx);
            }
            Err(error) => {
                self.state = Self::failed(&error);
                cx.notify();
            }
        }
    }

    /// A normal quit with an update ready: the helper installs it and does not reopen the app.
    fn launch_on_quit(&mut self) {
        let UpdateState::Ready(prepared) = &self.state else {
            return;
        };
        let Some(bundle) = &self.build.bundle else {
            return;
        };

        if self.helper_launched || !self.replaceable {
            return;
        }

        match self
            .host
            .launch_helper(prepared, bundle, std::process::id(), false)
        {
            Ok(()) => self.helper_launched = true,
            Err(error) => log::error!("cannot start the install helper: {error}"),
        }
    }
}

struct Updates(Entity<Updater>);

impl Global for Updates {}

/// The updater, once the app started it.
pub(crate) fn updater(cx: &App) -> Option<Entity<Updater>> {
    cx.try_global::<Updates>().map(|updates| updates.0.clone())
}

/// Starts the updater of the running app: GitHub Releases and the Developer ID check.
pub(crate) fn install(workspace: Entity<Workspace>, cx: &mut App) {
    let Some(home) = std::env::var_os("HOME") else {
        log::warn!("no HOME, so no updates");
        return;
    };

    let source = SystemSource {
        fetcher: SystemFetcher::new(),
        verifier: DeveloperIdVerifier,
        cache_root: PathBuf::from(home).join("Library/Caches/Swarm"),
    };

    start(
        Arc::new(source),
        Rc::new(SystemHost),
        swarm_update::running_build(),
        workspace,
        cx,
    );
}

fn start(
    source: Arc<dyn UpdateSource>,
    host: Rc<dyn Host>,
    build: RunningBuild,
    workspace: Entity<Workspace>,
    cx: &mut App,
) -> Entity<Updater> {
    let updater = cx.new(|cx| Updater::new(source, host, build, cx));

    cx.set_global(Updates(updater.clone()));
    cx.observe(&updater, move |_, cx| {
        workspace.update(cx, |_, cx| cx.notify());
    })
    .detach();
    updater.update(cx, Updater::start);

    updater
}

/// Checks now, whatever the setting says.
pub(crate) fn check_now(cx: &mut App) {
    if let Some(updater) = updater(cx) {
        updater.update(cx, Updater::check);
    }
}

pub(crate) fn restart_to_update(cx: &mut App) {
    if let Some(updater) = updater(cx) {
        updater.update(cx, Updater::restart_to_update);
    }
}

/// The status line. Without an updater the app cannot update.
pub(crate) fn status(cx: &App) -> SharedString {
    updater(cx).map_or_else(
        || UNSIGNED.into(),
        |updater| updater.read(cx).state.status(),
    )
}

/// What the status bar shows for an update that is ready.
pub(crate) struct ReadyUpdate {
    pub(crate) notes_url: SharedString,
    /// The app can replace itself. Else the button opens the release notes.
    pub(crate) replaceable: bool,
}

pub(crate) fn ready(cx: &App) -> Option<ReadyUpdate> {
    let updater = updater(cx)?;
    let updater = updater.read(cx);

    match &updater.state {
        UpdateState::Ready(prepared) => Some(ReadyUpdate {
            notes_url: prepared.notes_url.clone().into(),
            replaceable: updater.replaceable,
        }),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use gpui::TestAppContext;

    use super::*;

    pub(crate) const RUNNING: Version = Version::new(1, 0, 0);

    pub(crate) fn manifest(version: Version, minimum_macos: Version) -> Manifest {
        Manifest {
            version,
            url: format!("{REPO_PREFIX}releases/download/v{version}/Swarm-{version}.zip"),
            sha256: "0".repeat(64),
            minimum_macos,
            published: "2026-12-01T10:00:00Z".to_owned(),
            notes_url: format!("{REPO_PREFIX}releases/tag/v{version}"),
        }
    }

    pub(crate) fn prepared(version: Version) -> Prepared {
        Prepared {
            version,
            app: PathBuf::from(format!("/cache/updates/{version}/Swarm.app")),
            notes_url: format!("{REPO_PREFIX}releases/tag/v{version}"),
        }
    }

    pub(crate) fn offline() -> swarm_update::Error {
        swarm_update::Error::Fetch("offline".to_owned())
    }

    /// Answers the checks in order, then says the app is up to date.
    pub(crate) struct FakeSource {
        manifests: Mutex<VecDeque<swarm_update::Result<Manifest>>>,
        prepares: Mutex<VecDeque<swarm_update::Result<Prepared>>>,
        pub(crate) fetches: AtomicUsize,
        pub(crate) macos: Version,
    }

    impl FakeSource {
        pub(crate) fn new(
            manifests: Vec<swarm_update::Result<Manifest>>,
            prepares: Vec<swarm_update::Result<Prepared>>,
        ) -> Arc<Self> {
            Arc::new(Self {
                manifests: Mutex::new(manifests.into()),
                prepares: Mutex::new(prepares.into()),
                fetches: AtomicUsize::new(0),
                macos: Version::new(15, 0, 0),
            })
        }

        /// A release 1.2.0 that downloads fine.
        pub(crate) fn with_update() -> Arc<Self> {
            let version = Version::new(1, 2, 0);

            Self::new(
                vec![Ok(manifest(version, Version::new(13, 0, 0)))],
                vec![Ok(prepared(version))],
            )
        }

        pub(crate) fn fetched(&self) -> usize {
            self.fetches.load(Ordering::SeqCst)
        }
    }

    impl UpdateSource for FakeSource {
        fn latest(&self) -> swarm_update::Result<Manifest> {
            self.fetches.fetch_add(1, Ordering::SeqCst);

            self.manifests
                .lock()
                .expect("manifests")
                .pop_front()
                .unwrap_or_else(|| Ok(manifest(RUNNING, Version::new(13, 0, 0))))
        }

        fn prepare(&self, manifest: &Manifest) -> swarm_update::Result<Prepared> {
            self.prepares
                .lock()
                .expect("prepares")
                .pop_front()
                .unwrap_or_else(|| Ok(prepared(manifest.version)))
        }

        fn macos(&self) -> Version {
            self.macos
        }
    }

    /// Records the helper launches and the quits.
    pub(crate) struct FakeHost {
        pub(crate) replaceable: bool,
        pub(crate) launches: RefCell<Vec<(PathBuf, bool)>>,
        pub(crate) quits: Cell<usize>,
    }

    impl FakeHost {
        pub(crate) fn new(replaceable: bool) -> Rc<Self> {
            Rc::new(Self {
                replaceable,
                launches: RefCell::new(Vec::new()),
                quits: Cell::new(0),
            })
        }
    }

    impl Host for FakeHost {
        fn can_replace(&self, _: &Path) -> bool {
            self.replaceable
        }

        fn launch_helper(
            &self,
            prepared: &Prepared,
            _: &Path,
            pid: u32,
            reopen: bool,
        ) -> swarm_update::Result<()> {
            assert_eq!(pid, std::process::id());
            self.launches
                .borrow_mut()
                .push((prepared.app.clone(), reopen));

            Ok(())
        }

        fn quit(&self, _: &mut App) {
            self.quits.set(self.quits.get() + 1);
        }
    }

    /// Starts the updater of the app with fakes.
    pub(crate) fn install_fake(
        workspace: &Entity<Workspace>,
        source: &Arc<FakeSource>,
        host: &Rc<FakeHost>,
        build: RunningBuild,
        cx: &mut App,
    ) -> Entity<Updater> {
        start(
            Arc::clone(source) as Arc<dyn UpdateSource>,
            Rc::clone(host) as Rc<dyn Host>,
            build,
            workspace.clone(),
            cx,
        )
    }

    pub(crate) fn signed_build() -> RunningBuild {
        RunningBuild {
            version: RUNNING,
            bundle: Some(PathBuf::from("/Applications/Swarm.app")),
            signed_by_team: true,
        }
    }

    pub(crate) fn unsigned_build() -> RunningBuild {
        RunningBuild {
            signed_by_team: false,
            ..signed_build()
        }
    }

    fn updater_with(
        source: &Arc<FakeSource>,
        host: &Rc<FakeHost>,
        build: RunningBuild,
        cx: &mut TestAppContext,
    ) -> Entity<Updater> {
        let (source, host) = (Arc::clone(source), Rc::clone(host));
        let updater = cx.new(|cx| Updater::new(source, host, build, cx));
        updater.update(cx, Updater::start);
        cx.run_until_parked();

        updater
    }

    /// Records every state the updater shows.
    fn record(updater: &Entity<Updater>, cx: &mut TestAppContext) -> Rc<RefCell<Vec<UpdateState>>> {
        let seen = Rc::new(RefCell::new(Vec::new()));

        cx.update(|cx| {
            let seen = Rc::clone(&seen);

            cx.observe(updater, move |updater, cx| {
                seen.borrow_mut().push(updater.read(cx).state.clone());
            })
            .detach();
        });

        seen
    }

    fn state_of(updater: &Entity<Updater>, cx: &TestAppContext) -> UpdateState {
        updater.read_with(cx, |updater, _| updater.state.clone())
    }

    fn pass(time: Duration, cx: &mut TestAppContext) {
        cx.executor().advance_clock(time);
        cx.run_until_parked();
    }

    fn check(updater: &Entity<Updater>, cx: &mut TestAppContext) {
        updater.update(cx, Updater::check);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn the_first_check_comes_30_seconds_after_launch(cx: &mut TestAppContext) {
        let source = FakeSource::new(Vec::new(), Vec::new());
        let _updater = updater_with(&source, &FakeHost::new(true), signed_build(), cx);

        pass(FIRST_CHECK.saturating_sub(Duration::from_secs(1)), cx);
        let before = source.fetched();
        pass(Duration::from_secs(1), cx);

        assert_eq!(before, 0);
        assert_eq!(source.fetched(), 1);
    }

    #[gpui::test]
    fn checks_repeat_every_six_hours(cx: &mut TestAppContext) {
        let source = FakeSource::new(Vec::new(), Vec::new());
        let _updater = updater_with(&source, &FakeHost::new(true), signed_build(), cx);

        pass(FIRST_CHECK, cx);
        pass(CHECK_EVERY.saturating_sub(Duration::from_secs(1)), cx);
        let before = source.fetched();
        pass(Duration::from_secs(1), cx);

        assert_eq!(before, 1);
        assert_eq!(source.fetched(), 2);
    }

    #[gpui::test]
    fn scheduled_checks_stop_when_auto_check_is_off(cx: &mut TestAppContext) {
        cx.update(|cx| crate::settings::change_updates(|updates| updates.auto_check = false, cx));
        let source = FakeSource::new(Vec::new(), Vec::new());
        let _updater = updater_with(&source, &FakeHost::new(true), signed_build(), cx);

        pass(FIRST_CHECK, cx);
        pass(CHECK_EVERY, cx);

        assert_eq!(source.fetched(), 0);
    }

    #[gpui::test]
    fn a_manual_check_runs_with_auto_check_off(cx: &mut TestAppContext) {
        cx.update(|cx| crate::settings::change_updates(|updates| updates.auto_check = false, cx));
        let source = FakeSource::new(Vec::new(), Vec::new());
        let updater = updater_with(&source, &FakeHost::new(true), signed_build(), cx);

        check(&updater, cx);

        assert_eq!(source.fetched(), 1);
        assert_eq!(state_of(&updater, cx), UpdateState::Idle);
    }

    #[gpui::test]
    fn two_checks_at_once_fetch_once(cx: &mut TestAppContext) {
        let source = FakeSource::new(Vec::new(), Vec::new());
        let updater = updater_with(&source, &FakeHost::new(true), signed_build(), cx);
        let seen = record(&updater, cx);

        updater.update(cx, Updater::check);
        updater.update(cx, Updater::check);
        cx.run_until_parked();

        let starts = seen
            .borrow()
            .iter()
            .filter(|state| **state == UpdateState::Checking)
            .count();
        assert_eq!(starts, 1, "the second check started over");
        assert_eq!(source.fetched(), 1);
    }

    #[gpui::test]
    fn an_available_update_goes_from_checking_to_downloading_to_ready(cx: &mut TestAppContext) {
        let source = FakeSource::with_update();
        let updater = updater_with(&source, &FakeHost::new(true), signed_build(), cx);
        let seen = record(&updater, cx);

        check(&updater, cx);

        let version = Version::new(1, 2, 0);
        assert_eq!(
            *seen.borrow(),
            [
                UpdateState::Checking,
                UpdateState::Downloading(version),
                UpdateState::Ready(prepared(version)),
            ]
        );
    }

    #[test]
    fn each_state_has_its_status_line() {
        let version = Version::new(1, 2, 0);
        let lines = [
            (UpdateState::Idle, "Up to date"),
            (UpdateState::Checking, "Checking\u{2026}"),
            (
                UpdateState::Downloading(version),
                "Downloading 1.2.0\u{2026}",
            ),
            (UpdateState::Ready(prepared(version)), "Update ready: 1.2.0"),
            (
                UpdateState::NeedsMacos {
                    version,
                    needs: Version::new(14, 0, 0),
                },
                "Swarm 1.2.0 needs macOS 14.0",
            ),
            (UpdateState::Unsigned, "Updates need a signed build"),
            (
                UpdateState::Failed("offline".into()),
                "Check failed: offline",
            ),
        ];

        for (state, line) in lines {
            assert_eq!(state.status(), line);
        }
    }

    #[gpui::test]
    fn a_release_that_needs_a_newer_macos_is_not_downloaded(cx: &mut TestAppContext) {
        let version = Version::new(1, 2, 0);
        let source = FakeSource::new(
            vec![Ok(manifest(version, Version::new(16, 0, 0)))],
            Vec::new(),
        );
        let updater = updater_with(&source, &FakeHost::new(true), signed_build(), cx);

        check(&updater, cx);

        assert_eq!(
            state_of(&updater, cx),
            UpdateState::NeedsMacos {
                version,
                needs: Version::new(16, 0, 0)
            }
        );
    }

    #[gpui::test]
    fn a_release_that_is_not_newer_shows_nothing(cx: &mut TestAppContext) {
        let source = FakeSource::new(
            vec![Ok(manifest(RUNNING, Version::new(13, 0, 0)))],
            Vec::new(),
        );
        let updater = updater_with(&source, &FakeHost::new(true), signed_build(), cx);

        check(&updater, cx);

        assert_eq!(state_of(&updater, cx), UpdateState::Idle);
    }

    #[gpui::test]
    fn a_failed_check_shows_the_reason_and_the_next_check_retries(cx: &mut TestAppContext) {
        let version = Version::new(1, 2, 0);
        let source = FakeSource::new(
            vec![
                Err(offline()),
                Ok(manifest(version, Version::new(13, 0, 0))),
            ],
            Vec::new(),
        );
        let updater = updater_with(&source, &FakeHost::new(true), signed_build(), cx);

        check(&updater, cx);
        let failed = state_of(&updater, cx);
        check(&updater, cx);

        assert_eq!(
            failed,
            UpdateState::Failed("download failed: offline".into())
        );
        assert_eq!(source.fetched(), 2);
        assert_eq!(
            state_of(&updater, cx),
            UpdateState::Ready(prepared(version))
        );
    }

    #[gpui::test]
    fn a_failed_download_shows_the_reason(cx: &mut TestAppContext) {
        let version = Version::new(1, 2, 0);
        let source = FakeSource::new(
            vec![Ok(manifest(version, Version::new(13, 0, 0)))],
            vec![Err(swarm_update::Error::HashMismatch)],
        );
        let updater = updater_with(&source, &FakeHost::new(true), signed_build(), cx);

        check(&updater, cx);

        assert_eq!(
            state_of(&updater, cx).status(),
            "Check failed: the download does not match the SHA-256 of the manifest"
        );
    }

    #[gpui::test]
    fn a_long_reason_is_cut_short(cx: &mut TestAppContext) {
        let source = FakeSource::new(
            vec![Err(swarm_update::Error::Fetch("x".repeat(300)))],
            Vec::new(),
        );
        let updater = updater_with(&source, &FakeHost::new(true), signed_build(), cx);

        check(&updater, cx);

        let UpdateState::Failed(reason) = state_of(&updater, cx) else {
            panic!("the check should fail");
        };
        assert!(reason.chars().count() <= REASON_LIMIT + 1, "{reason}");
        assert!(reason.ends_with('\u{2026}'));
    }

    #[gpui::test]
    fn an_unsigned_build_never_fetches(cx: &mut TestAppContext) {
        let source = FakeSource::with_update();
        let updater = updater_with(&source, &FakeHost::new(true), unsigned_build(), cx);

        pass(FIRST_CHECK, cx);
        check(&updater, cx);

        assert_eq!(source.fetched(), 0);
        assert_eq!(state_of(&updater, cx), UpdateState::Unsigned);
    }

    #[gpui::test]
    fn a_build_with_no_bundle_never_fetches(cx: &mut TestAppContext) {
        let source = FakeSource::with_update();
        let build = RunningBuild {
            bundle: None,
            ..signed_build()
        };
        let updater = updater_with(&source, &FakeHost::new(true), build, cx);

        check(&updater, cx);

        assert_eq!(source.fetched(), 0);
        assert_eq!(state_of(&updater, cx), UpdateState::Unsigned);
    }

    #[gpui::test]
    fn restart_to_update_starts_the_helper_with_reopen_and_quits(cx: &mut TestAppContext) {
        let source = FakeSource::with_update();
        let host = FakeHost::new(true);
        let updater = updater_with(&source, &host, signed_build(), cx);
        check(&updater, cx);

        updater.update(cx, Updater::restart_to_update);

        assert_eq!(
            *host.launches.borrow(),
            [(prepared(Version::new(1, 2, 0)).app, true)]
        );
        assert_eq!(host.quits.get(), 1);
    }

    #[gpui::test]
    fn an_app_that_cannot_be_replaced_opens_the_release_notes(cx: &mut TestAppContext) {
        let source = FakeSource::with_update();
        let host = FakeHost::new(false);
        let updater = updater_with(&source, &host, signed_build(), cx);
        check(&updater, cx);

        updater.update(cx, Updater::restart_to_update);

        assert!(host.launches.borrow().is_empty());
        assert_eq!(host.quits.get(), 0);
        assert_eq!(
            cx.opened_url().as_deref(),
            Some("https://github.com/DriesH/swarm/releases/tag/v1.2.0")
        );
    }

    #[gpui::test]
    fn quitting_with_an_update_ready_starts_the_helper_without_reopen(cx: &mut TestAppContext) {
        let source = FakeSource::with_update();
        let host = FakeHost::new(true);
        let updater = updater_with(&source, &host, signed_build(), cx);
        check(&updater, cx);

        cx.update(App::shutdown);

        assert_eq!(
            *host.launches.borrow(),
            [(prepared(Version::new(1, 2, 0)).app, false)]
        );
    }

    #[gpui::test]
    fn quitting_after_restart_to_update_does_not_start_a_second_helper(cx: &mut TestAppContext) {
        let source = FakeSource::with_update();
        let host = FakeHost::new(true);
        let updater = updater_with(&source, &host, signed_build(), cx);
        check(&updater, cx);
        updater.update(cx, Updater::restart_to_update);

        cx.update(App::shutdown);

        assert_eq!(host.launches.borrow().len(), 1);
    }

    #[gpui::test]
    fn quitting_without_an_update_starts_no_helper(cx: &mut TestAppContext) {
        let source = FakeSource::new(Vec::new(), Vec::new());
        let host = FakeHost::new(true);
        let _updater = updater_with(&source, &host, signed_build(), cx);

        cx.update(App::shutdown);

        assert!(host.launches.borrow().is_empty());
    }

    #[gpui::test]
    fn quitting_with_an_app_that_cannot_be_replaced_starts_no_helper(cx: &mut TestAppContext) {
        let source = FakeSource::with_update();
        let host = FakeHost::new(false);
        let updater = updater_with(&source, &host, signed_build(), cx);
        check(&updater, cx);

        cx.update(App::shutdown);

        assert!(host.launches.borrow().is_empty());
    }

    struct Fetched(Vec<u8>);

    impl Fetcher for Fetched {
        fn get(&self, url: &str, user_agent: &str) -> swarm_update::Result<Vec<u8>> {
            assert_eq!(url, MANIFEST_URL);
            assert_eq!(user_agent, swarm_update::user_agent());

            Ok(self.0.clone())
        }

        fn download(&self, _: &str, _: &str, _: &Path) -> swarm_update::Result<()> {
            Err(offline())
        }
    }

    struct Accepts;

    impl SignatureVerifier for Accepts {
        fn verify(&self, _: &Path, _: &Version) -> swarm_update::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_system_source_reads_the_manifest_with_our_user_agent_and_prefix() {
        let json = format!(
            r#"{{"version":"1.2.0","url":"{REPO_PREFIX}releases/download/v1.2.0/Swarm-1.2.0.zip",
            "sha256":"{}","minimum_macos":"13.0","published":"2026-12-01T10:00:00Z",
            "notes_url":"{REPO_PREFIX}releases/tag/v1.2.0"}}"#,
            "0".repeat(64)
        );
        let source = SystemSource {
            fetcher: Fetched(json.into_bytes()),
            verifier: Accepts,
            cache_root: PathBuf::from("/nonexistent"),
        };

        let latest = source.latest().map(|manifest| manifest.version);

        assert_eq!(latest.ok(), Some(Version::new(1, 2, 0)));
    }

    #[test]
    fn the_system_source_ignores_a_manifest_that_points_elsewhere() {
        let json = format!(
            r#"{{"version":"1.2.0","url":"https://example.test/Swarm.zip","sha256":"{}",
            "minimum_macos":"13.0","published":"2026-12-01T10:00:00Z",
            "notes_url":"{REPO_PREFIX}releases/tag/v1.2.0"}}"#,
            "0".repeat(64)
        );
        let source = SystemSource {
            fetcher: Fetched(json.into_bytes()),
            verifier: Accepts,
            cache_root: PathBuf::from("/nonexistent"),
        };

        assert!(source.latest().is_err());
    }
}
