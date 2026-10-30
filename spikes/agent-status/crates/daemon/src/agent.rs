//! Agent status per terminal: merges OSC 7501, hook reports and process exit,
//! and streams every change to watchers.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use proto::{AgentKind, AgentView, BlockedKind, Event, Report, Source, State, TermId};
use serde_json::Value;

use crate::osc7501;
use crate::terminal::Frame;

#[derive(Default)]
pub struct Agents {
    views: HashMap<TermId, Tracked>,
    watchers: Vec<flume::Sender<Frame>>,
}

#[derive(Default)]
struct Tracked {
    view: AgentView,
    /// Once a program sends OSC 7501, hooks no longer set the state (except
    /// `StopFailure`, which carries a reason OSC 7501 does not).
    osc_seen: bool,
}

/// Session facts a hook reported.
pub struct Session {
    pub id: String,
    pub transcript: Option<String>,
}

pub fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
}

impl Agents {
    pub fn add(&mut self, id: TermId, kind: Option<AgentKind>) {
        let view = AgentView {
            kind,
            alive: true,
            ..AgentView::default()
        };
        self.views.insert(id, Tracked { view, osc_seen: false });
        self.publish(id, None);
    }

    pub fn view(&self, id: TermId) -> AgentView {
        self.views
            .get(&id)
            .map(|t| t.view.clone())
            .unwrap_or_default()
    }

    pub fn watch(&mut self, tx: flume::Sender<Frame>) -> Result<()> {
        let mut ids: Vec<_> = self.views.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let event = Event::Agent {
                id,
                view: self.views[&id].view.clone(),
                report: None,
            };
            tx.send(Arc::new(proto::encode(&event)?))?;
        }
        self.watchers.push(tx);
        Ok(())
    }

    pub fn osc(&mut self, id: TermId, status: osc7501::Status, at_us: u64) {
        let Some(t) = self.views.get_mut(&id) else {
            return;
        };
        // Child records (with an id) are logged but do not drive the agent status.
        let root = status.id.is_none();
        let report = Report {
            source: Source::Osc7501,
            state: root.then_some(status.state.unwrap_or(State::Unknown)),
            msg: status.msg,
            detail: status.raw,
            at_us,
            sent_us: None,
        };
        if root {
            t.osc_seen = true;
            t.view.state = status.state.unwrap_or(State::Unknown);
            t.view.msg.clone_from(&report.msg);
        }
        self.publish(id, Some(report));
    }

    /// Applies one hook payload (Claude Code or Codex hook JSON on stdin).
    /// Returns the session it reported, so the caller can persist it.
    pub fn hook(&mut self, id: TermId, sent_us: u64, payload: &str) -> Option<Session> {
        let at_us = now_us();
        let json: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
        let field = |k: &str| json.get(k).and_then(Value::as_str);
        let event = field("hook_event_name").unwrap_or("?").to_owned();
        let (state, msg) = hook_state(&event, &json);
        let session = field("session_id").map(|sid| Session {
            id: sid.to_owned(),
            transcript: field("transcript_path").map(str::to_owned),
        });
        let detail = match field("notification_type").or(field("source")).or(field("error")) {
            Some(extra) => format!("{event}:{extra}"),
            None => event.clone(),
        };
        let t = self.views.get_mut(&id)?;
        if let Some(s) = &session {
            t.view.session_id = Some(s.id.clone());
            if s.transcript.is_some() {
                t.view.transcript.clone_from(&s.transcript);
            }
        }
        let applies = !t.osc_seen || event == "StopFailure";
        if let Some(state) = state
            && applies
        {
            t.view.state = state;
            t.view.msg.clone_from(&msg);
        }
        self.publish(
            id,
            Some(Report {
                source: Source::Hook,
                state,
                msg,
                detail,
                at_us,
                sent_us: Some(sent_us),
            }),
        );
        session
    }

    /// Process exit, following the spec's record lifetime: `working` and
    /// `blocked` are dropped, `done` and `error` survive.
    pub fn exited(&mut self, id: TermId, code: Option<u32>) {
        let Some(t) = self.views.get_mut(&id) else {
            return;
        };
        t.view.alive = false;
        let state = match (t.view.state, code) {
            (State::Done | State::Error, _) => t.view.state,
            (_, Some(c)) if c != 0 => State::Error,
            (State::Working | State::Blocked(_), _) => State::Unknown,
            (s, _) => s,
        };
        let msg = match code {
            Some(c) => format!("exited with code {c}"),
            None => "exited".to_owned(),
        };
        t.view.state = state;
        t.view.msg.clone_from(&msg);
        self.publish(
            id,
            Some(Report {
                source: Source::Process,
                state: Some(state),
                msg,
                detail: "exit".into(),
                at_us: now_us(),
                sent_us: None,
            }),
        );
    }

    fn publish(&mut self, id: TermId, report: Option<Report>) {
        let Some(t) = self.views.get(&id) else {
            return;
        };
        if let Some(r) = &report {
            eprintln!(
                "[agent {id}] {:?} {} -> {} ({})",
                r.source,
                r.state.map_or("-".into(), |s| s.to_string()),
                t.view.state,
                r.detail
            );
        }
        let event = Event::Agent {
            id,
            view: t.view.clone(),
            report,
        };
        let Ok(frame) = proto::encode(&event) else {
            return;
        };
        let frame = Arc::new(frame);
        self.watchers.retain(|w| w.send(frame.clone()).is_ok());
    }
}

