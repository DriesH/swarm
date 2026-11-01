//! Settings › General: the system check and the About section.

use gpui::{AnyElement, FontWeight, Role, div, px};
use swarm_ui::prelude::*;

use super::SettingsWindow;
use super::pages::{page_title, plain_row, section};
use crate::system_check::SystemCheckView;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

const ICONS_CREDIT: &str = "File icons: vscode-icons, CC BY-SA 4.0";
const ICONS_REPOSITORY: &str = "https://github.com/vscode-icons/vscode-icons";
const ICONS_LICENSE: &str = "https://creativecommons.org/licenses/by-sa/4.0/";
const ICONS_CHANGES: &str = "Files unchanged, 2 logos left out";

/// The id, the label, the VoiceOver label and the address of each link of the icon credit.
const ICON_LINKS: [(&str, &str, &str, &str); 2] = [
    (
        "icons-repository",
        "Repository",
        "Repository, vscode-icons on GitHub",
        ICONS_REPOSITORY,
    ),
    (
        "icons-license",
        "License",
        "License, CC BY-SA 4.0",
        ICONS_LICENSE,
    ),
];

/// A button that opens `url` in the system browser.
fn link(
    id: &'static str,
    label: &'static str,
    accessible: &'static str,
    url: &'static str,
) -> Button {
    Button::new(id, label)
        .variant(ButtonVariant::Ghost)
        .icon(IconName::ExternalLink)
        .accessible_label(accessible)
        .debug_selector(|| id.to_owned())
        .on_click(move |_, _, cx| cx.open_url(url))
}

fn about() -> impl IntoElement {
    let version = PanelRow::new("Version").end_slot(
        div()
            .debug_selector(|| "about-version".to_owned())
            .child(Label::new(APP_VERSION).mono().size(LabelSize::Small)),
    );

    let icons = plain_row()
        .child(
            div()
                .flex_1()
                .debug_selector(|| "about-file-icons".to_owned())
                .child(Label::new(ICONS_CREDIT)),
        )
        .children(
            ICON_LINKS
                .into_iter()
                .map(|(id, label, accessible, url)| link(id, label, accessible, url)),
        );

    let changes = PanelRow::new("File icon changes").end_slot(
        div()
            .debug_selector(|| "about-icon-changes".to_owned())
            .child(Label::new(ICONS_CHANGES).size(LabelSize::Small)),
    );

    section("About", Panel::new().row(version).row(icons).row(changes))
}

