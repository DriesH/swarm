//! The command palette: search over commands, tasks and agents.

use gpui::{
    AnyElement, BoxShadow, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    MouseButton, Role, ScrollHandle, SharedString, Stateful, Subscription, Window, black, div,
    point, px,
};
use swarm_ui::fonts::MONO_FONT;
use swarm_ui::prelude::*;
use swarm_ui::{
    Confirm, Dismiss, MENU_CONTEXT, SelectNext, SelectPrevious, TextFieldRole, trap_focus,
};

use crate::actions::{
    CreatePullRequest, FocusAgent, NewTask, NewTerminal, OpenSettings, PlanWithOrchestrator,
    ShowReview,
};
use crate::close_task::CloseTaskDialog;
use crate::model::{AgentKind, FakeData, ProjectId, TaskId, TaskStatus, TerminalId};
use crate::new_task::NewTaskDialog;
use crate::pull_request::AnywayDialog;
use crate::remove_project::RemoveProjectDialog;
use crate::review_view::ReviewView;
use crate::workspace::{View, Workspace};

const KEY_COMMAND: &str = "\u{2318}";
const KEY_SHIFT: &str = "\u{21e7}";
const KEY_OPTION: &str = "\u{2325}";

/// What a row does when chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    ReviewChanges,
    FocusOrchestrator,
    NewTask,
    PlanWithOrchestrator,
    NewTerminal,
    Settings,
    NextFile,
    PreviousFile,
    ToggleViewed,
    CommentOnLine,
    CloseTask,
    CreatePullRequest,
    Buzz,
    /// Debug builds only.
    ShowWelcome,
    /// Debug builds only.
    CrownQueen,
    /// Debug builds only.
    ShowHoneyToast,
    Task(TaskId),
    Agent(TerminalId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PaletteEvent {
    Dismissed,
    Chosen(Command),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
    Suggested,
    Tasks,
    Agents,
    Swarm,
}

impl Group {
    const ALL: [Self; 4] = [Self::Suggested, Self::Tasks, Self::Agents, Self::Swarm];

    fn title(self) -> &'static str {
        match self {
            Self::Suggested => "Suggested",
            Self::Tasks => "Tasks",
            Self::Agents => "Agents",
            Self::Swarm => "Swarm",
        }
    }
}

#[derive(Clone, Debug)]
enum Detail {
    None,
    Changes { additions: u32, deletions: u32 },
    Status(AgentStatus, Option<&'static str>),
}

/// One line of the palette.
#[derive(Clone, Debug)]
pub(crate) struct Row {
    group: Group,
    icon: IconName,
    label: SharedString,
    detail: Detail,
    keys: Vec<&'static str>,
    command: Command,
    /// Shows only when the query is the row's whole label.
    secret: bool,
}

impl Row {
    fn new(group: Group, icon: IconName, label: impl Into<SharedString>, command: Command) -> Self {
        Self {
            group,
            icon,
            label: label.into(),
            detail: Detail::None,
            keys: Vec::new(),
            command,
            secret: false,
        }
    }

    fn secret(mut self) -> Self {
        self.secret = true;
        self
    }

    #[cfg(test)]
    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    fn detail(mut self, detail: Detail) -> Self {
        self.detail = detail;
        self
    }

