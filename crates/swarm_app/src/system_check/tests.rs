//! The rows of the system check, and the check against a fake daemon.

use std::sync::atomic::Ordering;

use gpui::accesskit::Node;
use gpui::{TestAppContext, VisualTestContext};
use swarm_protocol::{TerminalId, TerminalInfo, ToolState};
use swarm_term::ExitStatus;
use swarm_term_view::TerminalEvent;

use super::harness::{Fixture, report_with, tool_check, welcome_fixture, welcome_fixture_listing};
use super::*;
use crate::daemon::tests::wait_until;

const BREW: &str = "/opt/homebrew/bin/brew";
const GH: &str = "/opt/homebrew/bin/gh";

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
                &[BREW, "install", "--cask", "codex"],
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
    let fix = [BREW, "install", "gh"];
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
                tool_check(cli, None, &[BREW, "install", "gh"]),
            ),
            (
                Tool::GithubAccount,
                tool_check(ToolState::Missing, None, &[GH, "auth", "login", "--web"]),
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
        tool_check(ToolState::Missing, None, &[BREW, "install", "gh"]),
    )]);
    let busy = HashMap::from([(
        Tool::GithubCli,
        Busy::Fixing(
            ActionKind::Install,
            vec![BREW.to_owned(), "install".to_owned(), "gh".to_owned()],
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
    let fix = |program: &str| [program.to_owned(), "install".to_owned()];
    let curl = ["/bin/sh".to_owned(), "-c".to_owned()];

    for brew in [BREW, "/usr/local/bin/brew", "brew"] {
        assert_eq!(
            running_detail(ActionKind::Install, &fix(brew)),
            "installing with Homebrew",
            "{brew}"
        );
    }
    assert_eq!(
        running_detail(ActionKind::Install, &fix("/opt/homebrew/bin/brewery")),
        "installing"
    );
    assert_eq!(running_detail(ActionKind::Install, &curl), "installing");
    assert_eq!(running_detail(ActionKind::SignIn, &curl), "signing in");
    assert_eq!(running_detail(ActionKind::Install, &[]), "installing");
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
            tool_check(ToolState::Missing, None, &[BREW, "install", "gh"]),
        ),
        (
            Tool::GithubAccount,
            tool_check(ToolState::Missing, None, &[GH, "auth", "login", "--web"]),
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
        created[0].command,
        Some(terminal_command(&[
            BREW.to_owned(),
            "install".to_owned(),
            "gh".to_owned()
        ]))
    );
    assert_eq!(created[0].task.as_deref(), Some("system-check"));
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
        tool_check(ToolState::Missing, None, &[GH, "auth", "login", "--web"]),
    )]);
    let mut fixture = welcome_fixture(report, Permission::Allowed, cx);

    fixture.click("system-check-action-github-account");
    let asked = fixture.wait(|seen, _| !seen.created().is_empty());

    assert!(asked, "the sign-in never ran");
    assert_eq!(
        fixture.seen.created()[0].command,
        Some(terminal_command(&[
            GH.to_owned(),
            "auth".to_owned(),
            "login".to_owned(),
            "--web".to_owned()
        ]))
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

    fixture.click("system-check-action-github-cli");
    fixture
        .wait(|_, cx| check.read_with(cx, |check, _| check.output_of(Tool::GithubCli).is_some()));
    let before = fixture.seen.killed().len();
    drop(check);
    let workspace = fixture.workspace.clone();
    workspace.update(fixture.cx, Workspace::finish_welcome);
    let stopped = fixture.wait(|seen, _| seen.killed().len() == 1);

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
    fixture.wait(|seen, _| seen.checks() == 2);
    drop((check, view));
    let workspace = fixture.workspace.clone();
    workspace.update(fixture.cx, Workspace::finish_welcome);
    fixture.wait(|_, _| false);

    assert_eq!(fixture.seen.killed(), []);
}

fn spoken(check: &Entity<SystemCheckView>, tool: Tool, cx: &mut VisualTestContext) -> ButtonA11y {
    cx.update(|window, cx| {
        check.update(cx, |check, cx| {
            let rows = check.rows();
            let row = row_of(&rows, tool);
            let action = row.action.as_ref().expect("the row has a button");

            let element = check
                .action_button(0, row, action, cx)
                .render(window, cx)
                .into_element();
            let role = element.a11y_role();
            let mut node = Node::new(role.unwrap_or(Role::Unknown));
            element.write_a11y_info(&mut node);

            ButtonA11y {
                role,
                label: node.label().map(str::to_owned),
                description: node.description().map(str::to_owned),
            }
        })
    })
}

#[derive(Debug, PartialEq, Eq)]
struct ButtonA11y {
    role: Option<Role>,
    label: Option<String>,
    description: Option<String>,
}

