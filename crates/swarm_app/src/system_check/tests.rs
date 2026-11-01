//! The rows of the system check, and the check against a fake daemon.

use gpui::TestAppContext;
use swarm_protocol::{Request, ToolState};
use swarm_term::{ExitStatus, TerminalInput};
use swarm_term_view::TerminalEvent;

use super::harness::{Seen, report_with, tool_check, welcome_fixture};
use super::*;

fn row_of(shown: &[Row], tool: Tool) -> &Row {
    shown
        .iter()
        .find(|row| row.tool == tool)
        .expect("a row for every tool")
}

fn kind_of(shown: &[Row], tool: Tool) -> Option<ActionKind> {
    row_of(shown, tool)
        .action
        .as_ref()
        .map(|action| action.kind)
}

fn shown(report: Report, permission: Permission) -> Vec<Row> {
    rows(Some(&Ok(report)), Some(permission), &HashMap::new())
}

#[test]
fn every_tool_has_a_row_with_its_state_and_detail() {
    let report = report_with(&[
        (
            Tool::ClaudeCode,
            tool_check(ToolState::SignedOut, Some("2.1.314"), &["claude"]),
        ),
        (
            Tool::Codex,
            tool_check(
                ToolState::Missing,
                None,
                &["brew", "install", "--cask", "codex"],
            ),
        ),
    ]);

    let rows = shown(report, Permission::Allowed);

    assert_eq!(
        rows.iter().map(|row| row.tool).collect::<Vec<_>>(),
        Tool::ALL
    );
    let summary: Vec<(SystemCheckState, &str)> = rows
        .iter()
        .map(|row| (row.state, row.detail.as_str()))
        .collect();
    assert_eq!(
        summary,
        [
            (SystemCheckState::Ready, "2.50.1"),
            (SystemCheckState::NotSetUp, "2.1.314 \u{b7} not signed in"),
            (SystemCheckState::NotSetUp, "not installed"),
            (SystemCheckState::Ready, "2.80.0"),
            (SystemCheckState::Ready, "signed in"),
            (SystemCheckState::Ready, "allowed"),
        ]
    );
}

#[test]
fn a_ready_agent_shows_its_version_and_that_it_is_signed_in() {
    let rows = shown(report_with(&[]), Permission::Allowed);

    assert_eq!(
        row_of(&rows, Tool::ClaudeCode).detail,
        "2.1.314 \u{b7} signed in"
    );
    assert_eq!(
        row_of(&rows, Tool::Codex).detail,
        "0.154.0 \u{b7} signed in"
    );
}

#[test]
fn the_count_is_how_many_rows_are_ready() {
    let all = shown(report_with(&[]), Permission::Allowed);
    let some = shown(
        report_with(&[
            (
                Tool::Git,
                tool_check(ToolState::Missing, None, &["xcode-select", "--install"]),
            ),
            (Tool::Codex, tool_check(ToolState::Failed, None, &[])),
        ]),
        Permission::NotAsked,
    );

    assert_eq!((ready_count(&all), all.len()), (6, 6));
    assert_eq!((ready_count(&some), some.len()), (3, 6));
}

#[test]
fn the_button_kind_follows_the_state() {
    let fix = ["tool", "fix"];
    let cases = [
        (
            Tool::Git,
            ToolState::Missing,
            &fix[..],
            Some(ActionKind::Install),
        ),
        (
            Tool::ClaudeCode,
            ToolState::Missing,
            &fix[..],
            Some(ActionKind::Install),
        ),
        (
            Tool::ClaudeCode,
            ToolState::SignedOut,
            &fix[..],
            Some(ActionKind::SignIn),
        ),
        (
            Tool::Codex,
            ToolState::SignedOut,
            &fix[..],
            Some(ActionKind::SignIn),
        ),
        (
            Tool::GithubCli,
            ToolState::Missing,
            &fix[..],
            Some(ActionKind::Install),
        ),
        (
            Tool::GithubAccount,
            ToolState::Missing,
            &fix[..],
            Some(ActionKind::SignIn),
        ),
        (
            Tool::GithubAccount,
            ToolState::SignedOut,
            &fix[..],
            Some(ActionKind::SignIn),
        ),
        (
            Tool::Codex,
            ToolState::Failed,
            &[][..],
            Some(ActionKind::CheckAgain),
        ),
        (Tool::Codex, ToolState::Ready, &[][..], None),
        // Nothing can run without a command.
        (Tool::Git, ToolState::Missing, &[][..], None),
    ];

    for (tool, state, fix, expected) in cases {
        let report = report_with(&[(tool, tool_check(state, None, fix))]);
        let rows = shown(report, Permission::Allowed);

        assert_eq!(kind_of(&rows, tool), expected, "{tool:?} is {state:?}");
    }
}

