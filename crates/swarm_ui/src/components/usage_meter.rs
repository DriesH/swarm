use gpui::{
    AnyElement, App, ElementId, FontWeight, Hsla, Role, SharedString, Window, div, px, relative,
};

use crate::component::{Component, example, example_group};
use crate::prelude::*;

/// What a usage bar says about the limit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UsageTone {
    #[default]
    Normal,
    /// The pace so far reaches the limit before the window resets.
    Warn,
}

impl UsageTone {
    fn color(self) -> Color {
        match self {
            Self::Normal => Color::Default,
            Self::Warn => Color::Warn,
        }
    }
}

/// The surface a meter sits on. Where the pace tick crosses the fill it is drawn in this
/// color, as a notch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UsageSurface {
    /// A `Panel`.
    #[default]
    Panel,
    /// A raised `Panel`.
    Raised,
}

impl UsageSurface {
    fn color(self, cx: &App) -> Hsla {
        let colors = cx.theme().colors();

        match self {
            Self::Panel => colors.panel,
            Self::Raised => colors.raised,
        }
    }
}

/// The 0 to 100 figure a meter exposes to assistive technology, from its fill.
fn percent_of(fraction: f32) -> f64 {
    f64::from((fraction * 100.).round())
}

/// The role of a meter, and its numeric value, bounds and label for VoiceOver. A meter with
/// no bar has no figure to expose and stays a labelled group.
fn exposed<E: StatefulInteractiveElement>(
    element: E,
    fraction: Option<f32>,
    label: SharedString,
) -> E {
    let element = element.aria_label(label);

    match fraction {
        Some(fraction) => element
            .role(Role::Meter)
            .aria_numeric_value(percent_of(fraction))
            .aria_min_numeric_value(0.)
            .aria_max_numeric_value(100.),
        None => element.role(Role::Group),
    }
}

/// The track, the fill and the pace tick. The tick is decoration: the meter's accessible
/// label carries what it shows. The tick is centered on the pace and kept inside the bar.
fn usage_bar(
    height: f32,
    fraction: f32,
    pace: Option<f32>,
    tone: UsageTone,
    surface: UsageSurface,
    selector: Option<&str>,
    cx: &App,
) -> impl IntoElement + use<> {
    let colors = cx.theme().colors();
    let (track, fill, tick) = (colors.soft, tone.color().hsla(cx), colors.faint);
    let notch = surface.color(cx);
    let radius = px((height / 2.).ceil());

    let named = |name: &str| selector.map(|selector| format!("{selector}-{name}"));
    let (bar_name, fill_name) = (named("bar"), named("fill"));
    let (tick_name, notch_name) = (named("tick"), named("notch"));

    div()
        .relative()
        .h(px(height))
        .rounded(radius)
        .bg(track)
        .when_some(bar_name, |bar, name| bar.debug_selector(move || name))
        .child(
            div()
                .h(px(height))
                .w(relative(fraction))
                .rounded(radius)
                .bg(fill)
                .when_some(fill_name, |fill, name| fill.debug_selector(move || name)),
        )
        .children(pace.map(|pace| {
            // Faint is too close to the fill, so over the fill the middle of the tick is cut
            // out in the color of the surface. The ends that stick out stay faint.
            let notch = (pace <= fraction).then(|| {
                div()
                    .absolute()
                    .top(px(2.))
                    .left_0()
                    .w_full()
                    .h(px(height))
                    .bg(notch)
                    .when_some(notch_name, |notch, name| notch.debug_selector(move || name))
            });

            // A spacer as wide as the pace, then the tick pulled back by half its width. The
            // spacer gives way at the end of the bar and never goes below the half width.
            div()
                .absolute()
                .top(px(-2.))
                .left_0()
                .w_full()
                .h(px(height + 4.))
                .flex()
                .flex_row()
                .child(div().w(relative(pace)).min_w(px(1.)).flex_shrink(1.))
                .child(
                    div()
                        .relative()
                        .flex_none()
                        .ml(px(-1.))
                        .w(px(2.))
                        .h(px(height + 4.))
                        .rounded(px(1.))
                        .bg(tick)
                        .when_some(tick_name, |tick, name| tick.debug_selector(move || name))
                        .children(notch),
                )
        }))
}

