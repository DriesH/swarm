use std::rc::Rc;

use gpui::{
    AnyElement, App, BoxShadow, ElementId, Entity, FocusHandle, Hsla, Pixels, Role, ScrollHandle,
    SharedString, Toggled, Window, black, div, point, px,
};

use crate::component::{Component, example, example_group};
use crate::prelude::*;
use crate::{Confirm, Dismiss, MENU_CONTEXT, SelectNext, SelectPrevious};

type Handler = Rc<dyn Fn(&mut Window, &mut App) + 'static>;

const ITEM_HEIGHT: Pixels = px(34.);
const SEPARATOR_HEIGHT: Pixels = px(9.);
const MENU_PADDING: Pixels = px(5.);

/// One row of a [`Menu`].
#[must_use]
pub struct MenuItem {
    id: ElementId,
    label: SharedString,
    icon: Option<IconName>,
    status: Option<AgentStatus>,
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
            status: None,
            checked: None,
            disabled: false,
            on_select: None,
        }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Shows a status ring before the label, in place of an icon.
    pub fn status(mut self, status: AgentStatus) -> Self {
        self.status = Some(status);
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
    max_height: Option<Pixels>,
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
            max_height: None,
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

    /// Caps the height. A longer list scrolls, and the arrow keys keep the highlight in view.
    pub fn max_height(mut self, height: Pixels) -> Self {
        self.max_height = Some(height);
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
    scroll: &ScrollHandle,
    cx: &mut App,
) {
    highlight.update(cx, |current, cx| {
        *current = step(enabled, *current, forward).or(*current);

        if let Some(index) = *current {
            scroll.scroll_to_item(index);
        }

        cx.notify();
    });
}

/// How far a scrolling menu must scroll for the entry to show, worked out from the
/// fixed row heights because the menu has not been laid out yet.
fn reveal_offset(heights: &[Pixels], index: usize, max_height: Pixels) -> Pixels {
    let above: Pixels = heights.iter().take(index).copied().sum();
    let bottom = MENU_PADDING + above + heights.get(index).copied().unwrap_or_default();
    // The border takes 1px on each side of the viewport.
    let viewport = max_height - px(2.);

    (bottom + MENU_PADDING - viewport).max(Pixels::ZERO)
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
        .flex_none()
        .items_center()
        .gap(px(10.))
        .h(ITEM_HEIGHT)
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
        .when_some(item.status, |row, status| {
            row.child(
                div()
                    .flex()
                    .flex_none()
                    .justify_center()
                    .w(px(15.))
                    .child(StatusRing::new(status)),
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

/// The menu's scroll handle and highlight, in keyed window state. A new menu highlights its
/// checked item, or else its first, and a scrolling menu starts with that item in view.
fn menu_state(
    id: &ElementId,
    entries: &[MenuEntry],
    enabled: &[bool],
    max_height: Option<Pixels>,
    window: &mut Window,
    cx: &mut App,
) -> (ScrollHandle, Entity<Option<usize>>) {
    let checked = entries.iter().position(
        |entry| matches!(entry, MenuEntry::Item(item) if !item.disabled && item.checked == Some(true)),
    );
    let first = checked.or_else(|| step(enabled, None, true));

    let scroll = window
        .use_keyed_state((id.clone(), "scroll"), cx, |_, _| ScrollHandle::new())
        .read(cx)
        .clone();
    let heights: Vec<Pixels> = entries
        .iter()
        .map(|entry| match entry {
            MenuEntry::Item(_) => ITEM_HEIGHT,
            MenuEntry::Separator => SEPARATOR_HEIGHT,
        })
        .collect();

    let highlight = window.use_keyed_state((id.clone(), "highlight"), cx, {
        let scroll = scroll.clone();

        move |_, _| {
            if let (Some(max_height), Some(index)) = (max_height, first) {
                scroll.set_offset(point(px(0.), -reveal_offset(&heights, index, max_height)));
            }

            first
        }
    });

    (scroll, highlight)
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
        let (scroll, highlight) = menu_state(
            &self.id,
            &self.entries,
            &enabled,
            self.max_height,
            window,
            cx,
        );
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
                MenuEntry::Separator => div()
                    .h(px(1.))
                    .flex_none()
                    .my(px(4.))
                    .bg(line)
                    .into_any_element(),
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
            .p(MENU_PADDING)
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
            .when_some(self.max_height, |menu, max_height| {
                menu.max_h(max_height)
                    .overflow_y_scroll()
                    .track_scroll(&scroll)
            })
            .on_action({
                let (highlight, enabled) = (highlight.clone(), Rc::clone(&enabled));
                let scroll = scroll.clone();
                move |_: &SelectPrevious, _, cx| {
                    move_highlight(&highlight, &enabled, false, &scroll, cx);
                }
            })
            .on_action({
                let (highlight, enabled) = (highlight.clone(), Rc::clone(&enabled));
                move |_: &SelectNext, _, cx| {
                    move_highlight(&highlight, &enabled, true, &scroll, cx);
                }
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
            .child(example_group(
                "Status rings, scrolling",
                [example("Taller than 260 px", task_menu("menu-demo-tasks"))],
            ))
            .into_any_element()
    }
}

/// A long menu of tasks with status rings, for the `Menu` preview.
fn task_menu(id: &'static str) -> Menu {
    let tasks = [
        ("terminal-renderer", AgentStatus::Working),
        ("menu-bar-usage", AgentStatus::Blocked),
        ("worktree-scripts", AgentStatus::Idle),
        ("ui-tokens", AgentStatus::Working),
        ("daemon-restart", AgentStatus::Done),
        ("review-comments", AgentStatus::Blocked),
        ("swarm-graph", AgentStatus::Idle),
        ("palette-search", AgentStatus::Working),
        ("settings-pages", AgentStatus::Error),
        ("diff-split-view", AgentStatus::Idle),
    ];

    tasks.into_iter().enumerate().fold(
        Menu::new(id, "All tasks")
            .width(px(280.))
            .max_height(px(260.)),
        |menu, (index, (name, status))| {
            menu.item(
                MenuItem::new(("task", index), name)
                    .status(status)
                    .checked(index == 3),
            )
        },
    )
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
    use gpui::{Pixels, px};

    use super::{ITEM_HEIGHT, reveal_offset, step};

    fn rows(count: usize) -> Vec<Pixels> {
        vec![ITEM_HEIGHT; count]
    }

    #[test]
    fn reveal_offset_keeps_the_top_rows_in_place() {
        assert_eq!(reveal_offset(&rows(20), 0, px(360.)), px(0.));
        assert_eq!(reveal_offset(&rows(20), 5, px(360.)), px(0.));
    }

    #[test]
    fn reveal_offset_scrolls_a_row_below_the_view_into_it() {
        // Row 10 ends at 5 + 11 * 34 = 379; the view shows 358 px; 5 px of padding stay below.
        assert_eq!(reveal_offset(&rows(20), 10, px(360.)), px(26.));
        assert_eq!(reveal_offset(&rows(20), 19, px(360.)), px(332.));
    }

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
