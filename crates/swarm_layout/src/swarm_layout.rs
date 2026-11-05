//! Arranges terminal tiles in the workspace and moves the rest to the tray.
//!
//! [`layout`] is a pure function: the same input always gives the same
//! output. It has no GPUI dependency; `swarm_app` converts its rects to
//! pixels. The rules are listed in this crate's `README.md`.
//!
//! The engine works in whole grid cells of [`CELL`] pixels. It turns the area into
//! cells once, does all its work in integers and turns the result back into pixels.

mod order;
#[cfg(test)]
mod tests;

pub use order::{Direction, move_before, neighbour};

/// Logical pixels per grid cell.
pub const CELL: f32 = 8.0;

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
}

/// A size in whole grid cells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Cells {
    pub width: u16,
    pub height: u16,
}

impl Cells {
    pub const fn new(width: u16, height: u16) -> Self {
        Self { width, height }
    }

    /// The size in pixels.
    pub fn to_size(self) -> Size {
        Size::new(f32::from(self.width) * CELL, f32::from(self.height) * CELL)
    }

    /// The nearest whole cells to a size in pixels (for pointer resizes).
    pub fn nearest(size: Size) -> Self {
        Self::new(cells_nearest(size.width), cells_nearest(size.height))
    }
}

/// Pixels to whole cells, rounded up (font-sized boxes always fit their rows).
pub fn cells_up(pixels: f32) -> u16 {
    to_cells((pixels / CELL).ceil())
}

/// Pixels to the nearest whole cells.
pub fn cells_nearest(pixels: f32) -> u16 {
    to_cells((pixels / CELL).round())
}

/// Pixels to whole cells, rounded down.
fn cells_down(pixels: f32) -> u16 {
    to_cells((pixels / CELL).floor())
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to the `u16` range first"
)]
fn to_cells(value: f32) -> u16 {
    value.clamp(0.0, f32::from(u16::MAX)) as u16
}

/// Whole cells to pixels. Cell counts inside the layout never exceed `u16::MAX`.
fn pixels(cells: u32) -> f32 {
    f32::from(u16::try_from(cells).unwrap_or(u16::MAX)) * CELL
}

/// The content area: `area` minus the padding, in whole cells from its top-left corner.
/// The 0 to 7 px left over go to the right and bottom.
pub fn content_rect(area: Rect, style: &LayoutStyle) -> Rect {
    let padding = f32::from(style.padding) * CELL;
    let width = cells_down(area.size.width - 2.0 * padding);
    let height = cells_down(area.size.height - 2.0 * padding);

    Rect::new(
        area.origin.x + padding,
        area.origin.y + padding,
        f32::from(width) * CELL,
        f32::from(height) * CELL,
    )
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
    /// The size a user terminal has after the user resized it. The layout keeps it.
    /// Other kinds ignore it.
    pub fixed: Option<Cells>,
    /// The user pinned the tile: keep it on screen if at all possible.
    pub pinned: bool,
    /// The user minimized the tile: it always goes to the tray.
    pub minimized: bool,
    /// The worker asked for a large tile. The box is tall, like a box that needs
    /// attention. Other kinds and pinned tiles ignore it.
    pub large: bool,
}

impl TileSpec {
    /// A tile with no fixed size that is neither pinned, minimized nor large.
    pub const fn new(id: TileId, kind: TileKind, activity: Activity) -> Self {
        Self {
            id,
            kind,
            activity,
            fixed: None,
            pinned: false,
            minimized: false,
            large: false,
        }
    }
}

/// Spacing, shares and sizes, in grid cells.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutStyle {
    /// The space between tiles, in cells.
    pub gap: u16,
    /// The space between the area and the tiles, in cells.
    pub padding: u16,
    /// The part of the content width the orchestrator column takes, from 0 to 1.
    pub orchestrator_share: f32,
    pub min_tile: Cells,
    pub min_orchestrator: Cells,
    /// The height of a small box: a tile without a size of its own that does not
    /// need attention. The app measures it from the terminal font and rounds up.
    pub small_tile_height: u16,
    /// The height of a process terminal's box, the smallest tile. The app measures it
    /// from the terminal font and rounds up.
    pub process_tile_height: u16,
}

