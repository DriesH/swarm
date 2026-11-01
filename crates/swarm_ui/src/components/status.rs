use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, FontWeight, Hsla, Pixels, SharedString,
    Transformation, Window, div, percentage, px, svg,
};

use crate::component::{Component, example, example_group};
use crate::prelude::*;
use crate::theme::ThemeColors;

/// What an agent is doing right now (see GLOSSARY.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentStatus {
    Working,
    Blocked,
    Done,
    Idle,
    Error,
    /// The daemon has not reported a status.
    Unknown,
}

impl AgentStatus {
    pub const ALL: [Self; 6] = [
        Self::Working,
        Self::Blocked,
        Self::Done,
        Self::Idle,
        Self::Error,
        Self::Unknown,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Working => "Working",
            Self::Blocked => "Blocked",
            Self::Done => "Done",
            Self::Idle => "Idle",
            Self::Error => "Error",
            Self::Unknown => "Unknown",
        }
    }

    /// Text or mark color.
    pub fn color(self, cx: &App) -> Hsla {
        self.color_in(cx.theme().colors())
    }

    /// Status colors are semantic: none of them follows the accent the user picked.
    fn color_in(self, colors: &ThemeColors) -> Hsla {
        match self {
            Self::Working => colors.working,
            Self::Blocked => colors.warn,
            Self::Done => colors.ok,
            Self::Idle => colors.faint,
            Self::Error => colors.del,
            Self::Unknown => colors.muted,
        }
    }

    /// The soft fill that goes with [`Self::color`], for bars and pills.
    pub fn background(self, cx: &App) -> Hsla {
        self.pill_colors(cx).1
    }

    fn pill_colors(self, cx: &App) -> (Hsla, Hsla) {
        self.pill_colors_in(cx.theme().colors())
    }

    fn pill_colors_in(self, colors: &ThemeColors) -> (Hsla, Hsla) {
        match self {
            Self::Working => (colors.working, colors.working_bg),
            Self::Blocked => (colors.warn, colors.warn_bg),
            Self::Done => (colors.ok, colors.ok_bg),
            Self::Idle | Self::Unknown => (colors.muted, colors.soft),
            Self::Error => (colors.del, colors.del_bg),
        }
    }
}

/// Status as a small text badge, for tile headers and lists.
#[derive(IntoElement)]
#[must_use]
pub struct StatusPill {
    status: AgentStatus,
    label: Option<SharedString>,
    truncate: bool,
}

impl StatusPill {
    pub fn new(status: AgentStatus) -> Self {
        Self {
            status,
            label: None,
            truncate: false,
        }
    }

    /// Replaces the status word, for example "Needs you".
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Lets the pill shrink in a row: the label is cut with an ellipsis.
    pub fn truncate(mut self) -> Self {
        self.truncate = true;
        self
    }
}

impl RenderOnce for StatusPill {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (text, background) = self.status.pill_colors(cx);

        let truncate = self.truncate;
        let label = Label::new(
            self.label
                .unwrap_or_else(|| SharedString::from(self.status.label())),
        )
        .size(LabelSize::XSmall)
        .weight(FontWeight::MEDIUM)
        .color(Color::Custom(text));

