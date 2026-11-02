//! Draws the Swarm app icon: the pixel bee on a rounded cream tile, on the macOS icon grid.
//!
//! `app-icon <folder>` writes the PNG files of an `.iconset`, which `iconutil` turns into
//! `Swarm.icns`. `app-icon --png <file>` writes the 1024 px icon alone. Every size is drawn
//! at its own size: nothing is scaled down from the big one.

use std::error::Error;
use std::path::Path;

use image::{Rgba, RgbaImage};
use swarm_sprite::{Frame, RIM_DARK, Rgb, SIZE, Wings};

const USAGE: &str = "usage: app-icon <iconset folder> | app-icon --png <file>";

/// The files `iconutil` expects in an `.iconset`: the name and the width in pixels.
const ICONSET: [(&str, u32); 10] = [
    ("icon_16x16.png", 16),
    ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32),
    ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512),
    ("icon_512x512@2x.png", 1024),
];

/// The size of the picture `--png` writes.
const PREVIEW: u32 = 1024;

/// The macOS 11 icon grid on a 1024 px canvas: a 824 px tile with 100 px around it and
/// corners of 185 px. Every other size scales these numbers.
const CANVAS: f32 = 1024.;
const MARGIN: f32 = 100.;
const CORNER: f32 = 185.;

/// The tile fades from the top color to the bottom color. Sampled from the design.
const TILE_TOP: Rgb = [0xfe, 0xf7, 0xdf];
const TILE_BOTTOM: Rgb = [0xfa, 0xe2, 0xa4];

/// `honey_line` of the light theme. The honeycomb and the shadow are this brown, faintly.
const HONEY_LINE: Rgb = [0x8c, 0x54, 0x08];
const HONEYCOMB_OPACITY: f32 = 0.07;
const SHADOW_OPACITY: f32 = 0.2;

const SQRT_3: f32 = 1.732_050_8;

/// The honeycomb: cells of this radius, lines of this width, in pixels of the 1024 canvas.
const CELL_RADIUS: f32 = 64.;
const CELL_LINE: f32 = 4.;

/// Below this width the honeycomb lines are thinner than a pixel, so they are left out.
const HONEYCOMB_MIN: u32 = 128;

/// The bee with its ring takes this share of the tile.
const BEE_PERCENT: u32 = 76;

/// The sprite is `SIZE` cells wide, and its ring adds one cell on every side.
const BEE_GRID: usize = SIZE + 2;
const BEE_CELLS: u32 = 26;

/// The shadow is an ellipse below the bee, sized as shares of the bee's width.
const SHADOW_WIDTH: f32 = 0.36;
const SHADOW_HEIGHT: f32 = 0.05;

type BeeGrid = [[Option<Rgb>; BEE_GRID]; BEE_GRID];

/// A color with room to blend.
type Color = [f32; 3];

fn main() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();

    run(&arguments)
}

fn run(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    match arguments {
        [flag, file] if flag == "--png" => render(PREVIEW).save(file)?,
        [folder] if !folder.starts_with('-') => write_iconset(Path::new(folder))?,
        _ => return Err(USAGE.into()),
    }

    Ok(())
}

fn write_iconset(folder: &Path) -> Result<(), Box<dyn Error>> {
    std::fs::create_dir_all(folder)?;

    for (name, size) in ICONSET {
        render(size).save(folder.join(name))?;
    }

    Ok(())
}

/// The icon on a square canvas of `size` pixels. Outside the tile it is transparent.
fn render(size: u32) -> RgbaImage {
    let icon = Icon::new(size);

    RgbaImage::from_fn(size, size, |column, row| {
        let coverage = icon.coverage(column, row);

        if coverage <= 0. {
            return Rgba([0, 0, 0, 0]);
        }

        let [red, green, blue] = icon
            .bee_pixel(column, row)
            .unwrap_or_else(|| icon.background(column, row).map(channel));

        Rgba([red, green, blue, channel(coverage * 255.)])
    })
}

/// Where everything sits on one canvas.
struct Icon {
    size: u32,
    /// The scale from the 1024 canvas.
    scale: f32,
    margin: u32,
    tile: u32,
    bee_left: u32,
    bee_top: u32,
    bee_side: u32,
    grid: BeeGrid,
}

