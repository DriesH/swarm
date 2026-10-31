//! The text Swarm appends to an agent's own instructions (ADR 0004).
//!
//! A fixed harness block always comes first. The role text comes from the last of
//! three layers that has the file: built in, then `~/.config/swarm/prompts/`, then
//! `.swarm/prompts/` in the project.

use std::io;
use std::path::Path;

use swarm_protocol::{AgentId, AgentKind, AgentRole, TaskId};

const ORCHESTRATOR: &str = include_str!("../prompts/orchestrator.md");
const WORKER: &str = include_str!("../prompts/worker.md");
const START: &str = include_str!("../prompts/start.md");
const PR: &str = include_str!("../prompts/pr.md");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptFile {
    Orchestrator,
    Worker,
    Start,
    /// The pull request description, sent to the orchestrator inside a `pr draft` message.
    Pr,
}

impl PromptFile {
    pub fn for_role(role: AgentRole) -> Self {
        match role {
            AgentRole::Orchestrator => Self::Orchestrator,
            AgentRole::Worker => Self::Worker,
            AgentRole::StartOrchestrator => Self::Start,
        }
    }

    pub fn file_name(self) -> &'static str {
        match self {
            Self::Orchestrator => "orchestrator.md",
            Self::Worker => "worker.md",
            Self::Start => "start.md",
            Self::Pr => "pr.md",
        }
    }

    fn built_in(self) -> &'static str {
        match self {
            Self::Orchestrator => ORCHESTRATOR,
            Self::Worker => WORKER,
            Self::Start => START,
            Self::Pr => PR,
        }
    }
}

/// What the harness block says about the agent.
#[derive(Debug, Clone, Copy)]
pub struct Harness<'a> {
    pub agent: AgentId,
    /// `None` for a Start orchestrator.
    pub task: Option<TaskId>,
    pub task_name: &'a str,
    pub role: AgentRole,
    pub kind: AgentKind,
    pub skip_permissions: bool,
    pub socket: &'a Path,
}

/// One line per command: the command, then what it does.
const ORCHESTRATOR_COMMANDS: &[&str] = &[
    "`swarm spawn <name> [--kind claude|codex] [--model M] [--own-worktree REASON] [--brief TEXT]`: start a worker in the task worktree, or in its own worktree when you give a reason (the brief comes from stdin without `--brief`)",
    "`swarm send <worker> <text>`: send a message to one of your workers",
    "`swarm commit -m <message> <path>...`: commit only the named files in your worktree",
    "`swarm read`: take your unread messages",
    "`swarm wait [--timeout SECS]`: wait for a message (default 100, at most 600)",
    "`swarm merge <worker>`: merge the branch of a worker that has its own worktree into the task branch",
    "`swarm agents`: list your workers",
    "`swarm notify <text>`: ask the user for attention",
    "`swarm confirm`: say that you understood your brief",
    "`swarm pr draft --title <title> (--body <text> | --body-file <path|->)`: send the pull request title and description the `pr draft` message asks for (only after the user approved the task)",
];

/// Every role can run process terminals (dev servers, tests) in its worktree.
const TERMINAL_COMMANDS: &[&str] = &[
    "`swarm terminal spawn [--name N] [--size small|medium|large|COLSxROWS] [--cwd DIR] -- <command>...`: run a command in a process terminal; no shell is added, so pass `sh -c` yourself when you need one",
    "`swarm terminal list`: list the process terminals of your task",
    "`swarm terminal read <id> [--lines N]`: read the screen or the last lines of output (at most 2000)",
    "`swarm terminal wait <id> (--exit | --output TEXT) [--timeout SECS]`: wait for the process to exit or for a line that contains TEXT",
    "`swarm terminal resize <id> <size>`: resize a terminal you started",
    "`swarm terminal kill <id> [--signal TERM|INT|KILL|HUP]`: stop a process (the terminal's owner or your orchestrator)",
];

