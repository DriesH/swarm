//! Tab bar: the project's tasks, the view switch and the panel toggles.

use gpui::{ClickEvent, Context, SharedString, Window, px};
use swarm_ui::prelude::*;

use crate::actions::{NewTask, PlanWithOrchestrator};
use crate::model::{Task, TaskId, TaskStatus};
use crate::window_drag::drag_region;
use crate::workspace::{RightPanel, View, Workspace};

const VIEWS: [(View, &str); 3] = [
    (View::Terminals, "Terminals"),
    (View::Swarm, "Swarm"),
    (View::Review, "Review"),
];

/// The least room left for dragging the window by the header when tabs fill it.
const MIN_DRAG_WIDTH: gpui::Pixels = px(32.);

pub(crate) fn render(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui::AnyElement {
    if workspace.has_no_projects() {
        return h_flex()
            .size_full()
            .px(px(10.))
            .child(drag_region("tab-bar-drag", window, cx).flex_1().h_full())
            .into_any_element();
    }

    let task = workspace.active_task();
    let stat = workspace.data.live_stat(task.id);
    let panel = workspace.visible_panel();
    let panels_disabled = workspace.view == View::Swarm;
    let pull_request = workspace.pull_request_toggle_label(task.id);

    h_flex()
        .size_full()
        .px(px(10.))
        .gap(px(4.))
        .child(task_tabs(workspace, cx))
        .child(
            drag_region("tab-bar-drag", window, cx)
                .debug_selector(|| "tab-bar-drag".to_owned())
                .flex_1()
                .h_full()
                .min_w(MIN_DRAG_WIDTH),
        )
        .child(
            h_flex()
                .flex_none()
                .gap(px(4.))
                .child(view_control(workspace, cx))
                .when(!task.is_start, |controls| {
                    controls
                        .child(panel_toggle(
                            "files",
                            "Files",
                            IconName::File,
                            panel == Some(RightPanel::Files),
                            panels_disabled,
                            cx.listener(|workspace, _, _, cx| {
                                workspace.toggle_panel(RightPanel::Files, cx);
                            }),
                        ))
                        .child(
                            panel_toggle(
                                "changes",
                                "Changes",
                                IconName::Branch,
                                panel == Some(RightPanel::Changes),
                                panels_disabled,
                                cx.listener(|workspace, _, _, cx| {
                                    workspace.toggle_panel(RightPanel::Changes, cx);
                                }),
                            )
                            .end_slot(changes_count(stat.additions, stat.deletions)),
                        )
                        .children(pull_request.map(|label| {
                            panel_toggle(
                                "pull-request",
                                label,
                                IconName::PullRequest,
                                panel == Some(RightPanel::PullRequest),
                                panels_disabled,
                                cx.listener(|workspace, _, _, cx| {
                                    workspace.toggle_panel(RightPanel::PullRequest, cx);
                                }),
                            )
                        }))
                }),
        )
        .into_any_element()
}

fn task_tabs(workspace: &Workspace, cx: &mut Context<Workspace>) -> TabBar {
    let tasks: Vec<&Task> = workspace.data.tasks_of(workspace.active_project).collect();
    let ids: Vec<TaskId> = tasks.iter().map(|task| task.id).collect();
    let selected = ids
        .iter()
        .position(|id| *id == workspace.active_task)
        .unwrap_or(0);
    let workspace_handle = cx.entity();

    TabBar::new("task-tabs", "Tasks")
        .overflow_label("All tasks")
        .debug_selector(|| "task-tabs".to_owned())
        .tabs(tasks.into_iter().map(|task| task_tab(workspace, task, cx)))
        .selected(selected)
        .trailing(new_tab_menu())
        .on_select(move |index, _, cx| {
            let Some(id) = ids.get(index).copied() else {
                return;
            };

            workspace_handle.update(cx, |workspace, cx| workspace.select_task(id, cx));
        })
}

fn view_control(workspace: &Workspace, cx: &mut Context<Workspace>) -> SegmentedControl {
    let selected_view = VIEWS
        .iter()
        .position(|(view, _)| *view == workspace.view)
        .unwrap_or(0);
    let workspace_handle = cx.entity();

    SegmentedControl::new("view", "View", VIEWS.map(|(_, label)| label))
        .selected(selected_view)
        .debug_selector(|| "view-control".to_owned())
        .on_select(move |index, _, cx| {
            let Some((view, _)) = VIEWS.get(index) else {
                return;
            };

            workspace_handle.update(cx, |workspace, cx| workspace.set_view(*view, cx));
        })
}

pub(crate) fn ring_status(status: TaskStatus) -> AgentStatus {
    match status {
        TaskStatus::Working => AgentStatus::Working,
        TaskStatus::NeedsYou => AgentStatus::Blocked,
        TaskStatus::Idle => AgentStatus::Idle,
    }
}

fn task_tab(workspace: &Workspace, task: &Task, cx: &mut Context<Workspace>) -> Tab {
    let id = task.id;

    Tab::new(
        ("task", id.0 as usize),
        workspace.data.tab_label(task),
        ring_status(task.status),
    )
    .debug_selector(move || format!("tab-{}", id.0))
    .when(!task.is_start, |tab| {
        tab.on_close(cx.listener(move |workspace, _, window, cx| {
            workspace.open_close_task(id, window, cx);
        }))
    })
}

fn new_tab_menu() -> PopoverMenu {
    PopoverMenu::new(
        "new-tab-menu",
        IconButton::new("new-tab", IconName::Plus, "New tab").size(IconButtonSize::Medium),
        |_, _| {
            Menu::new("new-tab-items", "New tab")
                .item(
                    MenuItem::new("new-task", "New task")
                        .icon(IconName::PlusSquare)
                        .on_select(|window, cx| window.dispatch_action(Box::new(NewTask), cx)),
                )
                .item(
                    MenuItem::new("plan-with-orchestrator", "Plan with orchestrator")
                        .icon(IconName::Bee)
                        .on_select(|window, cx| {
                            window.dispatch_action(Box::new(PlanWithOrchestrator), cx);
                        }),
                )
        },
    )
}

fn panel_toggle(
    id: &'static str,
    label: impl Into<SharedString>,
    icon: IconName,
    open: bool,
    disabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    Button::new(id, label)
        .variant(ButtonVariant::Ghost)
        .icon(icon)
        .toggle_state(open)
        .disabled(disabled)
        .debug_selector(move || format!("{id}-button"))
        .on_click(on_click)
}

fn changes_count(additions: u32, deletions: u32) -> impl IntoElement {
    h_flex()
        .gap(px(4.))
        .child(
            Label::new(format!("+{additions}"))
                .mono()
                .size(LabelSize::XSmall)
                .color(Color::Add),
        )
        .child(
            Label::new(format!("\u{2212}{deletions}"))
                .mono()
                .size(LabelSize::XSmall)
                .color(Color::Del),
        )
}

#[cfg(test)]
mod tests {
    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::model::AgentKind;
    use crate::workspace::HEADER_CENTER;
    use swarm_ui::TAB_MIN_WIDTH;

    fn setup(cx: &mut TestAppContext) -> (gpui::Entity<Workspace>, &mut VisualTestContext) {
        cx.update(swarm_ui::theme::init);

        cx.add_window_view(Workspace::with_sample)
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx.debug_bounds(selector);
        assert!(bounds.is_some(), "{selector} is not on screen");

        cx.simulate_click(
            bounds.map_or_else(Default::default, |bounds| bounds.center()),
            Modifiers::none(),
        );
    }

    #[gpui::test]
    fn clicking_a_tab_selects_the_task(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        click("tab-1", cx);

        let active = workspace.read_with(cx, |workspace, _| workspace.active_task);
        assert_eq!(active, TaskId(1));
    }

    #[gpui::test]
    fn picking_a_view_segment_changes_the_view(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let bounds = cx.debug_bounds("view-control");
        assert!(bounds.is_some());
        let review = bounds.map_or_else(Default::default, |bounds| {
            gpui::point(bounds.right() - px(14.), bounds.center().y)
        });
        cx.simulate_click(review, Modifiers::none());

        let view = workspace.read_with(cx, |workspace, _| workspace.view);
        assert_eq!(view, View::Review);
    }

    #[gpui::test]
    fn panel_buttons_report_their_toggled_state(cx: &mut TestAppContext) {
        use gpui::Element as _;
        use gpui::accesskit::{Node, Role as AccessRole, Toggled};

        let (_, cx) = setup(cx);

        let toggled = |open: bool, cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                let button =
                    panel_toggle("files", "Files", IconName::File, open, false, |_, _, _| {});
                let element = button.render(window, cx).into_element();
                let mut node = Node::new(AccessRole::Unknown);
                element.write_a11y_info(&mut node);

                node.toggled()
            })
        };

        assert_eq!(toggled(true, cx), Some(Toggled::True));
        assert_eq!(toggled(false, cx), Some(Toggled::False));
    }

    #[gpui::test]
    fn the_panel_buttons_do_nothing_in_the_swarm_view(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| workspace.set_view(View::Swarm, cx));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));

        click("files-button", cx);

        let panel = workspace.read_with(cx, |workspace, _| workspace.right_panel);
        assert_eq!(panel, Some(RightPanel::Changes));
    }

    #[gpui::test]
    fn a_plan_task_has_no_panel_buttons(cx: &mut TestAppContext) {
        let (_, cx) = setup(cx);
        assert!(cx.debug_bounds("files-button").is_some());

        cx.dispatch_action(PlanWithOrchestrator);
        cx.run_until_parked();

        assert!(cx.debug_bounds("files-button").is_none());
        assert!(cx.debug_bounds("changes-button").is_none());
    }

    #[gpui::test]
    fn clicking_changes_toggles_the_panel(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let before = workspace.read_with(cx, |workspace, _| workspace.right_panel);

        click("changes-button", cx);

        let after = workspace.read_with(cx, |workspace, _| workspace.right_panel);
        assert_eq!(before, Some(RightPanel::Changes));
        assert_eq!(after, None);
    }

    fn drag_from(start: gpui::Point<gpui::Pixels>, cx: &mut VisualTestContext) {
        cx.simulate_mouse_down(start, gpui::MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            start + gpui::point(px(6.), px(0.)),
            gpui::MouseButton::Left,
            Modifiers::none(),
        );
    }

    fn center_of(selector: &'static str, cx: &mut VisualTestContext) -> gpui::Point<gpui::Pixels> {
        let bounds = cx.debug_bounds(selector);
        assert!(bounds.is_some(), "{selector} is not on screen");

        bounds.map_or_else(Default::default, |bounds| bounds.center())
    }

    fn empty_header_point(cx: &mut VisualTestContext) -> gpui::Point<gpui::Pixels> {
        center_of("tab-bar-drag", cx)
    }

    // The test platform panics in `start_window_move`, so a panic means a move began.
    #[gpui::test]
    #[should_panic(expected = "not implemented")]
    fn dragging_empty_header_space_moves_the_window(cx: &mut TestAppContext) {
        let (_, cx) = setup(cx);
        let point = empty_header_point(cx);

        drag_from(point, cx);
    }

    #[gpui::test]
    fn dragging_a_tab_does_not_move_the_window(cx: &mut TestAppContext) {
        let (_, cx) = setup(cx);
        let point = center_of("tab-1", cx);

        drag_from(point, cx);
    }

    #[gpui::test]
    fn dragging_buttons_does_not_move_the_window(cx: &mut TestAppContext) {
        let (_, cx) = setup(cx);

        for selector in ["view-control", "files-button", "changes-button"] {
            let point = center_of(selector, cx);
            drag_from(point, cx);
        }
    }

    #[gpui::test]
    fn moving_over_empty_header_space_without_a_press_does_not_move_the_window(
        cx: &mut TestAppContext,
    ) {
        let (_, cx) = setup(cx);
        let point = empty_header_point(cx);

        cx.simulate_mouse_move(point, gpui::MouseButton::Left, Modifiers::none());
    }

    const TASK_NAMES: [&str; 20] = [
        "terminal-renderer",
        "menu-bar-usage",
        "worktree-scripts",
        "ui-tokens",
        "daemon-restart",
        "review-comments",
        "swarm-graph",
        "palette-search",
        "settings-pages",
        "diff-split-view",
        "keyboard-shortcuts",
        "usage-cards",
        "pull-request-flow",
        "agent-limits",
        "status-bar",
        "markdown-render",
        "close-task-dialog",
        "tray-chips",
        "fonts-and-icons",
        "release-notes",
    ];

    /// Gives the open project `count` tasks, keeping the open one, in a window `width` wide.
    fn show_tasks(
        workspace: &gpui::Entity<Workspace>,
        count: usize,
        width: f32,
        cx: &mut VisualTestContext,
    ) {
        workspace.update(cx, |workspace, cx| {
            let project = workspace.active_project;
            let keep = workspace.active_task;
            let others: Vec<TaskId> = workspace
                .data
                .tasks_of(project)
                .map(|task| task.id)
                .filter(|id| *id != keep)
                .collect();

            for id in others {
                workspace.data.remove_task(id);
            }

            for name in TASK_NAMES.iter().take(count.saturating_sub(1)) {
                workspace
                    .data
                    .add_task(project, name, AgentKind::ClaudeCode, Vec::new());
            }

            cx.notify();
        });

        cx.simulate_resize(gpui::size(px(width), px(800.)));
        settle(cx);
    }

    /// Draws until the tab bar stops asking for another frame.
    fn settle(cx: &mut VisualTestContext) {
        for _ in 0..8 {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
    }

    fn bounds_of(selector: &'static str, cx: &mut VisualTestContext) -> gpui::Bounds<gpui::Pixels> {
        let bounds = cx.debug_bounds(selector);
        assert!(bounds.is_some(), "{selector} is not on screen");

        bounds.unwrap_or_default()
    }

    fn close(a: gpui::Pixels, b: gpui::Pixels) -> bool {
        (a - b).abs() < px(1.5)
    }

    fn level(a: gpui::Pixels, b: gpui::Pixels) -> bool {
        (a - b).abs() < px(0.5)
    }

    fn same_bounds(a: gpui::Bounds<gpui::Pixels>, b: gpui::Bounds<gpui::Pixels>) -> bool {
        level(a.left(), b.left())
            && level(a.top(), b.top())
            && level(a.right(), b.right())
            && level(a.bottom(), b.bottom())
    }

    /// The tabs of the sample's open project, first to last.
    fn tab_selectors(
        workspace: &gpui::Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Vec<String> {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .tasks_of(workspace.active_project)
                .map(|task| format!("tab-{}", task.id.0))
                .collect()
        })
    }

    fn tab_bounds(
        workspace: &gpui::Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Vec<gpui::Bounds<gpui::Pixels>> {
        tab_selectors(workspace, cx)
            .into_iter()
            // Leaked on purpose: `debug_bounds` takes a `&'static str` and this is a test.
            .map(|selector| bounds_of(Box::leak(selector.into_boxed_str()), cx))
            .collect()
    }

    const RIGHT_CONTROLS: [&str; 3] = ["view-control", "files-button", "changes-button"];
    const WIDTHS: [f32; 2] = [1360., 1100.];

    fn inside(tab: gpui::Bounds<gpui::Pixels>, row: gpui::Bounds<gpui::Pixels>) -> bool {
        tab.left() >= row.left() - px(0.5) && tab.right() <= row.right() + px(0.5)
    }

    fn task_ids(workspace: &gpui::Entity<Workspace>, cx: &mut VisualTestContext) -> Vec<TaskId> {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .tasks_of(workspace.active_project)
                .map(|task| task.id)
                .collect()
        })
    }

    fn select(workspace: &gpui::Entity<Workspace>, task: TaskId, cx: &mut VisualTestContext) {
        workspace.update(cx, |workspace, cx| workspace.select_task(task, cx));
        settle(cx);
    }

    fn scroll_row(
        delta: gpui::Point<gpui::Pixels>,
        modifiers: Modifiers,
        cx: &mut VisualTestContext,
    ) {
        let position = center_of("task-tabs", cx);

        cx.simulate_event(gpui::ScrollEvent {
            position,
            delta: gpui::ScrollDelta::Pixels(delta),
            modifiers,
            touch_phase: gpui::TouchPhase::Moved,
            source: gpui::ScrollSource::Pointer,
        });
        settle(cx);
    }

    #[gpui::test]
    fn the_right_controls_keep_their_bounds_with_any_number_of_tasks(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        for width in WIDTHS {
            show_tasks(&workspace, 1, width, cx);
            let expected = RIGHT_CONTROLS.map(|selector| bounds_of(selector, cx));

            for count in [6, 20, 1] {
                show_tasks(&workspace, count, width, cx);

                for (selector, before) in RIGHT_CONTROLS.into_iter().zip(expected) {
                    let after = bounds_of(selector, cx);

                    assert!(
                        same_bounds(before, after),
                        "{selector} moved with {count} tasks at {width} px: {before:?} -> {after:?}"
                    );
                    assert!(after.right() <= px(width), "{selector} leaves the window");
                }
            }
        }
    }

    #[gpui::test]
    fn the_tab_row_never_reaches_the_right_controls(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        for width in WIDTHS {
            for count in [1, 6, 20] {
                show_tasks(&workspace, count, width, cx);

                let row = bounds_of("task-tabs", cx);
                let controls = bounds_of("view-control", cx);

                assert!(
                    row.right() <= controls.left(),
                    "{count} tasks at {width} px"
                );
            }
        }
    }

    #[gpui::test]
    fn tabs_keep_their_width_while_there_is_room(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        show_tasks(&workspace, 1, 1360., cx);
        let alone = tab_bounds(&workspace, cx)[0].size.width;
        show_tasks(&workspace, 2, 2400., cx);
        let with_room = tab_bounds(&workspace, cx)[0].size.width;

        assert!(close(alone, with_room), "{alone:?} vs {with_room:?}");
        assert!(cx.debug_bounds("task-tabs-overflow").is_none());
    }

    fn widths(
        workspace: &gpui::Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Vec<gpui::Pixels> {
        tab_bounds(workspace, cx)
            .iter()
            .map(|tab| tab.size.width)
            .collect()
    }

    #[gpui::test]
    fn tabs_shrink_to_the_minimum_then_the_row_scrolls(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        show_tasks(&workspace, 4, 2400., cx);
        let natural = widths(&workspace, cx);

        // 1460 px: less room than the four tabs want, more than they need at their minimum.
        show_tasks(&workspace, 4, 1460., cx);
        let row = bounds_of("task-tabs", cx);
        let squeezed = widths(&workspace, cx);
        let tabs = tab_bounds(&workspace, cx);

        assert!(
            squeezed
                .iter()
                .zip(&natural)
                .all(|(now, natural)| *now <= *natural + px(0.5))
        );
        assert!(
            squeezed
                .iter()
                .zip(&natural)
                .any(|(now, natural)| *now < *natural - px(1.))
        );
        assert!(squeezed.iter().all(|now| *now >= TAB_MIN_WIDTH - px(0.5)));
        assert!(
            tabs.iter().all(|tab| inside(*tab, row)),
            "nothing scrolls yet"
        );
        assert!(cx.debug_bounds("task-tabs-overflow").is_none());

        for count in [6, 20] {
            show_tasks(&workspace, count, 2400., cx);
            let natural = widths(&workspace, cx);

            show_tasks(&workspace, count, 1360., cx);
            let row = bounds_of("task-tabs", cx);
            let tabs = tab_bounds(&workspace, cx);

            for (index, (tab, natural)) in tabs.iter().zip(&natural).enumerate() {
                let least = TAB_MIN_WIDTH.min(*natural);
                assert!(
                    close(tab.size.width, least),
                    "tab {index} of {count}: {tab:?} vs {least:?}"
                );
            }
            assert!(
                tabs.last().is_some_and(|tab| tab.right() > row.right()),
                "{count} tasks"
            );
        }
    }

    #[gpui::test]
    fn a_short_name_keeps_its_natural_width_in_a_crowded_row(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        show_tasks(&workspace, 20, 2400., cx);
        cx.dispatch_action(PlanWithOrchestrator);
        settle(cx);
        let natural = widths(&workspace, cx).pop().unwrap_or_default();

        cx.simulate_resize(gpui::size(px(1360.), px(800.)));
        settle(cx);
        let crowded = widths(&workspace, cx).pop().unwrap_or_default();

        assert!(natural < TAB_MIN_WIDTH, "the Plan tab is {natural:?} wide");
        assert!(close(crowded, natural), "{crowded:?} vs {natural:?}");
    }

    #[gpui::test]
    fn a_tab_holds_no_room_for_a_close_button_it_does_not_show(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        // The first two tabs have the same name, so only the close button differs.
        show_tasks(&workspace, 2, 2400., cx);

        let [selected, other] = widths(&workspace, cx)[..] else {
            panic!("two tabs");
        };

        // The button (24), its gap (8) and the end padding (4) against the plain end padding (12).
        assert!(
            close(selected - other, px(24.)),
            "{selected:?} vs {other:?}"
        );
    }

    #[gpui::test]
    fn the_close_button_floats_over_the_label_of_a_hovered_tab(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        show_tasks(&workspace, 3, 2400., cx);
        let ids = task_ids(&workspace, cx);
        let other = format!("tab-{}", ids[1].0);
        let other: &'static str = Box::leak(other.into_boxed_str());
        let close_selector: &'static str = Box::leak(format!("{other}-close").into_boxed_str());

        let before = bounds_of(other, cx);
        let point = before.center();
        cx.simulate_mouse_move(point, None, Modifiers::none());
        settle(cx);

        let tab = bounds_of(other, cx);
        let button = bounds_of(close_selector, cx);

        assert!(same_bounds(before, tab), "hovering moved the tab");
        assert!(button.left() > tab.left() + px(40.) && button.right() < tab.right());
        assert!(
            button.left() < tab.right() - px(28.),
            "it covers the end of the label"
        );
        assert!(level(button.center().y, tab.center().y));
    }

    #[gpui::test]
    fn the_tab_row_scrolls_with_the_wheel_and_shows_fades(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        show_tasks(&workspace, 20, 1360., cx);

        let start = tab_bounds(&workspace, cx)[0].left();
        assert!(cx.debug_bounds("task-tabs-fade-start").is_none());
        assert!(cx.debug_bounds("task-tabs-fade-end").is_some());

        // A trackpad swipe, a mouse wheel and a shift-wheel.
        let swipe = gpui::point(px(-60.), px(0.));
        let wheel = gpui::point(px(0.), px(-60.));
        let mut previous = start;

        for (delta, modifiers) in [
            (swipe, Modifiers::none()),
            (wheel, Modifiers::none()),
            (wheel, Modifiers::shift()),
        ] {
            scroll_row(delta, modifiers, cx);

            let left = tab_bounds(&workspace, cx)[0].left();
            assert!(
                left < previous - px(30.),
                "{delta:?} {modifiers:?}: {previous:?} -> {left:?}"
            );
            previous = left;
        }

        assert!(cx.debug_bounds("task-tabs-fade-start").is_some());

        scroll_row(gpui::point(px(-5000.), px(0.)), Modifiers::none(), cx);
        assert!(cx.debug_bounds("task-tabs-fade-start").is_some());
        assert!(cx.debug_bounds("task-tabs-fade-end").is_none());

        scroll_row(gpui::point(px(5000.), px(0.)), Modifiers::none(), cx);
        assert!(cx.debug_bounds("task-tabs-fade-start").is_none());
        assert!(cx.debug_bounds("task-tabs-fade-end").is_some());
    }

    #[gpui::test]
    fn selecting_a_hidden_task_scrolls_its_tab_into_view(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        show_tasks(&workspace, 20, 1360., cx);
        let ids = task_ids(&workspace, cx);

        for index in [19, 9, 0, 14, 15, 3] {
            select(&workspace, ids[index], cx);

            let row = bounds_of("task-tabs", cx);
            let tab = tab_bounds(&workspace, cx)[index];

            assert!(
                inside(tab, row),
                "tab {index} is hidden: {tab:?} in {row:?}"
            );
        }
    }

    #[gpui::test]
    fn a_new_tab_scrolls_into_view(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        show_tasks(&workspace, 20, 1360., cx);

        cx.dispatch_action(PlanWithOrchestrator);
        settle(cx);

        let ids = task_ids(&workspace, cx);
        let active = workspace.read_with(cx, |workspace, _| workspace.active_task);
        let row = bounds_of("task-tabs", cx);
        let last = tab_bounds(&workspace, cx).pop();

        assert_eq!(ids.last(), Some(&active));
        assert!(last.is_some_and(|tab| inside(tab, row)));
    }

    #[gpui::test]
    fn the_overflow_button_shows_only_when_the_tabs_overflow(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        for (count, overflows) in [
            (1, false),
            (3, false),
            (4, true),
            (20, true),
            (3, false),
            (1, false),
        ] {
            show_tasks(&workspace, count, 1360., cx);

            assert_eq!(
                cx.debug_bounds("task-tabs-overflow").is_some(),
                overflows,
                "{count} tasks"
            );
        }
    }

    #[gpui::test]
    fn the_overflow_menu_lists_the_tasks_and_selects_one(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        show_tasks(&workspace, 20, 1360., cx);
        let ids = task_ids(&workspace, cx);
        assert!(cx.debug_bounds("task-tabs-overflow-menu").is_none());

        click("task-tabs-overflow", cx);
        settle(cx);

        let menu = bounds_of("task-tabs-overflow-menu", cx);
        let row = px(34.);
        let pick = gpui::point(
            menu.center().x,
            menu.top() + px(1. + 5.) + row * 4. + row / 2.,
        );
        cx.simulate_click(pick, Modifiers::none());
        settle(cx);

        let active = workspace.read_with(cx, |workspace, _| workspace.active_task);
        assert_eq!(active, ids[4]);
        assert!(
            cx.debug_bounds("task-tabs-overflow-menu").is_none(),
            "the menu closes"
        );
        assert!(inside(
            tab_bounds(&workspace, cx)[4],
            bounds_of("task-tabs", cx)
        ));
    }

    #[gpui::test]
    fn the_overflow_menu_reaches_every_task_with_the_keyboard(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.update(|_, cx| {
            cx.bind_keys([
                gpui::KeyBinding::new("down", swarm_ui::SelectNext, Some(swarm_ui::MENU_CONTEXT)),
                gpui::KeyBinding::new("enter", swarm_ui::Confirm, Some(swarm_ui::MENU_CONTEXT)),
            ]);
        });
        show_tasks(&workspace, 20, 1360., cx);
        let ids = task_ids(&workspace, cx);

        click("task-tabs-overflow", cx);
        settle(cx);

        for _ in 0..19 {
            cx.simulate_keystrokes("down");
        }
        cx.simulate_keystrokes("enter");
        settle(cx);

        let active = workspace.read_with(cx, |workspace, _| workspace.active_task);
        assert_eq!(active, ids[19]);
        assert!(inside(
            tab_bounds(&workspace, cx)[19],
            bounds_of("task-tabs", cx)
        ));
    }

    #[gpui::test]
    #[should_panic(expected = "not implemented")]
    fn with_many_tasks_the_header_still_moves_the_window(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        show_tasks(&workspace, 20, 1100., cx);
        assert!(bounds_of("tab-bar-drag", cx).size.width >= px(24.));

        let point = empty_header_point(cx);
        drag_from(point, cx);
    }

    #[gpui::test]
    fn dragging_the_overflow_button_or_a_crowded_tab_does_not_move_the_window(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        show_tasks(&workspace, 20, 1100., cx);

        for selector in ["task-tabs-overflow", "tab-0"] {
            let point = center_of(selector, cx);
            drag_from(point, cx);
        }
    }

    #[gpui::test]
    fn the_traffic_lights_the_sidebar_toggle_and_the_tabs_share_one_center(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        show_tasks(&workspace, 3, 1360., cx);

        let toggle = bounds_of("toggle-sidebar", cx).center().y;
        let tab = tab_bounds(&workspace, cx)[0].center().y;
        let lights = crate::traffic_light_position().y + crate::TRAFFIC_LIGHT_SIZE / 2.;

        assert!(level(toggle, HEADER_CENTER), "sidebar toggle at {toggle:?}");
        assert!(level(tab, HEADER_CENTER), "tabs at {tab:?}");
        assert!(level(lights, HEADER_CENTER), "traffic lights at {lights:?}");
    }

    #[gpui::test]
    fn tabbing_to_a_hidden_tab_scrolls_it_into_view(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.update(|_, cx| {
            cx.bind_keys([gpui::KeyBinding::new(
                "tab",
                swarm_ui::FocusNext,
                Some(swarm_ui::ROOT_CONTEXT),
            )]);
        });
        show_tasks(&workspace, 20, 1360., cx);
        let start = tab_bounds(&workspace, cx)[0].left();

        let mut scrolled_to = Vec::new();
        for _ in 0..70 {
            cx.simulate_keystrokes("tab");
            settle(cx);

            scrolled_to.push(tab_bounds(&workspace, cx)[0].left());
        }

        // Focus walked through every tab, so the row must have followed it to the end.
        let furthest = scrolled_to.iter().copied().fold(start, Ord::min);
        let hidden = tab_bounds(&workspace, cx)[19].right() - bounds_of("task-tabs", cx).left();
        assert!(
            furthest < start - px(1000.),
            "{furthest:?} vs {start:?} ({hidden:?})"
        );
    }
}
