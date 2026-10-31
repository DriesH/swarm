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

/// A worker box in the default 1030 × 790 area with the orchestrator beside it:
/// 65 % of the 1010 px content width, then a 10 px gap.
const COLUMN_WIDTH: f32 = 656.5;
const RIGHT_X: f32 = 676.5;
const RIGHT_WIDTH: f32 = 343.5;
const SMALL: f32 = 286.0;
const GAP: f32 = 10.0;
const TALL: f32 = 2.0 * SMALL + GAP;
const TOP: f32 = 10.0;

fn default_layout(tiles: &[TileSpec]) -> Layout {
    layout(area(1030.0, 790.0), tiles, &LayoutStyle::default())
}

/// A box in the first column on the right of the default layout.
fn box_at(top: f32, height: f32) -> Rect {
    Rect::new(RIGHT_X, top, RIGHT_WIDTH, height)
}

// Orchestrator width.

#[test]
fn orchestrator_takes_65_percent_of_the_width() {
    for (width, expected) in [(1030.0, 656.5), (1300.0, 832.0), (800.0, 507.0)] {
        let tiles = [orchestrator(0), worker(1, Activity::Working)];

        let layout = layout(area(width, 790.0), &tiles, &LayoutStyle::default());

        assert_rect(&layout, 0, Rect::new(10.0, 10.0, expected, 770.0));
    }
}

#[test]
fn the_right_side_keeps_room_for_one_minimum_tile() {
    let tiles = [orchestrator(0), worker(1, Activity::Working)];

    let layout = layout(area(700.0, 790.0), &tiles, &LayoutStyle::default());

    // 65 % of 680 is 442, but that leaves less than 260 + 10.
    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 410.0, 770.0));
    assert_rect(&layout, 1, Rect::new(430.0, 10.0, 260.0, SMALL));
}

#[test]
fn orchestrator_is_never_narrower_than_its_minimum() {
    let style = LayoutStyle {
        orchestrator_share: 0.3,
        ..LayoutStyle::default()
    };
    let tiles = [orchestrator(0), worker(1, Activity::Working)];

    let layout = layout(area(1030.0, 790.0), &tiles, &style);

    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 320.0, 770.0));
}

#[test]
fn orchestrator_takes_all_the_width_when_no_tile_fits_beside_it() {
    let tiles = [orchestrator(0), worker(1, Activity::Working)];

    let layout = layout(area(600.0, 790.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 580.0, 770.0));
    assert_eq!(layout.tray, tray(&[1]));
}

#[test]
fn orchestrator_takes_all_the_width_when_the_other_tiles_are_minimized() {
    let tiles = [
        orchestrator(0),
        TileSpec {
            minimized: true,
            ..worker(1, Activity::Working)
        },
    ];

    let layout = default_layout(&tiles);

    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 1010.0, 770.0));
    assert_eq!(layout.tray, tray(&[1]));
}

#[test]
fn lone_orchestrator_fills_the_area() {
    let layout = default_layout(&[orchestrator(0)]);

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
fn orchestrator_shrinks_to_make_room_for_a_user_terminal() {
    let tiles = [orchestrator(0), user(1, Size::new(650.0, 470.0))];

    let layout = default_layout(&tiles);

    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 350.0, 770.0));
}

#[test]
fn second_orchestrator_flows_as_a_small_box() {
    let tiles = [orchestrator(0), orchestrator(1)];

    let layout = default_layout(&tiles);

    assert_rect(&layout, 1, box_at(TOP, SMALL));
}

// Small boxes.

#[test]
fn workers_are_small_boxes_stacked_from_the_top() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Working),
    ];

    let layout = default_layout(&tiles);

    assert_rect(&layout, 1, box_at(TOP, SMALL));
    assert_rect(&layout, 2, box_at(TOP + SMALL + GAP, SMALL));
}

