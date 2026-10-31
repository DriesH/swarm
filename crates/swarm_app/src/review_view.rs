//! The Review view: the changed files of a task and their diff.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, Context, Div, Entity, FocusHandle, Focusable as _, FontWeight,
    ListAlignment, ListOffset, ListState, Role, ScrollStrategy, SharedString, Stateful,
    StyleRefinement, UniformListScrollHandle, WeakEntity, Window, div, list, px, uniform_list,
};
use swarm_protocol::{
    CommentId, DiffLine, DiffLineKind, DiffSide, FileChange, FileDiff, Fold, ReviewComment,
    ReviewSource, TaskDiff,
};
use swarm_ui::fonts::MONO_FONT;
use swarm_ui::prelude::*;
use swarm_ui::styles::focus_ring;
use swarm_ui::{StatusPill, TagTone, ThemeColors};

use crate::actions::{
    CancelComment, CommentOnLine, CreatePullRequest, FILE_TREE_CONTEXT, NextFile, NextLine,
    PreviousFile, PreviousLine, REVIEW_BODY_CONTEXT, REVIEW_CONTEXT, SetUpGithub, ToggleViewed,
    TreeActivate, TreeCollapse, TreeExpand, TreeNext, TreePrevious,
};
use crate::file_tree::{self, Entry, EntryKind, Key, Mark, Move};
use crate::markdown::Markdown;
use crate::model::{AgentKind, TaskId};
use crate::pull_request::PrOffer;
use crate::review_comments::refusal;
use crate::review_model::{
    AgentLabel, BodyRows, DiffStyle, FileInfo, FileKey, FileLoad, FileLoads, LinePos, LoadKey,
    Marks, Note, Row, Side, Thread, build_rows, count_comments, file_infos, is_folded, plural,
    sample_lines, thousands, visible_files, worktree_count,
};
use crate::workspace::{View, Workspace};

mod comments;

use comments::{Composer, ReplyBox, ReplyFocus};

const NAV_WIDTH: f32 = 272.;
const HEADER_HEIGHT: f32 = 46.;
const CARD_HEADER: f32 = 38.;
const CARD_GAP: f32 = 14.;
const LINE_HEIGHT: f32 = 21.;
const NUMBER_WIDTH: f32 = 40.;
const SIGN_WIDTH: f32 = 14.;
/// The place gives way long before the status pill does when the header is short.
const PLACE_SHRINK: f32 = 1_000.;
const NO_NEWLINE: &str = "No newline at end of file";
const SOURCES: [ReviewSource; 2] = [ReviewSource::Live, ReviewSource::Merged];
const STYLES: [DiffStyle; 2] = [DiffStyle::Unified, DiffStyle::Split];

/// A request to scroll to a file, from the Swarm view or the Changes panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Jump {
    /// Goes up with every request, so one request is handled once.
    pub serial: u64,
    pub path: PathBuf,
    pub worktree: Option<PathBuf>,
}

/// What the workspace hands the view.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReviewInput {
    pub task: TaskId,
    pub source: ReviewSource,
    pub diff: Option<Rc<TaskDiff>>,
    pub filter: Option<AgentLabel>,
    pub agents: Rc<[AgentLabel]>,
    /// Workers that work or wait for the user.
    pub working: usize,
    pub base: SharedString,
    pub branch: SharedString,
    pub approved: Option<u64>,
    /// What an approved task offers next: Create PR, Set up GitHub, or nothing.
    pub offer: PrOffer,
    pub approving: bool,
    pub approve_error: Option<SharedString>,
    pub jump: Option<Jump>,
    /// The comments of the task, oldest first.
    pub comments: Rc<[ReviewComment]>,
    /// Which folded files of the diff the user asked for, and how that went.
    pub loads: Rc<FileLoads>,
    /// Request changes is on its way.
    pub requesting: bool,
    /// The fixed clock of fake data, else the system clock.
    pub fixed_now: Option<u64>,
}

impl ReviewInput {
    /// True when the files and rows must be worked out again. New comments alone do
    /// not: see [`ReviewView::place_comments`].
    fn changes_files(&self, other: &Self) -> bool {
        self.task != other.task
            || self.source != other.source
            || self.diff != other.diff
            || self.filter != other.filter
            || self.agents != other.agents
            || self.loads != other.loads
    }
}

/// Where the top of the body sits, so a new diff keeps the reader in place.
struct Anchor {
    key: FileKey,
    row_in_file: usize,
    offset: gpui::Pixels,
}

pub(crate) struct ReviewView {
    workspace: WeakEntity<Workspace>,
    input: Option<ReviewInput>,
    pub(crate) style: DiffStyle,
    selected: HashMap<TaskId, FileKey>,
    /// The digest each viewed file had when it was marked.
    viewed: HashMap<TaskId, HashMap<FileKey, u64>>,
    collapsed: HashMap<TaskId, HashSet<FileKey>>,
    handled_jump: u64,
    infos: Rc<[FileInfo]>,
    shown: Rc<[usize]>,
    body: Rc<BodyRows>,
    /// The visible rows of the file tree.
    tree: Rc<[Entry]>,
    /// The folders the user closed, by folder path.
    closed: HashMap<TaskId, HashSet<String>>,
    /// What the tree cursor stands on. Without one it stands on the selected file.
    tree_cursor: HashMap<TaskId, Mark>,
    body_list: ListState,
    nav_scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
    /// Focus of the file tree, which is one stop for Tab.
    nav_focus: FocusHandle,
    /// Focus of the diff body, where the line cursor keys work.
    body_focus: FocusHandle,
    threads: Rc<[Thread]>,
    /// Open threads per file of the diff.
    open_counts: Rc<[u32]>,
    /// The replies of each top-level comment, as indexes into the comments.
    replies_of: Rc<HashMap<CommentId, Vec<usize>>>,
    parsed: HashMap<CommentId, Rc<Markdown>>,
    cursor: Option<comments::Cursor>,
    composer: Composer,
    reply_boxes: HashMap<CommentId, ReplyBox>,
    /// Failures to show in a card.
    card_errors: HashMap<CommentId, SharedString>,
    /// Resolved threads the user opened.
    unfolded: HashSet<CommentId>,
    /// Focus the comment box at the next frame.
    focus_composer: bool,
    /// Focus the diff body at the next frame.
    focus_body: bool,
    /// Focus a reply field or its button at the next frame.
    focus_reply: Option<ReplyFocus>,
    /// Redraws the relative times of delivered comments.
    clock: Option<gpui::Task<()>>,
}

fn update_workspace(
    workspace: &WeakEntity<Workspace>,
    cx: &mut App,
    apply: impl FnOnce(&mut Workspace, &mut Context<Workspace>),
) {
    if let Err(error) = workspace.update(cx, apply) {
        log::debug!("the window closed before Review answered: {error}");
    }
}

/// Approve, then once the Merged diff is approved: Create PR, Set up GitHub, or Approved.
fn next_step_button(
    approved: bool,
    enabled: bool,
    offer: PrOffer,
    workspace: WeakEntity<Workspace>,
) -> Button {
    let next = approved
        .then_some(offer)
        .filter(|offer| *offer != PrOffer::None);

    // One id for the whole step, so keyboard focus stays when Approve turns into Create PR.
    match next {
        Some(PrOffer::Create) => Button::new("review-approve", "Create PR")
            .variant(ButtonVariant::Primary)
            .debug_selector(|| "review-create-pr".to_owned())
            .on_click(|_, window, cx| {
                window.dispatch_action(Box::new(CreatePullRequest), cx);
            }),
        Some(_) => Button::new("review-approve", "Set up GitHub")
            .variant(ButtonVariant::Primary)
            .debug_selector(|| "review-set-up-github".to_owned())
            .on_click(|_, window, cx| {
                window.dispatch_action(Box::new(SetUpGithub), cx);
            }),
        None => Button::new(
            "review-approve",
            if approved { "Approved" } else { "Approve" },
        )
        .variant(ButtonVariant::Primary)
        .debug_selector(|| "review-approve".to_owned())
        .when(approved, |button| button.icon(IconName::Check))
        .disabled(!enabled)
        .on_click(move |_, _, cx| {
            update_workspace(&workspace, cx, Workspace::approve_review);
        }),
    }
}

impl ReviewView {
    pub(crate) fn new(workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        Self {
            workspace,
            input: None,
            style: DiffStyle::default(),
            selected: HashMap::new(),
            viewed: HashMap::new(),
            collapsed: HashMap::new(),
            handled_jump: 0,
            infos: Rc::from([]),
            shown: Rc::from([]),
            body: Rc::default(),
            tree: Rc::from([]),
            closed: HashMap::new(),
            tree_cursor: HashMap::new(),
            body_list: ListState::new(0, ListAlignment::Top, px(400.)),
            nav_scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            nav_focus: cx.focus_handle().tab_stop(true),
            body_focus: cx.focus_handle().tab_stop(true),
            threads: Rc::from([]),
            open_counts: Rc::from([]),
            replies_of: Rc::default(),
            parsed: HashMap::new(),
            cursor: None,
            composer: Composer::new(cx),
            reply_boxes: HashMap::new(),
            card_errors: HashMap::new(),
            unfolded: HashSet::new(),
            focus_composer: false,
            focus_body: false,
            focus_reply: None,
            clock: None,
        }
    }

    /// The handle that gets focus when Review opens: the diff body.
    pub(crate) fn body_focus(&self) -> &FocusHandle {
        &self.body_focus
    }

    fn diff(&self) -> Option<&Rc<TaskDiff>> {
        self.input.as_ref()?.diff.as_ref()
    }

    fn task(&self) -> Option<TaskId> {
        self.input.as_ref().map(|input| input.task)
    }

    /// Takes what the workspace shows now. Only changed input redoes any work.
    pub(crate) fn show(&mut self, input: ReviewInput, cx: &mut Context<Self>) {
        if self.input.as_ref() == Some(&input) {
            return;
        }

        let previous = self.input.replace(input.clone());
        let same_scope = previous
            .as_ref()
            .is_some_and(|old| old.task == input.task && old.source == input.source);

        if previous
            .as_ref()
            .is_none_or(|old| old.changes_files(&input))
        {
            self.forget_changed_files(&input);
            self.follow_composer();
            self.rebuild(same_scope);
        } else if previous
            .as_ref()
            .is_some_and(|old| old.comments != input.comments)
        {
            self.place_comments();
            self.after_card_change();
        }

        self.apply_jump(&input);
        self.sync_clock(cx);
        cx.notify();
    }

    /// Forgets the viewed mark of every file present with another digest, and opens its card.
    /// A file the diff does not hold keeps its mark.
    fn forget_changed_files(&mut self, input: &ReviewInput) {
        let Some(diff) = &input.diff else {
            return;
        };
        let infos = file_infos(diff);
        let (Some(viewed), collapsed) = (
            self.viewed.get_mut(&input.task),
            self.collapsed.entry(input.task).or_default(),
        ) else {
            return;
        };

        viewed.retain(|key, digest| {
            let current = infos.iter().find(|info| info.key == *key).map(|i| i.digest);
            let keep = current.is_none_or(|current| current == *digest);

            if !keep {
                collapsed.remove(key);
            }

            keep
        });
    }

    /// Takes the viewer back to an empty state, for a tab with no diff.
    pub(crate) fn clear(&mut self, cx: &mut Context<Self>) {
        if self.input.is_none() {
            return;
        }

        self.input = None;
        self.clock = None;
        self.rebuild(false);
        cx.notify();
    }

    fn rebuild(&mut self, keep_scroll: bool) {
        let anchor = keep_scroll.then(|| self.top_anchor()).flatten();

        let Some(input) = self.input.clone() else {
            self.clear_rows();
            return;
        };
        let Some(diff) = input.diff.as_ref() else {
            self.clear_rows();
            return;
        };

        self.infos = file_infos(diff).into();
        self.shown = visible_files(diff, input.filter.as_ref().map(|agent| agent.id)).into();
        self.rebuild_nav();
        self.place_comments();
        self.rebuild_rows(anchor);
    }

    fn clear_rows(&mut self) {
        self.infos = Rc::from([]);
        self.shown = Rc::from([]);
        self.tree = Rc::from([]);
        self.body = Rc::default();
        self.threads = Rc::from([]);
        self.body_list.reset(0);
    }

    fn rebuild_nav(&mut self) {
        let Some(input) = &self.input else {
            return;
        };
        let Some(diff) = &input.diff else {
            return;
        };
        let closed = self.closed.get(&input.task).cloned().unwrap_or_default();

        self.tree = file_tree::build(&diff.files, &self.shown, &input.agents, &closed).into();
    }

    fn rebuild_rows(&mut self, anchor: Option<Anchor>) {
        let Some(input) = &self.input else {
            return;
        };
        let Some(diff) = &input.diff else {
            return;
        };
        let collapsed = self.collapsed.get(&input.task);
        let loads = Rc::clone(&input.loads);
        let marks = Marks {
            threads: Rc::clone(&self.threads),
            composer: self.composer_at(),
        };

        self.body = Rc::new(build_rows(
            diff,
            &self.infos,
            &self.shown,
            (
                |file| {
                    self.infos
                        .get(file)
                        .is_some_and(|info| collapsed.is_some_and(|set| set.contains(&info.key)))
                },
                |file| {
                    diff.files
                        .get(file)
                        .is_some_and(|data| is_folded(data, &loads))
                },
            ),
            self.style,
            &marks,
        ));
        self.body_list.reset(self.body.rows.len());

        if let Some(anchor) = anchor {
            self.restore(&anchor);
        }
    }

    fn top_anchor(&self) -> Option<Anchor> {
        let top = self.body_list.logical_scroll_top();
        let file = self.body.rows.get(top.item_ix)?.file()?;
        let start = (*self.body.headers.get(file)?)?;

        Some(Anchor {
            key: self.infos.get(file)?.key.clone(),
            row_in_file: top.item_ix.saturating_sub(start),
            offset: top.offset_in_item,
        })
    }

    fn restore(&self, anchor: &Anchor) {
        let Some(file) = self.infos.iter().position(|info| info.key == anchor.key) else {
            return;
        };
        let Some(range) = self.body.range_of(file) else {
            return;
        };

        let row = (range.start + anchor.row_in_file).min(range.end.saturating_sub(1));
        let offset = if row == range.start + anchor.row_in_file {
            anchor.offset
        } else {
            px(0.)
        };

        self.body_list.scroll_to(ListOffset {
            item_ix: row,
            offset_in_item: offset,
        });
    }

    fn apply_jump(&mut self, input: &ReviewInput) {
        let Some(jump) = &input.jump else {
            return;
        };
        let Some(diff) = &input.diff else {
            return;
        };
        if jump.serial <= self.handled_jump {
            return;
        }
        self.handled_jump = jump.serial;

        let matches = |index: &&usize| {
            diff.files.get(**index).is_some_and(|file| {
                file.path == jump.path
                    && jump
                        .worktree
                        .as_ref()
                        .is_none_or(|worktree| *worktree == file.worktree)
            })
        };
        let Some(file) = self.shown.iter().find(matches).copied() else {
            return;
        };

        self.reveal(file);
        self.select(file);
    }

    /// Opens the closed folders above a file, so its row shows.
    fn reveal(&mut self, file: usize) {
        let (Some(task), Some(data)) = (self.task(), self.diff().and_then(|d| d.files.get(file)))
        else {
            return;
        };
        let path = data.path.clone();
        let Some(closed) = self.closed.get_mut(&task) else {
            return;
        };
        let before = closed.len();

        closed.retain(|folder| !path.starts_with(folder));

        if closed.len() != before {
            self.rebuild_nav();
        }
    }

    /// Selects the file and shows it at the top of the body.
    fn select(&mut self, file: usize) {
        let (Some(task), Some(info)) = (self.task(), self.infos.get(file)) else {
            return;
        };

        self.selected.insert(task, info.key.clone());
        self.tree_cursor.insert(task, Mark::File(info.key.clone()));
        self.scroll_to_file(file);
    }

    fn scroll_to_file(&self, file: usize) {
        if let Some(Some(row)) = self.body.headers.get(file) {
            self.body_list.scroll_to(ListOffset {
                item_ix: *row,
                offset_in_item: px(0.),
            });
        }

        self.scroll_nav_to_cursor();
    }