/// Maps a hook event to a state. Event names are Claude Code's; Codex uses
/// the same names for the events it has.
fn hook_state(event: &str, json: &Value) -> (Option<State>, String) {
    let field = |k: &str| json.get(k).and_then(Value::as_str).unwrap_or_default();
    match event {
        "SessionStart" => (Some(State::Idle), String::new()),
        "UserPromptSubmit" | "PostToolUse" | "PreToolUse" => (Some(State::Working), String::new()),
        // Claude Code asks AskUserQuestion through the permission flow too.
        "PermissionRequest" if field("tool_name") == "AskUserQuestion" => (
            Some(State::Blocked(BlockedKind::Question)),
            field("tool_name").to_owned(),
        ),
        "PermissionRequest" => (
            Some(State::Blocked(BlockedKind::Permission)),
            field("tool_name").to_owned(),
        ),
        "Notification" => {
            let state = match field("notification_type") {
                "permission_prompt" => Some(State::Blocked(BlockedKind::Permission)),
                "elicitation_dialog" | "elicitation_url_dialog" => {
                    Some(State::Blocked(BlockedKind::Question))
                }
                "idle_prompt" => Some(State::Idle),
                _ => None,
            };
            (state, field("message").to_owned())
        }
        "Stop" => (Some(State::Done), String::new()),
        // Codex only; Claude Code runs no hook when the user interrupts.
        "Interrupt" => (Some(State::Idle), String::new()),
        "StopFailure" => (Some(State::Error), field("error").to_owned()),
        _ => (None, String::new()),
    }
}

/// Claude Code settings that add our hooks. Passed with `--settings`, so the
/// user's own settings files stay untouched; hooks from all sources merge.
pub fn write_claude_settings(path: &Path, ctl: &Path) -> Result<()> {
    const EVENTS: &[&str] = &[
        "SessionStart",
        "UserPromptSubmit",
        "PermissionRequest",
        "PostToolUse",
        "Notification",
        "Stop",
        "StopFailure",
        "SessionEnd",
    ];
    let handler = serde_json::json!([{
        "hooks": [{
            "type": "command",
            "command": ctl,
            "args": ["hook"],
            "timeout": 5,
        }]
    }]);
    let hooks: serde_json::Map<String, Value> = EVENTS
        .iter()
        .map(|e| ((*e).to_owned(), handler.clone()))
        .collect();
    let settings = serde_json::json!({ "hooks": hooks });
    std::fs::write(path, serde_json::to_vec_pretty(&settings)?)?;
    Ok(())
}

/// Codex hook events we install, with the hook timeout. Codex clamps
/// `SessionEnd` and `Interrupt` to 3 s, and the trust hash uses the clamped value.
const CODEX_EVENTS: &[(&str, &str, u64)] = &[
    ("SessionStart", "session_start", 5),
    ("UserPromptSubmit", "user_prompt_submit", 5),
    ("PermissionRequest", "permission_request", 5),
    ("PostToolUse", "post_tool_use", 5),
    ("Stop", "stop", 5),
    ("Interrupt", "interrupt", 3),
    ("SessionEnd", "session_end", 3),
];

/// `-c` flags that add our hooks to Codex for this run only, pre-trusted.
///
/// Hooks from `-c` are untrusted and skipped unless their hash is in
/// `hooks.state`. Codex reads `hooks.state` from the `-c` layer too, so we
/// send the hash we expect instead of `--dangerously-bypass-hook-trust`
/// (which would also run every untrusted hook of the user).
pub fn codex_hook_flags(ctl: &Path) -> Vec<String> {
    use sha2::{Digest, Sha256};
    // Codex runs the command through a shell.
    let command = format!("'{}' hook", ctl.display().to_string().replace('\'', r"'\''"));
    let mut flags = Vec::new();
    let mut state = Vec::new();
    for (event, label, timeout) in CODEX_EVENTS {
        let handler = serde_json::json!({
            "type": "command",
            "command": command,
            "timeout": timeout,
        });
        flags.push("-c".to_owned());
        flags.push(format!("hooks.{event}=[{{hooks=[{}]}}]", toml_inline(&handler)));
        // Same identity Codex hashes: sorted-key JSON of the normalized group.
        let identity = serde_json::json!({
            "event_name": label,
            "hooks": [{
                "async": false,
                "command": command,
                "timeout": timeout,
                "type": "command",
            }],
        });
        let digest = Sha256::digest(identity.to_string().as_bytes());
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        state.push(format!(
            "\"/<session-flags>/config.toml:{label}:0:0\"={{trusted_hash=\"sha256:{hex}\"}}"
        ));
    }
    flags.push("-c".to_owned());
    flags.push(format!("hooks.state={{{}}}", state.join(",")));
    flags
}

/// A flat JSON object as a TOML inline table. JSON strings are valid TOML
/// basic strings for the characters we use.
fn toml_inline(value: &Value) -> String {
    let Some(map) = value.as_object() else {
        return String::new();
    };
    let pairs: Vec<String> = map.iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!("{{{}}}", pairs.join(","))
}
