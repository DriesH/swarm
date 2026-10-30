//! The daemon connection and the user terminals it runs.
//!
//! Only user terminals are real. Agents, workers and process terminals stay fake.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use gpui::{AppContext as _, Context, Entity, Focusable as _, Size, Subscription, Window, px};
use swarm_protocol::{
    Client, ClientError, CreateTerminal, Paths, TerminalId as DaemonTerminalId, TerminalInfo,
    TerminalKind as DaemonKind,
};
use swarm_term::{TerminalChannels, TerminalInput};
use swarm_term_view::{TerminalEvent, TerminalView, palette_for};
use swarm_ui::ActiveTheme as _;

use crate::model::{TaskId, TerminalId};
use crate::workspace::Workspace;

/// Scrollback lines requested when attaching.
const ATTACH_HISTORY: u32 = 1_000;
const DAEMON_BINARY: &str = "swarm-daemon";
/// The tile's header, border and body padding.
const CHROME_WIDTH: f32 = 26.;
const CHROME_HEIGHT: f32 = 66.;
const FALLBACK_GRID: (u16, u16) = (100, 30);
/// How often the app checks that the daemon connection is alive.
const WATCH_INTERVAL: Duration = Duration::from_secs(1);
const RECONNECT_STEPS: [Duration; 3] = [
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
];
const RECONNECT_LONG: Duration = Duration::from_secs(5);

/// The wait before reconnect attempt number `attempt`, counted from zero.
fn reconnect_delay(attempt: usize) -> Duration {
    RECONNECT_STEPS
        .get(attempt)
        .copied()
        .unwrap_or(RECONNECT_LONG)
}

/// What the app needs to find or start the daemon again.
#[derive(Clone)]
pub(crate) struct DaemonLaunch {
    paths: Paths,
    binary: PathBuf,
}

/// A user terminal tile backed by a terminal in the daemon.
pub(crate) struct LiveTerminal {
    pub view: Entity<TerminalView>,
    daemon_id: DaemonTerminalId,
    input: flume::Sender<TerminalInput>,
    _subscription: Subscription,
}

impl LiveTerminal {
    /// Dropping the value then detaches from the daemon.
    pub(crate) fn kill_if_running(&self, cx: &Context<Workspace>) {
        if self.view.read(cx).exit_status().is_some() {
            return;
        }

        if let Err(error) = self.input.send(TerminalInput::Kill) {
            log::debug!("cannot kill the terminal: {error}");
        }
    }
}

type Connected = (Client, Vec<TerminalInfo>);

impl Workspace {
    /// Connects on a background thread, starting the daemon when needed. On failure
    /// user terminals stay fake.
    pub(crate) fn connect_daemon(window: &mut Window, cx: &mut Context<Self>) {
        let paths = match Paths::new() {
            Ok(paths) => paths,
            Err(error) => {
                log::warn!("cannot find the daemon folder: {error}");
                return;
            }
        };
        let binary = match std::env::current_exe() {
            Ok(path) => path.with_file_name(DAEMON_BINARY),
            Err(error) => {
                log::warn!("cannot find the app binary: {error}");
                return;
            }
        };

        let launch = DaemonLaunch { paths, binary };
        let connecting = cx.background_spawn({
            let launch = launch.clone();
            async move { connect(&launch) }
        });

        cx.spawn_in(window, async move |this, cx| match connecting.await {
            Ok((client, terminals)) => {
                let used = this.update_in(cx, |workspace, window, cx| {
                    workspace.daemon_launch = Some(launch);
                    workspace.use_daemon(client, &terminals, window, cx);
                    Self::watch_daemon(window, cx);
                });

                if let Err(error) = used {
                    log::debug!("the window closed before the daemon answered: {error}");
                }
            }
            Err(error) => log::warn!("no daemon, user terminals stay fake: {error}"),
        })
        .detach();
    }

    /// Starts a reconnect as soon as the connection is found dead.
    fn watch_daemon(window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(WATCH_INTERVAL).await;

                let checked = this.update_in(cx, |workspace, window, cx| {
                    workspace.reconnect_daemon(window, cx);
                });

                if checked.is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    /// Tries again with a growing wait until the daemon answers, then reattaches.
    pub(crate) fn reconnect_daemon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reconnecting || self.daemon.as_ref().is_none_or(Client::is_connected) {
            return;
        }

        let Some(launch) = self.daemon_launch.clone() else {
            log::warn!("the daemon connection is lost and cannot be restored");
            return;
        };

        log::warn!("the daemon connection is lost, reconnecting");
        self.reconnecting = true;

        cx.spawn_in(window, async move |this, cx| {
            for attempt in 0.. {
                cx.background_executor()
                    .timer(reconnect_delay(attempt))
                    .await;

                let result = cx
                    .background_executor()
                    .spawn({
                        let launch = launch.clone();
                        async move { connect(&launch) }
                    })
                    .await;

                let (client, terminals) = match result {
                    Ok(connected) => connected,
                    Err(error) => {
                        log::warn!("reconnect attempt {} failed: {error}", attempt + 1);
                        continue;
                    }
                };

                let used = this.update_in(cx, |workspace, window, cx| {
                    workspace.reconnecting = false;
                    workspace.use_daemon(client, &terminals, window, cx);
                });

                if let Err(error) = used {
                    log::debug!("the window closed while reconnecting: {error}");
                }

                return;
            }
        })
        .detach();
    }

