use super::*;

const MOCKUP_TOLERANCE: f32 = 1.0;
const FLOAT_TOLERANCE: f32 = 0.05;

fn tile(id: u32, kind: TileKind, activity: Activity) -> TileSpec {
    TileSpec::new(TileId(id), kind, activity)
}

fn worker(id: u32, activity: Activity) -> TileSpec {
    tile(id, TileKind::Worker, activity)
}

fn orchestrator(id: u32) -> TileSpec {
    tile(id, TileKind::Orchestrator, Activity::Working)
}

fn process(id: u32, activity: Activity, requested: Size) -> TileSpec {
    TileSpec {
        requested: Some(requested),
        ..tile(id, TileKind::Process, activity)
    }
}

fn user(id: u32, fixed: Size) -> TileSpec {
    TileSpec {
        fixed: Some(fixed),
        ..tile(id, TileKind::User, Activity::Running)
    }
}

/// A terminal of `columns` × `rows` cells in Geist Mono, plus the 34 px header.
fn terminal_size(columns: u16, rows: u16) -> Size {
    Size::new(f32::from(columns) * 7.2, f32::from(rows) * 17.0 + 34.0)
}

fn area(width: f32, height: f32) -> Rect {
    Rect::new(0.0, 0.0, width, height)
}

fn near(actual: Rect, expected: Rect, tolerance: f32) -> bool {
    (actual.origin.x - expected.origin.x).abs() <= tolerance
        && (actual.origin.y - expected.origin.y).abs() <= tolerance
        && (actual.size.width - expected.size.width).abs() <= tolerance
        && (actual.size.height - expected.size.height).abs() <= tolerance
}

fn assert_rect(layout: &Layout, id: u32, expected: Rect) {
    let actual = layout.rect(TileId(id));
    assert!(
        actual.is_some_and(|actual| near(actual, expected, MOCKUP_TOLERANCE)),
        "tile {id}: expected {expected:?}, got {actual:?}"
    );
}

fn tray(ids: &[u32]) -> Vec<TileId> {
    ids.iter().copied().map(TileId).collect()
}

// Workspace mockup: orchestrator, two workers, two processes, a done and an idle worker.

fn workspace_tiles(cargo_test: Size, pnpm_dev: Size) -> Vec<TileSpec> {
    vec![
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Attention),
        process(3, Activity::Running, cargo_test),
        process(4, Activity::Running, pnpm_dev),
        worker(5, Activity::Done),
        worker(6, Activity::Idle),
    ]
}

fn workspace_layout() -> Layout {
    let tiles = workspace_tiles(terminal_size(90, 14), terminal_size(80, 10));
    layout(area(1030.0, 790.0), &tiles, &LayoutStyle::default())
}

#[test]
fn workspace_mockup_puts_orchestrator_in_a_full_height_left_column() {
    assert_rect(&workspace_layout(), 0, Rect::new(10.0, 10.0, 400.0, 770.0));
}

#[test]
fn workspace_mockup_puts_workers_side_by_side_in_the_top_row() {
    let layout = workspace_layout();

    assert_rect(&layout, 1, Rect::new(420.0, 10.0, 295.0, 488.0));
    assert_rect(&layout, 2, Rect::new(725.0, 10.0, 295.0, 488.0));
}

#[test]
fn workspace_mockup_puts_processes_in_the_bottom_row_at_the_tallest_requested_height() {
    let layout = workspace_layout();

    assert_rect(&layout, 3, Rect::new(420.0, 508.0, 295.0, 272.0));
    assert_rect(&layout, 4, Rect::new(725.0, 508.0, 295.0, 272.0));
}

#[test]
fn workspace_mockup_moves_done_and_idle_workers_to_the_tray() {
    assert_eq!(workspace_layout().tray, tray(&[5, 6]));
}

#[test]
fn workspace_mockup_grid_matches_when_processes_request_the_mockup_row_height() {
    let tiles = workspace_tiles(Size::new(648.0, 236.0), Size::new(576.0, 204.0));
    let layout = layout(area(1030.0, 790.0), &tiles, &LayoutStyle::default());

    // grid-template-columns: 400px 1fr 1fr; grid-template-rows: 1fr 236px.
    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 400.0, 770.0));
    assert_rect(&layout, 1, Rect::new(420.0, 10.0, 295.0, 524.0));
    assert_rect(&layout, 2, Rect::new(725.0, 10.0, 295.0, 524.0));
    assert_rect(&layout, 3, Rect::new(420.0, 544.0, 295.0, 236.0));
    assert_rect(&layout, 4, Rect::new(725.0, 544.0, 295.0, 236.0));
}

