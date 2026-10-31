//! The pull request panels and Review's next step, against a fake daemon and sample data.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use swarm_protocol::{
    CheckState, ChecksRun, Client, Event, GithubCheck, GithubCheckState, GithubStatus, LocalCheck,
    MergeMethod, PrDraft, PrEvent, PrLink, PrPanel, PrState, PullRequest, Request, ReviewSource,
    TaskDiff, TaskId as DaemonTaskId, TerminalId as DaemonTerminalId, TerminalInfo, TerminalKind,
};
use swarm_term::ExitStatus;
use swarm_term_view::TerminalEvent;

use super::*;
use crate::daemon::tests::{
    diff_of, fake_daemon_replying, pending_comment, real_task_workspace, redraw, wait_a_little,
    wait_until,
};
use crate::model::{sample_github, sample_pull_request};

const TASK: DaemonTaskId = DaemonTaskId(7);
const APP_TASK: TaskId = TaskId(0);
const APP_PROJECT: ProjectId = ProjectId(0);
const MERGED: u64 = 9;

/// Every request a fake daemon got.
struct Seen {
    requests: flume::Receiver<Request>,
    all: RefCell<Vec<Request>>,
}

impl Seen {
    fn drain(&self) {
        self.all.borrow_mut().extend(self.requests.try_iter());
    }

    fn count(&self, matching: impl Fn(&Request) -> bool) -> usize {
        self.drain();

        self.all
            .borrow()
            .iter()
            .filter(|request| matching(request))
            .count()
    }

    fn first<T>(&self, pick: impl Fn(&Request) -> Option<T>) -> Option<T> {
        self.drain();

        self.all.borrow().iter().find_map(pick)
    }
}

/// A daemon that records every request and answers with `reply`, else with the defaults.
fn recording_daemon(reply: impl Fn(&Request) -> Option<Event> + Send + 'static) -> (PathBuf, Seen) {
    let (sent, requests) = flume::unbounded();
    let socket = fake_daemon_replying(move |request| {
        let event = reply(&request);
        sent.send(request).ok();

        event
    });

    (
        socket,
        Seen {
            requests,
            all: RefCell::new(Vec::new()),
        },
    )
}

fn github_reply(request: &Request) -> Option<Event> {
    match request {
        Request::GithubStatus { call, .. } => Some(Event::GithubStatus {
            call: *call,
            status: sample_github(),
        }),
        _ => None,
    }
}

fn wait_for(
    cx: &mut VisualTestContext,
    seen: &Seen,
    done: impl Fn(&Seen, &VisualTestContext) -> bool,
) -> bool {
    wait_until(cx, |cx| {
        redraw(cx);

        done(seen, cx)
    })
}

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let center = cx.debug_bounds(selector).map(|bounds| bounds.center());

    assert!(center.is_some(), "{selector} is not on screen");
    if let Some(center) = center {
        cx.simulate_click(center, Modifiers::none());
    }
    redraw(cx);
}

/// A task that is approved on its loaded Merged diff, in a project on GitHub, with the
/// pull request panel open.
fn approved<'a>(
    socket: &std::path::Path,
    cx: &'a mut TestAppContext,
) -> (Entity<Workspace>, &'a mut VisualTestContext, Client) {
    let (workspace, cx, client) = real_task_workspace(socket, cx);
    cx.update(|window, _| window.activate_window());

    workspace.update(cx, |workspace, cx| {
        workspace
            .data
            .store_diff(diff_of(7, ReviewSource::Merged, MERGED));
        workspace.data.github.insert(APP_PROJECT, sample_github());
        workspace.data.comments.clear();

        if let Some(task) = workspace.data.tasks.first_mut() {
            task.approved = Some(MERGED);
        }

        workspace.right_panel = Some(RightPanel::PullRequest);
        cx.notify();
    });
    redraw(cx);

    (workspace, cx, client)
}

fn text_of(
    workspace: &Entity<Workspace>,
    cx: &VisualTestContext,
    pick: impl Fn(&PrForm) -> Entity<TextInput>,
) -> String {
    workspace.read_with(cx, |workspace, cx| {
        workspace
            .pull_requests
            .form(APP_TASK)
            .map(|form| pick(form).read(cx).text().to_string())
            .unwrap_or_default()
    })
}

fn title_of(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> String {
    text_of(workspace, cx, |form| form.title.clone())
}

fn body_of(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> String {
    workspace.read_with(cx, |workspace, cx| {
        workspace
            .pull_requests
            .form(APP_TASK)
            .map(|form| form.description.read(cx).text().to_string())
            .unwrap_or_default()
    })
}

fn type_title(workspace: &Entity<Workspace>, text: &str, cx: &mut VisualTestContext) {
    let title = workspace.read_with(cx, |workspace, _| {
        workspace
            .pull_requests
            .form(APP_TASK)
            .map(|form| form.title.clone())
    });

    if let Some(title) = title {
        title.update(cx, |input, cx| input.set_text(text.to_owned(), cx));
    }
    redraw(cx);
}

fn draft(title: &str, body: &str) -> PrDraft {
    PrDraft {
        title: title.to_owned(),
        body: body.to_owned(),
    }
}

fn send_draft(workspace: &Entity<Workspace>, draft: PrDraft, cx: &mut VisualTestContext) {
    workspace.update(cx, |workspace, cx| {
        workspace.pr_event(PrEvent::Draft { task: TASK, draft }, cx);
    });
    redraw(cx);
}

fn local_check(command: &str, state: CheckState) -> LocalCheck {
    LocalCheck {
        command: command.to_owned(),
        state,
        terminal: None,
        exit: matches!(state, CheckState::Passed | CheckState::Failed)
            .then(|| ExitStatus::Code(u32::from(state == CheckState::Failed))),
        duration_ms: Some(36_000),
    }
}

fn checks_run(commit: &str, required: bool, checks: Vec<LocalCheck>) -> ChecksRun {
    ChecksRun {
        task: TASK,
        commit: commit.to_owned(),
        clean: true,
        required,
        checks,
        pre_push_hook: Some("husky".to_owned()),
    }
}

fn send_checks(workspace: &Entity<Workspace>, run: ChecksRun, cx: &mut VisualTestContext) {
    workspace.update(cx, |workspace, cx| {
        workspace.pr_event(PrEvent::Checks(Box::new(run)), cx);
    });
    redraw(cx);
}

fn create_state_of(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> CreateState {
    workspace.read_with(cx, |workspace, cx| workspace.create_state(APP_TASK, cx))
}

#[gpui::test]
fn opening_the_panel_reads_github_and_the_panel_data_then_asks_for_one_draft(
    cx: &mut TestAppContext,
) {
    let (socket, seen) = recording_daemon(github_reply);
    let (workspace, cx, _client) = approved(&socket, cx);

    let asked = wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::GithubStatus { .. })) >= 1
            && seen.count(|request| matches!(request, Request::PrPanel { .. })) >= 1
            && seen.count(|request| matches!(request, Request::DraftPr { .. })) >= 1
    });
    for _ in 0..4 {
        redraw(cx);
    }

    let (repository, waiting) = workspace.read_with(cx, |workspace, _| {
        (
            workspace
                .data
                .github
                .get(&APP_PROJECT)
                .and_then(|status| status.repository.clone()),
            workspace
                .pull_requests
                .form(APP_TASK)
                .map(|form| form.draft_wait.clone()),
        )
    });
    let order: Vec<&str> = {
        seen.drain();
        seen.all
            .borrow()
            .iter()
            .filter_map(|request| match request {
                Request::PrPanel { .. } => Some("panel"),
                Request::DraftPr { .. } => Some("draft"),
                _ => None,
            })
            .collect()
    };
    assert!(asked);
    assert_eq!(
        seen.count(|request| matches!(request, Request::DraftPr { .. })),
        1
    );
    assert_eq!(
        seen.count(|request| matches!(request, Request::PrPanel { .. })),
        1
    );
    assert_eq!(order, ["panel", "draft"], "the stored draft comes first");
    assert_eq!(repository.as_deref(), Some("swarm-labs/swarm"));
    assert_eq!(waiting, Some(DraftWait::Waiting));
}

#[gpui::test]
fn a_draft_fills_the_fields_and_a_later_one_never_overwrites_the_users_text(
    cx: &mut TestAppContext,
) {
    let (socket, _seen) = recording_daemon(github_reply);
    let (workspace, cx, _client) = approved(&socket, cx);

    send_draft(&workspace, draft("first title", "first body"), cx);
    let filled = (title_of(&workspace, cx), body_of(&workspace, cx));

    send_draft(&workspace, draft("second title", "second body"), cx);
    let replaced = title_of(&workspace, cx);

    type_title(&workspace, "my own title", cx);
    send_draft(&workspace, draft("third title", "third body"), cx);
    let kept = (title_of(&workspace, cx), body_of(&workspace, cx));
    let held = workspace.read_with(cx, |workspace, _| {
        workspace
            .pull_requests
            .form(APP_TASK)
            .and_then(|form| form.newer.clone())
    });
    let button = cx.debug_bounds("new-draft").is_some();

    click("new-draft", cx);
    let used = (title_of(&workspace, cx), body_of(&workspace, cx));
    let gone = cx.debug_bounds("new-draft").is_none();

    assert_eq!(filled, ("first title".to_owned(), "first body".to_owned()));
    assert_eq!(
        replaced, "second title",
        "untouched fields take a new draft"
    );
    assert_eq!(kept.0, "my own title");
    assert_eq!(kept.1, "second body");
    assert_eq!(held, Some(draft("third title", "third body")));
    assert!(button, "the new draft waits behind a button");
    assert_eq!(used, ("third title".to_owned(), "third body".to_owned()));
    assert!(gone);
}

#[gpui::test]
fn a_description_the_user_typed_before_any_draft_is_kept_too(cx: &mut TestAppContext) {
    let (socket, _seen) = recording_daemon(github_reply);
    let (workspace, cx, _client) = approved(&socket, cx);

    type_title(&workspace, "typed first", cx);
    send_draft(&workspace, draft("drafted", "body"), cx);

    assert_eq!(title_of(&workspace, cx), "typed first");
    assert!(cx.debug_bounds("new-draft").is_some());
}

