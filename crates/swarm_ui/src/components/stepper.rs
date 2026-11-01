use std::rc::Rc;

use gpui::{AnyElement, App, ElementId, Role, SharedString, Window, div, px};

use crate::component::{Component, example, example_group};
use crate::prelude::*;

type ChangeHandler = Rc<dyn Fn(u32, &mut Window, &mut App) + 'static>;

/// A whole number between two limits, with a minus and a plus button around it. The label
/// is required: VoiceOver reads it with the value.
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

    /// Called with the new value when a button changes it. A button at its limit does
    /// nothing.
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
}

impl RenderOnce for Stepper {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let shown = self.shown();
        let (value, min, max) = (self.value, self.min, self.max);

        // A button at its limit stays focusable, so pressing it never strands the keyboard.
        let button = |suffix: &'static str, icon: IconName, to: Option<u32>| {
            let verb = if icon == IconName::Minus {
                "Decrease"
            } else {
                "Increase"
            };
            let handler = self.on_change.clone();
            let selector = self.debug_selector.clone();

            div()
                .when(to.is_none(), |at_limit| at_limit.opacity(0.4))
                .child(
                    IconButton::new(
                        (self.id.clone(), suffix),
                        icon,
                        format!("{verb} {}", self.label.to_lowercase()),
                    )
                    .size(IconButtonSize::Medium)
                    .color(Color::Muted)
                    .when_some(selector, |button, selector| {
                        button.debug_selector(move || format!("{selector}-{suffix}"))
                    })
                    .on_click(move |_, window, cx| {
                        if let (Some(to), Some(handler)) = (to, &handler) {
                            handler(to, window, cx);
                        }
                    }),
                )
        };

        let decrease = button(
            "decrease",
            IconName::Minus,
            (value > min).then(|| value - 1),
        );
        let increase = button("increase", IconName::Plus, (value < max).then(|| value + 1));

        let readout = div()
            .id((self.id.clone(), "value"))
            .role(Role::Label)
            .aria_label(format!("{}: {shown}", self.label))
            .when_some(self.debug_selector, |readout, selector| {
                readout.debug_selector(move || format!("{selector}-value"))
            })
            .flex()
            .items_center()
            .justify_center()
            .min_w(px(52.))
            .child(Label::new(shown).mono());

        h_flex()
            .id(self.id)
            .role(Role::Group)
            .aria_label(self.label)
            .flex_none()
            .gap(px(2.))
            .child(decrease)
            .child(readout)
            .child(increase)
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
    fn the_value_shows_with_its_unit(cx: &mut TestAppContext) {
        let (_, _, cx) = setup(12, cx);

        assert!(cx.debug_bounds("size-value").is_some());
    }

    #[test]
    fn a_value_outside_the_limits_is_brought_in() {
        let stepper = Stepper::new("size", "Font size", 99, 11, 20).unit("pt");

        assert_eq!(stepper.shown(), "20 pt");
    }
}
