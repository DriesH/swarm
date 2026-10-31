//! Tab bar: the project's tasks, the view switch and the panel toggles.

use gpui::{ClickEvent, Context, Role, Window, px};
use swarm_ui::prelude::*;

use crate::actions::{NewTask, PlanWithOrchestrator};
use crate::model::{Task, TaskStatus};
use crate::window_drag::drag_region;
use crate::workspace::{RightPanel, View, Workspace};

const VIEWS: [(View, &str); 3] = [
    (View::Terminals, "Terminals"),
    (View::Swarm, "Swarm"),
    (View::Review, "Review"),
];

pub(crate) fn render(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui::AnyElement {
    let tabs: Vec<_> = workspace
        .data
        .tasks_of(workspace.active_project)
        .map(|task| task_tab(workspace, task, cx))
        .collect();

    let selected_view = VIEWS
        .iter()
        .position(|(view, _)| *view == workspace.view)
        .unwrap_or(0);
    let workspace_handle = cx.entity();

    let view_control = SegmentedControl::new("view", "View", VIEWS.map(|(_, label)| label))
        .selected(selected_view)
        .debug_selector(|| "view-control".to_owned())
        .on_select(move |index, _, cx| {
            let Some((view, _)) = VIEWS.get(index) else {
                return;
            };

            workspace_handle.update(cx, |workspace, cx| workspace.set_view(*view, cx));
        });

    if workspace.has_no_projects() {
        return h_flex()
            .size_full()
            .px(px(10.))
            .child(drag_region("tab-bar-drag", window, cx).flex_1().h_full())
            .into_any_element();
    }

    let task = workspace.active_task();
    let panel = workspace.visible_panel();

    h_flex()
        .size_full()
        .px(px(10.))
        .gap(px(4.))
        .child(
            h_flex()
                .id("task-tabs")
                .role(Role::TabList)
                .aria_label("Tasks")
                .gap(px(4.))
                .children(tabs),
        )
        .child(new_tab_menu())
        .child(drag_region("tab-bar-drag", window, cx).flex_1().h_full())
        .child(view_control)
        .when(!task.is_start, |bar| {
            bar.child(panel_toggle(
                "files",
                "Files",
                IconName::File,
                panel == Some(RightPanel::Files),
                cx.listener(|workspace, _, _, cx| workspace.toggle_panel(RightPanel::Files, cx)),
            ))
            .child(
                panel_toggle(
                    "changes",
                    "Changes",
                    IconName::Branch,
                    panel == Some(RightPanel::Changes),
                    cx.listener(|workspace, _, _, cx| {
                        workspace.toggle_panel(RightPanel::Changes, cx);
                    }),
                )
                .end_slot(changes_count(task.additions, task.deletions)),
            )
        })
        .into_any_element()
}

pub(crate) fn ring_status(status: TaskStatus) -> AgentStatus {
    match status {
        TaskStatus::Working => AgentStatus::Working,
        TaskStatus::NeedsYou => AgentStatus::Blocked,
        TaskStatus::Idle => AgentStatus::Idle,
    }
}

fn task_tab(
    workspace: &Workspace,
    task: &Task,
    cx: &mut Context<Workspace>,
) -> impl IntoElement + use<> {
    let id = task.id;

    Tab::new(
        ("task", id.0 as usize),
        workspace.data.tab_label(task),
        ring_status(task.status),
    )
    .debug_selector(move || format!("tab-{}", id.0))
    .toggle_state(workspace.active_task == id)
    .on_click(cx.listener(move |workspace, _, _, cx| workspace.select_task(id, cx)))
}

fn new_tab_menu() -> impl IntoElement {
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
    label: &'static str,
    icon: IconName,
    open: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    Button::new(id, label)
        .variant(ButtonVariant::Ghost)
        .icon(icon)
        .toggle_state(open)
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
    use crate::model::TaskId;

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
                let button = panel_toggle("files", "Files", IconName::File, open, |_, _, _| {});
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
        let bounds = cx.debug_bounds("view-control");
        assert!(bounds.is_some());

        bounds.map_or_else(Default::default, |bounds| {
            gpui::point(bounds.left() - px(40.), bounds.center().y)
        })
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
}
