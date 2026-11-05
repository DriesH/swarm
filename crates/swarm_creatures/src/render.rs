//! Draws one frame of a creature from ellipses, lines and wings.
//!
//! It is a port of the generator on the design canvas, and it matches it pixel for pixel.
//! The arithmetic follows the original, including its rounding.

use crate::{Frame, PAPER, Rgb};

/// The frame is `SIDE` pixels wide and high.
pub(crate) const SIDE: u16 = 64;

const CELLS: usize = 64 * 64;

const SPECULAR: Rgb = hex(0xfb_fff7);
const GLINT: Rgb = [0xff; 3];
const BLACK: Rgb = [0; 3];

/// Where the light comes from, before it is scaled to length 1.
const LIGHT: [f64; 3] = [-0.55, -0.65, 0.53];

/// A color from a number like `0xf4efe3`.
pub(crate) const fn hex(value: u32) -> Rgb {
    let [_, red, green, blue] = value.to_be_bytes();

    [red, green, blue]
}

/// The bands of a surface: from `u` of the band on, its palette is used.
pub(crate) type Bands = &'static [(f64, &'static [Rgb])];

#[derive(Clone, Copy, Debug)]
pub(crate) struct Oval {
    pub cx: f64,
    pub cy: f64,
    pub rx: f64,
    pub ry: f64,
    pub rot: f64,
}

/// How the surface of a body part looks.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Surface {
    pub fur: bool,
    /// How ragged the edge is, from 0.
    pub fuzz: f64,
    pub metallic: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Line {
    pub points: Vec<(f64, f64)>,
    /// The dark outline color first, then the lit color.
    pub palette: &'static [Rgb],
    pub width: u8,
}

#[derive(Clone, Debug)]
pub(crate) struct Ellipse {
    pub oval: Oval,
    pub palette: &'static [Rgb],
    pub bands: Bands,
    pub surface: Surface,
}

#[derive(Clone, Debug)]
pub(crate) struct Wing {
    pub oval: Oval,
    pub tint: Rgb,
    pub alpha: f64,
    pub vein: Rgb,
}

/// One shape of a creature. Shapes later in the list are in front.
#[derive(Clone, Debug)]
pub(crate) enum Shape {
    Line(Line),
    Ellipse(Ellipse),
    Wing(Wing),
}

impl Shape {
    /// The same shape, `rows` lower.
    pub(crate) fn lowered(&self, rows: f64) -> Self {
        match self {
            Self::Line(line) => Self::Line(Line {
                points: line.points.iter().map(|&(x, y)| (x, y + rows)).collect(),
                ..line.clone()
            }),
            Self::Ellipse(ellipse) => Self::Ellipse(Ellipse {
                oval: Oval {
                    cy: ellipse.oval.cy + rows,
                    ..ellipse.oval
                },
                ..ellipse.clone()
            }),
            Self::Wing(wing) => Self::Wing(Wing {
                oval: Oval {
                    cy: wing.oval.cy + rows,
                    ..wing.oval
                },
                ..wing.clone()
            }),
        }
    }
}

#[derive(Clone, Copy)]
struct Cell {
    z: u16,
    color: Rgb,
    /// The color of the outline next to this pixel.
    dark: Rgb,
    wing: bool,
}

type Grid = Vec<Option<Cell>>;

/// `Math.round` of the original: halves go up.
fn round(value: f64) -> f64 {
    (value + 0.5).floor()
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the value is a rounded pixel position, far inside i32"
)]
fn round_to_int(value: f64) -> i32 {
    round(value) as i32
}

/// A number from 0 to 1 that depends only on the pixel and the seed.
fn hash(x: u16, y: u16, seed: u32) -> f64 {
    let mut value = u32::from(x).wrapping_mul(374_761_393)
        ^ u32::from(y).wrapping_mul(668_265_263)
        ^ seed.wrapping_mul(982_451_653);
    value = (value ^ (value >> 13)).wrapping_mul(1_274_126_177);

    f64::from(value ^ (value >> 16)) / 4_294_967_295.
}

