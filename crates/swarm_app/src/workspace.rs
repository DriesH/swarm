//! The main window's root view.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gpui::{
    AnyWindowHandle, AppContext as _, Bounds, Context, Entity, FocusHandle, Focusable, Pixels,
    SharedString, Size, Subscription, Window, div, px,
};
use swarm_protocol::{Client, ReviewSource};
use swarm_ui::fonts::UI_FONT;
use swarm_ui::prelude::*;

use crate::actions::{
    CreatePullRequest, FocusAgent, NewTask, NewTerminal, OpenSettings, PlanWithOrchestrator,
    SetUpGithub, ShowReview, ShowSwarm, ShowTerminals, ToggleChanges, ToggleCommandPalette,
    ToggleFiles,
};
use crate::daemon::DiffPoll;
use crate::daemon::{DaemonLaunch, LiveTerminal};
use crate::model::{
    AgentId, FakeData, ProjectId, SAMPLE_NOW, Task, TaskId, TaskStatus, TerminalId, TerminalKind,
};
use crate::palette::Overlay;
use crate::panels::ChangesList;
use crate::pull_request::PullRequests;
use crate::review_view::ReviewView;
use crate::swarm_view::{
    Clicked, DotGrid, INSPECTOR_WIDTH, InspectorTab, MessageThread, TerminalPreview,
};
use crate::views::Resizing;
use crate::{panels, sidebar, status_bar, tab_bar, views};

pub(crate) const RIGHT_PANEL_WIDTH: Pixels = px(320.);

/// Space between the window edge and the main panel.
const PANEL_MARGIN: f32 = 8.;
/// The header's height, with its bottom border.
const HEADER_HEIGHT: f32 = 47.;

