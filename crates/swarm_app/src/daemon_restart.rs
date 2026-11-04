//! Replacing a daemon that is older than the app: the check after connecting, the
//! Restart daemon dialog and the restart itself.

use gpui::{AppContext as _, Context, EventEmitter, FocusHandle, Window, div, px};
use swarm_protocol::{AgentInfo, AgentLifecycle, AgentStatus, Client, ClientError, ProjectInfo};
use swarm_ui::prelude::*;

use crate::palette::Overlay;
use crate::workspace::Workspace;

/// The app's own build version, which the daemon next to it shares.
const APP_BUILD: &str = env!("CARGO_PKG_VERSION");
const LINE_WITH_RESUME: &str =
    "Every agent terminal stops. Agents with a saved conversation can resume.";
const LINE: &str = "Every agent terminal stops.";

/// A daemon the app should replace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StaleDaemon {
    /// The daemon's build version, when it told.
    pub(crate) build: Option<String>,
    /// How many agents run. `None` when the app cannot talk to the daemon, so it
    /// cannot count them.
    pub(crate) running: Option<usize>,
}

impl StaleDaemon {
    /// A daemon the app cannot talk to, from the error that said so.
    pub(crate) fn from_error(error: &ClientError) -> Option<Self> {
        match error {
            ClientError::Version { build, .. } => Some(Self {
                build: build.clone(),
                running: None,
            }),
            ClientError::Incompatible => Some(Self {
                build: None,
                running: None,
            }),
            _ => None,
        }
    }

    /// The sidebar's status text, for a daemon the app still works with.
    pub(crate) fn status(&self) -> Option<String> {
        let (build, running) = (self.build.as_ref()?, self.running?);
        let agents = if running == 1 { "agent" } else { "agents" };

        Some(format!("Daemon {build} \u{b7} {running} {agents} running"))
    }

    fn dialog_line(&self) -> &'static str {
        if self.running.is_some() {
            LINE_WITH_RESUME
        } else {
            LINE
        }
    }
}

/// What the daemon's agents are doing, from the projects it listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AgentLoad {
    running: usize,
    /// An agent works or starts, so a restart would cut it off.
    busy: bool,
}

impl AgentLoad {
    fn of(projects: &[ProjectInfo]) -> Self {
        let agents = projects.iter().flat_map(|project| {
            let tasks = project.tasks.iter().flat_map(|task| task.agents.iter());

            project.start.iter().chain(tasks)
        });

        let mut load = Self {
            running: 0,
            busy: false,
        };
        for agent in agents {
            load.add(agent);
        }

        load
    }

    fn add(&mut self, agent: &AgentInfo) {
        match agent.lifecycle {
            AgentLifecycle::Starting => {
                self.running += 1;
                self.busy = true;
            }
            AgentLifecycle::Running => {
                self.running += 1;
                self.busy |= agent.status == AgentStatus::Working;
            }
            AgentLifecycle::Resumable | AgentLifecycle::Stopped => {}
        }
    }
}

/// True when `daemon` is a lower version than `app`. A version that does not read as
/// `major.minor.patch` counts as older when it differs.
fn is_older(daemon: &str, app: &str) -> bool {
    match (parse_build(daemon), parse_build(app)) {
        (Some(daemon), Some(app)) => daemon < app,
        _ => daemon != app,
    }
}

