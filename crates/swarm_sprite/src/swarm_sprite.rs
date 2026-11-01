//! The pixel bee: a 24 × 24 sprite with two wing frames and a crown. The app draws it
//! with quads and `swarm buzz` prints it with half blocks, both from this data.

/// The sprite is `SIZE` pixels wide and high.
pub const SIZE: usize = 24;

/// A color as red, green and blue.
pub type Rgb = [u8; 3];

/// The body, one letter per pixel. `.` is empty; see [`color`] for the letters.
pub const BODY: [&str; SIZE] = [
    "....OOO..........OOO....",
    "....OAO..........OAO....",
    "....OOO..........OOO....",
    "......O..........O......",
    ".......O........O.......",
    "........O......O........",
    ".........OOOOOO.........",
    "........OLLLYYYO........",
    ".......OLLYYYYYDO.......",
    "......OLHEYYYYHEDO......",
    "......OLEEYYYYEEDO......",
    "......OLEEYYYYEEDO......",
    "......OPPYOYYOYPPO......",
    "......OLYYYOOYYYDO......",
    "......OLYYYYYYYYDO......",
    "......ObBBBBBBBBBO......",
    "......OBBBBBBBBBBO......",
    "......OLYYYYYYYYDO......",
    "......ODDYYYYYYDDO......",
    ".......OBBBBBBBBO.......",
    "........ODYYYYDO........",
    ".........OBBBBO.........",
    "..........OBBO..........",
    "...........OO...........",
];

/// The left wing with the wings up: the row, then the pixels from the left edge. The
/// right wing is the mirror image.
pub const WINGS_UP: [(usize, &str); 8] = [
    (4, "..OO.."),
    (5, ".OWWO."),
    (6, "OWWWWO"),
    (7, "OWWWwO"),
    (8, "OWWwwO"),
    (9, ".OWwvO"),
    (10, "..Ovv."),
    (11, "...O.."),
];

/// The left wing with the wings down.
pub const WINGS_DOWN: [(usize, &str); 8] = [
    (9, "..OOO."),
    (10, ".OWWWW"),
    (11, "OWWWWw"),
    (12, "OWWWww"),
    (13, "OWwwvv"),
    (14, ".Owvvv"),
    (15, "..Ovvv"),
    (16, "...OOO"),
];

/// The crown: the row, then the pixels from `CROWN_LEFT`.
pub const CROWN: [(usize, &str); 5] = [
    (1, "O..OO..O"),
    (2, "OLOLLOLO"),
    (3, "OYLYYLYO"),
    (4, "OYHYYHYO"),
    (5, "OOOOOOOO"),
];

/// The column where the crown starts.
pub const CROWN_LEFT: usize = 8;

/// The outline color around the sprite in dark mode. In light mode there is none.
pub const RIM_DARK: Rgb = [0x33, 0x2c, 0x23];

/// The color of a letter in the sprite, or `None` for `.` and any other letter.
pub fn color(letter: char) -> Option<Rgb> {
    let rgb = match letter {
        'O' | 'E' => [0x17, 0x13, 0x0e],
        'L' => [0xff, 0xe9, 0x8a],
        'Y' | 'A' => [0xfc, 0xc9, 0x35],
        'D' => [0xe0, 0x9b, 0x17],
        'B' => [0x2b, 0x23, 0x1c],
        'b' => [0x4a, 0x3d, 0x31],
        'W' | 'H' => [0xff, 0xff, 0xff],
        'w' => [0xe6, 0xee, 0xf6],
        'v' => [0xc4, 0xd4, 0xe4],
        'P' => [0xf5, 0x9a, 0x7a],
        _ => return None,
    };

    Some(rgb)
}

/// Which way the wings point. Flickering between the two is the buzz.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wings {
    Up,
    Down,
}

impl Wings {
    /// The other frame.
    #[must_use]
    pub fn flipped(self) -> Self {
        match self {
            Self::Up => Self::Down,
            Self::Down => Self::Up,
        }
    }

    fn rows(self) -> &'static [(usize, &'static str)] {
        match self {
            Self::Up => &WINGS_UP,
            Self::Down => &WINGS_DOWN,
        }
    }
}

/// One frame of the sprite: a letter in each of the 24 × 24 cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    cells: [[char; SIZE]; SIZE],
}

impl Frame {
    /// The body with the wings on both sides, and the crown when `crown` is true. A wing
    /// never covers a pixel of the body.
    pub fn new(wings: Wings, crown: bool) -> Self {
        let mut cells = [['.'; SIZE]; SIZE];

        for (row, line) in BODY.iter().enumerate() {
            for (column, letter) in line.chars().enumerate() {
                cells[row][column] = letter;
            }
        }

        if crown {
            for (row, line) in CROWN {
                for (offset, letter) in line.chars().enumerate() {
                    if letter != '.' {
                        cells[row][CROWN_LEFT + offset] = letter;
                    }
                }
            }
        }

        for (row, line) in wings.rows() {
            for (column, letter) in line.chars().enumerate() {
                if letter == '.' {
                    continue;
                }

                for target in [column, SIZE - 1 - column] {
                    if cells[*row][target] == '.' {
                        cells[*row][target] = letter;
                    }
                }
            }
        }

        Self { cells }
    }

