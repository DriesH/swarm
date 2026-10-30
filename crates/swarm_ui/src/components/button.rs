use gpui::{
    AnyElement, App, ClickEvent, ElementId, FontWeight, Hsla, Pixels, Role, SharedString, Window,
    div, px,
};

use crate::component::{Component, example, example_group};
use crate::prelude::*;
use crate::styles::{filled_focus_ring, focus_ring};
use crate::traits::ClickHandler;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonVariant {
    /// Filled with `fg`: the main action of a screen.
    Primary,
    /// Raised with a border.
    #[default]
    Secondary,
    /// Text only, for quiet actions.
    Ghost,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonSize {
    /// 28px high, 12px text.
    #[default]
    Small,
    /// 34px high, 13px text.
    Medium,
}

impl ButtonSize {
    fn height(self) -> Pixels {
        match self {
            Self::Small => px(28.),
            Self::Medium => px(34.),
        }
    }

    fn padding(self) -> Pixels {
        match self {
            Self::Small => px(12.),
            Self::Medium => px(16.),
        }
    }

    fn radius(self) -> Pixels {
        match self {
            Self::Small => px(7.),
            Self::Medium => px(8.),
        }
    }

    fn text(self) -> LabelSize {
        match self {
            Self::Small => LabelSize::Small,
            Self::Medium => LabelSize::Default,
        }
    }

    fn icon(self) -> IconSize {
        match self {
            Self::Small => IconSize::Small,
            Self::Medium => IconSize::Medium,
        }
    }
}

struct ButtonColors {
    background: Hsla,
    text: Hsla,
    border: Hsla,
    hover_background: Hsla,
    hover_text: Hsla,
}

impl ButtonVariant {
    fn colors(self, cx: &App) -> ButtonColors {
        let colors = cx.theme().colors();

        match self {
            Self::Primary => ButtonColors {
                background: colors.fg,
                text: colors.ground,
                border: colors.fg,
                hover_background: colors.fg.opacity(0.88),
                hover_text: colors.ground,
            },
            Self::Secondary => ButtonColors {
                background: colors.raised,
                text: colors.fg,
                border: colors.line,
                hover_background: colors.active,
                hover_text: colors.fg,
            },
            Self::Ghost => ButtonColors {
                background: gpui::transparent_black(),
                text: colors.muted,
                border: gpui::transparent_black(),
                hover_background: colors.active,
                hover_text: colors.fg,
            },
        }
    }
}

/// A text button. The label never wraps.
#[derive(IntoElement)]
#[must_use]
pub struct Button {
    id: ElementId,
    label: SharedString,
    variant: ButtonVariant,
    size: ButtonSize,
    icon: Option<IconName>,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            variant: ButtonVariant::default(),
            size: ButtonSize::default(),
            icon: None,
            disabled: false,
            on_click: None,
        }
    }

    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    /// Icon before the label.
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }
}

impl Clickable for Button {
    fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl Disableable for Button {
    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = self.variant.colors(cx);
        let (accent, ground) = (cx.theme().colors().accent, cx.theme().colors().ground);
        let filled = self.variant == ButtonVariant::Primary;

        let weight = if self.variant == ButtonVariant::Primary && self.size == ButtonSize::Medium {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::MEDIUM
        };

        let size = self.size;

        div()
            .id(self.id)
            .role(Role::Button)
            .aria_label(self.label.clone())
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .h(size.height())
            .px(size.padding())
            .rounded(size.radius())
            .border_1()
            .border_color(colors.border)
            .bg(colors.background)
            .text_color(colors.text)
            .whitespace_nowrap()
            .when_some(self.icon, |button, icon| {
                button.child(
                    Icon::new(icon)
                        .size(size.icon())
                        .color(Color::Custom(colors.text)),
                )
            })
            .child(
                Label::new(self.label)
                    .size(size.text())
                    .weight(weight)
                    .inherit_color(),
            )
            .map(|button| {
                if self.disabled {
                    button.opacity(0.5).cursor_default()
                } else {
                    button
                        .focusable()
                        .tab_stop(true)
                        .cursor_pointer()
                        .hover(|style| {
                            style
                                .bg(colors.hover_background)
                                .text_color(colors.hover_text)
                        })
                        .focus_visible(|style| {
                            if filled {
                                filled_focus_ring(style, accent, ground)
                            } else {
                                focus_ring(style, accent)
                            }
                        })
                        .when_some(self.on_click, |button, handler| button.on_click(handler))
                }
            })
    }
}

impl Component for Button {
    fn name() -> &'static str {
        "Button"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        let variants = [
            ("Primary", ButtonVariant::Primary),
            ("Secondary", ButtonVariant::Secondary),
            ("Ghost", ButtonVariant::Ghost),
        ];

        v_flex()
            .gap_6()
            .child(example_group(
                "Small",
                variants
                    .into_iter()
                    .enumerate()
                    .map(|(index, (label, variant))| {
                        example(
                            label,
                            Button::new(("small", index), "Review").variant(variant),
                        )
                    }),
            ))
            .child(example_group(
                "Medium",
                variants
                    .into_iter()
                    .enumerate()
                    .map(|(index, (label, variant))| {
                        example(
                            label,
                            Button::new(("medium", index), "Open a project")
                                .variant(variant)
                                .size(ButtonSize::Medium),
                        )
                    }),
            ))
            .child(example_group(
                "With icon",
                [
                    example(
                        "Ghost",
                        Button::new("add-check", "Add check")
                            .variant(ButtonVariant::Ghost)
                            .icon(IconName::Plus),
                    ),
                    example(
                        "Secondary",
                        Button::new("terminal", "Terminal").icon(IconName::Terminal),
                    ),
                    example(
                        "Primary",
                        Button::new("create-pr", "Create PR")
                            .variant(ButtonVariant::Primary)
                            .icon(IconName::PullRequest),
                    ),
                ],
            ))
            .child(example_group(
                "Disabled",
                [example(
                    "Secondary",
                    Button::new("disabled", "Run checks now").disabled(true),
                )],
            ))
            .child(example_group(
                "Narrow container",
                [example(
                    "No wrap",
                    div()
                        .w(px(60.))
                        .flex()
                        .child(Button::new("narrow", "Skip for now")),
                )],
            ))
            .into_any_element()
    }
}
