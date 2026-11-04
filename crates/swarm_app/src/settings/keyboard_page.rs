//! Settings › Keyboard: the shortcuts of the app, read-only. The rows come from the same
//! bindings the app registers, so the page cannot go stale.

use gpui::{KeybindingKeystroke, Role, SharedString, div, px};
use swarm_ui::prelude::*;

use super::SettingsWindow;
use super::pages::{PageBlocks, page_title, section};
use crate::actions::{self, ShortcutGroup};

const KEY_CONTROL: &str = "\u{2303}";
const KEY_COMMAND: &str = "\u{2318}";
const KEY_SHIFT: &str = "\u{21e7}";
const KEY_OPTION: &str = "\u{2325}";

/// One action with the keys that run it. Another key for the same action is another
/// alternative, and the keys of one alternative are shown together.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Row {
    pub group: ShortcutGroup,
    pub name: &'static str,
    pub alternatives: Vec<Vec<SharedString>>,
}

/// The key caps of one keystroke in the order macOS writes them (control, option, shift,
/// command), for example `["⇧", "⌘", "R"]`.
fn caps(keystroke: &KeybindingKeystroke) -> Vec<SharedString> {
    let modifiers = keystroke.modifiers();
    let mut caps: Vec<SharedString> = Vec::new();

    for (pressed, cap) in [
        (modifiers.control, KEY_CONTROL),
        (modifiers.alt, KEY_OPTION),
        (modifiers.shift, KEY_SHIFT),
        (modifiers.platform, KEY_COMMAND),
    ] {
        if pressed {
            caps.push(cap.into());
        }
    }

    caps.push(key_cap(keystroke.key()));

    caps
}

fn key_cap(key: &str) -> SharedString {
    match key {
        "enter" => "\u{21b5}".into(),
        "escape" => "esc".into(),
        "space" => "Space".into(),
        "tab" => "\u{21e5}".into(),
        "backspace" => "\u{232b}".into(),
        "up" => "\u{2191}".into(),
        "down" => "\u{2193}".into(),
        "left" => "\u{2190}".into(),
        "right" => "\u{2192}".into(),
        other => other.to_uppercase().into(),
    }
}

/// The keys of consecutive digits under the same modifiers as one range, like `⌘ 1–9`.
fn join_digits(alternatives: Vec<Vec<SharedString>>) -> Vec<Vec<SharedString>> {
    let digit = |keys: &Vec<SharedString>| {
        keys.last()
            .filter(|key| key.len() == 1 && key.chars().all(|character| character.is_ascii_digit()))
            .cloned()
    };

    let (Some(first), Some(last)) = (alternatives.first(), alternatives.last()) else {
        return alternatives;
    };
    let (Some(from), Some(to)) = (digit(first), digit(last)) else {
        return alternatives;
    };

    let same_modifiers = alternatives.iter().all(|keys| {
        keys.len() == first.len()
            && keys.get(..keys.len() - 1) == first.get(..first.len() - 1)
            && digit(keys).is_some()
    });
    let consecutive = alternatives.windows(2).all(|pair| {
        match (pair.first().and_then(digit), pair.get(1).and_then(digit)) {
            (Some(before), Some(after)) => {
                before.parse::<u8>().ok().map(|digit| digit + 1) == after.parse::<u8>().ok()
            }
            _ => false,
        }
    });

    if alternatives.len() < 3 || !same_modifiers || !consecutive {
        return alternatives;
    }

    let mut joined = first.clone();
    if let Some(cap) = joined.last_mut() {
        *cap = format!("{from}\u{2013}{to}").into();
    }

    vec![joined]
}

/// Every action that has a key: the registered bindings, then the keys that controls handle.
pub(super) fn rows() -> Vec<Row> {
    rows_of(
        actions::shortcuts()
            .into_iter()
            .chain(actions::listed_shortcuts()),
    )
}

