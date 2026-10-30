//! Scripted runs (`SPIKE_SCRIPT=file`), so the spike is testable without
//! clicking. One command per line, `#` starts a comment:
//!
//! `wait <ms>`, `click` (status item, through AppKit), `popover` (toggle),
//! `snap-popover <name>`, `snap-status <name>`, `needs-you on|off`,
//! `notify <1|2>`, `notify-status`, `bounce [info]`, `open-main`, `close-main`,
//! `agents <n>`, `stop-all`, `hide`, `popover-open?`, `quit`.

use std::time::Duration;

use gpui::{App, AsyncApp};

use crate::app::{self, Shell};
use crate::notify;

pub fn run(path: &str, cx: &mut App) {
    let script = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[script] cannot read {path}: {e}");
            return;
        }
    };
    cx.spawn(async move |cx| {
        for line in script.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            eprintln!("[script] {line}");
            let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
            if cmd == "wait" {
                let ms = arg.parse().unwrap_or(500);
                cx.background_executor()
                    .timer(Duration::from_millis(ms))
                    .await;
                continue;
            }
            if cmd == "snap-popover" {
                snap_popover(arg, cx).await;
                continue;
            }
            cx.update(|cx| step(cmd, arg, cx));
        }
    })
    .detach();
}

fn step(cmd: &str, arg: &str, cx: &mut App) {
    match cmd {
        "click" => app::click_status_item(cx),
        "popover" => app::toggle_popover(cx),
        "snap-status" => match app::status_png(cx) {
            Ok(png) => save(arg, &png),
            Err(e) => eprintln!("[snapshot] status item: {e:#}"),
        },
        "needs-you" => app::set_needs_you(cx, arg == "on"),
        "notify" => {
            notify::show(cx, arg.parse().unwrap_or(1));
            app::set_needs_you(cx, true);
        }
        "notify-status" => notify::log_status("script"),
        "bounce" => {
            let mtm = cx.global::<Shell>().mtm;
            let active = notify::app_is_active(mtm);
            let request = notify::bounce(mtm, arg != "info");
            // 0 means macOS ignored it (app active, or no dock icon).
            let policy = notify::activation_policy(mtm);
            eprintln!("[bounce] app active={active} policy={policy} request id={request}");
            if request != 0 {
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(Duration::from_secs(3)).await;
                    let mtm = cx.update(|cx| cx.global::<Shell>().mtm);
                    notify::cancel_bounce(mtm, request);
                    eprintln!("[bounce] cancelled");
                })
                .detach();
            }
        }
        "open-main" => app::open_main(cx),
        "close-main" => app::close_main(cx),
        "agents" => {
            let model = cx.global::<Shell>().model.clone();
            model.update(cx, |m, cx| {
                m.agents = arg.parse().unwrap_or(0);
                cx.notify();
            });
        }
        "stop-all" => app::stop_all(cx),
        // Deactivates the app like a click elsewhere; the popover should close.
        "hide" => cx.hide(),
        "popover-open?" => eprintln!("[app] popover open: {}", app::popover_is_open(cx)),
        "quit" => cx.quit(),
        other => eprintln!("[script] unknown command {other}"),
    }
}

fn save(name: &str, png: &[u8]) {
    let dir = std::env::var("SPIKE_SNAPSHOT_DIR").unwrap_or_else(|_| ".".into());
    let path = format!("{dir}/{name}.png");
    match std::fs::write(&path, png) {
        Ok(()) => eprintln!("[snapshot] {path}"),
        Err(e) => eprintln!("[snapshot] {path}: {e}"),
    }
}

/// Renders the popover view in a normal window and saves it. `render_to_image`
/// throws on macOS 14+ while the display link runs; minimizing detaches it.
#[cfg(feature = "snapshot")]
async fn snap_popover(name: &str, cx: &mut AsyncApp) {
    use gpui::{AppContext as _, Bounds, WindowBounds, WindowOptions, point, px, size};

    let opened = cx.update(|cx| {
        let model = cx.global::<Shell>().model.clone();
        let (w, h) = app::POPOVER_SIZE;
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(40.), px(80.)),
                size: size(px(w), px(h)),
            })),
            focus: false,
            ..Default::default()
        };
        cx.open_window(options, |window, cx| {
            cx.new(|cx| crate::popover::Popover::new(model, false, window, cx))
        })
    });
    let Ok(handle) = opened else {
        eprintln!("[snapshot] could not open window");
        return;
    };
    cx.background_executor()
        .timer(Duration::from_millis(600))
        .await;
    let _ = handle.update(cx, |_, window, _| window.minimize_window());
    cx.background_executor()
        .timer(Duration::from_millis(600))
        .await;
    let image = handle.update(cx, |_, window, _| window.render_to_image());
    match image {
        Ok(Ok(img)) => {
            let mut png = Vec::new();
            match img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png) {
                Ok(()) => save(name, &png),
                Err(e) => eprintln!("[snapshot] encode: {e}"),
            }
        }
        Ok(Err(e)) => eprintln!("[snapshot] render: {e:#}"),
        Err(e) => eprintln!("[snapshot] window gone: {e:#}"),
    }
    let _ = handle.update(cx, |_, window, _| window.remove_window());
}

#[cfg(not(feature = "snapshot"))]
async fn snap_popover(_name: &str, _cx: &mut AsyncApp) {
    eprintln!("[snapshot] build with --features snapshot");
}
