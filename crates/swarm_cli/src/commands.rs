//! What an agent can ask Swarm to do. The command line and the MCP server both build a
//! [`Command`] and share the code that runs it and words the result.

use std::io::Read as _;

use serde::de::DeserializeOwned;
use serde_json::Value;
use swarm_protocol::agent::{
    AgentSummary, Call, Committed, InboxMessage, Merged, Sent, TaskSummary, TaskUntil,
    TerminalSize, TerminalSummary, TerminalText, TerminalUntil, TerminalWaited, render_inbox,
};
use swarm_protocol::{AgentId, AgentKind, AgentLifecycle, GridSize, TaskId, TerminalId};

use crate::session::Session;

/// Longest text read from stdin.
const MAX_STDIN_BYTES: u64 = 4 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    Spawn {
        name: String,
        kind: Option<AgentKind>,
        model: Option<String>,
        brief: String,
        /// The reason the worker needs its own worktree; `None` shares the task worktree.
        own_worktree: Option<String>,
    },
    Commit {
        message: String,
        paths: Vec<std::path::PathBuf>,
    },
    /// `to` is an agent ID or a worker name.
    Send {
        to: String,
        text: String,
    },
    Ask {
        text: String,
    },
    Report {
        text: String,
    },
    Notify {
        text: String,
    },
    Read,
    Wait {
        timeout_secs: Option<u32>,
    },
    /// `worker` is an agent ID or a worker name.
    Merge {
        worker: String,
    },
    Agents,
    Confirm,
    PrDraft {
        title: String,
        body: String,
    },
    /// `agent` is an agent ID or a worker name; `None` is the caller's own tile.
    TileGrow {
        agent: Option<String>,
    },
    TileShrink {
        agent: Option<String>,
    },
    TaskCreate {
        name: String,
        from: Option<String>,
        brief: String,
        kind: Option<AgentKind>,
    },
    TaskList,
    TaskWait {
        ids: Vec<TaskId>,
        until: TaskUntil,
        timeout_secs: Option<u32>,
    },
    Exit,
    TerminalSpawn {
        command: Vec<String>,
        cwd: Option<std::path::PathBuf>,
        size: Option<TerminalSize>,
        name: Option<String>,
    },
    TerminalResize {
        id: TerminalId,
        size: TerminalSize,
    },
    TerminalRead {
        id: TerminalId,
        lines: Option<u32>,
    },
    TerminalWait {
        id: TerminalId,
        until: TerminalUntil,
        timeout_secs: Option<u32>,
    },
    TerminalKill {
        id: TerminalId,
        signal: Option<String>,
    },
    TerminalList,
}

/// What a command printed: the raw result for `--json`, the words for people and agents.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Output {
    pub(crate) json: Value,
    pub(crate) text: String,
    /// False when the command worked but the outcome needs a look, like a merge conflict.
    pub(crate) clean: bool,
}

impl Command {
    pub(crate) fn run(self, session: &Session) -> Result<Output, String> {
        match self {
            Self::Spawn {
                name,
                kind,
                model,
                brief,
                own_worktree,
            } => {
                let result = session.call(Call::Spawn {
                    name,
                    kind,
                    model,
                    brief,
                    own_worktree,
                })?;
                let worker: AgentSummary = decode(&result)?;
                let text = format!(
                    "started worker {} (agent {})\nbranch: {}\nworktree: {}",
                    worker.name,
                    worker.id.0,
                    worker.branch,
                    worker.worktree.display()
                );

                Ok(output(result, text))
            }
            Self::Commit { message, paths } => {
                let result = session.call(Call::Commit { message, paths })?;
                let committed: Committed = decode(&result)?;

                Ok(output(result, format!("committed {}", committed.commit)))
            }
            Self::Send { to, text } => {
                let to = resolve(session, &to)?;

                sent(session.call(Call::Send { to, text })?)
            }
            Self::Ask { text } => sent(session.call(Call::Ask { question: text })?),
            Self::Report { text } => sent(session.call(Call::Report { text })?),
            Self::Notify { text } => {
                let result = session.call(Call::Notify { text })?;

                Ok(output(result, "ok".to_owned()))
            }
            Self::Read => inbox(session.call(Call::Read)?),
            Self::Wait { timeout_secs } => inbox(session.call(Call::Wait { timeout_secs })?),
            Self::Merge { worker } => {
                let worker = resolve(session, &worker)?;

                merged(session.call(Call::Merge { worker })?)
            }
            Self::Agents => {
                let result = session.call(Call::Agents)?;
                let workers: Vec<AgentSummary> = decode(&result)?;

                Ok(output(result, agents_text(&workers)))
            }
            Self::Confirm => {
                let result = session.call(Call::Confirm)?;

                Ok(output(result, "ok".to_owned()))
            }
            Self::PrDraft { title, body } => {
                let result = session.call(Call::PrDraft { title, body })?;

                Ok(output(result, "pull request draft sent".to_owned()))
            }
            Self::TileGrow { agent } => tile(session, agent, true),
            Self::TileShrink { agent } => tile(session, agent, false),
            Self::TaskCreate { .. } | Self::TaskList | Self::TaskWait { .. } | Self::Exit => {
                self.run_task(session)
            }
            Self::TerminalSpawn { .. }
            | Self::TerminalResize { .. }
            | Self::TerminalRead { .. }
            | Self::TerminalWait { .. }
            | Self::TerminalKill { .. }
            | Self::TerminalList => self.run_terminal(session),
        }
    }
}

