//! The update card: in the sidebar, or behind a button of the rail, while an update is ready
//! and the user has not dismissed it for that version.

use gpui::{
    Anchor, Animation, AnimationExt, AnyElement, App, Context, FontWeight, KeyDownEvent,
    MouseDownEvent, SharedString, Window, anchored, deferred, div, ease_out_quint, px, relative,
};
use swarm_ui::motion::reduce_motion;
use swarm_ui::prelude::*;
use swarm_ui::{FocusNext, FocusPrevious};
use swarm_update::Version;

use crate::announce::announce;
use crate::sidebar::{CARD_WIDTH, HOVER_CARD_IN};
use crate::updates::{self, ReadyUpdate};
use crate::workspace::Workspace;

impl Workspace {
    /// The ready update, unless the user dismissed this version.
    pub(crate) fn shown_update(&self, cx: &App) -> Option<ReadyUpdate> {
        updates::ready(cx).filter(|ready| self.update_card_dismissed != Some(ready.version))
    }

    /// Hides the card and the rail's popover until a newer version is ready.
    fn dismiss_update(&mut self, version: Version, window: &mut Window, cx: &mut Context<Self>) {
        self.update_card_dismissed = Some(version);
        self.rail_update_open = false;
        self.sidebar_focus.focus(window, cx);

        cx.notify();
    }

    fn toggle_rail_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rail_update_open {
            self.close_rail_update(window, cx);
        } else {
            self.rail_update_open = true;
            self.rail_update_card_focus.focus(window, cx);
            cx.notify();
        }
    }

    /// Closes the rail's card. Focus goes back to the button when it sat in the card.
    fn close_rail_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.rail_update_open {
            return;
        }

        if self.rail_update_card_focus.contains_focused(window, cx) {
            self.rail_update_button_focus.focus(window, cx);
        }

        self.rail_update_open = false;
        cx.notify();
    }

    /// Moves focus on with Tab. Tab out of the card closes it.
    fn tab_in_rail_update(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if forward {
            window.focus_next(cx);
        } else {
            window.focus_prev(cx);
        }

        if !self.rail_update_card_focus.contains_focused(window, cx) {
            self.rail_update_card_focus.focus(window, cx);
            self.close_rail_update(window, cx);
        }
    }
}

/// What VoiceOver reads for the card.
fn card_label(ready: &ReadyUpdate) -> SharedString {
    match &ready.failure {
        Some(reason) => format!("Update failed, {reason}").into(),
        None => format!("Update ready, Swarm {}", ready.version).into(),
    }
}

/// What the live region says: nothing, until an update is ready.
fn spoken(ready: Option<&ReadyUpdate>) -> SharedString {
    ready.map_or_else(SharedString::default, |ready| match &ready.failure {
        Some(reason) => format!("Update failed: {reason}").into(),
        None => "Update ready".into(),
    })
}

/// The live region of the update. It is always drawn, so VoiceOver speaks its value once,
/// when the update becomes ready, whatever the layout shows.
pub(crate) fn announcement(cx: &App) -> gpui::Div {
    announce(
        "update-status",
        LiveRegion::status("update-ready-status", spoken(updates::ready(cx).as_ref())),
    )
}

