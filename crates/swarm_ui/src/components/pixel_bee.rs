use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, ElementId, Hsla, Pixels, Point, Window, canvas,
    ease_in_out, fill, point, px, size,
};
use swarm_sprite::{Frame, RIM_DARK, Rgb, SIZE, Wings};

use crate::component::{Component, example, example_group};
use crate::motion::reduce_motion;
use crate::prelude::*;
use crate::theme::ThemeKind;

/// One flap, both wing frames: each frame shows for half of it.
const FLAP_PERIOD: Duration = Duration::from_millis(260);

/// The ring around the sprite is one sprite pixel wide on every side.
const MARGIN: usize = 1;

/// The pixel bee: a 24 by 24 sprite drawn with quads. Each sprite pixel is a whole number of
/// logical pixels, and its edges sit on device pixels, so the bee stays sharp on Retina.
///
/// It is decoration. It has no role and no label, so VoiceOver skips it. Flapping needs
/// `flap(true)` and stops under Reduce Motion, which leaves the wings down.
#[derive(IntoElement)]
#[must_use]
pub struct PixelBee {
    id: ElementId,
    scale: u8,
    wings: Wings,
    crown: bool,
    flap: bool,
}

impl PixelBee {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            scale: 2,
            wings: Wings::Down,
            crown: false,
            flap: false,
        }
    }

    /// Logical pixels for each sprite pixel.
    pub fn scale(mut self, scale: u8) -> Self {
        self.scale = scale.max(1);
        self
    }

    /// The wings of a bee that does not flap.
    pub fn wings(mut self, wings: Wings) -> Self {
        self.wings = wings;
        self
    }

    pub fn crown(mut self, crown: bool) -> Self {
        self.crown = crown;
        self
    }

    pub fn flap(mut self, flap: bool) -> Self {
        self.flap = flap;
        self
    }

    /// The width and the height of the bee with its ring, in logical pixels.
    pub fn side(scale: u8) -> Pixels {
        px(f32::from(scale) * count(SIZE + 2 * MARGIN))
    }

    /// True when the wings move: asked for, and Reduce Motion is off.
    pub fn flaps(&self, cx: &App) -> bool {
        self.flap && !reduce_motion(cx)
    }
}

impl RenderOnce for PixelBee {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let sprite = BeeSprite {
            unit: f32::from(self.scale),
            wings: self.wings,
            crown: self.crown,
            dark: cx.theme().kind() == ThemeKind::Dark,
        };

        if self.flaps(cx) {
            sprite
                .with_animation(
                    self.id,
                    Animation::new(FLAP_PERIOD).repeat(),
                    |sprite, delta| BeeSprite {
                        wings: wings_at(delta),
                        ..sprite
                    },
                )
                .into_any_element()
        } else {
            sprite.into_any_element()
        }
    }
}

/// The wing frame at a point of the flap, from 0 to 1.
fn wings_at(delta: f32) -> Wings {
    if delta < 0.5 { Wings::Down } else { Wings::Up }
}

/// A small count as a float. A sprite is a few dozen pixels across.
fn count(value: usize) -> f32 {
    f32::from(u16::try_from(value).unwrap_or(u16::MAX))
}

fn offset(value: i32) -> f32 {
    f32::from(i16::try_from(value).unwrap_or(0))
}

fn paint_color(rgb: Rgb, opacity: f32) -> Hsla {
    let [red, green, blue] = rgb;
    let packed = (u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue);

    Hsla::from(gpui::rgb(packed)).opacity(opacity)
}

