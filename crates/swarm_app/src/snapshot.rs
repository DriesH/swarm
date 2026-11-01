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
use swarm_protocol::{
    CheckState, CloseCheck, RemoveCheck, ReviewSource, TaskCloseCheck, TaskId as DaemonTaskId,
    Usage, UsageProblem,
};
use swarm_ui::theme::{self, Appearance};

use crate::close_task::CheckState as DialogCheck;
use crate::model::{AgentKind, ProjectId, SAMPLE_NOW, TaskId, TaskStatus};
use crate::palette::Overlay;
use crate::remove_project::RemoveProjectDialog;
use crate::review_model::{DiffStyle, LinePos};
use crate::swarm_view::InspectorTab;
use crate::workspace::{RightPanel, View, Workspace};

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

/// The menu of the "website" row, opened as the keyboard does: the row has focus and
/// Shift-F10 is pressed. The projects get folders, so Show in Finder is on.
fn open_project_menu(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    for project in &mut workspace.data.projects {
        project.root = Some(Path::new("/Users/sam/personal").join(&*project.name));
    }

    let Some(row) = workspace.project_focus.get(&ProjectId(1)).cloned() else {
        log::error!("the project row has no focus handle");
        return;
    };

    window.focus(&row, cx);
    window.dispatch_action(Box::new(swarm_ui::ShowContextMenu), cx);
}

/// A folder under the home folder for each project, so the Remove dialog can say where the
/// code stays.
fn give_project_folders(workspace: &mut Workspace) {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();

    for project in &mut workspace.data.projects {
        project.root = Some(home.join("personal").join(&*project.name));
    }
}

fn close_check(
    running_agents: u32,
    user_terminals: u32,
    uncommitted_files: u32,
    merged_branches: &[&str],
) -> CloseCheck {
    CloseCheck {
        running_agents,
        user_terminals,
        uncommitted_files,
        unmerged_workers: Vec::new(),
        merged_branches: merged_branches
            .iter()
            .map(|&branch| branch.to_owned())
            .collect(),
    }
}

/// The Remove dialog of "swarm", with what the daemon would find in its three tasks. The
/// first one has uncommitted files.
fn open_remove_project_with_tasks(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    give_project_folders(workspace);
    workspace.open_remove_project(ProjectId(0), window, cx);

    let task = |id, name: &str, check| TaskCloseCheck {
        task: DaemonTaskId(id),
        name: name.to_owned(),
        check,
    };
    let check = RemoveCheck {
        tasks: vec![
            task(0, "terminal-renderer", close_check(4, 1, 3, &[])),
            task(
                1,
                "menu-bar-usage",
                close_check(2, 0, 0, &["swarm/menu-bar-usage"]),
            ),
            task(2, "worktree-scripts", close_check(1, 0, 0, &[])),
        ],
    };

    if let Some(Overlay::RemoveProject { view, .. }) = &workspace.overlay {
        view.update(cx, |dialog, cx| {
            dialog.set_check(DialogCheck::Ready(check), cx);
        });
    }
}

/// The same dialog while the project goes.
fn open_remove_project_busy(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    open_remove_project_with_tasks(workspace, window, cx);

    if let Some(Overlay::RemoveProject { view, .. }) = &workspace.overlay {
        view.update(cx, RemoveProjectDialog::set_busy);
    }
}

/// The Remove dialog of a project with no open task.
fn open_remove_project(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    give_project_folders(workspace);
    workspace.open_remove_project(ProjectId(3), window, cx);
}

/// The last Codex read failed: its card keeps the old numbers and says so.
fn open_usage_failed(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    let mut usage = workspace.data.usage.clone();
    usage.codex.problem = Some(UsageProblem::Failed(
        "the app server did not answer".to_owned(),
    ));

    workspace.usage_changed(usage, cx);
}

/// Claude's session is full. Codex was read an hour and a half ago, and at that rate its
/// session has likely hit the limit by now.
pub(crate) fn reach_limits(usage: &mut Usage) {
    let now = SAMPLE_NOW / 1_000;

    usage.claude.windows[0].used_percent = 100;

    usage.codex.windows[0].used_percent = 40;
    usage.codex.read_at = Some(now - 90 * 60);
}

