//! The main window's root view.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    AnyWindowHandle, AppContext as _, Axis, Bounds, Context, Div, ElementId, Entity, FocusHandle,
    Focusable, Hsla, Pixels, Point, Role, SharedString, Size, Stateful, Subscription, Window, div,
    ease_out_quint, point, px,
};
use swarm_layout::Direction;
use swarm_protocol::{Client, ReviewSource};
use swarm_ui::fonts::UI_FONT;
use swarm_ui::prelude::*;
use swarm_ui::roving::RovingFocus;

use crate::actions::{
    About, AddProject, CloseTask, CreatePullRequest, FocusAgent, MoveTileDown, MoveTileLeft,
    MoveTileRight, MoveTileUp, NewTask, NewTerminal, OpenSettings, PlanWithOrchestrator,
    SetUpGithub, ShowReview, ShowSwarm, ShowTerminals, ToggleChanges, ToggleCommandPalette,
    ToggleFiles, ToggleSidebar,
};
use crate::daemon::DiffPoll;
use crate::daemon::{DaemonLaunch, LiveTerminal};
use crate::model::{
    AgentId, FakeData, ProjectId, SAMPLE_NOW, Task, TaskId, TaskStatus, TerminalId, TerminalKind,
};
use crate::palette::Overlay;
use crate::panels::ChangesList;
use crate::plan_editor::{PlanEditor, PlanFocus};
use crate::pull_request::PullRequests;
use crate::review_view::ReviewView;
use crate::settings::Page;
use crate::swarm_view::{
    Clicked, DotGrid, INSPECTOR_WIDTH, InspectorTab, MessageThread, TerminalPreview,
};
use crate::views::Resizing;
use crate::welcome::Welcome;
use crate::window_drag::drag_region;
use crate::{panels, sidebar, status_bar, tab_bar, views};

pub(crate) const RIGHT_PANEL_WIDTH: Pixels = px(320.);

/// Space between the window edge and the main panel.
const PANEL_MARGIN: f32 = 8.;
/// How many terminals `Workspace::recent_focus` remembers.
const RECENT_FOCUS: usize = 32;

/// The title bar's height. The traffic lights, the sidebar button, the tabs and the view
/// switch are all centered on half of it.
pub(crate) const TITLE_BAR_HEIGHT: Pixels = px(40.);

/// The standard window buttons are 14pt tall on macOS.
pub(crate) const TRAFFIC_LIGHT_SIZE: Pixels = px(14.);

/// GPUI centers the buttons in a titlebar `size + 2y` tall, so this y makes it the title bar.
pub(crate) fn traffic_light_position() -> Point<Pixels> {
    point(px(9.), TITLE_BAR_HEIGHT / 2. - TRAFFIC_LIGHT_SIZE / 2.)
}

/// Room for the close, minimize and zoom buttons.
const TRAFFIC_LIGHTS_WIDTH: Pixels = px(78.);
/// What is left of that room in full screen, where macOS hides the buttons.
const FULL_SCREEN_INSET: Pixels = px(8.);

/// Space before and after the toggle while the sidebar is collapsed.
const COLLAPSED_GAP: Pixels = px(4.);
/// Space between the open sidebar's content and its edge. The toggle keeps the same distance
/// from the right end of the title bar's leading part, so it sits above the search button.
const SIDEBAR_INSET: Pixels = px(8.);

/// How long the sidebar takes to collapse or expand.
pub(crate) const SIDEBAR_MOTION: Duration = Duration::from_millis(180);

fn lerp(from: Pixels, to: Pixels, delta: f32) -> Pixels {
    from + (to - from) * delta
}

/// Opacity of the sidebar and of the rail while the column moves. The sidebar fades out in
/// the first half and the rail fades in over the second, so they never show on top of each other.
fn sidebar_and_rail_opacity(openness: f32) -> (f32, f32) {
    let sidebar = ((openness - 0.5) * 2.).clamp(0., 1.);
    let rail = ((0.5 - openness) * 2.).clamp(0., 1.);

    (sidebar, rail)
}

/// The width and right padding of the title bar's leading part, where openness 1 is the
/// expanded sidebar and 0 the collapsed one. Collapsed it holds the lights, the toggle and a
/// gap on both sides of the toggle, so the tabs follow the toggle.
fn leading_size(lights: Pixels, openness: f32) -> (Pixels, Pixels) {
    let toggle = IconButtonSize::Medium.square();
    let collapsed = lights + COLLAPSED_GAP + toggle + COLLAPSED_GAP;

    (
        lerp(collapsed, sidebar::SIDEBAR_WIDTH, openness),
        lerp(COLLAPSED_GAP, SIDEBAR_INSET, openness),
    )
}

/// A running collapse or expand of the sidebar. Openness 1 is expanded and 0 is collapsed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SidebarMotion {
    pub from: f32,
    pub to: f32,
    /// On the GPUI clock, `cx.background_executor().now()`.
    pub started: Instant,
    /// A snapshot holds the motion at one share of its time.
    #[cfg(feature = "snapshot")]
    pub frozen: Option<f32>,
}

impl SidebarMotion {
    pub(crate) fn new(from: f32, to: f32, started: Instant) -> Self {
        Self {
            from,
            to,
            started,
            #[cfg(feature = "snapshot")]
            frozen: None,
        }
    }

    /// A full collapse or expand takes [`SIDEBAR_MOTION`]; a part of one takes less.
    pub(crate) fn length(&self) -> Duration {
        SIDEBAR_MOTION.mul_f32((self.to - self.from).abs())
    }

    pub(crate) fn deadline(&self) -> Instant {
        self.started + self.length()
    }

    /// The share of the motion's time that has passed, from 0 to 1.
    fn progress(&self, now: Instant) -> f32 {
        #[cfg(feature = "snapshot")]
        {
            if let Some(frozen) = self.frozen {
                return frozen;
            }
        }

        let length = self.length();
        if length.is_zero() {
            return 1.;
        }

        let elapsed = now.saturating_duration_since(self.started);

        (elapsed.as_secs_f32() / length.as_secs_f32()).clamp(0., 1.)
    }

    /// Decided by time, like the end of the resize hold.
    fn finished(&self, now: Instant) -> bool {
        #[cfg(feature = "snapshot")]
        {
            if self.frozen.is_some() {
                return false;
            }
        }

        now >= self.deadline()
    }

