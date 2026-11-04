//! A disabled control keeps its focus handle: focus does not drop when a control turns
//! disabled, the control leaves the Tab order, and a click does nothing.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Context, Entity, FocusHandle, KeyBinding, Modifiers, Render, TestAppContext, VisualTestContext,
    Window, div, point, px,
};
use swarm_ui::prelude::*;
use swarm_ui::test_support::{focused_stop, tab_walk};
use swarm_ui::{FocusNext, FocusPrevious, ROOT_CONTEXT, tab_navigation};

#[derive(Clone, Copy, Debug)]
enum Kind {
    ButtonTracked,
    ButtonOwnFocus,
    IconButton,
    Switch,
    Checkbox,
    Dropdown,
    TextInput,
}

struct Harness {
    kind: Kind,
    disabled: bool,
    root: FocusHandle,
    before: FocusHandle,
    tracked: FocusHandle,
    clicks: Rc<Cell<usize>>,
    input: Entity<TextInput>,
}

impl Harness {
    fn control(&self) -> gpui::AnyElement {
        let clicks = Rc::clone(&self.clicks);
        let count = move || clicks.set(clicks.get() + 1);

        match self.kind {
            Kind::ButtonTracked => Button::new("control", "Control")
                .track_focus(&self.tracked)
                .disabled(self.disabled)
                .on_click(move |_, _, _| count())
                .into_any_element(),
            Kind::ButtonOwnFocus => Button::new("control", "Control")
                .disabled(self.disabled)
                .on_click(move |_, _, _| count())
                .into_any_element(),
            Kind::IconButton => IconButton::new("control", IconName::Plus, "Control")
                .disabled(self.disabled)
                .on_click(move |_, _, _| count())
                .into_any_element(),
            Kind::Switch => Switch::new("control", "Control")
                .disabled(self.disabled)
                .on_toggle(move |_, _, _| count())
                .into_any_element(),
            Kind::Checkbox => Checkbox::new("control", "Control")
                .disabled(self.disabled)
                .on_toggle(move |_, _, _| count())
                .into_any_element(),
            Kind::Dropdown => Dropdown::new("control", "Control", vec!["a".into(), "b".into()], 0)
                .disabled(self.disabled)
                .into_any_element(),
            Kind::TextInput => self.input.clone().into_any_element(),
        }
    }
}

impl Render for Harness {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let root = v_flex()
            .id("root")
            .key_context(ROOT_CONTEXT)
            .track_focus(&self.root)
            .size_full()
            .child(Button::new("before", "Before").track_focus(&self.before))
            .child(
                div()
                    .id("slot")
                    .debug_selector(|| "slot".to_owned())
                    .child(self.control()),
            )
            .child(Button::new("after", "After"));

        tab_navigation(root)
    }
}

fn setup(cx: &mut TestAppContext, kind: Kind) -> (Entity<Harness>, &mut VisualTestContext) {
    cx.update(|cx| {
        swarm_ui::theme::init(cx);
        cx.bind_keys([
            KeyBinding::new("tab", FocusNext, Some(ROOT_CONTEXT)),
            KeyBinding::new("shift-tab", FocusPrevious, Some(ROOT_CONTEXT)),
        ]);
    });

    let (harness, cx) = cx.add_window_view(move |_, cx| {
        let input = cx.new(|cx| {
            let mut input = TextInput::new(cx);
            input.set_label("Control", cx);
            input
        });

        Harness {
            kind,
            disabled: false,
            root: cx.focus_handle(),
            before: cx.focus_handle().tab_stop(true),
            tracked: cx.focus_handle().tab_stop(true),
            clicks: Rc::default(),
            input,
        }
    });

    let before = harness.read_with(cx, |harness, _| harness.before.clone());
    cx.update(|window, cx| before.focus(window, cx));
    cx.run_until_parked();

    (harness, cx)
}

fn set_disabled(harness: &Entity<Harness>, disabled: bool, cx: &mut VisualTestContext) {
    harness.update(cx, |harness, cx| {
        harness.disabled = disabled;

        harness
            .input
            .update(cx, |input, cx| input.set_disabled(disabled, cx));
        cx.notify();
    });
    cx.run_until_parked();
}

/// Tabs to the control and returns what has focus there.
fn focus_control(cx: &mut VisualTestContext) -> Option<FocusHandle> {
    cx.simulate_keystrokes("tab");

    cx.update(|window, cx| window.focused(cx))
}

fn clicks(harness: &Entity<Harness>, cx: &mut VisualTestContext) -> usize {
    harness.read_with(cx, |harness, _| harness.clicks.get())
}

fn click_control(cx: &mut VisualTestContext) {
    let Some(slot) = cx.debug_bounds("slot") else {
        panic!("the slot is not drawn");
    };

    cx.simulate_click(slot.origin + point(px(6.), px(6.)), Modifiers::none());
    cx.run_until_parked();
}

fn walk(harness: &Entity<Harness>, cx: &mut VisualTestContext) -> Vec<String> {
    let root = harness.read_with(cx, |harness, _| harness.root.clone());

    tab_walk(cx, &root, "tab")
}

fn control_name(kind: Kind) -> &'static str {
    match kind {
        Kind::ButtonTracked | Kind::ButtonOwnFocus | Kind::IconButton => "Button Control",
        Kind::Switch => "Switch Control",
        Kind::Checkbox => "CheckBox Control",
        Kind::Dropdown => "ComboBox Control",
        Kind::TextInput => "TextInput Control",
    }
}