#[test]
fn a_single_worker_is_not_stretched_to_the_full_height() {
    let tiles = [orchestrator(0), worker(1, Activity::Working)];

    let layout = default_layout(&tiles);

    assert_rect(&layout, 1, box_at(TOP, SMALL));
    assert!(
        layout
            .rect(TileId(1))
            .is_some_and(|rect| rect.bottom() < 400.0)
    );
}

#[test]
fn a_box_that_does_not_fit_below_the_others_goes_to_the_tray() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Working),
        worker(3, Activity::Working),
    ];

    let layout = default_layout(&tiles);

    assert_eq!(layout.tray, tray(&[3]));
}

#[test]
fn the_style_sets_the_small_box_height() {
    let style = LayoutStyle {
        small_tile_height: 200.0,
        ..LayoutStyle::default()
    };
    let tiles = [orchestrator(0), worker(1, Activity::Working)];

    let layout = layout(area(1030.0, 790.0), &tiles, &style);

    assert_rect(&layout, 1, box_at(TOP, 200.0));
}

#[test]
fn a_small_box_is_never_shorter_than_the_minimum_tile() {
    let style = LayoutStyle {
        small_tile_height: 100.0,
        ..LayoutStyle::default()
    };
    let tiles = [orchestrator(0), worker(1, Activity::Working)];

    let layout = layout(area(1030.0, 790.0), &tiles, &style);

    assert_rect(&layout, 1, box_at(TOP, 160.0));
}

#[test]
fn the_first_box_of_a_column_is_cut_to_a_short_area() {
    let tiles = [worker(0, Activity::Working)];

    let layout = layout(area(600.0, 250.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 285.0, 230.0));
}

#[test]
fn a_box_is_not_cut_below_the_minimum_tile_height() {
    let tiles = [worker(0, Activity::Working)];

    let layout = layout(area(600.0, 170.0), &tiles, &LayoutStyle::default());

    assert_eq!(layout.tray, tray(&[0]));
}

// Columns on a wide right side.

fn wide_tiles(workers: u32) -> Vec<TileSpec> {
    std::iter::once(orchestrator(0))
        .chain((1..=workers).map(|id| worker(id, Activity::Working)))
        .collect()
}

#[test]
fn a_wide_right_side_holds_two_columns() {
    let layout = layout(area(1700.0, 790.0), &wide_tiles(5), &LayoutStyle::default());

    // Content 1680 wide: the orchestrator takes 1092, two columns of 284 share the 578 left.
    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 1092.0, 770.0));
    assert_rect(&layout, 1, Rect::new(1112.0, 10.0, 284.0, SMALL));
    assert_rect(&layout, 2, Rect::new(1112.0, 306.0, 284.0, SMALL));
    assert_rect(&layout, 3, Rect::new(1406.0, 10.0, 284.0, SMALL));
    assert_rect(&layout, 4, Rect::new(1406.0, 306.0, 284.0, SMALL));
    assert_eq!(layout.tray, tray(&[5]));
}

#[test]
fn boxes_fill_a_column_from_top_to_bottom_before_the_next_column() {
    let layout = layout(area(1700.0, 790.0), &wide_tiles(3), &LayoutStyle::default());

    let tops: Vec<Option<f32>> = (1..=3)
        .map(|id| layout.rect(TileId(id)).map(|rect| rect.origin.y))
        .collect();
    let lefts: Vec<Option<f32>> = (1..=3)
        .map(|id| layout.rect(TileId(id)).map(|rect| rect.origin.x))
        .collect();

    assert_eq!(tops, [Some(10.0), Some(306.0), Some(10.0)]);
    assert_eq!(lefts, [Some(1112.0), Some(1112.0), Some(1406.0)]);
}

#[test]
fn a_second_column_needs_a_right_side_of_two_minimum_tiles() {
    let one_column = layout(area(1560.0, 790.0), &wide_tiles(3), &LayoutStyle::default());
    let two_columns = layout(area(1563.0, 790.0), &wide_tiles(3), &LayoutStyle::default());

    // The right side is 529 px wide, then 530.05 px.
    assert_eq!(one_column.tray, tray(&[3]));
    assert!(two_columns.tray.is_empty());
    assert_rect(&one_column, 1, Rect::new(1021.0, 10.0, 529.0, SMALL));
    assert!(
        two_columns
            .rect(TileId(1))
            .is_some_and(|rect| (rect.size.width - 260.025).abs() < 0.1)
    );
}

