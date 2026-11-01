//! The menu bar: the bee with the highest usage percent and a dot while an agent needs the
//! user, and a popover under it.
//!
//! All of the logic runs without `AppKit`. The status item sits behind [`StatusItem`], and the
//! real one is never made in tests or in a snapshot build.

#[cfg(target_os = "macos")]
mod appkit;
mod icon;
mod place;
mod popover;
#[cfg(feature = "snapshot")]
pub(crate) mod snapshot;
mod summary;

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext as _, Bounds, Entity, Global, Pixels, Size, Task, WindowBackgroundAppearance,
    WindowBounds, WindowHandle, WindowId, WindowKind, WindowOptions, point, px, size,
};

use self::place::Anchor;
use self::popover::Popover;
pub(crate) use self::popover::usage_section;
use self::summary::{BeeFrame, Summary};
pub(crate) use self::summary::{Look, usage_sections};
use crate::workspace::Workspace;

/// A click on the status item blurs the popover first, and that closes it. Without this wait
/// the same click would open it again.
const REOPEN_GUARD: Duration = Duration::from_millis(300);

/// Space between the popover and the corner, when the status item has no position.
const CORNER_GAP: f32 = 8.;

/// How often the look is worked out again. A usage window that resets changes the percent
/// with no news from the daemon.
const LOOK_INTERVAL: Duration = Duration::from_secs(60);

/// How long each wing frame shows while agents work.
const FLAP_INTERVAL: Duration = Duration::from_millis(130);

/// What the menu bar needs from the status item.
pub(crate) trait StatusItem {
    fn show(&self, look: &Look);

    /// The button's frame and its screen. `None` while the item has no window.
    fn anchor(&self) -> Option<Anchor>;
}

/// Remembers when the popover closed, to ignore the click that closed it.
#[derive(Default)]
struct ReopenGuard {
    closed_at: Option<Instant>,
}

impl ReopenGuard {
    fn closed(&mut self, at: Instant) {
        self.closed_at = Some(at);
    }

    fn allows(&self, now: Instant) -> bool {
        self.closed_at
            .is_none_or(|closed| now.saturating_duration_since(closed) >= REOPEN_GUARD)
    }
}

struct MenuBar {
    workspace: Entity<Workspace>,
    status: Option<Rc<dyn StatusItem>>,
    shown: Option<Look>,
    popover: Option<WindowHandle<Popover>>,
    guard: ReopenGuard,
    /// The wing frame the bee shows. Only the flapping changes it from `Open`.
    frame: BeeFrame,
    /// Flips the frame while agents work.
    flap: Option<Task<()>>,
}

impl Global for MenuBar {}

/// Puts the bee in the menu bar and keeps it current. `workspace` is the model it reads.
pub(crate) fn install(workspace: Entity<Workspace>, cx: &mut App) {
    let (clicks, receiver) = flume::unbounded();

    start(workspace, create_status_item(clicks), receiver, cx);
}

/// The `AppKit` item. Never made in tests or in a snapshot build.
#[cfg(target_os = "macos")]
fn create_status_item(clicks: flume::Sender<()>) -> Option<Rc<dyn StatusItem>> {
    if cfg!(any(test, feature = "snapshot")) {
        return None;
    }

    appkit::create(clicks)
}

#[cfg(not(target_os = "macos"))]
fn create_status_item(_clicks: flume::Sender<()>) -> Option<Rc<dyn StatusItem>> {
    None
}

fn start(
    workspace: Entity<Workspace>,
    status: Option<Rc<dyn StatusItem>>,
    clicks: flume::Receiver<()>,
    cx: &mut App,
) {
    popover::bind_keys(cx);
    cx.set_global(MenuBar {
        workspace: workspace.clone(),
        status,
        shown: None,
        popover: None,
        guard: ReopenGuard::default(),
        frame: BeeFrame::Open,
        flap: None,
    });

    cx.observe(&workspace, |_, cx| refresh(cx)).detach();
    let settings = crate::settings::state(cx);
    cx.observe(&settings, |_, cx| refresh(cx)).detach();
    cx.on_window_closed(window_closed).detach();

    // AppKit calls back on its own terms; the click goes through a channel to this task.
    cx.spawn(async move |cx| {
        while clicks.recv_async().await.is_ok() {
            cx.update(toggle_popover);
        }
    })
    .detach();

    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(LOOK_INTERVAL).await;
            cx.update(refresh);
        }
    })
    .detach();

    refresh(cx);
}

