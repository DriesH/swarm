use std::rc::Rc;

use gpui::{
    AnyElement, App, Div, ElementId, Entity, FocusHandle, Pixels, Role, ScrollHandle, SharedString,
    Stateful, Window, div, linear_color_stop, linear_gradient, px,
};

use crate::component::{Component, example, example_group};
use crate::components::menu::{Menu, MenuItem};
use crate::components::popover_menu::popover_state;
use crate::prelude::*;
use crate::styles::focus_ring;

type SelectHandler = Rc<dyn Fn(usize, &mut Window, &mut App) + 'static>;

const GAP: Pixels = px(4.);
const FADE_WIDTH: Pixels = px(24.);
const OVERFLOW_WIDTH: Pixels = px(44.);
const OVERFLOW_MENU_WIDTH: Pixels = px(280.);
const OVERFLOW_MENU_HEIGHT: Pixels = px(360.);
/// Scroll positions closer than this to an edge count as the edge.
const EDGE: Pixels = px(1.);
/// How many frames the bar tries to bring a tab into view. Layout from the previous
/// frame can be a little off, for example while the overflow button appears.
const REVEAL_TRIES: u8 = 4;

/// A row of [`Tab`]s that never grows past its container. Tabs shrink down to
/// [`TAB_MIN_WIDTH`](crate::TAB_MIN_WIDTH), then the row scrolls sideways without a
/// scrollbar, with a fade on an edge that hides tabs. When the tabs overflow, a button
/// before the trailing controls opens a menu of every tab. The selected tab and a tab
/// that takes keyboard focus scroll into view.
///
/// The bar sets each tab's selected state and click handler. Fades are drawn in the
/// `panel` color, so the bar belongs on a `panel` surface.
#[derive(IntoElement)]
#[must_use]
pub struct TabBar {
    id: ElementId,
    label: SharedString,
    overflow_label: SharedString,
    tabs: Vec<Tab>,
    selected: usize,
    trailing: Vec<AnyElement>,
    debug_selector: Option<String>,
    on_select: Option<SelectHandler>,
}

