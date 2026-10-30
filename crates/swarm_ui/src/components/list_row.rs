use gpui::{
    AnyElement, App, ClickEvent, ElementId, FocusHandle, FontWeight, Role, SharedString, Window,
    div, px,
};

use crate::component::{Component, example, example_group};
use crate::prelude::*;
use crate::styles::focus_ring;
use crate::traits::ClickHandler;

/// A sidebar row: leading icon, label, trailing slot.
#[derive(IntoElement)]
#[must_use]
pub struct ListRow {
    id: ElementId,
    label: SharedString,
    accessible_label: Option<SharedString>,
    start: Option<AnyElement>,
    end: Option<AnyElement>,
    selected: bool,
    compact: bool,
    indent: usize,
    focus_handle: Option<FocusHandle>,
    debug_selector: Option<String>,
    on_click: Option<ClickHandler>,
}

impl ListRow {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            accessible_label: None,
            start: None,
            end: None,
            selected: false,
            compact: false,
            indent: 0,
            focus_handle: None,
            debug_selector: None,
            on_click: None,
        }
    }

    /// Name for `VisualTestContext::debug_bounds`. Only recorded in test builds.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }

    pub fn start_slot(mut self, element: impl IntoElement) -> Self {
        self.start = Some(element.into_any_element());
        self
    }

    pub fn end_slot(mut self, element: impl IntoElement) -> Self {
        self.end = Some(element.into_any_element());
        self
    }

    /// What VoiceOver reads when the visible label is not enough, for example
    /// "pty-io, waiting". Defaults to the label.
    pub fn accessible_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessible_label = Some(label.into());
        self
    }

    /// 28px instead of 30px, for agent rows.
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    /// Nesting level; each level indents 14px.
    pub fn indent(mut self, level: usize) -> Self {
        self.indent = level;
        self
    }

    /// Use a focus handle the parent owns, so it can move focus with arrow keys.
    pub fn track_focus(mut self, handle: &FocusHandle) -> Self {
        self.focus_handle = Some(handle.clone());
        self
    }
}

impl Toggleable for ListRow {
    fn toggle_state(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl Clickable for ListRow {
    fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for ListRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (active, raised, accent) = (colors.active, colors.raised, colors.accent);
        #[expect(
            clippy::cast_precision_loss,
            reason = "nesting depth is a handful of levels"
        )]
        let indent = px(14. * self.indent as f32);

        div()
            .id(self.id)
            .when_some(self.debug_selector, |row, selector| {
                row.debug_selector(move || selector)
            })
            .role(Role::ListItem)
            .aria_label(self.accessible_label.unwrap_or_else(|| self.label.clone()))
            .aria_selected(self.selected)
            .map(|row| match &self.focus_handle {
                Some(handle) => row.track_focus(handle),
                None => row.focusable().tab_stop(true),
            })
            .cursor_pointer()
            .flex()
            .items_center()
            .gap(px(10.))
            .h(if self.compact { px(28.) } else { px(30.) })
            .px(px(8.))
            .ml(indent)
            .rounded(px(7.))
            .when(self.selected, |row| row.bg(active))
            .when(!self.selected, |row| row.hover(|style| style.bg(raised)))
            .focus_visible(|style| focus_ring(style, accent))
            .when_some(self.start, ParentElement::child)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .child(Label::new(self.label).weight(FontWeight::MEDIUM).truncate()),
            )
            .when_some(self.end, ParentElement::child)
            .when_some(self.on_click, |row, handler| row.on_click(handler))
    }
}

impl Component for ListRow {
    fn name() -> &'static str {
        "ListRow"
    }

    fn preview(window: &mut Window, cx: &mut App) -> AnyElement {
        let state = window.use_keyed_state("list-row-demo", cx, |_, _| 0_usize);
        let selected = *state.read(cx);
        let projects = [("swarm", "3"), ("website", "1"), ("billing-api", "2")];
        let agents = [
            (
                "orchestrator",
                IconName::ClaudeCode,
                AgentStatus::Working,
                0,
            ),
            ("term-render", IconName::ClaudeCode, AgentStatus::Working, 1),
            ("pty-io", IconName::Codex, AgentStatus::Waiting, 1),
            ("ui-tokens", IconName::ClaudeCode, AgentStatus::Done, 1),
            ("bench", IconName::Codex, AgentStatus::Idle, 1),
            ("cargo test", IconName::Terminal, AgentStatus::Working, 2),
        ];

        v_flex()
            .gap_6()
            .child(example_group(
                "Projects",
                [example(
                    "Selectable",
                    div()
                        .id("projects")
                        .role(Role::List)
                        .aria_label("Projects")
                        .w(px(232.))
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .children(
                            projects
                                .into_iter()
                                .enumerate()
                                .map(|(index, (name, tabs))| {
                                    let state = state.clone();
                                    ListRow::new(("project", index), name)
                                        .toggle_state(index == selected)
                                        .start_slot(Icon::new(IconName::Bee).size(IconSize::Large))
                                        .end_slot(
                                            Label::new(tabs)
                                                .mono()
                                                .size(LabelSize::XSmall)
                                                .color(Color::Faint),
                                        )
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
                "Agents",
                [example(
                    "Compact, indented",
                    div()
                        .id("agents")
                        .role(Role::List)
                        .aria_label("Agents")
                        .w(px(232.))
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .children(agents.into_iter().enumerate().map(
                            |(index, (name, icon, status, level))| {
                                let color = if icon == IconName::Terminal {
                                    Color::Muted
                                } else {
                                    Color::Default
                                };
                                ListRow::new(("agent", index), name)
                                    .accessible_label(format!("{name}, {}", status.label()))
                                    .compact(true)
                                    .indent(level)
                                    .start_slot(Icon::new(icon).size(IconSize::Large).color(color))
                                    .end_slot(StatusDot::new(status))
                            },
                        )),
                )],
            ))
            .into_any_element()
    }
}
