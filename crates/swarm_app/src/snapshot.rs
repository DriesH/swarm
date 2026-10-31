//! Saves the main window as `workspace-light.png`, `workspace-dark.png` and the other
//! `workspace-*.png` shots, then quits.
//! Run with `SWARM_APP_SNAPSHOT_DIR=<dir> cargo run -p swarm_app --features snapshot`.
//! `SWARM_APP_SNAPSHOT_FILTER=<text>` saves only files whose name contains it.
//!
//! GPUI's `render_to_image` throws on macOS 14+ while the window's display link
//! runs, so the window is minimized first; the last frame stays available.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui::{AnyWindowHandle, App, AsyncApp, Context, Window};
use swarm_protocol::ReviewSource;
use swarm_ui::theme::{self, Appearance};

use crate::model::{AgentKind, ProjectId, TaskId, TaskStatus};
use crate::review_model::{DiffStyle, LinePos};
use crate::swarm_view::InspectorTab;
use crate::workspace::{View, Workspace};

type OpenOverlay = fn(&mut Workspace, &mut Window, &mut Context<Workspace>);

fn open_island(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    workspace.new_user_terminal(window, cx);
}

fn open_start(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    workspace.open_start_task(cx);
}

fn open_start_empty(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    workspace.select_project(ProjectId(3), cx);
}

/// Nine more tasks in the open project, so the tabs overflow. Returns their ids.
fn add_many_tasks(workspace: &mut Workspace) -> Vec<TaskId> {
    let tasks = [
        ("menu-bar-usage", TaskStatus::NeedsYou),
        ("worktree-scripts", TaskStatus::Idle),
        ("ui-tokens", TaskStatus::Working),
        ("daemon-restart-after-crash", TaskStatus::Working),
        ("review-comments", TaskStatus::NeedsYou),
        ("palette-search", TaskStatus::Idle),
        ("settings-pages", TaskStatus::Working),
        ("pull-request-flow", TaskStatus::Idle),
        ("release-notes", TaskStatus::Working),
    ];
    let project = workspace.active_project;

    tasks
        .into_iter()
        .map(|(name, status)| {
            let id = workspace
                .data
                .add_task(project, name, AgentKind::ClaudeCode, Vec::new());
            if let Some(task) = workspace.data.tasks.iter_mut().find(|task| task.id == id) {
                task.status = status;
            }

            id
        })
        .collect()
}

fn open_many_tabs(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    add_many_tasks(workspace);
    cx.notify();
}

fn open_many_tabs_scrolled(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    let ids = add_many_tasks(workspace);

    if let Some(id) = ids.get(6) {
        workspace.select_task(*id, cx);
    }
}

fn open_swarm(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    workspace.set_view(View::Swarm, cx);
}

fn show_pty_io(workspace: &mut Workspace, tab: InspectorTab, cx: &mut Context<Workspace>) {
    let agent = workspace
        .data
        .agents
        .iter()
        .find(|agent| agent.name == "pty-io")
        .map(|agent| agent.id);

    workspace.set_view(View::Swarm, cx);
    if let Some(agent) = agent {
        workspace.select_swarm_agent(agent, cx);
    }
    workspace.select_inspector_tab(tab, cx);
}

fn open_swarm_messages(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    show_pty_io(workspace, InspectorTab::Messages, cx);
}

fn open_swarm_changes(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    show_pty_io(workspace, InspectorTab::Changes, cx);
}

fn open_swarm_terminal(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    show_pty_io(workspace, InspectorTab::Terminal, cx);
}

fn open_review(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    workspace.set_view(View::Review, cx);
}

fn open_review_split(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    workspace.set_view(View::Review, cx);
    workspace.set_review_source(ReviewSource::Merged, cx);
    workspace
        .review
        .update(cx, |review, cx| review.set_style(DiffStyle::Split, cx));
}

/// Opens Review on `source`, scrolled to element.rs. Returns the file's index.
fn review_element(
    workspace: &mut Workspace,
    source: ReviewSource,
    cx: &mut Context<Workspace>,
) -> Option<usize> {
    workspace.set_view(View::Review, cx);
    workspace.set_review_source(source, cx);
    workspace.sync_review(cx);

    let diff = workspace
        .data
        .task_diff(workspace.active_task, source)
        .map(Rc::clone)?;
    let index = diff
        .files
        .iter()
        .position(|file| file.path.ends_with("element.rs"))?;

    workspace
        .review
        .update(cx, |review, cx| review.select_file(index, cx));

    Some(index)
}