#[gpui::test]
fn a_refused_draft_shows_the_reason_and_the_fields_stay_empty(cx: &mut TestAppContext) {
    let (socket, seen) = recording_daemon(|request| match request {
        Request::DraftPr { call, .. } => Some(Event::Error {
            call: Some(*call),
            id: None,
            message: "the orchestrator is stopped".into(),
        }),
        other => github_reply(other),
    });
    let (workspace, cx, _client) = approved(&socket, cx);

    let refused = wait_for(cx, &seen, |_, cx| {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .pull_requests
                .form(APP_TASK)
                .is_some_and(|form| matches!(form.draft_wait, DraftWait::Refused(_)))
        })
    });

    let reason = workspace.read_with(cx, |workspace, _| {
        workspace
            .pull_requests
            .form(APP_TASK)
            .map(|form| form.draft_wait.clone())
    });
    assert!(refused);
    assert_eq!(
        reason,
        Some(DraftWait::Refused("the orchestrator is stopped".into()))
    );
    assert_eq!(title_of(&workspace, cx), "");
}

fn merged_at(head: &str, fingerprint: u64) -> TaskDiff {
    TaskDiff {
        head: head.to_owned(),
        ..diff_of(7, ReviewSource::Merged, fingerprint)
    }
}

/// Why the loaded run does not let a pull request through.
fn problem_of(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> Option<Blocker> {
    workspace.read_with(cx, |workspace, _| {
        let run = workspace
            .data
            .checks_of(APP_TASK)
            .and_then(|checks| checks.run.as_ref())?;

        run_problem(run, workspace.data.merged_head(APP_TASK))
    })
}

#[gpui::test]
fn the_checks_list_shows_a_row_each_and_a_new_head_makes_it_stale(cx: &mut TestAppContext) {
    let run = checks_run(
        "abc",
        true,
        vec![
            local_check("cargo test --workspace", CheckState::Passed),
            local_check("cargo clippy", CheckState::Passed),
            local_check("cargo fmt --check", CheckState::Passed),
        ],
    );
    let panel = PrPanel {
        run: Some(run.clone()),
        ..panel_with(&["cargo test"], true)
    };
    let (socket, seen) = recording_daemon(panel_reply(panel, |_| None));
    let (workspace, cx, _client) = approved(&socket, cx);

    send_checks(&workspace, run, cx);
    let rows = (0..3)
        .filter(|index| {
            let selector: &'static str = Box::leak(format!("local-check-{index}").into_boxed_str());

            cx.debug_bounds(selector).is_some()
        })
        .count();
    let fresh = problem_of(&workspace, cx);

    let first_read = wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::PrPanel { .. })) == 1
    });
    workspace.update(cx, |workspace, cx| {
        workspace.apply_diff(merged_at("def", MERGED), cx);
    });
    let read_again = wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::PrPanel { .. })) == 2
    });
    let stale = problem_of(&workspace, cx);

    assert_eq!(rows, 3);
    assert_eq!(fresh, None);
    assert!(first_read);
    assert!(
        read_again,
        "a new head reads the panel data again, also with the same fingerprint"
    );
    assert_eq!(stale, Some(Blocker::ChecksStale));
}

#[gpui::test]
fn run_checks_asks_the_daemon_and_the_button_says_run_again_after_a_run(cx: &mut TestAppContext) {
    let (socket, seen) = recording_daemon(|request| match request {
        Request::RunChecks { call, .. } => Some(Event::Done { call: *call }),
        other => github_reply(other),
    });
    let (workspace, cx, _client) = approved(&socket, cx);

    click("run-checks", cx);
    let started = wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::RunChecks { task, .. } if *task == TASK))
            == 1
    });

    send_checks(
        &workspace,
        checks_run(
            "abc",
            true,
            vec![local_check("cargo test", CheckState::Running)],
        ),
        cx,
    );
    let running = cx.debug_bounds("run-checks").is_some();
    send_checks(
        &workspace,
        checks_run(
            "abc",
            true,
            vec![local_check("cargo test", CheckState::Passed)],
        ),
        cx,
    );

    assert!(started);
    assert!(running);
    assert_eq!(
        create_state_of(&workspace, cx),
        CreateState::Blocked(Blocker::NoTitle)
    );
}

#[gpui::test]
fn show_output_focuses_the_process_terminal_of_a_failed_check(cx: &mut TestAppContext) {
    let (socket, _seen) = recording_daemon(github_reply);
    let (workspace, cx, client) = approved(&socket, cx);
    let process = TerminalInfo {
        id: DaemonTerminalId(31),
        kind: TerminalKind::Process,
        task: Some("task:7".into()),
        owner: None,
        name: Some("cargo clippy".into()),
        fixed_size: None,
        title: Some("cargo clippy".into()),
        cwd: None,
        exit: Some(ExitStatus::Code(1)),
    };
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.use_daemon(client.clone(), std::slice::from_ref(&process), window, cx);
        workspace.right_panel = Some(RightPanel::PullRequest);
    });

    let mut failed = local_check("cargo clippy", CheckState::Failed);
    failed.terminal = Some(DaemonTerminalId(31));
    send_checks(&workspace, checks_run("abc", true, vec![failed]), cx);
    workspace.update(cx, |workspace, cx| workspace.set_view(View::Review, cx));
    redraw(cx);

    click("check-output-0", cx);

    let (view, focused, tile) = workspace.read_with(cx, |workspace, _| {
        (
            workspace.view,
            workspace.focused_terminal,
            workspace.tile_of_daemon_terminal(DaemonTerminalId(31)),
        )
    });
    assert_eq!(view, View::Terminals);
    assert!(tile.is_some());
    assert_eq!(focused, tile);
}

#[gpui::test]
fn create_sends_the_fields_and_the_options_and_the_panel_shows_the_pull_request(
    cx: &mut TestAppContext,
) {
    let (socket, seen) = recording_daemon(|request| match request {
        Request::CreatePr { call, .. } => Some(Event::Done { call: *call }),
        other => github_reply(other),
    });
    let (workspace, cx, _client) = approved(&socket, cx);
    send_draft(&workspace, draft("feat: add it", "## What"), cx);
    send_checks(
        &workspace,
        checks_run(
            "abc",
            true,
            vec![local_check("cargo test", CheckState::Passed)],
        ),
        cx,
    );
    workspace.update(cx, |workspace, cx| {
        workspace.set_close_after_merge(true, cx);
    });
    redraw(cx);

    let ready = create_state_of(&workspace, cx);
    click("create-pr-button", cx);
    let sent = wait_for(cx, &seen, |seen, _| {
        seen.first(|request| match request {
            Request::CreatePr {
                draft,
                as_draft,
                close_after_merge,
                skip_checks,
                ..
            } => Some((draft.clone(), *as_draft, *close_after_merge, *skip_checks)),
            _ => None,
        })
        .is_some()
    });

    assert_eq!(ready, CreateState::Ready);
    assert!(sent);
    assert_eq!(
        seen.first(|request| match request {
            Request::CreatePr {
                draft,
                as_draft,
                close_after_merge,
                skip_checks,
                ..
            } => Some((draft.clone(), *as_draft, *close_after_merge, *skip_checks)),
            _ => None,
        }),
        Some((draft("feat: add it", "## What"), true, true, false))
    );

    workspace.update(cx, |workspace, cx| {
        if let Some(task) = workspace.data.tasks.first_mut() {
            task.pull_request = Some(PrLink {
                number: 42,
                url: "https://github.com/swarm-labs/swarm/pull/42".into(),
                head: "abc".into(),
            });
        }
        cx.notify();
    });
    redraw(cx);

    assert!(cx.debug_bounds("create-pr-button").is_none());
    assert!(cx.debug_bounds("open-on-github").is_some());
}

#[gpui::test]
fn the_create_button_waits_for_approval_comments_and_passing_checks(cx: &mut TestAppContext) {
    let (socket, _seen) = recording_daemon(github_reply);
    let (workspace, cx, _client) = approved(&socket, cx);
    send_draft(&workspace, draft("feat: add it", "body"), cx);

    let no_run = create_state_of(&workspace, cx);
    send_checks(
        &workspace,
        checks_run(
            "abc",
            true,
            vec![local_check("cargo test", CheckState::Failed)],
        ),
        cx,
    );
    let failed = create_state_of(&workspace, cx);
    send_checks(
        &workspace,
        checks_run(
            "abc",
            true,
            vec![local_check("cargo test", CheckState::Passed)],
        ),
        cx,
    );
    let passed = create_state_of(&workspace, cx);

    workspace.update(cx, |workspace, _| {
        workspace
            .data
            .comments
            .insert(TASK, vec![pending_comment(1, "rename this")].into());
    });
    let pending = create_state_of(&workspace, cx);
    workspace.update(cx, |workspace, _| workspace.data.comments.clear());

    workspace.update(cx, |workspace, _| {
        if let Some(task) = workspace.data.tasks.first_mut() {
            task.approved = Some(MERGED + 5);
        }
    });
    let outdated = create_state_of(&workspace, cx);

    assert_eq!(no_run, CreateState::Blocked(Blocker::ChecksNotRun));
    assert_eq!(failed, CreateState::Blocked(Blocker::ChecksFailed));
    assert_eq!(passed, CreateState::Ready);
    assert_eq!(pending, CreateState::Blocked(Blocker::CommentsPending));
    assert_eq!(outdated, CreateState::Blocked(Blocker::NotApproved));
    assert!(cx.debug_bounds("create-pr-button").is_some());
}

fn panel_with(commands: &[&str], required: bool) -> PrPanel {
    PrPanel {
        commands: commands
            .iter()
            .map(|command| (*command).to_owned())
            .collect(),
        required,
        pre_push_hook: None,
        run: None,
        draft: None,
    }
}

