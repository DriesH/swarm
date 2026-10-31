//! Every interactive component exposes an AccessKit role and label.
//! GPUI's test platform never turns on a screen reader, so these tests read the
//! role and properties straight from each rendered element.

use gpui::accesskit::{Node, Role, Toggled};
use gpui::{App, Context, Element as _, Render, RenderOnce, TestAppContext, Window, div};
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

/// Runs a check inside a view's render, where GPUI allows element state
/// (`SegmentedControl` keeps its glide state there).
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
fn button_is_a_labelled_button() {
    with_window(|window, cx| {
        let info = a11y_info(Button::new("review", "Review"), window, cx);

        assert_eq!(info.role, Some(Role::Button));
        assert_eq!(info.node.label(), Some("Review"));
    });
}

#[test]
fn icon_button_uses_its_required_label() {
    with_window(|window, cx| {
        let info = a11y_info(
            IconButton::new("min", IconName::Minimize, "Move pty-io to tray"),
            window,
            cx,
        );

        assert_eq!(info.role, Some(Role::Button));
        assert_eq!(info.node.label(), Some("Move pty-io to tray"));

        let pressed = a11y_info(
            IconButton::new("side", IconName::Sidebar, "Toggle sidebar").toggle_state(true),
            window,
            cx,
        );

        assert_eq!(pressed.node.toggled(), Some(Toggled::True));
    });
}

#[test]
fn toggle_icon_button_that_is_off_reports_not_toggled() {
    with_window(|window, cx| {
        let off = a11y_info(
            IconButton::new("side", IconName::Sidebar, "Toggle sidebar").toggle_state(false),
            window,
            cx,
        );
        let plain = a11y_info(
            IconButton::new("plain", IconName::Plus, "New task"),
            window,
            cx,
        );

        assert_eq!(off.node.toggled(), Some(Toggled::False));
        assert_eq!(plain.node.toggled(), None);
    });
}

#[test]
fn switch_reports_its_state() {
    with_window(|window, cx| {
        let on = a11y_info(
            Switch::new("s", "Run setup script").toggle_state(true),
            window,
            cx,
        );

        assert_eq!(on.role, Some(Role::Switch));
        assert_eq!(on.node.label(), Some("Run setup script"));
        assert_eq!(on.node.toggled(), Some(Toggled::True));

        let off = a11y_info(Switch::new("s", "Run setup script"), window, cx);

        assert_eq!(off.node.toggled(), Some(Toggled::False));
    });
}

#[test]
fn segmented_control_is_a_radio_group() {
    with_window(|window, cx| {
        let info = a11y_info(
            SegmentedControl::new("view", "View", ["Terminals", "Swarm"]),
            window,
            cx,
        );

        assert_eq!(info.role, Some(Role::RadioGroup));
        assert_eq!(info.node.label(), Some("View"));
    });
}

#[test]
fn tab_reports_name_status_and_selection() {
    with_window(|window, cx| {
        let info = a11y_info(
            Tab::new("t", "menu-bar-usage", AgentStatus::Blocked).toggle_state(true),
            window,
            cx,
        );

        assert_eq!(info.role, Some(Role::Tab));
        assert_eq!(info.node.label(), Some("menu-bar-usage, Blocked"));
        assert_eq!(info.node.is_selected(), Some(true));
    });
}

#[test]
fn list_row_is_a_selectable_list_item() {
    with_window(|window, cx| {
        let info = a11y_info(
            ListRow::new("r", "pty-io")
                .accessible_label("pty-io, Blocked")
                .toggle_state(true),
            window,
            cx,
        );

        assert_eq!(info.role, Some(Role::ListItem));
        assert_eq!(info.node.label(), Some("pty-io, Blocked"));
        assert_eq!(info.node.is_selected(), Some(true));
    });
}

#[test]
fn tray_chip_says_what_it_restores() {
    with_window(|window, cx| {
        let info = a11y_info(
            TrayChip::new("c", IconName::Codex, "bench", AgentStatus::Idle),
            window,
            cx,
        );

        assert_eq!(info.role, Some(Role::Button));
        assert_eq!(info.node.label(), Some("Show bench, idle"));
    });
}

#[test]
fn tile_frame_is_a_named_group() {
    with_window(|window, cx| {
        let info = a11y_info(TileFrame::new("t", IconName::Codex, "pty-io"), window, cx);

        assert_eq!(info.role, Some(Role::Group));
        assert_eq!(info.node.label(), Some("pty-io terminal"));
    });
}

