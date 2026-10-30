//! The main window's root view.

use std::collections::{HashMap, HashSet};

use gpui::{
    Bounds, Context, Entity, FocusHandle, Focusable, Pixels, SharedString, Size, Subscription,
    Window, div, px,
};
use swarm_ui::fonts::UI_FONT;
use swarm_ui::prelude::*;

use crate::actions::{
    FocusAgent, NewTask, NewTerminal, OpenSettings, PlanWithOrchestrator, ShowReview, ShowSwarm,
    ShowTerminals, ToggleChanges, ToggleCommandPalette, ToggleFiles,
};
use crate::model::{
    AgentId, FakeData, ProjectId, Task, TaskId, TaskStatus, TerminalId, TerminalKind,
};
use crate::palette::Overlay;
use crate::views::Resizing;
use crate::{panels, sidebar, status_bar, tab_bar, views};

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

pub(crate) struct Workspace {
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
    pub(crate) layout: swarm_layout::Layout,
    pub(crate) user_sizes: HashMap<TerminalId, Size<Pixels>>,
    pub(crate) resizing: Option<Resizing>,
    pub(crate) orchestrator_input: Entity<TextInput>,
    _orchestrator_subscription: Subscription,
}

impl Workspace {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut data = FakeData::sample();

        let active_project = data
            .tasks
            .first()
            .map(|task| task.project)
            .or_else(|| data.projects.first().map(|project| project.id))
            .unwrap_or(ProjectId(0));
        let active_task = data.first_or_start_task(active_project);
        let focused_terminal = data
            .task(active_task)
            .and_then(|task| data.agent(task.orchestrator))
            .map(|agent| agent.terminal);

        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        let (orchestrator_input, orchestrator_subscription) = views::orchestrator_input(cx);

        Self {
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
            layout: swarm_layout::Layout::default(),
            user_sizes: HashMap::new(),
            resizing: None,
            orchestrator_input,
            _orchestrator_subscription: orchestrator_subscription,
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
        let task = self.data.ensure_start_task(self.active_project);

        self.overlay = None;
        self.select_task(task, cx);
    }

    pub(crate) fn select_task(&mut self, task: TaskId, cx: &mut Context<Self>) {
        let Some(entry) = self.data.task(task) else {
            return;
        };

        self.active_project = entry.project;
        self.active_task = task;
        self.focused_terminal = self
            .data
            .agent(entry.orchestrator)
            .map(|agent| agent.terminal);
        self.pending_focus = self.focused_terminal;
        cx.notify();
    }

    pub(crate) fn set_view(&mut self, view: View, cx: &mut Context<Self>) {
        self.view = view;
        cx.notify();
    }

    /// Opens the panel, or closes it when it is already open.
    pub(crate) fn toggle_panel(&mut self, panel: RightPanel, cx: &mut Context<Self>) {
        self.right_panel = if self.right_panel == Some(panel) {
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
            .is_some_and(|entry| entry.kind == TerminalKind::Orchestrator);
        let handle = if is_orchestrator {
            self.orchestrator_input.focus_handle(cx)
        } else {
            self.terminal_focus_handle(terminal, cx)
        };

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

    /// The right panel to show. A Start or Plan task has none, but the choice is kept.
    pub(crate) fn visible_panel(&self) -> Option<RightPanel> {
        if self.active_task().is_start {
            None
        } else {
            self.right_panel
        }
    }
}

static NO_TASK: Task = Task {
    id: TaskId(u32::MAX),
    project: ProjectId(u32::MAX),
    name: SharedString::new_static(""),
    branch: SharedString::new_static(""),
    status: TaskStatus::Idle,
    orchestrator: AgentId(u32::MAX),
    workers: Vec::new(),
    processes: Vec::new(),
    user_terminals: Vec::new(),
    additions: 0,
    deletions: 0,
    is_start: false,
};

impl Focusable for Workspace {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (ground, panel, line, fg) = (colors.ground, colors.panel, colors.line, colors.fg);

        self.apply_pending_focus(window, cx);
        self.ensure_focus_handles(cx);
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
                    .w(px(320.))
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
                .on_action(cx.listener(|this, action: &FocusAgent, _, cx| {
                    this.focus_agent(action.index, cx);
                }))
                .on_action(cx.listener(|this, _: &NewTask, window, cx| {
                    this.open_new_task(window, cx);
                }))
                .on_action(cx.listener(|this, _: &PlanWithOrchestrator, _, cx| {
                    this.open_start_task(cx);
                }))
                .on_action(cx.listener(|this, _: &NewTerminal, _, cx| {
                    this.new_user_terminal(cx);
                }))
                .on_action(cx.listener(|_, _: &OpenSettings, window, cx| {
                    crate::settings::open(window, cx);
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
                .pt(px(12.))
                .px(px(8.))
                .pb(px(10.))
                .child(sidebar),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .m(px(8.))
                .ml_0()
                .bg(panel)
                .border_1()
                .border_color(line)
                .rounded(px(10.))
                .overflow_hidden()
                .child(
                    div()
                        .h(px(46.))
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

        cx.add_window_view(Workspace::new)
    }

    fn orchestrator_terminal(workspace: &Workspace) -> TerminalId {
        let agent = workspace.data.agent(workspace.active_task().orchestrator);
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
}