#[test]
fn an_install_button_carries_the_fix_command() {
    let fix = ["brew", "install", "gh"];
    let report = report_with(&[(Tool::GithubCli, tool_check(ToolState::Missing, None, &fix))]);

    let rows = shown(report, Permission::Allowed);

    let action = row_of(&rows, Tool::GithubCli).action.clone();
    assert_eq!(
        action,
        Some(Action {
            kind: ActionKind::Install,
            fix: fix.map(str::to_owned).to_vec(),
            dimmed: false,
        })
    );
}

#[test]
fn the_notification_button_follows_what_macos_says() {
    let kind = |permission| kind_of(&shown(report_with(&[]), permission), Tool::Notifications);

    assert_eq!(kind(Permission::NotAsked), Some(ActionKind::Allow));
    assert_eq!(
        kind(Permission::NotAllowed),
        Some(ActionKind::OpenSystemSettings)
    );
    assert_eq!(kind(Permission::Allowed), None);
    assert_eq!(kind(Permission::Unavailable), None);
}

#[test]
fn the_notifications_wait_for_macos() {
    let waiting = notifications_row(None);

    assert_eq!(waiting.state, SystemCheckState::InProgress);
    assert_eq!(waiting.action, None);
}

#[test]
fn the_account_button_waits_while_the_cli_is_missing() {
    let missing = |cli: ToolState| {
        report_with(&[
            (
                Tool::GithubCli,
                tool_check(cli, None, &["brew", "install", "gh"]),
            ),
            (
                Tool::GithubAccount,
                tool_check(ToolState::Missing, None, &["gh", "auth", "login", "--web"]),
            ),
        ])
    };

    let waiting = shown(missing(ToolState::Missing), Permission::Allowed);
    let ready = shown(missing(ToolState::Ready), Permission::Allowed);

    let dimmed = |rows: &[Row]| {
        row_of(rows, Tool::GithubAccount)
            .action
            .as_ref()
            .map(|action| (action.kind, action.dimmed))
    };
    assert_eq!(dimmed(&waiting), Some((ActionKind::SignIn, true)));
    assert_eq!(
        row_of(&waiting, Tool::GithubAccount).detail,
        "needs GitHub CLI"
    );
    assert_eq!(dimmed(&ready), Some((ActionKind::SignIn, false)));
}

#[test]
fn the_first_open_action_skips_ready_rows_and_dimmed_buttons() {
    let waiting = report_with(&[
        (Tool::GithubCli, tool_check(ToolState::Missing, None, &[])),
        (
            Tool::GithubAccount,
            tool_check(ToolState::Missing, None, &["gh", "auth", "login"]),
        ),
    ]);
    let with_codex = report_with(&[
        (
            Tool::Codex,
            tool_check(ToolState::Missing, None, &["codex"]),
        ),
        (Tool::GithubCli, tool_check(ToolState::Missing, None, &[])),
        (
            Tool::GithubAccount,
            tool_check(ToolState::Missing, None, &["gh", "auth", "login"]),
        ),
    ]);

    assert_eq!(
        first_open_action(&shown(waiting, Permission::Allowed)),
        None
    );
    assert_eq!(
        first_open_action(&shown(with_codex.clone(), Permission::Allowed)),
        Some(Tool::Codex)
    );
    assert_eq!(
        first_open_action(&shown(report_with(&[]), Permission::NotAsked)),
        Some(Tool::Notifications)
    );
}

