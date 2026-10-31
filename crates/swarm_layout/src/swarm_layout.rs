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
    /// The size a user terminal has after the user resized it. The layout keeps it.
    /// Other kinds ignore it.
    pub fixed: Option<Size>,
    /// The user pinned the tile: keep it on screen if at all possible.
    pub pinned: bool,
    /// The user minimized the tile: it always goes to the tray.
    pub minimized: bool,
}

impl TileSpec {
    /// A tile with no fixed size that is neither pinned nor minimized.
    pub const fn new(id: TileId, kind: TileKind, activity: Activity) -> Self {
        Self {
            id,
            kind,
            activity,
            fixed: None,
            pinned: false,
            minimized: false,
        }
    }
}

/// Spacing, shares and sizes, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutStyle {
    pub gap: f32,
    pub padding: f32,
    /// The part of the content width the orchestrator column takes, from 0 to 1.
    pub orchestrator_share: f32,
    pub min_tile: Size,
    pub min_orchestrator: Size,
    /// The height of a small box: a tile without a size of its own that does not
    /// need attention. The app measures it from the terminal font.
    pub small_tile_height: f32,
    /// The height of a process terminal's box, the smallest tile. The app measures it
    /// from the terminal font.
    pub process_tile_height: f32,
}

impl Default for LayoutStyle {
    fn default() -> Self {
        Self {
            gap: 10.0,
            padding: 10.0,
            orchestrator_share: 0.65,
            min_tile: Size::new(260.0, 160.0),
            min_orchestrator: Size::new(320.0, 300.0),
            small_tile_height: 286.0,
            process_tile_height: 218.0,
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
    let (mut island_indices, flow_indices): (Vec<usize>, Vec<usize>) = visible
        .iter()
        .copied()
        .filter(|&index| Some(index) != orchestrator)
        .partition(|&index| is_island(&tiles[index]));

    island_indices.sort_by_key(|&index| (Rank::of(&tiles[index]), index));

    let column_reserve = orchestrator
        .filter(|_| fits(metrics.min_orchestrator, content.size))
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
        rects[index] = Some(Rect::new(
            content.origin.x,
            content.origin.y,
            width,
            content.size.height,
        ));
    }
    for (index, rect) in boxes {
        rects[index] = Some(rect);
    }

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
}

/// The style with values that can't break the math.
#[derive(Clone, Copy, Debug)]
struct Metrics {
    gap: f32,
    padding: f32,
    orchestrator_share: f32,
    min_tile: Size,
    min_orchestrator: Size,
    small_height: f32,
    process_height: f32,
}

impl Metrics {
    fn new(style: &LayoutStyle) -> Self {
        let at_least_one = |size: Size| Size::new(size.width.max(1.0), size.height.max(1.0));
        let min_tile = at_least_one(style.min_tile);
        let small_height = style.small_tile_height.max(min_tile.height);

        Self {
            gap: style.gap.max(0.0),
            padding: style.padding.max(0.0),
            orchestrator_share: style.orchestrator_share.clamp(0.0, 1.0),
            min_tile,
            min_orchestrator: at_least_one(style.min_orchestrator),
            small_height,
            // The smallest tile: never taller than a small box.
            process_height: style
                .process_tile_height
                .max(min_tile.height)
                .min(small_height),
        }
    }

    /// The height of a tall box: room for two small boxes and the gap between them.
    fn tall_height(&self) -> f32 {
        2.0 * self.small_height + self.gap
    }
}

fn is_island(tile: &TileSpec) -> bool {
    tile.kind == TileKind::User && tile.fixed.is_some()
}

fn fits(size: Size, space: Size) -> bool {
    size.width <= space.width + EPSILON && size.height <= space.height + EPSILON
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
    rects: Vec<(usize, Rect)>,
    content: Rect,
}

impl Islands {
    /// Leaves room for the orchestrator column on the left when that loses no user
    /// terminal.
    fn place(
        content: Rect,
        tiles: &[TileSpec],
        indices: &[usize],
        column_reserve: Option<f32>,
        metrics: &Metrics,
    ) -> Self {
        let full = Self::pack(content, tiles, indices, content.size.width, metrics);
        let Some(reserve) = column_reserve else {
            return full;
        };

        let narrow = Self::pack(
            content,
            tiles,
            indices,
            content.size.width - reserve,
            metrics,
        );
        if narrow.rects.len() >= full.rects.len() {
            narrow
        } else {
            full
        }
    }

