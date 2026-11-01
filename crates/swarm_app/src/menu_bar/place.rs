//! Where the popover opens: under the status item, inside the screen.
//!
//! GPUI places a window from the top left corner of its display. Moving the `NSWindow`
//! after `open_window` calls back into GPUI while the app is borrowed, so the bounds are
//! worked out up front from the button's frame and its screen.

use gpui::{Bounds, DisplayId, Pixels, Size, point, px, size};

/// Space between the menu bar and the popover.
const GAP: f64 = 4.;
/// Space kept between the popover and the edges of the screen.
const EDGE: f64 = 8.;

/// A rectangle in Cocoa screen coordinates: y goes up, and the origin is the bottom left of
/// the main screen. A screen left of the main one has a negative x.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Frame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Frame {
    pub(crate) fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn right(&self) -> f64 {
        self.x + self.width
    }

    fn top(&self) -> f64 {
        self.y + self.height
    }
}

/// A display with the area apps may use (without the menu bar and the Dock).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Screen {
    pub display: u32,
    pub frame: Frame,
    pub visible: Frame,
}

/// The status item's button and the screen it is on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Anchor {
    pub button: Frame,
    pub screen: Screen,
}

/// What to pass to `open_window`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Placement {
    pub display: DisplayId,
    pub bounds: Bounds<Pixels>,
}

/// The bounds of a popover of `wanted` size, centered under the button. It stays inside the
/// visible part of the screen, and gets shorter when the screen is not tall enough.
pub(crate) fn below(anchor: &Anchor, wanted: Size<Pixels>) -> Placement {
    let Anchor { button, screen } = anchor;
    let width = f64::from(f32::from(wanted.width));

    let left = (button.x + button.width / 2. - width / 2.)
        .min(screen.visible.right() - width - EDGE)
        .max(screen.visible.x + EDGE);

    let top = button.y - GAP;
    let room = (top - screen.visible.y - EDGE).max(0.);
    let height = f64::from(f32::from(wanted.height)).min(room);

    let origin = point(
        px(to_f32(left - screen.frame.x)),
        px(to_f32(screen.frame.top() - top)),
    );

    Placement {
        display: DisplayId::new(u64::from(screen.display)),
        bounds: Bounds::new(origin, size(wanted.width, px(to_f32(height)))),
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "screen coordinates are far inside f32 range"
)]
fn to_f32(value: f64) -> f32 {
    value as f32
}

#[cfg(test)]
#[expect(clippy::float_cmp, reason = "the sizes are whole numbers")]
mod tests {
    use super::*;

    const WIDTH: f32 = 320.;
    const HEIGHT: f32 = 400.;

    fn screen(display: u32, frame: Frame) -> Screen {
        // The menu bar takes 24 points at the top.
        Screen {
            display,
            frame,
            visible: Frame::new(frame.x, frame.y, frame.width, frame.height - 24.),
        }
    }

    fn button(x: f64, screen: &Screen) -> Frame {
        Frame::new(x, screen.frame.top() - 24., 60., 24.)
    }

    fn placed(anchor: &Anchor) -> Placement {
        below(anchor, size(px(WIDTH), px(HEIGHT)))
    }

    fn origin_of(placement: &Placement) -> (f32, f32) {
        (
            f32::from(placement.bounds.origin.x),
            f32::from(placement.bounds.origin.y),
        )
    }

    #[test]
    fn the_popover_is_centered_under_the_button_and_a_little_below_the_menu_bar() {
        let main = screen(1, Frame::new(0., 0., 1_440., 900.));
        let anchor = Anchor {
            button: button(900., &main),
            screen: main,
        };

        let placement = placed(&anchor);

        // The button's middle is 930; the popover is 320 wide.
        assert_eq!(origin_of(&placement), (770., 28.));
        assert_eq!(placement.display, DisplayId::new(1));
        assert_eq!(f32::from(placement.bounds.size.width), WIDTH);
        assert_eq!(f32::from(placement.bounds.size.height), HEIGHT);
    }

    #[test]
    fn a_screen_left_of_the_main_one_gives_a_position_from_its_own_left_edge() {
        let left = screen(2, Frame::new(-1_920., 0., 1_920., 1_080.));
        let anchor = Anchor {
            button: button(-700., &left),
            screen: left,
        };

        let placement = placed(&anchor);

        // The button's middle is 1,250 points from the screen's left edge.
        assert_eq!(origin_of(&placement), (1_090., 28.));
        assert_eq!(placement.display, DisplayId::new(2));
    }

    #[test]
    fn a_screen_above_the_main_one_counts_y_from_its_own_top() {
        let above = screen(3, Frame::new(0., 900., 1_440., 900.));
        let anchor = Anchor {
            button: button(500., &above),
            screen: above,
        };

        let placement = placed(&anchor);

        assert_eq!(origin_of(&placement).1, 28.);
    }

    #[test]
    fn the_right_edge_is_clamped_inside_the_screen() {
        let main = screen(1, Frame::new(0., 0., 1_440., 900.));
        let anchor = Anchor {
            button: button(1_400., &main),
            screen: main,
        };

        let placement = placed(&anchor);

        // 1,440 wide, minus the popover and the 8 point edge.
        assert_eq!(origin_of(&placement).0, 1_112.);
    }

    #[test]
    fn the_left_edge_is_clamped_inside_a_screen_at_a_negative_x() {
        let left = screen(2, Frame::new(-1_920., 0., 1_920., 1_080.));
        let anchor = Anchor {
            button: button(-1_915., &left),
            screen: left,
        };

        let placement = placed(&anchor);

        assert_eq!(origin_of(&placement).0, 8.);
    }

    #[test]
    fn the_right_edge_is_clamped_on_a_screen_at_a_negative_x() {
        let left = screen(2, Frame::new(-1_920., 0., 1_920., 1_080.));
        let anchor = Anchor {
            button: button(-30., &left),
            screen: left,
        };

        let placement = placed(&anchor);

        assert_eq!(origin_of(&placement).0, 1_920. - WIDTH - 8.);
    }

    #[test]
    fn a_screen_that_is_too_short_makes_the_popover_shorter() {
        let small = screen(1, Frame::new(0., 0., 1_024., 300.));
        let anchor = Anchor {
            button: button(500., &small),
            screen: small,
        };

        let placement = placed(&anchor);

        // The menu bar is 24, the gap 4 and the bottom edge 8.
        assert_eq!(
            f32::from(placement.bounds.size.height),
            300. - 24. - 4. - 8.
        );
    }
}
