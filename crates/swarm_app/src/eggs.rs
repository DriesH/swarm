//! The easter eggs that need the workspace: "Honey's ready", the queen bee and the buzz.
//!
//! Each egg is off while Settings › Appearance › Easter eggs is off, and still under Reduce
//! Motion. None of them says anything to VoiceOver. The toast is not an egg: it shows with
//! the eggs off, without its honeycomb.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, Context, FocusHandle, Focusable as _, SharedString,
    Task, WeakEntity, Window, div, px,
};
use swarm_protocol::{Client, Notice, NoticeKind, SwarmStats};
use swarm_settings::ProjectMode;
use swarm_ui::prelude::*;
use swarm_ui::{BeeFlight, FLIGHT_DURATION, motion};

use crate::model::Agent;
use crate::settings;
use crate::workspace::Workspace;

/// The Start orchestrator wears the crown this long after the 100th merge.
pub(crate) const QUEEN_FOR: Duration = Duration::from_hours(24);

/// The toast goes away by itself after this long. The time stands still while the pointer or
/// the keyboard focus is on the toast.
pub(crate) const TOAST_FOR: Duration = Duration::from_secs(20);

/// How often the countdown of the toast looks at the pointer and the focus.
const TOAST_TICK: Duration = Duration::from_millis(250);

/// Space between the toast and the window edge.
const TOAST_RIGHT: f32 = 24.;
/// Clears the status bar.
const TOAST_BOTTOM: f32 = 62.;

/// The toast for a task that is ready for review.
pub(crate) struct HoneyToast {
    pub notice: Notice,
    pub task: SharedString,
    /// Counts up, so the honeycomb fills again for each toast.
    serial: usize,
    /// Tracks the focus in the whole toast.
    focus: FocusHandle,
    /// True while the pointer is on the toast.
    hovered: bool,
    _timeout: Task<()>,
}

/// A flight of bees across the window.
pub(crate) struct Flight {
    serial: usize,
    /// A snapshot holds the bees at one moment of the flight.
    #[cfg(feature = "snapshot")]
    frozen: Option<f32>,
    _landing: Task<()>,
}

/// What the eggs keep in the workspace.
#[derive(Default)]
pub(crate) struct Eggs {
    pub stats: SwarmStats,
    /// Goes up with each stats event, so an answer that came late does not win.
    stats_revision: u64,
    pub toast: Option<HoneyToast>,
    pub flight: Option<Flight>,
    serial: usize,
    /// Redraws the sidebar when the queen's day ends.
    queen_expiry: Option<Task<()>>,
}

/// True when the 100th merged pull request was less than [`QUEEN_FOR`] ago. `now` is in
/// milliseconds since the Unix epoch, like the stats.
pub(crate) fn queen_active(stats: &SwarmStats, now: u64) -> bool {
    stats.hundredth_merge_at.is_some_and(|merged| {
        let queen_for = u64::try_from(QUEEN_FOR.as_millis()).unwrap_or(u64::MAX);

        now.saturating_sub(merged) < queen_for
    })
}

#[cfg(feature = "snapshot")]
impl Workspace {
    /// Shows the toast for a task of the sample data, which the daemon does not know.
    pub(crate) fn show_toast_for_snapshot(&mut self, task: &str, cx: &mut Context<Self>) {
        let notice = Notice {
            kind: NoticeKind::ReadyForReview,
            project: swarm_protocol::ProjectId(0),
            task: None,
            agent: swarm_protocol::AgentId(0),
            text: task.to_owned(),
            at: 0,
        };

        self.place_toast(&notice, SharedString::from(task.to_owned()), None, cx);
    }

    /// Puts the bees of the buzz at `progress` of their flight and keeps them there.
    pub(crate) fn freeze_flight_for_snapshot(&mut self, progress: f32, cx: &mut Context<Self>) {
        self.eggs.flight = Some(Flight {
            serial: 0,
            frozen: Some(progress),
            _landing: Task::ready(()),
        });
        cx.notify();
    }
}

/// Runs `change` on the workspace, when it is still there.
fn update(
    workspace: &WeakEntity<Workspace>,
    change: impl FnOnce(&mut Workspace, &mut Context<Workspace>),
    cx: &mut App,
) {
    if let Err(error) = workspace.update(cx, change) {
        log::debug!("the workspace is gone: {error}");
    }
}