/// Answers the panel read with `panel`, and defers every other request to `other`.
fn panel_reply(
    panel: PrPanel,
    other: impl Fn(&Request) -> Option<Event> + Send + 'static,
) -> impl Fn(&Request) -> Option<Event> + Send + 'static {
    move |request| match request {
        Request::PrPanel { call, .. } => Some(Event::PrPanel {
            call: *call,
            panel: panel.clone(),
        }),
        request => other(request).or_else(|| github_reply(request)),
    }
}

fn accept_create(request: &Request) -> Option<Event> {
    match request {
        Request::CreatePr { call, .. } => Some(Event::Done { call: *call }),
        _ => None,
    }
}

#[gpui::test]
fn without_required_checks_create_anyway_asks_first_and_skips_the_checks(cx: &mut TestAppContext) {
    let (socket, seen) = recording_daemon(panel_reply(
        panel_with(&["cargo test"], false),
        accept_create,
    ));
    let (workspace, cx, _client) = approved(&socket, cx);
    send_draft(&workspace, draft("feat: add it", "body"), cx);
    send_checks(
        &workspace,
        checks_run(
            "abc",
            true,
            vec![local_check("cargo test", CheckState::Failed)],
        ),
        cx,
    );
    wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::PrPanel { .. })) >= 1
    });
    redraw(cx);

    let state = create_state_of(&workspace, cx);
    click("create-pr-button", cx);
    let asked = cx.debug_bounds("create-anyway-confirm").is_some();
    let sent_early = seen.count(|request| matches!(request, Request::CreatePr { .. }));

    click("create-anyway-cancel", cx);
    let after_cancel = seen.count(|request| matches!(request, Request::CreatePr { .. }));

    click("create-pr-button", cx);
    click("create-anyway-confirm", cx);
    let sent = wait_for(cx, &seen, |seen, _| {
        seen.count(|request| {
            matches!(
                request,
                Request::CreatePr {
                    skip_checks: true,
                    ..
                }
            )
        }) == 1
    });

    assert_eq!(state, CreateState::Anyway(Blocker::ChecksFailed));
    assert!(asked);
    assert_eq!(sent_early, 0);
    assert_eq!(after_cancel, 0);
    assert!(sent);
}

#[gpui::test]
fn checks_that_are_required_by_the_project_never_offer_create_anyway(cx: &mut TestAppContext) {
    let (socket, seen) = recording_daemon(panel_reply(
        panel_with(&["cargo test"], true),
        accept_create,
    ));
    let (workspace, cx, _client) = approved(&socket, cx);
    send_draft(&workspace, draft("feat: add it", "body"), cx);
    send_checks(
        &workspace,
        checks_run(
            "abc",
            false,
            vec![local_check("cargo test", CheckState::Failed)],
        ),
        cx,
    );
    wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::PrPanel { .. })) >= 1
    });
    redraw(cx);

    click("create-pr-button", cx);
    wait_a_little();
    redraw(cx);

    assert_eq!(
        create_state_of(&workspace, cx),
        CreateState::Blocked(Blocker::ChecksFailed),
        "the project's setting wins over the run's copy of it"
    );
    assert!(cx.debug_bounds("create-anyway-confirm").is_none());
    assert_eq!(
        seen.count(|request| matches!(request, Request::CreatePr { .. })),
        0
    );
}

#[gpui::test]
fn a_failed_create_shows_every_line_of_the_daemons_text_and_keeps_the_fields(
    cx: &mut TestAppContext,
) {
    let output = "remote: error: hook declined\nremote: line two\nerror: failed to push";
    let (socket, seen) = recording_daemon(move |request| match request {
        Request::CreatePr { call, .. } => Some(Event::Error {
            call: Some(*call),
            id: None,
            message: output.into(),
        }),
        other => github_reply(other),
    });
    let (workspace, cx, _client) = approved(&socket, cx);
    send_draft(&workspace, draft("feat: add it", "body"), cx);
    send_checks(
        &workspace,
        checks_run(
            "abc",
            true,
            vec![local_check("cargo test", CheckState::Passed)],
        ),
        cx,
    );

    click("create-pr-button", cx);
    let failed = wait_for(cx, &seen, |_, cx| {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .pull_requests
                .form(APP_TASK)
                .is_some_and(|form| matches!(form.create, CreateStatus::Failed(_)))
        })
    });

    let status = workspace.read_with(cx, |workspace, _| {
        workspace
            .pull_requests
            .form(APP_TASK)
            .map(|form| form.create.clone())
    });
    assert!(failed);
    assert_eq!(status, Some(CreateStatus::Failed(output.into())));
    assert!(cx.debug_bounds("create-error").is_some());
    assert_eq!(title_of(&workspace, cx), "feat: add it");
    assert!(cx.debug_bounds("create-pr-button").is_some());
}

fn github_check(name: &str, state: GithubCheckState) -> GithubCheck {
    GithubCheck {
        name: name.to_owned(),
        state,
        duration_secs: Some(41),
        url: None,
    }
}

fn open_pull_request(number: u64) -> PullRequest {
    PullRequest {
        number,
        checks: vec![
            github_check("build", GithubCheckState::Running),
            github_check("clippy", GithubCheckState::Passed),
        ],
        draft: true,
        mergeable: false,
        ..sample_pull_request()
    }
}

/// The task has pull request #42, and the daemon reads `status` for it.
fn with_pull_request(
    status: Arc<Mutex<PullRequest>>,
    reads: Arc<AtomicUsize>,
    cx: &mut TestAppContext,
) -> (Entity<Workspace>, &mut VisualTestContext, Client, Seen) {
    with_pull_request_replying(status, reads, |_| None, cx)
}

/// Like [`with_pull_request`], with `extra` answering first.
fn with_pull_request_replying(
    status: Arc<Mutex<PullRequest>>,
    reads: Arc<AtomicUsize>,
    extra: impl Fn(&Request) -> Option<Event> + Send + 'static,
    cx: &mut TestAppContext,
) -> (Entity<Workspace>, &mut VisualTestContext, Client, Seen) {
    let (socket, seen) = recording_daemon(move |request| {
        if let Some(event) = extra(request) {
            return Some(event);
        }

        match request {
            Request::PrStatus { call, .. } => {
                reads.fetch_add(1, Ordering::SeqCst);

                Some(Event::PrStatus {
                    call: *call,
                    pull_request: status
                        .lock()
                        .map_or_else(|_| open_pull_request(42), |status| status.clone()),
                })
            }
            Request::MarkPrReady { call, .. }
            | Request::AddReviewers { call, .. }
            | Request::MergePr { call, .. }
            | Request::SetCloseAfterMerge { call, .. } => Some(Event::Done { call: *call }),
            other => github_reply(other),
        }
    });
    let (workspace, cx, client) = approved(&socket, cx);

    workspace.update(cx, |workspace, cx| {
        if let Some(task) = workspace.data.tasks.first_mut() {
            task.pull_request = Some(PrLink {
                number: 42,
                url: "https://github.com/swarm-labs/swarm/pull/42".into(),
                head: "abc".into(),
            });
        }
        cx.notify();
    });
    redraw(cx);

    (workspace, cx, client, seen)
}

fn status_of(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> Option<PullRequest> {
    workspace.read_with(cx, |workspace, _| {
        workspace.data.pull_requests.get(&TASK).cloned()
    })
}

fn polling(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> bool {
    workspace.read_with(cx, |workspace, _| workspace.pull_requests.polling())
}

#[gpui::test]
fn the_status_is_read_every_10_seconds_while_checks_run_then_every_30(cx: &mut TestAppContext) {
    let status = Arc::new(Mutex::new(open_pull_request(42)));
    let reads = Arc::new(AtomicUsize::new(0));
    let (workspace, cx, _client, _seen) =
        with_pull_request(Arc::clone(&status), Arc::clone(&reads), cx);
    let count = || reads.load(Ordering::SeqCst);
    let advance = |seconds: u64, cx: &mut VisualTestContext| {
        cx.executor().advance_clock(Duration::from_secs(seconds));
        redraw(cx);
    };

    let first = wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some()
    });
    let after_first = count();

    advance(9, cx);
    wait_a_little();
    redraw(cx);
    let at_9 = count();

    advance(2, cx);
    let fast = wait_until(cx, |cx| {
        redraw(cx);
        count() == 2
    });

    if let Ok(mut status) = status.lock() {
        status.checks = vec![github_check("build", GithubCheckState::Passed)];
    }
    advance(10, cx);
    let second_fast = wait_until(cx, |cx| {
        redraw(cx);
        count() == 3
    });
    let settled = wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some_and(|status| !checks_running(&status))
    });

    advance(25, cx);
    wait_a_little();
    redraw(cx);
    let at_25 = count();
    advance(6, cx);
    let slow = wait_until(cx, |cx| {
        redraw(cx);
        count() == 4
    });

    assert!(first);
    assert_eq!(after_first, 1);
    assert_eq!(at_9, 1, "nothing is read before 10 s");
    assert!(fast, "read again after 10 s while checks run");
    assert!(second_fast);
    assert!(settled);
    assert_eq!(at_25, 3, "once the checks are done the wait is 30 s");
    assert!(slow);
}

fn advance(seconds: u64, cx: &mut VisualTestContext) {
    cx.executor().advance_clock(Duration::from_secs(seconds));
    redraw(cx);
}

/// The read count once it stopped growing.
fn settled(reads: &AtomicUsize, cx: &mut VisualTestContext) -> usize {
    let mut last = reads.load(Ordering::SeqCst);

    for _ in 0..20 {
        wait_a_little();
        redraw(cx);

        let now = reads.load(Ordering::SeqCst);
        if now == last && now > 0 {
            break;
        }
        last = now;
    }

    last
}

