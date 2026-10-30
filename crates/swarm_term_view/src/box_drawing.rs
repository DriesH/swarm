//! Box-drawing (U+2500-U+257F) and block elements (U+2580-U+259F) drawn as quads.
//!
//! Font glyphs for these do not join across cells at a 1.3 line height, so each one
//! becomes a few filled rectangles that reach the cell edges.

/// A filled rectangle inside one cell, in pixels from the cell's top left corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CellRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Multiplies the text color's alpha; shades use less than 1.
    pub alpha: f32,
}

impl CellRect {
    fn solid(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
            alpha: 1.,
        }
    }
}

/// Arm weights of each box-drawing character, as `[left, right, up, down]` digits:
/// 0 none, 1 light, 2 heavy. Empty entries (dashes, doubles, diagonals) use the font.
/// Rounded corners are drawn as square ones so they join their neighbors.
const ARMS: [&str; 128] = [
    "1100", "2200", "0011", "0022", "", "", "", "", "", "", "", "", "0101", "0201", "0102", "0202",
    "1001", "2001", "1002", "2002", "0110", "0210", "0120", "0220", "1010", "2010", "1020", "2020",
    "0111", "0211", "0121", "0112", "0122", "0221", "0212", "0222", "1011", "2011", "1021", "1012",
    "1022", "2021", "2012", "2022", "1101", "2101", "1201", "2201", "1102", "2102", "1202", "2202",
    "1110", "2110", "1210", "2210", "1120", "2120", "1220", "2220", "1111", "2111", "1211", "2211",
    "1121", "1112", "1122", "2121", "1221", "2112", "1212", "2221", "2212", "2122", "1222", "2222",
    "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "",
    "", "", "", "", "", "", "", "", "", "0101", "1001", "1010", "0110", "", "", "", "1000", "0010",
    "0100", "0001", "2000", "0020", "0200", "0002", "1200", "0012", "2100", "0021",
];

const FIRST_BOX: u32 = 0x2500;
const LAST_BOX: u32 = 0x257F;
const FIRST_BLOCK: u32 = 0x2580;
const LAST_BLOCK: u32 = 0x259F;

/// Whether this module draws `character` itself.
pub(crate) fn is_drawn(character: char) -> bool {
    cell_rects(character, 8., 16.).is_some()
}

/// The rectangles that draw `character` in a cell of the given size, or `None` when the
/// font should draw it.
pub(crate) fn cell_rects(
    character: char,
    cell_width: f32,
    cell_height: f32,
) -> Option<Vec<CellRect>> {
    let code = u32::from(character);

    if (FIRST_BOX..=LAST_BOX).contains(&code) {
        box_rects(code, cell_width, cell_height)
    } else if (FIRST_BLOCK..=LAST_BLOCK).contains(&code) {
        block_rects(code, cell_width, cell_height)
    } else {
        None
    }
}

fn box_rects(code: u32, cell_width: f32, cell_height: f32) -> Option<Vec<CellRect>> {
    let arms = ARMS.get(usize::try_from(code - FIRST_BOX).ok()?)?;
    let [left, right, up, down] = parse_arms(arms)?;

    let light = (cell_width / 8.).round().max(1.);
    let thickness = |weight: u8| match weight {
        0 => 0.,
        1 => light,
        _ => light * 2.,
    };

    let horizontal = thickness(left.max(right));
    let vertical = thickness(up.max(down));

    // The arms meet in a box at the center so corners and tees have no notch.
    let center_x = ((cell_width - vertical) / 2.).floor();
    let center_y = ((cell_height - horizontal) / 2.).floor();
    let center_right = center_x + vertical;
    let center_bottom = center_y + horizontal;

    let mut rects = Vec::with_capacity(4);

    if left > 0 && left == right {
        let size = thickness(left);
        let y = ((cell_height - size) / 2.).floor();
        rects.push(CellRect::solid(0., y, cell_width, size));
    } else {
        if left > 0 {
            let size = thickness(left);
            let y = ((cell_height - size) / 2.).floor();
            rects.push(CellRect::solid(0., y, center_right, size));
        }

        if right > 0 {
            let size = thickness(right);
            let y = ((cell_height - size) / 2.).floor();
            rects.push(CellRect::solid(center_x, y, cell_width - center_x, size));
        }
    }

    if up > 0 && up == down {
        let size = thickness(up);
        let x = ((cell_width - size) / 2.).floor();
        rects.push(CellRect::solid(x, 0., size, cell_height));
    } else {
        if up > 0 {
            let size = thickness(up);
            let x = ((cell_width - size) / 2.).floor();
            rects.push(CellRect::solid(x, 0., size, center_bottom));
        }

        if down > 0 {
            let size = thickness(down);
            let x = ((cell_width - size) / 2.).floor();
            rects.push(CellRect::solid(x, center_y, size, cell_height - center_y));
        }
    }

    Some(rects)
}

