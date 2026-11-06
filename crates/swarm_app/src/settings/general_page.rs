//! Settings › General: the system check and the About section.

use gpui::{FontWeight, Role, div, px};
use swarm_ui::a11y::accessible;
use swarm_ui::prelude::*;
use swarm_ui::styles::focus_ring;

use super::SettingsWindow;
use super::pages::{PageBlocks, input_row, page_title, plain_row, section, switch_row};
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

/// A link that opens `url` in the system browser.
#[derive(IntoElement)]
struct Link {
    id: &'static str,
    label: &'static str,
    accessible: &'static str,
    url: &'static str,
}

fn link(
    id: &'static str,
    label: &'static str,
    accessible: &'static str,
    url: &'static str,
) -> Link {
    Link {
        id,
        label,
        accessible,
        url,
    }
}

impl RenderOnce for Link {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (muted, fg, active, accent) = (colors.muted, colors.fg, colors.active, colors.accent);
        let (id, url) = (self.id, self.url);

        accessible(div().id(id), Role::Link, self.accessible)
            .debug_selector(|| id.to_owned())
            .focusable()
            .tab_stop(true)
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .h(px(28.))
            .px(px(12.))
            .rounded(px(7.))
            .text_color(muted)
            .cursor_pointer()
            .hover(move |style| style.bg(active).text_color(fg))
            .focus_visible(move |style| focus_ring(style, accent))
            .on_click(move |_, _, cx| cx.open_url(url))
            .child(
                Icon::new(IconName::ExternalLink)
                    .size(IconSize::Small)
                    .color(Color::Muted),
            )
            .child(Label::new(self.label).inherit_color())
    }
}

fn version_label() -> Label {
    Label::new(APP_VERSION)
        .accessible("about-version-text")
        .mono()
        .size(LabelSize::Small)
}

fn credit_label() -> Label {
    Label::new(ICONS_CREDIT).accessible("about-file-icons-text")
}

fn changes_label() -> Label {
    Label::new(ICONS_CHANGES)
        .accessible("about-icon-changes-text")
        .size(LabelSize::Small)
}

fn about() -> impl IntoElement {
    let version = PanelRow::new("Version").end_slot(
        div()
            .debug_selector(|| "about-version".to_owned())
            .child(version_label()),
    );

    let icons = plain_row()
        .child(
            div()
                .flex_1()
                .debug_selector(|| "about-file-icons".to_owned())
                .child(credit_label()),
        )
        .children(
            ICON_LINKS
                .into_iter()
                .map(|(id, label, accessible, url)| link(id, label, accessible, url)),
        );

    let changes = PanelRow::new("File icon changes").end_slot(
        div()
            .debug_selector(|| "about-icon-changes".to_owned())
            .child(changes_label()),
    );

    section("About", Panel::new().row(version).row(icons).row(changes))
}

/// The Updates section: the switch, the status line and the Check now button.
fn updates(state: &gpui::Entity<super::SettingsState>, cx: &App) -> impl IntoElement {
    let auto_check = state.read(cx).settings.updates.auto_check;
    let status = crate::updates::status(cx);

    let switch = switch_row(
        state,
        "auto-check-updates",
        "Check for updates automatically",
        auto_check,
        |state, on| state.settings.updates.auto_check = on,
    );

    let button = update_button(cx);

    let status_row = PanelRow::new("Status").end_slot(
        h_flex()
            .gap(px(10.))
            .child(
                div()
                    .debug_selector(|| "update-status-line".to_owned())
                    .child(
                        LiveRegion::status("update-status-text", status.clone())
                            .child(Label::new(status).size(LabelSize::Small)),
                    ),
            )
            .child(button),
    );

    section("Updates", Panel::new().row(switch).row(status_row))
}

