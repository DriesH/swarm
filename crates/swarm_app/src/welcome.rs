//! The welcome screen: shown on the first launch, until the user skips it or opens a project.

use gpui::{
    AnyElement, AppContext as _, Context, Entity, FocusHandle, FontWeight, Role, Subscription,
    WeakEntity, Window, div, px,
};
use swarm_ui::prelude::*;

use crate::system_check::SystemCheckView;
use crate::window_drag::drag_region;
use crate::workspace::Workspace;

/// The column of the logo, the check and the buttons.
const COLUMN_WIDTH: f32 = 520.;
/// The height of the strip at the top that moves the window.
const DRAG_HEIGHT: f32 = 36.;
const BEE_DEGREES: f32 = -20.;

pub(crate) struct Welcome {
    workspace: WeakEntity<Workspace>,
    check: Entity<SystemCheckView>,
    honeycomb: Entity<Honeycomb>,
    skip_focus: FocusHandle,
    check_again_focus: FocusHandle,
    open_focus: FocusHandle,
    _activation: Subscription,
}

impl Welcome {
    /// The check starts after the current update, as it reads the workspace.
    pub(crate) fn new(
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let open_focus = cx.focus_handle().tab_stop(true);
        let skip_focus = cx.focus_handle().tab_stop(true);
        let check_again_focus = cx.focus_handle().tab_stop(true);

        let check = cx.new({
            let workspace = workspace.clone();
            let fallback = open_focus.clone();
            let window = &mut *window;

            move |cx| {
                let mut check = SystemCheckView::new(Some(workspace), true, window, cx);
                check.set_fallback_focus(fallback);

                check
            }
        });
        cx.defer({
            let check = check.clone();

            move |cx| check.update(cx, SystemCheckView::refresh)
        });

        // The user may come back after an install the terminal could not wait for, such as
        // Apple's installer.
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.check_again(cx);
            }
        });

        Self {
            workspace,
            check,
            honeycomb: cx.new(|_| Honeycomb::new()),
            skip_focus,
            check_again_focus,
            open_focus,
            _activation: activation,
        }
    }

    fn check_again(&self, cx: &mut Context<Self>) {
        self.check.update(cx, SystemCheckView::refresh);
    }

    #[cfg(any(test, feature = "snapshot"))]
    pub(crate) fn check(&self) -> &Entity<SystemCheckView> {
        &self.check
    }

    /// Ends the welcome and, with `open`, starts the add-project flow. It waits until the
    /// click is over: ending the welcome drops this view.
    fn finish(&self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();

        window.defer(cx, move |window, cx| {
            let finished = workspace.update(cx, |workspace, cx| {
                workspace.finish_welcome(cx);

                if open {
                    Workspace::pick_project(window, cx);
                }
            });

            if let Err(error) = finished {
                log::debug!("the window closed before the welcome ended: {error}");
            }
        });
    }

    fn header() -> AnyElement {
        v_flex()
            .items_center()
            .gap(px(12.))
            .child(BrandMark::new().size(px(48.)))
            .child(
                div()
                    .id("welcome-title")
                    .role(Role::Heading)
                    .aria_level(1)
                    .aria_label("Welcome to Swarm")
                    .child(
                        Label::new("Welcome to Swarm")
                            .size(LabelSize::Title)
                            .weight(FontWeight::SEMIBOLD),
                    ),
            )
            .into_any_element()
    }
}

impl Render for Welcome {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let ground = colors.ground;