    /// The color at a pixel, or `None` where the sprite is empty or `x`, `y` are outside.
    pub fn at(&self, x: usize, y: usize) -> Option<Rgb> {
        self.cells.get(y)?.get(x).copied().and_then(color)
    }

    /// The pixels of `row` as runs of one color: where the run starts, how long it is
    /// and its color. One quad per run keeps neighbors from leaving seams.
    pub fn runs(&self, row: usize) -> Vec<(usize, usize, Rgb)> {
        let mut runs: Vec<(usize, usize, Rgb)> = Vec::new();

        for column in 0..SIZE {
            let Some(rgb) = self.at(column, row) else {
                continue;
            };

            match runs.last_mut() {
                Some((start, length, last)) if *start + *length == column && *last == rgb => {
                    *length += 1;
                }
                _ => runs.push((column, 1, rgb)),
            }
        }

        runs
    }

    /// The empty pixels that touch a filled one, up, down, left or right, including the
    /// ring just outside the sprite. Coordinates run from -1 to `SIZE`.
    pub fn rim(&self) -> Vec<(i32, i32)> {
        let size = i32::try_from(SIZE).unwrap_or(24);
        let filled = |x: i32, y: i32| {
            usize::try_from(x)
                .ok()
                .zip(usize::try_from(y).ok())
                .is_some_and(|(x, y)| self.at(x, y).is_some())
        };

        let mut rim = Vec::new();

        for y in -1..=size {
            for x in -1..=size {
                let touches =
                    filled(x + 1, y) || filled(x - 1, y) || filled(x, y + 1) || filled(x, y - 1);

                if !filled(x, y) && touches {
                    rim.push((x, y));
                }
            }
        }

        rim
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_body_is_24_by_24() {
        assert_eq!(BODY.len(), 24);

        for (row, line) in BODY.iter().enumerate() {
            assert_eq!(line.chars().count(), 24, "row {row}");
        }
    }

    #[test]
    fn every_letter_has_a_color() {
        let wings = WINGS_UP.iter().chain(&WINGS_DOWN).chain(&CROWN);
        let rows = BODY.iter().copied().chain(wings.map(|(_, line)| *line));

        for line in rows {
            for letter in line.chars().filter(|letter| *letter != '.') {
                assert!(color(letter).is_some(), "no color for {letter:?}");
            }
        }

        assert_eq!(color('.'), None);
        assert_eq!(color('Y'), Some([0xfc, 0xc9, 0x35]));
        assert_eq!(color('O'), Some([0x17, 0x13, 0x0e]));
    }

    #[test]
    fn a_wing_never_covers_the_body() {
        for wings in [Wings::Up, Wings::Down] {
            let frame = Frame::new(wings, false);

            for (row, line) in BODY.iter().enumerate() {
                for (column, letter) in line.chars().enumerate() {
                    if letter != '.' {
                        assert_eq!(frame.at(column, row), color(letter));
                    }
                }
            }
        }

        assert_ne!(Frame::new(Wings::Up, false), Frame::new(Wings::Down, false));
        assert_eq!(Frame::new(Wings::Up, false).at(24, 0), None);
    }

    #[test]
    fn the_wings_are_mirrored() {
        for wings in [Wings::Up, Wings::Down] {
            let frame = Frame::new(wings, true);

            for (y, line) in BODY.iter().enumerate() {
                for x in 0..SIZE / 2 {
                    let (left, right) = (frame.at(x, y), frame.at(SIZE - 1 - x, y));
                    if line.chars().nth(x) == Some('.') {
                        assert_eq!(left, right, "{x},{y}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_crown_adds_pixels_only_when_asked() {
        let plain = Frame::new(Wings::Down, false);
        let crowned = Frame::new(Wings::Down, true);

        assert_eq!(plain.at(CROWN_LEFT + 1, 5), None);
        assert_eq!(crowned.at(CROWN_LEFT + 1, 5), color('O'));
        assert_eq!(crowned.at(CROWN_LEFT + 2, 3), color('L'));
    }

    #[test]
    fn runs_join_neighbors_of_one_color() {
        let frame = Frame::new(Wings::Down, false);
        let row = frame.runs(16);

        let covered: usize = row.iter().map(|(_, length, _)| length).sum();
        let filled = (0..SIZE).filter(|x| frame.at(*x, 16).is_some()).count();

        assert_eq!(covered, filled);
        assert!(
            row.windows(2)
                .all(|pair| { pair[0].2 != pair[1].2 || pair[0].0 + pair[0].1 != pair[1].0 })
        );
    }

    #[test]
    fn the_rim_surrounds_the_sprite_and_touches_it() {
        let frame = Frame::new(Wings::Up, false);
        let rim = frame.rim();

        assert!(rim.contains(&(5, -1)));
        assert!(rim.contains(&(11, 24)));

        for (x, y) in rim {
            let at = |x: i32, y: i32| {
                usize::try_from(x)
                    .ok()
                    .zip(usize::try_from(y).ok())
                    .and_then(|(x, y)| frame.at(x, y))
            };

            assert!(at(x, y).is_none());
            assert!(
                [(1, 0), (-1, 0), (0, 1), (0, -1)]
                    .iter()
                    .any(|(dx, dy)| at(x + dx, y + dy).is_some())
            );
        }
    }
}
