//! The accessibility flags GPUI has no builder for, and the components that use them.
//! The test platform never turns on a screen reader, so these tests read each rendered
//! element's role and properties directly.

use gpui::accesskit::{Node, Role};
use gpui::{
    App, Context, Div, Element, Entity, Render, RenderOnce, Stateful, TestAppContext, Window, div,
};
use swarm_ui::a11y::{A11yFlags, Live, WithA11yFlags as _};
use swarm_ui::prelude::*;

struct A11yInfo {
    role: Option<Role>,
    node: Node,
}

fn info_of(element: &impl Element) -> A11yInfo {
    let role = element.a11y_role();
    let mut node = Node::new(role.unwrap_or(Role::Unknown));
    element.write_a11y_info(&mut node);

    A11yInfo { role, node }
}

fn a11y_info<C: RenderOnce>(component: C, window: &mut Window, cx: &mut App) -> A11yInfo {
    info_of(&component.render(window, cx).into_element())
}

fn view_a11y_info<V: Render>(view: &Entity<V>, window: &mut Window, cx: &mut App) -> A11yInfo {
    view.update(cx, |view, cx| {
        info_of(&view.render(window, cx).into_element())
    })
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

fn button_div() -> Stateful<Div> {
    div().id("flagged").role(Role::Button).aria_label("Run")
}

#[test]
fn a_disabled_flag_shows_on_the_node() {
    let flagged = button_div().a11y_flags().disabled(true);

    assert!(info_of(&flagged).node.is_disabled());
}

#[test]
fn a_busy_flag_shows_on_the_node() {
    let flagged = button_div().a11y_flags().busy(true);

    assert!(info_of(&flagged).node.is_busy());
}

#[test]
fn a_live_flag_shows_on_the_node() {
    let flagged = button_div().a11y_flags().live(Live::Assertive);

    assert_eq!(info_of(&flagged).node.live(), Some(Live::Assertive));
}

#[test]
fn a_modal_flag_shows_on_the_node() {
    let flagged = button_div().a11y_flags().modal(true);

    assert!(info_of(&flagged).node.is_modal());
}

#[test]
fn a_read_only_flag_shows_on_the_node() {
    let flagged = button_div().a11y_flags().read_only(true);

    assert!(info_of(&flagged).node.is_read_only());
}

#[test]
fn a_hidden_flag_shows_on_the_node() {
    let flagged = button_div().a11y_flags().hidden(true);

    assert!(info_of(&flagged).node.is_hidden());
}

#[test]
fn flags_that_are_off_leave_the_node_alone() {
    let flagged = button_div()
        .a11y_flags()
        .disabled(false)
        .busy(false)
        .modal(false)
        .read_only(false)
        .hidden(false);
    let node = info_of(&flagged).node;

    assert!(
        !node.is_disabled()
            && !node.is_busy()
            && !node.is_modal()
            && !node.is_read_only()
            && !node.is_hidden()
    );
}

#[test]
fn a_flagged_element_keeps_its_role_and_label() {
    let flagged = button_div().a11y_flags().disabled(true).busy(true);
    let info = info_of(&flagged);

    assert_eq!(info.role, Some(Role::Button));
    assert_eq!(info.node.label(), Some("Run"));
}

#[test]
fn a_flagged_element_keeps_its_id() {
    let flagged = button_div().a11y_flags();

    assert_eq!(Element::id(&flagged), Some("flagged".into()));
}

#[test]
fn a_status_region_has_a_value_and_speaks_politely() {
    with_window(|window, cx| {
        let info = a11y_info(LiveRegion::status("saved", "Settings saved"), window, cx);

        assert_eq!(info.role, Some(Role::Status));
        assert_eq!(info.node.value(), Some("Settings saved"));
        assert_eq!(info.node.live(), Some(Live::Polite));
    });
}

#[test]
fn an_empty_region_has_no_value() {
    with_window(|window, cx| {
        let info = a11y_info(LiveRegion::alert("failed", ""), window, cx);

        assert_eq!(info.node.value(), None);
        assert_eq!(info.node.live(), Some(Live::Assertive));
    });
}

#[test]
fn an_alert_region_has_a_value_and_speaks_at_once() {
    with_window(|window, cx| {
        let info = a11y_info(LiveRegion::alert("failed", "Merge failed"), window, cx);

        assert_eq!(info.role, Some(Role::Alert));
        assert_eq!(info.node.value(), Some("Merge failed"));
        assert_eq!(info.node.live(), Some(Live::Assertive));
    });
}

#[test]
fn a_toast_has_a_value_and_a_live_setting() {
    with_window(|window, cx| {
        let info = a11y_info(
            Toast::new("toast", "terminal-renderer", "Ready for review"),
            window,
            cx,
        );

        assert_eq!(
            info.node.value(),
            Some("terminal-renderer, Ready for review")
        );
        assert_eq!(info.node.live(), Some(Live::Polite));
    });
}

#[test]
fn a_plain_label_has_no_node() {
    with_window(|window, cx| {
        let info = a11y_info(Label::new("Plain"), window, cx);

        assert_eq!(info.role, None);
    });
}

#[test]
fn an_accessible_label_is_a_label_with_its_text_as_value() {
    with_window(|window, cx| {
        let mut element = Label::new("Command K")
            .accessible("shortcut")
            .render(window, cx)
            .into_any_element();
        let label = element
            .downcast_mut::<Stateful<Div>>()
            .expect("an accessible label is a node");
        let info = info_of(&*label);

        assert_eq!(info.role, Some(Role::Label));
        assert_eq!(info.node.value(), Some("Command K"));
    });
}

#[test]
fn a_heading_has_its_level_and_text() {
    with_window(|window, cx| {
        let mut element = Label::new("General")
            .heading("general-title", 2)
            .render(window, cx)
            .into_any_element();
        let heading = element
            .downcast_mut::<Stateful<Div>>()
            .expect("a heading is a node");
        let info = info_of(&*heading);

        assert_eq!(info.role, Some(Role::Heading));
        assert_eq!(info.node.level(), Some(2));
        assert_eq!(info.node.label(), Some("General"));
    });
}

#[test]
fn a_kbd_sets_its_value_as_well_as_its_label() {
    with_window(|window, cx| {
        let info = a11y_info(Kbd::new("kbd", ["\u{2318}", "K"]), window, cx);

        assert_eq!(info.node.label(), Some("Command K"));
        assert_eq!(info.node.value(), Some("Command K"));
    });
}

#[test]
fn a_dialog_panel_is_modal() {
    with_window(|_, cx| {
        let focus = cx.new(|cx| cx.focus_handle());
        let focus = focus.read(cx).clone();
        let panel = Dialog::new("d", "New task", &focus).modal_panel(cx);
        let info = info_of(&panel);

        assert_eq!(info.role, Some(Role::Dialog));
        assert!(info.node.is_modal());
    });
}

#[test]
fn a_loading_button_is_busy_and_reads_its_loading_text() {
    with_window(|window, cx| {
        let info = a11y_info(
            Button::new("remove", "Removing\u{2026}").loading(true),
            window,
            cx,
        );

        assert!(info.node.is_busy());
        assert_eq!(info.node.label(), Some("Removing\u{2026}"));
    });
}

#[test]
fn a_button_that_is_not_loading_is_not_busy() {
    with_window(|window, cx| {
        let info = a11y_info(Button::new("remove", "Remove"), window, cx);

        assert!(!info.node.is_busy());
    });
}

#[test]
fn a_disabled_button_is_flagged_disabled() {
    with_window(|window, cx| {
        let info = a11y_info(Button::new("run", "Run").disabled(true), window, cx);

        assert!(info.node.is_disabled());
    });
}

#[test]
fn a_disabled_icon_button_is_flagged_disabled() {
    with_window(|window, cx| {
        let info = a11y_info(
            IconButton::new("close", IconName::Close, "Close").disabled(true),
            window,
            cx,
        );

        assert!(info.node.is_disabled());
    });
}

#[test]
fn a_disabled_switch_is_flagged_disabled() {
    with_window(|window, cx| {
        let info = a11y_info(Switch::new("s", "Run").disabled(true), window, cx);

        assert!(info.node.is_disabled());
    });
}

#[test]
fn a_disabled_checkbox_is_flagged_disabled() {
    with_window(|window, cx| {
        let info = a11y_info(Checkbox::new("c", "Draft").disabled(true), window, cx);

        assert!(info.node.is_disabled());
    });
}

#[test]
fn a_disabled_dropdown_is_flagged_disabled() {
    with_window(|window, cx| {
        let mut element = Dropdown::new("d", "Branch", vec!["main".into()], 0)
            .disabled(true)
            .render(window, cx)
            .into_any_element();
        let trigger = element
            .downcast_mut::<A11yFlags<Stateful<Div>>>()
            .expect("a disabled dropdown is its flagged trigger");
        let info = info_of(&*trigger);

        assert_eq!(info.role, Some(Role::ComboBox));
        assert!(info.node.is_disabled());
    });
}

fn input(
    cx: &mut App,
    set: impl FnOnce(&mut TextInput, &mut Context<TextInput>) + 'static,
) -> Entity<TextInput> {
    cx.new(|cx| {
        let mut input = TextInput::new(cx);
        set(&mut input, cx);
        input
    })
}

#[test]
fn a_disabled_text_input_is_flagged_disabled() {
    with_window(|window, cx| {
        let field = input(cx, |input, cx| input.set_disabled(true, cx));
        let info = view_a11y_info(&field, window, cx);

        assert!(info.node.is_disabled());
    });
}

#[test]
fn a_read_only_text_input_is_flagged_read_only_and_not_disabled() {
    with_window(|window, cx| {
        let field = input(cx, |input, cx| input.set_read_only(true, cx));
        let info = view_a11y_info(&field, window, cx);

        assert!(info.node.is_read_only());
        assert!(!info.node.is_disabled());
    });
}

#[test]
fn an_editable_text_input_has_no_flags() {
    with_window(|window, cx| {
        let field = input(cx, |_, _| {});
        let node = view_a11y_info(&field, window, cx).node;

        assert!(!node.is_read_only() && !node.is_disabled());
    });
}
