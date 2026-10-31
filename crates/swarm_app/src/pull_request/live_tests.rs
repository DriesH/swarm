//! The pull request panels and Review's next step, against a fake daemon and sample data.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use swarm_protocol::{
    CheckState, ChecksRun, Client, Event, GithubCheck, GithubCheckState, GithubStatus, LocalCheck,
    MergeMethod, PrDraft, PrEvent, PrLink, PrState, PullRequest, Request, ReviewSource,
    TaskId as DaemonTaskId, TerminalId as DaemonTerminalId, TerminalInfo, TerminalKind,
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
fn opening_the_panel_reads_github_lists_the_checks_and_asks_for_one_draft(cx: &mut TestAppContext) {
    let (socket, seen) = recording_daemon(github_reply);
    let (workspace, cx, _client) = approved(&socket, cx);

    let asked = wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::GithubStatus { .. })) >= 1
            && seen.count(|request| matches!(request, Request::ListChecks { .. })) >= 1
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
    assert!(asked);
    assert_eq!(
        seen.count(|request| matches!(request, Request::DraftPr { .. })),
        1
    );
    assert_eq!(
        seen.count(|request| matches!(request, Request::ListChecks { .. })),
        1
    );
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

#[gpui::test]
fn the_checks_list_shows_a_row_each_and_a_new_diff_makes_it_stale(cx: &mut TestAppContext) {
    let (socket, seen) = recording_daemon(github_reply);
    let (workspace, cx, _client) = approved(&socket, cx);

    send_checks(
        &workspace,
        checks_run(
            "abc",
            true,
            vec![
                local_check("cargo test --workspace", CheckState::Passed),
                local_check("cargo clippy", CheckState::Failed),
                local_check("cargo fmt --check", CheckState::Waiting),
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
    let fresh = workspace.read_with(cx, |workspace, _| workspace.data.checks_are_stale(APP_TASK));

    let before = seen.count(|request| matches!(request, Request::ListChecks { .. }));
    workspace.update(cx, |workspace, cx| {
        workspace.apply_diff(diff_of(7, ReviewSource::Merged, MERGED + 1), cx);
    });
    let listed_again = wait_for(cx, &seen, |seen, _| {
        seen.count(|request| matches!(request, Request::ListChecks { .. })) > before
    });
    let stale = workspace.read_with(cx, |workspace, _| workspace.data.checks_are_stale(APP_TASK));

    assert_eq!(rows, 3);
    assert!(!fresh);
    assert!(listed_again, "a new Merged diff lists the checks again");
    assert!(stale);
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

#[gpui::test]
fn without_required_checks_create_anyway_asks_first_and_skips_the_checks(cx: &mut TestAppContext) {
    let (socket, seen) = recording_daemon(|request| match request {
        Request::CreatePr { call, .. } => Some(Event::Done { call: *call }),
        other => github_reply(other),
    });
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
    let (socket, seen) = recording_daemon(move |request| match request {
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
    });
    let (workspace, cx, client) = approved(&socket, cx);

    workspace.update(cx, |workspace, cx| {
        if let Some(task) = workspace.data.tasks.first_mut() {
            task.pull_request = Some(PrLink {
                number: 42,
                url: "https://github.com/swarm-labs/swarm/pull/42".into(),
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

#[gpui::test]
fn the_status_poll_stops_with_the_panel_the_window_and_a_merge(cx: &mut TestAppContext) {
    let status = Arc::new(Mutex::new(open_pull_request(42)));
    let reads = Arc::new(AtomicUsize::new(0));
    let (workspace, cx, _client, _seen) = with_pull_request(Arc::clone(&status), reads, cx);
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

    workspace.update(cx, |workspace, cx| {
        workspace.toggle_panel(RightPanel::PullRequest, cx);
    });
    redraw(cx);
    let closed = polling(&workspace, cx);

    assert!(shown);
    assert!(!inactive);
    assert!(back);
    assert!(!closed);
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
