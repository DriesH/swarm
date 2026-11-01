//! The menu bar: Swarm, File, Edit, View, Window and Help.
//!
//! Every item is an action. The shortcut next to an item comes from the keymap
//! (`actions::shortcuts`), so a key is bound once and the menu cannot disagree with it.
//!
//! Workspace items go to the main window. With that window closed they bring it back first.
//! The rest runs in the app, so it works from any window.

use gpui::{Action, App, Menu, MenuItem, OsAction, SystemMenuType};
use swarm_ui::edit;

use crate::actions::{
    About, AddProject, BringAllToFront, CloseTask, Hide, HideOthers, Minimize, NewTask,
    NewTerminal, OpenHelp, OpenSettings, Quit, ShowAll, ShowReview, ShowSwarm, ShowTerminals,
    ToggleChanges, ToggleCommandPalette, ToggleFiles, ToggleFullScreen, ToggleSidebar, Zoom,
};
use crate::lifecycle;
use crate::settings::Page;

/// Where Swarm Help goes. A placeholder until the docs website is live.
pub(crate) const HELP_URL: &str = "https://docs.swarm.example";

/// Sets the menu bar and the handlers of the actions no window handles.
pub(crate) fn install(cx: &mut App) {
    on_app_actions(cx);
    on_workspace_actions(cx);

    cx.set_menus(menus());
}