fn parse_arms(arms: &str) -> Option<[u8; 4]> {
    let mut weights = [0u8; 4];
    let mut digits = arms.bytes();

    for weight in &mut weights {
        *weight = digits.next()? - b'0';
    }

    Some(weights)
}

/// Quadrant bits: upper left, upper right, lower left, lower right.
const UPPER_LEFT: u8 = 1;
const UPPER_RIGHT: u8 = 1 << 1;
const LOWER_LEFT: u8 = 1 << 2;
const LOWER_RIGHT: u8 = 1 << 3;

fn block_rects(code: u32, width: f32, height: f32) -> Option<Vec<CellRect>> {
    // Eighths of the cell, rounded to whole pixels so edges stay sharp.
    let across = |eighths: u8| (width * f32::from(eighths) / 8.).round();
    let down = |eighths: u8| (height * f32::from(eighths) / 8.).round();

    let rect = |x: f32, y: f32, w: f32, h: f32| vec![CellRect::solid(x, y, w, h)];
    let shade = |alpha: f32| {
        vec![CellRect {
            alpha,
            ..CellRect::solid(0., 0., width, height)
        }]
    };

    let rects = match code {
        0x2580 => rect(0., 0., width, down(4)),
        0x2581..=0x2588 => {
            let eighths = u8::try_from(code - 0x2580).ok()?;

            rect(0., height - down(eighths), width, down(eighths))
        }
        0x2589..=0x258F => rect(0., 0., across(u8::try_from(0x2590 - code).ok()?), height),
        0x2590 => rect(across(4), 0., width - across(4), height),
        0x2591 => shade(0.25),
        0x2592 => shade(0.5),
        0x2593 => shade(0.75),
        0x2594 => rect(0., 0., width, down(1)),
        0x2595 => rect(width - across(1), 0., across(1), height),
        0x2596 => quadrants(LOWER_LEFT, width, height),
        0x2597 => quadrants(LOWER_RIGHT, width, height),
        0x2598 => quadrants(UPPER_LEFT, width, height),
        0x2599 => quadrants(UPPER_LEFT | LOWER_LEFT | LOWER_RIGHT, width, height),
        0x259A => quadrants(UPPER_LEFT | LOWER_RIGHT, width, height),
        0x259B => quadrants(UPPER_LEFT | UPPER_RIGHT | LOWER_LEFT, width, height),
        0x259C => quadrants(UPPER_LEFT | UPPER_RIGHT | LOWER_RIGHT, width, height),
        0x259D => quadrants(UPPER_RIGHT, width, height),
        0x259E => quadrants(UPPER_RIGHT | LOWER_LEFT, width, height),
        0x259F => quadrants(UPPER_RIGHT | LOWER_LEFT | LOWER_RIGHT, width, height),
        _ => return None,
    };

    Some(rects)
}