/// **Check now**, with a spinner while a check or a download runs. When an update is ready:
/// **Update now**, or **Download** when the app cannot replace itself.
fn update_button(cx: &App) -> Button {
    if let Some(ready) = crate::updates::ready(cx) {
        if ready.replaceable {
            return Button::new("check-now", "Update now")
                .variant(ButtonVariant::Primary)
                .debug_selector(|| "update-now".to_owned())
                .on_click(|_, _, cx| crate::updates::restart_to_update(cx));
        }

        return Button::new("check-now", "Download")
            .debug_selector(|| "update-download".to_owned())
            .on_click(move |_, _, cx| cx.open_url(&ready.notes_url));
    }

    Button::new("check-now", "Check now")
        .debug_selector(|| "check-now".to_owned())
        .loading(crate::updates::busy(cx))
        .on_click(|_, _, cx| crate::updates::check_now(cx))
}

/// The Branches section: the prefix of new task branches.
fn branches(inputs: &super::Inputs) -> impl IntoElement {
    section(
        "Branches",
        Panel::new().row(input_row("Branch prefix", &inputs.branch_prefix, 160.)),
    )
}

/// The Pull requests section: the draft switch.
fn pull_requests(state: &gpui::Entity<super::SettingsState>, cx: &App) -> impl IntoElement {
    let open_as_draft = state.read(cx).settings.pull_requests.open_as_draft;

    let switch = switch_row(
        state,
        "open-as-draft",
        "Open as draft",
        open_as_draft,
        |state, on| state.settings.pull_requests.open_as_draft = on,
    );

    section("Pull requests", Panel::new().row(switch))
}

