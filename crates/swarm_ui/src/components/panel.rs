use gpui::{AnyElement, App, Div, ElementId, Role, SharedString, Stateful, Window, div, px};

use crate::component::{Component, example, example_group};
use crate::prelude::*;

/// A bordered surface with a 10px radius. Rows added with `row` get a divider
/// between them, like the settings and system check cards.
#[derive(IntoElement)]
#[must_use]
pub struct Panel {
    accessible: Option<(ElementId, SharedString)>,
    role: Role,
    rows: Vec<AnyElement>,
    raised: bool,
}

impl Panel {
    pub fn new() -> Self {
        Self {
            accessible: None,
            role: Role::Group,
            rows: Vec::new(),
            raised: false,
        }
    }

    /// Exposes the panel to VoiceOver as a named group.
    pub fn accessible(mut self, id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        self.accessible = Some((id.into(), label.into()));
        self
    }

    /// Exposes the named panel as a list, for rows that are list items. Use it after
    /// `accessible`.
    pub fn list(mut self) -> Self {
        self.role = Role::List;
        self
    }

    /// The role the named panel has in the accessibility tree.
    pub fn role(&self) -> Role {
        self.role
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

/// What a panel draws: the card, inside a named wrapper when the panel is accessible.
enum Built {
    Card(Div),
    Named(Stateful<Div>),
}

impl Panel {
    fn build(self, cx: &App) -> Built {
        let colors = cx.theme().colors();
        let line = colors.line;

        let background = if self.raised {
            colors.raised
        } else {
            colors.panel
        };

        let card = div()
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
            Some((id, label)) => {
                Built::Named(div().id(id).role(self.role).aria_label(label).child(card))
            }
            None => Built::Card(card),
        }
    }
}

impl RenderOnce for Panel {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        match self.build(cx) {
            Built::Card(card) => card.into_any_element(),
            Built::Named(named) => named.into_any_element(),
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

#[cfg(test)]
mod tests {
    use gpui::accesskit::Node;
    use gpui::{Element as _, TestAppContext};

    use super::*;

    fn named(panel: Panel, cx: &mut TestAppContext) -> (Option<Role>, Option<String>) {
        cx.update(|cx| {
            let Built::Named(element) = panel.build(cx) else {
                return (None, None);
            };

            let element = element.into_element();
            let role = element.a11y_role();
            let mut node = Node::new(role.unwrap_or(Role::Unknown));
            element.write_a11y_info(&mut node);

            (role, node.label().map(str::to_owned))
        })
    }

    #[gpui::test]
    fn a_named_panel_is_a_group_unless_it_is_made_a_list(cx: &mut TestAppContext) {
        cx.update(crate::theme::init);

        let group = named(Panel::new().accessible("rows", "Checks"), cx);
        let list = named(Panel::new().accessible("rows", "Checks").list(), cx);

        assert_eq!(group, (Some(Role::Group), Some("Checks".to_owned())));
        assert_eq!(list, (Some(Role::List), Some("Checks".to_owned())));
    }

    #[gpui::test]
    fn a_panel_without_a_name_is_not_in_the_accessibility_tree(cx: &mut TestAppContext) {
        cx.update(crate::theme::init);

        let in_tree = cx.update(|cx| matches!(Panel::new().list().build(cx), Built::Named(_)));

        assert!(!in_tree);
    }
}