    fn pack(
        content: Rect,
        tiles: &[TileSpec],
        indices: &[usize],
        width: f32,
        metrics: &Metrics,
    ) -> Self {
        let gap = metrics.gap;
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
            let Some(wanted) = tiles[index].fixed else {
                continue;
            };
            if !(wanted.width > 0.0 && wanted.height > 0.0) {
                continue;
            }

            if !fits_at(wanted, row_top, cursor) {
                let next_top = row_top + row_height + gap;
                if !row_used || !fits_at(wanted, next_top, content.right()) {
                    continue;
                }

                row_top = next_top;
                row_height = 0.0;
                cursor = content.right();
            }

            rects.push((
                index,
                Rect::new(cursor - wanted.width, row_top, wanted.width, wanted.height),
            ));
            cursor -= wanted.width + gap;
            row_height = row_height.max(wanted.height);
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
    /// All the width left of the islands.
    available: f32,
    /// The widest column that still leaves a lane of `min_tile` width for the other tiles.
    room: f32,
}

impl ColumnRange {
    fn new(content: Rect, islands: &Islands, metrics: &Metrics) -> Option<Self> {
        let block = islands.block();
        let available = match block {
            Some(block) => content.size.width - block.width - metrics.gap,
            None => content.size.width,
        };
        if !fits(
            metrics.min_orchestrator,
            Size::new(available, content.size.height),
        ) {
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
    fn preferred(self, content_width: f32, metrics: &Metrics) -> f32 {
        let share = content_width * metrics.orchestrator_share;

        share.min(self.room).max(self.min).min(self.available)
    }
}

/// The widest orchestrator column that leaves a lane of `min_tile` width. The lane goes
/// below the islands when there is height for it, else beside them.
fn lane_room(content: Rect, block: Option<Size>, metrics: &Metrics) -> f32 {
    let (gap, min) = (metrics.gap, metrics.min_tile);
    let below = content.size.width - gap - min.width;
    let Some(block) = block else {
        return below;
    };

    let fits_below = content.size.height - block.height - gap >= min.height - EPSILON;
    let fits_beside = block.height >= min.height - EPSILON;
    if fits_below || !fits_beside {
        return below;
    }

    below - block.width - gap
}

/// How tall the boxes of tiles that need attention are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Growth {
    /// Room for two small boxes and the gap between them.
    Tall,
    /// One small box.
    Small,
}

/// Boxes that are tall when there is room for it.
fn grows(tile: &TileSpec) -> bool {
    tile.activity == Activity::Attention
}

/// Tiles that rank above running processes and done or idle tiles.
fn is_core(tile: &TileSpec) -> bool {
    Rank::of(tile) <= Rank::Working
}

/// The box sizes of one lane, top to bottom.
type LaneBoxes = Vec<(usize, f32)>;

/// The space for every tile that is not an island or the orchestrator column.
///
/// It is cut into lanes: columns at least `min_tile` wide. Boxes are stacked from the
/// top of a lane; the free space below stays empty, and a full lane hands over to the
/// next one.
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

    /// The lanes of a region as wide as `min_tile` allows, sharing the width equally.
    fn lanes_in(&self, region: Rect) -> Vec<Rect> {
        let (gap, min) = (self.metrics.gap, self.metrics.min_tile);
        let count = fit_count(region.size.width, min.width, gap);
        if count == 0 || region.size.height < min.height - EPSILON {
            return Vec::new();
        }

        let width = (region.size.width - gap * float(count - 1)) / float(count);

        (0..count)
            .map(|lane| {
                Rect::new(
                    region.origin.x + float(lane) * (width + gap),
                    region.origin.y,
                    width,
                    region.size.height,
                )
            })
            .collect()
    }

    fn lanes(&self, column: Option<f32>) -> Vec<Rect> {
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
    ) -> (f32, Vec<(usize, Rect)>) {
        if band.is_empty() {
            return (range.available, Vec::new());
        }

        let width = range.preferred(self.content.size.width, &self.metrics);
        let boxes = self.place(Some(width), tiles, band);
        if boxes.is_empty() {
            return (range.available, boxes);
        }

        (width, boxes)
    }

    /// Picks the tiles that fit, in priority order, and puts them in lanes in input order.
    ///
    /// Tiles that need attention are tall unless that sends a working tile or a tile
    /// that needs attention to the tray, which small boxes would keep on screen.
    fn place(&self, column: Option<f32>, tiles: &[TileSpec], band: &[usize]) -> Vec<(usize, Rect)> {
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
        lanes: &[Rect],
        tiles: &[TileSpec],
        band: &[usize],
        growth: Growth,
    ) -> Vec<usize> {
        let mut shown: Vec<usize> = Vec::new();

        for &index in band {
            let mut next = shown.clone();
            next.push(index);
            next.sort_by_key(|&slot| (tiles[slot].kind == TileKind::Process, slot));

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
        lanes: &[Rect],
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
                let used: f32 = boxes.iter().map(|(_, height)| height + gap).sum();

                if let Some(height) = self.fit(wanted, rect.size.height - used, boxes.is_empty()) {
                    boxes.push((index, height));
                    break;
                }

                lane += 1;
            }
        }

        Some(stacks)
    }

    /// The height a tile asks for. A process terminal is the smallest box; a tile that
    /// needs attention may ask for two small boxes.
    fn wanted_height(&self, tile: &TileSpec, growth: Growth) -> f32 {
        match tile.kind {
            _ if growth == Growth::Tall && grows(tile) => self.metrics.tall_height(),
            TileKind::Process => self.metrics.process_height,
            _ => self.metrics.small_height,
        }
    }

    /// The height a box gets in `room`, or `None` when it doesn't fit. The first box of a
    /// lane may be cut down to the lane, but never below `min_tile`.
    fn fit(&self, wanted: f32, room: f32, first: bool) -> Option<f32> {
        let height = if first { wanted.min(room) } else { wanted };
        let fits = height >= self.metrics.min_tile.height - EPSILON && height <= room + EPSILON;

        fits.then_some(height)
    }

    /// The rect of every box, stacked from the top of its lane.
    fn rects(&self, lanes: &[Rect], stacks: &[LaneBoxes]) -> Vec<(usize, Rect)> {
        let mut placed = Vec::new();

        for (lane, boxes) in lanes.iter().zip(stacks) {
            let mut top = lane.origin.y;

            for &(index, height) in boxes {
                placed.push((
                    index,
                    Rect::new(lane.origin.x, top, lane.size.width, height),
                ));
                top += height + self.metrics.gap;
            }
        }

        placed
    }
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