    fn scroll_nav_to_cursor(&self) {
        if let Some(row) = self.cursor_row() {
            self.nav_scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
    }

    /// The row of the tree cursor: where the user moved it, else the selected file.
    fn cursor_row(&self) -> Option<usize> {
        let task = self.task()?;
        let diff = self.diff()?;
        let mark = self.tree_cursor.get(&task).cloned().or_else(|| {
            let info = self.infos.get(self.selected_file()?)?;

            Some(Mark::File(info.key.clone()))
        })?;

        file_tree::row_of(&self.tree, &diff.files, &mark)
    }

    /// Puts the tree cursor on a row.
    fn put_cursor(&mut self, row: usize, cx: &mut Context<Self>) {
        let (Some(task), Some(diff), Some(entry)) = (self.task(), self.diff(), self.tree.get(row))
        else {
            return;
        };
        let Some(mark) = file_tree::mark_of(entry, &diff.files) else {
            return;
        };

        self.tree_cursor.insert(task, mark);
        self.scroll_nav_to_cursor();
        cx.notify();
    }

    /// Opens or closes the folder of a row. The cursor stays on it.
    fn set_folder_open(&mut self, row: usize, open: bool, cx: &mut Context<Self>) {
        let (Some(task), Some(entry)) = (self.task(), self.tree.get(row)) else {
            return;
        };
        let folder = entry.folder.to_string();
        let closed = self.closed.entry(task).or_default();

        if open {
            closed.remove(&folder);
        } else {
            closed.insert(folder.clone());
        }

        self.tree_cursor.insert(task, Mark::Folder(folder));
        self.rebuild_nav();
        self.scroll_nav_to_cursor();
        cx.notify();
    }

    /// What an arrow key does in the tree.
    fn move_tree(&mut self, key: Key, cx: &mut Context<Self>) {
        match file_tree::step(&self.tree, self.cursor_row(), key) {
            Move::Select(row) => self.put_cursor(row, cx),
            Move::Open(row) => self.set_folder_open(row, true, cx),
            Move::Close(row) => self.set_folder_open(row, false, cx),
            Move::Stay => {}
        }
    }

    /// Enter or Space: opens the file under the cursor in the diff, or toggles the folder.
    fn activate_row(&mut self, row: usize, cx: &mut Context<Self>) {
        match self.tree.get(row).map(|entry| entry.kind.clone()) {
            Some(EntryKind::File { file }) => self.select_file(file, cx),
            Some(EntryKind::Folder { open }) => self.set_folder_open(row, !open, cx),
            None => {}
        }
    }

    fn activate_cursor(&mut self, cx: &mut Context<Self>) {
        if let Some(row) = self.cursor_row() {
            self.activate_row(row, cx);
        }
    }

    /// The index in the diff of the selected file: the one picked, else the first shown.
    pub(crate) fn selected_file(&self) -> Option<usize> {
        let task = self.task()?;
        let picked = self.selected.get(&task).and_then(|key| {
            self.shown
                .iter()
                .find(|file| self.infos[**file].key == *key)
        });

        picked.or_else(|| self.shown.first()).copied()
    }

    fn is_viewed(&self, task: TaskId, file: usize) -> bool {
        let Some(info) = self.infos.get(file) else {
            return false;
        };

        self.viewed
            .get(&task)
            .and_then(|viewed| viewed.get(&info.key))
            .is_some_and(|digest| *digest == info.digest)
    }

    pub(crate) fn viewed_count(&self) -> usize {
        let Some(task) = self.task() else {
            return 0;
        };

        self.shown
            .iter()
            .filter(|file| self.is_viewed(task, **file))
            .count()
    }

    #[cfg(test)]
    pub(crate) fn is_file_viewed(&self, file: usize) -> bool {
        self.task().is_some_and(|task| self.is_viewed(task, file))
    }

    #[cfg(test)]
    pub(crate) fn is_collapsed(&self, file: usize) -> bool {
        let (Some(task), Some(info)) = (self.task(), self.infos.get(file)) else {
            return false;
        };

        self.collapsed
            .get(&task)
            .is_some_and(|set| set.contains(&info.key))
    }

    #[cfg(test)]
    pub(crate) fn rows(&self) -> &[Row] {
        &self.body.rows
    }

    #[cfg(test)]
    pub(crate) fn shown_files(&self) -> &[usize] {
        &self.shown
    }

    /// How the lines of a folded file stand, for tests.
    #[cfg(test)]
    pub(crate) fn load_of(&self, file: usize) -> Option<FileLoad> {
        let info = self.infos.get(file)?;

        self.input.as_ref()?.loads.get(&info.key).cloned()
    }

    #[cfg(test)]
    pub(crate) fn tree_rows(&self) -> &[Entry] {
        &self.tree
    }

    #[cfg(test)]
    pub(crate) fn tree_cursor_row(&self) -> Option<usize> {
        self.cursor_row()
    }

    #[cfg(test)]
    pub(crate) fn tree_focus(&self) -> &FocusHandle {
        &self.nav_focus
    }

    pub(crate) fn toggle_viewed(&mut self, file: usize, cx: &mut Context<Self>) {
        let (Some(task), Some(info)) = (self.task(), self.infos.get(file)) else {
            return;
        };
        let (key, digest) = (info.key.clone(), info.digest);
        let viewed = self.is_viewed(task, file);

        let marks = self.viewed.entry(task).or_default();
        let collapsed = self.collapsed.entry(task).or_default();

        if viewed {
            marks.remove(&key);
            collapsed.remove(&key);
        } else {
            marks.insert(key.clone(), digest);
            collapsed.insert(key);
        }

        self.after_card_change();
        cx.notify();
    }

    pub(crate) fn toggle_collapsed(&mut self, file: usize, cx: &mut Context<Self>) {
        let (Some(task), Some(info)) = (self.task(), self.infos.get(file)) else {
            return;
        };
        let key = info.key.clone();
        let collapsed = self.collapsed.entry(task).or_default();

        if !collapsed.remove(&key) {
            collapsed.insert(key);
        }

        self.after_card_change();
        cx.notify();
    }

    fn after_card_change(&mut self) {
        let anchor = self.top_anchor();

        self.rebuild_rows(anchor);
    }

    pub(crate) fn select_file(&mut self, file: usize, cx: &mut Context<Self>) {
        self.select(file);
        cx.notify();
    }

    pub(crate) fn next_file(&mut self, cx: &mut Context<Self>) {
        self.step_file(1, cx);
    }

    pub(crate) fn previous_file(&mut self, cx: &mut Context<Self>) {
        self.step_file(-1, cx);
    }

    fn step_file(&mut self, step: isize, cx: &mut Context<Self>) {
        let Some(current) = self.selected_file() else {
            return;
        };
        let position = self
            .shown
            .iter()
            .position(|file| *file == current)
            .unwrap_or(0);
        let target = position
            .saturating_add_signed(step)
            .min(self.shown.len().saturating_sub(1));

        if let Some(file) = self.shown.get(target).copied() {
            self.select_file(file, cx);
        }
    }

    pub(crate) fn toggle_selected_viewed(&mut self, cx: &mut Context<Self>) {
        if let Some(file) = self.selected_file() {
            self.toggle_viewed(file, cx);
        }
    }

    pub(crate) fn set_style(&mut self, style: DiffStyle, cx: &mut Context<Self>) {
        if self.style == style {
            return;
        }

        self.style = style;
        self.after_card_change();
        cx.notify();
    }
}

fn pill_label(count: usize) -> SharedString {
    if count == 0 {
        "All agents done".into()
    } else if count == 1 {
        "1 agent working".into()
    } else {
        format!("{count} agents working").into()
    }
}

fn kind_icon(kind: AgentKind) -> IconName {
    match kind {
        AgentKind::ClaudeCode => IconName::ClaudeCode,
        AgentKind::Codex => IconName::Codex,
    }
}

/// The tag text of a folded file.
fn fold_label(fold: Fold) -> &'static str {
    match fold {
        Fold::Lock => "Lock file",
        Fold::Generated => "Generated",
        Fold::Large => "Large diff",
    }
}

fn counts(additions: u32, deletions: u32, size: LabelSize) -> Div {
    h_flex()
        .flex_none()
        .gap(px(4.))
        .child(
            Label::new(format!("+{additions}"))
                .mono()
                .size(size)
                .color(Color::Add),
        )
        .child(
            Label::new(format!("\u{2212}{deletions}"))
                .mono()
                .size(size)
                .color(Color::Del),
        )
}

fn centered(text: &'static str) -> Div {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(Label::new(text).color(Color::Faint))
}

impl Render for ReviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors().clone();

        if std::mem::take(&mut self.focus_composer) {
            window.focus(&self.composer.input.focus_handle(cx), cx);
        } else if std::mem::take(&mut self.focus_body) {
            window.focus(&self.body_focus, cx);
        } else {
            self.apply_reply_focus(window, cx);
        }

        let root = h_flex()
            .id("review-view")
            .key_context(REVIEW_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .on_action(cx.listener(|this, _: &NextFile, _, cx| this.next_file(cx)))
            .on_action(cx.listener(|this, _: &PreviousFile, _, cx| this.previous_file(cx)))
            .on_action(cx.listener(|this, _: &ToggleViewed, _, cx| {
                this.toggle_selected_viewed(cx);
            }))
            .on_action(cx.listener(|this, _: &CommentOnLine, _, cx| this.comment_on_line(cx)))
            .on_action(cx.listener(|this, _: &CancelComment, _, cx| this.cancel_composer(cx)));

        let Some(input) = self.input.clone() else {
            return root.child(centered("No changes")).into_any_element();
        };

        root.child(self.nav_panel(&input, &colors, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.header_bar(&input, &colors, cx))
                    .child(self.body_area(&input, &colors, cx)),
            )
            .into_any_element()
    }
}

impl ReviewView {
    #[expect(
        clippy::too_many_lines,
        reason = "one flat element tree reads best in one place"
    )]
    fn nav_panel(
        &self,
        input: &ReviewInput,
        colors: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let total = self.shown.len();
        let viewed = self.viewed_count();
        let fraction = if total == 0 {
            0.
        } else {
            f32::from(u16::try_from(viewed).unwrap_or(u16::MAX))
                / f32::from(u16::try_from(total).unwrap_or(u16::MAX))
        };
        let workspace = self.workspace.clone();

        let summary = h_flex()
            .h(px(24.))
            .px(px(8.))
            .child(
                Label::new(plural(
                    u32::try_from(total).unwrap_or(u32::MAX),
                    "file",
                    "files",
                ))
                .size(LabelSize::Small)
                .weight(FontWeight::MEDIUM),
            )
            .child(div().flex_1())
            .child(
                Label::new(format!("{viewed} / {total} viewed"))
                    .mono()
                    .size(LabelSize::XSmall)
                    .color(Color::Faint),
            );

        let progress = div()
            .id("review-progress")
            .debug_selector(|| "review-progress".to_owned())
            .role(Role::ProgressIndicator)
            .aria_label(format!("{viewed} of {total} files viewed"))
            .mx(px(8.))
            .mt(px(4.))
            .mb(px(10.))
            .h(px(3.))
            .rounded(px(2.))
            .bg(colors.soft)
            .child(
                div()
                    .h(px(3.))
                    .w(gpui::relative(fraction))
                    .rounded(px(2.))
                    .bg(colors.fg),
            );

        let chip = input.filter.as_ref().map(|agent| {
            let name = agent.name.clone();
            let workspace = workspace.clone();

            h_flex().px(px(8.)).pb(px(8.)).child(
                h_flex()
                    .id("review-filter")
                    .debug_selector(|| "review-filter".to_owned())
                    .role(Role::Button)
                    .aria_label(format!("Remove filter, {name}"))
                    .focusable()
                    .tab_stop(true)
                    .cursor_pointer()
                    .h(px(24.))
                    .pl(px(8.))
                    .pr(px(6.))
                    .gap(px(6.))
                    .rounded(px(6.))
                    .bg(colors.soft)
                    .hover({
                        let active = colors.active;
                        move |style| style.bg(active)
                    })
                    .focus_visible({
                        let accent = colors.accent;
                        move |style| focus_ring(style, accent)
                    })
                    .on_click(move |_, _, cx| {
                        update_workspace(&workspace, cx, |workspace, cx| {
                            workspace.clear_review_agent(cx);
                        });
                    })
                    .child(Icon::new(kind_icon(agent.kind)).size(IconSize::Small))
                    .child(Label::new(name).size(LabelSize::Small))
                    .child(
                        Icon::new(IconName::Close)
                            .size(IconSize::XSmall)
                            .color(Color::Muted),
                    ),
            )
        });

        let items = uniform_list(
            "review-nav-list",
            self.tree.len(),
            cx.processor(|this, range: Range<usize>, window, cx| this.nav_items(range, window, cx)),
        )
        .track_scroll(&self.nav_scroll)
        .size_full();

        let tree = div()
            .id("review-tree")
            .debug_selector(|| "review-tree".to_owned())
            .role(Role::Tree)
            .aria_label("File tree")
            .key_context(FILE_TREE_CONTEXT)
            .track_focus(&self.nav_focus)
            .on_action(cx.listener(|this, _: &TreePrevious, _, cx| this.move_tree(Key::Up, cx)))
            .on_action(cx.listener(|this, _: &TreeNext, _, cx| this.move_tree(Key::Down, cx)))
            .on_action(cx.listener(|this, _: &TreeCollapse, _, cx| this.move_tree(Key::Left, cx)))
            .on_action(cx.listener(|this, _: &TreeExpand, _, cx| this.move_tree(Key::Right, cx)))
            .on_action(cx.listener(|this, _: &TreeActivate, _, cx| this.activate_cursor(cx)))
            .flex_1()
            .min_h_0()
            .px(px(4.))
            .child(items);

        v_flex()
            .id("review-nav")
            .role(Role::Navigation)
            .aria_label("Changed files")
            .w(px(NAV_WIDTH))
            .flex_none()
            .h_full()
            .pt(px(12.))
            .pb(px(10.))
            .border_r_1()
            .border_color(colors.line)
            .child(div().px(px(8.)).child(summary))
            .child(div().px(px(8.)).child(progress))
            .children(chip.map(|chip| div().px(px(8.)).child(chip)))
            .child(div().h(px(4.)))
            .child(tree)
            .into_any_element()
    }

    fn nav_items(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let colors = cx.theme().colors().clone();
        let Some(task) = self.task() else {
            return Vec::new();
        };
        let cursor = self.cursor_row();
        let ring = self.nav_focus.is_focused(window) && window.last_input_was_keyboard();
        let tree = Rc::clone(&self.tree);
        let mut elements = Vec::new();

        for position in range {
            let Some(entry) = tree.get(position) else {
                continue;
            };
            let selected = (cursor == Some(position), ring);
            let row = self.nav_row(position, entry, selected, task, &colors, cx);

            elements
                .push(row.map_or_else(|| div().into_any_element(), IntoElement::into_any_element));
        }

        elements
    }

    fn nav_row(
        &self,
        position: usize,
        entry: &Entry,
        selected: (bool, bool),
        task: TaskId,
        colors: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> Option<TreeRow> {
        match &entry.kind {
            EntryKind::Folder { open } => {
                Some(Self::nav_folder(position, entry, *open, selected, cx))
            }
            EntryKind::File { file } => {
                self.nav_file(position, entry, *file, selected, task, colors, cx)
            }
        }
    }

    fn nav_folder(
        position: usize,
        entry: &Entry,
        open: bool,
        (selected, ring): (bool, bool),
        cx: &mut Context<Self>,
    ) -> TreeRow {
        TreeRow::new(("review-nav-row", position), entry.name.clone())
            .debug_selector(|| format!("review-nav-folder-{position}"))
            .level(entry.level)
            .folder(open)
            .toggle_state(selected)
            .ring(selected && ring)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.nav_focus.focus(window, cx);
                this.set_folder_open(position, !open, cx);
            }))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one row needs its place, its data and the theme"
    )]
    fn nav_file(
        &self,
        position: usize,
        entry: &Entry,
        file: usize,
        (selected, ring): (bool, bool),
        task: TaskId,
        colors: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> Option<TreeRow> {
        let (diff, info) = (self.diff()?, self.infos.get(file)?);
        let data = diff.files.get(file)?;
        let viewed = self.is_viewed(task, file);
        let (warn, warn_bg) = (colors.warn, colors.warn_bg);

        let open = self.open_counts.get(file).copied().unwrap_or(0);
        let label = format!(
            "{}, {}, {}{}{}{}",
            info.path,
            plural(data.additions, "addition", "additions"),
            plural(data.deletions, "deletion", "deletions"),
            entry
                .agent
                .as_ref()
                .map_or_else(String::new, |agent| format!(", {agent}")),
            if open > 0 {
                format!(", {}", plural(open, "open comment", "open comments"))
            } else {
                String::new()
            },
            if viewed { ", viewed" } else { "" }
        );

        let end = h_flex()
            .flex_none()
            .gap(px(8.))
            .children(entry.agent.clone().map(|agent| {
                Label::new(agent)
                    .size(LabelSize::XSmall)
                    .color(Color::Muted)
                    .truncate()
            }))
            .child(counts(data.additions, data.deletions, LabelSize::XSmall))
            .when(open > 0, |row| {
                row.child(
                    div()
                        .id(("review-nav-open", position))
                        .debug_selector(|| format!("review-nav-open-{file}"))
                        .flex_none()
                        .px(px(5.))
                        .rounded(px(8.))
                        .bg(warn_bg)
                        .child(
                            Label::new(open.to_string())
                                .mono()
                                .size(LabelSize::XSmall)
                                .weight(FontWeight::MEDIUM)
                                .color(Color::Custom(warn)),
                        ),
                )
            })
            .child(div().w(px(12.)).flex_none().when(viewed, |slot| {
                slot.child(
                    Icon::new(IconName::Check)
                        .size(IconSize::Small)
                        .color(Color::Ok),
                )
            }));

        Some(
            TreeRow::new(("review-nav-row", position), info.name.clone())
                .debug_selector(|| format!("review-nav-file-{file}"))
                .accessible_label(label)
                .level(entry.level)
                .label_color(if viewed { Color::Muted } else { Color::Default })
                .toggle_state(selected)
                .ring(selected && ring)
                .end_slot(end)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.nav_focus.focus(window, cx);
                    this.select_file(file, cx);
                })),
        )
    }

    fn header_bar(
        &self,
        input: &ReviewInput,
        colors: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pill = if input.working == 0 {
            StatusPill::new(AgentStatus::Done)
        } else {
            StatusPill::new(AgentStatus::Blocked)
        }
        .label(pill_label(input.working))
        .truncate();

        let (place, counts) = Self::summary(input);

        let workspace = self.workspace.clone();
        let source = SegmentedControl::new("review-source", "Source", ["Live", "Merged"])
            .selected(
                SOURCES
                    .iter()
                    .position(|source| *source == input.source)
                    .unwrap_or(0),
            )
            .debug_selector(|| "review-source".to_owned())
            .on_select(move |index, _, cx| {
                let Some(source) = SOURCES.get(index).copied() else {
                    return;
                };

                update_workspace(&workspace, cx, |workspace, cx| {
                    workspace.set_review_source(source, cx);
                });
            });

        let view = cx.entity();
        let style = SegmentedControl::new("review-style", "Diff style", ["Unified", "Split"])
            .selected(
                STYLES
                    .iter()
                    .position(|style| *style == self.style)
                    .unwrap_or(0),
            )
            .debug_selector(|| "review-style".to_owned())
            .on_select(move |index, _, cx| {
                let Some(style) = STYLES.get(index).copied() else {
                    return;
                };

                view.update(cx, |this, cx| this.set_style(style, cx));
            });

        let loaded = input.diff.as_ref().map(|diff| diff.fingerprint);
        let merged = input.source == ReviewSource::Merged;
        let pending = count_comments(&input.comments).pending;
        let approved = merged && loaded.is_some() && input.approved == loaded;
        let enabled = merged && loaded.is_some() && !approved && !input.approving && pending == 0;

        let request_workspace = self.workspace.clone();
        let request = merged.then(|| {
            let label = if pending == 0 {
                "Request changes".to_owned()
            } else {
                format!("Request changes \u{b7} {pending}")
            };

            Button::new("review-request-changes", label)
                .variant(ButtonVariant::Secondary)
                .debug_selector(|| "review-request-changes".to_owned())
                .disabled(pending == 0 || input.requesting)
                .on_click(move |_, _, cx| {
                    update_workspace(&request_workspace, cx, Workspace::request_review_changes);
                })
        });

        let approve = next_step_button(approved, enabled, input.offer, self.workspace.clone());

        let error = input.approve_error.clone().map(|message| {
            div()
                .id("review-approve-error")
                .role(Role::Alert)
                .aria_label(message.clone())
                .max_w(px(260.))
                .min_w_0()
                .child(
                    Label::new(message)
                        .size(LabelSize::Small)
                        .color(Color::Del)
                        .truncate(),
                )
        });

        h_flex()
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .gap(px(12.))
            .pl(px(16.))
            .pr(px(12.))
            .border_b_1()
            .border_color(colors.line)
            .child(pill)
            .child(place)
            .children(counts)
            .child(div().flex_1())
            .child(source)
            .child(style)
            .children(error)
            .children(request)
            .child(approve)
            .into_any_element()
    }

    /// The place the diff is from, which shrinks with an ellipsis, and the totals,
    /// which never do. The place pulls the totals 8 px closer than the header gap.
    fn summary(input: &ReviewInput) -> (Stateful<Div>, Option<Stateful<Div>>) {
        let place = match input.source {
            ReviewSource::Merged => format!("{} \u{2190} {}", input.base, input.branch),
            ReviewSource::Live => input.diff.as_ref().map_or_else(String::new, |diff| {
                plural(
                    u32::try_from(worktree_count(diff)).unwrap_or(u32::MAX),
                    "worktree",
                    "worktrees",
                )
            }),
        };
        let place = if input.diff.is_some() && !place.is_empty() {
            format!("{place} \u{b7}")
        } else {
            place
        };

        let place = div()
            .id("review-place")
            .debug_selector(|| "review-place".to_owned())
            .flex_shrink(PLACE_SHRINK)
            .min_w_0()
            .mr(px(-8.))
            .overflow_hidden()
            .child(
                Label::new(place)
                    .mono()
                    .size(LabelSize::XSmall)
                    .color(Color::Faint)
                    .truncate(),
            );
        let counts = input.diff.as_ref().map(|diff| {
            counts(diff.stat.additions, diff.stat.deletions, LabelSize::XSmall)
                .id("review-counts")
                .debug_selector(|| "review-counts".to_owned())
        });

        (place, counts)
    }

    fn body_area(
        &self,
        input: &ReviewInput,
        colors: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let area = div().flex_1().min_h_0().bg(colors.term);

        let message = match (&input.diff, self.shown.is_empty()) {
            (None, _) => Some("Loading changes"),
            (Some(_), true) => Some("No changes"),
            (Some(_), false) => None,
        };

        if let Some(message) = message {
            return area.child(centered(message)).into_any_element();
        }

        let rows = list(
            self.body_list.clone(),
            cx.processor(|this, index: usize, _, cx| this.body_row(index, cx)),
        )
        .size_full();

        area.child(
            div()
                .id("review-body")
                .role(Role::Group)
                .aria_label("Diff")
                .key_context(REVIEW_BODY_CONTEXT)
                .track_focus(&self.body_focus)
                .on_action(cx.listener(|this, _: &PreviousLine, _, cx| this.move_cursor(-1, cx)))
                .on_action(cx.listener(|this, _: &NextLine, _, cx| this.move_cursor(1, cx)))
                .size_full()
                .child(rows),
        )
        .into_any_element()
    }

    fn body_row(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let body = Rc::clone(&self.body);
        let Some(row) = body.rows.get(index).copied() else {
            return div().into_any_element();
        };
        let colors = cx.theme().colors().clone();

        if row == Row::End {
            return div().h(px(CARD_GAP)).into_any_element();
        }

        let first = row.starts_card();
        let last = body.rows.get(index + 1).is_none_or(|next| next.ends_card());
        let Some(diff) = self.diff().cloned() else {
            return div().into_any_element();
        };

        let cursor = self.cursor.as_ref().map(|cursor| cursor.pos);
        let view = cx.entity();
        let line_ctx = |file: u32, hunk: u32, line: u32| LineCtx {
            view: view.clone(),
            file,
            hunk,
            line,
            cursor: cursor
                .filter(|at| at.file == file && at.hunk == hunk && at.line == line)
                .map(|at| at.side),
        };

        let content = match row {
            Row::Header { file } => self.card_header(file as usize, &diff, !last, &colors, cx),
            Row::Hunk { file, hunk } => hunk_row(index, &diff, file, hunk, &colors),
            Row::Line {
                file,
                hunk,
                line,
                old,
                new,
            } => {
                let data = hunk_line(&diff, file, hunk, line);
                unified_line(
                    index,
                    data,
                    (old, new),
                    &colors,
                    &line_ctx(file, hunk, line),
                )
            }
            Row::Pair {
                file,
                hunk,
                left,
                right,
            } => {
                let ctx = line_ctx(file, hunk, left.or(right).map_or(0, |side| side.line));
                let right_ctx = line_ctx(file, hunk, right.or(left).map_or(0, |side| side.line));

                split_line(
                    index,
                    &diff,
                    (file, hunk),
                    ((left, &ctx), (right, &right_ctx)),
                    &colors,
                )
                .into_any_element()
            }
            Row::Note { note, .. } => note_row(index, note, &colors),
            Row::Thread { thread, .. } => self.thread_row(thread as usize, index, &colors, cx),
            Row::Composer { .. } => self.composer_row(&colors, cx),
            Row::End => div().into_any_element(),
        };

        card_slot(first, last, content, &colors)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one flat element tree reads best in one place"
    )]
    fn card_header(
        &self,
        file: usize,
        diff: &TaskDiff,
        open: bool,
        colors: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (Some(task), Some(info), Some(data), Some(input)) = (
            self.task(),
            self.infos.get(file),
            diff.files.get(file),
            self.input.as_ref(),
        ) else {
            return div().into_any_element();
        };
        let viewed = self.is_viewed(task, file);
        let agent = self.agent_of(data);
        let view = cx.entity();
        let toggle_view = view.clone();
        let folded = is_folded(data, &input.loads);
        let load = input.loads.get(&info.key);
        let fold = data.fold.map(fold_label);
        let comments = self.open_counts.get(file).copied().unwrap_or(0);

        let summary = format!(
            "{}, {}, {}{}",
            info.path,
            plural(data.additions, "addition", "additions"),
            plural(data.deletions, "deletion", "deletions"),
            fold.map_or_else(String::new, |fold| format!(", {fold}"))
        );

        let tag = match data.change {
            FileChange::Added => Some(("Added", Color::Ok, colors.ok_bg)),
            FileChange::Deleted => Some(("Deleted", Color::Del, colors.del_bg)),
            FileChange::Modified => None,
        };

        let mode = data
            .mode_change
            .map(|(old, new)| format!("{old:o} \u{2192} {new:o}"));

        let loading = matches!(load, Some(FileLoad::Loading));
        let error = match load {
            Some(FileLoad::Failed(message)) => Some(message.clone()),
            _ => None,
        };
        let show = folded.then(|| {
            let workspace = self.workspace.clone();
            let (worktree, path) = (data.worktree.clone(), data.path.clone());

            Button::new(
                ("review-show-diff", file),
                if loading { "Loading" } else { "Show diff" },
            )
            .accessible_label(if loading {
                format!("Loading, {}", info.name)
            } else {
                format!("Show diff, {}", info.name)
            })
            .debug_selector(|| format!("review-show-diff-{file}"))
            .disabled(loading)
            .on_click(move |_, _, cx| {
                let (worktree, path) = (worktree.clone(), path.clone());

                update_workspace(&workspace, cx, |workspace, cx| {
                    workspace.show_file_diff(worktree, path, cx);
                });
            })
        });

        h_flex()
            .id(("review-file", file))
            .role(Role::Group)
            .aria_label(summary)
            .h(px(CARD_HEADER))
            .gap(px(10.))
            .px(px(12.))
            .when(open, |header| header.border_b_1().border_color(colors.line))
            .child(if folded {
                div().w(px(24.)).flex_none().into_any_element()
            } else {
                IconButton::new(
                    ("review-toggle", file),
                    if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    },
                    format!(
                        "{}, {}",
                        if open { "Collapse" } else { "Expand" },
                        info.name
                    ),
                )
                .color(Color::Muted)
                .on_click(move |_, _, cx| {
                    view.update(cx, |this, cx| this.toggle_collapsed(file, cx));
                })
                .into_any_element()
            })
            .child(
                h_flex()
                    .min_w_0()
                    .child(
                        Label::new(info.folder.clone())
                            .mono()
                            .size(LabelSize::Small)
                            .color(Color::Faint)
                            .truncate(),
                    )
                    .child(
                        div()
                            .flex_none()
                            .child(Label::new(info.name.clone()).mono().size(LabelSize::Small)),
                    ),
            )
            .child(counts(data.additions, data.deletions, LabelSize::XSmall))
            .children(fold.map(|fold| {
                div()
                    .id(("review-fold", file))
                    .debug_selector(|| format!("review-fold-{file}"))
                    .child(Tag::new(fold))
            }))
            .children(tag.map(|(text, color, background)| {
                div()
                    .flex_none()
                    .px(px(6.))
                    .py(px(1.))
                    .rounded(px(5.))
                    .bg(background)
                    .child(
                        Label::new(text)
                            .size(LabelSize::XSmall)
                            .weight(FontWeight::MEDIUM)
                            .color(color),
                    )
            }))
            .children(mode.map(|mode| {
                div()
                    .id(("review-mode", file))
                    .debug_selector(|| format!("review-mode-{file}"))
                    .role(Role::Label)
                    .aria_label(format!("File mode {mode}"))
                    .flex_none()
                    .px(px(6.))
                    .py(px(1.))
                    .rounded(px(5.))
                    .bg(colors.soft)
                    .child(
                        Label::new(mode)
                            .mono()
                            .size(LabelSize::XSmall)
                            .color(Color::Faint),
                    )
            }))
            .when(folded && comments > 0, |header| {
                header.child(
                    div()
                        .id(("review-folded-comments", file))
                        .debug_selector(|| format!("review-folded-comments-{file}"))
                        .child(
                            Tag::new(plural(comments, "comment", "comments")).tone(TagTone::Warn),
                        ),
                )
            })
            .child(div().flex_1())
            .children(error.map(|message| {
                div()
                    .id(("review-load-error", file))
                    .debug_selector(|| format!("review-load-error-{file}"))
                    .role(Role::Alert)
                    .aria_label(message.clone())
                    .max_w(px(260.))
                    .min_w_0()
                    .child(
                        Label::new(message)
                            .size(LabelSize::Small)
                            .color(Color::Del)
                            .truncate(),
                    )
            }))
            .children(show)
            .children(agent.map(|agent| {
                h_flex()
                    .flex_none()
                    .gap(px(6.))
                    .child(Icon::new(kind_icon(agent.kind)).size(IconSize::Medium))
                    .child(
                        Label::new(agent.name.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
            }))
            .child(
                Checkbox::new(("review-viewed", file), format!("Viewed, {}", info.name))
                    .display_label("Viewed")
                    .toggle_state(viewed)
                    .on_toggle(move |_, _, cx| {
                        toggle_view.update(cx, |this, cx| this.toggle_viewed(file, cx));
                    }),
            )
            .into_any_element()
    }

    fn agent_of(&self, file: &FileDiff) -> Option<AgentLabel> {
        let id = file.agent?;
        let input = self.input.as_ref()?;

        input.agents.iter().find(|agent| agent.id == id).cloned()
    }
}

