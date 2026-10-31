//! The Review view: the changed files of a task and their diff.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, Context, Div, FocusHandle, FontWeight, ListAlignment, ListOffset,
    ListState, Role, ScrollStrategy, SharedString, UniformListScrollHandle, WeakEntity, Window,
    div, list, px, uniform_list,
};
use swarm_protocol::{DiffLine, DiffLineKind, FileChange, FileDiff, ReviewSource, TaskDiff};
use swarm_ui::fonts::MONO_FONT;
use swarm_ui::prelude::*;
use swarm_ui::styles::focus_ring;
use swarm_ui::{StatusPill, ThemeColors};

use crate::actions::{NextFile, PreviousFile, REVIEW_CONTEXT, ToggleViewed};
use crate::model::{AgentKind, TaskId};
use crate::review_model::{
    AgentLabel, BodyRows, DiffStyle, FileGroup, FileInfo, FileKey, GroupKind, Grouping, Note, Row,
    Side, build_rows, file_infos, group_files, plural, thousands, visible_files, worktree_count,
};
use crate::workspace::{View, Workspace};

const NAV_WIDTH: f32 = 272.;
const NAV_ROW: f32 = 30.;
const HEADER_HEIGHT: f32 = 46.;
const CARD_HEADER: f32 = 38.;
const CARD_GAP: f32 = 14.;
const LINE_HEIGHT: f32 = 21.;
const NUMBER_WIDTH: f32 = 40.;
const SIGN_WIDTH: f32 = 14.;
const SOURCES: [ReviewSource; 2] = [ReviewSource::Live, ReviewSource::Merged];
const STYLES: [DiffStyle; 2] = [DiffStyle::Unified, DiffStyle::Split];
const GROUPINGS: [Grouping; 2] = [Grouping::ByAgent, Grouping::ByFolder];

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
    pub approving: bool,
    pub approve_error: Option<SharedString>,
    pub jump: Option<Jump>,
}

impl ReviewInput {
    /// True when the files and rows must be worked out again.
    fn changes_files(&self, other: &Self) -> bool {
        self.task != other.task
            || self.source != other.source
            || self.diff != other.diff
            || self.filter != other.filter
            || self.agents != other.agents
    }
}

#[derive(Clone, Debug, PartialEq)]
enum NavItem {
    Group {
        title: SharedString,
        kind: GroupKind,
        files: usize,
    },
    File {
        file: usize,
    },
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
    pub(crate) grouping: Grouping,
    selected: HashMap<TaskId, FileKey>,
    /// The hash each viewed file had when it was marked.
    viewed: HashMap<TaskId, HashMap<FileKey, u64>>,
    collapsed: HashMap<TaskId, HashSet<FileKey>>,
    handled_jump: u64,
    infos: Rc<[FileInfo]>,
    shown: Rc<[usize]>,
    body: Rc<BodyRows>,
    nav: Rc<[NavItem]>,
    body_list: ListState,
    nav_scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
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

impl ReviewView {
    pub(crate) fn new(workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        Self {
            workspace,
            input: None,
            style: DiffStyle::default(),
            grouping: Grouping::default(),
            selected: HashMap::new(),
            viewed: HashMap::new(),
            collapsed: HashMap::new(),
            handled_jump: 0,
            infos: Rc::from([]),
            shown: Rc::from([]),
            body: Rc::default(),
            nav: Rc::from([]),
            body_list: ListState::new(0, ListAlignment::Top, px(400.)),
            nav_scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
        }
    }

    pub(crate) fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
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
            self.rebuild(same_scope);
        }