fn button(label: &str, description: Option<&str>) -> ButtonA11y {
    ButtonA11y {
        role: Some(Role::Button),
        label: Some(label.to_owned()),
        description: description.map(str::to_owned),
    }
}

#[gpui::test]
fn each_row_button_names_its_tool_and_hints_at_the_command(cx: &mut TestAppContext) {
    let curl = "curl -fsSL https://chatgpt.com/codex/install.sh | sh";
    let report = report_with(&[
        (
            Tool::ClaudeCode,
            tool_check(
                ToolState::SignedOut,
                Some("2.1.314"),
                &["/Users/jonas/.local/bin/claude"],
            ),
        ),
        (
            Tool::Codex,
            tool_check(ToolState::Missing, None, &["/bin/sh", "-c", curl]),
        ),
        (
            Tool::GithubCli,
            tool_check(ToolState::Missing, None, &[BREW, "install", "gh"]),
        ),
        (
            Tool::GithubAccount,
            tool_check(ToolState::Missing, None, &[GH, "auth", "login", "--web"]),
        ),
        (Tool::Git, tool_check(ToolState::Failed, None, &[])),
    ]);
    let fixture = welcome_fixture(report, Permission::NotAsked, cx);
    let check = fixture.check();
    let cx = fixture.cx;

    let buttons: Vec<ButtonA11y> = Tool::ALL
        .map(|tool| spoken(&check, tool, cx))
        .into_iter()
        .collect();

    assert_eq!(
        buttons,
        [
            button("Check again for Git", None),
            button(
                "Sign in to Claude Code",
                Some("/Users/jonas/.local/bin/claude")
            ),
            button("Install Codex", Some(&format!("/bin/sh -c '{curl}'"))),
            button(
                "Install GitHub CLI",
                Some("/opt/homebrew/bin/brew install gh")
            ),
            button(
                "Sign in to GitHub, needs GitHub CLI",
                Some("/opt/homebrew/bin/gh auth login --web")
            ),
            button("Allow notifications", None),
        ]
    );
}

#[test]
fn every_button_label_starts_with_what_the_button_shows() {
    for tool in Tool::ALL {
        for kind in [
            ActionKind::Install,
            ActionKind::SignIn,
            ActionKind::Allow,
            ActionKind::OpenSystemSettings,
            ActionKind::CheckAgain,
        ] {
            let label = action_label(tool, kind);

            assert!(
                label.starts_with(kind.label().split(' ').next().unwrap_or_default()),
                "{label}"
            );
        }
    }
}

#[test]
fn the_card_around_the_rows_is_a_list() {
    assert_eq!(card().role(), Role::List);
}

#[test]
fn a_failed_check_gives_every_row_a_check_again_button() {
    let rows = rows(
        Some(&Err("the daemon did not answer in time".into())),
        Some(Permission::Allowed),
        &HashMap::new(),
    );

    let tools: Vec<Tool> = rows
        .iter()
        .filter(|row| kind_of(&rows, row.tool) == Some(ActionKind::CheckAgain))
        .map(|row| row.tool)
        .collect();
    assert_eq!(
        tools,
        [
            Tool::Git,
            Tool::ClaudeCode,
            Tool::Codex,
            Tool::GithubCli,
            Tool::GithubAccount
        ]
    );
    assert!(
        rows.iter()
            .filter_map(|row| row.action.as_ref())
            .all(|action| !action.dimmed)
    );
    assert_eq!(first_open_action(&rows), Some(Tool::Git));
}

#[gpui::test]
fn check_again_on_a_failed_check_asks_the_daemon_again(cx: &mut TestAppContext) {
    let mut fixture = welcome_fixture(report_with(&[]), Permission::Allowed, cx);
    let check = fixture.check();
    let before = fixture.seen.checks();

    fixture.fail_checks.store(true, Ordering::SeqCst);
    check.update(fixture.cx, SystemCheckView::refresh);
    fixture.settle();
    let failed = check.read_with(fixture.cx, |check, _| check.rows());
    let drawn = fixture.cx.debug_bounds("system-check-action-git").is_some();

    fixture.fail_checks.store(false, Ordering::SeqCst);
    fixture.click("system-check-action-git");
    let asked = fixture.wait(|seen, _| seen.checks() == before + 2);
    let answered = fixture.wait(|_, cx| check.read_with(cx, |check, _| check.count() == (6, 6)));

    assert_eq!(
        row_of(&failed, Tool::Git).detail,
        "the daemon refused the request: the check broke"
    );
    assert_eq!(kind_of(&failed, Tool::Codex), Some(ActionKind::CheckAgain));
    assert!(drawn, "the button is not on screen");
    assert!(asked, "Check again did not ask the daemon");
    assert!(answered, "the new answer did not reach the rows");
}

