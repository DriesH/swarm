//! `swarm mcp`: a stdio MCP server that exposes the same commands as tools.
//!
//! Written by hand on `serde_json`, one JSON-RPC message per line. It speaks both
//! eras of the protocol, see <https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning>:
//! the legacy `initialize` handshake that Claude Code and Codex use today, and the
//! stateless requests of 2026-07-28 with `server/discover`. The daemon checks every
//! call again; the tools listed only follow `SWARM_ROLE`.

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use swarm_protocol::agent::TerminalUntil;
use swarm_protocol::{TaskId, TerminalId};

use crate::commands::{Command, parse_kind, parse_size, parse_until};
use crate::session::{Cancel, Session};

/// The stateless protocol version this server speaks.
const MODERN_VERSION: &str = "2026-07-28";

/// The versions served after an `initialize` handshake, newest first. 2025-03-26 is
/// left out: it requires JSON-RPC batches, which this server does not read.
const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2024-11-05"];

/// After stdin closed, calls in flight get this long to answer before they are cancelled.
const CLOSE_GRACE: Duration = Duration::from_millis(500);

const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";
const SERVER_INFO_KEY: &str = "io.modelcontextprotocol/serverInfo";

const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const PARSE_ERROR: i64 = -32700;
const UNSUPPORTED_VERSION: i64 = -32022;

/// Who the agent is, from `SWARM_ROLE`; decides which tools are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Orchestrator,
    Worker,
    StartOrchestrator,
    /// No role or one we do not know: only the tools every agent has.
    Unknown,
}

impl Role {
    pub(crate) fn parse(name: Option<&str>) -> Self {
        match name {
            Some("orchestrator") => Self::Orchestrator,
            Some("worker") => Self::Worker,
            Some("start_orchestrator") => Self::StartOrchestrator,
            _ => Self::Unknown,
        }
    }
}