// Tiles that need attention.

#[test]
fn a_worker_that_needs_attention_is_two_small_boxes_tall() {
    let working = default_layout(&[orchestrator(0), worker(1, Activity::Working)]);
    let blocked = default_layout(&[orchestrator(0), worker(1, Activity::Attention)]);

    assert_rect(&working, 1, box_at(TOP, SMALL));
    assert_rect(&blocked, 1, box_at(TOP, TALL));
}

#[test]
fn an_attention_box_is_never_taller_than_two_small_boxes() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Attention),
    ];

    let layout = layout(area(1030.0, 1410.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, box_at(TOP, SMALL));
    assert_rect(&layout, 2, box_at(306.0, TALL));
}

#[test]
fn a_lone_attention_box_does_not_take_the_whole_lane() {
    let tiles = [orchestrator(0), worker(1, Activity::Attention)];

    let layout = layout(area(1030.0, 1410.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, box_at(TOP, TALL));
}

#[test]
fn an_attention_box_does_not_push_a_working_worker_to_the_tray() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Attention),
    ];

    let layout = default_layout(&tiles);

    // A small box and a tall one need 878 of the 770 px, so the box that needs attention
    // stays small.
    assert_rect(&layout, 1, box_at(TOP, SMALL));
    assert_rect(&layout, 2, box_at(306.0, SMALL));
    assert!(layout.tray.is_empty());
}

#[test]
fn an_attention_box_shrinks_back_when_the_worker_works_again() {
    let blocked = [
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Attention),
    ];
    let working = [
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Working),
    ];
    let style = LayoutStyle::default();

    let before = layout(area(1030.0, 1000.0), &blocked, &style).rect(TileId(2));
    let after = layout(area(1030.0, 1000.0), &working, &style).rect(TileId(2));

    assert_eq!(before.map(|rect| rect.size.height), Some(TALL));
    assert_eq!(after.map(|rect| rect.size.height), Some(SMALL));
    assert_eq!(
        before.map(|rect| rect.origin),
        after.map(|rect| rect.origin)
    );
}

#[test]
fn two_attention_boxes_start_small_when_tall_boxes_would_send_one_to_the_tray() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Attention),
        worker(2, Activity::Attention),
    ];

    let layout = default_layout(&tiles);

    assert_rect(&layout, 1, box_at(TOP, SMALL));
    assert_rect(&layout, 2, box_at(306.0, SMALL));
}

#[test]
fn two_attention_boxes_are_both_tall_when_there_is_room() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Attention),
        worker(2, Activity::Attention),
    ];

    let layout = layout(area(1030.0, 1410.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, box_at(TOP, TALL));
    assert_rect(&layout, 2, box_at(602.0, TALL));
}

#[test]
fn an_attention_box_gets_its_own_column_when_the_right_side_is_wide() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Attention),
    ];

    let layout = layout(area(1700.0, 790.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, Rect::new(1112.0, 10.0, 284.0, SMALL));
    assert_rect(&layout, 2, Rect::new(1406.0, 10.0, 284.0, TALL));
}

#[test]
fn a_worker_that_needs_attention_keeps_its_place_in_the_stack() {
    let working = [
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Working),
    ];
    let blocked = [
        orchestrator(0),
        worker(1, Activity::Attention),
        worker(2, Activity::Working),
    ];

    let before = default_layout(&working)
        .rect(TileId(1))
        .map(|rect| rect.origin);
    let after = default_layout(&blocked)
        .rect(TileId(1))
        .map(|rect| rect.origin);

    assert_eq!(before, after);
}

#[test]
fn a_process_with_a_requested_height_does_not_grow() {
    let tiles = [
        orchestrator(0),
        process(1, Activity::Attention, terminal_size(80, 10)),
    ];

    let layout = default_layout(&tiles);

    assert_rect(&layout, 1, box_at(TOP, 204.0));
}