/// True when the status item is in the menu bar. Without it the app must keep its Dock
/// icon, or it would have no way back to its window.
pub(crate) fn has_status_item(cx: &App) -> bool {
    cx.try_global::<MenuBar>()
        .is_some_and(|menu| menu.status.is_some())
}

fn summary(cx: &App) -> Summary {
    let workspace = cx.global::<MenuBar>().workspace.read(cx);

    Summary::of(&workspace.data, workspace.now_ms() / 1_000)
}

/// Shows the status item's new look, when it changed.
fn refresh(cx: &mut App) {
    let dot_allowed = crate::settings::notifications(cx).menu_bar_dot;
    let summary = summary(cx);

    // An egg: off with the switch, and still under Reduce Motion.
    let buzzing = summary.working > 0
        && crate::settings::easter_eggs(cx)
        && !swarm_ui::motion::reduce_motion(cx);
    set_buzzing(buzzing, cx);

    let frame = cx.global::<MenuBar>().frame;
    let look = summary.look(dot_allowed).with_frame(frame);
    let menu = cx.global_mut::<MenuBar>();
    if menu.shown.as_ref() == Some(&look) {
        return;
    }

    if let Some(status) = &menu.status {
        status.show(&look);
    }
    menu.shown = Some(look);
}

/// Starts the flapping while agents work. When they stop, it ends with the wings open.
fn set_buzzing(buzzing: bool, cx: &mut App) {
    if !buzzing {
        let menu = cx.global_mut::<MenuBar>();
        menu.flap = None;
        menu.frame = BeeFrame::Open;

        return;
    }

    if cx.global::<MenuBar>().flap.is_some() {
        return;
    }

    let flapping = cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(FLAP_INTERVAL).await;
            cx.update(flap);
        }
    });
    cx.global_mut::<MenuBar>().flap = Some(flapping);
}

/// One flap. `refresh` ends the flapping when the agents stopped or motion is reduced.
fn flap(cx: &mut App) {
    let menu = cx.global_mut::<MenuBar>();
    menu.frame = menu.frame.flipped();

    refresh(cx);
}

fn window_closed(cx: &mut App, window: WindowId) {
    let menu = cx.global_mut::<MenuBar>();

    if menu
        .popover
        .is_some_and(|popover| popover.window_id() == window)
    {
        menu.popover = None;
        menu.guard.closed(Instant::now());
    }
}

/// A click on the status item opens the popover, or closes it when it is open.
fn toggle_popover(cx: &mut App) {
    if let Some(popover) = cx.global::<MenuBar>().popover {
        if let Err(error) = popover.update(cx, |_, window, _| window.remove_window()) {
            log::debug!("the popover is already gone: {error}");
        }

        return;
    }

    if cx.global::<MenuBar>().guard.allows(Instant::now()) {
        open_popover(cx);
    }
}

fn open_popover(cx: &mut App) {
    let menu = cx.global::<MenuBar>();
    let workspace = menu.workspace.clone();
    let anchor = menu.status.as_ref().and_then(|status| status.anchor());

    let wanted = size(px(popover::WIDTH), px(popover::height(&summary(cx))));
    let (bounds, display_id) = match anchor {
        Some(anchor) => {
            let placed = place::below(&anchor, wanted);

            (placed.bounds, Some(placed.display))
        }
        None => (in_corner(wanted, cx), None),
    };

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        display_id,
        titlebar: None,
        kind: WindowKind::PopUp,
        focus: true,
        show: true,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Opaque,
        ..Default::default()
    };

    let opened = cx.open_window(options, |window, cx| {
        cx.new(|cx| Popover::new(workspace, true, window, cx))
    });

    match opened {
        Ok(handle) => {
            cx.global_mut::<MenuBar>().popover = Some(handle);

            // An accessory app must activate itself, or the panel never becomes key and never
            // gets the blur that closes it. Deferred: activating calls back into GPUI.
            cx.defer(|cx| cx.activate(true));
        }
        Err(error) => log::error!("could not open the menu bar popover: {error:#}"),
    }
}

