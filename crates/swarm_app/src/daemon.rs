//! The daemon connection, its projects and the terminals it runs.
//!
//! Projects, tasks, agents, user terminals and process terminals of real tasks come from
//! the daemon.

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AppContext as _, AsyncApp, Context, Entity, Focusable as _, PathPromptOptions, SharedString,
    Size, Subscription, WeakEntity, Window, px,
};
use swarm_protocol::{
    AgentChange, AgentId as DaemonAgentId, AgentInfo, AgentLifecycle, Client, ClientError,
    CreateTerminal, MessageEvent, Paths, ProjectInfo, ReviewSource, TaskDiff, TaskInfo,
    TerminalId as DaemonTerminalId, TerminalInfo, TerminalKind as DaemonKind, Usage,
};
use swarm_term::{ExitStatus, TerminalChannels, TerminalInput};
use swarm_term_view::{TerminalEvent, TerminalView, palette_for};
use swarm_ui::ActiveTheme as _;

use crate::model::{AgentChanges, FakeData, ProjectId, TaskId, TerminalId, TerminalKind};
use crate::pull_request::SetupStep;
use crate::workspace::{DataSource, View, Workspace};

/// Scrollback lines requested when attaching.
const ATTACH_HISTORY: u32 = 1_000;
const SETUP_TITLE: &str = "Setup";
const DAEMON_BINARY: &str = "swarm-daemon";
/// The tile's header, border and body padding.
pub(crate) const CHROME_WIDTH: f32 = 26.;
pub(crate) const CHROME_HEIGHT: f32 = 56.;
const FALLBACK_GRID: (u16, u16) = (100, 30);
/// How often the app checks that the daemon connection is alive.
const WATCH_INTERVAL: Duration = Duration::from_secs(1);
const RECONNECT_STEPS: [Duration; 3] = [
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
];
const RECONNECT_LONG: Duration = Duration::from_secs(5);
/// How often the Swarm view asks for the changes of the open task's agents.
const CHANGES_INTERVAL: Duration = Duration::from_secs(5);
/// How often the visible Review source is asked for while Review shows.
pub(crate) const REVIEW_INTERVAL: Duration = Duration::from_secs(2);
/// How often the open task's Live diff is asked for while the window is active.
pub(crate) const LIVE_INTERVAL: Duration = Duration::from_secs(5);
/// How often the usage card redoes its time text, which counts whole minutes.
const USAGE_TICK: Duration = Duration::from_secs(30);
/// How long an exited process terminal stays, so its exit code can be read.
pub(crate) const PROCESS_LINGER: Duration = Duration::from_secs(3);

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

impl DaemonLaunch {
    fn detect() -> Result<Self, String> {
        let paths =
            Paths::new().map_err(|error| format!("cannot find the daemon folder: {error}"))?;
        let binary = std::env::current_exe()
            .map_err(|error| format!("cannot find the app binary: {error}"))?
            .with_file_name(DAEMON_BINARY);

        Ok(Self { paths, binary })
    }
}

/// A running poll for one diff. Dropping it stops the poll.
pub(crate) struct DiffPoll {
    key: (swarm_protocol::TaskId, ReviewSource),
    interval: Duration,
    _task: gpui::Task<()>,
}

/// A user or process terminal tile backed by a terminal in the daemon.
pub(crate) struct LiveTerminal {
    pub view: Entity<TerminalView>,
    daemon_id: DaemonTerminalId,
    input: flume::Sender<TerminalInput>,
    _subscription: Subscription,
}

impl LiveTerminal {
    pub(crate) fn daemon_id(&self) -> DaemonTerminalId {
        self.daemon_id
    }

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

type Connected = (Client, Vec<TerminalInfo>, Vec<ProjectInfo>);

impl Workspace {
    /// Runs `apply` with the window the workspace shows in, or with none while it has
    /// none. The daemon connection outlives the window, so its work cannot need one.
    fn update_with_window<R>(
        this: &WeakEntity<Self>,
        cx: &mut AsyncApp,
        apply: impl FnOnce(&mut Self, Option<&mut Window>, &mut Context<Self>) -> R,
    ) -> anyhow::Result<R> {
        let window = this.read_with(cx, |workspace, _| workspace.window)?;

        let Some(window) = window else {
            return this.update(cx, |workspace, cx| apply(workspace, None, cx));
        };

        cx.update_window(window, |_, window, cx| {
            this.update(cx, |workspace, cx| apply(workspace, Some(window), cx))
        })?
    }

    /// Connects on a background thread, starting the daemon when needed.
    pub(crate) fn connect_daemon(&mut self, cx: &mut Context<Self>) {
        match DaemonLaunch::detect() {
            Ok(launch) => self.start_connect(launch, cx),
            Err(reason) => self.daemon_unavailable(reason, cx),
        }
    }

    /// Runs the connect again after it failed.
    pub(crate) fn retry_daemon(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.source, DataSource::Unavailable { .. }) {
            return;
        }

        match self.daemon_launch.clone() {
            Some(launch) => self.start_connect(launch, cx),
            None => self.connect_daemon(cx),
        }
    }

    pub(crate) fn daemon_unavailable(&mut self, reason: String, cx: &mut Context<Self>) {
        log::warn!("no daemon: {reason}");
        self.source = DataSource::Unavailable {
            reason: reason.into(),
        };
        cx.notify();
    }

    fn start_connect(&mut self, launch: DaemonLaunch, cx: &mut Context<Self>) {
        self.daemon_launch = Some(launch.clone());
        self.source = DataSource::Connecting;
        cx.notify();

        let connecting = cx.background_spawn(async move { connect(&launch) });

        cx.spawn(async move |this, cx| {
            let result = connecting.await;

            let used = Self::update_with_window(&this, cx, |workspace, window, cx| match result {
                Ok((client, terminals, projects)) => {
                    workspace.use_projects(&projects, cx);
                    workspace.connect_client(client, &terminals, window, cx);
                    Self::watch_daemon(cx);
                }
                Err(error) => workspace.daemon_unavailable(error.to_string(), cx),
            });

            if let Err(error) = used {
                log::debug!("the workspace is gone before the daemon answered: {error}");
            }
        })
        .detach();
    }

    /// Starts a reconnect as soon as the connection is found dead.
    fn watch_daemon(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(WATCH_INTERVAL).await;

                let checked = this.update(cx, |workspace, cx| {
                    workspace.reconnect_daemon(cx);
                });

                if checked.is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    /// Tries again with a growing wait until the daemon answers, then reattaches.
    pub(crate) fn reconnect_daemon(&mut self, cx: &mut Context<Self>) {
        if self.reconnecting || self.daemon.as_ref().is_none_or(Client::is_connected) {
            return;
        }

        let Some(launch) = self.daemon_launch.clone() else {
            log::warn!("the daemon connection is lost and cannot be restored");
            return;
        };

        log::warn!("the daemon connection is lost, reconnecting");
        self.reconnecting = true;

        cx.spawn(async move |this, cx| {
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

                let (client, terminals, projects) = match result {
                    Ok(connected) => connected,
                    Err(error) => {
                        log::warn!("reconnect attempt {} failed: {error}", attempt + 1);
                        continue;
                    }
                };

                let used = Self::update_with_window(&this, cx, |workspace, window, cx| {
                    workspace.reconnecting = false;
                    workspace.use_projects(&projects, cx);
                    workspace.connect_client(client, &terminals, window, cx);
                });

                if let Err(error) = used {
                    log::debug!("the workspace is gone while reconnecting: {error}");
                }

                return;
            }
        })
        .detach();
    }

    /// Takes the daemon's projects. The first time they replace the sample; later they
    /// are merged, so tabs keep their ids.
    pub(crate) fn use_projects(&mut self, projects: &[ProjectInfo], cx: &mut Context<Self>) {
        if self.source == DataSource::Live {
            self.data.sync_projects(projects);
            self.close_ended_start_tabs(projects, cx);
            cx.notify();
            return;
        }

        self.source = DataSource::Live;
        self.data = FakeData::live();
        self.live_terminals.clear();
        self.terminal_focus.clear();
        self.user_sizes.clear();
        self.minimized.clear();
        self.disconnected.clear();
        self.dismissed_terminals.clear();
        self.start_requested.clear();
        self.previous_tabs.clear();
        self.focused_terminal = None;
        self.data.sync_projects(projects);

        if let Some(project) = self.data.projects.first().map(|project| project.id) {
            self.select_project(project, cx);
        } else {
            self.active_project = ProjectId(u32::MAX);
            self.active_task = TaskId(u32::MAX);
        }

        cx.notify();
    }

    /// Takes a fresh daemon connection with the window the workspace shows in.
    #[cfg(test)]
    pub(crate) fn use_daemon(
        &mut self,
        client: Client,
        terminals: &[TerminalInfo],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.connect_client(client, terminals, Some(window), cx);
    }

    /// Takes a fresh daemon connection. Without a window only the data is taken: the
    /// tiles follow when a window opens.
    fn connect_client(
        &mut self,
        client: Client,
        terminals: &[TerminalInfo],
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        self.daemon = Some(client.clone());
        self.forget_comment_listing();
        Self::watch_events(&client, cx);

        if let Some(window) = window {
            self.sync_terminals(&client, terminals, window, cx);
        } else {
            self.drop_agent_terminals();
            self.apply_pending_agents(None, cx);
        }

        self.load_usage(client.clone(), cx);
        self.load_message_history(client, cx);

        cx.notify();
    }

