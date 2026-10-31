use gpui::{
    AnyElement, AnyView, App, BoxShadow, Context, Render, Role, SharedString, Window, black, div,
    point, px,
};

use crate::component::{Component, example, example_group};
use crate::fonts::MONO_FONT;
use crate::prelude::*;

/// A small box of text that GPUI shows next to the pointer. Build it in a tooltip
/// callback with [`Tooltip::text`].
pub struct Tooltip {
    text: SharedString,
}

impl Tooltip {
    pub fn text(text: impl Into<SharedString>, cx: &mut App) -> AnyView {
        let text = text.into();

        cx.new(|_| Self { text }).into()
    }
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();

        div()
            .id("tooltip")
            .role(Role::Tooltip)
            .aria_label(self.text.clone())
            .max_w(px(360.))
            .px(px(8.))
            .py(px(4.))
            .rounded(px(6.))
            .border_1()
            .border_color(colors.line)
            .bg(colors.panel)
            .text_color(colors.fg)
            .font_family(MONO_FONT)
            .text_size(px(11.))
            .shadow(vec![BoxShadow {
                color: black().opacity(0.28),
                offset: point(px(0.), px(6.)),
                blur_radius: px(16.),
                spread_radius: px(0.),
                inset: false,
            }])
            .child(self.text.clone())
    }
}

impl Component for Tooltip {
    fn name() -> &'static str {
        "Tooltip"
    }

    fn preview(_window: &mut Window, cx: &mut App) -> AnyElement {
        v_flex()
            .gap_6()
            .child(example_group(
                "Link address",
                [example(
                    "Shown on hover",
                    Tooltip::text("https://example.com/docs", cx),
                )],
            ))
            .into_any_element()
    }
}
