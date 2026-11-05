use std::f32::consts::{PI, TAU};

use gpui::{
    AnyElement, App, Bounds, ElementId, Hsla, PathBuilder, Pixels, Role, SharedString, Window,
    canvas, div, point, px,
};

use super::tooltip::Tooltip;
use super::usage_meter::UsageTone;
use crate::component::{Component, example, example_group};
use crate::prelude::*;

const RING_SIZE: f32 = 26.;
const STROKE: f32 = 4.;
const SEGMENTS: u8 = 64;

/// A ring that fills clockwise from the top: one usage figure in the room of one icon. The
/// label is required: VoiceOver reads it as a meter. The tooltip carries the numbers.
#[derive(IntoElement)]
#[must_use]
pub struct UsageRing {
    id: ElementId,
    fraction: f32,
    label: SharedString,
    tooltip: Option<SharedString>,
    tone: UsageTone,
    debug_selector: Option<String>,
}

impl UsageRing {
    /// `fraction` is how full the ring is, from 0 to 1.
    pub fn new(id: impl Into<ElementId>, fraction: f32, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            fraction: fraction.clamp(0., 1.),
            label: label.into(),
            tooltip: None,
            tone: UsageTone::Normal,
            debug_selector: None,
        }
    }

    /// The text that shows on hover.
    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    /// A warn tone draws the fill in the warn color.
    pub fn tone(mut self, tone: UsageTone) -> Self {
        self.tone = tone;
        self
    }

    /// Name for `VisualTestContext::debug_bounds`. Only recorded in test builds.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }
}

/// Strokes the arc from the top, clockwise, over `fraction` of the circle.
fn paint_arc(bounds: Bounds<Pixels>, fraction: f32, color: Hsla, window: &mut Window) {
    if fraction <= 0. {
        return;
    }

    let center = bounds.center();
    let radius = (f32::from(bounds.size.width.min(bounds.size.height)) - STROKE) / 2.;

    let mut builder = PathBuilder::stroke(px(STROKE));
    for step in 0..=SEGMENTS {
        let angle = -PI / 2. + TAU * fraction * f32::from(step) / f32::from(SEGMENTS);
        let at = point(
            center.x + px(radius * angle.cos()),
            center.y + px(radius * angle.sin()),
        );

        if step == 0 {
            builder.move_to(at);
        } else {
            builder.line_to(at);
        }
    }

    match builder.build() {
        Ok(path) => window.paint_path(path, color),
        Err(error) => log::debug!("cannot paint the usage ring: {error}"),
    }
}

impl RenderOnce for UsageRing {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (track, fill) = (
            colors.line,
            match self.tone {
                UsageTone::Normal => colors.fg,
                UsageTone::Warn => colors.warn,
            },
        );
        let fraction = self.fraction;

        let ring = canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                paint_arc(bounds, 1., track, window);
                paint_arc(bounds, fraction, fill, window);
            },
        )
        .size(px(RING_SIZE));

        div()
            .id(self.id)
            .role(Role::Meter)
            .aria_label(self.label)
            .aria_numeric_value(f64::from((fraction * 100.).round()))
            .aria_min_numeric_value(0.)
            .aria_max_numeric_value(100.)
            .when_some(self.debug_selector, |ring, selector| {
                ring.debug_selector(move || selector)
            })
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(36.))
            .when_some(self.tooltip, |ring, text| {
                ring.tooltip(move |_, cx| Tooltip::text(text.clone(), cx))
            })
            .child(ring)
    }
}

impl Component for UsageRing {
    fn name() -> &'static str {
        "UsageRing"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        v_flex()
            .gap_6()
            .child(example_group(
                "Tones",
                [
                    example(
                        "42%",
                        UsageRing::new("ring-normal", 0.42, "Usage: highest window 42 percent"),
                    ),
                    example(
                        "Warn",
                        UsageRing::new("ring-warn", 0.9, "Usage: highest window 90 percent")
                            .tone(UsageTone::Warn),
                    ),
                    example(
                        "Empty",
                        UsageRing::new("ring-empty", 0., "Usage: highest window 0 percent"),
                    ),
                ],
            ))
            .into_any_element()
    }
}