#[gpui::test]
fn the_status_poll_stops_with_the_window_and_a_merge(cx: &mut TestAppContext) {
    let status = Arc::new(Mutex::new(PullRequest {
        draft: false,
        mergeable: true,
        checks: vec![github_check("build", GithubCheckState::Passed)],
        ..open_pull_request(42)
    }));
    let reads = Arc::new(AtomicUsize::new(0));
    let merging = Arc::clone(&status);
    let (workspace, cx, _client, seen) = with_pull_request_replying(
        Arc::clone(&status),
        Arc::clone(&reads),
        move |request| match request {
            Request::MergePr { call, .. } => {
                if let Ok(mut status) = merging.lock() {
                    status.state = PrState::Merged;
                }

                Some(Event::Done { call: *call })
            }
            _ => None,
        },
        cx,
    );
    wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some()
    });

    let shown = polling(&workspace, cx);

    cx.deactivate_window();
    redraw(cx);
    let inactive = polling(&workspace, cx);

    cx.update(|window, _| window.activate_window());
    redraw(cx);
    let back = polling(&workspace, cx);

    click("merge", cx);
    let sent = wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::MergePr { .. })) == 1
    });
    let merged = wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some_and(|status| status.state == PrState::Merged)
    });
    let reads_at_merge = settled(&reads, cx);
    let stopped = polling(&workspace, cx);

    advance(120, cx);
    wait_a_little();
    redraw(cx);

    assert!(shown);
    assert!(!inactive);
    assert!(back);
    assert!(sent);
    assert!(merged, "the fake status turned MERGED");
    assert!(!stopped, "a merged pull request is not polled");
    assert_eq!(
        reads.load(Ordering::SeqCst),
        reads_at_merge,
        "no read after the merge"
    );
}

#[gpui::test]
fn the_header_status_is_read_every_60_seconds_without_the_panel_until_it_is_merged(
    cx: &mut TestAppContext,
) {
    let status = Arc::new(Mutex::new(open_pull_request(42)));
    let reads = Arc::new(AtomicUsize::new(0));
    let (workspace, cx, _client, _seen) =
        with_pull_request(Arc::clone(&status), Arc::clone(&reads), cx);
    workspace.update(cx, |workspace, cx| {
        workspace.toggle_panel(RightPanel::PullRequest, cx);
    });
    redraw(cx);
    let before = settled(&reads, cx);

    let closed = polling(&workspace, cx);
    advance(59, cx);
    wait_a_little();
    redraw(cx);
    let at_59 = reads.load(Ordering::SeqCst);

    advance(2, cx);
    let at_61 = wait_until(cx, |cx| {
        redraw(cx);
        reads.load(Ordering::SeqCst) == before + 1
    });

    if let Ok(mut status) = status.lock() {
        status.state = PrState::Merged;
    }
    advance(60, cx);
    let merged = wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some_and(|status| status.state == PrState::Merged)
    });
    let label = workspace.read_with(cx, |workspace, _| {
        workspace.pull_request_toggle_label(APP_TASK)
    });
    let after_merge = settled(&reads, cx);
    advance(180, cx);
    wait_a_little();
    redraw(cx);

    assert!(closed, "the poll goes on without the panel");
    assert_eq!(at_59, before, "nothing is read before 60 s");
    assert!(at_61, "read again after 60 s");
    assert!(merged);
    assert_eq!(label.as_deref(), Some("#42 \u{b7} merged"));
    assert_eq!(reads.load(Ordering::SeqCst), after_merge);
    assert!(!polling(&workspace, cx));
}

#[gpui::test]
fn a_closed_pull_request_stops_the_poll_and_an_inactive_window_pauses_it(cx: &mut TestAppContext) {
    let status = Arc::new(Mutex::new(open_pull_request(42)));
    let reads = Arc::new(AtomicUsize::new(0));
    let (workspace, cx, _client, _seen) =
        with_pull_request(Arc::clone(&status), Arc::clone(&reads), cx);
    settled(&reads, cx);

    cx.deactivate_window();
    redraw(cx);
    let paused_at = reads.load(Ordering::SeqCst);
    advance(300, cx);
    wait_a_little();
    redraw(cx);
    let paused = reads.load(Ordering::SeqCst);

    cx.update(|window, _| window.activate_window());
    redraw(cx);
    let resumed = wait_until(cx, |cx| {
        redraw(cx);
        reads.load(Ordering::SeqCst) > paused
    });
    settled(&reads, cx);

    if let Ok(mut status) = status.lock() {
        status.state = PrState::Closed;
    }
    advance(60, cx);
    let closed = wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some_and(|status| status.state == PrState::Closed)
    });
    let after_close = settled(&reads, cx);
    advance(300, cx);
    wait_a_little();
    redraw(cx);

    assert_eq!(paused, paused_at, "an inactive window reads nothing");
    assert!(resumed, "the window reads again when it is active");
    assert!(closed);
    assert_eq!(reads.load(Ordering::SeqCst), after_close);
    assert!(!polling(&workspace, cx));
}

#[gpui::test]
fn the_status_panel_shows_the_pull_request_and_the_header_its_state(cx: &mut TestAppContext) {
    let status = Arc::new(Mutex::new(open_pull_request(42)));
    let (workspace, cx, _client, _seen) =
        with_pull_request(status, Arc::new(AtomicUsize::new(0)), cx);
    wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some()
    });

    let label = workspace.read_with(cx, |workspace, _| {
        workspace.pull_request_toggle_label(APP_TASK)
    });

    assert_eq!(label.as_deref(), Some("#42 \u{b7} checks running"));
    assert!(cx.debug_bounds("pull-request-button").is_some());
    assert!(cx.debug_bounds("ready-for-review").is_some());
    assert!(cx.debug_bounds("merge").is_some());
    assert!(cx.debug_bounds("github-check-0").is_some());
    assert!(cx.debug_bounds("github-check-1").is_some());
}

#[gpui::test]
fn the_merge_goes_with_the_method_the_user_chose(cx: &mut TestAppContext) {
    let status = Arc::new(Mutex::new(PullRequest {
        draft: false,
        mergeable: true,
        checks: vec![github_check("build", GithubCheckState::Passed)],
        ..open_pull_request(42)
    }));
    let (workspace, cx, _client, seen) =
        with_pull_request(status, Arc::new(AtomicUsize::new(0)), cx);
    wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some()
    });

    let default = workspace.read_with(cx, |workspace, _| {
        workspace
            .pull_requests
            .form(APP_TASK)
            .map(|form| form.method)
    });
    click("merge", cx);
    let squashed = wait_for(cx, &seen, |seen, _| {
        seen.count(|request| {
            matches!(
                request,
                Request::MergePr {
                    method: MergeMethod::Squash,
                    ..
                }
            )
        }) == 1
    });

    workspace.update(cx, |workspace, cx| {
        workspace.choose_merge_method(MergeMethod::Rebase, cx);
    });
    redraw(cx);
    click("merge", cx);
    let rebased = wait_for(cx, &seen, |seen, _| {
        seen.count(|request| {
            matches!(
                request,
                Request::MergePr {
                    method: MergeMethod::Rebase,
                    ..
                }
            )
        }) == 1
    });

    assert_eq!(default, Some(MergeMethod::Squash));
    assert!(squashed);
    assert!(rebased);
}

#[gpui::test]
fn merge_waits_until_github_says_the_pull_request_is_mergeable(cx: &mut TestAppContext) {
    let status = Arc::new(Mutex::new(open_pull_request(42)));
    let (workspace, cx, _client, seen) =
        with_pull_request(status, Arc::new(AtomicUsize::new(0)), cx);
    wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some()
    });

    click("merge", cx);
    wait_a_little();
    redraw(cx);

    assert_eq!(
        seen.count(|request| matches!(request, Request::MergePr { .. })),
        0
    );
}

#[gpui::test]
fn ready_for_review_review_requests_and_close_after_merge_reach_the_daemon(
    cx: &mut TestAppContext,
) {
    let status = Arc::new(Mutex::new(open_pull_request(42)));
    let (workspace, cx, _client, seen) =
        with_pull_request(status, Arc::new(AtomicUsize::new(0)), cx);
    wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some()
    });

    click("ready-for-review", cx);
    let ready = wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::MarkPrReady { .. })) == 1
    });

    let reviewers = workspace.read_with(cx, |workspace, _| {
        workspace
            .pull_requests
            .form(APP_TASK)
            .map(|form| form.reviewers.clone())
    });
    if let Some(reviewers) = reviewers {
        reviewers.update(cx, |input, cx| input.set_text("@mara, jonas", cx));
    }
    redraw(cx);
    click("request-review", cx);
    let asked = wait_for(cx, &seen, |seen, _| {
        seen.first(|request| match request {
            Request::AddReviewers { logins, .. } => Some(logins.clone()),
            _ => None,
        })
        .is_some()
    });
    let logins = seen.first(|request| match request {
        Request::AddReviewers { logins, .. } => Some(logins.clone()),
        _ => None,
    });

    workspace.update(cx, |workspace, cx| {
        workspace.set_close_after_merge(true, cx);
    });
    let closed_after = wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::SetCloseAfterMerge { close: true, .. }))
            == 1
    });
    let flag = workspace.read_with(cx, |workspace, _| {
        workspace
            .data
            .task(APP_TASK)
            .is_some_and(|task| task.close_after_merge)
    });

    assert!(ready);
    assert!(asked);
    assert_eq!(logins, Some(vec!["mara".to_owned(), "jonas".to_owned()]));
    assert!(closed_after);
    assert!(flag);
}

#[gpui::test]
fn a_merged_pull_request_shows_plainly_and_the_poll_ends(cx: &mut TestAppContext) {
    let status = Arc::new(Mutex::new(PullRequest {
        state: PrState::Merged,
        draft: false,
        ..open_pull_request(42)
    }));
    let (workspace, cx, _client, _seen) =
        with_pull_request(status, Arc::new(AtomicUsize::new(0)), cx);
    wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some()
    });

    let label = workspace.read_with(cx, |workspace, _| {
        workspace.pull_request_toggle_label(APP_TASK)
    });

    assert_eq!(label.as_deref(), Some("#42 \u{b7} merged"));
    assert!(cx.debug_bounds("merge").is_none());
    assert!(cx.debug_bounds("ready-for-review").is_none());
    assert!(cx.debug_bounds("open-on-github").is_some());
}

fn github_without_gh() -> GithubStatus {
    GithubStatus {
        gh: None,
        gh_version: None,
        signed_in: false,
        ..sample_github()
    }
}

