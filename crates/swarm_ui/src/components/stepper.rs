use std::rc::Rc;

use gpui::{
    AccessibleAction, AnyElement, App, ElementId, FocusHandle, Role, SharedString, Window, div, px,
};

use crate::component::{Component, example, example_group};
use crate::prelude::*;
use crate::styles::focus_ring;

type ChangeHandler = Rc<dyn Fn(u32, &mut Window, &mut App) + 'static>;

/// A whole number between two limits, with a minus and a plus button around it. It is one
/// spin button for VoiceOver, which reads the label and the value and can raise or lower
/// it. The keyboard reaches it as one stop: Up and Down step, Home and End go to the
/// limits. The label is required.
#[derive(IntoElement)]
#[must_use]
pub struct Stepper {
    id: ElementId,
    label: SharedString,
    value: u32,
    min: u32,
    max: u32,
    unit: Option<SharedString>,
    debug_selector: Option<String>,
    on_change: Option<ChangeHandler>,
}

impl Stepper {
    /// `value` is brought between `min` and `max`.
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        value: u32,
        min: u32,
        max: u32,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value: value.clamp(min, max.max(min)),
            min,
            max: max.max(min),
            unit: None,
            debug_selector: None,
            on_change: None,
        }
    }

    /// Text after the number, for example "pt".
    pub fn unit(mut self, unit: impl Into<SharedString>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    /// Names the parts `<selector>-decrease`, `<selector>-increase` and `<selector>-value`
    /// for `VisualTestContext::debug_bounds`. Only recorded in test builds.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }

    /// Called with the new value when a button, a key or VoiceOver changes it. At a limit
    /// the way past it does nothing.
    pub fn on_change(mut self, handler: impl Fn(u32, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }

    fn shown(&self) -> String {
        match &self.unit {
            Some(unit) => format!("{} {unit}", self.value),
            None => self.value.to_string(),
        }
    }

    /// The value one step down, or `None` at the minimum.
    fn lower(&self) -> Option<u32> {
        (self.value > self.min).then(|| self.value - 1)
    }

    /// The value one step up, or `None` at the maximum.
    fn higher(&self) -> Option<u32> {
        (self.value < self.max).then(|| self.value + 1)
    }

    /// The minus and the plus button. A button at its limit is disabled, so it cannot be
    /// focused or pressed, and only its icon is faded.
    #[doc(hidden)]
    pub fn buttons(&self) -> [IconButton; 2] {
        [
            self.button("decrease", self.lower(), None),
            self.button("increase", self.higher(), None),
        ]
    }

    /// `owner` takes focus after a press, so the keyboard stays on the stepper when the
    /// button turns off at its limit.
    fn button(
        &self,
        suffix: &'static str,
        to: Option<u32>,
        owner: Option<FocusHandle>,
    ) -> IconButton {
        let (icon, verb) = if suffix == "decrease" {
            (IconName::Minus, "Decrease")
        } else {
            (IconName::Plus, "Increase")
        };
        let handler = self.on_change.clone();
        let selector = self.debug_selector.clone();

        IconButton::new(
            (self.id.clone(), suffix),
            icon,
            format!("{verb} {}", self.label.to_lowercase()),
        )
        .size(IconButtonSize::Medium)
        .color(Color::Muted)
        .skip_tab_stop()
        .disabled(to.is_none())
        .when_some(selector, |button, selector| {
            button.debug_selector(move || format!("{selector}-{suffix}"))
        })
        .on_click(move |_, window, cx| {
            if let Some(owner) = &owner {
                window.focus(owner, cx);
            }

            if let (Some(to), Some(handler)) = (to, &handler) {
                handler(to, window, cx);
            }
        })
    }
}

impl RenderOnce for Stepper {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let accent = cx.theme().colors().accent;
        let focus = window
            .use_keyed_state((self.id.clone(), "focus"), cx, |_, cx| {
                cx.focus_handle().tab_stop(true)
            })
            .read(cx)
            .clone();

        let shown = self.shown();
        let (value, min, max) = (self.value, self.min, self.max);
        let (lower, higher) = (self.lower(), self.higher());

        let decrease = self.button("decrease", lower, Some(focus.clone()));
        let increase = self.button("increase", higher, Some(focus.clone()));

        let on_key = self.on_change.clone();
        let with_handler = |to: Option<u32>| to.zip(self.on_change.clone());

        let readout = div()
            .id((self.id.clone(), "value"))
            .when_some(self.debug_selector, |readout, selector| {
                readout.debug_selector(move || format!("{selector}-value"))
            })
            .flex()
            .items_center()
            .justify_center()
            .min_w(px(52.))
            .child(Label::new(shown.clone()).mono());

        let mut stepper = h_flex()
            .id(self.id)
            .role(Role::SpinButton)
            .aria_label(self.label)
            .aria_value(shown)
            .aria_numeric_value(f64::from(value))
            .aria_min_numeric_value(f64::from(min))
            .aria_max_numeric_value(f64::from(max))
            .aria_numeric_value_step(1.)
            .track_focus(&focus)
            .focus_visible(move |style| focus_ring(style, accent))
            .on_key_down(move |event, window, cx| {
                let to = match event.keystroke.key.as_str() {
                    "up" => higher,
                    "down" => lower,
                    "home" => Some(min).filter(|to| *to != value),
                    "end" => Some(max).filter(|to| *to != value),
                    _ => return,
                };

                cx.stop_propagation();

                if let (Some(to), Some(handler)) = (to, &on_key) {
                    handler(to, window, cx);
                }
            })
            .flex_none()
            .gap(px(2.))
            .rounded(px(7.));

        if let Some((to, handler)) = with_handler(higher) {
            stepper = stepper.on_a11y_action(AccessibleAction::Increment, move |_, window, cx| {
                handler(to, window, cx);
            });
        }

        if let Some((to, handler)) = with_handler(lower) {
            stepper = stepper.on_a11y_action(AccessibleAction::Decrement, move |_, window, cx| {
                handler(to, window, cx);
            });
        }

        stepper.child(decrease).child(readout).child(increase)
    }
}