        self.apply_jump(&input);
        cx.notify();
    }

    /// Forgets the viewed mark of every file whose hunks changed, and opens its card.
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

        viewed.retain(|key, hash| {
            let current = infos.iter().find(|info| info.key == *key).map(|i| i.hash);
            let keep = current == Some(*hash);

            if !keep && current.is_some() {
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
        self.rebuild(false);
        cx.notify();
    }

    fn rebuild(&mut self, keep_scroll: bool) {
        let anchor = keep_scroll.then(|| self.top_anchor()).flatten();

        let Some(input) = self.input.clone() else {
            self.infos = Rc::from([]);
            self.shown = Rc::from([]);
            self.nav = Rc::from([]);
            self.body = Rc::default();
            self.body_list.reset(0);
            return;
        };
        let Some(diff) = input.diff.as_ref() else {
            self.infos = Rc::from([]);
            self.shown = Rc::from([]);
            self.nav = Rc::from([]);
            self.body = Rc::default();
            self.body_list.reset(0);
            return;
        };

        self.infos = file_infos(diff).into();
        self.shown = visible_files(diff, input.filter.as_ref().map(|agent| agent.id)).into();
        self.rebuild_nav();
        self.rebuild_rows(anchor);
    }

    fn rebuild_nav(&mut self) {
        let Some(input) = &self.input else {
            return;
        };
        let Some(diff) = &input.diff else {
            return;
        };

        let groups: Vec<FileGroup> =
            group_files(&diff.files, &self.shown, self.grouping, &input.agents);
        let mut items = Vec::new();

        for group in groups {
            items.push(NavItem::Group {
                title: group.title,
                kind: group.kind,
                files: group.files.len(),
            });
            items.extend(group.files.into_iter().map(|file| NavItem::File { file }));
        }

        self.nav = items.into();
    }

    fn rebuild_rows(&mut self, anchor: Option<Anchor>) {
        let Some(input) = &self.input else {
            return;
        };
        let Some(diff) = &input.diff else {
            return;
        };
        let collapsed = self.collapsed.get(&input.task);

        self.body = Rc::new(build_rows(
            diff,
            &self.infos,
            &self.shown,
            |file| {
                self.infos
                    .get(file)
                    .is_some_and(|info| collapsed.is_some_and(|set| set.contains(&info.key)))
            },
            self.style,
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

        self.select(file);
    }

    /// Selects the file and shows it at the top of the body.
    fn select(&mut self, file: usize) {
        let (Some(task), Some(info)) = (self.task(), self.infos.get(file)) else {
            return;
        };

        self.selected.insert(task, info.key.clone());
        self.scroll_to_file(file);
    }

    fn scroll_to_file(&self, file: usize) {
        if let Some(Some(row)) = self.body.headers.get(file) {
            self.body_list.scroll_to(ListOffset {
                item_ix: *row,
                offset_in_item: px(0.),
            });
        }

        let position = self
            .nav
            .iter()
            .position(|item| *item == NavItem::File { file });
        if let Some(position) = position {
            self.nav_scroll
                .scroll_to_item(position, ScrollStrategy::Nearest);
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
            .is_some_and(|hash| *hash == info.hash)
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

    #[cfg(test)]
    pub(crate) fn group_titles(&self) -> Vec<SharedString> {
        self.nav
            .iter()
            .filter_map(|item| match item {
                NavItem::Group { title, .. } => Some(title.clone()),
                NavItem::File { .. } => None,
            })
            .collect()
    }

    pub(crate) fn toggle_viewed(&mut self, file: usize, cx: &mut Context<Self>) {
        let (Some(task), Some(info)) = (self.task(), self.infos.get(file)) else {
            return;
        };
        let (key, hash) = (info.key.clone(), info.hash);
        let viewed = self.is_viewed(task, file);

        let marks = self.viewed.entry(task).or_default();
        let collapsed = self.collapsed.entry(task).or_default();

        if viewed {
            marks.remove(&key);
            collapsed.remove(&key);
        } else {
            marks.insert(key.clone(), hash);
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

    pub(crate) fn set_grouping(&mut self, grouping: Grouping, cx: &mut Context<Self>) {
        if self.grouping == grouping {
            return;
        }

        self.grouping = grouping;
        self.rebuild_nav();
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors().clone();

        let root = h_flex()
            .id("review-view")
            .key_context(REVIEW_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .on_action(cx.listener(|this, _: &NextFile, _, cx| this.next_file(cx)))
            .on_action(cx.listener(|this, _: &PreviousFile, _, cx| this.previous_file(cx)))
            .on_action(cx.listener(|this, _: &ToggleViewed, _, cx| {
                this.toggle_selected_viewed(cx);
            }));

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
        let view = cx.entity();
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

        let grouping = div().mx(px(6.)).mb(px(8.)).child(
            SegmentedControl::new("review-grouping", "Group files", ["By agent", "By folder"])
                .selected(
                    GROUPINGS
                        .iter()
                        .position(|grouping| *grouping == self.grouping)
                        .unwrap_or(0),
                )
                .on_select(move |index, _, cx| {
                    let Some(grouping) = GROUPINGS.get(index).copied() else {
                        return;
                    };

                    view.update(cx, |this, cx| this.set_grouping(grouping, cx));
                }),
        );

        let items = uniform_list(
            "review-nav-list",
            self.nav.len(),
            cx.processor(|this, range: Range<usize>, _, cx| this.nav_items(range, cx)),
        )
        .track_scroll(&self.nav_scroll)
        .size_full();

        v_flex()
            .id("review-nav")
            .role(Role::Navigation)
            .aria_label("Changed files")
            .w(px(NAV_WIDTH))
            .flex_none()
            .h_full()
            .pt(px(12.))
            .pb(px(10.))
            .px(px(8.))
            .border_r_1()
            .border_color(colors.line)
            .child(summary)
            .child(progress)
            .children(chip)
            .child(grouping)
            .child(div().flex_1().min_h_0().child(items))
            .into_any_element()
    }

    fn nav_items(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let colors = cx.theme().colors().clone();
        let Some(task) = self.task() else {
            return Vec::new();
        };
        let selected = self.selected_file();
        let nav = Rc::clone(&self.nav);
        let mut elements = Vec::new();

        for position in range {
            let Some(item) = nav.get(position) else {
                continue;
            };

            let element = match item {
                NavItem::Group { title, kind, files } => nav_group(position, title, *kind, *files),
                NavItem::File { file } => {
                    self.nav_file(position, *file, selected == Some(*file), task, &colors, cx)
                }
            };

            elements.push(element);
        }

        elements
    }

    fn nav_file(
        &self,
        position: usize,
        file: usize,
        selected: bool,
        task: TaskId,
        colors: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (Some(diff), Some(info)) = (self.diff(), self.infos.get(file)) else {
            return div().into_any_element();
        };
        let Some(data) = diff.files.get(file) else {
            return div().into_any_element();
        };
        let viewed = self.is_viewed(task, file);
        let (active, raised, accent) = (colors.active, colors.raised, colors.accent);

        let label = format!(
            "{}, {}, {}{}",
            info.path,
            plural(data.additions, "addition", "additions"),
            plural(data.deletions, "deletion", "deletions"),
            if viewed { ", viewed" } else { "" }
        );

        h_flex()
            .id(("review-nav-file", position))
            .debug_selector(|| format!("review-nav-file-{file}"))
            .role(Role::Button)
            .aria_label(label)
            .aria_selected(selected)
            .focusable()
            .tab_stop(true)
            .cursor_pointer()
            .h(px(NAV_ROW - 2.))
            .mt(px(2.))
            .pl(px(32.))
            .pr(px(8.))
            .gap(px(8.))
            .rounded(px(7.))
            .when(selected, |row| row.bg(active))
            .when(!selected, |row| row.hover(move |style| style.bg(raised)))
            .focus_visible(move |style| focus_ring(style, accent))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.select_file(file, cx);
            }))
            .child(
                div().flex_1().min_w_0().flex().child(
                    Label::new(info.name.clone())
                        .size(LabelSize::Small)
                        .color(if viewed { Color::Muted } else { Color::Default })
                        .truncate(),
                ),
            )
            .child(counts(data.additions, data.deletions, LabelSize::XSmall))
            .child(div().w(px(12.)).flex_none().when(viewed, |slot| {
                slot.child(
                    Icon::new(IconName::Check)
                        .size(IconSize::Small)
                        .color(Color::Ok),
                )
            }))
            .into_any_element()
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
        .label(pill_label(input.working));

        let summary = Self::summary(input);

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
        let approved = merged && loaded.is_some() && input.approved == loaded;
        let enabled = merged && loaded.is_some() && !approved && !input.approving;
        let workspace = self.workspace.clone();

        let approve = Button::new(
            "review-approve",
            if approved { "Approved" } else { "Approve" },
        )
        .variant(ButtonVariant::Primary)
        .debug_selector(|| "review-approve".to_owned())
        .when(approved, |button| button.icon(IconName::Check))
        .disabled(!enabled)
        .on_click(move |_, _, cx| {
            update_workspace(&workspace, cx, Workspace::approve_review);
        });

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
            .child(summary)
            .child(div().flex_1())
            .child(source)
            .child(style)
            .children(error)
            .child(approve)
            .into_any_element()
    }

    fn summary(input: &ReviewInput) -> Div {
        let mut line = h_flex().min_w_0().overflow_hidden().gap(px(4.));
        let faint = |text: String| {
            Label::new(text)
                .mono()
                .size(LabelSize::XSmall)
                .color(Color::Faint)
        };

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

        if !place.is_empty() {
            line = line.child(faint(place));
        }

        if let Some(diff) = &input.diff {
            line = line
                .child(faint(format!(
                    "\u{b7} {} \u{b7}",
                    plural(diff.stat.files, "file", "files")
                )))
                .child(counts(
                    diff.stat.additions,
                    diff.stat.deletions,
                    LabelSize::XSmall,
                ));
        }

        line
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
                unified_line(index, data, old, new, &colors)
            }
            Row::Pair {
                file,
                hunk,
                left,
                right,
            } => split_line(index, &diff, (file, hunk), (left, right), &colors),
            Row::Note { note, .. } => note_row(index, note, &colors),
            Row::End => div().into_any_element(),
        };

        card_slot(first, last, content, &colors)
    }

    fn card_header(
        &self,
        file: usize,
        diff: &TaskDiff,
        open: bool,
        colors: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (Some(task), Some(info), Some(data)) =
            (self.task(), self.infos.get(file), diff.files.get(file))
        else {
            return div().into_any_element();
        };
        let viewed = self.is_viewed(task, file);
        let agent = self.agent_of(data);
        let view = cx.entity();
        let toggle_view = view.clone();

        let summary = format!(
            "{}, {}, {}",
            info.path,
            plural(data.additions, "addition", "additions"),
            plural(data.deletions, "deletion", "deletions")
        );

        let tag = match data.change {
            FileChange::Added => Some(("Added", Color::Ok, colors.ok_bg)),
            FileChange::Deleted => Some(("Deleted", Color::Del, colors.del_bg)),
            FileChange::Modified => None,
        };

        h_flex()
            .id(("review-file", file))
            .role(Role::Group)
            .aria_label(summary)
            .h(px(CARD_HEADER))
            .gap(px(10.))
            .px(px(12.))
            .when(open, |header| header.border_b_1().border_color(colors.line))
            .child(
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
                }),
            )
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
            .child(div().flex_1())
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

fn nav_group(position: usize, title: &SharedString, kind: GroupKind, files: usize) -> AnyElement {
    let count = u32::try_from(files).unwrap_or(u32::MAX);

    h_flex()
        .id(("review-nav-group", position))
        .role(Role::Group)
        .aria_label(format!("{title}, {}", plural(count, "file", "files")))
        .h(px(NAV_ROW + 6.))
        .pt(px(6.))
        .px(px(8.))
        .gap(px(8.))
        .child(match kind {
            GroupKind::Agent(kind) => Icon::new(kind_icon(kind))
                .size(IconSize::Large)
                .into_any_element(),
            GroupKind::Folder | GroupKind::Other => div().into_any_element(),
        })
        .child({
            let title = Label::new(title.clone())
                .size(LabelSize::Small)
                .weight(FontWeight::MEDIUM)
                .truncate();
            let title = if kind == GroupKind::Folder {
                title.mono()
            } else {
                title
            };

            div().flex_1().min_w_0().flex().child(title)
        })
        .child(
            Label::new(files.to_string())
                .mono()
                .size(LabelSize::XSmall)
                .color(Color::Faint),
        )
        .into_any_element()
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

/// The text color, the tint and the sign of a line kind.
fn line_style(
    kind: DiffLineKind,
    colors: &ThemeColors,
) -> (gpui::Hsla, Option<gpui::Hsla>, &'static str) {
    match kind {
        DiffLineKind::Context => (colors.fg, None, " "),
        DiffLineKind::Added => (colors.add, Some(colors.add_bg), "+"),
        DiffLineKind::Deleted => (colors.del, Some(colors.del_bg), "\u{2212}"),
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

fn line_label(kind: DiffLineKind, number: u32, text: &str) -> String {
    let word = match kind {
        DiffLineKind::Context => "Line",
        DiffLineKind::Added => "Added line",
        DiffLineKind::Deleted => "Deleted line",
    };

    format!("{word} {number}: {text}")
}

fn unified_line(
    index: usize,
    line: Option<&DiffLine>,
    old: u32,
    new: u32,
    colors: &ThemeColors,
) -> AnyElement {
    let Some(line) = line else {
        return div().into_any_element();
    };
    let (text_color, tint, sign) = line_style(line.kind, colors);
    let number = if line.kind == DiffLineKind::Deleted {
        old
    } else {
        new
    };

    code_text()
        .id(("review-line", index))
        .role(Role::ListItem)
        .aria_label(line_label(line.kind, number, &line.text))
        .flex()
        .items_start()
        .w_full()
        .min_h(px(LINE_HEIGHT))
        .when_some(tint, Styled::bg)
        .child(number_cell(old, 6., colors))
        .child(number_cell(new, 10., colors))
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
                .child(display_text(&line.text)),
        )
        .into_any_element()
}

fn split_line(
    index: usize,
    diff: &TaskDiff,
    (file, hunk): (u32, u32),
    (left, right): (Option<Side>, Option<Side>),
    colors: &ThemeColors,
) -> AnyElement {
    let half = |side: Option<Side>, position: usize| {
        let line = side.and_then(|side| hunk_line(diff, file, hunk, side.line));
        let number = side.map_or(0, |side| side.number);

        match line {
            Some(line) => {
                let (text_color, tint, sign) = line_style(line.kind, colors);

                div()
                    .flex()
                    .items_start()
                    .flex_1()
                    .min_w_0()
                    .min_h(px(LINE_HEIGHT))
                    .when(position == 0, |half| {
                        half.border_r_1().border_color(colors.line)
                    })
                    .when_some(tint, Styled::bg)
                    .child(number_cell(number, 6., colors))
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
                            .child(display_text(&line.text)),
                    )
            }
            None => div()
                .flex_1()
                .min_w_0()
                .min_h(px(LINE_HEIGHT))
                .bg(colors.raised)
                .when(position == 0, |half| {
                    half.border_r_1().border_color(colors.line)
                }),
        }
    };

    let describe = |side: Option<Side>, word: &str| {
        side.and_then(|side| {
            hunk_line(diff, file, hunk, side.line)
                .map(|line| format!("{word} {}: {}", side.number, line.text))
        })
    };
    let label = [describe(left, "Old line"), describe(right, "New line")]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("; ");

    code_text()
        .id(("review-pair", index))
        .role(Role::ListItem)
        .aria_label(label)
        .flex()
        .w_full()
        .child(half(left, 0))
        .child(half(right, 1))
        .into_any_element()
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
            approving: self.review_approving == Some(task),
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

        if self.review_source(task) != ReviewSource::Merged
            || approved == Some(diff.fingerprint)
            || self.review_approving.is_some()
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

        self.review_approving = Some(task);
        let approving = cx.background_spawn(async move { client.approve_task(id, fingerprint) });

        cx.spawn(async move |this, cx| {
            let result = approving.await;

            let updated = this.update(cx, |workspace, cx| {
                workspace.review_approving = None;

                if let Err(error) = result {
                    log::warn!("cannot approve the task: {error}");
                    workspace.review_error = Some((task, short(&error.to_string())));
                }

                cx.notify();
            });

            if let Err(error) = updated {
                log::debug!("the window closed before the approval answered: {error}");
            }
        })
        .detach();
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
fn short(message: &str) -> SharedString {
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
    use swarm_protocol::{DiffHunk, DiffLine};

    use super::*;
    use crate::actions;
    use crate::model::AgentId;
    use crate::review_model::{Note, Row, sample_diff};

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

    #[gpui::test]
    fn the_nav_groups_files_by_agent_then_by_folder(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let review = open_review(&workspace, cx);

        let by_agent = review.read_with(cx, |review, _| review.group_titles());
        review.update(cx, |review, cx| review.set_grouping(Grouping::ByFolder, cx));
        let by_folder = review.read_with(cx, |review, _| review.group_titles());

        let names: Vec<_> = by_agent.iter().map(ToString::to_string).collect();
        assert_eq!(names, ["term-render", "pty-io", "ui-tokens"]);
        assert!(by_folder.iter().any(|title| title == "crates/swarm-ui/src"));
        assert!(by_folder.iter().any(|title| title == "benches"));
    }

    #[gpui::test]
    fn files_without_an_agent_go_to_other_changes(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, _| {
            for source in [ReviewSource::Live, ReviewSource::Merged] {
                let key = (swarm_protocol::TaskId(0), source);
                if let Some(diff) = workspace.data.diffs.get_mut(&key) {
                    let mut changed = (**diff).clone();
                    changed.files[0].agent = None;
                    *diff = Rc::new(changed);
                }
            }
        });
        let review = open_review(&workspace, cx);

        let titles = review.read_with(cx, |review, _| review.group_titles());

        assert_eq!(
            titles.last().map(ToString::to_string).as_deref(),
            Some("Other changes")
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
        let (shown, selected, titles) = review.read_with(cx, |review, _| {
            let selected = review.selected_file().and_then(|file| {
                review
                    .diff()
                    .and_then(|diff| diff.files.get(file))
                    .map(|file| file.path.clone())
            });

            (review.shown_files().len(), selected, review.group_titles())
        });

        assert_eq!(shown, 2);
        assert_eq!(
            selected,
            Some(PathBuf::from("crates/swarm-term/src/pty.rs"))
        );
        assert_eq!(titles.len(), 1);
        assert!(cx.debug_bounds("review-filter").is_some());

        click("review-filter", cx);
        let shown = review.read_with(cx, |review, _| review.shown_files().len());
        assert_eq!(shown, 9);
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
        open_review(&workspace, cx);

        click("review-approve", cx);
        let in_live = approve_state(&workspace, cx);

        workspace.update(cx, |workspace, cx| {
            workspace.set_review_source(ReviewSource::Merged, cx);
        });
        redraw(cx);
        click("review-approve", cx);
        let in_merged = approve_state(&workspace, cx);
        click("review-approve", cx);

        assert_eq!(in_live, None);
        assert_eq!(in_merged, Some(0x22));
    }

    #[gpui::test]
    fn an_approval_of_an_older_diff_does_not_count(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
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
    fn the_changes_panel_lists_the_live_diff_by_agent(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        redraw(cx);

        let list = workspace.read_with(cx, |workspace, _| workspace.changes_list.clone());
        let (titles, files) =
            list.read_with(cx, |list, _| (list.group_titles(), list.file_count()));
        let stat = workspace.read_with(cx, |workspace, _| workspace.data.live_stat(TaskId(0)));

        assert_eq!(titles.len(), 3);
        assert_eq!(files, 9);
        assert_eq!((stat.additions, stat.deletions), (321, 41));
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
