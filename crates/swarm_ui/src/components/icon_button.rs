use gpui::{AnyElement, App, ClickEvent, ElementId, Pixels, Role, SharedString, Window, div, px};

use crate::a11y::{accessible, ignore_press};
use crate::component::{Component, example, example_group};
use crate::contrast::composite;
use crate::prelude::*;
use crate::styles::{filled_focus_ring, focus_ring};
use crate::traits::ClickHandler;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconButtonSize {
    /// 20px square, 14px icon: the add-comment button in a diff gutter.
    XSmall,
    /// 24px square, 14px icon: tile header actions.
    #[default]
    Small,
    /// 30px square, 16px icon: toolbars.
    Medium,
}

impl IconButtonSize {
    /// The side of the button, in pixels.
    pub fn square(self) -> Pixels {
        match self {
            Self::XSmall => px(20.),
            Self::Small => px(24.),
            Self::Medium => px(30.),
        }
    }

    fn icon(self) -> IconSize {
        match self {
            Self::XSmall | Self::Small => IconSize::Medium,
            Self::Medium => IconSize::Large,
        }
    }

    fn radius(self) -> Pixels {
        match self {
            Self::XSmall | Self::Small => px(5.),
            Self::Medium => px(7.),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconButtonStyle {
    /// No fill until hovered.
    #[default]
    Subtle,
    /// An accent fill with the icon in the ground color.
    Filled,
}

/// A button that shows only an icon. The label is required: VoiceOver reads it.
#[derive(IntoElement)]
#[must_use]
pub struct IconButton {
    id: ElementId,
    icon: IconName,
    label: SharedString,
    size: IconButtonSize,
    style: IconButtonStyle,
    debug_selector: Option<String>,
    color: Color,
    toggle_state: Option<bool>,
    expanded: Option<bool>,
    reveal_group: Option<SharedString>,
    tab_stop: bool,
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
            style: IconButtonStyle::default(),
            color: Color::Faint,
            toggle_state: None,
            expanded: None,
            reveal_group: None,
            tab_stop: true,
            debug_selector: None,
            disabled: false,
            on_click: None,
        }
    }

    pub fn size(mut self, size: IconButtonSize) -> Self {
        self.size = size;
        self
    }

    pub fn style(mut self, style: IconButtonStyle) -> Self {
        self.style = style;
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    /// Name for `VisualTestContext::debug_bounds`. Only recorded in test builds.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }

    /// Hides the button until its parent `group` is hovered or the button has keyboard
    /// focus. It stays in the tab order.
    pub fn reveal_with_group(mut self, group: impl Into<SharedString>) -> Self {
        self.reveal_group = Some(group.into());
        self
    }

    /// Whether the popup this button opens is open. VoiceOver reads it on the button.
    pub fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = Some(expanded);
        self
    }

    /// Takes the button out of the Tab order, for one of many repeated buttons that
    /// has a keyboard route elsewhere.
    pub fn skip_tab_stop(mut self) -> Self {
        self.tab_stop = false;
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
        let (active, accent, ground) = (colors.active, colors.accent, colors.ground);
        let filled = self.style == IconButtonStyle::Filled;
        // `shadow` is translucent black, so the hovered fill is darker in both themes.
        let hover_fill = if filled {
            composite(accent, colors.shadow)
        } else {
            active
        };

        let pressed = self.toggle_state == Some(true);
        let reveals = self.reveal_group.is_some();

        let color = if filled {
            Color::Inverse
        } else if pressed {
            Color::Default
        } else {
            self.color
        };

        let disabled = self.disabled;

        accessible(div().id(self.id), Role::Button, self.label)
            .when_some(self.debug_selector, |element, selector| {
                element.debug_selector(move || selector)
            })
            .when_some(self.expanded, |button, expanded| {
                button.aria_expanded(expanded)
            })
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
            .when(filled, |button| button.bg(accent))
            .child(Icon::new(self.icon).size(self.size.icon()).color(color))
            .focusable()
            .tab_stop(self.tab_stop && !disabled)
            .focus_visible(|style| {
                let style = if filled {
                    filled_focus_ring(style, accent, ground)
                } else {
                    focus_ring(style, accent)
                };

                if reveals { style.opacity(1.) } else { style }
            })
            .map(|button| {
                if disabled {
                    ignore_press(button.opacity(0.5))
                } else {
                    button
                        .cursor_pointer()
                        .hover(|style| style.bg(hover_fill))
                        .when_some(self.reveal_group.clone(), |button, group| {
                            button
                                .opacity(0.)
                                .group_hover(group, |style| style.opacity(1.))
                        })
                        .when_some(self.on_click, |button, handler| button.on_click(handler))
                }
            })
            .a11y_flags()
            .disabled(disabled)
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
                "Filled",
                [example(
                    "Add comment",
                    IconButton::new("filled", IconName::Plus, "Comment on line 12")
                        .size(IconButtonSize::XSmall)
                        .style(IconButtonStyle::Filled),
                )],
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

#[cfg(test)]
mod tests {
    use gpui::{Context, Render, TestAppContext, size};

    use super::*;

    struct Buttons;

    impl Render for Buttons {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            h_flex()
                .gap(px(8.))
                .child(
                    IconButton::new("xsmall", IconName::Plus, "XSmall")
                        .size(IconButtonSize::XSmall)
                        .debug_selector(|| "xsmall".to_owned()),
                )
                .child(
                    IconButton::new("small", IconName::Plus, "Small")
                        .debug_selector(|| "small".to_owned()),
                )
                .child(
                    IconButton::new("medium", IconName::Plus, "Medium")
                        .size(IconButtonSize::Medium)
                        .debug_selector(|| "medium".to_owned()),
                )
        }
    }

    #[test]
    fn buttons_are_20_24_and_30_pixels_square() {
        let mut app = TestAppContext::single();
        app.update(crate::theme::init);
        let (_, cx) = app.add_window_view(|_, _| Buttons);
        cx.run_until_parked();

        let xsmall = cx.debug_bounds("xsmall").map(|bounds| bounds.size);
        let small = cx.debug_bounds("small").map(|bounds| bounds.size);
        let medium = cx.debug_bounds("medium").map(|bounds| bounds.size);

        assert_eq!(xsmall, Some(size(px(20.), px(20.))));
        assert_eq!(small, Some(size(px(24.), px(24.))));
        assert_eq!(medium, Some(size(px(30.), px(30.))));
    }

    #[test]
    fn the_filled_icon_reaches_3_to_1_on_the_hovered_fill() {
        use crate::contrast::{GRAPHIC_AA, contrast_ratio};
        use crate::theme::{Accent, Theme};

        for theme in [
            Theme::light(),
            Theme::dark(),
            Theme::light().with_accent(Accent::Honey),
            Theme::dark().with_accent(Accent::Honey),
        ] {
            let colors = theme.colors();
            let hovered = composite(colors.accent, colors.shadow);
            let ratio = contrast_ratio(colors.ground, hovered);

            assert!(ratio >= GRAPHIC_AA, "{}: {ratio:.2}", theme.name());
        }
    }

    #[test]
    fn icons_are_14_and_16_pixels() {
        assert_eq!(IconButtonSize::XSmall.icon().pixels(), px(14.));
        assert_eq!(IconButtonSize::Small.icon().pixels(), px(14.));
        assert_eq!(IconButtonSize::Medium.icon().pixels(), px(16.));
    }
}
