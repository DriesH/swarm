use std::rc::Rc;

use gpui::{
    AnyElement, App, ElementId, Role, SharedString, StyleRefinement, Styled as _, Toggled, Window,
    div, px,
};

use crate::component::{Component, example, example_group};
use crate::prelude::*;
use crate::styles::{filled_focus_ring, focus_ring};

type ToggleHandler = Rc<dyn Fn(bool, &mut Window, &mut App) + 'static>;

/// A checkbox with a label. The label is required: VoiceOver reads it.
#[derive(IntoElement)]
#[must_use]
pub struct Checkbox {
    id: ElementId,
    label: SharedString,
    checked: bool,
    disabled: bool,
    show_label: bool,
    display_label: Option<SharedString>,
    on_toggle: Option<ToggleHandler>,
}

impl Checkbox {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            checked: false,
            disabled: false,
            show_label: true,
            display_label: None,
            on_toggle: None,
        }
    }

    /// Hides the visible label. VoiceOver still reads it.
    pub fn show_label(mut self, show: bool) -> Self {
        self.show_label = show;
        self
    }

    /// Shows this text instead of the label. VoiceOver still reads the label.
    pub fn display_label(mut self, text: impl Into<SharedString>) -> Self {
        self.display_label = Some(text.into());
        self
    }

    /// Called with the new state when the user clicks the checkbox.
    pub fn on_toggle(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(handler));
        self
    }
}

impl Toggleable for Checkbox {
    fn toggle_state(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }
}

impl Disableable for Checkbox {
    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl RenderOnce for Checkbox {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus_handle = window
            .use_keyed_state((self.id.clone(), "focus"), cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let ring =
            !self.disabled && focus_handle.is_focused(window) && window.last_input_was_keyboard();

        let colors = cx.theme().colors();
        let (accent, ground) = (colors.accent, colors.ground);
        let checked = self.checked;

        let square = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(16.))
            .rounded(px(4.))
            .map(|square| {
                if checked {
                    square.bg(colors.fg).child(
                        Icon::new(IconName::Check)
                            .size(IconSize::Custom(px(12.)))
                            .color(Color::Inverse),
                    )
                } else {
                    square.border_1().border_color(colors.faint)
                }
            })
            .when(ring, |mut square| {
                let style = StyleRefinement::default();
                let style = if checked {
                    filled_focus_ring(style, accent, ground)
                } else {
                    focus_ring(style, accent)
                };
                square.style().box_shadow = style.box_shadow;
                square
            });

        div()
            .id(self.id)
            .role(Role::CheckBox)
            .aria_label(self.label.clone())
            .aria_toggled(if checked {
                Toggled::True
            } else {
                Toggled::False
            })
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .child(square)
            .when(self.show_label, |row| {
                row.child(Label::new(self.display_label.unwrap_or(self.label)))
            })
            .map(|row| {
                if self.disabled {
                    row.opacity(0.5)
                } else {
                    row.track_focus(&focus_handle)
                        .tab_stop(true)
                        .cursor_pointer()
                        .when_some(self.on_toggle, |row, handler| {
                            row.on_click(move |_, window, cx| handler(!checked, window, cx))
                        })
                }
            })
    }
}

impl Component for Checkbox {
    fn name() -> &'static str {
        "Checkbox"
    }

    fn preview(window: &mut Window, cx: &mut App) -> AnyElement {
        let state = window.use_keyed_state("checkbox-demo", cx, |_, _| true);
        let checked = *state.read(cx);

        v_flex()
            .gap_6()
            .child(example_group(
                "States",
                [
                    example(
                        "Unchecked",
                        Checkbox::new("unchecked", "Open as draft").toggle_state(false),
                    ),
                    example(
                        "Checked",
                        Checkbox::new("checked", "Remove agent worktrees").toggle_state(true),
                    ),
                    example(
                        "Disabled",
                        Checkbox::new("disabled", "Open as draft")
                            .toggle_state(true)
                            .disabled(true),
                    ),
                ],
            ))
            .child(example_group(
                "Interactive",
                [example(
                    if checked { "Checked" } else { "Unchecked" },
                    Checkbox::new("interactive", "Run setup script")
                        .toggle_state(checked)
                        .on_toggle(move |value, _, cx| {
                            state.update(cx, |state, cx| {
                                *state = value;
                                cx.notify();
                            });
                        }),
                )],
            ))
            .into_any_element()
    }
}
