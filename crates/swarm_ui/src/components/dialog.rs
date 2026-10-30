use std::rc::Rc;

use gpui::{
    AnyElement, App, BoxShadow, Div, ElementId, FocusHandle, FontWeight, MouseButton, Pixels, Role,
    SharedString, Stateful, Window, black, div, point, px,
};

use crate::component::{Component, example};
use crate::prelude::*;
use crate::traits::Clickable as _;
use crate::{DIALOG_CONTEXT, Dismiss, trap_focus};

type DismissHandler = Rc<dyn Fn(&mut Window, &mut App) + 'static>;

/// A modal sheet over its parent. The parent must be `relative`.
#[derive(IntoElement)]
#[must_use]
pub struct Dialog {
    id: ElementId,
    title: SharedString,
    width: Pixels,
    body: Vec<AnyElement>,
    footer: Option<AnyElement>,
    focus_handle: FocusHandle,
    on_dismiss: Option<DismissHandler>,
}

impl Dialog {
    /// `focus_handle` is focused when the dialog opens and traps Tab, so Esc reaches the dialog.
    pub fn new(
        id: impl Into<ElementId>,
        title: impl Into<SharedString>,
        focus_handle: &FocusHandle,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            width: px(480.),
            body: Vec::new(),
            footer: None,
            focus_handle: focus_handle.clone(),
            on_dismiss: None,
        }
    }

    pub fn footer(mut self, footer: impl IntoElement) -> Self {
        self.footer = Some(footer.into_any_element());
        self
    }

    pub fn width(mut self, width: Pixels) -> Self {
        self.width = width;
        self
    }

    /// Called on Esc, on the close button and on a click outside the panel.
    pub fn on_dismiss(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }
}

impl ParentElement for Dialog {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.body.extend(elements);
    }
}

impl Dialog {
    /// The labelled dialog panel, without the scrim. Public so tests can read its node.
    #[doc(hidden)]
    pub fn panel(self, cx: &App) -> Stateful<Div> {
        self.into_parts(cx).2
    }

    fn into_parts(self, cx: &App) -> (ElementId, Option<DismissHandler>, Stateful<Div>) {
        let colors = cx.theme().colors();

        let title = self.title.clone();
        let close = self.on_dismiss.clone();
        let on_scrim = self.on_dismiss.clone();
        let on_action = self.on_dismiss;

        let panel = v_flex()
            .id("dialog-panel")
            .role(Role::Dialog)
            .aria_label(title.clone())
            .key_context(DIALOG_CONTEXT)
            .track_focus(&self.focus_handle)
            .when_some(on_action, |panel, handler| {
                panel.on_action(move |_: &Dismiss, window, cx| handler(window, cx))
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .w(self.width)
            .overflow_hidden()
            .rounded(px(14.))
            .border_1()
            .border_color(colors.line)
            .bg(colors.panel)
            .shadow(vec![BoxShadow {
                color: black().opacity(0.35),
                offset: point(px(0.), px(24.)),
                blur_radius: px(64.),
                spread_radius: px(0.),
                inset: false,
            }])
            .child(
                h_flex()
                    .pt(px(18.))
                    .px(px(20.))
                    .pb(px(4.))
                    .child(
                        Label::new(title)
                            .size(LabelSize::Default)
                            .weight(FontWeight::SEMIBOLD),
                    )
                    .child(div().flex_1())
                    .child(
                        IconButton::new("dialog-close", IconName::Close, "Close").on_click(
                            move |_, window, cx| {
                                if let Some(handler) = &close {
                                    handler(window, cx);
                                }
                            },
                        ),
                    ),
            )
            .child(
                v_flex()
                    .gap(px(14.))
                    .pt(px(14.))
                    .px(px(20.))
                    .pb(px(20.))
                    .children(self.body),
            )
            .when_some(self.footer, |panel, footer| {
                panel.child(
                    h_flex()
                        .justify_end()
                        .gap(px(8.))
                        .px(px(20.))
                        .pb(px(20.))
                        .child(footer),
                )
            });

        (self.id, on_scrim, trap_focus(panel, self.focus_handle))
    }
}

impl RenderOnce for Dialog {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (id, on_scrim, panel) = self.into_parts(cx);

        div()
            .id(id)
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(120.))
            .bg(colors.scrim)
            .occlude()
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                if let Some(handler) = &on_scrim {
                    handler(window, cx);
                }
            })
            .child(panel)
    }
}

impl Component for Dialog {
    fn name() -> &'static str {
        "Dialog"
    }

    fn preview(window: &mut Window, cx: &mut App) -> AnyElement {
        let focus = window.use_keyed_state("dialog-preview-focus", cx, |_, cx| cx.focus_handle());
        let focus = focus.read(cx).clone();
        let colors = cx.theme().colors();

        let dialog = Dialog::new("new-task", "New task", &focus)
            .child(Checkbox::new("run-setup", "Run setup script").toggle_state(true))
            .child(Label::new("Branch from: main").color(Color::Muted))
            .footer(
                h_flex()
                    .gap(px(8.))
                    .child(Button::new("cancel", "Cancel").variant(ButtonVariant::Secondary))
                    .child(Button::new("create", "Create task").variant(ButtonVariant::Primary)),
            );

        example(
            "Open",
            div()
                .relative()
                .w(px(640.))
                .h(px(380.))
                .overflow_hidden()
                .rounded(px(10.))
                .bg(colors.ground)
                .child(dialog),
        )
    }
}
