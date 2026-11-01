use gpui::{
    AnyElement, App, ClickEvent, ElementId, FocusHandle, FontWeight, Hsla, Pixels, Role,
    SharedString, Toggled, Window, div, px,
};

use crate::component::{Component, example, example_group};
use crate::contrast::composite;
use crate::prelude::*;
use crate::styles::{filled_focus_ring, focus_ring};
use crate::theme::ThemeColors;
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
    /// Filled with `del`: an action that throws something away.
    Destructive,
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

pub(crate) struct ButtonColors {
    pub(crate) background: Hsla,
    pub(crate) text: Hsla,
    pub(crate) border: Hsla,
    pub(crate) hover_background: Hsla,
    pub(crate) hover_text: Hsla,
}

impl ButtonVariant {
    fn colors(self, cx: &App) -> ButtonColors {
        self.colors_in(cx.theme().colors())
    }

    pub(crate) fn colors_in(self, colors: &ThemeColors) -> ButtonColors {
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
            Self::Destructive => ButtonColors {
                background: colors.del,
                text: colors.ground,
                border: colors.del,
                // Solid, and toward `fg`: the label keeps its contrast in both themes.
                hover_background: composite(colors.del, colors.fg.opacity(0.12)),
                hover_text: colors.ground,
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
    accessible_label: Option<SharedString>,
    variant: ButtonVariant,
    size: ButtonSize,
    icon: Option<IconName>,
    end_slot: Option<AnyElement>,
    toggle_state: Option<bool>,
    full_width: bool,
    debug_selector: Option<String>,
    focus_handle: Option<FocusHandle>,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            accessible_label: None,
            variant: ButtonVariant::default(),
            size: ButtonSize::default(),
            icon: None,
            end_slot: None,
            toggle_state: None,
            full_width: false,
            debug_selector: None,
            focus_handle: None,
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

    /// What VoiceOver reads when the visible label does not say enough, for example
    /// "Show diff, Cargo.lock". It should start with the visible label.
    pub fn accessible_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessible_label = Some(label.into());
        self
    }

    /// Icon before the label.
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Content after the label, for example a diff count.
    pub fn end_slot(mut self, end_slot: impl IntoElement) -> Self {
        self.end_slot = Some(end_slot.into_any_element());
        self
    }

    /// Fills the container width and centers the content.
    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    /// Name for `VisualTestContext::debug_bounds`. Only recorded in test builds.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }

    /// Uses `handle` as the button's focus, so the owner can move focus to it. Create
    /// the handle with `tab_stop(true)` to keep the button in the Tab order.
    pub fn track_focus(mut self, handle: &FocusHandle) -> Self {
        self.focus_handle = Some(handle.clone());
        self
    }
}

/// On or off state, for toggle buttons.
impl Toggleable for Button {
    fn toggle_state(mut self, toggled: bool) -> Self {
        self.toggle_state = Some(toggled);
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
        let mut colors = self.variant.colors(cx);
        let (accent, ground) = (cx.theme().colors().accent, cx.theme().colors().ground);
        let filled = matches!(
            self.variant,
            ButtonVariant::Primary | ButtonVariant::Destructive
        );
        let toggled = self.toggle_state == Some(true);

        let weight = if self.variant == ButtonVariant::Primary && self.size == ButtonSize::Medium {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::MEDIUM
        };

        if toggled {
            let theme_colors = cx.theme().colors();

            colors.background = theme_colors.active;
            colors.border = theme_colors.line;
            colors.text = Color::Default.hsla(cx);
            colors.hover_text = colors.text;
        }

        let size = self.size;

        div()
            .id(self.id)
            .role(Role::Button)
            .aria_label(
                self.accessible_label
                    .clone()
                    .unwrap_or_else(|| self.label.clone()),
            )
            .when_some(self.toggle_state, |button, on| {
                button.aria_toggled(if on { Toggled::True } else { Toggled::False })
            })
            .when_some(self.debug_selector, |button, selector| {
                button.debug_selector(move || selector)
            })
            .flex()
            .map(|button| {
                if self.full_width {
                    button.w_full()
                } else {
                    button.flex_none()
                }
            })
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
            .when_some(self.end_slot, |button, end_slot| {
                button.child(div().ml(px(2.)).child(end_slot))
            })
            .map(|button| {
                if self.disabled {
                    button.opacity(0.5).cursor_default()
                } else {
                    button
                        .map(|button| match &self.focus_handle {
                            Some(handle) => button.track_focus(handle),
                            None => button.focusable().tab_stop(true),
                        })
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
            ("Destructive", ButtonVariant::Destructive),
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
                "Toggle",
                [
                    example(
                        "Off",
                        Button::new("toggle-off", "Changes").toggle_state(false),
                    ),
                    example("On", Button::new("toggle-on", "Changes").toggle_state(true)),
                ],
            ))
            .child(example_group(
                "Full width",
                [example(
                    "Secondary",
                    div()
                        .w(px(240.))
                        .child(Button::new("full-width", "Open a project").full_width()),
                )],
            ))
            .child(example_group(
                "With end slot",
                [example(
                    "Diff counts",
                    Button::new("end-slot", "Changes").end_slot(
                        h_flex()
                            .gap(px(4.))
                            .child(Label::new("+12").mono().color(Color::Ok))
                            .child(Label::new("\u{2212}3").mono().color(Color::Warn)),
                    ),
                )],
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
