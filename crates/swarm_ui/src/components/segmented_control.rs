use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Bounds, Context, ElementId, Entity, FocusHandle,
    FontWeight, Hsla, KeyDownEvent, Pixels, Role, SharedString, Toggled, Window, div,
    ease_out_quint, px,
};

use crate::component::{Component, example, example_group};
use crate::prelude::*;
use crate::styles::focus_ring;

type SelectHandler = Rc<dyn Fn(usize, &mut Window, &mut App) + 'static>;

/// A row of mutually exclusive options, like Terminals | Swarm | Review.
#[derive(IntoElement)]
#[must_use]
pub struct SegmentedControl {
    id: ElementId,
    label: SharedString,
    segments: Vec<SharedString>,
    selected: usize,
    debug_selector: Option<String>,
    on_select: Option<SelectHandler>,
}

impl SegmentedControl {
    /// `label` names the whole group for VoiceOver, for example "View".
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        segments: impl IntoIterator<Item = impl Into<SharedString>>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            segments: segments.into_iter().map(Into::into).collect(),
            selected: 0,
            debug_selector: None,
            on_select: None,
        }
    }

    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    /// Name for `VisualTestContext::debug_bounds`. Only recorded in test builds.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }

    /// Called with the index of the segment the user picked.
    pub fn on_select(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

/// Where each segment sits, measured after layout, so one highlight can glide
/// between them. Lives in keyed window state because the control is stateless.
struct Glide {
    /// (x from the first segment, width) per segment.
    segments: Vec<(Pixels, Pixels)>,
    height: Pixels,
    selected: usize,
    previous: usize,
    /// Bumped on every change, so the animation restarts.
    generation: usize,
    /// When the running animation started.
    started: Option<Instant>,
    /// The pill's latest animated (x, width), written on every animation frame.
    current: Rc<Cell<Option<(Pixels, Pixels)>>>,
    /// Where the running animation was when the selection changed again.
    resume: Option<(Pixels, Pixels)>,
    /// One focus handle per segment; only the selected one is a tab stop.
    focus: Vec<FocusHandle>,
}

/// The segment an arrow key selects, wrapping at both ends.
fn neighbor(key: &str, selected: usize, count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }

    match key {
        "left" | "up" => Some((selected + count - 1) % count),
        "right" | "down" => Some((selected + 1) % count),
        _ => None,
    }
}

const PADDING: Pixels = px(2.);
const GLIDE_DURATION: Duration = Duration::from_millis(180);

fn lerp(from: Pixels, to: Pixels, delta: f32) -> Pixels {
    from + (to - from) * delta
}

impl Glide {
    /// Records a new selection, so the next frame animates from the old one.
    /// An animation still running hands over its current position.
    fn select(&mut self, selected: usize) {
        if self.selected != selected {
            let running = self
                .started
                .is_some_and(|started| started.elapsed() < GLIDE_DURATION);

            self.resume = if running { self.current.get() } else { None };
            self.previous = self.selected;
            self.selected = selected;
            self.generation += 1;
            self.started = Some(Instant::now());
        }
    }

    /// Gives every segment a focus handle and makes only the selected one a tab stop.
    fn sync_focus(&mut self, count: usize, cx: &mut Context<Self>) -> Vec<FocusHandle> {
        self.focus.truncate(count);

        while self.focus.len() < count {
            self.focus.push(cx.focus_handle());
        }

        let selected = self.selected;
        self.focus = self
            .focus
            .iter()
            .enumerate()
            .map(|(index, handle)| handle.clone().tab_stop(index == selected))
            .collect();

        self.focus.clone()
    }

