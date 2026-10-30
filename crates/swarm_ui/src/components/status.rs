use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, FontWeight, Hsla, Pixels, SharedString,
    Transformation, Window, div, percentage, px, svg,
};

use crate::component::{Component, example, example_group};
use crate::prelude::*;

/// What an agent is doing right now (see GLOSSARY.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentStatus {
    Working,
    Waiting,
    Done,
    Idle,
}

impl AgentStatus {
    pub const ALL: [Self; 4] = [Self::Working, Self::Waiting, Self::Done, Self::Idle];

    pub fn label(self) -> &'static str {
        match self {
            Self::Working => "Working",
            Self::Waiting => "Waiting",
            Self::Done => "Done",
            Self::Idle => "Idle",
        }
    }

    /// Text or mark color.
    pub fn color(self, cx: &App) -> Hsla {
        let colors = cx.theme().colors();

        match self {
            Self::Working => colors.accent,
            Self::Waiting => colors.warn,
            Self::Done => colors.ok,
            Self::Idle => colors.faint,
        }
    }

    fn pill_colors(self, cx: &App) -> (Hsla, Hsla) {
        let colors = cx.theme().colors();

        match self {
            Self::Working => (colors.accent, colors.accent_bg),
            Self::Waiting => (colors.warn, colors.warn_bg),
            Self::Done => (colors.ok, colors.ok_bg),
            Self::Idle => (colors.muted, colors.soft),
        }
    }
}

/// Status as a small text badge, for tile headers and lists.
#[derive(IntoElement)]
#[must_use]
pub struct StatusPill {
    status: AgentStatus,
    label: Option<SharedString>,
}

impl StatusPill {
    pub fn new(status: AgentStatus) -> Self {
        Self {
            status,
            label: None,
        }
    }

    /// Replaces the status word, for example "Needs you".
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl RenderOnce for StatusPill {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (text, background) = self.status.pill_colors(cx);

        div()
            .flex_none()
            .px(px(6.))
            .py(px(1.))
            .rounded(px(5.))
            .bg(background)
            .child(
                Label::new(
                    self.label
                        .unwrap_or_else(|| SharedString::from(self.status.label())),
                )
                .size(LabelSize::XSmall)
                .weight(FontWeight::MEDIUM)
                .color(Color::Custom(text)),
            )
    }
}

fn circle_svg(size: f32, stroke: f32, filled: bool) -> String {
    let center = size / 2.;
    let radius = center - stroke / 2.;
    let fill = if filled { "#000" } else { "none" };

    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {size} {size}"><circle cx="{center}" cy="{center}" r="{radius}" fill="{fill}" stroke="#000" stroke-width="{stroke}"/></svg>"##
    )
}

fn arc_svg(size: f32, stroke: f32) -> String {
    let center = size / 2.;
    let radius = center - stroke / 2.;
    // Three quarters of a circle, open at the top like the mockup's spinner.
    let start_x = center + radius;
    let end_y = center - radius;

    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {size} {size}"><path d="M{start_x} {center}A{radius} {radius} 0 1 1 {center} {end_y}" fill="none" stroke="#000" stroke-width="{stroke}" stroke-linecap="butt"/></svg>"##
    )
}

const STROKE: f32 = 1.5;

fn status_mark(status: AgentStatus, size: Pixels, cx: &App) -> AnyElement {
    let color = status.color(cx);
    let size_f32 = f32::from(size);

    match status {
        AgentStatus::Working => svg()
            .data(arc_svg(size_f32, STROKE).as_bytes())
            .size(size)
            .flex_none()
            .text_color(color)
            .with_animation(
                "spin",
                Animation::new(Duration::from_millis(900)).repeat(),
                |mark, delta| mark.with_transformation(Transformation::rotate(percentage(delta))),
            )
            .into_any_element(),
        AgentStatus::Waiting | AgentStatus::Done => svg()
            .data(circle_svg(size_f32, STROKE, true).as_bytes())
            .size(size)
            .flex_none()
            .text_color(color)
            .into_any_element(),
        AgentStatus::Idle => svg()
            .data(circle_svg(size_f32, STROKE, false).as_bytes())
            .size(size)
            .flex_none()
            .text_color(color)
            .into_any_element(),
    }
}

/// 10px status ring for tabs and tile headers. Working spins (static when
/// reduce motion is on), waiting and done are filled, idle is an empty ring.
#[derive(IntoElement)]
#[must_use]
pub struct StatusRing {
    status: AgentStatus,
    size: Pixels,
}

impl StatusRing {
    pub fn new(status: AgentStatus) -> Self {
        Self {
            status,
            size: px(10.),
        }
    }

    pub fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for StatusRing {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        status_mark(self.status, self.size, cx)
    }
}

/// 7px status dot for sidebar rows and tray chips. Never spins.
#[derive(IntoElement)]
#[must_use]
pub struct StatusDot {
    status: AgentStatus,
    size: Pixels,
}

impl StatusDot {
    pub fn new(status: AgentStatus) -> Self {
        Self {
            status,
            size: px(7.),
        }
    }

    pub fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for StatusDot {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let filled = self.status != AgentStatus::Idle;

        svg()
            .data(circle_svg(f32::from(self.size), STROKE, filled).as_bytes())
            .size(self.size)
            .flex_none()
            .text_color(self.status.color(cx))
    }
}

impl Component for StatusPill {
    fn name() -> &'static str {
        "Status"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        v_flex()
            .gap_6()
            .child(example_group(
                "Pill",
                AgentStatus::ALL
                    .into_iter()
                    .map(|status| example(status.label(), StatusPill::new(status))),
            ))
            .child(example_group(
                "Ring",
                AgentStatus::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(index, status)| {
                        example(
                            status.label(),
                            div().id(("ring", index)).child(StatusRing::new(status)),
                        )
                    }),
            ))
            .child(example_group(
                "Dot",
                AgentStatus::ALL
                    .into_iter()
                    .map(|status| example(status.label(), StatusDot::new(status))),
            ))
            .child(example_group(
                "Custom label",
                [example(
                    "Needs you",
                    StatusPill::new(AgentStatus::Waiting).label("Needs you"),
                )],
            ))
            .into_any_element()
    }
}
