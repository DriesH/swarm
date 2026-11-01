use gpui::{
    AnyElement, App, ClickEvent, ElementId, FocusHandle, Role, SharedString, Window, div, px,
    relative,
};

use crate::component::{Component, example, example_group};
use crate::fonts::UI_FONT;
use crate::prelude::*;
use crate::traits::ClickHandler;

/// A question asked in place, with a Cancel and a confirm button. It takes the place of the
/// control that asked, so nothing opens over the screen.
#[derive(IntoElement)]
#[must_use]
pub struct ConfirmRow {
    id: ElementId,
    question: SharedString,
    cancel_label: SharedString,
    confirm_label: SharedString,
    destructive: bool,
    busy: bool,
    error: bool,
    cancel_focus: Option<FocusHandle>,
    confirm_focus: Option<FocusHandle>,
    debug_selector: Option<String>,
    on_cancel: Option<ClickHandler>,
    on_confirm: Option<ClickHandler>,
}

impl ConfirmRow {
    pub fn new(
        id: impl Into<ElementId>,
        question: impl Into<SharedString>,
        confirm_label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            question: question.into(),
            cancel_label: "Cancel".into(),
            confirm_label: confirm_label.into(),
            destructive: false,
            busy: false,
            error: false,
            cancel_focus: None,
            confirm_focus: None,
            debug_selector: None,
            on_cancel: None,
            on_confirm: None,
        }
    }

    /// Draws the confirm button as one that throws something away.
    pub fn destructive(mut self, destructive: bool) -> Self {
        self.destructive = destructive;
        self
    }

    /// Disables both buttons while the confirmed work runs.
    pub fn busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }

    /// Shows the question as an error, for the answer of work that failed.
    pub fn error(mut self, error: bool) -> Self {
        self.error = error;
        self
    }

    /// Focus handles the owner can move focus to. Create them with `tab_stop(true)`.
    pub fn track_focus(mut self, cancel: &FocusHandle, confirm: &FocusHandle) -> Self {
        self.cancel_focus = Some(cancel.clone());
        self.confirm_focus = Some(confirm.clone());
        self
    }

    /// Names the buttons `<selector>-cancel` and `<selector>-confirm` for
    /// `VisualTestContext::debug_bounds`. Only recorded in test builds.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }

    pub fn on_cancel(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_cancel = Some(Box::new(handler));
        self
    }

    pub fn on_confirm(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_confirm = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for ConfirmRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let variant = if self.destructive {
            ButtonVariant::Destructive
        } else {
            ButtonVariant::Primary
        };

        let cancel = Button::new((self.id.clone(), "cancel"), self.cancel_label)
            .disabled(self.busy)
            .when_some(self.cancel_focus.as_ref(), |button, handle| {
                button.track_focus(handle)
            })
            .when_some(self.debug_selector.clone(), |button, selector| {
                button.debug_selector(move || format!("{selector}-cancel"))
            })
            .when_some(self.on_cancel, |button, handler| {
                button.on_click(move |event, window, cx| handler(event, window, cx))
            });

        let confirm = Button::new((self.id.clone(), "confirm"), self.confirm_label)
            .variant(variant)
            .disabled(self.busy)
            .when_some(self.confirm_focus.as_ref(), |button, handle| {
                button.track_focus(handle)
            })
            .when_some(self.debug_selector, |button, selector| {
                button.debug_selector(move || format!("{selector}-confirm"))
            })
            .when_some(self.on_confirm, |button, handler| {
                button.on_click(move |event, window, cx| handler(event, window, cx))
            });

        // An error can be long: it takes up to two lines, where a question takes one.
        let text = if self.error {
            div()
                .font_family(UI_FONT)
                .text_size(LabelSize::Small.pixels())
                .line_height(relative(1.3))
                .text_color(Color::Del.hsla(cx))
                .line_clamp(2)
                .child(self.question.clone())
                .into_any_element()
        } else {
            Label::new(self.question.clone())
                .truncate()
                .into_any_element()
        };

        h_flex()
            .id(self.id)
            .role(Role::Group)
            .aria_label(self.question)
            .gap(px(8.))
            .child(div().flex_1().min_w_0().flex().child(text))
            .child(cancel)
            .child(confirm)
    }
}

impl Component for ConfirmRow {
    fn name() -> &'static str {
        "ConfirmRow"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        let sized = |row: ConfirmRow| div().w(px(296.)).child(row);

        v_flex()
            .gap_6()
            .child(example_group(
                "States",
                [
                    example(
                        "Destructive",
                        sized(
                            ConfirmRow::new("stop-agents", "Stop 3 agents?", "Stop")
                                .destructive(true),
                        ),
                    ),
                    example(
                        "Primary",
                        sized(ConfirmRow::new("send-it", "Send it?", "Send")),
                    ),
                    example(
                        "Failed",
                        sized(
                            ConfirmRow::new(
                                "failed",
                                "Could not stop the agents: no permission",
                                "Stop",
                            )
                            .destructive(true)
                            .error(true),
                        ),
                    ),
                    example(
                        "Busy",
                        sized(
                            ConfirmRow::new("stopping", "Stop 3 agents?", "Stop")
                                .destructive(true)
                                .busy(true),
                        ),
                    ),
                ],
            ))
            .into_any_element()
    }
}
