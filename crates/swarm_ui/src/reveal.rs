//! Scrolls a container so the child that takes keyboard focus is fully in view.
//!
//! GPUI's `focus_next` and `focus_prev` move focus but never scroll.

use gpui::{
    App, Axis, Div, ElementId, Entity, FocusHandle, Pixels, ScrollHandle, Stateful, Window, px,
};

use crate::prelude::*;

/// Scroll positions closer than this to an edge count as the edge.
const EDGE: Pixels = px(1.);

/// How many frames to try to bring a child into view. Layout from the previous frame can
/// be a little off, for example while a button appears.
const TRIES: u8 = 4;

/// Scroll state of a container that keeps its focused child in view.
pub struct RevealFocus {
    scroll: ScrollHandle,
    axis: Axis,
    focused: Option<usize>,
    pending: Option<(usize, u8)>,
}

impl RevealFocus {
    pub fn new(axis: Axis) -> Self {
        Self {
            scroll: ScrollHandle::new(),
            axis,
            focused: None,
            pending: None,
        }
    }

    /// The handle to give the container's `track_scroll`.
    pub fn scroll(&self) -> &ScrollHandle {
        &self.scroll
    }

    /// Scrolls the least amount that brings the child at `index` into view, for example
    /// the child that was just selected.
    pub fn reveal(&mut self, index: usize) {
        self.pending = Some((index, TRIES));
        self.scroll.scroll_to_item(index);
    }

    /// Runs after layout. `handles` holds one focus handle per child, in order; a handle
    /// may belong to the child or to an element inside it. Starts a reveal when keyboard
    /// focus has moved to another child (a click never scrolls), and repeats it until the child shows. Returns `true` when
    /// the container must draw again.
    pub fn settle(&mut self, handles: &[FocusHandle], window: &Window, cx: &App) -> bool {
        let focused = handles
            .iter()
            .position(|handle| handle.contains_focused(window, cx));

        if focused != self.focused {
            self.focused = focused;

            if let Some(index) = focused
                && window.last_input_was_keyboard()
            {
                self.pending = Some((index, TRIES));
            }
        }

        let Some((index, tries)) = self.pending else {
            return false;
        };

        if tries == 0 || self.is_visible(index) {
            self.pending = None;

            return false;
        }

        self.scroll.scroll_to_item(index);
        self.pending = Some((index, tries - 1));

        true
    }

    fn is_visible(&self, index: usize) -> bool {
        let Some(child) = self.scroll.bounds_for_item(index) else {
            return true;
        };

        let view = self.scroll.bounds();
        let offset = self.scroll.offset();

        let (start, end, view_start, view_end) = match self.axis {
            Axis::Horizontal => (
                child.left() + offset.x,
                child.right() + offset.x,
                view.left(),
                view.right(),
            ),
            Axis::Vertical => (
                child.top() + offset.y,
                child.bottom() + offset.y,
                view.top(),
                view.bottom(),
            ),
        };

        // A child taller than the view counts as visible while it overlaps the view.
        if end - start > view_end - view_start {
            return end > view_start && start < view_end;
        }

        start >= view_start - EDGE && end <= view_end + EDGE
    }
}

/// Makes `container` scroll the child that takes focus into view, and gives it its `id`.
/// `reveal` holds the state in keyed window state; `handles` has one focus handle per
/// child, in order. The container scrolls along the state's axis.
///
/// ```ignore
/// let reveal = window.use_keyed_state("rows", cx, |_, _| RevealFocus::new(Axis::Vertical));
/// let handles = rows.iter().map(|row| row.focus.clone()).collect();
///
/// let list = reveal_focused(v_flex().h(px(200.)), "rows", &reveal, handles, cx).children(rows);
/// ```
pub fn reveal_focused(
    container: Div,
    id: impl Into<ElementId>,
    reveal: &Entity<RevealFocus>,
    handles: Vec<FocusHandle>,
    cx: &App,
) -> Stateful<Div> {
    let (scroll, axis) = {
        let state = reveal.read(cx);
        (state.scroll.clone(), state.axis)
    };
    let reveal = reveal.clone();

    let container = container
        .on_children_prepainted(move |_, window, cx| {
            reveal.update(cx, |state, cx| {
                if state.settle(&handles, window, cx) {
                    cx.notify();
                }
            });
        })
        .id(id);

    match axis {
        Axis::Horizontal => container.overflow_x_scroll(),
        Axis::Vertical => container.overflow_y_scroll(),
    }
    .track_scroll(&scroll)
}