impl Command {
    fn run_task(self, session: &Session) -> Result<Output, String> {
        match self {
            Self::TaskCreate {
                name,
                from,
                brief,
                kind,
            } => {
                let result = session.call(Call::TaskCreate {
                    name,
                    from,
                    brief,
                    kind,
                })?;
                let task: TaskSummary = decode(&result)?;
                let orchestrator = task.orchestrator.as_ref().map_or_else(
                    || "none".to_owned(),
                    |agent| format!("agent {}", agent.id.0),
                );
                let text = format!(
                    "created task {} ({})\nbranch: {}\norchestrator: {orchestrator}",
                    task.id.0, task.name, task.branch
                );

                Ok(output(result, text))
            }
            Self::TaskList => {
                let result = session.call(Call::TaskList)?;
                let tasks: Vec<TaskSummary> = decode(&result)?;

                Ok(output(result, tasks_text(&tasks)))
            }
            Self::TaskWait {
                ids,
                until,
                timeout_secs,
            } => {
                let result = session.call(Call::TaskWait {
                    ids,
                    until,
                    timeout_secs,
                })?;
                let tasks: Vec<TaskSummary> = decode(&result)?;

                let reached = tasks.iter().all(|task| task_reached(until, task));
                let mut text = tasks_text(&tasks);
                if !reached {
                    text.push_str("\ntimed out");
                }

                Ok(Output {
                    json: result,
                    text,
                    clean: reached,
                })
            }
            Self::Exit => {
                let result = session.call(Call::AgentExit)?;

                Ok(output(result, "ok".to_owned()))
            }
            other => Err(format!("{other:?} is not a task command")),
        }
    }

    fn run_terminal(self, session: &Session) -> Result<Output, String> {
        match self {
            Self::TerminalSpawn {
                command,
                cwd,
                size,
                name,
            } => {
                let result = session.call(Call::TerminalSpawn {
                    command,
                    cwd,
                    size,
                    name,
                })?;
                let terminal: TerminalSummary = decode(&result)?;
                let text = format!(
                    "started terminal {} ({})\nsize: {}",
                    terminal.id.0,
                    terminal_label(&terminal),
                    grid_text(terminal.size)
                );

                Ok(output(result, text))
            }
            Self::TerminalResize { id, size } => {
                let result = session.call(Call::TerminalResize { id, size })?;
                let terminal: TerminalSummary = decode(&result)?;
                let text = format!(
                    "terminal {} is {}; its tile sets the size",
                    terminal.id.0,
                    grid_text(terminal.size)
                );

                Ok(output(result, text))
            }
            Self::TerminalRead { id, lines } => {
                let result = session.call(Call::TerminalRead { id, lines })?;
                let read: TerminalText = decode(&result)?;

                Ok(output(result, read.text))
            }
            Self::TerminalWait {
                id,
                until,
                timeout_secs,
            } => {
                let result = session.call(Call::TerminalWait {
                    id,
                    until,
                    timeout_secs,
                })?;

                waited(result)
            }
            Self::TerminalKill { id, signal } => {
                let result = session.call(Call::TerminalKill { id, signal })?;
                let terminal: TerminalSummary = decode(&result)?;

                Ok(output(result, terminal_state(&terminal)))
            }
            Self::TerminalList => {
                let result = session.call(Call::TerminalList)?;
                let terminals: Vec<TerminalSummary> = decode(&result)?;

                Ok(output(result, terminals_text(&terminals)))
            }
            other => Err(format!("{other:?} is not a terminal command")),
        }
    }
}

