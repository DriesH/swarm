//! The text Swarm appends to an agent's own instructions (ADR 0004).
//!
//! A fixed harness block always comes first. The role text comes from the last of
//! three layers that has the file: built in, then `~/.config/swarm/prompts/`, then
//! `.swarm/prompts/` in the project.

use std::io;
use std::path::{Path, PathBuf};

use swarm_protocol::{AgentId, AgentKind, AgentRole, PromptInfo, PromptLayer, TaskId};

use crate::Error;
use crate::config;

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
    /// Every prompt, in the order Settings lists them.
    pub const ALL: [Self; 4] = [Self::Orchestrator, Self::Worker, Self::Start, Self::Pr];

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

    /// The file name without `.md`.
    pub fn name(self) -> &'static str {
        self.file_name().trim_end_matches(".md")
    }

    fn from_name(name: &str) -> Result<Self, Error> {
        Self::ALL
            .into_iter()
            .find(|file| file.name() == name)
            .ok_or_else(|| Error::Invalid(format!("there is no prompt named {name:?}")))
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
    "`swarm terminal spawn [--name N] [--cwd DIR] -- <command>...`: run a command in a process terminal; no shell is added, so pass `sh -c` yourself when you need one",
    "`swarm terminal list`: list the process terminals of your task",
    "`swarm terminal read <id> [--lines N]`: read the screen or the last lines of output (at most 2000)",
    "`swarm terminal wait <id> (--exit | --output TEXT) [--timeout SECS]`: wait for the process to exit or for a line that contains TEXT",
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
        user_folder(config_dir).map(|folder| folder.join(file.file_name())),
        Some(project_file(project_root, file)),
    ];
    for path in layers.into_iter().flatten() {
        if let Some(text) = layer_text(&path) {
            body = text;
        }
    }

    body
}

/// Every prompt with the layer its agents get. Without a project, that is the layer
/// outside any project.
pub fn list(
    config_dir: Option<&Path>,
    project_root: Option<&Path>,
) -> Result<Vec<PromptInfo>, Error> {
    let folder = required_user_folder(config_dir)?;

    let prompts = PromptFile::ALL
        .into_iter()
        .map(|file| {
            let user_path = folder.join(file.file_name());
            let project_path = project_root
                .map(|root| project_file(root, file))
                .filter(|path| path.is_file());

            let layer = if project_path.as_deref().and_then(layer_text).is_some() {
                PromptLayer::Project
            } else if layer_text(&user_path).is_some() {
                PromptLayer::User
            } else {
                PromptLayer::BuiltIn
            };

            PromptInfo {
                name: file.name().to_owned(),
                layer,
                user_path,
                project_path,
            }
        })
        .collect();

    Ok(prompts)
}

/// Writes the built-in text to the user's folder, unless a copy with text is already
/// there. A blank copy counts as missing. Returns the path of the copy.
pub fn customize(name: &str, config_dir: Option<&Path>) -> Result<PathBuf, Error> {
    let file = PromptFile::from_name(name)?;
    let path = required_user_folder(config_dir)?.join(file.file_name());

    match std::fs::metadata(&path) {
        Ok(found) if found.is_file() => {
            let read_error = |source| Error::Io {
                action: format!("read {}", path.display()),
                source,
            };

            let bytes = std::fs::read(&path).map_err(read_error)?;
            if !String::from_utf8_lossy(&bytes).trim().is_empty() {
                return Ok(path);
            }
        }
        Ok(_) => return Err(Error::Invalid(format!("{} is not a file", path.display()))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(Error::Io {
                action: format!("read {}", path.display()),
                source,
            });
        }
    }

    config::write_atomic(&path, file.built_in())?;

    Ok(path)
}

/// Removes the user's copy, so the built-in text applies again. A missing copy is fine.
pub fn reset(name: &str, config_dir: Option<&Path>) -> Result<(), Error> {
    let file = PromptFile::from_name(name)?;
    let path = required_user_folder(config_dir)?.join(file.file_name());

    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::Io {
            action: format!("remove {}", path.display()),
            source,
        }),
    }
}

fn user_folder(config_dir: Option<&Path>) -> Option<PathBuf> {
    config_dir.map(|dir| dir.join("prompts"))
}

fn required_user_folder(config_dir: Option<&Path>) -> Result<PathBuf, Error> {
    user_folder(config_dir)
        .ok_or_else(|| Error::Invalid("cannot find the folder of the user's prompts".to_owned()))
}

fn project_file(project_root: &Path, file: PromptFile) -> PathBuf {
    project_root.join(".swarm/prompts").join(file.file_name())
}

