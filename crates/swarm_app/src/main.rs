//! `swarm`: the Swarm app.

// The frame-stats run replaces the normal start-up, so most of the app is never called.
#![cfg_attr(
    feature = "frame-stats",
    allow(dead_code, reason = "frame-stats replaces the normal start-up")
)]

mod actions;
mod announce;
mod app_menus;
mod close_task;
mod close_terminal;
mod daemon;
mod daemon_restart;
mod eggs;
mod file_tree;
mod lifecycle;
mod markdown;
mod menu_bar;
mod model;
mod new_task;
mod notifications;
mod palette;
mod panels;
mod plan_editor;
mod pull_request;
mod pull_request_panel;
mod remove_project;
mod review_comments;
mod review_model;
mod review_view;
mod settings;
mod sidebar;
mod status_bar;
mod swarm_view;
mod system_check;
mod tab_bar;
mod updates;
mod usage_pace;
mod usage_text;
mod views;
mod welcome;
mod window_drag;
mod workspace;

#[cfg(test)]
mod live_probe;

#[cfg(any(feature = "snapshot", feature = "frame-stats"))]
mod demo;
#[cfg(feature = "frame-stats")]
mod frame_stats;
#[cfg(feature = "snapshot")]
mod snapshot;

use gpui::{App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};

use crate::workspace::{Workspace, traffic_light_position};

fn window_options(cx: &App) -> WindowOptions {
    let bounds = Bounds::centered(None, size(px(1600.), px(1000.)), cx);

    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(1100.), px(700.))),
        titlebar: Some(TitlebarOptions {
            title: Some("Swarm".into()),
            appears_transparent: true,
            traffic_light_position: Some(traffic_light_position()),
        }),
        app_owns_titlebar_drag: true,
        ..Default::default()
    }
}

fn open_workspace(cx: &mut App) -> anyhow::Result<gpui::WindowHandle<Workspace>> {
    let options = window_options(cx);

    // Snapshot and frame-stats runs keep the sample data and never connect.
    let sample = cfg!(any(feature = "snapshot", feature = "frame-stats"));

    // Loaded before the first frame: the welcome screen reads its flag from the settings.
    if !sample {
        settings::load(swarm_settings::default_path(), cx);
    }

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
        handle.update(cx, |workspace, _, cx| {
            workspace.settings_path = swarm_settings::default_path();
            workspace.connect_daemon(cx);
        })?;
    }

    Ok(handle)
}

#[cfg(not(feature = "frame-stats"))]
fn open_main_window(cx: &mut App) {
    if let Err(error) = lifecycle::start(cx) {
        log::error!("could not open the main window: {error:#}");
        cx.quit();
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let application = gpui_platform::application();
    application.on_reopen(lifecycle::reopened);

    application.run(|cx: &mut App| {
        swarm_ui::init(cx);
        actions::bind_keys(cx);

        #[cfg(feature = "snapshot")]
        if let Ok(directory) = std::env::var("SWARM_SETTINGS_SNAPSHOT_DIR") {
            settings::snapshot(directory.into(), cx);
            return;
        }

        #[cfg(feature = "snapshot")]
        if let Ok(directory) = std::env::var("SWARM_MENU_BAR_SNAPSHOT_DIR") {
            menu_bar::snapshot::run(directory.into(), cx);
            return;
        }

        #[cfg(feature = "snapshot")]
        if let Ok(directory) = std::env::var("SWARM_APP_SNAPSHOT_DIR") {
            snapshot::run(directory.into(), cx);
            return;
        }

        app_menus::install(cx);

        #[cfg(feature = "frame-stats")]
        frame_stats::run(cx);

        #[cfg(not(feature = "frame-stats"))]
        open_main_window(cx);
    });
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;
    use crate::workspace::{TITLE_BAR_HEIGHT, TRAFFIC_LIGHT_SIZE};

    #[gpui::test]
    fn the_traffic_lights_are_centered_on_the_title_bar(cx: &mut TestAppContext) {
        let options = cx.update(|cx| window_options(cx));
        let position = options
            .titlebar
            .and_then(|titlebar| titlebar.traffic_light_position)
            .expect("the window places the traffic lights");

        assert_eq!(position.y + TRAFFIC_LIGHT_SIZE / 2., TITLE_BAR_HEIGHT / 2.);
        assert_eq!(position.x, px(9.));
    }
}