/// One subscription limit in little room: a name, a value such as "42%" and a 4px bar. The
/// tick on the bar shows where even use would be by now. A warn tone adds an alert icon next
/// to the value, so the warning is not color alone. VoiceOver reads one label for the whole
/// row; give it with `accessible_label` to say more than the name and the value.
#[derive(IntoElement)]
#[must_use]
pub struct UsageMeter {
    id: ElementId,
    name: SharedString,
    value: Option<SharedString>,
    fraction: Option<f32>,
    pace: Option<f32>,
    tone: UsageTone,
    surface: UsageSurface,
    accessible_label: Option<SharedString>,
    debug_selector: Option<String>,
}

impl UsageMeter {
    pub fn new(id: impl Into<ElementId>, name: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            value: None,
            fraction: None,
            pace: None,
            tone: UsageTone::Normal,
            surface: UsageSurface::default(),
            accessible_label: None,
            debug_selector: None,
        }
    }

    /// The figure at the end of the name line, for example "42%".
    pub fn value(mut self, value: impl Into<SharedString>) -> Self {
        self.value = Some(value.into());
        self
    }

    /// How full the bar is, from 0 to 1. Without it the row has no bar.
    pub fn fraction(mut self, fraction: f32) -> Self {
        self.fraction = Some(fraction.clamp(0., 1.));
        self
    }

    /// Where even use would be by now, from 0 to 1. It shows as a tick on the bar.
    pub fn pace(mut self, pace: f32) -> Self {
        self.pace = Some(pace.clamp(0., 1.));
        self
    }

    /// Colors the value and the bar, and adds the alert icon for a warning.
    pub fn tone(mut self, tone: UsageTone) -> Self {
        self.tone = tone;
        self
    }

    /// The surface the meter sits on. It is the color of the notch the tick cuts in the fill.
    pub fn surface(mut self, surface: UsageSurface) -> Self {
        self.surface = surface;
        self
    }

    /// Replaces what VoiceOver reads, which is the name and the value.
    pub fn accessible_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessible_label = Some(label.into());
        self
    }

    /// Name for `VisualTestContext::debug_bounds`. Only recorded in test builds. The bar,
    /// its fill, the tick and its notch add `-bar`, `-fill`, `-tick` and `-notch`; the alert
    /// icon adds `-alert`.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }

    fn spoken(&self) -> SharedString {
        if let Some(label) = &self.accessible_label {
            return label.clone();
        }

        let parts = [Some(&self.name), self.value.as_ref()];
        let parts: Vec<&str> = parts.into_iter().flatten().map(|part| &**part).collect();

        parts.join(", ").into()
    }
}

impl RenderOnce for UsageMeter {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let spoken = self.spoken();
        let tone = self.tone;
        let bar = self.fraction.map(|fraction| {
            usage_bar(
                4.,
                fraction,
                self.pace,
                tone,
                self.surface,
                self.debug_selector.as_deref(),
                cx,
            )
        });

        let alert_name = self
            .debug_selector
            .as_ref()
            .map(|selector| format!("{selector}-alert"));
        let alert = (tone == UsageTone::Warn).then(|| {
            div()
                .flex_none()
                .when_some(alert_name, |alert, name| alert.debug_selector(move || name))
                .child(
                    Icon::new(IconName::Alert)
                        .size(IconSize::XSmall)
                        .color(tone.color()),
                )
        });

        let meter = v_flex()
            .id(self.id)
            .when_some(self.debug_selector, |meter, selector| {
                meter.debug_selector(move || selector)
            })
            .gap(px(4.))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        div().flex_1().min_w_0().flex().child(
                            Label::new(self.name)
                                .size(LabelSize::XSmall)
                                .color(Color::Muted)
                                .truncate(),
                        ),
                    )
                    .child(h_flex().flex_none().gap(px(4.)).children(alert).children(
                        self.value.map(|value| {
                            Label::new(value)
                                .mono()
                                .size(LabelSize::XSmall)
                                .color(tone.color())
                        }),
                    )),
            )
            .children(bar);

        exposed(meter, self.fraction, spoken)
    }
}

