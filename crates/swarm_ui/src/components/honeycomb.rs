use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, AppContext as _, Bounds, ContentMask, Context,
    ElementId, Hsla, PathBuilder, Pixels, Point, StyleRefinement, Window, canvas, div,
    ease_out_quint, point, px,
};

use crate::component::{Component, example, example_group};
use crate::motion::reduce_motion;
use crate::prelude::*;

/// One tile of the pattern: a hexagon with a stem below it.
const TILE_WIDTH: f32 = 42.;
const TILE_HEIGHT: f32 = 72.75;
const STROKE_WIDTH: f32 = 1.;

/// The corners of the hexagon, from the top and clockwise.
const HEXAGON: [(f32, f32); 6] = [
    (21., 0.),
    (42., 12.125),
    (42., 36.375),
    (21., 48.5),
    (0., 36.375),
    (0., 12.125),
];

/// The faint hex pattern behind the welcome screen and empty states. It fills its parent
/// and draws only lines in the `hex` token, so text on top keeps its contrast.
///
/// It is a view, so a parent can cache it: only a resize or a theme change paints it again.
pub struct Honeycomb;

impl Honeycomb {
    pub fn new() -> Self {
        Self
    }

    /// Style for [`gpui::AnyView::cached`]: the view fills its parent.
    pub fn cache_style() -> StyleRefinement {
        StyleRefinement::default().size_full()
    }
}

impl Default for Honeycomb {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for Honeycomb {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let color = cx.theme().colors().hex;

        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| paint_pattern(bounds, color, window),
        )
        .size_full()
    }
}

/// One path per row of tiles: a path holds at most 65 535 vertices.
fn paint_pattern(bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let columns = tiles_across(f32::from(bounds.size.width), TILE_WIDTH);
    let rows = tiles_across(f32::from(bounds.size.height), TILE_HEIGHT);

    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        for row in 0..rows {
            let mut builder = PathBuilder::stroke(px(STROKE_WIDTH));

            for column in 0..columns {
                let origin = bounds.origin
                    + point(
                        px(tile_offset(column, TILE_WIDTH)),
                        px(tile_offset(row, TILE_HEIGHT)),
                    );
                let at = |(x, y): (f32, f32)| origin + point(px(x), px(y));

                builder.move_to(at(HEXAGON[0]));
                for corner in &HEXAGON[1..] {
                    builder.line_to(at(*corner));
                }
                builder.close();

                builder.move_to(at((21., 48.5)));
                builder.line_to(at((21., TILE_HEIGHT)));
            }

            match builder.build() {
                Ok(path) => window.paint_path(path, color),
                Err(error) => log::debug!("cannot paint the honeycomb: {error}"),
            }
        }
    });
}

/// How many tiles of `tile` pixels it takes to cover `length` pixels.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a count of tiles on screen is small and not negative"
)]
fn tiles_across(length: f32, tile: f32) -> usize {
    (length.max(0.) / tile).ceil() as usize
}

#[expect(
    clippy::cast_precision_loss,
    reason = "a tile index on screen is small"
)]
fn tile_offset(index: usize, tile: f32) -> f32 {
    index as f32 * tile
}

impl Component for Honeycomb {
    fn name() -> &'static str {
        "Honeycomb"
    }

    fn preview(_window: &mut Window, cx: &mut App) -> AnyElement {
        let honeycomb = cx.new(|_| Honeycomb::new());

        v_flex()
            .gap_6()
            .child(example_group(
                "Background",
                [example(
                    "Fills its parent",
                    div()
                        .relative()
                        .w(px(420.))
                        .h(px(240.))
                        .rounded(px(10.))
                        .overflow_hidden()
                        .bg(cx.theme().colors().ground)
                        .child(
                            div()
                                .absolute()
                                .size_full()
                                .child(honeycomb.into_any_element()),
                        ),
                )],
            ))
            .into_any_element()
    }
}

/// Seven cells fill one after the other. The tiny honeycomb of the "Honey's ready" toast.
///
/// It fills once when it appears. It is decoration: no role, no label. Under Reduce Motion
/// every cell is filled from the first frame.
#[derive(IntoElement)]
#[must_use]
pub struct HoneyFill {
    id: ElementId,
    progress: f32,
}

