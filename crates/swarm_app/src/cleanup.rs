//! The cleanup screen: what a task's surface shows while the daemon closes the task.

use std::cell::Cell;
use std::collections::HashMap;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, Context, FocusHandle, FontWeight, Role, SharedString,
    Window, div, px,
};
use swarm_protocol::{CloseCheck, CloseFailure, CloseStep};
use swarm_ui::motion::reduce_motion;
use swarm_ui::prelude::*;

use crate::announce::announce;
use crate::close_task::describe;
use crate::model::TaskId;
use crate::report_issue::Prefill;
use crate::workspace::Workspace;

const TITLE_WORKING: &str = "Cleaning up the swarm";
const TITLE_FAILED: &str = "Cleanup stopped";
const NOT_CONNECTED: &str = "Not connected to the daemon";
/// Logical pixels for each sprite pixel: the bee is 130 pixels wide.
const BEE_SCALE: u8 = 5;
const DRIFT_PERIOD: Duration = Duration::from_millis(4800);

/// Where the bee is, as (progress, x, y) with x and y in pixels, at each quarter of its drift.
const DRIFT: [(f32, f32, f32); 5] = [
    (0., 0., 0.),
    (0.25, 18., -8.),
    (0.5, 0., -14.),
    (0.75, -18., -6.),
    (1., 0., 0.),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Working(CloseStep),
    Failed(SharedString),
}

/// How a step stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    Done,
    InProgress,
    Next,
}

impl Mark {
    fn spoken(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::InProgress => "in progress",
            Self::Next => "next",
        }
    }
}

pub(crate) fn step_label(step: CloseStep) -> &'static str {
    match step {
        CloseStep::StoppingAgents => "Stopping agents",
        CloseStep::RemovingWorktrees => "Removing worktrees",
        CloseStep::DeletingBranch => "Deleting the merged branch",
    }
}

/// Where a close stands. It knows nothing of the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Progress {
    phase: Phase,
    /// The daemon began "Deleting the merged branch" in this close.
    deletes_branch: bool,
    /// The close stopped because it would lose more than the user saw.
    stale_check: bool,
}

impl Progress {
    pub(crate) fn new() -> Self {
        Self {
            phase: Phase::Working(CloseStep::StoppingAgents),
            deletes_branch: false,
            stale_check: false,
        }
    }

    /// True when a retry has to show the user a fresh check first.
    pub(crate) fn needs_new_check(&self) -> bool {
        self.stale_check
    }

    pub(crate) fn phase(&self) -> &Phase {
        &self.phase
    }

    /// Every step of this close, in order, with how it stands.
    pub(crate) fn steps(&self) -> Vec<(CloseStep, Mark)> {
        let current = match self.phase {
            Phase::Working(step) => Some(step),
            Phase::Failed(_) => None,
        };
        let planned = [CloseStep::StoppingAgents, CloseStep::RemovingWorktrees]
            .into_iter()
            .chain(self.deletes_branch.then_some(CloseStep::DeletingBranch));

        planned
            .map(|step| {
                let mark = match current {
                    Some(now) if step < now => Mark::Done,
                    Some(now) if step == now => Mark::InProgress,
                    _ => Mark::Next,
                };

                (step, mark)
            })
            .collect()
    }

    /// The daemon began `step`. A step never goes back. After a failure it starts the
    /// screen working again, for a close that someone else retried.
    pub(crate) fn reach(&mut self, step: CloseStep) {
        if step == CloseStep::DeletingBranch {
            self.deletes_branch = true;
        }

        match &self.phase {
            Phase::Working(now) if step <= *now => {}
            _ => self.phase = Phase::Working(step),
        }
    }

    /// The close stopped. The first reason stays: the daemon's report and the answer
    /// to the request say the same thing. True when this call changed the phase.
    pub(crate) fn fail(&mut self, message: SharedString) -> bool {
        if !matches!(self.phase, Phase::Working(_)) {
            return false;
        }

        self.phase = Phase::Failed(message);

        true
    }

    /// Like [`Self::fail`], for the daemon's typed reason.
    pub(crate) fn fail_with(&mut self, reason: &CloseFailure) -> bool {
        let changed = self.fail(reason.to_string().into());
        if *reason == CloseFailure::WouldLoseMore && matches!(self.phase, Phase::Failed(_)) {
            self.stale_check = true;
        }

        changed
    }

    /// Starts again from the first step.
    pub(crate) fn restart(&mut self) {
        self.phase = Phase::Working(CloseStep::StoppingAgents);
        self.stale_check = false;
    }

