//! Arranges terminal tiles in the workspace and moves the rest to the tray.
//!
//! [`layout`] is a pure function: the same input always gives the same
//! output. It has no GPUI dependency; `swarm_app` converts its rects to
//! pixels. The rules are listed in this crate's `README.md`.

#[cfg(test)]
mod tests;

/// Slack for float rounding when checking whether something fits.
const EPSILON: f32 = 0.01;

/// A point in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// A width and height in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

impl Size {
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

/// A rectangle in pixels: its top-left corner and its size.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

impl Rect {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            origin: Point::new(x, y),
            size: Size::new(width, height),
        }
    }

    pub fn right(self) -> f32 {
        self.origin.x + self.size.width
    }

    pub fn bottom(self) -> f32 {
        self.origin.y + self.size.height
    }

    /// Shrinks the rect by `amount` on every side. The size never goes below zero.
    #[must_use]
    pub fn inset(self, amount: f32) -> Self {
        Self::new(
            self.origin.x + amount,
            self.origin.y + amount,
            (self.size.width - 2.0 * amount).max(0.0),
            (self.size.height - 2.0 * amount).max(0.0),
        )
    }
}

/// Identifies a tile in the input and the output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileId(pub u32);

/// The kind of terminal a tile shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TileKind {
    Orchestrator,
    Worker,
    Process,
    User,
}

/// What the terminal in a tile is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Activity {
    /// Blocked or failed: the user must look at it.
    Attention,
    Working,
    /// A process terminal that is running without a status.
    Running,
    Done,
    Idle,
}

/// One tile the layout must place or move to the tray.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileSpec {
    pub id: TileId,
    pub kind: TileKind,
    pub activity: Activity,
    /// A process terminal's requested size. Only the height is used.
    pub requested: Option<Size>,
    /// The size of a user terminal the user resized. The layout never changes it.
    pub fixed: Option<Size>,
    /// The user pinned the tile: keep it on screen if at all possible.
    pub pinned: bool,
    /// The user minimized the tile: it always goes to the tray.
    pub minimized: bool,
}

impl TileSpec {
    /// A tile with no requested or fixed size that is neither pinned nor minimized.
    pub const fn new(id: TileId, kind: TileKind, activity: Activity) -> Self {
        Self {
            id,
            kind,
            activity,
            requested: None,
            fixed: None,
            pinned: false,
            minimized: false,
        }
    }
}

/// Spacing and minimum sizes, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutStyle {
    pub gap: f32,
    pub padding: f32,
    pub orchestrator_width: f32,
    pub min_tile: Size,
    pub min_orchestrator: Size,
}

impl Default for LayoutStyle {
    fn default() -> Self {
        Self {
            gap: 10.0,
            padding: 10.0,
            orchestrator_width: 400.0,
            min_tile: Size::new(260.0, 160.0),
            min_orchestrator: Size::new(320.0, 300.0),
        }
    }
}

/// The result of [`layout`]. Both lists keep the input order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub placed: Vec<(TileId, Rect)>,
    pub tray: Vec<TileId>,
}

impl Layout {
    /// The rect of a placed tile, or `None` when it is in the tray.
    pub fn rect(&self, id: TileId) -> Option<Rect> {
        self.placed
            .iter()
            .find(|(placed_id, _)| *placed_id == id)
            .map(|(_, rect)| *rect)
    }
}

/// Places `tiles` inside `area` and moves the tiles that don't fit to the tray.
///
/// Every tile ends up exactly once in [`Layout::placed`] or [`Layout::tray`].
/// Sizes must be finite.
pub fn layout(area: Rect, tiles: &[TileSpec], style: &LayoutStyle) -> Layout {
    let metrics = Metrics::new(style);
    let content = area.inset(metrics.padding);
    let mut rects: Vec<Option<Rect>> = vec![None; tiles.len()];

    let visible: Vec<usize> = (0..tiles.len())
        .filter(|&index| !tiles[index].minimized)
        .collect();
    let orchestrator = visible
        .iter()
        .copied()
        .find(|&index| tiles[index].kind == TileKind::Orchestrator);
    let (island_indices, flow_indices): (Vec<usize>, Vec<usize>) = visible
        .iter()
        .copied()
        .filter(|&index| Some(index) != orchestrator)
        .partition(|&index| is_island(&tiles[index]));

    let column_reserve = orchestrator
        .filter(|_| fits(metrics.min_orchestrator, content.size))
        .map(|_| metrics.min_orchestrator.width + metrics.gap);
    let islands = Islands::place(content, tiles, &island_indices, column_reserve, metrics.gap);
    for &(index, rect) in &islands.rects {
        rects[index] = Some(rect);
    }

    let band = flow_band(tiles, &flow_indices);
    let flow = Flow {
        content,
        block: islands.block(),
        metrics,
    };
    let column_width = orchestrator
        .and_then(|_| ColumnRange::new(content, &islands, &metrics))
        .map(|range| flow.column_width(range, band.len()));

    if let (Some(index), Some(width)) = (orchestrator, column_width) {
        rects[index] = Some(Rect::new(
            content.origin.x,
            content.origin.y,
            width,
            content.size.height,
        ));
    }
    flow.place(column_width, tiles, &band, &mut rects);

    let mut result = Layout::default();
    for (tile, rect) in tiles.iter().zip(rects) {
        match rect {
            Some(rect) => result.placed.push((tile.id, rect)),
            None => result.tray.push(tile.id),
        }
    }

    result
}

