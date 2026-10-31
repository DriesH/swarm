use std::rc::Rc;

use gpui::{
    AnyElement, App, Context, Div, ElementId, Empty, FocusHandle, KeyDownEvent, MouseButton,
    Orientation, Render, Role, SharedString, Stateful, Window, div, px,
};

use crate::component::{Component, example, example_group};
use crate::prelude::*;

type DragHook = Box<dyn FnOnce(Stateful<Div>) -> Stateful<Div>>;
type ResetHandler = Rc<dyn Fn(&mut Window, &mut App) + 'static>;
type NudgeHandler = Rc<dyn Fn(Nudge, &mut Window, &mut App) + 'static>;

/// How a key moves the border: an arrow key a step, Home and End to the limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nudge {
    Left,
    Right,
    /// Home: the smallest size.
    Smallest,
    /// End: the largest size.
    Largest,
}

/// A vertical drag handle on the left border of a panel. Its parent must be positioned
/// (`relative`). The hit area is 8 px wide and centered on the border.
#[derive(IntoElement)]
#[must_use]
pub struct ResizeHandle {
    id: ElementId,
    label: SharedString,
    focus_handle: FocusHandle,
    value: f32,
    range: (f32, f32),
    dragging: bool,
    drag: Option<DragHook>,
    on_reset: Option<ResetHandler>,
    on_nudge: Option<NudgeHandler>,
}

impl ResizeHandle {
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        focus_handle: &FocusHandle,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            focus_handle: focus_handle.clone(),
            value: 0.,
            range: (0., 0.),
            dragging: false,
            drag: None,
            on_reset: None,
            on_nudge: None,
        }
    }

    /// The size the handle sets, in pixels, with its smallest and largest value.
    pub fn value(mut self, value: f32, min: f32, max: f32) -> Self {
        self.value = value;
        self.range = (min, max);
        self
    }

    pub fn dragging(mut self, dragging: bool) -> Self {
        self.dragging = dragging;
        self
    }

    /// Makes the handle draggable. `payload` is what `DragMoveEvent<T>` listeners of an
    /// ancestor get while the drag lasts, wherever the pointer is in the window.
    pub fn on_drag<T: Clone + 'static>(mut self, payload: T) -> Self {
        self.drag = Some(Box::new(move |handle| {
            handle
                .on_drag(payload, |_, _, _, cx| {
                    cx.stop_propagation();
                    cx.new(|_| NoGhost)
                })
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        }));
        self
    }

    /// Called when the border is double-clicked.
    pub fn on_reset(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_reset = Some(Rc::new(handler));
        self
    }

    /// Called when an arrow key, Home or End is pressed on the focused handle.
    pub fn on_nudge(mut self, handler: impl Fn(Nudge, &mut Window, &mut App) + 'static) -> Self {
        self.on_nudge = Some(Rc::new(handler));
        self
    }
}

/// The drag shows nothing next to the pointer.
struct NoGhost;

impl Render for NoGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

impl RenderOnce for ResizeHandle {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (line, accent) = (colors.line, colors.accent);

        let focused = self.focus_handle.is_focused(window);
        let thick = self.dragging || focused;
        let group: SharedString = "resize-handle".into();

        div()
            .id(self.id)
            .group(group.clone())
            .role(Role::Splitter)
            .aria_label(self.label)
            .aria_orientation(Orientation::Vertical)
            .aria_value(format!("{} pixels", self.value.round()))
            .aria_numeric_value(f64::from(self.value))
            .aria_min_numeric_value(f64::from(self.range.0))
            .aria_max_numeric_value(f64::from(self.range.1))
            .track_focus(&self.focus_handle)
            .absolute()
            .left(px(-4.))
            .top_0()
            .bottom_0()
            .w(px(8.))
            .cursor_col_resize()
            .child(
                div()
                    .h_full()
                    .ml(px(if thick { 3. } else { 4. }))
                    .w(px(if thick { 3. } else { 1. }))
                    .bg(if thick { accent } else { line })
                    .group_hover(group, move |style| style.ml(px(3.)).w(px(3.)).bg(accent)),
            )
            .when_some(self.drag, |handle, apply| apply(handle))
            .when_some(self.on_reset, |handle, handler| {
                handle.on_mouse_up(MouseButton::Left, move |event, window, cx| {
                    if event.click_count == 2 {
                        cx.stop_propagation();
                        handler(window, cx);
                    }
                })
            })
            .when_some(self.on_nudge, |handle, handler| {
                handle.on_key_down(move |event: &KeyDownEvent, window, cx| {
                    if event.keystroke.modifiers.modified() {
                        return;
                    }

                    let nudge = match event.keystroke.key.as_str() {
                        "left" => Nudge::Left,
                        "right" => Nudge::Right,
                        "home" => Nudge::Smallest,
                        "end" => Nudge::Largest,
                        _ => return,
                    };

                    cx.stop_propagation();
                    handler(nudge, window, cx);
                })
            })
    }
}

impl Component for ResizeHandle {
    fn name() -> &'static str {
        "ResizeHandle"
    }

    fn preview(window: &mut Window, cx: &mut App) -> AnyElement {
        let handle = window.use_keyed_state("resize-demo", cx, |_, cx| cx.focus_handle());
        let focus = handle.read(cx).clone();

        v_flex()
            .gap_6()
            .child(example_group(
                "Panel border",
                [example(
                    "Hover or focus to widen",
                    div().relative().h(px(80.)).w(px(240.)).ml(px(8.)).child(
                        ResizeHandle::new("resize-demo-handle", "Resize panel", &focus)
                            .value(400., 320., 640.),
                    ),
                )],
            ))
            .into_any_element()
    }
}
