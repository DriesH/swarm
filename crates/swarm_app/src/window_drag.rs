//! Drag regions for the transparent titlebar.
//!
//! The windows set `app_owns_titlebar_drag`, so macOS no longer moves the window
//! from the titlebar strip. Only a [`drag_region`] moves it, so buttons, tabs and
//! menus never do.

use gpui::{Div, ElementId, MouseButton, Stateful, div};
use swarm_ui::prelude::*;

/// A part of the header that moves the window when dragged and zooms it on double-click.
///
/// Keep interactive elements out of it: a press on them would reach this region too.
pub(crate) fn drag_region(
    id: impl Into<ElementId>,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    let id: ElementId = id.into();
    let pressed = window.use_keyed_state((id.clone(), "drag-pressed"), cx, |_, _| false);

    let on_down = pressed.clone();
    let on_move = pressed.clone();
    let on_up = pressed.clone();
    let on_up_out = pressed;

    div()
        .id(id)
        .on_mouse_down(MouseButton::Left, move |event, window, cx| {
            if event.click_count >= 2 {
                window.titlebar_double_click();
            } else {
                on_down.update(cx, |pressed, _| *pressed = true);
            }
        })
        .on_mouse_move(move |event, window, cx| {
            let was_pressed = on_move.update(cx, |pressed, _| std::mem::take(pressed));
            if was_pressed && event.dragging() {
                window.start_window_move();
            }
        })
        .on_mouse_up(MouseButton::Left, move |_, _, cx| {
            on_up.update(cx, |pressed, _| *pressed = false);
        })
        .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
            on_up_out.update(cx, |pressed, _| *pressed = false);
        })
}
