//! Roving focus: one Tab stop for a list, arrow keys inside it.

use gpui::{App, Axis, ElementId, FocusHandle, Keystroke, Window};

/// Focus handles for the items of a list. Only the active item is a Tab stop, so Tab
/// enters the list once and the arrow keys move inside it.
///
/// Keep it in keyed window state, call [`sync`](Self::sync) on every render, give each
/// item `handle(index)` with `track_focus`, and pass key presses to
/// [`handle_key`](Self::handle_key). Call [`follow_focus`](Self::follow_focus) after `sync`
/// so an item focused by a click becomes the active one.
pub struct RovingFocus {
    handles: Vec<FocusHandle>,
    /// The key of each handle, set by `sync_keyed`.
    keys: Vec<ElementId>,
    active: usize,
    axis: Axis,
    wrap: bool,
}

impl RovingFocus {
    /// Arrow keys along `axis` move focus. The ends do not wrap.
    pub fn new(axis: Axis) -> Self {
        Self {
            handles: Vec::new(),
            keys: Vec::new(),
            active: 0,
            axis,
            wrap: false,
        }
    }

    /// Makes the arrow keys wrap from one end to the other.
    #[must_use]
    pub fn wrapping(mut self) -> Self {
        self.wrap = true;
        self
    }

    /// Grows or shrinks to `len` handles and keeps `active` in range.
    pub fn sync(&mut self, len: usize, cx: &mut App) {
        self.keys.clear();
        self.handles.truncate(len);

        while self.handles.len() < len {
            self.handles.push(cx.focus_handle());
        }

        self.active = self.active.min(len.saturating_sub(1));
        self.refresh();
    }

    /// One handle per key. An item that keeps its key keeps its handle and, if it was
    /// active, stays active when items before it come or go. When the active item
    /// is gone, the item now at its index takes over, or the last one.
    pub fn sync_keyed(&mut self, keys: &[ElementId], cx: &mut App) {
        let mut old: Vec<Option<FocusHandle>> = std::mem::take(&mut self.handles)
            .into_iter()
            .map(Some)
            .collect();
        let old_keys = std::mem::take(&mut self.keys);
        let active_key = old_keys.get(self.active).cloned();

        self.handles = keys
            .iter()
            .map(|key| {
                old_keys
                    .iter()
                    .position(|old_key| old_key == key)
                    .and_then(|index| old.get_mut(index)?.take())
                    .unwrap_or_else(|| cx.focus_handle())
            })
            .collect();
        self.keys = keys.to_vec();

        let kept = active_key.and_then(|active| keys.iter().position(|key| *key == active));
        self.active = kept.unwrap_or_else(|| self.active.min(keys.len().saturating_sub(1)));
        self.refresh();
    }

    /// The item's handle. Only the active item is a Tab stop.
    ///
    /// # Panics
    ///
    /// When `index` is not below the length given to [`sync`](Self::sync).
    pub fn handle(&self, index: usize) -> FocusHandle {
        self.handles[index].clone()
    }

    pub fn len(&self) -> usize {
        self.handles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }

    pub fn active(&self) -> usize {
        self.active
    }

    /// Makes `index` the Tab stop. An index past the end is ignored.
    pub fn set_active(&mut self, index: usize) {
        if index < self.handles.len() {
            self.active = index;
            self.refresh();
        }
    }

    /// The index of the item that has focus, or has it inside.
    pub fn focused(&self, window: &Window, cx: &App) -> Option<usize> {
        self.handles
            .iter()
            .position(|handle| handle.contains_focused(window, cx))
    }

    /// Makes the focused item the active one, for focus that arrived by a click or by
    /// a `focus` call. Returns the index when focus is in the list.
    pub fn follow_focus(&mut self, window: &Window, cx: &App) -> Option<usize> {
        let focused = self.focused(window, cx)?;
        self.set_active(focused);

        Some(focused)
    }

