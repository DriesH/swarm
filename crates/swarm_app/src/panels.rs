//! The right panel: changes, files and pull request.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    Action, AnyElement, ClickEvent, Context, ElementId, Entity, FocusHandle, FontWeight, Role,
    ScrollHandle, ScrollStrategy, SharedString, Subscription, UniformListScrollHandle, WeakEntity,
    Window, div, px, uniform_list,
};
use swarm_protocol::{ReviewSource, TaskDiff};
use swarm_ui::a11y::accessible;
use swarm_ui::prelude::*;

use crate::actions::{
    FILE_TREE_CONTEXT, TreeActivate, TreeCollapse, TreeExpand, TreeFirst, TreeLast, TreeNext,
    TreePrevious,
};
use crate::file_tree::{self, Entry, EntryKind, Key, Mark, Move};
use crate::model::{FileMark, TaskId, TreeEntry};
use crate::review_model::{AgentLabel, plural, visible_files};
use crate::swarm_view::full_size;
use crate::workspace::{RightPanel, View, Workspace};

pub(crate) fn render(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement + use<> {
    match workspace.visible_panel() {
        Some(RightPanel::Changes) => changes(workspace, cx),
        Some(RightPanel::Files) => files(workspace, window, cx),
        Some(RightPanel::PullRequest) => crate::pull_request_panel::render(workspace, cx),
        None => div().into_any_element(),
    }
}

/// An empty state that a screen reader speaks when it shows.
pub(crate) fn empty_status(id: &'static str, text: &'static str) -> LiveRegion {
    LiveRegion::status(id, text).child(Label::new(text).color(Color::Faint))
}

/// A failure that a screen reader speaks at once, cut to one line on screen.
pub(crate) fn error_alert(
    id: impl Into<ElementId>,
    message: impl Into<SharedString>,
) -> LiveRegion {
    let message = message.into();

    LiveRegion::alert(id, message.clone()).child(
        Label::new(message)
            .size(LabelSize::Small)
            .color(Color::Del)
            .truncate(),
    )
}

pub(crate) fn mono(text: impl Into<SharedString>) -> Label {
    Label::new(text).mono().size(LabelSize::XSmall)
}

pub(crate) fn header(cx: &App) -> gpui::Div {
    let line = cx.theme().colors().line;

    h_flex()
        .flex_none()
        .gap(px(8.))
        .pl(px(14.))
        .pr(px(10.))
        .border_b_1()
        .border_color(line)
}

fn delta(additions: u32, deletions: u32) -> impl IntoElement {
    h_flex()
        .flex_none()
        .gap(px(4.))
        .child(mono(format!("+{additions}")).color(Color::Add))
        .child(mono(format!("−{deletions}")).color(Color::Del))
}

pub(crate) fn pill(text: SharedString, cx: &App) -> impl IntoElement {
    let colors = cx.theme().colors();

    div()
        .flex_none()
        .px(px(7.))
        .py(px(2.))
        .rounded(px(5.))
        .bg(colors.soft)
        .child(mono(text))
}

fn changes(workspace: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let stat = workspace.data.live_stat(workspace.active_task);

    v_flex()
        .size_full()
        .child(
            header(cx)
                .h(px(40.))
                .child(
                    Label::new("Changes")
                        .weight(FontWeight::MEDIUM)
                        .heading("changes-heading", 2),
                )
                .child(
                    mono(plural(stat.files, "file", "files"))
                        .color(Color::Faint)
                        .accessible("changes-count"),
                )
                .child(div().flex_1())
                .child(
                    Button::new("review", "Review")
                        .variant(ButtonVariant::Primary)
                        .debug_selector(|| "review-button".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.set_view(View::Review, cx);
                        })),
                ),
        )
        .child(
            div()
                .id("changes-list")
                .flex_1()
                .min_h_0()
                .child(workspace.changes_list.clone().cached(full_size())),
        )
        .into_any_element()
}