/// Top right of the main display, for a status item that has no position.
fn in_corner(wanted: Size<Pixels>, cx: &App) -> Bounds<Pixels> {
    let screen = cx
        .primary_display()
        .map(|display| display.bounds())
        .unwrap_or_default();
    let origin = point(
        screen.right() - wanted.width - px(CORNER_GAP),
        screen.origin.y + px(CORNER_GAP),
    );

    Bounds::new(origin, wanted)
}

/// What the tests of the menu bar share: a status item that records instead of drawing.
#[cfg(test)]
pub(crate) mod harness {
    use std::cell::RefCell;

    use gpui::{TestAppContext, VisualTestContext};

    use super::place::{Frame, Screen};
    use super::*;
    use crate::{actions, lifecycle};

    pub(crate) struct FakeItem {
        pub looks: RefCell<Vec<Look>>,
        pub anchor: Option<Anchor>,
    }

    impl StatusItem for FakeItem {
        fn show(&self, look: &Look) {
            self.looks.borrow_mut().push(look.clone());
        }

        fn anchor(&self) -> Option<Anchor> {
            self.anchor
        }
    }

    /// A 1,440 by 900 screen with the item 900 points from the left.
    pub(crate) fn anchor() -> Anchor {
        let frame = Frame::new(0., 0., 1_440., 900.);
        let screen = Screen {
            display: 1,
            frame,
            visible: Frame::new(0., 0., 1_440., 876.),
        };

        Anchor {
            button: Frame::new(900., 876., 60., 24.),
            screen,
        }
    }

    pub(crate) struct Bar {
        pub item: Rc<FakeItem>,
        pub clicks: flume::Sender<()>,
    }

    impl Bar {
        /// The click `AppKit` would send.
        pub(crate) fn click(&self, cx: &mut VisualTestContext) {
            assert!(self.clicks.send(()).is_ok());
            cx.run_until_parked();
        }

        pub(crate) fn last_look(&self) -> Option<Look> {
            self.item.looks.borrow().last().cloned()
        }
    }

    pub(crate) fn init(cx: &mut TestAppContext) {
        cx.update(|cx| {
            swarm_ui::init(cx);
            actions::bind_keys(cx);
        });
    }

    /// Starts the menu bar and the lifecycle on a workspace that has a window.
    pub(crate) fn install(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Bar {
        let item = Rc::new(FakeItem {
            looks: RefCell::new(Vec::new()),
            anchor: Some(anchor()),
        });
        let (clicks, receiver) = flume::unbounded();
        let status: Rc<dyn StatusItem> = Rc::clone(&item) as Rc<dyn StatusItem>;

        cx.update(|_, cx| {
            lifecycle::keep(workspace.clone(), cx);
            start(workspace.clone(), Some(status), receiver, cx);
        });

        Bar { item, clicks }
    }

    /// Starts the menu bar and the lifecycle as they run when `AppKit` refused to make the
    /// status item.
    pub(crate) fn install_without_status_item(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) {
        let (_clicks, receiver) = flume::unbounded();

        cx.update(|_, cx| {
            lifecycle::keep(workspace.clone(), cx);
            start(workspace.clone(), None, receiver, cx);
        });
    }

    /// The sample workspace in a window, with the menu bar on it.
    pub(crate) fn setup(
        cx: &mut TestAppContext,
    ) -> (Entity<Workspace>, Bar, &mut VisualTestContext) {
        init(cx);
        let (workspace, cx) = cx.add_window_view(Workspace::with_sample);
        let bar = install(&workspace, cx);

        (workspace, bar, cx)
    }

    /// The popover's window, once it is open.
    pub(crate) fn popover_window(cx: &VisualTestContext) -> Option<VisualTestContext> {
        let popover = cx.read_global(|menu: &MenuBar, _| menu.popover);

        popover.map(|handle| VisualTestContext::from_window(handle.into(), cx))
    }

    /// Makes the popover the key window, as macOS does when it opens.
    pub(crate) fn activate(popover: &mut VisualTestContext) {
        popover.update(|window, _| window.activate_window());
        popover.run_until_parked();
    }

    pub(crate) fn popover_is_open(cx: &VisualTestContext) -> bool {
        cx.read_global(|menu: &MenuBar, _| menu.popover.is_some())
    }
}

#[cfg(test)]
#[expect(clippy::float_cmp, reason = "the sizes are whole numbers")]
mod tests {
    use gpui::TestAppContext;
    use swarm_protocol::{SubscriptionUsage, Usage, UsageWindow};

