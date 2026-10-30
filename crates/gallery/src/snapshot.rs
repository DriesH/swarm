//! Saves each gallery page as a PNG, in light and dark, then quits.
//! Run with `SWARM_GALLERY_SNAPSHOT_DIR=<dir> cargo run -p gallery --features snapshot`.
//! `SWARM_GALLERY_SNAPSHOT_FILTER=<text>` saves only files whose name contains it.
//!
//! GPUI's `render_to_image` throws on macOS 14+ while the window's display link
//! runs, so each window is minimized first; the last frame stays available.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{AnyWindowHandle, App, AsyncApp, KeyUpEvent, Keystroke, OperatingSystem, PlatformInput};
use swarm_ui::theme::{self, Appearance};

const SETTLE: Duration = Duration::from_millis(700);
const MID_ANIMATION: Duration = Duration::from_millis(40);

/// Extra shots after key presses, to check focus rings and keyboard use:
/// (page, keys, file suffix, wait for animations to finish).
const KEYBOARD_SHOTS: [(&str, &[&str], &str, bool); 7] = [
    ("Label", &["down"], "list-row", true),
    ("Button", &["tab"], "segment", true),
    ("Button", &["tab", "tab", "tab", "tab"], "button", true),
    ("Switch", &["tab", "tab", "tab", "tab"], "switch", true),
    (
        "TileFrame",
        &["tab", "tab", "tab", "tab"],
        "icon-button",
        true,
    ),
    ("SegmentedControl", &SELECT_SWARM, "glide-mid", false),
    ("SegmentedControl", &SELECT_SWARM, "glide-end", true),
];

/// Tabs past the theme control to "Swarm" in the view switch, then presses Space.
const SELECT_SWARM: [&str; 6] = ["tab", "tab", "tab", "tab", "tab", "space"];

pub(crate) fn run(directory: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        if let Err(error) = std::fs::create_dir_all(&directory) {
            log::error!("cannot create {}: {error}", directory.display());
            cx.update(|cx| cx.quit());
            return;
        }

        let names: Vec<&'static str> = crate::all_components()
            .iter()
            .map(swarm_ui::component::ComponentEntry::name)
            .collect();

        for appearance in [Appearance::Light, Appearance::Dark] {
            cx.update(|cx| theme::set_appearance(appearance, cx));
            let theme = theme_name(appearance);

            for (index, name) in names.iter().enumerate() {
                let file = format!("{theme}-{index:02}-{}.png", name.to_lowercase());
                snap(&directory.join(file), index, &[], true, cx).await;
            }

            for (name, keys, suffix, settle) in KEYBOARD_SHOTS {
                let Some(index) = names.iter().position(|candidate| *candidate == name) else {
                    continue;
                };

                let file = format!("{theme}-keyboard-{suffix}.png");
                snap(&directory.join(file), index, keys, settle, cx).await;
            }
        }

        cx.update(|cx| cx.quit());
    })
    .detach();
}

fn theme_name(appearance: Appearance) -> &'static str {
    match appearance {
        Appearance::Light => "light",
        Appearance::Dark => "dark",
        Appearance::System => "system",
    }
}

async fn snap(path: &Path, index: usize, keys: &[&str], settle: bool, cx: &mut AsyncApp) {
    if let Ok(filter) = std::env::var("SWARM_GALLERY_SNAPSHOT_FILTER")
        && !path.to_string_lossy().contains(&filter)
    {
        return;
    }

    let opened = cx.update(|cx| crate::open_gallery(index, cx));

    // AnyWindowHandle, not WindowHandle<Gallery>: the typed handle leases the
    // root view, and the key handlers then cannot update it.
    let window: AnyWindowHandle = match opened {
        Ok(handle) => handle.into(),
        Err(error) => {
            log::error!("cannot open window: {error:#}");
            return;
        }
    };

    cx.background_executor().timer(SETTLE).await;

    if !keys.is_empty() {
        press(window, keys, cx);
        // Without settling, the shot catches an animation part way through.
        let wait = if settle { SETTLE } else { MID_ANIMATION };
        cx.background_executor().timer(wait).await;
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

fn press(window: AnyWindowHandle, keys: &[&str], cx: &mut AsyncApp) {
    for key in keys {
        let keystroke = match Keystroke::parse_with_os(key, OperatingSystem::Mac) {
            Ok(keystroke) => keystroke,
            Err(error) => {
                log::error!("cannot parse {key}: {error:#}");
                return;
            }
        };

        // Key up too: GPUI clicks a focused element on Enter or Space release.
        let pressed = window.update(cx, |_, window, cx| {
            window.dispatch_keystroke(keystroke.clone(), cx);
            window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
        });

        if let Err(error) = pressed {
            log::error!("cannot press {key}: {error:#}");
        }
    }
}
