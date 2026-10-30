//! Saves the main window as `workspace-light.png`, `workspace-dark.png` and the other
//! `workspace-*.png` shots, then quits.
//! Run with `SWARM_APP_SNAPSHOT_DIR=<dir> cargo run -p swarm_app --features snapshot`.
//!
//! GPUI's `render_to_image` throws on macOS 14+ while the window's display link
//! runs, so the window is minimized first; the last frame stays available.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{AnyWindowHandle, App, AsyncApp, Context, Window};
use swarm_ui::theme::{self, Appearance};

use crate::workspace::Workspace;

type OpenOverlay = fn(&mut Workspace, &mut Window, &mut Context<Workspace>);

fn open_island(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    workspace.new_user_terminal(window, cx);
}

fn open_start(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    workspace.open_start_task(cx);
}

fn show_empty(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    workspace.use_projects(&[], cx);
}

fn show_unavailable(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    workspace.use_projects(&[], cx);
    workspace.daemon_unavailable(
        "the daemon speaks protocol version 1; stop it with `pkill -x swarm-daemon`".to_owned(),
        cx,
    );
}

const SETTLE: Duration = Duration::from_millis(700);

pub(crate) fn run(directory: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        if let Err(error) = std::fs::create_dir_all(&directory) {
            log::error!("cannot create {}: {error}", directory.display());
            cx.update(|cx| cx.quit());
            return;
        }

        for (appearance, name, open) in [
            (Appearance::Light, "workspace-light.png", None),
            (Appearance::Dark, "workspace-dark.png", None),
            (
                Appearance::Light,
                "workspace-island-light.png",
                Some(open_island as OpenOverlay),
            ),
            (
                Appearance::Dark,
                "workspace-island-dark.png",
                Some(open_island),
            ),
            (
                Appearance::Light,
                "workspace-start-light.png",
                Some(open_start),
            ),
            (
                Appearance::Dark,
                "workspace-start-dark.png",
                Some(open_start),
            ),
            (
                Appearance::Light,
                "workspace-palette-light.png",
                Some(Workspace::toggle_palette as OpenOverlay),
            ),
            (
                Appearance::Dark,
                "workspace-palette-dark.png",
                Some(Workspace::toggle_palette),
            ),
            (
                Appearance::Light,
                "workspace-new-task-light.png",
                Some(Workspace::open_new_task),
            ),
            (
                Appearance::Dark,
                "workspace-new-task-dark.png",
                Some(Workspace::open_new_task),
            ),
            (
                Appearance::Light,
                "workspace-empty-light.png",
                Some(show_empty),
            ),
            (
                Appearance::Dark,
                "workspace-empty-dark.png",
                Some(show_empty),
            ),
            (
                Appearance::Light,
                "workspace-unavailable-light.png",
                Some(show_unavailable),
            ),
            (
                Appearance::Dark,
                "workspace-unavailable-dark.png",
                Some(show_unavailable),
            ),
        ] {
            cx.update(|cx| theme::set_appearance(appearance, cx));
            snap(&directory.join(name), open, cx).await;
        }

        cx.update(|cx| cx.quit());
    })
    .detach();
}

async fn snap(path: &Path, open: Option<OpenOverlay>, cx: &mut AsyncApp) {
    let opened = cx.update(crate::open_workspace);

    // AnyWindowHandle: the typed handle leases the root view.
    let window: AnyWindowHandle = match opened {
        Ok(handle) => handle.into(),
        Err(error) => {
            log::error!("cannot open window: {error:#}");
            return;
        }
    };

    cx.background_executor().timer(SETTLE).await;

    if let Some(open) = open {
        let opened = window.update(cx, |root, window, cx| {
            if let Ok(workspace) = root.downcast::<Workspace>() {
                workspace.update(cx, |workspace, cx| open(workspace, window, cx));
            } else {
                log::error!("root is not a Workspace");
            }
        });

        if let Err(error) = opened {
            log::error!("cannot open the overlay: {error:#}");
        }

        // The window is not on screen, so no frame is drawn unless we draw one.
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
