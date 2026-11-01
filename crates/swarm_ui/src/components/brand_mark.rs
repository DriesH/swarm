use gpui::{AnyElement, App, Pixels, Window, div, px};

use crate::component::{Component, example, example_group};
use crate::prelude::*;

/// The Swarm logo: the line bee on a filled tile.
#[derive(IntoElement)]
#[must_use]
pub struct BrandMark {
    size: Pixels,
}

impl BrandMark {
    pub fn new() -> Self {
        Self { size: px(20.) }
    }

    pub fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }
}

impl Default for BrandMark {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for BrandMark {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let fg = cx.theme().colors().fg;

        div()
            .size(self.size)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(self.size * 0.3)
            .bg(fg)
            .child(
                Icon::new(IconName::Bee)
                    .size(IconSize::Custom(self.size * 0.65))
                    .color(Color::Inverse),
            )
    }
}

impl Component for BrandMark {
    fn name() -> &'static str {
        "BrandMark"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        v_flex()
            .gap_6()
            .child(example_group(
                "Sizes",
                [
                    example("20px", BrandMark::new()),
                    example("28px", BrandMark::new().size(px(28.))),
                    example("40px", BrandMark::new().size(px(40.))),
                ],
            ))
            .into_any_element()
    }
}