/// Asks for a large or a small tile. The layout decides, so the answer only says that the
/// request went out.
fn tile(session: &Session, agent: Option<String>, large: bool) -> Result<Output, String> {
    let agent = agent.map(|name| resolve(session, &name)).transpose()?;

    let (call, text) = if large {
        (Call::TileGrow { agent }, "asked for a large tile")
    } else {
        (Call::TileShrink { agent }, "asked for a small tile")
    };

    let result = session.call(call)?;

    Ok(output(result, text.to_owned()))
}

/// The task orchestrator confirmed its brief, or finished.
fn task_reached(until: TaskUntil, task: &TaskSummary) -> bool {
    match until {
        TaskUntil::Briefed => task.briefed,
        TaskUntil::Done => task.orchestrator.as_ref().is_some_and(|orchestrator| {
            orchestrator.status == "done"
                || matches!(
                    orchestrator.lifecycle,
                    AgentLifecycle::Stopped | AgentLifecycle::Resumable
                )
        }),
    }
}

fn tasks_text(tasks: &[TaskSummary]) -> String {
    if tasks.is_empty() {
        return "no tasks".to_owned();
    }

    tasks
        .iter()
        .map(|task| {
            let orchestrator = task.orchestrator.as_ref().map_or_else(
                || "no orchestrator".to_owned(),
                |agent| format!("{} (agent {})", agent.status, agent.id.0),
            );

            format!(
                "{}  {}  {}  {}  {orchestrator}",
                task.id.0,
                task.name,
                task.branch,
                if task.briefed {
                    "briefed"
                } else {
                    "not briefed"
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn grid_text(size: GridSize) -> String {
    format!("{}x{}", size.cols, size.rows)
}

fn terminal_label(terminal: &TerminalSummary) -> String {
    terminal
        .name
        .clone()
        .unwrap_or_else(|| terminal.command.join(" "))
}

fn terminal_state(terminal: &TerminalSummary) -> String {
    match (&terminal.exit, terminal.running) {
        (Some(exit), _) => format!("terminal {} ended, {exit}", terminal.id.0),
        (None, true) => format!("terminal {} runs", terminal.id.0),
        (None, false) => format!("terminal {} ended", terminal.id.0),
    }
}

fn terminals_text(terminals: &[TerminalSummary]) -> String {
    if terminals.is_empty() {
        return "no process terminals".to_owned();
    }

    terminals
        .iter()
        .map(|terminal| {
            let state = match (&terminal.exit, terminal.running) {
                (Some(exit), _) => format!("ended, {exit}"),
                (None, true) => "running".to_owned(),
                (None, false) => "ended".to_owned(),
            };

            format!(
                "{}  {}  {}  {}  agent {}",
                terminal.id.0,
                terminal_label(terminal),
                grid_text(terminal.size),
                state,
                terminal.owner.map_or(0, |owner| owner.0)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn waited(result: Value) -> Result<Output, String> {
    let waited: TerminalWaited = decode(&result)?;

    let text = match (&waited.line, waited.reached) {
        (Some(line), true) => format!("found: {line}"),
        (None, true) => terminal_state(&waited.terminal),
        (_, false) if !waited.terminal.running => format!(
            "{}; the text did not appear",
            terminal_state(&waited.terminal)
        ),
        (_, false) => "timed out".to_owned(),
    };

    Ok(Output {
        json: result,
        text,
        clean: waited.reached,
    })
}

/// `small`, `medium`, `large` or `COLSxROWS`.
pub(crate) fn parse_size(text: &str) -> Result<TerminalSize, String> {
    match text {
        "small" => return Ok(TerminalSize::Small),
        "medium" => return Ok(TerminalSize::Medium),
        "large" => return Ok(TerminalSize::Large),
        _ => {}
    }

    let wrong = || format!("{text:?} is not a size: use small, medium, large or COLSxROWS");
    let (cols, rows) = text.split_once(['x', 'X']).ok_or_else(wrong)?;

    Ok(TerminalSize::Grid(GridSize {
        cols: cols.parse().map_err(|_| wrong())?,
        rows: rows.parse().map_err(|_| wrong())?,
    }))
}

pub(crate) fn parse_until(name: &str) -> Result<TaskUntil, String> {
    match name {
        "briefed" => Ok(TaskUntil::Briefed),
        "done" => Ok(TaskUntil::Done),
        other => Err(format!("unknown state {other:?}: use briefed or done")),
    }
}

fn output(json: Value, text: String) -> Output {
    Output {
        json,
        text,
        clean: true,
    }
}

fn decode<T: DeserializeOwned>(value: &Value) -> Result<T, String> {
    serde_json::from_value(value.clone())
        .map_err(|error| format!("Swarm sent an answer that cannot be read: {error}"))
}

fn sent(result: Value) -> Result<Output, String> {
    let sent: Sent = decode(&result)?;

    Ok(output(result, format!("sent (message {})", sent.message.0)))
}

fn inbox(result: Value) -> Result<Output, String> {
    let messages: Vec<InboxMessage> = decode(&result)?;
    let text = if messages.is_empty() {
        "no new messages".to_owned()
    } else {
        render_inbox(&messages)
    };

    Ok(output(result, text))
}

fn merged(result: Value) -> Result<Output, String> {
    let merged: Merged = decode(&result)?;

    if merged.merged {
        let text = if merged.output.is_empty() {
            "merged".to_owned()
        } else {
            format!("merged\n{}", merged.output)
        };

        return Ok(output(result, text));
    }

    let files: Vec<String> = merged
        .conflicts
        .iter()
        .map(|path| format!("  {}", path.display()))
        .collect();
    let text = format!(
        "conflict in {} {}:\n{}\n{}\nResolve the conflicts in the task worktree, then commit.",
        files.len(),
        if files.len() == 1 { "file" } else { "files" },
        files.join("\n"),
        merged.output
    );

    Ok(Output {
        json: result,
        text,
        clean: false,
    })
}

fn agents_text(workers: &[AgentSummary]) -> String {
    if workers.is_empty() {
        return "no workers".to_owned();
    }

    workers
        .iter()
        .map(|worker| {
            let status = match &worker.message {
                Some(message) => format!("{}: {message}", worker.status),
                None => worker.status.clone(),
            };

            format!(
                "{}  {}  {}  {}  {}  {}  {}",
                worker.id.0,
                worker.name,
                kind_name(worker.kind),
                worker.model,
                status,
                worker.branch,
                worker.worktree.display()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn kind_name(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::ClaudeCode => "claude",
        AgentKind::Codex => "codex",
    }
}

pub(crate) fn parse_kind(name: &str) -> Result<AgentKind, String> {
    match name {
        "claude" | "claude-code" | "claude_code" => Ok(AgentKind::ClaudeCode),
        "codex" => Ok(AgentKind::Codex),
        other => Err(format!("unknown agent kind {other:?}: use claude or codex")),
    }
}

/// An agent ID, the caller's own name, or the ID of the worker with this name.
fn resolve(session: &Session, target: &str) -> Result<AgentId, String> {
    if let Ok(id) = target.parse::<u64>() {
        return Ok(AgentId(id));
    }

    // Only an orchestrator may list its workers, so a worker names itself without asking.
    if session.name.as_deref() == Some(target) {
        return Ok(session.agent);
    }

    let result = session.call(Call::Agents)?;
    let workers: Vec<AgentSummary> = decode(&result)?;

    find_worker(&workers, target)
}

fn find_worker(workers: &[AgentSummary], name: &str) -> Result<AgentId, String> {
    workers
        .iter()
        .find(|worker| worker.name == name)
        .map(|worker| worker.id)
        .ok_or_else(|| {
            let names: Vec<&str> = workers.iter().map(|worker| worker.name.as_str()).collect();

            if names.is_empty() {
                format!("no worker named {name:?}: you have no workers yet")
            } else {
                format!(
                    "no worker named {name:?}: your workers are {}",
                    names.join(", ")
                )
            }
        })
}

/// Text for a command: the words as given, or all of stdin when they are `-`.
pub(crate) fn text_or_stdin(
    words: &[String],
    stdin: &mut impl std::io::Read,
) -> Result<String, String> {
    let text = if matches!(words, [only] if only == "-") {
        read_all(stdin)?
    } else {
        words.join(" ")
    };

    let text = text.trim_end().to_owned();
    if text.trim().is_empty() {
        return Err("the text is empty".to_owned());
    }

    Ok(text)
}

pub(crate) fn read_all(input: &mut impl std::io::Read) -> Result<String, String> {
    let mut bytes = Vec::new();
    input
        .take(MAX_STDIN_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read the text: {error}"))?;
    if bytes.len() as u64 > MAX_STDIN_BYTES {
        return Err("the text is longer than 4 MiB".to_owned());
    }

    String::from_utf8(bytes).map_err(|_| "the text is not valid UTF-8".to_owned())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;
    use swarm_protocol::{AgentLifecycle, AgentRole, MessageId};

    use super::*;

    fn worker(id: u64, name: &str) -> AgentSummary {
        AgentSummary {
            id: AgentId(id),
            name: name.to_owned(),
            role: AgentRole::Worker,
            kind: AgentKind::ClaudeCode,
            model: "sonnet".to_owned(),
            status: "working".to_owned(),
            message: None,
            lifecycle: AgentLifecycle::Running,
            branch: "swarm/login--api".to_owned(),
            worktree: PathBuf::from("/repo/.worktrees/login--api"),
        }
    }

    #[test]
    fn a_worker_is_found_by_name() {
        let workers = [worker(4, "api"), worker(5, "ui")];

        assert_eq!(find_worker(&workers, "ui"), Ok(AgentId(5)));
    }

    #[test]
    fn a_caller_resolves_its_own_name_without_asking_the_daemon() {
        let session = Session::from_vars(|name| match name {
            "SWARM_SOCKET" => Some("/nonexistent/agent.sock".to_owned()),
            "SWARM_AGENT_ID" => Some("7".to_owned()),
            "SWARM_AGENT_NAME" => Some("api".to_owned()),
            "SWARM_TOKEN" => Some("token".to_owned()),
            _ => None,
        })
        .expect("session");

        assert_eq!(resolve(&session, "api"), Ok(AgentId(7)));
        assert_eq!(resolve(&session, "12"), Ok(AgentId(12)));
        let error = resolve(&session, "ui").expect_err("another name needs the daemon");
        assert!(error.contains("cannot reach Swarm"), "{error}");
    }

    #[test]
    fn an_unknown_worker_name_lists_the_workers() {
        let workers = [worker(4, "api"), worker(5, "ui")];

        let error = find_worker(&workers, "db").expect_err("unknown");

        assert!(error.contains("api, ui"), "{error}");
    }

    #[test]
    fn text_is_the_words_joined() {
        let words = ["fix".to_owned(), "the".to_owned(), "bug".to_owned()];

        assert_eq!(
            text_or_stdin(&words, &mut std::io::empty()).as_deref(),
            Ok("fix the bug")
        );
    }

    #[test]
    fn a_dash_reads_stdin() {
        let mut stdin = "line one\nline two\n".as_bytes();

        assert_eq!(
            text_or_stdin(&["-".to_owned()], &mut stdin).as_deref(),
            Ok("line one\nline two")
        );
    }

    #[test]
    fn empty_text_is_refused() {
        assert!(text_or_stdin(&["  ".to_owned()], &mut std::io::empty()).is_err());
        assert!(text_or_stdin(&["-".to_owned()], &mut std::io::empty()).is_err());
    }

    #[test]
    fn agent_kinds_have_short_names() {
        assert_eq!(parse_kind("claude"), Ok(AgentKind::ClaudeCode));
        assert_eq!(parse_kind("codex"), Ok(AgentKind::Codex));
        assert!(parse_kind("gpt").is_err());
        assert_eq!(kind_name(AgentKind::ClaudeCode), "claude");
    }

    #[test]
    fn a_workers_line_shows_status_branch_and_folder() {
        let mut blocked = worker(5, "ui");
        blocked.status = "blocked".to_owned();
        blocked.message = Some("Red or blue?".to_owned());

        let text = agents_text(&[worker(4, "api"), blocked]);

        assert_eq!(
            text,
            "4  api  claude  sonnet  working  swarm/login--api  /repo/.worktrees/login--api\n\
             5  ui  claude  sonnet  blocked: Red or blue?  swarm/login--api  /repo/.worktrees/login--api"
        );
        assert_eq!(agents_text(&[]), "no workers");
    }

    #[test]
    fn a_conflict_lists_the_files_and_is_not_clean() {
        let result = json!({
            "merged": false,
            "conflicts": ["src/a.rs", "src/b.rs"],
            "output": "CONFLICT (content): Merge conflict in src/a.rs",
        });

        let output = merged(result).expect("output");

        assert!(!output.clean);
        assert!(
            output
                .text
                .starts_with("conflict in 2 files:\n  src/a.rs\n  src/b.rs\n")
        );
        assert!(output.text.contains("CONFLICT (content)"));
    }

    #[test]
    fn a_clean_merge_is_clean() {
        let result = json!({ "merged": true, "conflicts": [], "output": "Merge made by the 'ort' strategy." });

        let output = merged(result).expect("output");

        assert!(output.clean);
        assert!(output.text.starts_with("merged\n"));
    }

    #[test]
    fn an_empty_inbox_says_so_and_messages_are_rendered() {
        assert_eq!(inbox(json!([])).expect("empty").text, "no new messages");

        let message = InboxMessage {
            id: MessageId(1),
            from: Some(AgentId(4)),
            from_name: "api".to_owned(),
            kind: swarm_protocol::MessageKind::Report,
            text: "done".to_owned(),
            sent_at: 5,
        };
        let result = serde_json::to_value([message]).expect("json");

        assert_eq!(
            inbox(result).expect("inbox").text,
            "[swarm] report from api (agent 4):\ndone"
        );
    }
}