/// The files of the open task's Live diff, as a folder tree. It owns its list, so a scroll
/// does not redraw the workspace.
pub(crate) struct ChangesList {
    workspace: WeakEntity<Workspace>,
    task: Option<TaskId>,
    diff: Option<Rc<TaskDiff>>,
    agents: Rc<[AgentLabel]>,
    tree: Rc<[Entry]>,
    /// The folders the user closed, by folder path, for each task.
    closed: HashMap<TaskId, HashSet<String>>,
    cursor: Option<Mark>,
    scroll: UniformListScrollHandle,
    /// Focus of the tree, which is one stop for Tab.
    focus_handle: FocusHandle,
}

impl ChangesList {
    pub(crate) fn new(workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        Self {
            workspace,
            task: None,
            diff: None,
            agents: Rc::from([]),
            tree: Rc::from([]),
            closed: HashMap::new(),
            cursor: None,
            scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle().tab_stop(true),
        }
    }

    pub(crate) fn show(
        &mut self,
        task: TaskId,
        diff: Option<Rc<TaskDiff>>,
        agents: Rc<[AgentLabel]>,
        cx: &mut Context<Self>,
    ) {
        if self.task == Some(task) && self.diff == diff && self.agents == agents {
            return;
        }

        if self.task != Some(task) {
            self.cursor = None;
        }

        self.task = Some(task);
        self.diff = diff;
        self.agents = agents;
        self.rebuild();
        cx.notify();
    }

    fn rebuild(&mut self) {
        let (Some(task), Some(diff)) = (self.task, &self.diff) else {
            self.tree = Rc::from([]);
            return;
        };
        let closed = self.closed.get(&task).cloned().unwrap_or_default();
        let visible = visible_files(diff, None);

        self.tree = file_tree::build(&diff.files, &visible, &self.agents, &closed).into();
    }

    /// The row of the cursor. A tree whose cursor was never moved starts on its first row.
    fn cursor_row(&self) -> Option<usize> {
        let diff = self.diff.as_ref()?;
        let moved = self
            .cursor
            .as_ref()
            .and_then(|mark| file_tree::row_of(&self.tree, &diff.files, mark));

        moved.or_else(|| (!self.tree.is_empty()).then_some(0))
    }

    /// The row that shows as selected. A cursor nobody moved shows only while the tree has
    /// focus.
    fn shown_cursor(&self, focused: bool) -> Option<usize> {
        self.cursor_row()
            .filter(|_| focused || self.cursor.is_some())
    }

