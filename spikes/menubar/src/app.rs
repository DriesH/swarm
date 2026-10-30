//! App state, windows and polling.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use gpui::{
    ActivationPolicy, AnyWindowHandle, App, AppContext as _, Bounds, Entity, Global,
    TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind,
    WindowOptions, point, px, size,
};
use objc2::MainThreadMarker;

use crate::claude::ClaudeSource;
use crate::codex;
use crate::notify;
use crate::popover::{MainView, Popover};
use crate::status_item::StatusItem;
use crate::usage::{POLL_INTERVAL, SourceState};

pub const POPOVER_SIZE: (f32, f32) = (320., 360.);
/// A click on the status item first deactivates the popover; ignore the toggle right after.
const REOPEN_GUARD: Duration = Duration::from_millis(300);
const CLAUDE_TICK: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct Model {
    pub claude: SourceState,
    pub codex: SourceState,
    pub agents: usize,
    pub needs_you: bool,
}

impl Model {
    /// The menu bar shows the fullest window: it is the one that will stop you.
    pub fn headline(&self) -> Option<f32> {
        [self.claude.max_percent(), self.codex.max_percent()]
            .into_iter()
            .flatten()
            .reduce(f32::max)
    }
}

pub struct Shell {
    pub model: Entity<Model>,
    status: Rc<StatusItem>,
    popover: Option<WindowHandle<Popover>>,
    popover_closed_at: Option<Instant>,
    main_window: Option<AnyWindowHandle>,
    claude: Arc<Mutex<ClaudeSource>>,
    pub mtm: MainThreadMarker,
}

impl Global for Shell {}

pub fn setup(cx: &mut App, state_dir: Option<PathBuf>) -> Result<()> {
    let mtm = MainThreadMarker::new().context("not on the main thread")?;
    let (clicks_tx, clicks_rx) = flume::unbounded();
    let status = Rc::new(StatusItem::new(mtm, clicks_tx)?);
    let model = cx.new(|_| Model {
        agents: 3,
        ..Default::default()
    });

    let observed = status.clone();
    cx.observe(&model, move |model, cx| {
        let m = model.read(cx);
        observed.set(m.headline(), m.needs_you);
    })
    .detach();
    status.set(None, false);

    cx.set_global(Shell {
        model,
        status,
        popover: None,
        popover_closed_at: None,
        main_window: None,
        claude: Arc::new(Mutex::new(ClaudeSource::new(state_dir))),
        mtm,
    });

    cx.spawn(async move |cx| {
        while clicks_rx.recv_async().await.is_ok() {
            cx.update(toggle_popover);
        }
    })
    .detach();

    cx.on_window_closed(|cx, id| {
        let shell = cx.global_mut::<Shell>();
        if shell.popover.is_some_and(|w| w.window_id() == id) {
            shell.popover = None;
            shell.popover_closed_at = Some(Instant::now());
        }
        if shell.main_window.is_some_and(|w| w.window_id() == id) {
            shell.main_window = None;
            eprintln!("[app] main window closed; accessory mode, status item stays");
            cx.set_activation_policy(ActivationPolicy::Accessory);
        }
    })
    .detach();

    if notify::in_bundle() {
        cx.on_system_notification_response(|response, cx| {
            let action = response.action_id.as_deref().unwrap_or("<body>");
            eprintln!("[notify] response tag={} action={action}", response.tag);
            let shell = cx.global::<Shell>();
            shell.model.clone().update(cx, |m, cx| {
                m.needs_you = false;
                cx.notify();
            });
            if action != "dismiss" {
                open_main(cx);
            }
        });
    }

    start_polling(cx);
    open_main(cx);
    Ok(())
}

pub fn toggle_popover(cx: &mut App) {
    let shell = cx.global::<Shell>();
    if let Some(handle) = shell.popover {
        close_popover(cx, handle);
        return;
    }
    if shell
        .popover_closed_at
        .is_some_and(|t| t.elapsed() < REOPEN_GUARD)
    {
        return;
    }
    open_popover(cx);
}