    /// Syncs the tiles with a fresh daemon connection: reattaches tiles whose terminal
    /// still exists, removes the others, and adds tiles for new user terminals of known
    /// tasks.
    pub(crate) fn use_daemon(
        &mut self,
        client: Client,
        terminals: &[TerminalInfo],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.daemon = Some(client.clone());

        let users: Vec<&TerminalInfo> = terminals
            .iter()
            .filter(|info| info.kind == DaemonKind::User)
            .collect();
        let listed: HashSet<DaemonTerminalId> = users.iter().map(|info| info.id).collect();

        let tiles: Vec<(TerminalId, DaemonTerminalId)> = self
            .live_terminals
            .iter()
            .map(|(terminal, live)| (*terminal, live.daemon_id))
            .collect();

        for (terminal, daemon_id) in tiles {
            if !listed.contains(&daemon_id) {
                log::info!("terminal {} is gone, removing its tile", daemon_id.0);
                self.close_user_terminal(terminal, cx);
                continue;
            }

            match client.attach(daemon_id, ATTACH_HISTORY) {
                Ok(channels) => {
                    let live = self.live_terminal(terminal, daemon_id, channels, window, cx);
                    self.live_terminals.insert(terminal, live);
                    self.disconnected.remove(&terminal);
                }
                Err(error) => log::warn!("cannot reattach terminal {}: {error}", daemon_id.0),
            }
        }

        for info in users {
            if self
                .live_terminals
                .values()
                .any(|live| live.daemon_id == info.id)
            {
                continue;
            }

            let Some(task) = info
                .task
                .as_deref()
                .and_then(|key| self.data.task_with_key(key))
            else {
                log::debug!("terminal {} belongs to no known task", info.id.0);
                continue;
            };

            match client.attach(info.id, ATTACH_HISTORY) {
                Ok(channels) => {
                    self.add_live_terminal(
                        task,
                        info.id,
                        info.title.as_deref(),
                        channels,
                        window,
                        cx,
                    );
                }
                Err(error) => log::warn!("cannot attach terminal {}: {error}", info.id.0),
            }
        }

        cx.notify();
    }

    /// Asks the daemon for a new user terminal. The tile appears when it answers.
    pub(crate) fn create_live_terminal(
        &mut self,
        client: Client,
        task: TaskId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.data.key_of_task(task) else {
            return;
        };

        let request = CreateTerminal {
            kind: DaemonKind::User,
            command: None,
            // Fake tasks have no worktree yet.
            cwd: std::env::var_os("HOME").map(PathBuf::from),
            size: self.initial_grid(window, cx),
            palette: palette_for(cx.theme()),
            task: Some(key),
        };

        let creating = cx.background_spawn(async move { client.create(request) });

        cx.spawn_in(window, async move |this, cx| match creating.await {
            Ok((daemon_id, channels)) => {
                let added = this.update_in(cx, |workspace, window, cx| {
                    let terminal =
                        workspace.add_live_terminal(task, daemon_id, None, channels, window, cx);
                    workspace.focus_terminal(terminal, cx);
                });

                if let Err(error) = added {
                    log::debug!("the window closed before the terminal was ready: {error}");
                }
            }
            Err(error) => log::error!("cannot create a user terminal: {error}"),
        })
        .detach();
    }

    fn initial_grid(&self, window: &mut Window, cx: &Context<Self>) -> swarm_term::Size {
        let planned = self.planned_user_size();
        let body = Size {
            width: planned.width - px(CHROME_WIDTH),
            height: planned.height - px(CHROME_HEIGHT),
        };
        let measured = TerminalView::size_for(body, window, cx);

        if self.terminal_area.is_some() {
            return measured;
        }

        swarm_term::Size {
            cols: FALLBACK_GRID.0,
            rows: FALLBACK_GRID.1,
            ..measured
        }
    }

