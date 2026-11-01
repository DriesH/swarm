use gpui::{
    AnyElement, App, AppContext as _, Bounds, ContentMask, Context, Hsla, Pixels, StyleRefinement,
    Window, canvas, div, point, px,
};

use crate::component::{Component, example, example_group};
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
            let mut builder = gpui::PathBuilder::stroke(px(STROKE_WIDTH));

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
    fn it_takes_enough_tiles_to_cover_the_length() {
        assert_eq!(tiles_across(0., TILE_WIDTH), 0);
        assert_eq!(tiles_across(42., TILE_WIDTH), 1);
        assert_eq!(tiles_across(43., TILE_WIDTH), 2);
        assert_eq!(tiles_across(-5., TILE_WIDTH), 0);
    }
}
