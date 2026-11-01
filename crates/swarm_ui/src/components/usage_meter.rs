use gpui::{AnyElement, App, ElementId, FontWeight, Role, SharedString, Window, div, px, relative};

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

/// The track, the fill and the pace tick. The tick is decoration: the label of the meter
/// says everything it shows.
fn usage_bar(
    height: f32,
    fraction: f32,
    pace: Option<f32>,
    tone: UsageTone,
    selector: Option<&str>,
    cx: &App,
) -> impl IntoElement + use<> {
    let colors = cx.theme().colors();
    let (track, fill, tick) = (colors.soft, tone.color().hsla(cx), colors.faint);
    let radius = px((height / 2.).ceil());

    let named = |name: &str| selector.map(|selector| format!("{selector}-{name}"));
    let (bar_name, fill_name, tick_name) = (named("bar"), named("fill"), named("tick"));

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
            div()
                .absolute()
                .top(px(-2.))
                .left(relative(pace))
                .w(px(2.))
                .h(px(height + 4.))
                .rounded(px(1.))
                .bg(tick)
                .when_some(tick_name, |tick, name| tick.debug_selector(move || name))
        }))
}

/// One subscription limit in little room: a name, a value such as "42%" and a 4px bar. The
/// tick on the bar shows where even use would be by now. VoiceOver reads one label for the
/// whole row.
#[derive(IntoElement)]
#[must_use]
pub struct UsageMeter {
    id: ElementId,
    name: SharedString,
    value: Option<SharedString>,
    fraction: Option<f32>,
    pace: Option<f32>,
    tone: UsageTone,
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

    /// Colors the value and the bar.
    pub fn tone(mut self, tone: UsageTone) -> Self {
        self.tone = tone;
        self
    }

    /// Replaces what VoiceOver reads, which is the name and the value.
    pub fn accessible_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessible_label = Some(label.into());
        self
    }

    /// Name for `VisualTestContext::debug_bounds`. Only recorded in test builds. The bar,
    /// its fill and the tick add `-bar`, `-fill` and `-tick`.
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
                self.debug_selector.as_deref(),
                cx,
            )
        });

        v_flex()
            .id(self.id)
            .when_some(self.debug_selector, |meter, selector| {
                meter.debug_selector(move || selector)
            })
            .role(Role::Group)
            .aria_label(spoken)
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
                    .children(self.value.map(|value| {
                        Label::new(value)
                            .mono()
                            .size(LabelSize::XSmall)
                            .color(tone.color())
                    })),
            )
            .children(bar)
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
    /// its fill and the tick add `-bar`, `-fill` and `-tick`; the warning, the value and the
    /// note add `-warning`, `-value` and `-note`.
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

        v_flex()
            .id(self.id)
            .when_some(self.debug_selector, |meter, selector| {
                meter.debug_selector(move || selector)
            })
            .role(Role::Group)
            .aria_label(spoken)
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
            })
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
    use gpui::{Context, TestAppContext, VisualTestContext};

    use super::*;

    struct Probe {
        pace: Option<f32>,
        warning: bool,
        compact: bool,
    }

    impl Render for Probe {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let pace = self.pace;

            let meter = if self.compact {
                let meter = UsageMeter::new("compact", "Weekly")
                    .value("61%")
                    .fraction(0.61)
                    .debug_selector(|| "meter".to_owned());

                match pace {
                    Some(pace) => meter.pace(pace).into_any_element(),
                    None => meter.into_any_element(),
                }
            } else {
                let meter = UsageDetailMeter::new("detail", "Weekly")
                    .value("61% used")
                    .fraction(0.61)
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

    fn bounds(
        selector: &'static str,
        cx: &mut VisualTestContext,
    ) -> Option<gpui::Bounds<gpui::Pixels>> {
        cx.debug_bounds(selector)
    }

    #[gpui::test]
    fn the_tick_sits_at_the_pace_along_the_bar(cx: &mut TestAppContext) {
        for compact in [true, false] {
            let probe = Probe {
                pace: Some(0.25),
                warning: false,
                compact,
            };
            let cx = setup(probe, cx);

            let bar = bounds("meter-bar", cx);
            let tick = bounds("meter-tick", cx);

            let offset = bar
                .zip(tick)
                .map(|(bar, tick)| f32::from(tick.origin.x - bar.origin.x));
            let width = bar.map(|bar| f32::from(bar.size.width));
            assert_eq!(width, Some(200.), "compact: {compact}");
            assert!(
                offset.is_some_and(|offset| (offset - 50.).abs() < 0.5),
                "compact: {compact}, offset {offset:?}"
            );
        }
    }

    #[gpui::test]
    fn the_bars_are_four_and_five_pixels_high_and_the_tick_sticks_out_two_each_side(
        cx: &mut TestAppContext,
    ) {
        for (compact, height) in [(true, 4.), (false, 5.)] {
            let probe = Probe {
                pace: Some(0.5),
                warning: false,
                compact,
            };
            let cx = setup(probe, cx);

            let bar = bounds("meter-bar", cx).map(|bar| f32::from(bar.size.height));
            let tick = bounds("meter-tick", cx).map(|tick| f32::from(tick.size.height));

            assert_eq!(bar, Some(height), "compact: {compact}");
            assert_eq!(tick, Some(height + 4.), "compact: {compact}");
        }
    }

    #[gpui::test]
    fn the_fill_follows_the_fraction(cx: &mut TestAppContext) {
        let cx = setup(
            Probe {
                pace: None,
                warning: false,
                compact: false,
            },
            cx,
        );

        let fill = bounds("meter-fill", cx).map(|fill| f32::from(fill.size.width));

        assert!(
            fill.is_some_and(|fill| (fill - 122.).abs() < 0.5),
            "{fill:?}"
        );
    }

    #[gpui::test]
    fn no_pace_draws_no_tick(cx: &mut TestAppContext) {
        for compact in [true, false] {
            let probe = Probe {
                pace: None,
                warning: false,
                compact,
            };
            let cx = setup(probe, cx);

            assert!(bounds("meter-bar", cx).is_some(), "compact: {compact}");
            assert!(bounds("meter-tick", cx).is_none(), "compact: {compact}");
        }
    }

    #[gpui::test]
    fn the_warning_shows_only_when_given_and_never_changes_the_height(cx: &mut TestAppContext) {
        let mut heights = Vec::new();

        for warning in [false, true] {
            let probe = Probe {
                pace: Some(0.5),
                warning,
                compact: false,
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
            pace: Some(0.5),
            warning: true,
            compact: false,
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
    fn the_warn_tone_uses_the_warn_color() {
        assert_eq!(UsageTone::Warn.color(), Color::Warn);
        assert_eq!(UsageTone::Normal.color(), Color::Default);
        assert_eq!(UsageTone::default(), UsageTone::Normal);
    }
}
