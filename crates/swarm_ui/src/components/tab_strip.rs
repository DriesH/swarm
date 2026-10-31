use std::rc::Rc;

use gpui::{
    AnyElement, App, Context, Div, ElementId, FocusHandle, FontWeight, Hsla, KeyDownEvent, Role,
    SharedString, Stateful, Window, div, px,
};

use crate::component::{Component, example, example_group};
use crate::components::segmented_control::neighbor;
use crate::prelude::*;
use crate::styles::focus_ring;

type SelectHandler = Rc<dyn Fn(usize, &mut Window, &mut App) + 'static>;

struct StripTab {
    label: SharedString,
    count: Option<SharedString>,
}

/// Text tabs with an underline on the selected one. Left and Right move between tabs,
/// Home and End jump to the first and the last.
#[derive(IntoElement)]
#[must_use]
pub struct TabStrip {
    id: ElementId,
    label: SharedString,
    tabs: Vec<StripTab>,
    selected: usize,
    debug_selector: Option<String>,
    on_select: Option<SelectHandler>,
}

impl TabStrip {
    /// `label` names the whole strip for VoiceOver, for example "Inspector".
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            tabs: Vec::new(),
            selected: 0,
            debug_selector: None,
            on_select: None,
        }
    }

    pub fn tab(mut self, label: impl Into<SharedString>) -> Self {
        self.tabs.push(StripTab {
            label: label.into(),
            count: None,
        });
        self
    }

    /// A tab with a small count after its label.
    pub fn tab_with_count(
        mut self,
        label: impl Into<SharedString>,
        count: impl Into<SharedString>,
    ) -> Self {
        self.tabs.push(StripTab {
            label: label.into(),
            count: Some(count.into()),
        });
        self
    }

    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    /// Names the tabs `<prefix>-<index>` for `VisualTestContext::debug_bounds`. Only
    /// recorded in test builds.
    pub fn debug_selector(mut self, prefix: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(prefix());
        self
    }

    /// Called with the index of the tab the user picked.
    pub fn on_select(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

/// One focus handle per tab, kept in keyed window state because the strip is stateless.
struct Focus {
    handles: Vec<FocusHandle>,
}

impl Focus {
    /// Only the selected tab is a tab stop, so Tab enters the strip once.
    fn sync(&mut self, count: usize, selected: usize, cx: &mut Context<Self>) -> Vec<FocusHandle> {
        self.handles.truncate(count);

        while self.handles.len() < count {
            self.handles.push(cx.focus_handle());
        }

        self.handles = self
            .handles
            .iter()
            .enumerate()
            .map(|(index, handle)| handle.clone().tab_stop(index == selected))
            .collect();

        self.handles.clone()
    }
}

/// One tab. `position` is its index and the number of tabs; `colors` are the text and
/// the focus ring.
fn strip_tab(
    tab: StripTab,
    position: (usize, usize),
    is_selected: bool,
    handle: &FocusHandle,
    colors: (Hsla, Hsla),
) -> Stateful<Div> {
    let ((index, count), (fg, accent)) = (position, colors);
    let accessible_label = match &tab.count {
        Some(tab_count) => SharedString::from(format!("{}, {tab_count}", tab.label)),
        None => tab.label.clone(),
    };

    div()
        .id(("strip-tab", index))
        .role(Role::Tab)
        .aria_label(accessible_label)
        .aria_selected(is_selected)
        .aria_position_in_set(index + 1)
        .aria_size_of_set(count)
        .track_focus(handle)
        .cursor_pointer()
        .flex()
        .items_center()
        .gap(px(5.))
        .pb(px(9.))
        .mb(px(-1.))
        .border_b_2()
        .border_color(if is_selected {
            fg
        } else {
            gpui::transparent_black()
        })
        .focus_visible(move |style| focus_ring(style, accent))
        .child(
            Label::new(tab.label)
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
                }),
        )
        .children(tab.count.map(|tab_count| {
            Label::new(tab_count)
                .mono()
                .size(LabelSize::XSmall)
                .color(Color::Faint)
        }))
}

impl RenderOnce for TabStrip {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (fg, line, accent) = (colors.fg, colors.line, colors.accent);
        let (count, selected) = (self.tabs.len(), self.selected);

        let focus = window.with_element_namespace(self.id.clone(), |window| {
            window.use_keyed_state("focus", cx, |_, _| Focus {
                handles: Vec::new(),
            })
        });
        let handles = focus.update(cx, |focus, cx| focus.sync(count, selected, cx));

        let on_select = self.on_select;
        let debug_selector = self.debug_selector;
        let tabs = self
            .tabs
            .into_iter()
            .zip(&handles)
            .enumerate()
            .map(|(index, (tab, handle))| {
                let on_select = on_select.clone();

                strip_tab(tab, (index, count), index == selected, handle, (fg, accent))
                    .when_some(debug_selector.clone(), |element, prefix| {
                        element.debug_selector(move || format!("{prefix}-{index}"))
                    })
                    .when_some(on_select, |element, handler| {
                        element.on_click(move |_, window, cx| handler(index, window, cx))
                    })
            })
            .collect::<Vec<_>>();

