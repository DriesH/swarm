use std::rc::Rc;

use gpui::{AnyElement, App, ElementId, Role, SharedString, Toggled, Window, div, px};

use crate::component::{Component, example, example_group};
use crate::prelude::*;
use crate::styles::{filled_focus_ring, focus_ring};

type ToggleHandler = Rc<dyn Fn(bool, &mut Window, &mut App) + 'static>;

/// An on/off switch. The label is required: VoiceOver reads it.
#[derive(IntoElement)]
#[must_use]
pub struct Switch {
    id: ElementId,
    label: SharedString,
    on: bool,
    disabled: bool,
    on_toggle: Option<ToggleHandler>,
}

impl Switch {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            on: false,
            disabled: false,
            on_toggle: None,
        }
    }

    /// Called with the new state when the user flips the switch.
    pub fn on_toggle(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(handler));
        self
    }
}

impl Toggleable for Switch {
    fn toggle_state(mut self, on: bool) -> Self {
        self.on = on;
        self
    }
}

impl Disableable for Switch {
    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl RenderOnce for Switch {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();

        let (track, knob) = if self.on {
            (colors.switch_on, colors.knob_on)
        } else {
            (colors.switch_off, colors.knob_off)
        };

        let (accent, ground) = (colors.accent, colors.ground);
        let on = self.on;

        div()
            .id(self.id)
            .role(Role::Switch)
            .aria_label(self.label)
            .aria_toggled(if on { Toggled::True } else { Toggled::False })
            .relative()
            .flex_none()
            .w(px(32.))
            .h(px(18.))
            .rounded_full()
            .bg(track)
            .child(
                div()
                    .absolute()
                    .top(px(2.))
                    .left(if on { px(16.) } else { px(2.) })
                    .size(px(14.))
                    .rounded_full()
                    .bg(knob),
            )
            .map(|switch| {
                if self.disabled {
                    switch.opacity(0.5)
                } else {
                    switch
                        .focusable()
                        .tab_stop(true)
                        .cursor_pointer()
                        .focus_visible(|style| {
                            if on {
                                filled_focus_ring(style, accent, ground)
                            } else {
                                focus_ring(style, accent)
                            }
                        })
                        .when_some(self.on_toggle, |switch, handler| {
                            switch.on_click(move |_, window, cx| handler(!on, window, cx))
                        })
                }
            })
    }
}

impl Component for Switch {
    fn name() -> &'static str {
        "Switch"
    }

    fn preview(window: &mut Window, cx: &mut App) -> AnyElement {
        let state = window.use_keyed_state("switch-demo", cx, |_, _| true);
        let on = *state.read(cx);

        v_flex()
            .gap_6()
            .child(example_group(
                "States",
                [
                    example(
                        "Off",
                        Switch::new("off", "Run setup script").toggle_state(false),
                    ),
                    example(
                        "On",
                        Switch::new("on", "Checks required to pass").toggle_state(true),
                    ),
                    example(
                        "Disabled",
                        Switch::new("disabled", "Notifications")
                            .toggle_state(true)
                            .disabled(true),
                    ),
                ],
            ))
            .child(example_group(
                "Interactive",
                [example(
                    if on { "On" } else { "Off" },
                    h_flex()
                        .gap_3()
                        .child(Label::new("Required to pass"))
                        .child(
                            Switch::new("interactive", "Required to pass")
                                .toggle_state(on)
                                .on_toggle(move |value, _, cx| {
                                    state.update(cx, |state, cx| {
                                        *state = value;
                                        cx.notify();
                                    });
                                }),
                        ),
                )],
            ))
            .into_any_element()
    }
}
