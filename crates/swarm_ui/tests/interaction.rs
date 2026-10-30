//! Drives the menu components with simulated clicks and keys in a test window.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Context, FocusHandle, KeyBinding, Modifiers, Render, TestAppContext, VisualTestContext, Window,
    point, px,
};
use swarm_ui::prelude::*;
use swarm_ui::{
    Confirm, Dialog, Dismiss, FocusNext, FocusPrevious, MENU_CONTEXT, Menu, MenuItem, PopoverMenu,
    ROOT_CONTEXT, SelectNext, SelectPrevious, tab_navigation,
};

type Log = Rc<RefCell<Vec<String>>>;

struct Harness {
    outside: FocusHandle,
    log: Log,
    dropdown: bool,
}

impl Render for Harness {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let log = Rc::clone(&self.log);

        let control = if self.dropdown {
            let changed = Rc::clone(&self.log);

            Dropdown::new(
                "dropdown",
                "Pick",
                vec!["a".into(), "b".into(), "c".into()],
                0,
            )
            .on_change(move |index, _, _| changed.borrow_mut().push(format!("change {index}")))
            .into_any_element()
        } else {
            PopoverMenu::new(
                "popover",
                gpui::div()
                    .id("trigger")
                    .size(px(40.))
                    .debug_selector(|| "trigger".to_owned()),
                move |_, _| {
                    let item = |id: &'static str, log: &Log| {
                        let log = Rc::clone(log);

                        MenuItem::new(id, id)
                            .on_select(move |_, _| log.borrow_mut().push(id.to_owned()))
                    };

                    Menu::new("menu", "Test menu")
                        .item(item("a", &log))
                        .item(item("b", &log).disabled(true))
                        .item(item("c", &log))
                },
            )
            .into_any_element()
        };

        v_flex().size_full().child(control).child(
            gpui::div()
                .id("outside")
                .track_focus(&self.outside)
                .size(px(40.)),
        )
    }
}

fn setup(cx: &mut TestAppContext, dropdown: bool) -> (Log, FocusHandle, &mut VisualTestContext) {
    cx.update(|cx| {
        swarm_ui::theme::init(cx);
        cx.bind_keys([
            KeyBinding::new("escape", Dismiss, Some(MENU_CONTEXT)),
            KeyBinding::new("up", SelectPrevious, Some(MENU_CONTEXT)),
            KeyBinding::new("down", SelectNext, Some(MENU_CONTEXT)),
            KeyBinding::new("enter", Confirm, Some(MENU_CONTEXT)),
        ]);
    });

    let log = Log::default();
    let harness_log = Rc::clone(&log);
    let (harness, cx) = cx.add_window_view(move |_, cx| Harness {
        outside: cx.focus_handle(),
        log: harness_log,
        dropdown,
    });

    let outside = harness.read_with(cx, |harness, _| harness.outside.clone());
    cx.update(|window, cx| outside.focus(window, cx));
    cx.run_until_parked();

    (log, outside, cx)
}

fn click_trigger(cx: &mut VisualTestContext) {
    cx.simulate_click(point(px(10.), px(10.)), Modifiers::none());
    cx.run_until_parked();
}

fn menu_is_open(cx: &mut VisualTestContext) -> bool {
    cx.debug_bounds("menu").is_some()
}

fn entries(log: &Log) -> Vec<String> {
    log.borrow().clone()
}

#[gpui::test]
fn clicking_the_trigger_opens_the_menu_and_focuses_it(cx: &mut TestAppContext) {
    let (_, outside, cx) = setup(cx, false);
    assert!(!menu_is_open(cx));

    click_trigger(cx);

    assert!(menu_is_open(cx));
    assert!(!cx.update(|window, _| outside.is_focused(window)));
    assert!(cx.update(|window, cx| window.focused(cx).is_some()));
}

#[gpui::test]
fn down_skips_a_disabled_item(cx: &mut TestAppContext) {
    let (log, _, cx) = setup(cx, false);
    click_trigger(cx);

    cx.simulate_keystrokes("down enter");

    assert_eq!(entries(&log), ["c"]);
}