#[gpui::test]
#[expect(
    clippy::too_many_lines,
    reason = "one scripted session reads best in one place"
)]
fn set_up_github_installs_then_signs_in_then_reads_the_status_again(cx: &mut TestAppContext) {
    let statuses = Arc::new(Mutex::new(std::collections::VecDeque::from([
        github_without_gh(),
        GithubStatus {
            signed_in: false,
            ..sample_github()
        },
        sample_github(),
    ])));
    let next_terminal = Arc::new(AtomicUsize::new(60));
    let (socket, seen) = recording_daemon(move |request| match request {
        Request::GithubStatus { call, .. } => {
            let mut queue = statuses.lock().ok()?;
            let status = if queue.len() > 1 {
                queue.pop_front()?
            } else {
                queue.front()?.clone()
            };

            Some(Event::GithubStatus {
                call: *call,
                status,
            })
        }
        Request::Create { call, .. } => Some(Event::Created {
            call: *call,
            id: DaemonTerminalId(next_terminal.fetch_add(1, Ordering::SeqCst) as u64),
        }),
        _ => None,
    });
    let (workspace, cx, _client) = approved(&socket, cx);
    wait_for(cx, &seen, |_, cx| {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .github
                .get(&APP_PROJECT)
                .is_some_and(|status| status.gh.is_none())
        })
    });

    let offered = cx.debug_bounds("set-up-github").is_some();
    click("set-up-github", cx);
    let install = wait_for(cx, &seen, |seen, _| {
        seen.first(|request| match request {
            Request::Create { terminal, .. } => Some(terminal.command.clone()),
            _ => None,
        })
        .is_some()
    });
    let installing = workspace.read_with(cx, |workspace, _| {
        (
            workspace.setup_of(APP_PROJECT).cloned(),
            workspace.view,
            workspace.pull_requests.setup_terminals.len(),
        )
    });

    let tile = workspace.read_with(cx, |workspace, _| {
        workspace
            .pull_requests
            .setup_terminals
            .keys()
            .next()
            .copied()
    });
    let tile = tile.expect("the install terminal has a tile");
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.on_terminal_event(
            tile,
            &TerminalEvent::Exited(ExitStatus::Code(0)),
            window,
            cx,
        );
    });
    let signing = wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::Create { .. })) == 2
    });
    let signing_state =
        workspace.read_with(cx, |workspace, _| workspace.setup_of(APP_PROJECT).cloned());

    let tile = workspace
        .read_with(cx, |workspace, _| {
            workspace
                .pull_requests
                .setup_terminals
                .keys()
                .next()
                .copied()
        })
        .expect("the sign-in terminal has a tile");
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.on_terminal_event(
            tile,
            &TerminalEvent::Exited(ExitStatus::Code(0)),
            window,
            cx,
        );
    });
    let done = wait_for(cx, &seen, |_, cx| {
        workspace.read_with(cx, |workspace, _| {
            workspace.setup_of(APP_PROJECT).is_none()
                && workspace
                    .data
                    .github
                    .get(&APP_PROJECT)
                    .is_some_and(|status| status.signed_in)
        })
    });

    let commands: Vec<Vec<String>> = {
        seen.drain();
        seen.all
            .borrow()
            .iter()
            .filter_map(|request| match request {
                Request::Create { terminal, .. } => terminal.command.clone(),
                _ => None,
            })
            .collect()
    };
    let reads = seen.count(|request| matches!(request, Request::GithubStatus { .. }));
    let tiles = workspace.read_with(cx, |workspace, _| workspace.live_terminals.len());

    assert!(offered);
    assert!(install);
    assert_eq!(installing.0, Some(GithubSetup::Running(SetupStep::Install)));
    assert_eq!(installing.1, View::Terminals);
    assert_eq!(installing.2, 1);
    assert!(signing);
    assert_eq!(signing_state, Some(GithubSetup::Running(SetupStep::SignIn)));
    assert!(done);
    assert_eq!(commands, [sample_github().install, sample_github().sign_in]);
    assert_eq!(
        reads, 3,
        "once on open, after the install, after the sign-in"
    );
    assert_eq!(tiles, 0, "finished steps close their tiles");
}

#[gpui::test]
fn a_failed_step_keeps_its_tile_and_try_again_runs_it_again(cx: &mut TestAppContext) {
    let next_terminal = Arc::new(AtomicUsize::new(70));
    let (socket, seen) = recording_daemon(move |request| match request {
        Request::GithubStatus { call, .. } => Some(Event::GithubStatus {
            call: *call,
            status: github_without_gh(),
        }),
        Request::Create { call, .. } => Some(Event::Created {
            call: *call,
            id: DaemonTerminalId(next_terminal.fetch_add(1, Ordering::SeqCst) as u64),
        }),
        _ => None,
    });
    let (workspace, cx, _client) = approved(&socket, cx);
    wait_for(cx, &seen, |_, cx| {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .github
                .get(&APP_PROJECT)
                .is_some_and(|status| status.gh.is_none())
        })
    });

    click("set-up-github", cx);
    wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::Create { .. })) == 1
    });
    let tile = workspace
        .read_with(cx, |workspace, _| {
            workspace
                .pull_requests
                .setup_terminals
                .keys()
                .next()
                .copied()
        })
        .expect("a tile");
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.on_terminal_event(
            tile,
            &TerminalEvent::Exited(ExitStatus::Code(1)),
            window,
            cx,
        );
    });
    redraw(cx);

    let failed = workspace.read_with(cx, |workspace, _| workspace.setup_of(APP_PROJECT).cloned());
    let kept = workspace.read_with(cx, |workspace, _| {
        workspace.live_terminals.contains_key(&tile)
    });
    let retry = cx.debug_bounds("github-try-again").is_some();

    click("github-try-again", cx);
    let again = wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::Create { .. })) == 2
    });
    let running = workspace.read_with(cx, |workspace, _| workspace.setup_of(APP_PROJECT).cloned());
    let old_tile = workspace.read_with(cx, |workspace, _| {
        workspace.live_terminals.contains_key(&tile)
    });

    assert_eq!(
        failed,
        Some(GithubSetup::Failed {
            step: SetupStep::Install,
            reason: "exit 1".into(),
            tile: Some(tile),
        })
    );
    assert!(kept);
    assert!(retry);
    assert!(again);
    assert_eq!(running, Some(GithubSetup::Running(SetupStep::Install)));
    assert!(!old_tile, "Try again closes the failed tile");
}

#[gpui::test]
fn closing_a_setup_tile_before_it_ends_offers_try_again(cx: &mut TestAppContext) {
    let next_terminal = Arc::new(AtomicUsize::new(80));
    let (socket, seen) = recording_daemon(move |request| match request {
        Request::GithubStatus { call, .. } => Some(Event::GithubStatus {
            call: *call,
            status: github_without_gh(),
        }),
        Request::Create { call, .. } => Some(Event::Created {
            call: *call,
            id: DaemonTerminalId(next_terminal.fetch_add(1, Ordering::SeqCst) as u64),
        }),
        _ => None,
    });
    let (workspace, cx, _client) = approved(&socket, cx);
    wait_for(cx, &seen, |_, cx| {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .github
                .get(&APP_PROJECT)
                .is_some_and(|status| status.gh.is_none())
        })
    });
    click("set-up-github", cx);
    wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::Create { .. })) == 1
    });
    let tile = workspace
        .read_with(cx, |workspace, _| {
            workspace
                .pull_requests
                .setup_terminals
                .keys()
                .next()
                .copied()
        })
        .expect("a tile");

    workspace.update(cx, |workspace, cx| workspace.close_user_terminal(tile, cx));
    redraw(cx);

    let state = workspace.read_with(cx, |workspace, _| workspace.setup_of(APP_PROJECT).cloned());
    assert_eq!(
        state,
        Some(GithubSetup::Failed {
            step: SetupStep::Install,
            reason: "Closed".into(),
            tile: None,
        })
    );
    assert!(cx.debug_bounds("github-try-again").is_some());
}

#[gpui::test]
fn the_branches_of_the_daemon_fill_the_new_task_sheet_with_the_base_first(cx: &mut TestAppContext) {
    let (socket, seen) = recording_daemon(|request| match request {
        Request::ListBranches { call, .. } => Some(Event::Branches {
            call: *call,
            branches: vec![
                "feature/search".to_owned(),
                "main".to_owned(),
                "swarm/login".to_owned(),
                "old-idea".to_owned(),
            ],
        }),
        other => github_reply(other),
    });
    let (workspace, cx, _client) = approved(&socket, cx);

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.open_new_task(window, cx);
    });
    let listed = wait_for(cx, &seen, |_, cx| {
        workspace.read_with(cx, |workspace, cx| {
            matches!(&workspace.overlay, Some(Overlay::NewTask { view, .. }) if view.read(cx).branch_names().len() > 2)
        })
    });

    let names = workspace.read_with(cx, |workspace, cx| match &workspace.overlay {
        Some(Overlay::NewTask { view, .. }) => view.read(cx).branch_names(),
        _ => Vec::new(),
    });
    let names: Vec<&str> = names.iter().map(AsRef::as_ref).collect();

    assert!(listed);
    assert_eq!(names, ["main", "swarm/login", "feature/search", "old-idea"]);
}

// Sample data

fn sample_workspace(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
    let (workspace, cx) = crate::daemon::tests::setup(cx);

    workspace.update(cx, |workspace, _| workspace.data.comments.clear());

    (workspace, cx)
}

/// Opens Review on the Merged diff and approves it.
fn approve_in_review(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
    workspace.update(cx, |workspace, cx| {
        workspace.set_view(View::Review, cx);
        workspace.set_review_source(ReviewSource::Merged, cx);
    });
    redraw(cx);

    click("review-approve", cx);
}

