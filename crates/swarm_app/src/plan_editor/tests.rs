//! Tests for the plan editor, on a fake daemon.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{Modifiers, TestAppContext, VisualTestContext};
use swarm_protocol::{AgentKind as DaemonKind, Event, PlanDraft, Request, TerminalId};

use super::*;
use crate::daemon::tests::{
    answer_late, drawn_quad, fake_daemon_replying, project_info, redraw, setup, start_agent,
    temp_settings_path, use_projects, wait_until,
};

type Requests = Arc<Mutex<Vec<Request>>>;

const TEMPLATE: &str = SAMPLE_PLAN_TEMPLATE;
/// The template with a line under its first heading, as the cursor leaves it.
const AFTER_FIRST_HEADING: usize = "## What\n".len();

struct Rig {
    workspace: Entity<Workspace>,
    editor: Entity<PlanEditor>,
    requests: Requests,
    client: Client,
}

/// What a rig sets up beyond the defaults.
#[derive(Default)]
struct Options {
    /// How long the app waits for the daemon to answer.
    timeout: Option<Duration>,
    /// The settings file the workspace reads.
    settings: Option<PathBuf>,
}

/// A workspace on a fake daemon that records every request. The daemon has `draft` saved,
/// answers with `reply` before the defaults, and starts the Start orchestrator with a
/// terminal of its own. The editor has not loaded yet.
fn open<'a>(
    draft: Option<&str>,
    reply: impl Fn(&Request) -> Option<Event> + Send + 'static,
    cx: &'a mut TestAppContext,
) -> (Rig, &'a mut VisualTestContext) {
    open_with(Options::default(), draft, reply, cx)
}

fn open_with<'a>(
    options: Options,
    draft: Option<&str>,
    reply: impl Fn(&Request) -> Option<Event> + Send + 'static,
    cx: &'a mut TestAppContext,
) -> (Rig, &'a mut VisualTestContext) {
    let requests = Requests::default();
    let record = Arc::clone(&requests);
    let draft = draft.map(str::to_owned);
    let socket = fake_daemon_replying(move |request| {
        record
            .lock()
            .expect("lock the requests")
            .push(request.clone());

        reply(&request).or_else(|| match &request {
            Request::PlanDraft { call, .. } => Some(Event::PlanDraft {
                call: *call,
                draft: PlanDraft {
                    text: draft.clone(),
                    template: TEMPLATE.to_owned(),
                },
            }),
            Request::StartOrchestrator { call, project, .. } => Some(Event::AgentStarted {
                call: *call,
                agent: AgentInfo {
                    terminal: Some(TerminalId(900)),
                    ..start_agent(100, project.0, swarm_protocol::AgentStatus::Idle)
                },
            }),
            _ => None,
        })
    });

    let (workspace, cx) = setup(cx);
    cx.executor().allow_parking();
    let mut client = Client::connect(&socket).expect("connect to the fake daemon");
    if let Some(timeout) = options.timeout {
        client = client.with_reply_timeout(timeout);
    }
    if let Some(path) = options.settings {
        workspace.update(cx, |workspace, _| workspace.settings_path = Some(path));
    }
    use_projects(&workspace, &[project_info(5, "demo", Vec::new())], cx);
    let attached = client.clone();
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.use_daemon(attached, &[], window, cx);
    });
    // Draws without waiting, so the draft is still on its way.
    cx.update(|window, cx| window.draw(cx).clear(cx));

    let editor = workspace
        .read_with(cx, |workspace, _| workspace.shown_plan_editor())
        .expect("the Start tab shows the plan editor");

    (
        Rig {
            workspace,
            editor,
            requests,
            client,
        },
        cx,
    )
}

fn loaded(rig: &Rig, cx: &mut VisualTestContext) -> bool {
    wait_until(cx, |cx| {
        redraw(cx);

        !rig.editor.read_with(cx, |editor, _| editor.is_loading())
    })
}

/// [`open`], then waits for the draft.
fn rig<'a>(
    draft: Option<&str>,
    reply: impl Fn(&Request) -> Option<Event> + Send + 'static,
    cx: &'a mut TestAppContext,
) -> (Rig, &'a mut VisualTestContext) {
    rig_with(Options::default(), draft, reply, cx)
}

/// [`open_with`], then waits for the draft.
fn rig_with<'a>(
    options: Options,
    draft: Option<&str>,
    reply: impl Fn(&Request) -> Option<Event> + Send + 'static,
    cx: &'a mut TestAppContext,
) -> (Rig, &'a mut VisualTestContext) {
    let (rig, cx) = open_with(options, draft, reply, cx);
    assert!(loaded(&rig, cx), "the draft did not load");

    (rig, cx)
}