impl Icon {
    fn new(size: u32) -> Self {
        let scale = float(size) / CANVAS;
        let margin = whole(MARGIN * scale);
        let tile = size.saturating_sub(2 * margin);
        let bee_side = bee_side(tile);
        let bee_left = margin + tile.saturating_sub(bee_side) / 2;

        Self {
            size,
            scale,
            margin,
            tile,
            bee_left,
            bee_top: bee_left,
            bee_side,
            grid: bee_grid(),
        }
    }

    /// How much of the pixel the tile covers, from 0 to 1. Only its rounded edge is smooth.
    fn coverage(&self, column: u32, row: u32) -> f32 {
        let half = float(self.tile) / 2.;
        let center = float(self.margin) + half;
        let reach = (half - CORNER * self.scale).max(0.);

        let across = (float(column) + 0.5 - center).abs() - reach;
        let down = (float(row) + 0.5 - center).abs() - reach;
        let outside = across.max(0.).hypot(down.max(0.));
        let inside = across.max(down).min(0.);
        let distance = outside + inside - CORNER * self.scale;

        (0.5 - distance).clamp(0., 1.)
    }

    /// The color of the pixel without the bee: the tile, the honeycomb and the shadow.
    fn background(&self, column: u32, row: u32) -> Color {
        let down = (float(row) + 0.5 - float(self.margin)) / float(self.tile);
        let tile = mix(color(TILE_TOP), color(TILE_BOTTOM), down.clamp(0., 1.));

        let lines = mix(
            tile,
            color(HONEY_LINE),
            HONEYCOMB_OPACITY * self.honeycomb(column, row),
        );

        mix(
            lines,
            color(HONEY_LINE),
            SHADOW_OPACITY * self.shadow(column, row),
        )
    }

    /// How much of the pixel lies on a honeycomb line, from 0 to 1. The cells have pointed
    /// tops, and one is centered on the canvas.
    fn honeycomb(&self, column: u32, row: u32) -> f32 {
        if self.size < HONEYCOMB_MIN {
            return 0.;
        }

        let radius = CELL_RADIUS * self.scale;
        let half_line = CELL_LINE * self.scale / 2.;

        let middle = float(self.size) / 2.;
        let across = float(column) + 0.5 - middle;
        let down = float(row) + 0.5 - middle;

        let (cell_column, cell_row) = round_to_cell(
            (SQRT_3 / 3. * across - down / 3.) / radius,
            (2. / 3. * down) / radius,
        );
        let from_across = across - radius * SQRT_3 * (cell_column + cell_row / 2.);
        let from_down = down - radius * 1.5 * cell_row;

        // A cell has three pairs of parallel sides. The pixel's distance to the nearest
        // side is the cell's inner radius minus how far it reaches toward the farthest pair.
        let reach = [
            from_across.abs(),
            (from_across / 2. + from_down * SQRT_3 / 2.).abs(),
            (from_across / 2. - from_down * SQRT_3 / 2.).abs(),
        ]
        .into_iter()
        .fold(0., f32::max);
        let to_side = radius * SQRT_3 / 2. - reach;

        (half_line + 0.5 - to_side).clamp(0., 1.)
    }

    /// How dark the soft shadow is at the pixel, from 0 to 1: an ellipse that sits one
    /// sprite pixel below the bee and fades toward its edge.
    fn shadow(&self, column: u32, row: u32) -> f32 {
        let side = float(self.bee_side);
        let center = (
            float(self.bee_left) + side / 2.,
            float(self.bee_top) + side + side / float(BEE_CELLS),
        );

        let across = (float(column) + 0.5 - center.0) / (SHADOW_WIDTH * side);
        let down = (float(row) + 0.5 - center.1) / (SHADOW_HEIGHT * side);
        let distance = across * across + down * down;

        if distance >= 1. {
            return 0.;
        }

        (1. - distance).powi(2)
    }