#[gpui::test]
fn an_approved_task_offers_create_pr_set_up_github_or_stays_approved(cx: &mut TestAppContext) {
    let (workspace, cx) = sample_workspace(cx);
    let before = {
        workspace.update(cx, |workspace, cx| {
            workspace.set_view(View::Review, cx);
            workspace.set_review_source(ReviewSource::Merged, cx);
        });
        redraw(cx);

        cx.debug_bounds("review-create-pr").is_some()
    };

    click("review-approve", cx);
    let create = cx.debug_bounds("review-create-pr").is_some();

    let offers = |change: fn(&mut GithubStatus), cx: &mut VisualTestContext| {
        workspace.update(cx, |workspace, cx| {
            let mut status = sample_github();
            change(&mut status);
            workspace.data.github.insert(APP_PROJECT, status);
            cx.notify();
        });
        redraw(cx);

        (
            cx.debug_bounds("review-create-pr").is_some(),
            cx.debug_bounds("review-set-up-github").is_some(),
            cx.debug_bounds("review-approve").is_some(),
        )
    };
    let no_gh = offers(|status| status.gh = None, cx);
    let signed_out = offers(|status| status.signed_in = false, cx);
    let no_repository = offers(|status| status.repository = None, cx);
    workspace.update(cx, |workspace, cx| {
        workspace.data.github.remove(&APP_PROJECT);
        cx.notify();
    });
    redraw(cx);
    let unknown = cx.debug_bounds("review-create-pr").is_some();

    assert!(!before, "an unapproved task offers Approve");
    assert!(create);
    assert_eq!(no_gh, (false, true, false));
    assert_eq!(signed_out, (false, true, false));
    assert_eq!(no_repository, (false, false, true), "it stays Approved");
    assert!(!unknown);
}

#[gpui::test]
fn create_pr_opens_the_panel_and_the_orchestrator_drafts(cx: &mut TestAppContext) {
    let (workspace, cx) = sample_workspace(cx);
    approve_in_review(&workspace, cx);

    click("review-create-pr", cx);

    let (panel, view) = workspace.read_with(cx, |workspace, _| {
        (workspace.visible_panel(), workspace.view)
    });
    assert_eq!(panel, Some(RightPanel::PullRequest));
    assert_eq!(view, View::Review);
    assert_eq!(title_of(&workspace, cx), crate::model::PULL_REQUEST_TITLE);
    assert!(cx.debug_bounds("create-pr-button").is_some());
}

#[gpui::test]
fn the_pull_request_toggle_appears_after_approval(cx: &mut TestAppContext) {
    let (workspace, cx) = sample_workspace(cx);
    let hidden = cx.debug_bounds("pull-request-button").is_none();
    let label_before = workspace.read_with(cx, |workspace, _| {
        workspace.pull_request_toggle_label(APP_TASK)
    });

    approve_in_review(&workspace, cx);
    let shown = cx.debug_bounds("pull-request-button").is_some();
    let label_after = workspace.read_with(cx, |workspace, _| {
        workspace.pull_request_toggle_label(APP_TASK)
    });

    click("pull-request-button", cx);
    let panel = workspace.read_with(cx, |workspace, _| workspace.visible_panel());

    assert!(hidden);
    assert_eq!(label_before, None);
    assert!(shown);
    assert_eq!(label_after.as_deref(), Some("Pull request"));
    assert_eq!(panel, Some(RightPanel::PullRequest));
}

#[gpui::test]
fn a_task_that_is_not_approved_shows_changes_instead_of_the_pull_request_panel(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = sample_workspace(cx);

    workspace.update(cx, |workspace, cx| {
        workspace.toggle_panel(RightPanel::PullRequest, cx);
    });
    redraw(cx);

    let (kept, shown) = workspace.read_with(cx, |workspace, _| {
        (workspace.right_panel, workspace.visible_panel())
    });
    assert_eq!(kept, Some(RightPanel::PullRequest));
    assert_eq!(shown, Some(RightPanel::Changes));
    assert!(cx.debug_bounds("review-button").is_some());
}

#[gpui::test]
fn on_sample_data_run_checks_then_create_shows_the_status_panel(cx: &mut TestAppContext) {
    let (workspace, cx) = sample_workspace(cx);
    approve_in_review(&workspace, cx);
    click("review-create-pr", cx);

    let blocked = create_state_of(&workspace, cx);
    click("run-checks", cx);
    let ready = create_state_of(&workspace, cx);
    click("create-pr-button", cx);

    let (number, label) = workspace.read_with(cx, |workspace, _| {
        (
            workspace
                .data
                .task(APP_TASK)
                .and_then(|task| task.pull_request.as_ref())
                .map(|link| link.number),
            workspace.pull_request_toggle_label(APP_TASK),
        )
    });
    assert_eq!(blocked, CreateState::Blocked(Blocker::ChecksNotRun));
    assert_eq!(ready, CreateState::Ready);
    assert_eq!(number, Some(42));
    assert_eq!(label.as_deref(), Some("#42 \u{b7} checks running"));
    assert!(cx.debug_bounds("create-pr-button").is_none());
    assert!(cx.debug_bounds("open-on-github").is_some());
}

#[gpui::test]
fn the_palette_offers_create_pull_request_only_when_allowed(cx: &mut TestAppContext) {
    let (workspace, cx) = sample_workspace(cx);
    let offered = |cx: &VisualTestContext| {
        workspace.read_with(cx, |workspace, _| {
            crate::palette::rows(
                &workspace.data,
                workspace.active_project,
                workspace.active_task,
                workspace.view,
            )
            .iter()
            .any(|row| row.label() == "Create pull request")
        })
    };
    let change = |cx: &mut VisualTestContext, change: fn(&mut crate::model::FakeData)| {
        workspace.update(cx, |workspace, _| change(&mut workspace.data));
    };

    let unapproved = offered(cx);
    approve_in_review(&workspace, cx);
    let approved = offered(cx);

    change(cx, |data| {
        data.github.insert(
            APP_PROJECT,
            GithubStatus {
                repository: None,
                ..sample_github()
            },
        );
    });
    let no_repository = offered(cx);
    change(cx, |data| {
        data.github.insert(APP_PROJECT, sample_github());
        if let Some(task) = data.tasks.first_mut() {
            task.pull_request = Some(PrLink {
                number: 42,
                url: "https://github.com/swarm-labs/swarm/pull/42".into(),
                head: "abc".into(),
            });
        }
    });
    let has_one = offered(cx);

    assert!(!unapproved);
    assert!(approved);
    assert!(!no_repository);
    assert!(!has_one);
}

#[gpui::test]
fn choosing_create_pull_request_in_the_palette_opens_the_panel(cx: &mut TestAppContext) {
    let (workspace, cx) = sample_workspace(cx);
    approve_in_review(&workspace, cx);
    cx.simulate_keystrokes("cmd-k");
    redraw(cx);

    cx.simulate_input("create pull");
    cx.simulate_keystrokes("enter");
    redraw(cx);

    let (panel, overlay) = workspace.read_with(cx, |workspace, _| {
        (workspace.visible_panel(), workspace.overlay.is_some())
    });
    assert_eq!(panel, Some(RightPanel::PullRequest));
    assert!(!overlay);
}

#[gpui::test]
fn the_sheet_keeps_the_branch_the_user_picked_when_the_full_list_arrives(cx: &mut TestAppContext) {
    let (workspace, cx) = sample_workspace(cx);
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.open_new_task(window, cx);
    });
    redraw(cx);

    let dialog = workspace.read_with(cx, |workspace, _| match &workspace.overlay {
        Some(Overlay::NewTask { view, .. }) => Some(view.clone()),
        _ => None,
    });
    let dialog = dialog.expect("the sheet is open");
    let before = dialog.read_with(cx, |dialog, _| dialog.branch_names());

    dialog.update(cx, |dialog, cx| {
        dialog.set_branches(vec!["main".into(), "swarm/a".into(), "swarm/b".into()], cx);
        dialog.choose_branch("swarm/b");
        dialog.set_branches(
            vec![
                "main".into(),
                "swarm/c".into(),
                "swarm/b".into(),
                "swarm/a".into(),
            ],
            cx,
        );
    });
    let chosen = dialog.read_with(cx, |dialog, _| dialog.chosen_branch());

    assert_eq!(before.len(), 2, "the base and the open tab's branch");
    assert_eq!(chosen.as_deref(), Some("swarm/b"));
}

// Local checks

fn passed_run() -> ChecksRun {
    checks_run(
        "abc",
        true,
        vec![local_check("cargo test", CheckState::Passed)],
    )
}

fn update_state_of(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> CreateState {
    workspace.read_with(cx, |workspace, _| workspace.update_state(APP_TASK))
}

fn accept_runs(request: &Request) -> Option<Event> {
    match request {
        Request::RunChecks { call, .. } => Some(Event::Done { call: *call }),
        _ => None,
    }
}

#[gpui::test]
fn a_failed_run_with_skipped_checks_is_over_so_run_again_works(cx: &mut TestAppContext) {
    let (socket, seen) =
        recording_daemon(panel_reply(panel_with(&["a", "b", "c"], true), accept_runs));
    let (workspace, cx, _client) = approved(&socket, cx);
    let run_requests =
        |seen: &Seen| seen.count(|request| matches!(request, Request::RunChecks { .. }));

    send_checks(
        &workspace,
        checks_run(
            "abc",
            true,
            vec![
                local_check("cargo fmt", CheckState::Passed),
                local_check("cargo test", CheckState::Failed),
                local_check("cargo clippy", CheckState::Skipped),
            ],
        ),
        cx,
    );
    let rows = (0..3)
        .filter(|index| {
            let selector: &'static str = Box::leak(format!("local-check-{index}").into_boxed_str());

            cx.debug_bounds(selector).is_some()
        })
        .count();

    click("run-checks", cx);
    let again = wait_for(cx, &seen, |seen, _| run_requests(seen) == 1);

    send_checks(
        &workspace,
        checks_run(
            "abc",
            true,
            vec![
                local_check("cargo fmt", CheckState::Passed),
                local_check("cargo test", CheckState::Running),
                local_check("cargo clippy", CheckState::Waiting),
            ],
        ),
        cx,
    );
    click("run-checks", cx);
    wait_a_little();
    redraw(cx);

    assert_eq!(rows, 3);
    assert!(again, "a run that failed is over");
    assert_eq!(
        run_requests(&seen),
        1,
        "a run that still runs cannot restart"
    );
}

#[gpui::test]
fn an_unclean_or_stale_run_does_not_let_a_pull_request_through(cx: &mut TestAppContext) {
    let (socket, seen) = recording_daemon(panel_reply(panel_with(&["cargo test"], true), |_| None));
    let (workspace, cx, _client) = approved(&socket, cx);
    send_draft(&workspace, draft("feat: add it", "body"), cx);
    wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::PrPanel { .. })) >= 1
    });

    send_checks(&workspace, passed_run(), cx);
    let passed = create_state_of(&workspace, cx);

    send_checks(
        &workspace,
        ChecksRun {
            clean: false,
            ..passed_run()
        },
        cx,
    );
    let dirty = create_state_of(&workspace, cx);

    send_checks(&workspace, checks_run("old", true, passed_run().checks), cx);
    let stale = create_state_of(&workspace, cx);

    assert_eq!(passed, CreateState::Ready);
    assert_eq!(dirty, CreateState::Blocked(Blocker::ChecksUnclean));
    assert_eq!(stale, CreateState::Blocked(Blocker::ChecksStale));
}

