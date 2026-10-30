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

use gpui::{App, InteractiveElement, KeyBinding, actions};

pub use components::*;
pub use theme::{ActiveTheme, Appearance, Theme, ThemeColors, ThemeKind};

actions!(swarm_ui, [FocusNext, FocusPrevious]);

/// Key context of an app's root element. Tab only moves focus inside it.
pub const ROOT_CONTEXT: &str = "SwarmRoot";

/// Loads fonts, sets the theme, reads Reduce motion and binds Tab and Shift-Tab.
pub fn init(cx: &mut App) {
    fonts::load(cx);
    theme::init(cx);
    motion::sync_reduce_motion(cx);
    cx.bind_keys([
        KeyBinding::new("tab", FocusNext, Some(ROOT_CONTEXT)),
        KeyBinding::new("shift-tab", FocusPrevious, Some(ROOT_CONTEXT)),
    ]);
}

/// Moves focus with Tab and Shift-Tab. Add it to the root element of each window,
/// and focus something when the window opens: with nothing focused, Tab goes nowhere.
pub fn tab_navigation<E: InteractiveElement>(element: E) -> E {
    element
        .on_action(|_: &FocusNext, window, cx| window.focus_next(cx))
        .on_action(|_: &FocusPrevious, window, cx| window.focus_prev(cx))
}
