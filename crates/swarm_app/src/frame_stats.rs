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
use gpui::{
    AnyWindowHandle, App, AsyncApp, Context, DebugFrameOverlayMode, Modifiers, PlatformInput,
    ScrollDelta, ScrollEvent, Window, point, px, size,
};
use swarm_protocol::{MessageId, MessageInfo, MessageKind};
use swarm_ui::theme::{self, Appearance};

use crate::model::Agent;
use crate::workspace::{RightPanel, View, Workspace};

const ROUNDS: usize = 20;
const SETTLE: Duration = Duration::from_secs(1);
const STEP_WAIT: Duration = Duration::from_millis(50);
const REFRESH_EVERY: Duration = Duration::from_millis(8);
const STEADY_REFRESHES: usize = 125;
const BUDGET_NS: u64 = 8_000_000;
const THRESHOLDS_MS: [u64; 5] = [1, 2, 4, 8, 16];
const PRESENT_THRESHOLDS_MS: [u64; 3] = [8, 16, 33];
const SWARM_STEP_WAIT: Duration = Duration::from_millis(16);
const THREAD_MESSAGES: u64 = 200;
const SWARM_RESIZES: usize = 30;
const SWARM_SCROLLS: usize = 40;
const REVIEW_SCROLLS: usize = 60;
const REVIEW_FILES: usize = 60;
const REVIEW_LINES: u32 = 800;

#[derive(Default)]
struct StepStats {
    runs: u64,
    over: [u64; 5],
    present_runs: u64,
    present_over: [u64; 3],
}

struct Recorder {
    window: AnyWindowHandle,
    last_len: u64,
    last_over: [u64; 5],
    last_present_len: u64,
    last_present_over: [u64; 3],
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

    if first_round {
        with_workspace(window, cx, |workspace, _, _| {
            add_long_thread(workspace);
            add_big_diff(workspace);
        })?;
    }

    if std::env::var_os("SWARM_FRAME_SWARM_ONLY").is_some() {
        return swarm_steps(recorder, cx).await;
    }
    if std::env::var_os("SWARM_FRAME_REVIEW_ONLY").is_some() {
        return review_steps(recorder, cx).await;
    }

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