impl Component for UsageMeter {
    fn name() -> &'static str {
        "UsageMeter"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        let sized = |meter: UsageMeter| div().w(px(208.)).child(meter);

        v_flex()
            .gap_6()
            .child(example_group(
                "States",
                [
                    example(
                        "Value and bar",
                        sized(
                            UsageMeter::new("usage-bar", "Session")
                                .value("42%")
                                .fraction(0.42),
                        ),
                    ),
                    example(
                        "Pace tick",
                        sized(
                            UsageMeter::new("usage-pace", "Session")
                                .value("42%")
                                .fraction(0.42)
                                .pace(0.58),
                        ),
                    ),
                    example(
                        "Warn tone",
                        sized(
                            UsageMeter::new("usage-warn", "Weekly")
                                .value("61%")
                                .fraction(0.61)
                                .pace(0.45)
                                .tone(UsageTone::Warn),
                        ),
                    ),
                    example(
                        "Full",
                        sized(
                            UsageMeter::new("usage-full", "Session")
                                .value("100%")
                                .fraction(1.)
                                .tone(UsageTone::Warn),
                        ),
                    ),
                    example(
                        "No bar",
                        sized(UsageMeter::new("usage-none", "Weekly").value("0%")),
                    ),
                ],
            ))
            .into_any_element()
    }
}

/// One subscription limit with room to spare: the name in bold, a warning next to it, a 5px
/// bar with the pace tick, then the value at the start of the last line and a note at its
/// end. VoiceOver reads one label for the whole meter.
#[derive(IntoElement)]
#[must_use]
pub struct UsageDetailMeter {
    id: ElementId,
    name: SharedString,
    value: Option<SharedString>,
    fraction: Option<f32>,
    pace: Option<f32>,
    tone: UsageTone,
    surface: UsageSurface,
    warning: Option<SharedString>,
    note: Option<SharedString>,
    accessible_label: Option<SharedString>,
    debug_selector: Option<String>,
}

impl UsageDetailMeter {
    pub fn new(id: impl Into<ElementId>, name: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            value: None,
            fraction: None,
            pace: None,
            tone: UsageTone::Normal,
            surface: UsageSurface::default(),
            warning: None,
            note: None,
            accessible_label: None,
            debug_selector: None,
        }
    }

    /// The figure at the start of the line under the bar, for example "42% used".
    pub fn value(mut self, value: impl Into<SharedString>) -> Self {
        self.value = Some(value.into());
        self
    }

    /// How full the bar is, from 0 to 1. Without it the meter has no bar.
    pub fn fraction(mut self, fraction: f32) -> Self {
        self.fraction = Some(fraction.clamp(0., 1.));
        self
    }

    /// Where even use would be by now, from 0 to 1. It shows as a tick on the bar.
    pub fn pace(mut self, pace: f32) -> Self {
        self.pace = Some(pace.clamp(0., 1.));
        self
    }

    /// Colors the bar.
    pub fn tone(mut self, tone: UsageTone) -> Self {
        self.tone = tone;
        self
    }

    /// The surface the meter sits on. It is the color of the notch the tick cuts in the fill.
    pub fn surface(mut self, surface: UsageSurface) -> Self {
        self.surface = surface;
        self
    }

    /// A warning with an alert icon at the end of the name line, for example "Limit in 3d 21h".
    pub fn warning(mut self, warning: impl Into<SharedString>) -> Self {
        self.warning = Some(warning.into());
        self
    }

    /// A faint note at the end of the line under the bar, for example "Resets in 2h 6m".
    pub fn note(mut self, note: impl Into<SharedString>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Replaces what VoiceOver reads, which is the name, the value, the warning and the note.
    pub fn accessible_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessible_label = Some(label.into());
        self
    }

    /// Name for `VisualTestContext::debug_bounds`. Only recorded in test builds. The bar,
    /// its fill, the tick and its notch add `-bar`, `-fill`, `-tick` and `-notch`; the
    /// warning, the value and the note add `-warning`, `-value` and `-note`.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }

    fn spoken(&self) -> SharedString {
        if let Some(label) = &self.accessible_label {
            return label.clone();
        }

        let parts = [
            Some(&self.name),
            self.value.as_ref(),
            self.warning.as_ref(),
            self.note.as_ref(),
        ];
        let parts: Vec<&str> = parts.into_iter().flatten().map(|part| &**part).collect();

        parts.join(", ").into()
    }
}