    /// What a screen reader speaks for the state: the step that runs, or the reason.
    pub(crate) fn spoken(&self) -> SharedString {
        match &self.phase {
            Phase::Working(step) => step_label(*step).into(),
            Phase::Failed(message) => message.clone(),
        }
    }
}

/// Which control takes the focus the next time the screen is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusRequest {
    Screen,
    Retry,
}

/// A task that closes: its progress and the screen's controls.
pub(crate) struct Cleanup {
    pub(crate) progress: Progress,
    /// What the user confirmed. A retry sends it again; without it the retry asks first.
    confirmed: Option<CloseCheck>,
    focus_request: Cell<Option<FocusRequest>>,
    screen_focus: FocusHandle,
    retry_focus: FocusHandle,
    back_focus: FocusHandle,
    report_focus: FocusHandle,
}

impl Cleanup {
    fn new<T: 'static>(confirmed: Option<CloseCheck>, cx: &mut Context<T>) -> Self {
        Self {
            progress: Progress::new(),
            confirmed,
            focus_request: Cell::new(Some(FocusRequest::Screen)),
            screen_focus: cx.focus_handle(),
            retry_focus: cx.focus_handle().tab_stop(true),
            back_focus: cx.focus_handle().tab_stop(true),
            report_focus: cx.focus_handle().tab_stop(true),
        }
    }
}

pub(crate) type Cleanups = HashMap<TaskId, Cleanup>;

/// Where the bee is at `progress` of its drift, from 0 to 1. It eases from point to
/// point and ends where it began.
pub(crate) fn drift_at(progress: f32) -> (f32, f32) {
    let progress = progress.clamp(0., 1.);

    for pair in DRIFT.windows(2) {
        let [(start, from_x, from_y), (end, to_x, to_y)] = [pair[0], pair[1]];
        if progress > end {
            continue;
        }

        let local = (progress - start) / (end - start);
        let eased = local * local * (3. - 2. * local);

        return (
            from_x + (to_x - from_x) * eased,
            from_y + (to_y - from_y) * eased,
        );
    }

    (0., 0.)
}

impl Workspace {
    pub(crate) fn is_cleaning(&self, task: TaskId) -> bool {
        self.cleanups.contains_key(&task)
    }

    /// True while the daemon closes the task, not after the close stopped.
    pub(crate) fn cleanup_is_working(&self, task: TaskId) -> bool {
        self.cleanups
            .get(&task)
            .is_some_and(|cleanup| matches!(cleanup.progress.phase(), Phase::Working(_)))
    }

    /// Replaces the surface of the task with the cleanup screen.
    pub(crate) fn begin_cleanup(
        &mut self,
        task: TaskId,
        confirmed: Option<CloseCheck>,
        cx: &mut Context<Self>,
    ) {
        let cleanup = Cleanup::new(confirmed, cx);

        self.cleanups.insert(task, cleanup);
        cx.notify();
    }

    /// The daemon began a step of a close, also one that began with a merge.
    pub(crate) fn close_step(
        &mut self,
        daemon_task: swarm_protocol::TaskId,
        step: CloseStep,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = self.data.task_with_daemon_id(daemon_task) else {
            return;
        };

        if !self.is_cleaning(task) {
            self.begin_cleanup(task, None, cx);
        }
        if let Some(cleanup) = self.cleanups.get_mut(&task) {
            cleanup.progress.reach(step);
        }

        cx.notify();
    }

    /// The daemon could not close the task.
    pub(crate) fn close_failed(
        &mut self,
        daemon_task: swarm_protocol::TaskId,
        reason: CloseFailure,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = self.data.task_with_daemon_id(daemon_task) else {
            return;
        };

        self.cleanup_stopped(task, &reason, cx);
    }

    pub(crate) fn cleanup_failed(
        &mut self,
        task: TaskId,
        message: SharedString,
        cx: &mut Context<Self>,
    ) {
        self.cleanup_stopped(task, &CloseFailure::Other(message.to_string()), cx);
    }

    fn cleanup_stopped(&mut self, task: TaskId, reason: &CloseFailure, cx: &mut Context<Self>) {
        let Some(cleanup) = self.cleanups.get_mut(&task) else {
            return;
        };

        if cleanup.progress.fail_with(reason) {
            cleanup.focus_request.set(Some(FocusRequest::Retry));
            cx.notify();
        }
    }

    /// The daemon connection is gone: no cleanup can go on, so each one offers the way back.
    pub(crate) fn fail_working_cleanups(&mut self, cx: &mut Context<Self>) {
        let working: Vec<TaskId> = self
            .cleanups
            .iter()
            .filter(|(_, cleanup)| matches!(cleanup.progress.phase(), Phase::Working(_)))
            .map(|(task, _)| *task)
            .collect();

        for task in working {
            self.cleanup_failed(task, NOT_CONNECTED.into(), cx);
        }
    }