impl Component for Stepper {
    fn name() -> &'static str {
        "Stepper"
    }

    fn preview(window: &mut Window, cx: &mut App) -> AnyElement {
        let state = window.use_keyed_state("stepper-demo", cx, |_, _| 13_u32);
        let value = *state.read(cx);

        v_flex()
            .gap_6()
            .child(example_group(
                "States",
                [
                    example(
                        "Minimum",
                        Stepper::new("stepper-min", "Font size", 11, 11, 20).unit("pt"),
                    ),
                    example(
                        "Between",
                        Stepper::new("stepper-mid", "Font size", 16, 11, 20).unit("pt"),
                    ),
                    example(
                        "Maximum",
                        Stepper::new("stepper-max", "Font size", 20, 11, 20).unit("pt"),
                    ),
                ],
            ))
            .child(example_group(
                "Interactive",
                [example(
                    "Font size",
                    Stepper::new("stepper-live", "Font size", value, 11, 20)
                        .unit("pt")
                        .on_change(move |value, _, cx| {
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::{Context, Modifiers, Render, TestAppContext, VisualTestContext};

    use super::*;

    struct Harness {
        value: u32,
        log: Rc<RefCell<Vec<u32>>>,
    }

    impl Render for Harness {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let log = Rc::clone(&self.log);
            let this = cx.entity();

            Stepper::new("size", "Font size", self.value, 11, 13)
                .unit("pt")
                .debug_selector(|| "size".to_owned())
                .on_change(move |value, _, cx| {
                    log.borrow_mut().push(value);
                    this.update(cx, |harness, cx| {
                        harness.value = value;
                        cx.notify();
                    });
                })
        }
    }

    fn setup(
        value: u32,
        cx: &mut TestAppContext,
    ) -> (
        Rc<RefCell<Vec<u32>>>,
        gpui::Entity<Harness>,
        &mut VisualTestContext,
    ) {
        cx.update(crate::theme::init);
        let log = Rc::new(RefCell::new(Vec::new()));
        let shared = Rc::clone(&log);
        let (harness, cx) = cx.add_window_view(move |_, _| Harness { value, log: shared });
        cx.run_until_parked();

        (log, harness, cx)
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"));

        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn plus_and_minus_step_the_value_by_one(cx: &mut TestAppContext) {
        let (log, harness, cx) = setup(12, cx);

        click("size-increase", cx);
        click("size-decrease", cx);
        click("size-decrease", cx);

        assert_eq!(*log.borrow(), [13, 12, 11]);
        assert_eq!(harness.read_with(cx, |harness, _| harness.value), 11);
    }

    #[gpui::test]
    fn a_button_at_its_limit_changes_nothing(cx: &mut TestAppContext) {
        let (log, _, cx) = setup(13, cx);

        click("size-increase", cx);

        assert!(log.borrow().is_empty());
    }

    #[gpui::test]
    fn the_stepper_is_one_tab_stop_and_up_and_down_step_the_value(cx: &mut TestAppContext) {
        let (log, harness, cx) = setup(12, cx);

        cx.update(Window::focus_next);
        cx.simulate_keystrokes("up");
        cx.simulate_keystrokes("up");
        cx.simulate_keystrokes("down");
        cx.simulate_keystrokes("down");
        cx.simulate_keystrokes("down");
        cx.simulate_keystrokes("down");

        assert_eq!(*log.borrow(), [13, 12, 11]);
        assert_eq!(harness.read_with(cx, |harness, _| harness.value), 11);
    }

    #[gpui::test]
    fn home_and_end_go_to_the_limits(cx: &mut TestAppContext) {
        let (log, _, cx) = setup(12, cx);

        cx.update(Window::focus_next);
        cx.simulate_keystrokes("end");
        cx.simulate_keystrokes("end");
        cx.simulate_keystrokes("home");

        assert_eq!(*log.borrow(), [13, 11]);
    }

    #[gpui::test]
    fn the_buttons_are_not_tab_stops_and_the_stepper_keeps_focus_after_a_press(
        cx: &mut TestAppContext,
    ) {
        let (log, _, cx) = setup(12, cx);

        cx.update(Window::focus_next);
        let stepper = cx.update(|window, cx| window.focused(cx));
        cx.update(Window::focus_next);
        let again = cx.update(|window, cx| window.focused(cx));

        click("size-increase", cx);
        click("size-increase", cx);
        let after_press = cx.update(|window, cx| window.focused(cx));

        assert_eq!(stepper, again, "Tab went past the stepper to another stop");
        assert_eq!(stepper, after_press, "the press took focus away");
        assert_eq!(*log.borrow(), [13]);
    }

    #[gpui::test]
    fn the_value_shows_with_its_unit(cx: &mut TestAppContext) {
        let (_, _, cx) = setup(12, cx);

        assert!(cx.debug_bounds("size-value").is_some());
    }

    #[test]
    fn the_buttons_at_the_limits_are_the_ones_that_cannot_step() {
        let stepper = |value| Stepper::new("size", "Font size", value, 11, 20);

        assert_eq!(
            [stepper(11).lower(), stepper(11).higher()],
            [None, Some(12)]
        );
        assert_eq!(
            [stepper(20).lower(), stepper(20).higher()],
            [Some(19), None]
        );
    }

    #[test]
    fn a_value_outside_the_limits_is_brought_in() {
        let stepper = Stepper::new("size", "Font size", 99, 11, 20).unit("pt");

        assert_eq!(stepper.shown(), "20 pt");
    }
}
