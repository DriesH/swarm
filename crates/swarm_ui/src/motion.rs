//! macOS "Reduce motion". GPUI animations already respect
//! `App::reduce_motion`; this keeps that flag in sync with the system setting.

use gpui::{App, Context, Subscription, Window};

/// True when animations should jump to their end state.
pub fn reduce_motion(cx: &App) -> bool {
    cx.reduce_motion()
}

/// Reads the system setting into GPUI.
pub fn sync_reduce_motion(cx: &mut App) {
    cx.set_reduce_motion(system_reduce_motion());
}

/// Re-reads the setting each time `window` becomes active, which covers the
/// usual case of changing it in System Settings and coming back.
/// Keep the returned subscription alive, for example in the root view.
pub fn follow_reduce_motion<V: 'static>(window: &mut Window, cx: &mut Context<V>) -> Subscription {
    sync_reduce_motion(cx);

    cx.observe_window_activation(window, |_, window, cx| {
        if window.is_window_active() {
            sync_reduce_motion(cx);
        }
    })
}

#[cfg(target_os = "macos")]
fn system_reduce_motion() -> bool {
    objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

#[cfg(not(target_os = "macos"))]
fn system_reduce_motion() -> bool {
    false
}
