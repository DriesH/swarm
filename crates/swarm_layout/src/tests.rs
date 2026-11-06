use super::*;

fn tile(id: u32, kind: TileKind, activity: Activity) -> TileSpec {
    TileSpec::new(TileId(id), kind, activity)
}

fn worker(id: u32, activity: Activity) -> TileSpec {
    tile(id, TileKind::Worker, activity)
}

fn large_worker(id: u32, activity: Activity) -> TileSpec {
    TileSpec {
        large: true,
        ..worker(id, activity)
    }
}

fn orchestrator(id: u32) -> TileSpec {
    tile(id, TileKind::Orchestrator, Activity::Working)
}

fn process(id: u32, activity: Activity) -> TileSpec {
    tile(id, TileKind::Process, activity)
}

fn user(id: u32, fixed: Cells) -> TileSpec {
    TileSpec {
        fixed: Some(fixed),
        ..tile(id, TileKind::User, Activity::Running)
    }
}

fn area(width: f32, height: f32) -> Rect {
    Rect::new(0.0, 0.0, width, height)
}

/// Whole cells to pixels.
fn c(cells: u16) -> f32 {
    f32::from(cells) * CELL
}

/// A rect given in cells from the top-left corner of the area. The padding is part of
/// the position: the first cell inside the default padding is 1.
fn cell_rect(x: u16, y: u16, width: u16, height: u16) -> Rect {
    Rect::new(c(x), c(y), c(width), c(height))
}

fn assert_rect(layout: &Layout, id: u32, expected: Rect) {
    assert_eq!(layout.rect(TileId(id)), Some(expected), "tile {id}");
}

fn tray(ids: &[u32]) -> Vec<TileId> {
    ids.iter().copied().map(TileId).collect()
}

/// The default 1030 × 790 area: 126 × 96 cells of content. The orchestrator takes 65 %
/// of the width (81 cells), then a gap; the right side is 44 cells wide.
const COLUMN_WIDTH: u16 = 81;
const FULL_WIDTH: u16 = 126;
const FULL_HEIGHT: u16 = 96;
const RIGHT_X: u16 = 83;
const RIGHT_WIDTH: u16 = 44;
const SMALL: u16 = 36;
const PROCESS: u16 = 28;
const GAP: u16 = 1;
const TALL: u16 = 2 * SMALL + GAP;
const TOP: u16 = 1;

fn default_layout(tiles: &[TileSpec]) -> Layout {
    layout(area(1030.0, 790.0), tiles, &LayoutStyle::default())
}

/// A box in the first column on the right of the default layout.
fn box_at(top: u16, height: u16) -> Rect {
    cell_rect(RIGHT_X, top, RIGHT_WIDTH, height)
}

/// The orchestrator column of the default layout.
fn column() -> Rect {
    cell_rect(1, 1, COLUMN_WIDTH, FULL_HEIGHT)
}

// The grid.

#[test]
fn the_content_area_is_whole_cells_and_the_rest_goes_right_and_bottom() {
    let exact = content_rect(area(1030.0, 790.0), &LayoutStyle::default());
    let spare = content_rect(area(1037.0, 797.0), &LayoutStyle::default());
    let moved = content_rect(Rect::new(5.0, 3.0, 1037.0, 797.0), &LayoutStyle::default());

    // 1030 - 16 = 1014 px is 126 cells and 6 px; 790 - 16 = 774 px is 96 cells and 6 px.
    assert_eq!(exact, Rect::new(8.0, 8.0, 1008.0, 768.0));
    assert_eq!(spare, Rect::new(8.0, 8.0, 1016.0, 776.0));
    assert_eq!(moved.origin, Point::new(13.0, 11.0));
}

#[test]
fn the_content_area_of_a_tiny_area_is_empty() {
    let content = content_rect(area(10.0, 20.0), &LayoutStyle::default());

    assert_eq!(content.size, Size::default());
}

#[test]
fn a_lone_orchestrator_fills_the_content_area() {
    let sizes = [(1030.0, 790.0), (1037.0, 797.0), (1100.0, 700.0)];

    for (width, height) in sizes {
        let area = Rect::new(4.0, 6.0, width, height);

        let layout = layout(area, &[orchestrator(0)], &LayoutStyle::default());

        assert_rect(&layout, 0, content_rect(area, &LayoutStyle::default()));
    }
}