/// A live workspace that has no daemon yet, so the editor cannot load its draft.
fn offline(
    cx: &mut TestAppContext,
) -> (
    Entity<Workspace>,
    Entity<PlanEditor>,
    &mut VisualTestContext,
) {
    let (workspace, cx) = setup(cx);
    cx.executor().allow_parking();
    use_projects(&workspace, &[project_info(5, "demo", Vec::new())], cx);
    redraw(cx);
    let editor = workspace
        .read_with(cx, |workspace, _| workspace.shown_plan_editor())
        .expect("the Start tab shows the plan editor");

    (workspace, editor, cx)
}

/// The daemon is back: a fake one that holds `draft` and records every request.
fn reconnect(workspace: &Entity<Workspace>, draft: &str, cx: &mut VisualTestContext) -> Requests {
    let requests = Requests::default();
    let record = Arc::clone(&requests);
    let draft = draft.to_owned();
    let socket = fake_daemon_replying(move |request| {
        record
            .lock()
            .expect("lock the requests")
            .push(request.clone());

        match request {
            Request::PlanDraft { call, .. } => Some(Event::PlanDraft {
                call,
                draft: PlanDraft {
                    text: Some(draft.clone()),
                    template: TEMPLATE.to_owned(),
                },
            }),
            _ => None,
        }
    });
    let client = Client::connect(&socket).expect("connect to the fake daemon");

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.use_daemon(client, &[], window, cx);
    });

    requests
}

fn no_reply(_: &Request) -> Option<Event> {
    None
}

fn text_of(rig: &Rig, cx: &VisualTestContext) -> String {
    rig.editor
        .read_with(cx, |editor, cx| editor.text.read(cx).text().to_string())
}

/// What the user does to the text: the new text, and the field says it changed. Work that
/// the edit starts only runs when the test waits.
fn edit(rig: &Rig, text: &str, cx: &mut VisualTestContext) {
    edit_in(&rig.editor, text, cx);
}

fn edit_in(editor: &Entity<PlanEditor>, text: &str, cx: &mut VisualTestContext) {
    let area = editor.read_with(cx, |editor, _| editor.text.clone());

    area.update(cx, |area, cx| {
        area.set_text(text.to_owned(), cx);
        cx.emit(TextInputEvent::Changed(text.to_owned().into()));
    });
}

/// Sends the plan as Send plan does, without waiting for the answer.
fn send(rig: &Rig, cx: &mut VisualTestContext) {
    rig.editor.update(cx, PlanEditor::send);
}

fn sending(rig: &Rig, cx: &VisualTestContext) -> bool {
    rig.editor.read_with(cx, |editor, _| editor.is_sending())
}

/// Waits until the plan is out and the editor answered.
fn sent(rig: &Rig, cx: &mut VisualTestContext) -> bool {
    wait_until(cx, |cx| {
        redraw(cx);

        !sending(rig, cx)
    })
}

fn can_send(rig: &Rig, cx: &VisualTestContext) -> bool {
    rig.editor.read_with(cx, PlanEditor::can_send)
}

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    redraw(cx);
    let bounds = cx.debug_bounds(selector);
    assert!(bounds.is_some(), "{selector} is not on screen");

    cx.simulate_click(
        bounds.map_or_else(Default::default, |bounds| bounds.center()),
        Modifiers::none(),
    );
    redraw(cx);
}

/// Lets the clock pass and the work it starts finish.
fn advance(duration: Duration, cx: &mut VisualTestContext) {
    cx.executor().advance_clock(duration);
    cx.executor().allow_parking();
    cx.run_until_parked();
}

/// Waits until the fake daemon has handled everything sent before.
fn flush(rig: &Rig, cx: &mut VisualTestContext) {
    cx.executor().allow_parking();
    cx.run_until_parked();
    rig.client.list().expect("the daemon answers");
}