/// Who may use a tool.
#[derive(Debug, Clone, Copy)]
enum Audience {
    Everyone,
    Only(&'static [Role]),
}

struct Tool {
    name: &'static str,
    description: &'static str,
    audience: Audience,
    schema: fn() -> Value,
    build: fn(&Map<String, Value>) -> Result<Command, String>,
}

/// Add a tool here; the list, the schema and the call all follow from this table.
const TOOLS: &[Tool] = &[
    Tool {
        name: "spawn",
        description: "Start a worker. It works in the task worktree unless you give own_worktree. The brief is its first message: say what to do, which files it owns and what done means.",
        audience: Audience::Only(&[Role::Orchestrator]),
        schema: || {
            schema(
                &[
                    (
                        "name",
                        "string",
                        "The worker's name; it becomes part of its branch.",
                    ),
                    ("brief", "string", "What to do and what done means."),
                    (
                        "kind",
                        "string",
                        "claude or codex. Defaults to your own kind.",
                    ),
                    (
                        "model",
                        "string",
                        "The model. Defaults to the worker model from the settings.",
                    ),
                    (
                        "own_worktree",
                        "string",
                        "The reason the worker needs its own worktree and branch. Leave it out to share the task worktree.",
                    ),
                ],
                &["name", "brief"],
            )
        },
        build: |arguments| {
            Ok(Command::Spawn {
                name: required(arguments, "name")?,
                kind: optional(arguments, "kind")?
                    .map(|kind| parse_kind(&kind))
                    .transpose()?,
                model: optional(arguments, "model")?,
                brief: required(arguments, "brief")?,
                own_worktree: optional(arguments, "own_worktree")?,
            })
        },
    },
    Tool {
        name: "send",
        description: "Send a message to one of your workers. It arrives when the worker is idle.",
        audience: Audience::Only(&[Role::Orchestrator]),
        schema: || {
            schema(
                &[
                    ("to", "string", "The worker's agent ID or name."),
                    ("text", "string", "The message."),
                ],
                &["to", "text"],
            )
        },
        build: |arguments| {
            Ok(Command::Send {
                to: identifier(arguments, "to")?,
                text: required(arguments, "text")?,
            })
        },
    },
    Tool {
        name: "ask",
        description: "Ask your orchestrator a question. The answer arrives as a message: call wait to receive it.",
        audience: Audience::Only(&[Role::Worker]),
        schema: || schema(&[("question", "string", "The question.")], &["question"]),
        build: |arguments| {
            Ok(Command::Ask {
                text: required(arguments, "question")?,
            })
        },
    },
    Tool {
        name: "report",
        description: "Send your orchestrator a progress report or your final result.",
        audience: Audience::Only(&[Role::Worker]),
        schema: || schema(&[("text", "string", "The report.")], &["text"]),
        build: |arguments| {
            Ok(Command::Report {
                text: required(arguments, "text")?,
            })
        },
    },
    Tool {
        name: "commit",
        description: "Commit the named files, and nothing else, in your worktree. The commit is yours: your name is the author. Use it instead of git commit.",
        audience: Audience::Only(&[Role::Orchestrator, Role::Worker]),
        schema: || {
            schema(
                &[
                    ("message", "string", "The commit message."),
                    (
                        "paths",
                        "strings",
                        "The files to commit, relative to your worktree.",
                    ),
                ],
                &["message", "paths"],
            )
        },
        build: |arguments| {
            Ok(Command::Commit {
                message: required(arguments, "message")?,
                paths: string_list(arguments, "paths")?
                    .into_iter()
                    .map(std::path::PathBuf::from)
                    .collect(),
            })
        },
    },
    Tool {
        name: "read",
        description: "Take your unread messages.",
        audience: Audience::Everyone,
        schema: || schema(&[], &[]),
        build: |_| Ok(Command::Read),
    },
    Tool {
        name: "wait",
        description: "Wait until you have an unread message, then take them. Returns nothing after the timeout.",
        audience: Audience::Everyone,
        schema: || {
            schema(
                &[(
                    "timeout_secs",
                    "integer",
                    "Seconds to wait. Defaults to 100, at most 600.",
                )],
                &[],
            )
        },
        build: |arguments| {
            Ok(Command::Wait {
                timeout_secs: optional_number(arguments, "timeout_secs")?,
            })
        },
    },
    Tool {
        name: "merge",
        description: "Merge a worker's branch into the task branch. On conflicts it lists them and leaves the merge for you to finish.",
        audience: Audience::Only(&[Role::Orchestrator]),
        schema: || {
            schema(
                &[("worker", "string", "The worker's agent ID or name.")],
                &["worker"],
            )
        },
        build: |arguments| {
            Ok(Command::Merge {
                worker: identifier(arguments, "worker")?,
            })
        },
    },
    Tool {
        name: "notify",
        description: "Ask the user for attention with a short reason.",
        audience: Audience::Only(&[Role::Orchestrator]),
        schema: || schema(&[("text", "string", "Why the user must look.")], &["text"]),
        build: |arguments| {
            Ok(Command::Notify {
                text: required(arguments, "text")?,
            })
        },
    },
    Tool {
        name: "agents",
        description: "List your workers with their status, branch and worktree.",
        audience: Audience::Only(&[Role::Orchestrator]),
        schema: || schema(&[], &[]),
        build: |_| Ok(Command::Agents),
    },
    Tool {
        name: "confirm",
        description: "Say that you understood your brief.",
        audience: Audience::Only(&[Role::Orchestrator]),
        schema: || schema(&[], &[]),
        build: |_| Ok(Command::Confirm),
    },
    Tool {
        name: "pr_draft",
        description: "Send the pull request title and description that the `pr draft` message asked for. Refused until the user approved the task.",
        audience: Audience::Only(&[Role::Orchestrator]),
        schema: || {
            schema(
                &[
                    (
                        "title",
                        "string",
                        "One line, at most 256 characters, that says what the change does.",
                    ),
                    (
                        "body",
                        "string",
                        "The description, in the format the message gives.",
                    ),
                ],
                &["title", "body"],
            )
        },
        build: |arguments| {
            Ok(Command::PrDraft {
                title: required(arguments, "title")?,
                body: required(arguments, "body")?,
            })
        },
    },
    Tool {
        name: "tile_grow",
        description: "Make the tile of a worker large, so the user sees more of it. The layout decides, and it ignores the request for a tile the user minimized. No agent gets a message.",
        audience: Audience::Only(&[Role::Orchestrator, Role::Worker]),
        schema: || {
            schema(
                &[(
                    "agent",
                    "string",
                    "A worker's agent ID or name (orchestrator). Leave it out for your own tile (worker).",
                )],
                &[],
            )
        },
        build: |arguments| {
            Ok(Command::TileGrow {
                agent: optional_identifier(arguments, "agent")?,
            })
        },
    },
    Tool {
        name: "tile_shrink",
        description: "Make the tile of a worker small again. The layout decides, and it ignores the request for a tile the user minimized. No agent gets a message.",
        audience: Audience::Only(&[Role::Orchestrator, Role::Worker]),
        schema: || {
            schema(
                &[(
                    "agent",
                    "string",
                    "A worker's agent ID or name (orchestrator). Leave it out for your own tile (worker).",
                )],
                &[],
            )
        },
        build: |arguments| {
            Ok(Command::TileShrink {
                agent: optional_identifier(arguments, "agent")?,
            })
        },
    },
    Tool {
        name: "task_create",
        description: "Make a task: a branch, a task worktree and a tab, with an orchestrator that starts on your brief. The brief states the goal, the scope and what done means.",
        audience: Audience::Only(&[Role::StartOrchestrator]),
        schema: || {
            schema(
                &[
                    (
                        "name",
                        "string",
                        "The task's name; it becomes part of its branch.",
                    ),
                    (
                        "brief",
                        "string",
                        "The goal, the scope and what done means.",
                    ),
                    (
                        "from",
                        "string",
                        "The branch to start from. Defaults to the base branch.",
                    ),
                    (
                        "kind",
                        "string",
                        "claude or codex. Defaults to the orchestrator agent from the settings.",
                    ),
                ],
                &["name", "brief"],
            )
        },
        build: |arguments| {
            Ok(Command::TaskCreate {
                name: required(arguments, "name")?,
                from: optional(arguments, "from")?,
                brief: required(arguments, "brief")?,
                kind: optional(arguments, "kind")?
                    .map(|kind| parse_kind(&kind))
                    .transpose()?,
            })
        },
    },
    Tool {
        name: "task_list",
        description: "List the tasks of the project with their branch, brief state and orchestrator status.",
        audience: Audience::Only(&[Role::StartOrchestrator]),
        schema: || schema(&[], &[]),
        build: |_| Ok(Command::TaskList),
    },
    Tool {
        name: "task_wait",
        description: "Wait until every task is briefed (its orchestrator confirmed the brief) or done (its orchestrator finished or stopped). Returns the tasks as they are when the wait ends.",
        audience: Audience::Only(&[Role::StartOrchestrator]),
        schema: || {
            schema(
                &[
                    ("ids", "integers", "The task IDs."),
                    ("until", "string", "briefed or done."),
                    (
                        "timeout_secs",
                        "integer",
                        "Seconds to wait. Defaults to 100, at most 600.",
                    ),
                ],
                &["ids", "until"],
            )
        },
        build: |arguments| {
            Ok(Command::TaskWait {
                ids: number_list(arguments, "ids")?
                    .into_iter()
                    .map(TaskId)
                    .collect(),
                until: parse_until(&required(arguments, "until")?)?,
                timeout_secs: optional_number(arguments, "timeout_secs")?,
            })
        },
    },
    Tool {
        name: "agent_exit",
        description: "End your own session. Call it when your work is done.",
        audience: Audience::Only(&[Role::StartOrchestrator]),
        schema: || schema(&[], &[]),
        build: |_| Ok(Command::Exit),
    },
    Tool {
        name: "terminal_spawn",
        description: "Run a command in a process terminal inside your worktree, such as a dev server or a test run. No shell is added: pass sh, -c and the script when you need one. Returns the terminal ID.",
        audience: Audience::Everyone,
        schema: || {
            schema(
                &[
                    (
                        "command",
                        "strings",
                        "The program and its arguments, for example [\"pnpm\", \"dev\"].",
                    ),
                    ("name", "string", "A label, such as \"dev server\"."),
                    ("size", "string", "Ignored: the terminal follows its tile."),
                    (
                        "cwd",
                        "string",
                        "A folder inside your worktree. Defaults to the worktree.",
                    ),
                ],
                &["command"],
            )
        },
        build: |arguments| {
            Ok(Command::TerminalSpawn {
                command: string_list(arguments, "command")?,
                cwd: optional(arguments, "cwd")?.map(std::path::PathBuf::from),
                size: optional(arguments, "size")?
                    .map(|size| parse_size(&size))
                    .transpose()?,
                name: optional(arguments, "name")?,
            })
        },
    },
    Tool {
        name: "terminal_resize",
        description: "Return the size of a process terminal you started. The tile sets the size, so the request changes nothing.",
        audience: Audience::Everyone,
        schema: || {
            schema(
                &[
                    ("id", "integer", "The terminal ID."),
                    (
                        "size",
                        "string",
                        "Ignored: small, medium, large or COLSxROWS.",
                    ),
                ],
                &["id", "size"],
            )
        },
        build: |arguments| {
            Ok(Command::TerminalResize {
                id: terminal_id(arguments)?,
                size: parse_size(&required(arguments, "size")?)?,
            })
        },
    },
    Tool {
        name: "terminal_read",
        description: "Read the screen of a process terminal, or its last lines of output.",
        audience: Audience::Everyone,
        schema: || {
            schema(
                &[
                    ("id", "integer", "The terminal ID."),
                    (
                        "lines",
                        "integer",
                        "How many lines. Defaults to the screen, at most 2000.",
                    ),
                ],
                &["id"],
            )
        },
        build: |arguments| {
            Ok(Command::TerminalRead {
                id: terminal_id(arguments)?,
                lines: optional_number(arguments, "lines")?,
            })
        },
    },
    Tool {
        name: "terminal_wait",
        description: "Wait for a process terminal to exit, or, with output, for a line that contains that text (also one already on screen).",
        audience: Audience::Everyone,
        schema: || {
            schema(
                &[
                    ("id", "integer", "The terminal ID."),
                    (
                        "output",
                        "string",
                        "Text to wait for in a line. Without it, waits for the exit.",
                    ),
                    (
                        "timeout_secs",
                        "integer",
                        "Seconds to wait. Defaults to 100, at most 600.",
                    ),
                ],
                &["id"],
            )
        },
        build: |arguments| {
            Ok(Command::TerminalWait {
                id: terminal_id(arguments)?,
                until: optional(arguments, "output")?
                    .map_or(TerminalUntil::Exit, TerminalUntil::Output),
                timeout_secs: optional_number(arguments, "timeout_secs")?,
            })
        },
    },
    Tool {
        name: "terminal_kill",
        description: "Stop the process of a terminal you started, or one in your task if you are its orchestrator.",
        audience: Audience::Everyone,
        schema: || {
            schema(
                &[
                    ("id", "integer", "The terminal ID."),
                    ("signal", "string", "TERM (the default), INT, KILL or HUP."),
                ],
                &["id"],
            )
        },
        build: |arguments| {
            Ok(Command::TerminalKill {
                id: terminal_id(arguments)?,
                signal: optional(arguments, "signal")?,
            })
        },
    },
    Tool {
        name: "terminal_list",
        description: "List the process terminals of your task with their size and state.",
        audience: Audience::Everyone,
        schema: || schema(&[], &[]),
        build: |_| Ok(Command::TerminalList),
    },
];

impl Tool {
    fn allowed_for(&self, role: Role) -> bool {
        match self.audience {
            Audience::Everyone => true,
            Audience::Only(roles) => roles.contains(&role),
        }
    }