fn open_review_comment_box(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    let Some(file) = review_element(workspace, ReviewSource::Merged, cx) else {
        return;
    };
    let added = workspace
        .data
        .task_diff(workspace.active_task, ReviewSource::Merged)
        .and_then(|diff| diff.files.get(file))
        .and_then(|data| data.hunks.first())
        .and_then(|hunk| {
            hunk.lines
                .iter()
                .position(|line| line.kind == swarm_protocol::DiffLineKind::Added)
        });

    if let Some(line) = added {
        let pos = LinePos {
            file: u32::try_from(file).unwrap_or(0),
            hunk: 0,
            line: u32::try_from(line + 1).unwrap_or(0),
            side: swarm_protocol::DiffSide::New,
        };

        workspace
            .review
            .update(cx, |review, cx| review.open_composer(pos, cx));
    }
}

fn open_review_live_comments(
    workspace: &mut Workspace,
    _: &mut Window,
    cx: &mut Context<Workspace>,
) {
    review_element(workspace, ReviewSource::Live, cx);
}

fn open_close_task(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let task = workspace.active_task;

    workspace.open_close_task(task, window, cx);
}

fn open_swarm_terminal_live(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    show_pty_io(workspace, InspectorTab::Terminal, cx);
    crate::demo::attach_terminal(workspace, window, cx);

    let terminal = workspace
        .selected_agent()
        .and_then(|id| workspace.data.agent(id))
        .map(|agent| agent.terminal);
    if let Some(terminal) = terminal {
        workspace.claim_focus(terminal, cx);
    }
}

fn show_empty(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    workspace.use_projects(&[], cx);
}

fn show_unavailable(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    workspace.use_projects(&[], cx);
    workspace.daemon_unavailable(
        "the daemon speaks protocol version 1; stop it with `pkill -x swarm-daemon`".to_owned(),
        cx,
    );
}

const SETTLE: Duration = Duration::from_millis(700);

