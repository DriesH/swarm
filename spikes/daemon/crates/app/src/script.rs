//! Drives the window from a script file (`SPIKE_SCRIPT=path`), so tests can run
//! without focus or accessibility permissions. Keys go through GPUI's normal
//! key dispatch, the same path as real key presses after the OS event.
//!
//! Lines: `type <text>`, `key <keystroke>` (e.g. `ctrl-c`, `up`, `enter`),
//! `wait <ms>`, `resize <w> <h>`, `scroll <lines>` (positive = back in history),
//! `snap`, `show`, `quit`, and for measurements: `mark`, `waitfor <text>`
//! (ms since `mark` until the screen shows `text`), `latency <n> <gap ms>`
//! (types `n` keys), `report` (latency percentiles), `dump <path>`.
//! `#` starts a comment.
//!
//! `snap` minimizes the window first: GPUI's `render_to_image` calls
//! `nextDrawable`, which throws while the macOS 14+ `CAMetalDisplayLink`
//! is attached, and GPUI only detaches it when the window is occluded.

use std::time::{Duration, Instant};

use gpui::{
    AnyWindowHandle, App, AsyncApp, Entity, Keystroke, Modifiers, OperatingSystem, PlatformInput,
    ScrollDelta, ScrollEvent, point, px, size,
};

use crate::view::TerminalView;

// AnyWindowHandle, not WindowHandle<V>: the typed handle leases the root view
// during the update, and the key handler then can't borrow it again.
pub fn run(path: String, window: AnyWindowHandle, view: Entity<TerminalView>, cx: &App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut mark = Instant::now();
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
                "mark" => {
                    mark = Instant::now();
                    Ok(())
                }
                "waitfor" => {
                    let deadline = Instant::now() + Duration::from_secs(120);
                    loop {
                        if view.read_with(cx, |v, _| v.visible_text().contains(arg)) {
                            eprintln!(
                                "[script] saw {arg:?} after {:.0} ms",
                                mark.elapsed().as_secs_f64() * 1e3
                            );
                            break;
                        }
                        if Instant::now() > deadline {
                            eprintln!("[script] timed out waiting for {arg:?}");
                            break;
                        }
                        cx.background_executor()
                            .timer(Duration::from_millis(2))
                            .await;
                    }
                    Ok(())
                }
                "latency" => {
                    let mut it = arg.split(' ').filter_map(|n| n.parse::<u64>().ok());
                    let (n, gap) = (it.next().unwrap_or(100), it.next().unwrap_or(30));
                    for i in 0..n {
                        let c = char::from(b'a' + (i % 26) as u8);
                        let _ = window.update(cx, |_, window, cx| {
                            window.dispatch_keystroke(char_keystroke(c), cx);
                        });
                        cx.background_executor()
                            .timer(Duration::from_millis(gap))
                            .await;
                    }
                    Ok(())
                }
                "report" => {
                    view.update(cx, |v, _| {
                        let update = percentiles(&mut v.stats.key_to_update_us);
                        let paint = percentiles(&mut v.stats.key_to_paint_us);
                        eprintln!("[latency] key→update {update} | key→paint {paint}");
                    });
                    Ok(())
                }
                "dump" => {
                    let text = view.read_with(cx, |v, _| v.visible_text());
                    std::fs::write(arg, text).map_err(Into::into)
                }
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

fn percentiles(samples: &mut Vec<u32>) -> String {
    samples.sort_unstable();
    let n = samples.len();
    let at = |p: usize| samples.get((n * p / 100).min(n.saturating_sub(1))).copied();
    let out = format!(
        "n={n} p50 {:?}us p99 {:?}us max {:?}us",
        at(50),
        at(99),
        samples.last()
    );
    samples.clear();
    out
}
