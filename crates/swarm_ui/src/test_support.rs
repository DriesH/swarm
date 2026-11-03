//! Helpers for Tab-walk tests: press Tab through a window and read the name of each stop.
//!
//! Components named with [`crate::a11y::accessible`] answer the [`NameStop`] action with
//! `"{role:?} {label}"`. A focusable element that does not answer is "unnamed".

use gpui::{FocusHandle, Global, VisualTestContext, actions};

actions!(swarm_ui_test, [NameStop]);

/// The name the last [`NameStop`] reached.
#[derive(Default)]
pub struct LastStop(pub Option<String>);

impl Global for LastStop {}

/// Most steps a walk takes before it gives up.
const MAX_STEPS: usize = 200;

/// Names the focused element: "nothing" with no focus, "not drawn" when focus sits on an
/// element that is not in the last frame of `root`, "unnamed" when the element is not
/// built with `accessible`, else its `"{role:?} {label}"`.
///
/// An unnamed focusable inside a named one reports the parent's name.
pub fn focused_stop(cx: &mut VisualTestContext, root: &FocusHandle) -> String {
    cx.update(|window, cx| {
        cx.set_global(LastStop(None));

        let Some(focused) = window.focused(cx) else {
            return "nothing".to_owned();
        };

        if !root.contains(&focused, window) {
            return "not drawn".to_owned();
        }

        focused.dispatch_action(&NameStop, window, cx);

        cx.global::<LastStop>()
            .0
            .clone()
            .unwrap_or_else(|| "unnamed".to_owned())
    })
}

/// Presses `key` until focus is back where it started and returns the name of every stop,
/// starting with the first. Panics if a press leaves focus where it was or the walk passes
/// 200 steps.
pub fn tab_walk(cx: &mut VisualTestContext, root: &FocusHandle, key: &str) -> Vec<String> {
    let start = cx.update(|window, cx| window.focused(cx));
    let mut stops = vec![focused_stop(cx, root)];
    let mut previous = start.clone();

    for _ in 0..MAX_STEPS {
        cx.simulate_keystrokes(key);

        let current = cx.update(|window, cx| window.focused(cx));

        assert!(
            current != previous,
            "{key} did not move focus after {stops:?}"
        );

        if current == start {
            return stops;
        }

        stops.push(focused_stop(cx, root));
        previous = current;
    }

    panic!("{key} did not come back to the start in {MAX_STEPS} steps: {stops:?}");
}