#[test]
fn font_sized_boxes_round_up_to_whole_cells() {
    // 250 px is 31.25 cells: a 12-row box of 250 px needs 32 cells.
    let style = LayoutStyle {
        small_tile_height: cells_up(250.0),
        process_tile_height: cells_up(201.0),
        ..LayoutStyle::default()
    };
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Working),
        process(2, Activity::Running),
    ];

    let layout = layout(area(1030.0, 790.0), &tiles, &style);

    assert_eq!(cells_up(250.0), 32);
    assert_eq!(cells_up(201.0), 26);
    assert_eq!(cells_up(8.0), 1);
    assert_eq!(cells_up(8.1), 2);
    assert_eq!(cells_up(0.0), 0);
    assert_eq!(cells_up(286.0), LayoutStyle::default().small_tile_height);
    assert_eq!(cells_up(218.0), LayoutStyle::default().process_tile_height);
    assert_rect(&layout, 1, box_at(TOP, 32));
    assert_rect(&layout, 2, box_at(34, 26));
}

#[test]
fn pixels_round_to_the_nearest_whole_cells() {
    assert_eq!(cells_nearest(3.9), 0);
    assert_eq!(cells_nearest(4.0), 1);
    assert_eq!(cells_nearest(11.9), 1);
    assert_eq!(cells_nearest(12.0), 2);
    assert_eq!(cells_nearest(-5.0), 0);
    assert_eq!(Cells::nearest(Size::new(300.0, 163.0)), Cells::new(38, 20));
    assert_eq!(Cells::new(38, 20).to_size(), Size::new(304.0, 160.0));
}

#[test]
fn gap_and_padding_are_one_cell() {
    let style = LayoutStyle::default();
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Working),
    ];

    let layout = default_layout(&tiles);
    let first = layout.rect(TileId(1)).unwrap();
    let second = layout.rect(TileId(2)).unwrap();
    let orchestrator = layout.rect(TileId(0)).unwrap();

    assert_eq!((style.gap, style.padding), (1, 1));
    assert_eq!(orchestrator.origin, Point::new(CELL, CELL));
    assert!((first.origin.x - orchestrator.right() - CELL).abs() < f32::EPSILON);
    assert!((second.origin.y - first.bottom() - CELL).abs() < f32::EPSILON);
}

// Orchestrator width.

#[test]
fn orchestrator_takes_65_percent_of_the_width() {
    // Content widths of 126, 160 and 98 cells.
    for (width, expected) in [(1030.0, 81), (1300.0, 104), (800.0, 63)] {
        let tiles = [orchestrator(0), worker(1, Activity::Working)];

        let layout = layout(area(width, 790.0), &tiles, &LayoutStyle::default());

        assert_rect(&layout, 0, cell_rect(1, 1, expected, FULL_HEIGHT));
    }
}

#[test]
fn the_right_side_keeps_room_for_one_minimum_tile() {
    let tiles = [orchestrator(0), worker(1, Activity::Working)];

    let layout = layout(area(700.0, 790.0), &tiles, &LayoutStyle::default());

    // 65 % of 85 cells is 55, but that leaves less than 33 + 1.
    assert_rect(&layout, 0, cell_rect(1, 1, 51, FULL_HEIGHT));
    assert_rect(&layout, 1, cell_rect(53, 1, 33, SMALL));
}

#[test]
fn orchestrator_is_never_narrower_than_its_minimum() {
    let style = LayoutStyle {
        orchestrator_share: 0.3,
        ..LayoutStyle::default()
    };
    let tiles = [orchestrator(0), worker(1, Activity::Working)];

    let layout = layout(area(1030.0, 790.0), &tiles, &style);

    assert_rect(&layout, 0, cell_rect(1, 1, 40, FULL_HEIGHT));
}

#[test]
fn orchestrator_takes_all_the_width_when_no_tile_fits_beside_it() {
    let tiles = [orchestrator(0), worker(1, Activity::Working)];

    let layout = layout(area(600.0, 790.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 0, cell_rect(1, 1, 73, FULL_HEIGHT));
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

    assert_rect(&layout, 0, cell_rect(1, 1, FULL_WIDTH, FULL_HEIGHT));
    assert_eq!(layout.tray, tray(&[1]));
}

#[test]
fn lone_orchestrator_fills_the_area() {
    let layout = default_layout(&[orchestrator(0)]);

    assert_rect(&layout, 0, cell_rect(1, 1, FULL_WIDTH, FULL_HEIGHT));
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
    let tiles = [orchestrator(0), user(1, Cells::new(81, 59))];

    let layout = default_layout(&tiles);

    // 126 - 81 - 1 cells are left of the user terminal.
    assert_rect(&layout, 0, cell_rect(1, 1, 44, FULL_HEIGHT));
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
        small_tile_height: 25,
        ..LayoutStyle::default()
    };
    let tiles = [orchestrator(0), worker(1, Activity::Working)];

    let layout = layout(area(1030.0, 790.0), &tiles, &style);

    assert_rect(&layout, 1, box_at(TOP, 25));
}

