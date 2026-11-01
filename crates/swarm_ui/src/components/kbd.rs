use gpui::{AnyElement, App, ElementId, Role, SharedString, Window, div, px};

use crate::component::{Component, example, example_group};
use crate::fonts::MONO_FONT;
use crate::prelude::*;

/// A keyboard shortcut, one box per key.
#[derive(IntoElement)]
#[must_use]
pub struct Kbd {
    id: ElementId,
    keys: Vec<SharedString>,
}

impl Kbd {
    /// `id` must be unique in the window: it identifies the element for VoiceOver.
    pub fn new(
        id: impl Into<ElementId>,
        keys: impl IntoIterator<Item = impl Into<SharedString>>,
    ) -> Self {
        Self {
            id: id.into(),
            keys: keys.into_iter().map(Into::into).collect(),
        }
    }

    fn spoken(&self) -> String {
        self.keys
            .iter()
            .map(|key| match key.as_ref() {
                "\u{2318}" => "Command",
                "\u{21e7}" => "Shift",
                "\u{2325}" => "Option",
                "\u{2303}" => "Control",
                "\u{21b5}" => "Return",
                "esc" => "Escape",
                "\u{232b}" => "Delete",
                "\u{2191}" => "Up Arrow",
                "\u{2193}" => "Down Arrow",
                "\u{2190}" => "Left Arrow",
                "\u{2192}" => "Right Arrow",
                other => other,
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl RenderOnce for Kbd {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (line, raised) = (colors.line, colors.raised);
        let muted = Color::Muted.hsla(cx);
        let spoken = self.spoken();

        h_flex()
            .id(self.id)
            .role(Role::Label)
            .aria_label(spoken)
            .flex_none()
            .gap(px(3.))
            .children(self.keys.into_iter().map(|key| {
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .min_w(px(18.))
                    .h(px(20.))
                    .px(px(5.))
                    .rounded(px(5.))
                    .border_1()
                    .border_color(line)
                    .bg(raised)
                    .font_family(MONO_FONT)
                    .text_size(px(11.))
                    .text_color(muted)
                    .whitespace_nowrap()
                    .child(key)
            }))
    }
}

impl Component for Kbd {
    fn name() -> &'static str {
        "Kbd"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        v_flex()
            .gap_6()
            .child(example_group(
                "Shortcuts",
                [
                    example("Palette", Kbd::new("kbd-palette", ["\u{2318}", "K"])),
                    example(
                        "Rename",
                        Kbd::new("kbd-rename", ["\u{2318}", "\u{21e7}", "R"]),
                    ),
                    example("Dismiss", Kbd::new("kbd-dismiss", ["esc"])),
                    example("Settings", Kbd::new("kbd-settings", ["\u{2318}", ","])),
                ],
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::Kbd;

    #[test]
    fn spoken_label_names_modifier_keys() {
        let kbd = Kbd::new("kbd", ["\u{2318}", "\u{21e7}", "R"]);

        assert_eq!(kbd.spoken(), "Command Shift R");
    }

    #[test]
    fn spoken_label_names_the_arrow_keys() {
        let kbd = Kbd::new("kbd", ["\u{2191}", "\u{2193}", "\u{2190}", "\u{2192}"]);

        assert_eq!(kbd.spoken(), "Up Arrow Down Arrow Left Arrow Right Arrow");
    }

    #[test]
    fn spoken_label_names_escape_and_return() {
        let kbd = Kbd::new("kbd", ["esc", "\u{21b5}"]);

        assert_eq!(kbd.spoken(), "Escape Return");
    }
}