/// How long the cells take to fill, the last one included.
pub const HONEY_FILL_DURATION: Duration = Duration::from_millis(1600);

const FILL_WIDTH: f32 = 58.;
const FILL_HEIGHT: f32 = 52.;
/// The drawing is 60 by 54 units; it is shown a little smaller.
const FILL_SCALE: f32 = FILL_WIDTH / 60.;
/// The radius of a cell, and the half gap between neighbors.
const CELL_RADIUS: f32 = 8.6;
const CELL_GAP: f32 = 0.6;
/// The seconds before the first cell starts, between two cells, and each cell takes.
const FIRST_DELAY: f32 = 0.15;
const CELL_STEP: f32 = 0.16;
const CELL_SECONDS: f32 = 0.42;

/// The order the cells fill in: the ring from the lower left, then the middle.
const FILL_ORDER: [usize; 7] = [1, 2, 3, 4, 5, 6, 0];

impl HoneyFill {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            progress: 1.,
        }
    }

    pub fn size() -> gpui::Size<Pixels> {
        gpui::size(px(FILL_WIDTH), px(FILL_HEIGHT))
    }

    /// How full the honeycomb is at `progress` of [`HONEY_FILL_DURATION`]: 0 to 7 cells,
    /// with a part of the cell that is filling.
    pub fn cells_filled(progress: f32) -> f32 {
        (0..FILL_ORDER.len())
            .map(|step| cell_progress(step, progress))
            .sum()
    }

    /// True when the cells fill one by one. Under Reduce Motion they are all full at once.
    pub fn animates(cx: &App) -> bool {
        !reduce_motion(cx)
    }
}

/// How far cell number `step` of the fill order is, from 0 to 1.
fn cell_progress(step: usize, progress: f32) -> f32 {
    let elapsed = progress.clamp(0., 1.) * HONEY_FILL_DURATION.as_secs_f32();
    let delay = FIRST_DELAY + f32::from(u8::try_from(step).unwrap_or(0)) * CELL_STEP;

    ((elapsed - delay) / CELL_SECONDS).clamp(0., 1.)
}

/// The center of each cell, relative to the middle of the honeycomb.
fn cell_centers() -> [(f32, f32); 7] {
    let width = 3f32.sqrt() * CELL_RADIUS;
    let row = 1.5 * CELL_RADIUS;

    [
        (0., 0.),
        (-width / 2., row),
        (width / 2., row),
        (width, 0.),
        (width / 2., -row),
        (-width / 2., -row),
        (-width, 0.),
    ]
}

fn hexagon(center: (f32, f32), radius: f32, middle: Point<Pixels>) -> Vec<Point<Pixels>> {
    (0..6u8)
        .map(|corner| {
            let angle = (60. * f32::from(corner) - 90.).to_radians();

            point(
                middle.x + px((center.0 + radius * angle.cos()) * FILL_SCALE),
                middle.y + px((center.1 + radius * angle.sin()) * FILL_SCALE),
            )
        })
        .collect()
}

fn polygon(corners: &[Point<Pixels>], mut builder: PathBuilder, color: Hsla, window: &mut Window) {
    let Some((first, rest)) = corners.split_first() else {
        return;
    };

    builder.move_to(*first);
    for corner in rest {
        builder.line_to(*corner);
    }
    builder.close();

    match builder.build() {
        Ok(path) => window.paint_path(path, color),
        Err(error) => log::debug!("cannot paint a honey cell: {error}"),
    }
}

impl RenderOnce for HoneyFill {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let cells = HoneyCells {
            progress: self.progress,
            colors: HoneyColors::of(cx),
        };

        if Self::animates(cx) {
            cells
                .with_animation(
                    self.id,
                    Animation::new(HONEY_FILL_DURATION),
                    |cells, delta| HoneyCells {
                        progress: delta,
                        ..cells
                    },
                )
                .into_any_element()
        } else {
            cells.into_any_element()
        }
    }
}