#[gpui::test]
fn without_configured_checks_the_section_is_hidden_and_create_is_not_blocked(
    cx: &mut TestAppContext,
) {
    let (socket, seen) = recording_daemon(panel_reply(panel_with(&[], true), accept_create));
    let (workspace, cx, _client) = approved(&socket, cx);
    send_draft(&workspace, draft("feat: add it", "body"), cx);
    wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::PrPanel { .. })) >= 1
    });
    redraw(cx);

    let state = create_state_of(&workspace, cx);
    let hidden = cx.debug_bounds("run-checks").is_none();
    click("create-pr-button", cx);
    let sent = wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::CreatePr { .. })) == 1
    });

    assert!(hidden);
    assert_eq!(state, CreateState::Ready);
    assert!(sent);
}

#[gpui::test]
fn checks_that_are_not_required_let_a_missing_run_create_anyway(cx: &mut TestAppContext) {
    let (socket, seen) = recording_daemon(panel_reply(
        panel_with(&["cargo test"], false),
        accept_create,
    ));
    let (workspace, cx, _client) = approved(&socket, cx);
    send_draft(&workspace, draft("feat: add it", "body"), cx);
    wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::PrPanel { .. })) >= 1
    });
    redraw(cx);

    let missing = create_state_of(&workspace, cx);
    click("create-pr-button", cx);
    click("create-anyway-confirm", cx);
    let sent = wait_for(cx, &seen, |seen, _| {
        seen.count(|request| {
            matches!(
                request,
                Request::CreatePr {
                    skip_checks: true,
                    ..
                }
            )
        }) == 1
    });

    assert_eq!(missing, CreateState::Anyway(Blocker::ChecksNotRun));
    assert!(sent);
}

#[gpui::test]
fn the_stored_draft_comes_with_the_panel_so_it_survives_a_restart(cx: &mut TestAppContext) {
    let panel = PrPanel {
        draft: Some(draft("stored title", "stored body")),
        ..panel_with(&["cargo test"], true)
    };
    let (socket, seen) = recording_daemon(panel_reply(panel, |_| None));
    let (workspace, cx, _client) = approved(&socket, cx);

    let filled = wait_for(cx, &seen, |_, cx| {
        title_of(&workspace, cx) == "stored title"
    });
    for _ in 0..4 {
        redraw(cx);
    }

    assert!(filled);
    assert_eq!(body_of(&workspace, cx), "stored body");
    assert_eq!(
        seen.count(|request| matches!(request, Request::DraftPr { .. })),
        0,
        "the orchestrator is not asked for what the daemon kept"
    );
    assert!(cx.debug_bounds("new-draft").is_none());
}

#[gpui::test]
fn the_panels_show_the_pr_base_not_the_branch_the_task_started_from(cx: &mut TestAppContext) {
    let (socket, _seen) = recording_daemon(github_reply);
    let (workspace, cx, _client) = approved(&socket, cx);

    workspace.update(cx, |workspace, _| {
        if let Some(daemon) = workspace
            .data
            .tasks
            .first_mut()
            .and_then(|task| task.daemon.as_mut())
        {
            daemon.base = "main".into();
            daemon.pr_base = "release/1.0".into();
        }
    });

    let base = workspace.read_with(cx, |workspace, _| {
        crate::pull_request_panel::base_of(workspace, workspace.active_task())
    });

    assert_eq!(base.as_ref(), "release/1.0");
}

#[gpui::test]
fn the_merged_diff_is_polled_while_the_panel_shows_also_when_review_shows_live(
    cx: &mut TestAppContext,
) {
    let (socket, seen) = recording_daemon(github_reply);
    let (workspace, cx, _client) = approved(&socket, cx);
    workspace.update(cx, |workspace, cx| {
        workspace.diff_intervals = (Duration::from_millis(20), Duration::from_millis(20));
        workspace.set_view(View::Review, cx);
        workspace.set_review_source(ReviewSource::Live, cx);
    });
    redraw(cx);

    let asks = |source: ReviewSource| {
        move |seen: &Seen, _: &VisualTestContext| {
            seen.count(
                |request| matches!(request, Request::TaskDiff { source: asked, .. } if *asked == source),
            ) >= 1
        }
    };
    let merged = wait_for(cx, &seen, asks(ReviewSource::Merged));
    let live = wait_for(cx, &seen, asks(ReviewSource::Live));

    workspace.update(cx, |workspace, cx| {
        workspace.toggle_panel(RightPanel::PullRequest, cx);
    });
    redraw(cx);
    let stopped = workspace.read_with(cx, |workspace, _| workspace.pr_poll.is_none());

    assert!(merged, "the head commit needs the Merged diff");
    assert!(live);
    assert!(stopped, "the Merged poll ends with the panel");
}

// Follow-up commits

fn set_link_head(workspace: &Entity<Workspace>, head: &str, cx: &mut VisualTestContext) {
    workspace.update(cx, |workspace, cx| {
        if let Some(link) = workspace
            .data
            .tasks
            .first_mut()
            .and_then(|task| task.pull_request.as_mut())
        {
            link.head = head.to_owned();
        }
        cx.notify();
    });
    redraw(cx);
}

fn accept_update(request: &Request) -> Option<Event> {
    match request {
        Request::UpdatePr { call, .. } => Some(Event::Done { call: *call }),
        _ => None,
    }
}

fn ready_pull_request() -> Arc<Mutex<PullRequest>> {
    Arc::new(Mutex::new(PullRequest {
        draft: false,
        ..open_pull_request(42)
    }))
}

fn update_requests(seen: &Seen) -> Vec<bool> {
    seen.drain();

    seen.all
        .borrow()
        .iter()
        .filter_map(|request| match request {
            Request::UpdatePr { skip_checks, .. } => Some(*skip_checks),
            _ => None,
        })
        .collect()
}

#[gpui::test]
fn merge_waits_until_new_commits_are_pushed(cx: &mut TestAppContext) {
    let mergeable = Arc::new(Mutex::new(PullRequest {
        draft: false,
        mergeable: true,
        ..open_pull_request(42)
    }));
    let (workspace, cx, _client, seen) =
        with_pull_request_replying(mergeable, Arc::new(AtomicUsize::new(0)), accept_update, cx);
    wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some()
    });
    let pushed = workspace.read_with(cx, |workspace, _| {
        workspace
            .data
            .tasks
            .first()
            .and_then(|task| task.pull_request.as_ref())
            .map(|link| link.head.clone())
            .unwrap_or_default()
    });
    let merges = |seen: &Seen| seen.count(|request| matches!(request, Request::MergePr { .. }));

    set_link_head(&workspace, "old", cx);
    click("merge", cx);
    wait_a_little();
    redraw(cx);
    let while_new = merges(&seen);

    set_link_head(&workspace, &pushed, cx);
    click("merge", cx);
    let after_push = wait_for(cx, &seen, |seen, _| merges(seen) == 1);

    assert_eq!(while_new, 0, "unpushed commits go first");
    assert!(after_push, "nothing new to push: merge works");
}

#[gpui::test]
fn new_commits_offer_push_changes_under_the_same_rules_as_create(cx: &mut TestAppContext) {
    let (workspace, cx, _client, seen) = with_pull_request_replying(
        ready_pull_request(),
        Arc::new(AtomicUsize::new(0)),
        accept_update,
        cx,
    );
    wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some()
    });

    let nothing_new = cx.debug_bounds("push-changes").is_none();

    set_link_head(&workspace, "old", cx);
    let offered =
        cx.debug_bounds("new-commits").is_some() && cx.debug_bounds("push-changes").is_some();
    let without_run = update_state_of(&workspace, cx);
    click("push-changes", cx);
    wait_a_little();
    redraw(cx);
    let blocked_sent = update_requests(&seen).len();

    send_checks(&workspace, passed_run(), cx);
    let ready = update_state_of(&workspace, cx);
    click("push-changes", cx);
    let sent = wait_for(cx, &seen, |seen, _| update_requests(seen) == [false]);

    workspace.update(cx, |workspace, _| {
        if let Some(task) = workspace.data.tasks.first_mut() {
            task.approved = Some(MERGED + 1);
        }
    });
    let unapproved = update_state_of(&workspace, cx);

    assert!(nothing_new, "the branch is where the pull request is");
    assert!(offered);
    assert_eq!(without_run, CreateState::Blocked(Blocker::ChecksNotRun));
    assert_eq!(blocked_sent, 0);
    assert_eq!(ready, CreateState::Ready);
    assert!(sent);
    assert_eq!(unapproved, CreateState::Blocked(Blocker::NotApproved));
}