    /// Asks the daemon to close the task, with the check the user saw. The answer is the
    /// failure, if any: a close that works ends with the daemon's report that the task
    /// closed.
    pub(crate) fn send_close(
        &mut self,
        task: TaskId,
        daemon_id: swarm_protocol::TaskId,
        confirmed: CloseCheck,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.daemon.clone() else {
            self.cleanup_failed(task, NOT_CONNECTED.into(), cx);
            return;
        };

        let closing = cx.background_spawn(async move { client.close_task(daemon_id, confirmed) });

        cx.spawn(async move |this, cx| {
            let Err(error) = closing.await else {
                return;
            };
            log::warn!("cannot close the task: {error}");

            let updated = this.update(cx, |workspace, cx| {
                workspace.cleanup_failed(task, describe(&error), cx);
            });
            if let Err(error) = updated {
                log::debug!("the window closed before the task failed to close: {error}");
            }
        })
        .detach();
    }

    /// Tries the close again. A close that would lose more than the user saw, and a close
    /// that began with a merge, show the Close task dialog first when work would be lost.
    pub(crate) fn retry_cleanup(
        &mut self,
        task: TaskId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(daemon_id) = self.data.diff_id(task) else {
            return;
        };
        let Some(cleanup) = self.cleanups.get(&task) else {
            return;
        };

        if cleanup.progress.needs_new_check() {
            self.open_close_task(task, window, cx);
            return;
        }

        match cleanup.confirmed.clone() {
            Some(confirmed) => self.restart_close(task, daemon_id, confirmed, cx),
            None => self.check_then_close(task, daemon_id, window, cx),
        }
    }

    fn restart_close(
        &mut self,
        task: TaskId,
        daemon_id: swarm_protocol::TaskId,
        confirmed: CloseCheck,
        cx: &mut Context<Self>,
    ) {
        if let Some(cleanup) = self.cleanups.get_mut(&task) {
            cleanup.progress.restart();
            cleanup.focus_request.set(Some(FocusRequest::Screen));
        }

        cx.notify();
        self.send_close(task, daemon_id, confirmed, cx);
    }

