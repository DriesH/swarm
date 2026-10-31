use gpui::{
    AnyElement, App, ElementId, Pixels, Role, ScrollHandle, SharedString, Window, div, point, px,
};

use crate::component::{Component, example};
use crate::fonts::MONO_FONT;
use crate::prelude::*;
use crate::styles::focus_ring;

const LINE_HEIGHT: Pixels = px(16.);

/// A block of command output in the mono font. It scrolls when it is taller than
/// its maximum height. It takes keyboard focus; arrow keys, Page Up, Page Down, Home
/// and End scroll it.
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

    /// Name for `VisualTestContext::debug_bounds`. Only recorded in test builds. The text
    /// inside is recorded as `<name>-text`.
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

/// Moves the scroll offset for a navigation key. Returns false for any other key.
fn scroll_for_key(scroll: &ScrollHandle, key: &str) -> bool {
    let top = scroll.offset();
    let bottom = -scroll.max_offset().y;
    let page = (scroll.bounds().size.height - LINE_HEIGHT).max(LINE_HEIGHT);

    let target = match key {
        "up" => top.y + LINE_HEIGHT,
        "down" => top.y - LINE_HEIGHT,
        "pageup" => top.y + page,
        "pagedown" => top.y - page,
        "home" => px(0.),
        "end" => bottom,
        _ => return false,
    };

    scroll.set_offset(point(top.x, target.min(px(0.)).max(bottom)));

    true
}

impl RenderOnce for OutputBlock {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let scroll = window
            .use_keyed_state((self.id.clone(), "scroll"), cx, |_, _| ScrollHandle::new())
            .read(cx)
            .clone();
        let on_key = scroll.clone();
        let text_selector = self
            .debug_selector
            .as_ref()
            .map(|selector| format!("{selector}-text"));

        let colors = cx.theme().colors();
        let accent = colors.accent;

        div()
            .id(self.id)
            .when_some(self.debug_selector, |block, selector| {
                block.debug_selector(move || selector)
            })
            .role(Role::Log)
            .aria_label(self.label)
            .aria_value(self.text.clone())
            .focusable()
            .tab_stop(true)
            .track_scroll(&scroll)
            .on_key_down(move |event, window, cx| {
                if scroll_for_key(&on_key, event.keystroke.key.as_str()) {
                    window.refresh();
                    cx.stop_propagation();
                }
            })
            .w_full()
            .max_h(self.max_height)
            .overflow_y_scroll()
            .p(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(colors.line)
            .bg(colors.raised)
            .focus_visible(move |style| focus_ring(style, accent))
            .font_family(MONO_FONT)
            .text_size(px(11.))
            .line_height(LINE_HEIGHT)
            .text_color(self.tone.hsla(cx))
            .child(
                div()
                    .when_some(text_selector, |text, selector| {
                        text.debug_selector(move || selector)
                    })
                    .child(self.text),
            )
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
