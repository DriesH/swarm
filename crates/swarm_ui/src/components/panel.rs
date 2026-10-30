use gpui::{AnyElement, App, ElementId, Role, SharedString, Window, div, px};

use crate::component::{Component, example, example_group};
use crate::prelude::*;

/// A bordered surface with a 10px radius. Rows added with `row` get a divider
/// between them, like the settings and system check cards.
#[derive(IntoElement)]
#[must_use]
pub struct Panel {
    accessible: Option<(ElementId, SharedString)>,
    rows: Vec<AnyElement>,
    raised: bool,
}

impl Panel {
    pub fn new() -> Self {
        Self {
            accessible: None,
            rows: Vec::new(),
            raised: false,
        }
    }

    /// Exposes the panel to VoiceOver as a named group.
    pub fn accessible(mut self, id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        self.accessible = Some((id.into(), label.into()));
        self
    }

    /// Uses the `raised` surface instead of `panel`.
    pub fn raised(mut self) -> Self {
        self.raised = true;
        self
    }

    pub fn row(mut self, row: impl IntoElement) -> Self {
        self.rows.push(row.into_any_element());
        self
    }
}

impl Default for Panel {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for Panel {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let line = colors.line;

        let background = if self.raised {
            colors.raised
        } else {
            colors.panel
        };

        let body = div()
            .flex()
            .flex_col()
            .rounded(px(10.))
            .border_1()
            .border_color(line)
            .bg(background)
            .overflow_hidden()
            .children(self.rows.into_iter().enumerate().map(|(index, row)| {
                div()
                    .when(index > 0, |row| row.border_t_1().border_color(line))
                    .child(row)
            }));

        match self.accessible {
            Some((id, label)) => div()
                .id(id)
                .role(Role::Group)
                .aria_label(label)
                .child(body)
                .into_any_element(),
            None => body.into_any_element(),
        }
    }
}

/// A settings-style row: label on the left, control on the right, 46px tall.
#[derive(IntoElement)]
#[must_use]
pub struct PanelRow {
    label: SharedString,
    end: Option<AnyElement>,
}

impl PanelRow {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            end: None,
        }
    }

    pub fn end_slot(mut self, element: impl IntoElement) -> Self {
        self.end = Some(element.into_any_element());
        self
    }
}

impl RenderOnce for PanelRow {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        h_flex()
            .gap_3()
            .min_h(px(46.))
            .px(px(14.))
            .child(div().flex_1().child(Label::new(self.label)))
            .when_some(self.end, ParentElement::child)
    }
}

impl Component for Panel {
    fn name() -> &'static str {
        "Panel"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        v_flex()
            .gap_6()
            .child(example_group(
                "Settings card",
                [example(
                    "Rows",
                    div().w(px(420.)).child(
                        Panel::new()
                            .accessible("checks", "Checks before PR")
                            .row(
                                PanelRow::new("Required to pass").end_slot(
                                    Switch::new("required", "Checks required to pass")
                                        .toggle_state(true),
                                ),
                            )
                            .row(
                                PanelRow::new("Git pre-push hook").end_slot(
                                    h_flex()
                                        .gap_2()
                                        .child(
                                            Label::new("lefthook")
                                                .mono()
                                                .size(LabelSize::XSmall)
                                                .color(Color::Muted),
                                        )
                                        .child(
                                            StatusPill::new(AgentStatus::Done).label("Detected"),
                                        ),
                                ),
                            )
                            .row(
                                PanelRow::new("Base branch")
                                    .end_slot(Label::new("main").mono().size(LabelSize::Small)),
                            ),
                    ),
                )],
            ))
            .child(example_group(
                "Raised",
                [example(
                    "Usage",
                    div().w(px(232.)).child(
                        Panel::new().raised().row(
                            h_flex()
                                .px(px(12.))
                                .py(px(10.))
                                .child(
                                    div().flex_1().child(
                                        Label::new("Claude · 5h")
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    ),
                                )
                                .child(Label::new("42%").mono().size(LabelSize::XSmall)),
                        ),
                    ),
                )],
            ))
            .into_any_element()
    }
}