impl RenderOnce for UsageDetailMeter {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let spoken = self.spoken();
        let bar = self.fraction.map(|fraction| {
            usage_bar(
                5.,
                fraction,
                self.pace,
                self.tone,
                self.surface,
                self.debug_selector.as_deref(),
                cx,
            )
        });

        let named = |name: &str| {
            self.debug_selector
                .as_ref()
                .map(|selector| format!("{selector}-{name}"))
        };
        let (warning_name, value_name, note_name) =
            (named("warning"), named("value"), named("note"));
        let warning = self.warning.map(|warning| {
            h_flex()
                .flex_none()
                .gap(px(4.))
                .when_some(warning_name, |row, name| row.debug_selector(move || name))
                .child(
                    Icon::new(IconName::Alert)
                        .size(IconSize::Small)
                        .color(Color::Warn),
                )
                .child(
                    Label::new(warning)
                        .size(LabelSize::XSmall)
                        .weight(FontWeight::MEDIUM)
                        .color(Color::Warn),
                )
        });

        let has_last_line = self.value.is_some() || self.note.is_some();

        let meter = v_flex()
            .id(self.id)
            .when_some(self.debug_selector, |meter, selector| {
                meter.debug_selector(move || selector)
            })
            .gap(px(5.))
            .child(
                h_flex()
                    .gap(px(6.))
                    .child(
                        div().flex_1().min_w_0().flex().child(
                            Label::new(self.name)
                                .size(LabelSize::Small)
                                .weight(FontWeight::MEDIUM)
                                .truncate(),
                        ),
                    )
                    .children(warning),
            )
            .children(bar)
            .when(has_last_line, |meter| {
                meter.child(
                    h_flex()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .children(self.value.map(|value| {
                                    div()
                                        .min_w_0()
                                        .when_some(value_name, |row, name| {
                                            row.debug_selector(move || name)
                                        })
                                        .child(
                                            Label::new(value)
                                                .size(LabelSize::XSmall)
                                                .color(Color::Muted)
                                                .truncate(),
                                        )
                                })),
                        )
                        .children(self.note.map(|note| {
                            div()
                                .flex_none()
                                .when_some(note_name, |row, name| row.debug_selector(move || name))
                                .child(Label::new(note).size(LabelSize::XSmall).color(Color::Faint))
                        })),
                )
            });

        exposed(meter, self.fraction, spoken)
    }
}

impl Component for UsageDetailMeter {
    fn name() -> &'static str {
        "UsageDetailMeter"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        let sized = |meter: UsageDetailMeter| div().w(px(270.)).child(meter);

        v_flex()
            .gap_6()
            .child(example_group(
                "States",
                [
                    example(
                        "On pace",
                        sized(
                            UsageDetailMeter::new("detail-ok", "Session")
                                .value("42% used")
                                .fraction(0.42)
                                .pace(0.58)
                                .note("Resets in 2h 6m"),
                        ),
                    ),
                    example(
                        "At risk",
                        sized(
                            UsageDetailMeter::new("detail-risk", "Weekly")
                                .value("61% used")
                                .fraction(0.61)
                                .pace(0.45)
                                .tone(UsageTone::Warn)
                                .warning("Limit in 3d 21h")
                                .note("Resets in 6d 11h"),
                        ),
                    ),
                    example(
                        "No pace",
                        sized(
                            UsageDetailMeter::new("detail-plain", "Monthly")
                                .value("9% used")
                                .fraction(0.09)
                                .note("Resets in 5d 2h"),
                        ),
                    ),
                    example(
                        "Value only",
                        sized(
                            UsageDetailMeter::new("detail-value", "limit window")
                                .value("30% used")
                                .fraction(0.3),
                        ),
                    ),
                ],
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Bounds, Context, Pixels, ScaledPixels, TestAppContext, VisualTestContext};

    use super::*;

    struct Probe {
        fraction: f32,
        pace: Option<f32>,
        tone: UsageTone,
        surface: UsageSurface,
        warning: bool,
        compact: bool,
    }

    impl Probe {
        fn new(compact: bool) -> Self {
            Self {
                fraction: 0.61,
                pace: None,
                tone: UsageTone::Normal,
                surface: UsageSurface::Panel,
                warning: false,
                compact,
            }
        }

        fn pace(self, pace: f32) -> Self {
            Self {
                pace: Some(pace),
                ..self
            }
        }
    }

    impl Render for Probe {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let pace = self.pace;

            let meter = if self.compact {
                let meter = UsageMeter::new("compact", "Weekly")
                    .value("61%")
                    .fraction(self.fraction)
                    .tone(self.tone)
                    .surface(self.surface)
                    .debug_selector(|| "meter".to_owned());

                match pace {
                    Some(pace) => meter.pace(pace).into_any_element(),
                    None => meter.into_any_element(),
                }
            } else {
                let meter = UsageDetailMeter::new("detail", "Weekly")
                    .value("61% used")
                    .fraction(self.fraction)
                    .tone(self.tone)
                    .surface(self.surface)
                    .note("Resets in 6d 11h")
                    .debug_selector(|| "meter".to_owned());
                let meter = match pace {
                    Some(pace) => meter.pace(pace),
                    None => meter,
                };

                if self.warning {
                    meter.warning("Limit in 3d 21h").into_any_element()
                } else {
                    meter.into_any_element()
                }
            };

            div().w(px(200.)).child(meter)
        }
    }