    fn put_cursor(&mut self, row: usize, cx: &mut Context<Self>) {
        let (Some(diff), Some(entry)) = (&self.diff, self.tree.get(row)) else {
            return;
        };

        self.cursor = file_tree::mark_of(entry, &diff.files);
        self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn set_folder_open(&mut self, row: usize, open: bool, cx: &mut Context<Self>) {
        let (Some(task), Some(entry)) = (self.task, self.tree.get(row)) else {
            return;
        };
        let folder = entry.folder.to_string();
        let closed = self.closed.entry(task).or_default();

        if open {
            closed.remove(&folder);
        } else {
            closed.insert(folder.clone());
        }

        self.cursor = Some(Mark::Folder(folder));
        self.rebuild();
        cx.notify();
    }

    fn move_cursor(&mut self, key: Key, cx: &mut Context<Self>) {
        match file_tree::step(&self.tree, self.cursor_row(), key) {
            Move::Select(row) => self.put_cursor(row, cx),
            Move::Open(row) => self.set_folder_open(row, true, cx),
            Move::Close(row) => self.set_folder_open(row, false, cx),
            Move::Stay => {}
        }
    }

    /// Opens Review at a file, or opens or closes a folder.
    fn activate_row(&mut self, row: usize, cx: &mut Context<Self>) {
        let (Some(diff), Some(entry)) = (self.diff.clone(), self.tree.get(row)) else {
            return;
        };

        match entry.kind {
            EntryKind::Folder { open } => self.set_folder_open(row, !open, cx),
            EntryKind::File { file } => {
                let Some(data) = diff.files.get(file) else {
                    return;
                };
                let (worktree, path) = (data.worktree.clone(), data.path.clone());

                self.cursor = file_tree::mark_of(entry, &diff.files);

                let opened = self.workspace.update(cx, |workspace, cx| {
                    workspace.open_changed_file(worktree, path, cx);
                });

                if let Err(error) = opened {
                    log::debug!("the window closed before the file opened: {error}");
                }
            }
        }
    }

    /// Closes the benches folder and puts the cursor on theme.rs, for a snapshot.
    #[cfg(feature = "snapshot")]
    pub(crate) fn mark_for_snapshot(&mut self, cx: &mut Context<Self>) {
        let folder = self.tree.iter().position(|entry| {
            matches!(entry.kind, EntryKind::Folder { .. }) && entry.folder == "benches"
        });

        if let Some(row) = folder {
            self.set_folder_open(row, false, cx);
        }

        if let Some(row) = self.tree.iter().position(|entry| entry.name == "theme.rs") {
            self.put_cursor(row, cx);
        }
    }

    #[cfg(test)]
    pub(crate) fn tree_rows(&self) -> &[Entry] {
        &self.tree
    }

    #[cfg(test)]
    pub(crate) fn file_count(&self) -> usize {
        self.tree
            .iter()
            .filter(|entry| matches!(entry.kind, EntryKind::File { .. }))
            .count()
    }

    #[cfg(test)]
    pub(crate) fn put_cursor_for_test(&mut self, row: usize, cx: &mut Context<Self>) {
        self.put_cursor(row, cx);
    }

    #[cfg(test)]
    pub(crate) fn cursor_at(&self) -> Option<usize> {
        self.cursor_row()
    }

    #[cfg(test)]
    pub(crate) fn tree_focus(&self) -> &FocusHandle {
        &self.focus_handle
    }

    fn rows(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let tree = Rc::clone(&self.tree);
        let Some(diff) = self.diff.clone() else {
            return Vec::new();
        };
        let focused = self.focus_handle.is_focused(window);
        let cursor = self.shown_cursor(focused);
        let ring = focused && window.last_input_was_keyboard();

        range
            .filter_map(|index| tree.get(index).map(|entry| (index, entry)))
            .map(|(index, entry)| {
                Self::row(index, entry, &diff, (cursor == Some(index), ring), cx)
                    .map_or_else(|| div().into_any_element(), IntoElement::into_any_element)
            })
            .collect()
    }

    fn row(
        index: usize,
        entry: &Entry,
        diff: &TaskDiff,
        (selected, ring): (bool, bool),
        cx: &mut Context<Self>,
    ) -> Option<TreeRow> {
        let row = match entry.kind {
            EntryKind::Folder { open } => TreeRow::new(("change", index), entry.name.clone())
                .debug_selector(|| format!("change-folder-{index}"))
                .level(entry.level)
                .position_in_set(entry.position_in_set, entry.size_of_set)
                .folder(open)
                .file_icon(entry.file_icon())
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.focus_handle.focus(window, cx);
                    this.set_folder_open(index, !open, cx);
                })),
            EntryKind::File { file } => {
                let data = diff.files.get(file)?;
                let summary = format!(
                    "{}, +{} \u{2212}{}{}",
                    data.path.display(),
                    data.additions,
                    data.deletions,
                    entry
                        .agent
                        .as_ref()
                        .map_or_else(String::new, |agent| format!(", {agent}"))
                );
                let end = h_flex()
                    .flex_none()
                    .gap(px(8.))
                    .children(
                        entry
                            .agent
                            .clone()
                            .map(|agent| mono(agent).color(Color::Muted).truncate()),
                    )
                    .child(delta(data.additions, data.deletions));

                TreeRow::new(("change", index), entry.name.clone())
                    .debug_selector(|| format!("change-file-{file}"))
                    .accessible_label(summary)
                    .file_icon(entry.file_icon())
                    .level(entry.level)
                    .position_in_set(entry.position_in_set, entry.size_of_set)
                    .end_slot(end)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.focus_handle.focus(window, cx);
                        this.activate_row(index, cx);
                    }))
            }
        };

        Some(row.toggle_state(selected).ring(selected && ring))
    }

    #[cfg(test)]
    pub(crate) fn tree_row(
        &self,
        index: usize,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> Option<TreeRow> {
        let (entry, diff) = (self.tree.get(index)?, self.diff.clone()?);

        Self::row(
            index,
            entry,
            &diff,
            (self.shown_cursor(focused) == Some(index), false),
            cx,
        )
    }
}