#[test]
fn a_running_process_stacks_below_a_tall_attention_box() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Attention),
        process(2, Activity::Running, terminal_size(80, 10)),
    ];

    let layout = layout(area(1030.0, 1000.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, box_at(TOP, TALL));
    assert_rect(&layout, 2, box_at(602.0, 204.0));
    assert!(layout.tray.is_empty());
}

#[test]
fn a_running_process_that_does_not_fit_below_a_tall_attention_box_goes_to_the_tray() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Attention),
        process(2, Activity::Running, terminal_size(80, 10)),
    ];

    let layout = default_layout(&tiles);

    assert_rect(&layout, 1, box_at(TOP, TALL));
    assert_eq!(layout.tray, tray(&[2]));
}

#[test]
fn done_and_idle_workers_stack_below_a_tall_attention_box() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Attention),
        worker(2, Activity::Done),
        worker(3, Activity::Idle),
    ];

    let layout = layout(area(1030.0, 1000.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, box_at(TOP, TALL));
    assert_rect(&layout, 2, box_at(602.0, SMALL));
    assert_eq!(layout.tray, tray(&[3]));
}

// Workspace mockup: orchestrator, two workers, two processes, a done and an idle worker.

fn workspace_tiles() -> Vec<TileSpec> {
    vec![
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Attention),
        process(3, Activity::Running, terminal_size(90, 14)),
        process(4, Activity::Running, terminal_size(80, 10)),
        worker(5, Activity::Done),
        worker(6, Activity::Idle),
    ]
}

#[test]
fn workspace_mockup_shows_both_workers() {
    let layout = default_layout(&workspace_tiles());

    assert_rect(&layout, 0, Rect::new(10.0, 10.0, COLUMN_WIDTH, 770.0));
    assert_rect(&layout, 1, box_at(TOP, SMALL));
    assert_rect(&layout, 2, box_at(306.0, SMALL));
}

#[test]
fn workspace_mockup_moves_processes_done_and_idle_workers_to_the_tray_when_crowded() {
    assert_eq!(default_layout(&workspace_tiles()).tray, tray(&[3, 4, 5, 6]));
}

#[test]
fn workspace_mockup_in_a_tall_area_stacks_the_processes_below_the_tall_box() {
    let layout = layout(
        area(1030.0, 1400.0),
        &workspace_tiles(),
        &LayoutStyle::default(),
    );

    assert_rect(&layout, 1, box_at(TOP, SMALL));
    assert_rect(&layout, 2, box_at(306.0, TALL));
    assert_rect(&layout, 3, box_at(898.0, 272.0));
    assert_rect(&layout, 4, box_at(1180.0, 204.0));
    assert_eq!(layout.tray, tray(&[5, 6]));
}

#[test]
fn processes_stack_under_the_workers_when_none_needs_attention() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Working),
        process(3, Activity::Running, terminal_size(90, 14)),
        process(4, Activity::Running, terminal_size(80, 10)),
    ];

    let layout = layout(area(1030.0, 1400.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, box_at(TOP, SMALL));
    assert_rect(&layout, 2, box_at(306.0, SMALL));
    assert_rect(&layout, 3, box_at(602.0, 272.0));
    assert_rect(&layout, 4, box_at(884.0, 204.0));
    assert!(layout.tray.is_empty());
}

// Island mockup: a 650 × 470 user terminal next to the orchestrator column.

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

    layout(area(1060.0, 790.0), &tiles, &LayoutStyle::default())
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

    assert_rect(&layout, 2, Rect::new(400.0, 490.0, 320.0, SMALL));
    assert_rect(&layout, 3, Rect::new(730.0, 490.0, 320.0, 272.0));
}

#[test]
fn island_mockup_moves_overflow_to_the_tray() {
    assert_eq!(island_layout().tray, tray(&[4, 5, 6]));
}