    fn definition(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": (self.schema)(),
        })
    }
}

fn schema(properties: &[(&str, &str, &str)], required: &[&str]) -> Value {
    let properties: Map<String, Value> = properties
        .iter()
        .map(|(name, kind, description)| {
            let definition = match *kind {
                "strings" => json!({
                    "type": "array",
                    "items": { "type": "string" },
                    "description": description,
                }),
                "integers" => json!({
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": description,
                }),
                kind => json!({ "type": kind, "description": description }),
            };

            ((*name).to_owned(), definition)
        })
        .collect();

    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn required(arguments: &Map<String, Value>, name: &str) -> Result<String, String> {
    optional(arguments, name)?.ok_or_else(|| format!("{name} is required"))
}

fn optional(arguments: &Map<String, Value>, name: &str) -> Result<Option<String>, String> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(format!("{name} must be a string")),
    }
}

/// An agent ID or name; a JSON number is accepted as the ID.
fn identifier(arguments: &Map<String, Value>, name: &str) -> Result<String, String> {
    match arguments.get(name) {
        Some(Value::Number(number)) => Ok(number.to_string()),
        _ => required(arguments, name),
    }
}

/// Like [`identifier`], and `None` when it is left out.
fn optional_identifier(
    arguments: &Map<String, Value>,
    name: &str,
) -> Result<Option<String>, String> {
    match arguments.get(name) {
        Some(Value::Number(number)) => Ok(Some(number.to_string())),
        _ => optional(arguments, name),
    }
}

