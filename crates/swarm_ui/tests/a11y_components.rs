//! Accessibility checks for Button, Kbd, Stepper, Dropdown, `PopoverMenu`, the tray chips and
//! `UsageMeter` and `UsageDetailMeter`.

use gpui::accesskit::{Action, Node, Role, Toggled};
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
fn a_system_check_row_reads_its_name_state_and_detail() {
    with_window(|window, cx| {
        let ready = a11y_info(
            SystemCheckRow::new("git", "Git", SystemCheckState::Ready).detail("installed"),
            window,
            cx,
        );

        assert_eq!(ready.role, Some(Role::ListItem));
        assert_eq!(ready.node.label(), Some("Git, Ready, installed"));

        let running = a11y_info(
            SystemCheckRow::new("gh", "GitHub CLI", SystemCheckState::InProgress)
                .detail("installing with Homebrew"),
            window,
            cx,
        );

        assert_eq!(
            running.node.label(),
            Some("GitHub CLI, In progress, installing with Homebrew")
        );

        let bare = a11y_info(
            SystemCheckRow::new("codex", "Codex", SystemCheckState::NotSetUp),
            window,
            cx,
        );

        assert_eq!(bare.node.label(), Some("Codex, Not set up"));
    });
}

#[test]
fn a_button_hint_is_read_after_the_label() {
    with_window(|window, cx| {
        let hinted = a11y_info(
            Button::new("install", "Install")
                .accessible_label("Install GitHub CLI")
                .hint("/opt/homebrew/bin/brew install gh"),
            window,
            cx,
        );
        let plain = a11y_info(Button::new("plain", "Install"), window, cx);

        assert_eq!(hinted.node.label(), Some("Install GitHub CLI"));
        assert_eq!(
            hinted.node.description(),
            Some("/opt/homebrew/bin/brew install gh")
        );
        assert_eq!(plain.node.description(), None);
    });
}

#[test]
fn a_usage_meter_is_one_labelled_meter() {
    with_window(|window, cx| {
        let plain = a11y_info(
            UsageMeter::new("plain", "Session")
                .value("42%")
                .fraction(0.42)
                .pace(0.58),
            window,
            cx,
        );

        assert_eq!(plain.role, Some(Role::Meter));
        assert_eq!(plain.node.label(), Some("Session, 42%"));
        assert_eq!(plain.node.numeric_value(), Some(42.));
        assert_eq!(plain.node.min_numeric_value(), Some(0.));
        assert_eq!(plain.node.max_numeric_value(), Some(100.));

        let without_value = a11y_info(UsageMeter::new("none", "Weekly"), window, cx);

        assert_eq!(without_value.node.label(), Some("Weekly"));
        assert_eq!(without_value.role, Some(Role::Group));
        assert_eq!(without_value.node.numeric_value(), None);

        let spoken = a11y_info(
            UsageMeter::new("spoken", "Weekly")
                .value("61%")
                .fraction(0.61)
                .pace(0.45)
                .tone(UsageTone::Warn)
                .accessible_label(
                    "Claude weekly 61% used, limit in 3 days 21 hours, resets in 6 days 11 hours",
                ),
            window,
            cx,
        );

        assert_eq!(
            spoken.node.label(),
            Some("Claude weekly 61% used, limit in 3 days 21 hours, resets in 6 days 11 hours")
        );
    });
}