/// The trimmed text of a layer file, or none when it is missing or blank.
fn layer_text(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => Some(text.trim().to_owned()),
        Ok(_) => None,
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            log::warn!("cannot read the prompt {}: {error}", path.display());

            None
        }
    }
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
    fn the_orchestrator_sends_the_draft_without_the_shell_reading_it() {
        assert!(ORCHESTRATOR.contains("Prefer the `pr_draft` tool"));
        assert!(ORCHESTRATOR.contains("swarm pr draft --title '<title>' --body-file - <<'EOF'"));
        assert!(ORCHESTRATOR.contains("single quotes"));
        assert!(ORCHESTRATOR.contains("backticks or `$`"));
        assert!(!ORCHESTRATOR.contains("--title \"<title>\""));
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

    fn project_prompts(folders: &Folders) -> PathBuf {
        folders.project().join(".swarm")
    }

    fn layer_of(prompts: &[PromptInfo], name: &str) -> PromptLayer {
        prompts
            .iter()
            .find(|prompt| prompt.name == name)
            .expect("prompt")
            .layer
    }

    #[test]
    fn every_built_in_prompt_is_listed_by_its_file_name() {
        let folders = Folders::new("list-names");

        let prompts = list(Some(&folders.config()), None).expect("list");

        let names: Vec<_> = prompts.iter().map(|prompt| prompt.name.as_str()).collect();
        assert_eq!(names, ["orchestrator", "worker", "start", "pr"]);
        for file in PromptFile::ALL {
            assert_eq!(format!("{}.md", file.name()), file.file_name());
        }
    }

    #[test]
    fn outside_any_project_the_built_in_layer_applies_and_the_user_path_is_known() {
        let folders = Folders::new("list-none");

        let prompts = list(Some(&folders.config()), None).expect("list");

        for prompt in &prompts {
            assert_eq!(prompt.layer, PromptLayer::BuiltIn, "{}", prompt.name);
            assert_eq!(prompt.project_path, None);
            assert_eq!(
                prompt.user_path,
                folders
                    .config()
                    .join("prompts")
                    .join(format!("{}.md", prompt.name))
            );
        }
    }

    #[test]
    fn a_user_copy_is_the_user_layer_and_only_for_its_own_prompt() {
        let folders = Folders::new("list-user");
        Folders::write(&folders.config(), "worker.md", "User worker text");

        let prompts = list(Some(&folders.config()), None).expect("list");

        assert_eq!(layer_of(&prompts, "worker"), PromptLayer::User);
        assert_eq!(layer_of(&prompts, "orchestrator"), PromptLayer::BuiltIn);
        assert_eq!(layer_of(&prompts, "pr"), PromptLayer::BuiltIn);
    }

    #[test]
    fn a_project_copy_wins_over_the_user_copy_and_shows_its_path() {
        let folders = Folders::new("list-project");
        Folders::write(&folders.config(), "worker.md", "User worker text");
        Folders::write(&folders.config(), "pr.md", "User pr text");
        Folders::write(
            &project_prompts(&folders),
            "worker.md",
            "Project worker text",
        );

        let prompts = list(Some(&folders.config()), Some(&folders.project())).expect("list");

        let worker = prompts.iter().find(|prompt| prompt.name == "worker");
        assert_eq!(
            worker.map(|prompt| prompt.project_path.clone()),
            Some(Some(folders.project().join(".swarm/prompts/worker.md")))
        );
        assert_eq!(layer_of(&prompts, "worker"), PromptLayer::Project);
        assert_eq!(layer_of(&prompts, "pr"), PromptLayer::User);
        assert_eq!(layer_of(&prompts, "start"), PromptLayer::BuiltIn);
    }

    #[test]
    fn a_project_copy_does_not_count_outside_the_project() {
        let folders = Folders::new("list-outside");
        Folders::write(
            &project_prompts(&folders),
            "worker.md",
            "Project worker text",
        );

        let prompts = list(Some(&folders.config()), None).expect("list");

        assert_eq!(layer_of(&prompts, "worker"), PromptLayer::BuiltIn);
    }

    #[test]
    fn a_blank_project_copy_is_listed_but_the_layer_below_applies() {
        let folders = Folders::new("list-blank");
        Folders::write(&folders.config(), "worker.md", "User worker text");
        Folders::write(&project_prompts(&folders), "worker.md", " \n");

        let prompts = list(Some(&folders.config()), Some(&folders.project())).expect("list");

        let worker = prompts
            .iter()
            .find(|prompt| prompt.name == "worker")
            .expect("worker");
        assert!(worker.project_path.is_some());
        assert_eq!(worker.layer, PromptLayer::User);
    }

    #[test]
    fn the_listed_layer_is_the_layer_the_agents_get() {
        let folders = Folders::new("list-agrees");
        Folders::write(&folders.config(), "worker.md", "User worker text");
        Folders::write(&folders.config(), "pr.md", "User pr text");
        Folders::write(&project_prompts(&folders), "pr.md", "Project pr text");
        Folders::write(&project_prompts(&folders), "start.md", "  ");

        let prompts = list(Some(&folders.config()), Some(&folders.project())).expect("list");

        for (file, layer, expected) in [
            (
                PromptFile::Orchestrator,
                PromptLayer::BuiltIn,
                ORCHESTRATOR.trim(),
            ),
            (PromptFile::Worker, PromptLayer::User, "User worker text"),
            (PromptFile::Start, PromptLayer::BuiltIn, START.trim()),
            (PromptFile::Pr, PromptLayer::Project, "Project pr text"),
        ] {
            let text = layered(file, Some(&folders.config()), &folders.project());

            assert_eq!(layer_of(&prompts, file.name()), layer, "{}", file.name());
            assert_eq!(text, expected, "{}", file.name());
        }
    }

    #[test]
    fn listing_needs_a_config_folder() {
        assert!(matches!(list(None, None), Err(Error::Invalid(_))));
    }

    #[test]
    fn customizing_writes_the_built_in_text_to_the_user_folder() {
        let folders = Folders::new("customize");

        let path = customize("worker", Some(&folders.config())).expect("customize");

        assert_eq!(path, folders.config().join("prompts/worker.md"));
        assert_eq!(std::fs::read_to_string(&path).expect("copy"), WORKER);
        let prompts = list(Some(&folders.config()), None).expect("list");
        assert_eq!(layer_of(&prompts, "worker"), PromptLayer::User);
    }

    #[test]
    fn customizing_never_overwrites_a_copy_that_is_there() {
        let folders = Folders::new("customize-twice");
        Folders::write(&folders.config(), "worker.md", "My own worker text");

        let path = customize("worker", Some(&folders.config())).expect("customize");

        assert_eq!(path, folders.config().join("prompts/worker.md"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("copy"),
            "My own worker text"
        );
    }

    #[test]
    fn customizing_a_blank_copy_writes_the_built_in_text_into_it() {
        for blank in ["", "  \n\t\n"] {
            let folders = Folders::new("customize-blank");
            Folders::write(&folders.config(), "worker.md", blank);

            let path = customize("worker", Some(&folders.config())).expect("customize");

            assert_eq!(path, folders.config().join("prompts/worker.md"));
            assert_eq!(std::fs::read_to_string(&path).expect("copy"), WORKER);
            let prompts = list(Some(&folders.config()), None).expect("list");
            assert_eq!(layer_of(&prompts, "worker"), PromptLayer::User);
        }
    }

    #[test]
    fn customizing_leaves_no_temporary_file_in_the_user_folder() {
        let folders = Folders::new("customize-temp");

        customize("pr", Some(&folders.config())).expect("customize");

        let names: Vec<_> = std::fs::read_dir(folders.config().join("prompts"))
            .expect("folder")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(names, ["pr.md"]);
    }

    #[test]
    fn resetting_removes_the_user_copy_and_the_built_in_layer_applies_again() {
        let folders = Folders::new("reset");
        Folders::write(&folders.config(), "worker.md", "User worker text");
        Folders::write(&folders.config(), "pr.md", "User pr text");

        reset("worker", Some(&folders.config())).expect("reset");

        assert!(!folders.config().join("prompts/worker.md").exists());
        assert!(folders.config().join("prompts/pr.md").exists());
        let prompts = list(Some(&folders.config()), None).expect("list");
        assert_eq!(layer_of(&prompts, "worker"), PromptLayer::BuiltIn);
        assert_eq!(layer_of(&prompts, "pr"), PromptLayer::User);
    }

    #[test]
    fn resetting_a_prompt_without_a_copy_is_fine() {
        let folders = Folders::new("reset-missing");

        reset("worker", Some(&folders.config())).expect("reset");
        reset("worker", Some(&folders.config())).expect("reset again");
    }

    #[test]
    fn an_unknown_prompt_name_is_refused() {
        let folders = Folders::new("unknown");
        let config = folders.config();

        for name in ["review", "", "Worker", "worker.md", "../worker", "a/b"] {
            assert!(
                matches!(customize(name, Some(&config)), Err(Error::Invalid(_))),
                "customize {name:?}"
            );
            assert!(
                matches!(reset(name, Some(&config)), Err(Error::Invalid(_))),
                "reset {name:?}"
            );
        }
        assert!(!config.exists(), "a refusal writes nothing");
    }

    #[test]
    fn customizing_and_resetting_never_touch_the_project_prompts() {
        let folders = Folders::new("project-untouched");
        let project = project_prompts(&folders);
        Folders::write(&project, "worker.md", "Project worker text");
        let file = folders.project().join(".swarm/prompts/worker.md");

        customize("worker", Some(&folders.config())).expect("customize");
        reset("worker", Some(&folders.config())).expect("reset");

        assert_eq!(
            std::fs::read_to_string(file).expect("project copy"),
            "Project worker text"
        );
    }

    #[test]
    fn without_a_config_folder_customizing_and_resetting_fail() {
        assert!(matches!(customize("worker", None), Err(Error::Invalid(_))));
        assert!(matches!(reset("worker", None), Err(Error::Invalid(_))));
    }
}