    pub(crate) fn openness(&self, now: Instant) -> f32 {
        if self.finished(now) {
            return self.to;
        }

        self.from + (self.to - self.from) * ease_out_quint()(self.progress(now))
    }
}

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
    /// True when the sidebar shows as a narrow rail of buttons.
    pub(crate) sidebar_collapsed: bool,
    /// True when the user opened the usage card in a window short enough to fold it.
    pub(crate) usage_expanded: bool,
    /// True while the pointer is on the rail's usage ring, which then shows the usage card.
    pub(crate) rail_usage_hovered: bool,
    /// The version whose update card the user dismissed. Not saved across launches.
    pub(crate) update_card_dismissed: Option<swarm_update::Version>,
    /// True while the rail's update button shows the card.
    pub(crate) rail_update_open: bool,
    /// Where the rail's update button was drawn, so a click on it does not count as outside.
    pub(crate) rail_update_trigger: Option<Bounds<Pixels>>,
    /// Focus of the rail's update button, where focus returns when its card closes.
    pub(crate) rail_update_button_focus: FocusHandle,
    /// Focus of the rail's update card, which takes focus when it opens.
    pub(crate) rail_update_card_focus: FocusHandle,
    /// The running collapse or expand animation, if any.
    pub(crate) sidebar_motion: Option<SidebarMotion>,
    /// Tracks focus inside the sidebar and the rail, so a toggle can take focus back.
    pub(crate) sidebar_focus: FocusHandle,
    /// The Add project button of the empty state, a drawn control when no project exists.
    pub(crate) add_project_focus: FocusHandle,
    pub minimized: HashSet<TerminalId>,
    /// The order of the tiles the user chose, per task. A task without an entry keeps the
    /// default order.
    pub(crate) tile_order: HashMap<TaskId, Vec<TerminalId>>,
    /// What the screen reader says about the last tile command.
    pub(crate) tile_status: SharedString,
    pub focused_terminal: Option<TerminalId>,
    /// The terminals that took the focus last, the latest first. A Start tab uses it to
    /// decide which tiles get a place in its band.
    pub(crate) recent_focus: Vec<TerminalId>,
    focus_handle: FocusHandle,
    pending_focus: Option<TerminalId>,
    /// True after focus fell back to a drawn control. The tile that holds it then does not
    /// become the focused one, so a tile that waits in the tray keeps its place.
    focus_landed: bool,
    pub(crate) terminal_focus: HashMap<TerminalId, FocusHandle>,
    /// The focus of the resize handle under each user terminal.
    pub(crate) resize_focus: HashMap<TerminalId, FocusHandle>,
    /// The focus of each project row in the sidebar.
    pub(crate) project_focus: HashMap<ProjectId, FocusHandle>,
    /// One Tab stop for the project list, arrow keys inside it. It owns the handles of
    /// `project_focus`.
    pub(crate) project_roving: RovingFocus,
    /// The same for the agent list below the projects.
    pub(crate) agent_roving: RovingFocus,
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
    /// Projects the daemon is removing. Send plan for one does nothing.
    pub(crate) removing_projects: HashSet<ProjectId>,
    /// The plan editor of each project, made when its Start tab first shows it.
    pub(crate) plan_editors: HashMap<ProjectId, Entity<PlanEditor>>,
    /// Whether the plan editor of the open Start tab takes the focus when it shows.
    pub(crate) plan_focus: PlanFocus,
    /// The tab used before the open one, for each project.
    pub(crate) previous_tabs: HashMap<ProjectId, TaskId>,
    /// Width and height of one cell of the terminal font, once measured.
    pub(crate) cell_size: Option<(f32, f32)>,
    /// The font size `cell_size` was measured at, in points.
    pub(crate) font_size: f32,
    /// Width and height of one cell at the default font size, once measured. Tiles are
    /// sized in rows of this font, so a bigger font shows fewer rows in the same tile.
    pub(crate) default_cell_size: Option<(f32, f32)>,
    pub(crate) daemon_launch: Option<DaemonLaunch>,
    pub(crate) reconnecting: bool,
    /// The daemon the app should replace, when it found one.
    pub(crate) stale_daemon: Option<crate::daemon_restart::StaleDaemon>,
    /// The app restarted the daemon once already.
    pub(crate) daemon_restarted: bool,
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
    /// Reads Reduce Motion again each time the window becomes active.
    _motion_subscription: Subscription,
    _focus_lost_subscription: Subscription,
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
    /// The tasks that close: their surface shows the cleanup screen.
    pub(crate) cleanups: crate::cleanup::Cleanups,
    pub(crate) review_error: Option<(TaskId, SharedString)>,
    /// The tasks whose Request changes is on its way to the daemon.
    pub(crate) review_requesting: HashSet<TaskId>,
    /// The comments listing the daemon last answered, so a list is asked once per diff.
    pub(crate) comments_listed: Option<crate::review_comments::Listed>,
    pub(crate) comment_sync: crate::review_comments::CommentSync,
    /// The viewed and folded marks of Review, kept in step with the daemon.
    pub(crate) marks_sync: crate::review_view::MarksSync,
    /// Moves keyboard focus into Review at the next frame.
    pub(crate) pending_review_focus: bool,
    pub(crate) review_poll: Option<DiffPoll>,
    pub(crate) live_poll: Option<DiffPoll>,
    /// The real task whose files the Files panel shows now, once the daemon was asked.
    pub(crate) files_task: Option<swarm_protocol::TaskId>,
    /// The tasks whose file list is being asked for now. The value says whether a change
    /// came in meanwhile, so the list is asked once more.
    pub(crate) files_asks: HashMap<swarm_protocol::TaskId, bool>,
    /// The Merged diff, while the pull request panel needs it and Review does not poll it.
    pub(crate) pr_poll: Option<DiffPoll>,
    /// How often the visible Review source and the open task's Live diff are asked for.
    pub(crate) diff_intervals: (Duration, Duration),
    pub(crate) pull_requests: PullRequests,
    /// The welcome screen, while it shows.
    pub(crate) welcome: Option<Entity<Welcome>>,
    /// Debug builds: ⌘K opened the welcome screen, whatever the projects and the flag.
    pub(crate) welcome_forced: bool,
    /// What the easter eggs keep: the swarm's stats, the toast and the flying bees.
    pub(crate) eggs: crate::eggs::Eggs,
    /// The creature in the title bar, the catches and the schedule of visits.
    pub(crate) field_guide: crate::field_guide::FieldGuide,
    /// The honeycomb behind the empty state and, when the settings ask, the Swarm view.
    pub(crate) honeycomb: Entity<Honeycomb>,
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

    #[expect(
        clippy::too_many_lines,
        reason = "one flat list of fields reads best in one place"
    )]
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
        let motion_subscription = swarm_ui::motion::follow_reduce_motion(window, cx);

        let inspector_subscription = cx.observe(&cx.entity(), |this, _, cx| {
            this.sync_inspector(cx);
            this.sync_review(cx);
            this.sync_changes_list(cx);
        });
        let workspace = cx.weak_entity();
        let review = cx.new(|cx| ReviewView::new(workspace.clone(), cx));
        cx.subscribe(
            &review,
            |this, _, event: &crate::review_view::ReviewEvent, cx| {
                let crate::review_view::ReviewEvent::MarksChanged(task) = event;

                this.save_review_marks(*task, cx);
            },
        )
        .detach();
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
            sidebar_collapsed: false,
            usage_expanded: false,
            rail_usage_hovered: false,
            update_card_dismissed: None,
            rail_update_open: false,
            rail_update_trigger: None,
            rail_update_button_focus: cx.focus_handle().tab_stop(true),
            rail_update_card_focus: cx.focus_handle(),
            sidebar_motion: None,
            sidebar_focus: cx.focus_handle(),
            add_project_focus: cx.focus_handle().tab_stop(true),
            minimized: HashSet::new(),
            tile_order: HashMap::new(),
            tile_status: SharedString::default(),
            focused_terminal,
            recent_focus: Vec::new(),
            focus_handle,
            pending_focus: None,
            focus_landed: false,
            terminal_focus: HashMap::new(),
            resize_focus: HashMap::new(),
            project_focus: HashMap::new(),
            project_roving: RovingFocus::new(Axis::Vertical),
            agent_roving: RovingFocus::new(Axis::Vertical),
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
            removing_projects: HashSet::new(),
            plan_editors: HashMap::new(),
            plan_focus: PlanFocus::Off,
            previous_tabs: HashMap::new(),
            cell_size: None,
            font_size: swarm_term_view::DEFAULT_FONT_SIZE,
            default_cell_size: None,
            daemon_launch: None,
            reconnecting: false,
            stale_daemon: None,
            daemon_restarted: false,
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
            _motion_subscription: motion_subscription,
            _focus_lost_subscription: Self::restore_focus_when_lost(window, cx),
            swarm_clock: None,
            usage_clock: None,
            review,
            changes_list,
            review_sources: HashMap::new(),
            review_jump_serial: 0,
            review_worktree: None,
            review_approving: HashSet::new(),
            cleanups: crate::cleanup::Cleanups::new(),
            review_error: None,
            review_requesting: HashSet::new(),
            comments_listed: None,
            comment_sync: crate::review_comments::CommentSync::default(),
            marks_sync: crate::review_view::MarksSync::default(),
            pending_review_focus: false,
            review_poll: None,
            live_poll: None,
            files_task: None,
            files_asks: HashMap::new(),
            pr_poll: None,
            diff_intervals: (crate::daemon::REVIEW_INTERVAL, crate::daemon::LIVE_INTERVAL),
            pull_requests: PullRequests::default(),
            welcome: None,
            welcome_forced: false,
            eggs: crate::eggs::Eggs::default(),
            field_guide: crate::field_guide::FieldGuide::default(),
            honeycomb: cx.new(|_| Honeycomb::new()),
            #[cfg(test)]
            activation_changes: 0,
        };
        workspace.sync_inspector(cx);
        workspace.sync_changes_list(cx);

        workspace
    }

    /// Gives focus back when it is lost: to the sidebar when the focused element of it went,
    /// and to a control that is drawn when nothing has focus or the focused element is not
    /// drawn any more. Without it, Tab and the shortcuts would do nothing. Focus in an
    /// overlay is left alone.
    fn restore_focus_when_lost(window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        cx.on_focus_lost(window, |this, window, cx| {
            let target = match window.focus_lost_restore_target(cx) {
                Some(parent) if parent == this.sidebar_focus => this.sidebar_landing(parent),
                _ if window.focused(cx).is_some() && this.overlay.is_some() => return,
                _ => {
                    this.focus_landed = true;
                    this.landing_handle(cx)
                }
            };

            window.focus(&target, cx);
        })
    }

    /// Where focus goes when a control of the sidebar is gone: the Add project button when
    /// there is no project, else the row of the open project (its rail button when the sidebar
    /// is collapsed), which shows a focus ring, else the sidebar itself.
    pub(crate) fn sidebar_landing(&self, sidebar: FocusHandle) -> FocusHandle {
        if self.has_no_projects() {
            return self.add_project_focus.clone();
        }

        self.project_focus
            .get(&self.active_project)
            .cloned()
            .unwrap_or(sidebar)
    }

    /// The control focus lands on when the one that had it is gone: the open overlay, else
    /// the focused tile, or the first tile in the Terminals view, the selected card in the
    /// Swarm view, then the row of the open project. The workspace itself is the last resort.
    pub(crate) fn landing_handle(&mut self, cx: &mut Context<Self>) -> FocusHandle {
        if let Some(overlay) = self.overlay.as_ref() {
            return overlay.focus_handle(cx);
        }

        let tile = match self.view {
            View::Terminals => self
                .focused_terminal
                .filter(|terminal| self.tile_is_drawn(*terminal, cx))
                .or_else(|| {
                    self.layout
                        .placed
                        .first()
                        .map(|(tile, _)| TerminalId(tile.0))
                        .filter(|terminal| self.tile_is_drawn(*terminal, cx))
                }),
            View::Swarm | View::Review => None,
        };
        if let Some(terminal) = tile {
            return self.tile_focus_handle(terminal, cx);
        }

        let card = (self.view == View::Swarm)
            .then(|| self.selected_agent())
            .flatten()
            .and_then(|agent| self.swarm_focus.get(&agent).cloned());
        if let Some(card) = card {
            return card;
        }

        let row = (!self.sidebar_collapsed)
            .then(|| self.project_focus.get(&self.active_project).cloned())
            .flatten();

        row.unwrap_or_else(|| self.focus_handle.clone())
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
        self._motion_subscription = swarm_ui::motion::follow_reduce_motion(window, cx);
        self._focus_lost_subscription = Self::restore_focus_when_lost(window, cx);
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
        self.welcome = None;
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

        let task = self.data.ensure_start_task(self.active_project);

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
        self.plan_focus = if entry.is_start {
            PlanFocus::Asked
        } else {
            PlanFocus::Off
        };

        cx.notify();
    }

    /// Opens the Terminals view, where the tiles are drawn. Does nothing when it is open.
    fn show_terminals(&mut self, cx: &mut Context<Self>) {
        if self.view != View::Terminals {
            self.set_view(View::Terminals, cx);
        }
    }

    /// Opening a view this way clears the agent filter of Review. Only View diff sets it.
    /// Review takes the keyboard focus.
    pub(crate) fn set_view(&mut self, view: View, cx: &mut Context<Self>) {
        self.set_view_keeping_focus(view, cx);
        self.pending_review_focus = view == View::Review;
    }

    /// Like `set_view`, but Review does not take the keyboard focus. The View control uses
    /// it while an arrow key moves over its segments.
    pub(crate) fn set_view_keeping_focus(&mut self, view: View, cx: &mut Context<Self>) {
        self.view = view;
        if view != View::Review {
            self.forget_comment_listing();
        }
        self.review_agent = None;
        self.review_file = None;
        self.pending_review_focus = false;
        cx.notify();
    }

    /// Opens a view from a shortcut and puts the focus on its first control, so the keys of
    /// the view work at once: the focused tile, the selected card, or the diff.
    pub(crate) fn show_view(&mut self, view: View, window: &mut Window, cx: &mut Context<Self>) {
        self.set_view(view, cx);

        if self.overlay.is_some() {
            return;
        }

        match view {
            View::Terminals => self.pending_focus = self.focused_terminal,
            View::Swarm => {
                if let Some(agent) = self.selected_agent() {
                    let card = self.swarm_handle(agent, cx);
                    window.focus(&card, cx);
                }
            }
            View::Review => {}
        }
    }

    /// Collapses the sidebar to its rail, or expands it. Focus in the sidebar or the rail moves
    /// back to the focused terminal, or to the workspace when there is none. The change
    /// animates, unless Reduce Motion is on. A toggle during the animation turns it around
    /// from where it is.
    pub(crate) fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar_focus.contains_focused(window, cx) {
            match self.focused_terminal {
                Some(_) => self.pending_focus = self.focused_terminal,
                None => window.focus(&self.focus_handle, cx),
            }
        }

        let now = cx.background_executor().now();
        let from = self.sidebar_openness(now);

        self.sidebar_collapsed = !self.sidebar_collapsed;
        self.rail_update_open = false;
        let to = if self.sidebar_collapsed { 0. } else { 1. };

        if swarm_ui::motion::reduce_motion(cx) || (to - from).abs() < f32::EPSILON {
            self.sidebar_motion = None;
        } else {
            let motion = SidebarMotion::new(from, to, now);

            swarm_term_view::hold_resizes_until(motion.deadline(), cx);
            self.sidebar_motion = Some(motion);
        }

        cx.notify();
    }

    /// Opens the sidebar at once, for a message it has to show.
    pub(crate) fn show_sidebar(&mut self) {
        self.sidebar_collapsed = false;
        self.rail_update_open = false;
        self.sidebar_motion = None;
    }

    /// How open the sidebar is at `now`: 1 expanded, 0 collapsed, in between while it moves.
    pub(crate) fn sidebar_openness(&self, now: Instant) -> f32 {
        match &self.sidebar_motion {
            Some(motion) => motion.openness(now),
            None if self.sidebar_collapsed => 0.,
            None => 1.,
        }
    }

    /// Ends the sidebar motion once it is over, and asks for the next frame while it runs.
    /// Returns how open to draw the sidebar.
    fn advance_sidebar_motion(&mut self, window: &mut Window, cx: &mut Context<Self>) -> f32 {
        let now = cx.background_executor().now();

        if let Some(motion) = self.sidebar_motion {
            if motion.finished(now) {
                self.sidebar_motion = None;
            } else {
                window.request_animation_frame();
            }
        }

        self.sidebar_openness(now)
    }

    /// Opens the settings window, or focuses it. `page` is the page to show.
    pub(crate) fn open_settings(&self, page: Option<Page>, cx: &mut Context<Self>) {
        crate::settings::open(
            self.settings_path.clone(),
            self.data.projects.clone(),
            cx.weak_entity(),
            page,
            cx,
        );
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

    /// Opens the Terminals view, restores the tile from the tray and focuses it.
    pub(crate) fn focus_terminal(&mut self, terminal: TerminalId, cx: &mut Context<Self>) {
        self.minimized.remove(&terminal);
        self.show_terminals(cx);
        self.claim_focus(terminal, cx);
    }

    /// Gives the terminal the focus where it is shown, and leaves the tray alone.
    pub(crate) fn claim_focus(&mut self, terminal: TerminalId, cx: &mut Context<Self>) {
        self.focused_terminal = Some(terminal);
        self.pending_focus = Some(terminal);
        self.note_recent_focus(terminal);
        cx.notify();
    }

    /// Puts the terminal first in the list of the terminals that had the focus.
    pub(crate) fn note_recent_focus(&mut self, terminal: TerminalId) {
        self.recent_focus.retain(|other| *other != terminal);
        self.recent_focus.insert(0, terminal);
        self.recent_focus.truncate(RECENT_FOCUS);
    }

    /// A closed terminal has no place in the list of the terminals that had the focus.
    pub(crate) fn forget_recent_focus(&mut self, terminal: TerminalId) {
        self.recent_focus.retain(|other| *other != terminal);
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

    /// Syncs the roving focus of the two sidebar lists with their rows, and keeps
    /// `project_focus` pointing at the project handles. A row focused by a click becomes the
    /// Tab stop of its list.
    fn ensure_project_focus(&mut self, window: &Window, cx: &mut Context<Self>) {
        let keys: Vec<ElementId> = self
            .data
            .projects
            .iter()
            .map(|project| ElementId::from(("project", project.id.0 as usize)))
            .collect();
        self.project_roving.sync_keyed(&keys, cx);

        self.project_focus = self
            .data
            .projects
            .iter()
            .enumerate()
            .map(|(index, project)| (project.id, self.project_roving.handle(index)))
            .collect();

        self.agent_roving.sync_keyed(&sidebar::row_keys(self), cx);

        self.project_roving.follow_focus(window, cx);
        self.agent_roving.follow_focus(window, cx);
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

        let users = self.active_task().user_terminals.clone();
        for terminal in users {
            self.resize_focus
                .entry(terminal)
                .or_insert_with(|| cx.focus_handle().tab_stop(true));
        }
    }

    /// True when the tile of `terminal` is part of the frame being built: the Terminals view
    /// shows it in the layout. Focus on a tile that is not drawn is lost.
    fn tile_is_drawn(&self, terminal: TerminalId, cx: &App) -> bool {
        self.view == View::Terminals
            && !self.has_no_projects()
            && !self.shows_welcome(cx)
            && self.data.terminal(terminal).is_some()
            && self
                .layout
                .placed
                .iter()
                .any(|(tile, _)| tile.0 == terminal.0)
    }

    /// Moves keyboard focus to the terminal that `focus_terminal` picked, once its tile is
    /// drawn. A tile that is not drawn leaves the focus where it is.
    fn apply_pending_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay.is_some() {
            return;
        }
        let Some(terminal) = self.pending_focus.take() else {
            return;
        };
        if !self.tile_is_drawn(terminal, cx) {
            return;
        }

        let handle = self.tile_focus_handle(terminal, cx);

        window.focus(&handle, cx);
    }

    /// The handle that takes keyboard focus for the tile of `terminal`. A sample orchestrator
    /// tile takes it in its input.
    pub(crate) fn tile_focus_handle(
        &mut self,
        terminal: TerminalId,
        cx: &mut Context<Self>,
    ) -> FocusHandle {
        let is_orchestrator = self
            .data
            .terminal(terminal)
            .is_some_and(|entry| entry.kind == TerminalKind::Orchestrator)
            && !self.live_terminals.contains_key(&terminal);

        if is_orchestrator {
            self.orchestrator_input.focus_handle(cx)
        } else {
            self.terminal_focus_handle(terminal, cx)
        }
    }

    /// Makes the tile that holds keyboard focus the focused one, so its ring follows Tab.
    fn follow_keyboard_focus(&mut self, window: &Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.focus_landed)
            || self.pending_focus.is_some()
            || self.overlay.is_some()
        {
            return;
        }

        let orchestrator_input = self.orchestrator_input.focus_handle(cx);
        let holder = self
            .terminal_focus
            .iter()
            .find(|(_, handle)| handle.contains_focused(window, cx))
            .map(|(terminal, _)| *terminal)
            .or_else(|| {
                orchestrator_input
                    .contains_focused(window, cx)
                    .then(|| self.active_task().orchestrator)
                    .flatten()
                    .and_then(|agent| self.data.agent(agent))
                    .map(|agent| agent.terminal)
            });

        if let Some(terminal) = holder
            && self.focused_terminal != Some(terminal)
        {
            self.focused_terminal = Some(terminal);
            self.note_recent_focus(terminal);
        }
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

#[cfg(feature = "snapshot")]
impl Workspace {
    /// Holds the running sidebar motion at `progress` of its time, so a snapshot shows it
    /// in flight.
    pub(crate) fn freeze_sidebar_motion_for_snapshot(
        &mut self,
        progress: f32,
        cx: &mut Context<Self>,
    ) {
        if let Some(motion) = self.sidebar_motion.as_mut() {
            motion.frozen = Some(progress);
        }

        cx.notify();
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
    approval_stale: false,
    pull_request: None,
    close_after_merge: false,
    is_start: false,
    daemon: None,
};

impl Workspace {
    /// The window's root element: the key context, the actions and the colors. The welcome
    /// screen and the workspace both sit in it.
    fn root(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let colors = cx.theme().colors();
        let (ground, fg) = (colors.ground, colors.fg);

        swarm_ui::tab_navigation(
            div()
                .id("workspace")
                .key_context(swarm_ui::ROOT_CONTEXT)
                .track_focus(&self.focus_handle)
                .on_action(cx.listener(|this, _: &ShowTerminals, window, cx| {
                    this.show_view(View::Terminals, window, cx);
                }))
                .on_action(cx.listener(|this, _: &ShowSwarm, window, cx| {
                    this.show_view(View::Swarm, window, cx);
                }))
                .on_action(cx.listener(|this, _: &ShowReview, window, cx| {
                    this.show_view(View::Review, window, cx);
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
                .on_action(cx.listener(
                    |this, _: &crate::actions::ExpandDescription, window, cx| {
                        this.open_pr_description(window, cx);
                    },
                ))
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
                .on_action(cx.listener(|_, _: &AddProject, window, cx| {
                    Self::pick_project(window, cx);
                }))
                .on_action(cx.listener(|this, _: &CloseTask, window, cx| {
                    this.open_close_task(this.active_task, window, cx);
                }))
                .on_action(cx.listener(|this, _: &ToggleSidebar, window, cx| {
                    this.toggle_sidebar(window, cx);
                }))
                .on_action(cx.listener(|this, _: &OpenSettings, _, cx| {
                    this.open_settings(None, cx);
                }))
                .on_action(cx.listener(|this, _: &About, _, cx| {
                    this.open_settings(Some(Page::General), cx);
                }))
                .on_action(cx.listener(|this, _: &MoveTileLeft, window, cx| {
                    this.move_tile(Direction::Left, window, cx);
                }))
                .on_action(cx.listener(|this, _: &MoveTileRight, window, cx| {
                    this.move_tile(Direction::Right, window, cx);
                }))
                .on_action(cx.listener(|this, _: &MoveTileUp, window, cx| {
                    this.move_tile(Direction::Up, window, cx);
                }))
                .on_action(cx.listener(|this, _: &MoveTileDown, window, cx| {
                    this.move_tile(Direction::Down, window, cx);
                }))
                .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| {
                    this.toggle_palette(window, cx);
                }))
                .size_full()
                .relative()
                .flex()
                .flex_col()
                .bg(ground)
                .text_color(fg)
                .font_family(UI_FONT)
                .text_size(px(13.)),
        )
    }

    /// The full-width row on top of the window: room for the traffic lights, the sidebar
    /// button and the tabs with the view switch and the panel buttons. The sidebar button is
    /// at the right end of the part over the sidebar, so the tabs follow it.
    fn title_bar(&self, openness: f32, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let lights = if window.is_fullscreen() {
            FULL_SCREEN_INSET
        } else {
            TRAFFIC_LIGHTS_WIDTH
        };
        let (width, right_padding) = leading_size(lights, openness);

        // Expanded, this part is as wide as the sidebar, so the tabs start where the card does.
        let leading = h_flex()
            .h_full()
            .flex_none()
            .w(width)
            .pr(right_padding)
            .child(
                drag_region("traffic-lights-drag", window, cx)
                    .w(lights)
                    .h_full()
                    .flex_none(),
            )
            .child(
                drag_region("sidebar-title-drag", window, cx)
                    .flex_1()
                    .h_full(),
            )
            .child(
                div()
                    .flex_none()
                    // A press must not take the focus: the field the user types in keeps it.
                    .capture_any_mouse_down(|_, window, _| window.prevent_default())
                    .child(
                        IconButton::new("toggle-sidebar", IconName::Sidebar, "Toggle sidebar")
                            .size(IconButtonSize::Medium)
                            .debug_selector(|| "toggle-sidebar".to_owned())
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(ToggleSidebar), cx);
                            }),
                    ),
            );

        h_flex()
            .h(TITLE_BAR_HEIGHT)
            .w_full()
            .flex_none()
            .pr(px(PANEL_MARGIN))
            .child(leading)
            .child(tab_bar::render(self, window, cx))
    }

    /// The column left of the main card. At rest it holds the sidebar or the rail. While it
    /// moves it holds both at their full width, so the text does not reflow: the column clips
    /// them, and one fades out before the other fades in. Only the sidebar tracks the focus.
    fn sidebar_column(&self, openness: f32, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let column = div()
            .debug_selector(|| "sidebar-column".to_owned())
            .w(lerp(sidebar::RAIL_WIDTH, sidebar::SIDEBAR_WIDTH, openness))
            .flex_none()
            .overflow_hidden();

        let moving = self.sidebar_motion.is_some();
        if self.sidebar_collapsed && !moving {
            return column.child(sidebar::render_rail(self, true, cx));
        }

        let sidebar = div()
            .w(sidebar::SIDEBAR_WIDTH)
            .h_full()
            .pt(px(4.))
            .px(SIDEBAR_INSET)
            .pb(px(10.))
            .child(sidebar::render(self, window, cx));
        if !moving {
            return column.child(sidebar);
        }

        let (sidebar_opacity, rail_opacity) = sidebar_and_rail_opacity(openness);

        column
            .child(sidebar.absolute().top_0().left_0().opacity(sidebar_opacity))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .w(sidebar::RAIL_WIDTH)
                    .h_full()
                    .opacity(rail_opacity)
                    .child(sidebar::render_rail(self, false, cx)),
            )
    }

    /// True while the welcome screen shows: no project yet, and the user has not dealt with
    /// it. The daemon must have said there are none: while it connects, an app that has
    /// projects looks the same.
    pub(crate) fn shows_welcome(&self, cx: &App) -> bool {
        self.welcome_forced
            || self.has_no_projects()
                && matches!(self.source, DataSource::Live | DataSource::Sample)
                && crate::settings::welcome_pending(cx)
    }

    /// Debug builds only: shows the welcome screen until it is finished.
    pub(crate) fn show_welcome(&mut self, cx: &mut Context<Self>) {
        self.welcome_forced = true;

        cx.notify();
    }

    /// The welcome screen, made when it first shows and dropped when it ends.
    pub(crate) fn welcome_screen(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<Welcome>> {
        if !self.shows_welcome(cx) {
            self.welcome = None;

            return None;
        }

        let workspace = cx.weak_entity();
        let welcome = self
            .welcome
            .get_or_insert_with(|| cx.new(|cx| Welcome::new(workspace, window, cx)));

        Some(welcome.clone())
    }

    /// Ends the welcome screen for good and shows the empty workspace.
    pub(crate) fn finish_welcome(&mut self, cx: &mut Context<Self>) {
        crate::settings::finish_welcome(cx);
        self.welcome = None;
        self.welcome_forced = false;

        cx.notify();
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (panel, line) = (colors.panel, colors.line);

        self.sync_plan_editor(window, cx);
        self.sync_swarm_tasks(window.is_window_active(), cx);
        self.sync_usage_clock(window.is_window_active(), cx);
        self.sync_pull_requests(window.is_window_active(), window, cx);
        self.apply_pending_review_focus(window, cx);
        self.apply_pending_pr_focus(window, cx);
        self.ensure_focus_handles(cx);
        self.ensure_project_focus(window, cx);

        if self.view == View::Swarm {
            self.ensure_swarm_handles(cx);
        }

        self.follow_keyboard_focus(window, cx);
        self.measure_cells(window, cx);
        self.layout = views::compute_layout(self);
        self.apply_pending_focus(window, cx);

        if let Some(welcome) = self.welcome_screen(window, cx) {
            let overlay = self.overlay.as_ref().map(Overlay::element);

            return self
                .root(cx)
                .child(welcome)
                .children(overlay)
                .into_any_element();
        }

        let openness = self.advance_sidebar_motion(window, cx);
        let sidebar = self.sidebar_column(openness, window, cx);
        let title_bar = self.title_bar(openness, window, cx);
        let views = views::render(self, window, cx);
        let status_bar = status_bar::render(self, window, cx);
        let right_panel = self
            .visible_panel()
            .filter(|_| !self.is_cleaning(self.active_task))
            .map(|_| panels::render(self, window, cx));
        let overlay = self.overlay.as_ref().map(Overlay::element);
        let panel_label = match self.visible_panel() {
            Some(RightPanel::Files) => "Files",
            Some(RightPanel::PullRequest) => "Pull request",
            Some(RightPanel::Changes) | None => "Changes",
        };

        let body = h_flex()
            .flex_1()
            .min_h_0()
            .child(div().flex_1().min_w_0().h_full().child(views))
            .children(
                right_panel
                    .map(|panel_element| right_panel_slot(panel_label, line).child(panel_element)),
            );

        let root = self.root(cx);

        root.child(title_bar)
            .child(
                h_flex()
                    .items_stretch()
                    .flex_1()
                    .min_h_0()
                    .child(sidebar)
                    .child(
                        v_flex()
                            .debug_selector(|| "main-card".to_owned())
                            .flex_1()
                            .min_w_0()
                            .mr(px(PANEL_MARGIN))
                            .mb(px(PANEL_MARGIN))
                            .bg(panel)
                            .border_1()
                            .border_color(line)
                            .rounded(px(10.))
                            .overflow_hidden()
                            .child(body)
                            .child(
                                div()
                                    .h(px(38.))
                                    .flex_none()
                                    .border_t_1()
                                    .border_color(line)
                                    .child(status_bar),
                            ),
                    ),
            )
            .child(crate::update_card::announcement(cx))
            .children(overlay)
            .children(self.eggs_layer(cx))
            .into_any_element()
    }
}

