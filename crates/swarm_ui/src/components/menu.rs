use std::rc::Rc;

use gpui::{
    AnyElement, App, BoxShadow, ElementId, Entity, FocusHandle, Hsla, Pixels, Role, SharedString,
    Toggled, Window, black, div, point, px,
};

use crate::component::{Component, example, example_group};
use crate::prelude::*;
use crate::{Confirm, Dismiss, MENU_CONTEXT, SelectNext, SelectPrevious};

type Handler = Rc<dyn Fn(&mut Window, &mut App) + 'static>;

/// One row of a [`Menu`].
#[must_use]
pub struct MenuItem {
    id: ElementId,
    label: SharedString,
    icon: Option<IconName>,
    checked: Option<bool>,
    disabled: bool,
    on_select: Option<Handler>,
}

impl MenuItem {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            checked: None,
            disabled: false,
            on_select: None,
        }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Makes the item a checkable one. A check mark shows at the right when `true`.
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Called when the user picks the item, before the menu is dismissed.
    pub fn on_select(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

enum MenuEntry {
    Item(MenuItem),
    Separator,
}

/// A list of actions. Arrow keys move the highlight, Enter picks, Escape dismisses.
/// The menu does not close itself: `on_dismiss` tells its owner to remove it.
#[derive(IntoElement)]
#[must_use]
pub struct Menu {
    id: ElementId,
    label: SharedString,
    entries: Vec<MenuEntry>,
    width: Pixels,
    on_dismiss: Option<Handler>,
    focus: Option<FocusHandle>,
}

impl Menu {
    /// `label` names the menu for VoiceOver, for example "New tab".
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            entries: Vec::new(),
            width: px(236.),
            on_dismiss: None,
            focus: None,
        }
    }

    pub fn item(mut self, item: MenuItem) -> Self {
        self.entries.push(MenuEntry::Item(item));
        self
    }

    pub fn separator(mut self) -> Self {
        self.entries.push(MenuEntry::Separator);
        self
    }

    pub fn width(mut self, width: Pixels) -> Self {
        self.width = width;
        self
    }

    /// Called on Escape and after an item was picked.
    pub fn on_dismiss(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }

    /// Lets the owner focus the menu, so its keys work.
    pub fn track_focus(mut self, focus: &FocusHandle) -> Self {
        self.focus = Some(focus.clone());
        self
    }
}

/// The next enabled entry after `from`, wrapping at the ends. With no
/// highlight yet, forward gives the first and backward the last.
fn step(enabled: &[bool], from: Option<usize>, forward: bool) -> Option<usize> {
    let count = enabled.len();

    (1..=count)
        .map(|offset| match (from, forward) {
            (Some(from), true) => (from + offset) % count,
            (Some(from), false) => (from + count - offset) % count,
            (None, true) => offset - 1,
            (None, false) => count - offset,
        })
        .find(|&index| enabled[index])
}

/// Runs the item's handler, then dismisses the menu.
fn choose(select: Option<&Handler>, dismiss: Option<&Handler>, window: &mut Window, cx: &mut App) {
    if let Some(select) = select {
        select(window, cx);
    }

    if let Some(dismiss) = dismiss {
        dismiss(window, cx);
    }
}

fn move_highlight(
    highlight: &Entity<Option<usize>>,
    enabled: &[bool],
    forward: bool,
    cx: &mut App,
) {
    highlight.update(cx, |current, cx| {
        *current = step(enabled, *current, forward).or(*current);
        cx.notify();
    });
}

fn render_item(
    item: MenuItem,
    index: usize,
    is_highlighted: bool,
    highlight: &Entity<Option<usize>>,
    on_dismiss: Option<&Handler>,
    active: Hsla,
) -> AnyElement {
    let hover_state = highlight.clone();
    let select = item.on_select.clone();
    let dismiss = on_dismiss.cloned();

    let role = if item.checked.is_some() {
        Role::MenuItemCheckBox
    } else {
        Role::MenuItem
    };

    div()
        .id(item.id)
        .role(role)
        .aria_label(item.label.clone())
        .when_some(item.checked, |row, checked| {
            row.aria_toggled(if checked {
                Toggled::True
            } else {
                Toggled::False
            })
        })
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.))
        .h(px(34.))
        .px(px(10.))
        .rounded(px(7.))
        .whitespace_nowrap()
        .when(is_highlighted, |row| {
            row.bg(active).aria_active_descendant()
        })
        .map(|row| {
            if item.disabled {
                row.opacity(0.5)
            } else {
                row.cursor_pointer()
                    .on_hover(move |hovered, _, cx| {
                        if *hovered {
                            hover_state.update(cx, |current, cx| {
                                if *current != Some(index) {
                                    *current = Some(index);
                                    cx.notify();
                                }
                            });
                        }
                    })
                    .on_click(move |_, window, cx| {
                        choose(select.as_ref(), dismiss.as_ref(), window, cx);
                    })
            }
        })
        .when_some(item.icon, |row, icon| {
            row.child(
                Icon::new(icon)
                    .size(IconSize::Custom(px(15.)))
                    .color(Color::Muted),
            )
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(Label::new(item.label).truncate()),
        )
        .when(item.checked == Some(true), |row| {
            row.child(Icon::new(IconName::Check).size(IconSize::Small))
        })
        .into_any_element()
}