    fn keys(mut self, keys: &[&'static str]) -> Self {
        self.keys = keys.to_vec();
        self
    }
}

const ROW_HEIGHT: f32 = 38.;
const GROUP_HEADER_HEIGHT: f32 = 24.;
const RESULT_GAP: f32 = 1.;
const RESULTS_PADDING_TOP: f32 = 10.;
const RESULTS_PADDING_BOTTOM: f32 = 8.;

const DIGITS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

fn task_status(status: TaskStatus) -> (AgentStatus, Option<&'static str>) {
    match status {
        TaskStatus::Working => (AgentStatus::Working, None),
        TaskStatus::NeedsYou => (AgentStatus::Blocked, Some("Needs you")),
        TaskStatus::Idle => (AgentStatus::Idle, None),
    }
}

/// The rows for the open project and task, in group order.
#[expect(
    clippy::too_many_lines,
    reason = "one flat list of rows reads best in one place"
)]
pub(crate) fn rows(data: &FakeData, project: ProjectId, task: TaskId, view: View) -> Vec<Row> {
    let stat = data.live_stat(task);
    let (additions, deletions) = (stat.additions, stat.deletions);

    let mut rows = vec![
        Row::new(
            Group::Suggested,
            IconName::Branch,
            "Review changes",
            Command::ReviewChanges,
        )
        .detail(Detail::Changes {
            additions,
            deletions,
        })
        .keys(&[KEY_SHIFT, KEY_COMMAND, "R"]),
        Row::new(
            Group::Suggested,
            IconName::ClaudeCode,
            "Focus orchestrator",
            Command::FocusOrchestrator,
        )
        .keys(&[KEY_COMMAND, "1"]),
        Row::new(
            Group::Suggested,
            IconName::PlusSquare,
            "New task",
            Command::NewTask,
        )
        .keys(&[KEY_COMMAND, "T"]),
        Row::new(
            Group::Suggested,
            IconName::Bee,
            "Plan with orchestrator",
            Command::PlanWithOrchestrator,
        )
        .keys(&[KEY_SHIFT, KEY_COMMAND, "T"]),
    ];

    if data.can_create_pr(task) {
        rows.insert(
            1,
            Row::new(
                Group::Suggested,
                IconName::PullRequest,
                "Create pull request",
                Command::CreatePullRequest,
            ),
        );
    }

    rows.extend(data.tasks_of(project).map(|entry| {
        let (status, label) = task_status(entry.status);

        Row::new(
            Group::Tasks,
            IconName::Tab,
            data.tab_label(entry),
            Command::Task(entry.id),
        )
        .detail(Detail::Status(status, label))
    }));

    rows.extend(
        data.agents_of(task)
            .enumerate()
            .skip(1)
            .map(|(index, agent)| {
                let icon = match agent.kind {
                    AgentKind::ClaudeCode => IconName::ClaudeCode,
                    AgentKind::Codex => IconName::Codex,
                };
                let keys = DIGITS
                    .get(index)
                    .map_or_else(Vec::new, |digit| vec![*digit]);
                let mut row = Row::new(
                    Group::Agents,
                    icon,
                    agent.name.clone(),
                    Command::Agent(agent.terminal),
                )
                .detail(Detail::Status(agent.status, None));

                if !keys.is_empty() {
                    row.keys = std::iter::once(KEY_COMMAND).chain(keys).collect();
                }

                row
            }),
    );

    if view == View::Review {
        rows.extend([
            Row::new(Group::Swarm, IconName::File, "Next file", Command::NextFile).keys(&["J"]),
            Row::new(
                Group::Swarm,
                IconName::File,
                "Previous file",
                Command::PreviousFile,
            )
            .keys(&["K"]),
            Row::new(
                Group::Swarm,
                IconName::Check,
                "Toggle viewed",
                Command::ToggleViewed,
            )
            .keys(&["V"]),
            Row::new(
                Group::Swarm,
                IconName::Plus,
                "Comment on line",
                Command::CommentOnLine,
            )
            .keys(&["C"]),
        ]);
    }

    if data.task(task).is_some_and(|task| !task.is_start) {
        rows.push(Row::new(
            Group::Swarm,
            IconName::Close,
            "Close task",
            Command::CloseTask,
        ));
    }

    rows.extend([
        Row::new(
            Group::Swarm,
            IconName::Terminal,
            "New terminal",
            Command::NewTerminal,
        )
        .keys(&[KEY_OPTION, KEY_COMMAND, "T"]),
        Row::new(
            Group::Swarm,
            IconName::Settings,
            "Settings",
            Command::Settings,
        )
        .keys(&[KEY_COMMAND, ","]),
    ]);

    rows
}

/// Indexes of the rows whose label contains `query`, ignoring case. A secret row shows only
/// when the query is its whole label.
fn filter(rows: &[Row], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();

    rows.iter()
        .enumerate()
        .filter(|(_, row)| {
            let label = row.label.to_lowercase();

            if row.secret {
                label == query
            } else {
                label.contains(&query)
            }
        })
        .map(|(index, _)| index)
        .collect()
}