/// Who stays on screen first. Lower is more important.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    User,
    Orchestrator,
    Pinned,
    Attention,
    Working,
    Running,
    Done,
    Idle,
}

impl Rank {
    fn of(tile: &TileSpec) -> Self {
        match (tile.kind, tile.pinned, tile.activity) {
            (TileKind::User, ..) => Self::User,
            (TileKind::Orchestrator, ..) => Self::Orchestrator,
            (_, true, _) => Self::Pinned,
            (_, false, Activity::Attention) => Self::Attention,
            (_, false, Activity::Working) => Self::Working,
            (_, false, Activity::Running) => Self::Running,
            (_, false, Activity::Done) => Self::Done,
            (_, false, Activity::Idle) => Self::Idle,
        }
    }

    fn is_active(self) -> bool {
        self <= Self::Running
    }
}

/// The style with values that can't break the math.
#[derive(Clone, Copy, Debug)]
struct Metrics {
    gap: f32,
    padding: f32,
    orchestrator_width: f32,
    min_tile: Size,
    min_orchestrator: Size,
}

impl Metrics {
    fn new(style: &LayoutStyle) -> Self {
        let at_least_one = |size: Size| Size::new(size.width.max(1.0), size.height.max(1.0));

        Self {
            gap: style.gap.max(0.0),
            padding: style.padding.max(0.0),
            orchestrator_width: style.orchestrator_width.max(0.0),
            min_tile: at_least_one(style.min_tile),
            min_orchestrator: at_least_one(style.min_orchestrator),
        }
    }
}

fn is_island(tile: &TileSpec) -> bool {
    tile.kind == TileKind::User && tile.fixed.is_some()
}

fn fits(size: Size, space: Size) -> bool {
    size.width <= space.width + EPSILON && size.height <= space.height + EPSILON
}

/// The flow tiles that compete for the grid, most important first.
///
/// Done and idle tiles only get the grid when no flow tile is active, so they
/// never take space from a tile that is working.
fn flow_band(tiles: &[TileSpec], flow: &[usize]) -> Vec<usize> {
    let mut band: Vec<usize> = flow
        .iter()
        .copied()
        .filter(|&index| Rank::of(&tiles[index]).is_active())
        .collect();
    if band.is_empty() {
        band = flow.to_vec();
    }

    band.sort_by_key(|&index| (Rank::of(&tiles[index]), index));
    band
}

/// User terminals at their fixed size, packed in rows from the top right.
struct Islands {
    rects: Vec<(usize, Rect)>,
    content: Rect,
}

impl Islands {
    /// Leaves room for the orchestrator column on the left when that loses no island.
    fn place(
        content: Rect,
        tiles: &[TileSpec],
        indices: &[usize],
        column_reserve: Option<f32>,
        gap: f32,
    ) -> Self {
        let full = Self::pack(content, tiles, indices, content.size.width, gap);
        let Some(reserve) = column_reserve else {
            return full;
        };

        let narrow = Self::pack(content, tiles, indices, content.size.width - reserve, gap);
        if narrow.rects.len() >= full.rects.len() {
            narrow
        } else {
            full
        }
    }

