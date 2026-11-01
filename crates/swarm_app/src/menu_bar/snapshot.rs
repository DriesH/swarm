//! Saves the popover as `popover-*.png` in light and dark, then quits.
//! Run with `SWARM_MENU_BAR_SNAPSHOT_DIR=<dir> cargo run -p swarm_app --features snapshot`.
//!
//! The popover sits in a normal window with sample data. No status item is made and no
//! daemon is asked. As in the other snapshots, the window is minimized first because GPUI's
//! `render_to_image` throws while the display link runs.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{
    AnyWindowHandle, App, AppContext as _, AsyncApp, Bounds, WindowBounds, WindowOptions, px, size,
};
use swarm_protocol::UsageProblem;
use swarm_ui::AgentStatus;
use swarm_ui::theme::{self, Appearance};

use super::popover::{self, Popover};
use super::summary::Summary;
use crate::model::{FakeData, SAMPLE_NOW, TaskStatus};
use crate::workspace::Workspace;

const SETTLE: Duration = Duration::from_millis(700);

/// What the Stop all control shows.
#[derive(Clone, Copy)]
enum Stop {
    Button,
    Asking,
    Failed,
}

type Prepare = fn(&mut FakeData);

fn quiet(data: &mut FakeData) {
    for task in &mut data.tasks {
        task.status = TaskStatus::Idle;
    }
}

/// Two tasks need the user: one with a reason from its agent, one without.
fn needs_you(data: &mut FakeData) {
    quiet(data);

    let waiting = ["menu-bar-usage", "landing-page"];
    for (index, name) in waiting.into_iter().enumerate() {
        let Some(task) = data.tasks.iter_mut().find(|task| task.name == name) else {
            continue;
        };
        task.status = TaskStatus::NeedsYou;
        let id = task.id;

        if index == 0
            && let Some(agent) = data.agents.iter_mut().find(|agent| agent.task == id)
        {
            agent.status = AgentStatus::Blocked;
            agent.note = Some("permission \u{b7} Allow cargo test?".into());
        }
    }
}

/// Claude is rate limited for 24 minutes: its meters keep the old numbers.
fn rate_limited(data: &mut FakeData) {
    quiet(data);
    data.usage.claude.problem = Some(UsageProblem::RateLimited {
        retry_at: SAMPLE_NOW / 1_000 + 24 * 60,
    });
}

pub(crate) fn run(directory: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        if let Err(error) = std::fs::create_dir_all(&directory) {
            log::error!("cannot create {}: {error}", directory.display());
            cx.update(|cx| cx.quit());
            return;
        }

        let variants: [(&str, Prepare, Stop); 4] = [
            ("popover", quiet, Stop::Button),
            ("popover-needs-you", needs_you, Stop::Button),
            ("popover-confirm", quiet, Stop::Asking),
            ("popover-failed", rate_limited, Stop::Failed),
        ];

        for (appearance, theme_name) in [(Appearance::Light, "light"), (Appearance::Dark, "dark")] {
            cx.update(|cx| theme::set_appearance(appearance, cx));

            for (name, prepare, stop) in variants {
                let path = directory.join(format!("{name}-{theme_name}.png"));

                snap(&path, prepare, stop, cx).await;
            }
        }

        cx.update(|cx| cx.quit());
    })
    .detach();
}

async fn snap(path: &Path, prepare: Prepare, stop: Stop, cx: &mut AsyncApp) {
    let opened = cx.update(|cx| {
        let mut data = FakeData::sample();
        prepare(&mut data);
        let height = popover::height(&Summary::of(&data, SAMPLE_NOW / 1_000));

        let bounds = Bounds::centered(None, size(px(popover::WIDTH), px(height)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            ..Default::default()
        };

        cx.open_window(options, move |window, cx| {
            let workspace = cx.new(|cx| Workspace::with_sample(window, cx));
            workspace.update(cx, |workspace, _| prepare(&mut workspace.data));

            cx.new(|cx| {
                let mut popover = Popover::new(workspace, false, window, cx);
                match stop {
                    Stop::Button => {}
                    Stop::Asking => popover.ask_for_snapshot(cx),
                    Stop::Failed => popover.fail_for_snapshot(cx),
                }

                popover
            })
        })
    });

    let window: AnyWindowHandle = match opened {
        Ok(handle) => handle.into(),
        Err(error) => {
            log::error!("cannot open window: {error:#}");
            return;
        }
    };

    cx.background_executor().timer(SETTLE).await;

    for _ in 0..2 {
        if let Err(error) = window.update(cx, |_, window, cx| window.draw(cx).clear(cx)) {
            log::error!("cannot draw: {error:#}");
        }

        cx.background_executor().timer(SETTLE).await;
    }

    if let Err(error) = window.update(cx, |_, window, _| window.minimize_window()) {
        log::error!("cannot minimize: {error:#}");
    }

    cx.background_executor().timer(SETTLE).await;

    match window.update(cx, |_, window, _| window.render_to_image()) {
        Ok(Ok(image)) => match image.save(path) {
            Ok(()) => log::info!("saved {}", path.display()),
            Err(error) => log::error!("cannot save {}: {error}", path.display()),
        },
        Ok(Err(error)) => log::error!("cannot render {}: {error:#}", path.display()),
        Err(error) => log::error!("window closed: {error:#}"),
    }

    if let Err(error) = window.update(cx, |_, window, _| window.remove_window()) {
        log::error!("cannot close window: {error:#}");
    }
}
