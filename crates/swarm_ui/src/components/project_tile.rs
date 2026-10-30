use gpui::{AnyElement, App, FontWeight, Hsla, Pixels, Window, div, px, rgb};

use crate::component::{Component, example, example_group};
use crate::prelude::*;

/// A project's colored letter tile.
#[derive(IntoElement)]
#[must_use]
pub struct ProjectTile {
    letter: char,
    color: Hsla,
    size: Pixels,
}

impl ProjectTile {
    pub fn new(letter: char, color: Hsla) -> Self {
        Self {
            letter,
            color,
            size: px(18.),
        }
    }

    pub fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for ProjectTile {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let on_tile = cx.theme().colors().on_tile;

        div()
            .size(self.size)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(self.size * 0.28)
            .bg(self.color)
            .text_size(self.size * 0.6)
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(on_tile)
            .line_height(self.size)
            .child(self.letter.to_string())
    }
}

impl Component for ProjectTile {
    fn name() -> &'static str {
        "ProjectTile"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        let indigo: Hsla = rgb(0x004f_46e5).into();
        let green: Hsla = rgb(0x0015_803d).into();

        v_flex()
            .gap_6()
            .child(example_group(
                "Sizes",
                [
                    example("18", ProjectTile::new('S', indigo)),
                    example("28", ProjectTile::new('P', green).size(px(28.))),
                ],
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui::rgb;

    use crate::contrast::{TEXT_AA, contrast_ratio};
    use crate::theme::Theme;

    /// The tile colors in `swarm_app`'s model.
    const TILE_COLORS: [u32; 4] = [0x004f_46e5, 0x000d_7d70, 0x00c2_410c, 0x0015_803d];

    #[test]
    fn on_tile_letter_meets_text_contrast_on_every_tile_color() {
        for theme in [Theme::light(), Theme::dark()] {
            for tile in TILE_COLORS {
                let ratio = contrast_ratio(theme.colors().on_tile, rgb(tile).into());

                assert!(
                    ratio >= TEXT_AA,
                    "{} on #{tile:06x} is {ratio:.2}",
                    theme.name()
                );
            }
        }
    }
}