fn parse_build(build: &str) -> Option<(u64, u64, u64)> {
    let numbers = build.split(['-', '+']).next()?;
    let mut parts = numbers.split('.');

    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;

    parts.next().is_none().then_some((major, minor, patch))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RestartDaemonEvent {
    Dismissed,
    Confirmed,
}

/// The open dialog. It reports what the user chose and leaves the rest to the workspace.
pub(crate) struct RestartDaemonDialog {
    line: &'static str,
    focus_handle: FocusHandle,
    cancel_focus: FocusHandle,
    /// What had the focus before the dialog opened.
    pub(crate) opener: Option<FocusHandle>,
}

impl EventEmitter<RestartDaemonEvent> for RestartDaemonDialog {}

impl gpui::Focusable for RestartDaemonDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl RestartDaemonDialog {
    fn new(line: &'static str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let opener = window.focused(cx);
        let cancel_focus = cx.focus_handle().tab_stop(true);
        window.focus(&cancel_focus, cx);

        Self {
            line,
            focus_handle: cx.focus_handle(),
            cancel_focus,
            opener,
        }
    }

    #[cfg(test)]
    fn cancel_focus(&self) -> &FocusHandle {
        &self.cancel_focus
    }
}

impl Render for RestartDaemonDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let on_dismiss = cx.entity();

        let info = div()
            .id("restart-daemon-info")
            .role(gpui::Role::Label)
            .aria_label(self.line)
            .child(Label::new(self.line).color(Color::Muted));

        let footer = h_flex()
            .gap(px(8.))
            .child(
                Button::new("restart-daemon-cancel", "Cancel")
                    .variant(ButtonVariant::Secondary)
                    .track_focus(&self.cancel_focus)
                    .debug_selector(|| "restart-daemon-cancel".to_owned())
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(RestartDaemonEvent::Dismissed))),
            )
            .child(
                Button::new("restart-daemon-confirm", "Restart")
                    .variant(ButtonVariant::Destructive)
                    .accessible_label("Restart daemon")
                    .debug_selector(|| "restart-daemon-confirm".to_owned())
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(RestartDaemonEvent::Confirmed))),
            );

        Dialog::new(
            "restart-daemon-dialog",
            "Restart daemon?",
            &self.focus_handle,
        )
        .width(px(420.))
        .on_dismiss(move |_, cx| {
            on_dismiss.update(cx, |_, cx| cx.emit(RestartDaemonEvent::Dismissed));
        })
        .child(info)
        .footer(footer)
    }
}

impl Workspace {
    /// Looks at the build of the daemon just connected. An older one is replaced at
    /// once when no agent works, and offered for replacement otherwise. Returns true
    /// when the restart began, so the caller drops the connection.
    pub(crate) fn check_daemon_build(
        &mut self,
        client: &Client,
        projects: &[ProjectInfo],
        cx: &mut Context<Self>,
    ) -> bool {
        self.stale_daemon = None;

        let build = client.daemon_build();
        // A restart that left an older daemon behind must not restart again and again.
        if self.daemon_restarted || !is_older(build, APP_BUILD) {
            return false;
        }

        let load = AgentLoad::of(projects);
        if !load.busy {
            log::info!("the daemon is build {build}, the app {APP_BUILD}: restarting it");
            self.restart_daemon(cx);

            return true;
        }

        self.stale_daemon = Some(StaleDaemon {
            build: Some(build.to_owned()),
            running: Some(load.running),
        });
        cx.notify();

        false
    }

    /// Asks before the restart ends every agent terminal.
    pub(crate) fn ask_restart_daemon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(line) = self.stale_daemon.as_ref().map(StaleDaemon::dialog_line) else {
            return;
        };

        let view = cx.new(|cx| RestartDaemonDialog::new(line, window, cx));
        let subscription =
            cx.subscribe_in(
                &view,
                window,
                move |this, _, event, window, cx| match event {
                    RestartDaemonEvent::Dismissed => this.close_overlay(window, cx),
                    RestartDaemonEvent::Confirmed => {
                        this.close_overlay(window, cx);
                        this.restart_daemon(cx);
                    }
                },
            );

