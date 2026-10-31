//! Window-level actions and their key bindings.

use gpui::{Action, App, KeyBinding, actions};
use swarm_ui::ROOT_CONTEXT;

actions!(
    swarm_app,
    [
        ShowTerminals,
        ShowSwarm,
        ShowReview,
        ToggleFiles,
        ToggleChanges,
        NewTask,
        PlanWithOrchestrator,
        NewTerminal,
        OpenSettings,
        ToggleCommandPalette,
        Quit
    ]
);

actions!(
    review,
    [
        /// Selects the next changed file.
        NextFile,
        /// Selects the previous changed file.
        PreviousFile,
        /// Marks the selected file viewed, or not viewed.
        ToggleViewed
    ]
);

/// Key context of the Review view.
pub(crate) const REVIEW_CONTEXT: &str = "Review";

/// Focuses the agent at `index` in the open task (the orchestrator is 0).
#[derive(Clone, PartialEq, Action)]
#[action(namespace = swarm_app, no_json)]
pub(crate) struct FocusAgent {
    pub index: usize,
}

const FOCUS_KEYS: [&str; 9] = [
    "cmd-1", "cmd-2", "cmd-3", "cmd-4", "cmd-5", "cmd-6", "cmd-7", "cmd-8", "cmd-9",
];

pub(crate) fn bind_keys(cx: &mut App) {
    let root = Some(ROOT_CONTEXT);

    let mut bindings = vec![
        KeyBinding::new("cmd-shift-r", ShowReview, root),
        KeyBinding::new("cmd-t", NewTask, root),
        KeyBinding::new("cmd-shift-t", PlanWithOrchestrator, root),
        KeyBinding::new("cmd-alt-t", NewTerminal, root),
        KeyBinding::new("cmd-,", OpenSettings, root),
        KeyBinding::new("cmd-k", ToggleCommandPalette, root),
        KeyBinding::new("cmd-q", Quit, None),
    ];

    bindings.extend(
        FOCUS_KEYS
            .iter()
            .enumerate()
            .map(|(index, key)| KeyBinding::new(key, FocusAgent { index }, root)),
    );

    let review = Some(REVIEW_CONTEXT);
    bindings.extend([
        KeyBinding::new("j", NextFile, review),
        KeyBinding::new("k", PreviousFile, review),
        KeyBinding::new("v", ToggleViewed, review),
    ]);

    cx.bind_keys(bindings);
    cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
}