impl RenderOnce for Menu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (line, panel, active) = (colors.line, colors.panel, colors.active);

        let enabled: Rc<[bool]> = self
            .entries
            .iter()
            .map(|entry| matches!(entry, MenuEntry::Item(item) if !item.disabled))
            .collect();
        let first = step(&enabled, None, true);
        let highlight =
            window.use_keyed_state((self.id.clone(), "highlight"), cx, move |_, _| first);
        let highlighted = *highlight.read(cx);

        let selects: Rc<[Option<Handler>]> = self
            .entries
            .iter()
            .map(|entry| match entry {
                MenuEntry::Item(item) => item.on_select.clone(),
                MenuEntry::Separator => None,
            })
            .collect();
        let on_dismiss = self.on_dismiss;

        let rows: Vec<AnyElement> = self
            .entries
            .into_iter()
            .enumerate()
            .map(|(index, entry)| match entry {
                MenuEntry::Separator => div().h(px(1.)).my(px(4.)).bg(line).into_any_element(),
                MenuEntry::Item(item) => {
                    let is_highlighted = highlighted == Some(index) && !item.disabled;

                    render_item(
                        item,
                        index,
                        is_highlighted,
                        &highlight,
                        on_dismiss.as_ref(),
                        active,
                    )
                }
            })
            .collect();

        let selector = self.id.to_string();

        v_flex()
            .id(self.id)
            .role(Role::Menu)
            .aria_label(self.label)
            .debug_selector(move || selector)
            .key_context(MENU_CONTEXT)
            .when_some(self.focus.as_ref(), InteractiveElement::track_focus)
            .occlude()
            .w(self.width)
            .p(px(5.))
            .rounded(px(10.))
            .border_1()
            .border_color(line)
            .bg(panel)
            .shadow(vec![BoxShadow {
                color: black().opacity(0.28),
                offset: point(px(0.), px(12.)),
                blur_radius: px(32.),
                spread_radius: px(0.),
                inset: false,
            }])
            .on_action({
                let dismiss = on_dismiss.clone();
                move |_: &Dismiss, window, cx| {
                    if let Some(dismiss) = &dismiss {
                        dismiss(window, cx);
                    }
                }
            })
            .on_action({
                let (highlight, enabled) = (highlight.clone(), Rc::clone(&enabled));
                move |_: &SelectPrevious, _, cx| move_highlight(&highlight, &enabled, false, cx)
            })
            .on_action({
                let (highlight, enabled) = (highlight.clone(), Rc::clone(&enabled));
                move |_: &SelectNext, _, cx| move_highlight(&highlight, &enabled, true, cx)
            })
            .on_action(move |_: &Confirm, window, cx| {
                let Some(index) = *highlight.read(cx) else {
                    return;
                };
                let select = selects.get(index).and_then(Option::as_ref);

                if enabled.get(index).copied().unwrap_or(false) {
                    choose(select, on_dismiss.as_ref(), window, cx);
                }
            })
            .children(rows)
    }
}

impl Component for Menu {
    fn name() -> &'static str {
        "Menu"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        v_flex()
            .gap_6()
            .child(example_group(
                "Default",
                [example("Default", demo_menu("menu-demo"))],
            ))
            .into_any_element()
    }
}

/// The menu shown by the `Menu` and `PopoverMenu` previews.
pub(crate) fn demo_menu(id: &'static str) -> Menu {
    Menu::new(id, "New tab")
        .item(MenuItem::new("new-task", "New task").icon(IconName::Plus))
        .item(MenuItem::new("plan", "Plan with orchestrator").icon(IconName::Bee))
        .separator()
        .item(MenuItem::new("create-pr", "Create pull request").disabled(true))
}

#[cfg(test)]
mod tests {
    use super::step;

    #[test]
    fn step_forward_skips_disabled_entries() {
        assert_eq!(step(&[true, false, true], Some(0), true), Some(2));
    }

    #[test]
    fn step_forward_wraps_to_the_first_enabled_entry() {
        assert_eq!(step(&[false, true, true], Some(2), true), Some(1));
    }

    #[test]
    fn step_backward_skips_disabled_entries() {
        assert_eq!(step(&[true, false, true], Some(2), false), Some(0));
    }

    #[test]
    fn step_backward_wraps_to_the_last_enabled_entry() {
        assert_eq!(step(&[true, true, false], Some(0), false), Some(1));
    }

    #[test]
    fn step_without_highlight_starts_at_the_first_enabled_entry() {
        assert_eq!(step(&[false, true], None, true), Some(1));
    }

    #[test]
    fn step_returns_none_when_nothing_is_enabled() {
        assert_eq!(step(&[false, false], Some(0), true), None);
    }

    #[test]
    fn step_returns_none_for_an_empty_menu() {
        assert_eq!(step(&[], None, true), None);
    }
}
