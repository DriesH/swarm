//! Every focusable component names itself, so a Tab walk over one of each finds no
//! "unnamed" stop. Also the focus handle of `RailButton` and the keys of `ResizeHandle`.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Context, FocusHandle, KeyBinding, Render, TestAppContext, VisualTestContext, Window, div, px,
};
use swarm_ui::prelude::*;
use swarm_ui::test_support::{focused_stop, tab_walk};
use swarm_ui::{FocusNext, FocusPrevious, Nudge, ROOT_CONTEXT, ResizeAxis, tab_navigation};

struct Everything {
    root: FocusHandle,
    first: FocusHandle,
    second: FocusHandle,
    menu: FocusHandle,
    resize: FocusHandle,
    toast: FocusHandle,
    tile: FocusHandle,
    list_row: FocusHandle,
    input: gpui::Entity<TextInput>,
}

impl Render for Everything {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let root = v_flex()
            .id("root")
            .key_context(ROOT_CONTEXT)
            .track_focus(&self.root)
            .size_full()
            .child(
                ConfirmRow::new("confirm", "Delete task?", "Delete")
                    .track_focus(&self.first, &self.second),
            )
            .child(ListRow::new("row", "Projects").track_focus(&self.list_row))
            .child(RailButton::new("rail", "swarm", Icon::new(IconName::Plus)))
            .child(
                Menu::new("menu", "Actions")
                    .item(MenuItem::new("one", "One"))
                    .track_focus(&self.menu),
            )
            .child(OutputBlock::new("output", "Build output", "ok"))
            .child(div().relative().h(px(20.)).child(ResizeHandle::new(
                "resize",
                "Resize panel",
                &self.resize,
            )))
            .child(ContextMenu::new(
                "context",
                "Project actions",
                Button::new("trigger", "More"),
                |menu, _, _| menu,
            ))
            .child(Stepper::new("stepper", "Columns", 2, 1, 4))
            .child(Tab::new("tab", "Plan", AgentStatus::Working))
            .child(SegmentedControl::new("segments", "View", ["Tree", "List"]))
            .child(
                TabStrip::new("strip", "Sections")
                    .tab("Files")
                    .tab_with_count("Changes", "3"),
            )
            .child(Toast::new("toast", "Saved", "Plan saved").track_focus(&self.toast))
            .child(TileFrame::new("tile", IconName::Terminal, "Terminal").track_focus(&self.tile))
            .child(TabBar::new("bar", "Tasks").tab(Tab::new(
                "bar-tab",
                "Review",
                AgentStatus::Idle,
            )))
            .child(TreeRow::new("tree", "src").focusable(true))
            .child(self.input.clone())
            .child(TrayChip::new(
                "chip",
                IconName::Terminal,
                "Terminal",
                AgentStatus::Working,
            ))
            .child(TrayMore::new("more", 3));

        tab_navigation(root)
    }
}

fn bind_tab(cx: &mut TestAppContext) {
    cx.update(|cx| {
        swarm_ui::theme::init(cx);
        cx.bind_keys([
            KeyBinding::new("tab", FocusNext, Some(ROOT_CONTEXT)),
            KeyBinding::new("shift-tab", FocusPrevious, Some(ROOT_CONTEXT)),
        ]);
    });
}

fn setup(cx: &mut TestAppContext) -> (FocusHandle, &mut VisualTestContext) {
    bind_tab(cx);

    let (window, cx) = cx.add_window_view(|_, cx| {
        let input = cx.new(|cx| {
            let mut input = TextInput::new(cx);
            input.set_label("Branch name", cx);
            input
        });

        Everything {
            root: cx.focus_handle(),
            first: cx.focus_handle().tab_stop(true),
            second: cx.focus_handle().tab_stop(true),
            menu: cx.focus_handle().tab_stop(true),
            resize: cx.focus_handle().tab_stop(true),
            toast: cx.focus_handle().tab_stop(true),
            tile: cx.focus_handle().tab_stop(true),
            list_row: cx.focus_handle().tab_stop(true),
            input,
        }
    });
    let (root, rail) = window.read_with(cx, |view, _| (view.root.clone(), view.list_row.clone()));

    cx.update(|window, cx| rail.focus(window, cx));
    cx.run_until_parked();

    (root, cx)
}

#[gpui::test]
fn a_tab_walk_over_one_of_each_component_finds_no_unnamed_stop(cx: &mut TestAppContext) {
    let (root, cx) = setup(cx);
    let stops = tab_walk(cx, &root, "tab");

    let unnamed: Vec<_> = stops.iter().filter(|stop| *stop == "unnamed").collect();
    assert!(unnamed.is_empty(), "unnamed stops in {stops:?}");
}