fn run(fix: &[&str]) -> (String, Option<i32>) {
    let fix: Vec<String> = fix.iter().map(|part| (*part).to_owned()).collect();
    let command = terminal_command(&fix);

    let output = std::process::Command::new(&command[0])
        .args(&command[1..])
        .output()
        .expect("run the shell");

    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        output.status.code(),
    )
}

#[test]
fn the_terminal_runs_the_fix_behind_a_wrapper_that_passes_it_as_arguments() {
    let fix = [BREW.to_owned(), "install".to_owned(), "gh".to_owned()];

    let command = terminal_command(&fix);

    assert_eq!(
        command,
        [
            "/bin/sh",
            "-c",
            ANNOUNCE,
            "sh",
            "/opt/homebrew/bin/brew install gh",
            BREW,
            "install",
            "gh",
        ]
    );
    assert!(
        !ANNOUNCE.contains("brew"),
        "the script holds no part of the fix"
    );
}

#[test]
fn the_wrapper_prints_the_command_first_and_keeps_its_exit_status() {
    let (loud, loud_code) = run(&["/bin/echo", "two words", "$(exit 7)", "it's"]);
    let (quiet, quiet_code) = run(&["/usr/bin/true"]);
    let (_, failed_code) = run(&["/usr/bin/false"]);

    assert_eq!(
        loud,
        "$ /bin/echo 'two words' '$(exit 7)' 'it'\\''s'\ntwo words $(exit 7) it's\n"
    );
    assert_eq!(loud_code, Some(0));
    assert_eq!(quiet, "$ /usr/bin/true\n");
    assert_eq!(quiet_code, Some(0));
    assert_eq!(failed_code, Some(1));
}

#[test]
fn an_argument_with_spaces_prints_in_quotes() {
    let fix = [
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        "curl -fsSL https://claude.ai/install.sh | bash".to_owned(),
        String::new(),
    ];

    assert_eq!(
        command_text(&fix),
        "/bin/sh -c 'curl -fsSL https://claude.ai/install.sh | bash' ''"
    );
}

fn output_shown(check: &Entity<SystemCheckView>, tool: Tool, cx: &VisualTestContext) -> bool {
    check.read_with(cx, |check, _| check.output_of(tool).is_some())
}

fn end_fix(
    check: &Entity<SystemCheckView>,
    tool: Tool,
    status: ExitStatus,
    fixture: &mut Fixture<'_>,
) {
    let view = check
        .read_with(fixture.cx, |check, _| check.output_of(tool))
        .expect("the terminal shows under the row");

    view.update(fixture.cx, |_, cx| cx.emit(TerminalEvent::Exited(status)));
}

#[gpui::test]
fn a_fix_that_ended_stays_under_its_row_with_its_exit_status(cx: &mut TestAppContext) {
    let mut fixture = welcome_fixture(installable_cli(), Permission::Allowed, cx);
    let check = fixture.check();
    let before = fixture.seen.checks();

    fixture.click("system-check-action-github-cli");
    fixture.wait(|_, cx| output_shown(&check, Tool::GithubCli, cx));
    let first = check
        .read_with(fixture.cx, |check, _| check.output_of(Tool::GithubCli))
        .expect("the terminal shows under the row");
    end_fix(&check, Tool::GithubCli, ExitStatus::Code(1), &mut fixture);
    let asked = fixture.wait(|seen, _| seen.checks() == before + 1);
    let settled = fixture.wait(|_, cx| check.read_with(cx, |check, _| check.is_settled()));
    let rows = check.read_with(fixture.cx, |check, _| check.rows());
    let kept = output_shown(&check, Tool::GithubCli, fixture.cx);
    let retry = fixture
        .cx
        .debug_bounds("system-check-action-github-cli")
        .is_some();

    fixture.click("system-check-action-github-cli");
    let again = fixture.wait(|seen, _| seen.created().len() == 2);
    fixture.wait(|_, cx| output_shown(&check, Tool::GithubCli, cx));
    let second = check
        .read_with(fixture.cx, |check, _| check.output_of(Tool::GithubCli))
        .expect("the new terminal shows under the row");

    assert!(asked, "the check was not asked again");
    assert!(settled, "the check never answered");
    assert!(kept, "the output folded away");
    let row = row_of(&rows, Tool::GithubCli);
    assert_eq!(row.state, SystemCheckState::NotSetUp);
    assert_eq!(row.detail, "not installed \u{b7} exit 1");
    assert_eq!(
        row.action.as_ref().map(|action| action.kind),
        Some(ActionKind::Install)
    );
    assert!(retry, "the row has no button to try again");
    assert!(again, "Install did not start a new terminal");
    assert_ne!(first.entity_id(), second.entity_id());
    assert_eq!(fixture.seen.killed(), [], "the old terminal was stopped");
}