#[test]
fn a_tall_user_terminal_leaves_a_lane_beside_it_for_the_workers() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Attention),
        user(3, Size::new(650.0, 878.0)),
    ];

    let layout = layout(area(1340.0, 898.0), &tiles, &LayoutStyle::default());

    assert!(layout.tray.is_empty());
    assert_rect(&layout, 3, Rect::new(680.0, 10.0, 650.0, 878.0));
    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 390.0, 878.0));
    assert_rect(&layout, 1, Rect::new(410.0, 10.0, 260.0, SMALL));
    assert_rect(&layout, 2, Rect::new(410.0, 306.0, 260.0, TALL));
}

#[test]
fn a_user_terminal_with_room_below_it_keeps_the_orchestrator_wide() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Working),
        user(2, Size::new(300.0, 300.0)),
    ];

    let layout = layout(area(1340.0, 898.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 858.0, 878.0));
    assert!(layout.tray.is_empty());
}

#[test]
fn tiles_flow_beside_a_user_terminal_when_space_allows() {
    let tiles = [
        user(0, Size::new(600.0, 400.0)),
        worker(1, Activity::Working),
        worker(2, Activity::Working),
    ];

    let layout = layout(area(1220.0, 800.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, Rect::new(10.0, 10.0, 290.0, SMALL));
    assert_rect(&layout, 2, Rect::new(310.0, 10.0, 290.0, SMALL));
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

    let layout = default_layout(&tiles);

    assert_eq!(layout.tray, tray(&[0]));
}

#[test]
fn minimized_user_terminal_goes_to_the_tray() {
    let tiles = [TileSpec {
        minimized: true,
        ..user(0, Size::new(300.0, 200.0))
    }];

    let layout = default_layout(&tiles);

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

    let layout = default_layout(&tiles);

    assert!(layout.tray.is_empty());
}

#[test]
fn attention_worker_beats_working_worker() {
    let tiles = [worker(0, Activity::Working), worker(1, Activity::Attention)];

    let layout = layout(area(300.0, 200.0), &tiles, &LayoutStyle::default());

    assert_eq!(layout.tray, tray(&[0]));
}

#[test]
fn an_idle_worker_stays_next_to_a_worker_that_needs_attention_when_there_is_room() {
    let tiles = [worker(0, Activity::Idle), worker(1, Activity::Attention)];

    let layout = layout(area(1030.0, 200.0), &tiles, &LayoutStyle::default());

    assert!(layout.tray.is_empty());
}

#[test]
fn attention_worker_pushes_an_idle_worker_to_the_tray_when_there_is_no_room() {
    let tiles = [worker(0, Activity::Idle), worker(1, Activity::Attention)];

    let layout = layout(area(350.0, 200.0), &tiles, &LayoutStyle::default());

    assert_eq!(layout.tray, tray(&[0]));
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

    let layout = default_layout(&tiles);

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
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Working),
        worker(3, Activity::Working),
    ];

    let layout = default_layout(&tiles);

    assert_eq!(layout.tray, tray(&[3]));
}

#[test]
fn a_crowded_right_side_sends_idle_then_running_then_working_to_the_tray() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Idle),
        worker(2, Activity::Working),
        worker(3, Activity::Attention),
        process(4, Activity::Running, terminal_size(80, 10)),
        worker(5, Activity::Working),
        worker(6, Activity::Done),
    ];

    let layout = default_layout(&tiles);

    // Two boxes fit. The one that needs attention goes first, then the earlier working one.
    assert_eq!(layout.tray, tray(&[1, 4, 5, 6]));
    assert!(layout.rect(TileId(2)).is_some());
    assert!(layout.rect(TileId(3)).is_some());
}

#[test]
fn a_crowded_right_side_keeps_a_done_worker_over_an_idle_one() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Idle),
        worker(2, Activity::Done),
    ];

    let layout = layout(area(1030.0, 500.0), &tiles, &LayoutStyle::default());

    assert_eq!(layout.tray, tray(&[1]));
}

