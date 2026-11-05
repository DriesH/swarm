//! The 20 creatures, in the order of the Field guide list, and how each is posed.

#![expect(
    clippy::unreadable_literal,
    reason = "colors are written like in CSS, so they can be checked against the canvas"
)]

use crate::render::{Bands, Ellipse, Line, Oval, Shape, Surface, Wing, hex, render, rounded};
use crate::{Frame, Rgb, Sprite};

const fn palette(darkest: u32, dark: u32, lit: u32, light: u32) -> [Rgb; 4] {
    [hex(darkest), hex(dark), hex(lit), hex(light)]
}

const fn pair(dark: u32, lit: u32) -> [Rgb; 2] {
    [hex(dark), hex(lit)]
}

const BLACK: &[Rgb] = &palette(0x16120f, 0x26211d, 0x38312b, 0x544b43);
const GLOSSY: &[Rgb] = &palette(0x0c0b10, 0x1a1822, 0x2b2838, 0x4f4a66);
const YELLOW: &[Rgb] = &palette(0x7d5a0c, 0xc08f17, 0xecc03a, 0xffe287);
const WASP_YELLOW: &[Rgb] = &palette(0x7a5800, 0xc29800, 0xf2ca18, 0xfff07a);
const BUFF: &[Rgb] = &palette(0x857457, 0xc2ad86, 0xece0c2, 0xfffaf0);
const GREEN: &[Rgb] = &palette(0x0b3322, 0x167043, 0x2fae5f, 0x86e59a);
const GOLD: &[Rgb] = &palette(0x3b3410, 0x6f7a1c, 0x9fbb35, 0xd7f07a);
const AMBER: &[Rgb] = &palette(0x6e3f08, 0xb8700f, 0xeda12c, 0xffd77a);
const HONEY: &[Rgb] = &palette(0x6a3d0a, 0xa8661a, 0xd99a3c, 0xf5cf80);
const BROWN: &[Rgb] = &palette(0x1b110a, 0x341f12, 0x52331f, 0x73503a);
const FUZZ_BROWN: &[Rgb] = &palette(0x4a3214, 0x7a5626, 0xa8803e, 0xd2ac6a);
const GINGER: &[Rgb] = &palette(0x6e2f0f, 0xb0551c, 0xdc8034, 0xf6b46a);
const FOX: &[Rgb] = &palette(0x6b2c0c, 0xad4f17, 0xde7a2c, 0xf7ad62);
const GREY_BROWN: &[Rgb] = &palette(0x1d1a17, 0x36312b, 0x504840, 0x6f655a);
const PALE: &[Rgb] = &palette(0x7d766a, 0xb9b0a0, 0xe3dccd, 0xfbf7ee);
const OLIVE: &[Rgb] = &palette(0x16140f, 0x2b2a22, 0x403e33, 0x5b5848);
const BLUE_BAND: &[Rgb] = &palette(0x0f3550, 0x1f6c9a, 0x3fa6d8, 0x9fdcff);
const TEAL: &[Rgb] = &palette(0x0b2c3c, 0x16607a, 0x2aa3b4, 0x8ff0e6);
const RUBY: &[Rgb] = &palette(0x4a0610, 0x8f1022, 0xd42f3f, 0xff9aa0);
const CHESTNUT: &[Rgb] = &palette(0x2a120a, 0x5a2716, 0x84402a, 0xa8624a);
const RED_ORANGE: &[Rgb] = &palette(0x5c1408, 0xa3270f, 0xd9512a, 0xf58c63);
const WHITE: &[Rgb] = &palette(0x8a8580, 0xc9c4bd, 0xf2eee8, 0xffffff);
const BLUE_BLACK: &[Rgb] = &palette(0x07080f, 0x141a2e, 0x22305a, 0x4a63a8);
const ORANGE_LEG: &[Rgb] = &pair(0x5b3208, 0x9a5c14);
const YELLOW_LEG: &[Rgb] = &pair(0x7a5800, 0xc29800);
const DARK_EYE: &[Rgb] = &palette(0x1a1408, 0x3a2c12, 0x5e4a22, 0x8c7442);
const EYE: &[Rgb] = &palette(0x0a0a0c, 0x16161a, 0x24242a, 0x3a3a44);
const BROWN_EYE: &[Rgb] = &palette(0x2a1606, 0x4a2a0e, 0x6b4318, 0x8f6026);
const LEAF: &[Rgb] = &palette(0x1f4a12, 0x3a7a22, 0x5fae3a, 0xa6dc78);
const SAPPHIRE: &[Rgb] = &palette(0x0d2f6b, 0x1d55b8, 0x3f86f0, 0xa6c8ff);
const DEFAULT_LEGS: &[Rgb] = &pair(0x1d1713, 0x3a312a);
const GLOSSY_ANTENNA: &[Rgb] = &pair(0x0c0b10, 0x2b2838);
const MASON_THORAX: &[Rgb] = &palette(0x2e2a26, 0x4a433c, 0x6b6158, 0x8f8478);
const PAPER_ANTENNA: &[Rgb] = &pair(0x8a4a08, 0xd07a14);
const BLUE_BAND_EYE: &[Rgb] = &palette(0x1c2a1e, 0x2f4a35, 0x4a7055, 0x7ea88a);
const ORCHID_LEG: &[Rgb] = &pair(0x0b2a1c, 0x1e5c3a);
const ORCHID_TONGUE: &[Rgb] = &pair(0x2a1d0c, 0x5a4320);
const RUBY_LEG: &[Rgb] = &pair(0x0b2c3c, 0x16607a);
const HORNET_ANTENNA: &[Rgb] = &pair(0x3a210a, 0x6b4318);
const HAWK_LEG: &[Rgb] = &pair(0x07080f, 0x22305a);
const HAWK_ANTENNA: &[Rgb] = &pair(0x5a2a06, 0xc25f12);
const JAW: &[Rgb] = &pair(0x0c0b0a, 0x2b2622);

