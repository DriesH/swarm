use gpui::{AnyElement, App, ElementId, Role, SharedString, Window, div, px, relative};

use crate::component::{Component, example, example_group};
use crate::prelude::*;

/// One subscription limit: a name, a value such as "42%", a bar and a note under it.
/// VoiceOver reads one label for the whole row.
#[derive(IntoElement)]
#[must_use]
pub struct UsageMeter {
    id: ElementId,
    name: SharedString,
    value: Option<SharedString>,
    fraction: Option<f32>,
    note: Option<SharedString>,
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
            note: None,
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

    /// A faint line under the bar, such as why the figure is old.
    pub fn note(mut self, note: impl Into<SharedString>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Replaces what VoiceOver reads, which is the name, the value and the note.
    pub fn accessible_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessible_label = Some(label.into());
        self
    }

    /// Name for `VisualTestContext::debug_bounds`. Only recorded in test builds.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }

    fn spoken(&self) -> SharedString {
        if let Some(label) = &self.accessible_label {
            return label.clone();
        }

        let parts = [Some(&self.name), self.value.as_ref(), self.note.as_ref()];
        let parts: Vec<&str> = parts.into_iter().flatten().map(|part| &**part).collect();

        parts.join(", ").into()
    }
}

impl RenderOnce for UsageMeter {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (track, fill) = (colors.soft, colors.fg);
        let spoken = self.spoken();

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
                    .gap(px(8.))
                    .child(
                        div().flex_1().min_w_0().flex().child(
                            Label::new(self.name)
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                                .truncate(),
                        ),
                    )
                    .children(
                        self.value
                            .map(|value| Label::new(value).mono().size(LabelSize::XSmall)),
                    ),
            )
            .children(self.fraction.map(|fraction| {
                div().h(px(4.)).rounded(px(2.)).bg(track).child(
                    div()
                        .h(px(4.))
                        .w(relative(fraction))
                        .rounded(px(2.))
                        .bg(fill),
                )
            }))
            .children(
                self.note
                    .map(|note| Label::new(note).size(LabelSize::XSmall).color(Color::Faint)),
            )
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
                            UsageMeter::new("usage-bar", "Claude · 5h")
                                .value("42%")
                                .fraction(0.42),
                        ),
                    ),
                    example(
                        "Full",
                        sized(
                            UsageMeter::new("usage-full", "Codex · 5h")
                                .value("100%")
                                .fraction(1.),
                        ),
                    ),
                    example(
                        "With a note",
                        sized(
                            UsageMeter::new("usage-note", "Claude · 5h")
                                .value("42%")
                                .fraction(0.42)
                                .note("Rate limited · 24m"),
                        ),
                    ),
                    example(
                        "No bar",
                        sized(UsageMeter::new("usage-none", "Codex").note("Not logged in")),
                    ),
                ],
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