    /// A close nobody confirmed: it sends only when the fresh check loses nothing.
    fn check_then_close(
        &mut self,
        task: TaskId,
        daemon_id: swarm_protocol::TaskId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.daemon.clone() else {
            return;
        };

        let checking = cx.background_spawn(async move { client.check_close(daemon_id) });

        cx.spawn_in(window, async move |this, cx| {
            let result = checking.await;
            let updated = this.update_in(cx, |workspace, window, cx| match result {
                Ok(check) if check.uncommitted_files > 0 || !check.unmerged_workers.is_empty() => {
                    workspace.open_close_task(task, window, cx);
                }
                Ok(check) => workspace.restart_close(task, daemon_id, check, cx),
                Err(error) => {
                    log::warn!("cannot check what closing the task does: {error}");
                    workspace.cleanup_failed(task, describe(&error), cx);
                }
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the close check ended: {error}");
            }
        })
        .detach();
    }

    /// Leaves the screen. The task stays open.
    pub(crate) fn back_to_task(&mut self, task: TaskId, cx: &mut Context<Self>) {
        if self.cleanups.remove(&task).is_some() {
            self.select_task(task, cx);
        }
    }

    /// Opens the report with the reason the close stopped.
    pub(crate) fn report_cleanup_failure(
        &mut self,
        task: TaskId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let reason = self
            .cleanups
            .get(&task)
            .map(|cleanup| cleanup.progress.phase().clone());
        let Some(Phase::Failed(message)) = reason else {
            return;
        };

        self.open_report_issue(Some(Prefill::bug(TITLE_FAILED, &message)), window, cx);
    }
}

/// The screen of the open task, when it closes.
pub(crate) fn screen(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Option<AnyElement> {
    let task = workspace.active_task;
    let cleanup = workspace.cleanups.get(&task)?;

    let panel = cx.theme().colors().panel;
    let failed = matches!(cleanup.progress.phase(), Phase::Failed(_));
    let title = if failed { TITLE_FAILED } else { TITLE_WORKING };

    match cleanup.focus_request.take() {
        Some(FocusRequest::Screen) => window.focus(&cleanup.screen_focus, cx),
        Some(FocusRequest::Retry) => window.focus(&cleanup.retry_focus, cx),
        None => {}
    }

    let bee = if failed || reduce_motion(cx) {
        PixelBee::new("cleanup-bee")
            .scale(BEE_SCALE)
            .into_any_element()
    } else {
        div()
            .debug_selector(|| "cleanup-bee-drift".to_owned())
            .child(PixelBee::new("cleanup-bee").scale(BEE_SCALE).flap(true))
            .with_animation(
                "cleanup-drift",
                Animation::new(DRIFT_PERIOD).repeat(),
                |bee, progress| {
                    let (x, y) = drift_at(progress);

                    bee.relative().left(px(x)).top(px(y))
                },
            )
            .into_any_element()
    };

    let body = match cleanup.progress.phase() {
        Phase::Working(_) => steps_list(&cleanup.progress).into_any_element(),
        Phase::Failed(message) => failure(cleanup, task, message, cx).into_any_element(),
    };

    let honeycomb = div()
        .debug_selector(|| "cleanup-honeycomb".to_owned())
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .child(workspace.honeycomb.clone().cached(Honeycomb::cache_style()));

    let region = if failed {
        LiveRegion::alert("cleanup-alert-region", cleanup.progress.spoken())
    } else {
        LiveRegion::status("cleanup-status-region", cleanup.progress.spoken())
    };

    Some(
        div()
            .id("cleanup")
            .role(Role::Main)
            .aria_label(title)
            .debug_selector(|| "cleanup".to_owned())
            .track_focus(&cleanup.screen_focus)
            .relative()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .bg(panel)
            .child(honeycomb)
            .child(
                v_flex()
                    .relative()
                    .items_center()
                    .gap(px(22.))
                    .child(bee)
                    .child(
                        Label::new(title)
                            .heading("cleanup-title", 1)
                            .size(LabelSize::Title)
                            .weight(FontWeight::BOLD),
                    )
                    .child(body),
            )
            .child(announce("cleanup-status", region))
            .into_any_element(),
    )
}

fn steps_list(progress: &Progress) -> impl IntoElement + use<> {
    v_flex()
        .id("cleanup-steps")
        .role(Role::List)
        .aria_label("Steps")
        .items_start()
        .gap(px(10.))
        .children(
            progress
                .steps()
                .into_iter()
                .enumerate()
                .map(|(index, (step, mark))| step_row(index, step, mark)),
        )
}

fn step_row(index: usize, step: CloseStep, mark: Mark) -> impl IntoElement + use<> {
    let label = step_label(step);

    let icon: AnyElement = match mark {
        Mark::Done => Icon::new(IconName::Check)
            .color(Color::Ok)
            .into_any_element(),
        Mark::InProgress => StatusRing::new(AgentStatus::Working)
            .size(px(14.))
            .into_any_element(),
        Mark::Next => StatusRing::new(AgentStatus::Idle)
            .size(px(14.))
            .into_any_element(),
    };
    let (color, weight) = match mark {
        Mark::Done => (Color::Muted, FontWeight::NORMAL),
        Mark::InProgress => (Color::Default, FontWeight::MEDIUM),
        Mark::Next => (Color::Faint, FontWeight::NORMAL),
    };

    h_flex()
        .id(("cleanup-step", index))
        .role(Role::ListItem)
        .aria_label(format!("{label}, {}", mark.spoken()))
        .debug_selector(move || format!("cleanup-step-{index}"))
        .gap(px(10.))
        .child(icon)
        .child(Label::new(label).color(color).weight(weight))
}

fn failure(
    cleanup: &Cleanup,
    task: TaskId,
    message: &SharedString,
    cx: &mut Context<Workspace>,
) -> impl IntoElement + use<> {
    v_flex()
        .items_center()
        .gap(px(14.))
        .child(
            div()
                .id("cleanup-error")
                .role(Role::Group)
                .aria_label(message.clone())
                .max_w(px(560.))
                .child(
                    Label::new(message.clone())
                        .mono()
                        .size(LabelSize::Small)
                        .color(Color::Del),
                ),
        )
        .child(
            h_flex()
                .gap(px(10.))
                .child(
                    Button::new("cleanup-retry", "Retry")
                        .variant(ButtonVariant::Primary)
                        .track_focus(&cleanup.retry_focus)
                        .debug_selector(|| "cleanup-retry".to_owned())
                        .on_click(cx.listener(move |workspace, _, window, cx| {
                            workspace.retry_cleanup(task, window, cx);
                        })),
                )
                .child(
                    Button::new("cleanup-back", "Back to the task")
                        .variant(ButtonVariant::Secondary)
                        .track_focus(&cleanup.back_focus)
                        .debug_selector(|| "cleanup-back".to_owned())
                        .on_click(cx.listener(move |workspace, _, _, cx| {
                            workspace.back_to_task(task, cx);
                        })),
                )
                .child(
                    Button::new("cleanup-report", "Report an issue")
                        .variant(ButtonVariant::Ghost)
                        .track_focus(&cleanup.report_focus)
                        .debug_selector(|| "cleanup-report".to_owned())
                        .on_click(cx.listener(move |workspace, _, window, cx| {
                            workspace.report_cleanup_failure(task, window, cx);
                        })),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks(progress: &Progress) -> Vec<Mark> {
        progress.steps().into_iter().map(|(_, mark)| mark).collect()
    }

    #[test]
    fn a_new_close_starts_with_stopping_agents_in_progress() {
        let progress = Progress::new();

        assert_eq!(marks(&progress), [Mark::InProgress, Mark::Next]);
    }

    #[test]
    fn each_step_marks_the_ones_before_it_done() {
        let mut progress = Progress::new();

        progress.reach(CloseStep::RemovingWorktrees);

        assert_eq!(marks(&progress), [Mark::Done, Mark::InProgress]);
    }

    #[test]
    fn the_branch_step_is_left_out_until_the_daemon_begins_it() {
        let progress = Progress::new();

        let steps: Vec<CloseStep> = progress.steps().into_iter().map(|(step, _)| step).collect();

        assert_eq!(
            steps,
            [CloseStep::StoppingAgents, CloseStep::RemovingWorktrees]
        );
    }

    #[test]
    fn a_reported_branch_step_joins_the_list() {
        let mut progress = Progress::new();

        progress.reach(CloseStep::DeletingBranch);

        assert_eq!(marks(&progress), [Mark::Done, Mark::Done, Mark::InProgress]);
    }

    #[test]
    fn a_step_never_goes_back() {
        let mut progress = Progress::new();
        progress.reach(CloseStep::DeletingBranch);

        progress.reach(CloseStep::StoppingAgents);

        assert_eq!(progress.phase(), &Phase::Working(CloseStep::DeletingBranch));
    }

    #[test]
    fn the_first_failure_stays() {
        let mut progress = Progress::new();

        let first = progress.fail("first".into());
        let second = progress.fail("second".into());

        assert!(first);
        assert!(!second);
        assert_eq!(progress.phase(), &Phase::Failed("first".into()));
    }

    #[test]
    fn a_failed_close_shows_no_step_as_running() {
        let mut progress = Progress::new();
        progress.fail("busy".into());

        assert!(marks(&progress).iter().all(|mark| *mark == Mark::Next));
    }

    #[test]
    fn a_retry_starts_from_the_first_step() {
        let mut progress = Progress::new();
        progress.reach(CloseStep::RemovingWorktrees);
        progress.fail("busy".into());

        progress.restart();

        assert_eq!(progress.phase(), &Phase::Working(CloseStep::StoppingAgents));
    }

    #[test]
    fn a_step_after_a_failure_starts_the_screen_working_again() {
        let mut progress = Progress::new();
        progress.fail("busy".into());

        progress.reach(CloseStep::RemovingWorktrees);

        assert_eq!(
            progress.phase(),
            &Phase::Working(CloseStep::RemovingWorktrees)
        );
    }

    #[test]
    fn the_screen_reader_hears_the_step_that_runs_and_then_the_reason() {
        let mut progress = Progress::new();
        progress.reach(CloseStep::RemovingWorktrees);
        let step = progress.spoken();
        progress.fail("worktree is dirty".into());

        assert_eq!(step, "Removing worktrees");
        assert_eq!(progress.spoken(), "worktree is dirty");
    }

    #[test]
    fn a_close_that_would_lose_more_asks_for_a_new_check_on_retry() {
        let mut progress = Progress::new();

        progress.fail_with(&CloseFailure::WouldLoseMore);

        assert!(progress.needs_new_check());
    }

    #[test]
    fn any_other_failure_retries_without_a_new_check() {
        let mut progress = Progress::new();

        progress.fail_with(&CloseFailure::Other("busy".to_owned()));

        assert!(!progress.needs_new_check());
    }

    #[test]
    fn a_retry_forgets_the_stale_check() {
        let mut progress = Progress::new();
        progress.fail_with(&CloseFailure::WouldLoseMore);

        progress.restart();

        assert!(!progress.needs_new_check());
    }

    #[test]
    fn the_bee_ends_each_drift_where_it_began() {
        assert_eq!(drift_at(0.), (0., 0.));
        assert_eq!(drift_at(1.), (0., 0.));
    }

    #[test]
    fn the_bee_reaches_the_points_of_the_drift_on_the_quarters() {
        assert_eq!(drift_at(0.25), (18., -8.));
        assert_eq!(drift_at(0.75), (-18., -6.));
    }

    #[test]
    fn the_bee_is_between_two_points_between_the_quarters() {
        let (x, y) = drift_at(0.125);

        assert!(x > 0. && x < 18.);
        assert!(y < 0. && y > -8.);
    }
}

#[cfg(test)]
mod screen_tests {
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use swarm_protocol::{Event, PrEvent, Request, TaskId as DaemonTaskId};

    use super::*;
    use crate::daemon::tests::{
        fake_daemon_replying, real_task_workspace, redraw, task_info, wait_until,
    };
    use crate::palette::Overlay;
    use crate::report_issue::Kind;

    fn workspace_with_task(
        cx: &mut TestAppContext,
    ) -> (
        Entity<Workspace>,
        &mut VisualTestContext,
        swarm_protocol::Client,
    ) {
        let socket = fake_daemon_replying(|_| None);

        real_task_workspace(&socket, cx)
    }

    fn active_task(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> TaskId {
        workspace.read_with(cx, |workspace, _| workspace.active_task)
    }

    fn phase(workspace: &Entity<Workspace>, task: TaskId, cx: &VisualTestContext) -> Option<Phase> {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .cleanups
                .get(&task)
                .map(|cleanup| cleanup.progress.phase().clone())
        })
    }

    #[gpui::test]
    fn a_report_that_the_close_started_replaces_the_surface_at_once(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = workspace_with_task(cx);
        let task = active_task(&workspace, cx);
        redraw(cx);
        let before = cx.debug_bounds("cleanup").is_some();

        workspace.update(cx, |workspace, cx| {
            workspace.pr_event(
                PrEvent::CloseStep {
                    task: DaemonTaskId(7),
                    step: CloseStep::StoppingAgents,
                },
                cx,
            );
        });
        redraw(cx);

        assert!(!before);
        assert!(cx.debug_bounds("cleanup").is_some());
        assert_eq!(
            phase(&workspace, task, cx),
            Some(Phase::Working(CloseStep::StoppingAgents))
        );
    }

    #[gpui::test]
    fn the_branch_step_joins_the_list_only_after_the_daemon_begins_it(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = workspace_with_task(cx);
        let task = active_task(&workspace, cx);

        let count = |workspace: &Entity<Workspace>, cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, _| {
                workspace.cleanups[&task].progress.steps().len()
            })
        };
        workspace.update(cx, |workspace, cx| {
            workspace.close_step(DaemonTaskId(7), CloseStep::StoppingAgents, cx);
        });
        let before = count(&workspace, cx);
        workspace.update(cx, |workspace, cx| {
            workspace.close_step(DaemonTaskId(7), CloseStep::DeletingBranch, cx);
        });

        assert_eq!(before, 2);
        assert_eq!(count(&workspace, cx), 3);
    }

    #[gpui::test]
    fn a_close_that_confirmed_a_merged_branch_lists_no_branch_step_at_first(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx, _client) = workspace_with_task(cx);
        let task = active_task(&workspace, cx);
        let confirmed = CloseCheck {
            merged_branches: vec!["swarm/login".to_owned()],
            ..CloseCheck::default()
        };

        workspace.update(cx, |workspace, cx| {
            workspace.begin_cleanup(task, Some(confirmed), cx);
        });

        let steps = workspace.read_with(cx, |workspace, _| {
            workspace.cleanups[&task].progress.steps().len()
        });
        assert_eq!(steps, 2);
    }

