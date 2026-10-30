mod app;
mod claude;
mod codex;
mod icon;
mod notify;
mod place;
mod popover;
mod script;
mod status_item;
mod usage;

use gpui::{App, KeyBinding};

/// Prints GPUI's log output; it logs through the `log` crate.
struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        meta.level() <= log::Level::Info
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
    let _ = log::set_logger(&StderrLogger).map(|()| log::set_max_level(log::LevelFilter::Info));
    let state_dir = std::env::var_os("SPIKE_STATE_DIR").map(std::path::PathBuf::from);
    eprintln!("[app] in app bundle: {}", notify::in_bundle());

    gpui_platform::application().run(move |cx: &mut App| {
        cx.bind_keys([KeyBinding::new("escape", popover::Dismiss, Some("Popover"))]);
        if let Err(e) = app::setup(cx, state_dir) {
            eprintln!("[app] setup failed: {e:#}");
            cx.quit();
            return;
        }
        notify::log_status("startup");
        if let Ok(path) = std::env::var("SPIKE_SCRIPT") {
            script::run(&path, cx);
        }
    });
}
