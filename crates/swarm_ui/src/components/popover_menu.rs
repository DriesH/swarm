use std::rc::Rc;

use gpui::{
    Anchor, AnyElement, App, Bounds, ElementId, Entity, FocusHandle, Pixels, Window, anchored,
    deferred, div, point, px,
};

use crate::component::{Component, example, example_group};
use crate::components::menu::{Menu, demo_menu};
use crate::prelude::*;
use crate::{FocusNext, FocusPrevious};

type MenuBuilder = Rc<dyn Fn(&mut Window, &mut App) -> Menu + 'static>;
type TriggerBuilder = Box<dyn FnOnce(bool) -> AnyElement>;

/// A trigger that opens a [`Menu`] below it. Tab stays in the open menu and Escape closes
/// it. Focus goes back to where it was unless a pick moved it on.
#[derive(IntoElement)]
#[must_use]
pub struct PopoverMenu {
    id: ElementId,
    trigger: TriggerBuilder,
    menu: MenuBuilder,
    upward: bool,
}

impl PopoverMenu {
    /// `menu` builds the menu each time it opens. Its `on_dismiss` is replaced:
    /// the popover closes itself on Escape and after a pick.
    pub fn new(
        id: impl Into<ElementId>,
        trigger: impl IntoElement + 'static,
        menu: impl Fn(&mut Window, &mut App) -> Menu + 'static,
    ) -> Self {
        Self::with_trigger(id, move |_| trigger, menu)
    }

    /// Like [`new`](Self::new), but `trigger` gets whether the menu is open. Pass it on to
    /// the trigger with `expanded`, so VoiceOver reads the state on the button.
    pub fn with_trigger<T: IntoElement>(
        id: impl Into<ElementId>,
        trigger: impl FnOnce(bool) -> T + 'static,
        menu: impl Fn(&mut Window, &mut App) -> Menu + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            trigger: Box::new(move |open| trigger(open).into_any_element()),
            menu: Rc::new(menu),
            upward: false,
        }
    }

    /// Opens the menu above the trigger, for a trigger at the bottom of the window.
    pub fn opens_upward(mut self) -> Self {
        self.upward = true;
        self
    }
}

pub(crate) struct PopoverState {
    open: bool,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    trigger: Option<Bounds<Pixels>>,
}

impl PopoverState {
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }
}

/// The popover's state, shared with components that style their own trigger.
pub(crate) fn popover_state(
    id: &ElementId,
    window: &mut Window,
    cx: &mut App,
) -> Entity<PopoverState> {
    window.use_keyed_state((id.clone(), "popover"), cx, |_, cx| PopoverState {
        open: false,
        focus: cx.focus_handle(),
        previous_focus: None,
        trigger: None,
    })
}

fn open(state: &Entity<PopoverState>, window: &mut Window, cx: &mut App) {
    let previous = window.focused(cx);

    let focus = state.update(cx, |state, cx| {
        state.open = true;
        state.previous_focus = previous;
        cx.notify();

        state.focus.clone()
    });

    focus.focus(window, cx);
}

/// Closes the menu. Focus goes back to where it was only when it still sits in the menu:
/// a picked item may have moved it on, for example into a dialog.
fn close(state: &Entity<PopoverState>, window: &mut Window, cx: &mut App) {
    let (previous, focus_in_menu) = state.update(cx, |state, cx| {
        state.open = false;
        cx.notify();

        (
            state.previous_focus.take(),
            state.focus.contains_focused(window, cx),
        )
    });

    if focus_in_menu && let Some(previous) = previous {
        previous.focus(window, cx);
    }
}

impl RenderOnce for PopoverMenu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = popover_state(&self.id, window, cx);
        let (is_open, focus, trigger_bounds) = {
            let state = state.read(cx);
            (state.open, state.focus.clone(), state.trigger)
        };

        let menu = is_open.then(|| {
            let menu = (self.menu)(window, cx).track_focus(&focus).on_dismiss({
                let state = state.clone();
                move |window, cx| close(&state, window, cx)
            });

            let (anchor, position) = if self.upward {
                (
                    Anchor::BottomLeft,
                    trigger_bounds.map(|bounds| point(bounds.left(), bounds.top() - px(4.))),
                )
            } else {
                (
                    Anchor::TopLeft,
                    trigger_bounds.map(|bounds| point(bounds.left(), bounds.bottom() + px(4.))),
                )
            };

            deferred(
                anchored()
                    .anchor(anchor)
                    .when_some(position, gpui::Anchored::position)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            // Tab stays in the menu, so nothing moves focus out of it.
                            .on_action(|_: &FocusPrevious, _, _| {})
                            .on_action(|_: &FocusNext, _, _| {})
                            .on_mouse_down_out({
                                let state = state.clone();
                                move |event, window, cx| {
                                    let on_trigger = state
                                        .read(cx)
                                        .trigger
                                        .is_some_and(|bounds| bounds.contains(&event.position));

                                    if !on_trigger {
                                        close(&state, window, cx);
                                    }
                                }
                            })
                            .child(menu),
                    ),
            )
            .priority(1)
        });

        div()
            .id(self.id)
            .on_click({
                let state = state.clone();
                move |_, window, cx| {
                    if state.read(cx).open {
                        close(&state, window, cx);
                    } else {
                        open(&state, window, cx);
                    }
                }
            })
            .on_key_down({
                let state = state.clone();
                move |event, window, cx| {
                    let opens = matches!(event.keystroke.key.as_str(), "enter" | "space" | "down");

                    if opens && !state.read(cx).open {
                        open(&state, window, cx);
                        cx.stop_propagation();
                    }
                }
            })
            .child(
                div()
                    .on_children_prepainted({
                        let state = state.clone();
                        move |bounds, _, cx| {
                            let bounds = bounds.first().copied();
                            state.update(cx, |state, _| state.trigger = bounds);
                        }
                    })
                    .child((self.trigger)(is_open)),
            )
            .children(menu)
    }
}