#[test]
fn a_small_box_is_never_shorter_than_the_minimum_tile() {
    let style = LayoutStyle {
        small_tile_height: 12,
        ..LayoutStyle::default()
    };
    let tiles = [orchestrator(0), worker(1, Activity::Working)];

    let layout = layout(area(1030.0, 790.0), &tiles, &style);

    assert_rect(&layout, 1, box_at(TOP, 20));
}

#[test]
fn the_first_box_of_a_column_is_cut_to_a_short_area() {
    let tiles = [worker(0, Activity::Working)];

    let layout = layout(area(600.0, 250.0), &tiles, &LayoutStyle::default());

    // Content of 73 × 29 cells: two lanes of 36.
    assert_rect(&layout, 0, cell_rect(1, 1, 36, 29));
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

    // Content 210 wide: the orchestrator takes 136, two columns of 36 share the 73 left.
    assert_rect(&layout, 0, cell_rect(1, 1, 136, FULL_HEIGHT));
    assert_rect(&layout, 1, cell_rect(138, 1, 36, SMALL));
    assert_rect(&layout, 2, cell_rect(138, 38, 36, SMALL));
    assert_rect(&layout, 3, cell_rect(175, 1, 36, SMALL));
    assert_rect(&layout, 4, cell_rect(175, 38, 36, SMALL));
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

    assert_eq!(tops, [Some(c(1)), Some(c(38)), Some(c(1))]);
    assert_eq!(lefts, [Some(c(138)), Some(c(138)), Some(c(175))]);
}

#[test]
fn a_second_column_needs_a_right_side_of_two_minimum_tiles() {
    let one_column = layout(area(1544.0, 790.0), &wide_tiles(3), &LayoutStyle::default());
    let two_columns = layout(area(1552.0, 790.0), &wide_tiles(3), &LayoutStyle::default());

    // The right side is 66 cells wide, then 67: two lanes of 33 and the gap.
    assert_eq!(one_column.tray, tray(&[3]));
    assert!(two_columns.tray.is_empty());
    assert_rect(&one_column, 1, cell_rect(126, 1, 66, SMALL));
    assert_rect(&two_columns, 1, cell_rect(126, 1, 33, SMALL));
}

#[test]
fn lanes_share_cells_that_do_not_divide_with_the_first_lanes() {
    let workers: Vec<TileSpec> = (0..5).map(|id| worker(id, Activity::Working)).collect();

    let layout = default_layout(&workers);

    // 126 cells less two gaps are 124 cells for three lanes. Each lane holds two boxes.
    let widths: Vec<f32> = [0, 2, 4]
        .into_iter()
        .filter_map(|id| layout.rect(TileId(id)).map(|rect| rect.size.width))
        .collect();
    assert_eq!(widths, [c(42), c(41), c(41)]);
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
    assert_rect(&layout, 2, box_at(38, TALL));
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

    // A small box and a tall one need 110 of the 96 cells, so the box that needs
    // attention stays small.
    assert_rect(&layout, 1, box_at(TOP, SMALL));
    assert_rect(&layout, 2, box_at(38, SMALL));
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

    assert_eq!(before.map(|rect| rect.size.height), Some(c(TALL)));
    assert_eq!(after.map(|rect| rect.size.height), Some(c(SMALL)));
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
    assert_rect(&layout, 2, box_at(38, SMALL));
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
    assert_rect(&layout, 2, box_at(75, TALL));
}

#[test]
fn an_attention_box_gets_its_own_column_when_the_right_side_is_wide() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Attention),
    ];

    let layout = layout(area(1700.0, 790.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, cell_rect(138, 1, 36, SMALL));
    assert_rect(&layout, 2, cell_rect(175, 1, 36, TALL));
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
fn a_process_that_needs_attention_grows_like_a_blocked_worker() {
    let tiles = [orchestrator(0), process(1, Activity::Attention)];

    let layout = default_layout(&tiles);

    assert_rect(&layout, 1, box_at(TOP, TALL));
}

#[test]
fn a_process_that_needs_attention_shrinks_back_when_it_runs_again() {
    let failed = [orchestrator(0), process(1, Activity::Attention)];
    let running = [orchestrator(0), process(1, Activity::Running)];

    let failed = default_layout(&failed);
    let running = default_layout(&running);

    assert_rect(&failed, 1, box_at(TOP, TALL));
    assert_rect(&running, 1, box_at(TOP, PROCESS));
}

#[test]
fn a_running_process_stacks_below_a_tall_attention_box() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Attention),
        process(2, Activity::Running),
    ];

    let layout = layout(area(1030.0, 1000.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, box_at(TOP, TALL));
    assert_rect(&layout, 2, box_at(75, PROCESS));
    assert!(layout.tray.is_empty());
}

