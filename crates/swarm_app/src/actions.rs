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
        CreatePullRequest,
        SetUpGithub,
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
        ToggleViewed,
        /// Opens the comment box on the line cursor.
        CommentOnLine,
        /// Moves the line cursor up one commentable line.
        PreviousLine,
        /// Moves the line cursor down one commentable line.
        NextLine,
        /// Closes the comment box and drops its draft.
        CancelComment,
        /// Closes the reply field of a thread and drops its text.
        CancelReply
    ]
);

actions!(
    file_tree,
    [
        /// Moves the cursor of a file tree to the row above.
        TreePrevious,
        /// Moves the cursor of a file tree to the row below.
        TreeNext,
        /// Closes the folder under the cursor, or moves to its parent.
        TreeCollapse,
        /// Opens the folder under the cursor, or moves to its first row.
        TreeExpand,
        /// Opens the file under the cursor in the diff, or opens or closes the folder.
        TreeActivate
    ]
);

/// Key context of a file tree, which is one stop for Tab and moves a cursor with the arrows.
pub(crate) const FILE_TREE_CONTEXT: &str = "FileTree";

/// Key context of the Review view.
pub(crate) const REVIEW_CONTEXT: &str = "Review";

/// Key context of the diff body, where the line cursor lives.
pub(crate) const REVIEW_BODY_CONTEXT: &str = "ReviewBody";

/// Key context of the box for a new comment.
pub(crate) const COMPOSER_CONTEXT: &str = "CommentComposer";

/// Key context of the reply field of a thread.
pub(crate) const REPLY_CONTEXT: &str = "CommentReply";

/// Plain keys of Review never fire while a text field has focus.
const REVIEW_KEYS: &str = "Review && !TextInput && !TextArea";
const BODY_KEYS: &str = "ReviewBody && !TextInput && !TextArea";

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

    let review = Some(REVIEW_KEYS);
    let body = Some(BODY_KEYS);
    let tree = Some(FILE_TREE_CONTEXT);
    bindings.extend([
        KeyBinding::new("j", NextFile, review),
        KeyBinding::new("k", PreviousFile, review),
        KeyBinding::new("v", ToggleViewed, review),
        KeyBinding::new("c", CommentOnLine, review),
        KeyBinding::new("up", PreviousLine, body),
        KeyBinding::new("down", NextLine, body),
        KeyBinding::new("up", TreePrevious, tree),
        KeyBinding::new("down", TreeNext, tree),
        KeyBinding::new("left", TreeCollapse, tree),
        KeyBinding::new("right", TreeExpand, tree),
        KeyBinding::new("enter", TreeActivate, tree),
        KeyBinding::new("space", TreeActivate, tree),
        KeyBinding::new("escape", CancelComment, Some(COMPOSER_CONTEXT)),
        KeyBinding::new("escape", CancelReply, Some(REPLY_CONTEXT)),
    ]);

    cx.bind_keys(bindings);
    cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
}