impl Render for ChangesList {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.tree.is_empty() {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(empty_status("changes-empty", "No changes"))
                .into_any_element();
        }

        let rows = uniform_list(
            "changes-files",
            self.tree.len(),
            cx.processor(|this, range: Range<usize>, window, cx| this.rows(range, window, cx)),
        )
        .track_scroll(&self.scroll)
        .size_full();

        accessible(
            div()
                .id("changes-tree")
                .debug_selector(|| "changes-tree".to_owned()),
            Role::Tree,
            "Changed files",
        )
        .key_context(FILE_TREE_CONTEXT)
        .track_focus(&self.focus_handle)
        .on_action(cx.listener(|this, _: &TreePrevious, _, cx| this.move_cursor(Key::Up, cx)))
        .on_action(cx.listener(|this, _: &TreeNext, _, cx| this.move_cursor(Key::Down, cx)))
        .on_action(cx.listener(|this, _: &TreeCollapse, _, cx| {
            this.move_cursor(Key::Left, cx);
        }))
        .on_action(cx.listener(|this, _: &TreeExpand, _, cx| {
            this.move_cursor(Key::Right, cx);
        }))
        .on_action(cx.listener(|this, _: &TreeFirst, _, cx| this.move_cursor(Key::First, cx)))
        .on_action(cx.listener(|this, _: &TreeLast, _, cx| this.move_cursor(Key::Last, cx)))
        .on_action(cx.listener(|this, _: &TreeActivate, _, cx| {
            if let Some(row) = this.cursor_row() {
                this.activate_row(row, cx);
            }
        }))
        .size_full()
        .px(px(4.))
        .child(rows)
        .into_any_element()
    }
}

impl Workspace {
    /// Hands the Changes panel the open task's Live diff.
    pub(crate) fn sync_changes_list(&mut self, cx: &mut Context<Self>) {
        if self.visible_panel() != Some(RightPanel::Changes) {
            return;
        }

        let task = self.active_task;
        let diff = self.data.task_diff(task, ReviewSource::Live).cloned();
        let agents: Rc<[AgentLabel]> = self
            .data
            .agents_of(task)
            .map(|agent| AgentLabel {
                id: agent.protocol_id(),
                name: agent.name.clone(),
                kind: agent.kind,
            })
            .collect();

        self.changes_list
            .update(cx, |list, cx| list.show(task, diff, agents, cx));
    }
}

fn visible_rows(tree: &[TreeEntry]) -> Vec<usize> {
    let mut rows = Vec::new();
    let mut hidden_below: Option<u8> = None;

    for (index, entry) in tree.iter().enumerate() {
        if hidden_below.is_some_and(|depth| entry.depth > depth) {
            continue;
        }

        hidden_below = (entry.is_dir && !entry.expanded).then_some(entry.depth);
        rows.push(index);
    }

    rows
}

/// Rows to show for a filter. A match keeps the folders above it, open or not.
fn matching_rows(tree: &[TreeEntry], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return visible_rows(tree);
    }

    let mut shown = vec![false; tree.len()];

    for (index, entry) in tree.iter().enumerate() {
        if entry.is_dir || !entry.path.to_lowercase().contains(&query) {
            continue;
        }

        if let Some(slot) = shown.get_mut(index) {
            *slot = true;
        }

        let mut depth = entry.depth;
        for (above, parent) in tree.iter().enumerate().take(index).rev() {
            if depth == 0 {
                break;
            }
            if parent.is_dir && parent.depth < depth {
                depth = parent.depth;
                if let Some(slot) = shown.get_mut(above) {
                    *slot = true;
                }
            }
        }
    }

    (0..tree.len())
        .filter(|index| shown.get(*index) == Some(&true))
        .collect()
}

/// The row's spoken name: the mark is a letter and a color on screen.
fn row_label(name: &SharedString, mark: Option<FileMark>) -> SharedString {
    match mark {
        Some(FileMark::Modified) => format!("{name}, Modified").into(),
        Some(FileMark::Added) => format!("{name}, Added").into(),
        None => name.clone(),
    }
}

/// A key of the Files tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FilesKey {
    Up,
    Down,
    First,
    Last,
    Left,
    Right,
}