/// One row of a card: the card's border, and its rounded ends on the first and last row.
fn card_slot(first: bool, last: bool, content: AnyElement, colors: &ThemeColors) -> AnyElement {
    let card = v_flex()
        .bg(colors.panel)
        .border_color(colors.line)
        .border_l_1()
        .border_r_1()
        .when(first, |card| {
            card.border_t_1().rounded_t(px(9.)).overflow_hidden()
        })
        .when(last, |card| {
            card.border_b_1().rounded_b(px(9.)).overflow_hidden()
        })
        .child(content);

    div()
        .px(px(CARD_GAP))
        .when(first, |slot| slot.pt(px(CARD_GAP)))
        .child(card)
        .into_any_element()
}

fn code_text() -> Div {
    div()
        .font_family(MONO_FONT)
        .text_size(px(12.))
        .line_height(px(LINE_HEIGHT))
}

fn hunk_line(diff: &TaskDiff, file: u32, hunk: u32, line: u32) -> Option<&DiffLine> {
    diff.files
        .get(file as usize)?
        .hunks
        .get(hunk as usize)?
        .lines
        .get(line as usize)
}

fn hunk_row(
    index: usize,
    diff: &TaskDiff,
    file: u32,
    hunk: u32,
    colors: &ThemeColors,
) -> AnyElement {
    let Some(data) = diff
        .files
        .get(file as usize)
        .and_then(|file| file.hunks.get(hunk as usize))
    else {
        return div().into_any_element();
    };
    let text = format!(
        "@@ -{},{} +{},{} @@ {}",
        data.old_start, data.old_lines, data.new_start, data.new_lines, data.section
    );

    code_text()
        .id(("review-hunk", index))
        .px(px(12.))
        .bg(colors.soft)
        .text_color(colors.faint)
        .whitespace_nowrap()
        .overflow_hidden()
        .child(text)
        .into_any_element()
}

fn note_row(index: usize, note: Note, colors: &ThemeColors) -> AnyElement {
    let text = match note {
        Note::Binary => "Binary file".to_owned(),
        Note::Empty => "Empty file".to_owned(),
        Note::TooLarge => "Too large to show".to_owned(),
        Note::Truncated(0) => "More lines not shown".to_owned(),
        Note::Truncated(count) => format!("{} lines not shown", thousands(count)),
    };

    h_flex()
        .id(("review-note", index))
        .h(px(30.))
        .px(px(12.))
        .text_color(colors.faint)
        .child(Label::new(text).size(LabelSize::Small).color(Color::Faint))
        .into_any_element()
}

/// Four spaces for a tab, so the text column lines up.
fn display_text(text: &str) -> SharedString {
    if text.contains('\t') {
        text.replace('\t', "    ").into()
    } else {
        text.to_owned().into()
    }
}

fn line_text(line: &DiffLine) -> SharedString {
    if line.kind == DiffLineKind::NoNewline {
        NO_NEWLINE.into()
    } else {
        display_text(&line.text)
    }
}

/// The text color, the tint and the sign of a line kind.
fn line_style(
    kind: DiffLineKind,
    colors: &ThemeColors,
) -> (gpui::Hsla, Option<gpui::Hsla>, &'static str) {
    match kind {
        DiffLineKind::Context => (colors.fg, None, " "),
        DiffLineKind::Added => (colors.add, Some(colors.add_bg), "+"),
        DiffLineKind::Deleted => (colors.del, Some(colors.del_bg), "\u{2212}"),
        DiffLineKind::NoNewline => (colors.faint, None, ""),
    }
}

fn number_cell(number: u32, right_padding: f32, colors: &ThemeColors) -> Div {
    div()
        .w(px(NUMBER_WIDTH))
        .flex_none()
        .text_right()
        .pr(px(right_padding))
        .text_color(colors.faint)
        .child(if number == 0 {
            SharedString::default()
        } else {
            number.to_string().into()
        })
}

/// What a screen reader says for a line: "Line 85, added, let cell = …".
fn line_label(kind: DiffLineKind, number: u32, text: &str) -> String {
    let word = match kind {
        DiffLineKind::Context => "unchanged",
        DiffLineKind::Added => "added",
        DiffLineKind::Deleted => "deleted",
        DiffLineKind::NoNewline => return NO_NEWLINE.to_owned(),
    };

    format!("Line {number}, {word}, {text}")
}

/// What a line row needs to take part in comments.
struct LineCtx {
    view: Entity<ReviewView>,
    file: u32,
    hunk: u32,
    line: u32,
    /// The side the line cursor sits on, when it sits on this line.
    cursor: Option<DiffSide>,
}

impl LineCtx {
    fn pos(&self, side: DiffSide) -> LinePos {
        LinePos {
            file: self.file,
            hunk: self.hunk,
            line: self.line,
            side,
        }
    }
}

/// Paints the line cursor: a ring inside the row.
fn cursor_ring<E: Styled>(mut element: E, colors: &ThemeColors) -> E {
    element
        .style()
        .refine(&focus_ring(StyleRefinement::default(), colors.accent));

    element
}

