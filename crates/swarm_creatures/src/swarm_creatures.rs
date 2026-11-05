//! The pixel art of the Field guide: 20 bees and wasps, each 64 × 64 pixels.
//!
//! A creature is drawn from ellipses, lines and wing shapes, lit from the top left. The
//! frames are built once, the first time a creature is asked for, and kept.

mod render;
mod species;

use std::sync::OnceLock;

/// A sprite is `SIDE` pixels wide and high.
pub const SIDE: usize = render::SIDE as usize;

/// How many creatures there are. The sprites follow the order of the Field guide list.
pub const COUNT: usize = 20;

/// A color as red, green and blue.
pub type Rgb = [u8; 3];

/// The paper the art is drawn for: wings are mixed with it, so the art looks right on it.
pub const PAPER: Rgb = [0xf4, 0xef, 0xe3];

/// The color of a creature that is not caught yet, drawn on [`PAPER`].
pub const SILHOUETTE: Rgb = [0x2b, 0x2a, 0x33];

/// How a creature moves, and so which frames it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    /// Two wing frames. A creature without wings has none.
    Fly,
    /// Four walking frames.
    Crawl,
    /// One frame.
    Still,
}

/// A run of pixels of one color in a row: where it starts, how long it is, its color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub column: usize,
    pub length: usize,
    pub color: Rgb,
}

/// A frame made from every second pixel is this many pixels wide and high.
pub const HALF_SIDE: usize = SIDE / 2;

/// One frame: a color or nothing for each of the 64 × 64 pixels. It also holds the runs of
/// each row and the frame of every second pixel, so a paint allocates nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    side: usize,
    pixels: Vec<Option<Rgb>>,
    runs: Vec<Vec<Run>>,
    half: Option<Box<Frame>>,
}

impl Frame {
    pub(crate) fn new(pixels: Vec<Option<Rgb>>) -> Self {
        debug_assert_eq!(pixels.len(), SIDE * SIDE);

        let half = Self::sampled(&pixels, HALF_SIDE);

        Self::with_half(SIDE, pixels, Some(Box::new(half)))
    }

    fn with_half(side: usize, pixels: Vec<Option<Rgb>>, half: Option<Box<Self>>) -> Self {
        let runs = (0..side)
            .map(|row| Self::row_runs(&pixels, side, row))
            .collect();

        Self {
            side,
            pixels,
            runs,
            half,
        }
    }

    /// The frame of a `side` of pixels made from every second pixel of `pixels`.
    fn sampled(pixels: &[Option<Rgb>], side: usize) -> Self {
        let taken = (0..side)
            .flat_map(|row| (0..side).map(move |column| (column, row)))
            .map(|(column, row)| pixels.get(row * 2 * SIDE + column * 2).copied().flatten())
            .collect();

        Self::with_half(side, taken, None)
    }

    fn row_runs(pixels: &[Option<Rgb>], side: usize, row: usize) -> Vec<Run> {
        let mut runs: Vec<Run> = Vec::new();

        for column in 0..side {
            let Some(color) = pixels.get(row * side + column).copied().flatten() else {
                continue;
            };

            match runs.last_mut() {
                Some(last) if last.column + last.length == column && last.color == color => {
                    last.length += 1;
                }
                _ => runs.push(Run {
                    column,
                    length: 1,
                    color,
                }),
            }
        }

        runs
    }

    /// How many pixels wide and high the frame is.
    pub fn side(&self) -> usize {
        self.side
    }

    /// The frame made from every second pixel, 32 × 32, for a screen that cannot show a
    /// pixel of the full frame. The half of a half frame is itself.
    pub fn halved(&self) -> &Self {
        self.half.as_deref().unwrap_or(self)
    }

    /// The pixel at a column and a row, or `None` when it is empty or off the frame.
    pub fn pixel(&self, column: usize, row: usize) -> Option<Rgb> {
        if column >= self.side || row >= self.side {
            return None;
        }

        self.pixels.get(row * self.side + column).copied().flatten()
    }

    /// The filled pixels of a row as runs, so a neighbor of one color is one quad.
    pub fn runs(&self, row: usize) -> &[Run] {
        self.runs.get(row).map_or(&[], Vec::as_slice)
    }
}

/// All the frames of one creature.
#[derive(Debug)]
pub struct Sprite {
    fly: Option<Vec<Frame>>,
    crawl: Vec<Frame>,
    still: Vec<Frame>,
}

impl Sprite {
    /// True when the creature has wings.
    pub fn flies(&self) -> bool {
        self.fly.is_some()
    }

    /// The motion the creature really has when `wanted` is asked for: a creature without
    /// wings crawls where it should fly.
    pub fn motion(&self, wanted: Motion) -> Motion {
        match wanted {
            Motion::Fly if !self.flies() => Motion::Crawl,
            other => other,
        }
    }

    /// The frames of a motion, in the order they play. See [`Sprite::motion`].
    pub fn frames(&self, wanted: Motion) -> &[Frame] {
        match self.motion(wanted) {
            Motion::Fly => self.fly.as_deref().unwrap_or(&self.still),
            Motion::Crawl => &self.crawl,
            Motion::Still => &self.still,
        }
    }
}

static SPRITES: [OnceLock<Sprite>; COUNT] = [const { OnceLock::new() }; COUNT];

/// The sprite of the creature at this place in the Field guide list. The first call for a
/// creature draws it.
pub fn sprite(index: usize) -> Option<&'static Sprite> {
    let cell = SPRITES.get(index)?;

    Some(cell.get_or_init(|| species::draw(index)))
}