#[test]
fn tiles_keep_their_place_when_their_activity_changes() {
    let working = [worker(0, Activity::Working), worker(1, Activity::Working)];
    let attention = [worker(0, Activity::Working), worker(1, Activity::Attention)];

    let before = default_layout(&working);
    let after = default_layout(&attention);

    assert_rect(&before, 0, Rect::new(10.0, 10.0, 330.0, SMALL));
    assert_eq!(before.rect(TileId(0)), after.rect(TileId(0)));
}

#[test]
fn processes_go_after_agents() {
    let tiles = [
        process(0, Activity::Attention, terminal_size(80, 10)),
        worker(1, Activity::Idle),
        worker(2, Activity::Working),
    ];

    let layout = layout(area(350.0, 790.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 2, Rect::new(10.0, 10.0, 330.0, SMALL));
    assert_rect(&layout, 0, Rect::new(10.0, 306.0, 330.0, 204.0));
    assert_eq!(layout.tray, tray(&[1]));
}

// Process heights.

#[test]
fn process_with_a_requested_height_gets_it() {
    let tiles = [
        orchestrator(0),
        process(1, Activity::Running, terminal_size(90, 14)),
    ];

    let layout = default_layout(&tiles);

    assert_rect(&layout, 1, box_at(TOP, 272.0));
}

#[test]
fn process_without_a_requested_size_is_a_small_box() {
    let tiles = [
        orchestrator(0),
        tile(1, TileKind::Process, Activity::Running),
    ];

    let layout = default_layout(&tiles);

    assert_rect(&layout, 1, box_at(TOP, SMALL));
}

#[test]
fn process_shorter_than_the_minimum_gets_the_minimum_height() {
    let tiles = [
        orchestrator(0),
        process(1, Activity::Running, Size::new(600.0, 60.0)),
    ];

    let layout = default_layout(&tiles);

    assert_rect(&layout, 1, box_at(TOP, 160.0));
}

#[test]
fn process_taller_than_the_area_is_cut_to_it_when_it_is_first_in_its_column() {
    let tiles = [process(0, Activity::Running, Size::new(600.0, 900.0))];

    let layout = layout(area(300.0, 400.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 280.0, 380.0));
}

#[test]
fn process_that_does_not_fit_below_the_others_goes_to_the_next_column_or_the_tray() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Working),
        process(2, Activity::Running, Size::new(600.0, 600.0)),
    ];

    let layout = default_layout(&tiles);

    assert_eq!(layout.tray, tray(&[2]));
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
    let fixed_chance = match kind {
        TileKind::User => 70,
        TileKind::Process => 35,
        _ => 0,
    };
    let fixed = random
        .chance(fixed_chance)
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
        orchestrator_share: f32::from(random.below(101)) / 100.0,
        min_tile: random.size(60, 400),
        min_orchestrator: random.size(100, 500),
        small_tile_height: random.pixels(40, 500),
    }
}

fn random_tiles(random: &mut Random) -> Vec<TileSpec> {
    let count = random.below(13);

    (0..u32::from(count))
        .map(|id| random_tile(random, id))
        .collect()
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
    let tiles = random_tiles(&mut random);

    Case { area, tiles, style }
}

/// The smallest area the app gives the layout: the main window's minimum size of
/// 1100 × 700, less the sidebar (248), the margins, the borders, the tab bar (46), the
/// status bar (38) and the right panel (320).
const APP_MIN_AREA: (u16, u16) = (522, 598);

/// A case like the app makes: the default style and an area of at least [`APP_MIN_AREA`].
fn app_case(seed: u64) -> Case {
    let mut random = Random(seed);
    let (min_width, min_height) = APP_MIN_AREA;
    let area = Rect::new(
        0.0,
        0.0,
        random.pixels(min_width, 2400),
        random.pixels(min_height, 1500),
    );
    let style = LayoutStyle {
        small_tile_height: random.pixels(200, 400),
        ..LayoutStyle::default()
    };
    let tiles = random_tiles(&mut random);

    Case { area, tiles, style }
}

fn cases() -> impl Iterator<Item = Case> {
    (0..2000).map(random_case)
}

