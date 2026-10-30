use gpui::{AnyElement, App, ClickEvent, ElementId, Pixels, Role, SharedString, Window, div, px};

use crate::component::{Component, example, example_group};
use crate::prelude::*;
use crate::styles::focus_ring;
use crate::traits::ClickHandler;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconButtonSize {
    /// 22px square, 12px icon: tile header actions.
    #[default]
    Small,
    /// 28px square, 14px icon: toolbars.
    Medium,
}

impl IconButtonSize {
    fn square(self) -> Pixels {
        match self {
            Self::Small => px(22.),
            Self::Medium => px(28.),
        }
    }

    fn icon(self) -> IconSize {
        match self {
            Self::Small => IconSize::Small,
            Self::Medium => IconSize::Medium,
        }
    }

    fn radius(self) -> Pixels {
        match self {
            Self::Small => px(5.),
            Self::Medium => px(7.),
        }
    }
}

/// A button that shows only an icon. The label is required: VoiceOver reads it.
#[derive(IntoElement)]
#[must_use]
pub struct IconButton {
    id: ElementId,
    icon: IconName,
    label: SharedString,
    size: IconButtonSize,
    color: Color,
    toggle_state: Option<bool>,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

impl IconButton {
    pub fn new(id: impl Into<ElementId>, icon: IconName, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            icon,
            label: label.into(),
            size: IconButtonSize::default(),
            color: Color::Faint,
            toggle_state: None,
            disabled: false,
            on_click: None,
        }
    }

    pub fn size(mut self, size: IconButtonSize) -> Self {
        self.size = size;
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }
}

impl Clickable for IconButton {
    fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl Disableable for IconButton {
    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

/// Pressed state, for toggles such as the sidebar button.
impl Toggleable for IconButton {
    fn toggle_state(mut self, pressed: bool) -> Self {
        self.toggle_state = Some(pressed);
        self
    }
}

impl RenderOnce for IconButton {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (active, accent) = (colors.active, colors.accent);

        let pressed = self.toggle_state == Some(true);

        let color = if pressed { Color::Default } else { self.color };

        div()
            .id(self.id)
            .role(Role::Button)
            .aria_label(self.label)
            .when_some(self.toggle_state, |button, on| {
                button
                    .aria_toggled(if on {
                        gpui::Toggled::True
                    } else {
                        gpui::Toggled::False
                    })
                    .when(on, |button| button.bg(active))
            })
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(self.size.square())
            .rounded(self.size.radius())
            .child(Icon::new(self.icon).size(self.size.icon()).color(color))
            .map(|button| {
                if self.disabled {
                    button.opacity(0.5)
                } else {
                    button
                        .focusable()
                        .tab_stop(true)
                        .cursor_pointer()
                        .hover(|style| style.bg(active))
                        .focus_visible(|style| focus_ring(style, accent))
                        .when_some(self.on_click, |button, handler| button.on_click(handler))
                }
            })
    }
}

impl Component for IconButton {
    fn name() -> &'static str {
        "IconButton"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        v_flex()
            .gap_6()
            .child(example_group(
                "Small",
                [
                    example(
                        "Minimize",
                        IconButton::new("minimize", IconName::Minimize, "Move pty-io to tray"),
                    ),
                    example(
                        "Close",
                        IconButton::new("close", IconName::Close, "Stop cargo test"),
                    ),
                    example(
                        "Restore",
                        IconButton::new("restore", IconName::Restore, "Show bench"),
                    ),
                ],
            ))
            .child(example_group(
                "Medium",
                [
                    example(
                        "Sidebar",
                        IconButton::new("sidebar", IconName::Sidebar, "Toggle sidebar")
                            .size(IconButtonSize::Medium),
                    ),
                    example(
                        "Pressed",
                        IconButton::new("sidebar-on", IconName::Sidebar, "Toggle sidebar")
                            .size(IconButtonSize::Medium)
                            .toggle_state(true),
                    ),
                    example(
                        "Search",
                        IconButton::new("search", IconName::Search, "Search")
                            .size(IconButtonSize::Medium)
                            .color(Color::Muted),
                    ),
                    example(
                        "New task",
                        IconButton::new("new-task", IconName::Plus, "New task")
                            .size(IconButtonSize::Medium),
                    ),
                ],
            ))
            .child(example_group(
                "Disabled",
                [example(
                    "Plus",
                    IconButton::new("disabled", IconName::Plus, "Add project").disabled(true),
                )],
            ))
            .into_any_element()
    }
}