    /// The sprite or ring color at the pixel, or `None` where the bee leaves it empty. Each
    /// pixel takes the color of the cell at its center, so edges stay hard.
    fn bee_pixel(&self, column: u32, row: u32) -> Option<Rgb> {
        let across = column
            .checked_sub(self.bee_left)
            .filter(|at| *at < self.bee_side)?;
        let down = row
            .checked_sub(self.bee_top)
            .filter(|at| *at < self.bee_side)?;

        let cell = |at: u32| usize::try_from((2 * at + 1) * BEE_CELLS / (2 * self.bee_side)).ok();

        self.grid
            .get(cell(down)?)?
            .get(cell(across)?)
            .copied()
            .flatten()
    }
}

/// The width of the bee with its ring on a tile of `tile` pixels. A whole number of pixels
/// for each sprite pixel when one fits, so the bee stays crisp; on the smallest icons the
/// bee is sampled instead.
fn bee_side(tile: u32) -> u32 {
    let target = tile * BEE_PERCENT / 100;
    let fits = tile * 9 / 10 / BEE_CELLS;
    let whole = ((target + BEE_CELLS / 2) / BEE_CELLS).min(fits) * BEE_CELLS;

    if whole == 0 { target } else { whole }
}

/// The sprite with its ring, wings down.
fn bee_grid() -> BeeGrid {
    let frame = Frame::new(Wings::Down, false);
    let mut grid: BeeGrid = [[None; BEE_GRID]; BEE_GRID];

    // The ring comes first, so cell 0 is the ring's first column and the sprite starts at 1.
    for (column, row) in frame.rim() {
        let cell = usize::try_from(row + 1)
            .ok()
            .and_then(|row| grid.get_mut(row))
            .zip(usize::try_from(column + 1).ok())
            .and_then(|(cells, column)| cells.get_mut(column));

        if let Some(cell) = cell {
            *cell = Some(RIM_DARK);
        }
    }

    for row in 0..SIZE {
        for column in 0..SIZE {
            if let Some(rgb) = frame.at(column, row) {
                grid[row + 1][column + 1] = Some(rgb);
            }
        }
    }

    grid
}

/// The hexagon cell that holds a point, in axial coordinates (cube rounding).
fn round_to_cell(column: f32, row: f32) -> (f32, f32) {
    let depth = -column - row;
    let (mut rounded_column, mut rounded_row, rounded_depth) =
        (column.round(), row.round(), depth.round());

    let off = (
        (rounded_column - column).abs(),
        (rounded_row - row).abs(),
        (rounded_depth - depth).abs(),
    );

    if off.0 > off.1 && off.0 > off.2 {
        rounded_column = -rounded_row - rounded_depth;
    } else if off.1 > off.2 {
        rounded_row = -rounded_column - rounded_depth;
    }

    (rounded_column, rounded_row)
}

fn color(rgb: Rgb) -> Color {
    rgb.map(f32::from)
}

fn mix(from: Color, to: Color, amount: f32) -> Color {
    std::array::from_fn(|index| from[index] + (to[index] - from[index]) * amount)
}

/// A pixel count as a float. A canvas stays far below 65 536 pixels.
fn float(value: u32) -> f32 {
    f32::from(u16::try_from(value).unwrap_or(u16::MAX))
}

/// A float as a whole number of pixels, rounded and never negative.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is rounded and not negative, and a canvas fits in a u32"
)]
fn whole(value: f32) -> u32 {
    value.round().max(0.) as u32
}