#[test]
fn a_detailed_usage_meter_is_one_labelled_meter() {
    with_window(|window, cx| {
        let plain = a11y_info(
            UsageDetailMeter::new("plain", "Weekly")
                .value("61% used")
                .fraction(0.61)
                .pace(0.45)
                .warning("Limit in 3d 21h")
                .note("Resets in 6d 11h"),
            window,
            cx,
        );

        assert_eq!(plain.role, Some(Role::Meter));
        assert_eq!(
            plain.node.label(),
            Some("Weekly, 61% used, Limit in 3d 21h, Resets in 6d 11h")
        );
        assert_eq!(plain.node.numeric_value(), Some(61.));
        assert_eq!(plain.node.min_numeric_value(), Some(0.));
        assert_eq!(plain.node.max_numeric_value(), Some(100.));

        let without_bar = a11y_info(
            UsageDetailMeter::new("none", "Weekly").value("61% used"),
            window,
            cx,
        );

        assert_eq!(without_bar.role, Some(Role::Group));
        assert_eq!(without_bar.node.numeric_value(), None);

        let spoken = a11y_info(
            UsageDetailMeter::new("spoken", "Weekly")
                .warning("Limit in 3d 21h")
                .accessible_label("Claude weekly 61% used, limit in 3 days 21 hours"),
            window,
            cx,
        );

        assert_eq!(
            spoken.node.label(),
            Some("Claude weekly 61% used, limit in 3 days 21 hours")
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
fn a_stepper_is_a_spin_button_with_its_value_range_and_actions() {
    with_window(|window, cx| {
        let info = a11y_info(
            Stepper::new("size", "Font size", 13, 11, 20)
                .unit("pt")
                .on_change(|_, _, _| {}),
            window,
            cx,
        );

        assert_eq!(info.role, Some(Role::SpinButton));
        assert_eq!(info.node.label(), Some("Font size"));
        assert_eq!(info.node.value(), Some("13 pt"));
        assert_eq!(info.node.numeric_value(), Some(13.));
        assert_eq!(info.node.min_numeric_value(), Some(11.));
        assert_eq!(info.node.max_numeric_value(), Some(20.));
        assert_eq!(info.node.numeric_value_step(), Some(1.));
        assert!(info.node.supports_action(Action::Increment));
        assert!(info.node.supports_action(Action::Decrement));
    });
}

#[test]
fn a_stepper_at_a_limit_offers_only_the_way_back() {
    with_window(|window, cx| {
        let mut stepper = |value| {
            a11y_info(
                Stepper::new("size", "Font size", value, 11, 20).on_change(|_, _, _| {}),
                window,
                cx,
            )
        };

        let (lowest, highest) = (stepper(11), stepper(20));

        assert!(lowest.node.supports_action(Action::Increment));
        assert!(!lowest.node.supports_action(Action::Decrement));
        assert!(highest.node.supports_action(Action::Decrement));
        assert!(!highest.node.supports_action(Action::Increment));
    });
}

#[test]
fn the_buttons_of_a_stepper_are_named_for_what_they_change() {
    with_window(|window, cx| {
        let [decrease, increase] = Stepper::new("size", "Font size", 13, 11, 20).buttons();

        let decrease = a11y_info(decrease, window, cx);
        let increase = a11y_info(increase, window, cx);

        assert_eq!(decrease.role, Some(Role::Button));
        assert_eq!(decrease.node.label(), Some("Decrease font size"));
        assert_eq!(increase.role, Some(Role::Button));
        assert_eq!(increase.node.label(), Some("Increase font size"));
    });
}

#[test]
fn a_button_at_a_limit_cannot_be_focused() {
    with_window(|window, cx| {
        let [decrease, increase] = Stepper::new("size", "Font size", 20, 11, 20).buttons();

        let decrease = a11y_info(decrease, window, cx);
        let increase = a11y_info(increase, window, cx);

        assert!(decrease.node.supports_action(Action::Focus));
        assert!(!increase.node.supports_action(Action::Focus));
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

#[test]
fn a_toast_is_a_status_named_by_its_title_and_message() {
    with_window(|window, cx| {
        let info = a11y_info(
            Toast::new("toast", "terminal-renderer", "Ready for review")
                .action("Review", |_, _, _| {})
                .on_dismiss(|_, _| {}),
            window,
            cx,
        );

        assert_eq!(info.role, Some(Role::Status));
        assert_eq!(
            info.node.label(),
            Some("terminal-renderer, Ready for review")
        );
    });
}

#[test]
fn the_pixel_bee_and_the_honey_fill_have_no_role() {
    with_window(|window, cx| {
        assert_eq!(a11y_info(PixelBee::new("bee"), window, cx).role, None);
        assert_eq!(
            a11y_info(PixelBee::new("flying-bee").flap(true), window, cx).role,
            None
        );
        assert_eq!(a11y_info(HoneyFill::new("honey"), window, cx).role, None);
    });
}