/// Distance from the window top to the middle of the header's content (the panel border
/// and the header's bottom border do not count). The traffic lights, the sidebar's window
/// row and the task tabs are all centered on this line.
pub(crate) const HEADER_CENTER: Pixels = px(PANEL_MARGIN + 1. + (HEADER_HEIGHT - 1.) / 2.);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum View {
    Terminals,
    Swarm,
    Review,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RightPanel {
    Files,
    Changes,
    PullRequest,
}

/// Where the workspace's projects come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DataSource {
    /// Fake data for snapshots and tests.
    Sample,
    Connecting,
    Live,
    Unavailable {
        reason: SharedString,
    },
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent flags, not one state"
)]
pub(crate) struct Workspace {
    /// The window that shows the workspace. `None` while it is closed: the workspace and its
    /// daemon connection live on, and a new window takes it over.
    pub(crate) window: Option<AnyWindowHandle>,
    pub data: FakeData,
    pub active_project: ProjectId,
    pub active_task: TaskId,
    pub view: View,
    pub right_panel: Option<RightPanel>,
    pub minimized: HashSet<TerminalId>,
    pub focused_terminal: Option<TerminalId>,
    focus_handle: FocusHandle,
    pending_focus: Option<TerminalId>,
    pub(crate) terminal_focus: HashMap<TerminalId, FocusHandle>,
    pub(crate) overlay: Option<Overlay>,
    pub(crate) terminal_area: Option<Bounds<Pixels>>,
    /// True when the right panel was shown as `terminal_area` was measured.
    pub(crate) terminal_area_had_panel: bool,
    pub(crate) layout: swarm_layout::Layout,
    /// The width of the tray in the status bar at the last paint, once known.
    pub(crate) tray_width: Option<Pixels>,
    pub(crate) user_sizes: HashMap<TerminalId, Size<Pixels>>,
    pub(crate) resizing: Option<Resizing>,
    pub(crate) daemon: Option<Client>,
    pub(crate) live_terminals: HashMap<TerminalId, LiveTerminal>,
    /// Live tiles whose terminal lost its daemon connection.
    pub(crate) disconnected: HashSet<TerminalId>,
    /// Agents of tasks the app does not know yet, applied when the task arrives.
    pub(crate) pending_agents: HashMap<swarm_protocol::AgentId, swarm_protocol::AgentInfo>,
    /// Where `settings.toml` lives. `None` keeps the defaults and never touches a file.
    pub(crate) settings_path: Option<PathBuf>,
    /// Tasks whose setup exit was already handled.
    pub(crate) setup_exits_shown: HashSet<swarm_protocol::TaskId>,
    /// Process terminals the user closed. They never get a tile again.
    pub(crate) dismissed_terminals: HashSet<swarm_protocol::TerminalId>,
    /// Projects whose Start orchestrator was asked for. Only Plan with orchestrator asks again.
    pub(crate) start_requested: HashSet<ProjectId>,
    /// The tab used before the open one, for each project.
    pub(crate) previous_tabs: HashMap<ProjectId, TaskId>,
    /// Width and height of one cell of the terminal font, once measured.
    pub(crate) cell_size: Option<(f32, f32)>,
    /// The font size `cell_size` was measured at, in points.
    pub(crate) font_size: f32,
    pub(crate) daemon_launch: Option<DaemonLaunch>,
    pub(crate) reconnecting: bool,
    pub(crate) source: DataSource,
    /// The last failed sidebar action, shown until dismissed.
    pub(crate) sidebar_error: Option<SharedString>,
    pub(crate) orchestrator_input: Entity<TextInput>,
    _orchestrator_subscription: Subscription,
    /// Replaces the system clock, so snapshots and tests show the same times.
    pub(crate) fixed_now: Option<u64>,
    /// The agent selected in the Swarm view, for each task.
    pub(crate) swarm_selection: HashMap<TaskId, AgentId>,
    pub(crate) swarm_focus: HashMap<AgentId, FocusHandle>,
    pub(crate) swarm_zoom: f32,
    /// The agent Review shows, set by View diff.
    pub(crate) review_agent: Option<AgentId>,
    /// The file Review opens at, set by a row of the Changes tab.
    pub(crate) review_file: Option<PathBuf>,
    /// The inspector tab, kept for the window and not for each agent.
    pub(crate) inspector_tab: InspectorTab,
    /// In memory only: `settings.toml` holds config, not window state.
    pub(crate) inspector_width: f32,
    /// The width of the Swarm view at the last paint, once known.
    pub(crate) swarm_view_width: Option<f32>,
    pub(crate) inspector_dragging: bool,
    pub(crate) inspector_handle: FocusHandle,
    pub(crate) thread: Entity<MessageThread>,
    pub(crate) preview: Entity<TerminalPreview>,
    pub(crate) dot_grid: Entity<DotGrid>,
    _inspector_subscription: Subscription,
    _preview_subscription: Subscription,
    pub(crate) changes_poll: Option<gpui::Task<()>>,
    /// The task the running poll asks about.
    pub(crate) polled_task: Option<TaskId>,
    _activation_subscription: Subscription,
    pub(crate) swarm_clock: Option<gpui::Task<()>>,
    /// Refreshes the usage card's time text. Runs only while the card shows a time.
    pub(crate) usage_clock: Option<gpui::Task<()>>,
    pub(crate) review: Entity<ReviewView>,
    pub(crate) changes_list: Entity<ChangesList>,
    /// The source the user picked, or the default decided when Review first opened.
    pub(crate) review_sources: HashMap<TaskId, ReviewSource>,
    /// Goes up with every request to scroll Review to a file.
    pub(crate) review_jump_serial: u64,
    /// The worktree of the file Review opens at, when the request knows it.
    pub(crate) review_worktree: Option<PathBuf>,
    /// The tasks whose approval is on its way to the daemon.
    pub(crate) review_approving: HashSet<TaskId>,
    pub(crate) review_error: Option<(TaskId, SharedString)>,
    /// The tasks whose Request changes is on its way to the daemon.
    pub(crate) review_requesting: HashSet<TaskId>,
    /// The comments listing the daemon last answered, so a list is asked once per diff.
    pub(crate) comments_listed: Option<crate::review_comments::Listed>,
    pub(crate) comment_sync: crate::review_comments::CommentSync,
    /// Moves keyboard focus into Review at the next frame.
    pub(crate) pending_review_focus: bool,
    pub(crate) review_poll: Option<DiffPoll>,
    pub(crate) live_poll: Option<DiffPoll>,
    /// The Merged diff, while the pull request panel needs it and Review does not poll it.
    pub(crate) pr_poll: Option<DiffPoll>,
    /// How often the visible Review source and the open task's Live diff are asked for.
    pub(crate) diff_intervals: (std::time::Duration, std::time::Duration),
    pub(crate) pull_requests: PullRequests,
    /// How often the window told the workspace it was activated or deactivated.
    #[cfg(test)]
    pub(crate) activation_changes: usize,
}