fn rows_of(shortcuts: impl IntoIterator<Item = actions::Shortcut>) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();

    for shortcut in shortcuts {
        let keys: Vec<SharedString> = shortcut
            .binding
            .keystrokes()
            .iter()
            .flat_map(caps)
            .collect();

        match rows
            .iter_mut()
            .find(|row| row.group == shortcut.group && row.name == shortcut.name)
        {
            Some(row) => row.alternatives.push(keys),
            None => rows.push(Row {
                group: shortcut.group,
                name: shortcut.name,
                alternatives: vec![keys],
            }),
        }
    }

    for row in &mut rows {
        row.alternatives = join_digits(std::mem::take(&mut row.alternatives));
    }

    rows
}

fn slug(name: &str) -> String {
    name.to_lowercase().replace(' ', "-")
}

fn row_element(index: usize, row: &Row) -> impl IntoElement + use<> {
    let mut keys = h_flex().gap(px(6.));
    let mut spoken = Vec::new();

    for (position, alternative) in row.alternatives.iter().enumerate() {
        if position > 0 {
            keys = keys.child(Label::new("or").size(LabelSize::Small).color(Color::Faint));
        }

        let kbd = Kbd::new(
            ("shortcut-keys", index * 10 + position),
            alternative.clone(),
        );
        spoken.push(kbd.spoken());
        keys = keys.child(kbd);
    }

    let selector = format!("shortcut-{}", slug(row.name));

    div()
        .id(("shortcut-row", index))
        .role(Role::Group)
        .aria_label(format!("{}, {}", row.name, spoken.join(" or ")))
        .debug_selector(move || selector)
        .child(PanelRow::new(row.name).end_slot(keys))
}

impl SettingsWindow {
    pub(super) fn keyboard_page() -> PageBlocks {
        let rows = rows();
        let mut page = PageBlocks::new(26.).child(page_title("Keyboard"));

        for group in ShortcutGroup::ALL {
            let panel = rows
                .iter()
                .enumerate()
                .filter(|(_, row)| row.group == group)
                .fold(Panel::new(), |panel, (index, row)| {
                    panel.row(row_element(index, row))
                });

            page = page.child(section(group.title(), panel));
        }

        page
    }
}

#[cfg(test)]
mod tests {
    use gpui::Keystroke;

    use super::*;

    fn caps_of(name: &str) -> Vec<Vec<String>> {
        let row = rows()
            .into_iter()
            .find(|row| row.name == name)
            .unwrap_or_else(|| panic!("no row named {name}"));

        row.alternatives
            .iter()
            .map(|keys| keys.iter().map(ToString::to_string).collect())
            .collect()
    }

    #[test]
    fn a_known_binding_shows_with_its_keys() {
        assert_eq!(caps_of("Open settings"), [["\u{2318}", ","]]);
        assert_eq!(caps_of("Open Review"), [["\u{21e7}", "\u{2318}", "R"]]);
        assert_eq!(caps_of("New terminal"), [["\u{2325}", "\u{2318}", "T"]]);
        assert_eq!(caps_of("Command palette"), [["\u{2318}", "K"]]);
    }

