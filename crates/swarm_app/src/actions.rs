//! Window-level actions and their key bindings.

use gpui::{Action, App, KeyBinding, NoAction, actions};
use swarm_term_view::{focus_key_bindings, shell_key_bindings};
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
        Quit,
        About,
        CheckForUpdates,
        AddProject,
        CloseTask,
        ToggleSidebar,
        OpenHelp,
        Hide,
        HideOthers,
        ShowAll,
        Minimize,
        Zoom,
        BringAllToFront,
        ToggleFullScreen,
        /// Opens the Field guide of caught creatures.
        OpenFieldGuide,
        /// Starts a report of an issue with Swarm.
        ReportIssue,
        /// Opens the pull request description as a large tile.
        ExpandDescription,
        /// Moves the focused tile to the place of its neighbour on the left.
        MoveTileLeft,
        /// Moves the focused tile to the place of its neighbour on the right.
        MoveTileRight,
        /// Moves the focused tile to the place of its neighbour above.
        MoveTileUp,
        /// Moves the focused tile to the place of its neighbour below.
        MoveTileDown
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
        TreeActivate,
        /// Moves the cursor of a file tree to the first row.
        TreeFirst,
        /// Moves the cursor of a file tree to the last row.
        TreeLast
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
    SwarmView,
    Review,
    FileTree,
    Settings,
}

impl ShortcutGroup {
    pub(crate) const ALL: [Self; 6] = [
        Self::Workspace,
        Self::Terminals,
        Self::SwarmView,
        Self::Review,
        Self::FileTree,
        Self::Settings,
    ];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Workspace => "Workspace",
            Self::Terminals => "Terminals",
            Self::SwarmView => "Swarm view",
            Self::Review => "Review",
            Self::FileTree => "File tree",
            Self::Settings => "Settings",
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

fn shortcut(group: ShortcutGroup, name: &'static str, binding: KeyBinding) -> Shortcut {
    Shortcut {
        group,
        name,
        binding,
    }
}

/// Every key binding of the app. [`bind_keys`] registers them and the Keyboard page lists
/// them, so a new binding shows up on the page without more work.
pub(crate) fn shortcuts() -> Vec<Shortcut> {
    use ShortcutGroup::{Terminals, Workspace};

    let root = Some(ROOT_CONTEXT);

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
        shortcut(
            Workspace,
            "Toggle sidebar",
            KeyBinding::new("cmd-b", ToggleSidebar, root),
        ),
        shortcut(
            Workspace,
            "Expand pull request description",
            KeyBinding::new("cmd-shift-d", ExpandDescription, root),
        ),
        shortcut(Workspace, "Quit", KeyBinding::new("cmd-q", Quit, None)),
        shortcut(
            Workspace,
            "Hide Swarm",
            KeyBinding::new("cmd-h", Hide, None),
        ),
        shortcut(
            Workspace,
            "Hide others",
            KeyBinding::new("cmd-alt-h", HideOthers, None),
        ),
        shortcut(
            Workspace,
            "Minimize window",
            KeyBinding::new("cmd-m", Minimize, None),
        ),
        shortcut(
            Workspace,
            "Full screen",
            KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
        ),
    ];

    shortcuts.extend([
        shortcut(
            Terminals,
            "Move tile left",
            KeyBinding::new("ctrl-cmd-left", MoveTileLeft, root),
        ),
        shortcut(
            Terminals,
            "Move tile right",
            KeyBinding::new("ctrl-cmd-right", MoveTileRight, root),
        ),
        shortcut(
            Terminals,
            "Move tile up",
            KeyBinding::new("ctrl-cmd-up", MoveTileUp, root),
        ),
        shortcut(
            Terminals,
            "Move tile down",
            KeyBinding::new("ctrl-cmd-down", MoveTileDown, root),
        ),
    ]);

    shortcuts.extend(FOCUS_KEYS.iter().enumerate().map(|(index, key)| {
        shortcut(
            Terminals,
            "Focus agent",
            KeyBinding::new(key, FocusAgent { index }, root),
        )
    }));

