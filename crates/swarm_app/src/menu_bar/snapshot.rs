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

/// The last Codex read failed: its meters keep the old numbers.
fn usage_failed(data: &mut FakeData) {
    quiet(data);
    data.usage.codex.problem = Some(UsageProblem::Failed(
        "the app server did not answer".to_owned(),
    ));
}

/// A full window, and numbers old enough to say the limit was likely reached.
fn usage_limits(data: &mut FakeData) {
    quiet(data);
    crate::snapshot::reach_limits(&mut data.usage);
}

/// The two wing frames of the buzzing bee on a strip of the menu bar, the way macOS tints a
/// template image: black on the light bar, white on the dark one. Saved as `egg-menu-bee-*.png`.
fn save_bee_frames(directory: &Path) {
    use super::icon::bee_png;
    use super::summary::BeeFrame;

    const ZOOM: u32 = 4;
    const GAP: u32 = 24;

    for (name, bar, ink) in [
        ("light", [236_u8, 236, 240], 0_u8),
        ("dark", [30, 30, 34], 255),
    ] {
        let frames: Vec<image::RgbaImage> = [BeeFrame::Open, BeeFrame::Flat]
            .into_iter()
            .filter_map(|frame| {
                let png = bee_png(false, frame).ok()?;

                image::load_from_memory(&png).ok().map(|bee| {
                    image::imageops::resize(
                        &bee.to_rgba8(),
                        bee.width() * ZOOM,
                        bee.height() * ZOOM,
                        image::imageops::FilterType::Triangle,
                    )
                })
            })
            .collect();

        let Some(first) = frames.first() else {
            log::error!("cannot draw the bee frames");
            return;
        };
        let (width, height) = (first.width(), first.height());
        let count = u32::try_from(frames.len()).unwrap_or(1);
        let mut strip = image::RgbaImage::from_pixel(
            GAP + count * (width + GAP),
            height + 2 * GAP,
            image::Rgba([bar[0], bar[1], bar[2], 255]),
        );

        for (index, frame) in (0..).zip(&frames) {
            let left = GAP + index * (width + GAP);

            for (x, y, pixel) in frame.enumerate_pixels() {
                let alpha = f32::from(pixel.0[3]) / 255.;
                let target = strip.get_pixel_mut(left + x, GAP + y);

                for channel in 0..3 {
                    let base = f32::from(target.0[channel]);
                    let blended = base + (f32::from(ink) - base) * alpha;
                    #[expect(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "a blend of two bytes stays between 0 and 255"
                    )]
                    let byte = blended.round() as u8;
                    target.0[channel] = byte;
                }
            }
        }

        let path = directory.join(format!("egg-menu-bee-{name}.png"));
        match strip.save(&path) {
            Ok(()) => log::info!("saved {}", path.display()),
            Err(error) => log::error!("cannot save {}: {error}", path.display()),
        }
    }
}

pub(crate) fn run(directory: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        if let Err(error) = std::fs::create_dir_all(&directory) {
            log::error!("cannot create {}: {error}", directory.display());
            cx.update(|cx| cx.quit());
            return;
        }

        save_bee_frames(&directory);

        let variants: [(&str, Prepare, Stop); 5] = [
            ("popover", quiet, Stop::Button),
            ("popover-needs-you", needs_you, Stop::Button),
            ("popover-confirm", quiet, Stop::Asking),
            ("popover-failed", usage_failed, Stop::Failed),
            ("popover-usage-limits", usage_limits, Stop::Button),
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
