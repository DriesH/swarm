//! Frame timing run. Needs a release build and the `frame-stats` feature.
//!
//! With `SWARM_FRAME_OVERLAY` set it opens the app with GPUI's live frame overlay.
//! Otherwise it replays a scripted session and logs frame statistics, then quits.
//!
//! GPUI cannot reset a window's frame histograms. The warm-up pass runs in a window
//! that is closed afterwards, and the measured run uses a fresh window.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::anyhow;
use gpui::{AnyWindowHandle, App, AsyncApp, Context, DebugFrameOverlayMode, Window, px, size};
use swarm_ui::theme::{self, Appearance};

use crate::workspace::{RightPanel, View, Workspace};

const ROUNDS: usize = 20;
const SETTLE: Duration = Duration::from_secs(1);
const STEP_WAIT: Duration = Duration::from_millis(50);
const REFRESH_EVERY: Duration = Duration::from_millis(8);
const STEADY_REFRESHES: usize = 125;
const BUDGET_NS: u64 = 8_000_000;

#[derive(Default)]
struct StepStats {
    runs: u64,
    over_budget: u64,
}

struct Recorder {
    window: AnyWindowHandle,
    last_len: u64,
    last_over: u64,
    steps: BTreeMap<&'static str, StepStats>,
}

pub(crate) fn run(cx: &mut App) {
    if std::env::var_os("SWARM_FRAME_OVERLAY").is_some() {
        show_overlay(cx);
        return;
    }

    cx.spawn(async move |cx: &mut AsyncApp| {
        if let Err(error) = measure(cx).await {
            log::error!("frame stats failed: {error:#}");
        }

        cx.update(|cx| cx.quit());
    })
    .detach();
}

fn show_overlay(cx: &mut App) {
    match crate::open_workspace(cx) {
        Ok(handle) => {
            let shown = handle.update(cx, |_, window, _| {
                window.set_debug_frame_overlay_mode(DebugFrameOverlayMode::Full);
            });

            if let Err(error) = shown {
                log::error!("cannot show the overlay: {error:#}");
            }

            cx.activate(true);
        }
        Err(error) => {
            log::error!("could not open the main window: {error:#}");
            cx.quit();
        }
    }
}

async fn measure(cx: &mut AsyncApp) -> anyhow::Result<()> {
    cx.update(|cx| theme::set_appearance(Appearance::Dark, cx));

    let warm_up = open_window(cx).await?;
    run_pass(&mut Recorder::new(warm_up), true, cx).await?;
    warm_up.update(cx, |_, window, _| window.remove_window())?;
    cx.background_executor().timer(SETTLE).await;
    log::info!("warm-up done, measuring in a fresh window");

    let window = open_window(cx).await?;
    let mut recorder = Recorder::new(window);
    for round in 0..ROUNDS {
        run_pass(&mut recorder, round == 0, cx).await?;
    }

    report(&recorder, cx)?;
    window.update(cx, |_, window, _| window.remove_window())?;

    Ok(())
}

async fn open_window(cx: &mut AsyncApp) -> anyhow::Result<AnyWindowHandle> {
    let handle = cx.update(|cx| {
        let handle = crate::open_workspace(cx)?;
        cx.activate(true);

        anyhow::Ok(AnyWindowHandle::from(handle))
    })?;

    cx.background_executor().timer(SETTLE).await;

    Ok(handle)
}

