//! The text Swarm appends to an agent's own instructions (ADR 0004).
//!
//! A fixed harness block always comes first. The role text comes from the last of
//! three layers that has the file: built in, then `~/.config/swarm/prompts/`, then
//! `.swarm/prompts/` in the project.

use std::io;
use std::path::Path;

use swarm_protocol::{AgentId, AgentRole, TaskId};

const ORCHESTRATOR: &str = include_str!("../prompts/orchestrator.md");
const WORKER: &str = include_str!("../prompts/worker.md");
const START: &str = include_str!("../prompts/start.md");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptFile {
    Orchestrator,
    Worker,
    Start,
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
        }
    }

    fn built_in(self) -> &'static str {
        match self {
            Self::Orchestrator => ORCHESTRATOR,
            Self::Worker => WORKER,
            Self::Start => START,
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
    pub socket: &'a Path,
}

/// One line per command: the command, then what it does.
const ORCHESTRATOR_COMMANDS: &[&str] = &[
    "`swarm spawn <name> [--kind claude|codex] [--model M] [--brief TEXT]`: start a worker (the brief comes from stdin without `--brief`)",
    "`swarm send <worker> <text>`: send a message to one of your workers",
    "`swarm read`: take your unread messages",
    "`swarm wait [--timeout SECS]`: wait for a message (default 120, at most 600)",
    "`swarm merge <worker>`: merge a worker's branch into the task branch",
    "`swarm agents`: list your workers",
    "`swarm notify <text>`: ask the user for attention",
    "`swarm confirm`: say that you understood your brief",
];

const WORKER_COMMANDS: &[&str] = &[
    "`swarm ask <text>`: ask your orchestrator a question; the answer arrives as a message",
    "`swarm report <text>`: send your orchestrator a progress or final report",
    "`swarm read`: take your unread messages",
    "`swarm wait [--timeout SECS]`: wait for a message (default 120, at most 600)",
];

const START_COMMANDS: &[&str] = &[
    "`swarm read`: take your unread messages",
    "`swarm wait [--timeout SECS]`: wait for a message (default 120, at most 600)",
];

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
            .map(|command| format!("- {command}"))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            "# Swarm harness\n\n\
             - Agent: {agent}\n\
             - Task: {task}\n\
             - Role: {role}\n\
             - SWARM_SOCKET: {socket}\n\n\
             Commands, also available as tools of the `swarm` MCP server:\n\n\
             {commands}\n\n\
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

    format!("{}\n\n{body}\n", harness.block())
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
        assert!(!orchestrator.contains("swarm ask"));
        assert!(worker.contains("- `swarm ask <text>`"));
        assert!(worker.contains("- `swarm report <text>`"));
        assert!(!worker.contains("swarm spawn"));
        assert!(!worker.contains("swarm send"));
        assert!(start.contains("- `swarm wait "));
        assert!(!start.contains("swarm spawn"));
        assert!(start.contains("Task: none"));
    }
}
