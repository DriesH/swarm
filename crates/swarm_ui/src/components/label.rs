use gpui::{AnyElement, App, FontWeight, Pixels, SharedString, Window, div, px, relative};

use crate::component::{Component, example, example_group};
use crate::fonts::{MONO_FONT, UI_FONT};
use crate::prelude::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LabelSize {
    /// 11px: meta, counts, pills.
    XSmall,
    /// 12px: section titles, small controls.
    Small,
    /// 13px: body text.
    #[default]
    Default,
    /// 14px: app title.
    Large,
    /// 20px: page headings.
    Title,
}

impl LabelSize {
    pub fn pixels(self) -> Pixels {
        match self {
            Self::XSmall => px(11.),
            Self::Small => px(12.),
            Self::Default => px(13.),
            Self::Large => px(14.),
            Self::Title => px(20.),
        }
    }
}

/// One line of text. Plain text is not read by VoiceOver on its own: the
/// interactive parent carries the accessible label.
#[derive(IntoElement)]
#[must_use]
pub struct Label {
    text: SharedString,
    size: LabelSize,
    color: Option<Color>,
    weight: FontWeight,
    mono: bool,
    truncate: bool,
}

impl Label {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            size: LabelSize::Default,
            color: Some(Color::Default),
            weight: FontWeight::NORMAL,
            mono: false,
            truncate: false,
        }
    }

    pub fn size(mut self, size: LabelSize) -> Self {
        self.size = size;
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }

    /// Uses the parent's text color, so a parent hover style can change it.
    pub fn inherit_color(mut self) -> Self {
        self.color = None;
        self
    }

    pub fn weight(mut self, weight: FontWeight) -> Self {
        self.weight = weight;
        self
    }

    /// Geist Mono, for paths, branches, counts and model names.
    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    /// Cuts the text with an ellipsis when it does not fit.
    pub fn truncate(mut self) -> Self {
        self.truncate = true;
        self
    }
}

impl RenderOnce for Label {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .font_family(if self.mono { MONO_FONT } else { UI_FONT })
            .text_size(self.size.pixels())
            .line_height(relative(1.4))
            .font_weight(self.weight)
            .when_some(self.color, |label, color| label.text_color(color.hsla(cx)))
            .whitespace_nowrap()
            .when(self.truncate, |label| label.min_w_0().truncate())
            .child(self.text)
    }
}

impl Component for Label {
    fn name() -> &'static str {
        "Label"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        v_flex()
            .gap_6()
            .child(example_group(
                "Sizes",
                [
                    ("11", LabelSize::XSmall),
                    ("12", LabelSize::Small),
                    ("13", LabelSize::Default),
                    ("14", LabelSize::Large),
                    ("20", LabelSize::Title),
                ]
                .into_iter()
                .map(|(label, size)| example(label, Label::new("terminal-renderer").size(size))),
            ))
            .child(example_group(
                "Weights",
                [
                    ("Regular", FontWeight::NORMAL),
                    ("Medium", FontWeight::MEDIUM),
                    ("Semibold", FontWeight::SEMIBOLD),
                ]
                .into_iter()
                .map(|(label, weight)| example(label, Label::new("orchestrator").weight(weight))),
            ))
            .child(example_group(
                "Colors",
                [
                    ("Default", Color::Default),
                    ("Muted", Color::Muted),
                    ("Faint", Color::Faint),
                    ("Accent", Color::Accent),
                    ("Ok", Color::Ok),
                    ("Warn", Color::Warn),
                    ("Add", Color::Add),
                    ("Del", Color::Del),
                ]
                .into_iter()
                .map(|(label, color)| example(label, Label::new("pty-io").color(color))),
            ))
            .child(example_group(
                "Mono",
                [
                    example(
                        "Meta",
                        Label::new("opus 5.5")
                            .mono()
                            .size(LabelSize::XSmall)
                            .color(Color::Faint),
                    ),
                    example(
                        "Branch",
                        Label::new("swarm/terminal-renderer")
                            .mono()
                            .size(LabelSize::Small),
                    ),
                    example(
                        "Truncated",
                        div().w(px(120.)).flex().child(
                            Label::new("crates/swarm-term/src/element.rs")
                                .mono()
                                .size(LabelSize::Small)
                                .truncate(),
                        ),
                    ),
                ],
            ))
            .into_any_element()
    }
}