/// What a key does in the Files tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FilesMove {
    /// Puts the selection on this index of the tree.
    Select(usize),
    /// Opens or closes the folder at this index of the tree.
    Fold(usize, bool),
    Stay,
}

/// Works out a key of the Files tree over `rows`, the indexes of the shown entries.
/// `selected` is the index of the selected entry, which a filter may hide.
fn files_step(tree: &[TreeEntry], rows: &[usize], selected: usize, key: FilesKey) -> FilesMove {
    let (Some(first), Some(last)) = (rows.first().copied(), rows.last().copied()) else {
        return FilesMove::Stay;
    };
    let position = rows.iter().position(|row| *row == selected);
    let entry = position.and_then(|_| tree.get(selected));

    match (key, position) {
        (FilesKey::First | FilesKey::Down, None) | (FilesKey::First, _) => FilesMove::Select(first),
        (FilesKey::Last | FilesKey::Up, None) | (FilesKey::Last, _) => FilesMove::Select(last),
        (FilesKey::Down, Some(at)) => FilesMove::Select(rows.get(at + 1).copied().unwrap_or(last)),
        (FilesKey::Up, Some(at)) => {
            FilesMove::Select(rows.get(at.saturating_sub(1)).copied().unwrap_or(first))
        }
        (FilesKey::Left, Some(at)) => match entry {
            Some(entry) if entry.is_dir && entry.expanded => FilesMove::Fold(selected, false),
            Some(entry) => rows[..at]
                .iter()
                .rev()
                .find(|row| {
                    tree.get(**row)
                        .is_some_and(|parent| parent.is_dir && parent.depth < entry.depth)
                })
                .map_or(FilesMove::Stay, |parent| FilesMove::Select(*parent)),
            None => FilesMove::Stay,
        },
        (FilesKey::Right, Some(at)) => match entry {
            Some(entry) if entry.is_dir && !entry.expanded => FilesMove::Fold(selected, true),
            Some(entry) if entry.is_dir => rows
                .get(at + 1)
                .map_or(FilesMove::Stay, |next| FilesMove::Select(*next)),
            _ => FilesMove::Stay,
        },
        (FilesKey::Left | FilesKey::Right, None) => FilesMove::Stay,
    }
}

/// What the Files tree needs to handle a key.
#[derive(Clone)]
struct FilesTree {
    selected: Entity<usize>,
    filter: Entity<TextInput>,
    scroll: ScrollHandle,
}

impl FilesTree {
    fn press(&self, key: FilesKey, workspace: &mut Workspace, cx: &mut Context<Workspace>) {
        let query = self.filter.read(cx).text();
        let rows = matching_rows(&workspace.data.file_tree, &query);
        let selected = *self.selected.read(cx);

        match files_step(&workspace.data.file_tree, &rows, selected, key) {
            FilesMove::Select(index) => {
                self.select(index, &rows);
                self.selected.update(cx, |selected, _| *selected = index);
            }
            FilesMove::Fold(index, open) => {
                if let Some(entry) = workspace.data.file_tree.get_mut(index) {
                    entry.expanded = open;
                }
            }
            FilesMove::Stay => {}
        }

        cx.notify();
    }

    /// Enter opens or closes a folder. A file stays selected.
    fn activate(&self, workspace: &mut Workspace, cx: &mut Context<Workspace>) {
        let selected = *self.selected.read(cx);

        if let Some(entry) = workspace.data.file_tree.get_mut(selected)
            && entry.is_dir
        {
            entry.expanded = !entry.expanded;
        }

        cx.notify();
    }

    fn select(&self, index: usize, rows: &[usize]) {
        if let Some(position) = rows.iter().position(|row| *row == index) {
            self.scroll.scroll_to_item(position);
        }
    }

    fn listener<A: Action>(
        &self,
        key: FilesKey,
        cx: &mut Context<Workspace>,
    ) -> impl Fn(&A, &mut Window, &mut App) + 'static {
        let tree = self.clone();

        cx.listener(move |workspace, _: &A, _, cx| tree.press(key, workspace, cx))
    }
}

/// The search field above the file tree.
struct FilesFilter {
    input: Entity<TextInput>,
    _subscription: Subscription,
}

