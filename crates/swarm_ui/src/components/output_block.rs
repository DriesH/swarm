use gpui::{AnyElement, App, ElementId, Pixels, Role, SharedString, Window, div, px};

use crate::component::{Component, example};
use crate::fonts::MONO_FONT;
use crate::prelude::*;

/// A block of command output in the mono font. It scrolls when it is taller than
/// its maximum height.
#[derive(IntoElement)]
#[must_use]
pub struct OutputBlock {
    id: ElementId,
    label: SharedString,
    text: SharedString,
    max_height: Pixels,
    tone: Color,
    debug_selector: Option<String>,
}

impl OutputBlock {
    /// `label` names the block for VoiceOver, for example "Push output".
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        text: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            text: text.into(),
            max_height: px(140.),
            tone: Color::Default,
            debug_selector: None,
        }
    }

    pub fn max_height(mut self, height: Pixels) -> Self {
        self.max_height = height;
        self
    }

    /// Name for `VisualTestContext::debug_bounds`. Only recorded in test builds.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }

    /// The text color, for example `Color::Del` for an error.
    pub fn tone(mut self, tone: Color) -> Self {
        self.tone = tone;
        self
    }
}

impl RenderOnce for OutputBlock {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();

        div()
            .id(self.id)
            .when_some(self.debug_selector, |block, selector| {
                block.debug_selector(move || selector)
            })
            .role(Role::Log)
            .aria_label(self.label)
            .aria_value(self.text.clone())
            .w_full()
            .max_h(self.max_height)
            .overflow_y_scroll()
            .p(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(colors.line)
            .bg(colors.raised)
            .font_family(MONO_FONT)
            .text_size(px(11.))
            .line_height(px(16.))
            .text_color(self.tone.hsla(cx))
            .child(self.text)
    }
}

impl Component for OutputBlock {
    fn name() -> &'static str {
        "OutputBlock"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        let long: String = (1..=14)
            .map(|line| format!("remote: line {line} of the push hook output"))
            .collect::<Vec<_>>()
            .join("\n");

        v_flex()
            .gap_6()
            .child(example(
                "Short",
                div().w(px(300.)).child(OutputBlock::new(
                    "short",
                    "Output",
                    "error: failed to push some refs",
                )),
            ))
            .child(example(
                "Scrolls",
                div().w(px(300.)).child(
                    OutputBlock::new("long", "Push output", long)
                        .max_height(px(110.))
                        .tone(Color::Del),
                ),
            ))
            .into_any_element()
    }
}