/// A line number that moves the line cursor to its line when clicked.
fn pick_cell(
    id: (&'static str, usize),
    number: u32,
    right_padding: f32,
    colors: &ThemeColors,
    ctx: &LineCtx,
    side: DiffSide,
) -> Stateful<Div> {
    let view = ctx.view.clone();
    let pos = ctx.pos(side);

    number_cell(number, right_padding, colors)
        .id(id)
        .debug_selector(move || format!("{}-{}", id.0, id.1))
        .role(Role::Button)
        .aria_label(format!("Select line {number}"))
        .cursor_pointer()
        .on_click(move |_, window, cx| {
            let handle = view.read(cx).body_focus.clone();

            window.focus(&handle, cx);
            view.update(cx, |this, cx| this.set_cursor(pos, cx));
        })
}

/// The **+** in a line's gutter: it shows while the row is hovered.
fn add_button(
    id: (&'static str, usize),
    ctx: &LineCtx,
    side: DiffSide,
    number: u32,
    group: &SharedString,
) -> AnyElement {
    let view = ctx.view.clone();
    let pos = ctx.pos(side);

    div()
        .absolute()
        .left(px(2.))
        .top(px(-1.5))
        .child(
            IconButton::new(id, IconName::Plus, format!("Comment on line {number}"))
                .debug_selector(move || format!("{}-{}", id.0, id.1))
                .reveal_with_group(group.clone())
                .skip_tab_stop()
                .on_click(move |_, _, cx| {
                    view.update(cx, |this, cx| this.open_composer(pos, cx));
                }),
        )
        .into_any_element()
}

fn unified_line(
    index: usize,
    line: Option<&DiffLine>,
    numbers: (u32, u32),
    colors: &ThemeColors,
    ctx: &LineCtx,
) -> AnyElement {
    match unified_row(index, line, numbers, colors, ctx) {
        Some(row) if ctx.cursor.is_some() => cursor_ring(row, colors).into_any_element(),
        Some(row) => row.into_any_element(),
        None => div().into_any_element(),
    }
}

/// A unified line without the cursor ring. The cursor line is the selected one, and the
/// body's active descendant, so a screen reader follows the cursor.
fn unified_row(
    index: usize,
    line: Option<&DiffLine>,
    (old, new): (u32, u32),
    colors: &ThemeColors,
    ctx: &LineCtx,
) -> Option<Stateful<Div>> {
    let line = line?;
    let (text_color, tint, sign) = line_style(line.kind, colors);
    let number = if line.kind == DiffLineKind::Deleted {
        old
    } else {
        new
    };
    let commentable = line.kind != DiffLineKind::NoNewline;
    let group: SharedString = format!("review-line-{index}").into();
    let side = if line.kind == DiffLineKind::Deleted {
        DiffSide::Old
    } else {
        DiffSide::New
    };

    let old_cell = if commentable && old > 0 {
        pick_cell(("review-old", index), old, 6., colors, ctx, DiffSide::Old).into_any_element()
    } else {
        number_cell(old, 6., colors).into_any_element()
    };
    let new_cell = if commentable && new > 0 {
        pick_cell(("review-new", index), new, 10., colors, ctx, DiffSide::New).into_any_element()
    } else {
        number_cell(new, 10., colors).into_any_element()
    };

    let row = code_text()
        .id(("review-line", index))
        .role(Role::ListItem)
        .aria_label(line_label(line.kind, number, &line.text))
        .aria_selected(ctx.cursor.is_some())
        .when(
            ctx.cursor.is_some(),
            StatefulInteractiveElement::aria_active_descendant,
        )
        .group(group.clone())
        .relative()
        .flex()
        .items_start()
        .w_full()
        .min_h(px(LINE_HEIGHT))
        .when_some(tint, Styled::bg)
        .child(old_cell)
        .child(new_cell)
        .child(
            div()
                .w(px(SIGN_WIDTH))
                .flex_none()
                .text_color(text_color)
                .child(sign),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .whitespace_normal()
                .text_color(text_color)
                .child(line_text(line)),
        )
        .when(commentable, |row| {
            row.child(add_button(("review-add", index), ctx, side, number, &group))
        });

    Some(row)
}

type Half<'a> = (Option<Side>, &'a LineCtx);

/// What a screen reader says for a split row, and whether the cursor sits in it. The
/// cursor half is named like a unified line, else both halves are read.
fn pair_label(
    diff: &TaskDiff,
    (file, hunk): (u32, u32),
    (left, right): (Half<'_>, Half<'_>),
) -> (String, bool) {
    let describe = |side: Option<Side>, word: &str| {
        side.and_then(|side| {
            hunk_line(diff, file, hunk, side.line).map(|line| {
                if line.kind == DiffLineKind::NoNewline {
                    format!("{word}: {NO_NEWLINE}")
                } else {
                    format!("{word} {}: {}", side.number, line.text)
                }
            })
        })
    };

    let cursor_half = if left.0.is_some() && left.1.cursor == Some(DiffSide::Old) {
        left.0
    } else if right.0.is_some() && right.1.cursor == Some(DiffSide::New) {
        right.0
    } else {
        None
    };
    let cursor_label = cursor_half.and_then(|side| {
        hunk_line(diff, file, hunk, side.line)
            .map(|line| line_label(line.kind, side.number, &line.text))
    });

    match cursor_label {
        Some(label) => (label, true),
        None => (
            [describe(left.0, "Old line"), describe(right.0, "New line")]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("; "),
            false,
        ),
    }
}

fn split_line(
    index: usize,
    diff: &TaskDiff,
    (file, hunk): (u32, u32),
    (left, right): (Half<'_>, Half<'_>),
    colors: &ThemeColors,
) -> Stateful<Div> {
    let half = |(side, ctx): Half<'_>, position: usize| {
        let line = side.and_then(|side| hunk_line(diff, file, hunk, side.line));
        let number = side.map_or(0, |side| side.number);
        let diff_side = if position == 0 {
            DiffSide::Old
        } else {
            DiffSide::New
        };

        match line {
            Some(line) => {
                let (text_color, tint, sign) = line_style(line.kind, colors);
                let commentable = line.kind != DiffLineKind::NoNewline;
                let group: SharedString = format!("review-half-{index}-{position}").into();
                let cell = if commentable {
                    pick_cell(
                        ("review-pick", index * 2 + position),
                        number,
                        6.,
                        colors,
                        ctx,
                        diff_side,
                    )
                    .into_any_element()
                } else {
                    number_cell(number, 6., colors).into_any_element()
                };

                let half = div()
                    .group(group.clone())
                    .relative()
                    .flex()
                    .items_start()
                    .flex_1()
                    .min_w_0()
                    .min_h(px(LINE_HEIGHT))
                    .when(position == 0, |half| {
                        half.border_r_1().border_color(colors.line)
                    })
                    .when_some(tint, Styled::bg)
                    .child(cell)
                    .child(
                        div()
                            .w(px(SIGN_WIDTH))
                            .flex_none()
                            .text_color(text_color)
                            .child(sign),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_normal()
                            .text_color(text_color)
                            .child(line_text(line)),
                    )
                    .when(commentable, |half| {
                        half.child(add_button(
                            (
                                if position == 0 {
                                    "review-add-old"
                                } else {
                                    "review-add-new"
                                },
                                index,
                            ),
                            ctx,
                            diff_side,
                            number,
                            &group,
                        ))
                    });

                if ctx.cursor == Some(diff_side) {
                    cursor_ring(half, colors).into_any_element()
                } else {
                    half.into_any_element()
                }
            }
            None => div()
                .flex_1()
                .min_w_0()
                .min_h(px(LINE_HEIGHT))
                .bg(colors.raised)
                .when(position == 0, |half| {
                    half.border_r_1().border_color(colors.line)
                })
                .into_any_element(),
        }
    };

    let (label, selected) = pair_label(diff, (file, hunk), (left, right));

    code_text()
        .id(("review-pair", index))
        .role(Role::ListItem)
        .aria_label(label)
        .aria_selected(selected)
        .when(selected, StatefulInteractiveElement::aria_active_descendant)
        .flex()
        .w_full()
        .child(half(left, 0))
        .child(half(right, 1))
}

impl Workspace {
    /// The source Review shows for a tab: the user's choice, else Live while a worker
    /// works and Merged when none does.
    pub(crate) fn review_source(&self, task: TaskId) -> ReviewSource {
        self.review_sources.get(&task).copied().unwrap_or_else(|| {
            if self.data.working_workers(task) > 0 {
                ReviewSource::Live
            } else {
                ReviewSource::Merged
            }
        })
    }

    pub(crate) fn set_review_source(&mut self, source: ReviewSource, cx: &mut Context<Self>) {
        self.review_sources.insert(self.active_task, source);
        self.review_error = None;
        cx.notify();
    }

    /// Removes the agent filter that View diff set.
    pub(crate) fn clear_review_agent(&mut self, cx: &mut Context<Self>) {
        self.review_agent = None;
        cx.notify();
    }

    /// Gives the view what it shows now. It runs after every notify of the workspace and
    /// only changed input reaches the view.
    pub(crate) fn sync_review(&mut self, cx: &mut Context<Self>) {
        if self.view != View::Review {
            return;
        }

        let task = self.active_task;
        let Some(id) = self.data.diff_id(task) else {
            self.review.update(cx, ReviewView::clear);
            return;
        };

        let source = self.review_source(task);
        self.review_sources.entry(task).or_insert(source);

        let fingerprint = self
            .data
            .diffs
            .get(&(id, source))
            .map(|diff| diff.fingerprint);
        self.list_comments_if_needed((id, source, fingerprint), cx);

        let Some(entry) = self.data.task(task) else {
            self.review.update(cx, ReviewView::clear);
            return;
        };
        let agents: Rc<[AgentLabel]> = self
            .data
            .agents_of(task)
            .map(|agent| AgentLabel {
                id: agent.protocol_id(),
                name: agent.name.clone(),
                kind: agent.kind,
            })
            .collect();
        let filter = self
            .review_agent
            .and_then(|agent| self.data.agent(agent))
            .map(|agent| AgentLabel {
                id: agent.protocol_id(),
                name: agent.name.clone(),
                kind: agent.kind,
            });
        let base = entry.daemon.as_ref().map_or_else(
            || {
                self.data
                    .project(entry.project)
                    .map(|project| project.base_branch.clone())
                    .unwrap_or_default()
            },
            |daemon| daemon.base.clone(),
        );

        let input = ReviewInput {
            task,
            source,
            diff: self.data.diffs.get(&(id, source)).cloned(),
            filter,
            agents,
            working: self.data.working_workers(task),
            base,
            branch: entry.branch.clone(),
            approved: entry.approved,
            offer: self.review_offer(task),
            approving: self.review_approving.contains(&task),
            approve_error: self
                .review_error
                .as_ref()
                .filter(|(other, _)| *other == task)
                .map(|(_, message)| message.clone()),
            jump: self.review_file.clone().map(|path| Jump {
                serial: self.review_jump_serial,
                path,
                worktree: self.review_worktree.clone(),
            }),
            comments: self.data.task_comments(task),
            loads: Rc::new(self.data.loaded.loads(id, source)),
            requesting: self.review_requesting.contains(&task),
            fixed_now: self.fixed_now,
        };

        self.review.update(cx, |review, cx| review.show(input, cx));
    }

    /// Approves the Merged diff the view shows.
    pub(crate) fn approve_review(&mut self, cx: &mut Context<Self>) {
        let task = self.active_task;
        let Some(id) = self.data.diff_id(task) else {
            return;
        };
        let Some(diff) = self.data.diffs.get(&(id, ReviewSource::Merged)).cloned() else {
            return;
        };
        let approved = self.data.task(task).and_then(|entry| entry.approved);

        let pending = count_comments(&self.data.task_comments(task)).pending;

        if self.review_source(task) != ReviewSource::Merged
            || approved == Some(diff.fingerprint)
            || self.review_approving.contains(&task)
            || pending > 0
        {
            return;
        }

        self.review_error = None;
        let fingerprint = diff.fingerprint;

        let Some(client) = self.daemon.clone() else {
            if self.source == crate::workspace::DataSource::Sample {
                if let Some(entry) = self.data.tasks.iter_mut().find(|entry| entry.id == task) {
                    entry.approved = Some(fingerprint);
                }
            } else {
                self.review_error = Some((task, "Not connected".into()));
            }

            cx.notify();
            return;
        };

        self.review_approving.insert(task);
        let approving = cx.background_spawn(async move { client.approve_task(id, fingerprint) });

        cx.spawn(async move |this, cx| {
            let result = approving.await;

            let updated = this.update(cx, |workspace, cx| {
                workspace.review_approving.remove(&task);

                if let Err(error) = result {
                    log::warn!("cannot approve the task: {error}");
                    workspace.review_error = Some((task, refusal(&error)));
                }

                cx.notify();
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the approval answered: {error}");
            }
        })
        .detach();
    }

    /// Loads the lines of a folded file of the diff Review shows. The daemon sends them
    /// once; a new diff with the same digest keeps them, see [`FakeData::store_diff`].
    ///
    /// [`FakeData::store_diff`]: crate::model::FakeData::store_diff
    pub(crate) fn show_file_diff(
        &mut self,
        worktree: PathBuf,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let task = self.active_task;
        let Some(id) = self.data.diff_id(task) else {
            return;
        };
        let source = self.review_source(task);
        let folded = self.data.diffs.get(&(id, source)).is_some_and(|diff| {
            diff.files
                .iter()
                .any(|file| file.worktree == worktree && file.path == path && file.fold.is_some())
        });
        let key = (id, source, worktree, path);

        if folded && self.data.loaded.begin(&key) {
            self.request_file_lines(key, cx);
            cx.notify();
        }
    }

    /// Asks for the lines of a file, and keeps the answer when it comes. Marks the file as
    /// loading first, see [`LoadedFiles::begin`].
    ///
    /// [`LoadedFiles::begin`]: crate::review_model::LoadedFiles::begin
    pub(crate) fn request_file_lines(&mut self, key: LoadKey, cx: &mut Context<Self>) {
        let Some(client) = self.daemon.clone() else {
            let answer = if self.source == crate::workspace::DataSource::Sample {
                Ok(self
                    .data
                    .diffs
                    .get(&(key.0, key.1))
                    .and_then(|diff| {
                        diff.files
                            .iter()
                            .find(|file| file.worktree == key.2 && file.path == key.3)
                    })
                    .map(sample_lines))
            } else {
                Err("Not connected".into())
            };

            self.data.finish_load(&key, answer);

            return;
        };

        let (task, source, worktree, path) = key.clone();
        let asking =
            cx.background_spawn(async move { client.file_hunks(task, source, worktree, path) });

        cx.spawn(async move |this, cx| {
            let answer = asking.await;

            let updated = this.update(cx, |workspace, cx| {
                let answer = answer.map_err(|error| {
                    log::warn!("cannot load the lines of a file: {error}");

                    refusal(&error)
                });

                workspace.data.finish_load(&key, answer);
                cx.notify();
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the file arrived: {error}");
            }
        })
        .detach();
    }

    /// Loads the files the user opened again when their digest changed.
    pub(crate) fn reload_changed_files(
        &mut self,
        task: swarm_protocol::TaskId,
        source: ReviewSource,
        cx: &mut Context<Self>,
    ) {
        let stale = self
            .data
            .diffs
            .get(&(task, source))
            .map(|diff| self.data.loaded.stale(diff))
            .unwrap_or_default();

        for key in stale {
            if self.data.loaded.begin(&key) {
                self.request_file_lines(key, cx);
            }
        }
    }

    /// Opens Review at a file of the Live diff, as the Changes panel does.
    pub(crate) fn open_changed_file(
        &mut self,
        worktree: PathBuf,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        self.set_view(View::Review, cx);
        self.review_sources
            .insert(self.active_task, ReviewSource::Live);
        self.request_review_file(path, Some(worktree));
        cx.notify();
    }

    /// Asks Review to scroll to a file once it shows.
    pub(crate) fn request_review_file(&mut self, path: PathBuf, worktree: Option<PathBuf>) {
        self.review_file = Some(path);
        self.review_worktree = worktree;
        self.review_jump_serial += 1;
    }
}

/// The error text cut to one short line.
pub(crate) fn short(message: &str) -> SharedString {
    const LIMIT: usize = 80;

    let line = message.lines().next().unwrap_or_default();

    if line.chars().count() <= LIMIT {
        return line.to_owned().into();
    }

    let cut: String = line.chars().take(LIMIT).collect();

    format!("{cut}\u{2026}").into()
}

#[cfg(test)]
mod tests {
    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
    use swarm_protocol::{DiffHunk, DiffLine, Fold};

    use super::*;
    use crate::actions;
    use crate::model::AgentId;
    use crate::review_model::{
        ComposerAt, LinePos, Note, Place, Row, count_comments, first_changed_line, sample_diff,
    };
    use swarm_protocol::{CommentEvent, CommentId, ReviewComment};

    fn setup(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
        cx.update(|cx| {
            swarm_ui::init(cx);
            actions::bind_keys(cx);
        });

        let (workspace, cx) = cx.add_window_view(Workspace::with_sample);
        cx.run_until_parked();

        (workspace, cx)
    }

    fn redraw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
    }

    fn open_review(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Entity<ReviewView> {
        workspace.update(cx, |workspace, cx| workspace.set_view(View::Review, cx));
        redraw(cx);

        workspace.read_with(cx, |workspace, _| workspace.review.clone())
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let center = cx.debug_bounds(selector).map(|bounds| bounds.center());

        assert!(center.is_some(), "{selector} is not on screen");
        if let Some(center) = center {
            cx.simulate_click(center, Modifiers::none());
        }
        redraw(cx);
    }

    fn agent_named(workspace: &Entity<Workspace>, name: &str, cx: &VisualTestContext) -> AgentId {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .agents
                .iter()
                .find(|agent| agent.name == name)
                .map_or(AgentId(u32::MAX), |agent| agent.id)
        })
    }

