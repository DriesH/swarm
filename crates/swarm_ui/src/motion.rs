//! macOS "Reduce motion". GPUI animations already respect
//! `App::reduce_motion`; this keeps that flag in sync with the system setting.

use gpui::{App, Context, Global, Subscription, Window};

/// Where [`sync_reduce_motion`] reads the setting. Without it, the system answers.
/// Tests install one to fake a change in System Settings.
pub struct ReduceMotionSource(pub fn() -> bool);

impl Global for ReduceMotionSource {}

/// True when animations should jump to their end state.
pub fn reduce_motion(cx: &App) -> bool {
    cx.reduce_motion()
}

/// Reads the system setting into GPUI. Code that runs while no window is active, such as
/// the menu bar bee, calls it again on each tick.
pub fn sync_reduce_motion(cx: &mut App) {
    let reduced = cx
        .try_global::<ReduceMotionSource>()
        .map_or_else(system_reduce_motion, |source| (source.0)());

    cx.set_reduce_motion(reduced);
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use gpui::TestAppContext;

    use super::*;

    thread_local! {
        static SETTING: Cell<bool> = const { Cell::new(false) };
    }

    fn setting() -> bool {
        SETTING.with(Cell::get)
    }

    #[gpui::test]
    fn syncing_reads_the_setting_again_each_time(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(ReduceMotionSource(setting)));

        SETTING.with(|cell| cell.set(true));
        cx.update(sync_reduce_motion);
        let on = cx.update(|cx| reduce_motion(cx));

        SETTING.with(|cell| cell.set(false));
        cx.update(sync_reduce_motion);
        let off = cx.update(|cx| reduce_motion(cx));

        assert!(on);
        assert!(!off);
    }
}
