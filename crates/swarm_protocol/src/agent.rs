//! The agent socket protocol (ADR 0008), shared by the daemon, the `swarm` CLI and its
//! MCP server.
//!
//! One JSON object per line each way. Every request names the calling agent and
//! carries its token; the daemon checks what the caller's role may do. Nothing here
//! can reach a user terminal (ADR 0006).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AgentId, AgentKind, AgentLifecycle, AgentRole, GridSize, MessageId, MessageKind, TaskId,
    TerminalId,
};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct AgentRequest {
    pub agent: AgentId,
    pub token: String,
    #[serde(flatten)]
    pub call: Call,
}

/// Who may call what is in each variant's doc. "Orchestrator" means a task's
/// orchestrator, and only for its own task.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Call {
    /// Any agent: a Claude Code or Codex hook event, forwarded as it came.
    Hook { event: Value },

    /// Orchestrator: starts a worker with `brief` as its first message. The worker
    /// works in the task worktree, or in a worktree of its own made from the task
    /// branch when `own_worktree` gives the reason. Returns [`AgentSummary`].
    Spawn {
        name: String,
        /// `None` uses the orchestrator's own kind.
        kind: Option<AgentKind>,
        /// `None` uses the worker default from the settings.
        model: Option<String>,
        brief: String,
        /// Why the worker needs its own worktree; `None` shares the task worktree.
        #[serde(default)]
        own_worktree: Option<String>,
    },
    /// Orchestrator: a message to one of its workers. Returns [`Sent`].
    Send { to: AgentId, text: String },
    /// Worker: a question to its orchestrator. Returns [`Sent`]; the answer arrives
    /// as a message.
    Ask { question: String },
    /// Worker: a progress or final report to its orchestrator. Returns [`Sent`].
    Report { text: String },
    /// Any agent: takes the caller's unread messages. Returns `Vec<`[`InboxMessage`]`>`.
    Read,
    /// Any agent: waits until the caller has an unread message, then takes them all.
    /// Returns `Vec<`[`InboxMessage`]`>`, empty after the timeout.
    Wait { timeout_secs: Option<u32> },
    /// Worker or task orchestrator: commits `paths` (and nothing else) in the caller's
    /// worktree, as the caller. Returns [`Committed`].
    Commit {
        message: String,
        paths: Vec<PathBuf>,
    },
    /// Orchestrator: merges a worker's branch into the task branch in the task
    /// worktree. Returns [`Merged`].
    Merge { worker: AgentId },
    /// Orchestrator: asks the user for attention with a short reason.
    Notify { text: String },
    /// Orchestrator: lists its workers. Returns `Vec<`[`AgentSummary`]`>`.
    Agents,
    /// Task orchestrator: confirms the brief it started with (see `task_wait`).
    Confirm,

    /// Start orchestrator: makes a task (branch, task worktree, tab, setup) and starts
    /// its orchestrator with `brief` as the first message. Returns [`TaskSummary`].
    TaskCreate {
        name: String,
        /// Branch to start from; `None` uses the base branch.
        from: Option<String>,
        brief: String,
        /// `None` uses the default orchestrator agent from the settings.
        kind: Option<AgentKind>,
    },
    /// Start orchestrator: the project's tasks. Returns `Vec<`[`TaskSummary`]`>`.
    TaskList,
    /// Start orchestrator: waits until every task in `ids` reached `until`. Returns
    /// `Vec<`[`TaskSummary`]`>`, as they are when the wait ends.
    TaskWait {
        ids: Vec<TaskId>,
        until: TaskUntil,
        timeout_secs: Option<u32>,
    },
    /// Any agent: ends the caller's own session after the reply.
    AgentExit,

    /// Any agent: starts a process terminal in the caller's worktree (or `cwd` inside
    /// it). Returns [`TerminalSummary`].
    TerminalSpawn {
        command: Vec<String>,
        cwd: Option<PathBuf>,
        size: Option<TerminalSize>,
        name: Option<String>,
    },
    /// Owner: asks for a new size. Returns [`TerminalSummary`] with the size granted.
    TerminalResize { id: TerminalId, size: TerminalSize },
    /// Owner or orchestrator: the last `lines` lines of output (default: the screen).
    /// Returns [`TerminalText`].
    TerminalRead { id: TerminalId, lines: Option<u32> },
    /// Owner or orchestrator: waits for the process to exit or for a line that
    /// contains `pattern`. Returns [`TerminalWaited`].
    TerminalWait {
        id: TerminalId,
        until: TerminalUntil,
        timeout_secs: Option<u32>,
    },
    /// Owner or orchestrator: stops the process (`signal` is "TERM", "INT", "KILL" or
    /// "HUP"; default "TERM"). Returns [`TerminalSummary`].
    TerminalKill {
        id: TerminalId,
        signal: Option<String>,
    },
    /// Any agent: the process terminals of the caller's task (never user or agent
    /// terminals). Returns `Vec<`[`TerminalSummary`]`>`.
    TerminalList,
}

