//! How to start Claude Code and Codex as agents. Pure functions: they return the
//! command line and the files to write, and the daemon does the rest.
//!
//! Prompts are appended, never replaced (ADR 0004), and no secret goes into argv,
//! because `ps` shows it. Flags and hook formats come from `docs/spikes/agent-status.md`.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use serde_json::json;
use sha2::{Digest as _, Sha256};
use swarm_protocol::AgentKind;

use crate::agents::to_hex;

/// Folders to search after `PATH`: apps started from Finder have a minimal `PATH`.
const SYSTEM_FOLDERS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

/// Folders below the home folder to search after `PATH`.
const HOME_FOLDERS: &[&str] = &[".local/bin", ".claude/local"];

/// Claude Code hook events Swarm listens to.
const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PermissionRequest",
    "Notification",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

/// Codex hook events: name, the label Codex hashes, and the timeout in seconds.
/// Codex clamps `Interrupt` to 3 s, and the trust hash uses the clamped value.
const CODEX_EVENTS: &[(&str, &str, u32)] = &[
    ("SessionStart", "session_start", 5),
    ("UserPromptSubmit", "user_prompt_submit", 5),
    ("PermissionRequest", "permission_request", 5),
    ("Stop", "stop", 5),
    ("Interrupt", "interrupt", 3),
];

/// Seconds a Claude Code hook may run.
const CLAUDE_HOOK_TIMEOUT: u32 = 5;

/// Codex stops an MCP tool call after 300 s unless told otherwise; `swarm wait` may take
/// 600 s.
const CODEX_TOOL_TIMEOUT_SECS: u32 = 660;

/// Permission rules that let the harness run without asking (Claude Code).
const CLAUDE_ALLOWED: &[&str] = &["Bash(swarm *)", "mcp__swarm__*"];

/// Whether the agent starts a new session or continues one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session<'a> {
    /// Claude Code takes the ID up front; Codex has none until its first prompt.
    New(Option<&'a str>),
    Resume(&'a str),
}

#[derive(Debug, Clone, Copy)]
pub struct LaunchSpec<'a> {
    pub kind: AgentKind,
    /// The CLI, already looked up with [`find_command`].
    pub command: &'a Path,
    pub model: &'a str,
    /// Harness block and role text.
    pub prompt: &'a str,
    pub worktree: &'a Path,
    pub skip_permissions: bool,
    /// The `swarm` binary the hooks run.
    pub swarm: &'a Path,
    /// Where this agent's files go: `<data>/agents/<id>/`.
    pub agent_dir: &'a Path,
    pub session: Session<'a>,
    /// The `SWARM_*` values of this agent. Claude Code gets them in its MCP file, Codex
    /// by name.
    pub harness_env: &'a [(String, String)],
    /// Git folders outside the worktree that a Codex sandbox must be able to write for
    /// `git commit` to work in a linked worktree.
    pub writable_roots: &'a [PathBuf],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchFile {
    pub path: PathBuf,
    pub contents: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub argv: Vec<String>,
    pub files: Vec<LaunchFile>,
}

pub fn build(spec: &LaunchSpec<'_>) -> Launch {
    match spec.kind {
        AgentKind::ClaudeCode => claude(spec),
        AgentKind::Codex => codex(spec),
    }
}