#[test]
fn a_running_fix_turns_its_row_into_progress_with_no_button() {
    let report = report_with(&[(
        Tool::GithubCli,
        tool_check(ToolState::Missing, None, &["brew", "install", "gh"]),
    )]);
    let busy = HashMap::from([(
        Tool::GithubCli,
        Busy::Fixing(
            ActionKind::Install,
            vec!["brew".to_owned(), "install".to_owned(), "gh".to_owned()],
        ),
    )]);

    let rows = rows(Some(&Ok(report)), Some(Permission::Allowed), &busy);

    let row = row_of(&rows, Tool::GithubCli);
    assert_eq!(row.state, SystemCheckState::InProgress);
    assert_eq!(row.detail, "installing with Homebrew");
    assert_eq!(row.action, None);
}

#[test]
fn the_running_line_names_the_fix() {
    let brew = ["brew".to_owned(), "install".to_owned()];
    let curl = ["sh".to_owned(), "-c".to_owned()];

    assert_eq!(
        running_detail(ActionKind::Install, &brew),
        "installing with Homebrew"
    );
    assert_eq!(running_detail(ActionKind::Install, &curl), "installing");
    assert_eq!(running_detail(ActionKind::SignIn, &curl), "signing in");
}

#[test]
fn before_the_daemon_answers_the_tools_are_in_progress() {
    let waiting = rows(None, Some(Permission::Allowed), &HashMap::new());
    let failed = rows(
        Some(&Err("Not connected to the daemon".into())),
        Some(Permission::Allowed),
        &HashMap::new(),
    );

    assert_eq!(waiting.len(), 6);
    assert_eq!(
        waiting
            .iter()
            .filter(|row| row.state == SystemCheckState::InProgress)
            .count(),
        5
    );
    assert_eq!(
        row_of(&failed, Tool::Git).detail,
        "Not connected to the daemon"
    );
    assert_eq!(row_of(&failed, Tool::Git).state, SystemCheckState::NotSetUp);
}

#[test]
fn a_failed_check_offers_to_ask_again_and_says_why() {
    let mut failed = tool_check(ToolState::Failed, None, &[]);
    failed.detail = Some("timed out".to_owned());
    let rows = shown(report_with(&[(Tool::Git, failed)]), Permission::Allowed);

    assert_eq!(row_of(&rows, Tool::Git).detail, "timed out");
    assert_eq!(kind_of(&rows, Tool::Git), Some(ActionKind::CheckAgain));
}

#[test]
fn the_sample_has_the_rows_of_the_design() {
    let rows = sample_rows();

    assert_eq!((ready_count(&rows), rows.len()), (3, 6));
    assert_eq!(
        row_of(&rows, Tool::GithubCli).state,
        SystemCheckState::InProgress
    );
    assert_eq!(
        row_of(&rows, Tool::GithubCli).detail,
        "installing with Homebrew"
    );
    assert_eq!(
        row_of(&rows, Tool::GithubAccount).detail,
        "needs GitHub CLI"
    );
    assert_eq!(
        kind_of(&rows, Tool::GithubAccount),
        Some(ActionKind::SignIn)
    );
    assert_eq!(kind_of(&rows, Tool::Notifications), Some(ActionKind::Allow));
}

fn installable_cli() -> Report {
    report_with(&[
        (
            Tool::GithubCli,
            tool_check(ToolState::Missing, None, &["brew", "install", "gh"]),
        ),
        (
            Tool::GithubAccount,
            tool_check(ToolState::Missing, None, &["gh", "auth", "login", "--web"]),
        ),
    ])
}

#[gpui::test]
fn a_daemon_answer_shows_in_the_rows_and_the_count(cx: &mut TestAppContext) {
    let report = report_with(&[
        (
            Tool::ClaudeCode,
            tool_check(ToolState::SignedOut, Some("2.1.314"), &["claude"]),
        ),
        (
            Tool::Codex,
            tool_check(ToolState::Missing, None, &["codex"]),
        ),
    ]);
    let fixture = welcome_fixture(report, Permission::NotAsked, cx);

    let check = fixture.check();
    let (count, rows) = check.read_with(fixture.cx, |check, _| (check.count(), check.rows()));

    assert_eq!(count, (3, 6));
    assert_eq!(kind_of(&rows, Tool::ClaudeCode), Some(ActionKind::SignIn));
    assert_eq!(kind_of(&rows, Tool::Codex), Some(ActionKind::Install));
    assert_eq!(kind_of(&rows, Tool::Notifications), Some(ActionKind::Allow));
    for selector in [
        "system-check",
        "system-check-row-git",
        "system-check-row-notifications",
        "system-check-action-claude-code",
        "system-check-action-codex",
        "system-check-action-notifications",
    ] {
        assert!(fixture.cx.debug_bounds(selector).is_some(), "{selector}");
    }
    for selector in ["system-check-action-git", "system-check-action-github-cli"] {
        assert!(fixture.cx.debug_bounds(selector).is_none(), "{selector}");
    }
}

