//! The order of the tiles in a tab, and the commands and the pointer drag that change it.
//!
//! The layout engine places tiles in input order, so the order the user chose decides where
//! each tile goes. Minimize, restore and the tray never change it.

use gpui::{App, Context, Empty, IntoElement, Render, SharedString, Window};
use swarm_layout::{Direction, TileId, TileKind, TileSpec, move_before, neighbour};

use crate::model::{TerminalId, TerminalKind};
use crate::views::tile_specs;
use crate::workspace::{View, Workspace};

/// Tiles move only among tiles of their own class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TileClass {
    User,
    Other,
}

impl TileClass {
    pub(crate) fn of(kind: TerminalKind) -> Self {
        match kind {
            TerminalKind::User => Self::User,
            _ => Self::Other,
        }
    }
}

/// Where a kind of tile goes in the default order.
fn rank(kind: TileKind) -> u8 {
    match kind {
        TileKind::Orchestrator => 0,
        TileKind::Worker => 1,
        TileKind::Process => 2,
        TileKind::User => 3,
    }
}

/// `specs` sorted by the order the user chose. Ids in `stored` that have no spec are
/// ignored. A spec that is not in `stored` joins after the last spec of its kind, or else
/// after the last spec of the kinds before it. With no stored order, `specs` stays as it is.
pub(crate) fn arranged(stored: &[TerminalId], specs: Vec<TileSpec>) -> Vec<TileSpec> {
    if stored.is_empty() {
        return specs;
    }

    let mut waiting = specs;
    let mut ordered = Vec::with_capacity(waiting.len());
    for terminal in stored {
        if let Some(position) = waiting.iter().position(|spec| spec.id.0 == terminal.0) {
            ordered.push(waiting.remove(position));
        }
    }

    for spec in waiting {
        let own = rank(spec.kind);
        let after = ordered
            .iter()
            .rposition(|placed| rank(placed.kind) == own)
            .or_else(|| ordered.iter().rposition(|placed| rank(placed.kind) < own))
            .map_or(0, |position| position + 1);

        ordered.insert(after, spec);
    }

    ordered
}

/// The payload of a drag on a tile header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TileMove(pub(crate) TerminalId);

/// The tile drag shows nothing next to the pointer.
pub(crate) struct NoGhost;

impl Render for NoGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

const MOVE_CANCELLED: &str = "Move cancelled";

fn direction_word(direction: Direction) -> &'static str {
    match direction {
        Direction::Left => "left",
        Direction::Right => "right",
        Direction::Up => "up",
        Direction::Down => "down",
    }
}