fn terminal_id(arguments: &Map<String, Value>) -> Result<TerminalId, String> {
    identifier(arguments, "id")?
        .parse()
        .map(TerminalId)
        .map_err(|_| "id must be a terminal ID".to_owned())
}

fn string_list(arguments: &Map<String, Value>, name: &str) -> Result<Vec<String>, String> {
    let list = match arguments.get(name) {
        Some(Value::Array(list)) => list,
        None | Some(Value::Null) => return Err(format!("{name} is required")),
        Some(_) => return Err(format!("{name} must be a list of strings")),
    };

    list.iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{name} must be a list of strings"))
        })
        .collect()
}

fn number_list(arguments: &Map<String, Value>, name: &str) -> Result<Vec<u64>, String> {
    let list = match arguments.get(name) {
        Some(Value::Array(list)) => list,
        None | Some(Value::Null) => return Err(format!("{name} is required")),
        Some(_) => return Err(format!("{name} must be a list of IDs")),
    };

    list.iter()
        .map(|item| {
            item.as_u64()
                .ok_or_else(|| format!("{name} must be a list of IDs"))
        })
        .collect()
}

fn optional_number(arguments: &Map<String, Value>, name: &str) -> Result<Option<u32>, String> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|number| u32::try_from(number).ok())
            .map(Some)
            .ok_or_else(|| format!("{name} must be a whole number of seconds")),
    }
}

pub(crate) struct Server {
    role: Role,
    session: Result<Session, String>,
    /// Tool calls in flight by request ID, so a cancel can reach their connection.
    pending: Mutex<HashMap<String, Arc<Cancel>>>,
}

impl Server {
    pub(crate) fn new(role: Role, session: Result<Session, String>) -> Self {
        Self {
            role,
            session,
            pending: Mutex::default(),
        }
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Cancel>>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The cancel handle of the call with this request ID. The reader registers a call
    /// before its thread starts, so a close never misses it.
    fn register(&self, id: &Value) -> Arc<Cancel> {
        Arc::clone(
            self.pending()
                .entry(id.to_string())
                .or_insert_with(|| Arc::new(Cancel::default())),
        )
    }

    /// Ends the call with this request ID, if it still runs.
    fn cancel(&self, request: &Value) {
        if let Some(cancel) = self.pending().get(&request.to_string()) {
            cancel.cancel();
        }
    }

    /// After stdin closed: gives the calls in flight a moment, then cancels the rest.
    fn close(&self) {
        let deadline = Instant::now() + CLOSE_GRACE;
        while !self.pending().is_empty() && Instant::now() < deadline {
            std::thread::park_timeout(Duration::from_millis(10));
        }

        for cancel in self.pending().values() {
            cancel.cancel();
        }
    }

    fn from_env() -> Self {
        let role = Role::parse(crate::session::harness_var("SWARM_ROLE").as_deref());

        Self::new(role, Session::from_env())
    }

    /// The reply to one message, or `None` for a notification, a response from the
    /// client or a call that was cancelled.
    pub(crate) fn handle(&self, message: &Value) -> Option<Value> {
        let Some(object) = message.as_object() else {
            return Some(error_reply(
                &Value::Null,
                INVALID_REQUEST,
                "not a JSON-RPC message",
            ));
        };
        let method = object.get("method").and_then(Value::as_str);

        if method == Some("notifications/cancelled") {
            if let Some(request) = object
                .get("params")
                .and_then(|params| params.get("requestId"))
            {
                self.cancel(request);
            }

            return None;
        }

        let id = object.get("id");
        let Some(method) = method else {
            let is_response = object.contains_key("result") || object.contains_key("error");
            if id.is_some() && is_response {
                return None;
            }

            return Some(error_reply(
                id.unwrap_or(&Value::Null),
                INVALID_REQUEST,
                "the request has no method",
            ));
        };
        let id = id?;
        if !(id.is_string() || id.is_number()) {
            return Some(error_reply(
                &Value::Null,
                INVALID_REQUEST,
                "the ID must be a string or a number",
            ));
        }
        let params = object.get("params").cloned().unwrap_or(Value::Null);

        let cancel = (method == "tools/call").then(|| self.register(id));
        let outcome = self.dispatch(method, &params, cancel.as_ref());
        if let Some(cancel) = cancel {
            self.pending().remove(&id.to_string());
            if cancel.is_cancelled() {
                return None;
            }
        }

        Some(match outcome {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => error.reply(id),
        })
    }

    fn dispatch(
        &self,
        method: &str,
        params: &Value,
        cancel: Option<&Arc<Cancel>>,
    ) -> Result<Value, RpcError> {
        if method == "initialize" {
            return Ok(Self::initialize(params));
        }

        let modern = check_modern(params)?;
        if method == "ping" && !modern {
            return Ok(json!({}));
        }

        let mut result = match method {
            "server/discover" => Self::discover(),
            "tools/list" => self.list_tools(),
            "tools/call" => self.call_tool(params, cancel)?,
            other => {
                return Err(RpcError::new(
                    METHOD_NOT_FOUND,
                    format!("method not found: {other}"),
                ));
            }
        };

        if modern && let Some(object) = result.as_object_mut() {
            object.insert("resultType".to_owned(), json!("complete"));
            if matches!(method, "server/discover" | "tools/list") {
                object.insert("ttlMs".to_owned(), json!(0));
                object.insert("cacheScope".to_owned(), json!("private"));
            }
        }

        Ok(result)
    }

    fn initialize(params: &Value) -> Value {
        let requested = params.get("protocolVersion").and_then(Value::as_str);
        let version = requested
            .filter(|version| LEGACY_VERSIONS.contains(version))
            .unwrap_or(LEGACY_VERSIONS[0]);

        json!({
            "protocolVersion": version,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": server_info(),
        })
    }

    fn discover() -> Value {
        let mut versions = vec![MODERN_VERSION];
        versions.extend(LEGACY_VERSIONS);

        json!({
            "supportedVersions": versions,
            "capabilities": { "tools": {} },
            "_meta": { SERVER_INFO_KEY: server_info() },
        })
    }

    fn list_tools(&self) -> Value {
        let tools: Vec<Value> = TOOLS
            .iter()
            .filter(|tool| tool.allowed_for(self.role))
            .map(Tool::definition)
            .collect();

        json!({ "tools": tools })
    }

    fn call_tool(&self, params: &Value, cancel: Option<&Arc<Cancel>>) -> Result<Value, RpcError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, "the call has no tool name"))?;
        let tool = TOOLS
            .iter()
            .find(|tool| tool.name == name && tool.allowed_for(self.role))
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, format!("unknown tool: {name}")))?;

        let empty = Map::new();
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(arguments)) => arguments,
            Some(_) => {
                return Err(RpcError::new(INVALID_PARAMS, "arguments must be an object"));
            }
        };

        let outcome = (tool.build)(arguments).and_then(|command| {
            let session = self.session.as_ref().map_err(Clone::clone)?;

            match cancel {
                Some(cancel) => command.run(&session.cancellable(Arc::clone(cancel))),
                None => command.run(session),
            }
        });

        Ok(match outcome {
            Ok(output) => tool_result(&output.text, false),
            Err(message) => tool_result(&message, true),
        })
    }
}

