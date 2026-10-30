//! `swarm mcp`: a stdio MCP server that exposes the same commands as tools.
//!
//! Written by hand on `serde_json`, one JSON-RPC message per line. It speaks both
//! eras of the protocol, see <https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning>:
//! the legacy `initialize` handshake that Claude Code and Codex use today, and the
//! stateless requests of 2026-07-28 with `server/discover`. The daemon checks every
//! call again; the tools listed only follow `SWARM_ROLE`.

use std::process::ExitCode;
use std::sync::Mutex;

use serde_json::{Map, Value, json};

use crate::commands::{Command, parse_kind};
use crate::session::Session;

/// The stateless protocol version this server speaks.
const MODERN_VERSION: &str = "2026-07-28";

/// The versions served after an `initialize` handshake, newest first.
const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

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
        description: "Start a worker in its own worktree and branch. The brief is its first message: say what to do and what done means.",
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
                    "Seconds to wait. Defaults to 120, at most 600.",
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
            (
                (*name).to_owned(),
                json!({ "type": kind, "description": description }),
            )
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
}

impl Server {
    pub(crate) fn new(role: Role, session: Result<Session, String>) -> Self {
        Self { role, session }
    }

    fn from_env() -> Self {
        let role = Role::parse(std::env::var("SWARM_ROLE").ok().as_deref());

        Self::new(role, Session::from_env())
    }

    /// The reply to one message, or `None` for a notification.
    pub(crate) fn handle(&self, message: &Value) -> Option<Value> {
        let Some(object) = message.as_object() else {
            return Some(error_reply(
                &Value::Null,
                INVALID_REQUEST,
                "not a JSON-RPC message",
            ));
        };
        let method = object.get("method").and_then(Value::as_str);
        let id = object.get("id")?;
        let Some(method) = method else {
            return Some(error_reply(
                id,
                INVALID_REQUEST,
                "the request has no method",
            ));
        };
        let params = object.get("params").cloned().unwrap_or(Value::Null);

        let outcome = self.dispatch(method, &params);

        Some(match outcome {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => error.reply(id),
        })
    }

    fn dispatch(&self, method: &str, params: &Value) -> Result<Value, RpcError> {
        if method == "initialize" {
            return Ok(Self::initialize(params));
        }
        if method == "ping" {
            return Ok(json!({}));
        }

        let modern = check_modern(params)?;
        let mut result = match method {
            "server/discover" => Self::discover(),
            "tools/list" => self.list_tools(),
            "tools/call" => self.call_tool(params)?,
            other => {
                return Err(RpcError::new(
                    METHOD_NOT_FOUND,
                    format!("method not found: {other}"),
                ));
            }
        };

        if modern && let Some(object) = result.as_object_mut() {
            object.insert("resultType".to_owned(), json!("complete"));
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

    fn call_tool(&self, params: &Value) -> Result<Value, RpcError> {
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

            command.run(session)
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
/// `wait` can take minutes and the client may send other requests meanwhile.
pub(crate) fn serve(
    server: &Server,
    input: impl std::io::BufRead,
    output: impl std::io::Write + Send,
) {
    let output = Mutex::new(output);
    let send = |reply: &Value| {
        let mut output = output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

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
                scope.spawn(move || {
                    if let Some(reply) = server.handle(&message) {
                        send(&reply);
                    }
                });
            } else if let Some(reply) = server.handle(&message) {
                send(&reply);
            }
        }
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
                "spawn", "send", "read", "wait", "merge", "notify", "agents", "confirm"
            ]
        );
    }

    #[test]
    fn a_worker_lists_only_its_tools() {
        assert_eq!(tool_names(Role::Worker), ["ask", "report", "read", "wait"]);
    }

    #[test]
    fn the_start_orchestrator_and_an_unknown_role_get_the_shared_tools() {
        assert_eq!(tool_names(Role::StartOrchestrator), ["read", "wait"]);
        assert_eq!(tool_names(Role::Unknown), ["read", "wait"]);
        assert_eq!(
            Role::parse(Some("start_orchestrator")),
            Role::StartOrchestrator
        );
        assert_eq!(Role::parse(None), Role::Unknown);
        assert_eq!(Role::parse(Some("admin")), Role::Unknown);
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
        assert_eq!(reply["result"]["tools"].as_array().expect("tools").len(), 4);
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