fn open_usage_limits(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    let mut usage = workspace.data.usage.clone();
    reach_limits(&mut usage);

    workspace.usage_changed(usage, cx);
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

/// The Terminals view with the Changes panel: one folder closed and one file under the cursor.
fn open_changes_tree(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    workspace.set_view(View::Terminals, cx);
    workspace.right_panel = Some(RightPanel::Changes);
    workspace.sync_changes_list(cx);
    workspace
        .changes_list
        .update(cx, crate::panels::ChangesList::mark_for_snapshot);
}

/// The Terminals view with the Files panel.
fn open_files_panel(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    workspace.set_view(View::Terminals, cx);
    workspace.right_panel = Some(RightPanel::Files);
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

/// Opens Review on Merged, where the folded lock file is the first card.
fn open_review_folded(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    workspace.set_view(View::Review, cx);
    workspace.set_review_source(ReviewSource::Merged, cx);
}

/// Merged, with the large file loaded and on screen.
fn open_review_loaded(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    workspace.set_view(View::Review, cx);
    workspace.set_review_source(ReviewSource::Merged, cx);
    workspace.sync_review(cx);

    let Some(diff) = workspace
        .data
        .task_diff(workspace.active_task, ReviewSource::Merged)
        .map(Rc::clone)
    else {
        return;
    };
    let Some((index, file)) = diff
        .files
        .iter()
        .enumerate()
        .find(|(_, file)| file.path.ends_with("glyphs.rs"))
    else {
        return;
    };

    workspace.show_file_diff(file.worktree.clone(), file.path.clone(), cx);
    workspace.sync_review(cx);
    workspace
        .review
        .update(cx, |review, cx| review.select_file(index, cx));
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

/// Changes a sample comment as a daemon event would.
fn change_comment(
    workspace: &mut Workspace,
    id: swarm_protocol::CommentId,
    change: impl FnOnce(&mut swarm_protocol::ReviewComment),
    cx: &mut Context<Workspace>,
) {
    let found = workspace
        .data
        .task_comments(workspace.active_task)
        .iter()
        .find(|comment| comment.id == id)
        .cloned();

    if let Some(mut comment) = found {
        change(&mut comment);
        workspace.comment_event(
            &swarm_protocol::CommentEvent::Changed(Box::new(comment)),
            cx,
        );
    }
}

/// Opens Review on Live with the sample thread 3 on screen. `reply` opens its reply field.
fn show_live_thread(
    workspace: &mut Workspace,
    id: swarm_protocol::CommentId,
    reply: Option<&str>,
    cx: &mut Context<Workspace>,
) {
    review_element(workspace, ReviewSource::Live, cx);
    workspace
        .review
        .update(cx, |review, cx| review.show_thread(id, reply, cx));
}

/// The sent thread that the orchestrator has not got yet. Its replies are in flight too.
fn open_review_thread_in_flight(
    workspace: &mut Workspace,
    _: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let id = swarm_protocol::CommentId(3);
    let thread: Vec<_> = workspace
        .data
        .task_comments(workspace.active_task)
        .iter()
        .filter(|comment| comment.id == id || comment.parent == Some(id))
        .map(|comment| comment.id)
        .collect();

    for comment in thread {
        change_comment(
            workspace,
            comment,
            |comment| comment.delivered_at = None,
            cx,
        );
    }
    show_live_thread(workspace, id, None, cx);
}

/// A delivered thread with its reply field open.
fn open_review_thread_reply(
    workspace: &mut Workspace,
    _: &mut Window,
    cx: &mut Context<Workspace>,
) {
    show_live_thread(
        workspace,
        swarm_protocol::CommentId(3),
        Some("Done in the next commit."),
        cx,
    );
}

/// A resolved thread, opened.
fn open_review_thread_resolved(
    workspace: &mut Workspace,
    _: &mut Window,
    cx: &mut Context<Workspace>,
) {
    show_live_thread(workspace, swarm_protocol::CommentId(5), None, cx);
}

/// Approves the open task's Merged diff and drops the sample's pending comments.
fn approve_sample(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    let task = workspace.active_task;
    let fingerprint = workspace.data.merged_fingerprint(task);

    if let Some(entry) = workspace
        .data
        .tasks
        .iter_mut()
        .find(|entry| entry.id == task)
    {
        entry.approved = fingerprint;
    }
    workspace.data.comments.clear();
    workspace.set_view(View::Review, cx);
    workspace.set_review_source(ReviewSource::Merged, cx);
    workspace.right_panel = Some(RightPanel::PullRequest);
}

/// A new pull request: the orchestrator's draft and local checks that passed.
fn open_pull_request_new(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    approve_sample(workspace, cx);

    if let Some(id) = workspace.data.diff_id(workspace.active_task) {
        workspace
            .data
            .set_checks(crate::model::sample_checks_run(id, "sample"));
    }
}

/// A new pull request whose local checks failed: the one after the failure was skipped.
fn open_pull_request_checks_failed(
    workspace: &mut Workspace,
    _: &mut Window,
    cx: &mut Context<Workspace>,
) {
    approve_sample(workspace, cx);

    if let Some(id) = workspace.data.diff_id(workspace.active_task) {
        let mut run = crate::model::sample_checks_run(id, "sample");

        if let [_, failed, skipped] = run.checks.as_mut_slice() {
            failed.state = CheckState::Failed;
            failed.exit = Some(swarm_term::ExitStatus::Code(101));
            skipped.state = CheckState::Skipped;
            skipped.exit = None;
            skipped.duration_ms = None;
        }

        workspace.data.set_checks(run);
    }
}

/// A pull request #42 whose GitHub checks run.
fn open_pull_request_status(
    workspace: &mut Workspace,
    _: &mut Window,
    cx: &mut Context<Workspace>,
) {
    approve_sample(workspace, cx);
    workspace.set_view(View::Terminals, cx);

    let task = workspace.active_task;
    let Some(id) = workspace.data.diff_id(task) else {
        return;
    };
    let pull_request = crate::model::sample_pull_request();

    if let Some(entry) = workspace
        .data
        .tasks
        .iter_mut()
        .find(|entry| entry.id == task)
    {
        entry.pull_request = Some(swarm_protocol::PrLink {
            number: pull_request.number,
            url: pull_request.url.clone(),
            head: "sample".to_owned(),
        });
    }
    workspace.data.pull_requests.insert(id, pull_request);
}

/// The pull request is open and the branch has a commit that was not pushed yet.
fn open_pull_request_update(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    open_pull_request_status(workspace, window, cx);

    let task = workspace.active_task;
    let Some(id) = workspace.data.diff_id(task) else {
        return;
    };

    if let Some(link) = workspace
        .data
        .tasks
        .iter_mut()
        .find(|entry| entry.id == task)
        .and_then(|entry| entry.pull_request.as_mut())
    {
        "pushed".clone_into(&mut link.head);
    }
    if let Some(status) = workspace.data.pull_requests.get_mut(&id) {
        status.draft = false;
        status.mergeable = true;
        status.checks.clear();
    }
    workspace
        .data
        .set_checks(crate::model::sample_checks_run(id, "sample"));
}

/// GitHub CLI is missing: Review and the panel offer Set up GitHub.
fn open_pull_request_setup(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    approve_sample(workspace, cx);

    let project = workspace.active_project;
    let mut github = crate::model::sample_github();
    github.gh = None;
    github.signed_in = false;
    workspace.data.github.insert(project, github);
}

/// The install step runs in a user terminal.
fn open_pull_request_setup_running(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    open_pull_request_setup(workspace, window, cx);

    let project = workspace.active_project;
    workspace.pull_requests.setups.insert(
        project,
        crate::pull_request::GithubSetup::Running(crate::pull_request::SetupStep::Install),
    );
}

/// The sign-in step failed.
fn open_pull_request_setup_failed(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    open_pull_request_setup(workspace, window, cx);

    let project = workspace.active_project;
    workspace.pull_requests.setups.insert(
        project,
        crate::pull_request::GithubSetup::Failed {
            step: crate::pull_request::SetupStep::SignIn,
            reason: "exit 1".into(),
            tile: None,
        },
    );
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

/// Every tile of the open task runs a live terminal at the font size the settings hold.
fn open_terminal_font(
    points: u8,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    crate::settings::set_terminal_font_size(points, cx);
    crate::demo::attach_task_terminals(workspace, window, cx);
    cx.notify();
}

fn open_font_13(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    open_terminal_font(13, workspace, window, cx);
}

fn open_font_16(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    open_terminal_font(16, workspace, window, cx);
}

fn show_empty(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    workspace.use_projects(&[], cx);
}

/// The welcome screen with sample rows: the GitHub CLI installs, and its terminal shows.
fn show_welcome(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    // The settings have no flag yet, and the app has no project.
    crate::settings::state(cx);
    workspace.data = crate::model::FakeData::live();

    let Some(welcome) = workspace.welcome_screen(window, cx) else {
        log::error!("the welcome screen does not show");
        return;
    };
    let check = welcome.read(cx).check().clone();
    let output = crate::demo::install_output(cx);

    check.update(cx, |check, cx| check.show_sample_output(output, window, cx));
}

fn show_unavailable(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    workspace.use_projects(&[], cx);
    workspace.daemon_unavailable(
        "the daemon speaks protocol version 1; stop it with `pkill -x swarm-daemon`".to_owned(),
        cx,
    );
}

/// Every egg setting back to the default, so a shot depends only on what it sets.
fn plain_eggs(cx: &mut Context<Workspace>) {
    crate::settings::change_appearance(
        |appearance| *appearance = swarm_settings::AppearanceSettings::default(),
        cx,
    );
    theme::set_accent(theme::Accent::Blue, cx);
    cx.set_reduce_motion(false);
}

/// The honeycomb of the empty state: the app has no project.
fn open_egg_no_projects(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    plain_eggs(cx);
    workspace.use_projects(&[], cx);
}

/// A project with no task: its Start view sits on the honeycomb.
fn open_egg_start_view(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    plain_eggs(cx);
    open_start_empty(workspace, window, cx);
}

/// The Swarm view with the honeycomb the Appearance page offers.
fn open_egg_swarm_honeycomb(
    workspace: &mut Workspace,
    _: &mut Window,
    cx: &mut Context<Workspace>,
) {
    plain_eggs(cx);
    crate::settings::change_appearance(|appearance| appearance.swarm_view_honeycomb = true, cx);
    workspace.set_view(View::Swarm, cx);
}

/// The Swarm view with the dots, after the eggs went off.
fn open_egg_swarm_dots(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    plain_eggs(cx);
    crate::settings::change_appearance(
        |appearance| {
            appearance.swarm_view_honeycomb = true;
            appearance.easter_eggs = false;
        },
        cx,
    );
    workspace.set_view(View::Swarm, cx);
}

/// "Honey's ready" with its honeycomb filled, as it looks under Reduce Motion.
fn open_egg_toast(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    plain_eggs(cx);
    cx.set_reduce_motion(true);
    workspace.show_toast_for_snapshot("terminal-renderer", cx);
}

/// The same toast with the eggs off: no honeycomb.
fn open_egg_toast_without_eggs(
    workspace: &mut Workspace,
    _: &mut Window,
    cx: &mut Context<Workspace>,
) {
    plain_eggs(cx);
    crate::settings::change_appearance(|appearance| appearance.easter_eggs = false, cx);
    cx.set_reduce_motion(true);
    workspace.show_toast_for_snapshot("terminal-renderer", cx);
}

/// The Start orchestrator with its crown, an hour after the 100th merge.
fn open_egg_queen(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    plain_eggs(cx);
    open_start(workspace, window, cx);
    workspace.eggs.stats = swarm_protocol::SwarmStats {
        running_agents: 4,
        open_tasks: 3,
        merged_prs: 100,
        hundredth_merge_at: Some(SAMPLE_NOW - 3_600_000),
    };
}

/// The bees of the buzz, caught in the air.
fn open_egg_buzz(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    plain_eggs(cx);
    workspace.freeze_flight_for_snapshot(0.42, cx);
}

/// The Honey accent on the open workspace.
fn open_egg_honey_accent(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    plain_eggs(cx);
    theme::set_accent(theme::Accent::Honey, cx);
    workspace.set_view(View::Swarm, cx);
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
                "workspace-usage-failed-light.png",
                Some(open_usage_failed as OpenOverlay),
            ),
            (
                Appearance::Dark,
                "workspace-usage-failed-dark.png",
                Some(open_usage_failed),
            ),
            (
                Appearance::Light,
                "workspace-usage-limits-light.png",
                Some(open_usage_limits as OpenOverlay),
            ),
            (
                Appearance::Dark,
                "workspace-usage-limits-dark.png",
                Some(open_usage_limits),
            ),
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
                "workspace-changes-tree-light.png",
                Some(open_changes_tree),
            ),
            (
                Appearance::Dark,
                "workspace-changes-tree-dark.png",
                Some(open_changes_tree),
            ),
            (
                Appearance::Light,
                "workspace-files-panel-light.png",
                Some(open_files_panel),
            ),
            (
                Appearance::Dark,
                "workspace-files-panel-dark.png",
                Some(open_files_panel),
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
                "workspace-project-menu-light.png",
                Some(open_project_menu),
            ),
            (
                Appearance::Dark,
                "workspace-project-menu-dark.png",
                Some(open_project_menu),
            ),
            (
                Appearance::Light,
                "workspace-remove-project-light.png",
                Some(open_remove_project),
            ),
            (
                Appearance::Dark,
                "workspace-remove-project-dark.png",
                Some(open_remove_project),
            ),
            (
                Appearance::Light,
                "workspace-remove-project-tasks-light.png",
                Some(open_remove_project_with_tasks),
            ),
            (
                Appearance::Dark,
                "workspace-remove-project-tasks-dark.png",
                Some(open_remove_project_with_tasks),
            ),
            (
                Appearance::Light,
                "workspace-remove-project-busy-light.png",
                Some(open_remove_project_busy),
            ),
            (
                Appearance::Dark,
                "workspace-remove-project-busy-dark.png",
                Some(open_remove_project_busy),
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
                "workspace-review-folded-light.png",
                Some(open_review_folded),
            ),
            (
                Appearance::Dark,
                "workspace-review-folded-dark.png",
                Some(open_review_folded),
            ),
            (
                Appearance::Light,
                "workspace-review-loaded-light.png",
                Some(open_review_loaded),
            ),
            (
                Appearance::Dark,
                "workspace-review-loaded-dark.png",
                Some(open_review_loaded),
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
                "workspace-review-thread-in-flight-light.png",
                Some(open_review_thread_in_flight),
            ),
            (
                Appearance::Dark,
                "workspace-review-thread-in-flight-dark.png",
                Some(open_review_thread_in_flight),
            ),
            (
                Appearance::Light,
                "workspace-review-thread-reply-light.png",
                Some(open_review_thread_reply),
            ),
            (
                Appearance::Dark,
                "workspace-review-thread-reply-dark.png",
                Some(open_review_thread_reply),
            ),
            (
                Appearance::Light,
                "workspace-review-thread-resolved-light.png",
                Some(open_review_thread_resolved),
            ),
            (
                Appearance::Dark,
                "workspace-review-thread-resolved-dark.png",
                Some(open_review_thread_resolved),
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
                "workspace-pull-request-new-light.png",
                Some(open_pull_request_new),
            ),
            (
                Appearance::Dark,
                "workspace-pull-request-new-dark.png",
                Some(open_pull_request_new),
            ),
            (
                Appearance::Light,
                "workspace-pull-request-status-light.png",
                Some(open_pull_request_status),
            ),
            (
                Appearance::Dark,
                "workspace-pull-request-status-dark.png",
                Some(open_pull_request_status),
            ),
            (
                Appearance::Light,
                "workspace-pull-request-update-light.png",
                Some(open_pull_request_update),
            ),
            (
                Appearance::Dark,
                "workspace-pull-request-update-dark.png",
                Some(open_pull_request_update),
            ),
            (
                Appearance::Light,
                "workspace-pull-request-checks-failed-light.png",
                Some(open_pull_request_checks_failed),
            ),
            (
                Appearance::Dark,
                "workspace-pull-request-checks-failed-dark.png",
                Some(open_pull_request_checks_failed),
            ),
            (
                Appearance::Light,
                "workspace-pull-request-setup-light.png",
                Some(open_pull_request_setup),
            ),
            (
                Appearance::Dark,
                "workspace-pull-request-setup-dark.png",
                Some(open_pull_request_setup),
            ),
            (
                Appearance::Light,
                "workspace-pull-request-setup-running-light.png",
                Some(open_pull_request_setup_running),
            ),
            (
                Appearance::Dark,
                "workspace-pull-request-setup-running-dark.png",
                Some(open_pull_request_setup_running),
            ),
            (
                Appearance::Light,
                "workspace-pull-request-setup-failed-light.png",
                Some(open_pull_request_setup_failed),
            ),
            (
                Appearance::Dark,
                "workspace-pull-request-setup-failed-dark.png",
                Some(open_pull_request_setup_failed),
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
                "workspace-welcome-light.png",
                Some(show_welcome),
            ),
            (
                Appearance::Dark,
                "workspace-welcome-dark.png",
                Some(show_welcome),
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
            (
                Appearance::Light,
                "workspace-font-13-light.png",
                Some(open_font_13),
            ),
            (
                Appearance::Light,
                "workspace-font-16-light.png",
                Some(open_font_16),
            ),
            (
                Appearance::Dark,
                "workspace-font-16-dark.png",
                Some(open_font_16),
            ),
            (
                Appearance::Light,
                "egg-no-projects-light.png",
                Some(open_egg_no_projects),
            ),
            (
                Appearance::Dark,
                "egg-no-projects-dark.png",
                Some(open_egg_no_projects),
            ),
            (
                Appearance::Light,
                "egg-start-view-light.png",
                Some(open_egg_start_view),
            ),
            (
                Appearance::Dark,
                "egg-start-view-dark.png",
                Some(open_egg_start_view),
            ),
            (
                Appearance::Light,
                "egg-swarm-honeycomb-light.png",
                Some(open_egg_swarm_honeycomb),
            ),
            (
                Appearance::Dark,
                "egg-swarm-honeycomb-dark.png",
                Some(open_egg_swarm_honeycomb),
            ),
            (
                Appearance::Light,
                "egg-swarm-dots-eggs-off-light.png",
                Some(open_egg_swarm_dots),
            ),
            (
                Appearance::Dark,
                "egg-swarm-dots-eggs-off-dark.png",
                Some(open_egg_swarm_dots),
            ),
            (
                Appearance::Light,
                "egg-toast-light.png",
                Some(open_egg_toast),
            ),
            (Appearance::Dark, "egg-toast-dark.png", Some(open_egg_toast)),
            (
                Appearance::Light,
                "egg-toast-eggs-off-light.png",
                Some(open_egg_toast_without_eggs),
            ),
            (
                Appearance::Dark,
                "egg-toast-eggs-off-dark.png",
                Some(open_egg_toast_without_eggs),
            ),
            (
                Appearance::Light,
                "egg-queen-light.png",
                Some(open_egg_queen),
            ),
            (Appearance::Dark, "egg-queen-dark.png", Some(open_egg_queen)),
            (Appearance::Light, "egg-buzz-light.png", Some(open_egg_buzz)),
            (Appearance::Dark, "egg-buzz-dark.png", Some(open_egg_buzz)),
            (
                Appearance::Light,
                "egg-honey-accent-light.png",
                Some(open_egg_honey_accent),
            ),
            (
                Appearance::Dark,
                "egg-honey-accent-dark.png",
                Some(open_egg_honey_accent),
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