#[derive(Clone, Copy)]
struct HoneyColors {
    honey: Hsla,
    line: Hsla,
    empty: Hsla,
}

impl HoneyColors {
    fn of(cx: &App) -> Self {
        let colors = cx.theme().colors();

        Self {
            honey: colors.honey,
            line: colors.honey_line,
            empty: colors.honey_empty,
        }
    }
}

/// The cells at one moment of the fill.
#[derive(IntoElement)]
struct HoneyCells {
    progress: f32,
    colors: HoneyColors,
}

impl RenderOnce for HoneyCells {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let Self { progress, colors } = self;

        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                let middle = bounds.center();
                let centers = cell_centers();

                for center in centers {
                    let corners = hexagon(center, CELL_RADIUS - CELL_GAP, middle);

                    polygon(&corners, PathBuilder::fill(), colors.empty, window);
                    polygon(&corners, PathBuilder::stroke(px(1.)), colors.line, window);
                }

                for (step, index) in FILL_ORDER.into_iter().enumerate() {
                    let filled = cell_progress(step, progress);
                    if filled <= 0. {
                        continue;
                    }

                    let radius = (CELL_RADIUS - CELL_GAP) * (0.4 + 0.6 * ease_out_quint()(filled));
                    let corners = hexagon(centers[index], radius, middle);

                    polygon(
                        &corners,
                        PathBuilder::fill(),
                        colors.honey.opacity(filled),
                        window,
                    );
                    polygon(
                        &corners,
                        PathBuilder::stroke(px(1.)),
                        colors.line.opacity(filled),
                        window,
                    );
                }
            },
        )
        .w(px(FILL_WIDTH))
        .h(px(FILL_HEIGHT))
        .flex_none()
    }
}

impl Component for HoneyFill {
    fn name() -> &'static str {
        "HoneyFill"
    }

    fn preview(_window: &mut Window, cx: &mut App) -> AnyElement {
        let filled = HoneyCells {
            progress: 1.,
            colors: HoneyColors::of(cx),
        };

        v_flex()
            .gap_6()
            .child(example_group(
                "States",
                [
                    example("Filling", HoneyFill::new("honey-filling")),
                    example("Filled", filled),
                ],
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    struct Page {
        honeycomb: gpui::Entity<Honeycomb>,
    }

    impl Render for Page {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size(px(300.))
                .child(self.honeycomb.clone().cached(Honeycomb::cache_style()))
        }
    }

    #[gpui::test]
    fn the_pattern_draws_in_a_cached_view_of_any_size(cx: &mut TestAppContext) {
        cx.update(crate::theme::init);
        let (_, cx) = cx.add_window_view(|_, cx| Page {
            honeycomb: cx.new(|_| Honeycomb::new()),
        });

        cx.run_until_parked();
        cx.simulate_resize(gpui::size(px(3000.), px(2000.)));
        cx.run_until_parked();
    }

    #[test]
    fn the_cells_fill_one_after_the_other() {
        assert!(HoneyFill::cells_filled(0.).abs() < f32::EPSILON);
        assert!((HoneyFill::cells_filled(1.) - 7.).abs() < f32::EPSILON);

        let half = HoneyFill::cells_filled(0.5);
        assert!(half > 1. && half < 6., "{half}");
        assert!(cell_progress(0, 0.3) > cell_progress(1, 0.3));
        assert!(cell_progress(5, 0.3) <= cell_progress(2, 0.3));
    }

    #[gpui::test]
    fn the_cells_fill_only_while_motion_is_allowed(cx: &mut TestAppContext) {
        cx.update(crate::theme::init);

        cx.update(|cx| {
            assert!(HoneyFill::animates(cx));

            cx.set_reduce_motion(true);
            assert!(!HoneyFill::animates(cx));
        });
    }

    #[test]
    fn it_takes_enough_tiles_to_cover_the_length() {
        assert_eq!(tiles_across(0., TILE_WIDTH), 0);
        assert_eq!(tiles_across(42., TILE_WIDTH), 1);
        assert_eq!(tiles_across(43., TILE_WIDTH), 2);
        assert_eq!(tiles_across(-5., TILE_WIDTH), 0);
    }
}