    fn pack(content: Rect, tiles: &[TileSpec], indices: &[usize], width: f32, gap: f32) -> Self {
        let left_limit = content.right() - width;
        let fits_at = |size: Size, top: f32, right: f32| {
            right - size.width >= left_limit - EPSILON
                && top + size.height <= content.bottom() + EPSILON
        };

        let mut rects = Vec::new();
        let mut row_top = content.origin.y;
        let mut row_height: f32 = 0.0;
        let mut row_used = false;
        let mut cursor = content.right();

        for &index in indices {
            let Some(size) = tiles[index].fixed else {
                continue;
            };
            if !(size.width > 0.0 && size.height > 0.0) {
                continue;
            }

            if !fits_at(size, row_top, cursor) {
                let next_top = row_top + row_height + gap;
                if !row_used || !fits_at(size, next_top, content.right()) {
                    continue;
                }

                row_top = next_top;
                row_height = 0.0;
                cursor = content.right();
            }

            rects.push((
                index,
                Rect::new(cursor - size.width, row_top, size.width, size.height),
            ));
            cursor -= size.width + gap;
            row_height = row_height.max(size.height);
            row_used = true;
        }

        Self { rects, content }
    }

    /// The bounding size of all islands, measured from the top-right corner.
    fn block(&self) -> Option<Size> {
        if self.rects.is_empty() {
            return None;
        }

        let width = self
            .rects
            .iter()
            .map(|(_, rect)| self.content.right() - rect.origin.x)
            .fold(0.0, f32::max);
        let height = self
            .rects
            .iter()
            .map(|(_, rect)| rect.bottom() - self.content.origin.y)
            .fold(0.0, f32::max);

        Some(Size::new(width, height))
    }
}

/// The widths the orchestrator column may take.
#[derive(Clone, Copy, Debug)]
struct ColumnRange {
    min: f32,
    max: f32,
    /// All the width left of the islands.
    available: f32,
}

impl ColumnRange {
    fn new(content: Rect, islands: &Islands, metrics: &Metrics) -> Option<Self> {
        let available = match islands.block() {
            Some(block) => content.size.width - block.width - metrics.gap,
            None => content.size.width,
        };
        if !fits(
            metrics.min_orchestrator,
            Size::new(available, content.size.height),
        ) {
            return None;
        }

        let min = metrics.min_orchestrator.width.min(available);
        let max = metrics.orchestrator_width.max(min).min(available);

        Some(Self {
            min,
            max,
            available,
        })
    }
}

/// The space for every tile that is not an island or the orchestrator column.
struct Flow {
    content: Rect,
    block: Option<Size>,
    metrics: Metrics,
}

impl Flow {
    /// Beside the islands (left of them) and below them. Without islands, all goes below.
    fn regions(&self, column: Option<f32>) -> [Rect; 2] {
        let gap = self.metrics.gap;
        let left = column.map_or(self.content.origin.x, |width| {
            self.content.origin.x + width + gap
        });
        let area = Rect::new(
            left,
            self.content.origin.y,
            (self.content.right() - left).max(0.0),
            self.content.size.height,
        );
        let Some(block) = self.block else {
            return [Rect::default(), area];
        };

        let beside = Rect::new(
            area.origin.x,
            area.origin.y,
            (area.size.width - block.width - gap).max(0.0),
            block.height,
        );
        let below = Rect::new(
            area.origin.x,
            area.origin.y + block.height + gap,
            area.size.width,
            (area.size.height - block.height - gap).max(0.0),
        );

        [beside, below]
    }

    fn capacity(&self, region: Rect) -> usize {
        let min = self.metrics.min_tile;
        let columns = fit_count(region.size.width, min.width, self.metrics.gap);
        let rows = fit_count(region.size.height, min.height, self.metrics.gap);

        columns.saturating_mul(rows)
    }

    fn capacities(&self, column: Option<f32>) -> [usize; 2] {
        self.regions(column).map(|region| self.capacity(region))
    }

    fn fitting(&self, column: Option<f32>, count: usize) -> usize {
        let [beside, below] = self.capacities(column);
        count.min(beside.saturating_add(below))
    }

    /// The widest column that still shows as many tiles as the narrowest one.
    fn column_width(&self, range: ColumnRange, count: usize) -> f32 {
        let target = self.fitting(Some(range.min), count);
        if target == 0 {
            return range.available;
        }
        if self.fitting(Some(range.max), count) == target {
            return range.max;
        }

        let (mut good, mut bad) = (range.min, range.max);
        while bad - good > 1.0 {
            let middle = f32::midpoint(good, bad).floor();
            if middle <= good {
                break;
            }

            if self.fitting(Some(middle), count) == target {
                good = middle;
            } else {
                bad = middle;
            }
        }

        good
    }

