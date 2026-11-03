use gpui::{AnyElement, App, ElementId, Role, SharedString, Window, div};

use crate::component::{Component, example, example_group};
use crate::prelude::*;

/// A node that screen readers speak when its value changes. macOS speaks a node only when
/// it has a value and a live setting.
#[derive(IntoElement)]
#[must_use]
pub struct LiveRegion {
    id: ElementId,
    role: Role,
    live: Live,
    value: SharedString,
    children: Vec<AnyElement>,
}

impl LiveRegion {
    /// A status message, spoken when the VoiceOver user is idle.
    pub fn status(id: impl Into<ElementId>, value: impl Into<SharedString>) -> Self {
        Self::new(id, Role::Status, Live::Polite, value)
    }

    /// An error or warning, spoken at once.
    pub fn alert(id: impl Into<ElementId>, value: impl Into<SharedString>) -> Self {
        Self::new(id, Role::Alert, Live::Assertive, value)
    }

    fn new(
        id: impl Into<ElementId>,
        role: Role,
        live: Live,
        value: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            role,
            live,
            value: value.into(),
            children: Vec::new(),
        }
    }
}

impl ParentElement for LiveRegion {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for LiveRegion {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .id(self.id)
            .role(self.role)
            .aria_value(self.value)
            .children(self.children)
            .a11y_flags()
            .live(self.live)
    }
}

impl Component for LiveRegion {
    fn name() -> &'static str {
        "LiveRegion"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        v_flex()
            .gap_6()
            .child(example_group(
                "Kinds",
                [
                    example(
                        "Status",
                        LiveRegion::status("preview-status", "Checks passed")
                            .child(Label::new("Checks passed")),
                    ),
                    example(
                        "Alert",
                        LiveRegion::alert("preview-alert", "Merge failed")
                            .child(Label::new("Merge failed").color(Color::Muted)),
                    ),
                ],
            ))
            .into_any_element()
    }
}