impl Component for PopoverMenu {
    fn name() -> &'static str {
        "PopoverMenu"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        v_flex()
            .gap_6()
            .child(example_group(
                "Default",
                [
                    example(
                        "Opens below the trigger",
                        PopoverMenu::new(
                            "popover-demo",
                            IconButton::new("new-tab", IconName::Plus, "New tab"),
                            |_, _| demo_menu("popover-demo-menu"),
                        ),
                    ),
                    example(
                        "Opens above the trigger",
                        PopoverMenu::new(
                            "popover-demo-up",
                            IconButton::new("new-tab-up", IconName::Plus, "New tab"),
                            |_, _| demo_menu("popover-demo-up-menu"),
                        )
                        .opens_upward(),
                    ),
                ],
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use gpui::accesskit::{Node, Role};
    use gpui::{Context, Element as _, Render, TestAppContext, WindowHandle};

    use super::*;

    struct Probe {
        open: bool,
        seen: Rc<Cell<Option<bool>>>,
        wrapper_role: Option<Role>,
        trigger_expanded: Option<bool>,
    }

    impl Render for Probe {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            if self.open {
                let state = popover_state(&"popover".into(), window, cx);
                state.update(cx, |state, _| state.open = true);
            }

            let seen = Rc::clone(&self.seen);
            let wrapper = PopoverMenu::with_trigger(
                "popover",
                move |open| {
                    seen.set(Some(open));
                    IconButton::new("trigger", IconName::Plus, "New").expanded(open)
                },
                |_, _| demo_menu("popover-menu"),
            )
            .render(window, cx)
            .into_element();
            self.wrapper_role = wrapper.a11y_role();

            let trigger = IconButton::new("trigger", IconName::Plus, "New")
                .expanded(self.open)
                .render(window, cx)
                .into_element();
            let mut node = Node::new(trigger.a11y_role().unwrap_or(Role::Unknown));
            trigger.write_a11y_info(&mut node);
            self.trigger_expanded = node.is_expanded();

            div()
        }
    }

    fn probe(open: bool) -> (TestAppContext, WindowHandle<Probe>) {
        let mut app = TestAppContext::single();
        app.update(crate::theme::init);
        let window = app.add_window(move |_, _| Probe {
            open,
            seen: Rc::default(),
            wrapper_role: None,
            trigger_expanded: None,
        });
        app.run_until_parked();

        (app, window)
    }

    fn read<T>(
        window: &WindowHandle<Probe>,
        app: &mut TestAppContext,
        read: impl FnOnce(&Probe) -> T,
    ) -> T {
        app.update(|cx| window.read(cx).map(read).expect("the window"))
    }

    #[test]
    fn the_trigger_is_told_the_menu_is_closed() {
        let (mut app, window) = probe(false);

        assert_eq!(read(&window, &mut app, |p| p.seen.get()), Some(false));
    }

    #[test]
    fn the_trigger_is_told_the_menu_is_open() {
        let (mut app, window) = probe(true);

        assert_eq!(read(&window, &mut app, |p| p.seen.get()), Some(true));
    }

    #[test]
    fn the_trigger_node_reports_expanded_false_when_closed() {
        let (mut app, window) = probe(false);

        assert_eq!(read(&window, &mut app, |p| p.trigger_expanded), Some(false));
    }

    #[test]
    fn the_trigger_node_reports_expanded_true_when_open() {
        let (mut app, window) = probe(true);

        assert_eq!(read(&window, &mut app, |p| p.trigger_expanded), Some(true));
    }

    #[test]
    fn the_wrapper_has_no_group_role() {
        let (mut app, window) = probe(false);

        assert_eq!(read(&window, &mut app, |p| p.wrapper_role), None);
    }
}