        let drag = drag_region("welcome-drag", window, cx)
            .debug_selector(|| "welcome-drag".to_owned())
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(DRAG_HEIGHT));

        let buttons = h_flex()
            .gap(px(8.))
            .child(
                Button::new("welcome-skip", "Skip for now")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Medium)
                    .track_focus(&self.skip_focus)
                    .debug_selector(|| "welcome-skip".to_owned())
                    .on_click(cx.listener(|this, _, window, cx| this.finish(false, window, cx))),
            )
            .child(
                Button::new("welcome-check-again", "Check again")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Medium)
                    .track_focus(&self.check_again_focus)
                    .debug_selector(|| "welcome-check-again".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.check_again(cx))),
            )
            .child(div().flex_1())
            .child(
                Button::new("welcome-open", "Open a project")
                    .variant(ButtonVariant::Primary)
                    .size(ButtonSize::Medium)
                    .track_focus(&self.open_focus)
                    .debug_selector(|| "welcome-open".to_owned())
                    .on_click(cx.listener(|this, _, window, cx| this.finish(true, window, cx))),
            );

        let eggs = crate::settings::easter_eggs(cx);

        let column = v_flex()
            .relative()
            .w(px(COLUMN_WIDTH))
            .flex_none()
            .gap(px(22.))
            .child(Self::header())
            .child(self.check.clone())
            .child(buttons)
            .when(eggs, |column| {
                column.child(
                    div()
                        .debug_selector(|| "welcome-bee".to_owned())
                        .absolute()
                        .left(px(-56.))
                        .bottom(px(64.))
                        .child(
                            Icon::new(IconName::Bee)
                                .size(IconSize::Custom(px(22.)))
                                .color(Color::Faint)
                                .rotate(BEE_DEGREES),
                        ),
                )
            });

        div()
            .id("welcome")
            .role(Role::Main)
            .aria_label("Welcome")
            .debug_selector(|| "welcome".to_owned())
            .relative()
            .size_full()
            .bg(ground)
            .when(eggs, |welcome| {
                welcome.child(
                    div()
                        .debug_selector(|| "honeycomb".to_owned())
                        .absolute()
                        .size_full()
                        .child(self.honeycomb.clone().cached(Honeycomb::cache_style())),
                )
            })
            .child(
                div()
                    .id("welcome-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .min_h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .py(px(DRAG_HEIGHT))
                            .child(column),
                    ),
            )
            .child(drag)
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use gpui::TestAppContext;
    use swarm_protocol::ToolState;

    use super::*;
    use crate::daemon::tests::{redraw, setup, use_projects};
    use crate::notifications::harness::Recorder;
    use crate::notifications::{self, Delivery, Permission};
    use crate::system_check::Tool;
    use crate::system_check::harness::{
        Fixture, report_with, tool_check, welcome_fixture, welcome_fixture_at,
    };

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("swarm-welcome-{name}-{}.toml", std::process::id()))
    }

    fn welcome_of(fixture: &Fixture<'_>) -> Entity<Welcome> {
        fixture
            .workspace
            .read_with(fixture.cx, |workspace, _| workspace.welcome.clone())
            .expect("the welcome screen shows")
    }

    fn action_focus(fixture: &Fixture<'_>, tool: Tool) -> FocusHandle {
        fixture
            .check()
            .read_with(fixture.cx, |check, _| check.action_focus(tool))
            .expect("a handle for every tool")
    }

    fn is_focused(handle: &FocusHandle, fixture: &mut Fixture<'_>) -> bool {
        fixture.cx.update(|window, _| handle.is_focused(window))
    }

    fn broken_agents() -> swarm_protocol::SystemCheck {
        report_with(&[
            (
                Tool::ClaudeCode,
                tool_check(ToolState::Missing, None, &["claude-install"]),
            ),
            (
                Tool::Codex,
                tool_check(ToolState::SignedOut, Some("0.154.0"), &["codex"]),
            ),
        ])
    }

    #[gpui::test]
    fn the_welcome_shows_on_the_first_launch_instead_of_the_workspace(cx: &mut TestAppContext) {
        let fixture = welcome_fixture(report_with(&[]), Permission::Allowed, cx);

        let mut shown = |selector| fixture.cx.debug_bounds(selector).is_some();

        assert!(shown("welcome"));
        assert!(shown("welcome-skip"));
        assert!(shown("welcome-open"));
        assert!(shown("system-check"));
        assert!(!shown("add-project"), "the sidebar is still there");
        assert!(
            !shown("empty-add-project"),
            "the empty state is still there"
        );
    }

    #[gpui::test]
    fn the_welcome_honeycomb_and_bee_are_eggs(cx: &mut TestAppContext) {
        let mut fixture = welcome_fixture(report_with(&[]), Permission::Allowed, cx);
        let eggs = |fixture: &mut Fixture<'_>| {
            (
                fixture.cx.debug_bounds("honeycomb").is_some(),
                fixture.cx.debug_bounds("welcome-bee").is_some(),
            )
        };

        let with_eggs = eggs(&mut fixture);
        fixture.cx.update(|_, cx| cx.set_reduce_motion(true));
        redraw(fixture.cx);
        let still = eggs(&mut fixture);

        fixture
            .cx
            .update(|_, cx| crate::settings::change_appearance(|a| a.easter_eggs = false, cx));
        redraw(fixture.cx);
        let without = eggs(&mut fixture);

        assert_eq!(with_eggs, (true, true));
        assert_eq!(still, (true, true), "both are still pictures");
        assert_eq!(without, (false, false));
    }

    #[gpui::test]
    fn the_welcome_waits_while_the_daemon_connects(cx: &mut TestAppContext) {
        cx.update(|cx| {
            swarm_ui::init(cx);
            crate::actions::bind_keys(cx);
        });
        let (workspace, cx) = cx.add_window_view(Workspace::new);
        cx.update(|_, cx| {
            crate::settings::state(cx);
        });
        redraw(cx);

        let shows = workspace.read_with(cx, Workspace::shows_welcome);

        assert!(
            !shows,
            "an app with projects looks like this until the daemon answers"
        );
        assert!(cx.debug_bounds("welcome").is_none());
        assert!(cx.debug_bounds("empty-add-project").is_some());
    }

    #[gpui::test]
    fn the_welcome_does_not_show_when_there_are_projects(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.update(|_, cx| {
            crate::settings::state(cx);
        });
        redraw(cx);

        let shows = workspace.read_with(cx, Workspace::shows_welcome);

        assert!(!shows);
        assert!(cx.debug_bounds("welcome").is_none());
        assert!(cx.debug_bounds("add-project").is_some());
    }

    #[gpui::test]
    fn the_welcome_does_not_show_before_the_settings_are_read(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        use_projects(&workspace, &[], cx);
        redraw(cx);

        let shows = workspace.read_with(cx, Workspace::shows_welcome);

        assert!(!shows);
        assert!(cx.debug_bounds("empty-add-project").is_some());
    }

    #[gpui::test]
    fn skip_sets_the_flag_and_shows_the_empty_workspace(cx: &mut TestAppContext) {
        let mut fixture = welcome_fixture(report_with(&[]), Permission::Allowed, cx);

        fixture.click("welcome-skip");
        fixture.cx.run_until_parked();
        redraw(fixture.cx);

        let pending = fixture
            .cx
            .update(|_, cx| crate::settings::welcome_pending(cx));
        let welcome = fixture
            .workspace
            .read_with(fixture.cx, |workspace, _| workspace.welcome.is_some());
        assert!(!pending, "the flag is not set");
        assert!(!welcome);
        assert!(fixture.cx.debug_bounds("welcome").is_none());
        assert!(fixture.cx.debug_bounds("empty-add-project").is_some());
        assert!(
            !fixture.cx.did_prompt_for_paths(),
            "skip asked for a folder"
        );
    }

    #[gpui::test]
    fn open_a_project_sets_the_flag_and_starts_the_add_project_flow(cx: &mut TestAppContext) {
        let mut fixture = welcome_fixture(report_with(&[]), Permission::Allowed, cx);

        fixture.click("welcome-open");
        fixture.cx.run_until_parked();
        redraw(fixture.cx);

        let pending = fixture
            .cx
            .update(|_, cx| crate::settings::welcome_pending(cx));
        assert!(!pending, "the flag is not set");
        assert!(fixture.cx.did_prompt_for_paths(), "no folder picker opened");
        assert!(fixture.cx.debug_bounds("welcome").is_none());
    }

    #[gpui::test]
    fn the_welcome_never_shows_again_once_the_flag_is_set(cx: &mut TestAppContext) {
        let mut fixture = welcome_fixture(report_with(&[]), Permission::Allowed, cx);
        fixture.click("welcome-skip");
        fixture.cx.run_until_parked();

        // The user removes every project later; a window comes and goes.
        use_projects(&fixture.workspace, &[], fixture.cx);
        fixture
            .workspace
            .update(fixture.cx, Workspace::window_closed);
        redraw(fixture.cx);

        let shows = fixture
            .workspace
            .read_with(fixture.cx, Workspace::shows_welcome);
        assert!(!shows);
        assert!(fixture.cx.debug_bounds("welcome").is_none());
    }

    #[gpui::test]
    fn the_flag_is_saved_in_the_settings_file(cx: &mut TestAppContext) {
        let path = temp_path("saved");
        std::fs::remove_file(&path).ok();
        let mut fixture = welcome_fixture_at(
            Some(path.clone()),
            report_with(&[]),
            Permission::Allowed,
            cx,
        );

        fixture.click("welcome-skip");
        fixture.cx.run_until_parked();

        let saved = swarm_settings::load(&path).expect("the settings file is written");
        std::fs::remove_file(&path).ok();
        assert!(saved.welcome_done);
    }

    #[gpui::test]
    fn a_flag_in_the_settings_file_keeps_the_welcome_away(cx: &mut TestAppContext) {
        let path = temp_path("kept");
        let settings = swarm_settings::Settings {
            welcome_done: true,
            ..Default::default()
        };
        swarm_settings::save(&path, &settings).expect("write the settings");
        let (workspace, cx) = setup(cx);
        cx.update(|_, cx| crate::settings::load(Some(path.clone()), cx));
        use_projects(&workspace, &[], cx);
        redraw(cx);
        std::fs::remove_file(&path).ok();

        let shows = workspace.read_with(cx, Workspace::shows_welcome);

        assert!(!shows);
        assert!(cx.debug_bounds("empty-add-project").is_some());
    }

    #[gpui::test]
    fn a_settings_file_that_cannot_be_read_keeps_the_welcome_away(cx: &mut TestAppContext) {
        let path = temp_path("broken");
        std::fs::write(&path, "welcome_done = [").expect("write the file");
        let (workspace, cx) = setup(cx);
        cx.update(|_, cx| crate::settings::load(Some(path.clone()), cx));
        use_projects(&workspace, &[], cx);
        redraw(cx);
        std::fs::remove_file(&path).ok();

        let shows = workspace.read_with(cx, Workspace::shows_welcome);

        assert!(
            !shows,
            "the flag could not be saved, so the welcome would come back"
        );
        assert!(cx.debug_bounds("empty-add-project").is_some());
    }

    #[gpui::test]
    fn the_first_action_that_is_not_ready_takes_the_focus(cx: &mut TestAppContext) {
        let mut fixture = welcome_fixture(broken_agents(), Permission::NotAsked, cx);

        let first = action_focus(&fixture, Tool::ClaudeCode);
        let others = [Tool::Codex, Tool::Notifications].map(|tool| action_focus(&fixture, tool));

        assert!(is_focused(&first, &mut fixture));
        for other in &others {
            assert!(!is_focused(other, &mut fixture));
        }
    }

    #[gpui::test]
    fn when_everything_is_ready_the_open_button_takes_the_focus(cx: &mut TestAppContext) {
        let mut fixture = welcome_fixture(report_with(&[]), Permission::Allowed, cx);

        let open =
            welcome_of(&fixture).read_with(fixture.cx, |welcome, _| welcome.open_focus.clone());

        assert!(is_focused(&open, &mut fixture));
    }

    #[gpui::test]
    fn a_dimmed_action_does_not_take_the_focus(cx: &mut TestAppContext) {
        let report = report_with(&[
            (Tool::GithubCli, tool_check(ToolState::Missing, None, &[])),
            (
                Tool::GithubAccount,
                tool_check(ToolState::Missing, None, &["gh", "auth", "login"]),
            ),
        ]);
        let mut fixture = welcome_fixture(report, Permission::Allowed, cx);

        let account = action_focus(&fixture, Tool::GithubAccount);
        let open =
            welcome_of(&fixture).read_with(fixture.cx, |welcome, _| welcome.open_focus.clone());

        assert!(!is_focused(&account, &mut fixture));
        assert!(is_focused(&open, &mut fixture));
    }

    #[gpui::test]
    fn tab_moves_through_the_row_buttons_and_then_the_three_buttons(cx: &mut TestAppContext) {
        let mut fixture = welcome_fixture(broken_agents(), Permission::NotAsked, cx);
        let welcome = welcome_of(&fixture);
        let (skip, check_again, open) = welcome.read_with(fixture.cx, |welcome, _| {
            (
                welcome.skip_focus.clone(),
                welcome.check_again_focus.clone(),
                welcome.open_focus.clone(),
            )
        });
        let order = [
            action_focus(&fixture, Tool::ClaudeCode),
            action_focus(&fixture, Tool::Codex),
            action_focus(&fixture, Tool::Notifications),
            skip,
            check_again,
            open,
        ];

        let mut focused = Vec::new();
        for _ in 0..order.len() {
            focused.push(
                order
                    .iter()
                    .position(|handle| is_focused(handle, &mut fixture)),
            );
            fixture.cx.simulate_keystrokes("tab");
            redraw(fixture.cx);
        }

        assert_eq!(
            focused,
            [Some(0), Some(1), Some(2), Some(3), Some(4), Some(5)]
        );
    }

    #[gpui::test]
    fn check_again_asks_the_daemon_again(cx: &mut TestAppContext) {
        let mut fixture = welcome_fixture(broken_agents(), Permission::Allowed, cx);
        let check = fixture.check();
        let before = fixture.seen.checks();

        *fixture.report.lock().expect("lock the report") = report_with(&[]);
        fixture.click("welcome-check-again");
        let asked = fixture.wait(|seen, _| seen.checks() == before + 1);
        let ready = fixture.wait(|_, cx| check.read_with(cx, |check, _| check.count() == (6, 6)));

        assert!(asked, "Check again did not ask the daemon");
        assert!(ready, "the new answer did not reach the rows");
    }

    #[gpui::test]
    fn coming_back_to_the_window_checks_again(cx: &mut TestAppContext) {
        let mut fixture = welcome_fixture(broken_agents(), Permission::Allowed, cx);
        let before = fixture.seen.checks();

        fixture.cx.deactivate_window();
        fixture.cx.update(|window, _| window.activate_window());
        let asked = fixture.wait(|seen, _| seen.checks() == before + 1);

        assert!(asked, "the check was not asked again");
    }

    #[gpui::test]
    fn the_check_asks_again_when_the_daemon_connects_after_the_welcome_showed(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = setup(cx);
        let recorder = Recorder::new(Permission::Allowed);
        cx.update(|_, cx| {
            crate::settings::state(cx);
            notifications::use_delivery(Rc::clone(&recorder) as Rc<dyn Delivery>, cx);
        });
        use_projects(&workspace, &[], cx);
        redraw(cx);
        cx.run_until_parked();
        let check = workspace
            .read_with(cx, |workspace, cx| {
                workspace
                    .welcome
                    .as_ref()
                    .map(|welcome| welcome.read(cx).check().clone())
            })
            .expect("the welcome shows");
        let before = check.read_with(cx, |check, _| check.count());

        cx.executor().allow_parking();
        let socket = crate::daemon::tests::fake_daemon_replying(|_| None);
        let client = swarm_protocol::Client::connect(&socket).expect("connect to the fake daemon");
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client, &[], window, cx);
        });
        let answered = crate::daemon::tests::wait_until(cx, |cx| {
            redraw(cx);

            check.read_with(cx, |check, _| check.count() == (6, 6))
        });

        assert_eq!(before.0, 1, "only the notifications were ready");
        assert!(answered, "the check never asked the daemon that came later");
    }
}
