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

/// The heading of a list of shortcuts on the Keyboard page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShortcutGroup {
    Workspace,
    Terminals,
    Review,
    FileTree,
}

impl ShortcutGroup {
    pub(crate) const ALL: [Self; 4] = [
        Self::Workspace,
        Self::Terminals,
        Self::Review,
        Self::FileTree,
    ];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Workspace => "Workspace",
            Self::Terminals => "Terminals",
            Self::Review => "Review",
            Self::FileTree => "File tree",
        }
    }
}

/// A key binding with the words the Keyboard page shows for it. Bindings that do the same
/// thing for another key, like the nine agent keys, share a name.
pub(crate) struct Shortcut {
    pub group: ShortcutGroup,
    pub name: &'static str,
    pub binding: KeyBinding,
}

/// Every key binding of the app. [`bind_keys`] registers them and the Keyboard page lists
/// them, so a new binding shows up on the page without more work.
pub(crate) fn shortcuts() -> Vec<Shortcut> {
    use ShortcutGroup::{FileTree, Review, Terminals, Workspace};

    let root = Some(ROOT_CONTEXT);
    let shortcut = |group, name, binding| Shortcut {
        group,
        name,
        binding,
    };

    let mut shortcuts = vec![
        shortcut(
            Workspace,
            "Open Review",
            KeyBinding::new("cmd-shift-r", ShowReview, root),
        ),
        shortcut(
            Workspace,
            "New task",
            KeyBinding::new("cmd-t", NewTask, root),
        ),
        shortcut(
            Workspace,
            "Plan with orchestrator",
            KeyBinding::new("cmd-shift-t", PlanWithOrchestrator, root),
        ),
        shortcut(
            Terminals,
            "New terminal",
            KeyBinding::new("cmd-alt-t", NewTerminal, root),
        ),
        shortcut(
            Workspace,
            "Open settings",
            KeyBinding::new("cmd-,", OpenSettings, root),
        ),
        shortcut(
            Workspace,
            "Command palette",
            KeyBinding::new("cmd-k", ToggleCommandPalette, root),
        ),
        shortcut(Workspace, "Quit", KeyBinding::new("cmd-q", Quit, None)),
    ];

    shortcuts.extend(FOCUS_KEYS.iter().enumerate().map(|(index, key)| {
        shortcut(
            Terminals,
            "Focus agent",
            KeyBinding::new(key, FocusAgent { index }, root),
        )
    }));

    let review = Some(REVIEW_KEYS);
    let body = Some(BODY_KEYS);
    let tree = Some(FILE_TREE_CONTEXT);
    shortcuts.extend([
        shortcut(Review, "Next file", KeyBinding::new("j", NextFile, review)),
        shortcut(
            Review,
            "Previous file",
            KeyBinding::new("k", PreviousFile, review),
        ),
        shortcut(
            Review,
            "Mark file viewed",
            KeyBinding::new("v", ToggleViewed, review),
        ),
        shortcut(
            Review,
            "Comment on line",
            KeyBinding::new("c", CommentOnLine, review),
        ),
        shortcut(
            Review,
            "Previous line",
            KeyBinding::new("up", PreviousLine, body),
        ),
        shortcut(Review, "Next line", KeyBinding::new("down", NextLine, body)),
        shortcut(
            FileTree,
            "Previous row",
            KeyBinding::new("up", TreePrevious, tree),
        ),
        shortcut(
            FileTree,
            "Next row",
            KeyBinding::new("down", TreeNext, tree),
        ),
        shortcut(
            FileTree,
            "Close folder",
            KeyBinding::new("left", TreeCollapse, tree),
        ),
        shortcut(
            FileTree,
            "Open folder",
            KeyBinding::new("right", TreeExpand, tree),
        ),
        shortcut(
            FileTree,
            "Open row",
            KeyBinding::new("enter", TreeActivate, tree),
        ),
        shortcut(
            FileTree,
            "Open row",
            KeyBinding::new("space", TreeActivate, tree),
        ),
        shortcut(
            Review,
            "Cancel comment",
            KeyBinding::new("escape", CancelComment, Some(COMPOSER_CONTEXT)),
        ),
        shortcut(
            Review,
            "Cancel reply",
            KeyBinding::new("escape", CancelReply, Some(REPLY_CONTEXT)),
        ),
    ]);

    shortcuts
}

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys(shortcuts().into_iter().map(|shortcut| shortcut.binding));
    cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
}
