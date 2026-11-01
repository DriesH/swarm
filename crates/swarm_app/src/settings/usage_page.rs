//! Settings › Usage: every window of both agent kinds, as the menu bar popover shows them.
//! It reads the workspace's usage, so it changes as the daemon reports.

use std::time::Duration;

use gpui::{Context, Entity, Render, Task, WeakEntity, Window, div, px};
use swarm_protocol::{Usage, UsageProblem};
use swarm_ui::prelude::*;

use super::SettingsState;
use super::pages::page_title;
use crate::menu_bar::{usage_section, usage_sections};
use crate::model::{FakeData, SAMPLE_NOW};

/// How often the "resets in" notes are worked out again.
const CLOCK: Duration = Duration::from_secs(30);

pub(super) struct UsagePage {
    state: Entity<SettingsState>,
    /// Shown while no workspace opened the window.
    sample: Usage,
    _clock: Task<()>,
}

impl UsagePage {
    pub(super) fn new(state: Entity<SettingsState>, cx: &mut Context<Self>) -> Self {
        let clock = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CLOCK).await;

                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        });

        Self {
            state,
            sample: sample_usage(),
            _clock: clock,
        }
    }

    /// The usage to show and the time to count from, in seconds since the Unix epoch.
    fn reading(&self, cx: &App) -> (Usage, u64) {
        let workspace = self
            .state
            .read(cx)
            .workspace
            .as_ref()
            .and_then(WeakEntity::upgrade);

        match workspace {
            Some(workspace) => {
                let workspace = workspace.read(cx);

                (workspace.data.usage.clone(), workspace.now_ms() / 1_000)
            }
            None => (self.sample.clone(), SAMPLE_NOW / 1_000),
        }
    }
}

/// The sample usage, with Codex rate limited, so every part of a section shows.
fn sample_usage() -> Usage {
    let mut usage = FakeData::sample().usage;
    usage.codex.problem = Some(UsageProblem::RateLimited {
        retry_at: SAMPLE_NOW / 1_000 + 24 * 60,
    });

    usage
}

impl Render for UsagePage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (usage, now) = self.reading(cx);
        let sections = usage_sections(&usage, now);

        let body = if sections.is_empty() {
            div()
                .id("usage-empty")
                .debug_selector(|| "usage-empty".to_owned())
                .role(gpui::Role::Status)
                .aria_label("No usage data")
                .child(
                    Label::new("No usage data")
                        .size(LabelSize::Small)
                        .color(Color::Faint),
                )
                .into_any_element()
        } else {
            sections
                .iter()
                .fold(
                    Panel::new().accessible("usage-sections", "Subscription usage"),
                    |panel, section| panel.row(div().p(px(14.)).child(usage_section(section))),
                )
                .into_any_element()
        };

        v_flex().gap(px(26.)).child(page_title("Usage")).child(body)
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext, WindowHandle, px, size};
    use swarm_protocol::{SubscriptionUsage, UsageWindow};

    use super::*;
    use crate::actions::OpenSettings;
    use crate::daemon::tests::{redraw, setup};
    use crate::settings::{Page, SettingsWindow};
    use crate::workspace::Workspace;

    fn window_of(percent: u8) -> UsageWindow {
        UsageWindow {
            label: "5h".to_owned(),
            used_percent: percent,
            resets_at: None,
        }
    }

    fn reading(percent: u8, problem: Option<UsageProblem>) -> SubscriptionUsage {
        SubscriptionUsage {
            windows: vec![window_of(percent)],
            plan: None,
            read_at: None,
            problem,
        }
    }

    fn open_page(
        workspace_usage: Option<Usage>,
        cx: &mut TestAppContext,
    ) -> (
        Entity<Workspace>,
        WindowHandle<SettingsWindow>,
        VisualTestContext,
    ) {
        let (workspace, main) = setup(cx);
        if let Some(usage) = workspace_usage {
            workspace.update(main, |workspace, cx| workspace.usage_changed(usage, cx));
        }

        main.dispatch_action(OpenSettings);
        let window = main
            .windows()
            .into_iter()
            .find_map(|window| window.downcast::<SettingsWindow>())
            .expect("the settings window opened");
        window
            .update(main, |settings, _, cx| {
                settings.select_page(Page::Usage, cx);
            })
            .expect("the window is open");

        let mut settings = VisualTestContext::from_window(window.into(), main);
        settings.simulate_resize(size(px(1280.), px(900.)));
        redraw(&mut settings);

        (workspace, window, settings)
    }

    fn shown(selector: &'static str, cx: &mut VisualTestContext) -> bool {
        redraw(cx);

        cx.debug_bounds(selector).is_some()
    }

    #[gpui::test]
    fn both_agent_kinds_show_with_their_windows(cx: &mut TestAppContext) {
        let (_workspace, _window, mut page) = open_page(None, cx);

        assert!(shown("popover-usage-claude", &mut page));
        assert!(shown("popover-usage-codex", &mut page));
        assert!(!shown("popover-usage-claude-problem", &mut page));
    }

    #[gpui::test]
    fn a_problem_shows_as_a_line_under_its_agent_kind(cx: &mut TestAppContext) {
        let usage = Usage {
            claude: reading(42, None),
            codex: reading(7, Some(UsageProblem::NotLoggedIn)),
        };
        let (_workspace, _window, mut page) = open_page(Some(usage), cx);

        assert!(shown("popover-usage-claude", &mut page));
        assert!(shown("popover-usage-codex", &mut page));
        assert!(shown("popover-usage-codex-problem", &mut page));
        assert!(!shown("popover-usage-claude-problem", &mut page));
    }

    #[gpui::test]
    fn the_page_follows_the_usage_the_daemon_reports(cx: &mut TestAppContext) {
        let usage = Usage {
            claude: reading(42, None),
            codex: reading(7, None),
        };
        let (workspace, _window, mut page) = open_page(Some(usage.clone()), cx);
        let before = shown("popover-usage-claude-problem", &mut page);

        let limited = Usage {
            claude: reading(
                42,
                Some(UsageProblem::RateLimited {
                    retry_at: u64::MAX / 2,
                }),
            ),
            ..usage
        };
        workspace.update(&mut page, |workspace, cx| {
            workspace.usage_changed(limited, cx);
        });
        let after = shown("popover-usage-claude-problem", &mut page);

        assert!(!before);
        assert!(after);
    }

    #[gpui::test]
    fn a_kind_that_is_not_installed_has_no_section(cx: &mut TestAppContext) {
        let usage = Usage {
            claude: reading(42, None),
            codex: SubscriptionUsage {
                problem: Some(UsageProblem::NotInstalled),
                ..SubscriptionUsage::default()
            },
        };
        let (_workspace, _window, mut page) = open_page(Some(usage), cx);

        assert!(shown("popover-usage-claude", &mut page));
        assert!(!shown("popover-usage-codex", &mut page));
    }

    #[gpui::test]
    fn no_usage_at_all_says_so(cx: &mut TestAppContext) {
        let (_workspace, _window, mut page) = open_page(Some(Usage::default()), cx);

        assert!(shown("usage-empty", &mut page));
        assert!(!shown("popover-usage-claude", &mut page));
    }
}
