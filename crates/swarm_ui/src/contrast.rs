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

/// How far apart two opaque colors look: the CIE76 color difference in CIELAB (D65).
///
/// About 2.3 is the smallest difference an eye notices; above roughly 10 two
/// colors read as different at a glance; 100 is the span from black to white.
pub fn color_distance(first: Hsla, second: Hsla) -> f32 {
    let (first, second) = (lab(first), lab(second));

    ((first[0] - second[0]).powi(2)
        + (first[1] - second[1]).powi(2)
        + (first[2] - second[2]).powi(2))
    .sqrt()
}

fn lab(color: Hsla) -> [f32; 3] {
    let rgba = Rgba::from(color);
    let linear = |channel: f32| {
        if channel <= 0.040_45 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    let (red, green, blue) = (linear(rgba.r), linear(rgba.g), linear(rgba.b));

    // sRGB to XYZ, scaled by the D65 white point.
    let x = (0.412_456_4 * red + 0.357_576_1 * green + 0.180_437_5 * blue) / 0.950_47;
    let y = 0.212_672_9 * red + 0.715_152_2 * green + 0.072_175 * blue;
    let z = (0.019_333_9 * red + 0.119_192 * green + 0.950_304_1 * blue) / 1.088_83;

    let curve = |value: f32| {
        if value > 216. / 24_389. {
            value.cbrt()
        } else {
            (24_389. / 27. * value + 16.) / 116.
        }
    };
    let (x, y, z) = (curve(x), curve(y), curve(z));

    [116. * y - 16., 500. * (x - y), 200. * (y - z)]
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

    #[test]
    fn color_distance_spans_black_to_white() {
        let span = color_distance(gpui::black(), gpui::white());
        assert!((span - 100.).abs() < 0.5, "{span}");

        let same = color_distance(rgb(0x6e_9b_ff).into(), rgb(0x6e_9b_ff).into());
        assert!(same < 0.001, "{same}");
    }

    #[test]
    fn color_distance_matches_a_known_pair() {
        // Red against green in sRGB: CIELAB (53.24, 80.09, 67.20) and (87.73, -86.18, 83.18).
        let distance = color_distance(rgb(0xff_00_00).into(), rgb(0x00_ff_00).into());
        assert!((distance - 170.6).abs() < 0.5, "{distance}");
    }
}