fn group_container(number: usize, title: &'static str) -> Stateful<Div> {
    v_flex()
        .id(("palette-group", number))
        .role(Role::Group)
        .aria_label(title)
        .gap(px(RESULT_GAP))
}

/// The open palette. It reports what the user chose and leaves closing to its owner.
pub(crate) struct Palette {
    rows: Vec<Row>,
    visible: Vec<usize>,
    highlighted: usize,
    input: Entity<TextInput>,
    scroll: ScrollHandle,
    follow_highlight: bool,
    _subscription: Subscription,
}

impl EventEmitter<PaletteEvent> for Palette {}

impl Focusable for Palette {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Palette {
    pub(crate) fn new(rows: Vec<Row>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            let mut input = TextInput::new(cx);
            input.set_label("Search commands, tasks and agents", cx);
            input.set_bordered(false, cx);
            input.set_role(TextFieldRole::ComboBox, cx);
            input.set_expanded(Some(true), cx);
            input.set_placeholder("Search", cx);
            input
        });
        window.focus(&input.focus_handle(cx), cx);

        let subscription = cx.subscribe(&input, |this, _, event, cx| match event {
            TextInputEvent::Changed(query) => this.set_query(query, cx),
            TextInputEvent::Submitted(_) => this.confirm(cx),
        });

