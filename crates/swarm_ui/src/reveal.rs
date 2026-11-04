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
    /// may belong to the child or to an element inside it. Starts a reveal when focus has
    /// moved to another child, and repeats it until the child shows. Returns `true` when
    /// the container must draw again.
    pub fn settle(&mut self, handles: &[FocusHandle], window: &Window, cx: &App) -> bool {
        let focused = handles
            .iter()
            .position(|handle| handle.contains_focused(window, cx));

        if focused != self.focused {
            self.focused = focused;

            if let Some(index) = focused {
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
                    .h(px(30.))
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
        let (view, cx) = cx.add_window_view(|_, cx| List {
            handles: (0..ROWS).map(|_| cx.focus_handle()).collect(),
            reveal: None,
        });
        cx.run_until_parked();

        let (handles, reveal) = view.read_with(cx, |list, _| {
            (list.handles.clone(), list.reveal.clone().expect("rendered"))
        });

        (handles, reveal, cx)
    }

    /// Focuses the handle and draws, as a real window does when focus moves.
    fn focus(handle: &FocusHandle, cx: &mut VisualTestContext) {
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
}