        self.overlay = Some(Overlay::RestartDaemon {
            view,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Stops the daemon and starts the one next to the app, then connects to it.
    pub(crate) fn restart_daemon(&mut self, cx: &mut Context<Self>) {
        let Some(launch) = self.daemon_launch.clone() else {
            log::warn!("cannot restart the daemon: the app never started a connection");
            return;
        };

        self.daemon_restarted = true;
        // Without a client the connection watcher leaves the reconnect to this restart.
        self.daemon = None;
        self.begin_connect(launch, true, cx);
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, BufWriter, Write};
    use std::os::unix::fs::DirBuilderExt as _;
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
    use swarm_protocol::{
        AgentStatus as DaemonAgentStatus, Event, Host, Paths, Request, Signal, VERSION, read, write,
    };

    use super::*;
    use crate::daemon::{
        DaemonLaunch,
        tests::{agent_info, default_reply, project_info, redraw, task_info},
    };
    use crate::workspace::DataSource;

    static NEXT_FOLDER: AtomicU32 = AtomicU32::new(0);

    /// Lets the test see which signals the app sends instead of sending them.
    struct FakeHost {
        signals: Mutex<Vec<(u32, Signal)>>,
        program: &'static str,
    }

    impl FakeHost {
        fn new(program: &'static str) -> Arc<Self> {
            Arc::new(Self {
                signals: Mutex::new(Vec::new()),
                program,
            })
        }

        fn sent(&self) -> Vec<(u32, Signal)> {
            self.signals.lock().expect("lock").clone()
        }
    }

    impl Host for FakeHost {
        fn socket_owner(&self, _: &Path) -> std::io::Result<u32> {
            Ok(31_337)
        }

        fn executable(&self, _: u32) -> Option<PathBuf> {
            Some(PathBuf::from(self.program))
        }

        fn is_running(&self, pid: u32) -> bool {
            !self
                .signals
                .lock()
                .expect("lock")
                .iter()
                .any(|(signaled, _)| *signaled == pid)
        }

        fn signal(&self, pid: u32, signal: Signal) -> std::io::Result<()> {
            self.signals.lock().expect("lock").push((pid, signal));

            Ok(())
        }

        fn pause(&self) {}
    }

    /// A daemon that serves one connection per entry of `builds`, in order, each with
    /// that build and the same projects. A version of `None` uses the protocol's own.
    fn fake_daemon(builds: Vec<(&'static str, Option<u32>)>, projects: Vec<ProjectInfo>) -> Paths {
        let folder = PathBuf::from(format!(
            "/tmp/swd-{}-{}",
            std::process::id(),
            NEXT_FOLDER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&folder)
            .expect("create the private folder");
        let paths = Paths::in_dir(folder.clone());
        let listener = UnixListener::bind(&paths.socket).expect("bind the fake socket");

        std::thread::spawn(move || {
            for (build, version) in builds {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let projects = projects.clone();

                std::thread::spawn(move || serve(stream, build, version, &projects));
            }
        });

        paths
    }

    fn serve(
        stream: std::os::unix::net::UnixStream,
        build: &str,
        version: Option<u32>,
        projects: &[ProjectInfo],
    ) {
        let Ok(clone) = stream.try_clone() else {
            return;
        };
        let mut reader = BufReader::new(clone);
        let mut writer = BufWriter::new(stream);
        let mut buffer = Vec::new();

        let hello: Result<Request, _> = read(&mut reader, &mut buffer);
        assert_eq!(hello.ok(), Some(Request::Hello { version: VERSION }));
        write(
            &mut writer,
            &Event::Hello {
                version: version.unwrap_or(VERSION),
                pid: 31_337,
                build: build.to_owned(),
            },
        )
        .ok();
        if version.is_some() {
            writer.flush().ok();
            return;
        }

        write(
            &mut writer,
            &Event::Challenge {
                nonce: "fake".to_owned(),
            },
        )
        .ok();
        writer.flush().ok();
        let proof: Result<Request, _> = read(&mut reader, &mut buffer);
        assert!(matches!(proof, Ok(Request::Prove { .. })));

        while let Ok(request) = read::<Request>(&mut reader, &mut buffer) {
            let reply = match request {
                Request::ListProjects { call } => Some(Event::Projects {
                    call,
                    projects: projects.to_vec(),
                }),
                other => default_reply(&other),
            };

            if let Some(reply) = reply {
                write(&mut writer, &reply).ok();
                writer.flush().ok();
            }
        }
    }

    fn launch(paths: Paths, host: &Arc<FakeHost>) -> DaemonLaunch {
        DaemonLaunch {
            paths,
            binary: PathBuf::from("/nonexistent/swarm-daemon"),
            host: Arc::clone(host) as Arc<dyn Host>,
        }
    }

    /// One project with one task whose orchestrator has `status`.
    fn busy_with(status: DaemonAgentStatus) -> Vec<ProjectInfo> {
        let mut task = task_info(7, 5, "login");
        task.agents = vec![agent_info(1, 7, status)];

        vec![project_info(5, "swarm", vec![task])]
    }

    fn connect(workspace: &Entity<Workspace>, launch: DaemonLaunch, cx: &mut VisualTestContext) {
        cx.executor().allow_parking();
        workspace.update(cx, |workspace, cx| {
            workspace.begin_connect(launch, false, cx);
        });
        cx.run_until_parked();
        redraw(cx);
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"));

        cx.simulate_click(bounds.center(), Modifiers::none());
        redraw(cx);
    }

    fn dialog(
        workspace: &Entity<Workspace>,
        cx: &VisualTestContext,
    ) -> Option<Entity<RestartDaemonDialog>> {
        workspace.read_with(cx, |workspace, _| match &workspace.overlay {
            Some(Overlay::RestartDaemon { view, .. }) => Some(view.clone()),
            _ => None,
        })
    }

    fn setup_real(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
        cx.update(|cx| {
            swarm_ui::init(cx);
            crate::actions::bind_keys(cx);
        });

        let (workspace, cx) = cx.add_window_view(Workspace::new);
        cx.run_until_parked();

        (workspace, cx)
    }

    #[test]
    fn a_lower_version_is_older() {
        assert!(is_older("1.0.9", "1.1.0"));
        assert!(is_older("0.9.0", "1.0.0"));
    }

    #[test]
    fn the_same_or_a_higher_version_is_not_older() {
        assert!(!is_older("1.1.0", "1.1.0"));
        assert!(!is_older("1.2.0", "1.1.0"));
        assert!(!is_older("2.0.0", "1.9.9"));
    }

    #[test]
    fn a_version_compares_as_numbers_not_as_text() {
        assert!(is_older("1.9.0", "1.10.0"));
    }

    #[test]
    fn a_version_that_does_not_parse_is_older_when_it_differs() {
        assert!(is_older("dev", "1.1.0"));
        assert!(!is_older("dev", "dev"));
    }

    #[test]
    fn the_status_text_names_the_build_and_counts_the_agents() {
        let stale = StaleDaemon {
            build: Some("1.1.0".to_owned()),
            running: Some(3),
        };

        assert_eq!(
            stale.status().as_deref(),
            Some("Daemon 1.1.0 \u{b7} 3 agents running")
        );
    }

    #[test]
    fn one_agent_is_singular_in_the_status_text() {
        let stale = StaleDaemon {
            build: Some("1.1.0".to_owned()),
            running: Some(1),
        };

        assert_eq!(
            stale.status().as_deref(),
            Some("Daemon 1.1.0 \u{b7} 1 agent running")
        );
    }

    #[test]
    fn a_daemon_the_app_cannot_count_has_no_status_text() {
        let stale = StaleDaemon {
            build: Some("1.1.0".to_owned()),
            running: None,
        };

        assert_eq!(stale.status(), None);
    }

    #[test]
    fn a_working_agent_makes_the_load_busy() {
        let load = AgentLoad::of(&busy_with(DaemonAgentStatus::Working));

        assert_eq!(
            load,
            AgentLoad {
                running: 1,
                busy: true
            }
        );
    }

    #[test]
    fn an_idle_agent_still_counts_as_running_but_not_busy() {
        let load = AgentLoad::of(&busy_with(DaemonAgentStatus::Idle));

        assert_eq!(
            load,
            AgentLoad {
                running: 1,
                busy: false
            }
        );
    }

    #[test]
    fn a_starting_agent_makes_the_load_busy() {
        let mut projects = busy_with(DaemonAgentStatus::Idle);
        projects[0].tasks[0].agents[0].lifecycle = AgentLifecycle::Starting;

        assert!(AgentLoad::of(&projects).busy);
    }

    #[test]
    fn an_ended_agent_that_still_says_working_is_not_busy() {
        let mut projects = busy_with(DaemonAgentStatus::Working);
        projects[0].tasks[0].agents[0].lifecycle = AgentLifecycle::Resumable;

        assert_eq!(
            AgentLoad::of(&projects),
            AgentLoad {
                running: 0,
                busy: false
            }
        );
    }

    #[gpui::test]
    fn an_older_daemon_with_no_working_agent_restarts_at_once(cx: &mut TestAppContext) {
        let (workspace, cx) = setup_real(cx);
        let host = FakeHost::new("/x/swarm-daemon");
        let paths = fake_daemon(
            vec![("0.0.1", None), (APP_BUILD, None)],
            busy_with(DaemonAgentStatus::Idle),
        );

        connect(&workspace, launch(paths, &host), cx);

        let (source, stale, restarted) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.source.clone(),
                workspace.stale_daemon.clone(),
                workspace.daemon_restarted,
            )
        });
        assert_eq!(host.sent(), [(31_337, Signal::Terminate)]);
        assert_eq!((source, stale, restarted), (DataSource::Live, None, true));
    }

    #[gpui::test]
    fn an_older_daemon_with_working_agents_offers_the_restart_and_keeps_working(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup_real(cx);
        let host = FakeHost::new("/x/swarm-daemon");
        let paths = fake_daemon(vec![("0.0.1", None)], busy_with(DaemonAgentStatus::Working));

        connect(&workspace, launch(paths, &host), cx);

        let (source, stale) = workspace.read_with(cx, |workspace, _| {
            (workspace.source.clone(), workspace.stale_daemon.clone())
        });
        assert!(host.sent().is_empty());
        assert_eq!(source, DataSource::Live);
        assert_eq!(
            stale.and_then(|stale| stale.status()).as_deref(),
            Some("Daemon 0.0.1 \u{b7} 1 agent running")
        );
        assert!(cx.debug_bounds("restart-daemon").is_some());
    }

    #[gpui::test]
    fn a_daemon_with_the_apps_build_shows_no_prompt(cx: &mut TestAppContext) {
        let (workspace, cx) = setup_real(cx);
        let host = FakeHost::new("/x/swarm-daemon");
        let paths = fake_daemon(
            vec![(APP_BUILD, None)],
            busy_with(DaemonAgentStatus::Working),
        );

        connect(&workspace, launch(paths, &host), cx);

        let stale = workspace.read_with(cx, |workspace, _| workspace.stale_daemon.clone());
        assert!(host.sent().is_empty());
        assert_eq!(stale, None);
    }

    #[gpui::test]
    fn a_newer_daemon_keeps_working_with_no_prompt(cx: &mut TestAppContext) {
        let (workspace, cx) = setup_real(cx);
        let host = FakeHost::new("/x/swarm-daemon");
        let paths = fake_daemon(
            vec![("999.0.0", None)],
            busy_with(DaemonAgentStatus::Working),
        );

        connect(&workspace, launch(paths, &host), cx);

        let (source, stale) = workspace.read_with(cx, |workspace, _| {
            (workspace.source.clone(), workspace.stale_daemon.clone())
        });
        assert!(host.sent().is_empty());
        assert_eq!((source, stale), (DataSource::Live, None));
    }

    #[gpui::test]
    fn cancel_in_the_dialog_restarts_nothing(cx: &mut TestAppContext) {
        let (workspace, cx) = setup_real(cx);
        let host = FakeHost::new("/x/swarm-daemon");
        let paths = fake_daemon(vec![("0.0.1", None)], busy_with(DaemonAgentStatus::Working));
        connect(&workspace, launch(paths, &host), cx);
        click("restart-daemon", cx);
        assert!(dialog(&workspace, cx).is_some());

        click("restart-daemon-cancel", cx);

        let stale = workspace.read_with(cx, |workspace, _| workspace.stale_daemon.clone());
        assert!(dialog(&workspace, cx).is_none());
        assert!(host.sent().is_empty());
        assert!(stale.is_some());
    }

    #[gpui::test]
    fn escape_in_the_dialog_restarts_nothing(cx: &mut TestAppContext) {
        let (workspace, cx) = setup_real(cx);
        let host = FakeHost::new("/x/swarm-daemon");
        let paths = fake_daemon(vec![("0.0.1", None)], busy_with(DaemonAgentStatus::Working));
        connect(&workspace, launch(paths, &host), cx);
        click("restart-daemon", cx);

        cx.simulate_keystrokes("escape");
        redraw(cx);

        assert!(dialog(&workspace, cx).is_none());
        assert!(host.sent().is_empty());
    }

    #[gpui::test]
    fn the_dialog_starts_with_the_focus_on_cancel(cx: &mut TestAppContext) {
        let (workspace, cx) = setup_real(cx);
        let host = FakeHost::new("/x/swarm-daemon");
        let paths = fake_daemon(vec![("0.0.1", None)], busy_with(DaemonAgentStatus::Working));
        connect(&workspace, launch(paths, &host), cx);

        click("restart-daemon", cx);

        let dialog = dialog(&workspace, cx).expect("the dialog is open");
        let cancel = dialog.read_with(cx, |dialog, _| dialog.cancel_focus().clone());
        assert!(cx.update(|window, _| cancel.is_focused(window)));
    }

    #[gpui::test]
    fn restart_in_the_dialog_stops_the_old_daemon_and_connects_to_a_new_one(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup_real(cx);
        let host = FakeHost::new("/x/swarm-daemon");
        let paths = fake_daemon(
            vec![("0.0.1", None), (APP_BUILD, None)],
            busy_with(DaemonAgentStatus::Working),
        );
        connect(&workspace, launch(paths, &host), cx);
        click("restart-daemon", cx);

        click("restart-daemon-confirm", cx);
        cx.run_until_parked();
        redraw(cx);

        let (source, stale, build) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.source.clone(),
                workspace.stale_daemon.clone(),
                workspace
                    .daemon
                    .as_ref()
                    .map(|client| client.daemon_build().to_owned()),
            )
        });
        assert_eq!(host.sent(), [(31_337, Signal::Terminate)]);
        assert_eq!(source, DataSource::Live);
        assert_eq!(stale, None);
        assert_eq!(build.as_deref(), Some(APP_BUILD));
    }

    #[gpui::test]
    fn another_protocol_shows_restart_daemon_in_place_of_retry(cx: &mut TestAppContext) {
        let (workspace, cx) = setup_real(cx);
        let host = FakeHost::new("/x/swarm-daemon");
        let paths = fake_daemon(vec![("0.0.1", Some(VERSION - 1))], Vec::new());

        connect(&workspace, launch(paths, &host), cx);

        let (source, stale) = workspace.read_with(cx, |workspace, _| {
            (workspace.source.clone(), workspace.stale_daemon.clone())
        });
        assert!(matches!(source, DataSource::Unavailable { .. }));
        assert_eq!(
            stale,
            Some(StaleDaemon {
                build: Some("0.0.1".to_owned()),
                running: None
            })
        );
        assert!(cx.debug_bounds("restart-daemon").is_some());
        assert!(cx.debug_bounds("retry-daemon").is_none());
    }

    #[gpui::test]
    fn the_dialog_for_another_protocol_does_not_promise_a_resume(cx: &mut TestAppContext) {
        let (workspace, cx) = setup_real(cx);
        let host = FakeHost::new("/x/swarm-daemon");
        let paths = fake_daemon(vec![("0.0.1", Some(VERSION - 1))], Vec::new());
        connect(&workspace, launch(paths, &host), cx);

        click("restart-daemon", cx);

        let dialog = dialog(&workspace, cx).expect("the dialog is open");
        assert_eq!(dialog.read_with(cx, |dialog, _| dialog.line), LINE);
    }

    #[gpui::test]
    fn the_dialog_for_a_known_load_says_that_agents_can_resume(cx: &mut TestAppContext) {
        let (workspace, cx) = setup_real(cx);
        let host = FakeHost::new("/x/swarm-daemon");
        let paths = fake_daemon(vec![("0.0.1", None)], busy_with(DaemonAgentStatus::Working));
        connect(&workspace, launch(paths, &host), cx);

        click("restart-daemon", cx);

        let dialog = dialog(&workspace, cx).expect("the dialog is open");
        assert_eq!(
            dialog.read_with(cx, |dialog, _| dialog.line),
            LINE_WITH_RESUME
        );
    }

    #[gpui::test]
    fn a_restart_that_leaves_an_older_daemon_does_not_restart_again(cx: &mut TestAppContext) {
        let (workspace, cx) = setup_real(cx);
        let host = FakeHost::new("/x/swarm-daemon");
        let paths = fake_daemon(
            vec![("0.0.1", None), ("0.0.2", None)],
            busy_with(DaemonAgentStatus::Idle),
        );

        connect(&workspace, launch(paths, &host), cx);

        let source = workspace.read_with(cx, |workspace, _| workspace.source.clone());
        assert_eq!(host.sent(), [(31_337, Signal::Terminate)]);
        assert_eq!(source, DataSource::Live);
    }

    #[gpui::test]
    fn a_process_that_is_not_the_daemon_is_not_stopped(cx: &mut TestAppContext) {
        let (workspace, cx) = setup_real(cx);
        let host = FakeHost::new("/bin/zsh");
        let paths = fake_daemon(vec![("0.0.1", Some(VERSION - 1))], Vec::new());
        connect(&workspace, launch(paths, &host), cx);

        click("restart-daemon", cx);
        click("restart-daemon-confirm", cx);
        cx.run_until_parked();
        redraw(cx);

        let source = workspace.read_with(cx, |workspace, _| workspace.source.clone());
        assert!(host.sent().is_empty());
        assert!(matches!(source, DataSource::Unavailable { .. }));
    }
}