impl Workspace {
    /// Asks the daemon for the stats, off the main thread. An event that arrives before the
    /// answer is newer, so it wins.
    pub(crate) fn load_stats(&self, client: Client, cx: &mut Context<Self>) {
        let asked_at = self.eggs.stats_revision;

        cx.spawn(async move |this, cx| {
            let asking = cx
                .background_executor()
                .spawn(async move { client.stats() });

            let stats = match asking.await {
                Ok(stats) => stats,
                Err(error) => {
                    log::warn!("cannot read the stats: {error}");
                    return;
                }
            };

            let shown = this.update(cx, |workspace, cx| {
                if workspace.eggs.stats_revision == asked_at {
                    workspace.stats_changed(stats, cx);
                }
            });

            if let Err(error) = shown {
                log::debug!("the workspace closed before the stats came: {error}");
            }
        })
        .detach();
    }

    pub(crate) fn stats_changed(&mut self, stats: SwarmStats, cx: &mut Context<Self>) {
        self.eggs.stats = stats;
        self.eggs.stats_revision += 1;
        self.watch_queen_expiry(cx);
        cx.notify();
    }

    /// Redraws once the crown's day is over. Nothing else redraws the sidebar then.
    pub(crate) fn watch_queen_expiry(&mut self, cx: &mut Context<Self>) {
        self.eggs.queen_expiry = None;

        let Some(merged) = self.eggs.stats.hundredth_merge_at else {
            return;
        };

        let queen_for = u64::try_from(QUEEN_FOR.as_millis()).unwrap_or(u64::MAX);
        let left = merged
            .saturating_add(queen_for)
            .saturating_sub(self.now_ms());
        if left == 0 {
            return;
        }

        self.eggs.queen_expiry = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(left))
                .await;

