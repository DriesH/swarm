//! The Tab-walk helpers: they name each stop, and an element that is not named shows as "unnamed".

use gpui::{
    Context, FocusHandle, KeyBinding, Render, TestAppContext, VisualTestContext, Window, div,
};
use swarm_ui::prelude::*;
use swarm_ui::test_support::{focused_stop, tab_walk};
use swarm_ui::{FocusNext, FocusPrevious, ROOT_CONTEXT, tab_navigation};

struct Window3 {
    root: FocusHandle,
    first: FocusHandle,
}

impl Render for Window3 {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let root = v_flex()
            .id("root")
            .key_context(ROOT_CONTEXT)
            .track_focus(&self.root)
            .child(Button::new("one", "One").track_focus(&self.first))
            .child(Button::new("two", "Two"))
            .child(div().id("plain").focusable().tab_stop(true))
            .child(Button::new("three", "Three"));

        tab_navigation(root)
    }
}

fn setup(cx: &mut TestAppContext) -> (FocusHandle, &mut VisualTestContext) {
    cx.update(|cx| {
        swarm_ui::theme::init(cx);
        cx.bind_keys([
            KeyBinding::new("tab", FocusNext, Some(ROOT_CONTEXT)),
            KeyBinding::new("shift-tab", FocusPrevious, Some(ROOT_CONTEXT)),
        ]);
    });

    let (window, cx) = cx.add_window_view(|_, cx| Window3 {
        root: cx.focus_handle(),
        first: cx.focus_handle().tab_stop(true),
    });
    let (root, first) =
        window.read_with(cx, |window, _| (window.root.clone(), window.first.clone()));

    cx.update(|window, cx| first.focus(window, cx));
    cx.run_until_parked();

    (root, cx)
}

#[gpui::test]
fn tab_visits_the_stops_in_order_and_names_the_plain_element_unnamed(cx: &mut TestAppContext) {
    let (root, cx) = setup(cx);

    assert_eq!(
        tab_walk(cx, &root, "tab"),
        ["Button One", "Button Two", "unnamed", "Button Three"]
    );
}

#[gpui::test]
fn shift_tab_visits_the_stops_in_reverse(cx: &mut TestAppContext) {
    let (root, cx) = setup(cx);

    assert_eq!(
        tab_walk(cx, &root, "shift-tab"),
        ["Button One", "Button Three", "unnamed", "Button Two"]
    );
}

#[gpui::test]
fn a_window_with_nothing_focused_reports_nothing(cx: &mut TestAppContext) {
    let (root, cx) = setup(cx);
    cx.update(Window::blur);

    assert_eq!(focused_stop(cx, &root), "nothing");
}

#[gpui::test]
fn focus_outside_the_root_reports_not_drawn(cx: &mut TestAppContext) {
    let (root, cx) = setup(cx);
    let stray = cx.update(|_, cx| cx.focus_handle());
    cx.update(|window, cx| stray.focus(window, cx));

    assert_eq!(focused_stop(cx, &root), "not drawn");
}