        Self {
            visible: filter(&rows, ""),
            rows,
            highlighted: 0,
            input,
            scroll: ScrollHandle::new(),
            follow_highlight: false,
            _subscription: subscription,
        }
    }

    fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        self.visible = filter(&self.rows, query);
        self.highlighted = 0;
        self.follow_highlight = true;
        cx.notify();
    }

    fn move_highlight(&mut self, forward: bool, cx: &mut Context<Self>) {
        let count = self.visible.len();

        if count == 0 {
            return;
        }

        self.highlighted = if forward {
            (self.highlighted + 1) % count
        } else {
            (self.highlighted + count - 1) % count
        };
        self.follow_highlight = true;
        cx.notify();
    }

    fn choose(&mut self, position: usize, cx: &mut Context<Self>) {
        let command = self
            .visible
            .get(position)
            .and_then(|index| self.rows.get(*index))
            .map(|row| row.command);

        if let Some(command) = command {
            cx.emit(PaletteEvent::Chosen(command));
        }
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        self.choose(self.highlighted, cx);
    }

    /// The group elements, and the top and bottom of the highlighted row in the content.
    fn render_groups(&self, cx: &mut Context<Self>) -> (Vec<AnyElement>, (f32, f32)) {
        let faint = cx.theme().colors().faint;
        let mut results = Vec::new();
        let mut group_number = 0;
        let mut offset = RESULTS_PADDING_TOP;
        let mut highlighted_span = (0., 0.);

        for group in Group::ALL {
            let in_group: Vec<(usize, usize)> = self
                .visible
                .iter()
                .enumerate()
                .filter(|(_, index)| self.rows[**index].group == group)
                .map(|(position, index)| (position, *index))
                .collect();

            if in_group.is_empty() {
                continue;
            }

            let mut rows_in_group = Vec::new();
            let group_top = offset;
            offset += GROUP_HEADER_HEIGHT + RESULT_GAP;

            for (position, index) in in_group {
                if position == self.highlighted {
                    let first = rows_in_group.is_empty();
                    highlighted_span =
                        (if first { group_top } else { offset }, offset + ROW_HEIGHT);
                }

                let row = self.rows[index].clone();
                rows_in_group.push(self.render_row(position, &row, cx).into_any_element());
                offset += ROW_HEIGHT + RESULT_GAP;
            }

            results.push(
                group_container(group_number, group.title())
                    .flex_none()
                    .child(
                        h_flex()
                            .flex_none()
                            .h(px(GROUP_HEADER_HEIGHT))
                            .px(px(10.))
                            .text_size(px(11.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(faint)
                            .child(group.title()),
                    )
                    .children(rows_in_group)
                    .into_any_element(),
            );
            group_number += 1;
        }

        (results, highlighted_span)
    }

    /// Scrolls the least that shows the span, given as top and bottom in the content.
    fn scroll_to(&self, (top, bottom): (f32, f32)) {
        let viewport = f32::from(self.scroll.bounds().size.height);
        let current = -f32::from(self.scroll.offset().y);

        let target = if top <= RESULTS_PADDING_TOP {
            0.
        } else if top < current {
            top
        } else if bottom + RESULTS_PADDING_BOTTOM > current + viewport {
            bottom + RESULTS_PADDING_BOTTOM - viewport
        } else {
            current
        };

        self.scroll.set_offset(point(px(0.), px(-target)));
    }

    fn render_row(&self, position: usize, row: &Row, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let selected = position == self.highlighted;
        let (active, soft) = (colors.active, colors.soft);
        let (add, del) = (colors.add, colors.del);

        let detail = match &row.detail {
            Detail::None => None,
            Detail::Changes {
                additions,
                deletions,
            } => Some(
                h_flex()
                    .flex_none()
                    .gap(px(4.))
                    .font_family(MONO_FONT)
                    .text_size(px(11.))
                    .child(div().text_color(add).child(format!("+{additions}")))
                    .child(div().text_color(del).child(format!("\u{2212}{deletions}")))
                    .into_any_element(),
            ),
            Detail::Status(status, label) => {
                let pill = StatusPill::new(*status);
                let pill = match label {
                    Some(label) => pill.label(*label),
                    None => pill,
                };

                Some(pill.into_any_element())
            }
        };

        h_flex()
            .id(("palette-row", position))
            .role(Role::ListBoxOption)
            .aria_label(row.label.clone())
            .aria_selected(selected)
            .flex_none()
            .h(px(ROW_HEIGHT))
            .px(px(10.))
            .gap(px(10.))
            .rounded(px(8.))
            .cursor_pointer()
            .when(selected, |row| row.bg(active).aria_active_descendant())
            .when(!selected, |row| row.hover(move |style| style.bg(soft)))
            .on_click(cx.listener(move |this, _, _, cx| this.choose(position, cx)))
            .child(
                Icon::new(row.icon)
                    .size(IconSize::Large)
                    .color(Color::Muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Label::new(row.label.clone()).truncate()),
            )
            .children(detail)
            .when(!row.keys.is_empty(), |line| {
                line.child(Kbd::new(
                    ("palette-keys", position),
                    row.keys.iter().copied(),
                ))
            })
    }
}

impl Render for Palette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (line, panel, scrim) = (colors.line, colors.panel, colors.scrim);

        let (results, highlighted_span) = self.render_groups(cx);

        if self.follow_highlight {
            self.follow_highlight = false;
            self.scroll_to(highlighted_span);
        }

        let search = h_flex()
            .flex_none()
            .h(px(52.))
            .px(px(16.))
            .gap(px(10.))
            .border_b_1()
            .border_color(line)
            .child(
                Icon::new(IconName::Search)
                    .size(IconSize::Custom(px(17.)))
                    .color(Color::Faint),
            )
            .child(div().flex_1().child(self.input.clone()))
            .child(Kbd::new("palette-dismiss-key", ["esc"]));

        let results = v_flex()
            .id("palette-results")
            .role(Role::ListBox)
            .aria_label("Results")
            .max_h(px(520.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .gap(px(RESULT_GAP))
            .px(px(8.))
            .pt(px(RESULTS_PADDING_TOP))
            .pb(px(RESULTS_PADDING_BOTTOM))
            .children(results);

        let input_focus = self.input.focus_handle(cx);

        let sheet = trap_focus(v_flex().id("palette-panel"), input_focus.clone())
            .role(Role::Dialog)
            .aria_label("Command palette")
            .key_context(MENU_CONTEXT)
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                this.move_highlight(false, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_highlight(true, cx)))
            .on_action(cx.listener(|this, _: &Confirm, _, cx| this.confirm(cx)))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(PaletteEvent::Dismissed)))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                window.focus(&input_focus, cx);
                cx.stop_propagation();
            })
            .w(px(640.))
            .overflow_hidden()
            .rounded(px(14.))
            .border_1()
            .border_color(line)
            .bg(panel)
            .shadow(vec![BoxShadow {
                color: black().opacity(0.35),
                offset: point(px(0.), px(24.)),
                blur_radius: px(64.),
                spread_radius: px(0.),
                inset: false,
            }])
            .child(search)
            .child(results);

        div()
            .id("palette")
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(110.))
            .bg(scrim)
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.emit(PaletteEvent::Dismissed)),
            )
            .child(sheet)
    }
}