/// The card: what is ready, **Update now** (or **Download**) and **Release notes**.
pub(crate) fn card(ready: &ReadyUpdate, cx: &mut Context<Workspace>) -> AnyElement {
    let version = ready.version;
    let failed = ready.failure.is_some();

    let (title, detail, tone) = match &ready.failure {
        Some(reason) => ("Update failed", reason.clone(), Color::Warn),
        None => (
            "Update ready",
            SharedString::from(format!("Swarm {version}")),
            Color::Accent,
        ),
    };

    let heading = h_flex()
        .gap(px(8.))
        .child(Icon::new(IconName::Download).color(tone))
        .child(
            Label::new(title)
                .size(LabelSize::Small)
                .weight(FontWeight::SEMIBOLD)
                .color(if failed { Color::Warn } else { Color::Default }),
        )
        .child(div().flex_1())
        .child(
            IconButton::new(
                "update-card-dismiss",
                IconName::Close,
                "Dismiss update notice",
            )
            .size(IconButtonSize::Small)
            .color(Color::Muted)
            .debug_selector(|| "update-card-dismiss".to_owned())
            .on_click(cx.listener(move |workspace, _, window, cx| {
                workspace.dismiss_update(version, window, cx);
            })),
        );

    let notes_url = ready.notes_url.clone();
    let action = if ready.replaceable {
        Button::new("update-card-now", "Update now")
            .variant(ButtonVariant::Primary)
            .on_click(|_, _, cx| updates::restart_to_update(cx))
    } else {
        let download_url = notes_url.clone();

        Button::new("update-card-now", "Download")
            .variant(ButtonVariant::Secondary)
            .on_click(move |_, _, cx| cx.open_url(&download_url))
    };

    let actions = h_flex()
        .flex_wrap()
        .gap(px(6.))
        .child(
            action
                .size(ButtonSize::Small)
                .debug_selector(|| "update-card-now".to_owned()),
        )
        .child(
            Button::new("update-card-notes", "Release notes")
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Small)
                .icon(IconName::ExternalLink)
                .debug_selector(|| "update-card-notes".to_owned())
                .on_click(move |_, _, cx| cx.open_url(&notes_url)),
        );

    let content = v_flex()
        .gap(px(8.))
        .px(px(12.))
        .py(px(10.))
        .child(heading)
        .child(
            Label::new(detail)
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
        .child(actions);

    let card = div()
        .debug_selector(|| "update-card".to_owned())
        .relative()
        .child(card_panel(ready, content));

    if reduce_motion(cx) {
        return card.into_any_element();
    }

    card.with_animation(
        SharedString::from(format!("update-card-in-{version}")),
        Animation::new(HOVER_CARD_IN).with_easing(ease_out_quint()),
        |card, progress| card.opacity(progress).top(px((1. - progress) * 6.)),
    )
    .into_any_element()
}

/// The surface of the card, a group named after what is ready.
fn card_panel(ready: &ReadyUpdate, content: impl IntoElement) -> Panel {
    Panel::new()
        .accessible("update-card", card_label(ready))
        .row(content)
}

/// The button of the folded sidebar that opens the card.
fn rail_button(
    workspace: &Workspace,
    ready: &ReadyUpdate,
    cx: &mut Context<Workspace>,
) -> RailButton {
    let label = card_label(ready);
    let tone = if ready.failure.is_some() {
        Color::Warn
    } else {
        Color::Accent
    };
    let tooltip = if ready.failure.is_some() {
        "Update failed"
    } else {
        "Update ready"
    };

    RailButton::new(
        "rail-update",
        label,
        Icon::new(IconName::Download)
            .size(IconSize::Large)
            .color(tone),
    )
    .track_focus(&workspace.rail_update_button_focus)
    .tooltip(tooltip)
    .debug_selector(|| "rail-update".to_owned())
    .on_click(cx.listener(|workspace, _, window, cx| {
        workspace.toggle_rail_update(window, cx);
    }))
}