impl Default for LayoutStyle {
    fn default() -> Self {
        Self {
            gap: 1,
            padding: 1,
            orchestrator_share: 0.65,
            min_tile: Cells::new(33, 20),
            min_orchestrator: Cells::new(40, 38),
            small_tile_height: cells_up(286.0),
            process_tile_height: cells_up(218.0),
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

/// A rect in whole cells, measured from the top-left corner of the content area.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CellRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl CellRect {
    const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    const fn right(self) -> u32 {
        self.x + self.width
    }

    const fn bottom(self) -> u32 {
        self.y + self.height
    }

    /// The pixel rect, with `origin` as the pixel position of cell (0, 0).
    fn to_rect(self, origin: Point) -> Rect {
        Rect::new(
            origin.x + pixels(self.x),
            origin.y + pixels(self.y),
            pixels(self.width),
            pixels(self.height),
        )
    }
}

/// A width and height in whole cells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Extent {
    width: u32,
    height: u32,
}

impl Extent {
    const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    fn of(cells: Cells) -> Self {
        Self::new(u32::from(cells.width), u32::from(cells.height))
    }

    const fn fits_in(self, space: Self) -> bool {
        self.width <= space.width && self.height <= space.height
    }
}

/// Places `tiles` inside `area` and moves the tiles that don't fit to the tray.
///
/// Every tile ends up exactly once in [`Layout::placed`] or [`Layout::tray`].
/// Sizes must be finite.
pub fn layout(area: Rect, tiles: &[TileSpec], style: &LayoutStyle) -> Layout {
    let metrics = Metrics::new(style);
    let pixel_content = content_rect(area, style);
    let content = CellRect::new(
        0,
        0,
        u32::from(cells_nearest(pixel_content.size.width)),
        u32::from(cells_nearest(pixel_content.size.height)),
    );
    let mut rects: Vec<Option<CellRect>> = vec![None; tiles.len()];

    let visible: Vec<usize> = (0..tiles.len())
        .filter(|&index| !tiles[index].minimized)
        .collect();
    let orchestrator = visible
        .iter()
        .copied()
        .find(|&index| tiles[index].kind == TileKind::Orchestrator);
    let (mut island_indices, flow_indices): (Vec<usize>, Vec<usize>) = visible
        .iter()
        .copied()
        .filter(|&index| Some(index) != orchestrator)
        .partition(|&index| is_island(&tiles[index]));

    island_indices.sort_by_key(|&index| (Rank::of(&tiles[index]), index));

    let column_reserve = orchestrator
        .filter(|_| {
            metrics
                .min_orchestrator
                .fits_in(Extent::new(content.width, content.height))
        })
        .map(|_| metrics.min_orchestrator.width + metrics.gap);
    let islands = Islands::place(content, tiles, &island_indices, column_reserve, &metrics);
    for &(index, rect) in &islands.rects {
        rects[index] = Some(rect);
    }

    let band = flow_band(tiles, &flow_indices);
    let flow = Flow {
        content,
        block: islands.block(),
        metrics,
    };
    let column = orchestrator.and_then(|_| ColumnRange::new(content, &islands, &metrics));

    let (column_width, boxes) = match column {
        Some(range) => {
            let (width, boxes) = flow.beside_column(range, tiles, &band);
            (Some(width), boxes)
        }
        None => (None, flow.place(None, tiles, &band)),
    };

    if let (Some(index), Some(width)) = (orchestrator, column_width) {
        rects[index] = Some(CellRect::new(0, 0, width, content.height));
    }
    for (index, rect) in boxes {
        rects[index] = Some(rect);
    }

    let mut result = Layout::default();
    for (tile, rect) in tiles.iter().zip(rects) {
        match rect {
            Some(rect) => result
                .placed
                .push((tile.id, rect.to_rect(pixel_content.origin))),
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
}

/// The style with values that can't break the math.
#[derive(Clone, Copy, Debug)]
struct Metrics {
    gap: u32,
    orchestrator_share: f32,
    min_tile: Extent,
    min_orchestrator: Extent,
    small_height: u32,
    process_height: u32,
}

impl Metrics {
    fn new(style: &LayoutStyle) -> Self {
        let at_least_one = |cells: Cells| {
            Extent::new(
                u32::from(cells.width.max(1)),
                u32::from(cells.height.max(1)),
            )
        };
        let min_tile = at_least_one(style.min_tile);
        let small_height = u32::from(style.small_tile_height).max(min_tile.height);

        Self {
            gap: u32::from(style.gap),
            orchestrator_share: style.orchestrator_share.clamp(0.0, 1.0),
            min_tile,
            min_orchestrator: at_least_one(style.min_orchestrator),
            small_height,
            // The smallest tile: never taller than a small box.
            process_height: u32::from(style.process_tile_height)
                .max(min_tile.height)
                .min(small_height),
        }
    }

    /// The height of a tall box: room for two small boxes and the gap between them.
    fn tall_height(&self) -> u32 {
        2 * self.small_height + self.gap
    }
}

fn is_island(tile: &TileSpec) -> bool {
    tile.kind == TileKind::User && tile.fixed.is_some()
}

/// The flow tiles that compete for the lanes, most important first.
fn flow_band(tiles: &[TileSpec], flow: &[usize]) -> Vec<usize> {
    let mut band = flow.to_vec();
    band.sort_by_key(|&index| (Rank::of(&tiles[index]), index));

    band
}

/// User terminals with a fixed size, packed in rows from the top right. They keep
/// their size.
struct Islands {
    rects: Vec<(usize, CellRect)>,
    content: CellRect,
}

impl Islands {
    /// Leaves room for the orchestrator column on the left when that loses no user
    /// terminal.
    fn place(
        content: CellRect,
        tiles: &[TileSpec],
        indices: &[usize],
        column_reserve: Option<u32>,
        metrics: &Metrics,
    ) -> Self {
        let full = Self::pack(content, tiles, indices, content.width, metrics);
        let Some(reserve) = column_reserve else {
            return full;
        };

        let narrow = Self::pack(
            content,
            tiles,
            indices,
            content.width.saturating_sub(reserve),
            metrics,
        );
        if narrow.rects.len() >= full.rects.len() {
            narrow
        } else {
            full
        }
    }

    fn pack(
        content: CellRect,
        tiles: &[TileSpec],
        indices: &[usize],
        width: u32,
        metrics: &Metrics,
    ) -> Self {
        let gap = metrics.gap;
        let left_limit = content.right().saturating_sub(width);
        let fits_at = |size: Extent, top: u32, right: u32| {
            right
                .checked_sub(size.width)
                .is_some_and(|left| left >= left_limit)
                && top + size.height <= content.bottom()
        };

        let mut rects = Vec::new();
        let mut row_top = content.y;
        let mut row_height: u32 = 0;
        let mut row_used = false;
        let mut cursor = content.right();

        for &index in indices {
            let Some(fixed) = tiles[index].fixed else {
                continue;
            };
            let wanted = Extent::of(fixed);
            if wanted.width == 0 || wanted.height == 0 {
                continue;
            }

            if !fits_at(wanted, row_top, cursor) {
                let next_top = row_top + row_height + gap;
                if !row_used || !fits_at(wanted, next_top, content.right()) {
                    continue;
                }

                row_top = next_top;
                row_height = 0;
                cursor = content.right();
            }

            rects.push((
                index,
                CellRect::new(cursor - wanted.width, row_top, wanted.width, wanted.height),
            ));
            cursor = cursor.saturating_sub(wanted.width + gap);
            row_height = row_height.max(wanted.height);
            row_used = true;
        }

        Self { rects, content }
    }

    /// The bounding size of all islands, measured from the top-right corner.
    fn block(&self) -> Option<Extent> {
        if self.rects.is_empty() {
            return None;
        }

        let width = self
            .rects
            .iter()
            .map(|(_, rect)| self.content.right() - rect.x)
            .max()
            .unwrap_or(0);
        let height = self
            .rects
            .iter()
            .map(|(_, rect)| rect.bottom() - self.content.y)
            .max()
            .unwrap_or(0);

        Some(Extent::new(width, height))
    }
}

/// The widths the orchestrator column may take.
#[derive(Clone, Copy, Debug)]
struct ColumnRange {
    min: u32,
    /// All the width left of the islands.
    available: u32,
    /// The widest column that still leaves a lane of `min_tile` width for the other tiles.
    room: u32,
}

impl ColumnRange {
    fn new(content: CellRect, islands: &Islands, metrics: &Metrics) -> Option<Self> {
        let block = islands.block();
        let available = match block {
            Some(block) => content.width.checked_sub(block.width + metrics.gap)?,
            None => content.width,
        };
        if !metrics
            .min_orchestrator
            .fits_in(Extent::new(available, content.height))
        {
            return None;
        }

        Some(Self {
            min: metrics.min_orchestrator.width.min(available),
            available,
            room: lane_room(content, block, metrics),
        })
    }

    /// The share of the content width, with room left for one `min_tile` lane on the
    /// right. The minimum width of the orchestrator wins over both.
    fn preferred(self, content_width: u32, metrics: &Metrics) -> u32 {
        let share = share_of(content_width, metrics.orchestrator_share);

        share.min(self.room).max(self.min).min(self.available)
    }
}

/// The part of `width` cells that `share` (0 to 1) gives, in whole cells rounded down.
fn share_of(width: u32, share: f32) -> u32 {
    let width = f32::from(u16::try_from(width).unwrap_or(u16::MAX));

    u32::from(to_cells((width * share).floor()))
}

/// The widest orchestrator column that leaves a lane of `min_tile` width. The lane goes
/// below the islands when there is height for it, else beside them.
fn lane_room(content: CellRect, block: Option<Extent>, metrics: &Metrics) -> u32 {
    let (gap, min) = (metrics.gap, metrics.min_tile);
    let below = content.width.saturating_sub(gap + min.width);
    let Some(block) = block else {
        return below;
    };

    let fits_below = content.height >= block.height + gap + min.height;
    let fits_beside = block.height >= min.height;
    if fits_below || !fits_beside {
        return below;
    }

    below.saturating_sub(block.width + gap)
}

/// How tall the boxes of tiles that grow are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Growth {
    /// Room for two small boxes and the gap between them.
    Tall,
    /// One small box.
    Small,
}

/// Boxes that are tall when there is room for it: tiles that need attention and unpinned
/// workers that asked for a large tile.
fn grows(tile: &TileSpec) -> bool {
    let asked = tile.large && tile.kind == TileKind::Worker && !tile.pinned;

    tile.activity == Activity::Attention || asked
}

/// Tiles that rank above running processes and done or idle tiles.
fn is_core(tile: &TileSpec) -> bool {
    Rank::of(tile) <= Rank::Working
}

/// The box sizes of one lane, top to bottom.
type LaneBoxes = Vec<(usize, u32)>;

/// The space for every tile that is not an island or the orchestrator column.
///
/// It is cut into lanes: columns at least `min_tile` wide. Boxes are stacked from the
/// top of a lane; the free space below stays empty, and a full lane hands over to the
/// next one.
struct Flow {
    content: CellRect,
    block: Option<Extent>,
    metrics: Metrics,
}

impl Flow {
    /// Beside the islands (left of them) and below them. Without islands, all goes below.
    fn regions(&self, column: Option<u32>) -> [CellRect; 2] {
        let gap = self.metrics.gap;
        let left = column.map_or(self.content.x, |width| self.content.x + width + gap);
        let area = CellRect::new(
            left,
            self.content.y,
            self.content.right().saturating_sub(left),
            self.content.height,
        );
        let Some(block) = self.block else {
            return [CellRect::default(), area];
        };

        let beside = CellRect::new(
            area.x,
            area.y,
            area.width.saturating_sub(block.width + gap),
            block.height,
        );
        let below = CellRect::new(
            area.x,
            area.y + block.height + gap,
            area.width,
            area.height.saturating_sub(block.height + gap),
        );

        [beside, below]
    }

    /// The lanes of a region as wide as `min_tile` allows, sharing the width equally.
    /// Cells that don't divide go to the first lanes.
    fn lanes_in(&self, region: CellRect) -> Vec<CellRect> {
        let (gap, min) = (self.metrics.gap, self.metrics.min_tile);
        let count = (region.width + gap) / (min.width + gap);
        if count == 0 || region.height < min.height {
            return Vec::new();
        }

        let usable = region.width - gap * (count - 1);
        let (width, extra) = (usable / count, usable % count);
        let mut left = region.x;

        (0..count)
            .map(|lane| {
                let lane_width = width + u32::from(lane < extra);
                let rect = CellRect::new(left, region.y, lane_width, region.height);
                left += lane_width + gap;

                rect
            })
            .collect()
    }

    fn lanes(&self, column: Option<u32>) -> Vec<CellRect> {
        self.regions(column)
            .into_iter()
            .flat_map(|region| self.lanes_in(region))
            .collect()
    }

    /// Places the band beside the orchestrator column and returns the column's width.
    /// With nothing to show beside it, the column takes all the width it may.
    fn beside_column(
        &self,
        range: ColumnRange,
        tiles: &[TileSpec],
        band: &[usize],
    ) -> (u32, Vec<(usize, CellRect)>) {
        if band.is_empty() {
            return (range.available, Vec::new());
        }

        let width = range.preferred(self.content.width, &self.metrics);
        let boxes = self.place(Some(width), tiles, band);
        if boxes.is_empty() {
            return (range.available, boxes);
        }

        (width, boxes)
    }

    /// Picks the tiles that fit, in priority order, and puts them in lanes in input order.
    ///
    /// Tiles that grow are tall unless that sends a working tile or a tile that needs
    /// attention to the tray, which small boxes would keep on screen.
    fn place(
        &self,
        column: Option<u32>,
        tiles: &[TileSpec],
        band: &[usize],
    ) -> Vec<(usize, CellRect)> {
        let lanes = self.lanes(column);

        let small = self.select(&lanes, tiles, band, Growth::Small);
        let tall = band
            .iter()
            .any(|&index| grows(&tiles[index]))
            .then(|| self.select(&lanes, tiles, band, Growth::Tall));
        let core = |shown: &[usize]| {
            shown
                .iter()
                .filter(|&&index| is_core(&tiles[index]))
                .count()
        };
        let (shown, growth) = match tall {
            Some(tall) if core(&tall) >= core(&small) => (tall, Growth::Tall),
            _ => (small, Growth::Small),
        };

        let boxes = self
            .stack(&lanes, tiles, &shown, growth)
            .unwrap_or_default();

        self.rects(&lanes, &boxes)
    }

    /// The longest start of `band` that fits, in input order. Priority picks who is
    /// shown; input order picks where, so tiles don't jump around when their activity
    /// changes.
    fn select(
        &self,
        lanes: &[CellRect],
        tiles: &[TileSpec],
        band: &[usize],
        growth: Growth,
    ) -> Vec<usize> {
        let mut shown: Vec<usize> = Vec::new();

        for &index in band {
            let mut next = shown.clone();
            next.push(index);
            next.sort_unstable();

            if self.stack(lanes, tiles, &next, growth).is_none() {
                break;
            }

            shown = next;
        }

        shown
    }

    /// Stacks the tiles in order, each in the first lane from the current one that has
    /// room for it. `None` when a tile fits in no lane.
    fn stack(
        &self,
        lanes: &[CellRect],
        tiles: &[TileSpec],
        slots: &[usize],
        growth: Growth,
    ) -> Option<Vec<LaneBoxes>> {
        let gap = self.metrics.gap;
        let mut stacks: Vec<LaneBoxes> = vec![Vec::new(); lanes.len()];
        let mut lane = 0;

        for &index in slots {
            let wanted = self.wanted_height(&tiles[index], growth);

            loop {
                let rect = lanes.get(lane)?;
                let boxes = &mut stacks[lane];
                let used: u32 = boxes.iter().map(|(_, height)| height + gap).sum();
                let room = rect.height.saturating_sub(used);

                if let Some(height) = self.fit(wanted, room, boxes.is_empty()) {
                    boxes.push((index, height));
                    break;
                }

                lane += 1;
            }
        }

        Some(stacks)
    }

    /// The height a tile asks for. A process terminal is the smallest box; a tile that
    /// grows may ask for two small boxes.
    fn wanted_height(&self, tile: &TileSpec, growth: Growth) -> u32 {
        match tile.kind {
            _ if growth == Growth::Tall && grows(tile) => self.metrics.tall_height(),
            TileKind::Process => self.metrics.process_height,
            _ => self.metrics.small_height,
        }
    }

    /// The height a box gets in `room`, or `None` when it doesn't fit. The first box of a
    /// lane may be cut down to the lane, but never below `min_tile`.
    fn fit(&self, wanted: u32, room: u32, first: bool) -> Option<u32> {
        let height = if first { wanted.min(room) } else { wanted };
        let fits = height >= self.metrics.min_tile.height && height <= room;

        fits.then_some(height)
    }

    /// The rect of every box, stacked from the top of its lane.
    fn rects(&self, lanes: &[CellRect], stacks: &[LaneBoxes]) -> Vec<(usize, CellRect)> {
        let mut placed = Vec::new();

        for (lane, boxes) in lanes.iter().zip(stacks) {
            let mut top = lane.y;

            for &(index, height) in boxes {
                placed.push((index, CellRect::new(lane.x, top, lane.width, height)));
                top += height + self.metrics.gap;
            }
        }

        placed
    }
}