#[gpui::test]
fn push_changes_asks_first_when_checks_are_not_required_and_failed(cx: &mut TestAppContext) {
    let (workspace, cx, _client, seen) = with_pull_request_replying(
        ready_pull_request(),
        Arc::new(AtomicUsize::new(0)),
        |request| match request {
            Request::PrPanel { call, .. } => Some(Event::PrPanel {
                call: *call,
                panel: panel_with(&["cargo test"], false),
            }),
            other => accept_update(other),
        },
        cx,
    );
    wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some()
    });
    set_link_head(&workspace, "old", cx);
    wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::PrPanel { .. })) >= 1
    });
    send_checks(
        &workspace,
        checks_run(
            "abc",
            false,
            vec![local_check("cargo test", CheckState::Failed)],
        ),
        cx,
    );
    redraw(cx);

    let state = update_state_of(&workspace, cx);
    click("push-changes", cx);
    let asked = cx.debug_bounds("create-anyway-confirm").is_some();
    let early = update_requests(&seen).len();
    click("create-anyway-cancel", cx);
    let after_cancel = update_requests(&seen).len();

    click("push-changes", cx);
    click("create-anyway-confirm", cx);
    let sent = wait_for(cx, &seen, |seen, _| update_requests(seen) == [true]);

    assert_eq!(state, CreateState::Anyway(Blocker::ChecksFailed));
    assert!(asked);
    assert_eq!((early, after_cancel), (0, 0));
    assert!(sent);
}

#[gpui::test]
fn a_failed_push_shows_every_line_of_the_daemons_text(cx: &mut TestAppContext) {
    let output = "remote: error: hook declined\nremote: line two\nerror: failed to push";
    let (workspace, cx, _client, seen) = with_pull_request_replying(
        ready_pull_request(),
        Arc::new(AtomicUsize::new(0)),
        move |request| match request {
            Request::UpdatePr { call, .. } => Some(Event::Error {
                call: Some(*call),
                id: None,
                message: output.into(),
            }),
            _ => None,
        },
        cx,
    );
    wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some()
    });
    set_link_head(&workspace, "old", cx);
    send_checks(&workspace, passed_run(), cx);

    click("push-changes", cx);
    let failed = wait_for(cx, &seen, |_, cx| {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .pull_requests
                .form(APP_TASK)
                .is_some_and(|form| matches!(form.create, CreateStatus::Failed(_)))
        })
    });

    let status = workspace.read_with(cx, |workspace, _| {
        workspace
            .pull_requests
            .form(APP_TASK)
            .map(|form| form.create.clone())
    });
    assert!(failed);
    assert_eq!(status, Some(CreateStatus::Failed(output.into())));
    assert!(cx.debug_bounds("push-error").is_some());
    assert!(
        cx.debug_bounds("push-changes").is_some(),
        "it can be tried again"
    );
}

#[gpui::test]
fn push_changes_moves_keyboard_focus_into_the_panel(cx: &mut TestAppContext) {
    let (workspace, cx, _client, seen) = with_pull_request_replying(
        ready_pull_request(),
        Arc::new(AtomicUsize::new(0)),
        accept_update,
        cx,
    );
    wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some()
    });
    set_link_head(&workspace, "old", cx);
    send_checks(&workspace, passed_run(), cx);
    let inside = |workspace: &Entity<Workspace>, cx: &mut VisualTestContext| {
        workspace.update_in(cx, |workspace, window, cx| {
            workspace
                .pull_requests
                .panel_focus()
                .is_some_and(|panel| panel.contains_focused(window, cx))
        })
    };
    workspace.update_in(cx, |workspace, window, cx| {
        let outside = workspace.focus_handle(cx);

        window.focus(&outside, cx);
    });
    redraw(cx);
    let before = inside(&workspace, cx);

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.update_pull_request(window, cx);
    });
    wait_for(cx, &seen, |seen, _| update_requests(seen).len() == 1);
    redraw(cx);

    assert!(!before);
    assert!(inside(&workspace, cx));
}

#[gpui::test]
fn create_pr_from_review_moves_keyboard_focus_to_the_title(cx: &mut TestAppContext) {
    let (workspace, cx) = sample_workspace(cx);
    approve_in_review(&workspace, cx);

    click("review-create-pr", cx);
    redraw(cx);

    let focused = workspace.update_in(cx, |workspace, window, cx| {
        workspace
            .pull_requests
            .form(APP_TASK)
            .is_some_and(|form| form.title.focus_handle(cx).is_focused(window))
    });
    assert!(focused);
}

// Closed pull requests

fn closed_pull_request(state: PrState) -> Arc<Mutex<PullRequest>> {
    Arc::new(Mutex::new(PullRequest {
        state,
        draft: false,
        ..open_pull_request(42)
    }))
}

#[gpui::test]
fn a_closed_pull_request_offers_create_pr_again_and_opens_the_new_form(cx: &mut TestAppContext) {
    let (workspace, cx, _client, seen) = with_pull_request_replying(
        closed_pull_request(PrState::Closed),
        Arc::new(AtomicUsize::new(0)),
        accept_create,
        cx,
    );
    wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some()
    });
    send_draft(&workspace, draft("feat: add it", "body"), cx);
    send_checks(&workspace, passed_run(), cx);

    let offered = cx.debug_bounds("create-pr-again").is_some();
    let status_only =
        cx.debug_bounds("merge").is_none() && cx.debug_bounds("create-pr-button").is_none();

    click("create-pr-again", cx);
    let form = cx.debug_bounds("create-pr-button").is_some()
        && cx.debug_bounds("create-pr-again").is_none();
    click("create-pr-button", cx);
    let sent = wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::CreatePr { .. })) == 1
    });

    workspace.update(cx, |workspace, cx| {
        if let Some(task) = workspace.data.tasks.first_mut() {
            task.pull_request = Some(PrLink {
                number: 43,
                url: "https://github.com/swarm-labs/swarm/pull/43".into(),
                head: "abc".into(),
            });
        }
        cx.notify();
    });
    redraw(cx);
    let label = workspace.read_with(cx, |workspace, _| {
        workspace.pull_request_toggle_label(APP_TASK)
    });

    assert!(offered);
    assert!(status_only);
    assert!(form, "the new form replaces the closed pull request");
    assert!(sent);
    assert!(cx.debug_bounds("create-pr-button").is_none());
    assert!(cx.debug_bounds("open-on-github").is_some());
    assert_eq!(
        label.as_deref(),
        Some("#43"),
        "the old status is not shown for the new number"
    );
}

#[gpui::test]
fn a_merged_pull_request_does_not_offer_create_pr_again(cx: &mut TestAppContext) {
    let (workspace, cx, _client, _seen) = with_pull_request(
        closed_pull_request(PrState::Merged),
        Arc::new(AtomicUsize::new(0)),
        cx,
    );
    wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some()
    });

    assert!(cx.debug_bounds("create-pr-again").is_none());
    assert!(cx.debug_bounds("open-on-github").is_some());
}

// Merge results

fn mergeable_pull_request() -> Arc<Mutex<PullRequest>> {
    Arc::new(Mutex::new(PullRequest {
        draft: false,
        mergeable: true,
        checks: vec![github_check("build", GithubCheckState::Passed)],
        ..open_pull_request(42)
    }))
}

#[gpui::test]
fn after_a_merge_the_status_is_read_and_a_queued_merge_says_so(cx: &mut TestAppContext) {
    let reads = Arc::new(AtomicUsize::new(0));
    let (workspace, cx, _client, seen) =
        with_pull_request(mergeable_pull_request(), Arc::clone(&reads), cx);
    wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some()
    });
    let before = settled(&reads, cx);
    let quiet = cx.debug_bounds("merge-queued").is_none();

    click("merge", cx);
    wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::MergePr { .. })) == 1
    });
    let refreshed = wait_until(cx, |cx| {
        redraw(cx);
        reads.load(Ordering::SeqCst) > before && cx.debug_bounds("merge-queued").is_some()
    });

    assert!(quiet);
    assert!(
        refreshed,
        "the status is read again and the pull request is still open"
    );
}

#[gpui::test]
fn a_merge_that_went_through_shows_no_queue_note(cx: &mut TestAppContext) {
    let status = mergeable_pull_request();
    let merging = Arc::clone(&status);
    let (workspace, cx, _client, seen) = with_pull_request_replying(
        status,
        Arc::new(AtomicUsize::new(0)),
        move |request| match request {
            Request::MergePr { call, .. } => {
                if let Ok(mut status) = merging.lock() {
                    status.state = PrState::Merged;
                }

                Some(Event::Done { call: *call })
            }
            _ => None,
        },
        cx,
    );
    wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some()
    });

    click("merge", cx);
    wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::MergePr { .. })) == 1
    });
    let merged = wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some_and(|status| status.state == PrState::Merged)
    });
    let action = workspace.read_with(cx, |workspace, _| {
        workspace
            .pull_requests
            .form(APP_TASK)
            .map(|form| form.action.clone())
    });

    assert!(merged);
    assert_eq!(action, Some(PanelAction::Idle));
    assert!(cx.debug_bounds("merge-queued").is_none());
}

#[gpui::test]
fn a_task_kept_open_after_a_merge_shows_the_daemons_text_and_the_merged_status(
    cx: &mut TestAppContext,
) {
    let text =
        "the pull request is merged, but the task did not close: 2 files have uncommitted changes";
    let status = mergeable_pull_request();
    let merging = Arc::clone(&status);
    let (workspace, cx, _client, _seen) = with_pull_request_replying(
        status,
        Arc::new(AtomicUsize::new(0)),
        move |request| match request {
            Request::MergePr { call, .. } => {
                if let Ok(mut status) = merging.lock() {
                    status.state = PrState::Merged;
                }

                Some(Event::Error {
                    call: Some(*call),
                    id: None,
                    message: text.into(),
                })
            }
            _ => None,
        },
        cx,
    );
    wait_until(cx, |cx| {
        redraw(cx);
        status_of(&workspace, cx).is_some()
    });

    click("merge", cx);
    let shown = wait_until(cx, |cx| {
        redraw(cx);
        cx.debug_bounds("pull-request-error").is_some()
            && status_of(&workspace, cx).is_some_and(|status| status.state == PrState::Merged)
    });
    let action = workspace.read_with(cx, |workspace, _| {
        workspace
            .pull_requests
            .form(APP_TASK)
            .map(|form| form.action.clone())
    });

    assert!(shown, "the status is read after a failed merge call too");
    assert_eq!(action, Some(PanelAction::Failed(text.into())));
}