    #[test]
    fn modifiers_come_in_the_order_of_macos() {
        let keystroke = |text: &'static str| {
            KeybindingKeystroke::from_keystroke(Keystroke::parse(text).expect("a valid keystroke"))
        };
        let shown = |text: &'static str| -> Vec<String> {
            caps(&keystroke(text))
                .iter()
                .map(ToString::to_string)
                .collect()
        };

        assert_eq!(
            shown("cmd-shift-alt-ctrl-t"),
            ["\u{2303}", "\u{2325}", "\u{21e7}", "\u{2318}", "T"]
        );
        assert_eq!(shown("cmd-alt-t"), ["\u{2325}", "\u{2318}", "T"]);
    }

    #[test]
    fn plain_keys_and_arrows_have_their_own_caps() {
        assert_eq!(caps_of("Next file"), [["J"]]);
        assert_eq!(caps_of("Previous line in diff"), [["\u{2191}"]]);
        assert_eq!(caps_of("Cancel comment"), [["esc"]]);
    }

    #[test]
    fn a_row_is_a_group_named_after_its_action_and_keys() {
        use gpui::Element as _;
        use gpui::accesskit::Node;

        let row = rows()
            .into_iter()
            .find(|row| row.name == "Command palette")
            .expect("the palette has a row");
        let element = row_element(0, &row).into_element();
        let mut node = Node::new(element.a11y_role().unwrap_or(Role::Unknown));
        element.write_a11y_info(&mut node);

        assert_eq!(element.a11y_role(), Some(Role::Group));
        assert_eq!(node.label(), Some("Command palette, Command K"));
    }

    #[test]
    fn the_nine_agent_keys_are_one_range() {
        assert_eq!(caps_of("Focus agent"), [["\u{2318}", "1\u{2013}9"]]);
    }

    #[test]
    fn two_keys_for_one_action_are_two_alternatives() {
        assert_eq!(caps_of("Open row"), [["\u{21b5}"], ["Space"]]);
    }

    /// Whether the rows show `keys`, counting a range like `1–9` for each digit in it.
    fn is_shown(keys: &[SharedString]) -> bool {
        rows()
            .iter()
            .flat_map(|row| &row.alternatives)
            .any(|shown| {
                if shown.as_slice() == keys {
                    return true;
                }

                let (Some((cap, key)), Some((range, pressed))) =
                    (shown.split_last(), keys.split_last())
                else {
                    return false;
                };
                let digits = cap.split_once('\u{2013}');
                let digit = |text: &str| text.parse::<u8>().ok();

                cap.contains('\u{2013}')
                    && key == pressed
                    && digits
                        .zip(digit(range))
                        .is_some_and(|((from, to), pressed)| {
                            digit(from)
                                .zip(digit(to))
                                .is_some_and(|(from, to)| (from..=to).contains(&pressed))
                        })
            })
    }

    #[gpui::test]
    fn every_binding_in_the_keymap_of_the_app_has_a_row(cx: &mut gpui::TestAppContext) {
        cx.update(actions::bind_keys);

        let registered: Vec<Vec<SharedString>> = cx.update(|cx| {
            cx.key_bindings()
                .borrow()
                .bindings()
                .map(|binding| binding.keystrokes().iter().flat_map(caps).collect())
                .collect()
        });
        let unlisted: Vec<_> = registered.iter().filter(|keys| !is_shown(keys)).collect();

        assert!(!registered.is_empty());
        assert!(unlisted.is_empty(), "no row for {unlisted:?}");
    }

    #[gpui::test]
    fn the_rows_do_not_name_more_keys_than_the_keymap_holds(cx: &mut gpui::TestAppContext) {
        cx.update(actions::bind_keys);
        let held = cx.update(|cx| cx.key_bindings().borrow().bindings().len());

        // A range like `1–9` stands for as many bindings as it names.
        let named: usize = rows_of(actions::shortcuts())
            .iter()
            .flat_map(|row| &row.alternatives)
            .map(|keys| {
                keys.last()
                    .and_then(|cap| cap.split_once('\u{2013}'))
                    .map_or(1, |(from, to)| {
                        to.parse::<usize>().unwrap_or(0) - from.parse::<usize>().unwrap_or(0) + 1
                    })
            })
            .sum();

        assert_eq!(named, held);
    }

    #[test]
    fn control_tab_moves_focus_out_of_a_terminal() {
        assert_eq!(caps_of("Next focus stop"), [["\u{2303}", "\u{21e5}"]]);
        assert_eq!(
            caps_of("Previous focus stop"),
            [["\u{2303}", "\u{21e7}", "\u{21e5}"]]
        );
    }

    #[test]
    fn tab_and_back_tab_in_a_terminal_go_to_the_shell() {
        assert_eq!(caps_of("Tab in terminal"), [["\u{21e5}"]]);
        assert_eq!(caps_of("Back-tab in terminal"), [["\u{21e7}", "\u{21e5}"]]);
    }

    #[test]
    fn keys_the_controls_handle_have_rows() {
        assert_eq!(caps_of("Open context menu"), [["\u{21e7}", "F10"]]);
        assert_eq!(caps_of("Post comment or reply"), [["\u{2318}", "\u{21b5}"]]);
        assert_eq!(caps_of("Close settings"), [["\u{2318}", "W"]]);
        assert_eq!(caps_of("Next page"), [["\u{2193}"]]);
        assert_eq!(caps_of("Focus agent tile"), [["\u{21b5}"]]);
        assert_eq!(caps_of("First row"), [["HOME"]]);
        assert_eq!(caps_of("Last row"), [["END"]]);
        assert_eq!(
            caps_of("Cursor on old side of split"),
            [["\u{2325}", "\u{2190}"]]
        );
        assert_eq!(
            caps_of("Cursor on new side of split"),
            [["\u{2325}", "\u{2192}"]]
        );
        assert_eq!(caps_of("Make tile shorter"), [["\u{2191}"]]);
    }

    #[gpui::test]
    fn listed_keys_are_not_registered(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            swarm_ui::init(cx);
            actions::bind_keys(cx);
        });

        let listed: Vec<Vec<SharedString>> = actions::listed_shortcuts()
            .iter()
            .map(|shortcut| {
                shortcut
                    .binding
                    .keystrokes()
                    .iter()
                    .flat_map(caps)
                    .collect()
            })
            .collect();
        let placeholders: Vec<Vec<SharedString>> = cx.update(|cx| {
            cx.key_bindings()
                .borrow()
                .bindings()
                .filter(|binding| binding.action().partial_eq(&gpui::NoAction))
                .map(|binding| binding.keystrokes().iter().flat_map(caps).collect())
                .collect()
        });

        let registered: Vec<_> = listed
            .iter()
            .filter(|keys| placeholders.contains(keys))
            .collect();
        assert!(!listed.is_empty());
        assert!(registered.is_empty(), "{registered:?}");
    }

    #[test]
    fn the_groups_are_workspace_terminals_swarm_view_review_file_tree_and_settings() {
        let groups: Vec<&str> = ShortcutGroup::ALL
            .iter()
            .map(|group| group.title())
            .collect();

        assert_eq!(
            groups,
            [
                "Workspace",
                "Terminals",
                "Swarm view",
                "Review",
                "File tree",
                "Settings"
            ]
        );
        assert!(
            ShortcutGroup::ALL
                .iter()
                .all(|group| rows().iter().any(|row| row.group == *group))
        );
    }
}

