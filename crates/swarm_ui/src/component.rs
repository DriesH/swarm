use gpui::{
    AnyElement, App, IntoElement, ParentElement as _, SharedString, Styled as _, Window, px,
};

use crate::prelude::*;

/// A component that can show itself in the gallery.
pub trait Component {
    fn name() -> &'static str;
    fn preview(window: &mut Window, cx: &mut App) -> AnyElement;
}

/// One entry in the component registry.
#[derive(Clone, Copy)]
pub struct ComponentEntry {
    name: &'static str,
    preview: fn(&mut Window, &mut App) -> AnyElement,
}

impl ComponentEntry {
    pub fn of<C: Component>() -> Self {
        Self {
            name: C::name(),
            preview: C::preview,
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn preview(&self, window: &mut Window, cx: &mut App) -> AnyElement {
        (self.preview)(window, cx)
    }
}

/// A titled row of examples.
pub fn example_group(
    title: impl Into<SharedString>,
    examples: impl IntoIterator<Item = AnyElement>,
) -> AnyElement {
    v_flex()
        .gap_3()
        .child(
            Label::new(title)
                .size(LabelSize::Small)
                .weight(gpui::FontWeight::MEDIUM)
                .color(Color::Faint),
        )
        .child(
            h_flex()
                .flex_wrap()
                .items_start()
                .gap_x(px(28.))
                .gap_y(px(20.))
                .children(examples),
        )
        .into_any_element()
}

/// One example with its name under it.
pub fn example(label: impl Into<SharedString>, element: impl IntoElement) -> AnyElement {
    v_flex()
        .gap_2()
        .child(element)
        .child(
            Label::new(label)
                .mono()
                .size(LabelSize::XSmall)
                .color(Color::Faint),
        )
        .into_any_element()
}
