//! WCAG 2.2 contrast math.

use gpui::{Hsla, Rgba};

/// Minimum ratio for normal text (WCAG 1.4.3, level AA).
pub const TEXT_AA: f32 = 4.5;
/// Minimum ratio for large text and for icons and other UI graphics (WCAG 1.4.3 and 1.4.11).
pub const GRAPHIC_AA: f32 = 3.0;

/// Paints `foreground` over `background` and returns the opaque result.
/// Use it for tinted backgrounds such as `accent_bg`, which are translucent.
pub fn composite(background: Hsla, foreground: Hsla) -> Hsla {
    Rgba::from(background).blend(Rgba::from(foreground)).into()
}

/// Relative luminance of an opaque color.
pub fn relative_luminance(color: Hsla) -> f32 {
    let rgba = Rgba::from(color);
    let linear = |channel: f32| {
        if channel <= 0.040_45 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };

    0.2126 * linear(rgba.r) + 0.7152 * linear(rgba.g) + 0.0722 * linear(rgba.b)
}

/// Contrast ratio between two opaque colors, from 1.0 to 21.0.
pub fn contrast_ratio(first: Hsla, second: Hsla) -> f32 {
    let first = relative_luminance(first);
    let second = relative_luminance(second);

    let (lighter, darker) = if first > second {
        (first, second)
    } else {
        (second, first)
    };

    (lighter + 0.05) / (darker + 0.05)
}

#[cfg(test)]
mod tests {
    use gpui::rgb;

    use super::*;

    #[test]
    fn black_on_white_is_21() {
        let ratio = contrast_ratio(gpui::black(), gpui::white());
        assert!((ratio - 21.0).abs() < 0.01, "{ratio}");
    }

    #[test]
    fn same_color_is_1() {
        let ratio = contrast_ratio(rgb(0x77_77_77).into(), rgb(0x77_77_77).into());
        assert!((ratio - 1.0).abs() < 0.001, "{ratio}");
    }

    #[test]
    fn known_pair_matches_webaim() {
        // WebAIM contrast checker: #767676 on #ffffff is 4.54:1.
        let ratio = contrast_ratio(rgb(0x76_76_76).into(), gpui::white());
        assert!((ratio - 4.54).abs() < 0.01, "{ratio}");
    }
}
