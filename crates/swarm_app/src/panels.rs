//! The right panel: changes, files and pull request.

use gpui::{AnyElement, Context, Entity, FontWeight, Role, SharedString, Window, div, px};
use swarm_ui::prelude::*;
use swarm_ui::styles::focus_ring;

use crate::model::{
    AgentKind, CheckState, FakePullRequest, FileMark, PULL_REQUEST_DESCRIPTION, PULL_REQUEST_TITLE,
    TreeEntry, sample_pull_request,
};
use crate::workspace::{RightPanel, View, Workspace};

const LOCAL_CHECKS: [(&str, &str); 3] = [
    ("cargo test --workspace", "36 passed"),
    ("cargo clippy -- -D warnings", "clean"),
    ("cargo fmt --check", "clean"),
];

pub(crate) fn render(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement + use<> {
    match workspace.right_panel {
        Some(RightPanel::Changes) => changes(workspace, cx),
        Some(RightPanel::Files) => files(workspace, window, cx),
        Some(RightPanel::PullRequest) => match &workspace.data.pull_request {
            Some(pull_request) => pull_request_status(pull_request, window, cx),
            None => new_pull_request(workspace, window, cx),
        },
        None => div().into_any_element(),
    }
}

fn mono(text: impl Into<SharedString>) -> Label {
    Label::new(text).mono().size(LabelSize::XSmall)
}

fn header(cx: &App) -> gpui::Div {
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

fn pill(text: SharedString, cx: &App) -> impl IntoElement {
    let colors = cx.theme().colors();

    div()
        .flex_none()
        .px(px(7.))
        .py(px(2.))
        .rounded(px(5.))
        .bg(colors.soft)
        .child(mono(text))
}

#[expect(
    clippy::too_many_lines,
    reason = "one flat element tree reads best in one place"
)]
fn changes(workspace: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let colors = cx.theme().colors();
    let (raised, accent) = (colors.raised, colors.accent);

    let task = workspace.active_task;
    let changed: Vec<_> = workspace
        .data
        .changes
        .iter()
        .filter(|file| file.task == task)
        .collect();

    let count = if changed.len() == 1 {
        "1 file".to_owned()
    } else {
        format!("{} files", changed.len())
    };

    let mut list = v_flex().flex_1().min_h_0().overflow_hidden().p(px(6.));
    let mut index = 0_usize;

    for agent in workspace.data.agents_of(task) {
        let files: Vec<_> = changed
            .iter()
            .filter(|file| file.worker == agent.id)
            .collect();

        if files.is_empty() {
            continue;
        }

        let additions = files.iter().map(|file| file.additions).sum();
        let deletions = files.iter().map(|file| file.deletions).sum();
        let icon = match agent.kind {
            AgentKind::ClaudeCode => IconName::ClaudeCode,
            AgentKind::Codex => IconName::Codex,
        };

        list = list.child(
            h_flex()
                .h(px(30.))
                .mt(px(4.))
                .px(px(8.))
                .gap(px(8.))
                .child(Icon::new(icon).size(IconSize::Medium).color(Color::Muted))
                .child(
                    div().flex_1().min_w_0().flex().child(
                        Label::new(agent.name.clone())
                            .size(LabelSize::Small)
                            .weight(FontWeight::MEDIUM)
                            .truncate(),
                    ),
                )
                .child(delta(additions, deletions)),
        );

        for file in files {
            let (directory, name) = file
                .path
                .rsplit_once('/')
                .map_or(("", file.path.as_ref()), |(directory, name)| {
                    (directory, name)
                });
            let summary = format!("{}, +{} −{}", file.path, file.additions, file.deletions);

            list = list.child(
                h_flex()
                    .id(("change", index))
                    .role(Role::Button)
                    .aria_label(summary)
                    .focusable()
                    .tab_stop(true)
                    .cursor_pointer()
                    .h(px(28.))
                    .pl(px(30.))
                    .pr(px(8.))
                    .gap(px(8.))
                    .rounded(px(7.))
                    .hover(move |style| style.bg(raised))
                    .focus_visible(move |style| focus_ring(style, accent))
                    .on_click(cx.listener(|this, _, _, cx| this.set_view(View::Review, cx)))
                    .child(
                        div().min_w_0().flex().child(
                            Label::new(name.to_owned())
                                .size(LabelSize::Default)
                                .truncate(),
                        ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .child(mono(directory.to_owned()).color(Color::Faint).truncate()),
                    )
                    .when(file.editing, |row| {
                        row.child(
                            div()
                                .id(("editing", index))
                                .role(Role::Image)
                                .aria_label("Editing")
                                .flex_none()
                                .size(px(6.))
                                .rounded_full()
                                .bg(accent),
                        )
                    })
                    .child(delta(file.additions, file.deletions)),
            );
            index += 1;
        }
    }

    v_flex()
        .size_full()
        .child(
            header(cx)
                .h(px(40.))
                .child(Label::new("Changes").weight(FontWeight::MEDIUM))
                .child(mono(count).color(Color::Faint))
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
        .child(list)
        .into_any_element()
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

#[expect(
    clippy::too_many_lines,
    reason = "one flat element tree reads best in one place"
)]
fn files(workspace: &Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let colors = cx.theme().colors();
    let (line, raised, active, accent) = (colors.line, colors.raised, colors.active, colors.accent);

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
        .px(px(6.))
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

        let chevron = if entry.expanded {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        };

        list = list.child(
            h_flex()
                .id(("file", index))
                .role(Role::TreeItem)
                .aria_label(name.clone())
                .aria_level(usize::from(entry.depth) + 1)
                .aria_selected(is_selected)
                .when(is_dir, |row| row.aria_expanded(entry.expanded))
                .focusable()
                .tab_stop(true)
                .cursor_pointer()
                .h(px(26.))
                .pl(px(4. + 14. * f32::from(entry.depth)))
                .pr(px(8.))
                .gap(px(6.))
                .rounded(px(6.))
                .when(is_selected, |row| row.bg(active))
                .when(!is_selected, |row| row.hover(move |style| style.bg(raised)))
                .focus_visible(move |style| focus_ring(style, accent))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if is_dir {
                        if let Some(entry) = this.data.file_tree.get_mut(index) {
                            entry.expanded = !entry.expanded;
                        }
                    } else {
                        state.update(cx, |selected, _| *selected = index);
                    }
                    cx.notify();
                }))
                .child(div().w(px(12.)).flex_none().when(is_dir, |slot| {
                    slot.child(Icon::new(chevron).size(IconSize::Small).color(Color::Faint))
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .child(Label::new(name).color(name_color).truncate()),
                )
                .when_some(entry.mark, |row, mark| {
                    let (letter, color) = match mark {
                        FileMark::Modified => ("M", Color::Warn),
                        FileMark::Added => ("A", Color::Add),
                    };

                    row.child(mono(letter).weight(FontWeight::SEMIBOLD).color(color))
                }),
        );
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

fn panel_header(title: AnyElement, action: impl IntoElement, cx: &App) -> gpui::Div {
    header(cx)
        .h(px(46.))
        .child(
            Icon::new(IconName::PullRequest)
                .size(IconSize::Medium)
                .color(Color::Default),
        )
        .child(title)
        .child(div().flex_1())
        .child(action)
}

fn close_button(cx: &mut Context<Workspace>) -> IconButton {
    IconButton::new(
        "close-pull-request",
        IconName::Close,
        "Close pull request panel",
    )
    .on_click(cx.listener(|this, _, _, cx| {
        this.toggle_panel(RightPanel::PullRequest, cx);
    }))
}

fn footer(cx: &App) -> gpui::Div {
    let line = cx.theme().colors().line;

    h_flex()
        .flex_none()
        .gap(px(8.))
        .px(px(14.))
        .py(px(12.))
        .border_t_1()
        .border_color(line)
}

fn text_box(cx: &App) -> gpui::Div {
    let colors = cx.theme().colors();

    div()
        .rounded(px(8.))
        .border_1()
        .border_color(colors.line)
        .bg(colors.raised)
}

fn flag(
    id: &'static str,
    label: &'static str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Checkbox {
    let state = window.use_keyed_state(id, cx, |_, _| true);
    let checked = *state.read(cx);

    Checkbox::new(id, label)
        .toggle_state(checked)
        .on_toggle(move |value, _, cx| {
            state.update(cx, |state, cx| {
                *state = value;
                cx.notify();
            });
        })
}

fn new_pull_request(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let task = workspace.active_task();
    let base = workspace
        .data
        .project(task.project)
        .map_or(SharedString::new("main"), |project| {
            project.base_branch.clone()
        });

    let header = panel_header(
        Label::new("New pull request")
            .weight(FontWeight::MEDIUM)
            .into_any_element(),
        close_button(cx),
        cx,
    );

    v_flex()
        .size_full()
        .child(header)
        .child(
            v_flex()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .p(px(14.))
                .gap(px(14.))
                .child(branches_row(base, task.branch.clone(), cx))
                .child(title_field(cx))
                .child(description_field(cx))
                .child(local_checks())
                .child(new_pull_request_options(window, cx)),
        )
        .child(new_pull_request_footer(cx))
        .into_any_element()
}

fn branches_row(base: SharedString, branch: SharedString, cx: &App) -> impl IntoElement {
    h_flex()
        .gap(px(8.))
        .child(pill(base, cx))
        .child(mono("←").color(Color::Muted))
        .child(pill(branch, cx))
        .child(div().flex_1())
        .child(mono("5 commits").color(Color::Muted))
}

fn title_field(cx: &App) -> impl IntoElement {
    v_flex()
        .gap(px(6.))
        .child(
            Label::new("Title")
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
        .child(
            text_box(cx)
                .h(px(34.))
                .px(px(10.))
                .flex()
                .items_center()
                .child(Label::new(PULL_REQUEST_TITLE).truncate()),
        )
}

fn description_field(cx: &App) -> impl IntoElement {
    v_flex()
        .gap(px(6.))
        .child(
            h_flex()
                .child(
                    Label::new("Description")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .child(div().flex_1())
                .child(
                    Label::new("Drafted by orchestrator")
                        .size(LabelSize::Small)
                        .color(Color::Faint),
                ),
        )
        .child(
            text_box(cx)
                .h(px(240.))
                .p(px(10.))
                .overflow_hidden()
                .flex()
                .flex_col()
                .children(PULL_REQUEST_DESCRIPTION.lines().map(|line| {
                    div()
                        .h(px(18.))
                        .child(mono(line.to_owned()).color(Color::Default))
                })),
        )
}

fn local_checks() -> impl IntoElement {
    v_flex()
        .gap(px(7.))
        .child(
            Label::new("Local checks")
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
        .children(LOCAL_CHECKS.iter().map(|(command, result)| {
            h_flex()
                .gap(px(8.))
                .child(
                    Icon::new(IconName::Check)
                        .size(IconSize::Small)
                        .color(Color::Ok),
                )
                .child(Label::new(*command).mono().size(LabelSize::Small))
                .child(div().flex_1())
                .child(mono(*result).color(Color::Faint))
        }))
}

fn new_pull_request_options(window: &mut Window, cx: &mut Context<Workspace>) -> impl IntoElement {
    v_flex()
        .gap(px(8.))
        .child(flag("open-as-draft", "Open as draft", window, cx))
        .child(flag(
            "remove-worktrees-after-merge",
            "Remove agent worktrees after merge",
            window,
            cx,
        ))
}

fn new_pull_request_footer(cx: &mut Context<Workspace>) -> impl IntoElement {
    footer(cx).justify_end().child(
        Button::new("create-pr", "Create draft PR")
            .variant(ButtonVariant::Primary)
            .size(ButtonSize::Medium)
            .debug_selector(|| "create-pr-button".to_owned())
            .on_click(cx.listener(|this, _, _, cx| {
                this.data.pull_request = Some(sample_pull_request());
                cx.notify();
            })),
    )
}

fn section_heading(text: &'static str) -> Label {
    Label::new(text)
        .size(LabelSize::Small)
        .weight(FontWeight::MEDIUM)
        .color(Color::Faint)
}

fn pull_request_status(
    pull_request: &FakePullRequest,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    v_flex()
        .size_full()
        .child(status_header(pull_request, cx))
        .child(
            v_flex()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .p(px(14.))
                .gap(px(18.))
                .child(status_summary(pull_request, cx))
                .child(github_checks(pull_request))
                .child(
                    v_flex()
                        .items_start()
                        .gap(px(6.))
                        .child(section_heading("Reviewers"))
                        .child(Button::new("request-review", "Request review")),
                )
                .child(after_merge(window, cx)),
        )
        .child(status_footer(cx))
        .into_any_element()
}

fn status_header(pull_request: &FakePullRequest, cx: &mut Context<Workspace>) -> gpui::Div {
    let soft = cx.theme().colors().soft;
    let number = pull_request.number;

    let open_on_github = IconButton::new(
        "open-pull-request",
        IconName::ExternalLink,
        format!("Open pull request #{number} on GitHub"),
    )
    .on_click(move |_, _, _| log::info!("Opening pull request #{number} is not built yet"));

    panel_header(
        h_flex()
            .gap(px(8.))
            .child(
                Label::new(format!("#{number}"))
                    .mono()
                    .size(LabelSize::Small)
                    .weight(FontWeight::MEDIUM),
            )
            .child(
                div()
                    .px(px(7.))
                    .py(px(1.))
                    .rounded(px(5.))
                    .bg(soft)
                    .child(mono("Draft").weight(FontWeight::MEDIUM).color(Color::Muted)),
            )
            .into_any_element(),
        open_on_github,
        cx,
    )
}

fn status_summary(pull_request: &FakePullRequest, cx: &App) -> impl IntoElement {
    v_flex()
        .gap(px(6.))
        .child(
            div()
                .text_size(px(14.))
                .line_height(px(19.6))
                .font_weight(FontWeight::SEMIBOLD)
                .child(pull_request.title.clone()),
        )
        .child(
            h_flex()
                .gap(px(6.))
                .child(pill(pull_request.base.clone(), cx))
                .child(mono("←").color(Color::Muted))
                .child(pill(pull_request.head.clone(), cx)),
        )
}

fn github_checks(pull_request: &FakePullRequest) -> impl IntoElement {
    let finished = pull_request
        .checks
        .iter()
        .filter(|(_, state, _)| *state != CheckState::Running)
        .count();

    v_flex()
        .child(
            h_flex()
                .mb(px(4.))
                .child(section_heading("GitHub checks"))
                .child(div().flex_1())
                .child(
                    mono(format!("{finished} / {}", pull_request.checks.len())).color(Color::Faint),
                ),
        )
        .children(pull_request.checks.iter().map(|(name, state, duration)| {
            let mark = match state {
                CheckState::Running => StatusRing::new(AgentStatus::Working)
                    .size(px(9.))
                    .into_any_element(),
                CheckState::Passed => Icon::new(IconName::Check)
                    .size(IconSize::Small)
                    .color(Color::Ok)
                    .into_any_element(),
            };

            h_flex()
                .h(px(32.))
                .px(px(4.))
                .gap(px(10.))
                .child(div().w(px(12.)).flex().justify_center().child(mark))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .child(Label::new(name.clone()).size(LabelSize::Small).truncate()),
                )
                .child(mono(duration.clone()).color(Color::Faint))
        }))
}

fn after_merge(window: &mut Window, cx: &mut Context<Workspace>) -> impl IntoElement {
    v_flex()
        .gap(px(8.))
        .child(section_heading("After merge"))
        .child(flag(
            "remove-worktrees",
            "Remove agent worktrees",
            window,
            cx,
        ))
        .child(flag("close-tab", "Close this tab", window, cx))
}

fn status_footer(cx: &App) -> impl IntoElement {
    footer(cx)
        .child(
            div().flex_1().child(
                Button::new("ready-for-review", "Ready for review")
                    .size(ButtonSize::Medium)
                    .full_width(),
            ),
        )
        .child(
            div().flex_1().child(
                Button::new("merge", "Merge")
                    .variant(ButtonVariant::Primary)
                    .size(ButtonSize::Medium)
                    .full_width()
                    .disabled(true),
            ),
        )
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

        let (workspace, cx) = cx.add_window_view(Workspace::new);
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

    #[gpui::test]
    fn create_draft_pr_shows_the_status_panel(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.toggle_panel(RightPanel::PullRequest, cx);
        });
        cx.run_until_parked();

        let before = workspace.read_with(cx, |workspace, _| workspace.data.pull_request.is_none());
        click("create-pr-button", cx);

        let number = workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .pull_request
                .as_ref()
                .map(|pull_request| pull_request.number)
        });
        assert!(before);
        assert_eq!(number, Some(42));
        assert!(cx.debug_bounds("create-pr-button").is_none());
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