impl TabBar {
    /// `label` names the tab list for VoiceOver, for example "Tasks".
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            overflow_label: "All tabs".into(),
            tabs: Vec::new(),
            selected: 0,
            trailing: Vec::new(),
            debug_selector: None,
            on_select: None,
        }
    }

    pub fn tab(mut self, tab: Tab) -> Self {
        self.tabs.push(tab);
        self
    }

    pub fn tabs(mut self, tabs: impl IntoIterator<Item = Tab>) -> Self {
        self.tabs.extend(tabs);
        self
    }

    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    /// Names the overflow button and its menu. VoiceOver reads the button as
    /// "<label>, <number of tabs>", for example "All tasks, 9".
    pub fn overflow_label(mut self, label: impl Into<SharedString>) -> Self {
        self.overflow_label = label.into();
        self
    }

    /// Controls after the tabs, such as a new tab button. They stay visible.
    pub fn trailing(mut self, element: impl IntoElement) -> Self {
        self.trailing.push(element.into_any_element());
        self
    }

    /// Names the scrolling row `<prefix>`, the overflow button `<prefix>-overflow` and the
    /// edge fades `<prefix>-fade-start` and `<prefix>-fade-end` for
    /// `VisualTestContext::debug_bounds`. Only recorded in test builds.
    pub fn debug_selector(mut self, prefix: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(prefix());
        self
    }

    /// Called with the index of the tab the user picked, from a tab or from the overflow menu.
    pub fn on_select(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

/// Scroll, overflow and focus state, kept in keyed window state because the bar is stateless.
struct BarState {
    scroll: ScrollHandle,
    focus: Vec<(ElementId, FocusHandle)>,
    order: Vec<ElementId>,
    overflowing: bool,
    fade_start: bool,
    fade_end: bool,
    /// The tab last scrolled to for being selected.
    revealed: Option<ElementId>,
    /// The tab to bring into view, and the frames left to try.
    reveal: Option<(usize, u8)>,
    /// The tab that had keyboard focus after the last layout.
    focused: Option<usize>,
}

impl BarState {
    fn new() -> Self {
        Self {
            scroll: ScrollHandle::new(),
            focus: Vec::new(),
            order: Vec::new(),
            overflowing: false,
            fade_start: false,
            fade_end: false,
            revealed: None,
            reveal: None,
            focused: None,
        }
    }

    /// One focus handle per tab, in tab order. A handle stays with its tab when others close.
    fn sync(
        &mut self,
        ids: &[ElementId],
        selected: usize,
        cx: &mut Context<Self>,
    ) -> Vec<FocusHandle> {
        self.focus.retain(|(id, _)| ids.contains(id));

        let handles = ids
            .iter()
            .map(|id| {
                if let Some((_, handle)) = self.focus.iter().find(|(known, _)| known == id) {
                    return handle.clone();
                }

                let handle = cx.focus_handle().tab_stop(true);
                self.focus.push((id.clone(), handle.clone()));

                handle
            })
            .collect();

        self.order = ids.to_vec();

        let current = ids.get(selected);
        if self.revealed.as_ref() != current {
            self.revealed = current.cloned();
            self.reveal = Some((selected, REVEAL_TRIES));
            self.scroll.scroll_to_item(selected);
        }

        handles
    }

    fn is_visible(&self, index: usize) -> bool {
        let Some(tab) = self.scroll.bounds_for_item(index) else {
            return true;
        };
        let row = self.scroll.bounds();
        let offset = self.scroll.offset().x;

        tab.left() + offset >= row.left() - EDGE && tab.right() + offset <= row.right() + EDGE
    }

    fn focused_tab(&self, window: &Window) -> Option<usize> {
        self.order.iter().position(|id| {
            self.focus
                .iter()
                .any(|(known, handle)| known == id && handle.is_focused(window))
        })
    }

    /// Runs after layout, when the scroll range of this frame is known.
    fn settle(&mut self, window: &Window, cx: &mut Context<Self>) {
        let max = self.scroll.max_offset().x;
        let offset = self.scroll.offset().x;

        // With the button shown, the row is narrower by the button and a gap. Counting that
        // back gives the same answer whether or not the button is shown.
        let extra = if self.overflowing {
            OVERFLOW_WIDTH + GAP
        } else {
            Pixels::ZERO
        };
        let overflowing = max - extra > EDGE;
        let fade_start = overflowing && offset < -EDGE;
        let fade_end = overflowing && max + offset > EDGE;

        let mut changed = (overflowing, fade_start, fade_end)
            != (self.overflowing, self.fade_start, self.fade_end);
        (self.overflowing, self.fade_start, self.fade_end) = (overflowing, fade_start, fade_end);

        let focused = self.focused_tab(window);
        if focused != self.focused {
            self.focused = focused;

            if let Some(index) = focused {
                self.reveal = Some((index, REVEAL_TRIES));
            }
        }

        if let Some((index, tries)) = self.reveal {
            if tries == 0 || self.is_visible(index) {
                self.reveal = None;
            } else {
                self.scroll.scroll_to_item(index);
                self.reveal = Some((index, tries - 1));
                changed = true;
            }
        }

        if changed {
            cx.notify();
        }
    }
}

fn fade(at_start: bool, color: gpui::Hsla, selector: Option<String>) -> Div {
    let clear = color.opacity(0.);
    let (from, to) = if at_start {
        (color, clear)
    } else {
        (clear, color)
    };

    div()
        .when_some(selector, |fade, selector| {
            fade.debug_selector(move || selector)
        })
        .absolute()
        .top_0()
        .bottom_0()
        .w(FADE_WIDTH)
        .when(at_start, Styled::left_0)
        .when(!at_start, Styled::right_0)
        .bg(linear_gradient(
            90.,
            linear_color_stop(from, 0.),
            linear_color_stop(to, 1.),
        ))
}

/// The count in the overflow button. Big counts stay short so the button keeps its width.
fn count_text(count: usize) -> String {
    if count > 99 {
        "99+".to_owned()
    } else {
        count.to_string()
    }
}

fn overflow_trigger(
    id: ElementId,
    label: &SharedString,
    count: usize,
    open: bool,
    debug_selector: Option<String>,
    cx: &App,
) -> Stateful<Div> {
    let colors = cx.theme().colors();
    let (active, accent) = (colors.active, colors.accent);

    div()
        .id(id)
        .when_some(debug_selector, |trigger, selector| {
            trigger.debug_selector(move || selector)
        })
        .role(Role::Button)
        .aria_label(format!("{label}, {count}"))
        .aria_expanded(open)
        .focusable()
        .tab_stop(true)
        .cursor_pointer()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(2.))
        .w(OVERFLOW_WIDTH)
        .h(px(30.))
        .rounded(px(7.))
        .when(open, |trigger| trigger.bg(active))
        .hover(move |style| style.bg(active))
        .focus_visible(move |style| focus_ring(style, accent))
        .child(
            Label::new(count_text(count))
                .mono()
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .child(
            Icon::new(IconName::ChevronDown)
                .size(IconSize::Small)
                .color(Color::Faint),
        )
}

impl RenderOnce for TabBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let panel = cx.theme().colors().panel;

        let count = self.tabs.len();
        let selected = self.selected.min(count.saturating_sub(1));
        let ids: Vec<ElementId> = self.tabs.iter().map(|tab| tab.id().clone()).collect();
        let entries: Rc<[(SharedString, AgentStatus)]> = self
            .tabs
            .iter()
            .map(|tab| (tab.label().clone(), tab.status()))
            .collect();

        let state: Entity<BarState> =
            window.use_keyed_state((self.id.clone(), "tab-bar"), cx, |_, _| BarState::new());
        let (focus_handles, scroll) = state.update(cx, |state, cx| {
            (state.sync(&ids, selected, cx), state.scroll.clone())
        });
        let (overflowing, fade_start, fade_end) = {
            let state = state.read(cx);
            (state.overflowing, state.fade_start, state.fade_end)
        };

        let on_select = self.on_select;
        let tabs = self
            .tabs
            .into_iter()
            .zip(focus_handles)
            .enumerate()
            .map(|(index, (tab, handle))| {
                let tab = tab.toggle_state(index == selected).focus_handle(handle);

                match on_select.clone() {
                    Some(select) => tab.on_click(move |_, window, cx| select(index, window, cx)),
                    None => tab,
                }
            })
            .collect::<Vec<_>>();

        let prefix = self.debug_selector.clone();
        let selector = |suffix: &str| prefix.as_ref().map(|prefix| format!("{prefix}-{suffix}"));
        let row_selector = prefix.clone();
        let row = h_flex()
            .id((self.id.clone(), "tabs"))
            .when_some(row_selector, |row, selector| {
                row.debug_selector(move || selector)
            })
            .role(Role::TabList)
            .aria_label(self.label)
            .min_w_0()
            .gap(GAP)
            .overflow_x_scroll()
            .track_scroll(&scroll)
            .children(tabs);

        let strip = div()
            .relative()
            .flex()
            .min_w_0()
            .child(row)
            .when(fade_start, |strip| {
                strip.child(fade(true, panel, selector("fade-start")))
            })
            .when(fade_end, |strip| {
                strip.child(fade(false, panel, selector("fade-end")))
            });

        let overflow = overflowing.then(|| {
            let popover_id = ElementId::from((self.id.clone(), "overflow"));
            let open = popover_state(&popover_id, window, cx).read(cx).is_open();
            let trigger = overflow_trigger(
                (popover_id.clone(), "button").into(),
                &self.overflow_label,
                count,
                open,
                selector("overflow"),
                cx,
            );

            let label = self.overflow_label.clone();
            let menu_id = SharedString::from(format!("{}-overflow-menu", self.id));

            div()
                .flex_none()
                .child(PopoverMenu::new(popover_id, trigger, move |_, _| {
                    entries.iter().enumerate().fold(
                        Menu::new(ElementId::from(menu_id.clone()), label.clone())
                            .width(OVERFLOW_MENU_WIDTH)
                            .max_height(OVERFLOW_MENU_HEIGHT),
                        |menu, (index, (name, status))| {
                            let select = on_select.clone();

                            menu.item(
                                MenuItem::new(("tab", index), name.clone())
                                    .status(*status)
                                    .checked(index == selected)
                                    .on_select(move |window, cx| {
                                        if let Some(select) = &select {
                                            select(index, window, cx);
                                        }
                                    }),
                            )
                        },
                    )
                }))
        });

        h_flex()
            .on_children_prepainted({
                let state = state.clone();
                move |_, window, cx| {
                    state.update(cx, |state, cx| state.settle(window, cx));
                }
            })
            .id(self.id)
            .min_w_0()
            .gap(GAP)
            .child(strip)
            .children(overflow)
            .children(
                self.trailing
                    .into_iter()
                    .map(|element| div().flex_none().child(element)),
            )
    }
}

