//! `swarm`: the Swarm app.

mod actions;
mod daemon;
mod model;
mod new_task;
mod palette;
mod panels;
mod settings;
mod sidebar;
mod status_bar;
mod swarm_view;
mod tab_bar;
mod views;
mod window_drag;
mod workspace;

#[cfg(feature = "frame-stats")]
mod frame_stats;
#[cfg(feature = "snapshot")]
mod snapshot;

use gpui::{App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};

use crate::workspace::Workspace;

fn window_options(cx: &App) -> WindowOptions {
    let bounds = Bounds::centered(None, size(px(1600.), px(1000.)), cx);

    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(1100.), px(700.))),
        titlebar: Some(TitlebarOptions {
            title: Some("Swarm".into()),
            appears_transparent: true,
            ..Default::default()
        }),
        app_owns_titlebar_drag: true,
        ..Default::default()
    }
}

fn open_workspace(cx: &mut App) -> anyhow::Result<gpui::WindowHandle<Workspace>> {
    let options = window_options(cx);

    // Snapshot and frame-stats runs keep the sample data and never connect.
    let sample = cfg!(any(feature = "snapshot", feature = "frame-stats"));

    let handle = cx.open_window(options, |window, cx| {
        cx.new(|cx| {
            if sample {
                Workspace::with_sample(window, cx)
            } else {
                Workspace::new(window, cx)
            }
        })
    })?;

    if !sample {
        handle.update(cx, |workspace, window, cx| {
            workspace.settings_path = swarm_settings::default_path();
            workspace.connect_daemon(window, cx);
        })?;
    }

    Ok(handle)
}

#[cfg(not(feature = "frame-stats"))]
fn open_main_window(cx: &mut App) {
    if let Err(error) = open_workspace(cx) {
        log::error!("could not open the main window: {error:#}");
        cx.quit();
        return;
    }

    cx.activate(true);
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    gpui_platform::application().run(|cx: &mut App| {
        swarm_ui::init(cx);
        actions::bind_keys(cx);

        #[cfg(feature = "snapshot")]
        if let Ok(directory) = std::env::var("SWARM_SETTINGS_SNAPSHOT_DIR") {
            settings::snapshot(directory.into(), cx);
            return;
        }

        #[cfg(feature = "snapshot")]
        if let Ok(directory) = std::env::var("SWARM_APP_SNAPSHOT_DIR") {
            snapshot::run(directory.into(), cx);
            return;
        }

        #[cfg(feature = "frame-stats")]
        frame_stats::run(cx);

        #[cfg(not(feature = "frame-stats"))]
        open_main_window(cx);
    });
}
