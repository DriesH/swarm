//! Helpers for the order of tiles: move one before another, find the tile beside one.

use crate::{Layout, Rect, TileId};

/// A side of a tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// Moves `moved` to the index of `target`; the tiles in between shift by one.
///
/// `[a b c d]`, move `a` to `c`: `[b c a d]`. Move `d` to `b`: `[a d b c]`.
/// Nothing changes when `moved == target` or either is not in `order`.
pub fn move_before(order: &mut Vec<TileId>, moved: TileId, target: TileId) {
    if moved == target {
        return;
    }

    let from = order.iter().position(|id| *id == moved);
    let to = order.iter().position(|id| *id == target);
    let (Some(from), Some(to)) = (from, to) else {
        return;
    };

    let tile = order.remove(from);
    order.insert(to, tile);
}

/// The nearest tile in `movable` whose center lies on the `direction` side of `from`'s
/// center (strictly).
///
/// Nearest means the smallest distance between centers on the axis of `direction`. Ties
/// break by the distance on the other axis, then by input order (`layout.placed` order).
/// `None` when `from` is not placed or no tile qualifies. `from` itself never qualifies.
/// Tiles not in `movable` are skipped.
pub fn neighbour(
    layout: &Layout,
    from: TileId,
    direction: Direction,
    movable: &[TileId],
) -> Option<TileId> {
    let origin = center(layout.rect(from)?);

    let mut best: Option<(f32, f32, TileId)> = None;
    for &(id, rect) in &layout.placed {
        if id == from || !movable.contains(&id) {
            continue;
        }

        let (along, across) = offsets(origin, center(rect), direction);
        if along <= 0.0 {
            continue;
        }

        let closer = best.is_none_or(|(best_along, best_across, _)| {
            along
                .total_cmp(&best_along)
                .then(across.total_cmp(&best_across))
                .is_lt()
        });
        if closer {
            best = Some((along, across, id));
        }
    }

    best.map(|(_, _, id)| id)
}

fn center(rect: Rect) -> (f32, f32) {
    (
        rect.origin.x + rect.size.width / 2.0,
        rect.origin.y + rect.size.height / 2.0,
    )
}

/// How far `to` lies in `direction` of `from` (negative on the wrong side), and how far
/// it lies off that axis.
fn offsets(from: (f32, f32), to: (f32, f32), direction: Direction) -> (f32, f32) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);

    match direction {
        Direction::Left => (-dx, dy.abs()),
        Direction::Right => (dx, dy.abs()),
        Direction::Up => (-dy, dx.abs()),
        Direction::Down => (dy, dx.abs()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(values: &[u32]) -> Vec<TileId> {
        values.iter().map(|value| TileId(*value)).collect()
    }

    fn moved(values: &[u32], moved: u32, target: u32) -> Vec<TileId> {
        let mut order = ids(values);
        move_before(&mut order, TileId(moved), TileId(target));

        order
    }

    #[test]
    fn move_before_down() {
        assert_eq!(moved(&[1, 2, 3, 4], 1, 3), ids(&[2, 3, 1, 4]));
    }

    #[test]
    fn move_before_up() {
        assert_eq!(moved(&[1, 2, 3, 4], 4, 2), ids(&[1, 4, 2, 3]));
    }

    #[test]
    fn move_before_to_the_first() {
        assert_eq!(moved(&[1, 2, 3, 4], 3, 1), ids(&[3, 1, 2, 4]));
    }

    #[test]
    fn move_before_to_the_last() {
        assert_eq!(moved(&[1, 2, 3, 4], 1, 4), ids(&[2, 3, 4, 1]));
    }

    #[test]
    fn move_before_the_same_tile_changes_nothing() {
        assert_eq!(moved(&[1, 2, 3], 2, 2), ids(&[1, 2, 3]));
    }

    #[test]
    fn move_before_an_unknown_tile_changes_nothing() {
        assert_eq!(moved(&[1, 2, 3], 2, 9), ids(&[1, 2, 3]));
        assert_eq!(moved(&[1, 2, 3], 9, 2), ids(&[1, 2, 3]));
    }

    fn tile(id: u32, x: f32, y: f32) -> (TileId, Rect) {
        (TileId(id), Rect::new(x, y, 100.0, 100.0))
    }

    fn layout(placed: Vec<(TileId, Rect)>) -> Layout {
        Layout {
            placed,
            tray: Vec::new(),
        }
    }

    /// A 3 x 3 block of tiles; ids 1 to 9 in reading order.
    fn block() -> Layout {
        layout(
            (0..9_u16)
                .map(|index| {
                    let (column, row) = (f32::from(index % 3), f32::from(index / 3));
                    tile(u32::from(index) + 1, column * 110.0, row * 110.0)
                })
                .collect(),
        )
    }

    fn all() -> Vec<TileId> {
        ids(&[1, 2, 3, 4, 5, 6, 7, 8, 9])
    }

    fn beside(direction: Direction) -> Option<TileId> {
        neighbour(&block(), TileId(5), direction, &all())
    }

    #[test]
    fn neighbour_left() {
        assert_eq!(beside(Direction::Left), Some(TileId(4)));
    }

    #[test]
    fn neighbour_right() {
        assert_eq!(beside(Direction::Right), Some(TileId(6)));
    }

    #[test]
    fn neighbour_up() {
        assert_eq!(beside(Direction::Up), Some(TileId(2)));
    }

    #[test]
    fn neighbour_down() {
        assert_eq!(beside(Direction::Down), Some(TileId(8)));
    }

    #[test]
    fn neighbour_skips_tiles_that_are_not_movable() {
        let movable = ids(&[1, 2, 3, 5, 6, 7, 8, 9]);

        assert_eq!(
            neighbour(&block(), TileId(5), Direction::Left, &movable),
            Some(TileId(1))
        );
        assert_eq!(
            neighbour(&block(), TileId(6), Direction::Left, &movable),
            Some(TileId(5))
        );
    }

    #[test]
    fn neighbour_is_none_at_the_edge() {
        assert_eq!(
            neighbour(&block(), TileId(1), Direction::Left, &all()),
            None
        );
        assert_eq!(neighbour(&block(), TileId(1), Direction::Up, &all()), None);
        assert_eq!(
            neighbour(&block(), TileId(9), Direction::Right, &all()),
            None
        );
        assert_eq!(
            neighbour(&block(), TileId(9), Direction::Down, &all()),
            None
        );
    }

    #[test]
    fn neighbour_of_a_tile_in_the_tray_is_none() {
        assert_eq!(
            neighbour(&block(), TileId(42), Direction::Left, &all()),
            None
        );
    }

    #[test]
    fn neighbour_ties_break_by_the_other_axis_then_input_order() {
        // Tiles 2 and 3 are equally far to the right; 3 is closer on the other axis.
        let by_other_axis = layout(vec![
            tile(1, 0.0, 100.0),
            tile(2, 200.0, 0.0),
            tile(3, 200.0, 90.0),
        ]);
        assert_eq!(
            neighbour(
                &by_other_axis,
                TileId(1),
                Direction::Right,
                &ids(&[1, 2, 3])
            ),
            Some(TileId(3))
        );

        // Equal on both axes: the earlier tile in the input wins.
        let by_input_order = layout(vec![
            tile(1, 0.0, 100.0),
            tile(3, 200.0, 0.0),
            tile(2, 200.0, 200.0),
        ]);
        assert_eq!(
            neighbour(
                &by_input_order,
                TileId(1),
                Direction::Right,
                &ids(&[1, 2, 3])
            ),
            Some(TileId(3))
        );
    }
}