impl Component for TabBar {
    fn name() -> &'static str {
        "TabBar"
    }

    fn preview(window: &mut Window, cx: &mut App) -> AnyElement {
        let state = window.use_keyed_state("tab-bar-demo", cx, |_, _| 3_usize);
        let selected = *state.read(cx);
        let names = [
            "terminal-renderer",
            "menu-bar-usage",
            "worktree-scripts",
            "ui-tokens",
            "daemon-restart",
            "review-comments",
            "swarm-graph",
            "palette-search",
        ];
        let statuses = [
            AgentStatus::Working,
            AgentStatus::Blocked,
            AgentStatus::Idle,
            AgentStatus::Working,
            AgentStatus::Done,
            AgentStatus::Blocked,
            AgentStatus::Idle,
            AgentStatus::Working,
        ];

        let bar = |id: &'static str, count: usize, state: Entity<usize>| {
            TabBar::new(id, "Tasks")
                .overflow_label("All tasks")
                .tabs(names.into_iter().zip(statuses).take(count).enumerate().map(
                    |(index, (name, status))| {
                        Tab::new((id, index), name, status).on_close(|_, _, _| {})
                    },
                ))
                .selected(selected.min(count - 1))
                .trailing(
                    IconButton::new(("tab-bar-new", count), IconName::Plus, "New tab")
                        .size(IconButtonSize::Medium),
                )
                .on_select(move |index, _, cx| {
                    state.update(cx, |state, cx| {
                        *state = index;
                        cx.notify();
                    });
                })
        };

        v_flex()
            .gap_6()
            .child(example_group(
                "Few tabs keep their width",
                [example(
                    "Room for all",
                    div()
                        .w(px(760.))
                        .child(bar("tab-bar-few", 3, state.clone())),
                )],
            ))
            .child(example_group(
                "Many tabs",
                [example(
                    "Shrink, then scroll, with an overflow menu",
                    div().w(px(520.)).child(bar("tab-bar-many", 8, state)),
                )],
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui::accesskit::Node;
    use gpui::{Element as _, TestAppContext};

    use super::*;

    #[test]
    fn the_overflow_button_is_a_labelled_button_with_the_task_count() {
        let app = TestAppContext::single();
        app.update(crate::theme::init);

        let (role, label, expanded) = app.update(|cx| {
            let button =
                overflow_trigger("overflow".into(), &"All tasks".into(), 9, false, None, cx);
            let mut node = Node::new(button.a11y_role().unwrap_or(Role::Unknown));
            button.write_a11y_info(&mut node);

            (
                button.a11y_role(),
                node.label().map(str::to_owned),
                node.is_expanded(),
            )
        });

        assert_eq!(role, Some(Role::Button));
        assert_eq!(label.as_deref(), Some("All tasks, 9"));
        assert_eq!(expanded, Some(false));
    }

    #[test]
    fn big_counts_stay_short() {
        assert_eq!(count_text(9), "9");
        assert_eq!(count_text(99), "99");
        assert_eq!(count_text(100), "99+");
    }
}