#[gpui::test]
fn a_fix_that_ended_folds_away_when_the_window_comes_back_and_the_tool_is_ready(
    cx: &mut TestAppContext,
) {
    let git = report_with(&[(
        Tool::Git,
        tool_check(
            ToolState::Missing,
            None,
            &["/usr/bin/xcode-select", "--install"],
        ),
    )]);
    let mut fixture = welcome_fixture(git, Permission::Allowed, cx);
    let check = fixture.check();
    let before = fixture.seen.checks();

    fixture.click("system-check-action-git");
    fixture.wait(|_, cx| output_shown(&check, Tool::Git, cx));
    end_fix(&check, Tool::Git, ExitStatus::Code(0), &mut fixture);
    fixture.wait(|seen, _| seen.checks() == before + 1);
    fixture.wait(|_, cx| check.read_with(cx, |check, _| check.is_settled()));
    let detail = check.read_with(fixture.cx, |check, _| {
        row_of(&check.rows(), Tool::Git).detail.clone()
    });
    let waiting = output_shown(&check, Tool::Git, fixture.cx);

    // Apple's installer finishes while the user is in another window.
    *fixture.report.lock().expect("lock the report") = report_with(&[]);
    fixture.cx.deactivate_window();
    fixture.cx.update(|window, _| window.activate_window());
    let folded = fixture.wait(|_, cx| !output_shown(&check, Tool::Git, cx));
    let ready = fixture.wait(|_, cx| check.read_with(cx, |check, _| check.count() == (6, 6)));

    assert_eq!(detail, "not installed \u{b7} exit 0");
    assert!(waiting, "the output folded before the tool was ready");
    assert!(folded, "the output did not fold away");
    assert!(ready);
}

#[gpui::test]
fn quitting_stops_a_running_fix(cx: &mut TestAppContext) {
    let mut fixture = welcome_fixture(installable_cli(), Permission::Allowed, cx);
    let check = fixture.check();

    fixture.click("system-check-action-github-cli");
    fixture.wait(|_, cx| output_shown(&check, Tool::GithubCli, cx));
    let before = fixture.seen.killed();

    fixture.cx.cx.update(App::shutdown);
    let seen = &fixture.seen;
    let stopped = wait_until(fixture.cx, |_| seen.killed().len() == 1);

    assert_eq!(before, []);
    assert!(stopped, "the program kept running after the quit");
}

#[gpui::test]
fn closing_the_screen_while_the_terminal_is_made_stops_it(cx: &mut TestAppContext) {
    let mut fixture = welcome_fixture(installable_cli(), Permission::Allowed, cx);
    let check = fixture.check();
    let screen = check.downgrade();
    let workspace = fixture.workspace.clone();
    let rows = check.read_with(fixture.cx, |check, _| check.rows());
    let action = row_of(&rows, Tool::GithubCli).action.clone();

    // Nothing runs the executor in between: the daemon has not made the terminal yet.
    fixture.cx.update(|window, cx| {
        check.update(cx, |check, cx| {
            let action = action.expect("the row has a button");

            check.start_fix(Tool::GithubCli, action, window, cx);
        });
    });
    drop(check);
    workspace.update(fixture.cx, Workspace::finish_welcome);
    fixture.cx.update(|window, cx| window.draw(cx).clear(cx));
    let gone = screen.upgrade().is_none();
    let stopped = fixture.wait(|seen, _| seen.killed().len() == 1);

    assert!(gone, "the screen was still there when the terminal came");
    assert!(stopped, "the terminal kept running with nothing to show it");
}

fn left_over(id: u64, task: Option<&str>, exit: Option<ExitStatus>) -> TerminalInfo {
    TerminalInfo {
        id: TerminalId(id),
        kind: swarm_protocol::TerminalKind::User,
        task: task.map(str::to_owned),
        owner: None,
        name: None,
        fixed_size: None,
        title: None,
        cwd: None,
        exit,
    }
}

#[gpui::test]
fn fix_terminals_left_by_an_earlier_run_are_stopped_when_the_app_connects(cx: &mut TestAppContext) {
    let listed = [
        left_over(7, Some("system-check"), None),
        left_over(8, Some("system-check"), Some(ExitStatus::Code(0))),
        left_over(9, Some("task:1"), None),
        left_over(10, None, None),
    ];
    let mut fixture =
        welcome_fixture_listing(None, &listed, report_with(&[]), Permission::Allowed, cx);

    fixture.wait(|seen, _| !seen.killed().is_empty());
    fixture.wait(|_, _| false);

    assert_eq!(fixture.seen.killed(), [TerminalId(7)]);
}