#[cfg(test)]
mod page_tests {
    use gpui::{TestAppContext, VisualTestContext, px, size};

    use super::*;
    use crate::actions::OpenSettings;
    use crate::daemon::tests::{redraw, setup};
    use crate::settings::{Page, SettingsWindow};

    fn open_page(cx: &mut TestAppContext) -> VisualTestContext {
        let (_workspace, main) = setup(cx);

        main.dispatch_action(OpenSettings);
        let window = main
            .windows()
            .into_iter()
            .find_map(|window| window.downcast::<SettingsWindow>())
            .expect("the settings window opened");
        window
            .update(main, |settings, _, cx| {
                settings.select_page(Page::Keyboard, cx);
            })
            .expect("the window is open");

        let mut settings = VisualTestContext::from_window(window.into(), main);
        settings.simulate_resize(size(px(1280.), px(2400.)));
        redraw(&mut settings);

        settings
    }

    #[gpui::test]
    fn the_page_shows_a_row_for_a_binding_of_the_app(cx: &mut TestAppContext) {
        let mut page = open_page(cx);

        for row in [
            "shortcut-open-settings",
            "shortcut-command-palette",
            "shortcut-new-task",
            "shortcut-focus-agent",
            "shortcut-next-file",
            "shortcut-close-folder",
        ] {
            assert!(page.debug_bounds(row).is_some(), "{row} is missing");
        }
    }

    #[gpui::test]
    fn every_row_of_the_table_is_on_the_page(cx: &mut TestAppContext) {
        let mut page = open_page(cx);

        for row in rows() {
            let selector: &'static str = format!("shortcut-{}", slug(row.name)).leak();

            assert!(
                page.debug_bounds(selector).is_some(),
                "{selector} is missing"
            );
        }
    }
}
