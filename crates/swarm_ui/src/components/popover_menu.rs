use std::rc::Rc;

use gpui::{
    Anchor, AnyElement, App, Bounds, ElementId, Entity, FocusHandle, Pixels, Window, anchored,
    deferred, div, point, px,
};

use crate::component::{Component, example, example_group};
use crate::components::menu::{Menu, demo_menu};
use crate::prelude::*;

type MenuBuilder = Rc<dyn Fn(&mut Window, &mut App) -> Menu + 'static>;

/// A trigger that opens a [`Menu`] below it.
#[derive(IntoElement)]
#[must_use]
pub struct PopoverMenu {
    id: ElementId,
    trigger: AnyElement,
    menu: MenuBuilder,
    upward: bool,
}

impl PopoverMenu {
    /// `menu` builds the menu each time it opens. Its `on_dismiss` is replaced:
    /// the popover closes itself on Escape and after a pick.
    pub fn new(
        id: impl Into<ElementId>,
        trigger: impl IntoElement,
        menu: impl Fn(&mut Window, &mut App) -> Menu + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            trigger: trigger.into_any_element(),
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

fn close(state: &Entity<PopoverState>, window: &mut Window, cx: &mut App) {
    let previous = state.update(cx, |state, cx| {
        state.open = false;
        cx.notify();

        state.previous_focus.take()
    });

    if let Some(previous) = previous {
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
                    .child(self.trigger),
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