fn tool_result(text: &str, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

fn server_info() -> Value {
    json!({
        "name": "swarm",
        "title": "Swarm",
        "version": env!("CARGO_PKG_VERSION"),
    })
}

/// A request in the stateless protocol names its version in `_meta`. Returns true for
/// one, false for a legacy request, and an error when the version is not served or the
/// request is not complete.
fn check_modern(params: &Value) -> Result<bool, RpcError> {
    let Some(meta) = params.get("_meta").and_then(Value::as_object) else {
        return Ok(false);
    };
    let Some(version) = meta.get(VERSION_KEY) else {
        return Ok(false);
    };

    let requested = version.as_str().unwrap_or_default();
    if requested != MODERN_VERSION {
        let mut supported = vec![MODERN_VERSION];
        supported.extend(LEGACY_VERSIONS);

        return Err(RpcError::with_data(
            UNSUPPORTED_VERSION,
            "Unsupported protocol version",
            json!({ "supported": supported, "requested": version }),
        ));
    }
    if !meta.contains_key(CAPABILITIES_KEY) {
        return Err(RpcError::new(
            INVALID_PARAMS,
            format!("the request has no _meta[\"{CAPABILITIES_KEY}\"]"),
        ));
    }

    Ok(true)
}

struct RpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    fn with_data(code: i64, message: impl Into<String>, data: Value) -> Self {
        Self {
            code,
            message: message.into(),
            data: Some(data),
        }
    }

    fn reply(self, id: &Value) -> Value {
        let mut error = json!({ "code": self.code, "message": self.message });
        if let Some(data) = self.data {
            error["data"] = data;
        }

        json!({ "jsonrpc": "2.0", "id": id, "error": error })
    }
}

fn error_reply(id: &Value, code: i64, message: &str) -> Value {
    RpcError::new(code, message).reply(id)
}

/// Reads messages until `input` ends. A tool call runs on its own thread, because a
/// `wait` can take minutes and the client may send other requests meanwhile. When
/// `input` ends, calls that still run are cancelled after a moment.
pub(crate) fn serve(
    server: &Server,
    input: impl std::io::BufRead,
    output: impl std::io::Write + Send,
) {
    let output = Mutex::new(output);
    let send = |reply: &Value| {
        let mut output = output.lock().unwrap_or_else(PoisonError::into_inner);

        writeln!(output, "{reply}")
            .and_then(|()| output.flush())
            .is_ok()
    };

    let send = &send;

    std::thread::scope(|scope| {
        for line in input.lines() {
            let Ok(line) = line else {
                break;
            };
            if line.trim().is_empty() {
                continue;
            }

            let message: Value = match serde_json::from_str(&line) {
                Ok(message) => message,
                Err(error) => {
                    send(&error_reply(&Value::Null, PARSE_ERROR, &error.to_string()));
                    continue;
                }
            };

            if message.get("method").and_then(Value::as_str) == Some("tools/call") {
                if let Some(id) = message
                    .get("id")
                    .filter(|id| id.is_string() || id.is_number())
                {
                    server.register(id);
                }
                scope.spawn(move || {
                    if let Some(reply) = server.handle(&message) {
                        send(&reply);
                    }
                });
            } else if let Some(reply) = server.handle(&message) {
                send(&reply);
            }
        }

        server.close();
    });
}

