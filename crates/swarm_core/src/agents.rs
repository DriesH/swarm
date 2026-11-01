//! Agent status: merges OSC 7501 reports, hook events and the process exit.
//!
//! The rules come from `docs/spikes/agent-status.md`. Nothing here touches a socket or
//! a process, so every rule has a plain unit test.

use std::fmt::Write as _;
use std::io::{self, Read as _};

use serde_json::Value;
use swarm_protocol::AgentStatus;
use swarm_term::{BlockedKind, ExitStatus, ProgramState, ProgramStatus};

/// The message shown when the folder trust dialog waits for an answer.
pub const TRUST_MESSAGE: &str = "Trust this folder";

/// The notice text of a failure that did not say why.
const FAILURE_FALLBACK: &str = "Failed";

/// What a hook changed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HookOutcome {
    /// The status or message changed.
    pub changed: bool,
    pub session_id: Option<String>,
    /// The event was `SessionStart`: the agent is up.
    pub session_started: bool,
    /// The event was `Stop`: the agent finished its turn.
    pub stopped: bool,
}

/// One agent's status. Before the first OSC 7501 report the hooks drive it. After that
/// OSC 7501 drives it, because no hook covers every case (Claude Code runs none when
/// the user presses Esc), and hooks only add the session ID and the `StopFailure` reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusMerge {
    status: AgentStatus,
    message: Option<String>,
    osc_seen: bool,
    hook_seen: bool,
    /// Set by `notify`: the agent shows as blocked with this text over `status`.
    notice: Option<String>,
    /// `status` became blocked or an error, and `take_prompt` or `take_failure` has not
    /// told yet.
    news_due: bool,
}

impl Default for StatusMerge {
    fn default() -> Self {
        Self {
            status: AgentStatus::Unknown,
            message: None,
            osc_seen: false,
            hook_seen: false,
            notice: None,
            news_due: false,
        }
    }
}

impl StatusMerge {
    pub fn status(&self) -> AgentStatus {
        if self.notice.is_some() {
            return AgentStatus::Blocked(BlockedKind::Other);
        }

        self.status
    }

    pub fn message(&self) -> Option<&str> {
        self.notice.as_deref().or(self.message.as_deref())
    }

    /// Shows the agent as blocked with `text` until it works again or the app sends
    /// input to its terminal. Returns true when the status or message changed.
    pub fn notify(&mut self, text: &str) -> bool {
        let before = self.visible();
        self.notice = Some(text.to_owned());

        before != self.visible()
    }

    /// The user typed or pasted into the agent's terminal: a notice no longer applies.
    pub fn app_input(&mut self) -> bool {
        let before = self.visible();
        self.notice = None;

        before != self.visible()
    }

    /// The agent's own status is idle or done, whatever a notice shows over it.
    pub fn is_resting(&self) -> bool {
        matches!(self.status, AgentStatus::Idle | AgentStatus::Done)
    }

    /// `notify` shows a notice that the agent has not worked past yet.
    pub fn has_notice(&self) -> bool {
        self.notice.is_some()
    }

    /// The text of the prompt the agent just blocked on. Returns it once per blocked
    /// spell: a repeat of the same state, or a new message in it, is not news. A notice
    /// from `notify` is not a prompt.
    pub fn take_prompt(&mut self) -> Option<String> {
        let AgentStatus::Blocked(kind) = self.status else {
            return None;
        };
        if !std::mem::take(&mut self.news_due) {
            return None;
        }

        let message = self
            .message
            .as_deref()
            .filter(|message| !message.is_empty());

        Some(message.map_or_else(|| prompt_reason(kind).to_owned(), str::to_owned))
    }

    /// The text of the failure the agent just reported. Returns it once per error spell,
    /// whether OSC 7501 or a `StopFailure` hook reported it first: the other one is not
    /// news. A new spell starts after the agent left the error state.
    pub fn take_failure(&mut self) -> Option<String> {
        if self.status != AgentStatus::Error || !std::mem::take(&mut self.news_due) {
            return None;
        }

        let message = self
            .message
            .as_deref()
            .filter(|message| !message.is_empty());

        Some(message.unwrap_or(FAILURE_FALLBACK).to_owned())
    }

    /// The agent is in an error spell and `take_failure` already told about it, so the
    /// process exit that follows is not news. Reads the agent's own status, not a notice.
    pub fn failure_told(&self) -> bool {
        self.status == AgentStatus::Error && !self.news_due
    }

    fn visible(&self) -> (AgentStatus, Option<String>) {
        (self.status(), self.message().map(str::to_owned))
    }

    /// Nothing reported yet, so the screen is the only thing that can tell more.
    pub fn is_silent(&self) -> bool {
        !self.osc_seen && !self.hook_seen
    }

