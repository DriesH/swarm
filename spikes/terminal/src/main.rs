mod element;
mod keys;
mod pty;
mod script;
mod view;

use gpui::{App, Bounds, TitlebarOptions, WindowBounds, WindowOptions, font, prelude::*, px, size};

use crate::view::{Metrics, TerminalView};

const FONT_SIZE: f32 = 13.;
const LINE_HEIGHT: f32 = 1.3;

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
    eprintln!("[font] {family}, cell width {cell_width:?}");
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
    let _ = log::set_logger(&StderrLogger).map(|()| log::set_max_level(log::LevelFilter::Warn));
    gpui_platform::application().run(|cx: &mut App| {
        let metrics = metrics(cx);
        let bounds = Bounds::centered(None, size(px(960.), px(600.)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("swarm terminal spike".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    TerminalView::new(metrics, window, cx)
                        .unwrap_or_else(|e| panic!("failed to start terminal: {e:#}"))
                })
            },
        );
        let window = match opened {
            Ok(window) => window,
            Err(e) => {
                eprintln!("failed to open window: {e:#}");
                cx.quit();
                return;
            }
        };
        if let Ok(path) = std::env::var("SPIKE_SCRIPT") {
            script::run(path, window.into(), cx);
        }
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}