#[test]
fn a_running_process_that_does_not_fit_below_a_tall_attention_box_goes_to_the_tray() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Attention),
        process(2, Activity::Running),
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
    assert_rect(&layout, 2, box_at(75, SMALL));
    assert_eq!(layout.tray, tray(&[3]));
}

// Workers that asked for a large tile.

#[test]
fn a_worker_that_asked_for_a_large_tile_is_two_small_boxes_tall() {
    let asked = default_layout(&[orchestrator(0), large_worker(1, Activity::Working)]);
    let small = default_layout(&[orchestrator(0), worker(1, Activity::Working)]);

    assert_rect(&asked, 1, box_at(TOP, TALL));
    assert_rect(&small, 1, box_at(TOP, SMALL));
}

#[test]
fn a_large_worker_grows_like_a_blocked_worker() {
    let asked = default_layout(&[orchestrator(0), large_worker(1, Activity::Idle)]);
    let blocked = default_layout(&[orchestrator(0), worker(1, Activity::Attention)]);

    assert_eq!(asked.rect(TileId(1)), blocked.rect(TileId(1)));
}

#[test]
fn a_blocked_worker_stays_tall_without_the_large_request() {
    let blocked = default_layout(&[orchestrator(0), worker(1, Activity::Attention)]);
    let both = default_layout(&[orchestrator(0), large_worker(1, Activity::Attention)]);

    assert_rect(&blocked, 1, box_at(TOP, TALL));
    assert_eq!(blocked, both);
}

#[test]
fn a_large_worker_keeps_its_place_in_the_stack() {
    let before = [
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Working),
    ];
    let after = [
        orchestrator(0),
        large_worker(1, Activity::Working),
        worker(2, Activity::Working),
    ];

    let before = default_layout(&before)
        .rect(TileId(1))
        .map(|rect| rect.origin);
    let after = default_layout(&after)
        .rect(TileId(1))
        .map(|rect| rect.origin);

    assert_eq!(before, after);
}

#[test]
fn a_large_worker_does_not_push_a_working_worker_to_the_tray() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Working),
        large_worker(2, Activity::Working),
    ];

    let cramped = default_layout(&tiles);
    let tall = layout(area(1030.0, 1000.0), &tiles, &LayoutStyle::default());

    assert_rect(&cramped, 2, box_at(38, SMALL));
    assert!(cramped.tray.is_empty());
    assert_rect(&tall, 2, box_at(38, TALL));
}

#[test]
fn a_large_request_does_not_change_the_priority() {
    let tiles = [
        orchestrator(0),
        large_worker(1, Activity::Idle),
        worker(2, Activity::Working),
    ];

    let layout = layout(area(1030.0, 400.0), &tiles, &LayoutStyle::default());

    assert!(layout.rect(TileId(2)).is_some());
    assert_eq!(layout.tray, tray(&[1]));
}

#[test]
fn the_orchestrator_keeps_its_share_when_a_worker_asks_for_a_large_tile() {
    let layout = default_layout(&[orchestrator(0), large_worker(1, Activity::Working)]);

    assert_rect(&layout, 0, column());
}

#[test]
fn the_orchestrator_ignores_the_large_request() {
    let asked = TileSpec {
        large: true,
        ..orchestrator(0)
    };
    let tiles = [asked, worker(1, Activity::Working)];

    let layout = default_layout(&tiles);

    assert_rect(&layout, 0, column());
    assert_rect(&layout, 1, box_at(TOP, SMALL));
}

#[test]
fn a_process_ignores_the_large_request() {
    let asked = TileSpec {
        large: true,
        ..process(1, Activity::Running)
    };

    let layout = default_layout(&[orchestrator(0), asked]);

    assert_rect(&layout, 1, box_at(TOP, PROCESS));
}