impl FilesFilter {
    fn new(workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            let mut input = TextInput::new(cx);
            input.set_label("Filter files", cx);
            input.set_placeholder("Filter files", cx);
            input
        });

        let subscription = cx.subscribe(&input, move |_, _, event, cx| {
            if matches!(event, TextInputEvent::Changed(_))
                && let Err(error) = workspace.update(cx, |_, cx| cx.notify())
            {
                log::debug!("the window closed before the file filter changed: {error}");
            }
        });

        Self {
            input,
            _subscription: subscription,
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one flat element tree reads best in one place"
)]
fn files(workspace: &Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let tree = &workspace.data.file_tree;
    let first_modified = tree
        .iter()
        .position(|entry| !entry.is_dir && entry.mark == Some(FileMark::Modified))
        .unwrap_or(0);
    let selected_state: Entity<usize> =
        window.use_keyed_state("files-selected", cx, |_, _| first_modified);
    let selected = *selected_state.read(cx);

    let weak = cx.entity().downgrade();
    let filter_state: Entity<FilesFilter> =
        window.use_keyed_state("files-filter", cx, |_, cx| FilesFilter::new(weak, cx));
    let input = filter_state.read(cx).input.clone();
    let query = input.read(cx).text();

    let focus: Entity<FocusHandle> =
        window.use_keyed_state("files-focus", cx, |_, cx| cx.focus_handle().tab_stop(true));
    let focus = focus.read(cx).clone();
    let scroll: Entity<ScrollHandle> =
        window.use_keyed_state("files-scroll", cx, |_, _| ScrollHandle::new());
    let keys = FilesTree {
        selected: selected_state.clone(),
        filter: input.clone(),
        scroll: scroll.read(cx).clone(),
    };
    let ring = focus.is_focused(window) && window.last_input_was_keyboard();

    let mut list = accessible(v_flex().id("file-tree"), Role::Tree, "Files")
        .key_context(FILE_TREE_CONTEXT)
        .track_focus(&focus)
        .on_action(keys.listener::<TreePrevious>(FilesKey::Up, cx))
        .on_action(keys.listener::<TreeNext>(FilesKey::Down, cx))
        .on_action(keys.listener::<TreeFirst>(FilesKey::First, cx))
        .on_action(keys.listener::<TreeLast>(FilesKey::Last, cx))
        .on_action(keys.listener::<TreeCollapse>(FilesKey::Left, cx))
        .on_action(keys.listener::<TreeExpand>(FilesKey::Right, cx))
        .on_action({
            let keys = keys.clone();

            cx.listener(move |workspace, _: &TreeActivate, _, cx| keys.activate(workspace, cx))
        })
        .track_scroll(&keys.scroll)
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(4.))
        .py(px(4.));

    for index in matching_rows(tree, &query) {
        let Some(entry) = tree.get(index) else {
            continue;
        };

        let name = entry
            .path
            .rsplit_once('/')
            .map_or(entry.path.clone(), |(_, name)| SharedString::new(name));
        let is_selected = index == selected;
        let is_dir = entry.is_dir;
        let state = selected_state.clone();
        let row_focus = focus.clone();

        let name_color = if is_selected {
            Color::Accent
        } else if is_dir {
            Color::Default
        } else {
            match entry.mark {
                Some(FileMark::Added) => Color::Add,
                Some(FileMark::Modified) => Color::Default,
                None => Color::Muted,
            }
        };

        let file_icon = if is_dir {
            FileIconName::for_folder(&name, entry.expanded)
        } else {
            FileIconName::for_file(&name)
        };

        let label = row_label(&name, entry.mark);
        let row = TreeRow::new(("file", index), name)
            .accessible_label(label)
            .debug_selector(|| format!("files-row-{index}"))
            .file_icon(file_icon)
            .level(usize::from(entry.depth))
            .label_color(name_color)
            .toggle_state(is_selected)
            .ring(is_selected && ring)
            .on_click(cx.listener(move |this, _, window, cx| {
                window.focus(&row_focus, cx);

                if is_dir && let Some(entry) = this.data.file_tree.get_mut(index) {
                    entry.expanded = !entry.expanded;
                }

                state.update(cx, |selected, _| *selected = index);
                cx.notify();
            }));
        let row = if is_dir {
            row.folder(entry.expanded)
        } else {
            row
        };

        list = list.child(match entry.mark {
            Some(mark) => {
                let (letter, color) = match mark {
                    FileMark::Modified => ("M", Color::Warn),
                    FileMark::Added => ("A", Color::Add),
                };

                row.end_slot(mono(letter).weight(FontWeight::SEMIBOLD).color(color))
            }
            None => row,
        });
    }

    let filter = div()
        .flex_none()
        .mt(px(8.))
        .mx(px(10.))
        .mb(px(4.))
        .child(input);

    v_flex()
        .size_full()
        .child(
            header(cx)
                .h(px(40.))
                .child(
                    Label::new("Files")
                        .weight(FontWeight::MEDIUM)
                        .heading("files-heading", 2),
                )
                .child(
                    mono(workspace.active_task().name.clone())
                        .color(Color::Faint)
                        .accessible("files-task"),
                ),
        )
        .child(filter)
        .child(list)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use gpui::{Entity, Focusable as _, Modifiers, TestAppContext, VisualTestContext};
    use swarm_ui::test_support::focused_stop;

    use super::*;
    use crate::actions;

    fn setup(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
        cx.update(|cx| {
            swarm_ui::theme::init(cx);
            actions::bind_keys(cx);
        });

        let (workspace, cx) = cx.add_window_view(Workspace::with_sample);
        cx.run_until_parked();

        (workspace, cx)
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let center = cx.debug_bounds(selector).map(|bounds| bounds.center());

        assert!(center.is_some(), "{selector} is not on screen");
        if let Some(center) = center {
            cx.simulate_click(center, Modifiers::none());
        }
        cx.run_until_parked();
    }

    fn redraw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
    }

    fn open_files(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
        workspace.update(cx, |workspace, cx| {
            workspace.toggle_panel(RightPanel::Files, cx);
        });
        redraw(cx);
    }

    fn expanded(workspace: &Entity<Workspace>, index: usize, cx: &VisualTestContext) -> bool {
        workspace.read_with(cx, |workspace, _| workspace.data.file_tree[index].expanded)
    }

    /// The role and value of an element, as the accessibility tree would hold them.
    fn text_node(element: impl IntoElement) -> (Option<Role>, Option<String>, Option<Live>) {
        use gpui::Element as _;

        let element = element.into_element();
        let mut node = gpui::accesskit::Node::new(Role::Unknown);

        element.write_a11y_info(&mut node);

        (
            element.a11y_role(),
            node.value().map(str::to_owned),
            node.live(),
        )
    }

    #[gpui::test]
    fn the_files_tree_takes_one_tab_stop_and_names_it(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        open_files(&workspace, cx);

        click("files-row-3", cx);
        let root = workspace.read_with(cx, Workspace::focus_handle);

        assert_eq!(focused_stop(cx, &root), "Tree Files");
    }

    #[gpui::test]
    fn the_changes_tree_is_a_named_tab_stop(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        redraw(cx);
        let list = workspace.read_with(cx, |workspace, _| workspace.changes_list.clone());
        let handle = list.read_with(cx, |list, _| list.tree_focus().clone());
        cx.update(|window, cx| window.focus(&handle, cx));
        redraw(cx);
        let root = workspace.read_with(cx, Workspace::focus_handle);

        assert_eq!(focused_stop(cx, &root), "Tree Changed files");
    }

    #[gpui::test]
    fn home_and_left_close_the_first_folder_of_the_files_tree(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        open_files(&workspace, cx);
        click("files-row-3", cx);
        assert!(expanded(&workspace, 0, cx));

        cx.simulate_keystrokes("home");
        cx.simulate_keystrokes("left");

        assert!(!expanded(&workspace, 0, cx));
    }

    #[gpui::test]
    fn end_then_left_then_right_opens_the_folder_above_the_last_row(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        open_files(&workspace, cx);
        click("files-row-3", cx);
        let last = workspace.read_with(cx, |workspace, _| workspace.data.file_tree.len() - 1);
        let parent = workspace.read_with(cx, |workspace, _| {
            let tree = &workspace.data.file_tree;

            (0..last)
                .rev()
                .find(|index| tree[*index].is_dir && tree[*index].depth < tree[last].depth)
        });
        let parent = parent.expect("the last row has a folder above it");
        assert!(!expanded(&workspace, parent, cx), "the sample closes it");

        cx.simulate_keystrokes("end");
        cx.simulate_keystrokes("left");
        cx.simulate_keystrokes("right");

        assert!(expanded(&workspace, parent, cx));
    }

    #[gpui::test]
    fn enter_opens_a_closed_folder_of_the_files_tree(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        open_files(&workspace, cx);
        click("files-row-14", cx);
        assert!(expanded(&workspace, 14, cx), "the click opened it");

        cx.simulate_keystrokes("enter");

        assert!(!expanded(&workspace, 14, cx));
    }

    #[test]
    fn a_files_key_with_nothing_to_show_does_nothing() {
        assert_eq!(files_step(&[], &[], 0, FilesKey::Down), FilesMove::Stay);
    }

    #[test]
    fn down_from_a_hidden_selection_lands_on_the_first_shown_row() {
        let data = crate::model::FakeData::sample();
        let rows = [4, 5];

        assert_eq!(
            files_step(&data.file_tree, &rows, 0, FilesKey::Down),
            FilesMove::Select(4)
        );
        assert_eq!(
            files_step(&data.file_tree, &rows, 0, FilesKey::Up),
            FilesMove::Select(5)
        );
    }

    #[test]
    fn right_on_an_open_folder_moves_to_its_first_row() {
        let data = crate::model::FakeData::sample();
        let rows = visible_rows(&data.file_tree);

        assert_eq!(
            files_step(&data.file_tree, &rows, 0, FilesKey::Right),
            FilesMove::Select(1)
        );
    }

    #[gpui::test]
    fn an_empty_state_is_a_status_with_its_text_as_the_value(cx: &mut TestAppContext) {
        let (_workspace, cx) = setup(cx);

        let status = cx.update(|window, cx| {
            let region = empty_status("changes-empty", "No changes").render(window, cx);

            text_node(region)
        });

        assert_eq!(
            status,
            (
                Some(Role::Status),
                Some("No changes".to_owned()),
                Some(Live::Polite)
            )
        );
    }

    #[gpui::test]
    fn the_review_button_opens_the_review_view(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        click("review-button", cx);

        let view = workspace.read_with(cx, |workspace, _| workspace.view);
        assert_eq!(view, View::Review);
    }

    #[test]
    fn a_marked_row_names_its_mark() {
        let name = SharedString::new("main.rs");

        assert_eq!(
            row_label(&name, Some(FileMark::Modified)).as_ref(),
            "main.rs, Modified"
        );
        assert_eq!(
            row_label(&name, Some(FileMark::Added)).as_ref(),
            "main.rs, Added"
        );
        assert_eq!(row_label(&name, None).as_ref(), "main.rs");
    }

    #[test]
    fn a_filter_keeps_matches_and_the_folders_above_them() {
        let data = crate::model::FakeData::sample();
        let Some(file) = data.file_tree.iter().find(|entry| !entry.is_dir) else {
            panic!("the sample tree has no file");
        };
        let query = file.path.rsplit('/').next().unwrap_or_default();

        let rows = matching_rows(&data.file_tree, query);

        assert!(rows.len() < data.file_tree.len());
        assert!(rows.iter().any(|index| {
            data.file_tree
                .get(*index)
                .is_some_and(|entry| entry.path == file.path)
        }));
    }

    #[test]
    fn an_empty_filter_shows_the_visible_rows() {
        let data = crate::model::FakeData::sample();

        assert_eq!(
            matching_rows(&data.file_tree, "  "),
            visible_rows(&data.file_tree)
        );
    }

    #[test]
    fn collapsed_directories_hide_their_children() {
        let mut data = crate::model::FakeData::sample();
        if let Some(entry) = data.file_tree.get_mut(1) {
            entry.expanded = false;
        }

        let rows = visible_rows(&data.file_tree);

        assert_eq!(rows.get(..3), Some([0, 1, 9].as_slice()));
    }
}