fn claude(spec: &LaunchSpec<'_>) -> Launch {
    let settings_path = spec.agent_dir.join("claude-settings.json");
    let prompt_path = spec.agent_dir.join("system-prompt.md");
    let mcp_path = spec.agent_dir.join("mcp.json");

    let mut argv = vec![
        text(spec.command),
        "--model".to_owned(),
        spec.model.to_owned(),
    ];
    match spec.session {
        Session::New(Some(id)) => argv.extend(["--session-id".to_owned(), id.to_owned()]),
        Session::New(None) => {}
        Session::Resume(id) => argv.extend(["--resume".to_owned(), id.to_owned()]),
    }
    argv.extend([
        "--append-system-prompt-file".to_owned(),
        text(&prompt_path),
        "--settings".to_owned(),
        text(&settings_path),
        "--mcp-config".to_owned(),
        text(&mcp_path),
    ]);
    if spec.skip_permissions {
        argv.push("--dangerously-skip-permissions".to_owned());
    }

    let handler = json!([{
        "hooks": [{
            "type": "command",
            "command": text(spec.swarm),
            "args": ["hook"],
            "timeout": CLAUDE_HOOK_TIMEOUT,
        }]
    }]);
    let hooks: serde_json::Map<String, serde_json::Value> = CLAUDE_EVENTS
        .iter()
        .map(|event| ((*event).to_owned(), handler.clone()))
        .collect();
    let mut settings = json!({
        "hooks": hooks,
        "permissions": { "allow": CLAUDE_ALLOWED },
    });
    if spec.skip_permissions {
        // Else Claude Code asks once before it enters bypass mode.
        settings["skipDangerousModePermissionPrompt"] = json!(true);
    }

    // Claude Code does not document which variables a stdio server inherits, so the
    // values go in the file. It is private (0600) like the folder it is in.
    let env: serde_json::Map<String, serde_json::Value> = spec
        .harness_env
        .iter()
        .map(|(name, value)| (name.clone(), json!(value)))
        .collect();
    let mcp = json!({
        "mcpServers": {
            "swarm": {
                "type": "stdio",
                "command": text(spec.swarm),
                "args": ["mcp"],
                "env": env,
            }
        }
    });

    Launch {
        argv,
        files: vec![
            LaunchFile {
                path: settings_path,
                contents: settings.to_string(),
            },
            LaunchFile {
                path: prompt_path,
                contents: spec.prompt.to_owned(),
            },
            LaunchFile {
                path: mcp_path,
                contents: mcp.to_string(),
            },
        ],
    }
}

fn codex(spec: &LaunchSpec<'_>) -> Launch {
    let mut argv = vec![text(spec.command)];
    if let Session::Resume(id) = spec.session {
        // Flags go after the subcommand.
        argv.extend(["resume".to_owned(), id.to_owned()]);
    }

    let config = |value: String| ["-c".to_owned(), value];
    argv.extend(config("check_for_update_on_startup=false".to_owned()));
    argv.extend(config(format!(
        "developer_instructions={}",
        toml_string(spec.prompt)
    )));
    argv.extend(["-m".to_owned(), spec.model.to_owned()]);

    // Codex runs the hook through a shell.
    let command = format!("{} hook", shell_quote(&text(spec.swarm)));
    let mut trusted = Vec::new();
    for (event, label, timeout) in CODEX_EVENTS {
        argv.extend(config(format!(
            "hooks.{event}=[{{hooks=[{{type=\"command\",command={},timeout={timeout}}}]}}]",
            toml_string(&command)
        )));
        trusted.push(format!(
            "\"/<session-flags>/config.toml:{label}:0:0\"={{trusted_hash=\"{}\"}}",
            trust_hash(label, &command, *timeout)
        ));
    }
    argv.extend(config(format!("hooks.state={{{}}}", trusted.join(","))));

    argv.extend(codex_mcp_config(spec));

    if spec.skip_permissions {
        argv.push("--dangerously-bypass-approvals-and-sandbox".to_owned());
    } else if !spec.writable_roots.is_empty() {
        let roots: Vec<String> = spec
            .writable_roots
            .iter()
            .map(|root| toml_string(&text(root)))
            .collect();
        argv.extend(config(format!(
            "sandbox_workspace_write.writable_roots=[{}]",
            roots.join(",")
        )));
    }

    Launch {
        argv,
        files: Vec::new(),
    }
}