pub(crate) fn run() -> ExitCode {
    let server = Server::from_env();
    let stdin = std::io::stdin();

    serve(&server, stdin.lock(), std::io::stdout());

    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(role: Role) -> Server {
        Server::new(role, Err("not connected".to_owned()))
    }

    fn request(method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
    }

    fn tool_names(role: Role) -> Vec<String> {
        let reply = server(role)
            .handle(&request("tools/list", json!({})))
            .expect("reply");

        reply["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|tool| tool["name"].as_str().expect("name").to_owned())
            .collect()
    }

    const TERMINAL_TOOLS: [&str; 6] = [
        "terminal_spawn",
        "terminal_resize",
        "terminal_read",
        "terminal_wait",
        "terminal_kill",
        "terminal_list",
    ];

    fn modern_meta() -> Value {
        json!({ VERSION_KEY: MODERN_VERSION, CAPABILITIES_KEY: {} })
    }

    #[test]
    fn initialize_echoes_a_version_it_serves() {
        let reply = server(Role::Worker)
            .handle(&request(
                "initialize",
                json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {} }),
            ))
            .expect("reply");

        assert_eq!(reply["id"], 1);
        assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(reply["result"]["serverInfo"]["name"], "swarm");
        assert!(reply["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn initialize_offers_its_newest_version_for_an_unknown_one() {
        let reply = server(Role::Worker)
            .handle(&request(
                "initialize",
                json!({ "protocolVersion": "1999-01-01" }),
            ))
            .expect("reply");

        assert_eq!(reply["result"]["protocolVersion"], LEGACY_VERSIONS[0]);
    }

    #[test]
    fn a_notification_gets_no_reply() {
        let message = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });

        assert!(server(Role::Worker).handle(&message).is_none());
    }

    #[test]
    fn ping_answers_with_an_empty_object() {
        let reply = server(Role::Worker)
            .handle(&request("ping", Value::Null))
            .expect("reply");

        assert_eq!(reply["result"], json!({}));
    }

    #[test]
    fn an_orchestrator_lists_its_tools() {
        assert_eq!(
            tool_names(Role::Orchestrator),
            [
                "spawn",
                "send",
                "commit",
                "read",
                "wait",
                "merge",
                "notify",
                "agents",
                "confirm",
                "pr_draft",
                "tile_grow",
                "tile_shrink"
            ]
            .into_iter()
            .chain(TERMINAL_TOOLS)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_worker_lists_only_its_tools() {
        assert_eq!(
            tool_names(Role::Worker),
            [
                "ask",
                "report",
                "commit",
                "read",
                "wait",
                "tile_grow",
                "tile_shrink"
            ]
            .into_iter()
            .chain(TERMINAL_TOOLS)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_start_orchestrator_and_an_unknown_role_get_the_shared_tools() {
        assert_eq!(
            tool_names(Role::StartOrchestrator),
            [
                "read",
                "wait",
                "task_create",
                "task_list",
                "task_wait",
                "agent_exit"
            ]
            .into_iter()
            .chain(TERMINAL_TOOLS)
            .collect::<Vec<_>>()
        );
        assert_eq!(
            tool_names(Role::Unknown),
            ["read", "wait"]
                .into_iter()
                .chain(TERMINAL_TOOLS)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            Role::parse(Some("start_orchestrator")),
            Role::StartOrchestrator
        );
        assert_eq!(Role::parse(None), Role::Unknown);
        assert_eq!(Role::parse(Some("admin")), Role::Unknown);
    }

    fn build(tool: &str, arguments: &Value) -> Result<Command, String> {
        let tool = TOOLS
            .iter()
            .find(|candidate| candidate.name == tool)
            .expect("tool");

        (tool.build)(arguments.as_object().expect("object"))
    }

    #[test]
    fn the_tile_tools_take_an_optional_agent_id_or_name() {
        assert_eq!(
            build("tile_grow", &json!({})),
            Ok(Command::TileGrow { agent: None })
        );
        assert_eq!(
            build("tile_grow", &json!({ "agent": 9 })),
            Ok(Command::TileGrow {
                agent: Some("9".to_owned())
            })
        );
        assert_eq!(
            build("tile_shrink", &json!({ "agent": "api" })),
            Ok(Command::TileShrink {
                agent: Some("api".to_owned())
            })
        );
        assert_eq!(
            build("tile_shrink", &json!({ "agent": null })),
            Ok(Command::TileShrink { agent: None })
        );
        assert_eq!(
            build("tile_grow", &json!({ "agent": true })),
            Err("agent must be a string".to_owned())
        );
    }

    #[test]
    fn the_tile_tools_need_no_arguments() {
        let reply = server(Role::Worker)
            .handle(&request("tools/list", json!({})))
            .expect("reply");

        for name in ["tile_grow", "tile_shrink"] {
            let tool = reply["result"]["tools"]
                .as_array()
                .expect("tools")
                .iter()
                .find(|tool| tool["name"] == name)
                .expect("tile tool");

            assert_eq!(tool["inputSchema"]["required"], json!([]), "{name}");
            assert_eq!(
                tool["inputSchema"]["properties"]["agent"]["type"], "string",
                "{name}"
            );
        }
    }

    #[test]
    fn the_tile_tools_name_only_what_the_app_can_do() {
        let reply = server(Role::Worker)
            .handle(&request("tools/list", json!({})))
            .expect("reply");

        for name in ["tile_grow", "tile_shrink"] {
            let tool = reply["result"]["tools"]
                .as_array()
                .expect("tools")
                .iter()
                .find(|tool| tool["name"] == name)
                .expect("tile tool");
            let description = tool["description"].as_str().expect("description");

            assert!(description.contains("minimized"), "{name}: {description}");
            assert!(!description.contains("pinned"), "{name}: {description}");
            assert!(!description.contains("resized"), "{name}: {description}");
        }
    }

    #[test]
    fn every_tool_has_a_description_and_an_object_schema() {
        let reply = server(Role::Orchestrator)
            .handle(&request("tools/list", json!({})))
            .expect("reply");

        for tool in reply["result"]["tools"].as_array().expect("tools") {
            assert!(
                !tool["description"]
                    .as_str()
                    .expect("description")
                    .is_empty()
            );
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert!(tool["inputSchema"]["properties"].is_object());
        }
    }

    #[test]
    fn a_tool_outside_the_role_is_unknown() {
        let reply = server(Role::Worker)
            .handle(&request(
                "tools/call",
                json!({ "name": "spawn", "arguments": {} }),
            ))
            .expect("reply");

        assert_eq!(reply["error"]["code"], INVALID_PARAMS);
        assert!(
            reply["error"]["message"]
                .as_str()
                .expect("message")
                .contains("spawn")
        );
    }

    #[test]
    fn the_start_orchestrator_cannot_commit() {
        let reply = server(Role::StartOrchestrator)
            .handle(&request(
                "tools/call",
                json!({ "name": "commit", "arguments": {} }),
            ))
            .expect("reply");

        assert_eq!(reply["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn the_commit_tool_takes_a_message_and_a_list_of_paths() {
        let reply = server(Role::Worker)
            .handle(&request("tools/list", json!({})))
            .expect("reply");
        let tools = reply["result"]["tools"].as_array().expect("tools");
        let commit = tools
            .iter()
            .find(|tool| tool["name"] == "commit")
            .expect("commit tool");

        assert_eq!(
            commit["inputSchema"]["required"],
            json!(["message", "paths"])
        );
        assert_eq!(
            commit["inputSchema"]["properties"]["paths"]["type"],
            "array"
        );
        assert_eq!(
            commit["inputSchema"]["properties"]["paths"]["items"]["type"],
            "string"
        );
    }

    #[test]
    fn the_spawn_tool_offers_own_worktree_but_does_not_require_it() {
        let reply = server(Role::Orchestrator)
            .handle(&request("tools/list", json!({})))
            .expect("reply");
        let tools = reply["result"]["tools"].as_array().expect("tools");
        let spawn = tools
            .iter()
            .find(|tool| tool["name"] == "spawn")
            .expect("spawn tool");

        assert_eq!(
            spawn["inputSchema"]["properties"]["own_worktree"]["type"],
            "string"
        );
        assert_eq!(spawn["inputSchema"]["required"], json!(["name", "brief"]));
    }

    #[test]
    fn bad_arguments_are_a_tool_error_with_is_error() {
        let reply = server(Role::Orchestrator)
            .handle(&request(
                "tools/call",
                json!({ "name": "send", "arguments": { "to": "api" } }),
            ))
            .expect("reply");

        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(reply["result"]["content"][0]["text"], "text is required");
    }

    #[test]
    fn a_call_without_a_daemon_is_a_tool_error() {
        let reply = server(Role::Worker)
            .handle(&request("tools/call", json!({ "name": "read" })))
            .expect("reply");

        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(reply["result"]["content"][0]["type"], "text");
        assert_eq!(reply["result"]["content"][0]["text"], "not connected");
    }

    #[test]
    fn an_unknown_method_is_not_found() {
        let reply = server(Role::Worker)
            .handle(&request("resources/list", json!({})))
            .expect("reply");

        assert_eq!(reply["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn a_modern_request_gets_a_result_type() {
        let reply = server(Role::Worker)
            .handle(&request("tools/list", json!({ "_meta": modern_meta() })))
            .expect("reply");

        assert_eq!(reply["result"]["resultType"], "complete");
        assert_eq!(
            reply["result"]["tools"].as_array().expect("tools").len(),
            13
        );
    }

    #[test]
    fn a_legacy_request_gets_no_result_type() {
        let reply = server(Role::Worker)
            .handle(&request("tools/list", json!({})))
            .expect("reply");

        assert!(reply["result"].get("resultType").is_none());
    }

    #[test]
    fn discover_lists_the_versions() {
        let reply = server(Role::Worker)
            .handle(&request(
                "server/discover",
                json!({ "_meta": modern_meta() }),
            ))
            .expect("reply");

        assert_eq!(reply["result"]["supportedVersions"][0], MODERN_VERSION);
        assert_eq!(reply["result"]["_meta"][SERVER_INFO_KEY]["name"], "swarm");
        assert_eq!(reply["result"]["resultType"], "complete");
    }

    #[test]
    fn the_tools_are_the_harness_calls_the_adapters_know() {
        let mut tools: Vec<String> = [Role::Orchestrator, Role::Worker, Role::StartOrchestrator]
            .into_iter()
            .flat_map(tool_names)
            .collect();
        tools.sort();
        tools.dedup();
        let mut calls: Vec<String> = swarm_protocol::agent::HARNESS_CALLS
            .iter()
            .map(|(tool, _)| (*tool).to_owned())
            .collect();
        calls.sort();

        assert_eq!(tools, calls);
    }

    #[test]
    fn modern_lists_say_how_long_they_may_be_cached() {
        for method in ["tools/list", "server/discover"] {
            let reply = server(Role::Worker)
                .handle(&request(method, json!({ "_meta": modern_meta() })))
                .expect("reply");

            assert_eq!(reply["result"]["ttlMs"], 0, "{method}");
            assert_eq!(reply["result"]["cacheScope"], "private", "{method}");
        }
    }

    #[test]
    fn a_legacy_list_has_no_cache_fields() {
        let reply = server(Role::Worker)
            .handle(&request("tools/list", json!({})))
            .expect("reply");

        assert!(reply["result"].get("ttlMs").is_none());
    }

    #[test]
    fn ping_is_unknown_in_the_stateless_protocol() {
        let reply = server(Role::Worker)
            .handle(&request("ping", json!({ "_meta": modern_meta() })))
            .expect("reply");

        assert_eq!(reply["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn the_protocol_with_batches_is_not_offered() {
        assert!(!LEGACY_VERSIONS.contains(&"2025-03-26"));
        let reply = server(Role::Worker)
            .handle(&request(
                "initialize",
                json!({ "protocolVersion": "2025-03-26" }),
            ))
            .expect("reply");

        assert_eq!(reply["result"]["protocolVersion"], LEGACY_VERSIONS[0]);
    }

    #[test]
    fn a_response_from_the_client_gets_no_reply() {
        let server = server(Role::Worker);

        assert!(
            server
                .handle(&json!({ "jsonrpc": "2.0", "id": 5, "result": {} }))
                .is_none()
        );
        assert!(
            server
                .handle(
                    &json!({ "jsonrpc": "2.0", "id": "a", "error": { "code": 1, "message": "x" } })
                )
                .is_none()
        );
    }

    #[test]
    fn a_null_id_is_an_invalid_request() {
        let reply = server(Role::Worker)
            .handle(&json!({ "jsonrpc": "2.0", "id": null, "method": "tools/list" }))
            .expect("reply");

        assert_eq!(reply["error"]["code"], INVALID_REQUEST);
        assert!(reply["id"].is_null());
    }

    #[test]
    fn a_cancel_closes_the_connection_of_the_call_and_sends_no_reply() {
        use std::io::Read as _;
        use std::os::unix::net::UnixListener;

        let socket = std::path::PathBuf::from(format!("/tmp/swm-cancel-{}", std::process::id()));
        std::fs::remove_file(&socket).ok();
        let listener = UnixListener::bind(&socket).expect("bind");
        let session = Session::from_vars(|name| match name {
            "SWARM_SOCKET" => Some(socket.display().to_string()),
            "SWARM_AGENT_ID" => Some("3".to_owned()),
            "SWARM_TOKEN" => Some("t".to_owned()),
            _ => None,
        })
        .expect("session");
        let server = Server::new(Role::Worker, Ok(session));

        let (reply, hung_up) = std::thread::scope(|scope| {
            let daemon = scope.spawn(|| {
                let (mut stream, _) = listener.accept().expect("accept");
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .expect("timeout");
                let mut seen = Vec::new();
                let mut buffer = [0_u8; 256];
                loop {
                    match stream.read(&mut buffer) {
                        Ok(0) => return true,
                        Ok(count) => seen.extend_from_slice(&buffer[..count]),
                        Err(_) => return false,
                    }
                }
            });
            let call = scope.spawn(|| {
                server.handle(&json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call",
                    "params": { "name": "wait", "arguments": { "timeout": 60 } } }))
            });

            while server.pending().is_empty() {
                std::thread::park_timeout(Duration::from_millis(10));
            }
            std::thread::park_timeout(Duration::from_millis(200));
            let cancelled = server.handle(&json!({ "jsonrpc": "2.0",
                "method": "notifications/cancelled", "params": { "requestId": 9 } }));
            assert!(cancelled.is_none());

            (call.join().expect("call"), daemon.join().expect("daemon"))
        });
        std::fs::remove_file(&socket).ok();

        assert!(reply.is_none(), "a cancelled call gets no reply");
        assert!(hung_up, "the daemon saw the connection close");
    }

    #[test]
    fn a_modern_request_for_another_version_lists_the_supported_ones() {
        let meta = json!({ VERSION_KEY: "1900-01-01", CAPABILITIES_KEY: {} });

        let reply = server(Role::Worker)
            .handle(&request("tools/list", json!({ "_meta": meta })))
            .expect("reply");

        assert_eq!(reply["error"]["code"], UNSUPPORTED_VERSION);
        assert_eq!(reply["error"]["data"]["requested"], "1900-01-01");
        assert_eq!(reply["error"]["data"]["supported"][0], MODERN_VERSION);
    }

    #[test]
    fn a_modern_request_without_capabilities_is_malformed() {
        let reply = server(Role::Worker)
            .handle(&request(
                "tools/list",
                json!({ "_meta": { VERSION_KEY: MODERN_VERSION } }),
            ))
            .expect("reply");

        assert_eq!(reply["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn the_server_answers_each_line_and_reports_bad_json() {
        let input = [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "not json",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        ]
        .join("\n");
        let mut output = Vec::new();

        serve(&server(Role::Worker), input.as_bytes(), &mut output);

        let lines: Vec<Value> = String::from_utf8(output)
            .expect("utf8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("json"))
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["id"], 1);
        assert_eq!(lines[1]["error"]["code"], PARSE_ERROR);
        assert_eq!(lines[2]["id"], 2);
    }

    #[test]
    fn numbers_and_names_are_accepted_for_a_worker() {
        let arguments = json!({ "worker": 12 });
        let by_name = json!({ "worker": "api" });

        assert_eq!(
            identifier(arguments.as_object().expect("object"), "worker"),
            Ok("12".to_owned())
        );
        assert_eq!(
            identifier(by_name.as_object().expect("object"), "worker"),
            Ok("api".to_owned())
        );
    }
}