#[expect(
    clippy::too_many_lines,
    reason = "one flat list of shots reads best in one place"
)]
pub(crate) fn run(directory: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        if let Err(error) = std::fs::create_dir_all(&directory) {
            log::error!("cannot create {}: {error}", directory.display());
            cx.update(|cx| cx.quit());
            return;
        }

        let filter = std::env::var("SWARM_APP_SNAPSHOT_FILTER").ok();

        for (appearance, name, open) in [
            (Appearance::Light, "workspace-light.png", None),
            (Appearance::Dark, "workspace-dark.png", None),
            (
                Appearance::Light,
                "workspace-many-tabs-light.png",
                Some(open_many_tabs as OpenOverlay),
            ),
            (
                Appearance::Dark,
                "workspace-many-tabs-dark.png",
                Some(open_many_tabs),
            ),
            (
                Appearance::Light,
                "workspace-many-tabs-scrolled-light.png",
                Some(open_many_tabs_scrolled),
            ),
            (
                Appearance::Dark,
                "workspace-many-tabs-scrolled-dark.png",
                Some(open_many_tabs_scrolled),
            ),
            (
                Appearance::Light,
                "workspace-island-light.png",
                Some(open_island),
            ),
            (
                Appearance::Dark,
                "workspace-island-dark.png",
                Some(open_island),
            ),
            (
                Appearance::Light,
                "workspace-swarm-light.png",
                Some(open_swarm),
            ),
            (
                Appearance::Dark,
                "workspace-swarm-dark.png",
                Some(open_swarm),
            ),
            (
                Appearance::Light,
                "workspace-swarm-pty-io-light.png",
                Some(open_swarm_messages),
            ),
            (
                Appearance::Dark,
                "workspace-swarm-pty-io-dark.png",
                Some(open_swarm_messages),
            ),
            (
                Appearance::Light,
                "workspace-swarm-changes-light.png",
                Some(open_swarm_changes),
            ),
            (
                Appearance::Dark,
                "workspace-swarm-changes-dark.png",
                Some(open_swarm_changes),
            ),
            (
                Appearance::Light,
                "workspace-swarm-terminal-light.png",
                Some(open_swarm_terminal),
            ),
            (
                Appearance::Dark,
                "workspace-swarm-terminal-dark.png",
                Some(open_swarm_terminal),
            ),
            (
                Appearance::Light,
                "workspace-swarm-terminal-live-light.png",
                Some(open_swarm_terminal_live),
            ),
            (
                Appearance::Dark,
                "workspace-swarm-terminal-live-dark.png",
                Some(open_swarm_terminal_live),
            ),
            (
                Appearance::Light,
                "workspace-start-light.png",
                Some(open_start),
            ),
            (
                Appearance::Dark,
                "workspace-start-dark.png",
                Some(open_start),
            ),
            (
                Appearance::Light,
                "workspace-start-empty-light.png",
                Some(open_start_empty),
            ),
            (
                Appearance::Dark,
                "workspace-start-empty-dark.png",
                Some(open_start_empty),
            ),
            (
                Appearance::Light,
                "workspace-palette-light.png",
                Some(Workspace::toggle_palette as OpenOverlay),
            ),
            (
                Appearance::Dark,
                "workspace-palette-dark.png",
                Some(Workspace::toggle_palette),
            ),
            (
                Appearance::Light,
                "workspace-new-task-light.png",
                Some(Workspace::open_new_task),
            ),
            (
                Appearance::Dark,
                "workspace-new-task-dark.png",
                Some(Workspace::open_new_task),
            ),
            (
                Appearance::Light,
                "workspace-review-light.png",
                Some(open_review),
            ),
            (
                Appearance::Dark,
                "workspace-review-dark.png",
                Some(open_review),
            ),
            (
                Appearance::Light,
                "workspace-review-split-light.png",
                Some(open_review_split),
            ),
            (
                Appearance::Dark,
                "workspace-review-split-dark.png",
                Some(open_review_split),
            ),
            (
                Appearance::Light,
                "workspace-review-comment-light.png",
                Some(open_review_comment_box),
            ),
            (
                Appearance::Dark,
                "workspace-review-comment-dark.png",
                Some(open_review_comment_box),
            ),
            (
                Appearance::Light,
                "workspace-review-live-comments-light.png",
                Some(open_review_live_comments),
            ),
            (
                Appearance::Dark,
                "workspace-review-live-comments-dark.png",
                Some(open_review_live_comments),
            ),
            (
                Appearance::Light,
                "workspace-close-task-light.png",
                Some(open_close_task),
            ),
            (
                Appearance::Dark,
                "workspace-close-task-dark.png",
                Some(open_close_task),
            ),
            (
                Appearance::Light,
                "workspace-empty-light.png",
                Some(show_empty),
            ),
            (
                Appearance::Dark,
                "workspace-empty-dark.png",
                Some(show_empty),
            ),
            (
                Appearance::Light,
                "workspace-unavailable-light.png",
                Some(show_unavailable),
            ),
            (
                Appearance::Dark,
                "workspace-unavailable-dark.png",
                Some(show_unavailable),
            ),
        ] {
            if filter
                .as_ref()
                .is_some_and(|filter| !name.contains(filter.as_str()))
            {
                continue;
            }

            cx.update(|cx| theme::set_appearance(appearance, cx));
            snap(&directory.join(name), open, cx).await;
        }

        cx.update(|cx| cx.quit());
    })
    .detach();
}

async fn snap(path: &Path, open: Option<OpenOverlay>, cx: &mut AsyncApp) {
    let opened = cx.update(crate::open_workspace);

    // AnyWindowHandle: the typed handle leases the root view.
    let window: AnyWindowHandle = match opened {
        Ok(handle) => handle.into(),
        Err(error) => {
            log::error!("cannot open window: {error:#}");
            return;
        }
    };

    cx.background_executor().timer(SETTLE).await;

    if let Some(open) = open {
        let opened = window.update(cx, |root, window, cx| {
            if let Ok(workspace) = root.downcast::<Workspace>() {
                workspace.update(cx, |workspace, cx| open(workspace, window, cx));
            } else {
                log::error!("root is not a Workspace");
            }
        });

        if let Err(error) = opened {
            log::error!("cannot open the overlay: {error:#}");
        }

        // The window is not on screen, so no frame is drawn unless we draw one.
        if let Err(error) = window.update(cx, |_, window, cx| window.draw(cx).clear(cx)) {
            log::error!("cannot draw: {error:#}");
        }

        cx.background_executor().timer(SETTLE).await;

        // Content that arrives after the first frame, like a terminal's screen, needs another.
        if let Err(error) = window.update(cx, |_, window, cx| window.draw(cx).clear(cx)) {
            log::error!("cannot draw: {error:#}");
        }
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