    /// The sliding highlight, once the segments have been measured.
    fn pill(&self, count: usize, color: Hsla) -> Option<AnyElement> {
        if self.segments.len() != count {
            return None;
        }

        let to = *self.segments.get(self.selected)?;
        let from = self
            .resume
            .or_else(|| self.segments.get(self.previous).copied())
            .unwrap_or(to);
        let current = Rc::clone(&self.current);

        let pill = div()
            .absolute()
            .top(PADDING)
            .left(PADDING + to.0)
            .w(to.1)
            .h(self.height)
            .rounded(px(6.))
            .bg(color);

        // The first render places the pill without animating.
        if self.generation == 0 {
            return Some(pill.into_any_element());
        }

        let animated = pill.with_animation(
            ("glide", self.generation),
            Animation::new(GLIDE_DURATION).with_easing(ease_out_quint()),
            move |pill, delta| {
                let (x, width) = (lerp(from.0, to.0, delta), lerp(from.1, to.1, delta));
                current.set(Some((x, width)));

                pill.left(PADDING + x).w(width)
            },
        );

        Some(animated.into_any_element())
    }

    /// Stores segment positions after layout and redraws once if they changed.
    fn measure(glide: &Entity<Self>, bounds: &[Bounds<Pixels>], cx: &mut App) {
        let Some(first) = bounds.first() else {
            return;
        };

        let segments: Vec<(Pixels, Pixels)> = bounds
            .iter()
            .map(|segment| (segment.origin.x - first.origin.x, segment.size.width))
            .collect();
        let height = first.size.height;

        glide.update(cx, |glide, cx| {
            if glide.segments != segments || glide.height != height {
                glide.segments = segments;
                glide.height = height;
                cx.notify();
            }
        });
    }
}

struct SegmentStyle {
    count: usize,
    selected: usize,
    /// Paint the selected background on the segment itself, before the pill can be placed.
    own_background: bool,
    active: Hsla,
    accent: Hsla,
}

fn render_segment(
    index: usize,
    label: SharedString,
    focus: &FocusHandle,
    style: &SegmentStyle,
    on_select: Option<SelectHandler>,
) -> impl IntoElement {
    let is_selected = index == style.selected;
    let (active, accent) = (style.active, style.accent);

    let visible_label = Label::new(label.clone())
        .size(LabelSize::Small)
        .weight(if is_selected {
            FontWeight::MEDIUM
        } else {
            FontWeight::NORMAL
        })
        .color(if is_selected {
            Color::Default
        } else {
            Color::Muted
        });

    div()
        .id(("segment", index))
        .role(Role::RadioButton)
        .aria_label(label.clone())
        .aria_toggled(if is_selected {
            Toggled::True
        } else {
            Toggled::False
        })
        .aria_position_in_set(index + 1)
        .aria_size_of_set(style.count)
        .track_focus(focus)
        .cursor_pointer()
        .relative()
        .px(px(10.))
        .py(px(3.))
        .rounded(px(6.))
        .when(is_selected && style.own_background, |segment| {
            segment.bg(active)
        })
        .focus_visible(|focus| focus_ring(focus, accent))
        // An invisible medium-weight copy fixes the width, so selecting never shifts the row.
        .child(
            div().invisible().child(
                Label::new(label)
                    .size(LabelSize::Small)
                    .weight(FontWeight::MEDIUM),
            ),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(visible_label),
        )
        .when_some(on_select, |segment, handler| {
            segment.on_click(move |_, window, cx| handler(index, window, cx))
        })
}

impl RenderOnce for SegmentedControl {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (raised, line, active, accent) =
            (colors.raised, colors.line, colors.active, colors.accent);
        let count = self.segments.len();
        let selected = self.selected;

        let glide = window.with_element_namespace(self.id.clone(), |window| {
            window.use_keyed_state("glide", cx, |_, _| Glide {
                segments: Vec::new(),
                height: px(0.),
                selected,
                previous: selected,
                generation: 0,
                started: None,
                current: Rc::new(Cell::new(None)),
                resume: None,
                focus: Vec::new(),
            })
        });
        let focus = glide.update(cx, |glide, cx| {
            glide.select(selected);
            glide.sync_focus(count, cx)
        });