/// Paints one frame of the bee. `origin` is the corner of the ring around the sprite, and
/// each sprite pixel is `unit` logical pixels. Every edge is snapped to a device pixel, and
/// neighbors of one color are one quad, so no seams show.
pub(crate) fn paint_bee(
    window: &mut Window,
    origin: Point<Pixels>,
    unit: f32,
    wings: Wings,
    crown: bool,
    dark: bool,
    opacity: f32,
) {
    let frame = Frame::new(wings, crown);
    let factor = window.scale_factor();
    let snap = |value: f32| (value * factor).round() / factor;
    let (left, top) = (f32::from(origin.x), f32::from(origin.y));

    let mut quad = |column: i32, row: i32, length: usize, rgb: Rgb| {
        let start = offset(column) + count(MARGIN);
        let end = start + count(length);
        let (top_edge, bottom_edge) = (
            offset(row) + count(MARGIN),
            offset(row) + 1. + count(MARGIN),
        );

        let bounds = gpui::Bounds::new(
            point(
                px(snap(left + start * unit)),
                px(snap(top + top_edge * unit)),
            ),
            size(
                px(snap(left + end * unit) - snap(left + start * unit)),
                px(snap(top + bottom_edge * unit) - snap(top + top_edge * unit)),
            ),
        );

        window.paint_quad(fill(bounds, paint_color(rgb, opacity)));
    };

    if dark {
        for (column, row, length) in rim_runs(&frame) {
            quad(column, row, length, RIM_DARK);
        }
    }

    for row in 0..SIZE {
        for (column, length, rgb) in frame.runs(row) {
            let (column, row) = (offset_of(column), offset_of(row));
            quad(column, row, length, rgb);
        }
    }
}

fn offset_of(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(0)
}

/// The ring around the sprite as runs: where each starts, its row and its length.
fn rim_runs(frame: &Frame) -> Vec<(i32, i32, usize)> {
    let mut runs: Vec<(i32, i32, usize)> = Vec::new();

    for (x, y) in frame.rim() {
        match runs.last_mut() {
            Some((start, row, length)) if *row == y && *start + offset_of(*length) == x => {
                *length += 1;
            }
            _ => runs.push((x, y, 1)),
        }
    }

    runs
}

/// The canvas of one frame.
#[derive(IntoElement)]
struct BeeSprite {
    unit: f32,
    wings: Wings,
    crown: bool,
    dark: bool,
}

impl RenderOnce for BeeSprite {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let Self {
            unit,
            wings,
            crown,
            dark,
        } = self;
        let side = px(unit * count(SIZE + 2 * MARGIN));

        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                paint_bee(window, bounds.origin, unit, wings, crown, dark, 1.);
            },
        )
        .size(side)
        .flex_none()
    }
}

/// Bees that fly across the window once. Each follows its own curve from the left edge to the
/// right edge and flaps on the way. Under Reduce Motion nothing shows.
#[derive(IntoElement)]
#[must_use]
pub struct BeeFlight {
    progress: f32,
    scale: u8,
}

/// How long the whole flight takes, from the first bee leaving to the last bee gone.
pub const FLIGHT_DURATION: Duration = Duration::from_millis(2960);

/// How long one bee flies, and how long each bee waits for the one before it.
const BEE_SECONDS: f32 = 2.4;
const BEE_DELAY_SECONDS: f32 = 0.14;

/// One curve for each bee: a start, two controls and the middle point of two cubic segments,
/// then the controls and the end of the second. Fractions of the window's width and height.
const CURVES: [[(f32, f32); 6]; 5] = [
    [
        (-0.075, 0.77),
        (0.15, 0.23),
        (0.4, 1.0),
        (0.625, 0.46),
        (0.825, 0.15),
        (1.0, 0.35),
    ],
    [
        (-0.075, 0.46),
        (0.2, 0.85),
        (0.375, 0.12),
        (0.6, 0.58),
        (0.8, 0.88),
        (1.0, 0.62),
    ],
    [
        (-0.075, 0.23),
        (0.175, 0.54),
        (0.425, 0.0),
        (0.65, 0.31),
        (0.825, 0.62),
        (1.0, 0.15),
    ],
    [
        (-0.075, 0.92),
        (0.25, 0.62),
        (0.35, 1.0),
        (0.55, 0.77),
        (0.8, 0.42),
        (1.0, 0.85),
    ],
    [
        (-0.075, 0.62),
        (0.15, 0.38),
        (0.5, 0.77),
        (0.7, 0.42),
        (0.85, 0.27),
        (1.0, 0.5),
    ],
];