impl SettingsWindow {
    pub(super) fn general_page(&self, _cx: &Context<Self>) -> AnyElement {
        let check = self.system_check.clone();

        let check_again = Button::new("check-again", "Check again")
            .track_focus(&self.check_again_focus)
            .debug_selector(|| "check-again".to_owned())
            .on_click(move |_, _, cx| check.update(cx, SystemCheckView::refresh));

        let system = v_flex()
            .id("System")
            .role(Role::Group)
            .aria_label("System")
            .child(
                h_flex()
                    .mb(px(8.))
                    .child(
                        Label::new("System")
                            .size(LabelSize::Small)
                            .weight(FontWeight::MEDIUM)
                            .color(Color::Faint),
                    )
                    .child(div().flex_1())
                    .child(check_again),
            )
            .child(self.system_check.clone());

        v_flex()
            .gap(px(26.))
            .child(page_title("General"))
            .child(system)
            .child(about())
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use gpui::{
        KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, TestAppContext, VisualTestContext,
        WindowHandle, px, size,
    };
    use swarm_protocol::{Client, Event, Request, ToolState};

    use super::super::{Page, SettingsWindow};
    use super::{APP_VERSION, ICON_LINKS, ICONS_CHANGES, ICONS_CREDIT, link};
    use crate::actions::OpenSettings;
    use crate::daemon::tests::{fake_daemon_replying, redraw, setup, wait_until};
    use crate::system_check::Tool;
    use crate::system_check::harness::{report_with, tool_check};
    use crate::workspace::DataSource;

    fn open_general(
        window: WindowHandle<SettingsWindow>,
        main: &mut VisualTestContext,
    ) -> VisualTestContext {
        window
            .update(main, |settings, _, cx| {
                settings.select_page(Page::General, cx);
            })
            .expect("the window is open");

        let settings = VisualTestContext::from_window(window.into(), main);
        settings.simulate_resize(size(px(1280.), px(1000.)));
        settings.executor().allow_parking();
        settings.run_until_parked();

        settings
    }

    fn settings_window(main: &VisualTestContext) -> WindowHandle<SettingsWindow> {
        main.windows()
            .into_iter()
            .find_map(|window| window.downcast::<SettingsWindow>())
            .expect("the settings window opened")
    }

    #[gpui::test]
    fn the_general_page_shows_the_check_under_a_system_heading(cx: &mut TestAppContext) {
        let (_workspace, main) = setup(cx);
        main.dispatch_action(OpenSettings);
        let window = settings_window(main);
        let mut settings = open_general(window, main);
        redraw(&mut settings);

        let count = window
            .read_with(&settings, |settings, cx| {
                settings.system_check().read(cx).count()
            })
            .expect("the window is open");
        let mut shown = |selector| settings.debug_bounds(selector).is_some();

        assert!(shown("system-check"));
        assert!(shown("system-check-row-git"));
        assert!(shown("system-check-row-notifications"));
        assert!(shown("check-again"));
        assert_eq!(count, (3, 6), "no daemon behind it: the sample");
    }

    #[gpui::test]
    fn check_again_asks_the_daemon_again(cx: &mut TestAppContext) {
        let checks = Arc::new(AtomicUsize::new(0));
        let socket = fake_daemon_replying({
            let checks = Arc::clone(&checks);

            move |request| match request {
                Request::SystemCheck { call } => {
                    checks.fetch_add(1, Ordering::SeqCst);

                    Some(Event::SystemCheck {
                        call,
                        check: report_with(&[(
                            Tool::Codex,
                            tool_check(ToolState::Missing, None, &["codex"]),
                        )]),
                    })
                }
                _ => None,
            }
        });
        let (workspace, main) = setup(cx);
        main.executor().allow_parking();
        let client = Client::connect(&socket).expect("connect to the fake daemon");
        workspace.update(main, |workspace, _| {
            workspace.source = DataSource::Live;
            workspace.daemon = Some(client);
        });
        main.dispatch_action(OpenSettings);
        let window = settings_window(main);
        let mut settings = open_general(window, main);

        let settled = wait_until(&mut settings, |cx| {
            redraw(cx);

            window
                .read_with(cx, |settings, cx| {
                    settings.system_check().read(cx).is_settled()
                })
                .unwrap_or(false)
        });
        let first = checks.load(Ordering::SeqCst);
        let center = settings
            .debug_bounds("check-again")
            .map(|bounds| bounds.center());
        if let Some(center) = center {
            settings.simulate_click(center, Modifiers::none());
        }
        let asked = wait_until(&mut settings, |cx| {
            redraw(cx);

            checks.load(Ordering::SeqCst) == 2
        });
        let count = window
            .read_with(&settings, |settings, cx| {
                settings.system_check().read(cx).count()
            })
            .expect("the window is open");

        assert!(settled, "the check never settled");
        assert_eq!(first, 1, "the page asks once when it opens");
        assert!(center.is_some(), "Check again is not on screen");
        assert!(asked, "Check again did not ask the daemon");
        assert_eq!(
            count,
            (4, 6),
            "Codex is missing, and macOS has no answer here"
        );
    }

    fn click_on(selector: &'static str, settings: &mut VisualTestContext) {
        let center = settings
            .debug_bounds(selector)
            .map(|bounds| bounds.center());

        assert!(center.is_some(), "{selector} is not on screen");
        if let Some(center) = center {
            settings.simulate_click(center, Modifiers::none());
        }
    }

    /// GPUI turns Enter on a focused button into a click when the key goes up.
    fn press_enter(settings: &mut VisualTestContext) {
        let keystroke = Keystroke::parse("enter").expect("enter is a key");

        settings.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        settings.simulate_event(KeyUpEvent { keystroke });
    }

    #[gpui::test]
    fn the_about_section_shows_the_version_and_the_icon_credit(cx: &mut TestAppContext) {
        let (_workspace, main) = setup(cx);
        main.dispatch_action(OpenSettings);
        let window = settings_window(main);
        let mut settings = open_general(window, main);
        redraw(&mut settings);

        let mut shown = |selector| settings.debug_bounds(selector).is_some();

        assert!(shown("about-version"));
        assert!(shown("about-file-icons"));
        assert!(shown("about-icon-changes"));
        assert!(shown("icons-repository"));
        assert!(shown("icons-license"));
        assert_eq!(APP_VERSION, env!("CARGO_PKG_VERSION"));
        assert_eq!(ICONS_CREDIT, "File icons: vscode-icons, CC BY-SA 4.0");
        assert_eq!(ICONS_CHANGES, "Files unchanged, 2 logos left out");
    }

    #[gpui::test]
    fn the_about_links_open_the_repository_and_the_license_in_the_browser(cx: &mut TestAppContext) {
        let (_workspace, main) = setup(cx);
        main.dispatch_action(OpenSettings);
        let window = settings_window(main);
        let mut settings = open_general(window, main);
        redraw(&mut settings);

        click_on("icons-repository", &mut settings);
        let repository = settings.opened_url();
        click_on("icons-license", &mut settings);
        let license = settings.opened_url();

        assert_eq!(
            repository.as_deref(),
            Some("https://github.com/vscode-icons/vscode-icons")
        );
        assert_eq!(
            license.as_deref(),
            Some("https://creativecommons.org/licenses/by-sa/4.0/")
        );
    }

    #[gpui::test]
    fn the_about_links_can_be_reached_and_pressed_with_the_keyboard(cx: &mut TestAppContext) {
        let (_workspace, main) = setup(cx);
        main.dispatch_action(OpenSettings);
        let window = settings_window(main);
        let mut settings = open_general(window, main);
        redraw(&mut settings);

        click_on("icons-repository", &mut settings);
        settings.simulate_keystrokes("tab");
        press_enter(&mut settings);

        assert_eq!(
            settings.opened_url().as_deref(),
            Some("https://creativecommons.org/licenses/by-sa/4.0/")
        );
    }

    #[gpui::test]
    fn the_about_links_are_labelled_buttons(cx: &mut TestAppContext) {
        use gpui::accesskit::{Node, Role as AccessRole};
        use gpui::{Element as _, IntoElement as _, RenderOnce as _};

        let (_workspace, main) = setup(cx);

        for (id, label, accessible, url) in ICON_LINKS {
            let (role, name) = main.update(|window, cx| {
                let element = link(id, label, accessible, url)
                    .render(window, cx)
                    .into_element();
                let mut node = Node::new(AccessRole::Unknown);
                element.write_a11y_info(&mut node);

                (element.a11y_role(), node.label().map(str::to_owned))
            });

            assert_eq!(role, Some(AccessRole::Button), "{id}");
            assert_eq!(name.as_deref(), Some(accessible), "{id}");
            assert!(accessible.starts_with(label), "{id}");
        }
    }
}
