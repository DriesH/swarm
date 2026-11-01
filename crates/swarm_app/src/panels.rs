//! The right panel: changes, files and pull request.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, Entity, FocusHandle, FontWeight, Role, ScrollStrategy,
    SharedString, UniformListScrollHandle, WeakEntity, Window, div, px, uniform_list,
};
use swarm_protocol::{ReviewSource, TaskDiff};
use swarm_ui::prelude::*;

use crate::actions::{
    FILE_TREE_CONTEXT, TreeActivate, TreeCollapse, TreeExpand, TreeNext, TreePrevious,
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
                .child(Label::new("Changes").weight(FontWeight::MEDIUM))
                .child(mono(plural(stat.files, "file", "files")).color(Color::Faint))
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
                .child(Label::new("No changes").color(Color::Faint))
                .into_any_element();
        }

        let rows = uniform_list(
            "changes-files",
            self.tree.len(),
            cx.processor(|this, range: Range<usize>, window, cx| this.rows(range, window, cx)),
        )
        .track_scroll(&self.scroll)
        .size_full();

        div()
            .id("changes-tree")
            .debug_selector(|| "changes-tree".to_owned())
            .role(Role::Tree)
            .aria_label("Changed files")
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

fn files(workspace: &Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let colors = cx.theme().colors();
    let (line, raised) = (colors.line, colors.raised);

    let tree = &workspace.data.file_tree;
    let first_modified = tree
        .iter()
        .position(|entry| !entry.is_dir && entry.mark == Some(FileMark::Modified))
        .unwrap_or(0);
    let selected_state: Entity<usize> =
        window.use_keyed_state("files-selected", cx, |_, _| first_modified);
    let selected = *selected_state.read(cx);

    let mut list = v_flex()
        .id("file-tree")
        .role(Role::Tree)
        .aria_label("Files")
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .px(px(4.))
        .py(px(4.));

    for index in visible_rows(tree) {
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

        let row = TreeRow::new(("file", index), name)
            .level(usize::from(entry.depth))
            .label_color(name_color)
            .toggle_state(is_selected)
            .focusable(true)
            .on_click(cx.listener(move |this, _, _, cx| {
                if is_dir {
                    if let Some(entry) = this.data.file_tree.get_mut(index) {
                        entry.expanded = !entry.expanded;
                    }
                } else {
                    state.update(cx, |selected, _| *selected = index);
                }
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

    let filter = h_flex()
        .flex_none()
        .mt(px(8.))
        .mx(px(10.))
        .mb(px(4.))
        .h(px(30.))
        .px(px(10.))
        .gap(px(8.))
        .rounded(px(7.))
        .border_1()
        .border_color(line)
        .bg(raised)
        .child(
            Icon::new(IconName::Search)
                .size(IconSize::Custom(px(13.)))
                .color(Color::Faint),
        )
        .child(
            Label::new("Filter files")
                .size(LabelSize::Small)
                .color(Color::Faint),
        );

    v_flex()
        .size_full()
        .child(
            header(cx)
                .h(px(40.))
                .child(Label::new("Files").weight(FontWeight::MEDIUM))
                .child(mono(workspace.active_task().name.clone()).color(Color::Faint)),
        )
        .child(filter)
        .child(list)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

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

    #[gpui::test]
    fn the_review_button_opens_the_review_view(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        click("review-button", cx);

        let view = workspace.read_with(cx, |workspace, _| workspace.view);
        assert_eq!(view, View::Review);
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