#[gpui::test]
fn down_wraps_to_the_first_item(cx: &mut TestAppContext) {
    let (log, _, cx) = setup(cx, false);
    click_trigger(cx);

    cx.simulate_keystrokes("down down enter");

    assert_eq!(entries(&log), ["a"]);
}

#[gpui::test]
fn up_wraps_to_the_last_item(cx: &mut TestAppContext) {
    let (log, _, cx) = setup(cx, false);
    click_trigger(cx);

    cx.simulate_keystrokes("up enter");

    assert_eq!(entries(&log), ["c"]);
}

#[gpui::test]
fn enter_selects_the_highlighted_item_and_closes(cx: &mut TestAppContext) {
    let (log, _, cx) = setup(cx, false);
    click_trigger(cx);

    cx.simulate_keystrokes("enter");

    assert_eq!(entries(&log), ["a"]);
    assert!(!menu_is_open(cx));
}

#[gpui::test]
fn escape_closes_and_returns_focus(cx: &mut TestAppContext) {
    let (log, outside, cx) = setup(cx, false);
    click_trigger(cx);

    cx.simulate_keystrokes("escape");

    assert!(!menu_is_open(cx));
    assert!(cx.update(|window, _| outside.is_focused(window)));
    assert!(entries(&log).is_empty());
}

#[gpui::test]
fn clicking_outside_closes_the_menu(cx: &mut TestAppContext) {
    let (_, _, cx) = setup(cx, false);
    click_trigger(cx);

    cx.simulate_click(point(px(600.), px(600.)), Modifiers::none());
    cx.run_until_parked();

    assert!(!menu_is_open(cx));
}

#[gpui::test]
fn clicking_the_trigger_again_closes_the_menu(cx: &mut TestAppContext) {
    let (_, _, cx) = setup(cx, false);
    click_trigger(cx);

    click_trigger(cx);

    assert!(!menu_is_open(cx));
}

#[gpui::test]
fn dropdown_choosing_an_option_reports_its_index(cx: &mut TestAppContext) {
    let (log, _, cx) = setup(cx, true);
    click_trigger(cx);
    assert!(cx.debug_bounds("dropdown-menu").is_some());

    cx.simulate_keystrokes("down enter");

    assert_eq!(entries(&log), ["change 1"]);
}

struct DialogHarness {
    dialog: FocusHandle,
}

impl Render for DialogHarness {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let root = v_flex()
            .id("root")
            .key_context(ROOT_CONTEXT)
            .relative()
            .size_full()
            .child(Button::new("behind-one", "Behind one"))
            .child(Button::new("behind-two", "Behind two"))
            .child(
                Dialog::new("dialog", "Confirm", &self.dialog)
                    .child(Label::new("Sure?"))
                    .footer(
                        h_flex()
                            .child(Button::new("no", "No"))
                            .child(Button::new("yes", "Yes")),
                    ),
            );

        tab_navigation(root)
    }
}

#[gpui::test]
fn tab_never_leaves_an_open_dialog(cx: &mut TestAppContext) {
    cx.update(|cx| {
        swarm_ui::theme::init(cx);
        cx.bind_keys([
            KeyBinding::new("tab", FocusNext, Some(ROOT_CONTEXT)),
            KeyBinding::new("shift-tab", FocusPrevious, Some(ROOT_CONTEXT)),
        ]);
    });

    let (harness, cx) = cx.add_window_view(|_, cx| DialogHarness {
        dialog: cx.focus_handle(),
    });
    let dialog = harness.read_with(cx, |harness, _| harness.dialog.clone());
    cx.update(|window, cx| dialog.focus(window, cx));
    cx.run_until_parked();

    for key in ["tab", "shift-tab"] {
        for _ in 0..8 {
            cx.simulate_keystrokes(key);

            assert!(
                cx.update(|window, cx| dialog.contains_focused(window, cx)),
                "{key} moved focus out of the dialog"
            );
        }
    }
}