    use super::harness::{activate, popover_is_open, popover_window, setup};
    use super::*;
    use crate::model::SAMPLE_NOW;

    fn seconds_ago(seconds: u64) -> Instant {
        let now = Instant::now();

        now.checked_sub(Duration::from_secs(seconds)).unwrap_or(now)
    }

    #[test]
    fn a_click_is_allowed_when_the_popover_never_closed() {
        assert!(ReopenGuard::default().allows(Instant::now()));
    }

    #[test]
    fn a_click_within_300_ms_of_a_close_is_ignored() {
        let closed = Instant::now();
        let mut guard = ReopenGuard::default();
        guard.closed(closed);

        assert!(!guard.allows(closed));
        assert!(!guard.allows(closed + Duration::from_millis(1)));
        assert!(!guard.allows(closed + Duration::from_millis(299)));
    }

    #[test]
    fn a_click_after_300_ms_opens_the_popover_again() {
        let closed = Instant::now();
        let mut guard = ReopenGuard::default();
        guard.closed(closed);

        assert!(guard.allows(closed + Duration::from_millis(300)));
        assert!(guard.allows(closed + Duration::from_secs(5)));
    }

    #[gpui::test]
    fn the_status_item_shows_the_highest_percent_and_no_dot_without_a_task_that_needs_you(
        cx: &mut TestAppContext,
    ) {
        let (workspace, bar, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| {
            for task in &mut workspace.data.tasks {
                task.status = crate::model::TaskStatus::Idle;
            }
            cx.notify();
        });

        let look = bar.last_look();

        assert_eq!(look, Some(Look::new(Some(42), false)));
        assert_eq!(look.and_then(|look| look.title).as_deref(), Some(" 42%"));
    }

    #[gpui::test]
    fn with_the_dot_setting_off_the_status_item_never_shows_the_dot(cx: &mut TestAppContext) {
        let (workspace, bar, cx) = setup(cx);
        let with_dot = bar.last_look();

        cx.update(|_, cx| crate::settings::change_notifications(|n| n.menu_bar_dot = false, cx));
        let turned_off = bar.last_look();
        workspace.update(cx, |workspace, cx| {
            workspace.usage_changed(Usage::default(), cx);
        });
        let after_news = bar.last_look();

        cx.update(|_, cx| crate::settings::change_notifications(|n| n.menu_bar_dot = true, cx));

        assert_eq!(with_dot, Some(Look::new(Some(42), true)));
        assert_eq!(turned_off, Some(Look::new(Some(42), false)));
        assert_eq!(after_news, Some(Look::new(None, false)));
        assert_eq!(bar.last_look(), Some(Look::new(None, true)));
    }

    #[gpui::test]
    fn the_status_item_follows_the_model(cx: &mut TestAppContext) {
        let (workspace, bar, cx) = setup(cx);
        let first = bar.last_look();

        workspace.update(cx, |workspace, cx| {
            workspace.usage_changed(Usage::default(), cx);
        });

        assert_eq!(first, Some(Look::new(Some(42), true)));
        assert_eq!(bar.last_look(), Some(Look::new(None, true)));
    }

    fn frames(bar: &harness::Bar) -> Vec<BeeFrame> {
        bar.item
            .looks
            .borrow()
            .iter()
            .map(|look| look.frame)
            .collect()
    }

    fn flap_for(times: u32, cx: &mut gpui::VisualTestContext) {
        for _ in 0..times {
            cx.executor().advance_clock(FLAP_INTERVAL);
            cx.run_until_parked();
        }
    }

    fn stop_the_work(workspace: &Entity<Workspace>, cx: &mut gpui::VisualTestContext) {
        workspace.update(cx, |workspace, cx| {
            for agent in &mut workspace.data.agents {
                agent.status = swarm_ui::AgentStatus::Idle;
            }
            cx.notify();
        });
    }