    #[gpui::test]
    fn the_steps_of_a_close_after_a_merge_are_listed_as_the_daemon_reports_them(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx, _client) = workspace_with_task(cx);
        let task = active_task(&workspace, cx);
        workspace.update(cx, |workspace, cx| {
            workspace.close_step(DaemonTaskId(7), CloseStep::StoppingAgents, cx);
            workspace.close_step(DaemonTaskId(7), CloseStep::DeletingBranch, cx);
        });

        let steps = workspace.read_with(cx, |workspace, _| {
            workspace.cleanups[&task]
                .progress
                .steps()
                .into_iter()
                .map(|(step, _)| step)
                .collect::<Vec<_>>()
        });
        assert_eq!(
            steps,
            [
                CloseStep::StoppingAgents,
                CloseStep::RemovingWorktrees,
                CloseStep::DeletingBranch
            ]
        );
    }

    #[gpui::test]
    fn each_step_the_daemon_reports_moves_the_marks_on_the_screen(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = workspace_with_task(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.close_step(DaemonTaskId(7), CloseStep::StoppingAgents, cx);
            workspace.close_step(DaemonTaskId(7), CloseStep::RemovingWorktrees, cx);
        });
        redraw(cx);

        let task = active_task(&workspace, cx);
        let spoken = workspace.read_with(cx, |workspace, _| {
            workspace.cleanups[&task].progress.spoken()
        });
        assert_eq!(spoken, "Removing worktrees");
        assert!(cx.debug_bounds("cleanup-step-0").is_some());
        assert!(cx.debug_bounds("cleanup-step-1").is_some());
        assert!(cx.debug_bounds("cleanup-step-2").is_none());
    }

    #[gpui::test]
    fn a_failure_shows_retry_back_and_report_and_the_report_has_the_error(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = workspace_with_task(cx);
        let task = active_task(&workspace, cx);
        workspace.update(cx, |workspace, cx| {
            workspace.close_step(DaemonTaskId(7), CloseStep::RemovingWorktrees, cx);
            workspace.close_failed(
                DaemonTaskId(7),
                CloseFailure::Other("worktree has changes".to_owned()),
                cx,
            );
        });
        redraw(cx);
        let buttons = [
            cx.debug_bounds("cleanup-retry").is_some(),
            cx.debug_bounds("cleanup-back").is_some(),
            cx.debug_bounds("cleanup-report").is_some(),
        ];

        let center = cx
            .debug_bounds("cleanup-report")
            .map_or_else(Default::default, |bounds| bounds.center());
        cx.simulate_click(center, gpui::Modifiers::none());
        redraw(cx);

        let draft = workspace.read_with(cx, |workspace, cx| match &workspace.overlay {
            Some(Overlay::ReportIssue { view, .. }) => Some(view.read(cx).draft(cx)),
            _ => None,
        });
        assert_eq!(buttons, [true; 3]);
        assert_eq!(
            phase(&workspace, task, cx),
            Some(Phase::Failed("worktree has changes".into()))
        );
        let draft = draft.expect("the report is open");
        assert_eq!(draft.kind, Kind::Bug);
        assert_eq!(draft.title, "Cleanup stopped");
        assert_eq!(draft.description, "worktree has changes");
    }

    #[gpui::test]
    fn focus_moves_to_the_screen_and_then_to_retry_when_the_close_fails(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = workspace_with_task(cx);
        let task = active_task(&workspace, cx);
        workspace.update(cx, |workspace, cx| {
            workspace.close_step(DaemonTaskId(7), CloseStep::StoppingAgents, cx);
        });
        redraw(cx);
        let (screen, retry) = workspace.read_with(cx, |workspace, _| {
            let cleanup = &workspace.cleanups[&task];

            (cleanup.screen_focus.clone(), cleanup.retry_focus.clone())
        });
        let on_screen = cx.update(|window, _| screen.is_focused(window));

        workspace.update(cx, |workspace, cx| {
            workspace.close_failed(DaemonTaskId(7), CloseFailure::Other("busy".to_owned()), cx);
        });
        redraw(cx);

        assert!(on_screen);
        assert!(cx.update(|window, _| retry.is_focused(window)));
    }

    #[gpui::test]
    fn the_first_reason_stays_when_the_request_answers_after_the_report(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = workspace_with_task(cx);
        let task = active_task(&workspace, cx);
        workspace.update(cx, |workspace, cx| {
            workspace.close_step(DaemonTaskId(7), CloseStep::StoppingAgents, cx);
            workspace.close_failed(DaemonTaskId(7), CloseFailure::Other("first".to_owned()), cx);
            workspace.cleanup_failed(task, "second".into(), cx);
        });

        assert_eq!(
            phase(&workspace, task, cx),
            Some(Phase::Failed("first".into()))
        );
    }

    #[gpui::test]
    fn a_task_that_the_daemon_reports_closed_drops_the_screen(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = workspace_with_task(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.close_step(DaemonTaskId(7), CloseStep::StoppingAgents, cx);
        });
        let mut gone = task_info(7, 5, "login");
        gone.closed = true;

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.task_changed(&gone, Some(window), cx);
        });
        redraw(cx);

        assert!(workspace.read_with(cx, |workspace, _| workspace.cleanups.is_empty()));
        assert!(cx.debug_bounds("cleanup").is_none());
    }

    fn socket_with_check(check: CloseCheck) -> (std::path::PathBuf, flume::Receiver<CloseCheck>) {
        let (sent, closes) = flume::unbounded();
        let socket = fake_daemon_replying(move |request| match request {
            Request::CheckClose { call, .. } => Some(Event::CloseCheck {
                call,
                check: check.clone(),
            }),
            Request::CloseTask {
                call, confirmed, ..
            } => {
                sent.send(confirmed).ok();

                Some(Event::Done { call })
            }
            _ => None,
        });

        (socket, closes)
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let center = cx
            .debug_bounds(selector)
            .map_or_else(Default::default, |bounds| bounds.center());

        cx.simulate_click(center, gpui::Modifiers::none());
    }

    fn fail_and_click_retry(
        workspace: &Entity<Workspace>,
        reason: CloseFailure,
        cx: &mut VisualTestContext,
    ) {
        workspace.update(cx, |workspace, cx| {
            workspace.close_step(DaemonTaskId(7), CloseStep::StoppingAgents, cx);
            workspace.close_failed(DaemonTaskId(7), reason, cx);
        });
        redraw(cx);

        click("cleanup-retry", cx);
    }

    fn dialog_is_open(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> bool {
        workspace.read_with(cx, |workspace, _| {
            matches!(workspace.overlay, Some(Overlay::CloseTask { .. }))
        })
    }

    #[gpui::test]
    fn retrying_a_close_from_a_merge_with_work_to_lose_opens_the_dialog_and_sends_nothing(
        cx: &mut TestAppContext,
    ) {
        let (socket, closes) = socket_with_check(CloseCheck {
            uncommitted_files: 2,
            ..CloseCheck::default()
        });
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);

        fail_and_click_retry(&workspace, CloseFailure::Other("busy".to_owned()), cx);
        let opened = wait_until(cx, |cx| {
            redraw(cx);
            dialog_is_open(&workspace, cx)
        });

        assert!(opened);
        assert!(closes.try_recv().is_err(), "the close was sent");
    }

    #[gpui::test]
    fn retrying_a_close_from_a_merge_with_nothing_to_lose_sends_the_fresh_check(
        cx: &mut TestAppContext,
    ) {
        let (socket, closes) = socket_with_check(CloseCheck::default());
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);

        fail_and_click_retry(&workspace, CloseFailure::Other("busy".to_owned()), cx);
        let sent = wait_until(cx, |_| !closes.is_empty());

        assert!(sent);
        assert!(!dialog_is_open(&workspace, cx));
    }

    #[gpui::test]
    fn retrying_after_the_daemon_found_more_to_lose_opens_the_dialog_with_a_fresh_check(
        cx: &mut TestAppContext,
    ) {
        let (socket, closes) = socket_with_check(CloseCheck::default());
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);
        let task = active_task(&workspace, cx);
        let confirmed = CloseCheck::default();
        workspace.update(cx, |workspace, cx| {
            workspace.begin_cleanup(task, Some(confirmed), cx);
            workspace.close_failed(DaemonTaskId(7), CloseFailure::WouldLoseMore, cx);
        });
        redraw(cx);

        click("cleanup-retry", cx);
        let opened = wait_until(cx, |cx| {
            redraw(cx);
            dialog_is_open(&workspace, cx)
        });

        assert!(opened);
        assert!(closes.try_recv().is_err(), "the old check was sent again");
    }

    #[gpui::test]
    fn the_close_dialog_does_not_open_over_a_cleanup_that_is_working(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = workspace_with_task(cx);
        let task = active_task(&workspace, cx);
        workspace.update(cx, |workspace, cx| {
            workspace.close_step(DaemonTaskId(7), CloseStep::StoppingAgents, cx);
        });

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_close_task(task, window, cx);
        });

        assert!(!dialog_is_open(&workspace, cx));
    }

    #[gpui::test]
    fn the_bee_does_not_drift_under_reduce_motion(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = workspace_with_task(cx);
        cx.update(|_, cx| crate::workspace::follow_test_reduce_motion(true, cx));
        workspace.update(cx, |workspace, cx| {
            workspace.close_step(DaemonTaskId(7), CloseStep::StoppingAgents, cx);
        });

        redraw(cx);

        assert!(cx.debug_bounds("cleanup").is_some());
        assert!(cx.debug_bounds("cleanup-bee-drift").is_none());
    }

    #[gpui::test]
    fn the_bee_drifts_while_motion_is_allowed(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = workspace_with_task(cx);
        cx.update(|_, cx| crate::workspace::follow_test_reduce_motion(false, cx));
        workspace.update(cx, |workspace, cx| {
            workspace.close_step(DaemonTaskId(7), CloseStep::StoppingAgents, cx);
        });

        redraw(cx);

        assert!(cx.debug_bounds("cleanup-bee-drift").is_some());
    }

    #[gpui::test]
    fn the_screen_keeps_the_tab_bar_usable(cx: &mut TestAppContext) {
        let (workspace, cx, _client) = workspace_with_task(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.close_step(DaemonTaskId(7), CloseStep::StoppingAgents, cx);
        });
        redraw(cx);

        assert!(cx.debug_bounds("task-tabs").is_some());
        assert!(wait_until(cx, |cx| cx.debug_bounds("cleanup").is_some()));
    }
}