    swarm_steps(recorder, cx).await?;
    review_steps(recorder, cx).await?;

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
        let apply =
            |workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>| {
                workspace.new_user_terminal(window, cx);
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

/// Gives the orchestrator a long thread: short tasks and long multi-paragraph briefs.
fn add_long_thread(workspace: &mut Workspace) {
    let Some(orchestrator) = workspace.active_task().orchestrator else {
        return;
    };
    let workers = workspace.active_task().workers.clone();
    let Some((project, task)) = workspace.data.message_key(workspace.active_task) else {
        return;
    };
    let Some(orchestrator) = workspace.data.agent(orchestrator).map(Agent::protocol_id) else {
        return;
    };
    let workers: Vec<_> = workers
        .into_iter()
        .filter_map(|id| workspace.data.agent(id).map(Agent::protocol_id))
        .collect();
    if workers.is_empty() {
        return;
    }

    let paragraph = "Build the part you own and keep the interface small. Read the plan first, \
        then write the code and its tests. Run the checks before you report. If a rule in the \
        conventions conflicts with the plan, ask me before you decide.";

    for id in 100..100 + THREAD_MESSAGES {
        let worker = workers[usize::try_from(id).unwrap_or(0) % workers.len()];
        let brief = id % 5 == 0;
        let text = if brief {
            format!("Brief {id}\n\n{paragraph}\n\n{paragraph}\n\n{paragraph}\n\n{paragraph}")
        } else {
            format!("Short note {id}: report when the build passes.")
        };
        let (from, to) = if id % 3 == 0 {
            (Some(worker), orchestrator)
        } else {
            (Some(orchestrator), worker)
        };

        workspace.data.record_message(&MessageInfo {
            id: MessageId(id),
            project,
            task,
            from,
            to,
            kind: if brief {
                MessageKind::Brief
            } else {
                MessageKind::Task
            },
            text,
            sent_at: 1_000 + id,
            delivered_at: Some(1_000 + id),
        });
    }
}

async fn swarm_steps(recorder: &mut Recorder, cx: &mut AsyncApp) -> anyhow::Result<()> {
    let show = |workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>| {
        window.resize(size(px(1600.), px(1000.)));
        workspace.set_view(View::Swarm, cx);
    };
    recorder.step_after("swarm idle", show, SETTLE, cx).await?;

    for index in 0..SWARM_RESIZES {
        #[expect(clippy::cast_precision_loss, reason = "a small step count")]
        let width = 1100. + 25. * ((index % 20) as f32 + 1.);
        let apply = move |_: &mut Workspace, window: &mut Window, _: &mut Context<Workspace>| {
            window.resize(size(px(width), px(800. + (width - 1100.) / 2.)));
        };
        recorder
            .step_after("swarm resize", apply, SWARM_STEP_WAIT, cx)
            .await?;
    }

    let restore = |_: &mut Workspace, window: &mut Window, _: &mut Context<Workspace>| {
        window.resize(size(px(1600.), px(1000.)));
    };
    recorder
        .step_after("swarm idle", restore, SETTLE, cx)
        .await?;

    for index in 0..SWARM_SCROLLS {
        let direction = if index < SWARM_SCROLLS / 2 { 48. } else { -48. };
        recorder.window.update(cx, |_, window, cx| {
            window.dispatch_event(
                PlatformInput::Scroll(ScrollEvent {
                    position: point(px(1440.), px(450.)),
                    delta: ScrollDelta::Pixels(point(px(0.), px(direction))),
                    modifiers: Modifiers::default(),
                    ..Default::default()
                }),
                cx,
            );
        })?;
        cx.background_executor().timer(SWARM_STEP_WAIT).await;
        recorder.record("swarm scroll", cx)?;
    }

    recorder
        .step_after(
            "swarm idle",
            |workspace, _, cx| workspace.set_view(View::Terminals, cx),
            STEP_WAIT,
            cx,
        )
        .await
}

/// Gives the open task a diff of about 48,000 lines in both sources.
fn add_big_diff(workspace: &mut Workspace) {
    use swarm_protocol::{DiffStat, FileChange, ReviewSource, TaskDiff};

    let Some(task) = workspace.data.diff_id(workspace.active_task) else {
        return;
    };

    for source in [ReviewSource::Live, ReviewSource::Merged] {
        let files: Vec<_> = (0..REVIEW_FILES)
            .map(|index| {
                crate::review_model::generated_file(
                    std::path::PathBuf::from("/work/swarm/big"),
                    &format!("crates/big/src/file_{index:02}.rs"),
                    None,
                    FileChange::Modified,
                    REVIEW_LINES / 2,
                    REVIEW_LINES / 2,
                )
            })
            .collect();
        let stat = DiffStat {
            files: u32::try_from(files.len()).unwrap_or(u32::MAX),
            additions: files.iter().map(|file| file.additions).sum(),
            deletions: files.iter().map(|file| file.deletions).sum(),
        };

        let comments = big_comments(task, source, &files);

        workspace.data.store_diff(TaskDiff {
            task,
            source,
            fingerprint: 0xB16,
            stat,
            files,
        });
        let mut all = workspace.data.task_list(task).to_vec();
        all.extend(comments);
        workspace.data.set_comments(task, all);
    }
}

/// One thread on every fifth file of the big diff, so scrolling meets comment rows.
fn big_comments(
    task: swarm_protocol::TaskId,
    source: swarm_protocol::ReviewSource,
    files: &[swarm_protocol::FileDiff],
) -> Vec<swarm_protocol::ReviewComment> {
    use swarm_protocol::{CommentId, CommentState, ReviewComment};

    files
        .iter()
        .enumerate()
        .step_by(5)
        .filter_map(|(index, file)| {
            let at = crate::review_model::first_changed_line(file, index)?;
            let anchor =
                crate::review_model::anchor_for(file, at.hunk as usize, at.line as usize, at.side)?;

            Some(ReviewComment {
                id: CommentId(
                    1_000
                        + index as u64 * 2
                        + u64::from(source == swarm_protocol::ReviewSource::Merged),
                ),
                task,
                parent: None,
                source,
                anchor,
                body: "Cache shaped lines per row and only reshape dirty rows.".to_owned(),
                state: CommentState::Pending,
                to: None,
                outdated: false,
                created_at: 0,
                sent_at: None,
            })
        })
        .collect()
}

/// Scrolls the big diff in the unified and the split style.
async fn review_steps(recorder: &mut Recorder, cx: &mut AsyncApp) -> anyhow::Result<()> {
    for (label, style) in [
        ("review scroll", crate::review_model::DiffStyle::Unified),
        ("review scroll split", crate::review_model::DiffStyle::Split),
    ] {
        let show =
            move |workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>| {
                window.resize(size(px(1600.), px(1000.)));
                workspace.set_view(View::Review, cx);
                workspace.set_review_source(swarm_protocol::ReviewSource::Merged, cx);
                workspace
                    .review
                    .update(cx, |review, cx| review.set_style(style, cx));
            };
        recorder.step_after("review idle", show, SETTLE, cx).await?;

        for index in 0..REVIEW_SCROLLS {
            let direction = if index < REVIEW_SCROLLS / 2 {
                160.
            } else {
                -160.
            };
            recorder.window.update(cx, |_, window, cx| {
                window.dispatch_event(
                    PlatformInput::Scroll(ScrollEvent {
                        position: point(px(900.), px(500.)),
                        delta: ScrollDelta::Pixels(point(px(0.), px(direction))),
                        modifiers: Modifiers::default(),
                        ..Default::default()
                    }),
                    cx,
                );
            })?;
            cx.background_executor().timer(SWARM_STEP_WAIT).await;
            recorder.record(label, cx)?;
        }
    }

    recorder
        .step_after(
            "review idle",
            |workspace, _, cx| workspace.set_view(View::Terminals, cx),
            STEP_WAIT,
            cx,
        )
        .await
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
            last_over: [0; 5],
            last_present_len: 0,
            last_present_over: [0; 3],
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

    async fn step_after(
        &mut self,
        label: &'static str,
        apply: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>),
        wait: Duration,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<()> {
        with_workspace(self.window, cx, apply)?;
        cx.background_executor().timer(wait).await;

        self.record(label, cx)
    }

    async fn settle(&mut self, label: &'static str, cx: &mut AsyncApp) -> anyhow::Result<()> {
        cx.background_executor().timer(STEP_WAIT).await;

        self.record(label, cx)
    }

    // Draw times only accumulate, so slow frames are attributed to the step before them.
    fn record(&mut self, label: &'static str, cx: &mut AsyncApp) -> anyhow::Result<()> {
        let snapshot = self
            .window
            .update(cx, |_, window, _| window.frame_duration_snapshot())?;
        let histogram = snapshot.draw_duration_histogram;
        let present = snapshot.dirty_to_present_histogram;

        let len = histogram.len();
        let over = THRESHOLDS_MS.map(|ms| len - histogram.count_between(0, ms * 1_000_000));

        let stats = self.steps.entry(label).or_default();
        stats.runs += len.saturating_sub(self.last_len);
        for (index, count) in over.iter().enumerate() {
            stats.over[index] += count.saturating_sub(self.last_over[index]);
        }

        let present_len = present.len();
        let present_over =
            PRESENT_THRESHOLDS_MS.map(|ms| present_len - present.count_between(0, ms * 1_000_000));
        stats.present_runs += present_len.saturating_sub(self.last_present_len);
        for (index, count) in present_over.iter().enumerate() {
            stats.present_over[index] += count.saturating_sub(self.last_present_over[index]);
        }

        self.last_len = len;
        self.last_over = over;
        self.last_present_len = present_len;
        self.last_present_over = present_over;

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
            "step {label}: draw frames {} | over 1/2/4/8/16 ms: {:?} | dirty to present frames {} over 8/16/33 ms: {:?}",
            stats.runs,
            stats.over,
            stats.present_runs,
            stats.present_over,
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
