//! `swarm-app`: attaches to a terminal owned by `swarm-daemon` and renders it.

mod client;
mod element;
mod script;
mod view;

use std::time::Instant;

use gpui::{App, Bounds, TitlebarOptions, WindowBounds, WindowOptions, font, prelude::*, px, size};
use proto::Size;

use crate::view::{Metrics, TerminalView};

const FONT_SIZE: f32 = 13.;
const LINE_HEIGHT: f32 = 1.3;
/// Must match the element's padding.
const PADDING: f32 = 8.;

fn metrics(cx: &App) -> Metrics {
    let text = cx.text_system();
    let family = if text.all_font_names().iter().any(|f| f == "Geist Mono") {
        "Geist Mono"
    } else {
        "Menlo"
    };
    let font = font(family);
    let font_size = px(FONT_SIZE);
    let font_id = text.resolve_font(&font);
    let cell_width = text
        .advance(font_id, font_size, 'm')
        .map_or(font_size * 0.6, |s| s.width);
    Metrics {
        font,
        font_size,
        cell_width,
        line_height: (font_size * LINE_HEIGHT).round(),
    }
}

/// Prints GPUI's warnings and errors; it logs through the `log` crate.
struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        meta.level() <= log::Level::Warn
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!(
                "[{}] {}: {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
    }
    fn flush(&self) {}
}

fn main() {
    let started = Instant::now();
    let _ = log::set_logger(&StderrLogger).map(|()| log::set_max_level(log::LevelFilter::Warn));
    gpui_platform::application().run(move |cx: &mut App| {
        let m = metrics(cx);
        let cell = (f32::from(m.cell_width), f32::from(m.line_height));
        // Size for a new terminal; an existing one keeps its size.
        let default_size = Size {
            cols: 120,
            rows: 34,
            cell_width: cell.0 as u16,
            cell_height: cell.1 as u16,
        };
        let attached = match client::attach(default_size) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("[app] cannot attach: {e:#}");
                cx.quit();
                return;
            }
        };
        eprintln!("[app] {}", attached.timings);

        // Open the window at the terminal's size, so attaching needs no resize.
        let screen = &attached.snapshot.screen;
        let width = f32::from(screen.cols) * cell.0 + PADDING * 2. + 1.;
        let height = f32::from(screen.rows) * cell.1 + PADDING * 2. + 1.;
        let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
        // Scripted runs must not take keyboard focus: real typing would land here.
        let script = std::env::var("SPIKE_SCRIPT").ok();
        let mut view = None;
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(
                        format!("swarm daemon spike · terminal {}", attached.snapshot.id).into(),
                    ),
                    ..Default::default()
                }),
                focus: script.is_none(),
                ..Default::default()
            },
            |window, cx| {
                let entity = cx.new(|cx| {
                    TerminalView::new(
                        attached.client,
                        attached.snapshot,
                        attached.events,
                        m,
                        started,
                        window,
                        cx,
                    )
                });
                view = Some(entity.clone());
                entity
            },
        );
        let (window, view) = match (opened, view) {
            (Ok(window), Some(view)) => (window, view),
            (Err(e), _) => {
                eprintln!("[app] failed to open window: {e:#}");
                cx.quit();
                return;
            }
            (Ok(_), None) => return,
        };
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        match script {
            Some(path) => script::run(path, window.into(), view, cx),
            None => cx.activate(true),
        }
    });
}