pub fn open_popover(cx: &mut App) {
    let shell = cx.global::<Shell>();
    if shell.popover.is_some() {
        return;
    }
    let model = shell.model.clone();
    let (w, h) = POPOVER_SIZE;
    let placed = shell
        .status
        .anchor()
        .and_then(|(anchor, screen)| crate::place::below(anchor, &screen, w, h));
    let fallback = Bounds::new(point(px(0.), px(0.)), size(px(w), px(h)));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(placed.map_or(fallback, |p| p.1))),
        display_id: placed.map(|p| p.0),
        titlebar: None,
        kind: WindowKind::PopUp,
        focus: true,
        show: true,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Opaque,
        ..Default::default()
    };
    match cx.open_window(options, |window, cx| {
        cx.new(|cx| Popover::new(model, true, window, cx))
    }) {
        Ok(handle) => {
            cx.global_mut::<Shell>().popover = Some(handle);
            // An accessory app must activate itself, or the panel never becomes key
            // and never gets the deactivation that closes it. Deferred: activation
            // calls back into GPUI synchronously.
            cx.defer(|cx| cx.activate(true));
            let bounds = handle.update(cx, |_, window, _| window.bounds()).ok();
            let anchor = cx.global::<Shell>().status.anchor().map(|a| a.0);
            eprintln!("[app] popover open at {bounds:?}, status item frame (Cocoa) {anchor:?}");
        }
        Err(e) => eprintln!("[app] popover failed: {e:#}"),
    }
}

pub fn close_popover(cx: &mut App, handle: WindowHandle<Popover>) {
    let _ = handle.update(cx, |_, window, _| window.remove_window());
    eprintln!("[app] popover closed");
}

pub fn open_main(cx: &mut App) {
    let shell = cx.global::<Shell>();
    if let Some(window) = shell.main_window {
        let _ = window.update(cx, |_, window, _| window.activate_window());
        return;
    }
    let model = shell.model.clone();
    cx.set_activation_policy(ActivationPolicy::Regular);
    let bounds = Bounds::centered(None, size(px(520.), px(320.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Swarm (menu bar spike)".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    match cx.open_window(options, |_, cx| cx.new(|cx| MainView::new(model, cx))) {
        Ok(handle) => cx.global_mut::<Shell>().main_window = Some(handle.into()),
        Err(e) => eprintln!("[app] main window failed: {e:#}"),
    }
}

pub fn close_main(cx: &mut App) {
    if let Some(window) = cx.global::<Shell>().main_window {
        let _ = window.update(cx, |_, window, _| window.remove_window());
    }
}

pub fn stop_all(cx: &mut App) {
    let model = cx.global::<Shell>().model.clone();
    model.update(cx, |m, cx| {
        eprintln!("[app] stop all: {} agents", m.agents);
        m.agents = 0;
        cx.notify();
    });
}

pub fn set_needs_you(cx: &mut App, on: bool) {
    let model = cx.global::<Shell>().model.clone();
    model.update(cx, |m, cx| {
        m.needs_you = on;
        cx.notify();
    });
}

pub fn popover_is_open(cx: &App) -> bool {
    cx.global::<Shell>().popover.is_some()
}

pub fn click_status_item(cx: &App) {
    cx.global::<Shell>().status.perform_click();
}

pub fn status_png(cx: &App) -> Result<Vec<u8>> {
    cx.global::<Shell>().status.snapshot_png()
}

fn start_polling(cx: &mut App) {
    let claude = cx.global::<Shell>().claude.clone();
    let model = cx.global::<Shell>().model.clone();
    cx.spawn({
        let model = model.clone();
        async move |cx| {
            loop {
                let source = claude.clone();
                let result = cx
                    .background_spawn(
                        async move { source.lock().map(|mut s| s.poll()).ok().flatten() },
                    )
                    .await;
                if let Some(result) = result {
                    log_result("claude", &result);
                    model.update(cx, |m, cx| {
                        m.claude.apply(result);
                        cx.notify();
                    });
                }
                cx.background_executor().timer(CLAUDE_TICK).await;
            }
        }
    })
    .detach();

    cx.spawn(async move |cx| {
        loop {
            let result = cx.background_spawn(async { codex::poll() }).await;
            log_result("codex", &result);
            model.update(cx, |m, cx| {
                m.codex.apply(result);
                cx.notify();
            });
            cx.background_executor().timer(POLL_INTERVAL).await;
        }
    })
    .detach();
}

fn log_result(source: &str, result: &Result<crate::usage::Usage, crate::usage::Problem>) {
    match result {
        Ok(u) => {
            let windows: Vec<String> = u
                .windows
                .iter()
                .map(|w| format!("{} {:.0}%", w.label, w.used_percent))
                .collect();
            eprintln!("[{source}] ok plan={:?} {windows:?}", u.plan);
        }
        Err(p) => eprintln!("[{source}] {p:?}"),
    }
}