pub(crate) fn menus() -> Vec<Menu> {
    vec![
        Menu::new("Swarm").items([
            MenuItem::action("About Swarm", About),
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Swarm", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Swarm", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Task", NewTask),
            MenuItem::action("New Terminal", NewTerminal),
            MenuItem::action("Add Project…", AddProject),
            MenuItem::separator(),
            MenuItem::action("Close Task", CloseTask),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", edit::Undo, OsAction::Undo),
            MenuItem::os_action("Redo", edit::Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", edit::Cut, OsAction::Cut),
            MenuItem::os_action("Copy", edit::Copy, OsAction::Copy),
            MenuItem::os_action("Paste", edit::Paste, OsAction::Paste),
            MenuItem::os_action("Select All", edit::SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("View").items([
            MenuItem::action("Terminals", ShowTerminals),
            MenuItem::action("Swarm", ShowSwarm),
            MenuItem::action("Review", ShowReview),
            MenuItem::separator(),
            MenuItem::action("Toggle Sidebar", ToggleSidebar),
            MenuItem::action("Files", ToggleFiles),
            MenuItem::action("Changes", ToggleChanges),
            MenuItem::separator(),
            MenuItem::action("Command Palette", ToggleCommandPalette),
            MenuItem::separator(),
            MenuItem::action("Enter Full Screen", ToggleFullScreen),
        ]),
        // GPUI hands the menu named "Window" to macOS, which lists the open windows in it.
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action("Bring All to Front", BringAllToFront),
        ]),
        Menu::new("Help").items([MenuItem::action("Swarm Help", OpenHelp)]),
    ]
}

/// What runs in the app, whichever window is in front.
///
/// An action no window handled arrives here while the window it was sent to is still being
/// updated, and a window cannot be updated twice. So whatever touches a window waits for the
/// end of the update.
fn on_app_actions(cx: &mut App) {
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &BringAllToFront, cx| cx.activate(true));
    cx.on_action(|_: &OpenHelp, cx| cx.open_url(HELP_URL));
    cx.on_action(|_: &Minimize, cx| {
        cx.defer(|cx| on_front_window(cx, gpui::Window::minimize_window));
    });
    cx.on_action(|_: &Zoom, cx| cx.defer(|cx| on_front_window(cx, gpui::Window::zoom_window)));
    cx.on_action(|_: &ToggleFullScreen, cx| cx.defer(toggle_full_screen));

    // The settings window may be the one in front, and the main window may be closed.
    cx.on_action(|_: &OpenSettings, cx| cx.defer(|cx| open_settings(None, cx)));
    cx.on_action(|_: &About, cx| cx.defer(|cx| open_settings(Some(Page::General), cx)));
}

fn on_front_window(cx: &mut App, act: impl FnOnce(&gpui::Window)) {
    let Some(window) = cx.active_window() else {
        return;
    };

    if let Err(error) = window.update(cx, |_, window, _| act(window)) {
        log::debug!("the window closed before the menu reached it: {error}");
    }
}

fn toggle_full_screen(cx: &mut App) {
    let Some(window) = lifecycle::reveal_main_window(cx) else {
        return;
    };

    if let Err(error) = window.update(cx, |_, window, _| window.toggle_fullscreen()) {
        log::debug!("the main window closed before it went full screen: {error}");
    }
}

fn open_settings(page: Option<Page>, cx: &mut App) {
    let Some(workspace) = lifecycle::workspace(cx) else {
        return;
    };

    workspace.update(cx, |workspace, cx| workspace.open_settings(page, cx));
}

/// The workspace handles these when its window has the focus. Anywhere else, they go to it.
fn on_workspace_actions(cx: &mut App) {
    forward::<ShowTerminals>(cx);
    forward::<ShowSwarm>(cx);
    forward::<ShowReview>(cx);
    forward::<ToggleSidebar>(cx);
    forward::<ToggleFiles>(cx);
    forward::<ToggleChanges>(cx);
    forward::<ToggleCommandPalette>(cx);
    forward::<NewTask>(cx);
    forward::<NewTerminal>(cx);
    forward::<AddProject>(cx);
    forward::<CloseTask>(cx);
}

fn forward<A: Action>(cx: &mut App) {
    cx.on_action(|action: &A, cx| {
        let action = action.boxed_clone();

        cx.defer(move |cx| to_main_window(action.as_ref(), cx));
    });
}

/// Brings the main window back and hands it the action. It sends the action only when the
/// window handles it: one it does not handle comes back here and would go round forever.
fn to_main_window(action: &dyn Action, cx: &mut App) {
    let Some(window) = lifecycle::reveal_main_window(cx) else {
        return;
    };

    let handled = window.update(cx, |_, window, cx| {
        let handled = window.is_action_available(action, cx);

        if handled {
            window.dispatch_action(action.boxed_clone(), cx);
        }

        handled
    });

    match handled {
        Ok(true) => {}
        Ok(false) => log::warn!("the main window does not handle {}", action.name()),
        Err(error) => log::debug!(
            "the main window closed before it got {}: {error}",
            action.name()
        ),
    }
}

#[cfg(test)]
mod tests {
    use gpui::{
        AnyWindowHandle, Entity, Focusable as _, Keystroke, OwnedMenu, OwnedMenuItem,
        TestAppContext, VisualTestContext, WindowHandle, px,
    };

    use super::*;
    use crate::actions::shortcuts;
    use crate::daemon::tests::redraw;
    use crate::menu_bar::harness;
    use crate::palette::Overlay;
    use crate::settings::SettingsWindow;
    use crate::workspace::{RightPanel, View, Workspace};

    /// The sample workspace in the front window, with the menus set as `main` sets them.
    fn app(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
        harness::init(cx);
        let (workspace, cx) = cx.add_window_view(Workspace::with_sample);
        harness::install(&workspace, cx);
        cx.update(|window, cx| {
            install(cx);
            window.activate_window();
        });
        redraw(cx);

        (workspace, cx)
    }

    fn menus_set(cx: &TestAppContext) -> Vec<OwnedMenu> {
        cx.update(|cx| cx.get_menus()).expect("the menus are set")
    }

    fn owned_menus() -> Vec<OwnedMenu> {
        menus().into_iter().map(Menu::owned).collect()
    }

    fn labels(menu: &OwnedMenu) -> Vec<String> {
        menu.items
            .iter()
            .map(|item| match item {
                OwnedMenuItem::Separator => "-".to_owned(),
                OwnedMenuItem::Action { name, .. } => name.clone(),
                OwnedMenuItem::Submenu(menu) => menu.name.to_string(),
                OwnedMenuItem::SystemMenu(menu) => menu.name.to_string(),
            })
            .collect()
    }

    /// Every action of the menus, with its label and OS action.
    fn menu_actions(menus: &[OwnedMenu]) -> Vec<(String, Box<dyn Action>, Option<OsAction>)> {
        menus
            .iter()
            .flat_map(|menu| &menu.items)
            .filter_map(|item| match item {
                OwnedMenuItem::Action {
                    name,
                    action,
                    os_action,
                    ..
                } => Some((name.clone(), action.boxed_clone(), *os_action)),
                _ => None,
            })
            .collect()
    }

    fn item(menu: &str, label: &str, cx: &TestAppContext) -> Box<dyn Action> {
        let menus = menus_set(cx);
        let menu = menus
            .iter()
            .find(|candidate| candidate.name.as_ref() == menu)
            .unwrap_or_else(|| panic!("no menu named {menu}"));

        menu_actions(std::slice::from_ref(menu))
            .into_iter()
            .find_map(|(name, action, _)| (name == label).then_some(action))
            .unwrap_or_else(|| panic!("no item named {label} in {}", menu.name))
    }

    /// A click on a menu item: `AppKit` gives the action to the app, which gives it to the
    /// window in front, if there is one.
    fn choose(menu: &str, label: &str, cx: &mut VisualTestContext) {
        let app: &TestAppContext = cx;
        let action = item(menu, label, app);

        app.update(|cx| cx.dispatch_action(action.as_ref()));
        cx.run_until_parked();
    }

    fn overlay_is(
        workspace: &Entity<Workspace>,
        cx: &VisualTestContext,
        is: fn(&Overlay) -> bool,
    ) -> bool {
        workspace.read_with(cx, |workspace, _| {
            workspace.overlay.as_ref().is_some_and(is)
        })
    }

    fn settings_window(cx: &VisualTestContext) -> WindowHandle<SettingsWindow> {
        cx.windows()
            .into_iter()
            .find_map(|window| window.downcast::<SettingsWindow>())
            .expect("the settings window is open")
    }

    /// Puts the settings window in front, as a click on it does.
    fn settings_in_front(cx: &mut VisualTestContext) -> VisualTestContext {
        let window = settings_window(cx);
        let mut front = VisualTestContext::from_window(window.into(), cx);

        front.update(|window, _| window.activate_window());
        front.run_until_parked();

        front
    }

    fn settings_page(front: &VisualTestContext) -> Page {
        settings_window(front)
            .read_with(front, |settings, _| settings.page())
            .expect("the settings window is open")
    }

    /// The user closes the main window. Then no window is in front.
    fn close_main_window(cx: &mut VisualTestContext) {
        cx.update(|window, _| window.remove_window());
        cx.run_until_parked();
        cx.deactivate_window();
    }

    fn main_window(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> AnyWindowHandle {
        workspace
            .read_with(cx, |workspace, _| workspace.window)
            .expect("the main window is open")
    }

    #[gpui::test]
    fn the_menu_bar_has_the_six_menus_in_order(cx: &mut TestAppContext) {
        let (_workspace, cx) = app(cx);

        let names: Vec<_> = menus_set(cx)
            .iter()
            .map(|menu| menu.name.to_string())
            .collect();

        // macOS lists the open windows in the menu GPUI finds by this name.
        assert_eq!(names, ["Swarm", "File", "Edit", "View", "Window", "Help"]);
    }

    #[gpui::test]
    fn every_menu_has_its_items(cx: &mut TestAppContext) {
        let (_workspace, cx) = app(cx);
        let menus = menus_set(cx);

        let items = |name: &str| {
            menus
                .iter()
                .find(|menu| menu.name.as_ref() == name)
                .map(labels)
                .unwrap_or_default()
        };

        assert_eq!(
            items("Swarm"),
            [
                "About Swarm",
                "Settings…",
                "-",
                "Services",
                "-",
                "Hide Swarm",
                "Hide Others",
                "Show All",
                "-",
                "Quit Swarm"
            ]
        );
        assert_eq!(
            items("File"),
            [
                "New Task",
                "New Terminal",
                "Add Project…",
                "-",
                "Close Task"
            ]
        );
        assert_eq!(
            items("Edit"),
            ["Undo", "Redo", "-", "Cut", "Copy", "Paste", "Select All"]
        );
        assert_eq!(
            items("View"),
            [
                "Terminals",
                "Swarm",
                "Review",
                "-",
                "Toggle Sidebar",
                "Files",
                "Changes",
                "-",
                "Command Palette",
                "-",
                "Enter Full Screen"
            ]
        );
        assert_eq!(
            items("Window"),
            ["Minimize", "Zoom", "-", "Bring All to Front"]
        );
        assert_eq!(items("Help"), ["Swarm Help"]);
    }

    #[gpui::test]
    fn the_services_item_is_the_one_macos_fills(cx: &mut TestAppContext) {
        let (_workspace, cx) = app(cx);

        let services = menus_set(cx).into_iter().flat_map(|menu| menu.items).any(|item| {
            matches!(
                item,
                OwnedMenuItem::SystemMenu(menu)
                    if menu.name.as_ref() == "Services" && menu.menu_type == SystemMenuType::Services
            )
        });

        assert!(services);
    }

    #[gpui::test]
    fn the_edit_items_are_os_actions(cx: &mut TestAppContext) {
        let (_workspace, cx) = app(cx);

        let os_actions: Vec<_> = menu_actions(&menus_set(cx))
            .into_iter()
            .filter_map(|(name, _, os_action)| os_action.map(|os_action| (name, os_action)))
            .collect();
        let same = |(name, os_action): &(String, OsAction), label: &str, expected: OsAction| {
            name == label && *os_action == expected
        };

        assert_eq!(os_actions.len(), 6);
        assert!(same(&os_actions[0], "Undo", OsAction::Undo));
        assert!(same(&os_actions[1], "Redo", OsAction::Redo));
        assert!(same(&os_actions[2], "Cut", OsAction::Cut));
        assert!(same(&os_actions[3], "Copy", OsAction::Copy));
        assert!(same(&os_actions[4], "Paste", OsAction::Paste));
        assert!(same(&os_actions[5], "Select All", OsAction::SelectAll));
    }

    #[gpui::test]
    fn every_menu_action_with_a_shortcut_is_in_the_shortcut_table(cx: &mut TestAppContext) {
        harness::init(cx);
        let table = shortcuts();

        for (label, action, os_action) in menu_actions(&owned_menus()) {
            // The keys of a text field belong to the field and are not in the table.
            if os_action.is_some() {
                continue;
            }

            let bound = cx.update(|cx| {
                cx.key_bindings()
                    .borrow()
                    .bindings_for_action(action.as_ref())
                    .next()
                    .is_some()
            });
            let listed = table
                .iter()
                .any(|shortcut| shortcut.binding.action().partial_eq(action.as_ref()));

            assert_eq!(bound, listed, "{label}: the keymap and the table differ");
        }
    }

    #[gpui::test]
    fn the_items_with_a_shortcut_show_these_keys(cx: &mut TestAppContext) {
        harness::init(cx);
        let table = shortcuts();
        let actions = menu_actions(&owned_menus());

        for (label, keys) in [
            ("Settings…", "cmd-,"),
            ("Hide Swarm", "cmd-h"),
            ("Hide Others", "cmd-alt-h"),
            ("Quit Swarm", "cmd-q"),
            ("New Task", "cmd-t"),
            ("Review", "cmd-shift-r"),
            ("Command Palette", "cmd-k"),
            ("Minimize", "cmd-m"),
            ("Enter Full Screen", "ctrl-cmd-f"),
        ] {
            let action = actions
                .iter()
                .find_map(|(name, action, _)| (name == label).then_some(action))
                .unwrap_or_else(|| panic!("no item named {label}"));
            let shortcut = table
                .iter()
                .find(|shortcut| shortcut.binding.action().partial_eq(action.as_ref()))
                .unwrap_or_else(|| panic!("{label} has no shortcut"));
            let expected = Keystroke::parse(keys).expect("a valid keystroke");

            assert_eq!(
                shortcut
                    .binding
                    .keystrokes()
                    .iter()
                    .map(|keystroke| keystroke.inner().clone())
                    .collect::<Vec<_>>(),
                [expected],
                "{label}"
            );
        }
    }

    #[gpui::test]
    fn the_edit_items_show_the_keys_of_a_text_field(cx: &mut TestAppContext) {
        harness::init(cx);

        for (label, keys) in [
            ("Undo", "cmd-z"),
            ("Redo", "cmd-shift-z"),
            ("Cut", "cmd-x"),
            ("Copy", "cmd-c"),
            ("Paste", "cmd-v"),
            ("Select All", "cmd-a"),
        ] {
            let action = menu_actions(&owned_menus())
                .into_iter()
                .find_map(|(name, action, _)| (name == label).then_some(action))
                .unwrap_or_else(|| panic!("no item named {label}"));
            let expected = Keystroke::parse(keys).expect("a valid keystroke");

            let shown = cx.update(|cx| {
                cx.key_bindings()
                    .borrow()
                    .bindings_for_action(action.as_ref())
                    .any(|binding| {
                        binding
                            .keystrokes()
                            .iter()
                            .any(|keystroke| keystroke.inner() == &expected)
                    })
            });

            assert!(shown, "{label} shows no {keys}");
        }
    }

    #[gpui::test]
    fn every_item_has_a_handler(cx: &mut TestAppContext) {
        let (workspace, cx) = app(cx);
        let input = workspace.read_with(cx, |workspace, _| workspace.orchestrator_input.clone());
        cx.update(|window, cx| window.focus(&input.focus_handle(cx), cx));
        redraw(cx);

        let unhandled: Vec<_> = menu_actions(&menus_set(cx))
            .into_iter()
            .filter(|(_, action, _)| {
                !cx.update(|window, cx| {
                    window.is_action_available(action.as_ref(), cx)
                        || cx.is_action_available(action.as_ref())
                })
            })
            .map(|(label, _, _)| label)
            .collect();

        assert!(unhandled.is_empty(), "nothing handles {unhandled:?}");
    }

    #[gpui::test]
    fn the_workspace_handles_its_items_itself(cx: &mut TestAppContext) {
        let (_workspace, cx) = app(cx);
        let actions: Vec<(&str, Box<dyn Action>)> = vec![
            ("Terminals", Box::new(ShowTerminals)),
            ("Swarm", Box::new(ShowSwarm)),
            ("Review", Box::new(ShowReview)),
            ("Toggle Sidebar", Box::new(ToggleSidebar)),
            ("Files", Box::new(ToggleFiles)),
            ("Changes", Box::new(ToggleChanges)),
            ("Command Palette", Box::new(ToggleCommandPalette)),
            ("New Task", Box::new(NewTask)),
            ("New Terminal", Box::new(NewTerminal)),
            ("Add Project…", Box::new(AddProject)),
            ("Close Task", Box::new(CloseTask)),
            ("Settings…", Box::new(OpenSettings)),
            ("About Swarm", Box::new(About)),
        ];

        for (label, action) in actions {
            let handled = cx.update(|window, cx| window.is_action_available(action.as_ref(), cx));

            assert!(handled, "the workspace does not handle {label}");
        }
    }

    #[gpui::test]
    fn new_task_opens_the_new_task_dialog(cx: &mut TestAppContext) {
        let (workspace, cx) = app(cx);

        choose("File", "New Task", cx);

        assert!(overlay_is(&workspace, cx, |overlay| matches!(
            overlay,
            Overlay::NewTask { .. }
        )));
    }

    #[gpui::test]
    fn new_terminal_adds_a_terminal_to_the_open_task(cx: &mut TestAppContext) {
        let (workspace, cx) = app(cx);
        let count = |workspace: &Entity<Workspace>, cx: &VisualTestContext| {
            workspace.read_with(cx, |workspace, _| {
                workspace.active_task().user_terminals.len()
            })
        };
        let before = count(&workspace, cx);

        choose("File", "New Terminal", cx);

        assert_eq!(count(&workspace, cx), before + 1);
    }

    #[gpui::test]
    fn add_project_opens_the_folder_picker(cx: &mut TestAppContext) {
        let (_workspace, cx) = app(cx);
        let before = cx.did_prompt_for_paths();

        choose("File", "Add Project…", cx);

        assert!(!before);
        assert!(cx.did_prompt_for_paths());
    }

    #[gpui::test]
    fn close_task_opens_the_close_task_dialog(cx: &mut TestAppContext) {
        let (workspace, cx) = app(cx);

        choose("File", "Close Task", cx);

        assert!(overlay_is(&workspace, cx, |overlay| matches!(
            overlay,
            Overlay::CloseTask { .. }
        )));
    }

    #[gpui::test]
    fn the_view_items_switch_the_view(cx: &mut TestAppContext) {
        let (workspace, cx) = app(cx);
        let view = |cx: &VisualTestContext| workspace.read_with(cx, |workspace, _| workspace.view);

        choose("View", "Swarm", cx);
        let swarm = view(cx);
        choose("View", "Review", cx);
        let review = view(cx);
        choose("View", "Terminals", cx);
        let terminals = view(cx);

        assert_eq!(
            (swarm, review, terminals),
            (View::Swarm, View::Review, View::Terminals)
        );
    }

    #[gpui::test]
    fn toggle_sidebar_hides_and_shows_the_sidebar(cx: &mut TestAppContext) {
        let (workspace, cx) = app(cx);
        let shown = |cx: &mut VisualTestContext| {
            redraw(cx);

            cx.debug_bounds("add-project").is_some()
        };
        let before = shown(cx);

        choose("View", "Toggle Sidebar", cx);
        let hidden = !shown(cx);
        let hidden_flag = workspace.read_with(cx, |workspace, _| !workspace.sidebar_visible);
        choose("View", "Toggle Sidebar", cx);
        let back = shown(cx);

        assert!(before);
        assert!(hidden);
        assert!(hidden_flag);
        assert!(back);
    }

    #[gpui::test]
    fn the_header_leaves_room_for_the_traffic_lights_without_the_sidebar(cx: &mut TestAppContext) {
        let (_workspace, cx) = app(cx);
        redraw(cx);
        let tabs = |cx: &mut VisualTestContext| {
            redraw(cx);

            cx.debug_bounds("task-tabs")
                .map(|bounds| bounds.origin.x)
                .expect("the task tabs are on screen")
        };
        let with_sidebar = tabs(cx);

        choose("View", "Toggle Sidebar", cx);
        let without = tabs(cx);

        // The close, minimize and zoom buttons end 68 points from the window's left edge.
        assert!(with_sidebar > px(248.));
        assert!(without >= px(68.), "the tabs start at {without:?}");
    }

    #[gpui::test]
    fn the_sidebar_button_hides_the_sidebar_through_the_same_action(cx: &mut TestAppContext) {
        let (workspace, cx) = app(cx);
        let button = cx
            .debug_bounds("toggle-sidebar")
            .expect("the sidebar button is on screen");

        cx.simulate_click(button.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        assert!(!workspace.read_with(cx, |workspace, _| workspace.sidebar_visible));
    }

    #[gpui::test]
    fn files_and_changes_toggle_the_right_panel(cx: &mut TestAppContext) {
        let (workspace, cx) = app(cx);
        let panel =
            |cx: &VisualTestContext| workspace.read_with(cx, |workspace, _| workspace.right_panel);
        let start = panel(cx);

        choose("View", "Files", cx);
        let files = panel(cx);
        choose("View", "Files", cx);
        let closed = panel(cx);
        choose("View", "Changes", cx);
        let changes = panel(cx);

        assert_eq!(start, Some(RightPanel::Changes));
        assert_eq!(files, Some(RightPanel::Files));
        assert_eq!(closed, None);
        assert_eq!(changes, Some(RightPanel::Changes));
    }

    #[gpui::test]
    fn command_palette_opens_the_palette(cx: &mut TestAppContext) {
        let (workspace, cx) = app(cx);

        choose("View", "Command Palette", cx);

        assert!(overlay_is(&workspace, cx, |overlay| matches!(
            overlay,
            Overlay::Palette { .. }
        )));
    }

    #[gpui::test]
    fn enter_full_screen_toggles_the_main_window(cx: &mut TestAppContext) {
        let (_workspace, cx) = app(cx);
        let full = |cx: &mut VisualTestContext| cx.update(|window, _| window.is_fullscreen());
        let before = full(cx);

        choose("View", "Enter Full Screen", cx);
        let entered = full(cx);
        choose("View", "Enter Full Screen", cx);
        let left = full(cx);

        assert!(!before);
        assert!(entered);
        assert!(!left);
    }

    #[gpui::test]
    fn settings_opens_the_settings_window(cx: &mut TestAppContext) {
        let (_workspace, cx) = app(cx);
        let before = cx
            .windows()
            .iter()
            .any(|window| window.downcast::<SettingsWindow>().is_some());

        choose("Swarm", "Settings…", cx);

        assert!(!before);
        assert!(
            cx.windows()
                .iter()
                .any(|window| window.downcast::<SettingsWindow>().is_some())
        );
    }

    #[gpui::test]
    fn about_opens_settings_on_the_general_page(cx: &mut TestAppContext) {
        let (_workspace, cx) = app(cx);

        choose("Swarm", "About Swarm", cx);

        assert_eq!(settings_page(cx), Page::General);
    }

    #[gpui::test]
    fn about_shows_the_general_page_of_a_settings_window_that_is_open(cx: &mut TestAppContext) {
        let (_workspace, cx) = app(cx);
        choose("Swarm", "Settings…", cx);
        settings_window(cx)
            .update(cx, |settings, _, cx| {
                settings.select_page(Page::Keyboard, cx);
            })
            .expect("the settings window is open");
        let before = settings_page(cx);

        choose("Swarm", "About Swarm", cx);

        assert_eq!(before, Page::Keyboard);
        assert_eq!(settings_page(cx), Page::General);
    }

    #[gpui::test]
    fn swarm_help_opens_the_docs_website_in_the_browser(cx: &mut TestAppContext) {
        let (_workspace, cx) = app(cx);

        choose("Help", "Swarm Help", cx);

        assert_eq!(cx.opened_url().as_deref(), Some(HELP_URL));
        assert!(HELP_URL.starts_with("https://"));
    }

    #[gpui::test]
    fn the_handlers_of_the_app_items_are_set(cx: &mut TestAppContext) {
        let (_workspace, cx) = app(cx);

        // The test platform cannot hide, minimize, zoom or quit, so the actions are not sent.
        for (menu, label) in [
            ("Swarm", "Hide Swarm"),
            ("Swarm", "Hide Others"),
            ("Swarm", "Show All"),
            ("Swarm", "Quit Swarm"),
            ("Window", "Minimize"),
            ("Window", "Zoom"),
            ("Window", "Bring All to Front"),
        ] {
            let action = item(menu, label, cx);
            let handled = cx.update(|_, cx| cx.is_action_available(action.as_ref()));

            assert!(handled, "{label}");
        }
    }

    #[gpui::test]
    fn bring_all_to_front_does_not_need_a_window(cx: &mut TestAppContext) {
        let (_workspace, cx) = app(cx);
        close_main_window(cx);

        choose("Window", "Bring All to Front", cx);

        assert!(cx.windows().is_empty());
    }

    #[gpui::test]
    fn with_the_settings_window_in_front_a_workspace_item_acts_on_the_main_window(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = app(cx);
        choose("Swarm", "Settings…", cx);
        let mut front = settings_in_front(cx);
        let before = overlay_is(&workspace, &front, |overlay| {
            matches!(overlay, Overlay::NewTask { .. })
        });

        choose("File", "New Task", &mut front);

        assert!(!before);
        assert!(overlay_is(&workspace, &front, |overlay| matches!(
            overlay,
            Overlay::NewTask { .. }
        )));
        assert_eq!(
            front.update(|_, cx| cx.active_window()),
            Some(main_window(&workspace, &front)),
            "the main window did not come to the front"
        );
    }

    #[gpui::test]
    fn with_the_settings_window_in_front_every_file_and_view_item_is_on(cx: &mut TestAppContext) {
        let (_workspace, cx) = app(cx);
        choose("Swarm", "Settings…", cx);
        let front = settings_in_front(cx);

        let candidates: Vec<_> = menus_set(&front)
            .into_iter()
            .filter(|menu| ["File", "View"].contains(&menu.name.as_ref()))
            .flat_map(|menu| menu_actions(&[menu]))
            .collect();
        let app: &TestAppContext = &front;
        let off: Vec<_> = candidates
            .iter()
            .filter(|(_, action, _)| !app.update(|cx| cx.is_action_available(action.as_ref())))
            .map(|(label, _, _)| label.clone())
            .collect();

        assert_eq!(candidates.len(), 12);
        assert!(off.is_empty(), "off while settings is in front: {off:?}");
    }

    #[gpui::test]
    fn with_the_settings_window_in_front_the_settings_items_still_work(cx: &mut TestAppContext) {
        let (_workspace, cx) = app(cx);
        choose("Swarm", "Settings…", cx);
        let mut front = settings_in_front(cx);
        let before = settings_page(&front);

        choose("Swarm", "About Swarm", &mut front);

        assert_ne!(before, Page::General);
        assert_eq!(settings_page(&front), Page::General);
    }

    #[gpui::test]
    fn with_the_window_closed_a_view_item_brings_it_back_and_acts(cx: &mut TestAppContext) {
        let (workspace, cx) = app(cx);
        close_main_window(cx);
        let closed = cx.windows().is_empty();

        choose("View", "Toggle Sidebar", cx);

        assert!(closed);
        assert_eq!(cx.windows().len(), 1, "the main window did not come back");
        assert!(workspace.read_with(cx, |workspace, _| workspace.window.is_some()));
        assert!(!workspace.read_with(cx, |workspace, _| workspace.sidebar_visible));
        let app: &TestAppContext = cx;
        assert_eq!(app.read(lifecycle::policy), gpui::ActivationPolicy::Regular);
    }

    #[gpui::test]
    fn with_the_window_closed_a_file_item_brings_it_back_and_acts(cx: &mut TestAppContext) {
        let (workspace, cx) = app(cx);
        close_main_window(cx);

        choose("File", "New Task", cx);

        assert_eq!(cx.windows().len(), 1, "the main window did not come back");
        assert!(overlay_is(&workspace, cx, |overlay| matches!(
            overlay,
            Overlay::NewTask { .. }
        )));
    }

    #[gpui::test]
    fn with_the_window_closed_full_screen_brings_it_back(cx: &mut TestAppContext) {
        let (_workspace, cx) = app(cx);
        close_main_window(cx);

        choose("View", "Enter Full Screen", cx);

        assert_eq!(cx.windows().len(), 1, "the main window did not come back");
    }

    #[gpui::test]
    fn with_the_window_closed_settings_opens_without_it(cx: &mut TestAppContext) {
        let (_workspace, cx) = app(cx);
        close_main_window(cx);

        choose("Swarm", "About Swarm", cx);

        let windows = cx.windows();
        assert_eq!(windows.len(), 1);
        assert!(windows[0].downcast::<SettingsWindow>().is_some());
    }

    #[gpui::test]
    fn an_action_the_main_window_cannot_handle_is_dropped_and_does_not_loop(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = app(cx);
        cx.update(gpui::Window::blur);
        redraw(cx);

        choose("View", "Toggle Sidebar", cx);

        assert!(workspace.read_with(cx, |workspace, _| workspace.sidebar_visible));
    }

    #[gpui::test]
    fn the_edit_menu_reaches_a_text_field(cx: &mut TestAppContext) {
        let (workspace, cx) = app(cx);
        let input = workspace.read_with(cx, |workspace, _| workspace.orchestrator_input.clone());
        input.update(cx, |input, cx| input.set_text("make a plan", cx));
        cx.update(|window, cx| window.focus(&input.focus_handle(cx), cx));
        redraw(cx);
        let text =
            |cx: &VisualTestContext| input.read_with(cx, |input, _| input.text().to_string());

        choose("Edit", "Select All", cx);
        choose("Edit", "Copy", cx);
        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        let after_copy = text(cx);
        choose("Edit", "Cut", cx);
        let after_cut = text(cx);
        choose("Edit", "Paste", cx);
        let after_paste = text(cx);
        choose("Edit", "Undo", cx);
        let after_undo = text(cx);
        choose("Edit", "Redo", cx);
        let after_redo = text(cx);

        assert_eq!(copied.as_deref(), Some("make a plan"));
        assert_eq!(after_copy, "make a plan");
        assert_eq!(after_cut, "");
        assert_eq!(after_paste, "make a plan");
        assert_eq!(after_undo, "");
        assert_eq!(after_redo, "make a plan");
    }
}
