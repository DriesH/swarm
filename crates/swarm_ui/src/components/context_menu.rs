use std::rc::Rc;

use gpui::{
    Anchor, AnyElement, App, Bounds, Context, ElementId, Entity, FocusHandle, Hsla, MouseButton,
    Pixels, Point, SharedString, Subscription, Window, anchored, deferred, div, point, px,
};

use crate::component::{Component, example, example_group};
use crate::components::menu::{Menu, MenuItem};
use crate::prelude::*;
use crate::{FocusNext, FocusPrevious, ShowContextMenu};

type MenuBuilder = Rc<dyn Fn(Menu, &mut Window, &mut App) -> Menu + 'static>;

/// The gap between a row and the menu a key press opens under it.
const TRIGGER_GAP: Pixels = px(4.);
const WINDOW_MARGIN: Pixels = px(8.);

/// A menu that opens where the pointer is on a right-click or a Control-click, and under the
/// trigger on Shift-F10 while the trigger has focus. It closes on Escape, after a pick, on a
/// click outside, when focus moves out of it and when the window loses focus. Focus then goes
/// back to where it was, unless it already moved on. Tab does nothing while the menu is open,
/// and the click that closes it goes no further.
#[derive(IntoElement)]
#[must_use]
pub struct ContextMenu {
    id: ElementId,
    label: SharedString,
    trigger: AnyElement,
    items: MenuBuilder,
    start_open: bool,
}