const WORKER_COMMANDS: &[&str] = &[
    "`swarm ask <text>`: ask your orchestrator a question; the answer arrives as a message",
    "`swarm report <text>`: send your orchestrator a progress or final report",
    "`swarm commit -m <message> <path>...`: commit only the named files in your worktree, as yourself",
    "`swarm read`: take your unread messages",
    "`swarm wait [--timeout SECS]`: wait for a message (default 100, at most 600)",
];

const START_COMMANDS: &[&str] = &[
    "`swarm task create <name> [--from BRANCH] [--kind claude|codex] [--brief TEXT]`: make a task and start its orchestrator with the brief (the brief comes from stdin without `--brief`)",
    "`swarm task list`: list the tasks of the project",
    "`swarm task wait <id>... --until briefed|done [--timeout SECS]`: wait until the task orchestrators confirmed their brief or finished (default 100, at most 600)",
    "`swarm exit`: end your own session",
    "`swarm read`: take your unread messages",
    "`swarm wait [--timeout SECS]`: wait for a message (default 100, at most 600)",
];

/// Claude Code's Bash tool stops a command after 120 s unless told otherwise.
const WAIT_NOTE: &str = "The waiting commands (`swarm wait`, `swarm task wait`, `swarm terminal wait`) \
     end after `--timeout` seconds. When you wait longer than 100 seconds in a shell, give the \
     Bash tool a timeout longer than `--timeout`.";

impl Harness<'_> {
    fn block(&self) -> String {
        let (role, commands) = match self.role {
            AgentRole::Orchestrator => ("orchestrator", ORCHESTRATOR_COMMANDS),
            AgentRole::Worker => ("worker", WORKER_COMMANDS),
            AgentRole::StartOrchestrator => ("start orchestrator", START_COMMANDS),
        };
        let task = self.task.map_or_else(
            || "none".to_owned(),
            |task| format!("{} ({})", task.0, self.task_name),
        );
        let commands = commands
            .iter()
            .chain(TERMINAL_COMMANDS)
            .map(|command| format!("- {command}"))
            .collect::<Vec<_>>()
            .join("\n");

        let access = if self.kind == AgentKind::Codex && !self.skip_permissions {
            "Your shell cannot reach Swarm inside the Codex sandbox. Use the tools of the `swarm` \
             MCP server. A tool is named like its command, with `_` between the words: \
             `swarm terminal spawn` is `terminal_spawn`, and `swarm exit` is `agent_exit`."
        } else {
            "Run these commands in your shell, or call the tools of the `swarm` MCP server. A tool \
             is named like its command, with `_` between the words: `swarm terminal spawn` is \
             `terminal_spawn`, and `swarm exit` is `agent_exit`."
        };

        format!(
            "# Swarm harness\n\n\
             - Agent: {agent}\n\
             - Task: {task}\n\
             - Role: {role}\n\
             - SWARM_SOCKET: {socket}\n\n\
             {access}\n\n\
             {commands}\n\n\
             {WAIT_NOTE}\n\n\
             Run `swarm --help` for the details of each command.",
            agent = self.agent.0,
            socket = self.socket.display(),
        )
    }
}

/// The harness block, then the role text.
///
/// `config_dir` is `~/.config/swarm`; `project_root` holds `.swarm/prompts`.
pub fn build(
    file: PromptFile,
    harness: &Harness<'_>,
    config_dir: Option<&Path>,
    project_root: &Path,
) -> String {
    format!(
        "{}\n\n{}\n",
        harness.block(),
        layered(file, config_dir, project_root)
    )
}