/// The text of every `SavePlanDraft` the daemon got.
fn saves(rig: &Rig) -> Vec<Option<String>> {
    rig.requests
        .lock()
        .expect("lock the requests")
        .iter()
        .filter_map(|request| match request {
            Request::SavePlanDraft { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// The kind and the plan of every `StartOrchestrator` the daemon got.
fn starts(rig: &Rig) -> Vec<(Option<DaemonKind>, Option<String>)> {
    rig.requests
        .lock()
        .expect("lock the requests")
        .iter()
        .filter_map(|request| match request {
            Request::StartOrchestrator { kind, plan, .. } => Some((*kind, plan.clone())),
            _ => None,
        })
        .collect()
}

fn focused(rig: &Rig, cx: &mut VisualTestContext) -> bool {
    let handle = rig.editor.read_with(cx, PlanEditor::text_focus);

    cx.update(|window, _| handle.is_focused(window))
}

#[test]
fn the_cursor_starts_on_the_line_after_the_first_heading() {
    assert_eq!(after_first_heading(TEMPLATE), AFTER_FIRST_HEADING);
}

#[test]
fn a_template_without_a_heading_starts_at_the_top() {
    assert_eq!(after_first_heading("What\n\nWhy"), 0);
}

#[test]
fn a_heading_on_the_last_line_puts_the_cursor_at_the_end() {
    assert_eq!(
        after_first_heading("intro\n## What"),
        "intro\n## What".len()
    );
}

#[gpui::test]
fn the_editor_shows_the_stored_draft_with_the_cursor_at_the_end(cx: &mut TestAppContext) {
    let (rig, cx) = rig(Some("My stored plan"), no_reply, cx);
    let focus = focused(&rig, cx);

    cx.simulate_input("!");

    assert_eq!(text_of(&rig, cx), "My stored plan!");
    assert!(focus, "the editor does not have the focus");
}

#[gpui::test]
fn without_a_draft_the_editor_shows_the_template_with_the_cursor_under_its_first_heading(
    cx: &mut TestAppContext,
) {
    let (rig, cx) = rig(None, no_reply, cx);
    let before = text_of(&rig, cx);

    cx.simulate_input("X");

    assert_eq!(before, TEMPLATE);
    assert_eq!(
        text_of(&rig, cx),
        "## What\nX\n## Requirements\n\n## Definition of done"
    );
}

#[gpui::test]
fn the_text_and_send_plan_wait_for_the_draft(cx: &mut TestAppContext) {
    let (rig, cx) = open(Some("Late draft"), no_reply, cx);
    let waiting = rig.editor.read_with(cx, |editor, _| editor.is_loading());
    let shown = text_of(&rig, cx);
    edit(&rig, "Typed early", cx);
    let sendable = can_send(&rig, cx);

    let done = loaded(&rig, cx);

    assert!(waiting);
    assert_eq!(shown, "");
    assert!(!sendable);
    assert!(done);
    assert_eq!(text_of(&rig, cx), "Late draft");
}

#[gpui::test]
fn a_failed_load_shows_the_error_and_locks_the_editor(cx: &mut TestAppContext) {
    let (rig, cx) = rig(
        None,
        |request| match request {
            Request::PlanDraft { call, .. } => Some(Event::Error {
                call: Some(*call),
                id: None,
                message: "cannot read the draft".into(),
            }),
            _ => None,
        },
        cx,
    );
    let empty = text_of(&rig, cx);
    let error = rig
        .editor
        .read_with(cx, |editor, _| editor.error_text().map(str::to_owned));
    let locked = rig
        .editor
        .read_with(cx, |editor, cx| editor.text.read(cx).is_disabled());

    edit(&rig, "A plan", cx);

    assert_eq!(empty, "");
    assert!(error.is_some_and(|error| error.contains("cannot read the draft")));
    assert!(cx.debug_bounds("plan-error").is_some());
    assert!(locked);
    assert!(!can_send(&rig, cx));
}

#[gpui::test]
fn an_edit_is_saved_one_second_after_the_last_edit_and_not_before(cx: &mut TestAppContext) {
    let (rig, cx) = rig(None, no_reply, cx);

    cx.simulate_input("a");
    advance(Duration::from_millis(600), cx);
    cx.simulate_input("b");
    advance(Duration::from_millis(600), cx);
    flush(&rig, cx);
    let early = saves(&rig);

    advance(Duration::from_millis(400), cx);
    flush(&rig, cx);

    assert!(
        early.is_empty(),
        "saved before the edits stopped: {early:?}"
    );
    assert_eq!(
        saves(&rig),
        [Some(
            "## What\nab\n## Requirements\n\n## Definition of done".to_owned()
        )]
    );
}

#[gpui::test]
fn text_equal_to_the_template_saves_nothing_to_keep(cx: &mut TestAppContext) {
    let (rig, cx) = rig(Some("Old draft"), no_reply, cx);

    edit(&rig, &format!("{TEMPLATE}\n\n"), cx);
    advance(Duration::from_secs(1), cx);
    flush(&rig, cx);

    assert_eq!(saves(&rig), [None]);
}

#[gpui::test]
fn blank_text_saves_nothing_to_keep(cx: &mut TestAppContext) {
    let (rig, cx) = rig(Some("Old draft"), no_reply, cx);

    edit(&rig, " \n ", cx);
    advance(Duration::from_secs(1), cx);
    flush(&rig, cx);

    assert_eq!(saves(&rig), [None]);
}

#[gpui::test]
fn the_same_text_is_not_saved_twice(cx: &mut TestAppContext) {
    let (rig, cx) = rig(None, no_reply, cx);
    edit(&rig, "A plan", cx);
    advance(Duration::from_secs(1), cx);
    assert!(wait_until(cx, |_| saves(&rig).len() == 1));

    edit(&rig, "Another plan", cx);
    edit(&rig, "A plan", cx);
    advance(Duration::from_secs(1), cx);
    flush(&rig, cx);

    assert_eq!(saves(&rig), [Some("A plan".to_owned())]);
}

#[gpui::test]
fn an_unchanged_draft_is_not_saved_back(cx: &mut TestAppContext) {
    let (rig, cx) = rig(Some("Stored"), no_reply, cx);

    edit(&rig, "Stored", cx);
    advance(Duration::from_secs(1), cx);
    flush(&rig, cx);

    assert!(saves(&rig).is_empty());
}

#[gpui::test]
fn a_change_that_waits_for_its_timer_is_saved_when_the_app_quits(cx: &mut TestAppContext) {
    let (rig, cx) = rig(None, no_reply, cx);
    edit(&rig, "Almost lost", cx);

    cx.cx.update(App::shutdown);

    assert_eq!(saves(&rig), [Some("Almost lost".to_owned())]);
}

#[gpui::test]
fn nothing_is_saved_on_quit_when_nothing_changed(cx: &mut TestAppContext) {
    let (rig, cx) = rig(Some("Stored"), no_reply, cx);

    cx.cx.update(App::shutdown);

    assert!(saves(&rig).is_empty());
}

#[gpui::test]
fn nothing_is_saved_on_quit_while_the_plan_goes_out(cx: &mut TestAppContext) {
    let (rig, cx) = rig(None, no_reply, cx);
    edit(&rig, "Build the login page", cx);
    send(&rig, cx);

    cx.cx.update(App::shutdown);

    assert!(saves(&rig).is_empty(), "a save went out: {:?}", saves(&rig));
}

#[gpui::test]
fn send_plan_starts_the_chosen_orchestrator_with_the_text(cx: &mut TestAppContext) {
    let (rig, cx) = rig(None, no_reply, cx);
    rig.editor
        .update(cx, |editor, _| editor.choose_orchestrator(AgentKind::Codex));
    edit(&rig, "Build the login page", cx);

    click("plan-send", cx);
    flush(&rig, cx);

    assert_eq!(
        starts(&rig),
        [(
            Some(DaemonKind::Codex),
            Some("Build the login page".to_owned())
        )]
    );
}

#[gpui::test]
fn the_default_orchestrator_is_the_one_the_settings_name(cx: &mut TestAppContext) {
    let path = temp_settings_path("plan-editor");
    let mut settings = swarm_settings::Settings::default();
    settings.agents.orchestrator = swarm_settings::AgentKindSetting::Codex;
    swarm_settings::save(&path, &settings).expect("write the settings");
    let options = Options {
        settings: Some(path.clone()),
        ..Options::default()
    };

    let (rig, cx) = rig_with(options, None, no_reply, cx);
    std::fs::remove_file(&path).ok();

    let kind = rig
        .editor
        .read_with(cx, |editor, _| editor.orchestrator_kind());

    assert_eq!(kind, AgentKind::Codex);
}

#[gpui::test]
fn the_answer_replaces_the_editor_with_the_orchestrator_tile_and_its_focus(
    cx: &mut TestAppContext,
) {
    let (rig, cx) = rig(None, no_reply, cx);
    edit(&rig, "Build the login page", cx);
    // Something else holds the focus on the Start tab when the answer comes.
    rig.workspace.update(cx, |workspace, _| {
        workspace.focused_terminal = Some(crate::model::TerminalId(999));
    });

    click("plan-send", cx);
    let shown = wait_until(cx, |cx| {
        redraw(cx);

        rig.workspace
            .read_with(cx, |workspace, _| workspace.shown_plan_editor().is_none())
    });
    redraw(cx);

    let (tiles, terminal, focused_terminal, handle) =
        rig.workspace.read_with(cx, |workspace, _| {
            let terminal = workspace
                .data
                .agent_with_daemon_id(swarm_protocol::AgentId(100))
                .map(|agent| agent.terminal);

            (
                crate::views::tile_specs(workspace).len(),
                terminal,
                workspace.focused_terminal,
                terminal.and_then(|terminal| workspace.terminal_focus.get(&terminal).cloned()),
            )
        });
    assert!(shown, "the tab still shows the plan editor");
    assert_eq!(tiles, 1);
    assert!(terminal.is_some());
    assert_eq!(focused_terminal, terminal);
    assert!(
        handle.is_some_and(|handle| cx.update(|window, _| handle.is_focused(window))),
        "the terminal does not have the focus"
    );
    assert!(cx.debug_bounds("plan-editor").is_none());
}

#[gpui::test]
fn after_the_start_the_workspace_drops_the_editor_and_nothing_is_saved(cx: &mut TestAppContext) {
    let (rig, cx) = rig(Some("Stored"), no_reply, cx);
    edit(&rig, "Build the login page", cx);

    send(&rig, cx);
    assert!(sent(&rig, cx));
    advance(Duration::from_secs(2), cx);
    flush(&rig, cx);

    let kept = rig
        .workspace
        .read_with(cx, |workspace, _| workspace.plan_editors.len());
    assert_eq!(kept, 0, "the workspace still holds the editor");
    assert!(saves(&rig).is_empty(), "a save followed the send");
}

#[gpui::test]
fn the_editor_after_a_start_loads_what_the_daemon_holds(cx: &mut TestAppContext) {
    // The daemon answered the start, then the agent failed: the plan is still its draft.
    let (rig, cx) = rig(Some("The plan the start lost"), no_reply, cx);
    edit(&rig, "Build the login page", cx);
    send(&rig, cx);
    assert!(sent(&rig, cx));
    let project = rig.editor.read_with(cx, |editor, _| editor.project);

    let next = rig
        .workspace
        .update(cx, |workspace, cx| workspace.plan_editor(project, cx));
    let done = wait_until(cx, |cx| {
        !next.read_with(cx, |editor, _| editor.is_loading())
    });

    assert!(done, "the draft did not load");
    assert_ne!(next, rig.editor, "the old editor came back");
    assert_eq!(
        next.read_with(cx, |editor, cx| editor.text.read(cx).text().to_string()),
        "The plan the start lost"
    );
}

#[gpui::test]
fn the_send_cancels_the_save_that_waits_for_its_timer(cx: &mut TestAppContext) {
    let (rig, cx) = rig(None, no_reply, cx);
    edit(&rig, "Build the login page", cx);
    let waiting = rig
        .editor
        .read_with(cx, |editor, _| editor.save_timer.is_some());

    click("plan-send", cx);
    let cancelled = rig
        .editor
        .read_with(cx, |editor, _| editor.save_timer.is_none());
    advance(Duration::from_secs(5), cx);
    flush(&rig, cx);

    assert!(waiting);
    assert!(cancelled, "the timer of the save still runs after the send");
    assert_eq!(starts(&rig).len(), 1);
    assert!(saves(&rig).is_empty(), "a save went out: {:?}", saves(&rig));
}

#[gpui::test]
fn no_save_goes_out_while_the_plan_goes_out(cx: &mut TestAppContext) {
    let (rig, cx) = rig(None, no_reply, cx);
    edit(&rig, "Build the login page", cx);

    send(&rig, cx);
    let was_sending = sending(&rig, cx);
    rig.editor.update(cx, PlanEditor::save);
    assert!(sent(&rig, cx));
    advance(Duration::from_secs(5), cx);
    flush(&rig, cx);

    assert!(was_sending);
    assert!(saves(&rig).is_empty(), "a save went out: {:?}", saves(&rig));
}

#[gpui::test]
fn typing_while_the_plan_goes_out_changes_nothing(cx: &mut TestAppContext) {
    let (rig, cx) = rig(None, no_reply, cx);
    edit(&rig, "Build the login page", cx);

    send(&rig, cx);
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.simulate_input("!");

    assert_eq!(text_of(&rig, cx), "Build the login page");
}

fn refusing_the_start(request: &Request) -> Option<Event> {
    match request {
        Request::StartOrchestrator { call, .. } => Some(Event::Error {
            call: Some(*call),
            id: None,
            message: "refused".into(),
        }),
        _ => None,
    }
}

#[gpui::test]
fn a_refused_start_turns_the_text_back_on(cx: &mut TestAppContext) {
    let (rig, cx) = rig(None, refusing_the_start, cx);
    edit(&rig, "Build the login page", cx);

    send(&rig, cx);
    assert!(sent(&rig, cx));
    redraw(cx);
    cx.simulate_input("!");

    assert_eq!(text_of(&rig, cx), "Build the login page!");
}

#[gpui::test]
fn a_refused_start_saves_the_plan_again_and_keeps_its_error(cx: &mut TestAppContext) {
    let (rig, cx) = rig(None, refusing_the_start, cx);
    edit(&rig, "Build the login page", cx);

    send(&rig, cx);
    assert!(sent(&rig, cx));
    advance(Duration::from_secs(1), cx);
    flush(&rig, cx);
    redraw(cx);

    assert_eq!(saves(&rig), [Some("Build the login page".to_owned())]);
    assert!(
        rig.editor.read_with(cx, |editor, _| editor
            .error_text()
            .is_some_and(|error| error.contains("refused"))),
        "the good save hid the refusal"
    );
}

#[gpui::test]
fn a_start_without_an_answer_saves_nothing(cx: &mut TestAppContext) {
    // The daemon may have started: a save would bring the plan back.
    let options = Options {
        timeout: Some(Duration::from_millis(200)),
        ..Options::default()
    };
    let (rig, cx) = rig_with(
        options,
        None,
        |request| match request {
            Request::StartOrchestrator { .. } => {
                answer_late();
                None
            }
            _ => None,
        },
        cx,
    );
    edit(&rig, "Build the login page", cx);

    send(&rig, cx);
    assert!(sent(&rig, cx));
    advance(Duration::from_secs(2), cx);
    answer_late();
    flush(&rig, cx);

    assert!(
        rig.editor
            .read_with(cx, |editor, _| editor.error_text().is_some())
    );
    assert!(saves(&rig).is_empty(), "a save went out: {:?}", saves(&rig));
}

#[gpui::test]
fn no_save_goes_out_while_the_start_orchestrator_runs(cx: &mut TestAppContext) {
    let (rig, cx) = rig(None, no_reply, cx);
    edit(&rig, "Build the login page", cx);
    send(&rig, cx);
    assert!(sent(&rig, cx));

    edit(&rig, "More text", cx);
    advance(Duration::from_secs(2), cx);
    flush(&rig, cx);

    assert!(saves(&rig).is_empty(), "a save went out: {:?}", saves(&rig));
}

#[gpui::test]
fn send_plan_shows_its_work_while_the_plan_goes_out_and_sends_once(cx: &mut TestAppContext) {
    let (rig, cx) = rig(None, no_reply, cx);
    edit(&rig, "Build the login page", cx);

    send(&rig, cx);
    let was_sending = sending(&rig, cx);
    let can_send_now = can_send(&rig, cx);
    send(&rig, cx);
    assert!(sent(&rig, cx));
    flush(&rig, cx);

    assert!(was_sending);
    assert!(!can_send_now);
    assert_eq!(starts(&rig).len(), 1, "the plan was sent twice");
}

#[gpui::test]
fn send_plan_is_off_for_empty_text_and_for_the_bare_template(cx: &mut TestAppContext) {
    let (rig, cx) = rig(None, no_reply, cx);
    let template = can_send(&rig, cx);
    edit(&rig, "", cx);
    let empty = can_send(&rig, cx);
    edit(&rig, &format!("\n{TEMPLATE}\n"), cx);
    let spaced_template = can_send(&rig, cx);
    edit(&rig, "x", cx);
    let text = can_send(&rig, cx);

    edit(&rig, "", cx);
    redraw(cx);
    click("plan-send", cx);
    flush(&rig, cx);

    assert!(!template);
    assert!(!empty);
    assert!(!spaced_template);
    assert!(text);
    assert!(starts(&rig).is_empty(), "an empty plan was sent");
}

#[gpui::test]
fn cmd_enter_in_the_editor_sends_the_plan(cx: &mut TestAppContext) {
    let (rig, cx) = rig(None, no_reply, cx);
    cx.simulate_input("A plan");

    cx.simulate_keystrokes("cmd-enter");
    flush(&rig, cx);

    assert_eq!(
        starts(&rig),
        [(
            Some(DaemonKind::ClaudeCode),
            Some("## What\nA plan\n## Requirements\n\n## Definition of done".to_owned())
        )]
    );
}

#[gpui::test]
fn cmd_enter_on_the_bare_template_sends_nothing(cx: &mut TestAppContext) {
    let (rig, cx) = rig(None, no_reply, cx);

    cx.simulate_keystrokes("cmd-enter");
    flush(&rig, cx);

    assert!(starts(&rig).is_empty());
}

#[gpui::test]
fn a_refused_start_shows_the_error_keeps_the_text_and_turns_send_plan_back_on(
    cx: &mut TestAppContext,
) {
    let (rig, cx) = rig(
        None,
        |request| match request {
            Request::StartOrchestrator { call, .. } => Some(Event::Error {
                call: Some(*call),
                id: None,
                message: "the Start orchestrator already runs".into(),
            }),
            _ => None,
        },
        cx,
    );
    edit(&rig, "Build the login page", cx);

    click("plan-send", cx);
    let failed = wait_until(cx, |cx| {
        redraw(cx);

        rig.editor
            .read_with(cx, |editor, _| editor.error_text().is_some())
    });
    redraw(cx);

    let error = rig
        .editor
        .read_with(cx, |editor, _| editor.error_text().map(str::to_owned));
    let sending = rig.editor.read_with(cx, |editor, _| editor.is_sending());
    assert!(failed, "the refusal did not show");
    assert!(error.is_some_and(|error| error.contains("the Start orchestrator already runs")));
    assert!(cx.debug_bounds("plan-error").is_some());
    assert!(!sending);
    assert!(can_send(&rig, cx));
    assert_eq!(text_of(&rig, cx), "Build the login page");
    assert!(
        rig.workspace
            .read_with(cx, |workspace, _| workspace.shown_plan_editor().is_some()),
        "the tab left the editor"
    );
}

#[gpui::test]
fn a_new_send_clears_the_error(cx: &mut TestAppContext) {
    let refusals = Arc::new(Mutex::new(1));
    let (rig, cx) = rig(
        None,
        move |request| match request {
            Request::StartOrchestrator { call, .. } => {
                let mut left = refusals.lock().expect("lock the refusals");
                if *left == 0 {
                    return None;
                }
                *left -= 1;

                Some(Event::Error {
                    call: Some(*call),
                    id: None,
                    message: "refused once".into(),
                })
            }
            _ => None,
        },
        cx,
    );
    edit(&rig, "Build the login page", cx);
    send(&rig, cx);
    assert!(sent(&rig, cx));
    let refused = rig
        .editor
        .read_with(cx, |editor, _| editor.error_text().is_some());

    send(&rig, cx);
    let cleared = rig
        .editor
        .read_with(cx, |editor, _| editor.error_text().is_none());
    assert!(sent(&rig, cx));

    assert!(refused);
    assert!(cleared, "the old error stayed while the plan went out");
}

#[gpui::test]
fn a_sample_editor_holds_the_sample_plan_and_cannot_send(cx: &mut TestAppContext) {
    let (workspace, cx) = setup(cx);

    workspace.update(cx, |workspace, cx| {
        workspace.select_project(ProjectId(3), cx);
    });
    redraw(cx);

    let editor = workspace
        .read_with(cx, |workspace, _| workspace.shown_plan_editor())
        .expect("the Start tab shows the plan editor");
    let (text, loading, sendable) = editor.read_with(cx, |editor, cx| {
        (
            editor.text.read(cx).text().to_string(),
            editor.is_loading(),
            editor.can_send(cx),
        )
    });
    assert_eq!(text, SAMPLE_PLAN);
    assert!(!loading);
    assert!(!sendable);
}

#[gpui::test]
fn the_editor_takes_the_focus_when_the_start_tab_opens(cx: &mut TestAppContext) {
    let (workspace, cx) = setup(cx);

    workspace.update(cx, |workspace, cx| {
        workspace.select_project(ProjectId(3), cx);
    });
    redraw(cx);

    let editor = workspace
        .read_with(cx, |workspace, _| workspace.shown_plan_editor())
        .expect("the Start tab shows the plan editor");
    let handle = editor.read_with(cx, PlanEditor::text_focus);
    assert!(cx.update(|window, _| handle.is_focused(window)));
}

#[gpui::test]
fn tab_goes_from_the_editor_to_the_orchestrator_and_then_to_send_plan(cx: &mut TestAppContext) {
    let (rig, cx) = rig(None, no_reply, cx);
    edit(&rig, "A plan", cx);
    redraw(cx);
    let send = rig
        .editor
        .read_with(cx, |editor, _| editor.send_focus.clone());
    let start = (
        focused(&rig, cx),
        cx.update(|window, _| send.is_focused(window)),
    );

    cx.simulate_keystrokes("tab");
    redraw(cx);
    let middle = (
        focused(&rig, cx),
        cx.update(|window, _| send.is_focused(window)),
    );
    cx.simulate_keystrokes("tab");
    redraw(cx);
    let end = (
        focused(&rig, cx),
        cx.update(|window, _| send.is_focused(window)),
    );

    assert_eq!(start, (true, false));
    assert_eq!(middle, (false, false), "the orchestrator is not next");
    assert_eq!(
        end,
        (false, true),
        "Send plan is not after the orchestrator"
    );
}

fn refusing_the_draft(request: &Request) -> Option<Event> {
    match request {
        Request::PlanDraft { call, .. } => Some(Event::Error {
            call: Some(*call),
            id: None,
            message: "cannot read the draft".into(),
        }),
        _ => None,
    }
}

#[gpui::test]
fn a_failed_load_never_overwrites_the_stored_draft(cx: &mut TestAppContext) {
    let (rig, cx) = rig(None, refusing_the_draft, cx);

    edit(&rig, "Typed after the error", cx);
    advance(Duration::from_secs(2), cx);
    flush(&rig, cx);
    cx.cx.update(App::shutdown);

    assert!(saves(&rig).is_empty(), "a save went out: {:?}", saves(&rig));
}

#[gpui::test]
fn without_the_daemon_the_editor_says_so_and_loads_the_draft_when_it_is_back(
    cx: &mut TestAppContext,
) {
    let (workspace, editor, cx) = offline(cx);
    let error = editor.read_with(cx, |editor, _| editor.error_text().map(str::to_owned));
    let locked = editor.read_with(cx, |editor, cx| editor.text.read(cx).is_disabled());

    reconnect(&workspace, "Stored plan", cx);
    let done = wait_until(cx, |cx| {
        redraw(cx);

        editor.read_with(cx, |editor, cx| {
            editor.text.read(cx).text() == "Stored plan"
        })
    });

    assert_eq!(error.as_deref(), Some(NOT_RUNNING));
    assert!(locked, "the editor took text before its draft loaded");
    assert!(done, "the draft did not load after the daemon came back");
    assert!(editor.read_with(cx, |editor, _| editor.error_text().is_none()));
    assert!(!editor.read_with(cx, |editor, cx| editor.text.read(cx).is_disabled()));
}

#[gpui::test]
fn a_refused_save_shows_what_the_daemon_said(cx: &mut TestAppContext) {
    let (rig, cx) = rig(
        None,
        |request| match request {
            Request::SavePlanDraft { call, .. } => Some(Event::Error {
                call: Some(*call),
                id: None,
                message: "the disk is full".into(),
            }),
            _ => None,
        },
        cx,
    );

    edit(&rig, "A plan", cx);
    advance(Duration::from_secs(1), cx);
    let shown = wait_until(cx, |cx| {
        redraw(cx);

        rig.editor
            .read_with(cx, |editor, _| editor.error_text().is_some())
    });

    let error = rig
        .editor
        .read_with(cx, |editor, _| editor.error_text().map(str::to_owned));
    assert!(shown, "the refusal did not show");
    assert!(error.is_some_and(|error| error.contains("the disk is full")));
    assert!(cx.debug_bounds("plan-error").is_some());
}

#[gpui::test]
fn the_next_good_save_clears_the_save_error(cx: &mut TestAppContext) {
    let refusals = Arc::new(Mutex::new(1));
    let (rig, cx) = rig(
        None,
        move |request| match request {
            Request::SavePlanDraft { call, .. } => {
                let mut left = refusals.lock().expect("lock the refusals");
                if *left == 0 {
                    return None;
                }
                *left -= 1;

                Some(Event::Error {
                    call: Some(*call),
                    id: None,
                    message: "the disk is full".into(),
                })
            }
            _ => None,
        },
        cx,
    );
    edit(&rig, "A plan", cx);
    advance(Duration::from_secs(1), cx);
    assert!(wait_until(cx, |cx| {
        rig.editor
            .read_with(cx, |editor, _| editor.error_text().is_some())
    }));

    edit(&rig, "A better plan", cx);
    advance(Duration::from_secs(1), cx);
    let cleared = wait_until(cx, |cx| {
        rig.editor
            .read_with(cx, |editor, _| editor.error_text().is_none())
    });

    assert!(cleared, "the error stayed after a good save");
}

#[gpui::test(iterations = 20)]
fn the_save_on_quit_lands_after_the_save_before_it(cx: &mut TestAppContext) {
    let (rig, cx) = rig(None, no_reply, cx);
    edit(&rig, "one", cx);
    // A save is on its way: its call has not run yet.
    rig.editor.update(cx, PlanEditor::save);
    edit(&rig, "two", cx);

    cx.cx.update(App::shutdown);

    assert_eq!(
        saves(&rig),
        [Some("one".to_owned()), Some("two".to_owned())]
    );
}

#[gpui::test]
fn a_draft_that_loads_late_does_not_pull_the_focus_from_where_the_user_went(
    cx: &mut TestAppContext,
) {
    let (rig, cx) = open(Some("Late draft"), no_reply, cx);
    let send = rig
        .editor
        .read_with(cx, |editor, _| editor.send_focus.clone());

    cx.update(|window, cx| send.focus(window, cx));
    let done = loaded(&rig, cx);
    redraw(cx);

    assert!(done, "the draft did not load");
    assert!(!focused(&rig, cx), "the editor took the focus");
    assert!(cx.update(|window, _| send.is_focused(window)));
}

#[gpui::test]
fn the_text_box_has_a_line_border_that_turns_accent_while_the_text_has_focus(
    cx: &mut TestAppContext,
) {
    let (rig, cx) = rig(None, no_reply, cx);
    let send = rig
        .editor
        .read_with(cx, |editor, _| editor.send_focus.clone());
    let (line, accent) = cx.update(|_, cx| {
        let colors = cx.theme().colors();

        (colors.line, colors.accent)
    });
    redraw(cx);
    let with_focus = drawn_quad("plan-text", cx);

    cx.update(|window, cx| send.focus(window, cx));
    redraw(cx);
    let without_focus = drawn_quad("plan-text", cx);

    assert_eq!(with_focus.map(|quad| quad.border_color), Some(accent));
    assert_eq!(
        without_focus.as_ref().map(|quad| quad.border_color),
        Some(line)
    );
    assert!(
        without_focus.is_some_and(|quad| quad.border_widths.top.0 > 0.),
        "the box has no border"
    );
}