    /// Makes the terminal views again for a new window: the old ones were made for a window
    /// that is gone. The daemon's list says which terminals still exist.
    pub(crate) fn reattach_terminals(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.daemon.clone().filter(Client::is_connected) else {
            return;
        };

        let listing = cx.background_spawn({
            let client = client.clone();
            async move { client.list() }
        });

        cx.spawn(async move |this, cx| {
            let terminals = match listing.await {
                Ok(terminals) => terminals,
                Err(error) => {
                    log::warn!("cannot list terminals for the new window: {error}");
                    return;
                }
            };

            let updated = Self::update_with_window(&this, cx, |workspace, window, cx| {
                if let Some(window) = window {
                    workspace.sync_terminals(&client, &terminals, window, cx);
                    workspace.refocus_terminal(cx);
                }
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the terminals were listed: {error}");
            }
        })
        .detach();
    }

    /// Stops the daemon's output to the views of a closed window, so nothing parses it
    /// while no window shows it. The terminals keep running, and a new window attaches
    /// again.
    pub(crate) fn detach_terminals(&self) {
        let Some(client) = &self.daemon else {
            return;
        };

        for live in self.live_terminals.values() {
            client.detach(live.daemon_id);
        }
    }

    /// Syncs the tiles with the daemon's terminals: reattaches tiles whose terminal
    /// still exists, removes the others, and adds tiles for new terminals of known
    /// tasks.
    pub(crate) fn sync_terminals(
        &mut self,
        client: &Client,
        terminals: &[TerminalInfo],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.drop_agent_terminals();

        let listed: HashSet<DaemonTerminalId> = terminals
            .iter()
            .filter(|info| self.wants_tile(info).is_some())
            .map(|info| info.id)
            .collect();

        let tiles: Vec<(TerminalId, DaemonTerminalId)> = self
            .live_terminals
            .iter()
            .map(|(terminal, live)| (*terminal, live.daemon_id))
            .collect();

        for (terminal, daemon_id) in tiles {
            if !listed.contains(&daemon_id) {
                log::info!("terminal {} is gone, removing its tile", daemon_id.0);
                self.close_tile(terminal, cx);
                continue;
            }

            match client.attach(daemon_id, ATTACH_HISTORY) {
                Ok(channels) => {
                    let name = self.accessible_name(terminal);
                    let live = self.live_terminal(terminal, daemon_id, name, channels, window, cx);
                    self.live_terminals.insert(terminal, live);
                    self.disconnected.remove(&terminal);
                }
                Err(error) => log::warn!("cannot reattach terminal {}: {error}", daemon_id.0),
            }
        }

        self.attach_new_terminals(client, terminals, window, cx);
        self.attach_agents(window, cx);
    }

    /// Hands the daemon's broadcasts to the workspace, until the connection ends. They
    /// keep coming while no window is open, so the menu bar stays current.
    fn watch_events(client: &Client, cx: &mut Context<Self>) {
        Self::forward(client.agent_events(), cx, |workspace, agent, window, cx| {
            workspace.agent_changed(&agent, window, cx);
        });
        Self::forward(client.task_events(), cx, |workspace, task, window, cx| {
            workspace.task_changed(&task, window, cx);
        });
        Self::forward(
            client.terminal_events(),
            cx,
            |workspace, terminal, window, cx| {
                if let Some(window) = window {
                    workspace.terminal_changed(&terminal, window, cx);
                }
            },
        );
        Self::forward(client.message_events(), cx, |workspace, event, _, cx| {
            workspace.message_event(event, cx);
        });
        Self::forward(client.comment_events(), cx, |workspace, event, _, cx| {
            workspace.comment_event(&event, cx);
        });
        Self::forward(client.pr_events(), cx, |workspace, event, _, cx| {
            workspace.pr_event(event, cx);
        });
        Self::forward(client.usage_events(), cx, |workspace, usage, _, cx| {
            workspace.usage_changed(usage, cx);
        });
        Self::forward(client.notices(), cx, |workspace, notice, window, cx| {
            workspace.notice_received(&notice, window, cx);
        });
    }

    fn forward<T: 'static>(
        events: flume::Receiver<T>,
        cx: &mut Context<Self>,
        apply: impl Fn(&mut Self, T, Option<&mut Window>, &mut Context<Self>) + 'static,
    ) {
        cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv_async().await {
                let updated = Self::update_with_window(&this, cx, |workspace, window, cx| {
                    apply(workspace, event, window, cx);
                });

                if updated.is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    /// Applies an agent the daemon reports: status, model and terminal. An agent of a
    /// task the app does not know yet waits until the task arrives.
    pub(crate) fn agent_changed(
        &mut self,
        info: &AgentInfo,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let Some((_, terminal)) = self.data.sync_agent(info) else {
            // An ended Start orchestrator has no tab to wait for.
            let ended_start = info.task.is_none() && is_ended(info.lifecycle);
            let newer = self
                .pending_agents
                .get(&info.id)
                .is_none_or(|waiting| waiting.revision < info.revision);
            if newer && !ended_start {
                self.pending_agents.insert(info.id, info.clone());
            }

            return;
        };

        self.pending_agents.remove(&info.id);
        if let Some(window) = window {
            self.sync_agent_terminal(terminal, window, cx);
        }

        if info.task.is_none() {
            self.start_agent_changed(info, terminal, cx);
        }

        cx.notify();
    }

    /// A Start orchestrator that ended closes its tab. A new one takes focus when
    /// nothing on its tab has it.
    fn start_agent_changed(
        &mut self,
        info: &AgentInfo,
        terminal: TerminalId,
        cx: &mut Context<Self>,
    ) {
        let known = self.data.agent_with_daemon_id(info.id);
        let ended = known
            .and_then(|agent| agent.daemon.as_ref())
            .is_some_and(|daemon| is_ended(daemon.lifecycle));

        if ended {
            self.close_start_tab(info.project, info.id, cx);
            return;
        }

        let on_active_tab = known.is_some_and(|agent| agent.task == self.active_task);
        if on_active_tab && self.focused_terminal.is_none() {
            self.focus_terminal(terminal, cx);
        }
    }

    /// Closes a project's Start tab after its orchestrator exited. Focus moves to the
    /// lowest task the orchestrator made, else to the tab used before. A project with
    /// no other tab keeps an empty Start tab.
    pub(crate) fn close_start_tab(
        &mut self,
        project: swarm_protocol::ProjectId,
        agent: DaemonAgentId,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.data.project_with_daemon_id(project) else {
            return;
        };
        let Some(tab) = self.data.start_task(project) else {
            return;
        };
        if self.data.start_agent_of(tab) != Some(agent) {
            return;
        }

        let was_active = self.active_task == tab;
        let made = self.data.first_task_made_by(project, agent);
        let before = self.previous_tabs.get(&project).copied();
        let others = self.data.tasks_of(project).any(|task| task.id != tab);

        let gone = if others {
            self.data.remove_task(tab)
        } else {
            self.data.remove_agents_of(tab)
        };
        for terminal in gone {
            self.forget_terminal(terminal);
        }

        if was_active && others {
            let remaining = |task: &TaskId| self.data.task(*task).is_some();
            let next = made
                .or(before.filter(remaining))
                .or_else(|| self.data.tasks_of(project).next().map(|task| task.id));

            if let Some(next) = next {
                self.select_task(next, cx);
            }
        }

        cx.notify();
    }

    /// Closes the Start tabs whose orchestrator the listing no longer shows running.
    fn close_ended_start_tabs(&mut self, projects: &[ProjectInfo], cx: &mut Context<Self>) {
        for info in projects {
            let Some(project) = self.data.project_with_daemon_id(info.id) else {
                continue;
            };
            let Some(agent) = self
                .data
                .start_task(project)
                .and_then(|tab| self.data.start_agent_of(tab))
            else {
                continue;
            };

            let runs = info
                .start
                .as_ref()
                .is_some_and(|start| start.id == agent && !is_ended(start.lifecycle));
            if !runs {
                self.close_start_tab(info.id, agent, cx);
            }
        }
    }

    /// Asks the daemon for the project's Start orchestrator once, when its Start tab
    /// shows and none runs. The daemon picks the kind from the settings.
    pub(crate) fn start_orchestrator_if_needed(&mut self, cx: &mut Context<Self>) {
        let task = self.active_task();
        if !task.is_start {
            return;
        }

        let project = task.project;
        if self.start_requested.contains(&project) || self.data.start_runs(project) {
            return;
        }

        let Some(client) = self.daemon.clone().filter(Client::is_connected) else {
            return;
        };
        let Some(daemon_project) = self.data.project(project).and_then(|entry| entry.daemon) else {
            return;
        };

        self.start_requested.insert(project);

        let starting =
            cx.background_spawn(async move { client.start_orchestrator(daemon_project, None) });

        cx.spawn(async move |this, cx| {
            let result = starting.await;

            let updated =
                Self::update_with_window(&this, cx, |workspace, window, cx| match result {
                    Ok(agent) => workspace.agent_changed(&agent, window, cx),
                    Err(error) => workspace.report_sidebar_error(error.to_string(), cx),
                });

            if let Err(error) = updated {
                log::debug!("the workspace is gone before the orchestrator started: {error}");
            }
        })
        .detach();
    }

    /// A task was added, also by an agent, or changed. Focus stays where it is.
    pub(crate) fn task_changed(
        &mut self,
        info: &TaskInfo,
        mut window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if info.closed {
            self.task_closed(info, window, cx);
            return;
        }

        let Some(project) = self.data.project_with_daemon_id(info.project) else {
            log::warn!("task {} belongs to an unknown project", info.id.0);
            return;
        };

        let known = self.data.has_daemon_task(info.id);
        let task = self.data.sync_task(project, info);

        match window.as_deref_mut() {
            Some(window) => self.attach_agents(window, cx),
            None => self.apply_pending_agents(None, cx),
        }
        self.show_setup_exit(task, cx);

        if !known && let Some(window) = window {
            self.attach_listed_terminals(window, cx);
        }

        cx.notify();
    }

    /// The daemon closed a task: drops its tab, its terminals and what Review kept.
    fn task_closed(
        &mut self,
        info: &TaskInfo,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = self
            .data
            .tasks
            .iter()
            .find(|task| {
                task.daemon
                    .as_ref()
                    .is_some_and(|daemon| daemon.id == info.id)
            })
            .map(|task| task.id)
        else {
            return;
        };

        if self.close_dialog_is_for(task, cx) {
            match window {
                Some(window) => self.close_overlay(window, cx),
                None => self.overlay = None,
            }
        }

        self.remove_closed_task(task, cx);
    }

    /// Removes a tab with its agents, terminals and Review state. When it was open, the
    /// tab beside it opens, else the project's Start tab.
    pub(crate) fn remove_closed_task(&mut self, task: TaskId, cx: &mut Context<Self>) {
        let Some(entry) = self.data.task(task) else {
            return;
        };
        let project = entry.project;
        let tabs: Vec<TaskId> = self.data.tasks_of(project).map(|other| other.id).collect();
        let position = tabs.iter().position(|other| *other == task).unwrap_or(0);
        let was_open = self.active_task == task;

        for terminal in self.data.remove_task(task) {
            self.forget_terminal(terminal);
        }

        self.review_sources.remove(&task);
        self.review
            .update(cx, |review, cx| review.forget_task_comments(task, cx));
        self.swarm_selection.remove(&task);
        self.previous_tabs.retain(|_, other| *other != task);
        self.review_approving.remove(&task);
        self.review_requesting.remove(&task);
        self.pull_requests.forget_task(task);

        if was_open {
            let next = tabs
                .get(position + 1)
                .or_else(|| position.checked_sub(1).and_then(|before| tabs.get(before)))
                .copied()
                .filter(|other| self.data.task(*other).is_some())
                .unwrap_or_else(|| self.data.first_or_start_task(project));

            self.select_task(next, cx);
        }

        cx.notify();
    }

    /// Lists the daemon's terminals and gives the new ones of known tasks a tile.
    fn attach_listed_terminals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.daemon.clone() else {
            return;
        };

        let listing = cx.background_spawn({
            let client = client.clone();
            async move { client.list() }
        });

        cx.spawn_in(window, async move |this, cx| {
            let terminals = match listing.await {
                Ok(terminals) => terminals,
                Err(error) => {
                    log::warn!("cannot list terminals after a task changed: {error}");
                    return;
                }
            };

            let updated = this.update_in(cx, |workspace, window, cx| {
                workspace.attach_new_terminals(&client, &terminals, window, cx);
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the terminals were listed: {error}");
            }
        })
        .detach();
    }

    /// An agent started a process terminal or resized it.
    pub(crate) fn terminal_changed(
        &mut self,
        info: &TerminalInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if info.kind != DaemonKind::Process {
            return;
        }

        if let Some(client) = self.daemon.clone() {
            self.attach_new_terminals(&client, std::slice::from_ref(info), window, cx);
        }

        cx.notify();
    }

    pub(crate) fn message_event(&mut self, event: MessageEvent, cx: &mut Context<Self>) {
        match event {
            MessageEvent::Sent(message) => self.data.record_message(&message),
            MessageEvent::Delivered { ids, delivered_at } => {
                self.data.mark_delivered(&ids, delivered_at);
            }
        }

        cx.notify();
    }

    pub(crate) fn usage_changed(&mut self, usage: Usage, cx: &mut Context<Self>) {
        self.data.set_usage(usage);
        cx.notify();
    }

    /// Asks the daemon for the last known usage, off the main thread. An event that
    /// arrives before the answer is newer, so it wins.
    fn load_usage(&self, client: Client, cx: &mut Context<Self>) {
        let asked_at = self.data.usage_revision;

        cx.spawn(async move |this, cx| {
            let asking = cx
                .background_executor()
                .spawn(async move { client.usage() });

            let usage = match asking.await {
                Ok(usage) => usage,
                Err(error) => {
                    log::warn!("cannot read the usage: {error}");
                    return;
                }
            };

            let shown = this.update(cx, |workspace, cx| {
                if workspace.data.usage_revision == asked_at {
                    workspace.usage_changed(usage, cx);
                }
            });

            if let Err(error) = shown {
                log::debug!("the window closed before the usage came: {error}");
            }
        })
        .detach();
    }

    /// Redoes the usage card's time text every [`USAGE_TICK`], only while the window is
    /// active and the card shows a time.
    pub(crate) fn sync_usage_clock(&mut self, window_active: bool, cx: &mut Context<Self>) {
        let wanted = window_active
            && self.fixed_now.is_none()
            && crate::sidebar::usage_shows_time(&self.data.usage);

        if !wanted {
            self.usage_clock = None;
            return;
        }

        if self.usage_clock.is_none() {
            self.usage_clock = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(USAGE_TICK).await;

                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        return;
                    }
                }
            }));
        }
    }

    /// Fetches the kept messages of every task and Start orchestrator, off the main thread.
    fn load_message_history(&self, client: Client, cx: &mut Context<Self>) {
        let mut scopes = Vec::new();

        for project in &self.data.projects {
            let Some(daemon) = project.daemon else {
                continue;
            };

            scopes.push((daemon, None));
            scopes.extend(
                self.data
                    .tasks_of(project.id)
                    .filter_map(|task| task.daemon.as_ref())
                    .map(|task| (daemon, Some(task.id))),
            );
        }

        cx.spawn(async move |this, cx| {
            let listing = cx.background_executor().spawn(async move {
                let mut messages = Vec::new();

                for (project, task) in scopes {
                    match client.list_messages(project, task) {
                        Ok(listed) => messages.extend(listed),
                        Err(error) => log::warn!("cannot list messages: {error}"),
                    }
                }

                messages
            });
            let messages = listing.await;

            let merged = this.update(cx, |workspace, cx| {
                workspace.data.merge_messages(&messages);
                cx.notify();
            });

            if let Err(error) = merged {
                log::debug!("the window closed before the messages came: {error}");
            }
        })
        .detach();
    }

    /// Keeps the Swarm view's background work running only while the view is shown in an
    /// active window: the poll for changes and the clock for running times. The poll
    /// restarts, and so asks at once, when the open task changes.
    pub(crate) fn sync_swarm_tasks(&mut self, window_active: bool, cx: &mut Context<Self>) {
        self.sync_diff_polls(window_active, cx);

        if self.view != View::Swarm || self.has_no_projects() || !window_active {
            self.changes_poll = None;
            self.polled_task = None;
            self.swarm_clock = None;
            return;
        }

        if self.polled_task != Some(self.active_task) {
            self.changes_poll = None;
        }

        if self.changes_poll.is_none() && self.daemon.is_some() {
            self.changes_poll = Some(Self::poll_changes(cx));
            self.polled_task = Some(self.active_task);
        }

        if self.swarm_clock.is_none() && self.fixed_now.is_none() {
            self.swarm_clock = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(1)).await;

                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        return;
                    }
                }
            }));
        }
    }

    /// Keeps the diff polls running only while they are needed: the visible Review source
    /// while Review shows, the open task's Live diff for the header and the Changes
    /// panel, and the Merged diff while the pull request panel shows. They need an active
    /// window and a connected daemon. A poll restarts, and so asks at once, when its task
    /// or source changes.
    pub(crate) fn sync_diff_polls(&mut self, window_active: bool, cx: &mut Context<Self>) {
        let connected = window_active && !self.has_no_projects() && self.daemon.is_some();
        let open = self
            .data
            .task(self.active_task)
            .and_then(|task| task.daemon.as_ref())
            .map(|daemon| daemon.id)
            .filter(|_| connected);

        let (review_every, live_every) = self.diff_intervals;

        let review = open
            .filter(|_| self.view == View::Review)
            .map(|id| (id, self.review_source(self.active_task), review_every));
        let review_key = review.map(|(id, source, _)| (id, source));

        // The pull request panel needs the Merged diff for the head commit, also while
        // Review shows the Live diff.
        let merged = open
            .filter(|_| self.pr_panel_needs_merged_diff())
            .map(|id| (id, ReviewSource::Merged))
            .filter(|key| Some(*key) != review_key);
        let live = open
            .map(|id| (id, ReviewSource::Live))
            .filter(|key| Some(*key) != review_key);

        Self::sync_poll(
            &mut self.review_poll,
            review.map(|(id, source, every)| ((id, source), every)),
            cx,
        );
        Self::sync_poll(&mut self.live_poll, live.map(|key| (key, live_every)), cx);
        Self::sync_poll(&mut self.pr_poll, merged.map(|key| (key, live_every)), cx);
    }

    fn sync_poll(
        slot: &mut Option<DiffPoll>,
        wanted: Option<((swarm_protocol::TaskId, ReviewSource), Duration)>,
        cx: &mut Context<Self>,
    ) {
        match wanted {
            None => *slot = None,
            Some((key, interval))
                if slot
                    .as_ref()
                    .is_some_and(|poll| poll.key == key && poll.interval == interval) => {}
            Some((key, interval)) => {
                *slot = Some(DiffPoll {
                    key,
                    interval,
                    _task: Self::poll_diff(key, interval, cx),
                });
            }
        }
    }

    fn poll_diff(
        (task, source): (swarm_protocol::TaskId, ReviewSource),
        interval: Duration,
        cx: &mut Context<Self>,
    ) -> gpui::Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(request) =
                    this.update(cx, |workspace, _| workspace.diff_request(task, source))
                else {
                    return;
                };

                if let Some((client, known)) = request {
                    let asking = cx
                        .background_executor()
                        .spawn(async move { client.task_diff(task, source, known) });

                    match asking.await {
                        Ok(Some(diff)) => {
                            let applied = this.update(cx, |workspace, cx| {
                                workspace.apply_diff(diff, cx);
                            });

                            if applied.is_err() {
                                return;
                            }
                        }
                        Ok(None) => {}
                        Err(error) => log::debug!("cannot read the task diff: {error}"),
                    }
                }

                cx.background_executor().timer(interval).await;
            }
        })
    }

    /// The client and the fingerprint already known, while the daemon answers.
    fn diff_request(
        &self,
        task: swarm_protocol::TaskId,
        source: ReviewSource,
    ) -> Option<(Client, Option<u64>)> {
        let client = self.daemon.clone().filter(Client::is_connected)?;
        let known = self
            .data
            .diffs
            .get(&(task, source))
            .map(|diff| diff.fingerprint);

        Some((client, known))
    }

    /// Keeps a diff the daemon sent, and redraws when it differs from the one known.
    pub(crate) fn apply_diff(&mut self, diff: TaskDiff, cx: &mut Context<Self>) {
        let (task, source) = (diff.task, diff.source);

        if self.data.store_diff(diff) {
            self.reload_changed_files(task, source, cx);
            cx.notify();
        }
    }

    /// Asks the daemon for a diff without saying which one the app has, so its answer
    /// decides: it replaces the stored diff when it differs.
    pub(crate) fn refresh_diff(
        &mut self,
        task: swarm_protocol::TaskId,
        source: ReviewSource,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.daemon.clone().filter(Client::is_connected) else {
            return;
        };
        let asking = cx.background_spawn(async move { client.task_diff(task, source, None) });

        cx.spawn(async move |this, cx| match asking.await {
            Ok(Some(diff)) => {
                let applied = this.update(cx, |workspace, cx| workspace.apply_diff(diff, cx));

                if let Err(error) = applied {
                    log::debug!("the window closed before the diff arrived: {error}");
                }
            }
            Ok(None) => {}
            Err(error) => log::debug!("cannot read the task diff: {error}"),
        })
        .detach();
    }

    fn poll_changes(cx: &mut Context<Self>) -> gpui::Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(request) = this.update(cx, |workspace, _| workspace.changes_request())
                else {
                    return;
                };

                if let Some((client, agents)) = request {
                    let asked = agents.clone();
                    let asking = cx
                        .background_executor()
                        .spawn(async move { client.agent_changes(agents) });

                    match asking.await {
                        Ok(changes) => {
                            let applied = this.update(cx, |workspace, cx| {
                                workspace.apply_changes(&asked, &changes, cx);
                            });

                            if applied.is_err() {
                                return;
                            }
                        }
                        Err(error) => log::debug!("cannot read agent changes: {error}"),
                    }
                }

                cx.background_executor().timer(CHANGES_INTERVAL).await;
            }
        })
    }

    /// The client and the daemon ids of the open task's agents, while the daemon answers.
    fn changes_request(&self) -> Option<(Client, Vec<DaemonAgentId>)> {
        let client = self.daemon.clone().filter(Client::is_connected)?;
        let agents: Vec<DaemonAgentId> = self
            .data
            .agents_of(self.active_task)
            .filter_map(|agent| agent.daemon.as_ref().map(|daemon| daemon.id))
            .collect();

        (!agents.is_empty()).then_some((client, agents))
    }

    /// Keeps the changes of the asked agents. An asked agent missing from the reply loses
    /// its changes, so a worktree that can no longer be read does not show old numbers.
    pub(crate) fn apply_changes(
        &mut self,
        asked: &[DaemonAgentId],
        changes: &[AgentChange],
        cx: &mut Context<Self>,
    ) {
        let mut updated = false;

        for id in asked {
            if changes.iter().any(|change| change.agent == *id) {
                continue;
            }
            let Some(agent) = self.data.agent_with_daemon_id(*id).map(|agent| agent.id) else {
                continue;
            };

            updated |= self.data.agent_changes.remove(&agent).is_some();
        }

        for change in changes {
            let Some(agent) = self
                .data
                .agent_with_daemon_id(change.agent)
                .map(|agent| agent.id)
            else {
                continue;
            };
            let known = AgentChanges {
                stat: change.stat,
                files: Rc::from(change.files.as_slice()),
            };

            if self.data.agent_changes.get(&agent) != Some(&known) {
                self.data.agent_changes.insert(agent, known);
                updated = true;
            }
        }

        if updated {
            cx.notify();
        }
    }

    /// Forgets a tile's live view and everything that points at it.
    fn forget_terminal(&mut self, terminal: TerminalId) {
        self.drop_live_terminal(terminal);
        self.user_sizes.remove(&terminal);
        self.minimized.remove(&terminal);

        if self.focused_terminal == Some(terminal) {
            self.focused_terminal = None;
        }
    }

    fn apply_pending_agents(&mut self, mut window: Option<&mut Window>, cx: &mut Context<Self>) {
        let pending = std::mem::take(&mut self.pending_agents);

        for info in pending.values() {
            self.agent_changed(info, window.as_deref_mut(), cx);
        }
    }

    /// Makes the live view of an agent's tile match the agent's terminal: attaches a new
    /// one, and drops the view of a terminal that ended.
    fn sync_agent_terminal(
        &mut self,
        terminal: TerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(agent) = self
            .data
            .agents
            .iter()
            .find(|agent| agent.terminal == terminal)
        else {
            return;
        };
        let wanted = agent.daemon.as_ref().and_then(|daemon| daemon.terminal);
        let name = SharedString::from(format!(
            "{}, {}, {}",
            agent.name,
            agent.kind.label(),
            agent.model
        ));
        let current = self
            .live_terminals
            .get(&terminal)
            .map(|live| live.daemon_id);

        if current == wanted {
            return;
        }

        if current.is_some() {
            self.drop_live_terminal(terminal);
        }

        let (Some(daemon_id), Some(client)) = (wanted, self.daemon.clone()) else {
            return;
        };

        match client.attach(daemon_id, ATTACH_HISTORY) {
            Ok(channels) => {
                let live = self.live_terminal(terminal, daemon_id, name, channels, window, cx);
                self.live_terminals.insert(terminal, live);
            }
            Err(error) => log::warn!("cannot attach agent terminal {}: {error}", daemon_id.0),
        }
    }

    fn attach_agents(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_pending_agents(Some(&mut *window), cx);

        let terminals: Vec<TerminalId> = self
            .data
            .agents
            .iter()
            .filter(|agent| agent.daemon.is_some())
            .map(|agent| agent.terminal)
            .collect();

        for terminal in terminals {
            self.sync_agent_terminal(terminal, window, cx);
        }
    }

    fn drop_live_terminal(&mut self, terminal: TerminalId) {
        self.live_terminals.remove(&terminal);
        self.terminal_focus.remove(&terminal);
        self.disconnected.remove(&terminal);
    }

    /// Before a fresh connection: the old views are dead, `attach_agents` makes new ones.
    fn drop_agent_terminals(&mut self) {
        let terminals: Vec<TerminalId> = self
            .data
            .agents
            .iter()
            .filter(|agent| agent.daemon.is_some())
            .map(|agent| agent.terminal)
            .collect();

        for terminal in terminals {
            self.drop_live_terminal(terminal);
        }
    }

    /// The task a listed terminal belongs on. User terminals fit any known task;
    /// process terminals only fit real tasks.
    fn wants_tile(&self, info: &TerminalInfo) -> Option<TaskId> {
        let task = self.data.task_with_key(info.task.as_deref()?)?;

        match info.kind {
            DaemonKind::User => Some(task),
            DaemonKind::Process => self.data.task(task)?.daemon.is_some().then_some(task),
            DaemonKind::Agent => None,
        }
    }

    /// Adds a tile for every listed terminal of a known task that has none yet. A
    /// process terminal that has one takes the new name, owner and fixed size.
    pub(crate) fn attach_new_terminals(
        &mut self,
        client: &Client,
        terminals: &[TerminalInfo],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for info in terminals {
            let existing = self
                .live_terminals
                .iter()
                .find(|(_, live)| live.daemon_id == info.id)
                .map(|(terminal, _)| *terminal);

            if let Some(terminal) = existing {
                if info.kind == DaemonKind::Process {
                    self.apply_process_info(terminal, info);
                }

                continue;
            }

            if self.dismissed_terminals.contains(&info.id) {
                continue;
            }

            let Some(task) = self.wants_tile(info) else {
                log::debug!("terminal {} belongs to no known task", info.id.0);
                continue;
            };

            let channels = match client.attach(info.id, ATTACH_HISTORY) {
                Ok(channels) => channels,
                Err(error) => {
                    log::warn!("cannot attach terminal {}: {error}", info.id.0);
                    continue;
                }
            };

            if info.kind == DaemonKind::Process {
                let title = self.process_title(task, info);
                let terminal = self.data.add_live_process(task, &title, None);
                let name = SharedString::from(format!("{title} process terminal"));
                let live = self.live_terminal(terminal, info.id, name, channels, window, cx);

                self.live_terminals.insert(terminal, live);
                self.measure_cells(window, cx);
                self.apply_process_info(terminal, info);
            } else {
                self.add_live_terminal(task, info.id, info.title.as_deref(), channels, window, cx);
            }
        }
    }

    /// Takes a process terminal's title and owner. Its size follows its tile, so the
    /// protocol's `fixed_size` is not used.
    fn apply_process_info(&mut self, terminal: TerminalId, info: &TerminalInfo) {
        let task = self
            .data
            .tasks
            .iter()
            .find(|task| task.processes.contains(&terminal))
            .map(|task| task.id);
        let Some(task) = task else {
            return;
        };

        let title = self.process_title(task, info);
        self.data.update_process(terminal, &title, info);
    }

    /// Reads the cell size of the terminal font, and of the default font that sizes the
    /// tiles. A new font size in the settings measures the font again and reaches every
    /// terminal view, so a tile keeps its pixels and gets more or fewer columns and rows.
    pub(crate) fn measure_cells(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.default_cell_size.is_none() {
            let size = TerminalView::cell_size(swarm_term_view::DEFAULT_FONT_SIZE, window, cx);
            self.default_cell_size = Some((f32::from(size.width), f32::from(size.height)));
        }

        let points = crate::settings::terminal_font_size(cx);

        if self.cell_size.is_some() && (self.font_size - points).abs() < f32::EPSILON {
            return;
        }

        let size = TerminalView::cell_size(points, window, cx);
        self.cell_size = Some((f32::from(size.width), f32::from(size.height)));
        self.font_size = points;

        for live in self.live_terminals.values() {
            live.view
                .update(cx, |view, cx| view.set_font_size(points, window, cx));
        }
    }

    /// What a screen reader announces for a live view.
    fn accessible_name(&self, terminal: TerminalId) -> SharedString {
        let title = self
            .data
            .terminal(terminal)
            .filter(|_| self.is_process(terminal))
            .map(|entry| entry.title.clone());

        match title {
            Some(title) => SharedString::from(format!("{title} process terminal")),
            None => "User terminal".into(),
        }
    }

    fn is_process(&self, terminal: TerminalId) -> bool {
        self.data
            .tasks
            .iter()
            .any(|task| task.processes.contains(&terminal))
    }

    fn process_title(&self, task: TaskId, info: &TerminalInfo) -> String {
        let is_setup = self
            .data
            .task(task)
            .and_then(|task| task.daemon.as_ref())
            .is_some_and(|daemon| daemon.setup_terminal == Some(info.id));

        if is_setup {
            SETUP_TITLE.to_owned()
        } else {
            info.name
                .clone()
                .or_else(|| info.title.clone())
                .unwrap_or_else(|| "Process".to_owned())
        }
    }

    /// Adds a real task to its project, attaches its terminals and opens it.
    pub(crate) fn task_created(
        &mut self,
        info: &TaskInfo,
        terminals: &[TerminalInfo],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = self
            .data
            .projects
            .iter()
            .find(|project| project.daemon == Some(info.project))
            .map(|project| project.id);
        let Some(project) = project else {
            log::warn!("task {} belongs to an unknown project", info.id.0);
            return;
        };

        let task = self.data.sync_task(project, info);

        if let Some(client) = self.daemon.clone() {
            self.attach_new_terminals(&client, terminals, window, cx);
        }

        self.attach_agents(window, cx);
        self.show_setup_exit(task, cx);
        self.select_task(task, cx);
    }

    /// Opens the system folder picker and adds the chosen folder as a project.
    pub(crate) fn pick_project(window: &mut Window, cx: &mut Context<Self>) {
        let picking = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project".into()),
        });

        cx.spawn_in(window, async move |this, cx| {
            let path = match picking.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) | Err(_) => None,
                Ok(Err(error)) => {
                    let reported = this.update(cx, |workspace, cx| {
                        workspace.report_sidebar_error(error.to_string(), cx);
                    });

                    if let Err(error) = reported {
                        log::debug!("the window closed before the picker answered: {error}");
                    }

                    None
                }
            };
            let Some(path) = path else {
                return;
            };

            let added = this.update_in(cx, |workspace, window, cx| {
                workspace.add_project(path, window, cx);
            });

            if let Err(error) = added {
                log::debug!("the window closed before the project was added: {error}");
            }
        })
        .detach();
    }

    /// Asks the daemon to add the project, then opens it.
    pub(crate) fn add_project(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.daemon.clone() else {
            self.report_sidebar_error("The daemon is not running".to_owned(), cx);
            return;
        };

        let adding = cx.background_spawn(async move {
            let project = client.add_project(path)?;
            let terminals = client.list().unwrap_or_else(|error| {
                log::warn!("cannot list terminals after adding a project: {error}");
                Vec::new()
            });

            Ok::<_, ClientError>((project, terminals))
        });

        cx.spawn_in(window, async move |this, cx| {
            let result = adding.await;

            let updated = this.update_in(cx, |workspace, window, cx| match result {
                Ok((project, terminals)) => {
                    workspace.project_added(&project, &terminals, window, cx);
                }
                Err(error) => workspace.report_sidebar_error(error.to_string(), cx),
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the project was added: {error}");
            }
        })
        .detach();
    }

    pub(crate) fn project_added(
        &mut self,
        info: &ProjectInfo,
        terminals: &[TerminalInfo],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = self.data.sync_project(info);

        if let Some(client) = self.daemon.clone() {
            self.attach_new_terminals(&client, terminals, window, cx);
        }

        self.attach_agents(window, cx);
        self.select_project(project, cx);
    }

    pub(crate) fn report_sidebar_error(&mut self, message: String, cx: &mut Context<Self>) {
        log::error!("{message}");
        self.sidebar_error = Some(SharedString::from(message));
        cx.notify();
    }

    pub(crate) fn dismiss_sidebar_error(&mut self, cx: &mut Context<Self>) {
        self.sidebar_error = None;
        cx.notify();
    }

    /// Asks the daemon for a new user terminal. The tile appears when it answers.
    ///
    /// `command` runs instead of the login shell. `setup` marks a GitHub setup step, whose
    /// end the pull request panel follows.
    pub(crate) fn create_live_terminal(
        &mut self,
        client: Client,
        task: TaskId,
        command: Option<Vec<String>>,
        setup: Option<(ProjectId, SetupStep)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.data.key_of_task(task) else {
            return;
        };

        let request = CreateTerminal {
            kind: DaemonKind::User,
            command,
            cwd: self
                .data
                .worktree_of(task)
                .or_else(|| std::env::var_os("HOME").map(PathBuf::from)),
            size: self.initial_grid(window, cx),
            palette: palette_for(cx.theme()),
            task: Some(key),
        };

        let creating = cx.background_spawn(async move { client.create(request) });

        cx.spawn_in(window, async move |this, cx| match creating.await {
            Ok((daemon_id, channels)) => {
                let added = this.update_in(cx, |workspace, window, cx| {
                    let title = setup.map(|(_, step)| step.title());
                    let terminal =
                        workspace.add_live_terminal(task, daemon_id, title, channels, window, cx);
                    workspace.focus_terminal(terminal, cx);

                    if let Some((project, step)) = setup {
                        workspace.setup_terminal_added(terminal, project, step);
                    }
                });

                if let Err(error) = added {
                    log::debug!("the window closed before the terminal was ready: {error}");
                }
            }
            Err(error) => {
                log::error!("cannot create a user terminal: {error}");

                if let Some((project, step)) = setup {
                    let failed = this.update(cx, |workspace, cx| {
                        workspace.setup_terminal_failed(project, step, &error, cx);
                    });

                    if let Err(error) = failed {
                        log::debug!("the window closed before the setup failed: {error}");
                    }
                }
            }
        })
        .detach();
    }

    fn initial_grid(&self, window: &mut Window, cx: &Context<Self>) -> swarm_term::Size {
        let planned = self.planned_user_size();
        let body = Size {
            width: planned.width - px(CHROME_WIDTH),
            height: planned.height - px(CHROME_HEIGHT),
        };
        let font_size = crate::settings::terminal_font_size(cx);
        let measured = TerminalView::size_for(body, font_size, window, cx);

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
        let live = self.live_terminal(
            terminal,
            daemon_id,
            "User terminal".into(),
            channels,
            window,
            cx,
        );

        self.user_sizes.insert(terminal, self.planned_user_size());
        self.live_terminals.insert(terminal, live);

        cx.notify();

        terminal
    }

    pub(crate) fn live_terminal(
        &mut self,
        terminal: TerminalId,
        daemon_id: DaemonTerminalId,
        accessible_name: SharedString,
        channels: TerminalChannels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> LiveTerminal {
        let input = channels.input.clone();
        let view = cx.new(|cx| TerminalView::new(channels, accessible_name, window, cx));
        let points = crate::settings::terminal_font_size(cx);
        view.update(cx, |view, cx| view.set_font_size(points, window, cx));

        // Both the tile and the Swarm view's Terminal tab show this view, so this is the
        // only place that decides who may type.
        let is_worker = self
            .data
            .terminal(terminal)
            .is_some_and(|entry| entry.kind == TerminalKind::Worker);
        if is_worker {
            view.update(cx, |view, _| {
                view.set_typing_gate(crate::settings::allow_worker_typing);
            });
        }

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

    fn on_exited(&mut self, terminal: TerminalId, status: &ExitStatus, cx: &mut Context<Self>) {
        if !self.is_process(terminal) {
            self.close_user_terminal(terminal, cx);
            return;
        }

        self.data
            .set_terminal_meta(terminal, Some(exit_text(status).into()));
        self.data.set_terminal_exit(terminal, status.clone());
        cx.notify();

        if !exit_failed(status) {
            Self::close_after_linger(terminal, cx);
        }
    }

    fn close_after_linger(terminal: TerminalId, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PROCESS_LINGER).await;

            let removed = this.update(cx, |workspace, cx| workspace.close_process(terminal, cx));

            if let Err(error) = removed {
                log::debug!("the window closed before the process tile went away: {error}");
            }
        })
        .detach();
    }

    /// Shows the exit of a task's setup script when no tile of its own will, so a fast
    /// script is never lost. A failure stays until the user closes the tile.
    pub(crate) fn show_setup_exit(&mut self, task: TaskId, cx: &mut Context<Self>) {
        let Some(daemon) = self.data.task(task).and_then(|task| task.daemon.clone()) else {
            return;
        };
        let Some(status) = daemon.setup_exit else {
            return;
        };

        if !self.setup_exits_shown.insert(daemon.id) {
            return;
        }

        let has_tile = daemon.setup_terminal.is_some_and(|setup| {
            self.live_terminals
                .values()
                .any(|live| live.daemon_id == setup)
        });
        if has_tile {
            return;
        }

        let terminal =
            self.data
                .add_live_process(task, SETUP_TITLE, Some(exit_text(&status).into()));
        self.data.set_terminal_exit(terminal, status.clone());

        if !exit_failed(&status) {
            Self::close_after_linger(terminal, cx);
        }

        cx.notify();
    }

    /// A process tile lost its stream while the daemon is still there: the terminal
    /// ended. Reads the task again for the setup script's exit, then shows it.
    fn refresh_ended_process(&mut self, terminal: TerminalId, cx: &mut Context<Self>) {
        let Some(client) = self.daemon.clone() else {
            return;
        };

        let listing = cx.background_spawn(async move { client.list_projects() });

        cx.spawn(async move |this, cx| {
            let result = listing.await;

            let updated = this.update(cx, |workspace, cx| {
                match result {
                    Ok(projects) => workspace.data.sync_projects(&projects),
                    Err(error) => log::warn!("cannot refresh after a terminal ended: {error}"),
                }

                workspace.show_ended_process(terminal, cx);
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the terminal was refreshed: {error}");
            }
        })
        .detach();
    }

    fn show_ended_process(&mut self, terminal: TerminalId, cx: &mut Context<Self>) {
        let owner = self
            .data
            .tasks
            .iter()
            .find(|task| task.processes.contains(&terminal))
            .and_then(|task| task.daemon.clone());
        let Some(daemon) = owner else {
            return;
        };

        let is_setup = self
            .live_terminals
            .get(&terminal)
            .is_some_and(|live| daemon.setup_terminal == Some(live.daemon_id));
        let status = daemon.setup_exit.filter(|_| is_setup);

        if is_setup {
            self.setup_exits_shown.insert(daemon.id);
        }

        let failed = status.as_ref().is_some_and(exit_failed);

        if let Some(status) = status {
            self.data
                .set_terminal_meta(terminal, Some(exit_text(&status).into()));
            self.data.set_terminal_exit(terminal, status);
            cx.notify();
        }

        if !failed {
            Self::close_after_linger(terminal, cx);
        }
    }

    pub(crate) fn on_terminal_event(
        &mut self,
        terminal: TerminalId,
        event: &TerminalEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TerminalEvent::Exited(status) => {
                if !self.github_terminal_exited(terminal, status, window, cx) {
                    self.on_exited(terminal, status, cx);
                }
            }
            TerminalEvent::Disconnected
                if self.is_process(terminal)
                    && self.daemon.as_ref().is_some_and(Client::is_connected) =>
            {
                self.refresh_ended_process(terminal, cx);
            }
            TerminalEvent::Disconnected => {
                self.disconnected.insert(terminal);
                self.reconnect_daemon(cx);
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

fn is_ended(lifecycle: AgentLifecycle) -> bool {
    matches!(
        lifecycle,
        AgentLifecycle::Stopped | AgentLifecycle::Resumable
    )
}

fn exit_text(status: &ExitStatus) -> String {
    match status {
        ExitStatus::Code(code) => format!("exit {code}"),
        ExitStatus::Signal(signal) => format!("signal {signal}"),
    }
}

pub(crate) fn exit_failed(status: &ExitStatus) -> bool {
    !matches!(status, ExitStatus::Code(0))
}

fn connect(launch: &DaemonLaunch) -> Result<Connected, ClientError> {
    let client = Client::connect_or_spawn(&launch.paths, &launch.binary)?;
    let terminals = client.list()?;
    let projects = client.list_projects()?;

    Ok((client, terminals, projects))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::{BufReader, BufWriter, Write};
    use std::os::unix::fs::DirBuilderExt as _;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    use gpui::{TestAppContext, VisualTestContext};
    use swarm_protocol::AgentStatus as DaemonAgentStatus;
    use swarm_protocol::{
        DiffStat, Event, MessageInfo, ProjectId as DaemonProjectId, Request,
        TaskId as DaemonTaskId, VERSION, read, write,
    };
    use swarm_term_view::TerminalEvent;
    use swarm_ui::AgentStatus;

    use super::*;
    use crate::actions::{self, NewTerminal};
    use crate::model::TaskStatus;
    use crate::new_task::NewTaskDialog;
    use crate::palette::Overlay;

    static NEXT_SOCKET: AtomicU32 = AtomicU32::new(0);

    pub(crate) fn setup(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
        cx.update(|cx| {
            swarm_ui::init(cx);
            actions::bind_keys(cx);
        });

        let (workspace, cx) = cx.add_window_view(Workspace::with_sample);
        cx.run_until_parked();

        (workspace, cx)
    }

    /// Starts the project's Start orchestrator and lists no terminals, for any request
    /// that asks for it.
    fn default_reply(request: &Request) -> Option<Event> {
        match request {
            Request::StartOrchestrator { call, project, .. } => Some(Event::AgentStarted {
                call: *call,
                agent: start_agent(100, project.0, DaemonAgentStatus::Idle),
            }),
            Request::List { call } => Some(Event::Terminals {
                call: *call,
                terminals: Vec::new(),
            }),
            Request::ListMessages { call, .. } => Some(Event::Messages {
                call: *call,
                messages: Vec::new(),
            }),
            Request::AgentChanges { call, .. } => Some(Event::AgentChanges {
                call: *call,
                changes: Vec::new(),
            }),
            // Unanswered, the client waits out the 60 s git timeout on the test thread.
            Request::TaskDiff { call, .. } => Some(Event::TaskDiff {
                call: *call,
                diff: None,
            }),
            Request::FileHunks { call, .. } => Some(Event::FileHunks {
                call: *call,
                file: None,
            }),
            Request::ListComments { call, .. } => Some(Event::Comments {
                call: *call,
                comments: Vec::new(),
            }),
            Request::Usage { call } => Some(Event::Usage {
                call: *call,
                usage: Usage::default(),
            }),
            Request::GithubStatus { call, .. } => Some(Event::GithubStatus {
                call: *call,
                status: swarm_protocol::GithubStatus::default(),
            }),
            Request::ListBranches { call, .. } => Some(Event::Branches {
                call: *call,
                branches: Vec::new(),
            }),
            Request::PrPanel { call, .. } => Some(Event::PrPanel {
                call: *call,
                panel: swarm_protocol::PrPanel {
                    commands: vec!["cargo test".to_owned()],
                    required: true,
                    pre_push_hook: None,
                    run: None,
                    draft: None,
                },
            }),
            Request::ProjectConfig { call, .. } => Some(Event::ProjectConfig {
                call: *call,
                fields: swarm_protocol::ProjectConfigFields::default(),
                error: None,
            }),
            Request::Prompts { call, .. } => Some(Event::Prompts {
                call: *call,
                prompts: Vec::new(),
            }),
            Request::DraftPr { call, .. }
            | Request::StopAgents { call }
            | Request::SaveProjectConfig { call, .. }
            | Request::RunChecks { call, .. } => Some(Event::Done { call: *call }),
            Request::PrStatus { call, .. } => Some(Event::Error {
                call: Some(*call),
                id: None,
                message: "no pull request".into(),
            }),
            _ => None,
        }
    }

    pub(crate) fn start_agent(id: u64, project: u64, status: DaemonAgentStatus) -> AgentInfo {
        AgentInfo {
            task: None,
            role: swarm_protocol::AgentRole::StartOrchestrator,
            name: "start".into(),
            worktree: PathBuf::from("/tmp/demo"),
            branch: "main".into(),
            project: DaemonProjectId(project),
            ..agent_info(id, 0, status)
        }
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
        let folder = test_socket_folder();
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
                    None => {
                        while let Ok(request) = read::<Request>(&mut reader, &mut buffer) {
                            if let Some(event) = default_reply(&request) {
                                write(&mut writer, &event).ok();
                                writer.flush().ok();
                            }
                        }
                    }
                }
            }

            std::fs::remove_dir_all(&folder).ok();
        });

        socket
    }

    /// A fresh private folder. A reused process ID must not meet an old folder.
    fn test_socket_folder() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.subsec_nanos());

        PathBuf::from(format!(
            "/tmp/swa-{}-{}-{nanos}",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// A daemon that answers every request with what `reply` returns.
    pub(crate) fn fake_daemon_replying(
        reply: impl Fn(Request) -> Option<Event> + Send + 'static,
    ) -> PathBuf {
        let folder = test_socket_folder();
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

                while let Ok(request) = read::<Request>(&mut reader, &mut buffer) {
                    let fallback = default_reply(&request);

                    if let Some(event) = reply(request).or(fallback) {
                        write(&mut writer, &event).ok();
                        writer.flush().ok();
                    }
                }
            }

            std::fs::remove_dir_all(&folder).ok();
        });

        socket
    }

    /// A daemon that sends `notices[n]` when the client attaches terminal `n`. The daemon
    /// speaks only when asked, so a test asks with [`send_notice`].
    pub(crate) fn fake_daemon_sending(notices: Vec<swarm_protocol::Notice>) -> PathBuf {
        fake_daemon_replying(move |request| match request {
            Request::Attach { id, .. } => usize::try_from(id.0)
                .ok()
                .and_then(|index| notices.get(index))
                .map(|notice| Event::Notice {
                    notice: notice.clone(),
                }),
            _ => None,
        })
    }

    /// Makes a [`fake_daemon_sending`] daemon send its notice number `index`.
    pub(crate) fn send_notice(client: &Client, index: u64) {
        let channels = client
            .attach(DaemonTerminalId(index), 0)
            .expect("send the attach");

        drop(channels);
    }

    pub(crate) fn task_info(id: u64, project: u64, name: &str) -> TaskInfo {
        TaskInfo {
            id: DaemonTaskId(id),
            project: DaemonProjectId(project),
            name: name.into(),
            branch: format!("swarm/{name}"),
            base: "main".into(),
            pr_base: "main".into(),
            worktree: PathBuf::from("/tmp/worktree"),
            ports: Vec::new(),
            setup_terminal: Some(swarm_protocol::TerminalId(10)),
            setup_exit: None,
            agents: Vec::new(),
            created_by: None,
            briefed: false,
            approved: None,
            closed: false,
            pull_request: None,
            close_after_merge: false,
        }
    }

    pub(crate) fn project_info(id: u64, name: &str, tasks: Vec<TaskInfo>) -> ProjectInfo {
        ProjectInfo {
            id: DaemonProjectId(id),
            name: name.into(),
            path: PathBuf::from(format!("/tmp/{name}")),
            base_branch: "main".into(),
            config_error: None,
            tasks,
            start: None,
        }
    }

    pub(crate) fn use_projects(
        workspace: &Entity<Workspace>,
        projects: &[ProjectInfo],
        cx: &mut VisualTestContext,
    ) {
        workspace.update(cx, |workspace, cx| workspace.use_projects(projects, cx));
    }

    pub(crate) fn info(id: u64, kind: DaemonKind, task: &str) -> TerminalInfo {
        TerminalInfo {
            id: swarm_protocol::TerminalId(id),
            kind,
            task: Some(task.into()),
            owner: None,
            name: None,
            fixed_size: None,
            title: Some("zsh".into()),
            cwd: None,
            exit: None,
        }
    }

    fn setup_real(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
        cx.update(|cx| {
            swarm_ui::init(cx);
            actions::bind_keys(cx);
        });

        let (workspace, cx) = cx.add_window_view(Workspace::new);
        cx.run_until_parked();

        (workspace, cx)
    }

    /// A launch that can never reach a daemon: no socket and no binary to start.
    fn unreachable_launch() -> DaemonLaunch {
        let folder = PathBuf::from(format!("/tmp/swa-{}-none", std::process::id()));

        DaemonLaunch {
            paths: Paths::in_dir(folder),
            binary: PathBuf::from("/nonexistent/swarm-daemon"),
        }
    }

    fn source_of(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> DataSource {
        workspace.read_with(cx, |workspace, _| workspace.source.clone())
    }

    #[gpui::test]
    fn the_real_app_starts_connecting_with_no_projects_or_tasks(cx: &mut TestAppContext) {
        let (workspace, cx) = setup_real(cx);

        let (source, projects, tasks, agents) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.source.clone(),
                workspace.data.projects.len(),
                workspace.data.tasks.len(),
                workspace.data.agents.len(),
            )
        });

        assert_eq!(source, DataSource::Connecting);
        assert_eq!((projects, tasks, agents), (0, 0, 0));
    }

    #[gpui::test]
    fn a_failed_connect_makes_the_daemon_unavailable_with_the_reason(cx: &mut TestAppContext) {
        let (workspace, cx) = setup_real(cx);

        workspace.update(cx, |workspace, cx| {
            workspace.start_connect(unreachable_launch(), cx);
        });
        cx.run_until_parked();

        let source = source_of(&workspace, cx);
        assert!(
            matches!(&source, DataSource::Unavailable { reason } if !reason.is_empty()),
            "{source:?}"
        );
    }

    #[gpui::test]
    fn retry_starts_a_new_connect(cx: &mut TestAppContext) {
        let (workspace, cx) = setup_real(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.daemon_launch = Some(unreachable_launch());
            workspace.daemon_unavailable("no daemon".to_owned(), cx);
            workspace.retry_daemon(cx);
        });

        let during = source_of(&workspace, cx);
        cx.run_until_parked();
        let after = source_of(&workspace, cx);

        assert_eq!(during, DataSource::Connecting);
        assert!(matches!(after, DataSource::Unavailable { .. }));
    }

    #[gpui::test]
    fn zero_projects_from_the_daemon_make_the_workspace_live_and_empty(cx: &mut TestAppContext) {
        let (workspace, cx) = setup_real(cx);

        use_projects(&workspace, &[], cx);

        let (source, empty) = workspace.read_with(cx, |workspace, _| {
            (workspace.source.clone(), workspace.has_no_projects())
        });
        assert_eq!(source, DataSource::Live);
        assert!(empty);
    }

    #[gpui::test]
    fn projects_from_the_daemon_show_in_the_real_app(cx: &mut TestAppContext) {
        let (workspace, cx) = setup_real(cx);

        use_projects(&workspace, &[project_info(5, "demo", Vec::new())], cx);

        let (source, names) = workspace.read_with(cx, |workspace, _| {
            let names: Vec<String> = workspace
                .data
                .projects
                .iter()
                .map(|project| project.name.to_string())
                .collect();

            (workspace.source.clone(), names)
        });
        assert_eq!(source, DataSource::Live);
        assert_eq!(names, ["demo"]);
    }

    #[gpui::test]
    fn a_new_terminal_while_unavailable_adds_no_tile(cx: &mut TestAppContext) {
        let (workspace, cx) = setup_real(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.daemon_unavailable("no daemon".to_owned(), cx);
        });

        cx.dispatch_action(NewTerminal);

        let (terminals, tasks) = workspace.read_with(cx, |workspace, _| {
            (workspace.data.terminals.len(), workspace.data.tasks.len())
        });
        assert_eq!((terminals, tasks), (0, 0));
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
        cx.executor().allow_parking();
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
        cx.executor().allow_parking();
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
        cx.executor().allow_parking();
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
        cx.executor().allow_parking();
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
        cx.executor().allow_parking();
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
        cx.executor().allow_parking();
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

    #[gpui::test]
    fn projects_from_the_daemon_replace_the_sample(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let projects = [project_info(5, "demo", vec![task_info(7, 5, "login")])];

        use_projects(&workspace, &projects, cx);

        let (names, tasks, active) = workspace.read_with(cx, |workspace, _| {
            let names: Vec<String> = workspace
                .data
                .projects
                .iter()
                .map(|project| project.name.to_string())
                .collect();
            let task = workspace.active_task();

            (
                names,
                workspace.data.tasks.len(),
                (
                    task.name.to_string(),
                    task.branch.to_string(),
                    task.orchestrator,
                ),
            )
        });
        assert_eq!(names, ["demo"]);
        assert_eq!(tasks, 1);
        assert_eq!(active, ("login".to_owned(), "swarm/login".to_owned(), None));
    }

    #[gpui::test]
    fn a_live_project_without_tasks_opens_on_start(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        use_projects(&workspace, &[project_info(5, "demo", Vec::new())], cx);

        let is_start = workspace.read_with(cx, |workspace, _| workspace.active_task().is_start);
        assert!(is_start);
    }

    #[gpui::test]
    fn a_real_task_gets_tiles_for_its_process_and_user_terminals(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.executor().allow_parking();
        let client = Client::connect(&fake_daemon()).expect("connect to the fake daemon");
        use_projects(
            &workspace,
            &[project_info(5, "demo", vec![task_info(7, 5, "login")])],
            cx,
        );
        let listed = [
            info(10, DaemonKind::Process, "task:7"),
            info(11, DaemonKind::User, "task:7"),
            info(12, DaemonKind::Process, "task:8"),
        ];

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &listed, window, cx);
        });

        let (processes, users, live, setup_title) = workspace.read_with(cx, |workspace, _| {
            let task = workspace.active_task();
            let title = task
                .processes
                .first()
                .and_then(|id| workspace.data.terminal(*id))
                .map(|terminal| terminal.title.to_string());

            (
                task.processes.len(),
                task.user_terminals.len(),
                workspace.live_terminals.len(),
                title,
            )
        });
        assert_eq!((processes, users, live), (1, 1, 2));
        assert_eq!(setup_title.as_deref(), Some("Setup"));
    }

    #[gpui::test]
    fn a_created_task_adds_and_selects_a_tab(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.executor().allow_parking();
        let client = Client::connect(&fake_daemon()).expect("connect to the fake daemon");
        use_projects(&workspace, &[project_info(5, "demo", Vec::new())], cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &[], window, cx);
        });

        workspace.update_in(cx, |workspace, window, cx| {
            let created = task_info(8, 5, "signup");
            let terminals = [info(10, DaemonKind::Process, "task:8")];
            workspace.task_created(&created, &terminals, window, cx);
        });

        let (active, processes, tabs) = workspace.read_with(cx, |workspace, _| {
            let task = workspace.active_task();

            (
                task.name.to_string(),
                task.processes.len(),
                workspace.data.tasks_of(workspace.active_project).count(),
            )
        });
        assert_eq!(active, "signup");
        assert_eq!(processes, 1);
        assert_eq!(tabs, 2);
    }

    static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

    pub(crate) fn agent_info(id: u64, task: u64, status: DaemonAgentStatus) -> AgentInfo {
        AgentInfo {
            id: swarm_protocol::AgentId(id),
            project: DaemonProjectId(5),
            task: Some(DaemonTaskId(task)),
            role: swarm_protocol::AgentRole::Orchestrator,
            kind: swarm_protocol::AgentKind::ClaudeCode,
            model: "opus".into(),
            name: "orchestrator".into(),
            worktree: PathBuf::from("/tmp/worktree"),
            branch: "swarm/login".into(),
            terminal: None,
            status,
            message: None,
            lifecycle: AgentLifecycle::Running,
            revision: NEXT_REVISION.fetch_add(1, Ordering::Relaxed),
            created_at: 0,
            started_at: None,
            ended_at: None,
        }
    }

    pub(crate) fn real_task_workspace<'a>(
        socket: &std::path::Path,
        cx: &'a mut TestAppContext,
    ) -> (Entity<Workspace>, &'a mut VisualTestContext, Client) {
        let (workspace, cx) = setup(cx);
        cx.executor().allow_parking();
        let client = Client::connect(socket).expect("connect to the fake daemon");
        use_projects(
            &workspace,
            &[project_info(5, "demo", vec![task_info(7, 5, "login")])],
            cx,
        );
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client.clone(), &[], window, cx);
        });

        (workspace, cx, client)
    }

    fn change_agent(workspace: &Entity<Workspace>, info: &AgentInfo, cx: &mut VisualTestContext) {
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.agent_changed(info, Some(window), cx);
        });
    }

    fn task_view(
        workspace: &Entity<Workspace>,
        cx: &VisualTestContext,
    ) -> (AgentStatus, TaskStatus, AgentStatus, usize) {
        workspace.read_with(cx, |workspace, _| {
            let task = workspace.active_task();
            let agent = task
                .orchestrator
                .and_then(|id| workspace.data.agent(id))
                .map_or(AgentStatus::Idle, |agent| agent.status);

            (
                agent,
                task.status,
                crate::tab_bar::ring_status(task.status),
                crate::views::tile_specs(workspace).len(),
            )
        })
    }

    #[gpui::test]
    fn a_real_task_shows_an_orchestrator_tile_once_the_daemon_reports_it(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = real_task_workspace(&fake_daemon(), cx);
        let before = task_view(&workspace, cx).3;

        change_agent(&workspace, &agent_info(1, 7, DaemonAgentStatus::Idle), cx);

        assert_eq!(before, 0);
        assert_eq!(task_view(&workspace, cx).3, 1);
    }

    #[gpui::test]
    fn an_agent_change_updates_the_tile_status_and_the_tab_ring(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = real_task_workspace(&fake_daemon(), cx);
        change_agent(&workspace, &agent_info(1, 7, DaemonAgentStatus::Idle), cx);

        change_agent(
            &workspace,
            &agent_info(1, 7, DaemonAgentStatus::Working),
            cx,
        );

        let (tile, _, ring, _) = task_view(&workspace, cx);
        assert_eq!(tile, AgentStatus::Working);
        assert_eq!(ring, AgentStatus::Working);
    }

    #[gpui::test]
    fn a_blocked_agent_makes_the_project_need_you(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = real_task_workspace(&fake_daemon(), cx);

        change_agent(
            &workspace,
            &agent_info(
                1,
                7,
                DaemonAgentStatus::Blocked(swarm_term::BlockedKind::Permission),
            ),
            cx,
        );

        let (tile, status, ring, _) = task_view(&workspace, cx);
        assert_eq!(tile, AgentStatus::Blocked);
        assert_eq!(status, TaskStatus::NeedsYou);
        assert_eq!(ring, AgentStatus::Blocked);
    }

    #[gpui::test]
    fn the_blocked_kind_and_message_show_in_the_tile_meta(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = real_task_workspace(&fake_daemon(), cx);
        let mut info = agent_info(
            1,
            7,
            DaemonAgentStatus::Blocked(swarm_term::BlockedKind::Question),
        );
        info.message = Some("which branch?".into());

        change_agent(&workspace, &info, cx);

        let (meta, label) = workspace.read_with(cx, |workspace, _| {
            let agent = workspace.data.agents.first().expect("one agent");
            (agent.meta().to_string(), agent.accessible_label())
        });
        assert_eq!(
            meta,
            "Claude Code \u{b7} opus \u{b7} question \u{b7} which branch?"
        );
        assert_eq!(
            label,
            "orchestrator, Claude Code, opus, needs you, question, which branch?"
        );
    }

    #[gpui::test]
    fn a_working_agent_becomes_idle_again_when_it_finishes(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = real_task_workspace(&fake_daemon(), cx);
        change_agent(
            &workspace,
            &agent_info(1, 7, DaemonAgentStatus::Working),
            cx,
        );

        change_agent(&workspace, &agent_info(1, 7, DaemonAgentStatus::Done), cx);

        let (tile, status, _, _) = task_view(&workspace, cx);
        assert_eq!(tile, AgentStatus::Done);
        assert_eq!(status, TaskStatus::Idle);
    }

    #[gpui::test]
    fn an_agent_of_an_unknown_task_waits_for_the_task(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = real_task_workspace(&fake_daemon(), cx);
        change_agent(
            &workspace,
            &agent_info(2, 8, DaemonAgentStatus::Working),
            cx,
        );

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.task_created(&task_info(8, 5, "signup"), &[], window, cx);
        });

        let status = workspace.read_with(cx, |workspace, _| workspace.active_task().status);
        assert_eq!(status, TaskStatus::Working);
    }

    #[gpui::test]
    fn a_stopped_orchestrator_keeps_its_tile_without_a_terminal(cx: &mut TestAppContext) {
        let socket = fake_daemon_replying(|_| None);
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);
        let mut running = agent_info(1, 7, DaemonAgentStatus::Working);
        running.terminal = Some(swarm_protocol::TerminalId(20));
        change_agent(&workspace, &running, cx);
        let live = workspace.read_with(cx, |workspace, _| workspace.live_terminals.len());

        let mut stopped = agent_info(1, 7, DaemonAgentStatus::Done);
        stopped.lifecycle = AgentLifecycle::Stopped;
        change_agent(&workspace, &stopped, cx);

        let (still_live, tiles) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.live_terminals.len(),
                crate::views::tile_specs(workspace).len(),
            )
        });
        assert_eq!((live, still_live, tiles), (1, 0, 1));
    }

    #[gpui::test]
    fn an_agent_event_from_the_daemon_reaches_the_tile(cx: &mut TestAppContext) {
        let socket = fake_daemon_replying(|request| match request {
            Request::Attach { .. } => Some(Event::AgentChanged {
                agent: agent_info(1, 7, DaemonAgentStatus::Working),
            }),
            _ => None,
        });
        let (workspace, cx, client) = real_task_workspace(&socket, cx);

        client
            .attach(swarm_protocol::TerminalId(99), 0)
            .expect("send the attach");
        cx.executor().allow_parking();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while task_view(&workspace, cx).0 != AgentStatus::Working
            && std::time::Instant::now() < deadline
        {
            cx.run_until_parked();
            std::thread::yield_now();
        }

        let (tile, status, _, _) = task_view(&workspace, cx);
        assert_eq!(tile, AgentStatus::Working);
        assert_eq!(status, TaskStatus::Working);
    }

    fn temp_settings_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "swarm-app-{name}-{}-{}",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[gpui::test]
    fn the_new_task_sheet_sends_the_chosen_kind_and_first_message(cx: &mut TestAppContext) {
        let (sent, requests) = flume::unbounded();
        let socket = fake_daemon_replying(move |request| match request {
            Request::CreateTask { call, task } => {
                sent.send(task).ok();

                Some(Event::TaskCreated {
                    call,
                    task: task_info(8, 5, "login"),
                })
            }
            Request::List { call } => Some(Event::Terminals {
                call,
                terminals: Vec::new(),
            }),
            _ => None,
        });
        let (workspace, cx, _client) = {
            let (workspace, cx) = setup(cx);
            cx.executor().allow_parking();
            let client = Client::connect(&socket).expect("connect to the fake daemon");
            use_projects(&workspace, &[project_info(5, "demo", Vec::new())], cx);
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.use_daemon(client.clone(), &[], window, cx);
            });

            (workspace, cx, client)
        };
        let path = temp_settings_path("new-task");
        let mut settings = swarm_settings::Settings::default();
        settings.agents.orchestrator = swarm_settings::AgentKindSetting::Codex;
        swarm_settings::save(&path, &settings).expect("write the settings");
        workspace.update(cx, |workspace, _| {
            workspace.settings_path = Some(path.clone());
        });

        cx.simulate_keystrokes("cmd-t");
        cx.simulate_input("login");
        let dialog = workspace.read_with(cx, |workspace, _| match &workspace.overlay {
            Some(Overlay::NewTask { view, .. }) => Some(view.clone()),
            _ => None,
        });
        let dialog = dialog.expect("the sheet is open");
        let default_kind = dialog.read_with(cx, |dialog, _| dialog.orchestrator_kind());
        dialog.update(cx, |dialog, cx| {
            dialog.choose_orchestrator(crate::model::AgentKind::ClaudeCode);
            dialog.type_message("  start with the tests ", cx);
            dialog.create(cx);
        });
        cx.executor().allow_parking();
        cx.run_until_parked();
        std::fs::remove_file(&path).ok();

        let task = requests.try_recv().expect("the daemon got CreateTask");
        assert_eq!(default_kind, crate::model::AgentKind::Codex);
        assert_eq!(
            task.orchestrator,
            Some(swarm_protocol::AgentKind::ClaudeCode)
        );
        assert_eq!(task.first_message.as_deref(), Some("start with the tests"));
    }

    #[gpui::test]
    fn a_create_error_keeps_the_sheet_open_with_the_message(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let socket = fake_daemon_replying(|request| match request {
            Request::CreateTask { call, .. } => Some(Event::Error {
                call: Some(call),
                id: None,
                message: "branch exists".into(),
            }),
            _ => None,
        });
        cx.executor().allow_parking();
        let client = Client::connect(&socket).expect("connect to the fake daemon");
        use_projects(&workspace, &[project_info(5, "demo", Vec::new())], cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &[], window, cx);
        });

        cx.simulate_keystrokes("cmd-t");
        cx.simulate_input("login");
        cx.simulate_keystrokes("enter");
        cx.executor().allow_parking();
        cx.run_until_parked();

        let state = workspace.read_with(cx, |workspace, cx| match &workspace.overlay {
            Some(Overlay::NewTask { view, .. }) => {
                let dialog = view.read(cx);
                Some((dialog.error_text().map(str::to_owned), dialog.is_busy()))
            }
            _ => None,
        });
        let (error, busy) = state.expect("the sheet is still open");
        assert!(error.is_some_and(|error| error.contains("branch exists")));
        assert!(!busy);
    }

    #[gpui::test]
    fn a_successful_create_closes_the_sheet_and_opens_the_tab(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let socket = fake_daemon_replying(|request| match request {
            Request::CreateTask { call, .. } => Some(Event::TaskCreated {
                call,
                task: task_info(8, 5, "login"),
            }),
            Request::List { call } => Some(Event::Terminals {
                call,
                terminals: vec![info(10, DaemonKind::Process, "task:8")],
            }),
            _ => None,
        });
        cx.executor().allow_parking();
        let client = Client::connect(&socket).expect("connect to the fake daemon");
        use_projects(&workspace, &[project_info(5, "demo", Vec::new())], cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &[], window, cx);
        });

        cx.simulate_keystrokes("cmd-t");
        cx.simulate_input("login");
        cx.simulate_keystrokes("enter");
        cx.executor().allow_parking();
        cx.run_until_parked();

        let (closed, name, processes) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.overlay.is_none(),
                workspace.active_task().name.to_string(),
                workspace.active_task().processes.len(),
            )
        });
        assert!(closed);
        assert_eq!(name, "login");
        assert_eq!(processes, 1);
    }

    #[gpui::test]
    fn a_create_error_after_the_sheet_closed_goes_to_the_sidebar(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let (release, gate) = flume::bounded::<()>(1);
        let socket = fake_daemon_replying(move |request| match request {
            Request::CreateTask { call, .. } => {
                gate.recv().ok();

                Some(Event::Error {
                    call: Some(call),
                    id: None,
                    message: "branch exists".into(),
                })
            }
            _ => None,
        });
        cx.executor().allow_parking();
        let client = Client::connect(&socket).expect("connect to the fake daemon");
        use_projects(&workspace, &[project_info(5, "demo", Vec::new())], cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &[], window, cx);
        });

        cx.simulate_keystrokes("cmd-t");
        cx.simulate_input("login");
        let dialog = workspace.read_with(cx, |workspace, _| match &workspace.overlay {
            Some(Overlay::NewTask { view, .. }) => Some(view.clone()),
            _ => None,
        });
        dialog
            .expect("the sheet is open")
            .update(cx, NewTaskDialog::create);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.close_overlay(window, cx);
        });
        release.send(()).expect("the fake daemon is waiting");
        cx.executor().allow_parking();
        cx.run_until_parked();

        let error = workspace.read_with(cx, |workspace, _| workspace.sidebar_error.clone());
        assert!(error.is_some_and(|message| message.contains("branch exists")));
    }

    #[gpui::test]
    fn adding_a_project_selects_it(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let socket = fake_daemon_replying(|request| match request {
            Request::AddProject { call, .. } => Some(Event::ProjectAdded {
                call,
                project: project_info(6, "extra", Vec::new()),
            }),
            Request::List { call } => Some(Event::Terminals {
                call,
                terminals: Vec::new(),
            }),
            _ => None,
        });
        cx.executor().allow_parking();
        let client = Client::connect(&socket).expect("connect to the fake daemon");
        use_projects(&workspace, &[project_info(5, "demo", Vec::new())], cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &[], window, cx);
        });

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_project(PathBuf::from("/tmp/extra"), window, cx);
        });
        cx.executor().allow_parking();
        cx.run_until_parked();

        let active = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .project(workspace.active_project)
                .map(|project| project.name.to_string())
        });
        assert_eq!(active.as_deref(), Some("extra"));
    }

    #[gpui::test]
    fn an_add_project_error_shows_a_dismissible_message(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let socket = fake_daemon_replying(|request| match request {
            Request::AddProject { call, .. } => Some(Event::Error {
                call: Some(call),
                id: None,
                message: "not a git repository".into(),
            }),
            _ => None,
        });
        cx.executor().allow_parking();
        let client = Client::connect(&socket).expect("connect to the fake daemon");
        use_projects(&workspace, &[project_info(5, "demo", Vec::new())], cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &[], window, cx);
        });

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_project(PathBuf::from("/tmp/nothing"), window, cx);
        });
        cx.executor().allow_parking();
        cx.run_until_parked();
        let shown = workspace.read_with(cx, |workspace, _| workspace.sidebar_error.clone());
        workspace.update(cx, Workspace::dismiss_sidebar_error);
        let dismissed = workspace.read_with(cx, |workspace, _| workspace.sidebar_error.clone());

        assert!(shown.is_some_and(|message| message.contains("not a git repository")));
        assert!(dismissed.is_none());
    }

    #[gpui::test]
    fn an_exited_process_tile_goes_away_after_three_seconds(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.executor().allow_parking();
        let client = Client::connect(&fake_daemon()).expect("connect to the fake daemon");
        use_projects(
            &workspace,
            &[project_info(5, "demo", vec![task_info(7, 5, "login")])],
            cx,
        );
        let listed = [info(10, DaemonKind::Process, "task:7")];
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &listed, window, cx);
        });
        let tile = workspace
            .read_with(cx, |workspace, _| {
                workspace.live_terminals.keys().next().copied()
            })
            .expect("a process tile");

        workspace.update_in(cx, |workspace, window, cx| {
            let exited = TerminalEvent::Exited(ExitStatus::Code(0));
            workspace.on_terminal_event(tile, &exited, window, cx);
        });
        let meta = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .terminal(tile)
                .and_then(|terminal| terminal.meta.clone())
        });
        cx.executor().advance_clock(Duration::from_millis(2900));
        cx.run_until_parked();
        let before =
            workspace.read_with(cx, |workspace, _| workspace.active_task().processes.len());
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        let after = workspace.read_with(cx, |workspace, _| {
            (
                workspace.active_task().processes.len(),
                workspace.live_terminals.len(),
            )
        });

        assert_eq!(meta.as_deref(), Some("exit 0"));
        assert_eq!(before, 1);
        assert_eq!(after, (0, 0));
    }

    #[gpui::test]
    fn a_failed_process_tile_stays_until_closed(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.executor().allow_parking();
        let client = Client::connect(&fake_daemon()).expect("connect to the fake daemon");
        use_projects(
            &workspace,
            &[project_info(5, "demo", vec![task_info(7, 5, "login")])],
            cx,
        );
        let listed = [info(10, DaemonKind::Process, "task:7")];
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &listed, window, cx);
        });
        let tile = workspace
            .read_with(cx, |workspace, _| {
                workspace.live_terminals.keys().next().copied()
            })
            .expect("a process tile");

        workspace.update_in(cx, |workspace, window, cx| {
            let exited = TerminalEvent::Exited(ExitStatus::Code(2));
            workspace.on_terminal_event(tile, &exited, window, cx);
        });
        let meta = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .terminal(tile)
                .and_then(|terminal| terminal.meta.clone())
        });
        cx.executor().advance_clock(Duration::from_millis(2900));
        cx.run_until_parked();
        let before =
            workspace.read_with(cx, |workspace, _| workspace.active_task().processes.len());
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        let after = workspace.read_with(cx, |workspace, _| {
            (
                workspace.active_task().processes.len(),
                workspace.live_terminals.len(),
            )
        });

        assert_eq!(meta.as_deref(), Some("exit 2"));
        assert_eq!(before, 1);
        assert_eq!(after, (1, 1));
    }

    fn finished_setup(id: u64, status: ExitStatus) -> TaskInfo {
        TaskInfo {
            setup_exit: Some(status),
            ..task_info(id, 5, "login")
        }
    }

    fn setup_tile_state(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> Vec<String> {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .active_task()
                .processes
                .iter()
                .filter_map(|id| workspace.data.terminal(*id))
                .map(|terminal| {
                    let meta = terminal.meta.clone().unwrap_or_default();
                    format!("{} {meta}", terminal.title)
                })
                .collect()
        })
    }

    #[gpui::test]
    fn a_setup_that_ended_before_its_tile_appeared_is_shown_then_removed(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.executor().allow_parking();
        let client = Client::connect(&fake_daemon()).expect("connect to the fake daemon");
        use_projects(&workspace, &[project_info(5, "demo", Vec::new())], cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &[], window, cx);
        });

        workspace.update_in(cx, |workspace, window, cx| {
            let created = finished_setup(8, ExitStatus::Code(0));
            workspace.task_created(&created, &[], window, cx);
        });
        let shown = setup_tile_state(&workspace, cx);
        cx.executor()
            .advance_clock(PROCESS_LINGER + Duration::from_millis(100));
        cx.run_until_parked();
        let later = setup_tile_state(&workspace, cx);

        assert_eq!(shown, ["Setup exit 0"]);
        assert!(later.is_empty());
    }

    #[gpui::test]
    fn a_failed_setup_that_ended_early_stays_until_closed(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.executor().allow_parking();
        let client = Client::connect(&fake_daemon()).expect("connect to the fake daemon");
        use_projects(&workspace, &[project_info(5, "demo", Vec::new())], cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &[], window, cx);
        });

        workspace.update_in(cx, |workspace, window, cx| {
            let created = finished_setup(8, ExitStatus::Code(3));
            workspace.task_created(&created, &[], window, cx);
        });
        cx.executor().advance_clock(PROCESS_LINGER * 2);
        cx.run_until_parked();
        let kept = setup_tile_state(&workspace, cx);
        let terminal = workspace.read_with(cx, |workspace, _| {
            workspace.active_task().processes.first().copied()
        });
        workspace.update(cx, |workspace, cx| {
            if let Some(terminal) = terminal {
                workspace.close_process(terminal, cx);
            }
        });
        let closed = setup_tile_state(&workspace, cx);

        assert_eq!(kept, ["Setup exit 3"]);
        assert!(closed.is_empty());
    }

    #[gpui::test]
    fn a_failed_setup_that_ended_early_needs_attention(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.executor().allow_parking();
        let client = Client::connect(&fake_daemon()).expect("connect to the fake daemon");
        use_projects(&workspace, &[project_info(5, "demo", Vec::new())], cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &[], window, cx);
        });

        workspace.update_in(cx, |workspace, window, cx| {
            let created = finished_setup(8, ExitStatus::Code(3));
            workspace.task_created(&created, &[], window, cx);
        });

        assert_eq!(
            process_activities(&workspace, cx),
            [swarm_layout::Activity::Attention]
        );
    }

    /// A process tile that lost its stream after its setup script ended with `status`.
    fn ended_process_workspace(
        status: ExitStatus,
        cx: &mut TestAppContext,
    ) -> (Entity<Workspace>, &mut VisualTestContext) {
        let (workspace, cx) = setup(cx);
        let task = finished_setup(7, status);
        let socket = fake_daemon_replying(move |request| match request {
            Request::ListProjects { call } => Some(Event::Projects {
                call,
                projects: vec![project_info(5, "demo", vec![task.clone()])],
            }),
            _ => None,
        });
        cx.executor().allow_parking();
        let client = Client::connect(&socket).expect("connect to the fake daemon");
        use_projects(
            &workspace,
            &[project_info(5, "demo", vec![task_info(7, 5, "login")])],
            cx,
        );
        let listed = [info(10, DaemonKind::Process, "task:7")];
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &listed, window, cx);
        });
        let tile = workspace
            .read_with(cx, |workspace, _| {
                workspace.live_terminals.keys().next().copied()
            })
            .expect("a process tile");

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.on_terminal_event(tile, &TerminalEvent::Disconnected, window, cx);
        });
        cx.executor().allow_parking();
        cx.run_until_parked();

        (workspace, cx)
    }

    fn ended_process_state(
        status: ExitStatus,
        cx: &mut TestAppContext,
    ) -> (Vec<String>, Vec<String>) {
        let (workspace, cx) = ended_process_workspace(status, cx);
        let shown = setup_tile_state(&workspace, cx);
        cx.executor()
            .advance_clock(PROCESS_LINGER + Duration::from_millis(100));
        cx.run_until_parked();

        (shown, setup_tile_state(&workspace, cx))
    }

    fn process_activities(
        workspace: &Entity<Workspace>,
        cx: &VisualTestContext,
    ) -> Vec<swarm_layout::Activity> {
        workspace.read_with(cx, |workspace, _| {
            crate::views::tile_specs(workspace)
                .iter()
                .filter(|spec| spec.kind == swarm_layout::TileKind::Process)
                .map(|spec| spec.activity)
                .collect()
        })
    }

    #[gpui::test]
    fn a_failed_setup_in_a_live_tile_needs_attention(cx: &mut TestAppContext) {
        let (workspace, cx) = ended_process_workspace(ExitStatus::Code(3), cx);

        assert_eq!(
            process_activities(&workspace, cx),
            [swarm_layout::Activity::Attention]
        );
    }

    #[gpui::test]
    fn a_setup_that_ended_well_in_a_live_tile_stays_running(cx: &mut TestAppContext) {
        let (workspace, cx) = ended_process_workspace(ExitStatus::Code(0), cx);

        assert_eq!(
            process_activities(&workspace, cx),
            [swarm_layout::Activity::Running]
        );
    }

    #[gpui::test]
    fn a_process_tile_that_ends_shows_the_setup_exit_then_goes_away(cx: &mut TestAppContext) {
        let (shown, later) = ended_process_state(ExitStatus::Code(0), cx);

        assert_eq!(shown, ["Setup exit 0"]);
        assert!(later.is_empty());
    }

    #[gpui::test]
    fn a_process_tile_that_ends_with_a_failure_stays(cx: &mut TestAppContext) {
        let (shown, later) = ended_process_state(ExitStatus::Code(3), cx);

        assert_eq!(shown, ["Setup exit 3"]);
        assert_eq!(later, shown);
    }

    fn worker_info(id: u64, task: u64, name: &str, status: DaemonAgentStatus) -> AgentInfo {
        AgentInfo {
            role: swarm_protocol::AgentRole::Worker,
            name: name.into(),
            ..agent_info(id, task, status)
        }
    }

    fn tile_titles(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> Vec<String> {
        workspace.read_with(cx, |workspace, _| {
            crate::views::tile_specs(workspace)
                .iter()
                .filter_map(|spec| {
                    let id = TerminalId(spec.id.0);
                    workspace.data.terminal(id).map(|t| t.title.to_string())
                })
                .collect()
        })
    }

    #[gpui::test]
    fn workers_get_tiles_after_the_orchestrator(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = real_task_workspace(&fake_daemon(), cx);

        change_agent(
            &workspace,
            &worker_info(2, 7, "api", DaemonAgentStatus::Working),
            cx,
        );
        change_agent(&workspace, &agent_info(1, 7, DaemonAgentStatus::Idle), cx);
        change_agent(
            &workspace,
            &worker_info(3, 7, "ui", DaemonAgentStatus::Idle),
            cx,
        );

        let (kinds, agents) = workspace.read_with(cx, |workspace, _| {
            let kinds: Vec<_> = crate::views::tile_specs(workspace)
                .iter()
                .map(|spec| spec.kind)
                .collect();

            (
                kinds,
                workspace.data.agents_of(workspace.active_task).count(),
            )
        });
        assert_eq!(
            kinds,
            [
                swarm_layout::TileKind::Orchestrator,
                swarm_layout::TileKind::Worker,
                swarm_layout::TileKind::Worker
            ]
        );
        assert_eq!(tile_titles(&workspace, cx), ["orchestrator", "api", "ui"]);
        assert_eq!(agents, 3);
    }

    #[gpui::test]
    fn a_worker_gets_a_live_view_and_keeps_its_tile_when_it_stops(cx: &mut TestAppContext) {
        let socket = fake_daemon_replying(|_| None);
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);
        let mut running = worker_info(2, 7, "api", DaemonAgentStatus::Working);
        running.terminal = Some(swarm_protocol::TerminalId(21));
        change_agent(&workspace, &running, cx);
        let live = workspace.read_with(cx, |workspace, _| workspace.live_terminals.len());

        let mut stopped = worker_info(2, 7, "api", DaemonAgentStatus::Done);
        stopped.lifecycle = AgentLifecycle::Stopped;
        change_agent(&workspace, &stopped, cx);

        let still_live = workspace.read_with(cx, |workspace, _| workspace.live_terminals.len());
        assert_eq!((live, still_live), (1, 0));
        assert_eq!(tile_titles(&workspace, cx), ["api"]);
    }

    fn process_info(id: u64, name: Option<&str>, title: &str) -> TerminalInfo {
        TerminalInfo {
            name: name.map(str::to_owned),
            title: Some(title.into()),
            owner: Some(swarm_protocol::AgentId(1)),
            ..info(id, DaemonKind::Process, "task:7")
        }
    }

    fn change_terminal(
        workspace: &Entity<Workspace>,
        info: &TerminalInfo,
        cx: &mut VisualTestContext,
    ) {
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.terminal_changed(info, window, cx);
        });
    }

    fn process_tiles(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> Vec<String> {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .active_task()
                .processes
                .iter()
                .filter_map(|id| workspace.data.terminal(*id))
                .map(|terminal| terminal.title.to_string())
                .collect()
        })
    }

    #[gpui::test]
    fn an_agent_process_gets_a_tile_titled_by_name_or_else_by_title(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = real_task_workspace(&fake_daemon(), cx);
        change_agent(&workspace, &agent_info(1, 7, DaemonAgentStatus::Idle), cx);

        change_terminal(&workspace, &process_info(30, Some("dev server"), "zsh"), cx);
        change_terminal(&workspace, &process_info(31, None, "npm test"), cx);
        change_terminal(&workspace, &process_info(30, Some("dev server"), "zsh"), cx);

        let (owner, orchestrator) = workspace.read_with(cx, |workspace, _| {
            let task = workspace.active_task();
            let process = task
                .processes
                .first()
                .and_then(|id| workspace.data.terminal(*id));

            (process.and_then(|process| process.owner), task.orchestrator)
        });
        assert_eq!(process_tiles(&workspace, cx), ["dev server", "npm test"]);
        assert!(owner.is_some());
        assert_eq!(owner, orchestrator);
    }

    #[gpui::test]
    fn a_renamed_agent_process_takes_the_new_title(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = real_task_workspace(&fake_daemon(), cx);
        change_terminal(&workspace, &process_info(30, None, "zsh"), cx);

        change_terminal(&workspace, &process_info(30, Some("tests"), "zsh"), cx);

        assert_eq!(process_tiles(&workspace, cx), ["tests"]);
    }

    #[gpui::test]
    fn a_process_with_a_size_an_agent_asked_for_still_gets_the_process_tile(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx, _client) = real_task_workspace(&fake_daemon(), cx);
        let asked = TerminalInfo {
            fixed_size: Some(swarm_protocol::GridSize {
                cols: 140,
                rows: 40,
            }),
            ..process_info(30, Some("dev server"), "zsh")
        };

        change_terminal(&workspace, &asked, cx);

        let (spec, height, process_height) = workspace.read_with(cx, |workspace, _| {
            let specs = crate::views::tile_specs(workspace);
            let spec = specs.last().copied().expect("a process spec");
            let layout = crate::views::compute_layout(workspace);

            (
                spec,
                layout.rect(spec.id).map(|rect| rect.size.height),
                crate::views::layout_style(workspace).process_tile_height,
            )
        });
        assert_eq!(spec.kind, swarm_layout::TileKind::Process);
        assert_eq!(spec.fixed, None);
        let height = height.expect("the process tile is placed");
        assert!((height - process_height).abs() < 0.01, "{height}");
    }

    #[gpui::test]
    fn the_app_resizes_a_process_terminal_to_its_tile_whatever_size_an_agent_asked_for(
        cx: &mut TestAppContext,
    ) {
        let (seen, requests) = flume::unbounded();
        let socket = fake_daemon_replying(move |request| {
            seen.send(request).ok();
            None
        });
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);
        let asked = TerminalInfo {
            fixed_size: Some(swarm_protocol::GridSize { cols: 80, rows: 12 }),
            ..process_info(30, Some("dev server"), "zsh")
        };
        change_terminal(&workspace, &asked, cx);

        let input = workspace.read_with(cx, |workspace, _| {
            workspace
                .live_terminals
                .values()
                .next()
                .map(|live| live.input.clone())
        });
        let input = input.expect("a live process tile");
        let size = swarm_term::Size {
            cols: 100,
            rows: 30,
            cell_width: 8,
            cell_height: 17,
        };
        input
            .send(TerminalInput::Resize(size))
            .expect("send resize");
        input.send(TerminalInput::Focus(true)).expect("send focus");

        let mut seen = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !seen.iter().any(|request| {
            matches!(
                request,
                Request::Input {
                    input: TerminalInput::Focus(true),
                    ..
                }
            )
        }) && std::time::Instant::now() < deadline
        {
            seen.extend(requests.try_iter());
            std::thread::yield_now();
        }

        let resized = seen.iter().any(|request| {
            matches!(
                request,
                Request::Input {
                    input: TerminalInput::Resize(resized),
                    ..
                } if *resized == size
            )
        });
        assert!(resized, "{seen:?}");
    }

    fn process_tile_after_exit(code: u32, cx: &mut TestAppContext) -> (usize, usize) {
        let (workspace, cx, _client) = real_task_workspace(&fake_daemon(), cx);
        change_terminal(&workspace, &process_info(30, Some("tests"), "zsh"), cx);
        let tile = workspace
            .read_with(cx, |workspace, _| {
                workspace.live_terminals.keys().next().copied()
            })
            .expect("a process tile");

        workspace.update_in(cx, |workspace, window, cx| {
            let exited = TerminalEvent::Exited(ExitStatus::Code(code));
            workspace.on_terminal_event(tile, &exited, window, cx);
        });
        cx.executor().advance_clock(PROCESS_LINGER / 2);
        cx.run_until_parked();
        let before = process_tiles(&workspace, cx).len();
        cx.executor().advance_clock(PROCESS_LINGER);
        cx.run_until_parked();

        (before, process_tiles(&workspace, cx).len())
    }

    fn process_height_after_exit(code: u32, cx: &mut TestAppContext) -> (f32, f32) {
        let (workspace, cx, _client) = real_task_workspace(&fake_daemon(), cx);
        change_terminal(&workspace, &process_info(30, Some("tests"), "zsh"), cx);
        let tile = workspace
            .read_with(cx, |workspace, _| {
                workspace.live_terminals.keys().next().copied()
            })
            .expect("a process tile");

        workspace.update_in(cx, |workspace, window, cx| {
            let exited = TerminalEvent::Exited(ExitStatus::Code(code));
            workspace.on_terminal_event(tile, &exited, window, cx);
        });

        workspace.read_with(cx, |workspace, _| {
            let layout = crate::views::compute_layout(workspace);
            let height = layout
                .rect(swarm_layout::TileId(tile.0))
                .map(|rect| rect.size.height);

            (
                height.expect("the process tile is placed"),
                crate::views::layout_style(workspace).process_tile_height,
            )
        })
    }

    #[gpui::test]
    fn an_agent_process_that_fails_gets_a_grown_tile(cx: &mut TestAppContext) {
        let (height, small) = process_height_after_exit(1, cx);

        assert!(height > small + 1., "{height} is not above {small}");
    }

    #[gpui::test]
    fn an_agent_process_that_exits_well_keeps_its_small_tile(cx: &mut TestAppContext) {
        let (height, small) = process_height_after_exit(0, cx);

        assert!((height - small).abs() < 0.01, "{height} is not {small}");
    }

    #[gpui::test]
    fn an_agent_process_that_exits_well_closes_after_the_delay(cx: &mut TestAppContext) {
        assert_eq!(process_tile_after_exit(0, cx), (1, 0));
    }

    #[gpui::test]
    fn an_agent_process_that_fails_stays_until_closed(cx: &mut TestAppContext) {
        assert_eq!(process_tile_after_exit(1, cx), (1, 1));
    }

    #[gpui::test]
    fn stopping_an_agent_process_kills_it_and_it_does_not_come_back(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = real_task_workspace(&fake_daemon(), cx);
        let process = process_info(30, Some("dev server"), "zsh");
        change_terminal(&workspace, &process, cx);
        let tile = workspace
            .read_with(cx, |workspace, _| {
                workspace.active_task().processes.first().copied()
            })
            .expect("a process tile");
        let input = workspace.read_with(cx, |workspace, _| {
            workspace
                .live_terminals
                .get(&tile)
                .map(|live| live.input.clone())
        });

        workspace.update(cx, |workspace, cx| workspace.stop_process(tile, cx));
        change_terminal(&workspace, &process, cx);

        assert!(input.is_some());
        assert!(process_tiles(&workspace, cx).is_empty());
    }

    #[gpui::test]
    fn a_task_an_agent_made_gets_a_tab_without_taking_focus(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = real_task_workspace(&fake_daemon(), cx);
        let made = TaskInfo {
            created_by: Some(swarm_protocol::AgentId(100)),
            ..task_info(8, 5, "signup")
        };

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.task_changed(&made, Some(window), cx);
        });
        let renamed = TaskInfo {
            name: "sign-up".into(),
            briefed: true,
            ..made
        };
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.task_changed(&renamed, Some(window), cx);
        });

        let (names, active) = workspace.read_with(cx, |workspace, _| {
            let names: Vec<String> = workspace
                .data
                .tasks_of(workspace.active_project)
                .map(|task| task.name.to_string())
                .collect();

            (names, workspace.active_task().name.to_string())
        });
        assert_eq!(names, ["login", "sign-up"]);
        assert_eq!(active, "login");
    }

    #[gpui::test]
    fn a_message_is_kept_under_its_task(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = real_task_workspace(&fake_daemon(), cx);
        let message = MessageInfo {
            id: swarm_protocol::MessageId(1),
            project: DaemonProjectId(5),
            task: Some(DaemonTaskId(7)),
            from: None,
            to: swarm_protocol::AgentId(1),
            kind: swarm_protocol::MessageKind::Brief,
            text: "go".into(),
            sent_at: 0,
            delivered_at: None,
        };

        workspace.update(cx, |workspace, cx| {
            workspace.message_event(MessageEvent::Sent(message), cx);
        });

        let kept = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .messages_of(DaemonProjectId(5), Some(DaemonTaskId(7)))
                .count()
        });
        assert_eq!(kept, 1);
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the fake daemon answers on its own thread"
    )]
    pub(crate) fn wait_a_little() {
        std::thread::sleep(Duration::from_millis(20));
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the fake daemon answers on its own thread"
    )]
    fn answer_late() {
        std::thread::sleep(Duration::from_millis(500));
    }

    pub(crate) fn wait_until(
        cx: &mut VisualTestContext,
        done: impl Fn(&mut VisualTestContext) -> bool,
    ) -> bool {
        for _ in 0..100 {
            cx.executor().allow_parking();
            cx.run_until_parked();

            if done(cx) {
                return true;
            }

            wait_a_little();
        }

        false
    }

    fn history_message(id: u64, delivered_at: Option<u64>) -> MessageInfo {
        MessageInfo {
            id: swarm_protocol::MessageId(id),
            project: DaemonProjectId(5),
            task: Some(DaemonTaskId(7)),
            from: None,
            to: swarm_protocol::AgentId(1),
            kind: swarm_protocol::MessageKind::Task,
            text: "go".into(),
            sent_at: id,
            delivered_at,
        }
    }

    #[gpui::test]
    fn the_message_history_is_loaded_once_the_daemon_is_used(cx: &mut TestAppContext) {
        let socket = fake_daemon_replying(|request| match request {
            Request::ListMessages { call, task, .. } if task.is_some() => Some(Event::Messages {
                call,
                messages: vec![history_message(1, Some(5)), history_message(2, None)],
            }),
            _ => None,
        });
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);

        let loaded = wait_until(cx, |cx| {
            workspace.read_with(cx, |workspace, _| {
                workspace
                    .data
                    .messages_of(DaemonProjectId(5), Some(DaemonTaskId(7)))
                    .count()
                    == 2
            })
        });

        assert!(loaded);
    }

    #[gpui::test]
    fn a_delivered_event_clears_a_message_in_flight(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = real_task_workspace(&fake_daemon(), cx);
        let message = history_message(1, None);

        workspace.update(cx, |workspace, cx| {
            workspace.message_event(MessageEvent::Sent(message), cx);
            workspace.message_event(
                MessageEvent::Delivered {
                    ids: vec![swarm_protocol::MessageId(1)],
                    delivered_at: 9,
                },
                cx,
            );
        });

        let delivered = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .messages_of(DaemonProjectId(5), Some(DaemonTaskId(7)))
                .map(|message| message.delivered_at)
                .collect::<Vec<_>>()
        });
        assert_eq!(delivered, [Some(9)]);
    }

    #[gpui::test]
    fn changes_are_polled_only_while_the_swarm_view_shows(cx: &mut TestAppContext) {
        let (asked, requests) = flume::unbounded();
        let socket = fake_daemon_replying(move |request| match request {
            Request::AgentChanges { call, agents } => {
                asked.send(agents.clone()).ok();

                Some(Event::AgentChanges {
                    call,
                    changes: agents
                        .into_iter()
                        .map(|agent| AgentChange {
                            agent,
                            stat: DiffStat {
                                files: 2,
                                additions: 5,
                                deletions: 1,
                            },
                            files: Vec::new(),
                        })
                        .collect(),
                })
            }
            _ => None,
        });
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);
        cx.update(|window, _| window.activate_window());
        change_agent(
            &workspace,
            &agent_info(1, 7, DaemonAgentStatus::Working),
            cx,
        );

        redraw(cx);
        for _ in 0..5 {
            wait_a_little();
            redraw(cx);
        }
        let before = requests.try_iter().count();

        workspace.update(cx, |workspace, cx| {
            workspace.set_view(View::Swarm, cx);
        });
        redraw(cx);

        let polled = wait_until(cx, |cx| {
            workspace.read_with(cx, |workspace, _| !workspace.data.agent_changes.is_empty())
        });

        assert_eq!(before, 0);
        assert!(polled);
        assert_eq!(
            requests.try_iter().next(),
            Some(vec![swarm_protocol::AgentId(1)])
        );
    }

    #[gpui::test]
    fn a_delivered_event_before_its_message_is_not_lost(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = real_task_workspace(&fake_daemon(), cx);

        workspace.update(cx, |workspace, cx| {
            workspace.message_event(
                MessageEvent::Delivered {
                    ids: vec![swarm_protocol::MessageId(1)],
                    delivered_at: 9,
                },
                cx,
            );
            workspace.message_event(MessageEvent::Sent(history_message(1, None)), cx);
        });

        let delivered = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .messages_of(DaemonProjectId(5), Some(DaemonTaskId(7)))
                .map(|message| message.delivered_at)
                .collect::<Vec<_>>()
        });
        assert_eq!(delivered, [Some(9)]);
    }

    fn polling(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> (bool, bool) {
        workspace.read_with(cx, |workspace, _| {
            (
                workspace.changes_poll.is_some(),
                workspace.swarm_clock.is_some(),
            )
        })
    }

    #[gpui::test]
    fn polling_stops_after_leaving_the_swarm_view_or_when_the_window_is_inactive(
        cx: &mut TestAppContext,
    ) {
        let socket = fake_daemon_replying(|_| None);
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);
        workspace.update(cx, |workspace, _| workspace.fixed_now = None);
        change_agent(
            &workspace,
            &agent_info(1, 7, DaemonAgentStatus::Working),
            cx,
        );
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        assert_eq!(polling(&workspace, cx), (false, false));

        workspace.update(cx, |workspace, cx| workspace.set_view(View::Swarm, cx));
        redraw(cx);
        assert_eq!(polling(&workspace, cx), (true, true));

        cx.deactivate_window();
        cx.run_until_parked();
        assert_eq!(polling(&workspace, cx), (false, false));

        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        assert_eq!(polling(&workspace, cx), (true, true));

        workspace.update(cx, |workspace, cx| workspace.set_view(View::Terminals, cx));
        redraw(cx);
        assert_eq!(polling(&workspace, cx), (false, false));
    }

    fn rate_limited_claude() -> Usage {
        let mut usage = claude_usage(42);
        usage.claude.problem =
            Some(swarm_protocol::UsageProblem::RateLimited { retry_at: u64::MAX });

        usage
    }

    #[gpui::test]
    fn the_usage_card_ticks_only_while_it_shows_a_time_in_an_active_window(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.fixed_now = None;
            workspace.usage_changed(rate_limited_claude(), cx);
        });
        let renders = Rc::new(std::cell::Cell::new(0));
        workspace.update(cx, |_, cx| {
            let renders = Rc::clone(&renders);
            cx.observe(&cx.entity(), move |_, _, _| renders.set(renders.get() + 1))
                .detach();
        });
        let clock = |cx: &VisualTestContext| {
            workspace.read_with(cx, |workspace, _| workspace.usage_clock.is_some())
        };

        redraw(cx);
        assert!(!clock(cx), "an inactive window does not tick");

        cx.update(|window, _| window.activate_window());
        redraw(cx);
        assert!(clock(cx), "an active window with a rate limit ticks");

        cx.executor().advance_clock(USAGE_TICK / 2);
        cx.run_until_parked();
        let early = renders.get();
        cx.executor().advance_clock(USAGE_TICK / 2);
        cx.run_until_parked();
        assert_eq!((early, renders.get()), (0, 1), "one render per tick");

        cx.deactivate_window();
        cx.run_until_parked();
        assert!(!clock(cx), "the clock stops with the window");

        cx.update(|window, _| window.activate_window());
        workspace.update(cx, |workspace, cx| {
            workspace.usage_changed(claude_usage(42), cx);
        });
        redraw(cx);
        assert!(!clock(cx), "numbers without a time need no clock");

        let before = renders.get();
        cx.executor().advance_clock(USAGE_TICK * 4);
        cx.run_until_parked();
        assert_eq!(renders.get(), before);
    }

    #[gpui::test]
    fn the_poll_restarts_when_the_open_task_changes(cx: &mut TestAppContext) {
        let socket = fake_daemon_replying(|_| None);
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);
        cx.update(|window, _| window.activate_window());
        workspace.update(cx, |workspace, cx| workspace.set_view(View::Swarm, cx));
        redraw(cx);

        let polled = workspace.read_with(cx, |workspace, _| workspace.polled_task);
        workspace.update(cx, |workspace, cx| {
            workspace.polled_task = Some(TaskId(u32::MAX - 1));
            cx.notify();
        });
        redraw(cx);

        let after = workspace.read_with(cx, |workspace, _| workspace.polled_task);
        assert!(polled.is_some());
        assert_eq!(after, polled);
    }

    #[gpui::test]
    fn an_agent_missing_from_the_changes_loses_its_stats(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = real_task_workspace(&fake_daemon(), cx);
        change_agent(
            &workspace,
            &agent_info(1, 7, DaemonAgentStatus::Working),
            cx,
        );
        let ask = [swarm_protocol::AgentId(1)];
        let stat = DiffStat {
            files: 1,
            additions: 2,
            deletions: 3,
        };

        workspace.update(cx, |workspace, cx| {
            workspace.apply_changes(
                &ask,
                &[AgentChange {
                    agent: ask[0],
                    stat,
                    files: Vec::new(),
                }],
                cx,
            );
        });
        let known = workspace.read_with(cx, |workspace, _| workspace.data.agent_changes.len());
        workspace.update(cx, |workspace, cx| workspace.apply_changes(&ask, &[], cx));
        let left = workspace.read_with(cx, |workspace, _| workspace.data.agent_changes.len());

        assert_eq!((known, left), (1, 0));
    }

    #[gpui::test]
    fn applied_changes_keep_the_files_next_to_the_stat(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = real_task_workspace(&fake_daemon(), cx);
        change_agent(
            &workspace,
            &agent_info(1, 7, DaemonAgentStatus::Working),
            cx,
        );
        let ask = [swarm_protocol::AgentId(1)];
        let change = AgentChange {
            agent: ask[0],
            stat: DiffStat {
                files: 1,
                additions: 2,
                deletions: 3,
            },
            files: vec![swarm_protocol::FileStat {
                path: "src/lib.rs".into(),
                additions: 2,
                deletions: 3,
            }],
        };

        workspace.update(cx, |workspace, cx| {
            workspace.apply_changes(&ask, std::slice::from_ref(&change), cx);
        });

        let kept = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .agent_changes
                .values()
                .next()
                .map(|changes| (changes.stat, changes.files.to_vec()))
        });
        assert_eq!(kept, Some((change.stat, change.files)));
    }

    type StartRequests = flume::Receiver<(DaemonProjectId, Option<swarm_protocol::AgentKind>)>;

    /// A project without tasks whose daemon records every Start orchestrator request.
    fn start_workspace(
        tasks: Vec<TaskInfo>,
        cx: &mut TestAppContext,
    ) -> (Entity<Workspace>, &mut VisualTestContext, StartRequests) {
        let (sent, requests) = flume::unbounded();
        let socket = fake_daemon_replying(move |request| {
            if let Request::StartOrchestrator { project, kind, .. } = request {
                sent.send((project, kind)).ok();
            }

            None
        });
        let (workspace, cx) = setup(cx);
        cx.executor().allow_parking();
        let client = Client::connect(&socket).expect("connect to the fake daemon");
        use_projects(&workspace, &[project_info(5, "demo", tasks)], cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &[], window, cx);
        });
        cx.executor().allow_parking();
        cx.run_until_parked();

        (workspace, cx, requests)
    }

    pub(crate) fn redraw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.executor().allow_parking();
        cx.run_until_parked();
    }

    #[gpui::test]
    fn a_start_tab_asks_for_its_orchestrator_once_and_lets_the_daemon_pick_the_kind(
        cx: &mut TestAppContext,
    ) {
        let (sent, requests) = flume::unbounded();
        let socket = fake_daemon_replying(move |request| {
            if let Request::StartOrchestrator { project, kind, .. } = request {
                sent.send((project, kind)).ok();
            }

            None
        });
        let (workspace, cx) = setup(cx);
        cx.executor().allow_parking();
        let client = Client::connect(&socket).expect("connect to the fake daemon");
        use_projects(&workspace, &[project_info(5, "demo", Vec::new())], cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &[], window, cx);
        });

        cx.executor().allow_parking();
        cx.run_until_parked();
        redraw(cx);
        redraw(cx);

        let sent: Vec<_> = requests.try_iter().collect();
        assert_eq!(sent, [(DaemonProjectId(5), None)]);
    }

    #[gpui::test]
    fn the_started_orchestrator_is_the_only_tile_of_the_start_tab(cx: &mut TestAppContext) {
        let (workspace, cx, _requests) = start_workspace(Vec::new(), cx);

        let (is_start, titles, label, runs) = workspace.read_with(cx, |workspace, _| {
            let task = workspace.active_task();

            (
                task.is_start,
                crate::views::tile_specs(workspace).len(),
                workspace.data.tab_label(task).to_string(),
                workspace.data.start_runs(workspace.active_project),
            )
        });
        assert!(is_start);
        assert_eq!(titles, 1);
        assert_eq!(label, "Start");
        assert!(runs);
        assert_eq!(tile_titles(&workspace, cx), ["start"]);
    }

    #[gpui::test]
    fn plan_with_orchestrator_starts_one_and_then_only_focuses_it(cx: &mut TestAppContext) {
        let (workspace, cx, requests) = start_workspace(vec![task_info(7, 5, "login")], cx);
        let before = requests.try_iter().count();

        workspace.update(cx, Workspace::open_start_task);
        redraw(cx);
        let first = requests.try_iter().count();
        workspace.update(cx, |workspace, cx| workspace.select_task(TaskId(0), cx));
        workspace.update(cx, Workspace::open_start_task);
        redraw(cx);

        let (second, starts, label) = (
            requests.try_iter().count(),
            workspace.read_with(cx, |workspace, _| {
                workspace
                    .data
                    .tasks
                    .iter()
                    .filter(|task| task.is_start)
                    .count()
            }),
            workspace.read_with(cx, |workspace, _| {
                workspace
                    .data
                    .tab_label(workspace.active_task())
                    .to_string()
            }),
        );
        assert_eq!((before, first, second, starts), (0, 1, 0, 1));
        assert_eq!(label, "Plan");
    }

    fn running_start(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
        change_agent(workspace, &start_agent(100, 5, DaemonAgentStatus::Idle), cx);
    }

    fn stop_start(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
        let mut ended = start_agent(100, 5, DaemonAgentStatus::Done);
        ended.lifecycle = AgentLifecycle::Resumable;
        change_agent(workspace, &ended, cx);
    }

    fn made_by_start(id: u64, name: &str) -> TaskInfo {
        TaskInfo {
            created_by: Some(swarm_protocol::AgentId(100)),
            ..task_info(id, 5, name)
        }
    }

    fn active_name(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> String {
        workspace.read_with(cx, |workspace, _| workspace.active_task().name.to_string())
    }

    #[gpui::test]
    fn the_start_tab_closes_and_focus_goes_to_the_lowest_task_it_made(cx: &mut TestAppContext) {
        let tasks = vec![
            task_info(7, 5, "manual"),
            made_by_start(9, "second"),
            made_by_start(8, "first"),
        ];
        let (workspace, cx, _requests) = start_workspace(tasks, cx);
        running_start(&workspace, cx);
        workspace.update(cx, Workspace::open_start_task);
        let on_start = workspace.read_with(cx, |workspace, _| workspace.active_task().is_start);

        stop_start(&workspace, cx);

        let starts = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .tasks
                .iter()
                .filter(|task| task.is_start)
                .count()
        });
        assert!(on_start);
        assert_eq!(starts, 0);
        assert_eq!(active_name(&workspace, cx), "first");
    }

    #[gpui::test]
    fn without_a_task_it_made_focus_goes_to_the_tab_used_before(cx: &mut TestAppContext) {
        let tasks = vec![task_info(7, 5, "one"), task_info(8, 5, "two")];
        let (workspace, cx, _requests) = start_workspace(tasks, cx);
        running_start(&workspace, cx);
        workspace.update(cx, |workspace, cx| workspace.select_task(TaskId(1), cx));
        workspace.update(cx, Workspace::open_start_task);

        stop_start(&workspace, cx);

        assert_eq!(active_name(&workspace, cx), "two");
    }

    #[gpui::test]
    fn an_ended_start_orchestrator_does_not_open_a_tab(cx: &mut TestAppContext) {
        let (workspace, cx, _requests) = start_workspace(vec![task_info(7, 5, "one")], cx);

        stop_start(&workspace, cx);

        let (starts, pending) = workspace.read_with(cx, |workspace, _| {
            (
                workspace
                    .data
                    .tasks
                    .iter()
                    .filter(|task| task.is_start)
                    .count(),
                workspace.pending_agents.len(),
            )
        });
        assert_eq!((starts, pending), (0, 0));
    }

    #[gpui::test]
    fn a_start_tab_is_a_plan_tab_once_a_user_task_exists(cx: &mut TestAppContext) {
        let tasks = vec![made_by_start(8, "first")];
        let (workspace, cx, _requests) = start_workspace(tasks, cx);
        running_start(&workspace, cx);
        workspace.update(cx, Workspace::open_start_task);
        let label = |workspace: &Entity<Workspace>, cx: &VisualTestContext| {
            workspace.read_with(cx, |workspace, _| {
                workspace
                    .data
                    .tab_label(workspace.active_task())
                    .to_string()
            })
        };
        let made_by_it = label(&workspace, cx);

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.task_changed(&task_info(9, 5, "by hand"), Some(window), cx);
        });

        assert_eq!(made_by_it, "Start");
        assert_eq!(label(&workspace, cx), "Plan");
    }

    #[gpui::test]
    fn a_listing_without_the_start_orchestrator_closes_its_tab(cx: &mut TestAppContext) {
        let (workspace, cx, _requests) = start_workspace(vec![task_info(7, 5, "one")], cx);
        running_start(&workspace, cx);
        let shown = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .tasks
                .iter()
                .filter(|task| task.is_start)
                .count()
        });

        use_projects(
            &workspace,
            &[project_info(5, "demo", vec![task_info(7, 5, "one")])],
            cx,
        );

        let gone = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .tasks
                .iter()
                .filter(|task| task.is_start)
                .count()
        });
        assert_eq!((shown, gone), (1, 0));
    }

    #[gpui::test]
    fn a_listing_with_a_running_start_orchestrator_shows_its_tab(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let mut listed = project_info(5, "demo", Vec::new());
        listed.start = Some(start_agent(100, 5, DaemonAgentStatus::Working));

        use_projects(&workspace, &[listed], cx);

        let (is_start, runs) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.active_task().is_start,
                workspace.data.start_runs(workspace.active_project),
            )
        });
        assert!(is_start);
        assert!(runs);
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

    pub(crate) fn diff_of(task: u64, source: ReviewSource, fingerprint: u64) -> TaskDiff {
        TaskDiff {
            task: DaemonTaskId(task),
            source,
            fingerprint,
            head: "abc".into(),
            stat: DiffStat {
                files: 0,
                additions: 3,
                deletions: 1,
            },
            files: Vec::new(),
        }
    }

    type DiffRequests = flume::Receiver<(ReviewSource, Option<u64>)>;

    /// A daemon that answers a diff once and then says nothing changed.
    fn diff_daemon() -> (PathBuf, DiffRequests) {
        let (asked, requests) = flume::unbounded();
        let socket = fake_daemon_replying(move |request| match request {
            Request::TaskDiff {
                call,
                task,
                source,
                known,
            } => {
                asked.send((source, known)).ok();
                let diff = known.is_none().then(|| diff_of(task.0, source, 9));

                Some(Event::TaskDiff { call, diff })
            }
            _ => None,
        });

        (socket, requests)
    }

    fn fast_polls(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
        workspace.update(cx, |workspace, _| {
            workspace.diff_intervals = (Duration::from_millis(20), Duration::from_millis(20));
        });
    }

    #[gpui::test]
    fn the_live_diff_is_polled_with_the_known_fingerprint(cx: &mut TestAppContext) {
        let (socket, requests) = diff_daemon();
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);
        fast_polls(&workspace, cx);
        cx.update(|window, _| window.activate_window());

        redraw(cx);
        let stored = wait_until(cx, |cx| {
            redraw(cx);
            workspace.read_with(cx, |workspace, _| {
                workspace.data.live_stat(TaskId(0)).additions == 3
                    && workspace
                        .data
                        .diffs
                        .contains_key(&(DaemonTaskId(7), ReviewSource::Live))
            })
        });
        let reused = wait_until(cx, |cx| {
            cx.executor().advance_clock(Duration::from_millis(30));
            redraw(cx);
            requests.len() >= 3
        });

        let asked: Vec<_> = requests.try_iter().collect();
        assert!(stored, "stored");
        assert!(reused, "reused {asked:?}");
        assert_eq!(asked.first(), Some(&(ReviewSource::Live, None)));
        assert!(
            asked
                .iter()
                .skip(1)
                .all(|request| *request == (ReviewSource::Live, Some(9)))
        );
    }

    #[gpui::test]
    fn the_review_source_is_polled_only_while_review_shows_and_the_window_is_active(
        cx: &mut TestAppContext,
    ) {
        let (socket, requests) = diff_daemon();
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);
        fast_polls(&workspace, cx);
        cx.update(|window, _| window.activate_window());
        redraw(cx);

        let polls = |cx: &VisualTestContext| {
            workspace.read_with(cx, |workspace, _| {
                (
                    workspace.review_poll.is_some(),
                    workspace.live_poll.is_some(),
                )
            })
        };
        let terminals = polls(cx);

        workspace.update(cx, |workspace, cx| workspace.set_view(View::Review, cx));
        redraw(cx);
        let review = polls(cx);
        let sources = wait_until(cx, |cx| {
            redraw(cx);
            requests
                .try_iter()
                .any(|(source, _)| source == ReviewSource::Merged)
        });

        cx.deactivate_window();
        redraw(cx);
        let inactive = polls(cx);

        workspace.update(cx, |workspace, cx| workspace.set_view(View::Terminals, cx));
        cx.update(|window, _| window.activate_window());
        redraw(cx);
        let back = polls(cx);

        assert_eq!(terminals, (false, true));
        assert_eq!(review, (true, true));
        assert!(sources);
        assert_eq!(inactive, (false, false));
        assert_eq!(back, (false, true));
    }

    type CommentRequests = flume::Receiver<Request>;

    /// A daemon that lists no comments and accepts adds, and reports what it was asked.
    fn comment_daemon() -> (PathBuf, CommentRequests) {
        let (asked, requests) = flume::unbounded();
        let socket = fake_daemon_replying(move |request| match request {
            Request::TaskDiff {
                call,
                task,
                source,
                known,
            } => {
                let diff = known.is_none().then(|| diff_of(task.0, source, 9));

                Some(Event::TaskDiff { call, diff })
            }
            Request::ListComments { call, .. } => {
                asked.send(request).ok();

                Some(Event::Comments {
                    call,
                    comments: Vec::new(),
                })
            }
            Request::AddComment { call, .. } | Request::SendComments { call, .. } => {
                asked.send(request).ok();

                Some(Event::Done { call })
            }
            _ => None,
        });

        (socket, requests)
    }

    #[gpui::test]
    fn comments_are_listed_when_review_opens_and_again_for_a_new_diff(cx: &mut TestAppContext) {
        let (socket, requests) = comment_daemon();
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);
        fast_polls(&workspace, cx);
        cx.update(|window, _| window.activate_window());
        redraw(cx);

        let before_review = requests.try_iter().count();
        workspace.update(cx, |workspace, cx| workspace.set_view(View::Review, cx));
        redraw(cx);
        let listed = wait_until(cx, |cx| {
            redraw(cx);
            requests
                .try_iter()
                .any(|request| matches!(request, Request::ListComments { .. }))
        });
        let settled = wait_until(cx, |cx| {
            redraw(cx);
            workspace.read_with(cx, |workspace, _| {
                workspace
                    .comments_listed
                    .is_some_and(|(_, _, fingerprint)| fingerprint == Some(9))
            })
        });
        // Drain whatever the diff arriving already asked.
        requests.try_iter().for_each(drop);

        workspace.update(cx, |workspace, cx| {
            let source = workspace.review_source(workspace.active_task);
            workspace.apply_diff(diff_of(7, source, 10), cx);
        });
        redraw(cx);
        let again = wait_until(cx, |cx| {
            redraw(cx);
            requests
                .try_iter()
                .any(|request| matches!(request, Request::ListComments { .. }))
        });

        assert_eq!(before_review, 0, "nothing is listed before Review opens");
        assert!(listed && settled && again);
    }

    #[gpui::test]
    fn a_comment_goes_to_the_daemon_and_the_box_closes_when_it_answers(cx: &mut TestAppContext) {
        let (socket, requests) = comment_daemon();
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);
        workspace.update(cx, |workspace, cx| workspace.set_view(View::Review, cx));
        let mut diff = diff_of(7, ReviewSource::Live, 9);
        diff.files = vec![swarm_protocol::FileDiff {
            worktree: PathBuf::from("/wt"),
            path: PathBuf::from("a.rs"),
            agent: None,
            change: swarm_protocol::FileChange::Modified,
            mode_change: None,
            additions: 1,
            deletions: 0,
            binary: false,
            hunks: vec![swarm_protocol::DiffHunk {
                old_start: 1,
                old_lines: 0,
                new_start: 1,
                new_lines: 1,
                section: String::new(),
                lines: vec![swarm_protocol::DiffLine {
                    kind: swarm_protocol::DiffLineKind::Added,
                    text: "let x = 1;".into(),
                }],
            }],
            truncated: false,
            digest: 0,
            fold: None,
        }];
        workspace.update(cx, |workspace, cx| {
            workspace.data.store_diff(diff);
            workspace.set_review_source(ReviewSource::Live, cx);
        });
        redraw(cx);
        let review = workspace.read_with(cx, |workspace, _| workspace.review.clone());

        cx.simulate_keystrokes("c");
        redraw(cx);
        cx.simulate_input("Rename x");
        cx.simulate_keystrokes("cmd-enter");
        let sent = wait_until(cx, |cx| {
            redraw(cx);
            requests.try_iter().any(|request| match request {
                Request::AddComment { comment, .. } => {
                    comment.body == "Rename x"
                        && comment.anchor.line == 1
                        && comment.anchor.text == "let x = 1;"
                        && comment.source == ReviewSource::Live
                }
                _ => false,
            })
        });
        let closed = wait_until(cx, |cx| {
            redraw(cx);
            !review.read_with(cx, |review, _| review.composer_is_open())
        });

        assert!(sent, "the daemon got the comment with its anchor");
        assert!(closed, "the box closes when the daemon says done");
    }

    #[gpui::test]
    fn comment_events_reach_the_comments_of_the_task(cx: &mut TestAppContext) {
        let (socket, _requests) = comment_daemon();
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);
        let comment = swarm_protocol::ReviewComment {
            id: swarm_protocol::CommentId(4),
            task: DaemonTaskId(7),
            parent: None,
            source: ReviewSource::Merged,
            anchor: swarm_protocol::CommentAnchor {
                worktree: PathBuf::from("/wt"),
                path: PathBuf::from("a.rs"),
                side: swarm_protocol::DiffSide::New,
                line: 1,
                text: String::new(),
                before: None,
                after: None,
            },
            body: "x".into(),
            state: swarm_protocol::CommentState::Pending,
            to: None,
            outdated: false,
            created_at: 0,
            sent_at: None,
            delivered_at: None,
        };

        workspace.update(cx, |workspace, cx| {
            workspace.comment_event(
                &swarm_protocol::CommentEvent::Changed(Box::new(comment)),
                cx,
            );
        });

        let kept = workspace.read_with(cx, |workspace, _| {
            workspace.data.task_list(DaemonTaskId(7)).len()
        });
        assert_eq!(kept, 1);
    }

    fn claude_usage(percent: u8) -> Usage {
        Usage {
            claude: swarm_protocol::SubscriptionUsage {
                windows: vec![swarm_protocol::UsageWindow {
                    label: "5h".into(),
                    used_percent: percent,
                    resets_at: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// The used share of Claude's shortest window, and whether the card has its row.
    fn claude_card(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> (Option<u8>, bool) {
        workspace.update(cx, |_, cx| cx.notify());
        redraw(cx);

        let percent = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .usage
                .claude
                .windows
                .first()
                .map(|window| window.used_percent)
        });

        (percent, cx.debug_bounds("usage-claude").is_some())
    }

    #[gpui::test]
    fn the_app_asks_for_usage_once_after_connecting_and_follows_its_changes(
        cx: &mut TestAppContext,
    ) {
        let (asked, asks) = flume::unbounded();
        let socket = fake_daemon_replying(move |request| match request {
            Request::Usage { call } => {
                asked.send(()).ok();

                Some(Event::Usage {
                    call,
                    usage: claude_usage(42),
                })
            }
            Request::Attach { .. } => Some(Event::UsageChanged {
                usage: claude_usage(77),
            }),
            _ => None,
        });
        let (workspace, cx, client) = real_task_workspace(&socket, cx);

        let answered = wait_until(cx, |cx| claude_card(&workspace, cx) == (Some(42), true));
        assert!(answered, "the card shows the answer");

        client
            .attach(swarm_protocol::TerminalId(99), 0)
            .expect("send the attach");
        let changed = wait_until(cx, |cx| claude_card(&workspace, cx) == (Some(77), true));
        assert!(changed, "the card follows the event");
        assert_eq!(asks.try_iter().count(), 1, "the app asks once");
    }

    #[gpui::test]
    fn a_reconnect_asks_for_usage_again(cx: &mut TestAppContext) {
        let first = fake_daemon_replying(|request| match request {
            Request::Usage { call } => Some(Event::Usage {
                call,
                usage: claude_usage(42),
            }),
            _ => None,
        });
        let second = fake_daemon_replying(|request| match request {
            Request::Usage { call } => Some(Event::Usage {
                call,
                usage: claude_usage(55),
            }),
            _ => None,
        });
        let (workspace, cx, _client) = real_task_workspace(&first, cx);
        let before = wait_until(cx, |cx| claude_card(&workspace, cx).0 == Some(42));

        let client = Client::connect(&second).expect("connect to the second fake daemon");
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &[], window, cx);
        });
        let after = wait_until(cx, |cx| claude_card(&workspace, cx).0 == Some(55));

        assert!(before, "the first connection is asked");
        assert!(after, "the second connection is asked again");
    }

    #[gpui::test]
    fn an_event_wins_over_an_answer_that_was_asked_before_it(cx: &mut TestAppContext) {
        let (release, gate) = flume::unbounded::<()>();
        let socket = fake_daemon_replying(move |request| match request {
            Request::Usage { call } => {
                gate.recv().ok();

                Some(Event::Usage {
                    call,
                    usage: claude_usage(42),
                })
            }
            _ => None,
        });
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);

        workspace.update(cx, |workspace, cx| {
            workspace.usage_changed(claude_usage(77), cx);
        });
        release.send(()).expect("the fake daemon is waiting");
        cx.executor().allow_parking();
        answer_late();
        cx.run_until_parked();

        let (percent, _) = claude_card(&workspace, cx);
        assert_eq!(percent, Some(77));
    }

    pub(crate) fn pending_comment(id: u64, body: &str) -> swarm_protocol::ReviewComment {
        swarm_protocol::ReviewComment {
            id: swarm_protocol::CommentId(id),
            task: DaemonTaskId(7),
            parent: None,
            source: ReviewSource::Merged,
            anchor: swarm_protocol::CommentAnchor {
                worktree: PathBuf::from("/wt"),
                path: PathBuf::from("a.rs"),
                side: swarm_protocol::DiffSide::New,
                line: 1,
                text: String::new(),
                before: None,
                after: None,
            },
            body: body.into(),
            state: swarm_protocol::CommentState::Pending,
            to: None,
            outdated: false,
            created_at: 0,
            sent_at: None,
            delivered_at: None,
        }
    }

    fn comment_ids(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> Vec<(u64, String)> {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .task_list(DaemonTaskId(7))
                .iter()
                .map(|comment| (comment.id.0, comment.body.clone()))
                .collect()
        })
    }

    #[gpui::test]
    fn a_list_answer_started_before_an_event_keeps_what_the_event_changed(cx: &mut TestAppContext) {
        let (release, gate) = flume::unbounded::<()>();
        let socket = fake_daemon_replying(move |request| match request {
            Request::ListComments { call, .. } => {
                gate.recv().ok();

                Some(Event::Comments {
                    call,
                    comments: vec![pending_comment(1, "older"), pending_comment(3, "deleted")],
                })
            }
            _ => None,
        });
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);

        // The list is asked but the daemon has not answered: nothing ran yet.
        workspace.update(cx, |workspace, cx| workspace.set_view(View::Review, cx));
        workspace.update(cx, Workspace::sync_review);
        let started = workspace.read_with(cx, |workspace, _| workspace.comment_sync.is_asking());
        workspace.update(cx, |workspace, cx| {
            let event = swarm_protocol::CommentEvent::Changed;

            workspace.comment_event(&event(Box::new(pending_comment(1, "newer"))), cx);
            workspace.comment_event(&event(Box::new(pending_comment(2, "added"))), cx);
            workspace.comment_event(
                &swarm_protocol::CommentEvent::Deleted {
                    task: DaemonTaskId(7),
                    id: swarm_protocol::CommentId(3),
                },
                cx,
            );
        });
        release.send(()).ok();
        drop(release);
        let listed = wait_until(cx, |cx| {
            redraw(cx);
            workspace.read_with(cx, |workspace, _| workspace.comments_listed.is_some())
        });

        assert!(started && listed);
        assert_eq!(
            comment_ids(&workspace, cx),
            [(1, "newer".to_owned()), (2, "added".to_owned())]
        );
    }

    #[gpui::test]
    fn a_failed_list_is_asked_again_for_a_new_diff_but_not_on_every_frame(cx: &mut TestAppContext) {
        let (asked, requests) = flume::unbounded();
        let socket = fake_daemon_replying(move |request| match request {
            Request::ListComments { call, .. } => {
                asked.send(()).ok();

                Some(Event::Error {
                    call: Some(call),
                    id: None,
                    message: "busy".into(),
                })
            }
            _ => None,
        });
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);

        workspace.update(cx, |workspace, cx| workspace.set_view(View::Review, cx));
        redraw(cx);
        let first = wait_until(cx, |cx| {
            redraw(cx);
            workspace.read_with(cx, |workspace, _| workspace.comment_sync.has_failed())
        });
        for _ in 0..5 {
            workspace.update(cx, |_, cx| cx.notify());
            redraw(cx);
        }
        let after_frames = requests.try_iter().count();
        let listed = workspace.read_with(cx, |workspace, _| workspace.comments_listed);

        workspace.update(cx, |workspace, cx| {
            let source = workspace.review_source(workspace.active_task);

            workspace.apply_diff(diff_of(7, source, 10), cx);
        });
        redraw(cx);
        let again = wait_until(cx, |cx| {
            redraw(cx);
            !requests.is_empty()
        });

        assert!(first && again);
        assert_eq!(after_frames, 1, "a failed list was asked again at once");
        assert_eq!(listed, None, "a failed list counted as listed");
    }

    #[gpui::test]
    fn the_refusal_of_a_stopped_orchestrator_shows_in_the_daemons_words(cx: &mut TestAppContext) {
        let socket = fake_daemon_replying(|request| match request {
            Request::SendComments { call, .. } => Some(Event::Error {
                call: Some(call),
                id: None,
                message: "the orchestrator is stopped".into(),
            }),
            _ => None,
        });
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);

        workspace.update(cx, |workspace, cx| {
            workspace
                .data
                .set_comments(DaemonTaskId(7), vec![pending_comment(1, "Rename it")]);
            workspace.request_review_changes(cx);
        });
        let shown = wait_until(cx, |cx| {
            workspace.read_with(cx, |workspace, _| workspace.review_error.is_some())
        });

        let (error, active, requesting) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.review_error.clone(),
                workspace.active_task,
                workspace.review_requesting.len(),
            )
        });
        assert!(shown);
        assert_eq!(
            error,
            Some((active, "the orchestrator is stopped".into())),
            "the daemon's refusal, without a prefix"
        );
        assert_eq!(requesting, 0, "the request stayed in flight");
    }

    #[gpui::test]
    fn the_refusal_of_an_approval_with_pending_comments_shows_in_the_daemons_words(
        cx: &mut TestAppContext,
    ) {
        let socket = fake_daemon_replying(|request| match request {
            Request::ApproveTask { call, .. } => Some(Event::Error {
                call: Some(call),
                id: None,
                message: "2 comments are pending".into(),
            }),
            _ => None,
        });
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);

        workspace.update(cx, |workspace, cx| {
            workspace
                .data
                .store_diff(diff_of(7, ReviewSource::Merged, 9));
            workspace.set_review_source(ReviewSource::Merged, cx);
            workspace.approve_review(cx);
        });
        let shown = wait_until(cx, |cx| {
            workspace.read_with(cx, |workspace, _| workspace.review_error.is_some())
        });

        let (error, active) = workspace.read_with(cx, |workspace, _| {
            (workspace.review_error.clone(), workspace.active_task)
        });
        assert!(shown);
        assert_eq!(error, Some((active, "2 comments are pending".into())));
    }

    fn close_daemon(check: swarm_protocol::CloseCheck) -> (PathBuf, flume::Receiver<DaemonTaskId>) {
        let (sent, closes) = flume::unbounded();
        let socket = fake_daemon_replying(move |request| match request {
            Request::CheckClose { call, .. } => Some(Event::CloseCheck {
                call,
                check: check.clone(),
            }),
            Request::CloseTask { call, task, .. } => {
                sent.send(task).ok();

                Some(Event::Done { call })
            }
            _ => None,
        });

        (socket, closes)
    }

    fn dialog(
        workspace: &Entity<Workspace>,
        cx: &VisualTestContext,
    ) -> Option<Entity<crate::close_task::CloseTaskDialog>> {
        workspace.read_with(cx, |workspace, _| match &workspace.overlay {
            Some(Overlay::CloseTask { view, .. }) => Some(view.clone()),
            _ => None,
        })
    }

    #[gpui::test]
    fn the_close_dialog_checks_then_closes_and_the_tab_goes_when_the_daemon_says_closed(
        cx: &mut TestAppContext,
    ) {
        let (socket, closes) = close_daemon(swarm_protocol::CloseCheck {
            running_agents: 3,
            user_terminals: 2,
            uncommitted_files: 4,
            unmerged_workers: vec!["a".into(), "b".into()],
            merged_branches: vec!["swarm/login-c".into()],
        });
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);
        let task = workspace.read_with(cx, |workspace, _| workspace.active_task);

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_close_task(task, window, cx);
        });
        let loading = dialog(&workspace, cx).is_some_and(|dialog| {
            dialog.read_with(cx, |dialog, _| {
                *dialog.check_state() == crate::close_task::CheckState::Checking
            })
        });
        let ready = wait_until(cx, |cx| {
            redraw(cx);
            dialog(&workspace, cx)
                .is_some_and(|dialog| dialog.read_with(cx, |dialog, _| !dialog.lines().is_empty()))
        });
        let lines = dialog(&workspace, cx)
            .map(|dialog| dialog.read_with(cx, |dialog, _| dialog.lines()))
            .unwrap_or_default();

        let texts: Vec<_> = lines.iter().map(|(text, _)| text.as_str()).collect();
        assert!(loading && ready);
        assert_eq!(
            texts,
            [
                "Stops 3 agents",
                "Stops 2 terminals",
                "Deletes 4 uncommitted files",
                "Deletes merged branch swarm/login-c",
                "Keeps branch swarm/login",
                "Not merged: a, b"
            ]
        );
        assert_eq!(lines[2].1, swarm_ui::prelude::Color::Warn);

        let bounds = cx.debug_bounds("close-task-confirm");
        assert!(bounds.is_some());
        cx.simulate_click(
            bounds.map_or_else(Default::default, |bounds| bounds.center()),
            gpui::Modifiers::none(),
        );
        let sent = wait_until(cx, |_| !closes.is_empty());
        assert!(sent);
        assert_eq!(closes.try_recv().ok(), Some(DaemonTaskId(7)));

        let mut gone = task_info(7, 5, "login");
        gone.closed = true;
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.task_changed(&gone, Some(window), cx);
        });
        cx.run_until_parked();

        let (has_task, overlay, active_is_start) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.data.has_daemon_task(DaemonTaskId(7)),
                workspace.overlay.is_some(),
                workspace.active_task().is_start,
            )
        });
        assert!(!has_task);
        assert!(!overlay);
        assert!(active_is_start);
    }

    #[gpui::test]
    fn a_closed_task_leaves_the_neighbor_tab_open(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = real_task_workspace(&fake_daemon(), cx);
        let second = task_info(8, 5, "signup");
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.task_changed(&second, Some(window), cx);
        });
        let (first, other) = workspace.read_with(cx, |workspace, _| {
            let ids: Vec<_> = workspace.data.tasks.iter().map(|task| task.id).collect();
            (ids[0], ids[1])
        });
        workspace.update(cx, |workspace, cx| workspace.select_task(first, cx));

        let mut gone = task_info(7, 5, "login");
        gone.closed = true;
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.task_changed(&gone, Some(window), cx);
        });

        let active = workspace.read_with(cx, |workspace, _| workspace.active_task);
        assert_eq!(active, other);
    }

    #[gpui::test]
    fn cancel_closes_the_dialog_and_a_failed_close_shows_its_error(cx: &mut TestAppContext) {
        let socket = fake_daemon_replying(|request| match request {
            Request::CheckClose { call, .. } => Some(Event::CloseCheck {
                call,
                check: swarm_protocol::CloseCheck::default(),
            }),
            Request::CloseTask { call, .. } => Some(Event::Error {
                call: Some(call),
                id: None,
                message: "teardown failed".into(),
            }),
            _ => None,
        });
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);
        let task = workspace.read_with(cx, |workspace, _| workspace.active_task);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_close_task(task, window, cx);
        });
        wait_until(cx, |cx| {
            redraw(cx);
            dialog(&workspace, cx)
                .is_some_and(|dialog| dialog.read_with(cx, |dialog, _| !dialog.lines().is_empty()))
        });

        let bounds = cx.debug_bounds("close-task-confirm");
        assert!(bounds.is_some());
        cx.simulate_click(
            bounds.map_or_else(Default::default, |bounds| bounds.center()),
            gpui::Modifiers::none(),
        );
        let failed = wait_until(cx, |cx| {
            redraw(cx);
            dialog(&workspace, cx).is_some_and(|dialog| {
                dialog.read_with(cx, |dialog, _| dialog.error_text().is_some())
            })
        });
        cx.simulate_keystrokes("escape");

        assert!(failed);
        assert!(dialog(&workspace, cx).is_none());
        assert!(workspace.read_with(cx, |workspace, _| {
            workspace.data.has_daemon_task(DaemonTaskId(7))
        }));
    }

    fn open_dialog_and_wait(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> DaemonTaskId {
        let task = workspace.read_with(cx, |workspace, _| workspace.active_task);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_close_task(task, window, cx);
        });
        let ready = wait_until(cx, |cx| {
            redraw(cx);
            dialog(workspace, cx).is_some_and(|dialog| {
                dialog.read_with(cx, |dialog, _| dialog.confirmed().is_some())
            })
        });
        assert!(ready);

        DaemonTaskId(7)
    }

    fn click_selector(selector: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx.debug_bounds(selector);
        assert!(bounds.is_some(), "{selector} is not on screen");
        cx.simulate_click(
            bounds.map_or_else(Default::default, |bounds| bounds.center()),
            gpui::Modifiers::none(),
        );
    }

    #[gpui::test]
    fn a_slow_close_keeps_the_tab_until_the_daemon_says_closed(cx: &mut TestAppContext) {
        let check = swarm_protocol::CloseCheck {
            running_agents: 1,
            user_terminals: 1,
            ..swarm_protocol::CloseCheck::default()
        };
        let (sent, closes) = flume::unbounded();
        let socket = fake_daemon_replying({
            let check = check.clone();

            move |request| match request {
                Request::CheckClose { call, .. } => Some(Event::CloseCheck {
                    call,
                    check: check.clone(),
                }),
                Request::CloseTask {
                    call, confirmed, ..
                } => {
                    sent.send(confirmed).ok();
                    answer_late();

                    Some(Event::Done { call })
                }
                _ => None,
            }
        });
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);
        open_dialog_and_wait(&workspace, cx);

        click_selector("close-task-confirm", cx);
        let answered = wait_until(cx, |cx| {
            redraw(cx);
            dialog(&workspace, cx).is_none()
        });
        let still_there = workspace.read_with(cx, |workspace, _| {
            workspace.data.has_daemon_task(DaemonTaskId(7))
        });

        let mut gone = task_info(7, 5, "login");
        gone.closed = true;
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.task_changed(&gone, Some(window), cx);
        });
        cx.run_until_parked();

        assert!(answered);
        assert_eq!(closes.try_recv().ok(), Some(check));
        assert!(still_there, "the tab went before the daemon said closed");
        assert!(workspace.read_with(cx, |workspace, _| {
            !workspace.data.has_daemon_task(DaemonTaskId(7))
        }));
    }

    #[gpui::test]
    fn cancel_and_escape_do_nothing_while_the_close_runs(cx: &mut TestAppContext) {
        let (socket, _closes) = close_daemon(swarm_protocol::CloseCheck::default());
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);
        open_dialog_and_wait(&workspace, cx);
        let Some(view) = dialog(&workspace, cx) else {
            panic!("the dialog is not open");
        };
        let events = Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = Rc::clone(&events);
        cx.update(|_, app| {
            app.subscribe(
                &view,
                move |_, event: &crate::close_task::CloseTaskEvent, _| {
                    seen.borrow_mut().push(event.clone());
                },
            )
            .detach();
        });

        view.update(cx, crate::close_task::CloseTaskDialog::set_busy);
        redraw(cx);
        click_selector("close-task-cancel", cx);
        cx.simulate_keystrokes("escape");
        let while_busy = events.borrow().len();
        let open = dialog(&workspace, cx).is_some();

        view.update(cx, |dialog, cx| dialog.fail("refused".into(), cx));
        redraw(cx);
        click_selector("close-task-cancel", cx);

        assert_eq!(while_busy, 0, "Cancel or Escape dismissed a running close");
        assert!(open);
        assert!(dialog(&workspace, cx).is_none());
    }

    #[gpui::test]
    fn a_refused_close_checks_again_and_confirms_the_new_check(cx: &mut TestAppContext) {
        let (sent, closes) = flume::unbounded();
        let checks = std::sync::Arc::new(AtomicU64::new(0));
        let socket = fake_daemon_replying({
            let checks = std::sync::Arc::clone(&checks);

            move |request| match request {
                Request::CheckClose { call, .. } => {
                    let round = checks.fetch_add(1, Ordering::Relaxed);

                    Some(Event::CloseCheck {
                        call,
                        check: swarm_protocol::CloseCheck {
                            uncommitted_files: u32::try_from(round + 1).unwrap_or(u32::MAX),
                            ..swarm_protocol::CloseCheck::default()
                        },
                    })
                }
                Request::CloseTask {
                    call, confirmed, ..
                } => {
                    sent.send(confirmed.clone()).ok();

                    if confirmed.uncommitted_files < 2 {
                        Some(Event::Error {
                            call: Some(call),
                            id: None,
                            message: "more files changed".into(),
                        })
                    } else {
                        Some(Event::Done { call })
                    }
                }
                _ => None,
            }
        });
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);
        open_dialog_and_wait(&workspace, cx);

        click_selector("close-task-confirm", cx);
        let refreshed = wait_until(cx, |cx| {
            redraw(cx);
            dialog(&workspace, cx).is_some_and(|dialog| {
                dialog.read_with(cx, |dialog, _| {
                    dialog.error_text().is_some()
                        && dialog
                            .confirmed()
                            .is_some_and(|check| check.uncommitted_files == 2)
                })
            })
        });
        let lines = dialog(&workspace, cx)
            .map(|dialog| dialog.read_with(cx, |dialog, _| dialog.lines()))
            .unwrap_or_default();

        click_selector("close-task-confirm", cx);
        let second = wait_until(cx, |_| closes.len() >= 2);

        let asked: Vec<_> = closes
            .try_iter()
            .map(|check| check.uncommitted_files)
            .collect();
        assert!(refreshed, "the dialog did not show the new check");
        assert_eq!(lines[0].0, "Deletes 2 uncommitted files");
        assert!(second);
        assert_eq!(asked, [1, 2]);
    }

    #[gpui::test]
    fn the_palette_closes_the_open_task_but_not_a_start_tab(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        cx.simulate_keystrokes("cmd-k");
        cx.simulate_input("close task");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        let opened = dialog(&workspace, cx).is_some();
        let lines = dialog(&workspace, cx)
            .map(|dialog| dialog.read_with(cx, |dialog, _| dialog.lines()))
            .unwrap_or_default();
        cx.simulate_keystrokes("escape");

        workspace.update(cx, Workspace::open_start_task);
        let rows = crate::palette::rows(
            &workspace.read_with(cx, |workspace, _| workspace.data.clone()),
            ProjectId(0),
            workspace.read_with(cx, |workspace, _| workspace.active_task),
            View::Terminals,
        );
        assert!(opened);
        assert!(!lines.is_empty());
        assert!(rows.iter().all(|row| row.label() != "Close task"));
    }
    /// What the daemon answers when it is asked for the lines of a file.
    enum Answer {
        File(Box<swarm_protocol::FileDiff>),
        Gone,
        Fail(&'static str),
    }

    type FileRequests = flume::Receiver<(ReviewSource, PathBuf, PathBuf)>;

    /// The `known` fingerprint of each request for the task diff.
    type DiffAsked = flume::Receiver<Option<u64>>;

    /// A daemon that answers each request for a file's lines with the next `Answer` it is
    /// handed, and reports what it was asked.
    fn file_daemon() -> (PathBuf, FileRequests, flume::Sender<Answer>) {
        let (socket, requests, answer, ..) = file_daemon_with_diffs();

        (socket, requests, answer)
    }

    /// Like `file_daemon`, and answers a request for the task diff with the next diff it is
    /// handed, else with none.
    fn file_daemon_with_diffs() -> (
        PathBuf,
        FileRequests,
        flume::Sender<Answer>,
        DiffAsked,
        flume::Sender<TaskDiff>,
    ) {
        let (asked, requests) = flume::unbounded();
        let (answer, answers) = flume::unbounded::<Answer>();
        let (diff_asked, diff_requests) = flume::unbounded();
        let (fresh, fresh_diffs) = flume::unbounded::<TaskDiff>();
        let socket = fake_daemon_replying(move |request| match request {
            Request::FileHunks {
                call,
                source,
                worktree,
                path,
                ..
            } => {
                asked.send((source, worktree, path)).ok();

                Some(match answers.recv() {
                    Ok(Answer::File(file)) => Event::FileHunks {
                        call,
                        file: Some(*file),
                    },
                    Ok(Answer::Fail(message)) => Event::Error {
                        call: Some(call),
                        id: None,
                        message: message.into(),
                    },
                    Ok(Answer::Gone) | Err(_) => Event::FileHunks { call, file: None },
                })
            }
            Request::TaskDiff { call, known, .. } => {
                diff_asked.send(known).ok();

                Some(Event::TaskDiff {
                    call,
                    diff: fresh_diffs.try_recv().ok(),
                })
            }
            _ => None,
        });

        (socket, requests, answer, diff_requests, fresh)
    }

    fn lock_file(digest: u64, text: Option<&str>) -> swarm_protocol::FileDiff {
        let lines = text.map(|text| swarm_protocol::DiffHunk {
            old_start: 1,
            old_lines: 0,
            new_start: 1,
            new_lines: 1,
            section: String::new(),
            lines: vec![swarm_protocol::DiffLine {
                kind: swarm_protocol::DiffLineKind::Added,
                text: text.into(),
            }],
        });

        swarm_protocol::FileDiff {
            worktree: PathBuf::from("/wt"),
            path: PathBuf::from("Cargo.lock"),
            agent: None,
            change: swarm_protocol::FileChange::Modified,
            mode_change: None,
            additions: 600,
            deletions: 12,
            binary: false,
            hunks: lines.into_iter().collect(),
            truncated: false,
            digest,
            fold: Some(swarm_protocol::Fold::Lock),
        }
    }

    fn folded_diff(fingerprint: u64, digest: u64) -> TaskDiff {
        let mut diff = diff_of(7, ReviewSource::Live, fingerprint);

        diff.files = vec![lock_file(digest, None)];
        diff.stat.files = 1;

        diff
    }

    /// Review on a Live diff with one folded file, behind a daemon that answers with `Answer`.
    fn folded_review<'a>(
        socket: &std::path::Path,
        cx: &'a mut TestAppContext,
    ) -> (
        Entity<Workspace>,
        Entity<crate::review_view::ReviewView>,
        &'a mut VisualTestContext,
        Client,
    ) {
        let (workspace, cx, client) = real_task_workspace(socket, cx);

        workspace.update(cx, |workspace, cx| {
            workspace.data.store_diff(folded_diff(1, 5));
            workspace.set_view(View::Review, cx);
            workspace.set_review_source(ReviewSource::Live, cx);
        });
        redraw(cx);
        let review = workspace.read_with(cx, |workspace, _| workspace.review.clone());

        (workspace, review, cx, client)
    }

    fn lines_of_the_file(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> Vec<String> {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .diffs
                .get(&(DaemonTaskId(7), ReviewSource::Live))
                .and_then(|diff| diff.files.first())
                .map(|file| {
                    file.hunks
                        .iter()
                        .flat_map(|hunk| hunk.lines.iter().map(|line| line.text.clone()))
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    fn rows_of_the_file(
        review: &Entity<crate::review_view::ReviewView>,
        cx: &VisualTestContext,
    ) -> usize {
        review.read_with(cx, |review, _| {
            review
                .rows()
                .iter()
                .filter(|row| row.file() == Some(0))
                .count()
        })
    }

    fn load_of(
        review: &Entity<crate::review_view::ReviewView>,
        cx: &VisualTestContext,
    ) -> Option<crate::review_model::FileLoad> {
        review.read_with(cx, |review, _| review.load_of(0))
    }

    /// Draws without running the tasks that wait on the daemon, which would wait for the test.
    fn draw_only(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn show_the_file(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
        workspace.update(cx, |workspace, cx| {
            workspace.show_file_diff(PathBuf::from("/wt"), PathBuf::from("Cargo.lock"), cx);
        });
    }

    #[gpui::test]
    fn show_diff_asks_the_daemon_once_reads_loading_and_then_shows_the_lines(
        cx: &mut TestAppContext,
    ) {
        use crate::review_model::FileLoad;

        let (socket, requests, answer) = file_daemon();
        let (workspace, review, cx, _client) = folded_review(&socket, cx);
        let before = (rows_of_the_file(&review, cx), load_of(&review, cx));

        show_the_file(&workspace, cx);
        show_the_file(&workspace, cx);
        draw_only(cx);
        let waiting = (rows_of_the_file(&review, cx), load_of(&review, cx));

        answer
            .send(Answer::File(Box::new(lock_file(5, Some("lock = 1")))))
            .ok();
        let shown = wait_until(cx, |cx| {
            redraw(cx);
            load_of(&review, cx) == Some(FileLoad::Loaded)
        });

        assert_eq!(before, (1, None));
        assert_eq!(waiting, (1, Some(FileLoad::Loading)));
        assert!(shown, "the lines never arrived");
        assert_eq!(requests.try_iter().count(), 1, "more than one request");
        assert!(rows_of_the_file(&review, cx) > 1);
        assert_eq!(lines_of_the_file(&workspace, cx), ["lock = 1"]);
        assert!(cx.debug_bounds("review-fold-0").is_some(), "the tag went");
        assert!(cx.debug_bounds("review-show-diff-0").is_none());
    }

    #[gpui::test]
    fn a_refresh_with_the_same_digest_keeps_the_lines_without_a_new_request(
        cx: &mut TestAppContext,
    ) {
        let (socket, requests, answer) = file_daemon();
        let (workspace, review, cx, _client) = folded_review(&socket, cx);
        answer
            .send(Answer::File(Box::new(lock_file(5, Some("lock = 1")))))
            .ok();
        click_selector("review-show-diff-0", cx);
        let loaded = wait_until(cx, |cx| {
            redraw(cx);
            !lines_of_the_file(&workspace, cx).is_empty()
        });
        requests.try_iter().for_each(drop);

        workspace.update(cx, |workspace, cx| {
            workspace.apply_diff(folded_diff(2, 5), cx);
        });
        redraw(cx);
        let settled = wait_until(cx, |cx| {
            redraw(cx);
            workspace.read_with(cx, |workspace, _| {
                workspace
                    .data
                    .diffs
                    .get(&(DaemonTaskId(7), ReviewSource::Live))
                    .is_some_and(|diff| diff.fingerprint == 2)
            })
        });
        wait_a_little();
        redraw(cx);

        assert!(loaded && settled);
        assert_eq!(lines_of_the_file(&workspace, cx), ["lock = 1"]);
        assert!(rows_of_the_file(&review, cx) > 1);
        assert_eq!(requests.try_iter().count(), 0, "a new request went out");
    }

    #[gpui::test]
    fn a_changed_digest_shows_loading_until_the_new_lines_arrive(cx: &mut TestAppContext) {
        use crate::review_model::FileLoad;

        let (socket, requests, answer) = file_daemon();
        let (workspace, review, cx, _client) = folded_review(&socket, cx);
        answer
            .send(Answer::File(Box::new(lock_file(5, Some("old")))))
            .ok();
        click_selector("review-show-diff-0", cx);
        wait_until(cx, |cx| {
            redraw(cx);
            !lines_of_the_file(&workspace, cx).is_empty()
        });
        requests.try_iter().for_each(drop);

        workspace.update(cx, |workspace, cx| {
            workspace.apply_diff(folded_diff(2, 6), cx);
        });
        draw_only(cx);
        let while_loading = (
            lines_of_the_file(&workspace, cx),
            rows_of_the_file(&review, cx),
            load_of(&review, cx),
        );

        answer
            .send(Answer::File(Box::new(lock_file(6, Some("new")))))
            .ok();
        let again = wait_until(cx, |cx| {
            redraw(cx);
            lines_of_the_file(&workspace, cx) == ["new"]
        });

        assert!(
            while_loading.0.is_empty(),
            "the old lines stayed in the card"
        );
        assert_eq!(while_loading.1, 1);
        assert_eq!(while_loading.2, Some(FileLoad::Loading));
        assert!(again, "the new lines never arrived");
        assert_eq!(requests.try_iter().count(), 1, "one new request per change");
        assert_eq!(load_of(&review, cx), Some(FileLoad::Loaded));
        assert!(rows_of_the_file(&review, cx) > 1);
    }

    #[gpui::test]
    fn a_failed_reload_shows_the_error_and_the_button_instead_of_the_old_lines(
        cx: &mut TestAppContext,
    ) {
        use crate::review_model::FileLoad;

        let (socket, requests, answer) = file_daemon();
        let (workspace, review, cx, _client) = folded_review(&socket, cx);
        answer
            .send(Answer::File(Box::new(lock_file(5, Some("old")))))
            .ok();
        click_selector("review-show-diff-0", cx);
        wait_until(cx, |cx| {
            redraw(cx);
            !lines_of_the_file(&workspace, cx).is_empty()
        });
        answer.send(Answer::Fail("the file is locked")).ok();

        workspace.update(cx, |workspace, cx| {
            workspace.apply_diff(folded_diff(2, 6), cx);
        });
        let failed = wait_until(cx, |cx| {
            redraw(cx);
            matches!(load_of(&review, cx), Some(FileLoad::Failed(_)))
        });

        assert!(failed, "the reload never failed");
        assert_eq!(
            load_of(&review, cx),
            Some(FileLoad::Failed("the file is locked".into()))
        );
        assert!(lines_of_the_file(&workspace, cx).is_empty());
        assert_eq!(rows_of_the_file(&review, cx), 1);
        assert!(cx.debug_bounds("review-load-error-0").is_some(), "no error");
        assert!(cx.debug_bounds("review-show-diff-0").is_some(), "no button");

        answer
            .send(Answer::File(Box::new(lock_file(6, Some("new")))))
            .ok();
        click_selector("review-show-diff-0", cx);
        let loaded = wait_until(cx, |cx| {
            redraw(cx);
            load_of(&review, cx) == Some(FileLoad::Loaded)
        });

        assert!(loaded);
        assert_eq!(requests.try_iter().count(), 3);
        assert_eq!(lines_of_the_file(&workspace, cx), ["new"]);
    }

    #[gpui::test]
    fn an_answer_for_an_older_digest_is_dropped_and_the_file_loads_again(cx: &mut TestAppContext) {
        use crate::review_model::FileLoad;

        let (socket, requests, answer) = file_daemon();
        let (workspace, review, cx, _client) = folded_review(&socket, cx);
        answer
            .send(Answer::File(Box::new(lock_file(5, Some("old")))))
            .ok();
        answer
            .send(Answer::File(Box::new(lock_file(6, Some("new")))))
            .ok();

        show_the_file(&workspace, cx);
        workspace.update(cx, |workspace, cx| {
            workspace.apply_diff(folded_diff(2, 6), cx);
        });
        let shown = wait_until(cx, |cx| {
            redraw(cx);
            load_of(&review, cx) == Some(FileLoad::Loaded)
        });

        assert!(shown, "the lines never arrived");
        assert_eq!(
            lines_of_the_file(&workspace, cx),
            ["new"],
            "the old answer landed in the new card"
        );
        assert_eq!(requests.try_iter().count(), 2, "no second request");
    }

    #[gpui::test]
    fn a_failed_load_shows_the_error_keeps_the_button_and_can_be_tried_again(
        cx: &mut TestAppContext,
    ) {
        use crate::review_model::FileLoad;

        let (socket, requests, answer) = file_daemon();
        let (workspace, review, cx, _client) = folded_review(&socket, cx);
        answer.send(Answer::Fail("the file is locked")).ok();

        click_selector("review-show-diff-0", cx);
        let failed = wait_until(cx, |cx| {
            redraw(cx);
            matches!(load_of(&review, cx), Some(FileLoad::Failed(_)))
        });

        assert!(failed);
        assert_eq!(
            load_of(&review, cx),
            Some(FileLoad::Failed("the file is locked".into()))
        );
        assert!(cx.debug_bounds("review-load-error-0").is_some(), "no error");
        assert!(cx.debug_bounds("review-show-diff-0").is_some(), "no button");
        assert_eq!(rows_of_the_file(&review, cx), 1);

        answer
            .send(Answer::File(Box::new(lock_file(5, Some("lock = 1")))))
            .ok();
        click_selector("review-show-diff-0", cx);
        let loaded = wait_until(cx, |cx| {
            redraw(cx);
            load_of(&review, cx) == Some(FileLoad::Loaded)
        });

        assert!(loaded);
        assert_eq!(requests.try_iter().count(), 2);
        assert_eq!(lines_of_the_file(&workspace, cx), ["lock = 1"]);
        assert!(cx.debug_bounds("review-load-error-0").is_none());
    }

    fn stored_diff(
        workspace: &Entity<Workspace>,
        cx: &VisualTestContext,
    ) -> Option<(u64, usize, u32)> {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .diffs
                .get(&(DaemonTaskId(7), ReviewSource::Live))
                .map(|diff| (diff.fingerprint, diff.files.len(), diff.stat.files))
        })
    }

    #[gpui::test]
    fn a_file_the_daemon_does_not_have_fails_the_load_and_the_fresh_diff_decides(
        cx: &mut TestAppContext,
    ) {
        let (socket, _requests, answer, diff_asked, fresh) = file_daemon_with_diffs();
        let (workspace, review, cx, _client) = folded_review(&socket, cx);
        let mut without_it = diff_of(7, ReviewSource::Live, 2);
        without_it.stat.files = 0;
        fresh.send(without_it).ok();
        answer.send(Answer::Gone).ok();

        click_selector("review-show-diff-0", cx);
        let replaced = wait_until(cx, |cx| {
            redraw(cx);
            stored_diff(&workspace, cx) == Some((2, 0, 0))
        });

        assert!(replaced, "the fresh diff never replaced the stored one");
        assert_eq!(diff_asked.try_iter().collect::<Vec<_>>(), [None]);
        assert_eq!(rows_of_the_file(&review, cx), 0);
    }

    #[gpui::test]
    fn a_file_the_daemon_does_not_have_leaves_the_stored_diff_alone(cx: &mut TestAppContext) {
        use crate::review_model::FileLoad;

        let (socket, _requests, answer, diff_asked, fresh) = file_daemon_with_diffs();
        let (workspace, review, cx, _client) = folded_review(&socket, cx);
        fresh.send(folded_diff(1, 5)).ok();
        answer.send(Answer::Gone).ok();

        click_selector("review-show-diff-0", cx);
        let asked = wait_until(cx, |cx| {
            redraw(cx);
            !diff_asked.is_empty()
        });
        wait_a_little();
        redraw(cx);

        assert!(asked, "no fresh diff was asked for");
        assert_eq!(diff_asked.try_iter().collect::<Vec<_>>(), [None]);
        assert_eq!(stored_diff(&workspace, cx), Some((1, 1, 1)));
        assert_eq!(
            load_of(&review, cx),
            Some(FileLoad::Failed("No longer in the diff".into()))
        );
        assert_eq!(rows_of_the_file(&review, cx), 1);
        assert!(cx.debug_bounds("review-load-error-0").is_some(), "no error");
        assert!(cx.debug_bounds("review-show-diff-0").is_some(), "no button");
    }

    #[gpui::test]
    fn a_load_that_ends_after_its_task_closed_leaves_no_entry_behind(cx: &mut TestAppContext) {
        use crate::review_model::AfterLoad;

        let (workspace, cx, _client) = real_task_workspace(&fake_daemon(), cx);
        let key = (
            DaemonTaskId(7),
            ReviewSource::Live,
            PathBuf::from("/wt"),
            PathBuf::from("Cargo.lock"),
        );

        let (next, left) = workspace.update(cx, |workspace, _| {
            workspace.data.store_diff(folded_diff(1, 5));
            workspace.data.loaded.begin(&key, 5);
            let task = workspace.active_task;
            workspace.data.remove_task(task);

            let next = workspace
                .data
                .finish_load(&key, Ok(Some(lock_file(5, Some("late")))));

            (next, workspace.data.loaded.loads(&folded_diff(1, 5)))
        });

        assert_eq!(next, AfterLoad::Nothing);
        assert!(left.is_empty(), "an entry came back for the closed task");
    }

    #[gpui::test]
    fn the_default_fake_daemon_answers_a_file_request(cx: &mut TestAppContext) {
        let socket = fake_daemon();
        let (_workspace, cx, client) = real_task_workspace(&socket, cx);
        cx.executor().allow_parking();

        let answer = client.file_hunks(
            DaemonTaskId(7),
            ReviewSource::Live,
            PathBuf::from("/wt"),
            PathBuf::from("Cargo.lock"),
        );

        assert_eq!(answer.ok(), Some(None));
    }
}