#[test]
fn a_pinned_worker_ignores_the_large_request() {
    let pinned = TileSpec {
        pinned: true,
        ..large_worker(1, Activity::Working)
    };

    let layout = default_layout(&[orchestrator(0), pinned]);

    assert_rect(&layout, 1, box_at(TOP, SMALL));
}

#[test]
fn a_minimized_worker_stays_in_the_tray_whatever_it_asks() {
    let minimized = TileSpec {
        minimized: true,
        ..large_worker(1, Activity::Working)
    };

    let layout = default_layout(&[orchestrator(0), minimized]);

    assert_eq!(layout.tray, tray(&[1]));
    assert_rect(&layout, 0, cell_rect(1, 1, FULL_WIDTH, FULL_HEIGHT));
}

// Workspace mockup: orchestrator, two workers, two processes, a done and an idle worker.

fn workspace_tiles() -> Vec<TileSpec> {
    vec![
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Attention),
        process(3, Activity::Running),
        process(4, Activity::Running),
        worker(5, Activity::Done),
        worker(6, Activity::Idle),
    ]
}

#[test]
fn workspace_mockup_shows_both_workers() {
    let layout = default_layout(&workspace_tiles());

    assert_rect(&layout, 0, column());
    assert_rect(&layout, 1, box_at(TOP, SMALL));
    assert_rect(&layout, 2, box_at(38, SMALL));
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
    assert_rect(&layout, 2, box_at(38, TALL));
    assert_rect(&layout, 3, box_at(112, PROCESS));
    assert_rect(&layout, 4, box_at(141, PROCESS));
    assert_eq!(layout.tray, tray(&[5, 6]));
}

#[test]
fn processes_stack_under_the_workers_when_none_needs_attention() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Working),
        process(3, Activity::Running),
        process(4, Activity::Running),
    ];

    let layout = layout(area(1030.0, 1400.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, box_at(TOP, SMALL));
    assert_rect(&layout, 2, box_at(38, SMALL));
    assert_rect(&layout, 3, box_at(75, PROCESS));
    assert_rect(&layout, 4, box_at(104, PROCESS));
    assert!(layout.tray.is_empty());
}

// Island mockup: an 81 × 59 cell user terminal (648 × 472 px) next to the orchestrator column.

fn island_layout() -> Layout {
    let tiles = vec![
        orchestrator(0),
        user(1, Cells::new(81, 59)),
        worker(2, Activity::Working),
        process(3, Activity::Working),
        worker(4, Activity::Working),
        worker(5, Activity::Done),
        worker(6, Activity::Idle),
    ];

    layout(area(1060.0, 790.0), &tiles, &LayoutStyle::default())
}

#[test]
fn island_mockup_puts_user_terminal_top_right_at_its_fixed_size() {
    assert_rect(&island_layout(), 1, cell_rect(50, 1, 81, 59));
}

#[test]
fn island_mockup_puts_orchestrator_in_the_left_column() {
    assert_rect(&island_layout(), 0, cell_rect(1, 1, 48, FULL_HEIGHT));
}

#[test]
fn island_mockup_puts_other_tiles_below_the_user_terminal() {
    let layout = island_layout();

    assert_rect(&layout, 2, cell_rect(50, 61, 40, SMALL));
    assert_rect(&layout, 3, cell_rect(91, 61, 40, PROCESS));
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
        user(3, Cells::new(81, 110)),
    ];

    let layout = layout(area(1340.0, 898.0), &tiles, &LayoutStyle::default());

    assert!(layout.tray.is_empty());
    assert_rect(&layout, 3, cell_rect(85, 1, 81, 110));
    assert_rect(&layout, 0, cell_rect(1, 1, 49, 110));
    assert_rect(&layout, 1, cell_rect(51, 1, 33, SMALL));
    assert_rect(&layout, 2, cell_rect(51, 38, 33, TALL));
}

#[test]
fn a_user_terminal_with_room_below_it_keeps_the_orchestrator_wide() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Working),
        user(2, Cells::new(37, 37)),
    ];

    let layout = layout(area(1340.0, 898.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 0, cell_rect(1, 1, 107, 110));
    assert!(layout.tray.is_empty());
}

#[test]
fn tiles_flow_beside_a_user_terminal_when_space_allows() {
    let tiles = [
        user(0, Cells::new(75, 50)),
        worker(1, Activity::Working),
        worker(2, Activity::Working),
    ];

    let layout = layout(area(1220.0, 800.0), &tiles, &LayoutStyle::default());

    // 74 cells beside the user terminal: two lanes of 37 and 36.
    assert_rect(&layout, 1, cell_rect(1, 1, 37, SMALL));
    assert_rect(&layout, 2, cell_rect(39, 1, 36, SMALL));
}