    fn file_named(
        workspace: &Entity<Workspace>,
        source: ReviewSource,
        name: &str,
        cx: &VisualTestContext,
    ) -> usize {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .task_diff(TaskId(0), source)
                .and_then(|diff| diff.files.iter().position(|file| file.path.ends_with(name)))
                .unwrap_or(usize::MAX)
        })
    }

    /// `level name` of each row of the tree, folders with a slash.
    fn outline(review: &Entity<ReviewView>, cx: &VisualTestContext) -> Vec<String> {
        review.read_with(cx, |review, _| {
            review
                .tree_rows()
                .iter()
                .map(|entry| {
                    let slash = matches!(entry.kind, EntryKind::Folder { .. });

                    format!(
                        "{} {}{}",
                        entry.level,
                        entry.name,
                        if slash { "/" } else { "" }
                    )
                })
                .collect()
        })
    }

    fn focus_tree(review: &Entity<ReviewView>, cx: &mut VisualTestContext) {
        let handle = review.read_with(cx, |review, _| review.tree_focus().clone());

        cx.update(|window, cx| window.focus(&handle, cx));
        redraw(cx);
    }

    fn put_cursor(review: &Entity<ReviewView>, row: usize, cx: &mut VisualTestContext) {
        review.update(cx, |review, cx| review.put_cursor(row, cx));
        redraw(cx);
    }

    fn cursor_row(review: &Entity<ReviewView>, cx: &VisualTestContext) -> Option<usize> {
        review.read_with(cx, |review, _| review.tree_cursor_row())
    }

    fn row_named(review: &Entity<ReviewView>, name: &str, cx: &VisualTestContext) -> usize {
        review.read_with(cx, |review, _| {
            review
                .tree_rows()
                .iter()
                .position(|entry| entry.name == name)
                .unwrap_or(usize::MAX)
        })
    }

    fn node_of(
        row: Option<TreeRow>,
        cx: &mut VisualTestContext,
    ) -> (Option<Role>, gpui::accesskit::Node) {
        use gpui::{Element as _, RenderOnce as _};

        let Some(row) = row else {
            return (None, gpui::accesskit::Node::new(Role::Unknown));
        };

        cx.update(|window, cx| {
            let element = row.render(window, cx).into_element();
            let mut node = gpui::accesskit::Node::new(Role::Unknown);

            element.write_a11y_info(&mut node);

            (element.a11y_role(), node)
        })
    }

    #[gpui::test]
    fn the_file_list_is_a_folder_tree_without_a_grouping_control(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);

        let rows = outline(&review, cx);

        assert_eq!(rows[0], "0 benches/");
        assert_eq!(rows[1], "1 grid.rs");
        assert_eq!(rows[2], "0 crates/");
        assert!(rows.contains(&"2 src/".to_owned()), "{rows:?}");
        assert!(rows.contains(&"1 swarm-ui/src/".to_owned()), "{rows:?}");
        assert_eq!(rows.last().map(String::as_str), Some("0 Cargo.lock"));
        assert!(rows.iter().all(|row| !row.contains("Other changes")));
        let progress = cx.debug_bounds("review-progress");
        let tree = cx.debug_bounds("review-tree");

        let gap = progress
            .zip(tree)
            .map(|(progress, tree)| tree.top() - progress.bottom());
        assert!(
            gap.is_some_and(|gap| gap < px(16.)),
            "something sits between the progress bar and the tree: {gap:?}"
        );
    }

    #[gpui::test]
    fn tree_rows_have_roles_levels_and_expanded_and_selected_states(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        put_cursor(&review, 2, cx);

        let rows = review.update(cx, |review, cx| {
            let colors = cx.theme().colors().clone();
            let cursor = review.tree_cursor_row();
            let task = review.task().unwrap_or(TaskId(0));

            review
                .tree_rows()
                .to_vec()
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    let row = review.nav_row(
                        index,
                        entry,
                        (cursor == Some(index), false),
                        task,
                        &colors,
                        cx,
                    );

                    (entry.clone(), row)
                })
                .collect::<Vec<_>>()
        });

        assert!(rows.len() > 8);

        for (index, (entry, row)) in rows.into_iter().enumerate() {
            let (role, node) = node_of(row, cx);

            assert_eq!(role, Some(Role::TreeItem), "row {index}");
            assert_eq!(node.level(), Some(entry.level + 1), "row {index}");
            assert_eq!(node.is_selected(), Some(index == 2), "row {index}");
            match entry.kind {
                EntryKind::Folder { open } => {
                    assert_eq!(node.is_expanded(), Some(open), "row {index}");
                    assert_eq!(node.label(), Some(&*entry.name), "row {index}");
                }
                EntryKind::File { .. } => {
                    assert_eq!(node.is_expanded(), None, "row {index}");
                    let label = node.label().unwrap_or_default();
                    assert!(label.contains("addition"), "{label}");
                }
            }
        }
    }

    #[gpui::test]
    fn the_up_and_down_arrows_move_the_cursor_and_stop_at_the_ends(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        focus_tree(&review, cx);
        let last = review.read_with(cx, |review, _| review.tree_rows().len() - 1);
        put_cursor(&review, 0, cx);

        cx.simulate_keystrokes("down");
        let down = cursor_row(&review, cx);
        cx.simulate_keystrokes("up");
        let up = cursor_row(&review, cx);
        cx.simulate_keystrokes("up");
        let top = cursor_row(&review, cx);
        put_cursor(&review, last, cx);
        cx.simulate_keystrokes("down");
        let bottom = cursor_row(&review, cx);

        assert_eq!(
            (down, up, top, bottom),
            (Some(1), Some(0), Some(0), Some(last))
        );
        let selected = review.read_with(cx, |review, _| review.selected_file());
        assert_eq!(
            selected,
            Some(file_named(&workspace, ReviewSource::Live, "lib.rs", cx))
        );
    }

    #[gpui::test]
    fn the_left_and_right_arrows_close_open_and_step_through_folders(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        focus_tree(&review, cx);
        let rows = outline(&review, cx);
        put_cursor(&review, 0, cx);

        cx.simulate_keystrokes("left");
        let closed = outline(&review, cx);
        cx.simulate_keystrokes("right");
        let reopened = outline(&review, cx);
        cx.simulate_keystrokes("right");
        let first_child = cursor_row(&review, cx);
        cx.simulate_keystrokes("right");
        let on_a_file = cursor_row(&review, cx);
        cx.simulate_keystrokes("left");
        let parent = cursor_row(&review, cx);

        assert_eq!(rows[..2], ["0 benches/", "1 grid.rs"]);
        assert_eq!(closed[..2], ["0 benches/", "0 crates/"]);
        assert_eq!(reopened, rows);
        assert_eq!(
            (first_child, on_a_file, parent),
            (Some(1), Some(1), Some(0))
        );
    }

    #[gpui::test]
    fn enter_and_space_open_a_file_in_the_diff_and_toggle_a_folder(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        focus_tree(&review, cx);
        let glyphs = file_named(&workspace, ReviewSource::Live, "glyphs.rs", cx);
        let grid = file_named(&workspace, ReviewSource::Live, "benches/grid.rs", cx);

        put_cursor(&review, row_named(&review, "glyphs.rs", cx), cx);
        let before = review.read_with(cx, |review, _| review.selected_file());
        cx.simulate_keystrokes("enter");
        let by_enter = review.read_with(cx, |review, _| review.selected_file());

        put_cursor(&review, row_named(&review, "grid.rs", cx), cx);
        cx.simulate_keystrokes("space");
        let by_space = review.read_with(cx, |review, _| review.selected_file());

        put_cursor(&review, 0, cx);
        cx.simulate_keystrokes("enter");
        let folded = outline(&review, cx).len();
        cx.simulate_keystrokes("space");
        let unfolded = outline(&review, cx).len();

        assert_ne!(before, Some(glyphs));
        assert_eq!(by_enter, Some(glyphs));
        assert_eq!(by_space, Some(grid));
        assert!(folded < unfolded);
    }

    #[gpui::test]
    fn moving_the_cursor_does_not_open_a_file_and_clicking_does(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        focus_tree(&review, cx);
        let before = review.read_with(cx, |review, _| review.selected_file());

        cx.simulate_keystrokes("down down down");
        let after_arrows = review.read_with(cx, |review, _| review.selected_file());
        let glyphs = file_named(&workspace, ReviewSource::Live, "glyphs.rs", cx);
        click(
            Box::leak(format!("review-nav-file-{glyphs}").into_boxed_str()),
            cx,
        );
        let after_click = review.read_with(cx, |review, _| review.selected_file());

        assert_eq!(before, after_arrows);
        assert_eq!(after_click, Some(glyphs));
    }

    #[gpui::test]
    fn j_and_k_still_work_while_the_tree_has_focus(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        focus_tree(&review, cx);
        let first = review.read_with(cx, |review, _| review.selected_file());

        cx.simulate_keystrokes("j");
        let second = review.read_with(cx, |review, _| review.selected_file());
        cx.simulate_keystrokes("v");
        let viewed = review.read_with(cx, |review, _| review.viewed_count());
        cx.simulate_keystrokes("k");
        let back = review.read_with(cx, |review, _| review.selected_file());

        assert_ne!(first, second);
        assert_eq!(first, back);
        assert_eq!(viewed, 1);
    }

    #[gpui::test]
    fn the_tree_is_one_stop_for_tab(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        focus_tree(&review, cx);
        let handle = review.read_with(cx, |review, _| review.tree_focus().clone());
        let inside = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| handle.contains_focused(window, cx))
        };

        let start = inside(cx);
        cx.simulate_keystrokes("tab");
        let after_tab = inside(cx);
        cx.simulate_keystrokes("shift-tab");
        let after_back = (inside(cx), cx.update(|window, _| handle.is_focused(window)));

        assert!(start);
        assert!(!after_tab, "Tab stayed inside the tree");
        assert_eq!(after_back, (true, true));
    }

    #[gpui::test]
    fn closed_folders_stay_closed_across_a_diff_refresh(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        let open = outline(&review, cx);

        click("review-nav-folder-0", cx);
        let closed = outline(&review, cx);
        change_first_file(&workspace, cx);
        let refreshed = outline(&review, cx);

        assert_eq!(closed[0], "0 benches/");
        assert_eq!(closed.len(), open.len() - 1);
        assert_eq!(refreshed, closed, "a refresh opened the folder again");
    }

    #[gpui::test]
    fn the_same_path_in_two_worktrees_shows_its_agent_on_each_row(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, _| {
            let key = (swarm_protocol::TaskId(0), ReviewSource::Live);
            let Some(diff) = workspace.data.diffs.get_mut(&key) else {
                return;
            };
            let mut changed = (**diff).clone();
            let pty_io = changed
                .files
                .iter()
                .find(|file| file.path.ends_with("pty.rs"))
                .map(|file| (file.worktree.clone(), file.agent));
            let mut copy = changed.files[0].clone();

            if let Some((worktree, agent)) = pty_io {
                copy.worktree = worktree;
                copy.agent = agent;
                copy.digest += 1;
            }
            changed.files.push(copy);
            changed.fingerprint += 1;
            *diff = Rc::new(changed);
        });
        let review = open_review(&workspace, cx);

        let (rows, labels) = review.update(cx, |review, cx| {
            let colors = cx.theme().colors().clone();
            let task = review.task().unwrap_or(TaskId(0));
            let rows = review.tree_rows().to_vec();
            let labels: Vec<_> = rows
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.agent.is_some())
                .filter_map(|(index, entry)| {
                    review.nav_row(index, entry, (false, false), task, &colors, cx)
                })
                .collect();

            (rows, labels.len())
        });
        let repeated: Vec<_> = rows
            .iter()
            .filter(|entry| entry.name == "lib.rs")
            .map(|entry| entry.agent.as_ref().map(ToString::to_string))
            .collect();

        assert_eq!(repeated.len(), 2);
        assert!(repeated.iter().all(Option::is_some));
        assert_ne!(repeated[0], repeated[1]);
        assert_eq!(labels, 2);
        assert!(
            rows.iter()
                .filter(|entry| entry.name != "lib.rs")
                .all(|entry| entry.agent.is_none())
        );
    }

    #[gpui::test]
    fn split_pairs_rows_and_unified_lists_each_line(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);

        let unified = review.read_with(cx, |review, _| review.rows().to_vec());
        review.update(cx, |review, cx| review.set_style(DiffStyle::Split, cx));
        let split = review.read_with(cx, |review, _| review.rows().to_vec());

        assert!(unified.iter().any(|row| matches!(row, Row::Line { .. })));
        assert!(!unified.iter().any(|row| matches!(row, Row::Pair { .. })));
        assert!(split.iter().any(|row| matches!(row, Row::Pair { .. })));
        assert!(split.len() < unified.len());
        redraw(cx);
    }

    #[gpui::test]
    fn binary_and_truncated_files_show_their_notes(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, _| {
            let key = (swarm_protocol::TaskId(0), ReviewSource::Live);
            let mut diff = sample_diff(
                swarm_protocol::TaskId(0),
                ReviewSource::Live,
                [swarm_protocol::AgentId(1); 3],
            );
            diff.files[0].binary = true;
            diff.files[0].hunks.clear();
            diff.files[1].truncated = true;
            diff.files[1].additions += 1_000;
            workspace.data.diffs.insert(key, Rc::new(diff));
        });
        let review = open_review(&workspace, cx);

        let rows = review.read_with(cx, |review, _| review.rows().to_vec());

        assert!(rows.iter().any(|row| matches!(
            row,
            Row::Note {
                note: Note::Binary,
                ..
            }
        )));
        assert!(rows.iter().any(|row| matches!(
            row,
            Row::Note {
                note: Note::Truncated(1_000),
                ..
            }
        )));
    }

    #[gpui::test]
    fn the_source_defaults_to_live_while_workers_run_and_merged_after(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let running = workspace.read_with(cx, |workspace, _| workspace.review_source(TaskId(0)));
        workspace.update(cx, |workspace, _| {
            for agent in &mut workspace.data.agents {
                agent.status = AgentStatus::Done;
            }
        });
        let done = workspace.read_with(cx, |workspace, _| workspace.review_source(TaskId(0)));
        open_review(&workspace, cx);
        workspace.update(cx, |workspace, cx| {
            workspace.set_review_source(ReviewSource::Live, cx);
        });
        let chosen = workspace.read_with(cx, |workspace, _| workspace.review_source(TaskId(0)));

        assert_eq!(running, ReviewSource::Live);
        assert_eq!(done, ReviewSource::Merged);
        assert_eq!(chosen, ReviewSource::Live);
    }

    #[gpui::test]
    fn the_first_open_decides_the_source_and_it_sticks(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        open_review(&workspace, cx);
        workspace.update(cx, |workspace, _| {
            for agent in &mut workspace.data.agents {
                agent.status = AgentStatus::Done;
            }
        });
        workspace.update(cx, |_, cx| cx.notify());
        redraw(cx);

        let source = workspace.read_with(cx, |workspace, _| workspace.review_source(TaskId(0)));

        assert_eq!(source, ReviewSource::Live);
    }

    fn change_first_file(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
        workspace.update(cx, |workspace, cx| {
            let key = (swarm_protocol::TaskId(0), ReviewSource::Live);
            if let Some(diff) = workspace.data.diffs.get_mut(&key) {
                let mut changed = (**diff).clone();
                changed.fingerprint += 1;
                changed.files[0].digest += 1;
                changed.files[0].hunks = vec![DiffHunk {
                    old_start: 1,
                    old_lines: 1,
                    new_start: 1,
                    new_lines: 1,
                    section: String::new(),
                    lines: vec![DiffLine {
                        kind: DiffLineKind::Added,
                        text: "different".into(),
                    }],
                }];
                *diff = Rc::new(changed);
            }
            cx.notify();
        });
        redraw(cx);
    }

    #[gpui::test]
    fn viewing_collapses_the_card_and_a_changed_file_is_no_longer_viewed(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);

        let before = review.read_with(cx, |review, _| review.rows().len());
        review.update(cx, |review, cx| review.toggle_viewed(0, cx));
        let (viewed, collapsed, after) = review.read_with(cx, |review, _| {
            (
                review.is_file_viewed(0),
                review.is_collapsed(0),
                review.rows().len(),
            )
        });
        change_first_file(&workspace, cx);
        let (still, open) = review.read_with(cx, |review, _| {
            (review.is_file_viewed(0), review.is_collapsed(0))
        });

        assert!(viewed && collapsed && after < before);
        assert!(!still && !open);
    }

    #[gpui::test]
    fn the_chevron_toggles_a_card_without_marking_it_viewed(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);

        review.update(cx, |review, cx| review.toggle_collapsed(0, cx));
        let closed = review.read_with(cx, |review, _| {
            (review.is_collapsed(0), review.is_file_viewed(0))
        });
        review.update(cx, |review, cx| review.toggle_collapsed(0, cx));
        let open = review.read_with(cx, |review, _| review.is_collapsed(0));

        assert_eq!(closed, (true, false));
        assert!(!open);
    }

    #[gpui::test]
    fn j_k_and_v_move_and_mark_files(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        let first = review.read_with(cx, |review, _| review.selected_file());

        cx.simulate_keystrokes("j");
        let second = review.read_with(cx, |review, _| review.selected_file());
        cx.simulate_keystrokes("k");
        let back = review.read_with(cx, |review, _| review.selected_file());
        cx.simulate_keystrokes("v");
        let viewed = review.read_with(cx, |review, _| review.viewed_count());

        assert_ne!(first, second);
        assert_eq!(first, back);
        assert_eq!(viewed, 1);
    }

    #[gpui::test]
    fn view_diff_filters_to_one_agent_and_jumps_to_the_file(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let pty_io = agent_named(&workspace, "pty-io", cx);

        workspace.update(cx, |workspace, cx| {
            workspace.view_file_diff(pty_io, PathBuf::from("crates/swarm-term/src/pty.rs"), cx);
        });
        redraw(cx);

        let review = workspace.read_with(cx, |workspace, _| workspace.review.clone());
        let (shown, selected, rows) = review.read_with(cx, |review, _| {
            let selected = review.selected_file().and_then(|file| {
                review
                    .diff()
                    .and_then(|diff| diff.files.get(file))
                    .map(|file| file.path.clone())
            });

            (
                review.shown_files().len(),
                selected,
                review.tree_rows().len(),
            )
        });

        assert_eq!(shown, 3);
        assert_eq!(
            selected,
            Some(PathBuf::from("crates/swarm-term/src/pty.rs"))
        );
        assert_eq!(rows, 5, "Cargo.lock and one folder with two files");
        assert!(cx.debug_bounds("review-filter").is_some());

        click("review-filter", cx);
        let shown = review.read_with(cx, |review, _| review.shown_files().len());
        assert_eq!(shown, 11);
    }

    #[gpui::test]
    fn opening_review_through_the_view_switch_clears_the_filter(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let pty_io = agent_named(&workspace, "pty-io", cx);
        workspace.update(cx, |workspace, cx| workspace.view_agent_diff(pty_io, cx));
        workspace.update(cx, |workspace, cx| workspace.set_view(View::Review, cx));

        let filter = workspace.read_with(cx, |workspace, _| workspace.review_agent);

        assert_eq!(filter, None);
    }

    fn approve_state(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> Option<u64> {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .task(TaskId(0))
                .and_then(|task| task.approved)
        })
    }

    #[gpui::test]
    fn approve_is_disabled_in_live_and_works_in_merged(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, _| workspace.data.comments.clear());
        open_review(&workspace, cx);

        click("review-approve", cx);
        let in_live = approve_state(&workspace, cx);

        workspace.update(cx, |workspace, cx| {
            workspace.set_review_source(ReviewSource::Merged, cx);
        });
        redraw(cx);
        click("review-approve", cx);
        let in_merged = approve_state(&workspace, cx);
        let next = cx.debug_bounds("review-create-pr").is_some();

        assert_eq!(in_live, None);
        assert_eq!(in_merged, Some(0x22));
        assert!(next, "an approved task offers Create PR");
    }

    #[gpui::test]
    fn an_approval_of_an_older_diff_does_not_count(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, _| workspace.data.comments.clear());
        workspace.update(cx, |workspace, cx| {
            if let Some(task) = workspace.data.tasks.first_mut() {
                task.approved = Some(1);
            }
            workspace.set_review_source(ReviewSource::Merged, cx);
        });
        open_review(&workspace, cx);

        click("review-approve", cx);

        assert_eq!(approve_state(&workspace, cx), Some(0x22));
    }

    #[gpui::test]
    fn the_changes_panel_lists_the_live_diff_as_a_tree(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        redraw(cx);

        let list = workspace.read_with(cx, |workspace, _| workspace.changes_list.clone());
        let (rows, files) =
            list.read_with(cx, |list, _| (list.tree_rows().to_vec(), list.file_count()));
        let stat = workspace.read_with(cx, |workspace, _| workspace.data.live_stat(TaskId(0)));

        assert_eq!(files, 11);
        assert!(
            rows.iter()
                .any(|row| row.name == "swarm-ui/src" && row.level == 1)
        );
        assert!(rows.iter().any(|row| row.name == "src" && row.level == 2));
        assert_eq!(rows[0].level, 0);
        assert_eq!((stat.additions, stat.deletions), (2_623, 394));
    }

    fn changes_outline(
        list: &Entity<crate::panels::ChangesList>,
        cx: &VisualTestContext,
    ) -> Vec<String> {
        list.read_with(cx, |list, _| {
            list.tree_rows()
                .iter()
                .map(|entry| format!("{} {}", entry.level, entry.name))
                .collect()
        })
    }

    #[gpui::test]
    fn the_changes_panel_rows_are_tree_items_with_levels_and_expanded_states(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        redraw(cx);
        let list = workspace.read_with(cx, |workspace, _| workspace.changes_list.clone());
        list.update(cx, |list, cx| list.put_cursor_for_test(1, cx));

        let rows = list.update(cx, |list, cx| {
            (0..list.tree_rows().len())
                .map(|index| (list.tree_rows()[index].clone(), list.tree_row(index, cx)))
                .collect::<Vec<_>>()
        });

        assert!(rows.len() > 8);
        for (index, (entry, row)) in rows.into_iter().enumerate() {
            let (role, node) = node_of(row, cx);

            assert_eq!(role, Some(Role::TreeItem));
            assert_eq!(node.level(), Some(entry.level + 1));
            assert_eq!(node.is_selected(), Some(index == 1));
            assert_eq!(
                node.is_expanded(),
                match entry.kind {
                    EntryKind::Folder { open } => Some(open),
                    EntryKind::File { .. } => None,
                }
            );
        }
    }

    #[gpui::test]
    fn a_click_on_a_file_of_the_changes_panel_opens_review_at_it(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        redraw(cx);
        let theme = file_named(&workspace, ReviewSource::Live, "theme.rs", cx);

        click(
            Box::leak(format!("change-file-{theme}").into_boxed_str()),
            cx,
        );

        let view = workspace.read_with(cx, |workspace, _| workspace.view);
        let review = workspace.read_with(cx, |workspace, _| workspace.review.clone());
        let selected = review.read_with(cx, |review, _| review.selected_file());
        assert_eq!(view, View::Review);
        assert_eq!(selected, Some(theme));
    }

    #[gpui::test]
    fn the_changes_panel_tree_takes_arrow_keys_and_enter_opens_review(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        redraw(cx);
        let list = workspace.read_with(cx, |workspace, _| workspace.changes_list.clone());
        let handle = list.read_with(cx, |list, _| list.tree_focus().clone());
        cx.update(|window, cx| window.focus(&handle, cx));
        redraw(cx);
        let open = changes_outline(&list, cx);

        cx.simulate_keystrokes("down");
        let first = list.read_with(cx, |list, _| list.cursor_at());
        cx.simulate_keystrokes("left");
        let closed = changes_outline(&list, cx);
        cx.simulate_keystrokes("right");
        let reopened = changes_outline(&list, cx);
        cx.simulate_keystrokes("down");
        cx.simulate_keystrokes("enter");

        let view = workspace.read_with(cx, |workspace, _| workspace.view);
        let review = workspace.read_with(cx, |workspace, _| workspace.review.clone());
        let selected = review
            .read_with(cx, |review, _| review.selected_file())
            .and_then(|file| {
                workspace.read_with(cx, |workspace, _| {
                    workspace
                        .data
                        .task_diff(TaskId(0), ReviewSource::Live)
                        .and_then(|diff| diff.files.get(file).map(|file| file.path.clone()))
                })
            });
        assert_eq!(first, Some(0));
        assert!(closed.len() < open.len());
        assert_eq!(reopened, open);
        assert_eq!(view, View::Review);
        assert_eq!(selected, Some(PathBuf::from("benches/grid.rs")));
    }

    #[gpui::test]
    fn the_changes_panel_keeps_its_closed_folders_across_a_diff_refresh(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        redraw(cx);
        let list = workspace.read_with(cx, |workspace, _| workspace.changes_list.clone());

        click("change-folder-0", cx);
        let closed = changes_outline(&list, cx);
        change_first_file(&workspace, cx);
        let refreshed = changes_outline(&list, cx);

        assert_eq!(closed[0], "0 benches");
        assert_eq!(closed[1], "0 crates");
        assert_eq!(refreshed, closed);
    }

    fn selector(text: String) -> &'static str {
        Box::leak(text.into_boxed_str())
    }

    /// Opens Review on Merged, where the folded lock file is file 0.
    fn open_merged(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Entity<ReviewView> {
        use_source(workspace, ReviewSource::Merged, cx);

        let review = open_review(workspace, cx);

        assert_eq!(
            file_named(workspace, ReviewSource::Merged, "Cargo.lock", cx),
            0
        );

        review
    }

    /// The rows of one file card.
    fn card_rows(review: &Entity<ReviewView>, file: usize, cx: &VisualTestContext) -> Vec<Row> {
        review.read_with(cx, |review, _| {
            review
                .rows()
                .iter()
                .copied()
                .filter(|row| row.file() == Some(file))
                .collect()
        })
    }

    #[gpui::test]
    fn a_folded_file_is_one_header_row_with_its_tag_and_a_show_diff_button(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        let review = open_merged(&workspace, cx);
        let large = file_named(&workspace, ReviewSource::Merged, "glyphs.rs", cx);

        for file in [0, large] {
            assert_eq!(
                card_rows(&review, file, cx),
                [Row::Header {
                    file: u32::try_from(file).unwrap_or(0)
                }],
                "file {file} is more than a header"
            );
        }
        redraw(cx);

        assert!(cx.debug_bounds("review-fold-0").is_some(), "the tag");
        assert!(
            cx.debug_bounds("review-show-diff-0").is_some(),
            "the button"
        );
        assert!(cx.debug_bounds("review-toggle-0").is_none());
        let normal = file_named(&workspace, ReviewSource::Merged, "benches/grid.rs", cx);
        assert!(card_rows(&review, normal, cx).len() > 3);
        assert!(
            cx.debug_bounds(selector(format!("review-show-diff-{normal}")))
                .is_none()
        );
    }

    #[gpui::test]
    fn show_diff_fills_in_the_sample_lines_at_once_and_the_tag_stays(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_merged(&workspace, cx);
        redraw(cx);

        click("review-show-diff-0", cx);

        let rows = card_rows(&review, 0, cx);
        let (unfolded, counts) = workspace.read_with(cx, |workspace, _| {
            let file = workspace
                .data
                .task_diff(TaskId(0), ReviewSource::Merged)
                .and_then(|diff| diff.files.first().cloned());

            (
                file.as_ref()
                    .map(|file| (file.fold, !file.hunks.is_empty())),
                file.map(|file| (file.additions, file.deletions)),
            )
        });
        assert!(rows.len() > 100, "{} rows", rows.len());
        assert_eq!(unfolded, Some((Some(Fold::Lock), true)));
        assert_eq!(counts, Some((1_690, 312)));
        assert!(cx.debug_bounds("review-fold-0").is_some(), "the tag went");
        assert!(
            cx.debug_bounds("review-show-diff-0").is_none(),
            "the button stayed"
        );
    }

    #[gpui::test]
    fn j_and_k_step_over_folded_files_and_v_marks_them_viewed(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_merged(&workspace, cx);
        let first = review.read_with(cx, |review, _| review.selected_file());

        cx.simulate_keystrokes("j");
        let second = review.read_with(cx, |review, _| review.selected_file());
        cx.simulate_keystrokes("k");
        let back = review.read_with(cx, |review, _| review.selected_file());
        cx.simulate_keystrokes("v");
        let viewed = review.read_with(cx, |review, _| review.is_file_viewed(0));

        assert_eq!((first, second, back), (Some(0), Some(1), Some(0)));
        assert!(viewed, "v did not mark the folded file");
    }

    #[gpui::test]
    fn loading_the_lines_does_not_reset_viewed(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_merged(&workspace, cx);
        review.update(cx, |review, cx| review.toggle_viewed(0, cx));
        redraw(cx);
        let before = review.read_with(cx, |review, _| review.is_file_viewed(0));

        click("review-show-diff-0", cx);

        let after = review.read_with(cx, |review, _| review.is_file_viewed(0));
        let digest = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .task_diff(TaskId(0), ReviewSource::Merged)
                .and_then(|diff| diff.files.first().map(|file| file.digest))
        });
        assert!(before && after, "viewed was lost");
        assert_ne!(digest, Some(0));
    }

    fn comment_on_the_lock_file(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> usize {
        let comment = workspace.update(cx, |workspace, _| {
            let diff = workspace
                .data
                .task_diff(TaskId(0), ReviewSource::Merged)
                .cloned()
                .expect("the Merged diff");
            let file = sample_lines(&diff.files[0]);
            let at = first_changed_line(&file, 0).expect("a changed line");
            let anchor =
                crate::review_model::anchor_for(&file, at.hunk as usize, at.line as usize, at.side)
                    .expect("an anchor");
            let comment = ReviewComment {
                id: CommentId(90),
                task: swarm_protocol::TaskId(0),
                parent: None,
                source: ReviewSource::Merged,
                anchor,
                body: "Is this bump needed?".to_owned(),
                state: swarm_protocol::CommentState::Sent,
                to: None,
                outdated: false,
                created_at: 1,
                sent_at: Some(2),
                delivered_at: Some(3),
            };
            let mut list = workspace.data.task_comments(TaskId(0)).to_vec();

            list.push(comment.clone());
            workspace.data.set_comments(swarm_protocol::TaskId(0), list);

            comment
        });
        workspace.update(cx, |_, cx| cx.notify());
        redraw(cx);

        comments_of(workspace, cx)
            .iter()
            .position(|other| other.id == comment.id)
            .expect("the comment is listed")
    }

    #[gpui::test]
    fn threads_on_a_folded_file_are_counted_but_not_placed_or_outdated_until_it_loads(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        let review = open_merged(&workspace, cx);
        let root = comment_on_the_lock_file(&workspace, cx);
        redraw(cx);

        let place_of = |review: &Entity<ReviewView>, cx: &VisualTestContext| {
            review.read_with(cx, |review, _| {
                review
                    .thread_places()
                    .into_iter()
                    .find(|(index, ..)| *index == root)
                    .map(|(_, place, _)| place)
            })
        };
        let before = place_of(&review, cx);
        let counted = review.read_with(cx, |review, _| review.open_count(0));
        let thread_rows = card_rows(&review, 0, cx)
            .iter()
            .filter(|row| matches!(row, Row::Thread { .. }))
            .count();
        let header_count = cx.debug_bounds("review-folded-comments-0").is_some();

        click("review-show-diff-0", cx);
        let after = place_of(&review, cx);
        let placed_rows = card_rows(&review, 0, cx)
            .iter()
            .filter(|row| matches!(row, Row::Thread { .. }))
            .count();

        assert_eq!(before, Some(Place::Folded));
        assert_eq!(counted, 1, "the badge lost the thread");
        assert_eq!(thread_rows, 0);
        assert!(header_count, "the header lost the count");
        assert!(matches!(after, Some(Place::Line { .. })), "{after:?}");
        assert_eq!(placed_rows, 1);
    }

    #[gpui::test]
    fn a_file_in_the_changes_panel_opens_review_at_that_file(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        workspace.update(cx, |workspace, cx| {
            workspace.open_changed_file(
                PathBuf::from("/work/swarm/terminal-renderer"),
                PathBuf::from("crates/swarm-ui/src/theme.rs"),
                cx,
            );
        });
        redraw(cx);

        let review = workspace.read_with(cx, |workspace, _| workspace.review.clone());
        let path = review.read_with(cx, |review, _| {
            review
                .selected_file()
                .and_then(|file| review.diff().and_then(|diff| diff.files.get(file)))
                .map(|file| file.path.clone())
        });
        let view = workspace.read_with(cx, |workspace, _| workspace.view);

        assert_eq!(view, View::Review);
        assert_eq!(path, Some(PathBuf::from("crates/swarm-ui/src/theme.rs")));
    }

    #[gpui::test]
    fn the_review_view_names_its_controls(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        open_review(&workspace, cx);

        for selector in [
            "review-source",
            "review-style",
            "review-approve",
            "review-nav-file-0",
        ] {
            assert!(cx.debug_bounds(selector).is_some(), "{selector}");
        }
    }

    #[gpui::test]
    fn a_narrow_header_cuts_the_branch_but_never_the_counts(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.set_review_source(ReviewSource::Merged, cx);
        });
        open_review(&workspace, cx);
        let pending = count_comments(&comments_of(&workspace, cx)).pending;

        cx.simulate_resize(gpui::size(px(2800.), px(800.)));
        redraw(cx);
        let wide = (
            cx.debug_bounds("review-place")
                .map(|bounds| bounds.size.width),
            cx.debug_bounds("review-counts")
                .map(|bounds| bounds.size.width),
        );
        let follows = cx
            .debug_bounds("review-place")
            .zip(cx.debug_bounds("review-counts"))
            .map(|(place, counts)| counts.left() - place.right());
        assert!(
            follows.is_some_and(|gap| gap < px(16.)),
            "the counts sit away from the place: {follows:?}"
        );

        for width in [1330., 1280.] {
            cx.simulate_resize(gpui::size(px(width), px(800.)));
            redraw(cx);
            let place = cx.debug_bounds("review-place");
            let counts = cx.debug_bounds("review-counts");
            let source = cx.debug_bounds("review-source");
            let request = cx.debug_bounds("review-request-changes");
            let approve = cx.debug_bounds("review-approve");

            let (Some(place), Some(counts), Some(source), Some(_), Some(approve)) =
                (place, counts, source, request, approve)
            else {
                panic!("the header is missing a part at {width} px");
            };

            assert!(pending > 0, "Request changes shows its count");
            assert!(
                Some(place.size.width) < wide.0,
                "the branch was not cut at {width} px"
            );
            assert_eq!(Some(counts.size.width), wide.1, "the counts shrank");
            assert!(place.right() <= counts.left(), "{place:?} {counts:?}");
            assert!(counts.right() <= source.left(), "{counts:?} {source:?}");
            assert!(approve.right() <= px(width), "{approve:?}");
        }
    }

    #[gpui::test]
    fn the_viewed_mark_survives_a_source_switch_unless_the_file_changed(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        workspace.update(cx, |workspace, cx| {
            workspace.set_review_source(ReviewSource::Live, cx);
        });
        redraw(cx);

        review.update(cx, |review, cx| review.toggle_viewed(0, cx));
        workspace.update(cx, |workspace, cx| {
            workspace.set_review_source(ReviewSource::Merged, cx);
        });
        redraw(cx);
        let merged = review.read_with(cx, |review, _| review.is_file_viewed(0));
        workspace.update(cx, |workspace, cx| {
            workspace.set_review_source(ReviewSource::Live, cx);
        });
        redraw(cx);
        let back = review.read_with(cx, |review, _| review.is_file_viewed(0));

        workspace.update(cx, |workspace, cx| {
            workspace.set_review_source(ReviewSource::Merged, cx);
        });
        redraw(cx);
        change_first_file(&workspace, cx);
        workspace.update(cx, |workspace, cx| {
            workspace.set_review_source(ReviewSource::Live, cx);
        });
        redraw(cx);
        let changed = review.read_with(cx, |review, _| review.is_file_viewed(0));

        assert!(!merged);
        assert!(back, "the mark was lost while the file was not shown");
        assert!(!changed, "a changed file kept its mark");
    }

    #[gpui::test]
    fn no_newline_rows_a_mode_tag_and_a_too_large_note_show(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, _| {
            let key = (swarm_protocol::TaskId(0), ReviewSource::Live);
            let mut diff = sample_diff(
                swarm_protocol::TaskId(0),
                ReviewSource::Live,
                [swarm_protocol::AgentId(1); 3],
            );
            let last = diff.files.len() - 1;
            diff.files[last].hunks.clear();
            diff.files[last].truncated = true;
            workspace.data.diffs.insert(key, Rc::new(diff));
        });
        let review = open_review(&workspace, cx);

        let unified = review.read_with(cx, |review, _| review.rows().to_vec());
        let diff = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .diffs
                .get(&(swarm_protocol::TaskId(0), ReviewSource::Live))
                .cloned()
        });
        let no_newline = |rows: &[Row]| {
            rows.iter()
                .filter(|row| match (row, &diff) {
                    (
                        Row::Line {
                            file, hunk, line, ..
                        },
                        Some(diff),
                    ) => hunk_line(diff, *file, *hunk, *line)
                        .is_some_and(|line| line.kind == DiffLineKind::NoNewline),
                    _ => false,
                })
                .count()
        };
        let tag = cx.debug_bounds("review-mode-0").is_some();
        review.update(cx, |review, cx| review.set_style(DiffStyle::Split, cx));
        redraw(cx);

        assert!(no_newline(&unified) >= 1);
        assert!(tag, "the mode tag is missing");
        assert!(unified.iter().any(|row| matches!(
            row,
            Row::Note {
                note: Note::TooLarge,
                ..
            }
        )));
    }

    fn comments_of(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> Vec<ReviewComment> {
        workspace.read_with(cx, |workspace, _| {
            workspace.data.task_comments(TaskId(0)).to_vec()
        })
    }

    fn use_source(workspace: &Entity<Workspace>, source: ReviewSource, cx: &mut VisualTestContext) {
        workspace.update(cx, |workspace, cx| workspace.set_review_source(source, cx));
        redraw(cx);
    }

    fn first_line_row(review: &Entity<ReviewView>, cx: &VisualTestContext) -> usize {
        review.read_with(cx, |review, _| {
            review
                .rows()
                .iter()
                .position(|row| matches!(row, Row::Line { new, .. } if *new > 0))
                .unwrap_or(0)
        })
    }

    #[gpui::test]
    fn live_comments_sit_on_their_lines_and_the_outdated_one_at_the_top(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);

        let places = review.read_with(cx, |review, _| review.thread_places());
        let rows = review.read_with(cx, |review, _| review.rows().to_vec());

        let on_lines = places
            .iter()
            .filter(|(_, place, _)| matches!(place, Place::Line { .. }))
            .count();
        let at_top = places
            .iter()
            .filter(|(_, place, _)| *place == Place::Top)
            .count();
        let threads = rows
            .iter()
            .filter(|row| matches!(row, Row::Thread { .. }))
            .count();
        assert_eq!((on_lines, at_top, threads), (2, 1, 3));
    }

    #[gpui::test]
    fn a_resolved_thread_is_folded_until_it_is_opened(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        let grid = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .task_diff(TaskId(0), ReviewSource::Live)
                .and_then(|diff| {
                    diff.files
                        .iter()
                        .position(|file| file.path.ends_with("benches/grid.rs"))
                })
                .unwrap_or(0)
        });

        review.update(cx, |review, cx| review.select_file(grid, cx));
        redraw(cx);
        let folded = review.read_with(cx, |review, _| review.is_folded(CommentId(5)));
        click("comment-folded-5", cx);
        let opened = review.read_with(cx, |review, _| review.is_folded(CommentId(5)));

        assert!(folded && !opened);
        assert!(cx.debug_bounds("comment-thread-5").is_some());
    }

    #[gpui::test]
    fn the_nav_counts_open_comments_per_file(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);

        let counts: Vec<u32> = review.read_with(cx, |review, _| {
            (0..11).map(|file| review.open_count(file)).collect()
        });

        assert_eq!(
            counts.iter().sum::<u32>(),
            2,
            "the resolved thread is not open"
        );
    }

    #[gpui::test]
    fn merged_has_request_changes_and_live_does_not(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        open_review(&workspace, cx);

        let live = cx.debug_bounds("review-request-changes").is_some();
        use_source(&workspace, ReviewSource::Merged, cx);
        let merged = cx.debug_bounds("review-request-changes").is_some();

        assert!(!live && merged);
        assert_eq!(comments::submit_label(ReviewSource::Live), "Comment");
        assert_eq!(comments::submit_label(ReviewSource::Merged), "Add comment");
    }

    #[gpui::test]
    fn request_changes_sends_the_pending_comments_and_then_approve_works(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        use_source(&workspace, ReviewSource::Merged, cx);
        open_review(&workspace, cx);

        click("review-approve", cx);
        let blocked = approve_state(&workspace, cx);
        click("review-request-changes", cx);
        let counts = count_comments(&comments_of(&workspace, cx));
        click("review-approve", cx);

        assert_eq!(blocked, None, "Approve waits while comments are pending");
        assert_eq!(counts.pending, 0);
        assert_eq!(approve_state(&workspace, cx), Some(0x22));
    }

    #[gpui::test]
    fn comment_events_change_and_delete_comments(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        let mut changed = comments_of(&workspace, cx)[0].clone();
        changed.state = swarm_protocol::CommentState::Resolved;
        let id = changed.id;
        let before = comments_of(&workspace, cx).len();

        workspace.update(cx, |workspace, cx| {
            workspace.comment_event(&CommentEvent::Changed(Box::new(changed)), cx);
        });
        redraw(cx);
        let folded = review.read_with(cx, |review, _| review.is_folded(id));
        workspace.update(cx, |workspace, cx| {
            workspace.comment_event(
                &CommentEvent::Deleted {
                    task: swarm_protocol::TaskId(0),
                    id,
                },
                cx,
            );
        });
        redraw(cx);

        assert!(folded);
        assert_eq!(comments_of(&workspace, cx).len(), before - 1);
    }

    #[gpui::test]
    fn the_status_bar_counts_comments_only_in_review(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        redraw(cx);
        let outside = cx.debug_bounds("comment-counts").is_some();

        open_review(&workspace, cx);

        assert!(!outside);
        assert!(cx.debug_bounds("comment-counts").is_some());
    }

    #[gpui::test]
    fn the_arrows_move_the_line_cursor(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);

        let none = review.read_with(cx, |review, _| review.cursor_pos());
        cx.simulate_keystrokes("down");
        let first = review.read_with(cx, |review, _| review.cursor_pos());
        cx.simulate_keystrokes("down");
        let second = review.read_with(cx, |review, _| review.cursor_pos());
        cx.simulate_keystrokes("up");
        let back = review.read_with(cx, |review, _| review.cursor_pos());

        assert!(none.is_none());
        assert!(first.is_some() && second.is_some());
        assert_ne!(first, second);
        assert_eq!(first, back);
    }

    #[gpui::test]
    fn the_cursor_stays_on_its_side_in_split(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        review.update(cx, |review, cx| review.set_style(DiffStyle::Split, cx));
        redraw(cx);

        cx.simulate_keystrokes("down down");
        let side = review.read_with(cx, |review, _| review.cursor_pos().map(|pos| pos.side));
        cx.simulate_keystrokes("down");
        let after = review.read_with(cx, |review, _| review.cursor_pos().map(|pos| pos.side));

        assert!(side.is_some());
        assert_eq!(side, after);
    }

    #[gpui::test]
    fn a_line_number_sets_the_cursor_and_the_plus_opens_the_box(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        let row = first_line_row(&review, cx);

        click(Box::leak(format!("review-new-{row}").into_boxed_str()), cx);
        let cursor = review.read_with(cx, |review, _| review.cursor_pos());
        click(Box::leak(format!("review-add-{row}").into_boxed_str()), cx);
        let open = review.read_with(cx, |review, _| review.composer_is_open());

        assert!(cursor.is_some());
        assert!(open);
        assert!(cx.debug_bounds("comment-composer").is_some());
    }

    #[gpui::test]
    fn c_opens_the_box_and_plain_keys_type_instead_of_firing(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        let file = review.read_with(cx, |review, _| review.selected_file());

        cx.simulate_keystrokes("c");
        redraw(cx);
        let open = review.read_with(cx, |review, _| review.composer_is_open());
        cx.simulate_input("jkvc");
        let (text, viewed, now) = review.read_with(cx, |review, cx| {
            (
                review.composer_text(cx),
                review.viewed_count(),
                review.selected_file(),
            )
        });

        assert!(open);
        assert_eq!(text, "jkvc");
        assert_eq!(viewed, 0);
        assert_eq!(file, now);
    }

    #[gpui::test]
    fn escape_cancels_and_cmd_enter_adds_a_live_comment_at_once(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        let before = comments_of(&workspace, cx).len();

        cx.simulate_keystrokes("c");
        redraw(cx);
        cx.simulate_keystrokes("escape");
        let cancelled = !review.read_with(cx, |review, _| review.composer_is_open());

        cx.simulate_keystrokes("c");
        redraw(cx);
        cx.simulate_input("Rename this");
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();
        redraw(cx);
        let added = comments_of(&workspace, cx);
        let closed = !review.read_with(cx, |review, _| review.composer_is_open());

        assert!(cancelled && closed);
        assert_eq!(added.len(), before + 1);
        let new = added
            .last()
            .map(|comment| (comment.body.clone(), comment.state));
        assert_eq!(
            new,
            Some(("Rename this".to_owned(), swarm_protocol::CommentState::Sent))
        );
    }

    #[gpui::test]
    fn a_merged_comment_waits_as_pending(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        use_source(&workspace, ReviewSource::Merged, cx);
        open_review(&workspace, cx);
        let before = count_comments(&comments_of(&workspace, cx)).pending;

        cx.simulate_keystrokes("j");
        cx.simulate_keystrokes("c");
        redraw(cx);
        cx.simulate_input("Why");
        click("comment-submit", cx);
        cx.run_until_parked();

        let after = count_comments(&comments_of(&workspace, cx)).pending;
        assert_eq!(after, before + 1);
    }

    #[gpui::test]
    fn a_body_over_the_limit_is_refused_in_the_app(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        let before = comments_of(&workspace, cx).len();

        cx.simulate_keystrokes("c");
        redraw(cx);
        let area = review.read_with(cx, |review, _| review.composer_input().clone());
        area.update(cx, |area, cx| {
            area.set_text("x".repeat(swarm_protocol::COMMENT_BYTES + 1), cx);
        });
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();

        let error = review.read_with(cx, |review, _| review.composer_error());
        assert!(error.is_some());
        assert_eq!(comments_of(&workspace, cx).len(), before);
    }

    #[gpui::test]
    fn an_unsent_draft_survives_a_file_switch_and_a_move_to_another_line(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);

        cx.simulate_keystrokes("c");
        redraw(cx);
        let first = review.read_with(cx, |review, _| review.cursor_pos());
        cx.simulate_input("draft one");
        review.update(cx, |review, cx| review.select_file(3, cx));
        redraw(cx);
        let still_open = review.read_with(cx, |review, _| review.composer_is_open());

        let moved = first.map(|pos| LinePos {
            line: pos.line + 1,
            ..pos
        });
        if let Some(pos) = moved {
            review.update(cx, |review, cx| review.open_composer(pos, cx));
        }
        let other = review.read_with(cx, ReviewView::composer_text);
        if let Some(pos) = first {
            review.update(cx, |review, cx| review.open_composer(pos, cx));
        }
        let restored = review.read_with(cx, ReviewView::composer_text);
        review.update(cx, ReviewView::cancel_composer);
        if let Some(pos) = first {
            review.update(cx, |review, cx| review.open_composer(pos, cx));
        }
        let after_cancel = review.read_with(cx, ReviewView::composer_text);

        assert!(still_open);
        assert_eq!(other, "");
        assert_eq!(restored, "draft one");
        assert_eq!(after_cancel, "");
    }

    #[gpui::test]
    fn a_reply_goes_under_its_thread_and_a_pending_one_can_be_deleted(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        use_source(&workspace, ReviewSource::Merged, cx);
        let review = open_review(&workspace, cx);
        let before = comments_of(&workspace, cx).len();

        review.update(cx, |review, cx| {
            review.reply_for_test("Thanks", CommentId(1), cx);
        });
        cx.run_until_parked();
        let replied = comments_of(&workspace, cx);
        workspace.update(cx, |workspace, cx| {
            workspace.delete_review_comment(swarm_protocol::TaskId(0), CommentId(1), cx);
        });
        cx.run_until_parked();

        assert_eq!(replied.len(), before + 1);
        assert_eq!(replied.last().and_then(|c| c.parent), Some(CommentId(1)));
        assert!(
            comments_of(&workspace, cx)
                .iter()
                .all(|comment| comment.id != CommentId(1) && comment.parent != Some(CommentId(1)))
        );
    }

    fn live_pos(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> LinePos {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .task_diff(TaskId(0), ReviewSource::Live)
                .and_then(|diff| {
                    diff.files
                        .first()
                        .and_then(|file| first_changed_line(file, 0))
                })
                .expect("the first Live file has a changed line")
        })
    }

    /// Stores a new Live diff after `edit` changed the first file.
    fn edit_first_live_file(
        workspace: &Entity<Workspace>,
        edit: impl FnOnce(&mut FileDiff),
        cx: &mut VisualTestContext,
    ) {
        workspace.update(cx, |workspace, cx| {
            let key = (swarm_protocol::TaskId(0), ReviewSource::Live);
            if let Some(diff) = workspace.data.diffs.get_mut(&key) {
                let mut changed = (**diff).clone();

                changed.fingerprint += 1;
                edit(&mut changed.files[0]);
                *diff = Rc::new(changed);
            }
            cx.notify();
        });
        redraw(cx);
    }

    fn context_lines(count: usize) -> Vec<DiffLine> {
        (0..count)
            .map(|n| DiffLine {
                kind: DiffLineKind::Context,
                text: format!("inserted {n}"),
            })
            .collect()
    }

    fn composer_focused(review: &Entity<ReviewView>, cx: &mut VisualTestContext) -> bool {
        let handle = review.read_with(cx, |review, cx| review.composer_input().focus_handle(cx));

        cx.update(|window, _| handle.is_focused(window))
    }

    /// The row index of the box, and the line row it sits under.
    fn composer_under(review: &Entity<ReviewView>, cx: &VisualTestContext) -> Option<(usize, Row)> {
        review.read_with(cx, |review, _| {
            let rows = review.rows();
            let at = rows
                .iter()
                .position(|row| matches!(row, Row::Composer { .. }))?;
            let above = rows[..at]
                .iter()
                .rposition(|row| !matches!(row, Row::Thread { .. }))?;

            Some((at, rows[above]))
        })
    }

    #[gpui::test]
    fn the_comment_box_follows_its_line_when_a_live_diff_moves_it(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        use_source(&workspace, ReviewSource::Live, cx);
        let pos = live_pos(&workspace, cx);

        review.update(cx, |review, cx| review.open_composer(pos, cx));
        redraw(cx);
        cx.simulate_input("keep me");
        let before = review.read_with(cx, |review, _| review.composer_line());

        edit_first_live_file(
            &workspace,
            |file| {
                let top = &mut file.hunks[0].lines;
                top.splice(0..0, context_lines(3));
            },
            cx,
        );
        let after = review.read_with(cx, |review, _| review.composer_line());
        let at = review.read_with(cx, |review, _| review.composer_at());
        let under = composer_under(&review, cx);

        assert_eq!(after, before.map(|line| line + 3));
        assert_eq!(
            at,
            Some(ComposerAt::Line(LinePos {
                line: pos.line + 3,
                ..pos
            }))
        );
        assert!(matches!(
            under,
            Some((_, Row::Line { line, .. })) if line == pos.line + 3
        ));
        assert_eq!(review.read_with(cx, ReviewView::composer_text), "keep me");
        assert!(composer_focused(&review, cx), "the box lost focus");
        cx.simulate_input("!");
        assert_eq!(review.read_with(cx, ReviewView::composer_text), "keep me!");
    }

    #[gpui::test]
    fn a_comment_box_whose_line_is_gone_waits_at_the_top_with_its_text(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        use_source(&workspace, ReviewSource::Live, cx);
        let pos = live_pos(&workspace, cx);
        let original = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .task_diff(TaskId(0), ReviewSource::Live)
                .map(|diff| diff.files[0].hunks.clone())
                .unwrap_or_default()
        });

        review.update(cx, |review, cx| review.open_composer(pos, cx));
        redraw(cx);
        cx.simulate_input("do not lose me");

        let removed = pos.line as usize;
        edit_first_live_file(
            &workspace,
            |file| {
                file.hunks[0].lines[removed].text = "something else entirely".into();
            },
            cx,
        );
        let at = review.read_with(cx, |review, _| review.composer_at());
        let under = composer_under(&review, cx);

        assert_eq!(at, Some(ComposerAt::Top { file: 0 }));
        assert!(
            matches!(under, Some((_, Row::Header { file: 0 }))),
            "the box sits right under the card header: {under:?}"
        );
        assert!(review.read_with(cx, |review, _| review.composer_is_open()));
        assert_eq!(
            review.read_with(cx, ReviewView::composer_text),
            "do not lose me"
        );
        assert!(composer_focused(&review, cx), "the box lost focus");
        review.update(cx, |review, cx| review.select_file(0, cx));
        redraw(cx);
        let tagged = cx.debug_bounds("comment-composer-outdated").is_some();

        edit_first_live_file(&workspace, |file| file.hunks = original.clone(), cx);
        let back = review.read_with(cx, |review, _| review.composer_at());
        let still_tagged = cx.debug_bounds("comment-composer-outdated").is_some();

        assert!(tagged, "the box shows no Outdated tag");
        assert_eq!(back, Some(ComposerAt::Line(pos)));
        assert!(!still_tagged, "the tag stayed after the line came back");
    }

    #[gpui::test]
    fn a_comment_sent_from_one_line_does_not_close_the_box_of_another(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        use_source(&workspace, ReviewSource::Merged, cx);
        review.update(cx, ReviewView::next_file);
        let first = review.read_with(cx, |review, _| review.selected_file());
        let pos = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .task_diff(TaskId(0), ReviewSource::Merged)
                .and_then(|diff| {
                    first
                        .and_then(|file| diff.files.get(file).map(|data| (file, data)))
                        .and_then(|(file, data)| first_changed_line(data, file))
                })
                .expect("the selected file has a changed line")
        });
        let other = LinePos {
            line: pos.line + 1,
            ..pos
        };

        review.update(cx, |review, cx| review.open_composer(pos, cx));
        redraw(cx);
        let first_line = review.read_with(cx, |review, _| review.composer_line());
        let area = review.read_with(cx, |review, _| review.composer_input().clone());
        area.update(cx, |area, cx| area.set_text("first", cx));
        review.update(cx, |review, cx| {
            review.submit_composer_for_test(cx);
            review.open_composer(other, cx);
        });
        area.update(cx, |area, cx| area.set_text("second", cx));
        let sending_while_away = review.read_with(cx, |review, _| review.composer_sending());
        cx.run_until_parked();

        let (line, text, error) = review.read_with(cx, |review, cx| {
            (
                review.composer_line(),
                review.composer_text(cx),
                review.composer_error(),
            )
        });
        let parked = review.read_with(cx, |review, _| review.draft_text(pos));
        review.update(cx, |review, cx| review.open_composer(pos, cx));
        let reopened = review.read_with(cx, ReviewView::composer_text);

        assert!(!sending_while_away, "the other line is not sending");
        assert!(line.is_some() && line != first_line, "the other box closed");
        assert!(review.read_with(cx, |review, _| review.composer_is_open()));
        assert_eq!(text, "second", "the answer touched the other box");
        assert_eq!(error, None);
        assert_eq!(parked, None, "sent text was parked as a draft");
        assert_eq!(reopened, "", "sent text came back");
    }

    #[gpui::test]
    fn a_failed_comment_comes_back_with_its_text_and_leaves_the_other_box_alone(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        use_source(&workspace, ReviewSource::Live, cx);
        workspace.update(cx, |workspace, _| {
            workspace.source = crate::workspace::DataSource::Connecting;
        });
        let pos = live_pos(&workspace, cx);
        let other = LinePos {
            line: pos.line + 1,
            ..pos
        };

        review.update(cx, |review, cx| review.open_composer(pos, cx));
        redraw(cx);
        let area = review.read_with(cx, |review, _| review.composer_input().clone());
        area.update(cx, |area, cx| area.set_text("first", cx));
        review.update(cx, |review, cx| {
            review.submit_composer_for_test(cx);
            review.open_composer(other, cx);
        });
        cx.run_until_parked();

        let (text, error, sending) = review.read_with(cx, |review, cx| {
            (
                review.composer_text(cx),
                review.composer_error(),
                review.composer_sending(),
            )
        });
        review.update(cx, |review, cx| review.open_composer(pos, cx));
        let (back, back_error, back_sending) = review.read_with(cx, |review, cx| {
            (
                review.composer_text(cx),
                review.composer_error(),
                review.composer_sending(),
            )
        });

        assert_eq!((text.as_ref(), error, sending), ("", None, false));
        assert_eq!(back, "first");
        assert_eq!(back_error.as_deref(), Some("Not connected"));
        assert!(!back_sending);
    }

    #[gpui::test]
    fn a_reply_is_sent_once_and_its_field_waits_for_the_answer(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        use_source(&workspace, ReviewSource::Merged, cx);
        let review = open_review(&workspace, cx);
        let before = comments_of(&workspace, cx).len();
        let root = CommentId(1);

        let input = review.update(cx, |review, cx| review.reply_input_for_test(root, cx));
        input.update(cx, |input, cx| input.set_text("Thanks", cx));
        review.update(cx, |review, cx| {
            review.submit_reply(root, cx);
            review.submit_reply(root, cx);
        });
        let sending = review.read_with(cx, |review, _| review.reply_sending(root));
        cx.run_until_parked();

        let (waiting, text) = (
            review.read_with(cx, |review, _| review.reply_sending(root)),
            input.read_with(cx, |input, _| input.text()),
        );

        assert!(sending);
        assert_eq!(comments_of(&workspace, cx).len(), before + 1, "sent twice");
        assert!(!waiting);
        assert_eq!(text, "", "the sent text stayed in the field");
    }

    #[gpui::test]
    fn a_failed_reply_keeps_its_text_and_can_be_sent_again(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        use_source(&workspace, ReviewSource::Merged, cx);
        let review = open_review(&workspace, cx);
        let before = comments_of(&workspace, cx).len();
        let root = CommentId(1);
        workspace.update(cx, |workspace, _| {
            workspace.source = crate::workspace::DataSource::Connecting;
        });

        let input = review.update(cx, |review, cx| review.reply_input_for_test(root, cx));
        input.update(cx, |input, cx| input.set_text("Thanks", cx));
        review.update(cx, |review, cx| review.submit_reply(root, cx));
        cx.run_until_parked();

        let (waiting, text, error) = (
            review.read_with(cx, |review, _| review.reply_sending(root)),
            input.read_with(cx, |input, _| input.text()),
            review.read_with(cx, |review, _| review.card_error(root)),
        );

        assert!(!waiting);
        assert_eq!(text, "Thanks");
        assert_eq!(error.as_deref(), Some("Not connected"));
        assert_eq!(comments_of(&workspace, cx).len(), before);
    }

    #[gpui::test]
    fn deleting_or_resolving_a_thread_leaves_its_reply_text_alone(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        use_source(&workspace, ReviewSource::Live, cx);
        let review = open_review(&workspace, cx);
        let root = CommentId(3);

        let input = review.update(cx, |review, cx| review.reply_input_for_test(root, cx));
        input.update(cx, |input, cx| input.set_text("half a reply", cx));
        workspace.update(cx, |workspace, cx| {
            workspace.resolve_review_comment(swarm_protocol::TaskId(0), root, true, cx);
        });
        cx.run_until_parked();

        assert_eq!(input.read_with(cx, |input, _| input.text()), "half a reply");
    }

    #[gpui::test]
    fn one_tasks_request_changes_does_not_block_another_task(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        use_source(&workspace, ReviewSource::Merged, cx);
        open_review(&workspace, cx);
        let active = workspace.read_with(cx, |workspace, _| workspace.active_task);
        let pending = |cx: &VisualTestContext| count_comments(&comments_of(&workspace, cx)).pending;

        workspace.update(cx, |workspace, cx| {
            workspace.review_requesting.insert(active);
            workspace.request_review_changes(cx);
        });
        let own_in_flight = pending(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.review_requesting.clear();
            workspace.review_requesting.insert(TaskId(99));
            workspace.request_review_changes(cx);
        });

        assert!(own_in_flight > 0, "a second request went out for the task");
        assert_eq!(pending(cx), 0, "another task's request blocked this one");
    }

    #[gpui::test]
    fn one_tasks_approval_does_not_block_another_task(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        use_source(&workspace, ReviewSource::Merged, cx);
        open_review(&workspace, cx);
        click("review-request-changes", cx);
        workspace.update(cx, |workspace, _| {
            workspace.review_approving.insert(TaskId(99));
        });

        click("review-approve", cx);

        assert_eq!(approve_state(&workspace, cx), Some(0x22));
    }

    #[gpui::test]
    fn a_comment_of_the_limit_goes_through_and_one_byte_more_does_not(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        let before = comments_of(&workspace, cx).len();
        let limit = swarm_protocol::COMMENT_BYTES;

        cx.simulate_keystrokes("c");
        redraw(cx);
        let area = review.read_with(cx, |review, _| review.composer_input().clone());
        area.update(cx, |area, cx| area.set_text("x".repeat(limit + 1), cx));
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();
        let refused = review.read_with(cx, |review, _| review.composer_error());
        let unchanged = comments_of(&workspace, cx).len();

        area.update(cx, |area, cx| area.set_text("x".repeat(limit), cx));
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();

        assert_eq!(limit, 16 << 10);
        assert_eq!(refused.as_deref(), Some("Comment is too long"));
        assert_eq!(unchanged, before);
        assert_eq!(comments_of(&workspace, cx).len(), before + 1);
    }

    #[gpui::test]
    fn a_reply_over_the_limit_is_refused_in_the_app(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        use_source(&workspace, ReviewSource::Merged, cx);
        let review = open_review(&workspace, cx);
        let before = comments_of(&workspace, cx).len();
        let root = CommentId(1);

        review.update(cx, |review, cx| {
            review.reply_for_test(&"x".repeat(swarm_protocol::COMMENT_BYTES + 1), root, cx);
        });
        cx.run_until_parked();

        let error = review.read_with(cx, |review, _| review.card_error(root));
        assert_eq!(error.as_deref(), Some("Comment is too long"));
        assert_eq!(comments_of(&workspace, cx).len(), before);
    }

    /// Changes a comment as a daemon event would and draws the result.
    fn change_comment(
        workspace: &Entity<Workspace>,
        id: CommentId,
        change: impl FnOnce(&mut ReviewComment),
        cx: &mut VisualTestContext,
    ) {
        let mut comment = comments_of(workspace, cx)
            .into_iter()
            .find(|comment| comment.id == id)
            .expect("the sample has the comment");
        change(&mut comment);

        workspace.update(cx, |workspace, cx| {
            workspace.comment_event(&CommentEvent::Changed(Box::new(comment)), cx);
        });
        redraw(cx);
    }

    fn comment_state(
        workspace: &Entity<Workspace>,
        id: CommentId,
        cx: &VisualTestContext,
    ) -> swarm_protocol::CommentState {
        comments_of(workspace, cx)
            .into_iter()
            .find(|comment| comment.id == id)
            .map(|comment| comment.state)
            .expect("the sample has the comment")
    }

    fn focused(handle: &FocusHandle, cx: &mut VisualTestContext) -> bool {
        cx.update(|window, _| handle.is_focused(window))
    }

    /// The sample's sent Live thread, open on screen.
    fn thread_three(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Entity<ReviewView> {
        let review = open_review(workspace, cx);
        let file = review.read_with(cx, |review, _| review.file_of_thread(CommentId(3)));

        review.update(cx, |review, cx| review.select_file(file.unwrap_or(0), cx));
        redraw(cx);
        assert!(
            cx.debug_bounds("comment-thread-3").is_some(),
            "thread 3 is not on screen"
        );

        review
    }

    #[gpui::test]
    fn reply_opens_a_field_that_takes_focus_and_cancel_gives_focus_back(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = thread_three(&workspace, cx);
        let root = CommentId(3);
        let button = review.update(cx, |review, cx| review.reply_button_focus(root, cx));
        let input = review.update(cx, |review, cx| review.reply_input_for_test(root, cx));
        let field = input.read_with(cx, |_, cx| input.focus_handle(cx));
        let closed = cx.debug_bounds("comment-reply-3").is_none();

        click("comment-reply-open-3", cx);
        let (open, in_field) = (
            review.read_with(cx, |review, _| review.reply_is_open(root)),
            focused(&field, cx),
        );
        cx.simulate_input("jkvc");
        let typed = input.read_with(cx, |input, _| input.text());
        let viewed = review.read_with(cx, |review, _| review.viewed_count());
        let composer = review.read_with(cx, |review, _| review.composer_is_open());

        cx.simulate_keystrokes("escape");
        redraw(cx);
        let (open_after, text_after, on_button) = (
            review.read_with(cx, |review, _| review.reply_is_open(root)),
            input.read_with(cx, |input, _| input.text()),
            focused(&button, cx),
        );

        assert!(closed && cx.debug_bounds("comment-reply-3").is_none());
        assert!(open && in_field, "the field opens with focus in it");
        assert_eq!(typed, "jkvc", "plain keys type in the field");
        assert_eq!(viewed, 0);
        assert!(!composer);
        assert!(!open_after && !focused(&field, cx));
        assert_eq!(text_after, "");
        assert!(on_button, "focus did not go back to the Reply button");
    }

    #[gpui::test]
    fn the_cancel_button_closes_the_field_and_restores_focus(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = thread_three(&workspace, cx);
        let root = CommentId(3);
        let button = review.update(cx, |review, cx| review.reply_button_focus(root, cx));
        let before = comments_of(&workspace, cx).len();

        click("comment-reply-open-3", cx);
        cx.simulate_input("half a thought");
        click("comment-reply-cancel-3", cx);

        assert!(!review.read_with(cx, |review, _| review.reply_is_open(root)));
        assert!(cx.debug_bounds("comment-reply-3").is_none());
        assert!(focused(&button, cx));
        assert_eq!(
            comments_of(&workspace, cx).len(),
            before,
            "nothing was sent"
        );
        assert_eq!(
            review
                .update(cx, |review, cx| review.reply_input_for_test(root, cx))
                .read_with(cx, |input, _| input.text()),
            ""
        );
    }

    #[gpui::test]
    fn send_sends_a_reply_once_and_closes_the_field(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = thread_three(&workspace, cx);
        let root = CommentId(3);
        let button = review.update(cx, |review, cx| review.reply_button_focus(root, cx));
        let before = comments_of(&workspace, cx).len();

        click("comment-reply-open-3", cx);
        click("comment-reply-send-3", cx);
        let empty = comments_of(&workspace, cx).len();

        click("comment-reply-open-3", cx);
        cx.simulate_input("Thanks");
        review.update(cx, |review, cx| {
            review.submit_reply(root, cx);
            review.submit_reply(root, cx);
        });
        cx.run_until_parked();
        redraw(cx);
        let replies = comments_of(&workspace, cx);

        assert_eq!(empty, before, "an empty reply was sent");
        assert_eq!(replies.len(), before + 1, "sent twice or not at all");
        let reply = replies.last().expect("the reply");
        assert_eq!(reply.parent, Some(root));
        assert_eq!(reply.state, swarm_protocol::CommentState::Sent);
        assert!(!review.read_with(cx, |review, _| review.reply_is_open(root)));
        assert!(focused(&button, cx), "focus did not go back to Reply");

        click("comment-reply-open-3", cx);
        cx.simulate_input("Again");
        click("comment-reply-send-3", cx);
        cx.run_until_parked();
        redraw(cx);

        assert_eq!(comments_of(&workspace, cx).len(), before + 2);
        assert!(focused(&button, cx));
    }

    #[gpui::test]
    fn a_reply_that_succeeds_leaves_focus_where_the_user_moved_it(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = thread_three(&workspace, cx);
        let root = CommentId(3);
        let button = review.update(cx, |review, cx| review.reply_button_focus(root, cx));
        let elsewhere = review.read_with(cx, |review, _| review.body_focus().clone());

        click("comment-reply-open-3", cx);
        cx.simulate_input("Thanks");
        review.update(cx, |review, cx| review.submit_reply(root, cx));
        cx.update(|window, cx| window.focus(&elsewhere, cx));
        cx.run_until_parked();
        redraw(cx);

        assert!(!review.read_with(cx, |review, _| review.reply_is_open(root)));
        assert!(focused(&elsewhere, cx), "the answer took focus away");
        assert!(!focused(&button, cx));
    }

    #[gpui::test]
    fn cmd_enter_sends_the_reply_from_the_field(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = thread_three(&workspace, cx);
        let before = comments_of(&workspace, cx).len();

        click("comment-reply-open-3", cx);
        cx.simulate_input("Thanks");
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();
        redraw(cx);

        assert_eq!(comments_of(&workspace, cx).len(), before + 1);
        assert!(!review.read_with(cx, |review, _| review.reply_is_open(CommentId(3))));
    }

    #[gpui::test]
    fn a_failed_reply_keeps_its_field_open_with_the_text(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = thread_three(&workspace, cx);
        let root = CommentId(3);
        workspace.update(cx, |workspace, _| {
            workspace.source = crate::workspace::DataSource::Connecting;
        });

        click("comment-reply-open-3", cx);
        cx.simulate_input("Thanks");
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();
        redraw(cx);

        assert!(review.read_with(cx, |review, _| review.reply_is_open(root)));
        assert_eq!(
            review
                .update(cx, |review, cx| review.reply_input_for_test(root, cx))
                .read_with(cx, |input, _| input.text()),
            "Thanks"
        );
        assert!(!review.read_with(cx, |review, _| review.reply_sending(root)));
    }

    #[gpui::test]
    fn resolve_folds_a_sent_thread_and_reopen_brings_it_back(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        thread_three(&workspace, cx);
        let id = CommentId(3);
        let resolve_there = cx.debug_bounds("comment-resolve-3").is_some();

        click("comment-resolve-3", cx);
        let resolved = comment_state(&workspace, id, cx);
        let folded = cx.debug_bounds("comment-folded-3").is_some();
        click("comment-folded-3", cx);
        click("comment-resolve-3", cx);

        assert!(resolve_there);
        assert_eq!(resolved, swarm_protocol::CommentState::Resolved);
        assert!(folded, "a resolved thread folds");
        assert_eq!(
            comment_state(&workspace, id, cx),
            swarm_protocol::CommentState::Sent,
            "Reopen sets the thread back to sent"
        );
    }

    #[gpui::test]
    fn only_a_pending_thread_offers_delete(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        thread_three(&workspace, cx);
        let sent = (
            cx.debug_bounds("comment-delete-3").is_some(),
            cx.debug_bounds("comment-resolve-3").is_some(),
        );

        change_comment(
            &workspace,
            CommentId(3),
            |comment| {
                comment.state = swarm_protocol::CommentState::Pending;
                comment.sent_at = None;
                comment.delivered_at = None;
            },
            cx,
        );
        let pending = (
            cx.debug_bounds("comment-delete-3").is_some(),
            cx.debug_bounds("comment-resolve-3").is_some(),
        );

        assert_eq!(sent, (false, true));
        assert_eq!(pending, (true, false));
    }

    #[gpui::test]
    fn a_delivery_event_changes_the_thread_from_in_flight_to_delivered(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = thread_three(&workspace, cx);
        let tag = |cx: &VisualTestContext| {
            review.read_with(cx, |review, _| review.state_text_for_test(CommentId(3)))
        };

        change_comment(&workspace, CommentId(3), |c| c.delivered_at = None, cx);
        let in_flight = tag(cx);
        change_comment(
            &workspace,
            CommentId(3),
            |c| c.delivered_at = c.sent_at.map(|at| at + 12_000),
            cx,
        );
        let delivered = tag(cx);

        assert_eq!(in_flight.as_deref(), Some("In flight"));
        assert!(
            delivered.is_some_and(|text| text.starts_with("Delivered ") && text.ends_with(" ago")),
        );
    }

    /// The accessibility name and state of an element.
    fn a11y_of(element: impl IntoElement) -> (Option<String>, Option<bool>) {
        use gpui::Element as _;

        let element = element.into_element();
        let mut node = gpui::accesskit::Node::new(Role::Unknown);

        element.write_a11y_info(&mut node);

        (node.label().map(str::to_owned), node.is_selected())
    }

    #[gpui::test]
    fn the_reply_button_does_not_claim_to_be_a_toggle(cx: &mut TestAppContext) {
        use gpui::{Element as _, RenderOnce as _};

        let (workspace, cx) = setup(cx);
        let review = thread_three(&workspace, cx);
        let root = CommentId(3);
        let handle = review.update(cx, |review, cx| review.reply_button_focus(root, cx));
        click("comment-reply-open-3", cx);
        assert!(review.read_with(cx, |review, _| review.reply_is_open(root)));

        let (label, toggled) = cx.update(|window, cx| {
            let button = comments::reply_open_button(root, &handle, &review);
            let element = button.render(window, cx).into_element();
            let mut node = gpui::accesskit::Node::new(Role::Unknown);

            element.write_a11y_info(&mut node);

            (node.label().map(str::to_owned), node.toggled())
        });

        assert_eq!(label.as_deref(), Some("Reply"));
        assert_eq!(toggled, None);
    }

    #[gpui::test]
    fn the_cursor_line_is_selected_and_named_for_a_screen_reader(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        let colors = cx.update(|_, cx| cx.theme().colors().clone());
        let line = DiffLine {
            kind: DiffLineKind::Added,
            text: "let cell = grid.cell(row, column);".into(),
        };
        let row = |cursor: Option<DiffSide>| {
            let ctx = LineCtx {
                view: review.clone(),
                file: 0,
                hunk: 0,
                line: 0,
                cursor,
            };

            unified_row(0, Some(&line), (0, 85), &colors, &ctx).map(a11y_of)
        };

        let on = row(Some(DiffSide::New));
        let off = row(None);

        assert_eq!(
            on,
            Some((
                Some("Line 85, added, let cell = grid.cell(row, column);".to_owned()),
                Some(true)
            ))
        );
        assert_eq!(
            off,
            Some((
                Some("Line 85, added, let cell = grid.cell(row, column);".to_owned()),
                Some(false)
            ))
        );
    }

    #[gpui::test]
    fn the_cursor_half_of_a_split_row_is_the_selected_one(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);
        review.update(cx, |review, cx| review.set_style(DiffStyle::Split, cx));
        redraw(cx);
        let colors = cx.update(|_, cx| cx.theme().colors().clone());
        let diff = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .task_diff(TaskId(0), ReviewSource::Live)
                .cloned()
        });
        let diff = diff.expect("the sample has a Live diff");
        let (hunk_index, hunk) = diff.files[0]
            .hunks
            .iter()
            .enumerate()
            .find(|(_, hunk)| {
                hunk.lines
                    .iter()
                    .any(|line| line.kind == DiffLineKind::Context)
            })
            .expect("a hunk with a context line");
        let at = hunk
            .lines
            .iter()
            .position(|line| line.kind == DiffLineKind::Context)
            .unwrap_or(0);
        let numbers = crate::review_model::hunk_numbers(hunk)[at];
        let side = |number: u32| Side {
            line: u32::try_from(at).unwrap_or(0),
            number,
        };
        let hunk_at = u32::try_from(hunk_index).unwrap_or(0);
        let ctx = |cursor: Option<DiffSide>| LineCtx {
            view: review.clone(),
            file: 0,
            hunk: hunk_at,
            line: u32::try_from(at).unwrap_or(0),
            cursor,
        };
        let read = |cursor: Option<DiffSide>| {
            let (left, right) = (ctx(cursor), ctx(cursor));

            a11y_of(split_line(
                0,
                &diff,
                (0, hunk_at),
                (
                    (Some(side(numbers.0)), &left),
                    (Some(side(numbers.1)), &right),
                ),
                &colors,
            ))
        };

        let (new_label, new_selected) = read(Some(DiffSide::New));
        let (old_label, old_selected) = read(Some(DiffSide::Old));
        let (none_label, none_selected) = read(None);

        assert_eq!(new_selected, Some(true));
        assert_eq!(old_selected, Some(true));
        assert_eq!(none_selected, Some(false));
        assert!(
            new_label
                .is_some_and(|label| label.starts_with(&format!("Line {}, unchanged", numbers.1)))
        );
        assert!(
            old_label
                .is_some_and(|label| label.starts_with(&format!("Line {}, unchanged", numbers.0)))
        );
        assert!(none_label.is_some_and(|label| label.starts_with("Old line")));
    }

    #[test]
    fn the_pill_counts_agents() {
        assert_eq!(pill_label(0), "All agents done");
        assert_eq!(pill_label(1), "1 agent working");
        assert_eq!(pill_label(2), "2 agents working");
    }

    #[test]
    fn a_long_error_is_cut_short() {
        assert_eq!(short("one\ntwo"), "one");
        assert!(short(&"x".repeat(200)).chars().count() <= 81);
    }
}