/// A color channel from 0 to 255.
fn channel(value: f32) -> u8 {
    u8::try_from(whole(value).min(255)).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn opaque(image: &RgbaImage, column: u32, row: u32) -> bool {
        image.get_pixel(column, row)[3] == 255
    }

    fn transparent(image: &RgbaImage, column: u32, row: u32) -> bool {
        image.get_pixel(column, row)[3] == 0
    }

    fn sizes() -> BTreeSet<u32> {
        ICONSET.iter().map(|(_, size)| *size).collect()
    }

    fn brightness(image: &RgbaImage, column: u32, row: u32) -> u32 {
        image
            .get_pixel(column, row)
            .0
            .iter()
            .take(3)
            .map(|channel| u32::from(*channel))
            .sum()
    }

    #[test]
    fn the_corners_of_every_size_are_transparent() {
        for size in sizes() {
            let image = render(size);
            let last = size - 1;

            for (column, row) in [(0, 0), (last, 0), (0, last), (last, last)] {
                assert!(
                    transparent(&image, column, row),
                    "{size} px at {column},{row}"
                );
            }
        }
    }

    #[test]
    fn the_tile_is_824_px_with_100_px_around_it() {
        let image = render(1024);

        assert!(transparent(&image, 99, 512));
        assert!(opaque(&image, 100, 512));
        assert!(opaque(&image, 923, 512));
        assert!(transparent(&image, 924, 512));
        assert!(transparent(&image, 512, 99));
        assert!(opaque(&image, 512, 100));
    }

    #[test]
    fn the_tile_corners_are_rounded_with_a_radius_of_185_px() {
        let image = render(1024);

        assert!(transparent(&image, 100, 100));
        assert!(transparent(&image, 120, 120));
        assert!(opaque(&image, 100 + 185, 100));
        assert!(opaque(&image, 100, 100 + 185));
        assert!(opaque(&image, 180, 180));
    }

    #[test]
    fn the_bee_is_drawn_with_hard_pixels_24_px_wide() {
        let image = render(1024);
        let frame = Frame::new(Wings::Down, false);
        let ring: BTreeSet<(i32, i32)> = frame.rim().into_iter().collect();

        // 24 px for each sprite pixel, with the ring's first cell starting at 200,200.
        let mut checked = 0;
        for row in -1..=24_i32 {
            for column in -1..=24_i32 {
                let expected = match (usize::try_from(column).ok(), usize::try_from(row).ok()) {
                    (Some(x), Some(y)) => frame.at(x, y),
                    _ => None,
                }
                .or_else(|| ring.contains(&(column, row)).then_some(RIM_DARK));

                let Some([red, green, blue]) = expected else {
                    continue;
                };

                let left = u32::try_from(200 + (column + 1) * 24).expect("on the canvas");
                let top = u32::try_from(200 + (row + 1) * 24).expect("on the canvas");

                for y in top..top + 24 {
                    for x in left..left + 24 {
                        assert_eq!(
                            image.get_pixel(x, y).0,
                            [red, green, blue, 255],
                            "sprite pixel {column},{row} at {x},{y}"
                        );
                    }
                }
                checked += 1;
            }
        }

        assert!(checked > 300, "only {checked} sprite pixels checked");
    }

    #[test]
    fn no_size_blends_the_bee_into_the_tile() {
        let sprite: BTreeSet<[u8; 3]> = Icon::new(1024)
            .grid
            .iter()
            .flatten()
            .flatten()
            .copied()
            .collect();

        for size in [16, 32, 64, 128, 256] {
            let image = render(size);
            let icon = Icon::new(size);

            for (column, row, pixel) in image.enumerate_pixels() {
                let [red, green, blue, alpha] = pixel.0;

                if alpha != 255 {
                    continue;
                }

                let tile = icon.background(column, row).map(channel);

                assert!(
                    sprite.contains(&[red, green, blue]) || [red, green, blue] == tile,
                    "{size} px at {column},{row}: {:?}",
                    pixel.0
                );
            }
        }
    }

    #[test]
    fn the_bee_is_a_whole_number_of_pixels_per_sprite_pixel_from_64_px() {
        for size in [64, 128, 256, 512, 1024] {
            let icon = Icon::new(size);

            assert_eq!(icon.bee_side % BEE_CELLS, 0, "{size} px");
            assert!(icon.bee_side > 0, "{size} px");
        }

        assert_eq!(Icon::new(1024).bee_side, 24 * BEE_CELLS);
        assert_eq!(Icon::new(512).bee_side, 12 * BEE_CELLS);
    }

    #[test]
    fn the_smallest_sizes_draw_a_bee_of_the_same_share_of_the_tile() {
        for size in [16, 32] {
            let icon = Icon::new(size);
            let share = icon.bee_side * 100 / icon.tile;

            assert!((70..=80).contains(&share), "{size} px: {share}%");
        }
    }

    #[test]
    fn the_bee_stays_inside_the_tile_at_every_size() {
        for size in sizes() {
            let icon = Icon::new(size);

            assert!(icon.bee_left >= icon.margin, "{size} px");
            assert!(
                icon.bee_left + icon.bee_side <= icon.margin + icon.tile,
                "{size} px"
            );
        }
    }

    #[test]
    fn the_grid_fits_the_sprite_and_its_ring() {
        assert_eq!(usize::try_from(BEE_CELLS), Ok(SIZE + 2));
        assert_eq!(BEE_GRID, SIZE + 2);
    }

    #[test]
    fn the_bee_casts_a_soft_shadow_below_it() {
        let image = render(1024);

        // One sprite pixel below the bee's last row, in the middle and far to the side.
        let under = brightness(&image, 512, 848);
        let aside = brightness(&image, 150, 848);

        assert!(under + 40 < aside, "under {under}, aside {aside}");
    }

    #[test]
    fn the_shadow_fades_toward_its_edge() {
        let icon = Icon::new(1024);
        let strengths = [0, 80, 160, 220].map(|offset| icon.shadow(512 + offset, 848));

        assert!(
            strengths.windows(2).all(|pair| pair[0] > pair[1]),
            "{strengths:?}"
        );
        assert!(strengths[3] > 0.);
        assert!(icon.shadow(512 + 300, 848) <= 0.);
        assert!(icon.shadow(512, 848 + 100) <= 0.);
    }

    #[test]
    fn the_tile_fades_from_the_top_color_to_the_bottom_color() {
        let image = render(1024);
        let near = |pixel: [u8; 3], expected: Rgb| {
            pixel
                .iter()
                .zip(expected)
                .all(|(found, wanted)| found.abs_diff(wanted) <= 12)
        };

        let [red, green, blue, _] = image.get_pixel(512, 110).0;
        assert!(
            near([red, green, blue], TILE_TOP),
            "{red:x} {green:x} {blue:x}"
        );

        let [red, green, blue, _] = image.get_pixel(512, 915).0;
        assert!(
            near([red, green, blue], TILE_BOTTOM),
            "{red:x} {green:x} {blue:x}"
        );
    }

    #[test]
    fn the_honeycomb_draws_faint_lines_on_the_tile() {
        let image = render(1024);

        // A row above the bee: the tile's gradient is flat along it, so only lines differ.
        let shades: BTreeSet<u32> = (300..700)
            .map(|column| brightness(&image, column, 150))
            .collect();
        let darkest = shades.first().copied().unwrap_or(0);
        let lightest = shades.last().copied().unwrap_or(0);

        assert!(shades.len() > 3, "no lines in {shades:?}");
        assert!(
            darkest + 5 < lightest && darkest + 60 > lightest,
            "{darkest} {lightest}"
        );
    }

    #[test]
    fn the_honeycomb_is_left_out_where_its_lines_are_thinner_than_a_pixel() {
        let icon = Icon::new(64);

        assert!(icon.honeycomb(32, 32) <= 0.);
        assert!((0..64).all(|column| icon.honeycomb(column, 20) <= 0.));
    }

    #[test]
    fn every_iconset_file_has_the_name_and_size_iconutil_expects() {
        let mut expected = Vec::new();
        for base in [16, 32, 128, 256, 512] {
            expected.push((format!("icon_{base}x{base}.png"), base));
            expected.push((format!("icon_{base}x{base}@2x.png"), base * 2));
        }

        let listed: Vec<(String, u32)> = ICONSET
            .iter()
            .map(|(name, size)| ((*name).to_owned(), *size))
            .collect();

        assert_eq!(listed, expected);
    }

    #[test]
    fn writing_the_iconset_makes_every_file_at_its_size() {
        let folder =
            std::env::temp_dir().join(format!("app-icon-test-{}.iconset", std::process::id()));

        write_iconset(&folder).expect("write the iconset");

        for (name, size) in ICONSET {
            let dimensions = image::image_dimensions(folder.join(name)).expect("a PNG");

            assert_eq!(dimensions, (size, size), "{name}");
        }

        std::fs::remove_dir_all(&folder).expect("remove the test folder");
    }

    #[test]
    fn a_wrong_command_line_is_refused_with_the_usage() {
        let arguments = |words: &[&str]| -> Vec<String> {
            words.iter().map(|word| (*word).to_owned()).collect()
        };

        for words in [
            &[][..],
            &["--png"],
            &["-x"],
            &["a", "b"],
            &["--png", "a", "b"],
        ] {
            let error = run(&arguments(words)).expect_err("a wrong command line");

            assert_eq!(error.to_string(), USAGE, "{words:?}");
        }
    }
}