impl BeeFlight {
    pub fn new() -> Self {
        Self {
            progress: 0.,
            scale: 3,
        }
    }

    /// How far the flight is, from 0 to 1 over [`FLIGHT_DURATION`].
    pub fn progress(mut self, progress: f32) -> Self {
        self.progress = progress.clamp(0., 1.);
        self
    }

    /// Logical pixels for each sprite pixel.
    pub fn scale(mut self, scale: u8) -> Self {
        self.scale = scale.max(1);
        self
    }

    /// True when the bees show. Under Reduce Motion they do not: they have no still frame.
    pub fn flies(cx: &App) -> bool {
        !reduce_motion(cx)
    }

    /// How many bees fly.
    pub fn bees() -> usize {
        CURVES.len()
    }

    /// Where bee `index` is at `progress` of the flight. `None` while it waits or after it
    /// landed.
    pub fn bee_at(index: usize, progress: f32) -> Option<Flyer> {
        let curve = CURVES.get(index)?;
        let total = FLIGHT_DURATION.as_secs_f32();
        let elapsed = progress.clamp(0., 1.) * total;
        let started = elapsed - count(index) * BEE_DELAY_SECONDS;

        if started < 0. || started > BEE_SECONDS {
            return None;
        }

        let time = started / BEE_SECONDS;
        let (x, y) = along(curve, ease_in_out(time));
        let opacity = (time / 0.08).min(1.).min(((1. - time) / 0.1).min(1.));
        let period = FLAP_PERIOD.as_secs_f32();
        let wings = wings_at(started.rem_euclid(period) / period);

        Some(Flyer {
            x,
            y,
            opacity,
            wings,
        })
    }
}

/// A bee in the air.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Flyer {
    /// Fractions of the window's width and height.
    pub x: f32,
    pub y: f32,
    pub opacity: f32,
    pub wings: Wings,
}

impl Default for BeeFlight {
    fn default() -> Self {
        Self::new()
    }
}

/// A point on the two cubic segments of `curve`, at `time` from 0 to 1. The second segment
/// starts smooth: its first control is the first segment's last control, mirrored.
fn along(curve: &[(f32, f32); 6], time: f32) -> (f32, f32) {
    let [start, control_a, control_b, middle, control_c, end] = *curve;
    let mirrored = (2. * middle.0 - control_b.0, 2. * middle.1 - control_b.1);

    if time < 0.5 {
        cubic([start, control_a, control_b, middle], time * 2.)
    } else {
        cubic([middle, mirrored, control_c, end], time * 2. - 1.)
    }
}

fn cubic(points: [(f32, f32); 4], time: f32) -> (f32, f32) {
    let rest = 1. - time;
    let weights = [
        rest * rest * rest,
        3. * rest * rest * time,
        3. * rest * time * time,
        time * time * time,
    ];

    points
        .iter()
        .zip(weights)
        .fold((0., 0.), |sum, (point, weight)| {
            (sum.0 + point.0 * weight, sum.1 + point.1 * weight)
        })
}

impl RenderOnce for BeeFlight {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let dark = cx.theme().kind() == ThemeKind::Dark;
        let unit = f32::from(self.scale);
        let progress = self.progress;
        let hidden = !Self::flies(cx);

        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                if hidden {
                    return;
                }

                let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                let half = count(SIZE + 2 * MARGIN) * unit / 2.;

                for index in 0..Self::bees() {
                    let Some(flyer) = Self::bee_at(index, progress) else {
                        continue;
                    };

                    // Whole sprite pixels, so the bee steps and stays sharp.
                    let step = |value: f32| (value / unit).round() * unit;
                    let origin = point(
                        bounds.origin.x + px(step(flyer.x * width - half)),
                        bounds.origin.y + px(step(flyer.y * height - half)),
                    );

                    paint_bee(
                        window,
                        origin,
                        unit,
                        flyer.wings,
                        false,
                        dark,
                        flyer.opacity,
                    );
                }
            },
        )
        .size_full()
    }
}

