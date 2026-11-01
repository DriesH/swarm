//! Swarm's design tokens, themes and components, built on GPUI.
//!
//! Call [`init`] once at startup. Components read colors with
//! `cx.theme().colors()` and never use raw colors.

pub mod component;
pub mod components;
pub mod contrast;
pub mod fonts;
pub mod motion;
pub mod prelude;
pub mod styles;
pub mod theme;
pub mod traits;

use gpui::{App, FocusHandle, InteractiveElement, KeyBinding, Window, actions};

pub use components::*;
pub use theme::{ActiveTheme, Appearance, Theme, ThemeColors, ThemeKind};

actions!(
    swarm_ui,
    [
        FocusNext,
        FocusPrevious,
        Dismiss,
        SelectPrevious,
        SelectNext,
        Confirm
    ]
);

/// Key context of an app's root element. Tab only moves focus inside it.
pub const ROOT_CONTEXT: &str = "SwarmRoot";

/// Key context of an open dialog.
pub const DIALOG_CONTEXT: &str = "Dialog";

/// Key context of an open menu or list popup.
pub const MENU_CONTEXT: &str = "Menu";

/// Key context of a confirm row that can be dismissed.
pub const CONFIRM_CONTEXT: &str = "ConfirmRow";

/// Loads fonts, sets the theme, reads Reduce motion and binds the shared keys.
pub fn init(cx: &mut App) {
    fonts::load(cx);
    theme::init(cx);
    motion::sync_reduce_motion(cx);
    text_input::init(cx);

    cx.bind_keys([
        KeyBinding::new("tab", FocusNext, Some(ROOT_CONTEXT)),
        KeyBinding::new("shift-tab", FocusPrevious, Some(ROOT_CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some(DIALOG_CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some(MENU_CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some(CONFIRM_CONTEXT)),
        KeyBinding::new("up", SelectPrevious, Some(MENU_CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(MENU_CONTEXT)),
        KeyBinding::new("enter", Confirm, Some(MENU_CONTEXT)),
    ]);
}

/// Moves focus with Tab and Shift-Tab. Add it to the root element of each window,
/// and focus something when the window opens: with nothing focused, Tab goes nowhere.
pub fn tab_navigation<E: InteractiveElement>(element: E) -> E {
    element
        .on_action(|_: &FocusNext, window, cx| window.focus_next(cx))
        .on_action(|_: &FocusPrevious, window, cx| window.focus_prev(cx))
}

const TRAP_MAX_STEPS: usize = 64;

/// Keeps Tab and Shift-Tab inside `handle`, for a dialog. If stepping leaves it,
/// steps on until focus is back inside, or focuses `handle` itself.
pub fn trap_focus<E: InteractiveElement>(element: E, handle: FocusHandle) -> E {
    let next = handle.clone();

    element
        .on_action(move |_: &FocusNext, window, cx| {
            step_inside(&next, window, cx, Window::focus_next);
        })
        .on_action(move |_: &FocusPrevious, window, cx| {
            step_inside(&handle, window, cx, Window::focus_prev);
        })
}

fn step_inside(
    handle: &FocusHandle,
    window: &mut Window,
    cx: &mut App,
    step: fn(&mut Window, &mut App),
) {
    for _ in 0..TRAP_MAX_STEPS {
        step(window, cx);

        if handle.contains_focused(window, cx) {
            return;
        }
    }

    handle.focus(window, cx);
}