/// The rail button, with the card in a popover to its right while open.
pub(crate) fn rail_entry(
    workspace: &Workspace,
    ready: &ReadyUpdate,
    cx: &mut Context<Workspace>,
) -> impl IntoElement + use<> {
    let button = rail_button(workspace, ready, cx);

    let popover = workspace.rail_update_open.then(|| {
        let card = card(ready, cx);

        div()
            .absolute()
            .left(relative(1.))
            .bottom_0()
            .ml(px(8.))
            .child(
                deferred(
                    anchored()
                        .anchor(Anchor::BottomLeft)
                        .snap_to_window_with_margin(px(8.))
                        .child(
                            div()
                                .w(CARD_WIDTH)
                                .track_focus(&workspace.rail_update_card_focus)
                                .on_action(cx.listener(|workspace, _: &FocusNext, window, cx| {
                                    workspace.tab_in_rail_update(true, window, cx);
                                }))
                                .on_action(cx.listener(
                                    |workspace, _: &FocusPrevious, window, cx| {
                                        workspace.tab_in_rail_update(false, window, cx);
                                    },
                                ))
                                .on_mouse_down_out(cx.listener(
                                    |workspace, event: &MouseDownEvent, window, cx| {
                                        let on_button = workspace
                                            .rail_update_trigger
                                            .is_some_and(|bounds| bounds.contains(&event.position));

                                        if !on_button {
                                            workspace.close_rail_update(window, cx);
                                        }
                                    },
                                ))
                                .child(card),
                        ),
                )
                .priority(1),
            )
    });

    let workspace = cx.entity();

    div()
        .relative()
        .on_key_down(cx.listener(|workspace, event: &KeyDownEvent, window, cx| {
            if workspace.rail_update_open && event.keystroke.key == "escape" {
                workspace.close_rail_update(window, cx);
                cx.stop_propagation();
            }
        }))
        .on_children_prepainted(move |bounds, _, cx| {
            workspace.update(cx, |workspace, _| {
                workspace.rail_update_trigger = bounds.first().copied();
            });
        })
        .child(button)
        .children(popover)
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::sync::Arc;

    use gpui::{
        Bounds, KeyUpEvent, Keystroke, Modifiers, Pixels, Role, TestAppContext, VisualTestContext,
        px,
    };

    use super::*;
    use crate::updates::Updater;
    use crate::updates::tests::{FakeHost, FakeSource, install_fake, signed_build};

    const NOTES: &str = "https://github.com/DriesH/swarm/releases/tag/v1.2.0";

    fn setup(cx: &mut TestAppContext) -> (gpui::Entity<Workspace>, &mut VisualTestContext) {
        cx.update(|cx| {
            swarm_ui::init(cx);
            crate::workspace::follow_test_reduce_motion(true, cx);
        });
        let (workspace, cx) = cx.add_window_view(Workspace::with_sample);
        cx.simulate_resize(gpui::size(px(1100.), px(900.)));
        redraw(cx);

        (workspace, cx)
    }

    fn redraw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
    }

    fn bounds_of(selector: &'static str, cx: &mut VisualTestContext) -> Bounds<Pixels> {
        let bounds = cx.debug_bounds(selector);
        assert!(bounds.is_some(), "{selector} is not on screen");

        bounds.unwrap_or_default()
    }

    fn shown(selector: &'static str, cx: &mut VisualTestContext) -> bool {
        cx.debug_bounds(selector).is_some()
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let center = bounds_of(selector, cx).center();

        cx.simulate_click(center, Modifiers::none());
        redraw(cx);
    }

    /// An updater that found release 1.2.0 and prepared it. `source` decides how.
    fn ready_with(
        source: &Arc<FakeSource>,
        replaceable: bool,
        workspace: &gpui::Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Rc<FakeHost> {
        let host = FakeHost::new(replaceable);
        let updater = cx.update(|_, cx| install_fake(workspace, source, &host, signed_build(), cx));
        updater.update(cx, Updater::check);
        redraw(cx);

        host
    }

    fn ready_update(
        replaceable: bool,
        workspace: &gpui::Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Rc<FakeHost> {
        ready_with(&FakeSource::with_update(), replaceable, workspace, cx)
    }

    fn collapse(workspace: &gpui::Entity<Workspace>, cx: &mut VisualTestContext) {
        workspace.update_in(cx, Workspace::toggle_sidebar);
        redraw(cx);
    }

    #[gpui::test]
    fn the_card_shows_when_an_update_is_ready_and_not_before(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let source = FakeSource::with_update();
        let host = FakeHost::new(true);
        let updater =
            cx.update(|_, cx| install_fake(&workspace, &source, &host, signed_build(), cx));
        redraw(cx);
        let before = shown("update-card", cx);

        updater.update(cx, Updater::check);
        redraw(cx);

        assert!(!before);
        assert!(shown("update-card", cx));
        assert!(shown("update-card-now", cx));
        assert!(shown("update-card-notes", cx));
        assert!(shown("update-card-dismiss", cx));
    }

    #[gpui::test]
    fn the_card_slides_in_and_ends_in_place_with_motion_on(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.update(|_, cx| crate::workspace::follow_test_reduce_motion(false, cx));
        ready_update(true, &workspace, cx);
        let start = bounds_of("update-card", cx).top();

        cx.executor().advance_clock(HOVER_CARD_IN * 2);
        redraw(cx);
        let end = bounds_of("update-card", cx).top();

        assert!(start > end, "{end:?} {start:?}");
        assert!(start - end <= px(6.));
    }

    #[gpui::test]
    fn the_card_names_the_version(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        ready_update(true, &workspace, cx);

        let ready = cx.update(|_, cx| updates::ready(cx)).expect("ready");

        assert_eq!(card_label(&ready), "Update ready, Swarm 1.2.0");
    }

    #[gpui::test]
    fn a_failed_install_names_the_reason(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        ready_with(
            &FakeSource::with_failed_install("the signature is wrong"),
            true,
            &workspace,
            cx,
        );

        let ready = cx.update(|_, cx| updates::ready(cx)).expect("ready");

        assert_eq!(card_label(&ready), "Update failed, the signature is wrong");
        assert_eq!(
            spoken(Some(&ready)),
            "Update failed: the signature is wrong"
        );
    }

    #[gpui::test]
    fn update_now_starts_the_helper_and_quits(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let host = ready_update(true, &workspace, cx);

        click("update-card-now", cx);

        assert_eq!(host.launches.borrow().len(), 1);
        assert!(host.launches.borrow()[0].1, "the app reopens");
        assert_eq!(host.quits.get(), 1);
    }

    #[gpui::test]
    fn download_opens_the_notes_when_the_app_cannot_replace_itself(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let host = ready_update(false, &workspace, cx);

        click("update-card-now", cx);

        assert!(host.launches.borrow().is_empty());
        assert_eq!(host.quits.get(), 0);
        assert_eq!(cx.opened_url().as_deref(), Some(NOTES));
    }

    #[gpui::test]
    fn download_replaces_update_now_after_a_failed_install(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let host = ready_with(
            &FakeSource::with_failed_install("the signature is wrong"),
            true,
            &workspace,
            cx,
        );

        click("update-card-now", cx);

        assert!(host.launches.borrow().is_empty());
        assert_eq!(cx.opened_url().as_deref(), Some(NOTES));
    }

    #[gpui::test]
    fn release_notes_opens_the_notes_of_the_release(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        ready_update(true, &workspace, cx);

        click("update-card-notes", cx);

        assert_eq!(cx.opened_url().as_deref(), Some(NOTES));
    }

    #[gpui::test]
    fn dismiss_hides_the_card(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        ready_update(true, &workspace, cx);

        click("update-card-dismiss", cx);

        let dismissed = workspace.read_with(cx, |workspace, _| workspace.update_card_dismissed);
        assert_eq!(dismissed, Some(Version::new(1, 2, 0)));
        assert!(!shown("update-card", cx));
    }

    #[gpui::test]
    fn a_newer_version_shows_the_card_again(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, _| {
            workspace.update_card_dismissed = Some(Version::new(1, 1, 0));
        });
        ready_update(true, &workspace, cx);

        assert!(shown("update-card", cx));
    }

    #[gpui::test]
    fn the_same_version_stays_dismissed(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, _| {
            workspace.update_card_dismissed = Some(Version::new(1, 2, 0));
        });
        ready_update(true, &workspace, cx);

        assert!(!shown("update-card", cx));
    }

    #[gpui::test]
    fn the_card_sits_above_the_usage_and_the_links_inside_the_sidebar(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        ready_update(true, &workspace, cx);

        let card = bounds_of("update-card", cx);
        let usage = bounds_of("usage-card", cx);
        let links = bounds_of("footer-settings", cx);
        let column = bounds_of("sidebar-column", cx);

        assert!(card.bottom() <= usage.top(), "{card:?} {usage:?}");
        assert!(usage.bottom() <= links.top());
        assert!(card.left() >= column.left() && card.right() <= column.right());
    }

    #[gpui::test]
    fn the_rail_button_shows_only_while_an_update_is_ready(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        collapse(&workspace, cx);
        let before = shown("rail-update", cx);

        ready_update(true, &workspace, cx);

        assert!(!before);
        assert!(shown("rail-update", cx));
        assert!(
            !shown("update-card", cx),
            "the card waits behind the button"
        );
    }

    #[gpui::test]
    fn the_rail_button_is_above_the_usage_ring(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        collapse(&workspace, cx);
        ready_update(true, &workspace, cx);

        let button = bounds_of("rail-update", cx);
        let ring = bounds_of("rail-usage", cx);

        assert!(button.bottom() <= ring.top());
    }

    #[gpui::test]
    fn the_rail_button_opens_the_card_to_the_right_of_the_rail(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        collapse(&workspace, cx);
        ready_update(true, &workspace, cx);

        click("rail-update", cx);

        let button = bounds_of("rail-update", cx);
        let card = bounds_of("update-card", cx);
        assert!(card.left() > button.right(), "{card:?} {button:?}");
        assert!(
            (card.bottom() - button.bottom()).abs() <= px(1.),
            "{card:?} {button:?}"
        );
    }

    #[gpui::test]
    fn the_rail_button_closes_the_card_it_opened(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        collapse(&workspace, cx);
        ready_update(true, &workspace, cx);

        click("rail-update", cx);
        click("rail-update", cx);

        assert!(!shown("update-card", cx));
    }

    #[gpui::test]
    fn a_click_outside_closes_the_rail_card(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        collapse(&workspace, cx);
        ready_update(true, &workspace, cx);
        click("rail-update", cx);
        let open = shown("update-card", cx);

        cx.simulate_click(gpui::point(px(700.), px(400.)), Modifiers::none());
        redraw(cx);

        assert!(open);
        assert!(!shown("update-card", cx));
    }

    #[gpui::test]
    fn a_click_inside_the_rail_card_keeps_it_open(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        collapse(&workspace, cx);
        ready_update(true, &workspace, cx);
        click("rail-update", cx);

        let card = bounds_of("update-card", cx);
        cx.simulate_click(card.origin + gpui::point(px(4.), px(4.)), Modifiers::none());
        redraw(cx);

        assert!(shown("update-card", cx));
    }

    #[gpui::test]
    fn escape_closes_the_rail_card(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        collapse(&workspace, cx);
        ready_update(true, &workspace, cx);
        click("rail-update", cx);

        cx.simulate_keystrokes("escape");
        redraw(cx);

        assert!(!shown("update-card", cx));
        assert!(shown("rail-update", cx));
    }

    #[gpui::test]
    fn dismiss_in_the_rail_card_hides_the_card_and_the_button(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        collapse(&workspace, cx);
        ready_update(true, &workspace, cx);
        click("rail-update", cx);

        click("update-card-dismiss", cx);

        assert!(!shown("update-card", cx));
        assert!(!shown("rail-update", cx));
    }

    /// GPUI turns Enter on a focused button into a click when the key goes up.
    fn press_enter(cx: &mut VisualTestContext) {
        let keystroke = Keystroke::parse("enter").expect("enter is a key");

        cx.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(KeyUpEvent { keystroke });
    }

    /// The role and the label of the node an element writes.
    fn node_of(element: &impl Element) -> (Option<Role>, String) {
        let mut node = gpui::accesskit::Node::new(Role::Unknown);
        element.write_a11y_info(&mut node);

        (
            element.a11y_role(),
            node.label().map(str::to_owned).unwrap_or_default(),
        )
    }

    #[gpui::test]
    fn the_rail_button_is_a_button_named_after_the_update(cx: &mut TestAppContext) {
        use gpui::RenderOnce as _;

        let (workspace, cx) = setup(cx);
        ready_update(true, &workspace, cx);
        let ready = cx.update(|_, cx| updates::ready(cx)).expect("ready");

        let (role, label) = workspace.update_in(cx, |workspace, window, cx| {
            let element = rail_button(workspace, &ready, cx)
                .render(window, cx)
                .into_element();

            node_of(&element)
        });

        assert_eq!(role, Some(Role::Button));
        assert_eq!(label, "Update ready, Swarm 1.2.0");
    }

    #[gpui::test]
    fn the_card_is_a_group_named_after_the_update(cx: &mut TestAppContext) {
        use gpui::RenderOnce as _;

        let (workspace, cx) = setup(cx);
        ready_update(true, &workspace, cx);
        let ready = cx.update(|_, cx| updates::ready(cx)).expect("ready");

        let (role, label) = cx.update(|window, cx| {
            let mut element = card_panel(&ready, gpui::Empty)
                .render(window, cx)
                .into_any_element();
            let named = element
                .downcast_mut::<gpui::Stateful<gpui::Div>>()
                .expect("a named panel");

            node_of(&*named)
        });

        assert_eq!(role, Some(Role::Group));
        assert_eq!(label, "Update ready, Swarm 1.2.0");
    }

    #[gpui::test]
    fn the_keyboard_opens_the_rail_card_with_focus_inside_and_escape_gives_it_back(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        collapse(&workspace, cx);
        ready_update(true, &workspace, cx);
        let button_focus = workspace.read_with(cx, |w, _| w.rail_update_button_focus.clone());
        let card_focus = workspace.read_with(cx, |w, _| w.rail_update_card_focus.clone());
        cx.update(|window, cx| button_focus.focus(window, cx));

        press_enter(cx);
        redraw(cx);
        let inside = cx.update(|window, cx| card_focus.contains_focused(window, cx));
        let open = shown("update-card", cx);

        cx.simulate_keystrokes("escape");
        redraw(cx);
        let back = cx.update(|window, _| button_focus.is_focused(window));

        assert!(open);
        assert!(inside, "focus stayed on the rail button");
        assert!(!shown("update-card", cx));
        assert!(back, "focus did not return to the rail button");
    }

    #[gpui::test]
    fn tab_inside_the_rail_card_stays_in_it_and_tab_out_closes_it(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        collapse(&workspace, cx);
        ready_update(true, &workspace, cx);
        let button_focus = workspace.read_with(cx, |w, _| w.rail_update_button_focus.clone());
        let card_focus = workspace.read_with(cx, |w, _| w.rail_update_card_focus.clone());
        click("rail-update", cx);

        cx.simulate_keystrokes("tab tab tab");
        redraw(cx);
        let inside = cx.update(|window, cx| card_focus.contains_focused(window, cx));
        let open = shown("update-card", cx);

        cx.simulate_keystrokes("tab");
        redraw(cx);
        let back = cx.update(|window, _| button_focus.is_focused(window));

        assert!(open && inside, "tab left the card early");
        assert!(!shown("update-card", cx));
        assert!(back);
    }

    #[gpui::test]
    fn folding_or_showing_the_sidebar_closes_the_rail_card(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        collapse(&workspace, cx);
        ready_update(true, &workspace, cx);
        click("rail-update", cx);

        collapse(&workspace, cx);
        collapse(&workspace, cx);

        assert!(!shown("update-card", cx));
        assert!(shown("rail-update", cx));
    }

    #[gpui::test]
    fn the_announcement_is_drawn_before_and_while_the_update_is_ready(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let source = FakeSource::with_update();
        let host = FakeHost::new(true);
        let updater =
            cx.update(|_, cx| install_fake(&workspace, &source, &host, signed_build(), cx));
        redraw(cx);
        let before = shown("update-status", cx);

        updater.update(cx, Updater::check);
        redraw(cx);
        let during = shown("update-status", cx);
        click("update-card-dismiss", cx);
        let dismissed = shown("update-status", cx);
        collapse(&workspace, cx);
        let folded = shown("update-status", cx);

        assert!(before, "the region exists before it speaks");
        assert!(during);
        assert!(dismissed);
        assert!(folded);
    }

    #[gpui::test]
    fn the_announcement_speaks_only_when_ready(cx: &mut TestAppContext) {
        use gpui::accesskit::Live;

        let (workspace, cx) = setup(cx);
        ready_update(true, &workspace, cx);
        let ready = cx.update(|_, cx| updates::ready(cx)).expect("ready");

        let quiet =
            crate::live_probe::see_in(LiveRegion::status("update-ready-status", spoken(None)), cx);
        let speaking = crate::live_probe::see_in(
            LiveRegion::status("update-ready-status", spoken(Some(&ready))),
            cx,
        );

        assert_eq!(quiet.role, Some(Role::Status));
        assert!(crate::live_probe::is_quiet(&quiet));
        assert_eq!(speaking.live, Some(Live::Polite));
        assert_eq!(speaking.value.as_deref(), Some("Update ready"));
    }
}
