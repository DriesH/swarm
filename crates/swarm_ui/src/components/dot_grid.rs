use gpui::{AnyElement, App, Bounds, Window, canvas, div, fill, point, px, size};

use crate::component::{Component, example, example_group};
use crate::prelude::*;

/// Faint dots at every corner of a grid of square cells. It fills its parent and draws one
/// 1 px dot per cell corner inside it, in the `dot` token. `cell` is the cell size in
/// logical pixels.
///
/// It is decoration: no role, no label, not focusable, and pointer events pass through.
#[derive(IntoElement)]
#[must_use]
pub struct DotGrid {
    cell: f32,
}

impl DotGrid {
    pub fn new(cell: f32) -> Self {
        Self { cell }
    }
}

impl RenderOnce for DotGrid {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = cx.theme().colors().dot;
        let cell = self.cell;

        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                if cell < 1. {
                    return;
                }

                let dot = size(px(1.), px(1.));
                let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));

                let mut top = 0.;
                while top < height {
                    let mut left = 0.;
                    while left < width {
                        let origin = point(bounds.origin.x + px(left), bounds.origin.y + px(top));
                        window.paint_quad(fill(Bounds::new(origin, dot), color));
                        left += cell;
                    }
                    top += cell;
                }
            },
        )
        .size_full()
    }
}

impl Component for DotGrid {
    fn name() -> &'static str {
        "DotGrid"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        v_flex()
            .gap_6()
            .child(example_group(
                "Cell sizes",
                [
                    example(
                        "8 px",
                        div()
                            .relative()
                            .w(px(160.))
                            .h(px(80.))
                            .child(DotGrid::new(8.)),
                    ),
                    example(
                        "16 px",
                        div()
                            .relative()
                            .w(px(160.))
                            .h(px(80.))
                            .child(DotGrid::new(16.)),
                    ),
                ],
            ))
            .into_any_element()
    }
}