#[cfg(test)]
mod tests {
    use gpui::{Context, Render, TestAppContext, VisualTestContext, div};

    use super::*;

    const ROWS: usize = 10;

    struct List {
        handles: Vec<FocusHandle>,
        row_height: Pixels,
        reveal: Option<Entity<RevealFocus>>,
    }

    impl Render for List {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let reveal = window
                .use_keyed_state("rows", cx, |_, _| RevealFocus::new(Axis::Vertical))
                .clone();
            self.reveal = Some(reveal.clone());

            reveal_focused(
                div().flex().flex_col().h(px(100.)),
                "rows",
                &reveal,
                self.handles.clone(),
                cx,
            )
            .children(self.handles.iter().enumerate().map(|(index, handle)| {
                div()
                    .id(("row", index))
                    .flex_none()
                    .h(self.row_height)
                    .track_focus(handle)
            }))
        }
    }

    fn list(
        cx: &mut TestAppContext,
    ) -> (
        Vec<FocusHandle>,
        Entity<RevealFocus>,
        &mut VisualTestContext,
    ) {
        sized_list(cx, px(30.), ROWS)
    }

    fn sized_list(
        cx: &mut TestAppContext,
        row_height: Pixels,
        rows: usize,
    ) -> (
        Vec<FocusHandle>,
        Entity<RevealFocus>,
        &mut VisualTestContext,
    ) {
        let (view, cx) = cx.add_window_view(|_, cx| List {
            handles: (0..rows).map(|_| cx.focus_handle()).collect(),
            row_height,
            reveal: None,
        });
        cx.run_until_parked();

        let (handles, reveal) = view.read_with(cx, |list, _| {
            (list.handles.clone(), list.reveal.clone().expect("rendered"))
        });

        (handles, reveal, cx)
    }

    /// Focuses the handle after a key press and draws, as a real window does when focus
    /// moves with the keyboard.
    fn focus(handle: &FocusHandle, cx: &mut VisualTestContext) {
        cx.simulate_keystrokes("tab");
        focus_as_is(handle, cx);
    }

    /// Focuses the handle without any key press: the last input stays the mouse.
    fn focus_as_is(handle: &FocusHandle, cx: &mut VisualTestContext) {
        cx.update(|window, cx| handle.focus(window, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    }

    fn offset(reveal: &Entity<RevealFocus>, cx: &mut VisualTestContext) -> Pixels {
        cx.update(|_, cx| reveal.read(cx).scroll().offset().y)
    }

    #[gpui::test]
    fn focusing_the_last_row_scrolls_it_into_view(cx: &mut TestAppContext) {
        let (handles, reveal, cx) = list(cx);
        assert_eq!(offset(&reveal, cx), Pixels::ZERO);

        focus(&handles[ROWS - 1], cx);

        // Ten rows of 30 px in a 100 px view: the last row ends at 300 px.
        assert_eq!(offset(&reveal, cx), px(-200.));
    }

    #[gpui::test]
    fn focusing_a_visible_row_does_not_scroll(cx: &mut TestAppContext) {
        let (handles, reveal, cx) = list(cx);

        focus(&handles[1], cx);

        assert_eq!(offset(&reveal, cx), Pixels::ZERO);
    }

    #[gpui::test]
    fn focusing_a_row_just_below_the_view_scrolls_the_least(cx: &mut TestAppContext) {
        let (handles, reveal, cx) = list(cx);

        focus(&handles[3], cx);

        // Row 3 ends at 120 px; the view shows 100 px.
        assert_eq!(offset(&reveal, cx), px(-20.));
    }

    #[gpui::test]
    fn focusing_a_row_by_click_does_not_scroll(cx: &mut TestAppContext) {
        let (handles, reveal, cx) = list(cx);

        focus_as_is(&handles[ROWS - 1], cx);

        assert_eq!(offset(&reveal, cx), Pixels::ZERO);
    }

    #[gpui::test]
    fn a_row_taller_than_the_view_in_view_does_not_snap(cx: &mut TestAppContext) {
        // Two rows of 300 px in a 100 px view. Scroll to the bottom of row 0.
        let (handles, reveal, cx) = sized_list(cx, px(300.), 2);
        cx.update(|_, cx| {
            reveal
                .read(cx)
                .scroll()
                .set_offset(gpui::point(px(0.), px(-200.)));
        });

        focus(&handles[0], cx);

        assert_eq!(offset(&reveal, cx), px(-200.));
    }
}