/// The color `t` of the way from `from` to `to`.
fn mix(from: Rgb, to: Rgb, t: f64) -> Rgb {
    let mut mixed = [0; 3];

    for ((slot, start), end) in mixed.iter_mut().zip(from).zip(to) {
        let start = f64::from(start);
        let value = round_to_int(start + (f64::from(end) - start) * t);

        *slot = u8::try_from(value).unwrap_or(u8::MAX);
    }

    mixed
}

fn index(x: i32, y: i32) -> Option<usize> {
    let (x, y) = (u16::try_from(x).ok()?, u16::try_from(y).ok()?);

    (x < SIDE && y < SIDE).then(|| usize::from(y) * usize::from(SIDE) + usize::from(x))
}

fn tone_of(dot: f64, metallic: bool) -> u8 {
    if metallic && dot > 0.9 {
        4
    } else if dot > if metallic { 0.62 } else { 0.72 } {
        3
    } else if dot > 0.3 {
        2
    } else {
        u8::from(dot > -0.15)
    }
}

/// Where a pixel is in the frame of an oval: the offset from its center, and the offset
/// as a fraction of the radii, turned with the oval.
struct Probe {
    dx: f64,
    dy: f64,
    u: f64,
    v: f64,
    distance: f64,
}

fn probe(oval: &Oval, sin: f64, cos: f64, x: u16, y: u16) -> Probe {
    let dx = f64::from(x) + 0.5 - oval.cx;
    let dy = f64::from(y) + 0.5 - oval.cy;
    let along = dx * cos + dy * sin;
    let across = -dx * sin + dy * cos;
    let (u, v) = (along / oval.rx, across / oval.ry);

    Probe {
        dx,
        dy,
        u,
        v,
        distance: u * u + v * v,
    }
}

fn paint_line(grid: &mut Grid, line: &Line, z: u16) {
    let (Some(&dark), Some(&lit)) = (line.palette.first(), line.palette.get(1)) else {
        return;
    };

    for pair in line.points.windows(2) {
        let [start, end] = pair else {
            continue;
        };
        let ((x0, y0), (x1, y1)) = (*start, *end);
        let steps = (x1 - x0).abs().max((y1 - y0).abs()) * 2. + 1.;

        let mut step = 0.;
        while step <= steps {
            let x = round_to_int(x0 + (x1 - x0) * step / steps);
            let y = round_to_int(y0 + (y1 - y0) * step / steps);

            for offset in 0..i32::from(line.width) {
                if let Some(slot) = index(x, y + offset).and_then(|at| grid.get_mut(at)) {
                    *slot = Some(Cell {
                        z,
                        color: if offset == 0 { lit } else { dark },
                        dark,
                        wing: false,
                    });
                }
            }

            step += 1.;
        }
    }
}

fn paint_wing(grid: &mut Grid, wing: &Wing, z: u16) {
    let (sin, cos) = (wing.oval.rot.sin(), wing.oval.rot.cos());

    for y in 0..SIDE {
        for x in 0..SIDE {
            let at = probe(&wing.oval, sin, cos, x, y);
            if at.distance > 1. {
                continue;
            }

            let Some(slot) = index(i32::from(x), i32::from(y)).and_then(|at| grid.get_mut(at))
            else {
                continue;
            };

            let base = slot.map_or(PAPER, |under| under.color);
            let vein =
                at.v.abs() < 0.09 || (at.u - 0.15).abs() < 0.06 || (at.v + 0.45).abs() < 0.07;

            *slot = Some(Cell {
                z,
                color: if vein {
                    wing.vein
                } else {
                    mix(base, wing.tint, wing.alpha)
                },
                dark: wing.vein,
                wing: true,
            });
        }
    }
}