    /// Returns true when the status or message changed.
    pub fn program_status(&mut self, report: &ProgramStatus) -> bool {
        self.osc_seen = true;

        let (status, message) = match report.state {
            ProgramState::Idle => (AgentStatus::Idle, report.message.clone()),
            ProgramState::Working => (AgentStatus::Working, report.message.clone()),
            ProgramState::Done => (AgentStatus::Done, report.message.clone()),
            ProgramState::Blocked(kind) => (AgentStatus::Blocked(kind), report.message.clone()),
            ProgramState::Error => (AgentStatus::Error, report.message.clone()),
            ProgramState::Clear => (AgentStatus::Unknown, None),
        };

        if matches!(report.state, ProgramState::Working | ProgramState::Error) {
            self.notice = None;
        }

        self.set(status, message)
    }

    /// `event` is the JSON a Claude Code or Codex hook received on stdin.
    pub fn hook(&mut self, event: &Value) -> HookOutcome {
        self.hook_seen = true;

        let name = event
            .get("hook_event_name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let session_id = event
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned);

        let applies = !self.osc_seen || name == "StopFailure";
        let changed = match hook_state(name, event) {
            Some((status, message)) if applies => {
                if status == AgentStatus::Working {
                    self.notice = None;
                }

                self.set(status, message)
            }
            _ => false,
        };

        HookOutcome {
            changed,
            session_id,
            session_started: name == "SessionStart",
            stopped: name == "Stop",
        }
    }

    /// Applies the process exit. `Done` and `Error` stay, `Working` and `Blocked` are
    /// stale and become `Unknown`, and a non-zero exit code is an error.
    pub fn process_exited(&mut self, exit: Option<&ExitStatus>) -> bool {
        let had_notice = self.notice.take().is_some();
        let code = match exit {
            Some(ExitStatus::Code(code)) => Some(*code),
            _ => None,
        };

        let (status, message) = match (self.status, code) {
            (AgentStatus::Done | AgentStatus::Error, _) => return had_notice,
            (_, Some(code)) if code != 0 => (AgentStatus::Error, exit_failure(exit)),
            (AgentStatus::Working | AgentStatus::Blocked(_), _) => (AgentStatus::Unknown, None),
            _ => return had_notice,
        };

        self.set(status, message) | had_notice
    }

    /// Marks the agent as waiting for the user's answer to the folder trust dialog.
    /// Only applies while nothing else has reported.
    pub fn trust_prompt_visible(&mut self) -> bool {
        if !self.is_silent() {
            return false;
        }

        self.set(
            AgentStatus::Blocked(BlockedKind::Other),
            Some(TRUST_MESSAGE.to_owned()),
        )
    }

    /// For an agent that could not start.
    pub fn fail(&mut self, message: &str) -> bool {
        self.notice = None;

        self.set(AgentStatus::Error, Some(message.to_owned()))
    }

    fn set(&mut self, status: AgentStatus, message: Option<String>) -> bool {
        let before = self.visible();
        let was_blocked = matches!(self.status, AgentStatus::Blocked(_));
        let was_error = self.status == AgentStatus::Error;
        self.status = status;
        self.message = message;

        let blocked = matches!(status, AgentStatus::Blocked(_));
        let failed = status == AgentStatus::Error;
        let entered = (blocked && !was_blocked) || (failed && !was_error);
        self.news_due = (blocked || failed) && (self.news_due || entered);

        before != self.visible()
    }
}

/// What a prompt of this kind says when the agent gave no question.
fn prompt_reason(kind: BlockedKind) -> &'static str {
    match kind {
        BlockedKind::Permission => "Permission prompt",
        BlockedKind::Question => "Question prompt",
        BlockedKind::Auth => "Login prompt",
        BlockedKind::Other => "Prompt",
    }
}

/// Why a process that ended counts as broken: an error code or a signal. `None` for a
/// clean exit or an unknown one.
pub fn exit_failure(exit: Option<&ExitStatus>) -> Option<String> {
    match exit? {
        ExitStatus::Code(0) => None,
        ExitStatus::Code(code) => Some(format!("Exited with code {code}")),
        ExitStatus::Signal(name) => Some(format!("Ended by signal {name}")),
    }
}

/// Maps a hook event to a status. Event names are Claude Code's; Codex uses the same
/// names for the events it has.
fn hook_state(name: &str, event: &Value) -> Option<(AgentStatus, Option<String>)> {
    let text = |key: &str| {
        event
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };

    let state = match name {
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => (AgentStatus::Working, None),
        // Claude Code asks AskUserQuestion through the permission flow too.
        "PermissionRequest" if text("tool_name").as_deref() == Some("AskUserQuestion") => {
            (AgentStatus::Blocked(BlockedKind::Question), None)
        }
        "PermissionRequest" => (
            AgentStatus::Blocked(BlockedKind::Permission),
            text("tool_name"),
        ),
        "Notification" => match text("notification_type").as_deref() {
            Some("permission_prompt") => (
                AgentStatus::Blocked(BlockedKind::Permission),
                text("message"),
            ),
            Some("elicitation_dialog" | "elicitation_url_dialog" | "agent_needs_input") => {
                (AgentStatus::Blocked(BlockedKind::Question), text("message"))
            }
            Some("idle_prompt") => (AgentStatus::Idle, None),
            _ => return None,
        },
        "Stop" => (AgentStatus::Done, None),
        // Codex only; Claude Code runs no hook when the user interrupts.
        "SessionStart" | "Interrupt" => (AgentStatus::Idle, None),
        "StopFailure" => (AgentStatus::Error, text("error")),
        _ => return None,
    };

    Some(state)
}