fn quadrants(bits: u8, width: f32, height: f32) -> Vec<CellRect> {
    let half_width = (width / 2.).round();
    let half_height = (height / 2.).round();

    [
        (UPPER_LEFT, 0., 0., half_width, half_height),
        (UPPER_RIGHT, half_width, 0., width - half_width, half_height),
        (
            LOWER_LEFT,
            0.,
            half_height,
            half_width,
            height - half_height,
        ),
        (
            LOWER_RIGHT,
            half_width,
            half_height,
            width - half_width,
            height - half_height,
        ),
    ]
    .into_iter()
    .filter(|(bit, ..)| bits & bit != 0)
    .map(|(_, x, y, w, h)| CellRect::solid(x, y, w, h))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: f32 = 8.;
    const HEIGHT: f32 = 16.;

    fn rects(character: char) -> Vec<CellRect> {
        cell_rects(character, WIDTH, HEIGHT).unwrap_or_default()
    }

    #[test]
    fn light_horizontal_line_spans_the_cell() {
        assert_eq!(rects('─'), [CellRect::solid(0., 7., 8., 1.)]);
    }

    #[test]
    fn light_vertical_line_spans_the_cell() {
        assert_eq!(rects('│'), [CellRect::solid(3., 0., 1., 16.)]);
    }

    #[test]
    fn heavy_line_is_thicker_than_light() {
        let light = rects('─')[0].height;
        let heavy = rects('━')[0].height;

        assert!(heavy > light);
    }

    #[test]
    fn corner_has_two_arms_that_overlap_at_the_center() {
        let corner = rects('┌');

        assert_eq!(corner.len(), 2);
        assert_eq!(corner[0], CellRect::solid(3., 7., 5., 1.));
        assert_eq!(corner[1], CellRect::solid(3., 7., 1., 9.));
    }

    #[test]
    fn rounded_corner_matches_the_square_corner() {
        assert_eq!(rects('╭'), rects('┌'));
    }

    #[test]
    fn tee_is_a_full_line_plus_one_arm() {
        assert_eq!(rects('├').len(), 2);
    }

    #[test]
    fn cross_is_two_full_lines() {
        assert_eq!(rects('┼').len(), 2);
    }

    #[test]
    fn mixed_weights_use_each_arm_weight() {
        let rects = rects('╼');

        assert!((rects[0].height - 1.).abs() < f32::EPSILON);
        assert!((rects[1].height - 2.).abs() < f32::EPSILON);
    }

    #[test]
    fn full_block_fills_the_cell() {
        assert_eq!(rects('█'), [CellRect::solid(0., 0., 8., 16.)]);
    }

    #[test]
    fn upper_half_block_fills_the_top() {
        assert_eq!(rects('▀'), [CellRect::solid(0., 0., 8., 8.)]);
    }

    #[test]
    fn left_half_block_fills_the_left() {
        assert_eq!(rects('▌'), [CellRect::solid(0., 0., 4., 16.)]);
    }

    #[test]
    fn lower_one_eighth_block_fills_the_bottom_strip() {
        assert_eq!(rects('▁'), [CellRect::solid(0., 14., 8., 2.)]);
    }

    #[test]
    fn shades_fill_the_cell_with_alpha() {
        let alphas: Vec<f32> = ['░', '▒', '▓'].map(|c| rects(c)[0].alpha).to_vec();

        assert_eq!(alphas, [0.25, 0.5, 0.75]);
    }

    #[test]
    fn quadrants_fill_only_their_corners() {
        assert_eq!(rects('▘'), [CellRect::solid(0., 0., 4., 8.)]);
        assert_eq!(rects('▟').len(), 3);
    }

    #[test]
    fn dashes_doubles_and_text_use_the_font() {
        for character in ['┄', '═', '╱', 'a', ' '] {
            assert_eq!(cell_rects(character, WIDTH, HEIGHT), None, "{character}");
        }
    }

    #[test]
    fn every_drawn_character_stays_inside_the_cell() {
        for code in FIRST_BOX..=LAST_BLOCK {
            let Some(character) = char::from_u32(code) else {
                continue;
            };

            for rect in cell_rects(character, 7.8, 17.).unwrap_or_default() {
                assert!(rect.x >= 0. && rect.y >= 0., "{character}");
                assert!(rect.x + rect.width <= 7.8 + 0.001, "{character}");
                assert!(rect.y + rect.height <= 17. + 0.001, "{character}");
            }
        }
    }
}
