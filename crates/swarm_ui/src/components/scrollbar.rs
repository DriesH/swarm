use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, CursorStyle, DispatchPhase, ElementId, Entity, EntityId, Hitbox,
    HitboxBehavior, ListAlignment, ListState, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Orientation, Pixels, Role, SharedString, Window, canvas, div, fill, point, px,
    size,
};

use crate::a11y::accessible;
use crate::component::{Component, example, example_group};
use crate::prelude::*;

/// The width of the bar's hit area at the right edge.
const TRACK_WIDTH: f32 = 12.;
const THUMB_WIDTH: f32 = 6.;
const THUMB_RIGHT: f32 = 3.;
/// Space above and below the track.
const TRACK_INSET: f32 = 2.;
const MIN_THUMB: f32 = 24.;

/// What a [`Scrollbar`] reads and moves. Offsets are positive: 0 is the top.
pub trait ScrollableHandle: 'static {
    /// How far the content can scroll. Zero when it fits.
    fn max_offset(&self) -> Pixels;
    /// How far the content is scrolled, from 0 to `max_offset`.
    fn offset(&self) -> Pixels;
    fn set_offset(&self, offset: Pixels);
    fn viewport_height(&self) -> Pixels;
    fn drag_started(&self) {}
    fn drag_ended(&self) {}
}

impl ScrollableHandle for ListState {
    fn max_offset(&self) -> Pixels {
        self.max_offset_for_scrollbar().y
    }

    fn offset(&self) -> Pixels {
        -self.scroll_px_offset_for_scrollbar().y
    }

    fn set_offset(&self, offset: Pixels) {
        self.set_offset_from_scrollbar(point(px(0.), -offset));
    }

    fn viewport_height(&self) -> Pixels {
        self.viewport_bounds().size.height
    }

    fn drag_started(&self) {
        self.scrollbar_drag_started();
    }

    fn drag_ended(&self) {
        self.scrollbar_drag_ended();
    }
}

/// Where the thumb sits in the track, relative to the track's top.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thumb {
    pub top: Pixels,
    pub height: Pixels,
}

impl Thumb {
    /// The thumb for a track, or `None` when the content fits and there is nothing to show.
    pub fn measure(
        track: Pixels,
        viewport: Pixels,
        max_offset: Pixels,
        offset: Pixels,
    ) -> Option<Self> {
        if track <= px(0.) || viewport <= px(0.) || max_offset <= px(0.) {
            return None;
        }

        let content = viewport + max_offset;
        let height = (track * (viewport / content)).max(px(MIN_THUMB)).min(track);
        let top = (track - height) * progress(max_offset, offset);

        Some(Self { top, height })
    }

    /// The scroll offset that puts the thumb's top at `top`.
    pub fn offset_at(self, top: Pixels, track: Pixels, max_offset: Pixels) -> Pixels {
        let range = track - self.height;

        if range <= px(0.) {
            return px(0.);
        }

        max_offset * (top / range).clamp(0., 1.)
    }
}

/// How far through the content the view is, from 0 to 1.
pub fn progress(max_offset: Pixels, offset: Pixels) -> f32 {
    if max_offset <= px(0.) {
        return 0.;
    }

    (offset / max_offset).clamp(0., 1.)
}

/// Where the pointer grabbed the thumb while a drag lasts, from the thumb's top.
#[derive(Default)]
struct DragState {
    grab: Option<Pixels>,
}

/// A vertical scrollbar laid over the right edge of its parent, which must be positioned
/// (`relative`) and the same size as the scrolled view. It shows while the content
/// overflows. Drag the thumb to scroll, or click the track to jump there. It is not a Tab
/// stop: the view it scrolls handles the keyboard.
#[derive(IntoElement)]
#[must_use]
pub struct Scrollbar {
    id: ElementId,
    label: SharedString,
    handle: Rc<dyn ScrollableHandle>,
}

impl Scrollbar {
    pub fn vertical(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        handle: impl ScrollableHandle,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            handle: Rc::new(handle),
        }
    }
}