/// Whether the screen shows the folder trust dialog of Claude Code or Codex (wording
/// from the Codex 0.166 `trust_directory` onboarding widget).
pub fn is_trust_prompt(screen: &str) -> bool {
    let screen = screen.to_lowercase();

    screen.contains("yes, i trust this folder")
        || screen.contains("do you trust the")
        || (screen.contains("trust and continue") && screen.contains("codex can read"))
}

/// A random version 4 UUID, for the session ID of a new Claude Code agent.
pub fn new_session_id() -> io::Result<String> {
    let mut bytes = random_bytes::<16>()?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    let hex = to_hex(&bytes);

    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

/// 32 random bytes as hex: the secret an agent shows on the agent socket.
pub fn new_token() -> io::Result<String> {
    Ok(to_hex(&random_bytes::<32>()?))
}

fn random_bytes<const N: usize>() -> io::Result<[u8; N]> {
    let mut bytes = [0_u8; N];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;

    Ok(bytes)
}

pub(crate) fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut hex, byte| {
        write!(hex, "{byte:02x}").ok();

        hex
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn osc(state: ProgramState) -> ProgramStatus {
        ProgramStatus {
            state,
            message: None,
        }
    }

    fn hook(merge: &mut StatusMerge, value: &Value) -> HookOutcome {
        merge.hook(value)
    }

    #[test]
    fn a_new_agent_is_unknown() {
        let merge = StatusMerge::default();

        assert_eq!(merge.status(), AgentStatus::Unknown);
        assert_eq!(merge.message(), None);
    }

    #[test]
    fn hooks_drive_the_status_before_the_first_osc_report() {
        let mut merge = StatusMerge::default();

        hook(&mut merge, &json!({"hook_event_name": "SessionStart"}));
        assert_eq!(merge.status(), AgentStatus::Idle);

        hook(&mut merge, &json!({"hook_event_name": "UserPromptSubmit"}));
        assert_eq!(merge.status(), AgentStatus::Working);

        hook(&mut merge, &json!({"hook_event_name": "Stop"}));
        assert_eq!(merge.status(), AgentStatus::Done);
    }

    #[test]
    fn osc_drives_the_status_after_the_first_report() {
        let mut merge = StatusMerge::default();
        merge.program_status(&osc(ProgramState::Working));

        let outcome = hook(&mut merge, &json!({"hook_event_name": "Stop"}));

        assert_eq!(merge.status(), AgentStatus::Working);
        assert!(!outcome.changed);
    }

    #[test]
    fn hooks_still_report_the_session_after_osc() {
        let mut merge = StatusMerge::default();
        merge.program_status(&osc(ProgramState::Idle));

        let outcome = hook(
            &mut merge,
            &json!({"hook_event_name": "SessionStart", "session_id": "abc"}),
        );

        assert_eq!(outcome.session_id.as_deref(), Some("abc"));
        assert!(outcome.session_started);
    }

    #[test]
    fn stop_failure_sets_the_error_reason_after_osc() {
        let mut merge = StatusMerge::default();
        merge.program_status(&osc(ProgramState::Working));

        let outcome = hook(
            &mut merge,
            &json!({"hook_event_name": "StopFailure", "error": "model_not_found"}),
        );

        assert!(outcome.changed);
        assert_eq!(merge.status(), AgentStatus::Error);
        assert_eq!(merge.message(), Some("model_not_found"));
    }

    #[test]
    fn osc_error_text_replaces_the_stop_failure_reason() {
        let mut merge = StatusMerge::default();
        hook(
            &mut merge,
            &json!({"hook_event_name": "StopFailure", "error": "model_not_found"}),
        );

        merge.program_status(&ProgramStatus {
            state: ProgramState::Error,
            message: Some("There is an issue with the model".to_owned()),
        });

        assert_eq!(merge.message(), Some("There is an issue with the model"));
    }

    #[test]
    fn osc_clear_means_unknown() {
        let mut merge = StatusMerge::default();
        merge.program_status(&ProgramStatus {
            state: ProgramState::Working,
            message: Some("Writing".to_owned()),
        });

        let changed = merge.program_status(&osc(ProgramState::Clear));

        assert!(changed);
        assert_eq!(merge.status(), AgentStatus::Unknown);
        assert_eq!(merge.message(), None);
    }

    #[test]
    fn osc_keeps_the_blocked_kind_and_message() {
        let mut merge = StatusMerge::default();

        merge.program_status(&ProgramStatus {
            state: ProgramState::Blocked(BlockedKind::Permission),
            message: Some("approve Write".to_owned()),
        });

        assert_eq!(
            merge.status(),
            AgentStatus::Blocked(BlockedKind::Permission)
        );
        assert_eq!(merge.message(), Some("approve Write"));
    }

    #[test]
    fn an_unchanged_osc_report_is_not_a_change() {
        let mut merge = StatusMerge::default();
        merge.program_status(&osc(ProgramState::Idle));

        assert!(!merge.program_status(&osc(ProgramState::Idle)));
    }

    #[test]
    fn ask_user_question_is_a_question() {
        let mut merge = StatusMerge::default();

        hook(
            &mut merge,
            &json!({"hook_event_name": "PermissionRequest", "tool_name": "AskUserQuestion"}),
        );

        assert_eq!(merge.status(), AgentStatus::Blocked(BlockedKind::Question));
    }

    #[test]
    fn another_permission_request_is_a_permission() {
        let mut merge = StatusMerge::default();

        hook(
            &mut merge,
            &json!({"hook_event_name": "PermissionRequest", "tool_name": "Write"}),
        );

        assert_eq!(
            merge.status(),
            AgentStatus::Blocked(BlockedKind::Permission)
        );
        assert_eq!(merge.message(), Some("Write"));
    }

    #[test]
    fn notifications_map_to_blocked_or_idle() {
        let mut merge = StatusMerge::default();

        hook(
            &mut merge,
            &json!({"hook_event_name": "Notification", "notification_type": "elicitation_dialog"}),
        );
        assert_eq!(merge.status(), AgentStatus::Blocked(BlockedKind::Question));

        hook(
            &mut merge,
            &json!({"hook_event_name": "Notification", "notification_type": "idle_prompt"}),
        );
        assert_eq!(merge.status(), AgentStatus::Idle);
    }

    #[test]
    fn an_unknown_notification_changes_nothing() {
        let mut merge = StatusMerge::default();

        let outcome = hook(
            &mut merge,
            &json!({"hook_event_name": "Notification", "notification_type": "auth_success"}),
        );

        assert!(!outcome.changed);
        assert_eq!(merge.status(), AgentStatus::Unknown);
    }

    #[test]
    fn interrupt_returns_to_idle() {
        let mut merge = StatusMerge::default();
        hook(&mut merge, &json!({"hook_event_name": "UserPromptSubmit"}));

        hook(&mut merge, &json!({"hook_event_name": "Interrupt"}));

        assert_eq!(merge.status(), AgentStatus::Idle);
    }

    #[test]
    fn session_end_does_not_change_the_status() {
        let mut merge = StatusMerge::default();
        hook(&mut merge, &json!({"hook_event_name": "Stop"}));

        hook(&mut merge, &json!({"hook_event_name": "SessionEnd"}));

        assert_eq!(merge.status(), AgentStatus::Done);
    }

    #[test]
    fn a_hook_without_a_session_reports_none() {
        let mut merge = StatusMerge::default();

        let outcome = hook(&mut merge, &json!({"hook_event_name": "Stop"}));

        assert_eq!(outcome.session_id, None);
    }

    #[test]
    fn done_and_error_survive_the_exit() {
        for status in [ProgramState::Done, ProgramState::Error] {
            let mut merge = StatusMerge::default();
            merge.program_status(&osc(status));
            let before = merge.status();

            merge.process_exited(Some(&ExitStatus::Code(1)));

            assert_eq!(merge.status(), before);
        }
    }

    #[test]
    fn working_and_blocked_become_unknown_on_exit() {
        for state in [
            ProgramState::Working,
            ProgramState::Blocked(BlockedKind::Permission),
        ] {
            let mut merge = StatusMerge::default();
            merge.program_status(&osc(state));

            merge.process_exited(Some(&ExitStatus::Code(0)));

            assert_eq!(merge.status(), AgentStatus::Unknown);
        }
    }

    #[test]
    fn a_non_zero_exit_is_an_error() {
        let mut merge = StatusMerge::default();
        merge.program_status(&osc(ProgramState::Idle));

        merge.process_exited(Some(&ExitStatus::Code(2)));

        assert_eq!(merge.status(), AgentStatus::Error);
        assert_eq!(merge.message(), Some("Exited with code 2"));
    }

    #[test]
    fn a_non_zero_exit_beats_a_stale_working_state() {
        let mut merge = StatusMerge::default();
        merge.program_status(&osc(ProgramState::Working));

        merge.process_exited(Some(&ExitStatus::Code(1)));

        assert_eq!(merge.status(), AgentStatus::Error);
    }

    #[test]
    fn a_clean_exit_keeps_idle() {
        let mut merge = StatusMerge::default();
        merge.program_status(&osc(ProgramState::Idle));

        let changed = merge.process_exited(Some(&ExitStatus::Code(0)));

        assert!(!changed);
        assert_eq!(merge.status(), AgentStatus::Idle);
    }

    #[test]
    fn a_signal_exit_is_not_an_error_by_itself() {
        let mut merge = StatusMerge::default();
        merge.program_status(&osc(ProgramState::Working));

        merge.process_exited(Some(&ExitStatus::Signal("Hangup".to_owned())));

        assert_eq!(merge.status(), AgentStatus::Unknown);
    }

    #[test]
    fn the_trust_prompt_blocks_a_silent_agent() {
        let mut merge = StatusMerge::default();

        let changed = merge.trust_prompt_visible();

        assert!(changed);
        assert_eq!(merge.status(), AgentStatus::Blocked(BlockedKind::Other));
        assert_eq!(merge.message(), Some(TRUST_MESSAGE));
    }

    #[test]
    fn the_trust_prompt_is_ignored_once_the_agent_reports() {
        let mut merge = StatusMerge::default();
        merge.program_status(&osc(ProgramState::Idle));

        assert!(!merge.trust_prompt_visible());
        assert_eq!(merge.status(), AgentStatus::Idle);
    }

    #[test]
    fn a_report_replaces_the_trust_prompt() {
        let mut merge = StatusMerge::default();
        merge.trust_prompt_visible();

        hook(&mut merge, &json!({"hook_event_name": "SessionStart"}));

        assert_eq!(merge.status(), AgentStatus::Idle);
    }

    #[test]
    fn a_failed_start_is_an_error_with_a_reason() {
        let mut merge = StatusMerge::default();

        merge.fail("Setup failed");

        assert_eq!(merge.status(), AgentStatus::Error);
        assert_eq!(merge.message(), Some("Setup failed"));
    }

    #[test]
    fn both_trust_dialogs_are_recognised() {
        assert!(is_trust_prompt(
            "  1. Yes, I trust this folder\n  2. No, exit"
        ));
        assert!(is_trust_prompt(
            "Do you trust the contents of this directory?"
        ));
    }

    #[test]
    fn the_codex_trust_dialog_is_recognised() {
        assert!(is_trust_prompt(
            "  Folder access\n  Trust this folder? Codex can read, edit, and run files here,\n\n› 1. Trust and continue\n  2. Quit"
        ));
    }

    #[test]
    fn a_normal_screen_is_not_a_trust_prompt() {
        assert!(!is_trust_prompt("> Try \"fix the failing test\""));
    }

    #[test]
    fn a_session_id_is_a_version_4_uuid() {
        let id = new_session_id().expect("uuid");

        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(
            parts.iter().map(|part| part.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert!(parts[2].starts_with('4'));
        assert!(matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'));
    }

    #[test]
    fn a_token_is_64_hex_characters_and_never_repeats() {
        let first = new_token().expect("token");
        let second = new_token().expect("token");

        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }
    #[test]
    fn a_notice_shows_the_agent_as_blocked_with_the_text() {
        let mut merge = StatusMerge::default();
        merge.program_status(&osc(ProgramState::Idle));

        let changed = merge.notify("Review needed");

        assert!(changed);
        assert_eq!(merge.status(), AgentStatus::Blocked(BlockedKind::Other));
        assert_eq!(merge.message(), Some("Review needed"));
    }

    #[test]
    fn a_notice_stays_through_idle_and_done_reports() {
        let mut merge = StatusMerge::default();
        merge.notify("Review needed");

        let idle = merge.program_status(&osc(ProgramState::Idle));
        let done = merge.program_status(&osc(ProgramState::Done));

        assert!(!idle && !done);
        assert_eq!(merge.status(), AgentStatus::Blocked(BlockedKind::Other));
        assert_eq!(merge.message(), Some("Review needed"));
    }

    #[test]
    fn working_clears_a_notice() {
        let mut merge = StatusMerge::default();
        merge.program_status(&osc(ProgramState::Idle));
        merge.notify("Review needed");

        let changed = merge.program_status(&osc(ProgramState::Working));

        assert!(changed);
        assert_eq!(merge.status(), AgentStatus::Working);
        assert_eq!(merge.message(), None);
    }

    #[test]
    fn a_working_hook_clears_a_notice_before_any_osc_report() {
        let mut merge = StatusMerge::default();
        merge.notify("Review needed");

        hook(&mut merge, &json!({"hook_event_name": "UserPromptSubmit"}));

        assert_eq!(merge.status(), AgentStatus::Working);
    }

    #[test]
    fn app_input_clears_a_notice_and_shows_the_real_status() {
        let mut merge = StatusMerge::default();
        merge.program_status(&osc(ProgramState::Idle));
        merge.notify("Review needed");
        merge.program_status(&osc(ProgramState::Done));

        let changed = merge.app_input();

        assert!(changed);
        assert_eq!(merge.status(), AgentStatus::Done);
        assert_eq!(merge.message(), None);
        assert!(!merge.app_input());
    }

    #[test]
    fn notifying_twice_with_the_same_text_changes_nothing() {
        let mut merge = StatusMerge::default();
        merge.notify("Review needed");

        assert!(!merge.notify("Review needed"));
        assert!(merge.notify("Another"));
    }

    #[test]
    fn the_process_exit_clears_a_notice() {
        let mut merge = StatusMerge::default();
        merge.program_status(&osc(ProgramState::Idle));
        merge.notify("Review needed");

        assert!(merge.process_exited(Some(&ExitStatus::Code(0))));
        assert_eq!(merge.status(), AgentStatus::Idle);
    }

    #[test]
    fn a_stop_hook_is_flagged() {
        let mut merge = StatusMerge::default();

        assert!(hook(&mut merge, &json!({"hook_event_name": "Stop"})).stopped);
        assert!(!hook(&mut merge, &json!({"hook_event_name": "SessionStart"})).stopped);
    }

    fn blocked(kind: BlockedKind, message: Option<&str>) -> ProgramStatus {
        ProgramStatus {
            state: ProgramState::Blocked(kind),
            message: message.map(str::to_owned),
        }
    }

    #[test]
    fn a_prompt_is_taken_once_with_its_question() {
        let mut merge = StatusMerge::default();
        merge.program_status(&blocked(BlockedKind::Permission, Some("approve Write")));

        assert_eq!(merge.take_prompt().as_deref(), Some("approve Write"));
        assert_eq!(merge.take_prompt(), None);
    }

    #[test]
    fn a_prompt_without_a_question_gets_a_short_reason() {
        for (kind, reason) in [
            (BlockedKind::Permission, "Permission prompt"),
            (BlockedKind::Question, "Question prompt"),
            (BlockedKind::Auth, "Login prompt"),
            (BlockedKind::Other, "Prompt"),
        ] {
            let mut merge = StatusMerge::default();
            merge.program_status(&blocked(kind, None));

            assert_eq!(merge.take_prompt().as_deref(), Some(reason), "{kind:?}");
        }
    }

    #[test]
    fn a_repeat_or_a_change_inside_one_blocked_spell_is_not_a_new_prompt() {
        let mut merge = StatusMerge::default();
        merge.program_status(&blocked(BlockedKind::Permission, Some("approve Write")));
        merge.take_prompt();

        merge.program_status(&blocked(BlockedKind::Permission, Some("approve Write")));
        assert_eq!(merge.take_prompt(), None);

        merge.program_status(&blocked(BlockedKind::Question, Some("another text")));
        assert_eq!(merge.take_prompt(), None);
    }

    #[test]
    fn a_prompt_after_the_agent_worked_again_is_a_new_prompt() {
        let mut merge = StatusMerge::default();
        merge.program_status(&blocked(BlockedKind::Permission, None));
        merge.take_prompt();

        merge.program_status(&osc(ProgramState::Working));
        assert_eq!(merge.take_prompt(), None);
        merge.program_status(&blocked(BlockedKind::Permission, None));

        assert_eq!(merge.take_prompt().as_deref(), Some("Permission prompt"));
    }

    #[test]
    fn a_prompt_that_ended_before_it_was_taken_is_gone() {
        let mut merge = StatusMerge::default();
        merge.program_status(&blocked(BlockedKind::Permission, None));
        merge.program_status(&osc(ProgramState::Working));

        assert_eq!(merge.take_prompt(), None);
    }

    #[test]
    fn a_message_that_comes_before_it_is_taken_replaces_the_reason() {
        let mut merge = StatusMerge::default();
        merge.program_status(&blocked(BlockedKind::Permission, None));
        merge.program_status(&blocked(BlockedKind::Permission, Some("approve Edit")));

        assert_eq!(merge.take_prompt().as_deref(), Some("approve Edit"));
    }

    #[test]
    fn a_notice_is_not_a_prompt() {
        let mut merge = StatusMerge::default();
        merge.program_status(&osc(ProgramState::Idle));

        merge.notify("Pick one");

        assert_eq!(merge.status(), AgentStatus::Blocked(BlockedKind::Other));
        assert_eq!(merge.take_prompt(), None);
    }

    #[test]
    fn the_trust_prompt_is_a_prompt() {
        let mut merge = StatusMerge::default();
        merge.trust_prompt_visible();

        assert_eq!(merge.take_prompt().as_deref(), Some(TRUST_MESSAGE));
        assert_eq!(merge.take_prompt(), None);
    }

    #[test]
    fn a_prompt_hook_starts_a_prompt_with_the_message() {
        for (notification, kind) in [
            ("permission_prompt", "permission"),
            ("elicitation_dialog", "question"),
            ("elicitation_url_dialog", "question"),
            ("agent_needs_input", "question"),
        ] {
            let mut merge = StatusMerge::default();

            hook(
                &mut merge,
                &json!({
                    "hook_event_name": "Notification",
                    "notification_type": notification,
                    "message": format!("{kind} please"),
                }),
            );

            assert_eq!(
                merge.take_prompt(),
                Some(format!("{kind} please")),
                "{notification}"
            );
        }
    }

    #[test]
    fn a_permission_request_followed_by_its_notification_is_one_prompt() {
        let mut merge = StatusMerge::default();
        hook(
            &mut merge,
            &json!({"hook_event_name": "PermissionRequest", "tool_name": "Write"}),
        );
        assert!(merge.take_prompt().is_some());

        hook(
            &mut merge,
            &json!({
                "hook_event_name": "Notification",
                "notification_type": "permission_prompt",
                "message": "Claude needs your permission to use Write",
            }),
        );

        assert_eq!(merge.take_prompt(), None);
    }

    #[test]
    fn only_idle_and_done_rest_and_a_notice_does_not_change_that() {
        let mut merge = StatusMerge::default();
        assert!(!merge.is_resting());

        merge.program_status(&osc(ProgramState::Working));
        assert!(!merge.is_resting());

        merge.program_status(&osc(ProgramState::Done));
        assert!(merge.is_resting());

        merge.notify("Review needed");
        assert!(merge.is_resting());
        assert!(merge.has_notice());

        merge.program_status(&osc(ProgramState::Idle));
        assert!(merge.is_resting());

        merge.program_status(&blocked(BlockedKind::Permission, None));
        assert!(!merge.is_resting());
    }

    fn osc_error(message: Option<&str>) -> ProgramStatus {
        ProgramStatus {
            state: ProgramState::Error,
            message: message.map(str::to_owned),
        }
    }

    fn stop_failure(merge: &mut StatusMerge, error: &str) {
        hook(
            merge,
            &json!({"hook_event_name": "StopFailure", "error": error}),
        );
    }

    #[test]
    fn a_stop_failure_hook_is_a_failure_with_its_reason() {
        let mut merge = StatusMerge::default();

        stop_failure(&mut merge, "rate_limit");

        assert_eq!(merge.take_failure().as_deref(), Some("rate_limit"));
        assert_eq!(merge.take_failure(), None);
    }

    #[test]
    fn a_stop_failure_hook_without_a_reason_gets_a_short_one() {
        let mut merge = StatusMerge::default();
        hook(&mut merge, &json!({"hook_event_name": "StopFailure"}));
        assert_eq!(merge.take_failure().as_deref(), Some("Failed"));

        let mut merge = StatusMerge::default();
        stop_failure(&mut merge, "");
        assert_eq!(merge.take_failure().as_deref(), Some("Failed"));
    }

    #[test]
    fn other_hooks_report_no_failure() {
        let mut merge = StatusMerge::default();

        for name in ["SessionStart", "Stop", "PermissionRequest", "Notification"] {
            hook(&mut merge, &json!({"hook_event_name": name}));

            assert_eq!(merge.take_failure(), None, "{name}");
        }
    }

    #[test]
    fn an_osc_error_is_a_failure_with_the_status_text() {
        let mut merge = StatusMerge::default();

        merge.program_status(&osc_error(Some("usage limit reached")));

        assert_eq!(merge.take_failure().as_deref(), Some("usage limit reached"));
        assert_eq!(merge.take_failure(), None);
    }

    #[test]
    fn an_osc_error_without_text_gets_a_short_reason() {
        for message in [None, Some("")] {
            let mut merge = StatusMerge::default();

            merge.program_status(&osc_error(message));

            assert_eq!(
                merge.take_failure().as_deref(),
                Some("Failed"),
                "{message:?}"
            );
        }
    }

    #[test]
    fn no_other_osc_state_is_a_failure() {
        for state in [
            ProgramState::Idle,
            ProgramState::Working,
            ProgramState::Done,
            ProgramState::Blocked(BlockedKind::Permission),
            ProgramState::Clear,
        ] {
            let mut merge = StatusMerge::default();

            merge.program_status(&osc(state));

            assert_eq!(merge.take_failure(), None, "{state:?}");
        }
    }

    #[test]
    fn a_repeat_or_a_new_text_inside_one_error_spell_is_not_a_new_failure() {
        let mut merge = StatusMerge::default();
        merge.program_status(&osc_error(Some("usage limit reached")));
        merge.take_failure();

        merge.program_status(&osc_error(Some("usage limit reached")));
        assert_eq!(merge.take_failure(), None);

        merge.program_status(&osc_error(Some("another text")));
        assert_eq!(merge.take_failure(), None);
    }

    #[test]
    fn an_osc_error_and_a_stop_failure_in_either_order_are_one_failure() {
        let mut merge = StatusMerge::default();
        merge.program_status(&osc(ProgramState::Working));
        merge.program_status(&osc_error(Some("overloaded")));
        assert_eq!(merge.take_failure().as_deref(), Some("overloaded"));
        stop_failure(&mut merge, "overloaded_error");
        assert_eq!(merge.take_failure(), None);

        let mut merge = StatusMerge::default();
        merge.program_status(&osc(ProgramState::Working));
        stop_failure(&mut merge, "overloaded_error");
        assert_eq!(merge.take_failure().as_deref(), Some("overloaded_error"));
        merge.program_status(&osc_error(Some("overloaded")));
        assert_eq!(merge.take_failure(), None);
    }

    #[test]
    fn a_failure_is_told_once_it_was_taken_and_while_the_agent_stays_in_error() {
        let mut merge = StatusMerge::default();
        assert!(!merge.failure_told());

        merge.program_status(&osc_error(Some("overloaded")));
        assert!(!merge.failure_told(), "not taken yet");

        merge.take_failure();
        assert!(merge.failure_told());

        merge.notify("look here");
        assert!(merge.failure_told(), "a notice does not hide the error");

        merge.program_status(&osc(ProgramState::Working));
        assert!(!merge.failure_told());
    }

    #[test]
    fn two_reports_of_one_spell_are_one_failure_even_when_none_was_taken_between() {
        let mut merge = StatusMerge::default();
        merge.program_status(&osc(ProgramState::Working));

        merge.program_status(&osc_error(Some("overloaded")));
        stop_failure(&mut merge, "overloaded_error");

        assert!(merge.take_failure().is_some());
        assert_eq!(merge.take_failure(), None);
    }

    #[test]
    fn an_error_after_the_agent_left_the_error_state_is_a_new_failure() {
        for between in [
            ProgramState::Working,
            ProgramState::Idle,
            ProgramState::Done,
            ProgramState::Clear,
        ] {
            let mut merge = StatusMerge::default();
            merge.program_status(&osc_error(Some("first")));
            assert_eq!(merge.take_failure().as_deref(), Some("first"));

            merge.program_status(&osc(between));
            assert_eq!(merge.take_failure(), None, "{between:?}");
            merge.program_status(&osc_error(Some("second")));

            assert_eq!(
                merge.take_failure().as_deref(),
                Some("second"),
                "{between:?}"
            );
        }
    }

    #[test]
    fn a_stop_failure_after_the_agent_worked_again_is_a_new_failure() {
        let mut merge = StatusMerge::default();
        merge.program_status(&osc_error(Some("first")));
        merge.take_failure();

        merge.program_status(&osc(ProgramState::Working));
        stop_failure(&mut merge, "second");

        assert_eq!(merge.take_failure().as_deref(), Some("second"));
    }

    #[test]
    fn a_prompt_and_a_failure_that_follow_each_other_are_each_told_once() {
        let mut merge = StatusMerge::default();

        merge.program_status(&blocked(BlockedKind::Permission, None));
        assert_eq!(merge.take_failure(), None);
        assert!(merge.take_prompt().is_some());

        merge.program_status(&osc_error(Some("broke")));
        assert_eq!(merge.take_prompt(), None);
        assert_eq!(merge.take_failure().as_deref(), Some("broke"));

        merge.program_status(&blocked(BlockedKind::Question, None));
        assert_eq!(merge.take_failure(), None);
        assert!(merge.take_prompt().is_some());
    }

    #[test]
    fn a_failure_right_after_a_prompt_that_nobody_took_is_still_told() {
        let mut merge = StatusMerge::default();
        merge.program_status(&blocked(BlockedKind::Permission, None));

        merge.program_status(&osc_error(Some("broke")));

        assert_eq!(merge.take_failure().as_deref(), Some("broke"));
    }

    #[test]
    fn an_error_that_ended_before_it_was_taken_is_gone() {
        let mut merge = StatusMerge::default();
        merge.program_status(&osc_error(Some("first")));
        merge.program_status(&osc(ProgramState::Working));

        assert_eq!(merge.take_failure(), None);
    }

    #[test]
    fn an_error_code_or_a_signal_is_a_failure_and_a_clean_exit_is_not() {
        assert_eq!(
            exit_failure(Some(&ExitStatus::Code(3))).as_deref(),
            Some("Exited with code 3")
        );
        assert_eq!(
            exit_failure(Some(&ExitStatus::Signal("Killed".to_owned()))).as_deref(),
            Some("Ended by signal Killed")
        );
        assert_eq!(exit_failure(Some(&ExitStatus::Code(0))), None);
        assert_eq!(exit_failure(None), None);
    }
}
