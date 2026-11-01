//! Accessibility checks for Button, Kbd, Dropdown, `PopoverMenu`, the tray chips and `UsageMeter`.

use gpui::accesskit::{Node, Role, Toggled};
use gpui::{
    App, Context, Element as _, Render, RenderOnce, TestAppContext, VisualTestContext, Window, div,
    px,
};
use swarm_ui::prelude::*;

struct A11yInfo {
    role: Option<Role>,
    node: Node,
}

fn a11y_info<C: RenderOnce>(component: C, window: &mut Window, cx: &mut App) -> A11yInfo {
    let element = component.render(window, cx).into_element();
    let role = element.a11y_role();
    let mut node = Node::new(role.unwrap_or(Role::Unknown));
    element.write_a11y_info(&mut node);

    A11yInfo { role, node }
}

type Check = Box<dyn FnOnce(&mut Window, &mut App)>;

struct Probe {
    check: Option<Check>,
}

impl Render for Probe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(check) = self.check.take() {
            check(window, cx);
        }

        div()
    }
}

fn with_window(check: impl FnOnce(&mut Window, &mut App) + 'static) {
    let mut app = TestAppContext::single();
    app.update(swarm_ui::theme::init);

    let window = app.add_window(|_, _| Probe {
        check: Some(Box::new(check)),
    });
    app.run_until_parked();

    let ran = app.update(|cx| window.read(cx).map(|probe| probe.check.is_none()));
    assert!(matches!(ran, Ok(true)), "the probe view never rendered");
}

#[test]
fn button_without_toggle_state_reports_no_toggled() {
    with_window(|window, cx| {
        let info = a11y_info(Button::new("plain", "Changes"), window, cx);

        assert_eq!(info.node.toggled(), None);
    });
}

#[test]
fn button_toggled_off_reports_false() {
    with_window(|window, cx| {
        let info = a11y_info(
            Button::new("off", "Changes").toggle_state(false),
            window,
            cx,
        );

        assert_eq!(info.node.toggled(), Some(Toggled::False));
    });
}

#[test]
fn button_toggled_on_reports_true() {
    with_window(|window, cx| {
        let info = a11y_info(Button::new("on", "Changes").toggle_state(true), window, cx);

        assert_eq!(info.node.toggled(), Some(Toggled::True));
    });
}

#[test]
fn a_check_row_reads_its_name_status_and_value() {
    with_window(|window, cx| {
        let info = a11y_info(
            CheckRow::new("check", "cargo test", CheckStatus::Failed).value("exit 1"),
            window,
            cx,
        );

        assert_eq!(info.role, Some(Role::ListItem));
        assert_eq!(info.node.label(), Some("cargo test, Failed, exit 1"));

        let stopped = a11y_info(
            CheckRow::new("stopped", "cargo fmt", CheckStatus::Cancelled).status_label("Stopped"),
            window,
            cx,
        );

        assert_eq!(stopped.node.label(), Some("cargo fmt, Stopped"));

        let skipped = a11y_info(
            CheckRow::new("skipped", "cargo clippy", CheckStatus::Skipped),
            window,
            cx,
        );

        assert_eq!(skipped.node.label(), Some("cargo clippy, Skipped"));
    });
}

#[test]
fn a_usage_meter_is_one_labelled_group() {
    with_window(|window, cx| {
        let plain = a11y_info(
            UsageMeter::new("plain", "Claude · 5h")
                .value("42%")
                .fraction(0.42)
                .note("Rate limited · 24m"),
            window,
            cx,
        );

        assert_eq!(plain.role, Some(Role::Group));
        assert_eq!(
            plain.node.label(),
            Some("Claude · 5h, 42%, Rate limited · 24m")
        );

        let without_value = a11y_info(
            UsageMeter::new("none", "Codex").note("Not logged in"),
            window,
            cx,
        );

        assert_eq!(without_value.node.label(), Some("Codex, Not logged in"));

        let spoken = a11y_info(
            UsageMeter::new("spoken", "Claude · 5h")
                .value("42%")
                .accessible_label(
                    "Claude usage: 5 hour window 42%, resets in 2 hours; 7 day window 18%",
                ),
            window,
            cx,
        );

        assert_eq!(
            spoken.node.label(),
            Some("Claude usage: 5 hour window 42%, resets in 2 hours; 7 day window 18%")
        );
    });
}

#[test]
fn an_output_block_is_a_labelled_log_with_its_text() {
    with_window(|window, cx| {
        let info = a11y_info(
            OutputBlock::new("output", "Push output", "error: hook failed"),
            window,
            cx,
        );

        assert_eq!(info.role, Some(Role::Log));
        assert_eq!(info.node.label(), Some("Push output"));
        assert_eq!(info.node.value(), Some("error: hook failed"));
    });
}

#[test]
fn kbd_is_a_label_with_a_spoken_name() {
    with_window(|window, cx| {
        let info = a11y_info(Kbd::new("kbd", ["\u{2318}", "\u{21e7}", "R"]), window, cx);

        assert_eq!(info.role, Some(Role::Label));
        assert_eq!(info.node.label(), Some("Command Shift R"));
    });
}

#[test]
fn tray_more_chip_says_how_many_tiles_it_stands_for() {
    with_window(|window, cx| {
        let one = a11y_info(TrayMore::new("one", 1), window, cx);
        let four = a11y_info(TrayMore::new("four", 4), window, cx);

        assert_eq!(four.role, Some(Role::Button));
        assert_eq!(four.node.label(), Some("4 more tiles"));
        assert_eq!(one.node.label(), Some("1 more tile"));
    });
}

#[test]
fn a_long_name_stops_widening_a_tray_chip_at_its_cap() {
    with_window(|window, _| {
        let short = TrayChip::width("a", window);
        let long = TrayChip::width(&"w".repeat(200), window);
        let longer = TrayChip::width(&"w".repeat(400), window);

        assert!(short < long);
        assert_eq!(long, longer);
    });
}

struct Harness {
    dropdown: bool,
}

impl Render for Harness {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if self.dropdown {
            div()
                .debug_selector(|| "dropdown-host".to_owned())
                .size(px(200.))
                .child(Dropdown::new(
                    "dropdown",
                    "Branch from",
                    vec!["main".into(), "terminal-renderer".into()],
                    1,
                ))
                .into_any_element()
        } else {
            PopoverMenu::new(
                "popover",
                Button::new("trigger", "Open").debug_selector(|| "trigger".to_owned()),
                |_, _| Menu::new("menu", "Test menu").item(MenuItem::new("a", "A")),
            )
            .into_any_element()
        }
    }
}

fn harness(dropdown: bool) -> (TestAppContext, gpui::WindowHandle<Harness>) {
    let mut app = TestAppContext::single();
    app.update(swarm_ui::theme::init);

    let window = app.add_window(move |_, _| Harness { dropdown });

    (app, window)
}

#[test]
fn popover_menu_renders_its_trigger() {
    let (app, window) = harness(false);
    let mut cx = VisualTestContext::from_window(window.into(), &app);
    cx.run_until_parked();

    assert!(cx.debug_bounds("trigger").is_some());
}

#[test]
fn dropdown_renders_closed_with_its_trigger() {
    let (app, window) = harness(true);
    let mut cx = VisualTestContext::from_window(window.into(), &app);
    cx.run_until_parked();

    let host = cx.debug_bounds("dropdown-host");

    assert!(host.is_some_and(|bounds| bounds.size.height > px(0.)));
}