// Island mockup: a 650 × 470 user terminal next to a 380 px orchestrator column.

fn island_style() -> LayoutStyle {
    LayoutStyle {
        orchestrator_width: 380.0,
        ..LayoutStyle::default()
    }
}

fn island_layout() -> Layout {
    let tiles = vec![
        orchestrator(0),
        user(1, Size::new(650.0, 470.0)),
        worker(2, Activity::Working),
        process(3, Activity::Working, terminal_size(90, 14)),
        worker(4, Activity::Working),
        worker(5, Activity::Done),
        worker(6, Activity::Idle),
    ];

    layout(area(1060.0, 790.0), &tiles, &island_style())
}

#[test]
fn island_mockup_puts_user_terminal_top_right_at_its_fixed_size() {
    assert_rect(&island_layout(), 1, Rect::new(400.0, 10.0, 650.0, 470.0));
}

#[test]
fn island_mockup_puts_orchestrator_in_the_left_column() {
    assert_rect(&island_layout(), 0, Rect::new(10.0, 10.0, 380.0, 770.0));
}

#[test]
fn island_mockup_puts_other_tiles_below_the_user_terminal() {
    let layout = island_layout();

    assert_rect(&layout, 2, Rect::new(400.0, 490.0, 320.0, 290.0));
    assert_rect(&layout, 3, Rect::new(730.0, 490.0, 320.0, 290.0));
}

#[test]
fn island_mockup_moves_overflow_to_the_tray() {
    assert_eq!(island_layout().tray, tray(&[4, 5, 6]));
}

#[test]
fn tiles_flow_beside_a_user_terminal_when_space_allows() {
    let tiles = [
        user(0, Size::new(600.0, 400.0)),
        worker(1, Activity::Working),
        worker(2, Activity::Working),
    ];

    let layout = layout(area(1220.0, 800.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, Rect::new(10.0, 10.0, 590.0, 400.0));
}

#[test]
fn second_user_terminal_goes_left_of_the_first() {
    let tiles = [
        user(0, Size::new(400.0, 300.0)),
        user(1, Size::new(300.0, 200.0)),
    ];

    let layout = layout(area(1220.0, 800.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, Rect::new(500.0, 10.0, 300.0, 200.0));
}

#[test]
fn user_terminal_wraps_below_when_the_top_row_is_full() {
    let tiles = [
        user(0, Size::new(700.0, 300.0)),
        user(1, Size::new(600.0, 200.0)),
    ];

    let layout = layout(area(1220.0, 800.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, Rect::new(610.0, 320.0, 600.0, 200.0));
}

#[test]
fn user_terminal_larger_than_the_area_goes_to_the_tray() {
    let tiles = [user(0, Size::new(1300.0, 300.0))];

    let layout = layout(area(1220.0, 800.0), &tiles, &LayoutStyle::default());

    assert_eq!(layout.tray, tray(&[0]));
}

#[test]
fn user_terminal_without_fixed_size_flows_ahead_of_agents() {
    let tiles = [
        worker(0, Activity::Attention),
        tile(1, TileKind::User, Activity::Running),
    ];

    let layout = layout(area(300.0, 200.0), &tiles, &LayoutStyle::default());

    assert_eq!(layout.tray, tray(&[0]));
}

// Orchestrator column.

#[test]
fn orchestrator_shrinks_to_make_room_for_a_user_terminal() {
    let tiles = [orchestrator(0), user(1, Size::new(650.0, 470.0))];

    let layout = layout(area(1030.0, 790.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 350.0, 770.0));
}

#[test]
fn orchestrator_shrinks_before_a_tile_is_dropped() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Working),
        worker(3, Activity::Working),
    ];

    let layout = layout(area(1150.0, 320.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 320.0, 300.0));
}

#[test]
fn orchestrator_shrinks_only_as_much_as_needed() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Working),
        worker(3, Activity::Working),
    ];

    let layout = layout(area(1190.0, 320.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 360.0, 300.0));
}

#[test]
fn orchestrator_keeps_its_width_when_everything_fits() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Working),
    ];

    let layout = layout(area(1150.0, 320.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 400.0, 300.0));
}