/// Registers the `swarm` MCP server. Codex starts it with a small allow-list of
/// variables, so the `SWARM_*` names are added with `env_vars`.
fn codex_mcp_config(spec: &LaunchSpec<'_>) -> Vec<String> {
    let names: Vec<String> = spec
        .harness_env
        .iter()
        .map(|(name, _)| toml_string(name))
        .collect();

    [
        format!(
            "mcp_servers.swarm.command={}",
            toml_string(&text(spec.swarm))
        ),
        "mcp_servers.swarm.args=[\"mcp\"]".to_owned(),
        format!("mcp_servers.swarm.env_vars=[{}]", names.join(",")),
        "mcp_servers.swarm.default_tools_approval_mode=\"approve\"".to_owned(),
        format!("mcp_servers.swarm.tool_timeout_sec={CODEX_TOOL_TIMEOUT_SECS}"),
    ]
    .into_iter()
    .flat_map(|value| ["-c".to_owned(), value])
    .collect()
}

/// The hash Codex expects for a hook it may run without asking: `sha256:` and the
/// SHA-256 of the sorted-key, compact JSON of the hook group (`config/src/fingerprint.rs`
/// in Codex). Written by hand because the key order is the contract.
fn trust_hash(label: &str, command: &str, timeout: u32) -> String {
    let identity = format!(
        "{{\"event_name\":{},\"hooks\":[{{\"async\":false,\"command\":{},\"timeout\":{timeout},\"type\":\"command\"}}]}}",
        json_string(label),
        json_string(command),
    );

    let digest = Sha256::digest(identity.as_bytes());

    format!("sha256:{}", to_hex(&digest))
}

fn json_string(value: &str) -> String {
    serde_json::Value::String(value.to_owned()).to_string()
}