/// The modal over the workspace: the palette or the New task dialog.
pub(crate) enum Overlay {
    Palette {
        view: Entity<Palette>,
        _subscription: Subscription,
    },
    NewTask {
        view: Entity<NewTaskDialog>,
        _subscription: Subscription,
    },
    CloseTask {
        view: Entity<CloseTaskDialog>,
        _subscription: Subscription,
    },
    CreateAnyway {
        view: Entity<AnywayDialog>,
        _subscription: Subscription,
    },
    RemoveProject {
        view: Entity<RemoveProjectDialog>,
        _subscription: Subscription,
    },
}

impl Overlay {
    pub(crate) fn element(&self) -> AnyElement {
        match self {
            Self::Palette { view, .. } => view.clone().into_any_element(),
            Self::NewTask { view, .. } => view.clone().into_any_element(),
            Self::CloseTask { view, .. } => view.clone().into_any_element(),
            Self::CreateAnyway { view, .. } => view.clone().into_any_element(),
            Self::RemoveProject { view, .. } => view.clone().into_any_element(),
        }
    }
}

impl Workspace {
    pub(crate) fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.overlay, Some(Overlay::Palette { .. })) {
            self.close_overlay(window, cx);
            return;
        }

        let mut rows = rows(&self.data, self.active_project, self.active_task, self.view);
        if crate::settings::easter_eggs(cx) {
            rows.push(Row::new(Group::Swarm, IconName::Bee, "Buzz", Command::Buzz).secret());
        }
        if cfg!(debug_assertions) {
            rows.push(Row::new(
                Group::Swarm,
                IconName::Bee,
                "Show welcome screen",
                Command::ShowWelcome,
            ));
            rows.push(Row::new(
                Group::Swarm,
                IconName::Bee,
                "Crown the queen bee",
                Command::CrownQueen,
            ));
            rows.push(Row::new(
                Group::Swarm,
                IconName::Bee,
                "Show Honey's ready toast",
                Command::ShowHoneyToast,
            ));
        }

        let view = cx.new(|cx| Palette::new(rows, window, cx));
        let subscription =
            cx.subscribe_in(&view, window, |this, _, event, window, cx| match event {
                PaletteEvent::Dismissed => this.close_overlay(window, cx),
                PaletteEvent::Chosen(command) => this.run_command(*command, window, cx),
            });

        self.overlay = Some(Overlay::Palette {
            view,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Closes the open overlay and gives focus back to the workspace.
    pub(crate) fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = None;
        window.focus(&self.focus_handle(cx), cx);
        cx.notify();
    }

    fn run_command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        self.close_overlay(window, cx);

        match command {
            Command::Task(task) => self.select_task(task, cx),
            Command::Agent(terminal) => self.focus_terminal(terminal, cx),
            Command::ReviewChanges => window.dispatch_action(Box::new(ShowReview), cx),
            Command::FocusOrchestrator => {
                window.dispatch_action(Box::new(FocusAgent { index: 0 }), cx);
            }
            Command::NewTask => window.dispatch_action(Box::new(NewTask), cx),
            Command::PlanWithOrchestrator => {
                window.dispatch_action(Box::new(PlanWithOrchestrator), cx);
            }
            Command::NewTerminal => window.dispatch_action(Box::new(NewTerminal), cx),
            Command::Settings => window.dispatch_action(Box::new(OpenSettings), cx),
            Command::NextFile => self.review.update(cx, ReviewView::next_file),
            Command::PreviousFile => self.review.update(cx, ReviewView::previous_file),
            Command::ToggleViewed => self.review.update(cx, ReviewView::toggle_selected_viewed),
            Command::CommentOnLine => self.review.update(cx, ReviewView::comment_on_line),
            Command::CloseTask => self.open_close_task(self.active_task, window, cx),
            Command::CreatePullRequest => window.dispatch_action(Box::new(CreatePullRequest), cx),
            Command::Buzz => self.start_buzz(cx),
            Command::ShowWelcome => self.show_welcome(cx),
            #[cfg(debug_assertions)]
            Command::CrownQueen => self.crown_queen(cx),
            #[cfg(debug_assertions)]
            Command::ShowHoneyToast => self.show_honey_toast_now(window, cx),
            #[cfg(not(debug_assertions))]
            Command::CrownQueen | Command::ShowHoneyToast => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_rows() -> Vec<Row> {
        rows(
            &FakeData::sample(),
            ProjectId(0),
            TaskId(0),
            View::Terminals,
        )
    }

    fn labels(rows: &[Row], indexes: &[usize]) -> Vec<String> {
        indexes
            .iter()
            .map(|index| rows[*index].label.to_string())
            .collect()
    }

    #[test]
    fn comment_on_line_is_in_the_palette_only_while_review_is_open() {
        let data = FakeData::sample();
        let has = |view| {
            rows(&data, ProjectId(0), TaskId(0), view)
                .iter()
                .any(|row| row.command == Command::CommentOnLine)
        };

        assert!(has(View::Review));
        assert!(!has(View::Terminals));
    }

    #[test]
    fn an_empty_query_keeps_every_row() {
        let rows = sample_rows();

        assert_eq!(filter(&rows, "").len(), rows.len());
    }

    #[test]
    fn filter_matches_a_substring_ignoring_case() {
        let rows = sample_rows();
        let found = filter(&rows, "REV");

        assert_eq!(labels(&rows, &found), ["Review changes"]);
    }

    #[test]
    fn filter_finds_nothing_for_an_unknown_word() {
        assert!(filter(&sample_rows(), "zzzz").is_empty());
    }

    #[test]
    fn the_modifiers_of_a_shortcut_come_in_the_order_of_macos() {
        let keys_of = |label: &str| {
            sample_rows()
                .into_iter()
                .find(|row| row.label == label)
                .map(|row| row.keys)
                .unwrap_or_default()
        };

        assert_eq!(keys_of("Review changes"), [KEY_SHIFT, KEY_COMMAND, "R"]);
        assert_eq!(keys_of("New terminal"), [KEY_OPTION, KEY_COMMAND, "T"]);
        assert_eq!(keys_of("New task"), [KEY_COMMAND, "T"]);
    }

    use gpui::{TestAppContext, VisualTestContext};

    use crate::actions::{self, ToggleCommandPalette};
    use crate::model::PLAN_TASK;

    fn setup(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
        cx.update(|cx| {
            swarm_ui::init(cx);
            actions::bind_keys(cx);
        });

        let (workspace, cx) = cx.add_window_view(Workspace::with_sample);
        cx.run_until_parked();

        (workspace, cx)
    }

    fn palette_labels(
        workspace: &Entity<Workspace>,
        cx: &VisualTestContext,
    ) -> Option<Vec<String>> {
        workspace.read_with(cx, |workspace, cx| match &workspace.overlay {
            Some(Overlay::Palette { view, .. }) => {
                let palette = view.read(cx);
                Some(
                    palette
                        .visible
                        .iter()
                        .map(|index| palette.rows[*index].label.to_string())
                        .collect(),
                )
            }
            _ => None,
        })
    }

    fn dialog_is_open(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> bool {
        workspace.read_with(cx, |workspace, _| {
            matches!(workspace.overlay, Some(Overlay::NewTask { .. }))
        })
    }

    fn task_count(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> usize {
        workspace.read_with(cx, |workspace, _| workspace.data.tasks.len())
    }

    #[gpui::test]
    fn cmd_k_opens_the_palette_and_escape_closes_it(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        cx.simulate_keystrokes("cmd-k");
        let opened = palette_labels(&workspace, cx).is_some();
        cx.simulate_keystrokes("escape");
        let closed = palette_labels(&workspace, cx).is_none();

        assert!(opened);
        assert!(closed);
    }

    #[gpui::test]
    fn cmd_k_twice_closes_the_palette(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        cx.dispatch_action(ToggleCommandPalette);
        cx.simulate_keystrokes("cmd-k");

        assert!(palette_labels(&workspace, cx).is_none());
    }

    #[gpui::test]
    fn typing_rev_leaves_only_review_rows(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        cx.simulate_keystrokes("cmd-k");
        cx.simulate_input("rev");

        assert_eq!(
            palette_labels(&workspace, cx),
            Some(vec!["Review changes".to_owned()])
        );
    }

    fn flight_is_up(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> bool {
        workspace.read_with(cx, |workspace, _| workspace.eggs.flight.is_some())
    }

    #[gpui::test]
    fn typing_buzz_offers_the_buzz_item_and_nothing_else_does(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        cx.simulate_keystrokes("cmd-k");
        let at_first = palette_labels(&workspace, cx).unwrap_or_default();
        cx.simulate_input("buz");
        let partial = palette_labels(&workspace, cx).unwrap_or_default();
        cx.simulate_input("z");
        let typed = palette_labels(&workspace, cx);

        assert!(!at_first.iter().any(|label| label == "Buzz"));
        assert!(partial.is_empty());
        assert_eq!(typed, Some(vec!["Buzz".to_owned()]));
    }

    // Tests build with debug assertions, like `cargo run`.
    #[gpui::test]
    fn show_welcome_screen_opens_it_with_projects_and_skip_closes_it(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let shows = |cx: &mut VisualTestContext| {
            workspace.update(cx, |workspace, cx| workspace.shows_welcome(cx))
        };
        assert!(!shows(cx), "the sample has projects");

        cx.simulate_keystrokes("cmd-k");
        cx.simulate_input("welcome");
        let typed = palette_labels(&workspace, cx);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert_eq!(typed, Some(vec!["Show welcome screen".to_owned()]));
        assert!(shows(cx));

        workspace.update(cx, Workspace::finish_welcome);
        assert!(!shows(cx));
    }

    #[gpui::test]
    fn show_honeys_ready_toast_shows_it_for_the_open_task(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        cx.simulate_keystrokes("cmd-k");
        cx.simulate_input("honey");
        let typed = palette_labels(&workspace, cx);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        let (toast, open) = workspace.read_with(cx, |workspace, _| {
            (
                workspace
                    .eggs
                    .toast
                    .as_ref()
                    .map(|toast| toast.task.to_string()),
                workspace
                    .data
                    .task(workspace.active_task)
                    .map(|task| task.name.to_string()),
            )
        });
        assert_eq!(typed, Some(vec!["Show Honey's ready toast".to_owned()]));
        assert!(toast.is_some());
        assert_eq!(toast, open);
    }

    #[gpui::test]
    fn running_buzz_sends_the_bees_across_once_and_they_go_away(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        assert!(!flight_is_up(&workspace, cx));

        cx.simulate_keystrokes("cmd-k");
        cx.simulate_input("buzz");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert!(
            palette_labels(&workspace, cx).is_none(),
            "the palette closes"
        );
        assert!(flight_is_up(&workspace, cx));

        cx.executor().advance_clock(swarm_ui::FLIGHT_DURATION / 2);
        cx.run_until_parked();
        assert!(flight_is_up(&workspace, cx), "still in the air");

        cx.executor().advance_clock(swarm_ui::FLIGHT_DURATION);
        cx.run_until_parked();
        assert!(!flight_is_up(&workspace, cx), "the bees are gone");
    }

    #[gpui::test]
    fn buzz_is_not_offered_with_the_easter_eggs_off(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.update(|_, cx| crate::settings::change_appearance(|a| a.easter_eggs = false, cx));

        cx.simulate_keystrokes("cmd-k");
        cx.simulate_input("buzz");

        assert_eq!(palette_labels(&workspace, cx), Some(Vec::new()));
        assert!(!flight_is_up(&workspace, cx));
    }

    #[gpui::test]
    fn no_bee_flies_under_reduce_motion(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.update(|_, cx| cx.set_reduce_motion(true));

        cx.simulate_keystrokes("cmd-k");
        cx.simulate_input("buzz");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert!(!flight_is_up(&workspace, cx));
    }

    #[gpui::test]
    fn enter_on_new_task_opens_the_dialog(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        cx.simulate_keystrokes("cmd-k");
        cx.simulate_input("new task");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert!(dialog_is_open(&workspace, cx));
        assert!(palette_labels(&workspace, cx).is_none());
    }

    #[gpui::test]
    fn down_then_enter_runs_the_second_row(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        cx.simulate_keystrokes("cmd-k down down enter");
        cx.run_until_parked();

        assert!(dialog_is_open(&workspace, cx));
    }

    fn palette_has_input_focus(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
        let focus = workspace.read_with(cx, |workspace, cx| match &workspace.overlay {
            Some(Overlay::Palette { view, .. }) => Some(view.read(cx).focus_handle(cx)),
            _ => None,
        });

        focus.is_some_and(|focus| cx.update(|window, _| focus.is_focused(window)))
    }

    #[gpui::test]
    fn tab_keeps_focus_inside_the_open_palette(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        cx.simulate_keystrokes("cmd-k");
        let before = palette_has_input_focus(&workspace, cx);
        cx.simulate_keystrokes("tab");
        let after_tab = palette_has_input_focus(&workspace, cx);
        cx.simulate_keystrokes("shift-tab");
        let after_shift_tab = palette_has_input_focus(&workspace, cx);

        assert!(before);
        assert!(after_tab);
        assert!(after_shift_tab);
    }

    #[gpui::test]
    fn groups_have_the_group_role_and_a_label(cx: &mut TestAppContext) {
        use gpui::Element as _;
        use gpui::accesskit::{Node, Role as AccessRole};

        cx.update(swarm_ui::theme::init);

        let element = group_container(0, "Tasks").into_element();
        let mut node = Node::new(AccessRole::Unknown);
        element.write_a11y_info(&mut node);

        assert_eq!(element.a11y_role(), Some(AccessRole::Group));
        assert_eq!(node.label(), Some("Tasks"));
    }

    #[gpui::test]
    fn create_task_adds_and_selects_a_task(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let before = task_count(&workspace, cx);

        cx.simulate_keystrokes("cmd-t");
        cx.simulate_input("docs-site");
        cx.simulate_keystrokes("enter");

        let (active, newest, closed) = workspace.read_with(cx, |workspace, _| {
            (
                workspace.active_task,
                workspace
                    .data
                    .tasks
                    .last()
                    .map(|task| (task.id, task.name.to_string())),
                workspace.overlay.is_none(),
            )
        });
        assert_eq!(task_count(&workspace, cx), before + 1);
        assert_eq!(newest.clone().map(|(id, _)| id), Some(active));
        assert_eq!(newest.map(|(_, name)| name), Some("docs-site".to_owned()));
        assert!(closed);
    }

    #[gpui::test]
    fn create_task_is_disabled_with_an_empty_name(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let before = task_count(&workspace, cx);

        cx.simulate_keystrokes("cmd-t enter");

        assert_eq!(task_count(&workspace, cx), before);
        assert!(dialog_is_open(&workspace, cx));
    }

    #[gpui::test]
    fn plan_with_orchestrator_twice_keeps_one_start_task(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        cx.simulate_keystrokes("cmd-shift-t");
        cx.simulate_keystrokes("cmd-shift-t");

        let (starts, active_is_start) = workspace.read_with(cx, |workspace, _| {
            let starts = workspace
                .data
                .tasks
                .iter()
                .filter(|task| task.is_start)
                .count();
            (starts, workspace.active_task().name == PLAN_TASK)
        });
        assert_eq!(starts, 1);
        assert!(active_is_start);
    }
}