#[test]
fn tile_is_dropped_when_the_orchestrator_is_at_its_minimum() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Working),
        worker(3, Activity::Working),
        worker(4, Activity::Working),
    ];

    let layout = layout(area(1150.0, 320.0), &tiles, &LayoutStyle::default());

    assert_eq!(layout.tray, tray(&[4]));
}

#[test]
fn lone_orchestrator_fills_the_area() {
    let layout = layout(
        area(1030.0, 790.0),
        &[orchestrator(0)],
        &LayoutStyle::default(),
    );

    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 1010.0, 770.0));
}

#[test]
fn orchestrator_goes_to_the_tray_when_shorter_than_its_minimum() {
    let layout = layout(
        area(1030.0, 250.0),
        &[orchestrator(0)],
        &LayoutStyle::default(),
    );

    assert_eq!(layout.tray, tray(&[0]));
}

#[test]
fn second_orchestrator_flows_as_a_tile() {
    let tiles = [orchestrator(0), orchestrator(1)];

    let layout = layout(area(1030.0, 790.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, Rect::new(420.0, 10.0, 600.0, 770.0));
}

// Small areas.

#[test]
fn tiny_area_keeps_only_orchestrator_and_user_terminal() {
    let tiles = [
        orchestrator(0),
        user(1, Size::new(600.0, 480.0)),
        worker(2, Activity::Attention),
        worker(3, Activity::Working),
        process(4, Activity::Running, terminal_size(80, 10)),
    ];

    let layout = layout(area(1000.0, 500.0), &tiles, &LayoutStyle::default());

    assert_eq!(layout.tray, tray(&[2, 3, 4]));
}

#[test]
fn tiny_area_gives_the_orchestrator_all_width_left_of_the_user_terminal() {
    let tiles = [
        orchestrator(0),
        user(1, Size::new(600.0, 480.0)),
        worker(2, Activity::Working),
    ];

    let layout = layout(area(1000.0, 500.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 370.0, 480.0));
}

#[test]
fn area_smaller_than_the_padding_puts_everything_in_the_tray() {
    let tiles = [orchestrator(0), worker(1, Activity::Working)];

    let layout = layout(area(15.0, 15.0), &tiles, &LayoutStyle::default());

    assert_eq!(layout.tray, tray(&[0, 1]));
}

#[test]
fn empty_input_gives_an_empty_layout() {
    let layout = layout(area(1030.0, 790.0), &[], &LayoutStyle::default());

    assert_eq!(layout, Layout::default());
}

// Minimized, pinned and priority.

#[test]
fn minimized_tile_goes_to_the_tray_even_when_space_is_free() {
    let tiles = [TileSpec {
        minimized: true,
        ..worker(0, Activity::Attention)
    }];

    let layout = layout(area(1030.0, 790.0), &tiles, &LayoutStyle::default());

    assert_eq!(layout.tray, tray(&[0]));
}

#[test]
fn minimized_user_terminal_goes_to_the_tray() {
    let tiles = [TileSpec {
        minimized: true,
        ..user(0, Size::new(300.0, 200.0))
    }];

    let layout = layout(area(1030.0, 790.0), &tiles, &LayoutStyle::default());

    assert_eq!(layout.tray, tray(&[0]));
}

#[test]
fn pinned_tile_beats_working_tile() {
    let tiles = [
        worker(0, Activity::Working),
        TileSpec {
            pinned: true,
            ..worker(1, Activity::Idle)
        },
    ];

    let layout = layout(area(300.0, 200.0), &tiles, &LayoutStyle::default());

    assert_eq!(layout.tray, tray(&[0]));
}

#[test]
fn pinned_done_tile_stays_next_to_active_tiles() {
    let tiles = [
        worker(0, Activity::Working),
        TileSpec {
            pinned: true,
            ..worker(1, Activity::Done)
        },
    ];

    let layout = layout(area(1030.0, 790.0), &tiles, &LayoutStyle::default());

    assert!(layout.tray.is_empty());
}

#[test]
fn attention_worker_beats_working_worker() {
    let tiles = [worker(0, Activity::Working), worker(1, Activity::Attention)];

    let layout = layout(area(300.0, 200.0), &tiles, &LayoutStyle::default());

    assert_eq!(layout.tray, tray(&[0]));
}

#[test]
fn attention_worker_pushes_an_idle_worker_to_the_tray() {
    let idle = [worker(0, Activity::Idle), worker(1, Activity::Idle)];
    let attention = [worker(0, Activity::Idle), worker(1, Activity::Attention)];

    let before = layout(area(1030.0, 200.0), &idle, &LayoutStyle::default());
    let after = layout(area(1030.0, 200.0), &attention, &LayoutStyle::default());

    assert!(before.tray.is_empty());
    assert_eq!(after.tray, tray(&[0]));
}

#[test]
fn working_worker_beats_running_process() {
    let tiles = [
        process(0, Activity::Running, terminal_size(80, 10)),
        worker(1, Activity::Working),
    ];

    let layout = layout(area(300.0, 200.0), &tiles, &LayoutStyle::default());

    assert_eq!(layout.tray, tray(&[0]));
}

#[test]
fn done_and_idle_workers_fill_the_area_when_nothing_is_active() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Done),
        worker(2, Activity::Idle),
    ];

    let layout = layout(area(1030.0, 790.0), &tiles, &LayoutStyle::default());

    assert!(layout.tray.is_empty());
}