impl ContextMenu {
    /// `label` names the menu for VoiceOver, for example "Project actions". `items` adds the
    /// entries to the menu each time it opens; its id, label, roles and dismissal are set.
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        trigger: impl IntoElement,
        items: impl Fn(Menu, &mut Window, &mut App) -> Menu + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            trigger: trigger.into_any_element(),
            items: Rc::new(items),
            start_open: false,
        }
    }

    /// Opens the menu under the trigger when it is first drawn, without taking focus. For
    /// previews and snapshots.
    pub fn initially_open(mut self) -> Self {
        self.start_open = true;
        self
    }

    /// The menu as it opens: named, focusable, and closing itself on Escape and after a pick.
    fn build_menu(
        &self,
        state: &Entity<ContextState>,
        focus: &FocusHandle,
        window: &mut Window,
        cx: &mut App,
    ) -> Menu {
        let menu = Menu::new((self.id.clone(), "menu"), self.label.clone());
        let state = state.clone();

        (self.items)(menu, window, cx)
            .track_focus(focus)
            .on_dismiss(move |window, cx| state.update(cx, |state, cx| state.close(window, cx)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Placement {
    /// Where the pointer was, in window coordinates.
    Pointer(Point<Pixels>),
    /// Under the trigger.
    Trigger,
}

struct ContextState {
    placement: Option<Placement>,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    trigger: Option<Bounds<Pixels>>,
    _subscriptions: [Subscription; 2],
}

impl ContextState {
    fn open(&mut self, placement: Placement, window: &mut Window, cx: &mut Context<Self>) {
        if self.placement.is_none() {
            self.previous_focus = window.focused(cx);
        }

        self.placement = Some(placement);
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Closes the menu. Focus goes back to the row only when it still sits in the menu: a
    /// picked item may have moved it on, for example into a dialog.
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.placement.take().is_none() {
            return;
        }

        let previous = self.previous_focus.take();
        if self.focus.contains_focused(window, cx)
            && let Some(previous) = previous
        {
            previous.focus(window, cx);
        }

        cx.notify();
    }
}

fn context_state(
    id: &ElementId,
    start_open: bool,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ContextState> {
    window.use_keyed_state((id.clone(), "context-menu"), cx, |window, cx| {
        let focus = cx.focus_handle();

        let deactivation =
            cx.observe_window_activation(window, |state: &mut ContextState, window, cx| {
                if !window.is_window_active() {
                    state.close(window, cx);
                }
            });
        // Focus events fire while the window draws, where a redraw request is lost. Close
        // right after the draw instead.
        let focus_out = cx.on_focus_out(&focus, window, |_, _, window, cx| {
            cx.defer_in(window, ContextState::close);
        });

        ContextState {
            placement: start_open.then_some(Placement::Trigger),
            focus,
            previous_focus: None,
            trigger: None,
            _subscriptions: [deactivation, focus_out],
        }
    })
}

impl RenderOnce for ContextMenu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = context_state(&self.id, self.start_open, window, cx);
        let (placement, focus, trigger) = {
            let state = state.read(cx);
            (state.placement, state.focus.clone(), state.trigger)
        };

        let position = placement.and_then(|placement| match placement {
            Placement::Pointer(position) => Some(position),
            Placement::Trigger => {
                trigger.map(|bounds| point(bounds.left(), bounds.bottom() + TRIGGER_GAP))
            }
        });

        let menu = position.map(|position| {
            let menu = self.build_menu(&state, &focus, window, cx);

            deferred(
                anchored()
                    .anchor(Anchor::TopLeft)
                    .position(position)
                    .snap_to_window_with_margin(WINDOW_MARGIN)
                    .child(
                        div()
                            .on_mouse_down_out({
                                let state = state.clone();
                                move |_, window, cx| {
                                    // Like a macOS menu, the click only closes it.
                                    cx.stop_propagation();
                                    state.update(cx, |state, cx| state.close(window, cx));
                                }
                            })
                            // Tab stays in the menu, so nothing moves focus out of it.
                            .on_action(|_: &FocusNext, _, _| {})
                            .on_action(|_: &FocusPrevious, _, _| {})
                            .child(menu),
                    ),
            )
            .priority(1)
        });

        div()
            .on_mouse_down(MouseButton::Right, {
                let state = state.clone();
                move |event, window, cx| {
                    cx.stop_propagation();
                    window.prevent_default();
                    state.update(cx, |state, cx| {
                        state.open(Placement::Pointer(event.position), window, cx);
                    });
                }
            })
            // The platform turns a Control-click into a right-click, so this only runs where
            // it does not, such as in tests.
            .on_mouse_down(MouseButton::Left, {
                let state = state.clone();
                move |event, window, cx| {
                    if !event.modifiers.control {
                        return;
                    }

                    cx.stop_propagation();
                    window.prevent_default();
                    state.update(cx, |state, cx| {
                        state.open(Placement::Pointer(event.position), window, cx);
                    });
                }
            })
            .on_action({
                let state = state.clone();
                move |_: &ShowContextMenu, window, cx| {
                    state.update(cx, |state, cx| state.open(Placement::Trigger, window, cx));
                }
            })
            .child(
                div()
                    .on_children_prepainted({
                        let state = state.clone();
                        move |bounds, _, cx| {
                            let bounds = bounds.first().copied();

                            state.update(cx, |state, cx| {
                                let moved = state.trigger != bounds;
                                state.trigger = bounds;

                                if moved && state.placement == Some(Placement::Trigger) {
                                    cx.notify();
                                }
                            });
                        }
                    })
                    .child(self.trigger),
            )
            .children(menu)
    }
}

/// The menu of the previews: three project actions.
fn demo_items(menu: Menu, _: &mut Window, _: &mut App) -> Menu {
    menu.width(px(200.))
        .item(MenuItem::new("show-in-finder", "Show in Finder").icon(IconName::Folder))
        .item(MenuItem::new("project-settings", "Project settings").icon(IconName::Settings))
        .separator()
        .item(MenuItem::new("remove-project", "Remove project\u{2026}").icon(IconName::Close))
}

fn demo_row(id: &'static str, tile: Hsla, start_open: bool) -> ContextMenu {
    let trigger = ListRow::new((ElementId::from(id), "row"), "swarm")
        .debug_selector(move || format!("{id}-row"))
        .start_slot(ProjectTile::new('S', tile));
    let menu = ContextMenu::new(id, "Project actions", trigger, demo_items);

    if start_open {
        menu.initially_open()
    } else {
        menu
    }
}

impl Component for ContextMenu {
    fn name() -> &'static str {
        "ContextMenu"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        // A project color, as in the ProjectTile preview: the letter passes AA on it.
        let tile: Hsla = gpui::rgb(0x004f_46e5).into();
        let closed = demo_row("context-menu-demo", tile, false);
        let open = demo_row("context-menu-demo-open", tile, true);

        v_flex()
            .gap_6()
            .child(example_group(
                "Default",
                [example(
                    "Right-click, Control-click or Shift-F10 on the row",
                    div().w(px(232.)).child(closed),
                )],
            ))
            .child(example_group(
                "Open",
                [example(
                    "Under the row",
                    div().w(px(232.)).h(px(190.)).child(open),
                )],
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use gpui::accesskit::{Node, Role};
    use gpui::{Element as _, KeyBinding, Modifiers, Render, TestAppContext, VisualTestContext};

    use super::*;
    use crate::{Dismiss, MENU_CONTEXT, ROOT_CONTEXT, tab_navigation};

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

    #[test]
    fn the_open_menu_is_a_labelled_menu_of_items() {
        let mut app = TestAppContext::single();
        app.update(crate::theme::init);

        let window = app.add_window(|_, _| Probe {
            check: Some(Box::new(|window, cx| {
                let menu = ContextMenu::new("menu", "Project actions", div(), demo_items);
                let state = context_state(&menu.id, false, window, cx);
                let focus = state.read(cx).focus.clone();

                let element = menu
                    .build_menu(&state, &focus, window, cx)
                    .render(window, cx)
                    .into_element();
                let role = element.a11y_role();
                let mut node = Node::new(role.unwrap_or(Role::Unknown));
                element.write_a11y_info(&mut node);

                assert_eq!(role, Some(Role::Menu));
                assert_eq!(node.label(), Some("Project actions"));
            })),
        });
        app.run_until_parked();

        let ran = app.update(|cx| window.read(cx).map(|probe| probe.check.is_none()));
        assert!(matches!(ran, Ok(true)), "the probe view never rendered");
    }

    struct Scene {
        row: FocusHandle,
        outside: FocusHandle,
        clicks: Rc<Cell<usize>>,
    }

    impl Render for Scene {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let clicks = Rc::clone(&self.clicks);

            let control = div()
                .id("control")
                .w(px(100.))
                .h(px(30.))
                .debug_selector(|| "control".to_owned())
                .on_click(move |_, _, _| clicks.set(clicks.get() + 1));
            let row = div()
                .id("row")
                .track_focus(&self.row)
                .w(px(200.))
                .h(px(30.))
                .debug_selector(|| "row".to_owned());
            let outside = div().id("outside").track_focus(&self.outside).size(px(20.));

            tab_navigation(
                div()
                    .id("root")
                    .key_context(ROOT_CONTEXT)
                    .size_full()
                    .child(control)
                    .child(ContextMenu::new("ctx", "Row actions", row, demo_items))
                    .child(outside),
            )
        }
    }

    struct Rig {
        row: FocusHandle,
        outside: FocusHandle,
        clicks: Rc<Cell<usize>>,
    }

    fn rig(cx: &mut TestAppContext) -> (Rig, &mut VisualTestContext) {
        cx.update(|cx| {
            crate::theme::init(cx);
            cx.bind_keys([
                KeyBinding::new("escape", Dismiss, Some(MENU_CONTEXT)),
                KeyBinding::new("tab", FocusNext, Some(ROOT_CONTEXT)),
                KeyBinding::new("shift-tab", FocusPrevious, Some(ROOT_CONTEXT)),
            ]);
        });

        let clicks = Rc::new(Cell::new(0));
        let shared = Rc::clone(&clicks);
        let (scene, cx) = cx.add_window_view(move |_, cx| Scene {
            row: cx.focus_handle().tab_stop(true),
            outside: cx.focus_handle().tab_stop(true),
            clicks: shared,
        });
        let (row, outside) =
            scene.read_with(cx, |scene, _| (scene.row.clone(), scene.outside.clone()));

        cx.update(|window, cx| {
            window.activate_window();
            outside.focus(window, cx);
        });
        cx.run_until_parked();

        (
            Rig {
                row,
                outside,
                clicks,
            },
            cx,
        )
    }

    fn row_center(cx: &mut VisualTestContext) -> Point<Pixels> {
        cx.debug_bounds("row")
            .unwrap_or_else(|| panic!("the row is not on screen"))
            .center()
    }

    /// Right-clicks on the right half of the row, so the menu opens clear of the control.
    fn open_menu(cx: &mut VisualTestContext) -> Point<Pixels> {
        let pointer = row_center(cx) + point(px(50.), px(0.));

        cx.simulate_mouse_down(pointer, MouseButton::Right, Modifiers::none());
        cx.simulate_mouse_up(pointer, MouseButton::Right, Modifiers::none());
        cx.run_until_parked();

        pointer
    }

    fn menu_bounds(cx: &mut VisualTestContext) -> Option<Bounds<Pixels>> {
        cx.debug_bounds("ctx-menu")
    }

    #[gpui::test]
    fn a_right_click_opens_the_menu_at_the_pointer_and_focuses_it(cx: &mut TestAppContext) {
        let (rig, cx) = rig(cx);
        assert!(menu_bounds(cx).is_none());

        let pointer = open_menu(cx);

        let menu = menu_bounds(cx).expect("the menu is open");
        assert_eq!(menu.origin, pointer);
        cx.update(|window, cx| {
            assert!(window.focused(cx).is_some());
            assert!(!rig.row.is_focused(window), "focus is in the menu");
            assert!(!rig.outside.is_focused(window), "focus is in the menu");
        });
    }

    #[gpui::test]
    fn escape_closes_the_menu_and_gives_focus_back_to_the_row(cx: &mut TestAppContext) {
        let (rig, cx) = rig(cx);
        open_menu(cx);
        assert!(menu_bounds(cx).is_some());

        cx.simulate_keystrokes("escape");

        assert!(menu_bounds(cx).is_none());
        assert!(cx.update(|window, _| rig.row.is_focused(window)));
    }

    #[gpui::test]
    fn tab_and_shift_tab_leave_the_menu_open_and_focused(cx: &mut TestAppContext) {
        let (rig, cx) = rig(cx);
        open_menu(cx);
        let focused = cx.update(|window, cx| window.focused(cx));

        for keys in ["tab", "shift-tab", "tab tab"] {
            cx.simulate_keystrokes(keys);

            assert!(menu_bounds(cx).is_some(), "{keys} closed the menu");
            assert_eq!(
                cx.update(|window, cx| window.focused(cx)),
                focused,
                "{keys}"
            );
        }

        cx.simulate_keystrokes("escape");

        assert!(menu_bounds(cx).is_none());
        assert!(cx.update(|window, _| rig.row.is_focused(window)));
    }

    #[gpui::test]
    fn a_click_outside_only_closes_the_menu(cx: &mut TestAppContext) {
        let (rig, cx) = rig(cx);
        open_menu(cx);
        let control = cx
            .debug_bounds("control")
            .expect("the control is on screen")
            .center();

        cx.simulate_click(control, Modifiers::none());
        cx.run_until_parked();

        assert!(menu_bounds(cx).is_none());
        assert_eq!(rig.clicks.get(), 0, "the control got the click");
        assert!(cx.update(|window, _| rig.row.is_focused(window)));

        cx.simulate_click(control, Modifiers::none());
        cx.run_until_parked();

        assert_eq!(rig.clicks.get(), 1, "the control takes clicks again");
    }

    #[gpui::test]
    fn moving_focus_out_of_the_menu_closes_it(cx: &mut TestAppContext) {
        let (rig, cx) = rig(cx);
        open_menu(cx);

        cx.update(|window, cx| rig.outside.focus(window, cx));
        cx.run_until_parked();

        assert!(menu_bounds(cx).is_none());
        assert!(
            cx.update(|window, _| rig.outside.is_focused(window)),
            "focus stayed where it moved"
        );
    }
}