fn app_cases() -> impl Iterator<Item = Case> {
    (0..2000).map(app_case)
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
fn no_placed_tile_is_smaller_than_the_minimum_tile_in_an_app_sized_area() {
    for case in app_cases() {
        let layout = case.layout();
        let column = case.column_orchestrator();

        for tile in &case.tiles {
            let user_island = tile.kind == TileKind::User && tile.fixed.is_some();
            let Some(rect) = layout.rect(tile.id) else {
                continue;
            };
            if user_island {
                continue;
            }

            let min = if Some(tile.id) == column {
                case.style.min_orchestrator
            } else {
                case.style.min_tile
            };

            assert!(
                rect.size.width >= min.width - FLOAT_TOLERANCE
                    && rect.size.height >= min.height - FLOAT_TOLERANCE,
                "{:?} ({:?}, fixed {:?}) is {:?}, smaller than {min:?} in {:?}",
                tile.id,
                tile.kind,
                tile.fixed,
                rect.size,
                case.area
            );
        }
    }
}

#[test]
fn attention_tile_in_the_tray_means_no_lower_priority_tile_is_placed() {
    // Fixed terminals are packed on their own, so they don't follow the ranking.
    let ranked = |tile: &&TileSpec| {
        matches!(tile.kind, TileKind::Worker | TileKind::Process)
            && !tile.pinned
            && !tile.minimized
            && tile.fixed.is_none()
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
fn boxes_without_a_size_of_their_own_are_never_taller_than_a_small_box() {
    for case in cases() {
        let layout = case.layout();
        let column = case.column_orchestrator();
        let small = case
            .style
            .small_tile_height
            .max(case.style.min_tile.height.max(1.0));

        for tile in case.tiles.iter().filter(|tile| {
            tile.fixed.is_none()
                && tile.requested.is_none()
                && tile.activity != Activity::Attention
                && Some(tile.id) != column
        }) {
            let Some(rect) = layout.rect(tile.id) else {
                continue;
            };

            assert!(
                rect.size.height <= small + FLOAT_TOLERANCE,
                "{:?} is {:?} high, a small box is {small}",
                tile.id,
                rect.size.height
            );
        }
    }
}

#[test]
fn the_orchestrator_is_never_wider_than_its_share_when_a_tile_sits_beside_it() {
    for case in cases() {
        let Some(id) = case.column_orchestrator() else {
            continue;
        };
        let layout = case.layout();
        let Some(rect) = layout.rect(id) else {
            continue;
        };

        // Without a flow tile the column takes all the width left of the islands.
        let flow_tile_placed = case
            .tiles
            .iter()
            .any(|tile| tile.id != id && tile.fixed.is_none() && layout.rect(tile.id).is_some());
        if !flow_tile_placed {
            continue;
        }

        let share = case.content().size.width * case.style.orchestrator_share.clamp(0.0, 1.0);
        assert!(
            rect.size.width
                <= share.max(case.style.min_orchestrator.width.max(1.0)) + FLOAT_TOLERANCE,
            "{rect:?} is wider than {share}"
        );
    }
}

/// The size of the placed fixed tiles together, from the top-right corner of the content.
fn island_block(case: &Case, layout: &Layout) -> Option<Size> {
    let content = case.content();

    case.tiles
        .iter()
        .filter(|tile| tile.fixed.is_some())
        .filter_map(|tile| layout.rect(tile.id))
        .map(|rect| {
            Size::new(
                content.right() - rect.origin.x,
                rect.bottom() - content.origin.y,
            )
        })
        .reduce(|first, second| {
            Size::new(
                first.width.max(second.width),
                first.height.max(second.height),
            )
        })
}

#[test]
fn a_lane_that_is_free_at_the_minimum_orchestrator_width_is_used() {
    let probe = worker(1000, Activity::Working);

    for case in cases() {
        let Some(column) = case.column_orchestrator() else {
            continue;
        };
        let tiles: Vec<TileSpec> = case
            .tiles
            .iter()
            .filter(|tile| tile.id == column || tile.fixed.is_some())
            .copied()
            .chain([probe])
            .collect();
        let case = Case {
            tiles,
            area: case.area,
            style: case.style,
        };
        let layout = case.layout();
        if layout.rect(column).is_none() {
            continue;
        }

        let (content, style) = (case.content(), case.style);
        let block = island_block(&case, &layout).unwrap_or_default();
        let left = content.size.width - style.min_orchestrator.width.max(1.0) - style.gap;
        let lane_width = style.min_tile.width.max(1.0);
        let lane_height = style.min_tile.height.max(1.0);
        let lane_below =
            left >= lane_width && content.size.height - block.height - style.gap >= lane_height;
        let lane_beside =
            left - block.width - style.gap >= lane_width && block.height >= lane_height;
        if !(lane_below || lane_beside) {
            continue;
        }

        assert!(
            layout.rect(probe.id).is_some(),
            "no room found for a worker in {:?} with islands {block:?}",
            case.area
        );
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

// Fixed process terminals.

fn fixed_process(id: u32, fixed: Size) -> TileSpec {
    TileSpec {
        fixed: Some(fixed),
        ..tile(id, TileKind::Process, Activity::Running)
    }
}

#[test]
fn fixed_process_that_fits_gets_its_exact_size() {
    let tiles = [
        worker(0, Activity::Working),
        fixed_process(1, terminal_size(80, 12)),
    ];

    let layout = layout(area(1220.0, 800.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, Rect::new(634.0, 10.0, 576.0, 238.0));
}

#[test]
fn fixed_process_wider_than_the_area_is_clamped_to_it() {
    let tiles = [fixed_process(0, Size::new(2000.0, 300.0))];

    let layout = layout(area(800.0, 600.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 780.0, 300.0));
}

#[test]
fn fixed_process_taller_than_the_area_is_clamped_to_it() {
    let tiles = [fixed_process(0, Size::new(400.0, 2000.0))];

    let layout = layout(area(800.0, 600.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 0, Rect::new(390.0, 10.0, 400.0, 580.0));
}

#[test]
fn fixed_process_leaves_room_for_the_orchestrator_column() {
    let tiles = [orchestrator(0), fixed_process(1, Size::new(2000.0, 300.0))];

    let layout = layout(area(1000.0, 600.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 320.0, 580.0));
    assert_rect(&layout, 1, Rect::new(340.0, 10.0, 650.0, 300.0));
}

#[test]
fn fixed_process_never_goes_below_the_minimum_tile_size() {
    let tiles = [fixed_process(0, Size::new(50.0, 50.0))];

    let layout = layout(area(800.0, 600.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 0, Rect::new(530.0, 10.0, 260.0, 160.0));
}

#[test]
fn fixed_process_that_would_be_squeezed_below_the_minimum_goes_to_the_tray() {
    let tiles = [orchestrator(0), fixed_process(1, Size::new(578.0, 822.0))];

    let layout = layout(area(536.0, 1218.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 516.0, 1198.0));
    assert_eq!(layout.tray, tray(&[1]));
}

#[test]
fn other_tiles_reflow_around_a_fixed_process() {
    let tiles = [
        worker(0, Activity::Working),
        worker(1, Activity::Working),
        fixed_process(2, Size::new(500.0, 300.0)),
    ];

    let layout = layout(area(1220.0, 800.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 2, Rect::new(710.0, 10.0, 500.0, 300.0));
    assert_rect(&layout, 0, Rect::new(10.0, 10.0, 340.0, SMALL));
    assert_rect(&layout, 1, Rect::new(360.0, 10.0, 340.0, SMALL));
}

#[test]
fn user_terminal_is_packed_before_a_fixed_process() {
    let tiles = [
        fixed_process(0, Size::new(400.0, 300.0)),
        user(1, Size::new(300.0, 200.0)),
    ];

    let layout = layout(area(1220.0, 800.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, Rect::new(910.0, 10.0, 300.0, 200.0));
    assert_rect(&layout, 0, Rect::new(500.0, 10.0, 400.0, 300.0));
}