impl RenderOnce for Scrollbar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let handle = self.handle;
        let (max_offset, offset) = (handle.max_offset(), handle.offset());
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| DragState::default());

        if max_offset <= px(0.) || handle.viewport_height() <= px(0.) {
            // The content can stop overflowing mid-drag; no mouse up reaches us then.
            if state.read(cx).grab.is_some() {
                state.update(cx, |state, _| state.grab = None);
                handle.drag_ended();
            }

            return div().id(self.id);
        }

        let colors = cx.theme().colors();
        let (rest, strong) = (colors.scrollbar_thumb, colors.faint);
        let percent = (progress(max_offset, offset) * 100.).round();

        let prepaint_handle = Rc::clone(&handle);
        let prepaint = move |bounds: Bounds<Pixels>, window: &mut Window, _: &mut App| {
            let track = track_bounds(bounds);
            let thumb = Thumb::measure(
                track.size.height,
                prepaint_handle.viewport_height(),
                prepaint_handle.max_offset(),
                prepaint_handle.offset(),
            );

            thumb.map(|thumb| Layout {
                hitbox: window.insert_hitbox(bounds, HitboxBehavior::Normal),
                track,
                thumb,
                view: window.current_view(),
            })
        };

        let paint = move |bounds: Bounds<Pixels>,
                          layout: Option<Layout>,
                          window: &mut Window,
                          cx: &mut App| {
            let Some(layout) = layout else {
                return;
            };

            let hovered = layout.hitbox.is_hovered(window);
            let dragging = state.read(cx).grab.is_some();
            let thumb_bounds = Bounds::new(
                point(
                    bounds.right() - px(THUMB_RIGHT + THUMB_WIDTH),
                    layout.track.top() + layout.thumb.top,
                ),
                size(px(THUMB_WIDTH), layout.thumb.height),
            );

            window.paint_quad(
                fill(
                    thumb_bounds,
                    if hovered || dragging { strong } else { rest },
                )
                .corner_radii(px(THUMB_WIDTH / 2.)),
            );
            window.set_cursor_style(CursorStyle::Arrow, &layout.hitbox);

            listen(&layout, &handle, &state, hovered, window);
        };

        accessible(div().id(self.id), Role::ScrollBar, self.label)
            .aria_orientation(Orientation::Vertical)
            .aria_value(format!("{percent}%"))
            .aria_numeric_value(f64::from(percent))
            .aria_min_numeric_value(0.)
            .aria_max_numeric_value(100.)
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .w(px(TRACK_WIDTH))
            .child(canvas(prepaint, paint).size_full())
    }
}

/// What the bar measured in prepaint, for painting and the mouse.
struct Layout {
    hitbox: Hitbox,
    track: Bounds<Pixels>,
    thumb: Thumb,
    view: EntityId,
}

/// A press on the thumb grabs it, a press on the track jumps there and grabs the
/// thumb's middle. Moves and the release are caught anywhere in the window.
fn listen(
    layout: &Layout,
    handle: &Rc<dyn ScrollableHandle>,
    state: &Entity<DragState>,
    hovered: bool,
    window: &mut Window,
) {
    let Layout {
        hitbox,
        track,
        thumb,
        view,
    } = layout;
    let (track, thumb, view) = (*track, *thumb, *view);

    let down = (Rc::clone(handle), state.clone(), hitbox.clone());
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        let (handle, state, hitbox) = &down;

        if phase != DispatchPhase::Bubble
            || event.button != MouseButton::Left
            || !hitbox.is_hovered(window)
        {
            return;
        }

        cx.stop_propagation();

        let y = event.position.y - track.top();
        let grab = if y >= thumb.top && y <= thumb.top + thumb.height {
            y - thumb.top
        } else {
            let half = thumb.height / 2.;
            let offset = thumb.offset_at(y - half, track.size.height, handle.max_offset());
            handle.set_offset(offset);
            half
        };

        handle.drag_started();
        state.update(cx, |state, _| state.grab = Some(grab));
        cx.notify(view);
    });

    let moved = (Rc::clone(handle), state.clone(), hitbox.clone());
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
        let (handle, state, hitbox) = &moved;

        match state.read(cx).grab {
            Some(grab) if phase == DispatchPhase::Capture => {
                // The release can happen outside the window, where no mouse up arrives.
                if event.pressed_button != Some(MouseButton::Left) {
                    state.update(cx, |state, _| state.grab = None);
                    handle.drag_ended();
                    cx.notify(view);
                    return;
                }

                let top = event.position.y - track.top() - grab;
                handle.set_offset(thumb.offset_at(top, track.size.height, handle.max_offset()));
                cx.notify(view);
            }
            None if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) != hovered => {
                cx.notify(view);
            }
            _ => {}
        }
    });

    let up = (Rc::clone(handle), state.clone());
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        let (handle, state) = &up;

        if phase != DispatchPhase::Capture
            || event.button != MouseButton::Left
            || state.read(cx).grab.is_none()
        {
            return;
        }

        state.update(cx, |state, _| state.grab = None);
        handle.drag_ended();
        cx.notify(view);
    });
}

fn track_bounds(bounds: Bounds<Pixels>) -> Bounds<Pixels> {
    let inset = px(TRACK_INSET);

    Bounds::new(
        point(bounds.left(), bounds.top() + inset),
        size(
            bounds.size.width,
            (bounds.size.height - inset * 2.).max(px(0.)),
        ),
    )
}

