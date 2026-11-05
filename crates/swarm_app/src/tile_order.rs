//! The order of the tiles in a tab, and the commands that change it.
//!
//! The layout engine places tiles in input order, so the order the user chose decides where
//! each tile goes. Minimize, restore and the tray never change it.

use gpui::{Context, SharedString, Window};
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
    fn of(kind: TerminalKind) -> Self {
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
        let moved = TileId(focused.0);
        let movable = self.movable_tiles(class);

        let Some(target) = neighbour(&self.layout, moved, direction, &movable) else {
            self.tile_status = format!("Can't move {}", direction_word(direction)).into();
            cx.notify();
            return;
        };

        let mut order: Vec<TileId> = tile_specs(self).iter().map(|spec| spec.id).collect();
        move_before(&mut order, moved, target);
        let task = self.active_task().id;
        self.tile_order
            .insert(task, order.iter().map(|id| TerminalId(id.0)).collect());

        let mut among = movable;
        move_before(&mut among, moved, target);
        let position = among
            .iter()
            .position(|id| *id == moved)
            .map_or(1, |at| at + 1);
        let name = self
            .data
            .terminal(focused)
            .map(|terminal| terminal.title.clone())
            .unwrap_or_default();
        self.tile_status = SharedString::from(format!(
            "{name} moved, position {position} of {}",
            among.len()
        ));

        self.claim_focus(focused, cx);
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
}