    /// Arrows along the axis, Home and End: moves focus and the active item.
    /// Returns the new index, or `None` when the key is not one of these.
    pub fn handle_key(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<usize> {
        if keystroke.modifiers.modified() || self.handles.is_empty() {
            return None;
        }

        let last = self.handles.len() - 1;
        let (previous, next) = match self.axis {
            Axis::Horizontal => ("left", "right"),
            Axis::Vertical => ("up", "down"),
        };

        let key = keystroke.key.as_str();
        let target = if key == "home" {
            0
        } else if key == "end" {
            last
        } else if key == previous && self.wrap {
            (self.active + last) % (last + 1)
        } else if key == previous {
            self.active.saturating_sub(1)
        } else if key == next && self.wrap {
            (self.active + 1) % (last + 1)
        } else if key == next {
            (self.active + 1).min(last)
        } else {
            return None;
        };

        self.set_active(target);
        self.handles[target].focus(window, cx);

        Some(target)
    }

    fn refresh(&mut self) {
        let active = self.active;

        for (index, handle) in self.handles.iter_mut().enumerate() {
            *handle = handle.clone().tab_stop(index == active);
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Context, Render, TestAppContext, div};

    use super::*;
    use crate::prelude::*;

    struct Probe {
        roving: RovingFocus,
        len: usize,
    }

    impl Render for Probe {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.roving.sync(self.len, cx);

            div().children((0..self.len).map(|index| {
                div()
                    .id(("item", index))
                    .size(gpui::px(10.))
                    .track_focus(&self.roving.handle(index))
            }))
        }
    }

    fn probe(cx: &mut TestAppContext, axis: Axis, len: usize) -> gpui::WindowHandle<Probe> {
        let window = cx.add_window(|_, _| Probe {
            roving: RovingFocus::new(axis),
            len,
        });
        cx.run_until_parked();

        window
    }

    fn press(
        window: &gpui::WindowHandle<Probe>,
        key: &'static str,
        cx: &mut TestAppContext,
    ) -> Option<usize> {
        let keystroke = Keystroke::parse(key).expect("a key");

        cx.update(|cx| {
            window
                .update(cx, |probe, window, cx| {
                    probe.roving.handle_key(&keystroke, window, cx)
                })
                .expect("the window")
        })
    }

    fn focused_item(window: &gpui::WindowHandle<Probe>, cx: &mut TestAppContext) -> Option<usize> {
        cx.update(|cx| {
            window
                .update(cx, |probe, window, cx| probe.roving.focused(window, cx))
                .expect("the window")
        })
    }

    fn active(window: &gpui::WindowHandle<Probe>, cx: &mut TestAppContext) -> usize {
        cx.update(|cx| {
            window
                .read(cx)
                .map(|probe| probe.roving.active())
                .expect("the window")
        })
    }

    #[gpui::test]
    fn down_and_up_move_focus_and_the_active_item(cx: &mut TestAppContext) {
        let window = probe(cx, Axis::Vertical, 3);

        assert_eq!(press(&window, "down", cx), Some(1));
        assert_eq!(focused_item(&window, cx), Some(1));
        assert_eq!(active(&window, cx), 1);

        assert_eq!(press(&window, "up", cx), Some(0));
        assert_eq!(focused_item(&window, cx), Some(0));
        assert_eq!(active(&window, cx), 0);
    }

    #[gpui::test]
    fn the_arrows_of_the_other_axis_do_nothing(cx: &mut TestAppContext) {
        let window = probe(cx, Axis::Vertical, 3);

        assert_eq!(press(&window, "right", cx), None);
        assert_eq!(press(&window, "left", cx), None);
    }

    #[gpui::test]
    fn the_ends_do_not_wrap(cx: &mut TestAppContext) {
        let window = probe(cx, Axis::Horizontal, 3);

        assert_eq!(press(&window, "left", cx), Some(0));
        assert_eq!(press(&window, "end", cx), Some(2));
        assert_eq!(press(&window, "right", cx), Some(2));
    }

    #[gpui::test]
    fn home_and_end_jump_to_the_first_and_last_item(cx: &mut TestAppContext) {
        let window = probe(cx, Axis::Vertical, 4);

        assert_eq!(press(&window, "end", cx), Some(3));
        assert_eq!(focused_item(&window, cx), Some(3));

        assert_eq!(press(&window, "home", cx), Some(0));
        assert_eq!(focused_item(&window, cx), Some(0));
    }

    #[gpui::test]
    fn a_modified_arrow_is_left_alone(cx: &mut TestAppContext) {
        let window = probe(cx, Axis::Vertical, 3);

        assert_eq!(press(&window, "cmd-down", cx), None);
    }

    #[gpui::test]
    fn only_the_active_item_is_a_tab_stop(cx: &mut TestAppContext) {
        let window = probe(cx, Axis::Vertical, 3);
        press(&window, "down", cx);

        let stops = cx.update(|cx| {
            window
                .read(cx)
                .map(|probe| {
                    (0..3)
                        .map(|index| probe.roving.handle(index).tab_stop)
                        .collect::<Vec<_>>()
                })
                .expect("the window")
        });

        assert_eq!(stops, [false, true, false]);
    }

    #[gpui::test]
    fn syncing_to_a_shorter_list_keeps_a_valid_active_item(cx: &mut TestAppContext) {
        let window = probe(cx, Axis::Vertical, 5);
        press(&window, "end", cx);

        cx.update(|cx| {
            window
                .update(cx, |probe, _, cx| probe.roving.sync(2, cx))
                .expect("the window");
        });

        assert_eq!(active(&window, cx), 1);
    }

    struct Keyed {
        roving: RovingFocus,
        keys: Vec<ElementId>,
    }

    impl Render for Keyed {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.roving.sync_keyed(&self.keys, cx);

            div().children((0..self.keys.len()).map(|index| {
                div()
                    .id(("item", index))
                    .size(gpui::px(10.))
                    .track_focus(&self.roving.handle(index))
            }))
        }
    }