        div()
            .when(!truncate, Styled::flex_none)
            .when(truncate, |pill| pill.flex().min_w_0().overflow_hidden())
            .px(px(6.))
            .py(px(1.))
            .rounded(px(5.))
            .bg(background)
            .child(if truncate { label.truncate() } else { label })
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

/// A spinning three-quarter ring. It stands still when Reduce Motion is on.
pub(crate) fn spinner(size: Pixels, color: Hsla) -> AnyElement {
    svg()
        .data(arc_svg(f32::from(size), STROKE).as_bytes())
        .size(size)
        .flex_none()
        .text_color(color)
        .with_animation(
            "spin",
            Animation::new(Duration::from_millis(900)).repeat(),
            |mark, delta| mark.with_transformation(Transformation::rotate(percentage(delta))),
        )
        .into_any_element()
}

fn status_mark(status: AgentStatus, size: Pixels, cx: &App) -> AnyElement {
    let color = status.color(cx);
    let size_f32 = f32::from(size);

    match status {
        AgentStatus::Working => spinner(size, color),
        AgentStatus::Blocked | AgentStatus::Done | AgentStatus::Error => svg()
            .data(circle_svg(size_f32, STROKE, true).as_bytes())
            .size(size)
            .flex_none()
            .text_color(color)
            .into_any_element(),
        AgentStatus::Idle | AgentStatus::Unknown => svg()
            .data(circle_svg(size_f32, STROKE, false).as_bytes())
            .size(size)
            .flex_none()
            .text_color(color)
            .into_any_element(),
    }
}

/// 10px status ring for tabs and tile headers. Working spins (static when
/// reduce motion is on), blocked, done and error are filled, idle and unknown are empty rings.
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
        let filled = !matches!(self.status, AgentStatus::Idle | AgentStatus::Unknown);

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
                [
                    example(
                        "Needs you",
                        StatusPill::new(AgentStatus::Blocked).label("Needs you"),
                    ),
                    example(
                        "Truncated",
                        div().w(px(80.)).flex().child(
                            StatusPill::new(AgentStatus::Blocked)
                                .label("2 agents working")
                                .truncate(),
                        ),
                    ),
                ],
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Render, TestAppContext, VisualTestContext};

    use super::*;
    use crate::contrast::color_distance;
    use crate::theme::{Accent, Theme};

    /// Every look: light and dark, each with the blue and the honey accent.
    fn themes() -> [Theme; 4] {
        [
            Theme::light(),
            Theme::dark(),
            Theme::light().with_accent(Accent::Honey),
            Theme::dark().with_accent(Accent::Honey),
        ]
    }

    /// Smallest CIELAB distance between two status colors. The closest pair today is 36
    /// (Blocked and Error in light mode); 30 is far above the 10 where two colors read as
    /// different at a glance, and far below the 0 a shared amber gave Working and Blocked.
    const MIN_STATUS_DISTANCE: f32 = 30.;

    #[test]
    fn status_colors_differ_clearly_in_every_theme_and_accent() {
        let mut failures = Vec::new();

        for theme in themes() {
            for (index, first) in AgentStatus::ALL.into_iter().enumerate() {
                for second in AgentStatus::ALL.into_iter().skip(index + 1) {
                    // The app treats these two as one activity (`Activity::Idle`) and draws
                    // both as an empty ring in a neutral grey; the label tells them apart.
                    if matches!((first, second), (AgentStatus::Idle, AgentStatus::Unknown)) {
                        continue;
                    }

                    let distance = color_distance(
                        first.color_in(theme.colors()),
                        second.color_in(theme.colors()),
                    );

                    if distance < MIN_STATUS_DISTANCE {
                        failures.push(format!(
                            "{}: {} and {} are {distance:.1} apart, need {MIN_STATUS_DISTANCE}",
                            theme.name(),
                            first.label(),
                            second.label(),
                        ));
                    }
                }
            }
        }

        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn status_pills_do_not_follow_the_accent() {
        for (blue, honey) in [
            (Theme::light(), Theme::light().with_accent(Accent::Honey)),
            (Theme::dark(), Theme::dark().with_accent(Accent::Honey)),
        ] {
            for status in AgentStatus::ALL {
                assert_eq!(
                    status.pill_colors_in(honey.colors()),
                    status.pill_colors_in(blue.colors()),
                    "{}: {}",
                    honey.name(),
                    status.label(),
                );
            }
        }
    }

    struct Row;

    impl Render for Row {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let pill = |id: &'static str, truncate: bool| {
                let pill = StatusPill::new(AgentStatus::Blocked).label("2 agents working");
                let pill = if truncate { pill.truncate() } else { pill };

                div()
                    .flex()
                    .w(px(80.))
                    .child(pill)
                    .child(div().size(px(1.)).debug_selector(move || id.to_owned()))
            };

            div()
                .flex()
                .flex_col()
                .child(pill("cut", true))
                .child(pill("whole", false))
        }
    }

    fn after(selector: &'static str, cx: &mut VisualTestContext) -> f32 {
        cx.debug_bounds(selector)
            .map_or(0., |bounds| f32::from(bounds.left()))
    }

    #[gpui::test]
    fn a_truncating_pill_shrinks_to_its_row_and_a_plain_one_does_not(cx: &mut TestAppContext) {
        cx.update(crate::theme::init);
        let (_, cx) = cx.add_window_view(|_, _| Row);
        cx.run_until_parked();

        let (cut, whole) = (after("cut", cx), after("whole", cx));

        assert!(cut <= 80., "the pill did not shrink: {cut}");
        assert!(whole > 80., "the plain pill shrank: {whole}");
    }
}