    fn setup(probe: Probe, cx: &mut TestAppContext) -> &mut VisualTestContext {
        cx.update(crate::theme::init);

        let (_, cx) = cx.add_window_view(move |_, _| probe);
        cx.run_until_parked();

        cx
    }

    fn bounds(selector: &'static str, cx: &mut VisualTestContext) -> Option<Bounds<Pixels>> {
        cx.debug_bounds(selector)
    }

    /// The color of the quad drawn at the bounds of `selector`. When more than one quad has
    /// those bounds, the one drawn last, which is the one on top.
    fn drawn_color(selector: &'static str, cx: &mut VisualTestContext) -> Option<Hsla> {
        let bounds = bounds(selector, cx)?;

        cx.update(|window, _| {
            let wanted = bounds.scale(window.scale_factor());
            let near = |a: ScaledPixels, b: ScaledPixels| (a.0 - b.0).abs() < 0.01;

            window
                .painted_quads()
                .iter()
                .rev()
                .find(|quad| {
                    near(quad.bounds.origin.x, wanted.origin.x)
                        && near(quad.bounds.origin.y, wanted.origin.y)
                        && near(quad.bounds.size.width, wanted.size.width)
                        && near(quad.bounds.size.height, wanted.size.height)
                })
                .and_then(|quad| quad.background.as_solid())
        })
    }

    #[gpui::test]
    fn the_tick_is_centered_on_the_pace_along_the_bar(cx: &mut TestAppContext) {
        for compact in [true, false] {
            let cx = setup(Probe::new(compact).pace(0.25), cx);

            let bar = bounds("meter-bar", cx);
            let tick = bounds("meter-tick", cx);

            let middle = bar
                .zip(tick)
                .map(|(bar, tick)| f32::from(tick.center().x - bar.origin.x));
            let width = bar.map(|bar| f32::from(bar.size.width));
            assert_eq!(width, Some(200.), "compact: {compact}");
            assert!(
                middle.is_some_and(|middle| (middle - 50.).abs() < 0.5),
                "compact: {compact}, middle {middle:?}"
            );
        }
    }

    #[gpui::test]
    fn the_tick_stays_inside_the_bar_at_both_ends(cx: &mut TestAppContext) {
        for compact in [true, false] {
            for pace in [0., 0.002, 0.5, 0.998, 1.] {
                let cx = setup(Probe::new(compact).pace(pace), cx);

                let bar = bounds("meter-bar", cx).expect("the bar is drawn");
                let tick = bounds("meter-tick", cx).expect("the tick is drawn");

                assert!(
                    tick.left() >= bar.left() && tick.right() <= bar.right(),
                    "compact: {compact}, pace {pace}, tick {tick:?}, bar {bar:?}"
                );
            }
        }
    }