            if let Err(error) = this.update(cx, |_, cx| cx.notify()) {
                log::debug!("the workspace closed before the crown ended: {error}");
            }
        }));
    }

    /// True for the Start orchestrator while the queen egg shows: the eggs are on and the
    /// 100th merge was less than a day ago.
    pub(crate) fn is_queen(&self, agent: &Agent, cx: &App) -> bool {
        let starts = self
            .data
            .task(agent.task)
            .is_some_and(|task| task.is_start && task.orchestrator == Some(agent.id));

        starts && settings::easter_eggs(cx) && queen_active(&self.eggs.stats, self.now_ms())
    }

    /// Shows the toast for a notice that says a task is ready for review, when the notice is
    /// about the open project and the notification settings want it.
    pub(crate) fn show_honey_toast(
        &mut self,
        notice: &Notice,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if notice.kind != NoticeKind::ReadyForReview {
            return;
        }

        let Some(task) = notice
            .task
            .and_then(|id| self.data.task_with_daemon_id(id))
            .and_then(|id| self.data.task(id))
        else {
            return;
        };

        let project = self.data.project_with_daemon_id(notice.project);
        if project != Some(self.active_project) {
            return;
        }

        let notifications = settings::notifications(cx);
        let mode = project
            .and_then(|id| self.data.project(id))
            .and_then(|project| project.root.as_deref())
            .map_or(ProjectMode::All, |root| notifications.mode(root));
        if mode != ProjectMode::All || !notifications.triggers(notice.kind) {
            return;
        }

        let name = task.name.clone();

        self.place_toast(notice, name, window, cx);
    }

    /// Puts a toast up. A toast that was up goes away, and so does the focus in it.
    fn place_toast(
        &mut self,
        notice: &Notice,
        task: SharedString,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        self.leave_toast(window, cx);

        self.eggs.serial += 1;
        let serial = self.eggs.serial;

        self.eggs.toast = Some(HoneyToast {
            notice: notice.clone(),
            task,
            serial,
            focus: cx.focus_handle(),
            hovered: false,
            _timeout: Self::count_down_toast(serial, cx),
        });
        cx.notify();
    }

    /// Waits [`TOAST_FOR`] for the toast, not counting the time the pointer or the focus is on
    /// it, and then takes it down.
    fn count_down_toast(serial: usize, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut left = TOAST_FOR;

            while !left.is_zero() {
                cx.background_executor().timer(TOAST_TICK).await;

                let held = Self::update_with_window(&this, cx, |workspace, window, cx| {
                    workspace.toast_is_held(serial, window, cx)
                })
                .unwrap_or_else(|error| {
                    log::debug!("cannot tell if the toast is held: {error}");
                    false
                });

                if !held {
                    left = left.saturating_sub(TOAST_TICK);
                }
            }

            let dismissed = Self::update_with_window(&this, cx, |workspace, window, cx| {
                if workspace.eggs.toast.as_ref().map(|toast| toast.serial) == Some(serial) {
                    workspace.dismiss_toast(window, cx);
                }
            });

            if let Err(error) = dismissed {
                log::debug!("the workspace closed before the toast timed out: {error}");
            }
        })
    }

    /// True while the pointer or the keyboard focus is on the toast with this serial.
    fn toast_is_held(&self, serial: usize, window: Option<&mut Window>, cx: &App) -> bool {
        let Some(toast) = self
            .eggs
            .toast
            .as_ref()
            .filter(|toast| toast.serial == serial)
        else {
            return false;
        };

        toast.hovered || window.is_some_and(|window| toast.focus.contains_focused(window, cx))
    }

    /// Gives the focus to the workspace when it is in the toast that goes away. Without this
    /// it falls to the window, and Tab starts from the top.
    fn leave_toast(&self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let (Some(toast), Some(window)) = (&self.eggs.toast, window) else {
            return;
        };

        if toast.focus.contains_focused(window, cx) {
            window.focus(&self.focus_handle(cx), cx);
        }
    }

    pub(crate) fn dismiss_toast(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        self.leave_toast(window, cx);

        if self.eggs.toast.take().is_some() {
            cx.notify();
        }
    }

    /// The Review button of the toast: opens the task in Review.
    fn review_from_toast(&mut self, cx: &mut Context<Self>) {
        let Some(toast) = self.eggs.toast.take() else {
            return;
        };

        self.open_notice(&toast.notice, cx);
        cx.notify();
    }

    /// Sends the bees across the window once. Nothing flies with the eggs off or under
    /// Reduce Motion: the bees have no still frame.
    pub(crate) fn start_buzz(&mut self, cx: &mut Context<Self>) {
        if !settings::easter_eggs(cx) || motion::reduce_motion(cx) {
            return;
        }

        self.eggs.serial += 1;
        let serial = self.eggs.serial;

        self.eggs.flight = Some(Flight {
            serial,
            #[cfg(feature = "snapshot")]
            frozen: None,
            _landing: cx.spawn(async move |this, cx| {
                cx.background_executor().timer(FLIGHT_DURATION).await;

                let landed = this.update(cx, |workspace, cx| {
                    if workspace.eggs.flight.as_ref().map(|flight| flight.serial) == Some(serial) {
                        workspace.eggs.flight = None;
                        cx.notify();
                    }
                });

                if let Err(error) = landed {
                    log::debug!("the workspace closed before the bees landed: {error}");
                }
            }),
        });
        cx.notify();
    }

    /// The toast and the flying bees, over everything else.
    pub(crate) fn eggs_layer(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut layer = Vec::new();

        if let Some(toast) = &self.eggs.toast {
            layer.push(self.toast_element(toast, cx));
        }

        if let Some(flight) = &self.eggs.flight {
            let bees = BeeFlight::new();

            #[cfg(feature = "snapshot")]
            let frozen = flight.frozen;
            #[cfg(not(feature = "snapshot"))]
            let frozen: Option<f32> = None;

            let bees = match frozen {
                Some(progress) => bees.progress(progress).into_any_element(),
                None => bees
                    .with_animation(
                        ("buzz", flight.serial),
                        Animation::new(FLIGHT_DURATION),
                        BeeFlight::progress,
                    )
                    .into_any_element(),
            };

            layer.push(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .child(bees)
                    .into_any_element(),
            );
        }

        layer
    }

    fn toast_element(&self, toast: &HoneyToast, cx: &mut Context<Self>) -> AnyElement {
        let workspace = cx.weak_entity();
        let dismissed = workspace.clone();
        let hover_target = workspace.clone();
        let serial = toast.serial;
        // The button that had focus goes away with the toast, and Tab needs a focus to start from.
        let review_focus = self.focus_handle(cx);
        let dismiss_focus = review_focus.clone();
        let decorated = settings::easter_eggs(cx);

        let element = Toast::new(
            ("honey-toast", toast.serial),
            toast.task.clone(),
            "Ready for review",
        )
        .debug_selector(|| "honey-toast".to_owned())
        .track_focus(&toast.focus)
        .on_hover(move |hovered, _, cx| {
            update(
                &hover_target,
                move |workspace, _| {
                    if let Some(toast) = workspace
                        .eggs
                        .toast
                        .as_mut()
                        .filter(|toast| toast.serial == serial)
                    {
                        toast.hovered = hovered;
                    }
                },
                cx,
            );
        })
        .when(decorated, |element| {
            element.leading(
                div()
                    .debug_selector(|| "honey-fill".to_owned())
                    .child(HoneyFill::new(("honey-fill", toast.serial))),
            )
        })
        .action("Review", move |_, window, cx| {
            window.focus(&review_focus, cx);
            update(&workspace, Workspace::review_from_toast, cx);
        })
        .on_dismiss(move |window, cx| {
            window.focus(&dismiss_focus, cx);
            update(
                &dismissed,
                |workspace, cx| workspace.dismiss_toast(None, cx),
                cx,
            );
        });

        div()
            .absolute()
            .right(px(TOAST_RIGHT))
            .bottom(px(TOAST_BOTTOM))
            .child(element)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext};
    use swarm_protocol::{AgentId, Event, ProjectId, ProjectInfo, Request, TaskId};

    use super::*;
    use crate::daemon::tests::{
        fake_daemon_replying, project_info, send_notice, setup, start_agent, task_info,
        use_projects, wait_until,
    };
    use crate::model::SAMPLE_NOW;
    use crate::workspace::View;

    const HOUR: u64 = 3_600_000;

    fn stats_merged_at(at: Option<u64>) -> SwarmStats {
        SwarmStats {
            running_agents: 3,
            open_tasks: 2,
            merged_prs: 100,
            hundredth_merge_at: at,
        }
    }

    fn notice(kind: NoticeKind, project: u64, task: Option<u64>) -> Notice {
        Notice {
            kind,
            project: ProjectId(project),
            task: task.map(TaskId),
            agent: AgentId(1),
            text: String::new(),
            at: 0,
        }
    }

    /// The open project "demo" has the tasks "login" and "signup"; "other" is not open.
    fn open_projects(cx: &mut TestAppContext) -> (gpui::Entity<Workspace>, &mut VisualTestContext) {
        let (workspace, cx) = setup(cx);

        use_projects(
            &workspace,
            &[
                project_info(
                    5,
                    "demo",
                    vec![task_info(7, 5, "login"), task_info(8, 5, "signup")],
                ),
                project_info(6, "other", vec![task_info(9, 6, "billing")]),
            ],
            cx,
        );

        (workspace, cx)
    }

    fn receive(workspace: &gpui::Entity<Workspace>, notice: &Notice, cx: &mut VisualTestContext) {
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.notice_received(notice, Some(window), cx);
        });
        cx.run_until_parked();
    }

    fn toast_is_up(cx: &mut VisualTestContext) -> bool {
        cx.debug_bounds("honey-toast").is_some()
    }

    /// Turns the eggs off the way the app sees it: the settings change, and the workspace
    /// follows them.
    fn eggs_off(workspace: &gpui::Entity<Workspace>, cx: &mut VisualTestContext) {
        cx.update(|_, cx| {
            settings::follow_appearance(workspace.clone(), cx);
            settings::change_appearance(|a| a.easter_eggs = false, cx);
        });
        cx.run_until_parked();
    }

    #[test]
    fn the_queen_wears_the_crown_for_a_day_after_the_hundredth_merge() {
        let merged = 1_000_000;
        let day = u64::try_from(QUEEN_FOR.as_millis()).expect("a day fits");
        let within = |now| queen_active(&stats_merged_at(Some(merged)), now);

        assert!(within(merged));
        assert!(within(merged + day - 1));
        assert!(!within(merged + day));
        assert!(!within(merged + 3 * day));
        assert!(!queen_active(&stats_merged_at(None), merged));
        assert!(!queen_active(&SwarmStats::default(), merged));
    }

    #[gpui::test]
    fn a_ready_for_review_notice_for_the_open_project_shows_the_toast(cx: &mut TestAppContext) {
        let (workspace, cx) = open_projects(cx);
        assert!(!toast_is_up(cx));

        receive(
            &workspace,
            &notice(NoticeKind::ReadyForReview, 5, Some(8)),
            cx,
        );

        assert!(toast_is_up(cx));
        assert!(cx.debug_bounds("honey-toast-action").is_some());
        assert!(cx.debug_bounds("honey-toast-dismiss").is_some());
        let task = workspace.read_with(cx, |workspace, _| {
            workspace
                .eggs
                .toast
                .as_ref()
                .map(|toast| toast.task.to_string())
        });
        assert_eq!(task.as_deref(), Some("signup"));
    }

    #[gpui::test]
    fn the_toast_comes_only_for_work_ready_in_the_open_project(cx: &mut TestAppContext) {
        let (workspace, cx) = open_projects(cx);

        receive(&workspace, &notice(NoticeKind::Prompt, 5, Some(8)), cx);
        assert!(!toast_is_up(cx));

        receive(
            &workspace,
            &notice(NoticeKind::ReadyForReview, 6, Some(9)),
            cx,
        );
        assert!(!toast_is_up(cx));

        receive(&workspace, &notice(NoticeKind::ReadyForReview, 5, None), cx);
        assert!(!toast_is_up(cx));
    }

    #[gpui::test]
    fn the_toast_follows_the_notification_settings(cx: &mut TestAppContext) {
        let (workspace, cx) = open_projects(cx);
        cx.update(|_, cx| {
            settings::change_notifications(|n| n.ready_for_review = false, cx);
        });

        receive(
            &workspace,
            &notice(NoticeKind::ReadyForReview, 5, Some(8)),
            cx,
        );

        assert!(!toast_is_up(cx));
    }

    #[gpui::test]
    fn review_opens_review_on_the_task_and_closes_the_toast(cx: &mut TestAppContext) {
        let (workspace, cx) = open_projects(cx);
        receive(
            &workspace,
            &notice(NoticeKind::ReadyForReview, 5, Some(8)),
            cx,
        );

        let review = cx.debug_bounds("honey-toast-action").expect("the button");
        cx.simulate_click(review.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        let (view, task) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.view,
                workspace
                    .active_task()
                    .daemon
                    .as_ref()
                    .map(|daemon| daemon.id),
            )
        });
        assert_eq!(view, View::Review);
        assert_eq!(task, Some(TaskId(8)));
        assert!(!toast_is_up(cx));
    }

    #[gpui::test]
    fn dismiss_and_escape_close_the_toast_and_it_goes_by_itself(cx: &mut TestAppContext) {
        let (workspace, cx) = open_projects(cx);
        let ready = notice(NoticeKind::ReadyForReview, 5, Some(8));

        receive(&workspace, &ready, cx);
        let dismiss = cx.debug_bounds("honey-toast-dismiss").expect("the button");
        cx.simulate_click(dismiss.center(), gpui::Modifiers::none());
        assert!(!toast_is_up(cx));

        receive(&workspace, &ready, cx);
        // Escape does nothing until Tab has reached the toast, the last stop in the window.
        for _ in 0..100 {
            if !toast_is_up(cx) {
                break;
            }

            cx.simulate_keystrokes("tab escape");
        }
        assert!(!toast_is_up(cx), "Tab never reached the toast");

        receive(&workspace, &ready, cx);
        cx.executor().advance_clock(TOAST_FOR);
        cx.run_until_parked();
        assert!(!toast_is_up(cx));
    }

    #[gpui::test]
    fn a_second_toast_replaces_the_first(cx: &mut TestAppContext) {
        let (workspace, cx) = open_projects(cx);

        receive(
            &workspace,
            &notice(NoticeKind::ReadyForReview, 5, Some(7)),
            cx,
        );
        receive(
            &workspace,
            &notice(NoticeKind::ReadyForReview, 5, Some(8)),
            cx,
        );

        let task = workspace.read_with(cx, |workspace, _| {
            workspace
                .eggs
                .toast
                .as_ref()
                .map(|toast| toast.task.to_string())
        });
        assert_eq!(task.as_deref(), Some("signup"));
    }

    fn focus_is_on_the_workspace(
        workspace: &gpui::Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> bool {
        let focus = workspace.read_with(cx, gpui::Focusable::focus_handle);

        cx.update(|window, _| focus.is_focused(window))
    }

    fn focus_is_in_the_toast(
        workspace: &gpui::Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> bool {
        let toast = workspace.read_with(cx, |workspace, _| {
            workspace
                .eggs
                .toast
                .as_ref()
                .map(|toast| toast.focus.clone())
        });

        toast.is_some_and(|focus| cx.update(|window, cx| focus.contains_focused(window, cx)))
    }

    /// Presses Tab until the keyboard focus is in the toast, the last stop in the window.
    fn tab_into_the_toast(workspace: &gpui::Entity<Workspace>, cx: &mut VisualTestContext) {
        for _ in 0..100 {
            if focus_is_in_the_toast(workspace, cx) {
                return;
            }

            cx.simulate_keystrokes("tab");
        }

        panic!("Tab never reached the toast");
    }

    fn pass(time: Duration, cx: &mut VisualTestContext) {
        cx.executor().advance_clock(time);
        cx.run_until_parked();
    }

    fn point_at_the_toast(cx: &mut VisualTestContext) {
        let toast = cx.debug_bounds("honey-toast").expect("the toast is up");

        cx.simulate_mouse_move(toast.center(), None, gpui::Modifiers::none());
        cx.run_until_parked();
    }

    fn point_away_from_the_toast(cx: &mut VisualTestContext) {
        cx.simulate_mouse_move(gpui::point(px(5.), px(5.)), None, gpui::Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn a_second_toast_takes_the_focus_to_the_workspace_when_it_was_in_the_first(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = open_projects(cx);
        receive(
            &workspace,
            &notice(NoticeKind::ReadyForReview, 5, Some(7)),
            cx,
        );
        tab_into_the_toast(&workspace, cx);
        assert!(!focus_is_on_the_workspace(&workspace, cx));

        receive(
            &workspace,
            &notice(NoticeKind::ReadyForReview, 5, Some(8)),
            cx,
        );

        assert!(toast_is_up(cx));
        assert!(focus_is_on_the_workspace(&workspace, cx));
    }

    #[gpui::test]
    fn a_toast_that_goes_away_takes_the_focus_to_the_workspace(cx: &mut TestAppContext) {
        let (workspace, cx) = open_projects(cx);
        receive(
            &workspace,
            &notice(NoticeKind::ReadyForReview, 5, Some(8)),
            cx,
        );
        tab_into_the_toast(&workspace, cx);

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.dismiss_toast(Some(window), cx);
        });
        cx.run_until_parked();

        assert!(!toast_is_up(cx));
        assert!(focus_is_on_the_workspace(&workspace, cx));
    }

    #[gpui::test]
    fn the_toast_waits_while_the_pointer_is_on_it(cx: &mut TestAppContext) {
        let (workspace, cx) = open_projects(cx);
        receive(
            &workspace,
            &notice(NoticeKind::ReadyForReview, 5, Some(8)),
            cx,
        );
        point_at_the_toast(cx);

        pass(TOAST_FOR * 3, cx);
        assert!(toast_is_up(cx), "the toast left with the pointer on it");

        point_away_from_the_toast(cx);
        pass(TOAST_FOR, cx);

        assert!(!toast_is_up(cx), "the toast stayed with the pointer gone");
    }

    #[gpui::test]
    fn the_toast_waits_while_the_keyboard_focus_is_in_it(cx: &mut TestAppContext) {
        let (workspace, cx) = open_projects(cx);
        receive(
            &workspace,
            &notice(NoticeKind::ReadyForReview, 5, Some(8)),
            cx,
        );
        tab_into_the_toast(&workspace, cx);

        pass(TOAST_FOR * 3, cx);
        assert!(toast_is_up(cx), "the toast left with the focus in it");
        assert!(focus_is_in_the_toast(&workspace, cx));

        workspace.update_in(cx, |workspace, window, cx| {
            window.focus(&workspace.focus_handle(cx), cx);
        });
        pass(TOAST_FOR, cx);

        assert!(!toast_is_up(cx), "the toast stayed with the focus gone");
    }

    #[gpui::test]
    fn the_countdown_goes_on_where_it_stopped(cx: &mut TestAppContext) {
        let (workspace, cx) = open_projects(cx);
        receive(
            &workspace,
            &notice(NoticeKind::ReadyForReview, 5, Some(8)),
            cx,
        );

        pass(Duration::from_secs(15), cx);
        point_at_the_toast(cx);
        pass(Duration::from_secs(60), cx);
        point_away_from_the_toast(cx);
        pass(Duration::from_millis(4_500), cx);
        assert!(toast_is_up(cx), "5 seconds were left, 4.5 have passed");

        pass(Duration::from_secs(1), cx);

        assert!(!toast_is_up(cx));
    }

    #[gpui::test]
    fn the_toast_has_a_honeycomb_with_the_eggs_on_and_none_with_them_off(cx: &mut TestAppContext) {
        let (workspace, cx) = open_projects(cx);
        let ready = notice(NoticeKind::ReadyForReview, 5, Some(8));

        receive(&workspace, &ready, cx);
        let with_eggs = cx.debug_bounds("honey-fill").is_some();

        eggs_off(&workspace, cx);
        receive(&workspace, &ready, cx);

        assert!(with_eggs);
        assert!(toast_is_up(cx), "the toast is not an egg");
        assert!(cx.debug_bounds("honey-fill").is_none());
    }

    #[gpui::test]
    fn the_honeycomb_of_the_toast_is_still_under_reduce_motion(cx: &mut TestAppContext) {
        let (workspace, cx) = open_projects(cx);
        cx.update(|_, cx| cx.set_reduce_motion(true));

        receive(
            &workspace,
            &notice(NoticeKind::ReadyForReview, 5, Some(8)),
            cx,
        );

        assert!(
            cx.debug_bounds("honey-fill").is_some(),
            "still there, and filled"
        );
        assert!(!cx.update(|_, cx| HoneyFill::animates(cx)));
    }

    fn honeycomb_shows(cx: &mut VisualTestContext) -> bool {
        cx.debug_bounds("honeycomb").is_some()
    }

    #[gpui::test]
    fn the_honeycomb_fills_the_empty_states_while_the_eggs_are_on(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        use_projects(&workspace, &[], cx);
        let no_projects = honeycomb_shows(cx);

        cx.update(|_, cx| cx.set_reduce_motion(true));
        cx.run_until_parked();
        let still = honeycomb_shows(cx);

        eggs_off(&workspace, cx);

        assert!(no_projects);
        assert!(still, "the pattern does not move");
        assert!(!honeycomb_shows(cx));
    }

    #[gpui::test]
    fn a_task_with_its_terminals_has_no_honeycomb(cx: &mut TestAppContext) {
        let (_, cx) = open_projects(cx);

        assert!(!honeycomb_shows(cx));
    }

    #[gpui::test]
    fn the_honeycomb_fills_the_start_view_of_a_project_with_no_tasks(cx: &mut TestAppContext) {
        let (workspace, cx) = on_the_start_task(cx);
        let on_start = honeycomb_shows(cx);

        eggs_off(&workspace, cx);

        assert!(on_start);
        assert!(!honeycomb_shows(cx));
    }

    #[gpui::test]
    fn the_swarm_view_shows_a_honeycomb_only_when_the_switch_and_the_eggs_are_on(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = open_projects(cx);
        workspace.update(cx, |workspace, cx| workspace.set_view(View::Swarm, cx));
        cx.update(|_, cx| settings::follow_appearance(workspace.clone(), cx));
        cx.run_until_parked();
        let background = |cx: &mut VisualTestContext| {
            (
                cx.debug_bounds("swarm-dots").is_some(),
                cx.debug_bounds("swarm-honeycomb").is_some(),
            )
        };

        let by_default = background(cx);

        cx.update(|_, cx| {
            settings::change_appearance(|a| a.swarm_view_honeycomb = true, cx);
        });
        cx.run_until_parked();
        let switched_on = background(cx);

        cx.update(|_, cx| cx.set_reduce_motion(true));
        cx.run_until_parked();
        let still = background(cx);

        cx.update(|_, cx| settings::change_appearance(|a| a.easter_eggs = false, cx));
        cx.run_until_parked();
        let eggs_off = background(cx);

        assert_eq!(by_default, (true, false));
        assert_eq!(switched_on, (false, true));
        assert_eq!(still, (false, true));
        assert_eq!(eggs_off, (true, false));
    }

    /// The project "demo" with its Start orchestrator, opened on the Start task.
    fn on_the_start_task(
        cx: &mut TestAppContext,
    ) -> (gpui::Entity<Workspace>, &mut VisualTestContext) {
        let (workspace, cx) = setup(cx);
        let project = ProjectInfo {
            start: Some(start_agent(100, 5, swarm_protocol::AgentStatus::Idle)),
            ..project_info(5, "demo", vec![task_info(7, 5, "login")])
        };

        use_projects(&workspace, &[project], cx);
        workspace.update_in(cx, |workspace, window, cx| {
            let info = start_agent(100, 5, swarm_protocol::AgentStatus::Idle);
            workspace.agent_changed(&info, Some(window), cx);
            let start = workspace.data.start_task(workspace.active_project);
            if let Some(start) = start {
                workspace.select_task(start, cx);
            }
        });
        cx.run_until_parked();

        (workspace, cx)
    }

    fn queen_crown_shows(cx: &mut VisualTestContext) -> bool {
        cx.debug_bounds("queen-bee").is_some()
    }

    fn the_hundredth_merge_was(
        workspace: &gpui::Entity<Workspace>,
        hours_ago: u64,
        cx: &mut VisualTestContext,
    ) {
        workspace.update(cx, |workspace, cx| {
            workspace.stats_changed(stats_merged_at(Some(SAMPLE_NOW - hours_ago * HOUR)), cx);
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn the_start_orchestrator_wears_the_crown_within_a_day_of_the_hundredth_merge(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = on_the_start_task(cx);
        assert!(!queen_crown_shows(cx), "no merges, no crown");

        the_hundredth_merge_was(&workspace, 23, cx);
        let within = queen_crown_shows(cx);

        the_hundredth_merge_was(&workspace, 25, cx);
        let after = queen_crown_shows(cx);

        assert!(within);
        assert!(!after);
    }

    #[gpui::test]
    fn only_the_start_orchestrator_gets_the_crown(cx: &mut TestAppContext) {
        let (workspace, cx) = on_the_start_task(cx);
        the_hundredth_merge_was(&workspace, 1, cx);
        assert!(queen_crown_shows(cx));

        let login = workspace.read_with(cx, |workspace, _| {
            workspace.data.task_with_daemon_id(TaskId(7))
        });
        workspace.update(cx, |workspace, cx| {
            if let Some(login) = login {
                workspace.select_task(login, cx);
            }
        });
        cx.run_until_parked();

        assert!(!queen_crown_shows(cx));
    }

    #[gpui::test]
    fn the_crown_is_off_with_the_eggs_off_and_still_under_reduce_motion(cx: &mut TestAppContext) {
        let (workspace, cx) = on_the_start_task(cx);
        the_hundredth_merge_was(&workspace, 1, cx);

        cx.update(|_, cx| cx.set_reduce_motion(true));
        cx.run_until_parked();
        let still = queen_crown_shows(cx);

        eggs_off(&workspace, cx);

        assert!(still, "the crown has no motion to lose");
        assert!(!queen_crown_shows(cx));
    }

    /// The accessibility role and label of an element, which the test window does not build
    /// into a tree.
    fn a11y_of<E: Element>(element: &E) -> (Option<gpui::accesskit::Role>, Option<String>) {
        let mut node = gpui::accesskit::Node::new(
            element
                .a11y_role()
                .unwrap_or(gpui::accesskit::Role::Unknown),
        );
        element.write_a11y_info(&mut node);

        (element.a11y_role(), node.label().map(str::to_owned))
    }

    #[gpui::test]
    fn the_crown_stays_silent_for_voiceover(cx: &mut TestAppContext) {
        use gpui::RenderOnce as _;

        let (workspace, cx) = on_the_start_task(cx);
        the_hundredth_merge_was(&workspace, 1, cx);
        assert!(queen_crown_shows(cx));

        let queen = workspace
            .read_with(cx, |workspace, cx| {
                workspace
                    .data
                    .agents
                    .iter()
                    .find(|agent| workspace.is_queen(agent, cx))
                    .cloned()
            })
            .expect("the Start orchestrator wears the crown");
        let row = workspace.update(cx, |workspace, cx| {
            crate::sidebar::agent_list_row(workspace, &queen, 0, cx)
        });

        let (wrapper, bee, row) = cx.update(|window, cx| {
            let bee = PixelBee::new("crown").scale(1).crown(true);

            (
                a11y_of(&crate::sidebar::queen_crown(&queen)),
                a11y_of(&bee.render(window, cx).into_element()),
                a11y_of(&row.render(window, cx).into_element()),
            )
        });

        assert_eq!(wrapper, (None, None), "the crown wrapper");
        assert_eq!(bee, (None, None), "the crown sprite");
        let label = row.1.expect("the row has a label").to_lowercase();
        assert!(
            !label.contains("queen") && !label.contains("crown"),
            "{label}"
        );
    }

    #[gpui::test]
    fn the_crown_goes_when_its_day_ends_even_if_nothing_else_redraws(cx: &mut TestAppContext) {
        let (workspace, cx) = on_the_start_task(cx);
        the_hundredth_merge_was(&workspace, 23, cx);
        assert!(queen_crown_shows(cx));

        // The clock passes the end of the day, and nothing tells the workspace.
        workspace.update(cx, |workspace, _| {
            workspace.fixed_now = Some(SAMPLE_NOW + 2 * HOUR);
        });
        cx.run_until_parked();
        assert!(queen_crown_shows(cx), "nothing has redrawn yet");

        cx.executor().advance_clock(Duration::from_millis(HOUR));
        cx.run_until_parked();

        assert!(!queen_crown_shows(cx));
    }

    #[gpui::test]
    fn the_stats_come_from_the_daemon_at_connect_and_when_they_change(cx: &mut TestAppContext) {
        let known = stats_merged_at(Some(SAMPLE_NOW - HOUR));
        let changed = SwarmStats {
            merged_prs: 101,
            ..known
        };
        let socket = fake_daemon_replying(move |request| match request {
            Request::Stats { call } => Some(Event::Stats { call, stats: known }),
            Request::Attach { .. } => Some(Event::StatsChanged { stats: changed }),
            _ => None,
        });
        let (workspace, cx) = setup(cx);
        cx.executor().allow_parking();
        let client = Client::connect(&socket).expect("connect to the fake daemon");

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client.clone(), &[], window, cx);
        });
        let asked = wait_until(cx, |cx| {
            workspace.read_with(cx, |workspace, _| workspace.eggs.stats == known)
        });
        send_notice(&client, 0);
        let told = wait_until(cx, |cx| {
            workspace.read_with(cx, |workspace, _| workspace.eggs.stats == changed)
        });

        assert!(asked, "the stats never loaded");
        assert!(told, "the change never arrived");
    }
}