macro_rules! disabled_control {
    ($module:ident, $kind:expr) => {
        mod $module {
            use super::*;

            #[gpui::test]
            fn an_enabled_control_is_a_tab_stop(cx: &mut TestAppContext) {
                let (harness, cx) = setup(cx, $kind);

                assert_eq!(
                    walk(&harness, cx),
                    ["Button Before", control_name($kind), "Button After"]
                );
            }

            #[gpui::test]
            fn a_focused_control_that_turns_disabled_keeps_focus(cx: &mut TestAppContext) {
                let (harness, cx) = setup(cx, $kind);
                let focused = focus_control(cx);
                assert!(focused.is_some());

                set_disabled(&harness, true, cx);

                let root = harness.read_with(cx, |harness, _| harness.root.clone());

                assert_eq!(cx.update(|window, cx| window.focused(cx)), focused);
                assert_eq!(focused_stop(cx, &root), control_name($kind));
            }

            #[gpui::test]
            fn a_disabled_control_is_not_a_tab_stop(cx: &mut TestAppContext) {
                let (harness, cx) = setup(cx, $kind);
                set_disabled(&harness, true, cx);

                assert_eq!(walk(&harness, cx), ["Button Before", "Button After"]);
            }

            #[gpui::test]
            fn a_click_on_a_disabled_control_does_nothing(cx: &mut TestAppContext) {
                let (harness, cx) = setup(cx, $kind);
                set_disabled(&harness, true, cx);

                click_control(cx);

                assert_eq!(clicks(&harness, cx), 0);
            }

            #[gpui::test]
            fn a_click_on_a_disabled_control_leaves_focus_where_it_was(cx: &mut TestAppContext) {
                let (harness, cx) = setup(cx, $kind);
                set_disabled(&harness, true, cx);
                let before = cx.update(|window, cx| window.focused(cx));

                click_control(cx);

                assert_eq!(cx.update(|window, cx| window.focused(cx)), before);
            }

            #[gpui::test]
            fn a_control_enabled_again_is_a_tab_stop_again(cx: &mut TestAppContext) {
                let (harness, cx) = setup(cx, $kind);
                set_disabled(&harness, true, cx);
                set_disabled(&harness, false, cx);

                assert_eq!(
                    walk(&harness, cx),
                    ["Button Before", control_name($kind), "Button After"]
                );
            }
        }
    };
}

disabled_control!(button_with_a_given_handle, Kind::ButtonTracked);
disabled_control!(button_with_its_own_focus, Kind::ButtonOwnFocus);
disabled_control!(icon_button, Kind::IconButton);
disabled_control!(switch, Kind::Switch);
disabled_control!(checkbox, Kind::Checkbox);
disabled_control!(dropdown, Kind::Dropdown);
disabled_control!(text_input, Kind::TextInput);

#[gpui::test]
fn a_click_on_an_enabled_button_counts(cx: &mut TestAppContext) {
    let (harness, cx) = setup(cx, Kind::ButtonOwnFocus);

    click_control(cx);

    assert_eq!(clicks(&harness, cx), 1);
}

#[gpui::test]
fn a_disabled_control_shows_which_stop_has_focus(cx: &mut TestAppContext) {
    let (harness, cx) = setup(cx, Kind::Switch);
    focus_control(cx);
    set_disabled(&harness, true, cx);
    let root = harness.read_with(cx, |harness, _| harness.root.clone());

    assert_eq!(focused_stop(cx, &root), "Switch Control");
}

fn set_read_only(harness: &Entity<Harness>, read_only: bool, cx: &mut VisualTestContext) {
    harness.update(cx, |harness, cx| {
        harness
            .input
            .update(cx, |input, cx| input.set_read_only(read_only, cx));
    });
    cx.run_until_parked();
}

fn typed_text(harness: &Entity<Harness>, cx: &mut VisualTestContext) -> String {
    harness.read_with(cx, |harness, cx| harness.input.read(cx).text().to_string())
}

#[gpui::test]
fn typing_edits_an_editable_text_input(cx: &mut TestAppContext) {
    let (harness, cx) = setup(cx, Kind::TextInput);
    focus_control(cx);

    cx.simulate_input("x");

    assert_eq!(typed_text(&harness, cx), "x");
}

#[gpui::test]
fn typing_does_not_edit_a_read_only_text_input(cx: &mut TestAppContext) {
    let (harness, cx) = setup(cx, Kind::TextInput);
    focus_control(cx);
    set_read_only(&harness, true, cx);

    cx.simulate_input("x");

    assert_eq!(typed_text(&harness, cx), "");
}

#[gpui::test]
fn a_read_only_text_input_stays_a_tab_stop(cx: &mut TestAppContext) {
    let (harness, cx) = setup(cx, Kind::TextInput);
    set_read_only(&harness, true, cx);

    assert_eq!(
        walk(&harness, cx),
        ["Button Before", "TextInput Control", "Button After"]
    );
}

#[gpui::test]
fn a_text_input_that_turns_read_only_keeps_focus(cx: &mut TestAppContext) {
    let (harness, cx) = setup(cx, Kind::TextInput);
    let focused = focus_control(cx);

    set_read_only(&harness, true, cx);

    assert_eq!(cx.update(|window, cx| window.focused(cx)), focused);
}