impl Workspace {
    /// The placed tiles of `class` that can move, in order. Empty outside the Terminals view
    /// and on the Start tab. The orchestrator never moves.
    pub(crate) fn movable_tiles(&self, class: TileClass) -> Vec<TileId> {
        if self.view != View::Terminals || self.active_task().is_start {
            return Vec::new();
        }

        self.layout
            .placed
            .iter()
            .filter(|(id, _)| {
                self.data
                    .terminal(TerminalId(id.0))
                    .is_some_and(|terminal| {
                        terminal.kind != TerminalKind::Orchestrator
                            && TileClass::of(terminal.kind) == class
                    })
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// The focused tile and its class, when the move commands apply to it.
    fn movable_focus(&self) -> Option<(TerminalId, TileClass)> {
        let focused = self.focused_terminal?;
        let class = TileClass::of(self.data.terminal(focused)?.kind);

        self.movable_tiles(class)
            .contains(&TileId(focused.0))
            .then_some((focused, class))
    }

    /// True when the focused tile can move, so the palette offers the move commands.
    pub(crate) fn can_move_tile(&self) -> bool {
        self.movable_focus().is_some()
    }

    /// True when `terminal` is a placed tile that can move: not the orchestrator, in the
    /// Terminals view, on a task tab.
    pub(crate) fn is_movable(&self, terminal: TerminalId) -> bool {
        self.data.terminal(terminal).is_some_and(|entry| {
            self.movable_tiles(TileClass::of(entry.kind))
                .contains(&TileId(terminal.0))
        })
    }

    /// The name the tile header and the tray chip show.
    pub(crate) fn tile_name(&self, terminal: TerminalId) -> SharedString {
        self.data
            .terminal(terminal)
            .map(|entry| entry.title.clone())
            .unwrap_or_default()
    }

    /// Says `text` in the tile live region. Each call makes a new region, so a screen reader
    /// also speaks a text that did not change.
    pub(crate) fn say_tile_status(
        &mut self,
        text: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.tile_status = text.into();
        self.tile_status_serial = self.tile_status_serial.wrapping_add(1);
        cx.notify();
    }

    /// Moves the focused tile to the place of its neighbour on the `direction` side.
    pub(crate) fn move_tile(
        &mut self,
        direction: Direction,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((focused, class)) = self.movable_focus() else {
            return;
        };
        let movable = self.movable_tiles(class);

        let Some(target) = neighbour(&self.layout, TileId(focused.0), direction, &movable) else {
            self.say_tile_status(format!("Can't move {}", direction_word(direction)), cx);
            return;
        };

        self.move_tile_before(focused, target, cx);
    }

    /// Gives `moved` the place of `target` in the order, says its new position and focuses it.
    fn move_tile_before(&mut self, moved: TerminalId, target: TileId, cx: &mut Context<Self>) {
        let Some(class) = self
            .data
            .terminal(moved)
            .map(|terminal| TileClass::of(terminal.kind))
        else {
            return;
        };
        let moved_tile = TileId(moved.0);

        let mut order: Vec<TileId> = tile_specs(self).iter().map(|spec| spec.id).collect();
        move_before(&mut order, moved_tile, target);
        let task = self.active_task().id;
        self.tile_order
            .insert(task, order.iter().map(|id| TerminalId(id.0)).collect());

        let mut among = self.movable_tiles(class);
        move_before(&mut among, moved_tile, target);
        let position = among
            .iter()
            .position(|id| *id == moved_tile)
            .map_or(1, |at| at + 1);
        let name = self.tile_name(moved);
        self.say_tile_status(
            format!("{name} moved, position {position} of {}", among.len()),
            cx,
        );

        self.claim_focus(moved, cx);
    }

    /// The tile being dragged, while GPUI still runs the drag.
    pub(crate) fn dragged_tile(&self, cx: &App) -> Option<TerminalId> {
        self.tile_drag.filter(|_| cx.has_active_drag())
    }

    pub(crate) fn start_tile_drag(&mut self, terminal: TerminalId, cx: &mut Context<Self>) {
        self.tile_drag = Some(terminal);
        cx.notify();
    }

    /// True when a drop of `dragged` on `target` reorders: both are movable tiles of one
    /// class, and not the same tile.
    pub(crate) fn accepts_tile_drop(&self, dragged: TerminalId, target: TerminalId) -> bool {
        let class = |terminal: TerminalId| {
            self.data
                .terminal(terminal)
                .map(|entry| TileClass::of(entry.kind))
        };

        dragged != target
            && class(dragged) == class(target)
            && self.is_movable(dragged)
            && self.is_movable(target)
    }

    /// A drop on the tile of `target`.
    pub(crate) fn drop_tile_on(
        &mut self,
        dragged: TerminalId,
        target: TerminalId,
        cx: &mut Context<Self>,
    ) {
        if !self.accepts_tile_drop(dragged, target) {
            self.tile_drag_ended(cx);
            return;
        }

        self.tile_drag = None;
        self.move_tile_before(dragged, TileId(target.0), cx);
    }

    /// A drop on the tray minimizes the tile. Focus goes to the next movable tile, else the
    /// previous one, else the orchestrator (M7).
    pub(crate) fn drop_tile_on_tray(&mut self, dragged: TerminalId, cx: &mut Context<Self>) {
        if !self.is_movable(dragged) {
            self.tile_drag_ended(cx);
            return;
        }

        self.tile_drag = None;
        let next = self.focus_after_leaving(dragged);

        self.minimize(dragged, cx);
        let name = self.tile_name(dragged);
        self.say_tile_status(format!("{name} moved to tray"), cx);

        if let Some(next) = next {
            self.claim_focus(next, cx);
        }
    }

    /// Esc during a tile drag stops it. Nothing changes.
    pub(crate) fn cancel_tile_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_active_drag(window);
        self.tile_drag_ended(cx);
    }

    /// A mouse up ends every drag. When no drop took the tile drag, it is cancelled once the
    /// drop listeners have run.
    pub(crate) fn end_tile_drag_after_release(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.tile_drag.is_some() {
            cx.defer_in(window, |workspace, _, cx| workspace.tile_drag_ended(cx));
        }
    }

    /// The tile drag ended without a move: nothing changes and the dragged tile keeps the
    /// focus.
    fn tile_drag_ended(&mut self, cx: &mut Context<Self>) {
        let Some(dragged) = self.tile_drag.take() else {
            return;
        };

        self.say_tile_status(MOVE_CANCELLED, cx);

        let target = if self.is_movable(dragged) {
            Some(dragged)
        } else {
            self.focus_after_leaving(dragged)
        };
        if let Some(target) = target {
            self.claim_focus(target, cx);
        }
    }

    /// Where focus goes when `terminal` leaves the layout: the next movable tile in the
    /// order, else the previous one, else the orchestrator.
    fn focus_after_leaving(&self, terminal: TerminalId) -> Option<TerminalId> {
        let movable: Vec<TerminalId> = self
            .layout
            .placed
            .iter()
            .map(|(id, _)| TerminalId(id.0))
            .filter(|id| *id == terminal || self.is_movable(*id))
            .collect();
        let beside = movable
            .iter()
            .position(|id| *id == terminal)
            .and_then(|at| {
                movable
                    .get(at + 1)
                    .or_else(|| at.checked_sub(1).and_then(|before| movable.get(before)))
            });
        let orchestrator = self
            .active_task()
            .orchestrator
            .and_then(|agent| self.data.agent(agent))
            .map(|agent| agent.terminal);

        beside
            .or_else(|| movable.iter().find(|id| **id != terminal))
            .copied()
            .or(orchestrator)
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Entity, TestAppContext, VisualTestContext};

    use super::*;
    use crate::actions::{
        self, MoveTileDown, MoveTileLeft, MoveTileRight, MoveTileUp, NewTerminal,
    };
    use crate::daemon::tests::redraw;
    use crate::model::TaskId;

    fn spec(id: u32, kind: TileKind) -> TileSpec {
        TileSpec::new(TileId(id), kind, swarm_layout::Activity::Idle)
    }

    fn stored(ids: &[u32]) -> Vec<TerminalId> {
        ids.iter().map(|id| TerminalId(*id)).collect()
    }

    fn order_of(specs: &[TileSpec]) -> Vec<u32> {
        specs.iter().map(|spec| spec.id.0).collect()
    }

    #[test]
    fn an_empty_stored_order_keeps_the_input() {
        let specs = vec![
            spec(1, TileKind::Orchestrator),
            spec(2, TileKind::Worker),
            spec(3, TileKind::Process),
        ];

        assert_eq!(order_of(&arranged(&[], specs)), [1, 2, 3]);
    }

    #[test]
    fn the_stored_order_is_followed() {
        let specs = vec![
            spec(1, TileKind::Orchestrator),
            spec(2, TileKind::Worker),
            spec(3, TileKind::Worker),
            spec(4, TileKind::Process),
        ];

        let result = arranged(&stored(&[1, 3, 4, 2]), specs);

        assert_eq!(order_of(&result), [1, 3, 4, 2]);
    }

    #[test]
    fn a_new_worker_joins_after_the_last_worker() {
        let specs = vec![
            spec(1, TileKind::Orchestrator),
            spec(2, TileKind::Worker),
            spec(3, TileKind::Worker),
            spec(4, TileKind::Process),
            spec(5, TileKind::Worker),
        ];

        let result = arranged(&stored(&[1, 3, 2, 4]), specs);

        assert_eq!(order_of(&result), [1, 3, 2, 5, 4]);
    }

    #[test]
    fn the_first_process_goes_after_the_last_worker() {
        let specs = vec![
            spec(1, TileKind::Orchestrator),
            spec(2, TileKind::Worker),
            spec(3, TileKind::Worker),
            spec(4, TileKind::Process),
        ];

        let result = arranged(&stored(&[1, 3, 2]), specs);

        assert_eq!(order_of(&result), [1, 3, 2, 4]);
    }

    #[test]
    fn the_first_user_terminal_goes_after_the_last_process() {
        let specs = vec![
            spec(1, TileKind::Orchestrator),
            spec(2, TileKind::Process),
            spec(3, TileKind::User),
        ];

        let result = arranged(&stored(&[1, 2]), specs);

        assert_eq!(order_of(&result), [1, 2, 3]);
    }

    #[test]
    fn a_closed_tile_leaves_the_order() {
        let specs = vec![spec(1, TileKind::Orchestrator), spec(3, TileKind::Worker)];

        let result = arranged(&stored(&[1, 2, 3]), specs);

        assert_eq!(order_of(&result), [1, 3]);
    }

    #[test]
    fn a_minimized_tile_keeps_its_index() {
        let mut hidden = spec(3, TileKind::Worker);
        hidden.minimized = true;
        let specs = vec![
            spec(1, TileKind::Orchestrator),
            spec(2, TileKind::Worker),
            hidden,
            spec(4, TileKind::Worker),
        ];

        let result = arranged(&stored(&[1, 4, 3, 2]), specs);

        assert_eq!(order_of(&result), [1, 4, 3, 2]);
        assert!(result[2].minimized);
    }

    fn setup(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
        cx.update(|cx| {
            swarm_ui::theme::init(cx);
            actions::bind_keys(cx);
        });

        let (workspace, cx) = cx.add_window_view(Workspace::with_sample);
        cx.run_until_parked();
        redraw(cx);

        (workspace, cx)
    }

    fn status(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> String {
        workspace.read_with(cx, |workspace, _| workspace.tile_status.to_string())
    }

    fn focused(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> Option<TerminalId> {
        workspace.read_with(cx, |workspace, _| workspace.focused_terminal)
    }

    fn focus(workspace: &Entity<Workspace>, terminal: TerminalId, cx: &mut VisualTestContext) {
        workspace.update(cx, |workspace, cx| workspace.focus_terminal(terminal, cx));
        redraw(cx);
    }

    /// The ids of the stored or default order of the open tab.
    fn current_order(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> Vec<u32> {
        workspace.read_with(cx, |workspace, _| order_of(&tile_specs(workspace)))
    }

    fn workers(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> Vec<TerminalId> {
        workspace.read_with(cx, |workspace, _| {
            let task = workspace.active_task();
            task.workers
                .iter()
                .filter_map(|id| workspace.data.agent(*id))
                .map(|agent| agent.terminal)
                .filter(|terminal| workspace.layout.rect(TileId(terminal.0)).is_some())
                .collect()
        })
    }

    fn dispatch(direction: Direction, cx: &mut VisualTestContext) {
        match direction {
            Direction::Left => cx.dispatch_action(MoveTileLeft),
            Direction::Right => cx.dispatch_action(MoveTileRight),
            Direction::Up => cx.dispatch_action(MoveTileUp),
            Direction::Down => cx.dispatch_action(MoveTileDown),
        }
        redraw(cx);
    }

    #[gpui::test]
    fn processes_go_after_agents(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let kinds: Vec<TileKind> = workspace.read_with(cx, |workspace, _| {
            tile_specs(workspace).iter().map(|spec| spec.kind).collect()
        });

        let first_process = kinds.iter().position(|kind| *kind == TileKind::Process);
        let last_worker = kinds.iter().rposition(|kind| *kind == TileKind::Worker);
        assert!(last_worker < first_process);
    }

    #[gpui::test]
    fn a_move_command_reorders_the_focused_tile_and_keeps_its_focus(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let workers = workers(&workspace, cx);
        let before = current_order(&workspace, cx);
        focus(&workspace, workers[0], cx);

        let mut moved = false;
        for direction in [Direction::Right, Direction::Down] {
            dispatch(direction, cx);
            moved = current_order(&workspace, cx) != before;
            if moved {
                break;
            }
        }

        assert!(moved, "the first worker has no neighbour right or below");
        assert!(status(&workspace, cx).starts_with(&format!(
            "{} moved, position ",
            workspace.read_with(cx, |workspace, _| {
                workspace
                    .data
                    .terminal(workers[0])
                    .map(|terminal| terminal.title.to_string())
                    .unwrap_or_default()
            })
        )));
        assert_eq!(focused(&workspace, cx), Some(workers[0]));
    }

    #[gpui::test]
    fn the_order_follows_the_move_of_one_worker_past_its_neighbour(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let workers = workers(&workspace, cx);
        focus(&workspace, workers[0], cx);
        let target = workspace.read_with(cx, |workspace, _| {
            let movable = workspace.movable_tiles(TileClass::Other);
            [Direction::Right, Direction::Down]
                .into_iter()
                .find_map(|direction| {
                    neighbour(&workspace.layout, TileId(workers[0].0), direction, &movable)
                        .map(|target| (direction, target))
                })
        });
        let (direction, target) = target.expect("a neighbour exists");
        let mut expected: Vec<TileId> = current_order(&workspace, cx)
            .into_iter()
            .map(TileId)
            .collect();
        move_before(&mut expected, TileId(workers[0].0), target);

        dispatch(direction, cx);

        let expected: Vec<u32> = expected.into_iter().map(|id| id.0).collect();
        assert_eq!(current_order(&workspace, cx), expected);
    }

    #[gpui::test]
    fn the_move_keys_work_while_a_tile_has_keyboard_focus(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let workers = workers(&workspace, cx);
        focus(&workspace, workers[0], cx);

        for (keys, direction) in [
            ("ctrl-cmd-left", Direction::Left),
            ("ctrl-cmd-right", Direction::Right),
            ("ctrl-cmd-up", Direction::Up),
            ("ctrl-cmd-down", Direction::Down),
        ] {
            let has_neighbour = workspace.read_with(cx, |workspace, _| {
                let movable = workspace.movable_tiles(TileClass::Other);
                neighbour(&workspace.layout, TileId(workers[0].0), direction, &movable).is_some()
            });
            let before = current_order(&workspace, cx);

            cx.simulate_keystrokes(keys);
            redraw(cx);

            let moved = current_order(&workspace, cx) != before;
            assert_eq!(moved, has_neighbour, "{keys}");
            assert_eq!(
                status(&workspace, cx).starts_with("Can't move"),
                !has_neighbour,
                "{keys}"
            );
            assert_eq!(focused(&workspace, cx), Some(workers[0]));

            workspace.update(cx, |workspace, _| workspace.tile_order.clear());
            redraw(cx);
        }
    }

    #[gpui::test]
    fn no_neighbour_says_so_and_changes_nothing(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let workers = workers(&workspace, cx);
        focus(&workspace, workers[0], cx);
        let before = current_order(&workspace, cx);

        dispatch(Direction::Up, cx);

        assert_eq!(status(&workspace, cx), "Can't move up");
        assert_eq!(current_order(&workspace, cx), before);
    }

    #[gpui::test]
    fn the_orchestrator_does_not_move(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let orchestrator = workspace.read_with(cx, |workspace, _| {
            let agent = workspace.active_task().orchestrator;
            agent
                .and_then(|id| workspace.data.agent(id))
                .map(|agent| agent.terminal)
        });
        focus(&workspace, orchestrator.expect("an orchestrator"), cx);
        let before = current_order(&workspace, cx);

        dispatch(Direction::Right, cx);

        assert_eq!(status(&workspace, cx), "");
        assert_eq!(current_order(&workspace, cx), before);
    }

    #[gpui::test]
    fn nothing_moves_on_the_start_tab(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let workers = workers(&workspace, cx);
        focus(&workspace, workers[0], cx);
        cx.dispatch_action(actions::PlanWithOrchestrator);
        redraw(cx);

        dispatch(Direction::Right, cx);

        assert_eq!(status(&workspace, cx), "");
        assert!(workspace.read_with(cx, |workspace, _| workspace.tile_order.is_empty()));
    }

    #[gpui::test]
    fn a_tile_in_the_tray_does_not_move(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let workers = workers(&workspace, cx);
        workspace.update(cx, |workspace, cx| {
            workspace.minimize(workers[0], cx);
            workspace.focused_terminal = Some(workers[0]);
        });
        redraw(cx);

        dispatch(Direction::Right, cx);

        assert_eq!(status(&workspace, cx), "");
        assert!(workspace.read_with(cx, |workspace, _| workspace.tile_order.is_empty()));
    }

    #[gpui::test]
    fn a_user_terminal_moves_only_among_user_terminals(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.dispatch_action(NewTerminal);
        cx.dispatch_action(NewTerminal);
        redraw(cx);
        let users: Vec<TerminalId> = workspace.read_with(cx, |workspace, _| {
            workspace.active_task().user_terminals.clone()
        });
        assert_eq!(users.len(), 2);
        let before = current_order(&workspace, cx);
        focus(&workspace, users[0], cx);

        dispatch(Direction::Down, cx);

        let order = current_order(&workspace, cx);
        let is_user = |id: &u32| users.iter().any(|user| user.0 == *id);
        let others = |order: &[u32]| {
            order
                .iter()
                .copied()
                .filter(|id| !is_user(id))
                .collect::<Vec<_>>()
        };
        let user_ids = |order: &[u32]| order.iter().copied().filter(is_user).collect::<Vec<_>>();
        assert_eq!(others(&order), others(&before));
        assert_ne!(user_ids(&order), user_ids(&before));
        assert!(status(&workspace, cx).ends_with("position 2 of 2"));
    }

    #[gpui::test]
    fn the_move_commands_are_in_the_palette_only_with_a_movable_tile_in_the_terminals_view(
        cx: &mut TestAppContext,
    ) {
        use crate::palette::rows;

        let (workspace, cx) = setup(cx);
        let workers = workers(&workspace, cx);
        let has_rows = |workspace: &Workspace| {
            rows(
                &workspace.data,
                workspace.active_project,
                workspace.active_task,
                workspace.view,
                workspace.can_move_tile(),
            )
            .iter()
            .any(|row| row.label() == "Move tile left")
        };

        focus(&workspace, workers[0], cx);
        assert!(workspace.read_with(cx, |workspace, _| has_rows(workspace)));

        workspace.update(cx, |workspace, cx| {
            workspace.set_view(View::Swarm, cx);
        });
        assert!(!workspace.read_with(cx, |workspace, _| has_rows(workspace)));

        workspace.update(cx, |workspace, cx| {
            workspace.set_view(View::Terminals, cx);
            workspace.select_task(TaskId(0), cx);
        });
        let orchestrator = workspace.read_with(cx, |workspace, _| {
            workspace
                .active_task()
                .orchestrator
                .and_then(|id| workspace.data.agent(id))
                .map(|agent| agent.terminal)
        });
        focus(&workspace, orchestrator.expect("an orchestrator"), cx);
        assert!(!workspace.read_with(cx, |workspace, _| has_rows(workspace)));
    }

    mod drag {
        use gpui::{Bounds, Modifiers, MouseButton, Pixels, Point, point, px};

        use super::*;

        /// The sample workspace in a window that places at least four workers and processes.
        fn wide(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
            let (workspace, cx) = setup(cx);
            cx.simulate_resize(gpui::size(px(2400.), px(1500.)));
            redraw(cx);
            redraw(cx);

            (workspace, cx)
        }

        fn bounds(selector: String, cx: &mut VisualTestContext) -> Bounds<Pixels> {
            let selector: &'static str = Box::leak(selector.into_boxed_str());

            cx.debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} is not drawn"))
        }

        /// A point on the tile header, left of its actions.
        fn header(terminal: TerminalId, cx: &mut VisualTestContext) -> Point<Pixels> {
            let area = bounds(format!("tile-{}-header", terminal.0), cx);

            point(area.left() + px(40.), area.center().y)
        }

        fn body(terminal: TerminalId, cx: &mut VisualTestContext) -> Point<Pixels> {
            let slot = bounds(format!("tile-slot-{}", terminal.0), cx);

            point(slot.center().x, slot.bottom() - px(20.))
        }

        /// Presses on `from` and moves past the drag threshold, then over `to`.
        fn drag(from: Point<Pixels>, to: Point<Pixels>, cx: &mut VisualTestContext) {
            cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
            cx.simulate_mouse_move(
                from + point(px(6.), px(6.)),
                MouseButton::Left,
                Modifiers::none(),
            );
            redraw(cx);
            cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
            redraw(cx);
        }

        fn release(at: Point<Pixels>, cx: &mut VisualTestContext) {
            cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
            redraw(cx);
        }

        fn drag_and_drop(from: Point<Pixels>, to: Point<Pixels>, cx: &mut VisualTestContext) {
            drag(from, to, cx);
            release(to, cx);
        }

        /// The placed tiles that can move, workers and processes, in order.
        fn movable(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> Vec<TerminalId> {
            workspace.read_with(cx, |workspace, _| {
                workspace
                    .movable_tiles(TileClass::Other)
                    .into_iter()
                    .map(|id| TerminalId(id.0))
                    .collect()
            })
        }

        /// `ids` in the order the tab keeps them.
        fn order_among(
            ids: &[TerminalId],
            workspace: &Entity<Workspace>,
            cx: &VisualTestContext,
        ) -> Vec<TerminalId> {
            current_order(workspace, cx)
                .into_iter()
                .map(TerminalId)
                .filter(|id| ids.contains(id))
                .collect()
        }

        /// `ids` in the order the layout places them.
        fn placed_among(
            ids: &[TerminalId],
            workspace: &Entity<Workspace>,
            cx: &VisualTestContext,
        ) -> Vec<TerminalId> {
            workspace.read_with(cx, |workspace, _| {
                workspace
                    .layout
                    .placed
                    .iter()
                    .map(|(id, _)| TerminalId(id.0))
                    .filter(|id| ids.contains(id))
                    .collect()
            })
        }

        fn orchestrator(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> TerminalId {
            workspace.read_with(cx, |workspace, _| {
                workspace
                    .active_task()
                    .orchestrator
                    .and_then(|id| workspace.data.agent(id))
                    .map_or(TerminalId(u32::MAX), |agent| agent.terminal)
            })
        }

        fn new_user_terminal(
            workspace: &Entity<Workspace>,
            cx: &mut VisualTestContext,
        ) -> TerminalId {
            cx.dispatch_action(NewTerminal);
            redraw(cx);

            workspace.read_with(cx, |workspace, _| {
                workspace
                    .active_task()
                    .user_terminals
                    .last()
                    .copied()
                    .unwrap_or(TerminalId(u32::MAX))
            })
        }

        fn is_dragging(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
            cx.update(|_, cx| workspace.read(cx).dragged_tile(cx).is_some())
        }

        fn has_keyboard_focus(
            workspace: &Entity<Workspace>,
            terminal: TerminalId,
            cx: &mut VisualTestContext,
        ) -> bool {
            cx.update(|window, cx| {
                workspace
                    .read(cx)
                    .terminal_focus
                    .get(&terminal)
                    .is_some_and(|handle| handle.is_focused(window))
            })
        }

        fn four(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> [TerminalId; 4] {
            let movable = movable(workspace, cx);
            assert!(movable.len() >= 4, "the sample places {movable:?}");

            [movable[0], movable[1], movable[2], movable[3]]
        }

        #[gpui::test]
        fn a_drop_on_a_later_tile_puts_the_tile_after_it(cx: &mut TestAppContext) {
            let (workspace, cx) = wide(cx);
            let [a, b, c, d] = four(&workspace, cx);

            drag_and_drop(header(a, cx), body(c, cx), cx);

            let ids = [a, b, c, d];
            assert_eq!(order_among(&ids, &workspace, cx), [b, c, a, d]);
            assert_eq!(placed_among(&ids, &workspace, cx), [b, c, a, d]);
        }

        #[gpui::test]
        fn a_drop_on_an_earlier_tile_puts_the_tile_before_it(cx: &mut TestAppContext) {
            let (workspace, cx) = wide(cx);
            let [a, b, c, d] = four(&workspace, cx);

            drag_and_drop(header(d, cx), body(b, cx), cx);

            let ids = [a, b, c, d];
            assert_eq!(order_among(&ids, &workspace, cx), [a, d, b, c]);
            assert_eq!(placed_among(&ids, &workspace, cx), [a, d, b, c]);
        }

        #[gpui::test]
        fn a_drop_says_the_new_position_and_focuses_the_dragged_tile(cx: &mut TestAppContext) {
            let (workspace, cx) = wide(cx);
            let [a, _, c, _] = four(&workspace, cx);
            let count = movable(&workspace, cx).len();
            let name = workspace.read_with(cx, |workspace, _| workspace.tile_name(a));

            drag_and_drop(header(a, cx), body(c, cx), cx);

            assert_eq!(
                status(&workspace, cx),
                format!("{name} moved, position 3 of {count}")
            );
            assert_eq!(focused(&workspace, cx), Some(a));
            assert!(has_keyboard_focus(&workspace, a, cx));
        }

        #[gpui::test]
        fn a_drop_on_the_orchestrator_changes_nothing(cx: &mut TestAppContext) {
            let (workspace, cx) = wide(cx);
            let [a, ..] = four(&workspace, cx);
            let orchestrator = orchestrator(&workspace, cx);
            let before = current_order(&workspace, cx);

            drag_and_drop(header(a, cx), body(orchestrator, cx), cx);

            assert_eq!(current_order(&workspace, cx), before);
            assert_eq!(status(&workspace, cx), "Move cancelled");
            assert_eq!(focused(&workspace, cx), Some(a));
        }

        #[gpui::test]
        fn a_drop_on_empty_space_changes_nothing(cx: &mut TestAppContext) {
            let (workspace, cx) = wide(cx);
            let [a, ..] = four(&workspace, cx);
            let orchestrator = bounds(format!("tile-slot-{}", orchestrator(&workspace, cx).0), cx);
            let before = current_order(&workspace, cx);
            // The gap right of the orchestrator.
            let gap = point(orchestrator.right() + px(4.), orchestrator.center().y);

            drag_and_drop(header(a, cx), gap, cx);

            assert_eq!(current_order(&workspace, cx), before);
            assert_eq!(status(&workspace, cx), "Move cancelled");
        }

        #[gpui::test]
        fn a_user_terminal_does_not_drop_on_a_worker(cx: &mut TestAppContext) {
            let (workspace, cx) = wide(cx);
            let user = new_user_terminal(&workspace, cx);
            let [a, ..] = four(&workspace, cx);
            let before = current_order(&workspace, cx);

            drag_and_drop(header(user, cx), body(a, cx), cx);

            assert_eq!(current_order(&workspace, cx), before);
            assert_eq!(status(&workspace, cx), "Move cancelled");
            assert_eq!(focused(&workspace, cx), Some(user));
        }

        #[gpui::test]
        fn a_worker_does_not_drop_on_a_user_terminal(cx: &mut TestAppContext) {
            let (workspace, cx) = wide(cx);
            let user = new_user_terminal(&workspace, cx);
            let [a, ..] = four(&workspace, cx);
            let before = current_order(&workspace, cx);

            drag_and_drop(header(a, cx), body(user, cx), cx);

            assert_eq!(current_order(&workspace, cx), before);
            assert_eq!(status(&workspace, cx), "Move cancelled");
        }

        #[gpui::test]
        fn a_drop_on_the_tray_minimizes_and_focuses_the_tile_beside_it(cx: &mut TestAppContext) {
            let (workspace, cx) = wide(cx);
            let [a, ..] = four(&workspace, cx);
            let beside = workspace.read_with(cx, |workspace, _| workspace.neighbor_tile(a));
            let name = workspace.read_with(cx, |workspace, _| workspace.tile_name(a));
            let tray = bounds("tray".to_owned(), cx);

            drag_and_drop(header(a, cx), tray.center(), cx);

            assert!(workspace.read_with(cx, |workspace, _| workspace.minimized.contains(&a)));
            assert_eq!(status(&workspace, cx), format!("{name} moved to tray"));
            assert!(beside.is_some());
            assert_eq!(focused(&workspace, cx), beside);
        }

        #[gpui::test]
        fn esc_cancels_the_drag_and_does_not_reach_the_terminal(cx: &mut TestAppContext) {
            let (workspace, cx) = wide(cx);
            let (channels, emulator) = swarm_term::channels();
            let worker = workers(&workspace, cx)[0];
            workspace.update_in(cx, |workspace, window, cx| {
                let live = workspace.live_terminal(
                    worker,
                    swarm_protocol::TerminalId(900),
                    "worker".into(),
                    channels,
                    window,
                    cx,
                );
                workspace.live_terminals.insert(worker, live);
                cx.notify();
            });
            redraw(cx);
            let keys = || {
                emulator
                    .input
                    .try_iter()
                    .filter(|input| matches!(input, swarm_term::TerminalInput::Key(_)))
                    .count()
            };
            let before = current_order(&workspace, cx);
            let target = movable(&workspace, cx)[2];

            drag(header(worker, cx), body(target, cx), cx);
            cx.simulate_keystrokes("escape");
            redraw(cx);
            let during = keys();
            let cancelled = status(&workspace, cx);
            let still_dragging = is_dragging(&workspace, cx);
            release(body(target, cx), cx);

            cx.simulate_keystrokes("escape");
            let after = keys();

            assert_eq!(during, 0, "the terminal got the Esc of the drag");
            assert_eq!(cancelled, "Move cancelled");
            assert!(!still_dragging);
            assert_eq!(current_order(&workspace, cx), before);
            assert_eq!(focused(&workspace, cx), Some(worker));
            assert_eq!(after, 1, "Esc after the drag reaches the terminal");
        }

        #[gpui::test]
        fn a_release_outside_the_window_cancels(cx: &mut TestAppContext) {
            let (workspace, cx) = wide(cx);
            let [a, _, c, _] = four(&workspace, cx);
            let before = current_order(&workspace, cx);

            drag(header(a, cx), body(c, cx), cx);
            release(point(px(-40.), px(-40.)), cx);

            assert_eq!(current_order(&workspace, cx), before);
            assert_eq!(status(&workspace, cx), "Move cancelled");
            assert_eq!(focused(&workspace, cx), Some(a));
            assert!(workspace.read_with(cx, |workspace, _| workspace.tile_drag.is_none()));
        }

        #[gpui::test]
        fn a_release_outside_the_tile_area_cancels(cx: &mut TestAppContext) {
            let (workspace, cx) = wide(cx);
            let [a, ..] = four(&workspace, cx);
            let before = current_order(&workspace, cx);
            let sidebar = point(px(40.), px(300.));

            drag_and_drop(header(a, cx), sidebar, cx);

            assert_eq!(current_order(&workspace, cx), before);
            assert_eq!(status(&workspace, cx), "Move cancelled");
        }

        #[gpui::test]
        fn a_cancel_after_the_dragged_tile_left_focuses_a_drawn_tile(cx: &mut TestAppContext) {
            let (workspace, cx) = wide(cx);
            let [a, ..] = four(&workspace, cx);
            let sidebar = point(px(40.), px(300.));

            drag(header(a, cx), sidebar, cx);
            workspace.update(cx, |workspace, cx| workspace.minimize(a, cx));
            redraw(cx);
            release(sidebar, cx);

            let focused = focused(&workspace, cx).expect("a focused tile");
            assert_ne!(focused, a);
            assert!(movable(&workspace, cx).contains(&focused));
        }

        #[gpui::test]
        fn a_release_on_the_dragged_tile_cancels(cx: &mut TestAppContext) {
            let (workspace, cx) = wide(cx);
            let [a, ..] = four(&workspace, cx);
            let before = current_order(&workspace, cx);

            drag_and_drop(header(a, cx), body(a, cx), cx);

            assert_eq!(current_order(&workspace, cx), before);
            assert_eq!(status(&workspace, cx), "Move cancelled");
            assert_eq!(focused(&workspace, cx), Some(a));
            assert!(has_keyboard_focus(&workspace, a, cx));
        }

        #[gpui::test]
        fn less_than_the_drag_threshold_is_a_click(cx: &mut TestAppContext) {
            let (workspace, cx) = wide(cx);
            let [a, b, ..] = four(&workspace, cx);
            focus(&workspace, a, cx);
            let before = current_order(&workspace, cx);
            let start = header(b, cx);

            cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
            cx.simulate_mouse_move(
                start + point(px(1.), px(1.)),
                MouseButton::Left,
                Modifiers::none(),
            );
            let dragging = is_dragging(&workspace, cx);
            release(start + point(px(1.), px(1.)), cx);

            assert!(!dragging);
            assert_eq!(focused(&workspace, cx), Some(b));
            assert_eq!(current_order(&workspace, cx), before);
            assert_eq!(status(&workspace, cx), "");
        }

        #[gpui::test]
        fn a_press_on_a_header_button_does_not_start_a_drag(cx: &mut TestAppContext) {
            let (workspace, cx) = wide(cx);
            let [a, _, c, _] = four(&workspace, cx);
            let button = bounds(format!("tile-minimize-{}", a.0), cx).center();

            drag(button, body(c, cx), cx);
            let dragging = cx.update(|_, cx| cx.has_active_drag()) || is_dragging(&workspace, cx);
            release(body(c, cx), cx);

            assert!(!dragging);
            assert!(workspace.read_with(cx, |workspace, _| workspace.tile_order.is_empty()));
        }

        #[gpui::test]
        fn the_orchestrator_does_not_start_a_drag(cx: &mut TestAppContext) {
            let (workspace, cx) = wide(cx);
            let [a, ..] = four(&workspace, cx);
            let orchestrator = bounds(format!("tile-slot-{}", orchestrator(&workspace, cx).0), cx);

            drag(
                point(orchestrator.left() + px(60.), orchestrator.top() + px(17.)),
                body(a, cx),
                cx,
            );
            let dragging = cx.update(|_, cx| cx.has_active_drag());
            release(body(a, cx), cx);

            assert!(!dragging);
            assert!(workspace.read_with(cx, |workspace, _| workspace.tile_order.is_empty()));
        }

        #[gpui::test]
        fn a_start_tab_tile_does_not_start_a_drag(cx: &mut TestAppContext) {
            let (workspace, cx) = wide(cx);
            cx.dispatch_action(actions::PlanWithOrchestrator);
            redraw(cx);
            let user = new_user_terminal(&workspace, cx);
            let slot = bounds(format!("tile-slot-{}", user.0), cx);
            let start = point(slot.left() + px(60.), slot.top() + px(17.));

            drag(start, start + point(px(0.), px(200.)), cx);
            let dragging = cx.update(|_, cx| cx.has_active_drag());
            release(start + point(px(0.), px(200.)), cx);

            assert!(!dragging);
            assert!(
                cx.debug_bounds(Box::leak(
                    format!("tile-{}-header", user.0).into_boxed_str()
                ))
                .is_none()
            );
        }

        #[gpui::test]
        fn the_order_survives_a_minimize_and_a_restore(cx: &mut TestAppContext) {
            let (workspace, cx) = wide(cx);
            let [a, _, c, _] = four(&workspace, cx);
            drag_and_drop(header(a, cx), body(c, cx), cx);
            let dropped = current_order(&workspace, cx);

            workspace.update(cx, |workspace, cx| workspace.minimize(a, cx));
            redraw(cx);
            workspace.update(cx, |workspace, cx| workspace.restore(a, cx));
            redraw(cx);

            assert_eq!(current_order(&workspace, cx), dropped);
        }

        #[gpui::test]
        fn the_dot_grid_shows_during_a_drag_and_hides_after_a_drop(cx: &mut TestAppContext) {
            let (workspace, cx) = wide(cx);
            let [a, _, c, _] = four(&workspace, cx);
            assert!(cx.debug_bounds("tile-dot-grid").is_none());

            drag(header(a, cx), body(c, cx), cx);
            let during = cx.debug_bounds("tile-dot-grid");
            release(body(c, cx), cx);

            assert!(during.is_some());
            assert!(cx.debug_bounds("tile-dot-grid").is_none());
        }

        #[gpui::test]
        fn the_dot_grid_hides_after_a_cancel(cx: &mut TestAppContext) {
            let (workspace, cx) = wide(cx);
            let [a, _, c, _] = four(&workspace, cx);

            drag(header(a, cx), body(c, cx), cx);
            let during = cx.debug_bounds("tile-dot-grid");
            cx.simulate_keystrokes("escape");
            redraw(cx);

            assert!(during.is_some());
            assert!(cx.debug_bounds("tile-dot-grid").is_none());
            assert!(!is_dragging(&workspace, cx));
        }

        #[gpui::test]
        fn a_valid_target_gets_the_accent_outline_and_the_orchestrator_none(
            cx: &mut TestAppContext,
        ) {
            use crate::daemon::tests::drawn_quad;

            let (workspace, cx) = wide(cx);
            let [a, _, c, _] = four(&workspace, cx);
            let orchestrator = orchestrator(&workspace, cx);
            let accent = cx.update(|_, cx| swarm_ui::ActiveTheme::theme(cx).colors().accent);
            let outline = |terminal: TerminalId, cx: &mut VisualTestContext| {
                let selector: &'static str =
                    Box::leak(format!("tile-drop-{}", terminal.0).into_boxed_str());
                drawn_quad(selector, cx).map(|quad| quad.border_color)
            };

            drag(header(a, cx), body(c, cx), cx);
            let on_target = outline(c, cx);
            let over = body(orchestrator, cx);
            cx.simulate_mouse_move(over, MouseButton::Left, Modifiers::none());
            redraw(cx);
            let on_orchestrator = outline(orchestrator, cx);
            release(body(orchestrator, cx), cx);

            assert_eq!(on_target, Some(accent));
            assert_ne!(on_orchestrator, Some(accent));
        }

        #[gpui::test]
        fn a_repeated_status_makes_a_new_live_region(cx: &mut TestAppContext) {
            let (workspace, cx) = wide(cx);
            let workers = workers(&workspace, cx);
            focus(&workspace, workers[0], cx);
            let serial = |cx: &VisualTestContext| {
                workspace.read_with(cx, |workspace, _| workspace.tile_status_serial)
            };

            dispatch(Direction::Up, cx);
            let first = serial(cx);
            dispatch(Direction::Up, cx);

            assert_eq!(status(&workspace, cx), "Can't move up");
            assert_ne!(serial(cx), first);
        }
    }
}