    #[gpui::test]
    fn the_bee_flaps_between_two_frames_while_an_agent_works(cx: &mut TestAppContext) {
        let (_, bar, cx) = setup(cx);
        assert_eq!(bar.last_look().map(|look| look.frame), Some(BeeFrame::Open));

        flap_for(4, cx);
        let frames = frames(&bar);

        assert!(frames.contains(&BeeFrame::Flat));
        assert!(
            frames.windows(2).all(|pair| pair[0] != pair[1]),
            "every look is a new frame: {frames:?}"
        );
        assert!(bar.item.looks.borrow().len() >= 5);
    }

    #[gpui::test]
    fn the_bee_flaps_only_while_an_agent_works(cx: &mut TestAppContext) {
        let (workspace, bar, cx) = setup(cx);
        stop_the_work(&workspace, cx);
        flap_for(1, cx);
        let looks = bar.item.looks.borrow().len();

        flap_for(6, cx);

        assert_eq!(bar.item.looks.borrow().len(), looks, "no work, no flicker");
        assert_eq!(bar.last_look().map(|look| look.frame), Some(BeeFrame::Open));

        workspace.update(cx, |workspace, cx| {
            workspace.data.agents[0].status = swarm_ui::AgentStatus::Working;
            cx.notify();
        });
        flap_for(2, cx);

        assert!(frames(&bar).contains(&BeeFrame::Flat));
    }

    #[gpui::test]
    fn the_bee_stops_with_its_wings_open_when_the_work_ends(cx: &mut TestAppContext) {
        let (workspace, bar, cx) = setup(cx);
        flap_for(1, cx);
        assert_eq!(bar.last_look().map(|look| look.frame), Some(BeeFrame::Flat));

        stop_the_work(&workspace, cx);

        assert_eq!(bar.last_look().map(|look| look.frame), Some(BeeFrame::Open));
    }

    #[gpui::test]
    fn the_bee_is_still_with_the_easter_eggs_off(cx: &mut TestAppContext) {
        let (_, bar, cx) = setup(cx);
        cx.update(|_, cx| crate::settings::change_appearance(|a| a.easter_eggs = false, cx));

        flap_for(6, cx);

        assert!(frames(&bar).iter().all(|frame| *frame == BeeFrame::Open));
    }

    #[gpui::test]
    fn the_bee_is_still_under_reduce_motion(cx: &mut TestAppContext) {
        let (_, bar, cx) = setup(cx);
        cx.update(|_, cx| cx.set_reduce_motion(true));

        flap_for(6, cx);

        assert!(frames(&bar).iter().all(|frame| *frame == BeeFrame::Open));
    }

    #[gpui::test]
    fn the_flicker_does_not_change_what_voiceover_says(cx: &mut TestAppContext) {
        let (_, bar, cx) = setup(cx);

        flap_for(4, cx);
        let looks = bar.item.looks.borrow();
        let labels: std::collections::HashSet<_> =
            looks.iter().map(|look| look.label.clone()).collect();

        assert_eq!(labels.len(), 1);
    }

    #[gpui::test]
    fn the_status_item_is_told_only_when_its_look_changes(cx: &mut TestAppContext) {
        let (workspace, bar, cx) = setup(cx);
        let before = bar.item.looks.borrow().len();

        workspace.update(cx, |_, cx| cx.notify());
        workspace.update(cx, |_, cx| cx.notify());

        assert_eq!(bar.item.looks.borrow().len(), before);
    }

    #[gpui::test]
    fn a_usage_window_that_resets_changes_the_percent_without_news_from_the_daemon(
        cx: &mut TestAppContext,
    ) {
        let (workspace, bar, cx) = setup(cx);
        stop_the_work(&workspace, cx);
        let resets = SAMPLE_NOW / 1_000 + 90;
        workspace.update(cx, |workspace, cx| {
            for task in &mut workspace.data.tasks {
                task.status = crate::model::TaskStatus::Idle;
            }
            workspace.usage_changed(
                Usage {
                    claude: SubscriptionUsage {
                        windows: vec![UsageWindow {
                            label: "5h".to_owned(),
                            used_percent: 42,
                            resets_at: Some(resets),
                        }],
                        ..Default::default()
                    },
                    ..Default::default()
                },
                cx,
            );
        });
        let before = bar.last_look();
        let shown = bar.item.looks.borrow().len();

        // The clock passes the reset, and nothing tells the workspace.
        workspace.update(cx, |workspace, _| {
            workspace.fixed_now = Some(SAMPLE_NOW + 120_000);
        });
        cx.run_until_parked();
        let unchanged = bar.last_look();
        cx.executor().advance_clock(LOOK_INTERVAL);
        cx.run_until_parked();

        assert_eq!(before, Some(Look::new(Some(42), false)));
        assert_eq!(unchanged, before, "the look changed before the timer fired");
        assert_eq!(bar.last_look(), Some(Look::new(Some(0), false)));
        assert_eq!(bar.item.looks.borrow().len(), shown + 1);
    }

