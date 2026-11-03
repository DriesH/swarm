//! Drives the task tab bar with simulated keys: one Tab stop, arrows inside it.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Context, FocusHandle, KeyBinding, Render, TestAppContext, VisualTestContext, Window, px,
};
use swarm_ui::prelude::*;
use swarm_ui::{FocusNext, FocusPrevious, ROOT_CONTEXT, Tab, TabBar, tab_navigation};

type Log = Rc<RefCell<Vec<String>>>;

struct Harness {
    before: FocusHandle,
    after: FocusHandle,
    log: Log,
    closable: bool,
    /// Eight tabs in 300 px, so the row scrolls.
    narrow: bool,
}

impl Render for Harness {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let (select_log, close_log) = (Rc::clone(&self.log), Rc::clone(&self.log));
        let closable = self.closable;
        let (count, width) = if self.narrow {
            (8_usize, 300.)
        } else {
            (3, 800.)
        };

        let bar = TabBar::new("tasks", "Tasks")
            .debug_selector(|| "bar".to_owned())
            .tabs((0..count).map(|index| {
                let close_log = Rc::clone(&close_log);

                Tab::new(("task", index), format!("task {index}"), AgentStatus::Idle)
                    .debug_selector(move || format!("task-{index}"))
                    .when(closable, |tab| {
                        tab.on_close(move |_, _, _| {
                            close_log.borrow_mut().push(format!("close {index}"));
                        })
                    })
            }))
            .selected(1)
            .on_select(move |index, _, _| select_log.borrow_mut().push(format!("select {index}")));

        tab_navigation(
            gpui::div()
                .id("root")
                .key_context(ROOT_CONTEXT)
                .w(px(width))
                .child(
                    gpui::div()
                        .id("before")
                        .track_focus(&self.before)
                        .size(px(20.)),
                )
                .child(bar)
                .child(
                    gpui::div()
                        .id("after")
                        .track_focus(&self.after)
                        .size(px(20.)),
                ),
        )
    }
}

struct Rig {
    log: Log,
    before: FocusHandle,
    after: FocusHandle,
}

fn setup(cx: &mut TestAppContext) -> (Rig, &mut VisualTestContext) {
    setup_with(cx, false, false)
}

fn setup_with(
    cx: &mut TestAppContext,
    closable: bool,
    narrow: bool,
) -> (Rig, &mut VisualTestContext) {
    cx.update(|cx| {
        swarm_ui::theme::init(cx);
        cx.bind_keys([
            KeyBinding::new("tab", FocusNext, Some(ROOT_CONTEXT)),
            KeyBinding::new("shift-tab", FocusPrevious, Some(ROOT_CONTEXT)),
        ]);
    });

    let log = Log::default();
    let shared = Rc::clone(&log);
    let (harness, cx) = cx.add_window_view(move |_, cx| Harness {
        before: cx.focus_handle().tab_stop(true),
        after: cx.focus_handle().tab_stop(true),
        log: shared,
        closable,
        narrow,
    });
    let (before, after) = harness.read_with(cx, |harness, _| {
        (harness.before.clone(), harness.after.clone())
    });
    cx.update(|window, cx| before.focus(window, cx));
    cx.run_until_parked();

    (Rig { log, before, after }, cx)
}

/// A full key press: GPUI turns Enter into a click on the key up.
fn press(key: &'static str, cx: &mut VisualTestContext) {
    let keystroke =
        gpui::Keystroke::parse(key).unwrap_or_else(|error| panic!("{key} is not a key: {error}"));

    cx.simulate_event(gpui::KeyDownEvent {
        keystroke: keystroke.clone(),
        is_held: false,
        prefer_character_input: false,
    });
    cx.simulate_event(gpui::KeyUpEvent { keystroke });
    cx.run_until_parked();
}

/// Presses Enter and returns what the focused tab reported.
fn enter(rig: &Rig, cx: &mut VisualTestContext) -> Vec<String> {
    rig.log.borrow_mut().clear();
    press("enter", cx);

    rig.log.borrow().clone()
}

#[gpui::test]
fn the_tabs_are_one_tab_stop(cx: &mut TestAppContext) {
    let (rig, cx) = setup(cx);

    press("tab", cx);
    assert_eq!(enter(&rig, cx), ["select 1"]);

    press("tab", cx);
    assert!(cx.update(|window, _| rig.after.is_focused(window)));
}

#[gpui::test]
fn shift_tab_leaves_the_tabs_in_one_step(cx: &mut TestAppContext) {
    let (rig, cx) = setup(cx);
    cx.update(|window, cx| rig.after.focus(window, cx));

    press("shift-tab", cx);
    assert_eq!(enter(&rig, cx), ["select 1"]);

    press("shift-tab", cx);
    assert!(cx.update(|window, _| rig.before.is_focused(window)));
}

#[gpui::test]
fn the_arrows_move_between_tabs(cx: &mut TestAppContext) {
    let (rig, cx) = setup(cx);
    press("tab", cx);

    press("right", cx);
    assert_eq!(enter(&rig, cx), ["select 2"]);

    press("left", cx);
    press("left", cx);
    assert_eq!(enter(&rig, cx), ["select 0"]);
}

#[gpui::test]
fn the_arrows_stop_at_the_ends(cx: &mut TestAppContext) {
    let (rig, cx) = setup(cx);
    press("tab", cx);

    press("end", cx);
    press("right", cx);
    assert_eq!(enter(&rig, cx), ["select 2"]);

    press("home", cx);
    press("left", cx);
    assert_eq!(enter(&rig, cx), ["select 0"]);
}

#[gpui::test]
fn tab_leaves_from_the_tab_the_arrows_chose(cx: &mut TestAppContext) {
    let (rig, cx) = setup(cx);
    press("tab", cx);
    press("end", cx);

    press("tab", cx);
    assert!(cx.update(|window, _| rig.after.is_focused(window)));

    press("shift-tab", cx);
    assert_eq!(enter(&rig, cx), ["select 2"]);
}

#[gpui::test]
fn a_close_button_is_not_a_tab_stop(cx: &mut TestAppContext) {
    let (rig, cx) = setup_with(cx, true, false);
    press("tab", cx);

    press("tab", cx);

    assert!(cx.update(|window, _| rig.after.is_focused(window)));
}

#[gpui::test]
fn a_close_button_still_closes_on_a_click(cx: &mut TestAppContext) {
    let (rig, cx) = setup_with(cx, true, false);
    let button = cx
        .debug_bounds("task-1-close")
        .expect("the selected tab shows its close button");

    cx.simulate_click(button.center(), gpui::Modifiers::none());
    cx.run_until_parked();

    assert_eq!(*rig.log.borrow(), ["close 1"]);
}

#[gpui::test]
fn a_tab_the_arrows_reach_scrolls_into_view(cx: &mut TestAppContext) {
    let (_, cx) = setup_with(cx, false, true);
    press("tab", cx);

    press("end", cx);
    // A real window draws when focus moves.
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();

    let bar = cx.debug_bounds("bar").expect("the tab row");
    let last = cx.debug_bounds("task-7").expect("the last tab");
    assert!(last.right() <= bar.right() + px(1.));
}