#[test]
fn second_user_terminal_goes_left_of_the_first() {
    let tiles = [user(0, Cells::new(50, 37)), user(1, Cells::new(37, 25))];

    let layout = layout(area(1220.0, 800.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, cell_rect(63, 1, 37, 25));
}

#[test]
fn user_terminal_wraps_below_when_the_top_row_is_full() {
    let tiles = [user(0, Cells::new(88, 37)), user(1, Cells::new(75, 25))];

    let layout = layout(area(1220.0, 800.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, cell_rect(76, 39, 75, 25));
}

#[test]
fn user_terminal_larger_than_the_area_goes_to_the_tray() {
    let tiles = [user(0, Cells::new(163, 37))];

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
        user(1, Cells::new(75, 60)),
        worker(2, Activity::Attention),
        worker(3, Activity::Working),
        process(4, Activity::Running),
    ];

    let layout = layout(area(1000.0, 500.0), &tiles, &LayoutStyle::default());

    assert_eq!(layout.tray, tray(&[2, 3, 4]));
}

#[test]
fn tiny_area_gives_the_orchestrator_all_width_left_of_the_user_terminal() {
    let tiles = [
        orchestrator(0),
        user(1, Cells::new(75, 60)),
        worker(2, Activity::Working),
    ];

    let layout = layout(area(1000.0, 500.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 0, cell_rect(1, 1, 47, 60));
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
        ..user(0, Cells::new(37, 25))
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
    let tiles = [process(0, Activity::Running), worker(1, Activity::Working)];

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
        process(4, Activity::Running),
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

    // Three lanes share 124 cells: 42, 41 and 41.
    assert_rect(&before, 0, cell_rect(1, 1, 42, SMALL));
    assert_eq!(before.rect(TileId(0)), after.rect(TileId(0)));
}

#[test]
fn input_order_is_final_a_process_above_a_worker_stays_above() {
    let tiles = [
        process(0, Activity::Attention),
        worker(1, Activity::Idle),
        worker(2, Activity::Working),
    ];

    let layout = layout(area(350.0, 790.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 0, cell_rect(1, 1, 41, PROCESS));
    assert_rect(&layout, 2, cell_rect(1, 30, 41, SMALL));
    assert_eq!(layout.tray, tray(&[1]));
}

// Process terminals.

#[test]
fn a_process_is_the_smallest_box() {
    let tiles = [orchestrator(0), process(1, Activity::Running)];

    let layout = default_layout(&tiles);

    assert_rect(&layout, 1, box_at(TOP, PROCESS));
}

#[test]
fn a_process_shorter_than_the_minimum_gets_the_minimum_height() {
    let style = LayoutStyle {
        process_tile_height: 12,
        ..LayoutStyle::default()
    };
    let tiles = [orchestrator(0), process(1, Activity::Running)];

    let layout = layout(area(1030.0, 790.0), &tiles, &style);

    assert_rect(&layout, 1, box_at(TOP, 20));
}

#[test]
fn a_process_taller_than_the_lane_is_cut_to_it_when_it_is_first_in_its_column() {
    let tiles = [process(0, Activity::Running)];

    let layout = layout(area(300.0, 220.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 0, cell_rect(1, 1, 35, 25));
}

#[test]
fn a_process_stacks_below_the_workers() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Working),
        process(2, Activity::Running),
    ];

    let layout = default_layout(&tiles);

    assert_rect(&layout, 1, box_at(TOP, SMALL));
    assert_rect(&layout, 2, box_at(38, PROCESS));
}

#[test]
fn a_process_never_takes_width_from_the_orchestrator() {
    let without = [orchestrator(0), worker(1, Activity::Working)];
    let with = [
        orchestrator(0),
        worker(1, Activity::Working),
        process(2, Activity::Running),
    ];

    let before = default_layout(&without).rect(TileId(0));
    let after = default_layout(&with).rect(TileId(0));

    assert_eq!(before, after);
    assert_rect(&default_layout(&with), 0, column());
}

#[test]
fn a_process_with_a_fixed_size_is_still_a_small_box_and_not_an_island() {
    let tiles = [
        orchestrator(0),
        TileSpec {
            fixed: Some(Cells::new(87, 50)),
            ..process(1, Activity::Running)
        },
    ];

    let layout = default_layout(&tiles);

    assert_rect(&layout, 0, column());
    assert_rect(&layout, 1, box_at(TOP, PROCESS));
}

#[test]
fn a_process_that_does_not_fit_below_two_workers_goes_to_the_tray() {
    let tiles = [
        orchestrator(0),
        worker(1, Activity::Working),
        worker(2, Activity::Working),
        process(3, Activity::Running),
    ];

    let layout = default_layout(&tiles);

    assert_eq!(layout.tray, tray(&[3]));
}

#[test]
fn a_process_beside_a_user_terminal_island_flows_below_it_like_a_worker() {
    let tiles = [
        orchestrator(0),
        user(1, Cells::new(81, 59)),
        process(2, Activity::Running),
    ];

    let layout = layout(area(1060.0, 790.0), &tiles, &LayoutStyle::default());

    assert_rect(&layout, 1, cell_rect(50, 1, 81, 59));
    assert_rect(&layout, 2, cell_rect(50, 61, 40, PROCESS));
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

    fn between(&mut self, low: u16, high: u16) -> u16 {
        low + self.below(high - low + 1)
    }

    fn pixels(&mut self, low: u16, high: u16) -> f32 {
        f32::from(self.between(low, high))
    }

    fn cells(&mut self, low: u16, high: u16) -> Cells {
        Cells::new(self.between(low, high), self.between(low, high))
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
        content_rect(self.area, &self.style)
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
    let fixed = (kind == TileKind::User && random.chance(70)).then(|| {
        let width = random.between(10, 150);
        Cells::new(width, random.between(8, 112))
    });

    TileSpec {
        id: TileId(id),
        kind,
        activity,
        fixed,
        pinned: random.chance(15),
        minimized: random.chance(10),
        large: random.chance(20),
    }
}

fn random_style(random: &mut Random) -> LayoutStyle {
    if random.chance(50) {
        return LayoutStyle::default();
    }

    let small_tile_height = random.between(5, 63);

    LayoutStyle {
        gap: random.below(4),
        padding: random.below(4),
        orchestrator_share: f32::from(random.below(101)) / 100.0,
        min_tile: random.cells(8, 50),
        min_orchestrator: random.cells(12, 63),
        small_tile_height,
        process_tile_height: random.between(5, small_tile_height),
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

/// A case like the app makes: the default style, a font-sized box height and an area of
/// at least [`APP_MIN_AREA`].
fn app_case(seed: u64) -> Case {
    let mut random = Random(seed);
    let (min_width, min_height) = APP_MIN_AREA;
    let area = Rect::new(
        0.0,
        0.0,
        random.pixels(min_width, 2400),
        random.pixels(min_height, 1500),
    );
    let small_tile_height = random.pixels(200, 400);
    let style = LayoutStyle {
        small_tile_height: cells_up(small_tile_height),
        process_tile_height: cells_up((small_tile_height * 0.75).round()),
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

/// An unpinned worker with the large request.
fn asks_to_grow(tile: &TileSpec) -> bool {
    tile.large && tile.kind == TileKind::Worker && !tile.pinned
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

/// The least a style lets a tile be, in pixels.
fn at_least_one_cell(cells: Cells) -> Size {
    Cells::new(cells.width.max(1), cells.height.max(1)).to_size()
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
fn every_placed_rect_starts_and_ends_on_a_cell_edge() {
    for case in cases() {
        let content = case.content();

        for (id, rect) in case.layout().placed {
            let edges = [
                rect.origin.x - content.origin.x,
                rect.origin.y - content.origin.y,
                rect.size.width,
                rect.size.height,
            ];

            assert!(
                edges.iter().all(|edge| edge % CELL == 0.0),
                "{id:?} at {rect:?} is off the grid of {content:?}"
            );
        }
    }
}

#[test]
fn the_content_area_is_never_larger_than_the_area_less_the_padding() {
    for case in cases() {
        let content = case.content();
        let padding = f32::from(case.style.padding) * CELL;

        assert!(content.size.width <= (case.area.size.width - 2.0 * padding).max(0.0));
        assert!(content.size.height <= (case.area.size.height - 2.0 * padding).max(0.0));
        assert!(content.size.width % CELL == 0.0 && content.size.height % CELL == 0.0);
    }
}

#[test]
fn placed_tiles_lie_inside_the_padded_area() {
    for case in cases() {
        let content = case.content();

        for (id, rect) in case.layout().placed {
            assert!(
                rect.origin.x >= content.origin.x
                    && rect.origin.y >= content.origin.y
                    && rect.right() <= content.right()
                    && rect.bottom() <= content.bottom(),
                "{id:?} at {rect:?} is outside {content:?}"
            );
        }
    }
}

#[test]
fn placed_tiles_never_overlap_and_keep_at_least_the_gap() {
    for case in cases() {
        let placed = case.layout().placed;
        let gap = f32::from(case.style.gap) * CELL;

        for (position, (first_id, first)) in placed.iter().enumerate() {
            for (second_id, second) in &placed[position + 1..] {
                assert!(
                    separation(*first, *second) >= gap,
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
                assert_eq!(rect.size, fixed.to_size());
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
            let min = at_least_one_cell(min);

            assert!(
                rect.size.width >= min.width && rect.size.height >= min.height,
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
            }
            .to_size();

            assert!(
                rect.size.width >= min.width && rect.size.height >= min.height,
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
        assert_eq!(
            Rect::new(
                content.origin.x,
                content.origin.y,
                rect.size.width,
                content.size.height
            ),
            rect
        );
    }
}

#[test]
fn boxes_without_a_size_of_their_own_are_never_taller_than_a_small_box() {
    for case in cases() {
        let layout = case.layout();
        let column = case.column_orchestrator();
        let small = c(case
            .style
            .small_tile_height
            .max(case.style.min_tile.height.max(1)));

        for tile in case.tiles.iter().filter(|tile| {
            tile.fixed.is_none()
                && tile.activity != Activity::Attention
                && !asks_to_grow(tile)
                && Some(tile.id) != column
        }) {
            let Some(rect) = layout.rect(tile.id) else {
                continue;
            };

            assert!(
                rect.size.height <= small,
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

        let share = (case.content().size.width / CELL
            * case.style.orchestrator_share.clamp(0.0, 1.0))
        .floor()
            * CELL;
        assert!(
            rect.size.width <= share.max(at_least_one_cell(case.style.min_orchestrator).width),
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
        let gap = c(style.gap);
        let min_tile = at_least_one_cell(style.min_tile);
        let left = content.size.width - at_least_one_cell(style.min_orchestrator).width - gap;
        let lane_below =
            left >= min_tile.width && content.size.height - block.height - gap >= min_tile.height;
        let lane_beside =
            left - block.width - gap >= min_tile.width && block.height >= min_tile.height;
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

#[test]
fn a_process_is_never_taller_than_its_box_unless_it_needs_attention() {
    for case in cases() {
        let layout = case.layout();
        let wanted = c(case
            .style
            .process_tile_height
            .max(case.style.min_tile.height.max(1)));

        for tile in case
            .tiles
            .iter()
            .filter(|tile| tile.kind == TileKind::Process && tile.activity != Activity::Attention)
        {
            let Some(rect) = layout.rect(tile.id) else {
                continue;
            };

            assert!(
                rect.size.height <= wanted,
                "{:?} is {:?} high, a process box is {wanted}",
                tile.id,
                rect.size.height
            );
        }
    }
}

#[test]
fn a_process_ignores_a_fixed_size() {
    for case in cases() {
        let fixed = Some(Cells::new(87, 50));
        let sized = Case {
            tiles: case
                .tiles
                .iter()
                .map(|tile| TileSpec {
                    fixed: fixed
                        .filter(|_| tile.kind == TileKind::Process)
                        .or(tile.fixed),
                    ..*tile
                })
                .collect(),
            area: case.area,
            style: case.style,
        };

        assert_eq!(sized.layout(), case.layout());
    }
}

#[test]
fn the_large_request_only_counts_for_unpinned_workers() {
    for case in cases() {
        let stripped = Case {
            tiles: case
                .tiles
                .iter()
                .map(|tile| TileSpec {
                    large: asks_to_grow(tile),
                    ..*tile
                })
                .collect(),
            area: case.area,
            style: case.style,
        };

        assert_eq!(stripped.layout(), case.layout());
    }
}

#[test]
fn a_worker_that_asks_to_grow_is_never_taller_than_two_small_boxes() {
    for case in cases() {
        let layout = case.layout();
        let tall = c(2 * case
            .style
            .small_tile_height
            .max(case.style.min_tile.height.max(1))
            + case.style.gap);

        for tile in case.tiles.iter().filter(|tile| asks_to_grow(tile)) {
            let Some(rect) = layout.rect(tile.id) else {
                continue;
            };

            assert!(
                rect.size.height <= tall,
                "{:?} is {:?} high, a tall box is {tall}",
                tile.id,
                rect.size.height
            );
        }
    }
}