    let [shell_tab, shell_back_tab] = shell_key_bindings();
    let [next_stop, previous_stop] = focus_key_bindings();
    shortcuts.extend([
        shortcut(Terminals, "Tab in terminal", shell_tab),
        shortcut(Terminals, "Back-tab in terminal", shell_back_tab),
        shortcut(Terminals, "Next focus stop", next_stop),
        shortcut(Terminals, "Previous focus stop", previous_stop),
    ]);

    shortcuts.extend(review_shortcuts());

    shortcuts
}

/// A key the app handles in a place that registers no binding. The rows only name the keys
/// for the Keyboard page, and `bind_keys` does not register them.
fn listed(group: ShortcutGroup, name: &'static str, keys: &'static str) -> Shortcut {
    shortcut(group, name, KeyBinding::new(keys, NoAction, None))
}

/// The keys handled by the controls themselves, which the Keyboard page lists too.
pub(crate) fn listed_shortcuts() -> Vec<Shortcut> {
    use ShortcutGroup::{Review, Settings, SwarmView, Terminals, Workspace};

    vec![
        listed(Workspace, "Open context menu", "shift-f10"),
        listed(Terminals, "Make tile shorter", "up"),
        listed(Terminals, "Make tile taller", "down"),
        listed(SwarmView, "Previous card", "left"),
        listed(SwarmView, "Next card", "right"),
        listed(SwarmView, "Card above", "up"),
        listed(SwarmView, "Card below", "down"),
        listed(SwarmView, "Focus agent tile", "enter"),
        listed(Review, "Post comment or reply", "cmd-enter"),
        listed(Review, "Cursor on old side of split", "alt-left"),
        listed(Review, "Cursor on new side of split", "alt-right"),
        listed(Settings, "Close settings", "cmd-w"),
        listed(Settings, "Previous page", "up"),
        listed(Settings, "Next page", "down"),
    ]
}

/// The keys of the Review view and of the file trees.
fn review_shortcuts() -> Vec<Shortcut> {
    use ShortcutGroup::{FileTree, Review};

    let review = Some(REVIEW_KEYS);
    let body = Some(BODY_KEYS);
    let tree = Some(FILE_TREE_CONTEXT);

    vec![
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
            "Previous line in diff",
            KeyBinding::new("up", PreviousLine, body),
        ),
        shortcut(
            Review,
            "Next line in diff",
            KeyBinding::new("down", NextLine, body),
        ),
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
            "First row",
            KeyBinding::new("home", TreeFirst, tree),
        ),
        shortcut(FileTree, "Last row", KeyBinding::new("end", TreeLast, tree)),
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
    ]
}

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys(shortcuts().into_iter().map(|shortcut| shortcut.binding));
    cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
}

#[cfg(test)]
mod tests {
    use gpui::Keystroke;

    use super::*;

    #[test]
    fn the_shortcut_list_has_toggle_sidebar() {
        let toggle = shortcuts()
            .into_iter()
            .find(|shortcut| shortcut.name == "Toggle sidebar");

        let keys: Option<Vec<Keystroke>> = toggle.map(|shortcut| {
            shortcut
                .binding
                .keystrokes()
                .iter()
                .map(|keystroke| keystroke.inner().clone())
                .collect()
        });

        assert_eq!(
            keys,
            Some(vec![Keystroke::parse("cmd-b").expect("a valid keystroke")])
        );
    }

    #[test]
    fn the_move_tile_keys_are_listed_under_terminals() {
        let listed: Vec<(ShortcutGroup, &str, Keystroke)> = shortcuts()
            .into_iter()
            .filter(|shortcut| shortcut.name.starts_with("Move tile"))
            .map(|shortcut| {
                let keystroke = shortcut.binding.keystrokes()[0].inner().clone();
                (shortcut.group, shortcut.name, keystroke)
            })
            .collect();
        let key = |keys: &'static str| Keystroke::parse(keys).expect("a valid keystroke");

        assert_eq!(
            listed,
            [
                (
                    ShortcutGroup::Terminals,
                    "Move tile left",
                    key("ctrl-cmd-left")
                ),
                (
                    ShortcutGroup::Terminals,
                    "Move tile right",
                    key("ctrl-cmd-right")
                ),
                (ShortcutGroup::Terminals, "Move tile up", key("ctrl-cmd-up")),
                (
                    ShortcutGroup::Terminals,
                    "Move tile down",
                    key("ctrl-cmd-down")
                ),
            ]
        );
    }
}