#[gpui::test]
fn a_fix_runs_in_a_user_terminal_and_the_check_asks_again_when_it_ends(cx: &mut TestAppContext) {
    let mut fixture = welcome_fixture(installable_cli(), Permission::Allowed, cx);
    let before = fixture.seen.checks();
    let check = fixture.check();

    fixture.click("system-check-action-github-cli");
    let started = fixture
        .wait(|_, cx| check.read_with(cx, |check, _| check.output_of(Tool::GithubCli).is_some()));
    let created = fixture.seen.created();
    let during = check.read_with(fixture.cx, |check, _| check.rows());

    *fixture.report.lock().expect("lock the report") = report_with(&[]);
    let view = check
        .read_with(fixture.cx, |check, _| check.output_of(Tool::GithubCli))
        .expect("the terminal shows under the row");
    view.update(fixture.cx, |_, cx| {
        cx.emit(TerminalEvent::Exited(ExitStatus::Code(0)));
    });
    let asked_again = fixture.wait(|seen, _| seen.checks() == before + 1);
    let folded = fixture
        .wait(|_, cx| check.read_with(cx, |check, _| check.output_of(Tool::GithubCli).is_none()));
    let answered = fixture.wait(|_, cx| check.read_with(cx, |check, _| check.count() == (6, 6)));

    assert!(started, "the terminal never showed");
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].kind, swarm_protocol::TerminalKind::User);
    assert_eq!(
        created[0].command.as_deref(),
        Some(["brew", "install", "gh"].map(str::to_owned).as_slice())
    );
    assert_eq!(created[0].task, None);
    let during = row_of(&during, Tool::GithubCli);
    assert_eq!(during.state, SystemCheckState::InProgress);
    assert_eq!(during.detail, "installing with Homebrew");
    assert_eq!(during.action, None);
    assert!(asked_again, "the check was not asked again");
    assert!(folded, "the terminal did not fold away");
    assert!(answered, "the new answer did not reach the rows");
}

#[gpui::test]
fn a_second_click_while_a_fix_runs_starts_nothing(cx: &mut TestAppContext) {
    let mut fixture = welcome_fixture(installable_cli(), Permission::Allowed, cx);
    let check = fixture.check();

    fixture.click("system-check-action-github-cli");
    fixture
        .wait(|_, cx| check.read_with(cx, |check, _| check.output_of(Tool::GithubCli).is_some()));
    let button = fixture.cx.debug_bounds("system-check-action-github-cli");

    assert!(button.is_none(), "the row kept its button");
    assert_eq!(fixture.seen.created().len(), 1);
}

#[gpui::test]
fn the_github_account_button_is_dimmed_and_does_nothing_while_the_cli_is_missing(
    cx: &mut TestAppContext,
) {
    let mut fixture = welcome_fixture(installable_cli(), Permission::Allowed, cx);

    let drawn = fixture
        .cx
        .debug_bounds("system-check-action-github-account")
        .is_some();
    fixture.click("system-check-action-github-account");
    fixture.wait(|_, _| false);
    let dimmed = fixture.check().read_with(fixture.cx, |check, _| {
        row_of(&check.rows(), Tool::GithubAccount)
            .action
            .as_ref()
            .is_some_and(|action| action.dimmed)
    });

    assert!(drawn, "the button is not on screen");
    assert!(dimmed);
    assert!(
        fixture.seen.created().is_empty(),
        "the dimmed button ran its fix"
    );
}

