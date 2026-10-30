//! Drives the window from a script file (`SPIKE_SCRIPT=path`), so tests can run
//! without focus or accessibility permissions. Keys go through GPUI's normal
//! key dispatch, the same path as real key presses after the OS event.
//!
//! Lines: `type <text>`, `key <keystroke>` (e.g. `ctrl-c`, `up`, `enter`),
//! `wait <ms>`, `resize <w> <h>`, `scroll <lines>` (positive = back in history),
//! `snap`, `show`, `quit`. `#` starts a comment.
//!
//! `snap` minimizes the window first: GPUI's `render_to_image` calls
//! `nextDrawable`, which throws while the macOS 14+ `CAMetalDisplayLink`
//! is attached, and GPUI only detaches it when the window is occluded.

use std::time::Duration;

use gpui::{
    AnyWindowHandle, App, AsyncApp, Keystroke, Modifiers, OperatingSystem, PlatformInput,
    ScrollDelta, ScrollEvent, point, px, size,
};

// AnyWindowHandle, not WindowHandle<V>: the typed handle leases the root view
// during the update, and the key handler then can't borrow it again.
pub fn run(path: String, window: AnyWindowHandle, cx: &App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        let script = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => return eprintln!("[script] cannot read {path}: {e}"),
        };
        for line in script.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
            eprintln!("[script] {line}");
            let result = match cmd {
                "wait" => {
                    let ms = arg.parse().unwrap_or(500);
                    cx.background_executor()
                        .timer(Duration::from_millis(ms))
                        .await;
                    Ok(())
                }
                "type" => window.update(cx, |_, window, cx| {
                    for c in arg.chars() {
                        window.dispatch_keystroke(char_keystroke(c), cx);
                    }
                }),
                "key" => match Keystroke::parse_with_os(arg, OperatingSystem::Mac) {
                    Ok(ks) => window.update(cx, |_, window, cx| {
                        window.dispatch_keystroke(ks, cx);
                    }),
                    Err(e) => Err(e.into()),
                },
                "resize" => {
                    let mut it = arg.split(' ').filter_map(|n| n.parse::<f32>().ok());
                    let (w, h) = (it.next().unwrap_or(800.), it.next().unwrap_or(500.));
                    window.update(cx, |_, window, _| window.resize(size(px(w), px(h))))
                }
                "snap" => {
                    let _ = window.update(cx, |_, window, _| window.minimize_window());
                    cx.background_executor()
                        .timer(Duration::from_millis(800))
                        .await;
                    window.update(cx, |_, window, cx| {
                        window.dispatch_keystroke(
                            Keystroke {
                                modifiers: Modifiers::command(),
                                key: "s".into(),
                                key_char: None,
                            },
                            cx,
                        );
                    })
                }
                "scroll" => {
                    let lines = arg.parse().unwrap_or(5.);
                    window.update(cx, |_, window, cx| {
                        let center = window.viewport_size() / 2.;
                        let event = ScrollEvent {
                            position: point(center.width, center.height),
                            delta: ScrollDelta::Lines(point(0., lines)),
                            ..Default::default()
                        };
                        window.dispatch_event(PlatformInput::Scroll(event), cx);
                    })
                }
                "show" => window.update(cx, |_, window, _| window.activate_window()),
                "quit" => {
                    cx.update(|cx| cx.quit());
                    return;
                }
                _ => Err(anyhow::anyhow!("unknown command")),
            };
            if let Err(e) = result {
                eprintln!("[script] {line}: {e:#}");
            }
        }
    })
    .detach();
}

/// What macOS reports for a plain key press of `c` (with shift for capitals).
fn char_keystroke(c: char) -> Keystroke {
    let (key, shift) = match c {
        ' ' => ("space".to_owned(), false),
        c if c.is_ascii_uppercase() => (c.to_ascii_lowercase().to_string(), true),
        c => (c.to_string(), false),
    };
    Keystroke {
        modifiers: Modifiers {
            shift,
            ..Modifiers::none()
        },
        key,
        key_char: Some(c.to_string()),
    }
}