impl Component for PixelBee {
    fn name() -> &'static str {
        "PixelBee"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        v_flex()
            .gap_6()
            .child(example_group(
                "Frames",
                [
                    example("Wings down", PixelBee::new("bee-down")),
                    example("Wings up", PixelBee::new("bee-up").wings(Wings::Up)),
                    example("Crown", PixelBee::new("bee-crown").crown(true)),
                    example("Flapping", PixelBee::new("bee-flap").flap(true)),
                ],
            ))
            .child(example_group(
                "Scales",
                [
                    example("1x", PixelBee::new("bee-1").scale(1)),
                    example("2x", PixelBee::new("bee-2")),
                    example("3x", PixelBee::new("bee-3").scale(3)),
                ],
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    #[test]
    fn the_wings_flip_halfway_through_a_flap() {
        assert_eq!(wings_at(0.), Wings::Down);
        assert_eq!(wings_at(0.49), Wings::Down);
        assert_eq!(wings_at(0.5), Wings::Up);
        assert_eq!(wings_at(0.99), Wings::Up);
    }

    #[test]
    fn the_bee_with_its_ring_is_26_sprite_pixels_wide() {
        assert_eq!(PixelBee::side(2), px(52.));
        assert_eq!(PixelBee::side(1), px(26.));
    }

    #[test]
    fn the_ring_runs_cover_every_ring_pixel_once() {
        let frame = Frame::new(Wings::Up, true);
        let covered: usize = rim_runs(&frame).iter().map(|(_, _, length)| length).sum();

        assert_eq!(covered, frame.rim().len());
    }

    #[gpui::test]
    fn a_bee_flaps_only_when_asked_and_motion_is_allowed(cx: &mut TestAppContext) {
        cx.update(crate::theme::init);

        let still = PixelBee::new("still");
        let flapping = PixelBee::new("flapping").flap(true);
        cx.update(|cx| {
            assert!(!still.flaps(cx));
            assert!(flapping.flaps(cx));

            cx.set_reduce_motion(true);
            assert!(!flapping.flaps(cx));
        });
    }

    #[gpui::test]
    fn no_bee_flies_under_reduce_motion(cx: &mut TestAppContext) {
        cx.update(crate::theme::init);

        cx.update(|cx| {
            assert!(BeeFlight::flies(cx));

            cx.set_reduce_motion(true);
            assert!(!BeeFlight::flies(cx));
        });
    }

    #[test]
    fn the_flight_sends_every_bee_across_and_ends_empty() {
        for index in 0..BeeFlight::bees() {
            let early = BeeFlight::bee_at(index, 0.4).expect("in the air");
            let late = BeeFlight::bee_at(index, 0.7).expect("still in the air");

            assert!(late.x > early.x, "bee {index} moves right");
        }

        assert!(BeeFlight::bee_at(0, 0.).is_some());
        assert!(BeeFlight::bee_at(BeeFlight::bees() - 1, 0.).is_none());
        assert!(BeeFlight::bee_at(0, 1.).is_none());
        assert!(BeeFlight::bee_at(BeeFlight::bees(), 0.5).is_none());
    }

    #[test]
    fn a_bee_fades_in_and_out() {
        let early = BeeFlight::bee_at(0, 0.002).expect("just started");
        let middle = BeeFlight::bee_at(0, 0.4).expect("in the air");

        assert!(early.opacity < 1.);
        assert!((middle.opacity - 1.).abs() < f32::EPSILON);
    }

    #[test]
    fn the_wings_flip_while_a_bee_flies() {
        let frames: Vec<Wings> = (1..40)
            .filter_map(|step| BeeFlight::bee_at(0, count(step) / 100.))
            .map(|flyer| flyer.wings)
            .collect();

        assert!(frames.contains(&Wings::Down));
        assert!(frames.contains(&Wings::Up));
    }
}