        let pill = glide.read(cx).pill(count, active);
        let style = SegmentStyle {
            count,
            selected,
            own_background: pill.is_none(),
            active,
            accent,
        };

        let on_select = self.on_select;
        let segments =
            self.segments
                .into_iter()
                .zip(&focus)
                .enumerate()
                .map(|(index, (label, handle))| {
                    render_segment(index, label, handle, &style, on_select.clone())
                });

        let on_key_down = on_select.clone().map(|handler| {
            let focus = focus.clone();
            move |event: &KeyDownEvent, window: &mut Window, cx: &mut App| {
                if event.keystroke.modifiers.modified() {
                    return;
                }

                let Some(next) = neighbor(&event.keystroke.key, selected, count) else {
                    return;
                };

                cx.stop_propagation();
                handler(next, window, cx);

                if let Some(handle) = focus.get(next) {
                    handle.focus(window, cx);
                }
            }
        });

        let row = div()
            .flex()
            .gap(px(2.))
            .on_children_prepainted(move |bounds, _, cx| Glide::measure(&glide, &bounds, cx))
            .children(segments);

        div()
            .id(self.id)
            .role(Role::RadioGroup)
            .aria_label(self.label)
            .relative()
            .flex()
            .flex_none()
            .p(PADDING)
            .rounded(px(8.))
            .bg(raised)
            .border_1()
            .border_color(line)
            .when_some(self.debug_selector, |group, selector| {
                group.debug_selector(move || selector)
            })
            .when_some(on_key_down, |group, handler| group.on_key_down(handler))
            .children(pill)
            .child(row)
    }
}

impl Component for SegmentedControl {
    fn name() -> &'static str {
        "SegmentedControl"
    }

    fn preview(window: &mut Window, cx: &mut App) -> AnyElement {
        let state = window.use_keyed_state("segmented-demo", cx, |_, _| 0_usize);
        let selected = *state.read(cx);

        v_flex()
            .gap_6()
            .child(example_group(
                "View switch",
                [example(
                    "Interactive",
                    SegmentedControl::new("view", "View", ["Terminals", "Swarm", "Review"])
                        .selected(selected)
                        .on_select(move |index, _, cx| {
                            state.update(cx, |state, cx| {
                                *state = index;
                                cx.notify();
                            });
                        }),
                )],
            ))
            .child(example_group(
                "Two options",
                [example(
                    "Second selected",
                    SegmentedControl::new("group-by", "Group by", ["Worker", "Folder"]).selected(1),
                )],
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Render, TestAppContext};

    use super::*;

    #[test]
    fn arrows_wrap_around_the_ends() {
        assert_eq!(neighbor("left", 0, 3), Some(2));
        assert_eq!(neighbor("up", 1, 3), Some(0));
        assert_eq!(neighbor("right", 2, 3), Some(0));
        assert_eq!(neighbor("down", 0, 3), Some(1));
        assert_eq!(neighbor("tab", 0, 3), None);
    }

    struct Probe {
        tab_stops: Vec<bool>,
    }

    impl Render for Probe {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let glide = window.use_keyed_state("glide", cx, |_, _| Glide {
                segments: Vec::new(),
                height: px(0.),
                selected: 1,
                previous: 1,
                generation: 0,
                started: None,
                current: Rc::new(Cell::new(None)),
                resume: None,
                focus: Vec::new(),
            });
            let handles = glide.update(cx, |glide, cx| glide.sync_focus(3, cx));
            self.tab_stops = handles.iter().map(|handle| handle.tab_stop).collect();

            div()
        }
    }

    #[test]
    fn only_the_selected_segment_is_a_tab_stop() {
        let mut app = TestAppContext::single();
        let window = app.add_window(|_, _| Probe {
            tab_stops: Vec::new(),
        });
        app.run_until_parked();

        let tab_stops = app.update(|cx| window.read(cx).map(|probe| probe.tab_stops.clone()));

        assert_eq!(tab_stops.ok(), Some(vec![false, true, false]));
    }
}