impl SettingsWindow {
    pub(super) fn general_page(&self, cx: &Context<Self>) -> PageBlocks {
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

        PageBlocks::new(26.)
            .child(page_title("General"))
            .child(system)
            .child(updates(&self.state, cx))
            .child(branches(&self.inputs))
            .child(pull_requests(&self.state, cx))
            .child(about())
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

            checks.load(Ordering::SeqCst) == 3
        });
        let count = window
            .read_with(&settings, |settings, cx| {
                settings.system_check().read(cx).count()
            })
            .expect("the window is open");

        assert!(settled, "the check never settled");
        assert_eq!(
            first, 2,
            "the Agents page the window opens on asks once, and General asks once when it opens"
        );
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

    fn with_updater(
        build: swarm_update::RunningBuild,
        workspace: &gpui::Entity<crate::workspace::Workspace>,
        main: &mut VisualTestContext,
    ) -> Arc<crate::updates::tests::FakeSource> {
        use crate::updates::tests::{FakeHost, FakeSource, install_fake};

        let source = FakeSource::with_update();
        let host = FakeHost::new(true);
        main.update(|_, cx| install_fake(workspace, &source, &host, build, cx));

        source
    }

    #[gpui::test]
    fn the_updates_section_shows_the_switch_the_status_and_check_now(cx: &mut TestAppContext) {
        let (workspace, main) = setup(cx);
        with_updater(crate::updates::tests::signed_build(), &workspace, main);
        main.dispatch_action(OpenSettings);
        let window = settings_window(main);
        let mut settings = open_general(window, main);
        redraw(&mut settings);

        let mut shown = |selector| settings.debug_bounds(selector).is_some();

        assert!(shown("auto-check-updates"));
        assert!(shown("update-status-line"));
        assert!(shown("check-now"));
    }

    #[gpui::test]
    fn check_now_checks_and_the_status_line_follows(cx: &mut TestAppContext) {
        let (workspace, main) = setup(cx);
        let source = with_updater(crate::updates::tests::signed_build(), &workspace, main);
        main.dispatch_action(OpenSettings);
        let window = settings_window(main);
        let mut settings = open_general(window, main);
        redraw(&mut settings);

        click_on("check-now", &mut settings);
        redraw(&mut settings);

        assert_eq!(source.fetched(), 1);
        assert_eq!(
            settings.update(|_, cx| crate::updates::status(cx)),
            "Update ready: 1.2.0"
        );
    }

    #[gpui::test]
    fn check_now_on_an_unsigned_build_only_shows_the_status(cx: &mut TestAppContext) {
        let (workspace, main) = setup(cx);
        let source = with_updater(crate::updates::tests::unsigned_build(), &workspace, main);
        main.dispatch_action(OpenSettings);
        let window = settings_window(main);
        let mut settings = open_general(window, main);
        redraw(&mut settings);

        click_on("check-now", &mut settings);
        redraw(&mut settings);

        assert_eq!(source.fetched(), 0);
        assert_eq!(
            settings.update(|_, cx| crate::updates::status(cx)),
            "Updates need a signed build"
        );
    }

    /// A started updater with a release 1.2.0 to find, and the host it installs through.
    fn updater_with_host(
        replaceable: bool,
        workspace: &gpui::Entity<crate::workspace::Workspace>,
        main: &mut VisualTestContext,
    ) -> (
        gpui::Entity<crate::updates::Updater>,
        std::rc::Rc<crate::updates::tests::FakeHost>,
    ) {
        use crate::updates::tests::{FakeHost, FakeSource, install_fake, signed_build};

        let (source, host) = (FakeSource::with_update(), FakeHost::new(replaceable));
        let updater =
            main.update(|_, cx| install_fake(workspace, &source, &host, signed_build(), cx));

        (updater, host)
    }

    /// The name and the busy flag of the button `update_button` builds for the updater's
    /// state right now.
    fn update_button_seen(main: &mut VisualTestContext) -> (Option<String>, bool) {
        use gpui::{Element as _, IntoElement as _, RenderOnce as _};

        main.update(|window, cx| {
            let element = super::update_button(cx).render(window, cx).into_element();
            let mut node = gpui::accesskit::Node::new(gpui::accesskit::Role::Unknown);
            element.write_a11y_info(&mut node);

            (node.label().map(str::to_owned), node.is_busy())
        })
    }

    fn draw(settings: &mut VisualTestContext) {
        settings.update(|window, cx| window.draw(cx).clear(cx));
    }

    #[gpui::test]
    fn the_button_shows_a_spinner_while_the_updater_checks(cx: &mut TestAppContext) {
        let (workspace, main) = setup(cx);
        let (updater, _host) = updater_with_host(true, &workspace, main);
        main.dispatch_action(OpenSettings);
        let window = settings_window(main);
        let mut settings = open_general(window, main);
        redraw(&mut settings);
        let before = update_button_seen(&mut settings);

        updater.update(&mut settings, crate::updates::Updater::check);
        draw(&mut settings);
        let during = update_button_seen(&mut settings);

        assert_eq!(before, (Some("Check now".to_owned()), false));
        assert_eq!(during, (Some("Check now".to_owned()), true));
        assert!(settings.debug_bounds("check-now").is_some());
    }

    #[gpui::test]
    fn the_button_shows_a_spinner_while_the_updater_downloads(cx: &mut TestAppContext) {
        use crate::updates::UpdateState;

        let (workspace, main) = setup(cx);
        let (updater, _host) = updater_with_host(true, &workspace, main);
        main.dispatch_action(OpenSettings);
        let window = settings_window(main);
        let mut settings = open_general(window, main);
        redraw(&mut settings);

        crate::updates::tests::show_state(
            &updater,
            UpdateState::Downloading(swarm_update::Version::new(1, 2, 0)),
            &mut settings,
        );
        draw(&mut settings);

        assert_eq!(
            update_button_seen(&mut settings),
            (Some("Check now".to_owned()), true)
        );
        assert!(settings.debug_bounds("check-now").is_some());
    }

    #[gpui::test]
    fn a_ready_update_turns_the_button_into_update_now_and_it_installs(cx: &mut TestAppContext) {
        let (workspace, main) = setup(cx);
        let (_updater, host) = updater_with_host(true, &workspace, main);
        main.dispatch_action(OpenSettings);
        let window = settings_window(main);
        let mut settings = open_general(window, main);
        redraw(&mut settings);
        let before = settings.debug_bounds("update-now").is_some();

        click_on("check-now", &mut settings);
        redraw(&mut settings);
        let after = settings.debug_bounds("update-now").is_some();
        let old_button = settings.debug_bounds("check-now").is_some();
        click_on("update-now", &mut settings);

        assert!(!before);
        assert!(after, "the button did not turn into Update now");
        assert!(!old_button);
        assert_eq!(
            update_button_seen(&mut settings),
            (Some("Update now".to_owned()), false)
        );
        assert_eq!(host.launches.borrow().len(), 1);
        assert!(host.launches.borrow()[0].1, "the app reopens");
        assert_eq!(host.quits.get(), 1);
    }

    #[gpui::test]
    fn an_app_that_cannot_replace_itself_gets_a_download_button(cx: &mut TestAppContext) {
        let (workspace, main) = setup(cx);
        let (_updater, host) = updater_with_host(false, &workspace, main);
        main.dispatch_action(OpenSettings);
        let window = settings_window(main);
        let mut settings = open_general(window, main);
        redraw(&mut settings);

        click_on("check-now", &mut settings);
        redraw(&mut settings);
        let shown = settings.debug_bounds("update-download").is_some();
        click_on("update-download", &mut settings);

        assert!(shown);
        assert!(settings.debug_bounds("update-now").is_none());
        assert!(host.launches.borrow().is_empty());
        assert_eq!(host.quits.get(), 0);
        assert_eq!(
            settings.opened_url().as_deref(),
            Some("https://github.com/DriesH/swarm/releases/tag/v1.2.0")
        );
    }

    #[gpui::test]
    fn a_failed_install_gets_a_download_button(cx: &mut TestAppContext) {
        use crate::updates::tests::{FakeHost, FakeSource, install_fake, signed_build};

        let (workspace, main) = setup(cx);
        let source = FakeSource::with_failed_install("the signature is wrong");
        let host = FakeHost::new(true);
        main.update(|_, cx| install_fake(&workspace, &source, &host, signed_build(), cx));
        main.dispatch_action(OpenSettings);
        let window = settings_window(main);
        let mut settings = open_general(window, main);
        redraw(&mut settings);

        click_on("check-now", &mut settings);
        redraw(&mut settings);

        assert!(settings.debug_bounds("update-download").is_some());
        assert!(settings.debug_bounds("update-now").is_none());
    }

    #[gpui::test]
    fn the_switch_turns_automatic_checks_off(cx: &mut TestAppContext) {
        let (workspace, main) = setup(cx);
        with_updater(crate::updates::tests::signed_build(), &workspace, main);
        main.dispatch_action(OpenSettings);
        let window = settings_window(main);
        let mut settings = open_general(window, main);
        redraw(&mut settings);
        let before = settings.update(|_, cx| crate::settings::auto_check_updates(cx));

        click_on("auto-check-updates", &mut settings);
        redraw(&mut settings);

        assert!(before, "on by default");
        assert!(!settings.update(|_, cx| crate::settings::auto_check_updates(cx)));
    }

    #[gpui::test]
    fn the_switch_turns_open_as_draft_off(cx: &mut TestAppContext) {
        let (_workspace, main) = setup(cx);
        main.dispatch_action(OpenSettings);
        let window = settings_window(main);
        let mut settings = open_general(window, main);
        redraw(&mut settings);
        let before = settings.update(|_, cx| crate::settings::open_pull_requests_as_draft(cx));

        click_on("open-as-draft", &mut settings);
        redraw(&mut settings);

        assert!(before, "on by default");
        assert!(!settings.update(|_, cx| crate::settings::open_pull_requests_as_draft(cx)));
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
    fn the_about_links_are_labelled_links(cx: &mut TestAppContext) {
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

            assert_eq!(role, Some(AccessRole::Link), "{id}");
            assert_eq!(name.as_deref(), Some(accessible), "{id}");
            assert!(accessible.starts_with(label), "{id}");
        }
    }
}

#[cfg(test)]
mod value_tests {
    use gpui::TestAppContext;
    use gpui::accesskit::Role as NodeRole;

    use super::*;
    use crate::live_probe::see_label_in;

    #[gpui::test]
    fn the_read_only_values_are_text_in_the_tree(cx: &mut TestAppContext) {
        cx.update(swarm_ui::init);
        let (_, cx) = cx.add_window_view(|_, _| gpui::Empty);

        for (label, expected) in [
            (version_label(), APP_VERSION),
            (credit_label(), ICONS_CREDIT),
            (changes_label(), ICONS_CHANGES),
        ] {
            let seen = see_label_in(label, cx);

            assert_eq!(seen.role, Some(NodeRole::Label), "{expected}");
            assert_eq!(seen.value.as_deref(), Some(expected));
        }
    }
}
