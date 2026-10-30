//! `swarm`: the command line that agents use to talk to Swarm (ADR 0008).
//!
//! It speaks one JSON object per line to the daemon's agent socket. No async, no
//! terminal socket: the daemon refuses agents on that one.

mod commands;
mod hook;
mod mcp;
mod session;

use std::io::Write as _;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

use crate::commands::{Command, Output, read_all, text_or_stdin};
use crate::session::Session;

#[derive(Parser, Debug)]
#[command(
    name = "swarm",
    version,
    about = "Talk to Swarm from an agent terminal",
    long_about = "Talk to Swarm from an agent terminal.\n\nWhich commands work depends on your role: orchestrators spawn, send, merge, notify, agents and confirm; workers ask and report; every agent can read and wait.\nA text of - is read from stdin."
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
    /// Start a worker in its own worktree (orchestrator).
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
        /// Seconds to wait: 120 by default, 600 at most.
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
    /// Run the MCP server on stdin and stdout.
    Mcp,
    /// Forward an agent hook event (JSON on stdin) to Swarm.
    Hook,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum KindArg {
    Claude,
    Codex,
}

impl Action {
    /// The command to run, with texts read from `stdin` where they ask for it. `None`
    /// for `mcp` and `hook`, which are not commands.
    fn command(self, stdin: &mut impl std::io::Read) -> Result<Option<Command>, String> {
        let command = match self {
            Self::Spawn {
                name,
                kind,
                model,
                brief,
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
                    kind: kind.map(|kind| match kind {
                        KindArg::Claude => swarm_protocol::AgentKind::ClaudeCode,
                        KindArg::Codex => swarm_protocol::AgentKind::Codex,
                    }),
                    model,
                    brief,
                }
            }
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
            Self::Mcp | Self::Hook => return Ok(None),
        };

        Ok(Some(command))
    }
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().collect();

    // A hook must never fail, whatever it is given.
    if arguments.get(1).map(String::as_str) == Some("hook") {
        hook::run();

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
            }
        );
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
    fn mcp_and_hook_are_not_commands() {
        for name in ["mcp", "hook"] {
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