async fn run_pass(
    recorder: &mut Recorder,
    first_round: bool,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let window = recorder.window;

    let tasks: Vec<_> = with_workspace(window, cx, |workspace, _, _| {
        workspace
            .data
            .tasks_of(workspace.active_project)
            .map(|task| task.id)
            .collect()
    })?;
    for task in tasks {
        let apply =
            move |workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>| {
                workspace.select_task(task, cx);
            };
        recorder.step("select task", apply, cx).await?;
    }

    for view in [View::Terminals, View::Swarm, View::Review, View::Terminals] {
        let apply =
            move |workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>| {
                workspace.set_view(view, cx);
            };
        recorder.step("set view", apply, cx).await?;
    }

    for panel in [
        RightPanel::Files,
        RightPanel::Files,
        RightPanel::Changes,
        RightPanel::Changes,
    ] {
        let apply =
            move |workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>| {
                workspace.toggle_panel(panel, cx);
            };
        recorder.step("toggle panel", apply, cx).await?;
    }

    recorder
        .step("open palette", Workspace::toggle_palette, cx)
        .await?;
    recorder
        .step("close palette", Workspace::close_overlay, cx)
        .await?;
    recorder
        .step("open new task", Workspace::open_new_task, cx)
        .await?;
    recorder
        .step("close new task", Workspace::close_overlay, cx)
        .await?;

    for (width, height) in [(1200., 800.), (1600., 1000.)] {
        let apply = move |_: &mut Workspace, window: &mut Window, _: &mut Context<Workspace>| {
            window.resize(size(px(width), px(height)));
        };
        recorder.step("resize", apply, cx).await?;
    }

    let projects: Vec<_> = with_workspace(window, cx, |workspace, _, _| {
        workspace
            .data
            .projects
            .iter()
            .map(|project| project.id)
            .collect()
    })?;
    let first = projects.first().copied();
    for project in projects.into_iter().skip(1).chain(first) {
        let apply =
            move |workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>| {
                workspace.select_project(project, cx);
            };
        recorder.step("select project", apply, cx).await?;
    }

    for appearance in [Appearance::Light, Appearance::Dark] {
        cx.update(|cx| theme::set_appearance(appearance, cx));
        recorder.settle("set appearance", cx).await?;
    }

    if first_round {
        let apply = |workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>| {
            workspace.new_user_terminal(cx);
        };
        recorder.step("new user terminal", apply, cx).await?;
    }

    for _ in 0..STEADY_REFRESHES {
        window.update(cx, |_, window, _| window.refresh())?;
        cx.background_executor().timer(REFRESH_EVERY).await;
    }
    recorder.record("steady redraw", cx)?;

    Ok(())
}

fn with_workspace<R>(
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    apply: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) -> R,
) -> anyhow::Result<R> {
    window.update(cx, |root, window, cx| {
        let workspace = root
            .downcast::<Workspace>()
            .map_err(|_| anyhow!("root is not a Workspace"))?;

        Ok(workspace.update(cx, |workspace, cx| apply(workspace, window, cx)))
    })?
}

impl Recorder {
    fn new(window: AnyWindowHandle) -> Self {
        Self {
            window,
            last_len: 0,
            last_over: 0,
            steps: BTreeMap::new(),
        }
    }

    async fn step(
        &mut self,
        label: &'static str,
        apply: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>),
        cx: &mut AsyncApp,
    ) -> anyhow::Result<()> {
        with_workspace(self.window, cx, apply)?;

        self.settle(label, cx).await
    }

    async fn settle(&mut self, label: &'static str, cx: &mut AsyncApp) -> anyhow::Result<()> {
        cx.background_executor().timer(STEP_WAIT).await;

        self.record(label, cx)
    }

    // Draw times only accumulate, so slow frames are attributed to the step before them.
    fn record(&mut self, label: &'static str, cx: &mut AsyncApp) -> anyhow::Result<()> {
        let histogram = self
            .window
            .update(cx, |_, window, _| window.frame_duration_snapshot())?
            .draw_duration_histogram;

        let len = histogram.len();
        let over = len - histogram.count_between(0, BUDGET_NS);

        let stats = self.steps.entry(label).or_default();
        stats.runs += len.saturating_sub(self.last_len);
        stats.over_budget += over.saturating_sub(self.last_over);

        self.last_len = len;
        self.last_over = over;

        Ok(())
    }
}

fn report(recorder: &Recorder, cx: &mut AsyncApp) -> anyhow::Result<()> {
    let snapshot = recorder
        .window
        .update(cx, |_, window, _| window.frame_duration_snapshot())?;

    for (name, histogram) in [
        ("draw", &snapshot.draw_duration_histogram),
        ("dirty to present", &snapshot.dirty_to_present_histogram),
    ] {
        let over = histogram.len() - histogram.count_between(0, BUDGET_NS);

        log::info!(
            "{name}: frames {} | p50 {} | p90 {} | p99 {} | max {} ms | over 8 ms: {over}",
            histogram.len(),
            ms(histogram.value_at_quantile(0.5)),
            ms(histogram.value_at_quantile(0.9)),
            ms(histogram.value_at_quantile(0.99)),
            ms(histogram.max()),
        );
    }

    for (label, stats) in &recorder.steps {
        log::info!(
            "step {label}: draw frames {} | over 8 ms: {}",
            stats.runs,
            stats.over_budget,
        );
    }

    Ok(())
}

fn ms(nanoseconds: u64) -> String {
    format!(
        "{:.2}",
        Duration::from_nanos(nanoseconds).as_secs_f64() * 1000.0
    )
}