    fn keys(names: &[&'static str]) -> Vec<ElementId> {
        names.iter().map(|name| ElementId::from(*name)).collect()
    }

    fn keyed(cx: &mut TestAppContext, names: &[&'static str]) -> gpui::WindowHandle<Keyed> {
        let names = keys(names);
        let window = cx.add_window(move |_, _| Keyed {
            roving: RovingFocus::new(Axis::Horizontal),
            keys: names,
        });
        cx.run_until_parked();

        window
    }

    fn set_keys(
        window: &gpui::WindowHandle<Keyed>,
        names: &[&'static str],
        cx: &mut TestAppContext,
    ) {
        let names = keys(names);
        cx.update(|cx| {
            window
                .update(cx, |keyed, _, cx| {
                    keyed.keys = names;
                    cx.notify();
                })
                .expect("the window");
        });
        cx.run_until_parked();
    }

    fn keyed_state(
        window: &gpui::WindowHandle<Keyed>,
        cx: &mut TestAppContext,
    ) -> (Option<usize>, usize) {
        cx.update(|cx| {
            window
                .update(cx, |keyed, window, cx| {
                    (keyed.roving.focused(window, cx), keyed.roving.active())
                })
                .expect("the window")
        })
    }

    #[gpui::test]
    fn closing_an_earlier_item_keeps_focus_on_the_same_item(cx: &mut TestAppContext) {
        let window = keyed(cx, &["a", "b", "c", "d"]);
        cx.update(|cx| {
            window
                .update(cx, |keyed, window, cx| {
                    keyed.roving.set_active(2);
                    keyed.roving.handle(2).focus(window, cx);
                })
                .expect("the window");
        });

        set_keys(&window, &["b", "c", "d"], cx);

        assert_eq!(keyed_state(&window, cx), (Some(1), 1));
    }

    #[gpui::test]
    fn closing_the_focused_last_item_moves_the_active_item_to_the_new_last(
        cx: &mut TestAppContext,
    ) {
        let window = keyed(cx, &["a", "b", "c"]);
        cx.update(|cx| {
            window
                .update(cx, |keyed, window, cx| {
                    keyed.roving.set_active(2);
                    keyed.roving.handle(2).focus(window, cx);
                })
                .expect("the window");
        });

        set_keys(&window, &["a", "b"], cx);

        assert_eq!(keyed_state(&window, cx).1, 1);
    }

    #[gpui::test]
    fn follow_focus_makes_a_clicked_item_the_active_one(cx: &mut TestAppContext) {
        let window = probe(cx, Axis::Vertical, 3);

        let followed = cx.update(|cx| {
            window
                .update(cx, |probe, window, cx| {
                    probe.roving.handle(2).focus(window, cx);
                    probe.roving.follow_focus(window, cx)
                })
                .expect("the window")
        });

        assert_eq!(followed, Some(2));
        assert_eq!(active(&window, cx), 2);
    }
}