#[gpui::test]
fn shift_tab_over_one_of_each_component_finds_no_unnamed_stop(cx: &mut TestAppContext) {
    let (root, cx) = setup(cx);
    let stops = tab_walk(cx, &root, "shift-tab");

    let unnamed: Vec<_> = stops.iter().filter(|stop| *stop == "unnamed").collect();
    assert!(unnamed.is_empty(), "unnamed stops in {stops:?}");
}

struct DialogWindow {
    root: FocusHandle,
    dialog: FocusHandle,
}

impl Render for DialogWindow {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let root = v_flex()
            .id("root")
            .key_context(ROOT_CONTEXT)
            .track_focus(&self.root)
            .size_full()
            .child(Dialog::new("dialog", "New task", &self.dialog).child(Label::new("Body")));

        tab_navigation(root)
    }
}

#[gpui::test]
fn the_dialog_close_button_is_named_close_dialog(cx: &mut TestAppContext) {
    bind_tab(cx);

    let (window, cx) = cx.add_window_view(|_, cx| DialogWindow {
        root: cx.focus_handle(),
        dialog: cx.focus_handle().tab_stop(true),
    });
    let (root, dialog) = window.read_with(cx, |view, _| (view.root.clone(), view.dialog.clone()));
    cx.update(|window, cx| dialog.focus(window, cx));
    cx.run_until_parked();

    let stops = tab_walk(cx, &root, "tab");

    assert!(
        stops.iter().any(|stop| stop == "Button Close dialog"),
        "no close button in {stops:?}"
    );
    assert!(
        !stops.iter().any(|stop| stop == "unnamed"),
        "unnamed stops in {stops:?}"
    );
}

struct RailWindow {
    rail: FocusHandle,
}

impl Render for RailWindow {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().child(
            RailButton::new("rail", "swarm", Icon::new(IconName::Plus)).track_focus(&self.rail),
        )
    }
}

#[gpui::test]
fn a_rail_button_takes_focus_from_its_handle(cx: &mut TestAppContext) {
    cx.update(swarm_ui::theme::init);

    let (window, cx) = cx.add_window_view(|_, cx| RailWindow {
        rail: cx.focus_handle().tab_stop(true),
    });
    let rail = window.read_with(cx, |view, _| view.rail.clone());
    cx.run_until_parked();

    cx.update(|window, cx| rail.focus(window, cx));
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();

    assert!(cx.update(|window, _| rail.is_focused(window)));
    assert_eq!(focused_stop(cx, &rail), "Button swarm");
}

struct ResizeWindow {
    handle: FocusHandle,
    axis: ResizeAxis,
    nudges: Rc<RefCell<Vec<Nudge>>>,
}

impl Render for ResizeWindow {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let nudges = Rc::clone(&self.nudges);

        div().relative().size(px(100.)).child(
            ResizeHandle::new("resize", "Resize panel", &self.handle)
                .axis(self.axis)
                .on_nudge(move |nudge, _, _| nudges.borrow_mut().push(nudge)),
        )
    }
}

fn nudges_for(cx: &mut TestAppContext, axis: ResizeAxis, keys: &str) -> Vec<Nudge> {
    cx.update(swarm_ui::theme::init);

    let nudges = Rc::new(RefCell::new(Vec::new()));
    let shared = Rc::clone(&nudges);
    let (window, cx) = cx.add_window_view(|_, cx| ResizeWindow {
        handle: cx.focus_handle().tab_stop(true),
        axis,
        nudges: shared,
    });
    let handle = window.read_with(cx, |view, _| view.handle.clone());
    cx.run_until_parked();

    cx.update(|window, cx| handle.focus(window, cx));
    cx.simulate_keystrokes(keys);

    nudges.borrow().clone()
}

#[gpui::test]
fn a_horizontal_handle_moves_with_left_and_right_only(cx: &mut TestAppContext) {
    assert_eq!(
        nudges_for(cx, ResizeAxis::Horizontal, "left right up down"),
        [Nudge::Left, Nudge::Right]
    );
}

#[gpui::test]
fn a_vertical_handle_moves_with_up_and_down_only(cx: &mut TestAppContext) {
    assert_eq!(
        nudges_for(cx, ResizeAxis::Vertical, "left up down right"),
        [Nudge::Left, Nudge::Right]
    );
}

#[gpui::test]
fn home_and_end_work_on_both_axes(cx: &mut TestAppContext) {
    assert_eq!(
        nudges_for(cx, ResizeAxis::Vertical, "home end"),
        [Nudge::Smallest, Nudge::Largest]
    );
}