/// The ragged fur of most bees.
const FUR: Surface = Surface {
    fur: true,
    fuzz: 0.35,
    metallic: false,
};

const SHINY: Surface = Surface {
    fur: false,
    fuzz: 0.,
    metallic: true,
};

const fn furry(fuzz: f64) -> Surface {
    Surface {
        fur: true,
        fuzz,
        metallic: false,
    }
}

/// A change to the usual place and size of a body part.
#[derive(Clone, Copy, Default)]
struct Tweak {
    cx: Option<f64>,
    cy: Option<f64>,
    rx: Option<f64>,
    ry: Option<f64>,
    rot: Option<f64>,
}

impl Tweak {
    fn on(self, usual: Oval) -> Oval {
        Oval {
            cx: self.cx.unwrap_or(usual.cx),
            cy: self.cy.unwrap_or(usual.cy),
            rx: self.rx.unwrap_or(usual.rx),
            ry: self.ry.unwrap_or(usual.ry),
            rot: self.rot.unwrap_or(usual.rot),
        }
    }
}

/// Where the body parts are, after the tweaks.
struct Parts {
    head: Oval,
    thorax: Oval,
    abdomen: Oval,
    eye: Oval,
}

type Extra = fn(&Parts) -> Vec<Shape>;

/// Everything that makes one creature look like itself.
struct Look {
    head: Tweak,
    thorax: Tweak,
    abdomen: Tweak,
    eye: Tweak,
    head_palette: &'static [Rgb],
    thorax_palette: &'static [Rgb],
    abdomen_palette: &'static [Rgb],
    thorax_bands: Bands,
    abdomen_bands: Bands,
    body: Surface,
    eye_palette: &'static [Rgb],
    legs: &'static [Rgb],
    leg_width: u8,
    long_legs: bool,
    antenna_palette: Option<&'static [Rgb]>,
    antennas: Option<[Vec<(f64, f64)>; 2]>,
    wingless: bool,
    wing_tint: Rgb,
    wing_vein: Rgb,
    wing_alpha: f64,
    wing_scale: f64,
    under: Option<Extra>,
    over: Option<Extra>,
}