#[test]
fn done_worker_beats_idle_worker() {
    let tiles = [worker(0, Activity::Idle), worker(1, Activity::Done)];

    let layout = layout(area(300.0, 200.0), &tiles, &LayoutStyle::default());

    assert_eq!(layout.tray, tray(&[0]));
}

#[test]
fn ties_keep_input_order() {
    let tiles = [
        worker(0, Activity::Working),
        worker(1, Activity::Working),
        worker(2, Activity::Working),
    ];

    let layout = layout(area(570.0, 200.0), &tiles, &LayoutStyle::default());

    assert_eq!(layout.tray, tray(&[2]));
}

#[test]
fn tiles_keep_their_place_when_their_activity_changes() {
    let tiles = [worker(0, Activity::Working), worker(1, Activity::Attention)];

    let layout = layout(area(1030.0, 790.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 500.0, 770.0));
}

#[test]
fn processes_go_after_agents() {
    let tiles = [
        process(0, Activity::Attention, terminal_size(80, 10)),
        worker(1, Activity::Idle),
        worker(2, Activity::Working),
    ];

    let layout = layout(area(1030.0, 790.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 2, Rect::new(10.0, 10.0, 500.0, 770.0));
}

// Process heights.

#[test]
fn process_row_takes_what_other_rows_leave_when_its_request_is_too_tall() {
    let tiles = [
        worker(0, Activity::Working),
        process(1, Activity::Running, Size::new(600.0, 900.0)),
    ];

    let layout = layout(area(300.0, 400.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, Rect::new(10.0, 180.0, 280.0, 210.0));
}

#[test]
fn process_shorter_than_the_minimum_gets_the_minimum_height() {
    let tiles = [
        worker(0, Activity::Working),
        worker(1, Activity::Working),
        process(2, Activity::Running, Size::new(600.0, 60.0)),
    ];

    let layout = layout(area(300.0, 800.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 2, Rect::new(10.0, 630.0, 280.0, 160.0));
}

#[test]
fn process_rows_share_the_height_when_no_row_is_flexible() {
    let tiles = [
        process(0, Activity::Running, Size::new(600.0, 200.0)),
        process(1, Activity::Running, Size::new(600.0, 200.0)),
    ];

    let layout = layout(area(300.0, 800.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, Rect::new(10.0, 405.0, 280.0, 385.0));
}

#[test]
fn process_without_requested_size_is_a_flexible_tile() {
    let tiles = [
        process(0, Activity::Running, Size::new(600.0, 200.0)),
        tile(1, TileKind::Process, Activity::Running),
    ];

    let layout = layout(area(300.0, 800.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, Rect::new(10.0, 220.0, 280.0, 570.0));
}

// Properties over many random inputs.

/// `SplitMix64`: tiny, deterministic and good enough for test inputs.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    fn below(&mut self, bound: u16) -> u16 {
        u16::try_from(self.next() % u64::from(bound)).unwrap()
    }

    fn chance(&mut self, percent: u16) -> bool {
        self.below(100) < percent
    }

    fn pixels(&mut self, low: u16, high: u16) -> f32 {
        f32::from(low + self.below(high - low + 1))
    }

    fn size(&mut self, low: u16, high: u16) -> Size {
        Size::new(self.pixels(low, high), self.pixels(low, high))
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        let bound = u16::try_from(items.len()).unwrap();
        items[usize::from(self.below(bound))]
    }
}

struct Case {
    area: Rect,
    tiles: Vec<TileSpec>,
    style: LayoutStyle,
}

impl Case {
    fn layout(&self) -> Layout {
        layout(self.area, &self.tiles, &self.style)
    }

    fn content(&self) -> Rect {
        self.area.inset(self.style.padding)
    }

    /// The orchestrator that gets the left column, if it is placed.
    fn column_orchestrator(&self) -> Option<TileId> {
        self.tiles
            .iter()
            .find(|tile| tile.kind == TileKind::Orchestrator && !tile.minimized)
            .map(|tile| tile.id)
    }

    fn with_tile(&self, tile: TileSpec) -> Self {
        let mut tiles = self.tiles.clone();
        tiles.push(tile);

        Self {
            area: self.area,
            tiles,
            style: self.style,
        }
    }
}

fn random_tile(random: &mut Random, id: u32) -> TileSpec {
    let kind = random.pick(&[
        TileKind::Orchestrator,
        TileKind::Worker,
        TileKind::Worker,
        TileKind::Worker,
        TileKind::Process,
        TileKind::Process,
        TileKind::User,
    ]);
    let activity = random.pick(&[
        Activity::Attention,
        Activity::Working,
        Activity::Running,
        Activity::Done,
        Activity::Idle,
    ]);
    let requested = (kind == TileKind::Process && random.chance(70))
        .then(|| Size::new(random.pixels(100, 900), random.pixels(40, 900)));
    let fixed = (kind == TileKind::User && random.chance(70))
        .then(|| Size::new(random.pixels(80, 1200), random.pixels(60, 900)));

    TileSpec {
        id: TileId(id),
        kind,
        activity,
        requested,
        fixed,
        pinned: random.chance(15),
        minimized: random.chance(10),
    }
}

fn random_style(random: &mut Random) -> LayoutStyle {
    if random.chance(50) {
        return LayoutStyle::default();
    }

    LayoutStyle {
        gap: random.pixels(0, 24),
        padding: random.pixels(0, 24),
        orchestrator_width: random.pixels(150, 700),
        min_tile: random.size(60, 400),
        min_orchestrator: random.size(100, 500),
    }
}

fn random_case(seed: u64) -> Case {
    let mut random = Random(seed);
    let area = Rect::new(
        random.pixels(0, 300),
        random.pixels(0, 300),
        random.pixels(0, 2400),
        random.pixels(0, 1500),
    );
    let style = random_style(&mut random);
    let count = random.below(13);
    let tiles = (0..u32::from(count))
        .map(|id| random_tile(&mut random, id))
        .collect();

    Case { area, tiles, style }
}

fn cases() -> impl Iterator<Item = Case> {
    (0..2000).map(random_case)
}

/// The empty space between two rects, along the axis where they are apart.
fn separation(first: Rect, second: Rect) -> f32 {
    [
        second.origin.x - first.right(),
        first.origin.x - second.right(),
        second.origin.y - first.bottom(),
        first.origin.y - second.bottom(),
    ]
    .into_iter()
    .fold(f32::MIN, f32::max)
}

/// Unpinned workers and processes rank by activity alone.
fn activity_rank(activity: Activity) -> u8 {
    match activity {
        Activity::Attention => 0,
        Activity::Working => 1,
        Activity::Running => 2,
        Activity::Done => 3,
        Activity::Idle => 4,
    }
}

#[test]
fn layout_is_deterministic() {
    for case in cases() {
        assert_eq!(case.layout(), case.layout());
    }
}

#[test]
fn every_tile_is_placed_or_in_the_tray_exactly_once() {
    for case in cases() {
        let layout = case.layout();
        let mut seen: Vec<TileId> = layout
            .placed
            .iter()
            .map(|(id, _)| *id)
            .chain(layout.tray.iter().copied())
            .collect();
        seen.sort();

        let mut expected: Vec<TileId> = case.tiles.iter().map(|tile| tile.id).collect();
        expected.sort();

        assert_eq!(seen, expected);
    }
}

#[test]
fn placed_tiles_lie_inside_the_padded_area() {
    for case in cases() {
        let content = case.content();

        for (id, rect) in case.layout().placed {
            assert!(
                rect.origin.x >= content.origin.x - FLOAT_TOLERANCE
                    && rect.origin.y >= content.origin.y - FLOAT_TOLERANCE
                    && rect.right() <= content.right() + FLOAT_TOLERANCE
                    && rect.bottom() <= content.bottom() + FLOAT_TOLERANCE,
                "{id:?} at {rect:?} is outside {content:?}"
            );
        }
    }
}

#[test]
fn placed_tiles_never_overlap_and_keep_at_least_the_gap() {
    for case in cases() {
        let placed = case.layout().placed;

        for (position, (first_id, first)) in placed.iter().enumerate() {
            for (second_id, second) in &placed[position + 1..] {
                assert!(
                    separation(*first, *second) >= case.style.gap - FLOAT_TOLERANCE,
                    "{first_id:?} {first:?} and {second_id:?} {second:?} are too close"
                );
            }
        }
    }
}

#[test]
fn fixed_sizes_never_change() {
    for case in cases() {
        let layout = case.layout();

        for tile in &case.tiles {
            let (Some(fixed), Some(rect)) = (tile.fixed, layout.rect(tile.id)) else {
                continue;
            };
            if tile.kind == TileKind::User {
                assert_eq!(rect.size, fixed);
            }
        }
    }
}

#[test]
fn minimized_tiles_are_always_in_the_tray() {
    for case in cases() {
        let layout = case.layout();

        for tile in case.tiles.iter().filter(|tile| tile.minimized) {
            assert!(layout.tray.contains(&tile.id));
        }
    }
}

#[test]
fn placed_tiles_are_at_least_their_minimum_size() {
    for case in cases() {
        let layout = case.layout();
        let column = case.column_orchestrator();

        for tile in case.tiles.iter().filter(|tile| tile.fixed.is_none()) {
            let Some(rect) = layout.rect(tile.id) else {
                continue;
            };
            let min = if Some(tile.id) == column {
                case.style.min_orchestrator
            } else {
                case.style.min_tile
            };

            assert!(
                rect.size.width >= min.width - FLOAT_TOLERANCE
                    && rect.size.height >= min.height - FLOAT_TOLERANCE,
                "{:?} is {:?}, smaller than {min:?}",
                tile.id,
                rect.size
            );
        }
    }
}

#[test]
fn attention_tile_in_the_tray_means_no_lower_priority_tile_is_placed() {
    let ranked = |tile: &&TileSpec| {
        matches!(tile.kind, TileKind::Worker | TileKind::Process) && !tile.pinned && !tile.minimized
    };

    for case in cases() {
        let layout = case.layout();
        let attention_in_tray = case
            .tiles
            .iter()
            .filter(ranked)
            .any(|tile| tile.activity == Activity::Attention && layout.tray.contains(&tile.id));
        if !attention_in_tray {
            continue;
        }

        for tile in case.tiles.iter().filter(ranked) {
            if activity_rank(tile.activity) > activity_rank(Activity::Attention) {
                assert!(layout.tray.contains(&tile.id), "{:?} is placed", tile.id);
            }
        }
    }
}

#[test]
fn adding_an_idle_tile_never_moves_another_tile_to_the_tray() {
    for case in cases() {
        let before = case.layout();
        let idle = worker(1000, Activity::Idle);
        let after = case.with_tile(idle).layout();

        for (id, _) in &before.placed {
            assert!(after.rect(*id).is_some(), "{id:?} moved to the tray");
        }
    }
}

#[test]
fn adding_a_tile_that_lands_in_the_tray_changes_nothing_else() {
    for case in cases() {
        let before = case.layout();
        let idle = worker(1000, Activity::Idle);
        let after = case.with_tile(idle).layout();
        if after.rect(idle.id).is_some() {
            continue;
        }

        assert_eq!(after.placed, before.placed);
    }
}

#[test]
fn placed_orchestrator_column_spans_the_full_height() {
    for case in cases() {
        let Some(id) = case.column_orchestrator() else {
            continue;
        };
        let layout = case.layout();
        let Some(rect) = layout.rect(id) else {
            continue;
        };

        let content = case.content();
        assert!(near(
            Rect::new(
                content.origin.x,
                content.origin.y,
                rect.size.width,
                content.size.height
            ),
            rect,
            FLOAT_TOLERANCE
        ));
    }
}

#[test]
fn random_cases_place_some_tiles() {
    let placed: usize = cases().map(|case| case.layout().placed.len()).sum();
    let attention_dropped = cases()
        .filter(|case| {
            let layout = case.layout();
            case.tiles
                .iter()
                .any(|tile| tile.activity == Activity::Attention && layout.tray.contains(&tile.id))
        })
        .count();

    // Guards against a generator that only makes empty or trivial cases.
    assert!(placed > 2000 && attention_dropped > 0);
}