    fn add_live_terminal(
        &mut self,
        task: TaskId,
        daemon_id: DaemonTerminalId,
        title: Option<&str>,
        channels: TerminalChannels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> TerminalId {
        let terminal = self.data.add_live_user_terminal(task, title);
        let live = self.live_terminal(terminal, daemon_id, channels, window, cx);

        self.user_sizes.insert(terminal, self.planned_user_size());
        self.live_terminals.insert(terminal, live);

        cx.notify();

        terminal
    }

    fn live_terminal(
        &mut self,
        terminal: TerminalId,
        daemon_id: DaemonTerminalId,
        channels: TerminalChannels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> LiveTerminal {
        let input = channels.input.clone();
        let view = cx.new(|cx| TerminalView::new(channels, "User terminal".into(), window, cx));

        let subscription =
            cx.subscribe_in(&view, window, move |workspace, _, event, window, cx| {
                workspace.on_terminal_event(terminal, event, window, cx);
            });

        self.terminal_focus.insert(terminal, view.focus_handle(cx));

        LiveTerminal {
            view,
            daemon_id,
            input,
            _subscription: subscription,
        }
    }

    fn on_terminal_event(
        &mut self,
        terminal: TerminalId,
        event: &TerminalEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TerminalEvent::Exited(_) => self.close_user_terminal(terminal, cx),
            TerminalEvent::Disconnected => {
                self.disconnected.insert(terminal);
                self.reconnect_daemon(window, cx);
                cx.notify();
            }
            TerminalEvent::TitleChanged(title) => {
                self.data
                    .set_terminal_meta(terminal, Some(title.clone().into()));
                cx.notify();
            }
            TerminalEvent::WorkingDirectoryChanged(_) | TerminalEvent::Bell => {}
        }
    }
}

fn connect(launch: &DaemonLaunch) -> Result<Connected, ClientError> {
    let client = Client::connect_or_spawn(&launch.paths, &launch.binary)?;
    let terminals = client.list()?;

    Ok((client, terminals))
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, BufWriter, Write};
    use std::os::unix::fs::DirBuilderExt as _;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU32, Ordering};

    use gpui::{TestAppContext, VisualTestContext};
    use swarm_protocol::{Event, Request, VERSION, read, write};
    use swarm_term_view::TerminalEvent;

    use super::*;
    use crate::actions::{self, NewTerminal};

    static NEXT_SOCKET: AtomicU32 = AtomicU32::new(0);

    fn setup(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
        cx.update(|cx| {
            swarm_ui::theme::init(cx);
            actions::bind_keys(cx);
        });

        cx.add_window_view(Workspace::new)
    }

    /// A daemon that accepts one connection, says hello and reads until the client leaves.
    fn fake_daemon() -> PathBuf {
        spawn_fake_daemon(None)
    }

    /// A daemon that says hello and hangs up when the sender fires.
    fn fake_daemon_that_leaves() -> (PathBuf, flume::Sender<()>) {
        let (leave, leaving) = flume::bounded(1);

        (spawn_fake_daemon(Some(leaving)), leave)
    }

    fn spawn_fake_daemon(leave: Option<flume::Receiver<()>>) -> PathBuf {
        let folder = PathBuf::from(format!(
            "/tmp/swa-{}-{}",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&folder)
            .expect("create the private folder");
        let socket = folder.join("d.sock");
        let listener = UnixListener::bind(&socket).expect("bind the fake socket");

        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept()
                && let Ok(clone) = stream.try_clone()
            {
                let mut reader = BufReader::new(clone);
                let mut writer = BufWriter::new(stream);
                let mut buffer = Vec::new();

                let hello: Result<Request, _> = read(&mut reader, &mut buffer);
                assert_eq!(hello.ok(), Some(Request::Hello { version: VERSION }));
                write(
                    &mut writer,
                    &Event::Hello {
                        version: VERSION,
                        pid: 1,
                    },
                )
                .ok();
                writer.flush().ok();

                match leave {
                    Some(leaving) => {
                        leaving.recv().ok();
                    }
                    None => while read::<Request>(&mut reader, &mut buffer).is_ok() {},
                }
            }

            std::fs::remove_dir_all(&folder).ok();
        });

        socket
    }

    fn info(id: u64, kind: DaemonKind, task: &str) -> TerminalInfo {
        TerminalInfo {
            id: swarm_protocol::TerminalId(id),
            kind,
            task: Some(task.into()),
            title: Some("zsh".into()),
            cwd: None,
            exit: None,
        }
    }

    #[gpui::test]
    fn without_a_daemon_a_new_terminal_is_fake(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        cx.dispatch_action(NewTerminal);

        let (tiles, live, lines) = workspace.read_with(cx, |workspace, _| {
            let ids = &workspace.active_task().user_terminals;
            let lines = ids
                .first()
                .and_then(|id| workspace.data.terminal(*id))
                .map_or(0, |terminal| terminal.lines.len());

            (ids.len(), workspace.live_terminals.len(), lines)
        });
        assert_eq!(tiles, 1);
        assert_eq!(live, 0);
        assert!(lines > 0);
    }

    #[gpui::test]
    fn a_listed_user_terminal_of_a_known_task_gets_a_tile(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let client = Client::connect(&fake_daemon()).expect("connect to the fake daemon");
        let listed = [
            info(1, DaemonKind::User, "swarm/terminal-renderer"),
            info(2, DaemonKind::User, "swarm/no-such-task"),
            info(3, DaemonKind::Process, "swarm/terminal-renderer"),
        ];

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &listed, window, cx);
        });

        let (tiles, live, meta) = workspace.read_with(cx, |workspace, _| {
            let task = workspace.data.task_with_key("swarm/terminal-renderer");
            let ids = task
                .and_then(|task| workspace.data.task(task))
                .map(|task| task.user_terminals.clone())
                .unwrap_or_default();
            let meta = ids
                .first()
                .and_then(|id| workspace.data.terminal(*id))
                .and_then(|terminal| terminal.meta.clone());

            (ids.len(), workspace.live_terminals.len(), meta)
        });
        assert_eq!(tiles, 1);
        assert_eq!(live, 1);
        assert_eq!(meta.as_deref(), Some("zsh"));
    }

    #[gpui::test]
    fn closing_a_live_tile_removes_it(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let client = Client::connect(&fake_daemon()).expect("connect to the fake daemon");
        let listed = [info(1, DaemonKind::User, "swarm/terminal-renderer")];
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &listed, window, cx);
        });
        let terminal = workspace.read_with(cx, |workspace, _| {
            workspace.live_terminals.keys().next().copied()
        });

        workspace.update(cx, |workspace, cx| {
            if let Some(terminal) = terminal {
                workspace.close_user_terminal(terminal, cx);
            }
        });

        let live = workspace.read_with(cx, |workspace, _| workspace.live_terminals.len());
        assert!(terminal.is_some());
        assert_eq!(live, 0);
    }

    #[gpui::test]
    fn a_disconnected_event_marks_the_tile(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let client = Client::connect(&fake_daemon()).expect("connect to the fake daemon");
        let listed = [info(1, DaemonKind::User, "swarm/terminal-renderer")];
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &listed, window, cx);
        });
        let terminal = workspace
            .read_with(cx, |workspace, _| {
                workspace.live_terminals.keys().next().copied()
            })
            .expect("a live tile");

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.on_terminal_event(terminal, &TerminalEvent::Disconnected, window, cx);
        });

        let marked = workspace.read_with(cx, |workspace, _| {
            (
                workspace.disconnected.contains(&terminal),
                workspace.live_terminals.len(),
            )
        });
        assert_eq!(marked, (true, 1));
    }

    #[gpui::test]
    fn a_lost_daemon_stops_new_terminals_from_falling_back_to_fake(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let (socket, leave) = fake_daemon_that_leaves();
        let client = Client::connect(&socket).expect("connect");
        leave.send(()).expect("the fake daemon is waiting");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while client.is_connected() && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(!client.is_connected());
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &[], window, cx);
        });

        cx.dispatch_action(NewTerminal);

        let (tiles, live) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.active_task().user_terminals.len(),
                workspace.live_terminals.len(),
            )
        });
        assert_eq!((tiles, live), (0, 0));
    }

    #[gpui::test]
    fn a_tile_whose_terminal_is_gone_is_removed_on_sync(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let client = Client::connect(&fake_daemon()).expect("connect to the fake daemon");
        let listed = [info(1, DaemonKind::User, "swarm/terminal-renderer")];
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &listed, window, cx);
        });
        let client = Client::connect(&fake_daemon()).expect("connect to the fake daemon");

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &[], window, cx);
        });

        let live = workspace.read_with(cx, |workspace, _| workspace.live_terminals.len());
        assert_eq!(live, 0);
    }

    #[gpui::test]
    fn a_surviving_tile_is_reattached_on_sync(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let client = Client::connect(&fake_daemon()).expect("connect to the fake daemon");
        let listed = [info(1, DaemonKind::User, "swarm/terminal-renderer")];
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &listed, window, cx);
        });
        let tile = workspace
            .read_with(cx, |workspace, _| {
                workspace.live_terminals.keys().next().copied()
            })
            .expect("a live tile");
        workspace.update(cx, |workspace, _| {
            workspace.disconnected.insert(tile);
        });
        let client = Client::connect(&fake_daemon()).expect("connect to the fake daemon");

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &listed, window, cx);
        });

        let state = workspace.read_with(cx, |workspace, _| {
            (
                workspace.live_terminals.len(),
                workspace.disconnected.contains(&tile),
            )
        });
        assert_eq!(state, (1, false));
    }

    #[test]
    fn reconnect_waits_grow_then_settle() {
        let delays: Vec<Duration> = (0..5).map(reconnect_delay).collect();

        assert_eq!(
            delays,
            [
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(5),
                Duration::from_secs(5),
            ]
        );
    }
}
