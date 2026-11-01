//! Settings › General: the system check.

use gpui::{AnyElement, FontWeight, Role, div, px};
use swarm_ui::prelude::*;

use super::SettingsWindow;
use super::pages::page_title;
use crate::system_check::SystemCheckView;

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
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle, px, size};
    use swarm_protocol::{Client, Event, Request, ToolState};

    use super::super::{Page, SettingsWindow};
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
}
