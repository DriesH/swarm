use gpui::{
    AnyElement, App, ClickEvent, ElementId, FontWeight, Role, SharedString, Window, div, px,
};

use crate::component::{Component, example, example_group};
use crate::prelude::*;
use crate::styles::focus_ring;
use crate::traits::ClickHandler;

/// A task tab with a status ring.
#[derive(IntoElement)]
#[must_use]
pub struct Tab {
    id: ElementId,
    label: SharedString,
    status: AgentStatus,
    selected: bool,
    debug_selector: Option<String>,
    on_click: Option<ClickHandler>,
}

impl Tab {
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        status: AgentStatus,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            status,
            selected: false,
            debug_selector: None,
            on_click: None,
        }
    }

    /// Name for `VisualTestContext::debug_bounds`. Only recorded in test builds.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }
}

impl Toggleable for Tab {
    fn toggle_state(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl Clickable for Tab {
    fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Tab {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (raised, line, active, accent) =
            (colors.raised, colors.line, colors.active, colors.accent);
        let accessible_label =
            SharedString::from(format!("{}, {}", self.label, self.status.label()));

        div()
            .id(self.id)
            .when_some(self.debug_selector, |element, selector| {
                element.debug_selector(move || selector)
            })
            .role(Role::Tab)
            .aria_label(accessible_label)
            .aria_selected(self.selected)
            .focusable()
            .tab_stop(true)
            .cursor_pointer()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(32.))
            .px(px(12.))
            .rounded(px(8.))
            .border_1()
            .map(|tab| {
                if self.selected {
                    tab.bg(raised).border_color(line)
                } else {
                    tab.border_color(gpui::transparent_black())
                        .hover(|style| style.bg(active))
                }
            })
            .focus_visible(|style| focus_ring(style, accent))
            .child(StatusRing::new(self.status))
            .child(
                Label::new(self.label)
                    .weight(FontWeight::MEDIUM)
                    .color(if self.selected {
                        Color::Default
                    } else {
                        Color::Muted
                    }),
            )
            .when_some(self.on_click, |tab, handler| tab.on_click(handler))
    }
}

impl Component for Tab {
    fn name() -> &'static str {
        "Tab"
    }

    fn preview(window: &mut Window, cx: &mut App) -> AnyElement {
        let state = window.use_keyed_state("tab-demo", cx, |_, _| 0_usize);
        let selected = *state.read(cx);
        let tabs = [
            ("terminal-renderer", AgentStatus::Working),
            ("menu-bar-usage", AgentStatus::Blocked),
            ("worktree-scripts", AgentStatus::Idle),
            ("ui-tokens", AgentStatus::Done),
        ];

        v_flex()
            .gap_6()
            .child(example_group(
                "Tab bar",
                [example(
                    "Interactive",
                    div()
                        .id("tab-list")
                        .role(Role::TabList)
                        .aria_label("Tasks")
                        .flex()
                        .gap_1()
                        .children(
                            tabs.into_iter()
                                .enumerate()
                                .map(|(index, (label, status))| {
                                    let state = state.clone();
                                    Tab::new(("tab", index), label, status)
                                        .toggle_state(index == selected)
                                        .on_click(move |_, _, cx| {
                                            state.update(cx, |state, cx| {
                                                *state = index;
                                                cx.notify();
                                            });
                                        })
                                }),
                        ),
                )],
            ))
            .child(example_group(
                "States",
                tabs.into_iter().enumerate().map(|(index, (_, status))| {
                    example(
                        status.label(),
                        Tab::new(("state", index), status.label(), status).toggle_state(index == 0),
                    )
                }),
            ))
            .into_any_element()
    }
}