/// Every harness call as its MCP tool name and its CLI words.
pub const HARNESS_CALLS: &[(&str, &str)] = &[
    ("spawn", "spawn"),
    ("send", "send"),
    ("ask", "ask"),
    ("report", "report"),
    ("read", "read"),
    ("wait", "wait"),
    ("commit", "commit"),
    ("merge", "merge"),
    ("notify", "notify"),
    ("agents", "agents"),
    ("confirm", "confirm"),
    ("task_create", "task create"),
    ("task_list", "task list"),
    ("task_wait", "task wait"),
    ("agent_exit", "exit"),
    ("terminal_spawn", "terminal spawn"),
    ("terminal_resize", "terminal resize"),
    ("terminal_read", "terminal read"),
    ("terminal_wait", "terminal wait"),
    ("terminal_kill", "terminal kill"),
    ("terminal_list", "terminal list"),
];

/// The tool that starts a program outside any sandbox. Adapters never pre-approve it.
pub const PROMPTED_TOOL: &str = "terminal_spawn";

/// One line back for each request.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub result: Value,
}

impl Reply {
    pub fn success(result: impl Serialize) -> Self {
        match serde_json::to_value(result) {
            Ok(result) => Self {
                ok: true,
                error: None,
                result,
            },
            Err(error) => Self::failure(format!("cannot encode the reply: {error}")),
        }
    }

    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(error.into()),
            result: Value::Null,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskUntil {
    /// The task orchestrator called `confirm`.
    Briefed,
    /// The task orchestrator stopped: its session ended or can only be resumed.
    Done,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalUntil {
    Exit,
    /// A line of output contains this text.
    Output(String),
}

/// `small` is 80 × 12, `medium` 100 × 24 and `large` 140 × 40.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalSize {
    Small,
    Medium,
    Large,
    Grid(GridSize),
}

impl TerminalSize {
    pub fn grid(self) -> GridSize {
        match self {
            Self::Small => GridSize { cols: 80, rows: 12 },
            Self::Medium => GridSize {
                cols: 100,
                rows: 24,
            },
            Self::Large => GridSize {
                cols: 140,
                rows: 40,
            },
            Self::Grid(grid) => grid,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AgentSummary {
    pub id: AgentId,
    pub name: String,
    pub role: AgentRole,
    pub kind: AgentKind,
    pub model: String,
    /// The agent status in glossary words: "unknown", "idle", "working", "done",
    /// "blocked" or "error".
    pub status: String,
    pub message: Option<String>,
    pub lifecycle: AgentLifecycle,
    pub branch: String,
    pub worktree: PathBuf,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sent {
    pub message: MessageId,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct InboxMessage {
    pub id: MessageId,
    /// `None` when Swarm itself sends it.
    pub from: Option<AgentId>,
    /// The sender's agent name, or "swarm".
    pub from_name: String,
    pub kind: MessageKind,
    pub text: String,
    /// Milliseconds since the Unix epoch.
    pub sent_at: u64,
}

impl InboxMessage {
    /// The message as it is pasted into a terminal and printed by `swarm read`:
    /// `[swarm] <kind> from <name> (agent <id>):`, then the text.
    pub fn render(&self) -> String {
        let sender = self.from.map_or_else(
            || self.from_name.clone(),
            |agent| format!("{} (agent {})", self.from_name, agent.0),
        );

        format!(
            "[swarm] {} from {sender}:\n{}",
            self.kind.label(),
            self.text
        )
    }
}

/// Messages one after the other, separated by a blank line.
pub fn render_inbox(messages: &[InboxMessage]) -> String {
    messages
        .iter()
        .map(InboxMessage::render)
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_field_names,
    reason = "`merged` is the field name of the reply"
)]
pub struct Merged {
    /// False when git stopped on conflicts; the task worktree is left mid-merge.
    pub merged: bool,
    pub conflicts: Vec<PathBuf>,
    /// What git printed.
    pub output: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    /// The new commit.
    pub commit: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TaskSummary {
    pub id: TaskId,
    pub name: String,
    pub branch: String,
    pub briefed: bool,
    pub orchestrator: Option<AgentSummary>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TerminalSummary {
    pub id: TerminalId,
    pub name: Option<String>,
    pub command: Vec<String>,
    pub owner: Option<AgentId>,
    pub size: GridSize,
    pub running: bool,
    /// "code 0", "code 1" or "signal 15" once it ended.
    pub exit: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TerminalText {
    pub text: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TerminalWaited {
    /// False after the timeout.
    pub reached: bool,
    pub terminal: TerminalSummary,
    /// The matching line, for `output`.
    pub line: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_request_is_flat_json_with_method_and_params() {
        let request = AgentRequest {
            agent: AgentId(3),
            token: "abc".to_owned(),
            call: Call::Send {
                to: AgentId(4),
                text: "hi".to_owned(),
            },
        };

        let value = serde_json::to_value(&request).expect("encode");

        assert_eq!(
            value,
            json!({
                "agent": 3,
                "token": "abc",
                "method": "send",
                "params": { "to": 4, "text": "hi" },
            })
        );
        assert_eq!(
            serde_json::from_value::<AgentRequest>(value).expect("decode"),
            request
        );
    }

    #[test]
    fn a_spawn_without_own_worktree_still_parses() {
        let request: AgentRequest = serde_json::from_value(json!({
            "agent": 1, "token": "t", "method": "spawn",
            "params": { "name": "api", "kind": null, "model": null, "brief": "go" },
        }))
        .expect("decode");

        assert_eq!(
            request.call,
            Call::Spawn {
                name: "api".to_owned(),
                kind: None,
                model: None,
                brief: "go".to_owned(),
                own_worktree: None,
            }
        );
    }

    #[test]
    fn a_commit_carries_a_message_and_paths() {
        let call = Call::Commit {
            message: "feat: api".to_owned(),
            paths: vec![PathBuf::from("src/api.rs")],
        };

        let value = serde_json::to_value(&call).expect("encode");

        assert_eq!(
            value,
            json!({
                "method": "commit",
                "params": { "message": "feat: api", "paths": ["src/api.rs"] },
            })
        );
    }

    #[test]
    fn a_call_without_params_needs_none() {
        let request: AgentRequest =
            serde_json::from_value(json!({ "agent": 1, "token": "t", "method": "read" }))
                .expect("decode");

        assert_eq!(request.call, Call::Read);
    }

    #[test]
    fn messages_render_with_a_header_and_a_blank_line_between() {
        let messages = [
            InboxMessage {
                id: MessageId(1),
                from: Some(AgentId(4)),
                from_name: "api".to_owned(),
                kind: MessageKind::Question,
                text: "Which port?".to_owned(),
                sent_at: 0,
            },
            InboxMessage {
                id: MessageId(2),
                from: None,
                from_name: "swarm".to_owned(),
                kind: MessageKind::UserInput,
                text: "typed".to_owned(),
                sent_at: 0,
            },
        ];

        assert_eq!(
            render_inbox(&messages),
            "[swarm] question from api (agent 4):\nWhich port?\n\n[swarm] user input from swarm:\ntyped"
        );
    }

    #[test]
    fn a_failure_has_no_result() {
        let value = serde_json::to_value(Reply::failure("refused")).expect("encode");

        assert_eq!(value, json!({ "ok": false, "error": "refused" }));
    }
}