        let on_key_down = on_select.map(|handler| {
            move |event: &KeyDownEvent, window: &mut Window, cx: &mut App| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                let key = event.keystroke.key.as_str();
                let next = match key {
                    "home" => Some(0),
                    "end" => count.checked_sub(1),
                    "left" | "right" => neighbor(key, selected, count),
                    _ => None,
                };
                let Some(next) = next else {
                    return;
                };

                cx.stop_propagation();
                handler(next, window, cx);

                if let Some(handle) = handles.get(next) {
                    handle.focus(window, cx);
                }
            }
        });

        div()
            .id(self.id)
            .role(Role::TabList)
            .aria_label(self.label)
            .flex()
            .gap(px(18.))
            .border_b_1()
            .border_color(line)
            .when_some(on_key_down, |strip, handler| strip.on_key_down(handler))
            .children(tabs)
    }
}

impl Component for TabStrip {
    fn name() -> &'static str {
        "TabStrip"
    }

    fn preview(window: &mut Window, cx: &mut App) -> AnyElement {
        let state = window.use_keyed_state("tab-strip-demo", cx, |_, _| 0_usize);
        let selected = *state.read(cx);

        v_flex()
            .gap_6()
            .child(example_group(
                "Inspector tabs",
                [example(
                    "Interactive",
                    div().w(px(320.)).child(
                        TabStrip::new("inspector-tabs", "Inspector")
                            .tab_with_count("Messages", "3")
                            .tab_with_count("Changes", "2")
                            .tab("Terminal")
                            .selected(selected)
                            .on_select(move |index, _, cx| {
                                state.update(cx, |state, cx| {
                                    *state = index;
                                    cx.notify();
                                });
                            }),
                    ),
                )],
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui::accesskit::Node;
    use gpui::{Element as _, Render, TestAppContext};

    use super::*;

    /// Role, label, selected and position of one tab.
    type TabInfo = (Option<Role>, Option<String>, Option<bool>, Option<usize>);

    struct Probe {
        tab_stops: Vec<bool>,
        strip: Option<(Option<Role>, Option<String>)>,
        tabs: Vec<TabInfo>,
    }

    impl Render for Probe {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let focus = window.use_keyed_state("focus", cx, |_, _| Focus {
                handles: Vec::new(),
            });
            let handles = focus.update(cx, |focus, cx| focus.sync(3, 1, cx));
            self.tab_stops = handles.iter().map(|handle| handle.tab_stop).collect();

            let strip = TabStrip::new("strip", "Inspector")
                .tab_with_count("Messages", "3")
                .tab("Changes")
                .tab("Terminal")
                .render(window, cx)
                .into_element();
            let mut node = Node::new(strip.a11y_role().unwrap_or(Role::Unknown));
            strip.write_a11y_info(&mut node);
            self.strip = Some((strip.a11y_role(), node.label().map(str::to_owned)));

            self.tabs = handles
                .iter()
                .zip([
                    StripTab {
                        label: "Messages".into(),
                        count: Some("3".into()),
                    },
                    StripTab {
                        label: "Changes".into(),
                        count: None,
                    },
                    StripTab {
                        label: "Terminal".into(),
                        count: None,
                    },
                ])
                .enumerate()
                .map(|(index, (handle, tab))| {
                    let element = strip_tab(
                        tab,
                        (index, 3),
                        index == 1,
                        handle,
                        (Hsla::default(), Hsla::default()),
                    );
                    let mut node = Node::new(element.a11y_role().unwrap_or(Role::Unknown));
                    element.write_a11y_info(&mut node);

                    (
                        element.a11y_role(),
                        node.label().map(str::to_owned),
                        node.is_selected(),
                        node.position_in_set(),
                    )
                })
                .collect();

            div()
        }
    }

    fn probe(app: &mut TestAppContext) -> gpui::WindowHandle<Probe> {
        app.update(crate::theme::init);
        let window = app.add_window(|_, _| Probe {
            tab_stops: Vec::new(),
            strip: None,
            tabs: Vec::new(),
        });
        app.run_until_parked();

        window
    }

    #[test]
    fn only_the_selected_tab_is_a_tab_stop() {
        let mut app = TestAppContext::single();
        let window = probe(&mut app);

        let tab_stops = app.update(|cx| window.read(cx).map(|probe| probe.tab_stops.clone()));

        assert_eq!(tab_stops.ok(), Some(vec![false, true, false]));
    }

    #[test]
    fn tab_strip_is_a_labelled_tab_list() {
        let mut app = TestAppContext::single();
        let window = probe(&mut app);

        let (strip, tabs) = app
            .update(|cx| {
                window
                    .read(cx)
                    .map(|probe| (probe.strip.clone(), probe.tabs.clone()))
            })
            .expect("the probe");

        assert_eq!(
            strip,
            Some((Some(Role::TabList), Some("Inspector".to_owned())))
        );
        assert_eq!(
            tabs,
            [
                (
                    Some(Role::Tab),
                    Some("Messages, 3".to_owned()),
                    Some(false),
                    Some(1)
                ),
                (
                    Some(Role::Tab),
                    Some("Changes".to_owned()),
                    Some(true),
                    Some(2)
                ),
                (
                    Some(Role::Tab),
                    Some("Terminal".to_owned()),
                    Some(false),
                    Some(3)
                ),
            ]
        );
    }
}
