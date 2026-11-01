use std::rc::Rc;

use gpui::{
    AnyElement, App, BoxShadow, ClickEvent, ElementId, FocusHandle, FontWeight, Role, SharedString,
    Window, black, point, px,
};

use crate::component::{Component, example, example_group};
use crate::prelude::*;
use crate::traits::ClickHandler;
use crate::{Dismiss, TOAST_CONTEXT};

type DismissHandler = Rc<dyn Fn(&mut Window, &mut App) + 'static>;
type HoverHandler = Box<dyn Fn(bool, &mut Window, &mut App) + 'static>;

/// A short message that appears over the window: a title, one line of status, an optional
/// action and a dismiss button.
///
/// It is a status region named "title, message". The action and the dismiss button are
/// ordinary buttons in the tab order, and Escape dismisses it while focus is inside. It
/// blocks the mouse, so a click on it never reaches what is under it.
#[derive(IntoElement)]
#[must_use]
pub struct Toast {
    id: ElementId,
    title: SharedString,
    message: SharedString,
    detail: Option<SharedString>,
    leading: Option<AnyElement>,
    action: Option<(SharedString, ClickHandler)>,
    on_dismiss: Option<DismissHandler>,
    on_hover: Option<HoverHandler>,
    focus_handle: Option<FocusHandle>,
    debug_selector: Option<String>,
}

/// The toast is this wide.
pub const TOAST_WIDTH: f32 = 360.;

impl Toast {
    pub fn new(
        id: impl Into<ElementId>,
        title: impl Into<SharedString>,
        message: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            message: message.into(),
            detail: None,
            leading: None,
            action: None,
            on_dismiss: None,
            on_hover: None,
            focus_handle: None,
            debug_selector: None,
        }
    }

    /// A line of numbers under the message.
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Decoration before the text. Keep it out of the accessibility tree.
    pub fn leading(mut self, leading: impl IntoElement) -> Self {
        self.leading = Some(leading.into_any_element());
        self
    }

    /// The one action, as a button: `Review`, for example.
    pub fn action(
        mut self,
        label: impl Into<SharedString>,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.action = Some((label.into(), Box::new(handler)));
        self
    }

    /// Called by the dismiss button and by Escape while focus is in the toast.
    pub fn on_dismiss(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }

    /// Called when the pointer enters or leaves the toast.
    pub fn on_hover(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_hover = Some(Box::new(handler));
        self
    }

    /// Tracks focus for the whole toast: `handle.contains_focused` is true while focus is on
    /// the toast or in it.
    pub fn track_focus(mut self, handle: &FocusHandle) -> Self {
        self.focus_handle = Some(handle.clone());
        self
    }

    /// Names the parts `<selector>`, `<selector>-action` and `<selector>-dismiss` for
    /// `VisualTestContext::debug_bounds`. Only recorded in test builds.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }
}

impl RenderOnce for Toast {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (raised, line) = (colors.raised, colors.line);

        let label = format!("{}, {}", self.title, self.message);
        let selector = self.debug_selector.clone();

        let text = v_flex()
            .flex_1()
            .min_w_0()
            .gap(px(2.))
            .child(
                Label::new(self.title.clone())
                    .weight(FontWeight::SEMIBOLD)
                    .truncate(),
            )
            .child(
                Label::new(self.message)
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .when_some(self.detail, |text, detail| {
                text.child(
                    Label::new(detail)
                        .mono()
                        .size(LabelSize::XSmall)
                        .color(Color::Faint),
                )
            });

        let action = self.action.map(|(action_label, handler)| {
            Button::new((self.id.clone(), "action"), action_label.clone())
                .accessible_label(format!("{action_label} {}", self.title))
                .when_some(selector.clone(), |button, selector| {
                    button.debug_selector(move || format!("{selector}-action"))
                })
                .on_click(move |event, window, cx| handler(event, window, cx))
        });

        let dismiss = self.on_dismiss.clone().map(|handler| {
            IconButton::new(
                (self.id.clone(), "dismiss"),
                IconName::Close,
                "Dismiss notification",
            )
            .when_some(selector.clone(), |button, selector| {
                button.debug_selector(move || format!("{selector}-dismiss"))
            })
            .on_click(move |_, window, cx| handler(window, cx))
        });

        h_flex()
            .id(self.id)
            .role(Role::Status)
            .aria_label(label)
            .when_some(selector, |toast, selector| {
                toast.debug_selector(move || selector)
            })
            .when_some(self.on_dismiss, |toast, handler| {
                toast
                    .key_context(TOAST_CONTEXT)
                    .on_action(move |_: &Dismiss, window, cx| handler(window, cx))
            })
            .when_some(self.focus_handle, |toast, handle| {
                toast.track_focus(&handle)
            })
            .when_some(self.on_hover, |toast, handler| {
                toast.on_hover(move |hovered, window, cx| handler(*hovered, window, cx))
            })
            .occlude()
            .w(px(TOAST_WIDTH))
            .gap(px(12.))
            .p(px(12.))
            .rounded(px(14.))
            .bg(raised)
            .border_1()
            .border_color(line)
            .shadow(vec![BoxShadow {
                color: black().opacity(0.18),
                offset: point(px(0.), px(8.)),
                blur_radius: px(24.),
                spread_radius: px(0.),
                inset: false,
            }])
            .children(self.leading)
            .child(text)
            .children(action)
            .children(dismiss)
    }
}

impl Component for Toast {
    fn name() -> &'static str {
        "Toast"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        v_flex()
            .gap_6()
            .child(example_group(
                "States",
                [
                    example(
                        "Action and dismiss",
                        Toast::new("toast-action", "terminal-renderer", "Ready for review")
                            .detail("3 workers \u{b7} +312 \u{2212}48")
                            .action("Review", |_, _, _| {})
                            .on_dismiss(|_, _| {}),
                    ),
                    example(
                        "Message only",
                        Toast::new("toast-plain", "Settings saved", "Nothing else to do")
                            .on_dismiss(|_, _| {}),
                    ),
                ],
            ))
            .into_any_element()
    }
}