impl Workspace {
    /// The real app: no projects until the daemon answers.
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::with_data(DataSource::Connecting, FakeData::live(), window, cx)
    }

    /// Fake projects, tasks and agents for snapshots and tests.
    pub(crate) fn with_sample(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut workspace = Self::with_data(DataSource::Sample, FakeData::sample(), window, cx);
        workspace.fixed_now = Some(SAMPLE_NOW);
        workspace.sync_inspector(cx);

        workspace
    }

    fn with_data(
        source: DataSource,
        mut data: FakeData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let active_project = data
            .tasks
            .first()
            .map(|task| task.project)
            .or_else(|| data.projects.first().map(|project| project.id));
        let active_task = active_project.map_or(TaskId(u32::MAX), |project| {
            data.first_or_start_task(project)
        });
        let active_project = active_project.unwrap_or(ProjectId(u32::MAX));
        let focused_terminal = data
            .task(active_task)
            .and_then(|task| task.orchestrator)
            .and_then(|id| data.agent(id))
            .map(|agent| agent.terminal);

        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        let (orchestrator_input, orchestrator_subscription) = views::orchestrator_input(cx);
        let activation_subscription = Self::observe_activation(window, cx);

        let inspector_subscription = cx.observe(&cx.entity(), |this, _, cx| {
            this.sync_inspector(cx);
            this.sync_review(cx);
            this.sync_changes_list(cx);
        });
        let workspace = cx.weak_entity();
        let review = cx.new({
            let workspace = workspace.clone();
            |cx| ReviewView::new(workspace, cx)
        });
        let changes_list = cx.new(|cx| ChangesList::new(workspace, cx));
        let preview = cx.new(|_| TerminalPreview::new());
        let preview_subscription = cx.subscribe(&preview, |this, _, _: &Clicked, cx| {
            this.focus_selected_terminal(cx);
        });

        let mut workspace = Self {
            window: Some(window.window_handle()),
            data,
            active_project,
            active_task,
            view: View::Terminals,
            right_panel: Some(RightPanel::Changes),
            minimized: HashSet::new(),
            focused_terminal,
            focus_handle,
            pending_focus: None,
            terminal_focus: HashMap::new(),
            overlay: None,
            terminal_area: None,
            terminal_area_had_panel: false,
            layout: swarm_layout::Layout::default(),
            tray_width: None,
            user_sizes: HashMap::new(),
            resizing: None,
            daemon: None,
            live_terminals: HashMap::new(),
            disconnected: HashSet::new(),
            pending_agents: HashMap::new(),
            settings_path: None,
            setup_exits_shown: HashSet::new(),
            dismissed_terminals: HashSet::new(),
            start_requested: HashSet::new(),
            previous_tabs: HashMap::new(),
            cell_size: None,
            font_size: swarm_term_view::DEFAULT_FONT_SIZE,
            daemon_launch: None,
            reconnecting: false,
            source,
            sidebar_error: None,
            orchestrator_input,
            _orchestrator_subscription: orchestrator_subscription,
            fixed_now: None,
            swarm_selection: HashMap::new(),
            swarm_focus: HashMap::new(),
            swarm_zoom: 1.,
            review_agent: None,
            review_file: None,
            inspector_tab: InspectorTab::default(),
            inspector_width: INSPECTOR_WIDTH,
            swarm_view_width: None,
            inspector_dragging: false,
            inspector_handle: cx.focus_handle().tab_stop(true),
            thread: cx.new(|_| MessageThread::new()),
            preview,
            dot_grid: cx.new(|_| DotGrid),
            _inspector_subscription: inspector_subscription,
            _preview_subscription: preview_subscription,
            changes_poll: None,
            polled_task: None,
            _activation_subscription: activation_subscription,
            swarm_clock: None,
            usage_clock: None,
            review,
            changes_list,
            review_sources: HashMap::new(),
            review_jump_serial: 0,
            review_worktree: None,
            review_approving: HashSet::new(),
            review_error: None,
            review_requesting: HashSet::new(),
            comments_listed: None,
            comment_sync: crate::review_comments::CommentSync::default(),
            pending_review_focus: false,
            review_poll: None,
            live_poll: None,
            pr_poll: None,
            diff_intervals: (crate::daemon::REVIEW_INTERVAL, crate::daemon::LIVE_INTERVAL),
            pull_requests: PullRequests::default(),
            #[cfg(test)]
            activation_changes: 0,
        };
        workspace.sync_inspector(cx);
        workspace.sync_changes_list(cx);

        workspace
    }

    fn observe_activation(window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        cx.observe_window_activation(window, |this, window, cx| {
            #[cfg(test)]
            {
                this.activation_changes += 1;
            }

            this.sync_swarm_tasks(window.is_window_active(), cx);
            this.sync_usage_clock(window.is_window_active(), cx);

            if window.is_window_active() {
                crate::notifications::refresh_authorization(cx);
            }
        })
    }

    /// A new window takes over the workspace. Everything tied to the old window is made again:
    /// the activation observer, the focus and the terminal views.
    #[expect(
        clippy::used_underscore_binding,
        reason = "assigning drops the observer of the closed window"
    )]
    pub(crate) fn attach_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.window = Some(window.window_handle());
        self._activation_subscription = Self::observe_activation(window, cx);
        self.pending_focus = self.focused_terminal;
        window.focus(&self.focus_handle, cx);
        self.reattach_terminals(cx);

        cx.notify();
    }

    /// The window closed. The workspace keeps its data and its daemon connection, but drops
    /// what only made sense in that window: open dialogs, measurements and the polls that
    /// need an active window.
    pub(crate) fn window_closed(&mut self, cx: &mut Context<Self>) {
        self.window = None;
        self.overlay = None;
        self.resizing = None;
        self.inspector_dragging = false;
        self.terminal_area = None;
        self.tray_width = None;
        self.swarm_view_width = None;
        self.sync_swarm_tasks(false, cx);
        self.sync_usage_clock(false, cx);
        self.rest_pull_requests();
        self.detach_terminals();

        cx.notify();
    }

    /// Focuses the focused terminal again. A new window makes its views after the first
    /// frame, so the first focus went to a view that is gone.
    pub(crate) fn refocus_terminal(&mut self, cx: &mut Context<Self>) {
        self.pending_focus = self.focused_terminal;

        cx.notify();
    }

    /// True when no project is shown, so the main area shows the empty state.
    pub(crate) fn has_no_projects(&self) -> bool {
        self.data.projects.is_empty()
    }

    /// True when New task, Plan and "+ Terminal" may act: fake data, or a live task.
    pub(crate) fn can_work(&self) -> bool {
        match self.source {
            DataSource::Sample => true,
            DataSource::Live => self.data.task(self.active_task).is_some(),
            DataSource::Connecting | DataSource::Unavailable { .. } => false,
        }
    }

    /// The open task. The active project always has a task, so the last
    /// fallback is only a guard.
    pub(crate) fn active_task(&self) -> &Task {
        self.data
            .task(self.active_task)
            .or_else(|| self.data.tasks.first())
            .unwrap_or(&NO_TASK)
    }

    /// Opens the project's first task. A project without tasks gets a Start task.
    pub(crate) fn select_project(&mut self, project: ProjectId, cx: &mut Context<Self>) {
        self.active_project = project;

        let task = self.data.first_or_start_task(project);
        self.select_task(task, cx);
    }

    /// Opens the project's Start or Plan task, or makes it when there is none.
    pub(crate) fn open_start_task(&mut self, cx: &mut Context<Self>) {
        if !self.can_work() {
            log::debug!("no task is open, cannot plan with the orchestrator");
            return;
        }

        let project = self.active_project;
        let task = self.data.ensure_start_task(project);

        if !self.data.start_runs(project) {
            self.start_requested.remove(&project);
        }

        self.overlay = None;
        self.select_task(task, cx);
    }

    pub(crate) fn select_task(&mut self, task: TaskId, cx: &mut Context<Self>) {
        let Some(entry) = self.data.task(task) else {
            return;
        };

        let before = self.active_task;
        let same_project = self
            .data
            .task(before)
            .is_some_and(|old| old.project == entry.project);
        if before != task && same_project {
            self.previous_tabs.insert(entry.project, before);
        }

        self.active_project = entry.project;
        self.active_task = task;
        self.focused_terminal = entry
            .orchestrator
            .and_then(|id| self.data.agent(id))
            .map(|agent| agent.terminal);
        self.pending_focus = self.focused_terminal;
        cx.notify();
    }

    /// Opening a view this way clears the agent filter of Review. Only View diff sets it.
    pub(crate) fn set_view(&mut self, view: View, cx: &mut Context<Self>) {
        self.view = view;
        if view != View::Review {
            self.forget_comment_listing();
        }
        self.review_agent = None;
        self.review_file = None;
        self.pending_review_focus = view == View::Review;
        cx.notify();
    }

    /// Opens the panel, or closes it when it is already open.
    pub(crate) fn toggle_panel(&mut self, panel: RightPanel, cx: &mut Context<Self>) {
        if panel == RightPanel::Changes && self.view == View::Review {
            return;
        }

        self.right_panel = if self.panel_in(View::Terminals) == Some(panel) {
            None
        } else {
            Some(panel)
        };
        cx.notify();
    }

    pub(crate) fn minimize(&mut self, terminal: TerminalId, cx: &mut Context<Self>) {
        self.minimized.insert(terminal);
        cx.notify();
    }

    pub(crate) fn restore(&mut self, terminal: TerminalId, cx: &mut Context<Self>) {
        self.minimized.remove(&terminal);
        cx.notify();
    }

    pub(crate) fn focus_terminal(&mut self, terminal: TerminalId, cx: &mut Context<Self>) {
        self.minimized.remove(&terminal);
        self.claim_focus(terminal, cx);
    }

    /// Gives the terminal the focus where it is shown, and leaves the tray alone.
    pub(crate) fn claim_focus(&mut self, terminal: TerminalId, cx: &mut Context<Self>) {
        self.focused_terminal = Some(terminal);
        self.pending_focus = Some(terminal);
        cx.notify();
    }

    /// The handle that takes keyboard focus for a tile. Made on first use.
    pub(crate) fn terminal_focus_handle(
        &mut self,
        terminal: TerminalId,
        cx: &mut Context<Self>,
    ) -> FocusHandle {
        self.terminal_focus
            .entry(terminal)
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone()
    }

    fn ensure_focus_handles(&mut self, cx: &mut Context<Self>) {
        let task = self.active_task();
        let terminals: Vec<TerminalId> = task
            .workers
            .iter()
            .filter_map(|id| self.data.agent(*id))
            .map(|agent| agent.terminal)
            .chain(task.processes.iter().copied())
            .chain(task.user_terminals.iter().copied())
            .collect();

        for terminal in terminals {
            self.terminal_focus_handle(terminal, cx);
        }
    }

    /// Moves keyboard focus to the terminal that `focus_terminal` picked.
    fn apply_pending_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay.is_some() {
            return;
        }
        let Some(terminal) = self.pending_focus.take() else {
            return;
        };

        let is_orchestrator = self
            .data
            .terminal(terminal)
            .is_some_and(|entry| entry.kind == TerminalKind::Orchestrator)
            && !self.live_terminals.contains_key(&terminal);
        let handle = if is_orchestrator {
            self.orchestrator_input.focus_handle(cx)
        } else {
            self.terminal_focus_handle(terminal, cx)
        };

        window.focus(&handle, cx);
    }

    /// Moves keyboard focus into Review after it opened, so its keys work at once.
    fn apply_pending_review_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay.is_some() || self.view != View::Review {
            return;
        }
        if !std::mem::take(&mut self.pending_review_focus) {
            return;
        }

        let handle = self.review.read(cx).body_focus().clone();

        window.focus(&handle, cx);
    }

    /// Focuses the agent at `index` in the open task; the orchestrator is 0.
    pub(crate) fn focus_agent(&mut self, index: usize, cx: &mut Context<Self>) {
        let terminal = self
            .data
            .agents_of(self.active_task)
            .nth(index)
            .map(|agent| agent.terminal);

        if let Some(terminal) = terminal {
            self.focus_terminal(terminal, cx);
        }
    }

    /// The terminal area for the panel shown now. A tab switch can show or hide the
    /// panel before the area is measured again.
    pub(crate) fn terminal_area_size(&self) -> Option<Size<Pixels>> {
        let mut size = self.terminal_area?.size;
        // The layout stays what the Terminals view shows, also while the Swarm view hides the panel.
        match (
            self.terminal_area_had_panel,
            self.panel_in(View::Terminals).is_some(),
        ) {
            (true, false) => size.width += RIGHT_PANEL_WIDTH,
            (false, true) => size.width -= RIGHT_PANEL_WIDTH,
            _ => {}
        }

        Some(size)
    }

    /// The right panel to show. A Start or Plan task and the Swarm view have none, and
    /// Review has no Changes panel, but the choice is kept.
    pub(crate) fn visible_panel(&self) -> Option<RightPanel> {
        self.panel_in(self.view)
    }

    fn panel_in(&self, view: View) -> Option<RightPanel> {
        if self.has_no_projects() || self.active_task().is_start || view == View::Swarm {
            return None;
        }

        // A task that is not approved has no pull request panel: Changes shows instead.
        let panel = match self.right_panel {
            Some(RightPanel::PullRequest) if !self.pr_available(self.active_task) => {
                Some(RightPanel::Changes)
            }
            panel => panel,
        };

        // The file tree of Review lists the changes already.
        if view == View::Review && panel == Some(RightPanel::Changes) {
            return None;
        }

        panel
    }
}