impl Component for Scrollbar {
    fn name() -> &'static str {
        "Scrollbar"
    }

    fn preview(window: &mut Window, cx: &mut App) -> AnyElement {
        let list_state = window
            .use_keyed_state("scrollbar-demo", cx, |_, _| {
                ListState::new(40, ListAlignment::Top, px(200.))
            })
            .read(cx)
            .clone();

        let rows = gpui::list(list_state.clone(), |index, _, _| {
            div()
                .h(px(24.))
                .px(px(10.))
                .flex()
                .items_center()
                .child(Label::new(format!("Line {}", index + 1)).size(LabelSize::Small))
                .into_any_element()
        })
        .size_full();

        v_flex()
            .gap_6()
            .child(example_group(
                "Vertical",
                [example(
                    "List",
                    div()
                        .relative()
                        .w(px(240.))
                        .h(px(160.))
                        .border_1()
                        .border_color(cx.theme().colors().line)
                        .child(rows)
                        .child(Scrollbar::vertical(
                            "scrollbar-demo-bar",
                            "Demo lines",
                            list_state,
                        )),
                )],
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui::{
        Context, Modifiers, MouseButton, Render, TestAppContext, VisualTestContext, list, size,
    };

    use super::*;

    #[test]
    fn thumb_height_is_the_share_of_content_in_view() {
        let thumb = Thumb::measure(px(200.), px(200.), px(600.), px(0.));

        assert_eq!(thumb.map(|thumb| thumb.height), Some(px(50.)));
    }

    #[test]
    fn thumb_never_gets_shorter_than_the_minimum() {
        let thumb = Thumb::measure(px(200.), px(200.), px(100_000.), px(0.));

        assert_eq!(thumb.map(|thumb| thumb.height), Some(px(MIN_THUMB)));
    }

    #[test]
    fn thumb_moves_with_the_scroll_offset() {
        let halfway = Thumb::measure(px(200.), px(200.), px(600.), px(300.));
        let end = Thumb::measure(px(200.), px(200.), px(600.), px(600.));

        assert_eq!(halfway.map(|thumb| thumb.top), Some(px(75.)));
        assert_eq!(end.map(|thumb| thumb.top), Some(px(150.)));
    }

    #[test]
    fn no_thumb_when_the_content_fits() {
        assert_eq!(Thumb::measure(px(200.), px(200.), px(0.), px(0.)), None);
    }

    #[test]
    fn thumb_position_maps_back_to_the_offset() {
        let Some(thumb) = Thumb::measure(px(200.), px(200.), px(600.), px(0.)) else {
            panic!("the content overflows");
        };

        assert_eq!(thumb.offset_at(px(75.), px(200.), px(600.)), px(300.));
        assert_eq!(thumb.offset_at(px(-20.), px(200.), px(600.)), px(0.));
        assert_eq!(thumb.offset_at(px(500.), px(200.), px(600.)), px(600.));
    }

    struct Lines {
        state: ListState,
    }

    impl Render for Lines {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .relative()
                .size(px(200.))
                .child(
                    list(self.state.clone(), |_, _, _| {
                        div().h(px(20.)).into_any_element()
                    })
                    .size_full(),
                )
                .child(Scrollbar::vertical("bar", "Lines", self.state.clone()))
        }
    }

    fn lines(app: &mut TestAppContext, count: usize) -> (ListState, &mut VisualTestContext) {
        app.update(crate::theme::init);

        let state = ListState::new(count, ListAlignment::Top, px(100.)).measure_all();
        let view_state = state.clone();
        let (_, cx) = app.add_window_view(|_, _| Lines { state: view_state });
        cx.simulate_resize(size(px(200.), px(200.)));
        cx.run_until_parked();

        (state, cx)
    }

    fn bar_x() -> Pixels {
        px(200. - THUMB_RIGHT - THUMB_WIDTH / 2.)
    }

    #[test]
    fn dragging_the_thumb_scrolls_the_list() {
        let mut app = TestAppContext::single();
        let (state, cx) = lines(&mut app, 40);
        let none = Modifiers::none();

        // Track 196 px, thumb 49 px: moving the thumb 73.5 px is half its 147 px range.
        cx.simulate_mouse_down(point(bar_x(), px(20.)), MouseButton::Left, none);
        cx.simulate_mouse_move(point(bar_x(), px(93.5)), MouseButton::Left, none);
        cx.simulate_mouse_up(point(bar_x(), px(93.5)), MouseButton::Left, none);
        cx.run_until_parked();

        let offset = ScrollableHandle::offset(&state);
        assert!((offset - px(300.)).abs() < px(1.), "offset is {offset:?}");
    }

    #[test]
    fn clicking_the_track_jumps_there() {
        let mut app = TestAppContext::single();
        let (state, cx) = lines(&mut app, 40);
        let none = Modifiers::none();

        cx.simulate_mouse_down(point(bar_x(), px(198.)), MouseButton::Left, none);
        cx.simulate_mouse_up(point(bar_x(), px(198.)), MouseButton::Left, none);
        cx.run_until_parked();

        assert_eq!(ScrollableHandle::offset(&state), px(600.));
    }

    #[test]
    fn the_scroll_stays_when_the_content_fits() {
        let mut app = TestAppContext::single();
        let (state, cx) = lines(&mut app, 5);
        let none = Modifiers::none();

        cx.simulate_mouse_down(point(bar_x(), px(150.)), MouseButton::Left, none);
        cx.simulate_mouse_up(point(bar_x(), px(150.)), MouseButton::Left, none);
        cx.run_until_parked();

        assert_eq!(ScrollableHandle::max_offset(&state), px(0.));
        assert_eq!(ScrollableHandle::offset(&state), px(0.));
    }
}
