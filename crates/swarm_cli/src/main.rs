//! `swarm`: the command line that agents use to talk to Swarm (ADR 0008).
//!
//! It speaks one JSON object per line to the daemon's agent socket. No async, no
//! terminal socket: the daemon refuses agents on that one.

mod commands;
mod hook;
mod mcp;
mod session;
mod statusline;

use std::io::Write as _;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use swarm_protocol::agent::{TaskUntil, TerminalSize, TerminalUntil};
use swarm_protocol::{TaskId, TerminalId};

use crate::commands::{Command, Output, parse_size, parse_until, read_all, text_or_stdin};
use crate::session::Session;

#[derive(Parser, Debug)]
#[command(
    name = "swarm",
    version,
    about = "Talk to Swarm from an agent terminal",
    long_about = "Talk to Swarm from an agent terminal.\n\nWhich commands work depends on your role: orchestrators spawn, send, merge, notify, agents, confirm, pr draft and tile (for their workers); workers ask, report and tile (for themselves); workers and orchestrators commit; the Start orchestrator makes tasks; every agent can read, wait, exit and run terminals.\nA text of - is read from stdin."
)]
struct Cli {
    /// Print the raw result as JSON.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand, Debug)]
enum Action {
    /// Start a worker in the task worktree, or in its own worktree (orchestrator).
    Spawn {
        /// The worker's name; it becomes part of its branch.
        name: String,
        /// The agent kind; the default is your own.
        #[arg(long, value_enum)]
        kind: Option<KindArg>,
        /// The model; the default comes from the settings.
        #[arg(long)]
        model: Option<String>,
        /// What to do and what done means; read from stdin when missing.
        #[arg(long)]
        brief: Option<String>,
        /// Give the worker its own worktree, for this reason. Without it the worker
        /// shares the task worktree.
        #[arg(long, value_name = "REASON")]
        own_worktree: Option<String>,
    },
    /// Commit the named files, and nothing else, in your worktree (worker, orchestrator).
    Commit {
        /// The commit message.
        #[arg(short = 'm', long)]
        message: String,
        /// The files to commit, relative to your worktree.
        #[arg(required = true, num_args = 1..)]
        paths: Vec<std::path::PathBuf>,
    },
    /// Send a message to one of your workers (orchestrator).
    Send {
        /// A worker's agent ID or name.
        agent: String,
        /// The message; - reads stdin.
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
    },
    /// Ask your orchestrator a question (worker).
    Ask {
        /// The question; - reads stdin.
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
    },
    /// Report progress or the result to your orchestrator (worker).
    Report {
        /// The report; - reads stdin.
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
    },
    /// Ask the user for attention (orchestrator).
    Notify {
        /// Why; - reads stdin.
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
    },
    /// Take your unread messages.
    Read,
    /// Wait for a message, then take your unread messages.
    Wait {
        /// Seconds to wait: 100 by default, 600 at most.
        #[arg(long)]
        timeout: Option<u32>,
    },
    /// Merge a worker's branch into the task branch (orchestrator).
    Merge {
        /// A worker's agent ID or name.
        worker: String,
    },
    /// List your workers (orchestrator).
    Agents,
    /// Say that you understood your brief (orchestrator).
    Confirm,
    /// Work on the task's pull request (orchestrator).
    Pr {
        #[command(subcommand)]
        action: PrAction,
    },
    /// Make a worker's tile large or small again (orchestrator for its workers, worker
    /// for itself).
    Tile {
        #[command(subcommand)]
        action: TileAction,
    },
    /// Make, list and wait for tasks (Start orchestrator).
    Task {
        #[command(subcommand)]
        action: TaskAction,
    },
    /// End your own session.
    Exit,
    /// Run and watch process terminals, such as a dev server or a test run.
    Terminal {
        #[command(subcommand)]
        action: TerminalAction,
    },
    /// Run the MCP server on stdin and stdout.
    Mcp,
    /// Forward an agent hook event (JSON on stdin) to Swarm.
    Hook,
    /// Claude Code's status line: send its rate limits (JSON on stdin) to Swarm, then
    /// run your own status line.
    Statusline,
}

#[derive(Subcommand, Debug)]
enum PrAction {
    /// Send the pull request title and description the `pr draft` message asks for. The
    /// user must have approved the task.
    Draft {
        /// One line that says what the change does.
        #[arg(long)]
        title: String,
        /// The description.
        #[arg(
            long,
            conflicts_with = "body_file",
            required_unless_present = "body_file"
        )]
        body: Option<String>,
        /// A file that holds the description; - reads stdin.
        #[arg(long, value_name = "PATH")]
        body_file: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
enum TileAction {
    /// Ask for a large tile. Swarm lays out the tiles and may not follow, for example when
    /// the user minimized the tile.
    Grow {
        /// A worker's agent ID or name (orchestrator). Leave it out for your own tile
        /// (worker).
        agent: Option<String>,
    },
    /// Ask for a small tile again.
    Shrink {
        /// A worker's agent ID or name (orchestrator). Leave it out for your own tile
        /// (worker).
        agent: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum TaskAction {
    /// Make a task and start its orchestrator with a brief.
    Create {
        /// The task's name; it becomes part of its branch.
        name: String,
        /// The branch to start from; the default is the base branch.
        #[arg(long)]
        from: Option<String>,
        /// The agent kind of the task's orchestrator; the default comes from the settings.
        #[arg(long, value_enum)]
        kind: Option<KindArg>,
        /// The goal, the scope and what done means; read from stdin when missing.
        #[arg(long)]
        brief: Option<String>,
    },
    /// List the tasks of the project.
    List,
    /// Wait until every task reached a state.
    Wait {
        /// The task IDs.
        #[arg(required = true, num_args = 1..)]
        ids: Vec<u64>,
        /// briefed: the orchestrators confirmed their brief; done: they finished.
        #[arg(long, value_parser = parse_until)]
        until: TaskUntil,
        /// Seconds to wait: 100 by default, 600 at most.
        #[arg(long)]
        timeout: Option<u32>,
    },
}

#[derive(Subcommand, Debug)]
enum TerminalAction {
    /// Start a command in a process terminal.
    Spawn(SpawnArguments),
    /// Show the size of a terminal you started. The tile sets the size, so the request
    /// changes nothing.
    Resize {
        /// The terminal ID.
        id: u64,
        /// Ignored: small, medium, large or a grid such as 100x30.
        #[arg(value_parser = parse_size)]
        size: TerminalSize,
    },
    /// Read the screen or the last lines of output.
    Read {
        /// The terminal ID.
        id: u64,
        /// How many lines; the default is the screen, 2000 at most.
        #[arg(long)]
        lines: Option<u32>,
    },
    /// Wait for the process to exit or for a line of output.
    Wait(WaitArguments),
    /// Stop the process.
    Kill {
        /// The terminal ID.
        id: u64,
        /// TERM (the default), INT, KILL or HUP.
        #[arg(long)]
        signal: Option<String>,
    },
    /// List the process terminals of your task.
    List,
}

#[derive(Args, Debug)]
struct SpawnArguments {
    /// A label, such as "dev server".
    #[arg(long)]
    name: Option<String>,
    /// Ignored: the terminal follows its tile. small, medium, large or a grid such as 100x30.
    #[arg(long, value_parser = parse_size)]
    size: Option<TerminalSize>,
    /// A folder inside your worktree; the default is the worktree.
    #[arg(long)]
    cwd: Option<std::path::PathBuf>,
    /// The command and its arguments. No shell is added: start `sh -c` yourself.
    #[arg(last = true, required = true, num_args = 1..)]
    command: Vec<String>,
}

#[derive(Args, Debug)]
struct WaitArguments {
    /// The terminal ID.
    id: u64,
    /// Wait for the process to exit.
    #[arg(long, conflicts_with = "output", required_unless_present = "output")]
    exit: bool,
    /// Wait for a line that contains this text.
    #[arg(long)]
    output: Option<String>,
    /// Seconds to wait: 100 by default, 600 at most.
    #[arg(long)]
    timeout: Option<u32>,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum KindArg {
    Claude,
    Codex,
}

impl KindArg {
    fn kind(self) -> swarm_protocol::AgentKind {
        match self {
            Self::Claude => swarm_protocol::AgentKind::ClaudeCode,
            Self::Codex => swarm_protocol::AgentKind::Codex,
        }
    }
}

impl Action {
    /// The command to run, with texts read from `stdin` where they ask for it. `None`
    /// for `mcp`, `hook` and `statusline`, which are not commands.
    fn command(self, stdin: &mut impl std::io::Read) -> Result<Option<Command>, String> {
        let command = match self {
            Self::Spawn {
                name,
                kind,
                model,
                brief,
                own_worktree,
            } => {
                let brief = match brief {
                    Some(brief) => brief,
                    None => read_all(stdin)?,
                };
                if brief.trim().is_empty() {
                    return Err(
                        "a worker needs a brief: use --brief or write it to stdin".to_owned()
                    );
                }

                Command::Spawn {
                    name,
                    kind: kind.map(KindArg::kind),
                    model,
                    brief,
                    own_worktree,
                }
            }
            Self::Commit { message, paths } => Command::Commit { message, paths },
            Self::Send { agent, text } => Command::Send {
                to: agent,
                text: text_or_stdin(&text, stdin)?,
            },
            Self::Ask { text } => Command::Ask {
                text: text_or_stdin(&text, stdin)?,
            },
            Self::Report { text } => Command::Report {
                text: text_or_stdin(&text, stdin)?,
            },
            Self::Notify { text } => Command::Notify {
                text: text_or_stdin(&text, stdin)?,
            },
            Self::Read => Command::Read,
            Self::Wait { timeout } => Command::Wait {
                timeout_secs: timeout,
            },
            Self::Merge { worker } => Command::Merge { worker },
            Self::Agents => Command::Agents,
            Self::Confirm => Command::Confirm,
            Self::Pr { action } => action.command(stdin)?,
            Self::Tile { action } => action.command(),
            Self::Task { action } => action.command(stdin)?,
            Self::Exit => Command::Exit,
            Self::Terminal { action } => action.command(),
            Self::Mcp | Self::Hook | Self::Statusline => return Ok(None),
        };

        Ok(Some(command))
    }
}

impl PrAction {
    fn command(self, stdin: &mut impl std::io::Read) -> Result<Command, String> {
        let Self::Draft {
            title,
            body,
            body_file,
        } = self;

        let body = match (body, body_file) {
            (Some(body), _) => body,
            (None, Some(path)) if path.as_os_str() == "-" => read_all(stdin)?,
            (None, Some(path)) => {
                let mut file = std::fs::File::open(&path)
                    .map_err(|error| format!("cannot read {}: {error}", path.display()))?;

                read_all(&mut file)?
            }
            (None, None) => {
                return Err("give the description with --body or --body-file".to_owned());
            }
        };

        Ok(Command::PrDraft { title, body })
    }
}

impl TileAction {
    fn command(self) -> Command {
        match self {
            Self::Grow { agent } => Command::TileGrow { agent },
            Self::Shrink { agent } => Command::TileShrink { agent },
        }
    }
}

impl TaskAction {
    fn command(self, stdin: &mut impl std::io::Read) -> Result<Command, String> {
        Ok(match self {
            Self::Create {
                name,
                from,
                kind,
                brief,
            } => {
                let brief = match brief {
                    Some(brief) => brief,
                    None => read_all(stdin)?,
                };
                if brief.trim().is_empty() {
                    return Err("a task needs a brief: use --brief or write it to stdin".to_owned());
                }

                Command::TaskCreate {
                    name,
                    from,
                    brief,
                    kind: kind.map(KindArg::kind),
                }
            }
            Self::List => Command::TaskList,
            Self::Wait {
                ids,
                until,
                timeout,
            } => Command::TaskWait {
                ids: ids.into_iter().map(TaskId).collect(),
                until,
                timeout_secs: timeout,
            },
        })
    }
}

impl TerminalAction {
    fn command(self) -> Command {
        match self {
            Self::Spawn(SpawnArguments {
                name,
                size,
                cwd,
                command,
            }) => Command::TerminalSpawn {
                command,
                cwd,
                size,
                name,
            },
            Self::Resize { id, size } => Command::TerminalResize {
                id: TerminalId(id),
                size,
            },
            Self::Read { id, lines } => Command::TerminalRead {
                id: TerminalId(id),
                lines,
            },
            Self::Wait(WaitArguments {
                id,
                output,
                timeout,
                ..
            }) => Command::TerminalWait {
                id: TerminalId(id),
                until: output.map_or(TerminalUntil::Exit, TerminalUntil::Output),
                timeout_secs: timeout,
            },
            Self::Kill { id, signal } => Command::TerminalKill {
                id: TerminalId(id),
                signal,
            },
            Self::List => Command::TerminalList,
        }
    }
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().collect();

    // A hook must never fail, whatever it is given.
    if arguments.get(1).map(String::as_str) == Some("hook") {
        hook::run();

        return ExitCode::SUCCESS;
    }

    // The same for the status line: it must print only the user's own line.
    if arguments.get(1).map(String::as_str) == Some("statusline") {
        statusline::run();

        return ExitCode::SUCCESS;
    }

    let cli = Cli::parse();
    if matches!(cli.command, Action::Mcp) {
        return mcp::run();
    }

    run(cli)
}

fn run(cli: Cli) -> ExitCode {
    let json = cli.json;

    let command = match cli.command.command(&mut std::io::stdin()) {
        Ok(Some(command)) => command,
        Ok(None) => return ExitCode::SUCCESS,
        Err(message) => return fail(&message),
    };
    let session = match Session::from_env() {
        Ok(session) => session,
        Err(message) => return fail(&message),
    };

    match command.run(&session) {
        Ok(output) => print_output(&output, json),
        Err(message) => fail(&message),
    }
}

fn print_output(output: &Output, json: bool) -> ExitCode {
    let line = if json {
        output.json.to_string()
    } else {
        output.text.clone()
    };

    if writeln!(std::io::stdout(), "{line}").is_err() {
        return ExitCode::FAILURE;
    }

    if output.clean {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn fail(message: &str) -> ExitCode {
    writeln!(std::io::stderr(), "swarm: {message}").ok();

    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use swarm_protocol::AgentKind;

    use super::*;
    use crate::commands::parse_size;

    fn parse(arguments: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("swarm").chain(arguments.iter().copied()))
    }

    fn command(arguments: &[&str], stdin: &str) -> Command {
        parse(arguments)
            .expect("parse")
            .command
            .command(&mut stdin.as_bytes())
            .expect("command")
            .expect("a command")
    }

    #[test]
    fn spawn_takes_a_name_kind_model_and_brief() {
        let command = command(
            &[
                "spawn", "api", "--kind", "codex", "--model", "gpt-6", "--brief", "do it",
            ],
            "",
        );

        assert_eq!(
            command,
            Command::Spawn {
                name: "api".to_owned(),
                kind: Some(AgentKind::Codex),
                model: Some("gpt-6".to_owned()),
                brief: "do it".to_owned(),
                own_worktree: None,
            }
        );
    }

    #[test]
    fn spawn_takes_a_reason_for_its_own_worktree() {
        let command = command(
            &[
                "spawn",
                "api",
                "--brief",
                "do it",
                "--own-worktree",
                "same files as ui",
            ],
            "",
        );

        assert_eq!(
            command,
            Command::Spawn {
                name: "api".to_owned(),
                kind: None,
                model: None,
                brief: "do it".to_owned(),
                own_worktree: Some("same files as ui".to_owned()),
            }
        );
    }

    #[test]
    fn spawn_needs_a_reason_after_own_worktree() {
        assert!(parse(&["spawn", "api", "--brief", "x", "--own-worktree"]).is_err());
    }

    #[test]
    fn commit_takes_a_message_and_paths() {
        let command = command(
            &["commit", "-m", "feat: api", "src/api.rs", "src/lib.rs"],
            "",
        );

        assert_eq!(
            command,
            Command::Commit {
                message: "feat: api".to_owned(),
                paths: vec![
                    std::path::PathBuf::from("src/api.rs"),
                    std::path::PathBuf::from("src/lib.rs"),
                ],
            }
        );
    }

    #[test]
    fn commit_needs_a_message_and_at_least_one_path() {
        assert!(parse(&["commit", "src/api.rs"]).is_err());
        assert!(parse(&["commit", "-m", "feat: api"]).is_err());
    }

    #[test]
    fn spawn_reads_the_brief_from_stdin_without_brief() {
        let command = command(&["spawn", "api"], "line one\nline two\n");

        assert_eq!(
            command,
            Command::Spawn {
                name: "api".to_owned(),
                kind: None,
                model: None,
                brief: "line one\nline two\n".to_owned(),
                own_worktree: None,
            }
        );
    }

    #[test]
    fn spawn_without_any_brief_is_an_error() {
        let result = parse(&["spawn", "api"])
            .expect("parse")
            .command
            .command(&mut "  \n".as_bytes());

        assert!(result.is_err());
    }

    #[test]
    fn send_joins_the_words_after_the_agent() {
        let command = command(&["send", "api", "use", "port", "5173"], "");

        assert_eq!(
            command,
            Command::Send {
                to: "api".to_owned(),
                text: "use port 5173".to_owned(),
            }
        );
    }

    #[test]
    fn a_dash_reads_the_text_from_stdin() {
        let command = command(&["report", "-"], "all done\n");

        assert_eq!(
            command,
            Command::Report {
                text: "all done".to_owned()
            }
        );
    }

    #[test]
    fn wait_takes_a_timeout() {
        assert_eq!(
            command(&["wait", "--timeout", "30"], ""),
            Command::Wait {
                timeout_secs: Some(30)
            }
        );
        assert_eq!(command(&["wait"], ""), Command::Wait { timeout_secs: None });
    }

    #[test]
    fn the_simple_commands_parse() {
        assert_eq!(command(&["read"], ""), Command::Read);
        assert_eq!(command(&["agents"], ""), Command::Agents);
        assert_eq!(command(&["confirm"], ""), Command::Confirm);
        assert_eq!(
            command(&["merge", "api"], ""),
            Command::Merge {
                worker: "api".to_owned()
            }
        );
        assert_eq!(
            command(&["notify", "look", "here"], ""),
            Command::Notify {
                text: "look here".to_owned()
            }
        );
        assert_eq!(
            command(&["ask", "which", "port?"], ""),
            Command::Ask {
                text: "which port?".to_owned()
            }
        );
    }

    #[test]
    fn tile_grow_and_shrink_take_an_optional_agent() {
        assert_eq!(
            command(&["tile", "grow", "api"], ""),
            Command::TileGrow {
                agent: Some("api".to_owned())
            }
        );
        assert_eq!(
            command(&["tile", "grow", "7"], ""),
            Command::TileGrow {
                agent: Some("7".to_owned())
            }
        );
        assert_eq!(
            command(&["tile", "grow"], ""),
            Command::TileGrow { agent: None }
        );
        assert_eq!(
            command(&["tile", "shrink", "api"], ""),
            Command::TileShrink {
                agent: Some("api".to_owned())
            }
        );
        assert_eq!(
            command(&["tile", "shrink"], ""),
            Command::TileShrink { agent: None }
        );
    }

    #[test]
    fn tile_needs_grow_or_shrink_and_at_most_one_agent() {
        assert!(parse(&["tile"]).is_err());
        assert!(parse(&["tile", "enlarge"]).is_err());
        assert!(parse(&["tile", "grow", "api", "ui"]).is_err());
    }

    #[test]
    fn task_create_takes_a_name_branch_kind_and_brief() {
        let command = command(
            &[
                "task", "create", "login", "--from", "develop", "--kind", "codex", "--brief",
                "build it",
            ],
            "",
        );

        assert_eq!(
            command,
            Command::TaskCreate {
                name: "login".to_owned(),
                from: Some("develop".to_owned()),
                brief: "build it".to_owned(),
                kind: Some(AgentKind::Codex),
            }
        );
        assert_eq!(
            self::command(&["task", "create", "x"], "from stdin\n"),
            Command::TaskCreate {
                name: "x".to_owned(),
                from: None,
                brief: "from stdin\n".to_owned(),
                kind: None,
            }
        );
    }

    #[test]
    fn task_wait_takes_ids_a_state_and_a_timeout() {
        assert_eq!(
            command(
                &[
                    "task",
                    "wait",
                    "3",
                    "4",
                    "--until",
                    "briefed",
                    "--timeout",
                    "9"
                ],
                ""
            ),
            Command::TaskWait {
                ids: vec![TaskId(3), TaskId(4)],
                until: TaskUntil::Briefed,
                timeout_secs: Some(9),
            }
        );
        assert!(parse(&["task", "wait", "3"]).is_err());
        assert!(parse(&["task", "wait", "3", "--until", "later"]).is_err());
        assert_eq!(command(&["task", "list"], ""), Command::TaskList);
        assert_eq!(command(&["exit"], ""), Command::Exit);
    }

    #[test]
    fn terminal_spawn_takes_everything_after_the_dashes_as_the_command() {
        let command = command(
            &[
                "terminal", "spawn", "--name", "dev", "--size", "100x30", "--cwd", "web", "--",
                "pnpm", "dev", "--port", "5173",
            ],
            "",
        );

        assert_eq!(
            command,
            Command::TerminalSpawn {
                command: ["pnpm", "dev", "--port", "5173"]
                    .map(str::to_owned)
                    .to_vec(),
                cwd: Some("web".into()),
                size: Some(TerminalSize::Grid(swarm_protocol::GridSize {
                    cols: 100,
                    rows: 30
                })),
                name: Some("dev".to_owned()),
            }
        );
        assert!(parse(&["terminal", "spawn"]).is_err());
    }

    #[test]
    fn terminal_sizes_have_names_or_a_grid() {
        assert_eq!(parse_size("small"), Ok(TerminalSize::Small));
        assert_eq!(parse_size("large"), Ok(TerminalSize::Large));
        assert!(parse_size("huge").is_err());
        assert!(parse_size("80x").is_err());
    }

    #[test]
    fn terminal_wait_needs_exactly_one_condition() {
        assert_eq!(
            command(&["terminal", "wait", "7", "--exit"], ""),
            Command::TerminalWait {
                id: TerminalId(7),
                until: TerminalUntil::Exit,
                timeout_secs: None,
            }
        );
        assert_eq!(
            command(
                &[
                    "terminal",
                    "wait",
                    "7",
                    "--output",
                    "ready in",
                    "--timeout",
                    "5"
                ],
                ""
            ),
            Command::TerminalWait {
                id: TerminalId(7),
                until: TerminalUntil::Output("ready in".to_owned()),
                timeout_secs: Some(5),
            }
        );
        assert!(parse(&["terminal", "wait", "7"]).is_err());
        assert!(parse(&["terminal", "wait", "7", "--exit", "--output", "x"]).is_err());
    }

    #[test]
    fn the_other_terminal_commands_parse() {
        assert_eq!(
            command(&["terminal", "read", "7", "--lines", "20"], ""),
            Command::TerminalRead {
                id: TerminalId(7),
                lines: Some(20)
            }
        );
        assert_eq!(
            command(&["terminal", "kill", "7", "--signal", "KILL"], ""),
            Command::TerminalKill {
                id: TerminalId(7),
                signal: Some("KILL".to_owned())
            }
        );
        assert_eq!(
            command(&["terminal", "resize", "7", "large"], ""),
            Command::TerminalResize {
                id: TerminalId(7),
                size: TerminalSize::Large
            }
        );
        assert_eq!(command(&["terminal", "list"], ""), Command::TerminalList);
    }

    #[test]
    fn pr_draft_takes_a_title_and_a_body() {
        assert_eq!(
            command(
                &["pr", "draft", "--title", "Add login", "--body", "## What"],
                ""
            ),
            Command::PrDraft {
                title: "Add login".to_owned(),
                body: "## What".to_owned(),
            }
        );
    }

    #[test]
    fn pr_draft_reads_the_body_from_stdin_with_a_dash() {
        assert_eq!(
            command(
                &["pr", "draft", "--title", "T", "--body-file", "-"],
                "line one\nline two\n"
            ),
            Command::PrDraft {
                title: "T".to_owned(),
                body: "line one\nline two\n".to_owned(),
            }
        );
    }

    #[test]
    fn pr_draft_reads_the_body_from_a_file() {
        let path = std::env::temp_dir().join(format!("swarm-cli-body-{}.md", std::process::id()));
        std::fs::write(&path, "from a file\n").expect("file");

        let command = command(
            &[
                "pr",
                "draft",
                "--title",
                "T",
                "--body-file",
                path.to_str().expect("utf8"),
            ],
            "",
        );
        std::fs::remove_file(&path).expect("cleanup");

        assert_eq!(
            command,
            Command::PrDraft {
                title: "T".to_owned(),
                body: "from a file\n".to_owned(),
            }
        );
    }

    #[test]
    fn pr_draft_needs_a_title_and_exactly_one_body_source() {
        assert!(parse(&["pr", "draft", "--body", "x"]).is_err());
        assert!(parse(&["pr", "draft", "--title", "T"]).is_err());
        assert!(
            parse(&[
                "pr",
                "draft",
                "--title",
                "T",
                "--body",
                "x",
                "--body-file",
                "-"
            ])
            .is_err()
        );
        assert!(parse(&["pr"]).is_err());
    }

    #[test]
    fn pr_draft_with_a_missing_file_says_which_file() {
        let action = parse(&[
            "pr",
            "draft",
            "--title",
            "T",
            "--body-file",
            "/nonexistent/body.md",
        ])
        .expect("parse")
        .command;

        let error = action
            .command(&mut std::io::empty())
            .expect_err("no such file");

        assert!(error.contains("/nonexistent/body.md"), "{error}");
    }

    #[test]
    fn mcp_hook_and_statusline_are_not_commands() {
        for name in ["mcp", "hook", "statusline"] {
            let action = parse(&[name]).expect("parse").command;

            assert!(action.command(&mut std::io::empty()).expect("ok").is_none());
        }
    }

    #[test]
    fn usage_errors_are_reported_by_clap() {
        assert!(parse(&["send", "api"]).is_err());
        assert!(parse(&["spawn"]).is_err());
        assert!(parse(&["spawn", "api", "--kind", "gpt"]).is_err());
        assert!(parse(&["wait", "--timeout", "soon"]).is_err());
        assert!(parse(&["nothing"]).is_err());
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn json_works_before_and_after_the_command() {
        assert!(parse(&["--json", "read"]).expect("parse").json);
        assert!(parse(&["read", "--json"]).expect("parse").json);
        assert!(!parse(&["read"]).expect("parse").json);
    }
}
