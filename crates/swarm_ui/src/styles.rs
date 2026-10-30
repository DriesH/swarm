use gpui::{App, BoxShadow, Div, Hsla, Pixels, StyleRefinement, Styled as _, div, point, px};

use crate::theme::ActiveTheme as _;

/// Text and icon colors, mapped to theme tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Color {
    #[default]
    Default,
    Muted,
    Faint,
    Accent,
    Ok,
    Warn,
    Add,
    Del,
    /// Text on a `fg` fill, such as a primary button.
    Inverse,
    /// The Claude Code brand mark.
    Claude,
    /// Any theme token, for example a status color.
    Custom(Hsla),
}

impl Color {
    pub fn hsla(self, cx: &App) -> Hsla {
        let colors = cx.theme().colors();

        match self {
            Self::Default => colors.fg,
            Self::Muted => colors.muted,
            Self::Faint => colors.faint,
            Self::Accent => colors.accent,
            Self::Ok => colors.ok,
            Self::Warn => colors.warn,
            Self::Add => colors.add,
            Self::Del => colors.del,
            Self::Inverse => colors.ground,
            Self::Claude => colors.claude,
            Self::Custom(color) => color,
        }
    }
}

/// Width of the focus and attention rings.
pub const RING_WIDTH: Pixels = px(2.);

/// A 2px ring inside the element's edge, like the mockup's `inset 0 0 0 2px`.
/// Inset because GPUI fills an outer shadow behind the element, which shows as
/// a solid block on transparent controls. Pass the theme's `accent`.
pub fn focus_ring(style: StyleRefinement, color: Hsla) -> StyleRefinement {
    style.shadow(vec![BoxShadow {
        color,
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: RING_WIDTH,
        inset: true,
    }])
}

/// The focus ring for controls with a solid fill: the 2px accent ring plus a 1px
/// `ground` gap inside it, so the ring keeps 3:1 contrast against the fill.
/// Pass the theme's `accent` and `ground`.
pub fn filled_focus_ring(style: StyleRefinement, color: Hsla, gap: Hsla) -> StyleRefinement {
    let ring = |color: Hsla, spread_radius: Pixels| BoxShadow {
        color,
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius,
        inset: true,
    };

    style.shadow(vec![
        ring(gap, RING_WIDTH + px(1.)),
        ring(color, RING_WIDTH),
    ])
}

/// A row that centers its children vertically.
pub fn h_flex() -> Div {
    div().flex().flex_row().items_center()
}

/// A column.
pub fn v_flex() -> Div {
    div().flex().flex_col()
}