#[test]
fn checkbox_reports_its_state() {
    with_window(|window, cx| {
        let checked = a11y_info(
            Checkbox::new("c", "Open as draft").toggle_state(true),
            window,
            cx,
        );

        assert_eq!(checked.role, Some(Role::CheckBox));
        assert_eq!(checked.node.label(), Some("Open as draft"));
        assert_eq!(checked.node.toggled(), Some(Toggled::True));

        let unchecked = a11y_info(Checkbox::new("c", "Open as draft"), window, cx);

        assert_eq!(unchecked.node.toggled(), Some(Toggled::False));
    });
}

#[test]
fn dialog_is_a_labelled_dialog() {
    with_window(|_, cx| {
        let focus = cx.new(|cx| cx.focus_handle());
        let focus = focus.read(cx).clone();
        let panel = Dialog::new("d", "New task", &focus).panel(cx);
        let role = panel.a11y_role();
        let mut node = Node::new(role.unwrap_or(Role::Unknown));
        panel.write_a11y_info(&mut node);
        let info = A11yInfo { role, node };

        assert_eq!(info.role, Some(Role::Dialog));
        assert_eq!(info.node.label(), Some("New task"));
    });
}

#[test]
fn menu_is_a_labelled_menu_of_items() {
    with_window(|window, cx| {
        let menu = Menu::new("m", "New tab")
            .item(MenuItem::new("a", "New task"))
            .separator()
            .item(MenuItem::new("b", "Plan with orchestrator").checked(true));
        let info = a11y_info(menu, window, cx);

        assert_eq!(info.role, Some(Role::Menu));
        assert_eq!(info.node.label(), Some("New tab"));
    });
}

fn view_a11y_info<V: Render>(
    view: &gpui::Entity<V>,
    window: &mut Window,
    cx: &mut App,
) -> A11yInfo {
    view.update(cx, |view, cx| {
        let element = view.render(window, cx).into_element();
        let role = element.a11y_role();
        let mut node = Node::new(role.unwrap_or(Role::Unknown));
        element.write_a11y_info(&mut node);

        A11yInfo { role, node }
    })
}

#[test]
fn text_input_reports_its_label_and_text() {
    with_window(|window, cx| {
        let input = cx.new(|cx| {
            let mut input = TextInput::new(cx);
            input.set_label("Name", cx);
            input.set_text("docs-site", cx);
            input
        });
        let info = view_a11y_info(&input, window, cx);

        assert_eq!(info.role, Some(Role::TextInput));
        assert_eq!(info.node.label(), Some("Name"));
        assert_eq!(info.node.value(), Some("docs-site"));
    });
}

#[test]
fn text_area_is_a_multiline_text_input() {
    with_window(|window, cx| {
        let area = cx.new(|cx| {
            let mut area = TextArea::new(cx);
            area.set_label("First message", cx);
            area
        });
        let info = view_a11y_info(&area, window, cx);

        assert_eq!(info.role, Some(Role::MultilineTextInput));
        assert_eq!(info.node.label(), Some("First message"));
    });
}

#[test]
fn tab_strip_is_a_labelled_tab_list() {
    with_window(|window, cx| {
        let info = a11y_info(
            TabStrip::new("strip", "Inspector")
                .tab_with_count("Messages", "3")
                .tab("Terminal"),
            window,
            cx,
        );

        assert_eq!(info.role, Some(Role::TabList));
        assert_eq!(info.node.label(), Some("Inspector"));
    });
}

#[test]
fn resize_handle_is_a_splitter_with_its_width() {
    with_window(|window, cx| {
        let focus = cx.focus_handle();
        let info = a11y_info(
            ResizeHandle::new("split", "Resize inspector", &focus).value(400., 320., 640.),
            window,
            cx,
        );

        assert_eq!(info.role, Some(Role::Splitter));
        assert_eq!(info.node.label(), Some("Resize inspector"));
        assert_eq!(info.node.value(), Some("400 pixels"));
        assert_eq!(info.node.numeric_value(), Some(400.));
        assert_eq!(info.node.min_numeric_value(), Some(320.));
        assert_eq!(info.node.max_numeric_value(), Some(640.));
    });
}