/// A TOML basic string.
fn toml_string(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            control if control.is_control() => {
                write!(quoted, "\\u{:04X}", u32::from(control)).ok();
            }
            other => quoted.push(other),
        }
    }
    quoted.push('"');

    quoted
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Looks the CLI up the way a shell would, then in the usual install folders.
/// A command with a slash is used as it is.
pub fn find_command(command: &str, path: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    if command.contains('/') {
        let given = PathBuf::from(command);

        return is_executable(&given).then_some(given);
    }

    let from_path = path.into_iter().flat_map(std::env::split_paths);
    let system = SYSTEM_FOLDERS.iter().map(PathBuf::from);
    let from_home = home.into_iter().flat_map(|home| {
        HOME_FOLDERS
            .iter()
            .map(|folder| home.join(folder))
            .collect::<Vec<_>>()
    });

    from_path
        .chain(system)
        .chain(from_home)
        .map(|folder| folder.join(command))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(kind: AgentKind) -> LaunchSpec<'static> {
        LaunchSpec {
            kind,
            command: Path::new("/bin/agent"),
            model: "opus",
            prompt: "Be \"kind\".\nSecond line.",
            worktree: Path::new("/repo/.worktrees/fix it"),
            skip_permissions: false,
            swarm: Path::new("/bin/swarm"),
            agent_dir: Path::new("/data/agents/7"),
            session: Session::New(Some("11111111-2222-4333-8444-555555555555")),
            harness_env: &[],
            writable_roots: &[],
        }
    }

    fn after(argv: &[String], flag: &str) -> Option<String> {
        argv.iter()
            .position(|argument| argument == flag)
            .and_then(|index| argv.get(index + 1))
            .cloned()
    }

    fn config_values(argv: &[String]) -> Vec<String> {
        argv.windows(2)
            .filter(|pair| pair[0] == "-c")
            .map(|pair| pair[1].clone())
            .collect()
    }

    #[test]
    fn claude_gets_model_session_prompt_and_settings() {
        let launch = build(&spec(AgentKind::ClaudeCode));

        assert_eq!(launch.argv[0], "/bin/agent");
        assert_eq!(after(&launch.argv, "--model").as_deref(), Some("opus"));
        assert_eq!(
            after(&launch.argv, "--session-id").as_deref(),
            Some("11111111-2222-4333-8444-555555555555")
        );
        assert_eq!(
            after(&launch.argv, "--append-system-prompt-file").as_deref(),
            Some("/data/agents/7/system-prompt.md")
        );
        assert!(!launch.argv.contains(&"--append-system-prompt".to_owned()));
        assert_eq!(
            launch.files[1].path,
            Path::new("/data/agents/7/system-prompt.md")
        );
        assert_eq!(launch.files[1].contents, "Be \"kind\".\nSecond line.");
        assert_eq!(
            after(&launch.argv, "--settings").as_deref(),
            Some("/data/agents/7/claude-settings.json")
        );
    }

    #[test]
    fn claude_never_replaces_the_system_prompt() {
        let launch = build(&spec(AgentKind::ClaudeCode));

        assert!(
            !launch
                .argv
                .iter()
                .any(|argument| argument == "--system-prompt" || argument == "--bare")
        );
    }

    #[test]
    fn claude_resumes_instead_of_choosing_a_session() {
        let mut resume = spec(AgentKind::ClaudeCode);
        resume.session = Session::Resume("abc");

        let launch = build(&resume);

        assert_eq!(after(&launch.argv, "--resume").as_deref(), Some("abc"));
        assert_eq!(after(&launch.argv, "--session-id"), None);
    }

    #[test]
    fn claude_skips_permissions_only_when_asked() {
        let mut skipping = spec(AgentKind::ClaudeCode);
        skipping.skip_permissions = true;

        assert!(
            !build(&spec(AgentKind::ClaudeCode))
                .argv
                .contains(&"--dangerously-skip-permissions".to_owned())
        );
        assert!(
            build(&skipping)
                .argv
                .contains(&"--dangerously-skip-permissions".to_owned())
        );
    }

    #[test]
    fn claude_settings_hold_one_exec_form_hook_per_event() {
        let launch = build(&spec(AgentKind::ClaudeCode));

        let file = &launch.files[0];
        let settings: serde_json::Value = serde_json::from_str(&file.contents).expect("json");
        assert_eq!(file.path, Path::new("/data/agents/7/claude-settings.json"));
        for event in CLAUDE_EVENTS {
            let hook = &settings["hooks"][*event][0]["hooks"][0];
            assert_eq!(hook["command"], "/bin/swarm", "{event}");
            assert_eq!(hook["args"], json!(["hook"]), "{event}");
        }
        assert_eq!(
            settings["hooks"].as_object().map(serde_json::Map::len),
            Some(CLAUDE_EVENTS.len())
        );
    }

    #[test]
    fn claude_settings_skip_the_bypass_dialog_only_with_skip_permissions() {
        let mut skipping = spec(AgentKind::ClaudeCode);
        skipping.skip_permissions = true;

        let plain: serde_json::Value =
            serde_json::from_str(&build(&spec(AgentKind::ClaudeCode)).files[0].contents)
                .expect("json");
        let bypass: serde_json::Value =
            serde_json::from_str(&build(&skipping).files[0].contents).expect("json");

        assert!(plain.get("skipDangerousModePermissionPrompt").is_none());
        assert_eq!(bypass["skipDangerousModePermissionPrompt"], true);
    }

    #[test]
    fn codex_turns_off_update_checks_and_leaves_trust_to_its_dialog() {
        let launch = build(&spec(AgentKind::Codex));

        let values = config_values(&launch.argv);
        assert!(values.contains(&"check_for_update_on_startup=false".to_owned()));
        assert!(!values.iter().any(|value| value.starts_with("projects=")));
    }

    #[test]
    fn codex_instructions_are_a_valid_toml_string() {
        let launch = build(&spec(AgentKind::Codex));

        let value = config_values(&launch.argv)
            .into_iter()
            .find_map(|value| {
                value
                    .strip_prefix("developer_instructions=")
                    .map(str::to_owned)
            })
            .expect("developer_instructions");
        let parsed: toml::Table = toml::from_str(&format!("v = {value}")).expect("toml");
        assert_eq!(parsed["v"].as_str(), Some("Be \"kind\".\nSecond line."));
    }

    #[test]
    fn codex_gets_the_model_and_no_session_flag() {
        let launch = build(&spec(AgentKind::Codex));

        assert_eq!(after(&launch.argv, "-m").as_deref(), Some("opus"));
        assert_eq!(launch.argv[1], "-c");
        assert!(launch.files.is_empty());
    }

    #[test]
    fn codex_resume_puts_the_flags_after_the_subcommand() {
        let mut resume = spec(AgentKind::Codex);
        resume.session = Session::Resume("abc");

        let launch = build(&resume);

        assert_eq!(&launch.argv[..3], ["/bin/agent", "resume", "abc"]);
        assert_eq!(launch.argv[3], "-c");
    }

    #[test]
    fn codex_bypasses_approvals_only_when_asked() {
        let mut skipping = spec(AgentKind::Codex);
        skipping.skip_permissions = true;
        let flag = "--dangerously-bypass-approvals-and-sandbox".to_owned();

        assert!(!build(&spec(AgentKind::Codex)).argv.contains(&flag));
        assert!(build(&skipping).argv.contains(&flag));
    }

    #[test]
    fn codex_hooks_parse_as_toml_and_run_the_swarm_command() {
        let launch = build(&spec(AgentKind::Codex));

        let hooks: Vec<String> = config_values(&launch.argv)
            .into_iter()
            .filter(|value| value.starts_with("hooks.") && !value.starts_with("hooks.state"))
            .collect();
        assert_eq!(hooks.len(), CODEX_EVENTS.len());
        for hook in hooks {
            let parsed: toml::Table = toml::from_str(&hook).expect("toml");
            let group = &parsed["hooks"]
                .as_table()
                .expect("hooks")
                .values()
                .next()
                .expect("event")[0];
            assert_eq!(
                group["hooks"][0]["command"].as_str(),
                Some("'/bin/swarm' hook")
            );
        }
    }

    #[test]
    fn the_trust_hash_matches_the_documented_recipe() {
        // `{"event_name":"stop","hooks":[{"async":false,"command":"'/bin/swarm' hook",
        // "timeout":5,"type":"command"}]}` hashed by hand with `shasum -a 256`.
        assert_eq!(
            trust_hash("stop", "'/bin/swarm' hook", 5),
            "sha256:2144cca52e4f68dd44397b4744dd3de3a2d4be57df2141377951d17d932c5635"
        );
    }

    #[test]
    fn the_interrupt_hash_uses_the_clamped_timeout() {
        assert_eq!(
            trust_hash("interrupt", "'/bin/swarm' hook", 3),
            "sha256:0a2b3b9007048ce182f2f5f8975cf84a5fd4bb47de0c0a0db6edfb274291f22e"
        );
    }

    #[test]
    fn every_codex_hook_has_a_matching_trust_entry() {
        let launch = build(&spec(AgentKind::Codex));

        let state = config_values(&launch.argv)
            .into_iter()
            .find(|value| value.starts_with("hooks.state="))
            .expect("hooks.state");
        let parsed: toml::Table = toml::from_str(&state).expect("toml");
        let entries = parsed["hooks"]["state"].as_table().expect("state");
        assert_eq!(entries.len(), CODEX_EVENTS.len());
        for (_, label, timeout) in CODEX_EVENTS {
            let key = format!("/<session-flags>/config.toml:{label}:0:0");
            assert_eq!(
                entries[&key]["trusted_hash"].as_str(),
                Some(trust_hash(label, "'/bin/swarm' hook", *timeout).as_str()),
                "{label}"
            );
        }
    }

    #[test]
    fn a_swarm_path_with_a_quote_is_shell_quoted() {
        let mut quoted = spec(AgentKind::Codex);
        quoted.swarm = Path::new("/it's/swarm");

        let launch = build(&quoted);

        let hook = config_values(&launch.argv)
            .into_iter()
            .find(|value| value.starts_with("hooks.Stop="))
            .expect("hook");
        let parsed: toml::Table = toml::from_str(&hook).expect("toml");
        assert_eq!(
            parsed["hooks"]["Stop"][0]["hooks"][0]["command"].as_str(),
            Some("'/it'\\''s/swarm' hook")
        );
    }

    fn harness_env() -> Vec<(String, String)> {
        [
            ("SWARM_SOCKET", "/data/agent.sock"),
            ("SWARM_AGENT_ID", "7"),
            ("SWARM_TASK", "3"),
            ("SWARM_TOKEN", "s3cret-token"),
            ("SWARM_ROLE", "worker"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
    }

    #[test]
    fn no_secret_is_in_the_arguments() {
        let env = harness_env();
        for kind in [AgentKind::Codex, AgentKind::ClaudeCode] {
            let launch = build(&LaunchSpec {
                harness_env: &env,
                ..spec(kind)
            });

            assert!(
                !launch
                    .argv
                    .iter()
                    .any(|argument| argument.contains("s3cret-token")),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn claude_gets_an_mcp_file_with_the_swarm_server_and_its_environment() {
        let env = harness_env();
        let launch = build(&LaunchSpec {
            harness_env: &env,
            ..spec(AgentKind::ClaudeCode)
        });

        assert_eq!(
            after(&launch.argv, "--mcp-config").as_deref(),
            Some("/data/agents/7/mcp.json")
        );
        assert!(!launch.argv.contains(&"--strict-mcp-config".to_owned()));
        let file = launch
            .files
            .iter()
            .find(|file| file.path == Path::new("/data/agents/7/mcp.json"))
            .expect("mcp file");
        let mcp: serde_json::Value = serde_json::from_str(&file.contents).expect("json");
        let server = &mcp["mcpServers"]["swarm"];
        assert_eq!(server["command"], "/bin/swarm");
        assert_eq!(server["args"], json!(["mcp"]));
        assert_eq!(server["env"]["SWARM_TOKEN"], "s3cret-token");
        assert_eq!(server["env"]["SWARM_ROLE"], "worker");
    }

    #[test]
    fn claude_may_use_the_harness_without_asking() {
        let launch = build(&spec(AgentKind::ClaudeCode));

        let settings: serde_json::Value =
            serde_json::from_str(&launch.files[0].contents).expect("json");

        assert_eq!(
            settings["permissions"]["allow"],
            json!(["Bash(swarm *)", "mcp__swarm__*"])
        );
    }

    fn codex_config(launch: &Launch) -> toml::Table {
        let lines: Vec<String> = config_values(&launch.argv)
            .into_iter()
            .filter(|value| value.starts_with("mcp_servers.") || value.starts_with("sandbox_"))
            .collect();

        toml::from_str(&lines.join("\n")).expect("toml")
    }

    #[test]
    fn codex_registers_the_swarm_server_and_approves_its_tools() {
        let env = harness_env();
        let launch = build(&LaunchSpec {
            harness_env: &env,
            ..spec(AgentKind::Codex)
        });

        let config = codex_config(&launch);
        let server = &config["mcp_servers"]["swarm"];

        assert_eq!(server["command"].as_str(), Some("/bin/swarm"));
        assert_eq!(server["args"][0].as_str(), Some("mcp"));
        assert_eq!(
            server["default_tools_approval_mode"].as_str(),
            Some("approve")
        );
        assert_eq!(server["tool_timeout_sec"].as_integer(), Some(660));
        let names: Vec<&str> = server["env_vars"]
            .as_array()
            .expect("env_vars")
            .iter()
            .filter_map(toml::Value::as_str)
            .collect();
        assert_eq!(
            names,
            [
                "SWARM_SOCKET",
                "SWARM_AGENT_ID",
                "SWARM_TASK",
                "SWARM_TOKEN",
                "SWARM_ROLE"
            ]
        );
    }

    #[test]
    fn codex_may_write_the_git_folders_of_a_worktree() {
        let roots = [
            PathBuf::from("/repo/.git"),
            PathBuf::from("/repo/.git/worktrees/fix it"),
        ];
        let launch = build(&LaunchSpec {
            writable_roots: &roots,
            ..spec(AgentKind::Codex)
        });

        let config = codex_config(&launch);

        let written: Vec<&str> = config["sandbox_workspace_write"]["writable_roots"]
            .as_array()
            .expect("roots")
            .iter()
            .filter_map(toml::Value::as_str)
            .collect();
        assert_eq!(written, ["/repo/.git", "/repo/.git/worktrees/fix it"]);
    }

    #[test]
    fn codex_gets_no_sandbox_roots_when_it_runs_without_a_sandbox() {
        let roots = [PathBuf::from("/repo/.git")];
        let launch = build(&LaunchSpec {
            writable_roots: &roots,
            skip_permissions: true,
            ..spec(AgentKind::Codex)
        });

        assert!(
            !config_values(&launch.argv)
                .iter()
                .any(|value| value.starts_with("sandbox_workspace_write"))
        );
    }

    #[test]
    fn toml_strings_survive_control_characters() {
        let value = "tab\tquote\"back\\slash\u{1}bell";

        let parsed: toml::Table =
            toml::from_str(&format!("v = {}", toml_string(value))).expect("toml");

        assert_eq!(parsed["v"].as_str(), Some(value));
    }

    struct Bin {
        root: PathBuf,
    }

    impl Bin {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("swarm-adapters-{}-{name}", std::process::id()));
            std::fs::create_dir_all(&root).expect("folder");

            Self { root }
        }

        fn tool(&self, folder: &str, name: &str, mode: u32) -> PathBuf {
            let folder = self.root.join(folder);
            std::fs::create_dir_all(&folder).expect("folder");
            let path = folder.join(name);
            std::fs::write(&path, "#!/bin/sh\n").expect("tool");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("mode");

            path
        }
    }

    impl Drop for Bin {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    #[test]
    fn a_command_is_found_in_path() {
        let bin = Bin::new("path");
        let tool = bin.tool("one", "claude", 0o755);

        let found = find_command("claude", Some(bin.root.join("one").as_os_str()), None);

        assert_eq!(found, Some(tool));
    }

    #[test]
    fn path_wins_over_the_home_folders() {
        let bin = Bin::new("order");
        let in_path = bin.tool("path", "claude", 0o755);
        bin.tool("home/.local/bin", "claude", 0o755);

        let found = find_command(
            "claude",
            Some(bin.root.join("path").as_os_str()),
            Some(&bin.root.join("home")),
        );

        assert_eq!(found, Some(in_path));
    }

    #[test]
    fn the_home_folders_are_searched_after_path() {
        let bin = Bin::new("home");
        let tool = bin.tool("home/.claude/local", "claude", 0o755);

        let found = find_command("claude", None, Some(&bin.root.join("home")));

        assert_eq!(found, Some(tool));
    }

    #[test]
    fn a_file_that_is_not_executable_is_skipped() {
        let bin = Bin::new("mode");
        bin.tool("one", "claude", 0o644);

        let found = find_command("claude", Some(bin.root.join("one").as_os_str()), None);

        assert_eq!(found, None);
    }

    #[test]
    fn a_command_with_a_slash_is_used_as_it_is() {
        let bin = Bin::new("slash");
        let tool = bin.tool("one", "fake-agent", 0o755);

        let found = find_command(tool.to_str().expect("utf-8"), None, None);

        assert_eq!(found, Some(tool));
    }

    #[test]
    fn a_missing_command_is_none() {
        assert_eq!(
            find_command("no-such-agent-cli", Some(OsStr::new("/nonexistent")), None),
            None
        );
    }
}