    #[gpui::test]
    fn the_tick_touches_the_edge_of_the_bar_at_the_start_and_at_the_end(cx: &mut TestAppContext) {
        let cx = setup(Probe::new(true).pace(0.), cx);
        let (bar, tick) = (bounds("meter-bar", cx), bounds("meter-tick", cx));
        assert_eq!(tick.map(|tick| tick.left()), bar.map(|bar| bar.left()));

        let cx = setup(Probe::new(true).pace(1.), cx);
        let (bar, tick) = (bounds("meter-bar", cx), bounds("meter-tick", cx));
        assert_eq!(tick.map(|tick| tick.right()), bar.map(|bar| bar.right()));
    }

    #[gpui::test]
    fn the_bars_are_four_and_five_pixels_high_and_the_tick_sticks_out_two_each_side(
        cx: &mut TestAppContext,
    ) {
        for (compact, height) in [(true, 4.), (false, 5.)] {
            let cx = setup(Probe::new(compact).pace(0.5), cx);

            let bar = bounds("meter-bar", cx).map(|bar| f32::from(bar.size.height));
            let tick = bounds("meter-tick", cx).map(|tick| f32::from(tick.size.height));

            assert_eq!(bar, Some(height), "compact: {compact}");
            assert_eq!(tick, Some(height + 4.), "compact: {compact}");
        }
    }

    #[gpui::test]
    fn the_fill_follows_the_fraction(cx: &mut TestAppContext) {
        let cx = setup(Probe::new(false), cx);

        let fill = bounds("meter-fill", cx).map(|fill| f32::from(fill.size.width));

        assert!(
            fill.is_some_and(|fill| (fill - 122.).abs() < 0.5),
            "{fill:?}"
        );
    }

    #[gpui::test]
    fn no_pace_draws_no_tick(cx: &mut TestAppContext) {
        for compact in [true, false] {
            let cx = setup(Probe::new(compact), cx);

            assert!(bounds("meter-bar", cx).is_some(), "compact: {compact}");
            assert!(bounds("meter-tick", cx).is_none(), "compact: {compact}");
            assert!(bounds("meter-notch", cx).is_none(), "compact: {compact}");
        }
    }

    #[gpui::test]
    fn a_tick_over_the_fill_is_cut_out_in_the_color_of_the_surface(cx: &mut TestAppContext) {
        for compact in [true, false] {
            for (tone, fill) in [
                (UsageTone::Normal, Color::Default),
                (UsageTone::Warn, Color::Warn),
            ] {
                for surface in [UsageSurface::Panel, UsageSurface::Raised] {
                    let probe = Probe {
                        tone,
                        surface,
                        ..Probe::new(compact).pace(0.45)
                    };
                    let cx = setup(probe, cx);
                    let wanted = cx.update(|_, cx| (surface.color(cx), fill.hsla(cx)));
                    let name = format!("compact: {compact}, {tone:?}, {surface:?}");

                    assert_eq!(drawn_color("meter-notch", cx), Some(wanted.0), "{name}");
                    assert_eq!(drawn_color("meter-fill", cx), Some(wanted.1), "{name}");
                }
            }
        }
    }

    #[gpui::test]
    fn the_ends_of_the_tick_that_stick_out_stay_faint(cx: &mut TestAppContext) {
        let cx = setup(Probe::new(true).pace(0.45), cx);

        let faint = cx.update(|_, cx| cx.theme().colors().faint);
        let tick = bounds("meter-tick", cx).expect("the tick is drawn");
        let notch = bounds("meter-notch", cx).expect("the notch is drawn");

        assert_eq!(drawn_color("meter-tick", cx), Some(faint));
        assert!(tick.top() < notch.top() && notch.bottom() < tick.bottom());
    }

    #[gpui::test]
    fn a_tick_over_the_track_stays_faint_with_no_notch(cx: &mut TestAppContext) {
        let probe = Probe {
            fraction: 0.3,
            ..Probe::new(true).pace(0.58)
        };
        let cx = setup(probe, cx);

        let faint = cx.update(|_, cx| cx.theme().colors().faint);

        assert!(bounds("meter-notch", cx).is_none());
        assert_eq!(drawn_color("meter-tick", cx), Some(faint));
    }

