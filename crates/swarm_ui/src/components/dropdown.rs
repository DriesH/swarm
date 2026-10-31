use std::rc::Rc;

use gpui::{AnyElement, App, Div, ElementId, Role, SharedString, Stateful, Window, div, px};

use crate::component::{Component, example, example_group};
use crate::components::menu::{Menu, MenuItem};
use crate::components::popover_menu::popover_state;
use crate::prelude::*;
use crate::styles::focus_ring;

/// A longer list of options scrolls.
const MENU_HEIGHT: gpui::Pixels = px(272.);

type ChangeHandler = Rc<dyn Fn(usize, &mut Window, &mut App) + 'static>;

/// A select: a trigger showing the chosen option, and a menu of all options.
#[derive(IntoElement)]
#[must_use]
pub struct Dropdown {
    id: ElementId,
    label: SharedString,
    options: Vec<SharedString>,
    selected: usize,
    disabled: bool,
    full_width: bool,
    on_change: Option<ChangeHandler>,
}

impl Dropdown {
    /// `label` names the select for VoiceOver, for example "Branch from".
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        options: Vec<SharedString>,
        selected: usize,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            options,
            selected,
            disabled: false,
            full_width: false,
            on_change: None,
        }
    }

    /// Makes the trigger fill its container, with the label left and the chevron right.
    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    /// Called with the index of the option the user picked.
    pub fn on_change(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl Disableable for Dropdown {
    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

/// The always-visible part: role, label, current value and look.
fn trigger(
    id: &ElementId,
    label: &SharedString,
    options: &[SharedString],
    selected: usize,
    expanded: bool,
    full_width: bool,
    cx: &App,
) -> Stateful<Div> {
    let colors = cx.theme().colors();
    let (line, raised) = (colors.line, colors.raised);
    let current = options.get(selected).cloned().unwrap_or_default();

    div()
        .id((id.clone(), "trigger"))
        .role(Role::ComboBox)
        .aria_label(label.clone())
        .aria_value(current.clone())
        .aria_expanded(expanded)
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .h(px(28.))
        .when(full_width, Styled::w_full)
        .min_w(px(110.))
        .pl(px(8.))
        .pr(px(30.))
        .rounded(px(7.))
        .border_1()
        .border_color(line)
        .bg(raised)
        .child(Label::new(current).size(LabelSize::Small))
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right(px(9.))
                .flex()
                .items_center()
                .child(
                    Icon::new(IconName::ChevronDown)
                        .size(IconSize::Small)
                        .color(Color::Faint),
                ),
        )
}

impl RenderOnce for Dropdown {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let expanded = popover_state(&self.id, window, cx).read(cx).is_open();
        let trigger = trigger(
            &self.id,
            &self.label,
            &self.options,
            self.selected,
            expanded,
            self.full_width,
            cx,
        );

        if self.disabled {
            return trigger.opacity(0.5).into_any_element();
        }

        let accent = cx.theme().colors().accent;
        let trigger = trigger
            .focusable()
            .tab_stop(true)
            .cursor_pointer()
            .focus_visible(move |style| focus_ring(style, accent));

        let (menu_id, label) = (self.id.clone(), self.label);
        let (options, selected, on_change) = (self.options, self.selected, self.on_change);

        PopoverMenu::new(self.id, trigger, move |_, _| {
            options.iter().enumerate().fold(
                Menu::new((menu_id.clone(), "menu"), label.clone()).max_height(MENU_HEIGHT),
                |menu, (index, option)| {
                    let on_change = on_change.clone();

                    menu.item(
                        MenuItem::new(("option", index), option.clone())
                            .checked(index == selected)
                            .on_select(move |window, cx| {
                                if let Some(on_change) = &on_change {
                                    on_change(index, window, cx);
                                }
                            }),
                    )
                },
            )
        })
        .into_any_element()
    }
}

impl Component for Dropdown {
    fn name() -> &'static str {
        "Dropdown"
    }

    fn preview(window: &mut Window, cx: &mut App) -> AnyElement {
        let branch = window.use_keyed_state("dropdown-branch", cx, |_, _| 0_usize);
        let idle = window.use_keyed_state("dropdown-idle", cx, |_, _| 1_usize);
        let (branch_selected, idle_selected) = (*branch.read(cx), *idle.read(cx));

        v_flex()
            .gap_6()
            .child(example_group(
                "Default",
                [
                    example(
                        "Branch from",
                        Dropdown::new(
                            "branch",
                            "Branch from",
                            vec!["main".into(), "terminal-renderer".into()],
                            branch_selected,
                        )
                        .on_change(move |index, _, cx| {
                            branch.update(cx, |selected, cx| {
                                *selected = index;
                                cx.notify();
                            });
                        }),
                    ),
                    example(
                        "Stop idle agents after",
                        Dropdown::new(
                            "idle",
                            "Stop idle agents after",
                            vec![
                                "10 minutes".into(),
                                "30 minutes".into(),
                                "1 hour".into(),
                                "Never".into(),
                            ],
                            idle_selected,
                        )
                        .on_change(move |index, _, cx| {
                            idle.update(cx, |selected, cx| {
                                *selected = index;
                                cx.notify();
                            });
                        }),
                    ),
                ],
            ))
            .child(example_group(
                "Disabled",
                [example(
                    "Disabled",
                    Dropdown::new("disabled", "Branch from", vec!["main".into()], 0).disabled(true),
                )],
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui::Element as _;
    use gpui::accesskit::{Node, Role};

    use super::*;

    #[gpui::test]
    fn trigger_is_a_combo_box_with_label_and_value(cx: &mut gpui::TestAppContext) {
        cx.update(crate::theme::init);

        let options: Vec<SharedString> = vec!["main".into(), "terminal-renderer".into()];
        let element = cx.update(|cx| {
            trigger(
                &"d".into(),
                &"Branch from".into(),
                &options,
                1,
                false,
                false,
                cx,
            )
            .into_element()
        });

        let mut node = Node::new(Role::Unknown);
        element.write_a11y_info(&mut node);

        assert_eq!(element.a11y_role(), Some(Role::ComboBox));
        assert_eq!(node.label(), Some("Branch from"));
        assert_eq!(node.value(), Some("terminal-renderer"));
    }
}