/// The text of a prompt file: the last layer that has it, from the built-in text, then
/// `<config_dir>/prompts/`, then `<project_root>/.swarm/prompts/`.
pub fn layered(file: PromptFile, config_dir: Option<&Path>, project_root: &Path) -> String {
    let mut body = file.built_in().trim().to_owned();

    let layers = [
        config_dir.map(|dir| dir.join("prompts").join(file.file_name())),
        Some(project_root.join(".swarm/prompts").join(file.file_name())),
    ];
    for path in layers.into_iter().flatten() {
        match std::fs::read_to_string(&path) {
            Ok(text) if !text.trim().is_empty() => text.trim().clone_into(&mut body),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => log::warn!("cannot read the prompt {}: {error}", path.display()),
        }
    }

    body
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    struct Folders {
        root: PathBuf,
    }

    impl Folders {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("swarm-prompts-{}-{name}", std::process::id()));
            std::fs::create_dir_all(&root).expect("folder");

            Self { root }
        }

        fn config(&self) -> PathBuf {
            self.root.join("config")
        }

        fn project(&self) -> PathBuf {
            self.root.join("project")
        }

        fn write(folder: &Path, file: &str, text: &str) {
            let prompts = folder.join("prompts");
            std::fs::create_dir_all(&prompts).expect("folder");
            std::fs::write(prompts.join(file), text).expect("file");
        }
    }

    impl Drop for Folders {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    fn harness(socket: &Path) -> Harness<'_> {
        Harness {
            agent: AgentId(7),
            task: Some(TaskId(3)),
            task_name: "Fix it",
            role: AgentRole::Orchestrator,
            kind: AgentKind::ClaudeCode,
            skip_permissions: false,
            socket,
        }
    }

    #[test]
    fn the_harness_block_comes_first_and_names_the_agent() {
        let folders = Folders::new("harness");
        let socket = Path::new("/data/agent.sock");

        let prompt = build(
            PromptFile::Orchestrator,
            &harness(socket),
            Some(&folders.config()),
            &folders.project(),
        );

        assert!(prompt.starts_with("# Swarm harness"));
        assert!(prompt.contains("Agent: 7"));
        assert!(prompt.contains("Task: 3 (Fix it)"));
        assert!(prompt.contains("Role: orchestrator"));
        assert!(prompt.contains("SWARM_SOCKET: /data/agent.sock"));
        assert!(prompt.contains("swarm --help"));
    }

    #[test]
    fn the_built_in_text_follows_the_harness_block() {
        let folders = Folders::new("builtin");

        let prompt = build(
            PromptFile::Worker,
            &harness(Path::new("/s")),
            None,
            &folders.project(),
        );

        let harness_end = prompt.find("Run `swarm --help`").expect("harness");
        let role_text = prompt.find("You are a worker").expect("worker text");
        assert!(role_text > harness_end);
    }

    #[test]
    fn the_user_layer_replaces_the_built_in_text() {
        let folders = Folders::new("user");
        Folders::write(&folders.config(), "worker.md", "User worker text");

        let prompt = build(
            PromptFile::Worker,
            &harness(Path::new("/s")),
            Some(&folders.config()),
            &folders.project(),
        );

        assert!(prompt.contains("User worker text"));
        assert!(!prompt.contains("You are a worker"));
    }

    #[test]
    fn the_project_layer_wins_over_the_user_layer() {
        let folders = Folders::new("project");
        Folders::write(&folders.config(), "worker.md", "User worker text");
        let project_prompts = folders.project().join(".swarm");
        Folders::write(&project_prompts, "worker.md", "Project worker text");

        let prompt = build(
            PromptFile::Worker,
            &harness(Path::new("/s")),
            Some(&folders.config()),
            &folders.project(),
        );

        assert!(prompt.contains("Project worker text"));
        assert!(!prompt.contains("User worker text"));
    }

    #[test]
    fn a_layer_replaces_only_its_own_file() {
        let folders = Folders::new("files");
        Folders::write(&folders.config(), "worker.md", "User worker text");

        let prompt = build(
            PromptFile::Orchestrator,
            &harness(Path::new("/s")),
            Some(&folders.config()),
            &folders.project(),
        );

        assert!(prompt.contains("You are the orchestrator"));
    }

    #[test]
    fn an_empty_layer_file_keeps_the_lower_layer() {
        let folders = Folders::new("empty");
        Folders::write(&folders.config(), "worker.md", "  \n");

        let prompt = build(
            PromptFile::Worker,
            &harness(Path::new("/s")),
            Some(&folders.config()),
            &folders.project(),
        );

        assert!(prompt.contains("You are a worker"));
    }

    #[test]
    fn a_layer_never_removes_the_harness_block() {
        let folders = Folders::new("block");
        Folders::write(&folders.config(), "worker.md", "User worker text");

        let prompt = build(
            PromptFile::Worker,
            &harness(Path::new("/s")),
            Some(&folders.config()),
            &folders.project(),
        );

        assert!(prompt.starts_with("# Swarm harness"));
    }
    #[test]
    fn each_role_lists_only_its_commands() {
        let block = |role| {
            Harness {
                role,
                ..harness(Path::new("/s"))
            }
            .block()
        };

        let orchestrator = block(AgentRole::Orchestrator);
        let worker = block(AgentRole::Worker);
        let start = Harness {
            role: AgentRole::StartOrchestrator,
            task: None,
            ..harness(Path::new("/s"))
        }
        .block();

        assert!(orchestrator.contains("- `swarm spawn "));
        assert!(orchestrator.contains("- `swarm merge <worker>`"));
        assert!(orchestrator.contains("[--own-worktree REASON]"));
        assert!(orchestrator.contains("- `swarm commit -m <message> <path>...`"));
        assert!(!orchestrator.contains("swarm ask"));
        assert!(worker.contains("- `swarm ask <text>`"));
        assert!(worker.contains("- `swarm report <text>`"));
        assert!(worker.contains("- `swarm commit -m <message> <path>...`"));
        assert!(!worker.contains("swarm spawn"));
        assert!(!worker.contains("swarm send"));
        assert!(start.contains("- `swarm wait "));
        assert!(!start.contains("swarm spawn"));
        assert!(!start.contains("swarm commit"));
        assert!(start.contains("Task: none"));
    }

    #[test]
    fn the_start_orchestrator_gets_the_task_commands_and_the_others_do_not() {
        let block = |role| {
            Harness {
                role,
                ..harness(Path::new("/s"))
            }
            .block()
        };

        let start = block(AgentRole::StartOrchestrator);

        assert!(start.contains("- `swarm task create "));
        assert!(start.contains("- `swarm task wait "));
        assert!(start.contains("- `swarm task list`"));
        assert!(start.contains("- `swarm exit`"));
        assert!(!block(AgentRole::Orchestrator).contains("swarm task create"));
        assert!(!block(AgentRole::Worker).contains("swarm task create"));
    }

    #[test]
    fn every_role_is_told_to_give_the_bash_tool_a_longer_timeout() {
        for role in [
            AgentRole::Orchestrator,
            AgentRole::Worker,
            AgentRole::StartOrchestrator,
        ] {
            let block = Harness {
                role,
                ..harness(Path::new("/s"))
            }
            .block();

            assert!(
                block.contains("timeout longer than `--timeout`"),
                "{role:?}"
            );
            assert!(block.contains("(default 100, at most 600)"), "{role:?}");
        }
    }

    #[test]
    fn the_start_orchestrator_agrees_the_features_with_the_user_first() {
        assert!(START.contains("Count the features"));
        assert!(START.contains("ask the user to confirm"));
        assert!(START.contains("exactly one task for each agreed feature"));
        assert!(START.contains("never split one feature over tasks"));
    }

    #[test]
    fn the_orchestrator_shares_the_task_worktree_unless_it_has_a_reason() {
        assert!(ORCHESTRATOR.contains("share the task worktree"));
        assert!(ORCHESTRATOR.contains("--own-worktree \"<reason>\""));
        assert!(ORCHESTRATOR.contains("only for a worker that has its own worktree"));
    }

    #[test]
    fn the_worker_commits_only_its_own_files() {
        assert!(WORKER.contains("swarm commit -m"));
        assert!(WORKER.contains("`commit` MCP tool"));
        for forbidden in [
            "git add -A",
            "git commit -a",
            "git reset",
            "git checkout",
            "git stash",
            "git clean",
        ] {
            assert!(WORKER.contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn the_start_orchestrator_waits_again_after_a_timeout() {
        assert!(START.contains("`timed out`"));
        assert!(START.contains("run it again"));
    }

    #[test]
    fn the_orchestrator_knows_what_to_do_with_every_swarm_message() {
        for kind in [
            "user input",
            "terminal exited",
            "worker stopped",
            "comment",
            "pr draft",
        ] {
            assert!(ORCHESTRATOR.contains(&format!("`{kind}`")), "{kind}");
        }
    }

    #[test]
    fn the_orchestrator_drafts_the_pull_request_from_the_diff_and_sends_it_back() {
        assert!(ORCHESTRATOR.contains("git diff <base>...HEAD"));
        assert!(ORCHESTRATOR.contains("swarm pr draft --title"));
        assert!(ORCHESTRATOR.contains("local checks"));
        assert!(ORCHESTRATOR.contains("before you report the task done"));
    }

    #[test]
    fn the_orchestrator_harness_lists_pr_draft_and_the_others_do_not() {
        let block = |role| {
            Harness {
                role,
                ..harness(Path::new("/s"))
            }
            .block()
        };

        assert!(block(AgentRole::Orchestrator).contains("- `swarm pr draft --title <title>"));
        assert!(!block(AgentRole::Worker).contains("swarm pr draft"));
        assert!(!block(AgentRole::StartOrchestrator).contains("swarm pr draft"));
    }

    #[test]
    fn the_pr_prompt_has_the_four_sections_and_no_attribution() {
        for heading in [
            "## What",
            "## Why",
            "## Notes for reviewers",
            "## Open tasks",
        ] {
            assert!(PR.contains(heading), "{heading}");
        }
        assert!(PR.contains("- [ ]"));
        assert!(PR.contains("Do not add attribution lines"));
        assert!(!PR.contains("Co-Authored"));
    }

    #[test]
    fn the_pr_prompt_is_layered_like_the_others() {
        let folders = Folders::new("pr");

        let built_in = layered(PromptFile::Pr, Some(&folders.config()), &folders.project());
        Folders::write(&folders.config(), "pr.md", "User PR text");
        let user = layered(PromptFile::Pr, Some(&folders.config()), &folders.project());
        let project_prompts = folders.project().join(".swarm");
        Folders::write(&project_prompts, "pr.md", "Project PR text");
        let project = layered(PromptFile::Pr, Some(&folders.config()), &folders.project());

        assert!(built_in.contains("## What"));
        assert_eq!(user, "User PR text");
        assert_eq!(project, "Project PR text");
        assert_eq!(PromptFile::Pr.file_name(), "pr.md");
    }

    #[test]
    fn every_role_gets_the_terminal_commands() {
        for role in [
            AgentRole::Orchestrator,
            AgentRole::Worker,
            AgentRole::StartOrchestrator,
        ] {
            let block = Harness {
                role,
                ..harness(Path::new("/s"))
            }
            .block();

            assert!(block.contains("- `swarm terminal spawn "), "{role:?}");
            assert!(block.contains("- `swarm terminal kill "), "{role:?}");
            assert!(block.contains("- `swarm terminal list`"), "{role:?}");
        }
    }

    #[test]
    fn a_sandboxed_codex_agent_is_told_to_use_the_mcp_tools() {
        let block = |kind, skip_permissions| {
            Harness {
                kind,
                skip_permissions,
                ..harness(Path::new("/s"))
            }
            .block()
        };

        let sandboxed = block(AgentKind::Codex, false);
        let skipping = block(AgentKind::Codex, true);
        let claude = block(AgentKind::ClaudeCode, false);

        assert!(sandboxed.contains("cannot reach Swarm"));
        assert!(skipping.contains("Run these commands in your shell"));
        assert!(claude.contains("Run these commands in your shell"));
    }
}
