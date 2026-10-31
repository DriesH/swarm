use gpui::{AnyElement, App, FontWeight, Hsla, SharedString, Window, div, px};

use crate::component::{Component, example, example_group};
use crate::prelude::*;

/// The color pair of a [`Tag`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TagTone {
    /// Muted text on a soft fill.
    #[default]
    Neutral,
    Accent,
    Ok,
    Warn,
    Del,
}

impl TagTone {
    pub const ALL: [Self; 5] = [Self::Neutral, Self::Accent, Self::Ok, Self::Warn, Self::Del];

    /// The text color and the fill.
    pub fn colors(self, cx: &App) -> (Hsla, Hsla) {
        let colors = cx.theme().colors();

        match self {
            Self::Neutral => (colors.muted, colors.soft),
            Self::Accent => (colors.accent, colors.accent_bg),
            Self::Ok => (colors.ok, colors.ok_bg),
            Self::Warn => (colors.warn, colors.warn_bg),
            Self::Del => (colors.del, colors.del_bg),
        }
    }
}

/// A small text badge for a state, such as "Pending".
#[derive(IntoElement)]
#[must_use]
pub struct Tag {
    label: SharedString,
    tone: TagTone,
}

impl Tag {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            tone: TagTone::default(),
        }
    }

    pub fn tone(mut self, tone: TagTone) -> Self {
        self.tone = tone;
        self
    }
}

impl RenderOnce for Tag {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (text, background) = self.tone.colors(cx);

        div()
            .flex_none()
            .px(px(6.))
            .py(px(1.))
            .rounded(px(5.))
            .bg(background)
            .child(
                Label::new(self.label)
                    .size(LabelSize::XSmall)
                    .weight(FontWeight::MEDIUM)
                    .color(Color::Custom(text)),
            )
    }
}

impl Component for Tag {
    fn name() -> &'static str {
        "Tag"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        v_flex()
            .gap_6()
            .child(example_group(
                "Tones",
                [
                    example("Neutral", Tag::new("Resolved")),
                    example("Accent", Tag::new("Sent 40s ago").tone(TagTone::Accent)),
                    example("Ok", Tag::new("Added").tone(TagTone::Ok)),
                    example("Warn", Tag::new("Pending").tone(TagTone::Warn)),
                    example("Del", Tag::new("Deleted").tone(TagTone::Del)),
                ],
            ))
            .into_any_element()
    }
}