    fn place(
        &self,
        column: Option<f32>,
        tiles: &[TileSpec],
        band: &[usize],
        rects: &mut [Option<Rect>],
    ) {
        let [beside, below] = self.regions(column);
        let capacities = [self.capacity(beside), self.capacity(below)];
        let shown = band.len().min(capacities[0].saturating_add(capacities[1]));

        // Priority picks who is shown; input order picks where, so tiles don't
        // jump around when their activity changes.
        let mut slots = band[..shown].to_vec();
        slots.sort_by_key(|&index| (tiles[index].kind == TileKind::Process, index));
        let (beside_slots, below_slots) = slots.split_at(beside_share(shown, capacities));

        let placed = self
            .grid(beside, tiles, beside_slots)
            .into_iter()
            .chain(self.grid(below, tiles, below_slots));
        for (index, rect) in placed {
            rects[index] = Some(rect);
        }
    }

    /// Rows of equal-width tiles. The caller makes sure the slots fit.
    fn grid(&self, region: Rect, tiles: &[TileSpec], slots: &[usize]) -> Vec<(usize, Rect)> {
        if slots.is_empty() {
            return Vec::new();
        }

        let gap = self.metrics.gap;
        let max_columns = fit_count(region.size.width, self.metrics.min_tile.width, gap).max(1);
        let row_count = slots.len().div_ceil(max_columns);
        let columns = slots.len().div_ceil(row_count);
        let rows: Vec<&[usize]> = slots.chunks(columns).collect();
        let heights = self.row_heights(region.size.height, tiles, &rows);

        let mut placed = Vec::with_capacity(slots.len());
        let mut top = region.origin.y;
        for (row, height) in rows.iter().zip(heights) {
            let width = (region.size.width - gap * float(row.len() - 1)) / float(row.len());
            for (column, &index) in row.iter().enumerate() {
                let left = region.origin.x + float(column) * (width + gap);
                placed.push((index, Rect::new(left, top, width, height)));
            }

            top += height + gap;
        }

        placed
    }

    /// Process rows get their requested height when it fits; other rows share the rest.
    fn row_heights(&self, region_height: f32, tiles: &[TileSpec], rows: &[&[usize]]) -> Vec<f32> {
        let min = self.metrics.min_tile.height;
        let space = region_height - self.metrics.gap * float(rows.len() - 1);
        let wanted: Vec<Option<f32>> = rows
            .iter()
            .map(|row| requested_row_height(tiles, row).map(|height| height.max(min).min(space)))
            .collect();

        let flexible = wanted.iter().filter(|height| height.is_none()).count();
        let requested_total: f32 = wanted.iter().flatten().sum();

        if requested_total + float(flexible) * min <= space {
            let leftover = space - requested_total;
            if flexible > 0 {
                let share = leftover / float(flexible);
                return wanted
                    .iter()
                    .map(|height| height.unwrap_or(share))
                    .collect();
            }

            let extra = leftover / float(rows.len());
            return wanted
                .iter()
                .flatten()
                .map(|height| height + extra)
                .collect();
        }

        let budget = (space - float(rows.len()) * min).max(0.0);
        let excess: f32 = wanted.iter().flatten().map(|height| height - min).sum();
        let scale = if excess > 0.0 {
            (budget / excess).min(1.0)
        } else {
            0.0
        };

        wanted
            .iter()
            .map(|height| height.map_or(min, |height| min + (height - min) * scale))
            .collect()
    }
}

/// The tallest requested height when every tile in the row is a process with one.
fn requested_row_height(tiles: &[TileSpec], row: &[usize]) -> Option<f32> {
    row.iter()
        .map(|&index| {
            let tile = &tiles[index];
            match (tile.kind, tile.requested) {
                (TileKind::Process, Some(size)) => Some(size.height),
                _ => None,
            }
        })
        .try_fold(0.0, |tallest: f32, height| Some(tallest.max(height?)))
}

/// How many of `count` tiles go beside the islands, in proportion to the space there.
fn beside_share(count: usize, [beside, below]: [usize; 2]) -> usize {
    if count == 0 || beside == 0 {
        return 0;
    }

    let total = beside.saturating_add(below);
    let share = whole((float(count) * float(beside) / float(total)).round());

    share
        .max(count.saturating_sub(below))
        .min(beside)
        .min(count)
}

/// How many items of `item` length fit in `length` with `gap` between them.
fn fit_count(length: f32, item: f32, gap: f32) -> usize {
    whole((length + gap + EPSILON) / (item + gap))
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "floored and clamped to zero first; `as` saturates large values"
)]
fn whole(value: f32) -> usize {
    value.max(0.0).floor() as usize
}

#[expect(clippy::cast_precision_loss, reason = "tile counts are small")]
fn float(count: usize) -> f32 {
    count as f32
}