impl Default for Look {
    fn default() -> Self {
        Self {
            head: Tweak::default(),
            thorax: Tweak::default(),
            abdomen: Tweak::default(),
            eye: Tweak::default(),
            head_palette: BLACK,
            thorax_palette: BLACK,
            abdomen_palette: BLACK,
            thorax_bands: &[],
            abdomen_bands: &[],
            body: Surface::default(),
            eye_palette: EYE,
            legs: DEFAULT_LEGS,
            leg_width: 2,
            long_legs: false,
            antenna_palette: None,
            antennas: None,
            wingless: false,
            wing_tint: hex(0xc9d6e4),
            wing_vein: hex(0x6d7f93),
            wing_alpha: 0.48,
            wing_scale: 1.,
            under: None,
            over: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pose {
    WingsUp,
    WingsDown,
    Still,
    /// One of the four walking frames.
    Walk(u8),
}

fn tweak() -> Tweak {
    Tweak::default()
}

fn line(points: Vec<(f64, f64)>, palette: &'static [Rgb], width: u8) -> Shape {
    Shape::Line(Line {
        points,
        palette,
        width,
    })
}

fn blob(cx: f64, cy: f64, rx: f64, ry: f64, rot: f64, palette: &'static [Rgb]) -> Ellipse {
    Ellipse {
        oval: Oval {
            cx,
            cy,
            rx,
            ry,
            rot,
        },
        palette,
        bands: &[],
        surface: Surface::default(),
    }
}

impl Look {
    fn parts(&self) -> Parts {
        Parts {
            head: self.head.on(Oval {
                cx: 18.,
                cy: 30.,
                rx: 7.,
                ry: 7.,
                rot: 0.,
            }),
            thorax: self.thorax.on(Oval {
                cx: 30.,
                cy: 30.,
                rx: 9.5,
                ry: 8.5,
                rot: 0.,
            }),
            abdomen: self.abdomen.on(Oval {
                cx: 46.,
                cy: 36.,
                rx: 14.5,
                ry: 10.5,
                rot: 0.32,
            }),
            eye: self.eye.on(Oval {
                cx: 15.,
                cy: 29.,
                rx: 2.7,
                ry: 3.7,
                rot: 0.,
            }),
        }
    }

    /// Three legs: a hip, a knee and a foot. Walking moves them in two alternating sets.
    fn legs(&self, parts: &Parts, pose: Pose) -> Vec<Shape> {
        let flying = matches!(pose, Pose::WingsUp | Pose::WingsDown);
        let reach = if self.long_legs { 1.35 } else { 1. };

        let bottom = parts.thorax.cy + parts.thorax.ry;
        let spread = parts.thorax.rx * 0.5;
        let hips = [
            (parts.thorax.cx - spread, bottom - 2.),
            (parts.thorax.cx, bottom - 1.),
            (parts.thorax.cx + spread, bottom - 2.),
        ];
        let knees = [(-4., 8.), (0., 9.), (5., 8.)];
        let feet = [(-7., 12.), (-2., 14.), (8., 12.)];

        let mut legs = Vec::new();

        for (leg, (&(hx, hy), (&knee, &foot))) in
            hips.iter().zip(knees.iter().zip(feet.iter())).enumerate()
        {
            let points = if flying {
                vec![
                    (hx, hy),
                    (hx + knee.0 * 0.5 + 2., hy + knee.1 * 0.75),
                    (hx + foot.0 * 0.4 + 4., hy + foot.1 * 0.85),
                ]
            } else {
                let (shift, lift) = match pose {
                    Pose::Walk(frame) => {
                        let group_a = leg != 1;
                        let phase = (usize::from(frame) + if group_a { 0 } else { 2 }) % 4;

                        (
                            [-3., 0., 3., 0.][phase],
                            if phase == 3 || phase == 1 { -2. } else { 0. },
                        )
                    }
                    _ => (0., 0.),
                };

                vec![
                    (hx, hy),
                    (hx + knee.0 * reach + shift / 2., hy + knee.1 * reach + lift),
                    (hx + foot.0 * reach + shift, hy + foot.1 * reach + lift),
                ]
            };

            legs.push(line(points, self.legs, self.leg_width));
        }

        legs
    }

    /// The hind wing and the fore wing. A folded wing lies along the abdomen.
    fn wings(&self, parts: &Parts, pose: Pose) -> Option<(Shape, Shape)> {
        if self.wingless {
            return None;
        }

        let scale = self.wing_scale;
        let wing = |cx: f64, cy: f64, rx: f64, ry: f64, rot: f64, alpha: f64| {
            Shape::Wing(Wing {
                oval: Oval {
                    cx,
                    cy,
                    rx,
                    ry,
                    rot,
                },
                tint: self.wing_tint,
                alpha,
                vein: self.wing_vein,
            })
        };
        let (thorax, abdomen) = (&parts.thorax, &parts.abdomen);
        let alpha = self.wing_alpha;

        Some(match pose {
            Pose::WingsUp => (
                wing(
                    thorax.cx + 9.,
                    thorax.cy - 15.,
                    12. * scale,
                    5.,
                    -0.95,
                    alpha,
                ),
                wing(
                    thorax.cx + 11.,
                    thorax.cy - 12.,
                    15. * scale,
                    6.,
                    -0.7,
                    alpha * 0.9,
                ),
            ),
            Pose::WingsDown | Pose::Still => (
                wing(
                    thorax.cx + 10.,
                    thorax.cy - 11.,
                    12. * scale,
                    5.,
                    -0.35,
                    alpha,
                ),
                wing(
                    thorax.cx + 13.,
                    thorax.cy - 8.,
                    15. * scale,
                    6.,
                    -0.15,
                    alpha * 0.9,
                ),
            ),
            Pose::Walk(_) => (
                wing(
                    abdomen.cx - 2.,
                    abdomen.cy - 8.,
                    13. * scale,
                    4.5,
                    0.2,
                    alpha,
                ),
                wing(
                    abdomen.cx,
                    abdomen.cy - 7.,
                    15. * scale,
                    5.,
                    0.28,
                    alpha * 0.9,
                ),
            ),
        })
    }

    fn antennas(&self, parts: &Parts) -> [Vec<(f64, f64)>; 2] {
        self.antennas.clone().unwrap_or_else(|| {
            let (x, y) = (parts.head.cx, parts.head.cy);

            [
                vec![(x - 4., y - 6.), (x - 8., y - 13.), (x - 12., y - 15.)],
                vec![(x, y - 7.), (x - 2., y - 15.), (x - 6., y - 19.)],
            ]
        })
    }

    /// One frame of the creature in a pose.
    fn frame(&self, pose: Pose) -> Frame {
        let parts = self.parts();
        let wings = self.wings(&parts, pose);
        let body = self.body;

        let part = |oval: Oval, palette: &'static [Rgb], bands: Bands, surface: Surface| {
            Shape::Ellipse(Ellipse {
                oval,
                palette,
                bands,
                surface,
            })
        };

        let mut shapes = self.legs(&parts, pose);

        if let Some(under) = self.under {
            shapes.extend(under(&parts));
        }
        if let Some((hind, _)) = &wings {
            shapes.push(hind.clone());
        }

        shapes.push(part(
            parts.abdomen,
            self.abdomen_palette,
            self.abdomen_bands,
            body,
        ));
        shapes.push(part(
            parts.thorax,
            self.thorax_palette,
            self.thorax_bands,
            body,
        ));
        shapes.push(part(
            parts.head,
            self.head_palette,
            &[],
            Surface {
                fuzz: body.fuzz * 0.7,
                ..body
            },
        ));
        shapes.push(part(parts.eye, self.eye_palette, &[], Surface::default()));

        if let Some(over) = self.over {
            shapes.extend(over(&parts));
        }
        if let Some((_, fore)) = &wings {
            shapes.push(fore.clone());
        }

        let antenna_palette = self.antenna_palette.unwrap_or(self.legs);
        for points in self.antennas(&parts) {
            shapes.push(line(points, antenna_palette, 1));
        }

        let raise = if pose == Pose::WingsUp { -1. } else { 0. };
        let lowered: Vec<Shape> = shapes.iter().map(|shape| shape.lowered(raise)).collect();
        let glint = (
            rounded(parts.eye.cx - 1.),
            rounded(parts.eye.cy - parts.eye.ry + 1.) + if raise < 0. { -1 } else { 0 },
        );

        render(&lowered, glint)
    }

    fn sprite(&self) -> Sprite {
        Sprite {
            fly: (!self.wingless).then(|| {
                [Pose::WingsUp, Pose::WingsDown]
                    .map(|pose| self.frame(pose))
                    .to_vec()
            }),
            crawl: (0..4).map(|walk| self.frame(Pose::Walk(walk))).collect(),
            still: vec![self.frame(Pose::Still)],
        }
    }
}

/// Draws the creature at this place in the list.
pub(crate) fn draw(index: usize) -> Sprite {
    looks()
        .into_iter()
        .nth(index)
        .map_or_else(|| Look::default().sprite(), |look| look.sprite())
}

#[expect(
    clippy::too_many_lines,
    reason = "one entry for each creature, in the order of the list"
)]
fn looks() -> Vec<Look> {
    vec![
        // Western honey bee
        Look {
            head_palette: BROWN,
            thorax_palette: FUZZ_BROWN,
            abdomen_palette: HONEY,
            body: furry(0.2),
            abdomen: Tweak {
                rx: Some(13.5),
                ry: Some(9.),
                ..tweak()
            },
            abdomen_bands: &[
                (-1., HONEY),
                (-0.32, BROWN),
                (-0.12, HONEY),
                (0.18, BROWN),
                (0.38, HONEY),
                (0.6, BROWN),
            ],
            ..Look::default()
        },
        // Honey bee drone
        Look {
            head_palette: BROWN,
            thorax_palette: FUZZ_BROWN,
            abdomen_palette: BROWN,
            body: furry(0.25),
            eye: Tweak {
                cx: Some(17.),
                cy: Some(27.),
                rx: Some(5.),
                ry: Some(5.8),
                ..tweak()
            },
            abdomen: Tweak {
                rx: Some(13.),
                ry: Some(11.),
                rot: Some(0.25),
                ..tweak()
            },
            abdomen_bands: &[(-1., HONEY), (-0.3, BROWN), (-0.05, HONEY), (0.2, BROWN)],
            ..Look::default()
        },
        // Buff-tailed bumblebee
        Look {
            head_palette: BLACK,
            thorax_palette: BLACK,
            abdomen_palette: BLACK,
            body: FUR,
            abdomen: Tweak {
                rx: Some(15.),
                ry: Some(11.),
                ..tweak()
            },
            thorax: Tweak {
                rx: Some(10.),
                ry: Some(9.),
                ..tweak()
            },
            thorax_bands: &[(-1., YELLOW), (-0.45, BLACK)],
            abdomen_bands: &[(-1., BLACK), (-0.55, YELLOW), (-0.2, BLACK), (0.42, BUFF)],
            ..Look::default()
        },
        // Violet carpenter bee
        Look {
            head_palette: GLOSSY,
            thorax_palette: GLOSSY,
            abdomen_palette: GLOSSY,
            body: SHINY,
            abdomen: Tweak {
                rx: Some(15.),
                ry: Some(10.5),
                ..tweak()
            },
            thorax: Tweak {
                rx: Some(10.),
                ry: Some(9.),
                ..tweak()
            },
            wing_tint: hex(0x3e2f6e),
            wing_alpha: 0.72,
            wing_vein: hex(0x1d153a),
            antenna_palette: Some(GLOSSY_ANTENNA),
            ..Look::default()
        },
        // Red mason bee
        Look {
            head_palette: BLACK,
            thorax_palette: MASON_THORAX,
            abdomen_palette: GINGER,
            body: FUR,
            abdomen: Tweak {
                rx: Some(13.),
                ry: Some(9.5),
                ..tweak()
            },
            over: Some(|parts| {
                let (x, y) = (parts.head.cx, parts.head.cy);

                vec![
                    line(vec![(x - 6., y + 2.), (x - 9., y + 1.)], BLACK, 1),
                    line(vec![(x - 6., y + 4.), (x - 9., y + 4.)], BLACK, 1),
                ]
            }),
            ..Look::default()
        },
        // Patchwork leafcutter bee
        Look {
            head_palette: GREY_BROWN,
            thorax_palette: GREY_BROWN,
            abdomen_palette: GREY_BROWN,
            body: furry(0.15),
            abdomen: Tweak {
                rx: Some(13.5),
                ry: Some(9.),
                ..tweak()
            },
            abdomen_bands: &[
                (-1., GREY_BROWN),
                (-0.25, PALE),
                (-0.15, GREY_BROWN),
                (0.15, PALE),
                (0.25, GREY_BROWN),
                (0.52, PALE),
                (0.6, GREY_BROWN),
            ],
            under: Some(|parts| {
                vec![Shape::Ellipse(blob(
                    parts.thorax.cx + 2.,
                    parts.thorax.cy + 12.,
                    9.,
                    4.,
                    -0.1,
                    LEAF,
                ))]
            }),
            over: Some(|parts| {
                let (x, y) = (parts.head.cx, parts.head.cy);

                vec![line(
                    vec![(x - 6., y + 3.), (x - 10., y + 5.)],
                    GREY_BROWN,
                    2,
                )]
            }),
            ..Look::default()
        },
        // Orange-legged furrow bee
        Look {
            head_palette: OLIVE,
            thorax_palette: OLIVE,
            abdomen_palette: OLIVE,
            body: furry(0.12),
            legs: ORANGE_LEG,
            abdomen: Tweak {
                rx: Some(13.),
                ry: Some(8.5),
                rot: Some(0.3),
                ..tweak()
            },
            abdomen_bands: &[
                (-1., OLIVE),
                (-0.3, PALE),
                (-0.22, OLIVE),
                (0.05, PALE),
                (0.13, OLIVE),
                (0.4, PALE),
                (0.48, OLIVE),
            ],
            ..Look::default()
        },
        // Tawny mining bee
        Look {
            head_palette: BLACK,
            thorax_palette: FOX,
            abdomen_palette: FOX,
            body: FUR,
            abdomen: Tweak {
                rx: Some(14.),
                ry: Some(10.),
                ..tweak()
            },
            ..Look::default()
        },
        // Common wasp
        Look {
            head_palette: WASP_YELLOW,
            thorax_palette: BLACK,
            abdomen_palette: WASP_YELLOW,
            legs: YELLOW_LEG,
            eye: Tweak {
                cx: Some(15.),
                cy: Some(28.),
                rx: Some(2.6),
                ry: Some(4.2),
                ..tweak()
            },
            abdomen: Tweak {
                cx: Some(48.),
                cy: Some(37.),
                rx: Some(13.5),
                ry: Some(9.5),
                ..tweak()
            },
            thorax_bands: &[(-1., WASP_YELLOW), (-0.75, BLACK)],
            abdomen_bands: &[
                (-1., BLACK),
                (-0.55, WASP_YELLOW),
                (-0.32, BLACK),
                (-0.12, WASP_YELLOW),
                (0.08, BLACK),
                (0.3, WASP_YELLOW),
                (0.5, BLACK),
                (0.72, WASP_YELLOW),
            ],
            under: Some(|parts| {
                vec![line(
                    vec![
                        (parts.thorax.cx + 8., parts.thorax.cy + 2.),
                        (parts.abdomen.cx - 12., parts.abdomen.cy - 2.),
                    ],
                    BLACK,
                    2,
                )]
            }),
            antenna_palette: Some(BLACK),
            wing_tint: hex(0xd8cfa8),
            wing_vein: hex(0x7a6a3e),
            ..Look::default()
        },
        // European paper wasp
        Look {
            head_palette: WASP_YELLOW,
            thorax_palette: BLACK,
            abdomen_palette: WASP_YELLOW,
            legs: YELLOW_LEG,
            long_legs: true,
            eye: Tweak {
                cx: Some(15.),
                cy: Some(28.),
                rx: Some(2.4),
                ry: Some(4.),
                ..tweak()
            },
            abdomen: Tweak {
                cx: Some(49.),
                cy: Some(37.),
                rx: Some(14.),
                ry: Some(8.),
                rot: Some(0.3),
            },
            abdomen_bands: &[
                (-1., BLACK),
                (-0.6, WASP_YELLOW),
                (-0.38, BLACK),
                (-0.25, WASP_YELLOW),
                (0.02, BLACK),
                (0.14, WASP_YELLOW),
                (0.4, BLACK),
                (0.5, WASP_YELLOW),
            ],
            under: Some(|parts| {
                vec![line(
                    vec![
                        (parts.thorax.cx + 8., parts.thorax.cy + 2.),
                        (parts.abdomen.cx - 13., parts.abdomen.cy - 2.),
                    ],
                    BLACK,
                    2,
                )]
            }),
            antenna_palette: Some(PAPER_ANTENNA),
            wing_tint: hex(0xd8c49a),
            wing_vein: hex(0x7a5a2e),
            ..Look::default()
        },
        // Blue-banded bee
        Look {
            head_palette: BLACK,
            thorax_palette: FUZZ_BROWN,
            abdomen_palette: BLACK,
            body: furry(0.2),
            abdomen_bands: &[
                (-1., BLACK),
                (-0.42, BLUE_BAND),
                (-0.28, BLACK),
                (-0.02, BLUE_BAND),
                (0.12, BLACK),
                (0.38, BLUE_BAND),
                (0.52, BLACK),
            ],
            eye: Tweak {
                rx: Some(3.),
                ry: Some(4.2),
                ..tweak()
            },
            eye_palette: BLUE_BAND_EYE,
            ..Look::default()
        },
        // Green orchid bee
        Look {
            head_palette: GREEN,
            thorax_palette: GREEN,
            abdomen_palette: GREEN,
            body: SHINY,
            abdomen: Tweak {
                rx: Some(13.),
                ry: Some(8.5),
                rot: Some(0.3),
                ..tweak()
            },
            thorax: Tweak {
                rx: Some(9.),
                ry: Some(8.),
                ..tweak()
            },
            eye: Tweak {
                cx: Some(16.),
                cy: Some(28.),
                rx: Some(3.6),
                ry: Some(4.6),
                ..tweak()
            },
            eye_palette: DARK_EYE,
            abdomen_bands: &[(-1., GREEN), (0.35, GOLD)],
            legs: ORCHID_LEG,
            leg_width: 1,
            antenna_palette: Some(ORCHID_LEG),
            wing_tint: hex(0xe6d9b8),
            wing_alpha: 0.38,
            wing_vein: hex(0x8a7a55),
            over: Some(|parts| {
                let (x, y) = (parts.head.cx, parts.head.cy);

                vec![line(
                    vec![(x - 4., y + 5.), (x - 8., y + 13.), (x - 9., y + 18.)],
                    ORCHID_TONGUE,
                    1,
                )]
            }),
            ..Look::default()
        },
        // Ruby-tailed wasp
        Look {
            head_palette: TEAL,
            thorax_palette: TEAL,
            abdomen_palette: RUBY,
            body: SHINY,
            head: Tweak {
                cx: Some(20.),
                rx: Some(6.),
                ry: Some(6.),
                ..tweak()
            },
            thorax: Tweak {
                cx: Some(30.),
                rx: Some(8.5),
                ry: Some(7.5),
                ..tweak()
            },
            abdomen: Tweak {
                cx: Some(43.),
                cy: Some(35.),
                rx: Some(10.5),
                ry: Some(8.5),
                rot: Some(0.25),
            },
            eye: Tweak {
                cx: Some(18.),
                cy: Some(29.),
                rx: Some(2.4),
                ry: Some(3.2),
                ..tweak()
            },
            legs: RUBY_LEG,
            leg_width: 1,
            wing_scale: 0.75,
            wing_tint: hex(0xcfd8dc),
            wing_vein: hex(0x6c7a80),
            antenna_palette: Some(RUBY_LEG),
            ..Look::default()
        },
        // Black-and-yellow mud dauber
        Look {
            head_palette: BLACK,
            thorax_palette: BLACK,
            abdomen_palette: BLACK,
            legs: YELLOW_LEG,
            leg_width: 1,
            long_legs: true,
            thorax: Tweak {
                rx: Some(8.5),
                ry: Some(7.5),
                ..tweak()
            },
            abdomen: Tweak {
                cx: Some(52.),
                cy: Some(40.),
                rx: Some(9.5),
                ry: Some(7.),
                rot: Some(0.35),
            },
            abdomen_bands: &[(-1., BLACK), (0.55, WASP_YELLOW)],
            under: Some(|parts| {
                vec![line(
                    vec![
                        (parts.thorax.cx + 7., parts.thorax.cy + 3.),
                        (parts.abdomen.cx - 6., parts.abdomen.cy - 3.),
                    ],
                    WASP_YELLOW,
                    1,
                )]
            }),
            over: Some(|parts| {
                vec![Shape::Ellipse(blob(
                    parts.thorax.cx - 2.,
                    parts.thorax.cy - 5.,
                    2.5,
                    1.4,
                    0.,
                    WASP_YELLOW,
                ))]
            }),
            antenna_palette: Some(BLACK),
            wing_tint: hex(0xb9a77a),
            wing_alpha: 0.5,
            wing_vein: hex(0x5e4e2a),
            ..Look::default()
        },
        // European hornet
        Look {
            head_palette: AMBER,
            thorax_palette: CHESTNUT,
            abdomen_palette: WASP_YELLOW,
            legs: ORANGE_LEG,
            head: Tweak {
                cx: Some(17.),
                rx: Some(8.5),
                ry: Some(8.),
                ..tweak()
            },
            thorax: Tweak {
                rx: Some(10.),
                ry: Some(9.),
                ..tweak()
            },
            abdomen: Tweak {
                rx: Some(15.5),
                ry: Some(10.5),
                rot: Some(0.35),
                ..tweak()
            },
            eye: Tweak {
                cx: Some(13.),
                cy: Some(28.),
                rx: Some(2.8),
                ry: Some(4.2),
                ..tweak()
            },
            eye_palette: BROWN_EYE,
            abdomen_bands: &[
                (-1., CHESTNUT),
                (-0.45, WASP_YELLOW),
                (-0.25, CHESTNUT),
                (-0.12, WASP_YELLOW),
                (0.12, CHESTNUT),
                (0.2, WASP_YELLOW),
            ],
            wing_tint: hex(0xc9a46a),
            wing_alpha: 0.4,
            wing_vein: hex(0x6b4a1e),
            antenna_palette: Some(HORNET_ANTENNA),
            ..Look::default()
        },
        // Large velvet ant
        Look {
            wingless: true,
            head_palette: BLACK,
            thorax_palette: RED_ORANGE,
            abdomen_palette: BLACK,
            body: FUR,
            head: Tweak {
                cx: Some(18.),
                cy: Some(32.),
                rx: Some(6.),
                ry: Some(6.),
                ..tweak()
            },
            thorax: Tweak {
                cx: Some(28.),
                cy: Some(32.),
                rx: Some(7.),
                ry: Some(6.),
                ..tweak()
            },
            abdomen: Tweak {
                cx: Some(44.),
                cy: Some(34.),
                rx: Some(13.),
                ry: Some(10.5),
                rot: Some(0.15),
            },
            eye: Tweak {
                cx: Some(15.),
                cy: Some(31.),
                rx: Some(1.8),
                ry: Some(2.4),
                ..tweak()
            },
            abdomen_bands: &[
                (-1., BLACK),
                (-0.3, WHITE),
                (-0.18, BLACK),
                (0.25, WHITE),
                (0.35, BLACK),
            ],
            over: Some(|parts| {
                let (x, y) = (parts.abdomen.cx, parts.abdomen.cy);

                vec![
                    Shape::Ellipse(blob(x - 2., y - 6., 2.2, 1.8, 0., WHITE)),
                    Shape::Ellipse(blob(x + 4., y - 5., 2.2, 1.8, 0., WHITE)),
                ]
            }),
            ..Look::default()
        },
        // Queen bee
        Look {
            head_palette: BROWN,
            thorax_palette: FUZZ_BROWN,
            abdomen_palette: HONEY,
            body: furry(0.15),
            abdomen: Tweak {
                cx: Some(49.),
                cy: Some(38.),
                rx: Some(18.5),
                ry: Some(9.),
                rot: Some(0.3),
            },
            wing_scale: 0.85,
            abdomen_bands: &[
                (-1., HONEY),
                (-0.4, BROWN),
                (-0.28, HONEY),
                (-0.05, BROWN),
                (0.07, HONEY),
                (0.3, BROWN),
                (0.42, HONEY),
                (0.65, BROWN),
            ],
            over: Some(|parts| {
                let mut gem = blob(
                    parts.thorax.cx + 1.,
                    parts.thorax.cy - 5.,
                    2.4,
                    2.,
                    0.,
                    SAPPHIRE,
                );
                gem.surface.metallic = true;

                vec![Shape::Ellipse(gem)]
            }),
            ..Look::default()
        },
        // Asian giant hornet
        Look {
            head_palette: AMBER,
            thorax_palette: BROWN,
            abdomen_palette: BROWN,
            legs: ORANGE_LEG,
            head: Tweak {
                cx: Some(17.),
                rx: Some(9.),
                ry: Some(8.5),
                ..tweak()
            },
            thorax: Tweak {
                rx: Some(10.),
                ry: Some(9.),
                ..tweak()
            },
            abdomen: Tweak {
                rx: Some(16.),
                ry: Some(10.5),
                rot: Some(0.35),
                ..tweak()
            },
            eye: Tweak {
                cx: Some(13.),
                cy: Some(28.),
                rx: Some(2.8),
                ry: Some(4.2),
                ..tweak()
            },
            eye_palette: BROWN_EYE,
            thorax_bands: &[(-1., BROWN), (0.55, AMBER)],
            abdomen_bands: &[
                (-1., BROWN),
                (-0.5, AMBER),
                (-0.28, BROWN),
                (0., AMBER),
                (0.22, BROWN),
                (0.42, AMBER),
                (0.68, BROWN),
            ],
            wing_tint: hex(0xc99a52),
            wing_alpha: 0.42,
            wing_vein: hex(0x6b4a1e),
            antenna_palette: Some(HORNET_ANTENNA),
            over: Some(|parts| {
                vec![Shape::Ellipse(blob(
                    parts.head.cx - 6.,
                    parts.head.cy + 5.,
                    3.,
                    2.,
                    0.,
                    BROWN_EYE,
                ))]
            }),
            ..Look::default()
        },
        // Tarantula hawk
        Look {
            head_palette: BLUE_BLACK,
            thorax_palette: BLUE_BLACK,
            abdomen_palette: BLUE_BLACK,
            body: SHINY,
            legs: HAWK_LEG,
            long_legs: true,
            abdomen: Tweak {
                cx: Some(48.),
                cy: Some(37.),
                rx: Some(14.),
                ry: Some(8.5),
                rot: Some(0.3),
            },
            wing_tint: hex(0xe8761a),
            wing_alpha: 0.82,
            wing_vein: hex(0x7a3608),
            wing_scale: 1.1,
            antenna_palette: Some(HAWK_ANTENNA),
            antennas: Some([
                vec![(14., 24.), (9., 18.), (5., 18.), (3., 21.)],
                vec![(18., 23.), (15., 14.), (10., 11.), (7., 13.)],
            ]),
            under: Some(|parts| {
                vec![line(
                    vec![
                        (parts.thorax.cx + 8., parts.thorax.cy + 2.),
                        (parts.abdomen.cx - 12., parts.abdomen.cy - 2.),
                    ],
                    HAWK_LEG,
                    2,
                )]
            }),
            ..Look::default()
        },
        // Wallace's giant bee
        Look {
            head_palette: BLACK,
            thorax_palette: BLACK,
            abdomen_palette: BLACK,
            body: furry(0.12),
            head: Tweak {
                cx: Some(17.),
                rx: Some(8.5),
                ry: Some(8.),
                ..tweak()
            },
            thorax: Tweak {
                rx: Some(10.),
                ry: Some(9.),
                ..tweak()
            },
            abdomen: Tweak {
                rx: Some(16.),
                ry: Some(11.),
                rot: Some(0.3),
                ..tweak()
            },
            eye: Tweak {
                cx: Some(15.),
                cy: Some(27.),
                rx: Some(2.4),
                ry: Some(3.4),
                ..tweak()
            },
            wing_tint: hex(0x9a8f86),
            wing_alpha: 0.55,
            wing_vein: hex(0x3c342e),
            abdomen_bands: &[(-1., PALE), (-0.82, BLACK)],
            over: Some(|parts| {
                let (x, y) = (parts.head.cx, parts.head.cy);
                let jaw = JAW;

                vec![
                    line(
                        vec![(x - 6., y + 2.), (x - 12., y + 2.), (x - 14., y - 1.)],
                        jaw,
                        2,
                    ),
                    line(
                        vec![(x - 6., y + 5.), (x - 11., y + 7.), (x - 14., y + 5.)],
                        jaw,
                        2,
                    ),
                ]
            }),
            ..Look::default()
        },
    ]
}