#[gpui::test]
fn the_account_button_works_once_the_cli_is_there(cx: &mut TestAppContext) {
    let report = report_with(&[(
        Tool::GithubAccount,
        tool_check(ToolState::Missing, None, &["gh", "auth", "login", "--web"]),
    )]);
    let mut fixture = welcome_fixture(report, Permission::Allowed, cx);

    fixture.click("system-check-action-github-account");
    let asked = fixture.wait(|seen, _| !seen.created().is_empty());

    assert!(asked, "the sign-in never ran");
    assert_eq!(
        fixture.seen.created()[0].command.as_deref(),
        Some(
            ["gh", "auth", "login", "--web"]
                .map(str::to_owned)
                .as_slice()
        )
    );
}

#[gpui::test]
fn allow_asks_macos_and_the_row_turns_ready(cx: &mut TestAppContext) {
    let mut fixture = welcome_fixture(report_with(&[]), Permission::NotAsked, cx);
    let check = fixture.check();

    fixture.click("system-check-action-notifications");
    let ready = fixture.wait(|_, cx| check.read_with(cx, |check, _| check.count() == (6, 6)));

    assert_eq!(fixture.recorder.requests.get(), 1);
    assert!(ready, "the row did not turn ready");
    assert!(
        fixture
            .cx
            .debug_bounds("system-check-action-notifications")
            .is_none()
    );
}

#[gpui::test]
fn a_denied_permission_opens_system_settings(cx: &mut TestAppContext) {
    let mut fixture = welcome_fixture(report_with(&[]), Permission::NotAllowed, cx);

    fixture.click("system-check-action-notifications");

    let url = fixture.cx.opened_url();
    assert_eq!(
        fixture.recorder.requests.get(),
        0,
        "the user was asked again"
    );
    assert!(
        url.as_deref()
            .is_some_and(|url| url.contains("Notifications")),
        "{url:?}"
    );
}

#[gpui::test]
fn coming_back_from_system_settings_reads_the_permission_again(cx: &mut TestAppContext) {
    let mut fixture = welcome_fixture(report_with(&[]), Permission::NotAllowed, cx);
    let check = fixture.check();

    fixture.recorder.permission.set(Permission::Allowed);
    fixture.cx.deactivate_window();
    fixture.cx.update(|window, _| window.activate_window());
    let ready = fixture.wait(|_, cx| check.read_with(cx, |check, _| check.count() == (6, 6)));

    assert!(ready);
}

#[gpui::test]
fn leaving_while_a_fix_runs_stops_its_program(cx: &mut TestAppContext) {
    let mut fixture = welcome_fixture(installable_cli(), Permission::Allowed, cx);
    let check = fixture.check();
    let kills = |seen: &Seen| {
        seen.count(|request| {
            matches!(
                request,
                Request::Input {
                    input: TerminalInput::Kill,
                    ..
                }
            )
        })
    };

    fixture.click("system-check-action-github-cli");
    fixture
        .wait(|_, cx| check.read_with(cx, |check, _| check.output_of(Tool::GithubCli).is_some()));
    let before = kills(&fixture.seen);
    drop(check);
    let workspace = fixture.workspace.clone();
    workspace.update(fixture.cx, Workspace::finish_welcome);
    let stopped = fixture.wait(|seen, _| kills(seen) == 1);

    assert_eq!(before, 0);
    assert!(stopped, "the program kept running");
}

#[gpui::test]
fn a_fix_that_ended_is_not_stopped_again(cx: &mut TestAppContext) {
    let mut fixture = welcome_fixture(installable_cli(), Permission::Allowed, cx);
    let check = fixture.check();

    fixture.click("system-check-action-github-cli");
    fixture
        .wait(|_, cx| check.read_with(cx, |check, _| check.output_of(Tool::GithubCli).is_some()));
    let view = check
        .read_with(fixture.cx, |check, _| check.output_of(Tool::GithubCli))
        .expect("the terminal shows under the row");
    view.update(fixture.cx, |_, cx| {
        cx.emit(TerminalEvent::Exited(ExitStatus::Code(1)));
    });
    fixture
        .wait(|_, cx| check.read_with(cx, |check, _| check.output_of(Tool::GithubCli).is_none()));
    fixture.wait(|_, _| false);

    let kills = fixture.seen.count(|request| {
        matches!(
            request,
            Request::Input {
                input: TerminalInput::Kill,
                ..
            }
        )
    });
    assert_eq!(kills, 0);
}