    #[gpui::test]
    fn the_timer_leaves_the_status_item_alone_when_the_look_is_the_same(cx: &mut TestAppContext) {
        let (workspace, bar, cx) = setup(cx);
        stop_the_work(&workspace, cx);
        let shown = bar.item.looks.borrow().len();

        cx.executor().advance_clock(LOOK_INTERVAL * 3);
        cx.run_until_parked();

        assert_eq!(bar.item.looks.borrow().len(), shown);
    }

    #[gpui::test]
    fn a_click_opens_the_popover_and_the_next_click_closes_it(cx: &mut TestAppContext) {
        let (_workspace, bar, cx) = setup(cx);

        bar.click(cx);
        let opened = popover_is_open(cx);
        let windows = cx.windows().len();

        bar.click(cx);

        assert!(opened);
        assert_eq!(windows, 2);
        assert!(!popover_is_open(cx));
        assert_eq!(cx.windows().len(), 1);
    }

    #[gpui::test]
    fn a_click_right_after_a_close_does_not_open_the_popover_again(cx: &mut TestAppContext) {
        let (_workspace, bar, cx) = setup(cx);
        bar.click(cx);
        let mut popover = popover_window(cx).expect("the popover is open");
        activate(&mut popover);
        popover.deactivate_window();
        let closed = !popover_is_open(cx);

        bar.click(cx);
        let ignored = !popover_is_open(cx);
        cx.update_global(|menu: &mut MenuBar, _| menu.guard.closed(seconds_ago(1)));
        bar.click(cx);

        assert!(closed);
        assert!(ignored, "the click that closed it opened it again");
        assert!(popover_is_open(cx));
    }

    #[gpui::test]
    fn the_popover_opens_under_the_status_item(cx: &mut TestAppContext) {
        let (workspace, bar, cx) = setup(cx);
        let wanted = workspace.read_with(cx, |workspace, _| {
            let summary = Summary::of(&workspace.data, workspace.now_ms() / 1_000);

            size(px(popover::WIDTH), px(popover::height(&summary)))
        });

        bar.click(cx);
        let mut popover = popover_window(cx).expect("the popover is open");
        let bounds = popover.update(|window, _| window.bounds());

        assert_eq!(bounds, place::below(&harness::anchor(), wanted).bounds);
        assert_eq!(f32::from(bounds.size.width), popover::WIDTH);
        assert_eq!(f32::from(bounds.origin.y), 28.);
    }

    #[gpui::test]
    fn the_popover_closes_when_its_window_loses_focus(cx: &mut TestAppContext) {
        let (_workspace, bar, cx) = setup(cx);
        bar.click(cx);
        let mut popover = popover_window(cx).expect("the popover is open");
        activate(&mut popover);
        assert!(popover_is_open(cx));

        popover.deactivate_window();

        assert!(!popover_is_open(cx));
    }

    #[gpui::test]
    fn escape_closes_the_popover(cx: &mut TestAppContext) {
        let (_workspace, bar, cx) = setup(cx);
        bar.click(cx);
        let mut popover = popover_window(cx).expect("the popover is open");
        popover.run_until_parked();

        popover.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert!(!popover_is_open(cx));
    }

    #[gpui::test]
    fn the_click_goes_through_the_channel_and_not_through_the_status_item(cx: &mut TestAppContext) {
        let (_workspace, bar, cx) = setup(cx);

        // Nothing opened before the task read the click.
        assert!(bar.clicks.send(()).is_ok());
        let before = popover_is_open(cx);
        cx.run_until_parked();

        assert!(!before);
        assert!(popover_is_open(cx));
    }
}