/// The column of the right panel. A complementary landmark named after the panel, so a screen
/// reader user can jump to it.
fn right_panel_slot(label: &'static str, line: Hsla) -> Stateful<Div> {
    div()
        .id("right-panel")
        .role(Role::Complementary)
        .aria_label(label)
        .w(RIGHT_PANEL_WIDTH)
        .flex_none()
        .h_full()
        .border_l_1()
        .border_color(line)
}

/// Makes the app follow Reduce Motion as `reduced` says, whatever System Settings says.
/// Tests that read where the sidebar ends up turn it on, so a toggle ends at once.
#[cfg(test)]
pub(crate) fn follow_test_reduce_motion(reduced: bool, cx: &mut App) {
    fn on() -> bool {
        true
    }

    fn off() -> bool {
        false
    }

    let source = if reduced { on } else { off };

    cx.set_global(swarm_ui::motion::ReduceMotionSource(source));
    swarm_ui::motion::sync_reduce_motion(cx);
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
            follow_test_reduce_motion(true, cx);
        });

        cx.add_window_view(Workspace::with_sample)
    }

    /// The same workspace with Reduce Motion off, so the sidebar animates.
    fn setup_with_motion(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
        let (workspace, cx) = setup(cx);
        cx.update(|_, cx| follow_test_reduce_motion(false, cx));

        (workspace, cx)
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

    fn nothing_has_focus(cx: &mut VisualTestContext) -> bool {
        cx.update(|window, cx| window.focused(cx).is_none())
    }

    #[gpui::test]
    fn after_the_toggle_button_collapses_the_sidebar_the_keys_and_actions_still_work(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        redraw(cx);
        let button = cx
            .debug_bounds("toggle-sidebar")
            .expect("the sidebar button is on screen");

        cx.simulate_click(button.center(), gpui::Modifiers::none());
        redraw(cx);
        let collapsed = workspace.read_with(cx, |workspace, _| workspace.sidebar_collapsed);
        let focus_lost = nothing_has_focus(cx);

        cx.simulate_keystrokes("cmd-k");
        redraw(cx);
        let palette = workspace.read_with(cx, |workspace, _| workspace.overlay.is_some());
        cx.simulate_keystrokes("escape");
        cx.dispatch_action(ToggleSidebar);
        redraw(cx);

        assert!(collapsed);
        assert!(!focus_lost, "nothing has focus after the sidebar collapsed");
        assert!(palette, "Cmd-K does nothing");
        assert!(workspace.read_with(cx, |workspace, _| !workspace.sidebar_collapsed));
    }

    fn collapsed(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> bool {
        workspace.read_with(cx, |workspace, _| workspace.sidebar_collapsed)
    }

    /// Focuses the terminal of the first worker and returns it.
    fn focus_a_worker(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> TerminalId {
        redraw(cx);
        let terminal = workspace.read_with(cx, |workspace, _| {
            let id = workspace.active_task().workers.first().copied();
            id.and_then(|id| workspace.data.agent(id))
                .map_or(TerminalId(u32::MAX), |agent| agent.terminal)
        });

        workspace.update(cx, |workspace, cx| workspace.focus_terminal(terminal, cx));
        redraw(cx);

        terminal
    }

    fn holds_focus(
        workspace: &Entity<Workspace>,
        terminal: TerminalId,
        cx: &mut VisualTestContext,
    ) -> bool {
        let handle = workspace.read_with(cx, |workspace, _| {
            workspace.terminal_focus.get(&terminal).cloned()
        });

        handle.is_some_and(|handle| cx.update(|window, _| handle.is_focused(window)))
    }

    fn click_toggle(cx: &mut VisualTestContext) {
        let button = cx
            .debug_bounds("toggle-sidebar")
            .expect("the toggle button is on screen");

        cx.simulate_click(button.center(), gpui::Modifiers::none());
        redraw(cx);
    }

    #[gpui::test]
    fn the_rail_button_expands_the_sidebar(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let terminal = focus_a_worker(&workspace, cx);
        workspace.update_in(cx, Workspace::toggle_sidebar);
        redraw(cx);
        let was_collapsed = collapsed(&workspace, cx);

        click_toggle(cx);

        assert!(was_collapsed);
        assert!(!collapsed(&workspace, cx));
        assert!(cx.debug_bounds("add-project").is_some());
        assert!(
            holds_focus(&workspace, terminal, cx),
            "the focus did not come back to the terminal"
        );
    }

    #[gpui::test]
    fn clicking_the_sidebar_toggle_keeps_the_focus_on_the_terminal(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let terminal = focus_a_worker(&workspace, cx);

        click_toggle(cx);
        let after_collapse = (
            collapsed(&workspace, cx),
            holds_focus(&workspace, terminal, cx),
        );
        click_toggle(cx);
        let after_expand = (
            collapsed(&workspace, cx),
            holds_focus(&workspace, terminal, cx),
        );

        assert_eq!(after_collapse, (true, true));
        assert_eq!(after_expand, (false, true));
        assert_eq!(
            workspace.read_with(cx, |workspace, _| workspace.focused_terminal),
            Some(terminal)
        );
    }

    fn toggle_bounds(cx: &mut VisualTestContext) -> Bounds<Pixels> {
        cx.debug_bounds("toggle-sidebar")
            .expect("the toggle button is on screen")
    }

    #[gpui::test]
    fn the_toggle_lines_up_with_the_search_button_while_the_sidebar_is_open(
        cx: &mut TestAppContext,
    ) {
        let (_workspace, cx) = setup(cx);
        redraw(cx);

        let toggle = toggle_bounds(cx).center().x;
        let search = cx
            .debug_bounds("sidebar-search")
            .expect("the search button is on screen")
            .center()
            .x;

        assert_eq!(toggle, search);
    }

    #[gpui::test]
    fn the_sidebar_toggle_starts_right_of_the_traffic_lights(cx: &mut TestAppContext) {
        // The close, minimize and zoom buttons end 69 points from the window's left edge.
        const ZOOM_BUTTON_END: Pixels = px(69.);
        let (workspace, cx) = setup(cx);
        redraw(cx);
        let expanded = toggle_bounds(cx).left();

        workspace.update_in(cx, Workspace::toggle_sidebar);
        redraw(cx);
        let collapsed = toggle_bounds(cx).left();

        assert!(expanded >= ZOOM_BUTTON_END, "expanded: {expanded:?}");
        assert!(collapsed >= ZOOM_BUTTON_END, "collapsed: {collapsed:?}");
    }

    #[gpui::test]
    fn in_full_screen_the_collapsed_sidebar_toggle_moves_left_to_where_the_lights_were(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        workspace.update_in(cx, Workspace::toggle_sidebar);
        redraw(cx);
        let windowed = toggle_bounds(cx).left();

        cx.update(|window, _| window.toggle_fullscreen());
        redraw(cx);
        let full_screen = toggle_bounds(cx).left();

        assert_eq!(
            windowed - full_screen,
            TRAFFIC_LIGHTS_WIDTH - FULL_SCREEN_INSET
        );
    }

    #[gpui::test]
    fn in_full_screen_the_open_sidebar_toggle_stays_above_the_search_button(
        cx: &mut TestAppContext,
    ) {
        let (_workspace, cx) = setup(cx);
        cx.update(|window, _| window.toggle_fullscreen());
        redraw(cx);

        let toggle = toggle_bounds(cx).center().x;
        let search = cx
            .debug_bounds("sidebar-search")
            .expect("the search button is on screen")
            .center()
            .x;

        assert_eq!(toggle, search);
    }

    #[gpui::test]
    fn clicking_the_toggle_keeps_the_focus_in_the_plan_editor(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.select_project(ProjectId(3), cx);
        });
        redraw(cx);
        let editor = workspace
            .read_with(cx, |workspace, _| workspace.shown_plan_editor())
            .expect("the Start tab shows the plan editor");
        let field = editor.read_with(cx, PlanEditor::text_focus);
        let before = cx.update(|window, _| field.is_focused(window));

        click_toggle(cx);

        assert!(before, "the plan editor does not hold the focus");
        assert!(collapsed(&workspace, cx));
        assert!(cx.update(|window, _| field.is_focused(window)));
    }

    /// A key press that also lets go, which is what turns Space on a button into a click.
    fn press_space(cx: &mut VisualTestContext) {
        cx.simulate_keystrokes("space");

        if let Ok(keystroke) = gpui::Keystroke::parse("space") {
            cx.simulate_event(gpui::KeyUpEvent { keystroke });
        }

        redraw(cx);
    }

    #[gpui::test]
    fn space_on_the_toggle_toggles_twice_and_keeps_its_focus(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.update(|_, cx| {
            cx.bind_keys([gpui::KeyBinding::new(
                "tab",
                swarm_ui::FocusNext,
                Some(swarm_ui::ROOT_CONTEXT),
            )]);
        });
        focus_workspace(&workspace, cx);

        cx.simulate_keystrokes("tab");
        redraw(cx);
        let on_toggle = cx.update(|window, cx| window.focused(cx));
        press_space(cx);
        let after_first = collapsed(&workspace, cx);
        press_space(cx);
        let after_second = collapsed(&workspace, cx);
        let still = cx.update(|window, cx| window.focused(cx));

        assert!(on_toggle.is_some());
        assert!(
            after_first,
            "Space on the toggle did not collapse the sidebar"
        );
        assert!(!after_second, "Space did not expand it again");
        assert_eq!(still, on_toggle);
    }

    fn focus_workspace(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
        let focus = workspace.read_with(cx, |workspace, _| workspace.focus_handle.clone());

        cx.update(|window, cx| window.focus(&focus, cx));
        redraw(cx);
    }

    #[gpui::test]
    fn cmd_b_collapses_and_expands_the_sidebar(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        focus_workspace(&workspace, cx);

        cx.simulate_keystrokes("cmd-b");
        redraw(cx);
        let after_first = collapsed(&workspace, cx);
        cx.simulate_keystrokes("cmd-b");
        redraw(cx);
        let after_second = collapsed(&workspace, cx);

        assert!(after_first);
        assert!(!after_second);
    }

    // A sample tile is a plain focus handle. The real terminal view lets ⌘ keys through:
    // see `swarm_term_view/tests/terminal_view.rs::cmd_shortcuts_are_not_consumed`.
    #[gpui::test]
    fn cmd_b_works_while_a_tile_has_focus(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let terminal = focus_a_worker(&workspace, cx);
        let holds = holds_focus(&workspace, terminal, cx);

        cx.simulate_keystrokes("cmd-b");
        redraw(cx);

        assert!(holds, "the tile does not hold the focus");
        assert!(collapsed(&workspace, cx));
    }

    #[gpui::test]
    fn focus_goes_to_the_workspace_when_the_focused_row_leaves_the_window(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        redraw(cx);
        let row = workspace
            .read_with(cx, |workspace, _| {
                workspace.project_focus.get(&ProjectId(3)).cloned()
            })
            .expect("the project has a row");
        cx.update(|window, cx| window.focus(&row, cx));
        redraw(cx);

        workspace.update(cx, |workspace, cx| {
            workspace.project_removed(ProjectId(3), cx);
        });
        redraw(cx);
        redraw(cx);

        cx.simulate_keystrokes("cmd-k");
        redraw(cx);
        let palette = workspace.read_with(cx, |workspace, _| workspace.overlay.is_some());
        assert!(palette, "Cmd-K does nothing after the focused row went");
    }

    #[gpui::test]
    fn focus_lost_behind_an_open_overlay_lands_in_the_overlay(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        redraw(cx);
        cx.simulate_keystrokes("cmd-k");
        redraw(cx);
        let overlay = workspace.read_with(cx, |workspace, cx| {
            workspace
                .overlay
                .as_ref()
                .map(|overlay| overlay.focus_handle(cx))
        });
        let overlay = overlay.expect("the palette is open");

        cx.update(Window::blur);
        redraw(cx);
        redraw(cx);

        assert!(cx.update(|window, cx| overlay.contains_focused(window, cx)));
    }

    #[gpui::test]
    fn the_right_panel_is_a_complementary_landmark_named_after_the_panel(cx: &mut TestAppContext) {
        use gpui::Element as _;
        use gpui::accesskit::{Node, Role as AccessRole};

        cx.update(swarm_ui::theme::init);

        let element = right_panel_slot("Files", gpui::black()).into_element();
        let mut node = Node::new(AccessRole::Unknown);
        element.write_a11y_info(&mut node);

        assert_eq!(element.a11y_role(), Some(AccessRole::Complementary));
        assert_eq!(node.label(), Some("Files"));
    }

    /// True when the handle of the tile of `terminal` has the keyboard focus.
    fn tile_holds_focus(
        workspace: &Entity<Workspace>,
        terminal: TerminalId,
        cx: &mut VisualTestContext,
    ) -> bool {
        let handle = workspace.update(cx, |workspace, cx| {
            workspace.tile_focus_handle(terminal, cx)
        });

        cx.update(|window, _| handle.is_focused(window))
    }

    fn view_of(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> View {
        workspace.read_with(cx, |workspace, _| workspace.view)
    }

    /// Opens the Swarm view and runs `enter`, which returns the terminal it wants focused.
    fn enter_from_swarm(
        cx: &mut TestAppContext,
        enter: impl FnOnce(&mut Workspace, &mut Context<Workspace>) -> TerminalId,
    ) -> (View, bool) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| workspace.set_view(View::Swarm, cx));
        redraw(cx);

        let target = workspace.update(cx, enter);
        redraw(cx);

        (
            view_of(&workspace, cx),
            tile_holds_focus(&workspace, target, cx),
        )
    }

    /// True when something has keyboard focus and it is not the tile of `terminal`.
    fn focus_is_on_a_drawn_control_not_the_tile(
        workspace: &Entity<Workspace>,
        terminal: TerminalId,
        cx: &mut VisualTestContext,
    ) -> bool {
        let tile = workspace.update(cx, |workspace, cx| {
            workspace.tile_focus_handle(terminal, cx)
        });
        let focused = cx.update(|window, cx| window.focused(cx));

        focused.is_some() && focused != Some(tile)
    }

    #[gpui::test]
    fn from_the_swarm_view_enter_on_a_task_tab_keeps_the_view(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| workspace.set_view(View::Swarm, cx));
        redraw(cx);

        let target = workspace.update(cx, |workspace, cx| {
            workspace.select_task(TaskId(1), cx);

            orchestrator_terminal(workspace)
        });
        redraw(cx);

        assert_eq!(view_of(&workspace, cx), View::Swarm);
        assert!(focus_is_on_a_drawn_control_not_the_tile(
            &workspace, target, cx
        ));
    }

    #[gpui::test]
    fn from_the_swarm_view_enter_on_a_project_row_keeps_the_view(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| workspace.set_view(View::Swarm, cx));
        redraw(cx);

        let target = workspace.update(cx, |workspace, cx| {
            workspace.select_project(workspace.active_project, cx);

            orchestrator_terminal(workspace)
        });
        redraw(cx);

        assert_eq!(view_of(&workspace, cx), View::Swarm);
        assert!(focus_is_on_a_drawn_control_not_the_tile(
            &workspace, target, cx
        ));
    }

    #[gpui::test]
    fn from_the_review_view_enter_on_a_task_tab_keeps_the_view(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| workspace.set_view(View::Review, cx));
        redraw(cx);

        let target = workspace.update(cx, |workspace, cx| {
            workspace.select_task(TaskId(1), cx);

            orchestrator_terminal(workspace)
        });
        redraw(cx);

        assert_eq!(view_of(&workspace, cx), View::Review);
        assert!(focus_is_on_a_drawn_control_not_the_tile(
            &workspace, target, cx
        ));
    }

    #[gpui::test]
    fn from_the_swarm_view_a_tray_chip_or_agent_row_focuses_a_drawn_tile(cx: &mut TestAppContext) {
        let (view, focused) = enter_from_swarm(cx, |workspace, cx| {
            workspace.focus_terminal(TerminalId(1), cx);

            TerminalId(1)
        });

        assert_eq!(view, View::Terminals);
        assert!(focused);
    }

    #[gpui::test]
    fn from_the_swarm_view_a_number_key_focuses_a_drawn_tile(cx: &mut TestAppContext) {
        let (view, focused) = enter_from_swarm(cx, |workspace, cx| {
            workspace.focus_agent(1, cx);

            TerminalId(1)
        });

        assert_eq!(view, View::Terminals);
        assert!(focused);
    }

    #[gpui::test]
    fn from_the_review_view_a_number_key_focuses_a_drawn_tile(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| workspace.set_view(View::Review, cx));
        redraw(cx);

        workspace.update(cx, |workspace, cx| workspace.focus_agent(1, cx));
        redraw(cx);

        assert_eq!(view_of(&workspace, cx), View::Terminals);
        assert!(tile_holds_focus(&workspace, TerminalId(1), cx));
    }

    #[gpui::test]
    fn a_pending_focus_on_a_tile_that_is_not_drawn_leaves_the_focus_on_a_drawn_control(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| workspace.set_view(View::Swarm, cx));
        redraw(cx);

        workspace.update(cx, |workspace, cx| workspace.claim_focus(TerminalId(1), cx));
        redraw(cx);

        let focused = cx.update(|window, cx| window.focused(cx));
        let tile = workspace.update(cx, |workspace, cx| {
            workspace.tile_focus_handle(TerminalId(1), cx)
        });
        assert!(focused.is_some(), "nothing has focus");
        assert!(focused != Some(tile), "the tile is not drawn");
    }

    #[gpui::test]
    fn a_tile_is_drawn_in_the_terminals_view_only(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        redraw(cx);

        let in_terminals = workspace.read_with(cx, |workspace, cx| {
            workspace.tile_is_drawn(TerminalId(1), cx)
        });
        workspace.update(cx, |workspace, cx| workspace.set_view(View::Swarm, cx));
        let in_swarm = workspace.read_with(cx, |workspace, cx| {
            workspace.tile_is_drawn(TerminalId(1), cx)
        });

        assert!(in_terminals);
        assert!(!in_swarm);
    }

    #[gpui::test]
    fn a_focused_tile_that_goes_away_moves_focus_to_a_drawn_control(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        focus_a_worker(&workspace, cx);

        workspace.update(cx, |workspace, cx| workspace.set_view(View::Swarm, cx));
        redraw(cx);
        redraw(cx);

        let card = workspace.update(cx, |workspace, cx| {
            let agent = workspace.selected_agent().expect("an agent is selected");

            workspace.swarm_handle(agent, cx)
        });
        assert!(cx.update(|window, _| card.is_focused(window)));
    }

    #[gpui::test]
    fn the_ring_follows_the_tile_that_takes_keyboard_focus(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        redraw(cx);
        let worker = workspace.update(cx, |workspace, cx| {
            workspace.tile_focus_handle(TerminalId(2), cx)
        });

        cx.update(|window, cx| window.focus(&worker, cx));
        redraw(cx);

        let focused = workspace.read_with(cx, |workspace, _| workspace.focused_terminal);
        assert_eq!(focused, Some(TerminalId(2)));
    }

    fn on_a_drawn_tile(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
        let focused = workspace.read_with(cx, |workspace, _| workspace.focused_terminal);

        focused.is_some_and(|terminal| tile_holds_focus(workspace, terminal, cx))
    }

    #[gpui::test]
    fn removing_a_focused_project_row_moves_focus_to_the_row_of_the_open_project(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        redraw(cx);
        let (open, other) = workspace.read_with(cx, |workspace, _| {
            let open = workspace.active_project;
            let other = workspace
                .data
                .projects
                .iter()
                .map(|project| project.id)
                .find(|id| *id != open)
                .expect("a second project");

            (open, other)
        });
        let rows = workspace.read_with(cx, |workspace, _| {
            (
                workspace.project_focus.get(&open).cloned(),
                workspace.project_focus.get(&other).cloned(),
            )
        });
        let (open_row, other_row) = (
            rows.0.expect("the open project has a row"),
            rows.1.expect("the other project has a row"),
        );
        cx.update(|window, cx| window.focus(&other_row, cx));
        drop(other_row);
        redraw(cx);

        workspace.update(cx, |workspace, cx| workspace.project_removed(other, cx));
        redraw(cx);
        redraw(cx);

        assert!(cx.update(|window, _| open_row.is_focused(window)));
    }

    #[gpui::test]
    fn a_closed_tile_gives_the_focus_to_the_tile_beside_it(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        redraw(cx);
        cx.dispatch_action(NewTerminal);
        redraw(cx);
        let user = workspace.read_with(cx, |workspace, _| {
            workspace
                .active_task()
                .user_terminals
                .last()
                .copied()
                .expect("a user terminal")
        });
        workspace.update(cx, |workspace, cx| workspace.focus_terminal(user, cx));
        redraw(cx);
        assert!(tile_holds_focus(&workspace, user, cx));

        workspace.update(cx, |workspace, cx| workspace.close_user_terminal(user, cx));
        redraw(cx);
        redraw(cx);

        assert!(on_a_drawn_tile(&workspace, cx), "no tile has focus");
        assert!(!tile_holds_focus(&workspace, user, cx));
    }

    #[gpui::test]
    fn a_background_tab_that_closes_leaves_the_focus_on_a_drawn_tile(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        redraw(cx);
        let bounds = cx.debug_bounds("tab-1").expect("the tab is on screen");
        cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        redraw(cx);
        workspace.update(cx, |workspace, cx| workspace.select_task(TaskId(0), cx));
        redraw(cx);

        workspace.update(cx, |workspace, cx| {
            workspace.remove_closed_task(TaskId(1), cx);
        });
        redraw(cx);
        redraw(cx);

        assert!(on_a_drawn_tile(&workspace, cx), "no tile has focus");
    }

    #[gpui::test]
    fn the_view_shortcuts_put_the_focus_on_the_new_view(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        focus_a_worker(&workspace, cx);

        cx.dispatch_action(ShowSwarm);
        redraw(cx);
        let card = workspace.update(cx, |workspace, cx| {
            let agent = workspace.selected_agent().expect("an agent is selected");

            workspace.swarm_handle(agent, cx)
        });
        let on_card = cx.update(|window, _| card.is_focused(window));

        cx.dispatch_action(ShowTerminals);
        redraw(cx);
        let on_tile = on_a_drawn_tile(&workspace, cx);

        cx.dispatch_action(ShowReview);
        redraw(cx);
        let body = workspace.read_with(cx, |workspace, cx| {
            workspace.review.read(cx).body_focus().clone()
        });
        let on_diff = cx.update(|window, _| body.is_focused(window));

        assert!(on_card, "the Swarm view has no focus");
        assert!(on_tile, "the Terminals view has no focus");
        assert!(on_diff, "Review has no focus");
    }

    /// Lets `time` pass on the GPUI clock and draws a frame.
    fn pass(time: Duration, cx: &mut VisualTestContext) {
        cx.executor().advance_clock(time);
        redraw(cx);
    }

    fn column_width(cx: &mut VisualTestContext) -> Pixels {
        cx.debug_bounds("sidebar-column")
            .expect("the sidebar column is on screen")
            .size
            .width
    }

    fn sidebar_motion(
        workspace: &Entity<Workspace>,
        cx: &VisualTestContext,
    ) -> Option<SidebarMotion> {
        workspace.read_with(cx, |workspace, _| workspace.sidebar_motion)
    }

    #[gpui::test]
    fn under_reduce_motion_the_sidebar_collapses_at_once(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        redraw(cx);

        workspace.update_in(cx, Workspace::toggle_sidebar);
        redraw(cx);

        assert!(cx.debug_bounds("sidebar-rail").is_some());
        assert_eq!(column_width(cx), sidebar::RAIL_WIDTH);
        assert!(sidebar_motion(&workspace, cx).is_none());
    }

    /// The opacity of the quad drawn for `selector`, which is the alpha of its fill.
    fn drawn_alpha(selector: &'static str, cx: &mut VisualTestContext) -> f32 {
        crate::daemon::tests::drawn_color(selector, cx)
            .unwrap_or_else(|| panic!("nothing is drawn for {selector}"))
            .a
    }

    #[test]
    fn the_sidebar_and_the_rail_never_show_on_top_of_each_other() {
        let steps = (0..=100u8).map(|step| f32::from(step) / 100.);
        let overlaps: Vec<f32> = steps
            .filter(|&openness| {
                let (sidebar, rail) = sidebar_and_rail_opacity(openness);
                sidebar > 0. && rail > 0.
            })
            .collect();

        assert_eq!(sidebar_and_rail_opacity(1.), (1., 0.));
        assert_eq!(sidebar_and_rail_opacity(0.), (0., 1.));
        assert!(overlaps.is_empty(), "both show at {overlaps:?}");
    }

    #[gpui::test]
    fn halfway_through_a_collapse_the_column_and_the_toggle_are_between_their_ends(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup_with_motion(cx);
        redraw(cx);
        let expanded = toggle_bounds(cx).left();
        let row_at_rest = drawn_alpha("project-row-0", cx);

        // About 10 ms in, the sidebar is three quarters open and fading; about 46 ms in, a
        // quarter open and the rail fading in.
        workspace.update_in(cx, Workspace::toggle_sidebar);
        pass(SIDEBAR_MOTION / 18, cx);
        let (width, toggle) = (column_width(cx), toggle_bounds(cx).left());
        let (rail, content) = (
            cx.debug_bounds("sidebar-rail").is_some(),
            cx.debug_bounds("add-project").is_some(),
        );
        let row = drawn_alpha("project-row-0", cx);
        pass(SIDEBAR_MOTION / 5, cx);
        let button = drawn_alpha("rail-project-0", cx);
        pass(SIDEBAR_MOTION, cx);
        let collapsed = toggle_bounds(cx).left();
        let button_at_rest = drawn_alpha("rail-project-0", cx);

        assert!(sidebar::RAIL_WIDTH < width && width < sidebar::SIDEBAR_WIDTH);
        assert!(
            collapsed < toggle && toggle < expanded,
            "{collapsed:?} {toggle:?} {expanded:?}"
        );
        assert!(rail, "the rail was not drawn during the motion");
        assert!(content, "the sidebar was not drawn during the motion");
        assert!(
            0. < row && row < row_at_rest,
            "sidebar {row} of {row_at_rest}"
        );
        assert!(
            0. < button && button < button_at_rest,
            "rail {button} of {button_at_rest}"
        );
    }

    #[gpui::test]
    fn an_error_during_a_collapse_opens_the_sidebar_and_stops_the_motion(cx: &mut TestAppContext) {
        let (workspace, cx) = setup_with_motion(cx);
        redraw(cx);

        workspace.update_in(cx, Workspace::toggle_sidebar);
        pass(SIDEBAR_MOTION / 2, cx);
        workspace.update(cx, |workspace, cx| {
            workspace.report_sidebar_error("no daemon".to_owned(), cx);
        });
        redraw(cx);

        assert!(sidebar_motion(&workspace, cx).is_none());
        assert_eq!(column_width(cx), sidebar::SIDEBAR_WIDTH);
    }

    #[gpui::test]
    fn a_running_motion_asks_for_the_next_frame(cx: &mut TestAppContext) {
        // Under Reduce Motion the spinners ask for no frames, so the motion's request stands out.
        let (workspace, cx) = setup(cx);
        redraw(cx);
        cx.update(Window::simulate_next_frame);
        redraw(cx);
        let at_rest = cx.update(Window::simulate_next_frame);

        workspace.update(cx, |workspace, cx| {
            let now = cx.background_executor().now();

            workspace.sidebar_collapsed = true;
            workspace.sidebar_motion = Some(SidebarMotion::new(1., 0., now));
            cx.notify();
        });
        redraw(cx);
        let moving = cx.update(Window::simulate_next_frame);

        assert!(moving > at_rest, "{moving} callbacks, {at_rest} at rest");
    }

    #[gpui::test]
    fn after_the_motion_the_rail_shows_and_the_motion_clears(cx: &mut TestAppContext) {
        let (workspace, cx) = setup_with_motion(cx);
        redraw(cx);

        workspace.update_in(cx, Workspace::toggle_sidebar);
        pass(SIDEBAR_MOTION / 2, cx);
        let running = sidebar_motion(&workspace, cx).is_some();
        pass(SIDEBAR_MOTION, cx);

        assert!(running);
        assert!(cx.debug_bounds("sidebar-rail").is_some());
        assert!(sidebar_motion(&workspace, cx).is_none());
        assert_eq!(column_width(cx), sidebar::RAIL_WIDTH);
        assert_eq!(
            toggle_bounds(cx).left(),
            TRAFFIC_LIGHTS_WIDTH + COLLAPSED_GAP
        );
    }

    #[gpui::test]
    fn a_second_toggle_mid_motion_reverses_from_where_it_is(cx: &mut TestAppContext) {
        let (workspace, cx) = setup_with_motion(cx);
        redraw(cx);

        workspace.update_in(cx, Workspace::toggle_sidebar);
        pass(SIDEBAR_MOTION / 2, cx);
        let openness = workspace.read_with(cx, |workspace, cx| {
            workspace.sidebar_openness(cx.background_executor().now())
        });
        workspace.update_in(cx, Workspace::toggle_sidebar);
        let motion = sidebar_motion(&workspace, cx).expect("the second toggle starts a motion");

        assert!(0. < openness && openness < 1., "openness {openness}");
        assert!((motion.from - openness).abs() < f32::EPSILON);
        assert!((motion.to - 1.).abs() < f32::EPSILON);
    }

    /// Gives the first worker a live terminal and returns the emulator's end of it.
    fn attach_worker(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> swarm_term::EmulatorChannels {
        let (channels, emulator) = swarm_term::channels();

        workspace.update_in(cx, |workspace, window, cx| {
            let terminal = workspace
                .active_task()
                .workers
                .first()
                .and_then(|id| workspace.data.agent(*id))
                .map_or(TerminalId(u32::MAX), |agent| agent.terminal);
            let live = workspace.live_terminal(
                terminal,
                swarm_protocol::TerminalId(900),
                "worker".into(),
                channels,
                window,
                cx,
            );

            workspace.live_terminals.insert(terminal, live);
            cx.notify();
        });
        redraw(cx);

        emulator
    }

    /// The grids the emulator was told about since the last call.
    fn grids(emulator: &swarm_term::EmulatorChannels) -> Vec<(u16, u16)> {
        emulator
            .input
            .try_iter()
            .filter_map(|input| match input {
                swarm_term::TerminalInput::Resize(size) => Some((size.cols, size.rows)),
                _ => None,
            })
            .collect()
    }

    #[gpui::test]
    fn the_terminals_hold_their_size_until_the_motion_ends(cx: &mut TestAppContext) {
        let (workspace, cx) = setup_with_motion(cx);
        redraw(cx);
        let emulator = attach_worker(&workspace, cx);
        let before = grids(&emulator);

        workspace.update_in(cx, Workspace::toggle_sidebar);
        for _ in 0..3 {
            pass(SIDEBAR_MOTION / 4, cx);
        }
        let during = grids(&emulator);
        pass(SIDEBAR_MOTION, cx);
        let after = grids(&emulator);
        redraw(cx);
        let settled = grids(&emulator);

        assert!(!before.is_empty(), "the terminal sent no grid at first");
        assert!(during.is_empty(), "resized during the motion: {during:?}");
        assert_eq!(after.len(), 1, "after the motion: {after:?}");
        assert_ne!(before.last(), after.last(), "the grid did not change");
        assert!(settled.is_empty(), "resized again: {settled:?}");
    }

    mod tab_walk {
        use swarm_ui::test_support::tab_walk;

        use super::*;
        use crate::live_probe::{assert_named, reversed};

        fn setup(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
            cx.update(|cx| {
                swarm_ui::init(cx);
                actions::bind_keys(cx);
                follow_test_reduce_motion(true, cx);
            });

            cx.add_window_view(Workspace::with_sample)
        }

        const OPEN: [&str; 27] = [
            "Button Toggle sidebar",
            "Tab terminal-renderer, Working",
            "Button New tab",
            "RadioButton Terminals",
            "Button Files",
            "Button Changes, 2623 additions, 394 deletions",
            "Button Search",
            "Button Add project",
            "ListItem swarm, 3 tasks, needs you",
            "ListItem orchestrator, Claude Code, working",
            "ListItem Settings",
            "ListItem Field guide, 0 of 20 caught",
            "ListItem Report an issue",
            "TextInput Message the orchestrator",
            "Group term-render terminal",
            "Button Move term-render to tray",
            "Group pty-io terminal",
            "Button Move pty-io to tray",
            "Button Review",
            "Tree Changed files",
            "Button Show ui-tokens, done",
            "Button Show bench, idle",
            "Button Show cargo test, working",
            "Button Show pnpm dev, working",
            "Button Show cargo test -p swarm-term --watch, working",
            "Button Show cargo build -p swarm-term, done",
            "Button Terminal",
        ];

        const COLLAPSED: [&str; 27] = [
            "Button Toggle sidebar",
            "Tab terminal-renderer, Working",
            "Button New tab",
            "RadioButton Terminals",
            "Button Files",
            "Button Changes, 2623 additions, 394 deletions",
            "Button Search",
            "Button swarm, 3 tasks, needs you",
            "Button Add project",
            "Button orchestrator, Claude Code, working",
            "Button Settings",
            "Button Field guide, 0 of 20 caught",
            "Button Report an issue",
            "TextInput Message the orchestrator",
            "Group term-render terminal",
            "Button Move term-render to tray",
            "Group pty-io terminal",
            "Button Move pty-io to tray",
            "Button Review",
            "Tree Changed files",
            "Button Show ui-tokens, done",
            "Button Show bench, idle",
            "Button Show cargo test, working",
            "Button Show pnpm dev, working",
            "Button Show cargo test -p swarm-term --watch, working",
            "Button Show cargo build -p swarm-term, done",
            "Button Terminal",
        ];

        /// Presses Tab once, so focus is on the first stop, then walks the whole window.
        fn walks(
            workspace: &Entity<Workspace>,
            cx: &mut VisualTestContext,
        ) -> (Vec<String>, Vec<String>) {
            redraw(cx);
            let root = workspace.read_with(cx, |workspace, _| workspace.focus_handle.clone());
            cx.update(|window, cx| window.focus(&root, cx));
            cx.simulate_keystrokes("tab");

            let forward = tab_walk(cx, &root, "tab");
            let backward = tab_walk(cx, &root, "shift-tab");

            (forward, backward)
        }

        #[gpui::test]
        fn the_main_window_tab_order_with_the_sidebar_open(cx: &mut TestAppContext) {
            let (workspace, cx) = setup(cx);
            let (forward, backward) = walks(&workspace, cx);

            assert_named(&forward);
            assert_eq!(forward, OPEN);
            assert_eq!(backward, reversed(&forward));
        }

        #[gpui::test]
        fn the_main_window_tab_order_with_the_sidebar_collapsed(cx: &mut TestAppContext) {
            let (workspace, cx) = setup(cx);
            workspace.update_in(cx, Workspace::toggle_sidebar);
            let (forward, backward) = walks(&workspace, cx);

            assert_named(&forward);
            assert_eq!(forward, COLLAPSED);
            assert_eq!(backward, reversed(&forward));
        }
    }
}