fn paint_ellipse(grid: &mut Grid, ellipse: &Ellipse, z: u16, light: [f64; 3]) {
    let oval = &ellipse.oval;
    let surface = ellipse.surface;
    let (sin, cos) = (oval.rot.sin(), oval.rot.cos());

    for y in 0..SIDE {
        for x in 0..SIDE {
            let at = probe(oval, sin, cos, x, y);
            let edge = if surface.fuzz > 0. {
                1. + (hash(x, y, u32::from(z) + 7) - 0.5) * surface.fuzz
            } else {
                1.
            };
            if at.distance > edge {
                continue;
            }

            let mut palette = ellipse.palette;
            for &(from, banded) in ellipse.bands {
                if at.u >= from {
                    palette = banded;
                }
            }

            let depth = (1. - at.distance.min(1.)).max(0.).sqrt();
            let dot = at.dx / oval.rx * light[0] + at.dy / oval.ry * light[1] + depth * light[2];
            let mut tone = tone_of(dot, surface.metallic);

            if surface.fur {
                let grain = hash(x, y, u32::from(z) + 31);
                if grain < 0.18 {
                    tone = tone.saturating_sub(1);
                }
                if grain > 0.9 {
                    tone = (tone + 1).min(3);
                }
            }

            let Some(&dark) = palette.first() else {
                continue;
            };
            let color = if tone == 4 {
                SPECULAR
            } else {
                palette
                    .get(usize::from(tone).min(palette.len().saturating_sub(1)))
                    .copied()
                    .unwrap_or(dark)
            };

            if let Some(slot) = index(i32::from(x), i32::from(y)).and_then(|at| grid.get_mut(at)) {
                *slot = Some(Cell {
                    z,
                    color,
                    dark,
                    wing: false,
                });
            }
        }
    }
}

/// The four neighbors of a pixel that are in the frame, in the order the original looks.
fn neighbors(grid: &Grid, x: i32, y: i32) -> impl Iterator<Item = Cell> + '_ {
    [(1, 0), (-1, 0), (0, 1), (0, -1)]
        .into_iter()
        .filter_map(move |(a, b)| {
            index(x + a, y + b)
                .and_then(|at| grid.get(at))
                .copied()
                .flatten()
        })
}

/// Draws the shapes, back to front, then the outline and the glint.
pub(crate) fn render(shapes: &[Shape], glint: (i32, i32)) -> Frame {
    let length = LIGHT.iter().map(|part| part * part).sum::<f64>().sqrt();
    let light = LIGHT.map(|part| part / length);

    let mut grid: Grid = vec![None; CELLS];

    for (z, shape) in (0..).zip(shapes) {
        match shape {
            Shape::Line(line) => paint_line(&mut grid, line, z),
            Shape::Wing(wing) => paint_wing(&mut grid, wing, z),
            Shape::Ellipse(ellipse) => paint_ellipse(&mut grid, ellipse, z, light),
        }
    }

    // The outline: empty pixels next to the creature, and lower parts along a higher part.
    let mut out = grid.clone();

    for y in 0..i32::from(SIDE) {
        for x in 0..i32::from(SIDE) {
            let Some(at) = index(x, y) else {
                continue;
            };
            let here = grid.get(at).copied().flatten();

            let Some(here) = here else {
                let mut front: Option<Cell> = None;
                for near in neighbors(&grid, x, y) {
                    if front.is_none_or(|best| near.z > best.z) {
                        front = Some(near);
                    }
                }

                if let (Some(front), Some(slot)) = (front, out.get_mut(at)) {
                    *slot = Some(Cell {
                        z: 0,
                        color: mix(front.dark, BLACK, 0.45),
                        dark: front.dark,
                        wing: false,
                    });
                }

                continue;
            };

            if here.wing {
                continue;
            }

            let covered = neighbors(&grid, x, y).any(|near| !near.wing && near.z > here.z);
            if let (true, Some(slot)) = (covered, out.get_mut(at)) {
                *slot = Some(Cell {
                    color: mix(here.dark, BLACK, 0.25),
                    ..here
                });
            }
        }
    }

    if let Some(slot) = index(glint.0, glint.1).and_then(|at| out.get_mut(at)) {
        *slot = Some(Cell {
            z: 99,
            color: GLINT,
            dark: GLINT,
            wing: false,
        });
    }

    Frame::new(
        out.into_iter()
            .map(|cell| cell.map(|cell| cell.color))
            .collect(),
    )
}

pub(crate) fn rounded(value: f64) -> i32 {
    round_to_int(value)
}