    #[gpui::test]
    fn the_warning_shows_only_when_given_and_never_changes_the_height(cx: &mut TestAppContext) {
        let mut heights = Vec::new();

        for warning in [false, true] {
            let probe = Probe {
                warning,
                ..Probe::new(false).pace(0.5)
            };
            let cx = setup(probe, cx);

            heights.push(bounds("meter", cx).map(|meter| f32::from(meter.size.height)));
            assert_eq!(
                bounds("meter-warning", cx).is_some(),
                warning,
                "warning: {warning}"
            );
        }

        assert!(heights[0].is_some());
        assert_eq!(heights[0], heights[1]);
    }

    #[gpui::test]
    fn the_warning_and_the_note_end_at_the_bar_and_the_value_starts_at_it(cx: &mut TestAppContext) {
        let probe = Probe {
            warning: true,
            ..Probe::new(false).pace(0.5)
        };
        let cx = setup(probe, cx);

        let bar = bounds("meter-bar", cx).expect("the bar is drawn");
        let warning = bounds("meter-warning", cx).expect("the warning is drawn");
        let value = bounds("meter-value", cx).expect("the value is drawn");
        let note = bounds("meter-note", cx).expect("the note is drawn");

        assert_eq!(warning.right(), bar.right());
        assert_eq!(note.right(), bar.right());
        assert_eq!(value.left(), bar.left());
        assert!(
            warning.bottom() <= bar.top(),
            "the warning is above the bar"
        );
        assert!(value.top() >= bar.bottom(), "the value is under the bar");
        assert_eq!(value.top(), note.top());
    }

    #[gpui::test]
    fn a_warn_tone_adds_an_alert_icon_next_to_the_value(cx: &mut TestAppContext) {
        let normal = setup(Probe::new(true), cx);
        assert!(bounds("meter-alert", normal).is_none());

        let probe = Probe {
            tone: UsageTone::Warn,
            ..Probe::new(true)
        };
        let warn = setup(probe, cx);

        let alert = bounds("meter-alert", warn).expect("the alert icon is drawn");
        let bar = bounds("meter-bar", warn).expect("the bar is drawn");
        assert!(alert.right() < bar.right(), "the icon is left of the end");
        assert!(alert.bottom() <= bar.top(), "the icon is above the bar");
    }

    #[gpui::test]
    fn the_alert_icon_does_not_change_the_height(cx: &mut TestAppContext) {
        let normal = setup(Probe::new(true), cx);
        let plain = bounds("meter", normal).map(|meter| f32::from(meter.size.height));

        let probe = Probe {
            tone: UsageTone::Warn,
            ..Probe::new(true)
        };
        let warn = setup(probe, cx);
        let alerted = bounds("meter", warn).map(|meter| f32::from(meter.size.height));

        assert!(plain.is_some());
        assert_eq!(plain, alerted);
    }

    #[test]
    fn the_bar_never_goes_past_full_or_below_empty() {
        assert_eq!(
            UsageMeter::new("meter", "x").fraction(1.4).fraction,
            Some(1.)
        );
        assert_eq!(
            UsageMeter::new("meter", "x").fraction(-0.2).fraction,
            Some(0.)
        );
        assert_eq!(
            UsageDetailMeter::new("meter", "x").fraction(1.4).fraction,
            Some(1.)
        );
    }

    #[test]
    fn the_tick_stays_on_the_bar() {
        assert_eq!(UsageMeter::new("meter", "x").pace(1.7).pace, Some(1.));
        assert_eq!(UsageMeter::new("meter", "x").pace(-0.3).pace, Some(0.));
        assert_eq!(
            UsageDetailMeter::new("meter", "x").pace(0.45).pace,
            Some(0.45)
        );
    }

    #[test]
    fn the_exposed_figure_is_the_rounded_percent_of_the_fill() {
        let figures = [0.61, 0., 1., 0.075].map(|fraction| percent_of(fraction).to_bits());

        assert_eq!(figures, [61., 0., 100., 8.].map(f64::to_bits));
    }

    #[test]
    fn the_warn_tone_uses_the_warn_color() {
        assert_eq!(UsageTone::Warn.color(), Color::Warn);
        assert_eq!(UsageTone::Normal.color(), Color::Default);
        assert_eq!(UsageTone::default(), UsageTone::Normal);
    }
}