static NO_TASK: Task = Task {
    id: TaskId(u32::MAX),
    project: ProjectId(u32::MAX),
    name: SharedString::new_static(""),
    branch: SharedString::new_static(""),
    status: TaskStatus::Idle,
    orchestrator: None,
    workers: Vec::new(),
    processes: Vec::new(),
    user_terminals: Vec::new(),
    approved: None,
    pull_request: None,
    close_after_merge: false,
    is_start: false,
    daemon: None,
};

impl Focusable for Workspace {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workspace {
    #[expect(
        clippy::too_many_lines,
        reason = "one flat element tree reads best in one place"
    )]
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (ground, panel, line, fg) = (colors.ground, colors.panel, colors.line, colors.fg);

        self.start_orchestrator_if_needed(cx);
        self.sync_swarm_tasks(window.is_window_active(), cx);
        self.sync_usage_clock(window.is_window_active(), cx);
        self.sync_pull_requests(window.is_window_active(), window, cx);
        self.apply_pending_focus(window, cx);
        self.apply_pending_review_focus(window, cx);
        self.apply_pending_pr_focus(window, cx);
        self.ensure_focus_handles(cx);

        if self.view == View::Swarm {
            self.ensure_swarm_handles(cx);
        }

        self.measure_cells(window, cx);
        self.layout = views::compute_layout(self);

        let sidebar = sidebar::render(self, window, cx);
        let tab_bar = tab_bar::render(self, window, cx);
        let views = views::render(self, window, cx);
        let status_bar = status_bar::render(self, window, cx);
        let right_panel = self
            .visible_panel()
            .map(|_| panels::render(self, window, cx));
        let overlay = self.overlay.as_ref().map(Overlay::element);

        let body = h_flex()
            .flex_1()
            .min_h_0()
            .child(div().flex_1().min_w_0().h_full().child(views))
            .children(right_panel.map(|panel_element| {
                div()
                    .w(RIGHT_PANEL_WIDTH)
                    .flex_none()
                    .h_full()
                    .border_l_1()
                    .border_color(line)
                    .child(panel_element)
            }));

        swarm_ui::tab_navigation(
            div()
                .id("workspace")
                .key_context(swarm_ui::ROOT_CONTEXT)
                .track_focus(&self.focus_handle)
                .on_action(cx.listener(|this, _: &ShowTerminals, _, cx| {
                    this.set_view(View::Terminals, cx);
                }))
                .on_action(cx.listener(|this, _: &ShowSwarm, _, cx| {
                    this.set_view(View::Swarm, cx);
                }))
                .on_action(cx.listener(|this, _: &ShowReview, _, cx| {
                    this.set_view(View::Review, cx);
                }))
                .on_action(cx.listener(|this, _: &ToggleFiles, _, cx| {
                    this.toggle_panel(RightPanel::Files, cx);
                }))
                .on_action(cx.listener(|this, _: &ToggleChanges, _, cx| {
                    this.toggle_panel(RightPanel::Changes, cx);
                }))
                .on_action(cx.listener(|this, _: &CreatePullRequest, _, cx| {
                    this.open_pull_request(cx);
                }))
                .on_action(cx.listener(|this, _: &SetUpGithub, window, cx| {
                    this.start_github_setup(window, cx);
                }))
                .on_action(cx.listener(|this, action: &FocusAgent, _, cx| {
                    this.focus_agent(action.index, cx);
                }))
                .on_action(cx.listener(|this, _: &NewTask, window, cx| {
                    this.open_new_task(window, cx);
                }))
                .on_action(cx.listener(|this, _: &PlanWithOrchestrator, _, cx| {
                    this.open_start_task(cx);
                }))
                .on_action(cx.listener(|this, _: &NewTerminal, window, cx| {
                    this.new_user_terminal(window, cx);
                }))
                .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                    crate::settings::open(
                        this.settings_path.clone(),
                        this.data.projects.clone(),
                        cx.weak_entity(),
                        window,
                        cx,
                    );
                }))
                .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| {
                    this.toggle_palette(window, cx);
                }))
                .size_full()
                .relative()
                .flex()
                .bg(ground)
                .text_color(fg)
                .font_family(UI_FONT)
                .text_size(px(13.)),
        )
        .child(
            div()
                .w(px(248.))
                .flex_none()
                .h_full()
                .pt(HEADER_CENTER - sidebar::WINDOW_ROW_HEIGHT / 2.)
                .px(px(8.))
                .pb(px(10.))
                .child(sidebar),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .m(px(PANEL_MARGIN))
                .ml_0()
                .bg(panel)
                .border_1()
                .border_color(line)
                .rounded(px(10.))
                .overflow_hidden()
                .child(
                    div()
                        .h(px(HEADER_HEIGHT))
                        .flex_none()
                        .border_b_1()
                        .border_color(line)
                        .child(tab_bar),
                )
                .child(body)
                .child(
                    div()
                        .h(px(38.))
                        .flex_none()
                        .border_t_1()
                        .border_color(line)
                        .child(status_bar),
                ),
        )
        .children(overlay)
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext};

    use super::*;
    use crate::actions;

    fn setup(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
        cx.update(|cx| {
            swarm_ui::theme::init(cx);
            actions::bind_keys(cx);
        });

        cx.add_window_view(Workspace::with_sample)
    }

    fn orchestrator_terminal(workspace: &Workspace) -> TerminalId {
        let id = workspace.active_task().orchestrator;
        let agent = id.and_then(|id| workspace.data.agent(id));
        agent.map_or(TerminalId(u32::MAX), |agent| agent.terminal)
    }

    #[gpui::test]
    fn set_view_changes_the_view(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        workspace.update(cx, |workspace, cx| workspace.set_view(View::Review, cx));

        assert_eq!(
            workspace.read_with(cx, |workspace, _| workspace.view),
            View::Review
        );
    }

    #[gpui::test]
    fn toggling_a_panel_twice_closes_it(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        workspace.update(cx, |workspace, cx| {
            workspace.toggle_panel(RightPanel::Files, cx);
        });
        let opened = workspace.read_with(cx, |workspace, _| workspace.right_panel);
        workspace.update(cx, |workspace, cx| {
            workspace.toggle_panel(RightPanel::Files, cx);
        });
        let closed = workspace.read_with(cx, |workspace, _| workspace.right_panel);

        assert_eq!(opened, Some(RightPanel::Files));
        assert_eq!(closed, None);
    }

    #[gpui::test]
    fn minimize_and_restore_update_the_set(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let terminal = TerminalId(1);

        workspace.update(cx, |workspace, cx| workspace.minimize(terminal, cx));
        let minimized =
            workspace.read_with(cx, |workspace, _| workspace.minimized.contains(&terminal));
        workspace.update(cx, |workspace, cx| workspace.restore(terminal, cx));
        let restored = workspace.read_with(cx, |workspace, _| workspace.minimized.is_empty());

        assert!(minimized);
        assert!(restored);
    }

    #[gpui::test]
    fn the_orchestrator_starts_focused(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let (focused, orchestrator) = workspace.read_with(cx, |workspace, _| {
            (workspace.focused_terminal, orchestrator_terminal(workspace))
        });

        assert_eq!(focused, Some(orchestrator));
    }

    #[gpui::test]
    fn focus_agent_action_maps_index_to_agent(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        cx.dispatch_action(FocusAgent { index: 2 });

        let (focused, expected) = workspace.read_with(cx, |workspace, _| {
            let agent = workspace.data.agents_of(workspace.active_task).nth(2);
            (
                workspace.focused_terminal,
                agent.map(|agent| agent.terminal),
            )
        });

        assert!(expected.is_some());
        assert_eq!(focused, expected);
    }

    #[gpui::test]
    fn focus_agent_past_the_last_agent_does_nothing(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let before = workspace.read_with(cx, |workspace, _| workspace.focused_terminal);

        cx.dispatch_action(FocusAgent { index: 8 });

        let after = workspace.read_with(cx, |workspace, _| workspace.focused_terminal);
        assert_eq!(before, after);
    }

    #[gpui::test]
    fn selecting_a_task_focuses_its_orchestrator(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        workspace.update(cx, |workspace, cx| workspace.select_task(TaskId(1), cx));

        let (task, focused, orchestrator) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.active_task,
                workspace.focused_terminal,
                orchestrator_terminal(workspace),
            )
        });
        assert_eq!(task, TaskId(1));
        assert_eq!(focused, Some(orchestrator));
    }

    fn redraw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    fn focus_agent_zero_focuses_the_orchestrator_input(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        redraw(cx);
        cx.dispatch_action(FocusAgent { index: 1 });
        redraw(cx);

        cx.dispatch_action(FocusAgent { index: 0 });
        redraw(cx);

        let input = workspace.read_with(cx, |workspace, cx| {
            workspace.orchestrator_input.focus_handle(cx)
        });
        assert!(cx.update(|window, _| input.is_focused(window)));
    }

    #[gpui::test]
    fn clicking_a_worker_tile_focuses_its_handle(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        redraw(cx);
        let worker = workspace.read_with(cx, |workspace, _| {
            let id = workspace.active_task().workers.first().copied();
            id.and_then(|id| workspace.data.agent(id))
                .map_or(TerminalId(u32::MAX), |agent| agent.terminal)
        });

        assert_eq!(worker, TerminalId(1));
        let bounds = cx.debug_bounds("tile-slot-1");
        assert!(bounds.is_some());
        cx.simulate_click(
            bounds.map_or_else(Default::default, |bounds| bounds.center()),
            gpui::Modifiers::none(),
        );
        redraw(cx);

        let handle = workspace.read_with(cx, |workspace, _| {
            workspace.terminal_focus.get(&worker).cloned()
        });
        assert!(handle.is_some_and(|handle| cx.update(|window, _| handle.is_focused(window))));
    }

    #[gpui::test]
    fn selecting_a_project_without_tasks_creates_a_start_task(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        workspace.update(cx, |workspace, cx| {
            workspace.select_project(ProjectId(3), cx);
        });

        let (is_start, count, project) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.active_task().is_start,
                workspace.data.tasks_of(ProjectId(3)).count(),
                workspace.active_task().project,
            )
        });
        assert!(is_start);
        assert_eq!(count, 1);
        assert_eq!(project, ProjectId(3));
    }

    #[gpui::test]
    fn plan_with_orchestrator_twice_keeps_one_start_task(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        cx.dispatch_action(PlanWithOrchestrator);
        cx.dispatch_action(PlanWithOrchestrator);

        let (starts, active_is_start) = workspace.read_with(cx, |workspace, _| {
            let starts = workspace.data.tasks.iter().filter(|task| task.is_start);
            (starts.count(), workspace.active_task().is_start)
        });
        assert_eq!(starts, 1);
        assert!(active_is_start);
    }

    #[gpui::test]
    fn a_start_task_hides_the_right_panel_but_keeps_the_choice(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        cx.dispatch_action(PlanWithOrchestrator);
        let on_start = workspace.read_with(cx, |workspace, _| {
            (workspace.visible_panel(), workspace.right_panel)
        });

        workspace.update(cx, |workspace, cx| workspace.select_task(TaskId(1), cx));
        let on_task = workspace.read_with(cx, |workspace, _| workspace.visible_panel());

        assert_eq!(on_start, (None, Some(RightPanel::Changes)));
        assert_eq!(on_task, Some(RightPanel::Changes));
    }
    #[gpui::test]
    fn the_changes_panel_does_not_show_in_review_but_the_choice_comes_back(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        redraw(cx);
        let terminals = workspace.read_with(cx, |workspace, _| workspace.visible_panel());
        let drawn_in_terminals = cx.debug_bounds("changes-tree").is_some();

        workspace.update(cx, |workspace, cx| workspace.set_view(View::Review, cx));
        redraw(cx);
        let (review, choice) = workspace.read_with(cx, |workspace, _| {
            (workspace.visible_panel(), workspace.right_panel)
        });
        let drawn_in_review = cx.debug_bounds("changes-tree").is_some();

        workspace.update(cx, |workspace, cx| workspace.set_view(View::Terminals, cx));
        redraw(cx);
        let back = workspace.read_with(cx, |workspace, _| workspace.visible_panel());
        let drawn_again = cx.debug_bounds("changes-tree").is_some();

        assert_eq!(terminals, Some(RightPanel::Changes));
        assert!(drawn_in_terminals);
        assert_eq!((review, choice), (None, Some(RightPanel::Changes)));
        assert!(!drawn_in_review, "Review drew the Changes panel");
        assert_eq!(back, Some(RightPanel::Changes));
        assert!(drawn_again);
    }

    #[gpui::test]
    fn toggling_changes_does_nothing_in_review_and_files_still_opens(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| workspace.set_view(View::Review, cx));

        workspace.update(cx, |workspace, cx| {
            workspace.toggle_panel(RightPanel::Changes, cx);
        });
        let kept = workspace.read_with(cx, |workspace, _| workspace.right_panel);
        workspace.update(cx, |workspace, cx| {
            workspace.toggle_panel(RightPanel::Files, cx);
        });
        let files = workspace.read_with(cx, |workspace, _| workspace.visible_panel());
        workspace.update(cx, |workspace, cx| workspace.set_view(View::Terminals, cx));
        let in_terminals = workspace.read_with(cx, |workspace, _| workspace.visible_panel());

        assert_eq!(kept, Some(RightPanel::Changes));
        assert_eq!(files, Some(RightPanel::Files));
        assert_eq!(in_terminals, Some(RightPanel::Files));
    }

    #[gpui::test]
    fn the_pull_request_panel_still_shows_in_review(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| {
            let task = workspace.active_task;
            let fingerprint = workspace.data.merged_fingerprint(task);

            if let Some(entry) = workspace
                .data
                .tasks
                .iter_mut()
                .find(|entry| entry.id == task)
            {
                entry.approved = fingerprint;
            }
            workspace.data.comments.clear();
            workspace.right_panel = Some(RightPanel::PullRequest);
            workspace.set_view(View::Review, cx);
        });

        let panel = workspace.read_with(cx, |workspace, _| workspace.visible_panel());

        assert_eq!(panel, Some(RightPanel::PullRequest));
    }
}
