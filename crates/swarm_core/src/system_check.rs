//! The system check (PLAN.md, System check): which of the tools Swarm needs are installed
//! and signed in.
//!
//! Every command runs without a shell and ends after a timeout. Nothing here reads a
//! secret: the Keychain is asked whether an item exists, and the Codex file only has to
//! exist.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use swarm_protocol::{SystemCheck, ToolCheck, ToolState};

use crate::adapters;
use crate::github::Github;
use crate::process::{self, Captured};

/// How long one command may run.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

/// `security` exits with this when the Keychain has no such item.
const ITEM_NOT_FOUND: i32 = 44;

const CLAUDE_KEYCHAIN_ITEM: &str = "Claude Code-credentials";

const CLAUDE_INSTALL: &str = "curl -fsSL https://claude.ai/install.sh | bash";

const CODEX_INSTALL: &str = "curl -fsSL https://chatgpt.com/codex/install.sh | sh";

/// The longest piece of tool output a detail quotes.
const DETAIL_CHARS: usize = 100;

/// The programs and folders the check uses. [`Tools::system`] is the real Mac; tests
/// replace parts.
#[derive(Debug, Clone)]
pub struct Tools {
    pub xcode_select: PathBuf,
    pub security: PathBuf,
    /// `PATH`, where `git`, `claude` and `codex` are looked up first.
    pub path: Option<OsString>,
    /// Looked up after `PATH`, for a `PATH` as small as launchd's.
    pub folders: Vec<PathBuf>,
    pub home: Option<PathBuf>,
    /// How long one command may run.
    pub timeout: Duration,
}

impl Tools {
    /// The system programs, with the folders and variables of this process.
    pub fn system() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);

        Self {
            xcode_select: PathBuf::from("/usr/bin/xcode-select"),
            security: PathBuf::from("/usr/bin/security"),
            path: std::env::var_os("PATH"),
            folders: adapters::search_folders(None, home.as_deref()),
            home,
            timeout: COMMAND_TIMEOUT,
        }
    }

    /// Every folder a program is looked up in, in order.
    pub fn search_folders(&self) -> Vec<PathBuf> {
        let from_path = self.path.iter().flat_map(std::env::split_paths);

        from_path.chain(self.folders.iter().cloned()).collect()
    }
}

/// The commands the agent settings name, which are looked up like the agents do.
#[derive(Debug, Clone, Copy)]
pub struct AgentCommands<'a> {
    pub claude: &'a str,
    pub codex: &'a str,
}

type Job<'scope> = Box<dyn FnOnce() -> ToolCheck + Send + 'scope>;

/// Checks every tool, each on its own thread.
pub fn check(tools: &Tools, commands: &AgentCommands<'_>, github: &Github) -> SystemCheck {
    let timeout = tools.timeout;
    let git: Job<'_> = Box::new(|| check_git(tools));
    let claude: Job<'_> = Box::new(|| check_claude(tools, commands.claude));
    let codex: Job<'_> = Box::new(|| check_codex(tools, commands.codex, github));
    let gh: Job<'_> = Box::new(move || check_gh(github, timeout));
    let account: Job<'_> = Box::new(move || check_account(github, timeout));

    let [git, claude, codex, gh, github_account] = run_together([
        ("git", git),
        ("claude", claude),
        ("codex", codex),
        ("gh", gh),
        ("account", account),
    ]);

    SystemCheck {
        git,
        claude,
        codex,
        gh,
        github_account,
    }
}

fn run_together<const COUNT: usize>(jobs: [(&str, Job<'_>); COUNT]) -> [ToolCheck; COUNT] {
    thread::scope(|scope| {
        let started = jobs.map(|(name, job)| {
            thread::Builder::new()
                .name(format!("check-{name}"))
                .spawn_scoped(scope, job)
        });

        started.map(|started| match started {
            Ok(handle) => handle
                .join()
                .unwrap_or_else(|_| failed(None, "the check stopped unexpectedly".to_owned())),
            Err(error) => failed(None, format!("cannot start the check: {error}")),
        })
    })
}

fn check_git(tools: &Tools) -> ToolCheck {
    let install = vec!["xcode-select".to_owned(), "--install".to_owned()];

    // Without the Command Line Tools, `/usr/bin/git` opens Apple's install dialog instead
    // of running, so `git` must not start.
    match run(&tools.xcode_select, &["-p"], tools.timeout) {
        Err(detail) => return failed(None, detail),
        Ok(captured) if !captured.success() => return missing(install),
        Ok(_) => {}
    }

    let Some(git) = find(tools, "git") else {
        return missing(install);
    };

    match version(&git, tools.timeout) {
        Ok(version) => ready(Some(version)),
        Err(detail) => failed(None, detail),
    }
}

fn check_claude(tools: &Tools, command: &str) -> ToolCheck {
    let Some(claude) = find(tools, command) else {
        return missing(shell(CLAUDE_INSTALL));
    };

    let version = match version(&claude, tools.timeout) {
        Ok(version) => version,
        Err(detail) => return failed(None, detail),
    };

    match claude_item_exists(tools) {
        Ok(true) => ready(Some(version)),
        Ok(false) => signed_out(Some(version), vec![text(&claude)]),
        Err(detail) => failed(Some(version), detail),
    }
}

/// Whether the Keychain has Claude Code's credentials. Without `-w`, `security` never
/// reads the secret, so no dialog can open.
fn claude_item_exists(tools: &Tools) -> Result<bool, String> {
    let arguments = ["find-generic-password", "-s", CLAUDE_KEYCHAIN_ITEM];
    let captured = run(&tools.security, &arguments, tools.timeout)?;

    if captured.success() {
        return Ok(true);
    }
    if captured.status.and_then(|status| status.code()) == Some(ITEM_NOT_FOUND) {
        return Ok(false);
    }

    Err(exit_detail("security", &arguments[..1], &captured))
}

fn check_codex(tools: &Tools, command: &str, github: &Github) -> ToolCheck {
    let Some(codex) = find(tools, command) else {
        let fix = match github.brew() {
            Some(brew) => vec![
                text(&brew),
                "install".to_owned(),
                "--cask".to_owned(),
                "codex".to_owned(),
            ],
            None => shell(CODEX_INSTALL),
        };

        return missing(fix);
    };

    let version = match version(&codex, tools.timeout) {
        Ok(version) => version,
        Err(detail) => return failed(None, detail),
    };

    match codex_auth_exists(tools) {
        Ok(true) => ready(Some(version)),
        Ok(false) => signed_out(Some(version), vec![text(&codex)]),
        Err(detail) => failed(Some(version), detail),
    }
}

/// Whether `~/.codex/auth.json` exists. Agents never get `CODEX_HOME`, so it does not count.
fn codex_auth_exists(tools: &Tools) -> Result<bool, String> {
    let home = tools
        .home
        .as_ref()
        .ok_or_else(|| "cannot find the home folder".to_owned())?;
    let file = home.join(".codex").join("auth.json");

    match std::fs::metadata(&file) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("cannot read {}: {error}", file.display())),
    }
}

fn check_gh(github: &Github, timeout: Duration) -> ToolCheck {
    let Some(result) = github.probe_version(timeout) else {
        return missing(github.install_command());
    };

    let version = finished("gh", &["--version"], timeout, result).and_then(|captured| {
        parse_version(&captured.stdout, &captured.stderr)
            .ok_or_else(|| "gh printed no version".to_owned())
    });

    match version {
        Ok(version) => ready(Some(version)),
        Err(detail) => failed(None, detail),
    }
}

fn check_account(github: &Github, timeout: Duration) -> ToolCheck {
    let Some(result) = github.probe_sign_in(timeout) else {
        return missing(Vec::new());
    };

    match finished("gh", &["auth", "status"], timeout, result) {
        Ok(captured) if captured.success() => ready(None),
        Ok(_) => signed_out(None, github.sign_in_command()),
        Err(detail) => failed(None, detail),
    }
}

fn find(tools: &Tools, command: &str) -> Option<PathBuf> {
    adapters::find_in(command, tools.search_folders())
}

/// The version `<program> --version` prints.
fn version(program: &Path, timeout: Duration) -> Result<String, String> {
    let name = name_of(program);
    let captured = run(program, &["--version"], timeout)?;

    if !captured.success() {
        return Err(exit_detail(&name, &["--version"], &captured));
    }

    parse_version(&captured.stdout, &captured.stderr)
        .ok_or_else(|| format!("{name} printed no version"))
}

/// Runs a program without a shell. `Err` has the detail of a program that did not end in
/// time or could not start; a program that ended with an error still gives `Ok`.
fn run(program: &Path, arguments: &[&str], timeout: Duration) -> Result<Captured, String> {
    let mut command = Command::new(program);
    command.args(arguments);

    let result = process::capture(command, None, timeout, false);

    finished(&name_of(program), arguments, timeout, result)
}

/// Turns how a command ended into the detail of a failed check, or its output.
fn finished(
    name: &str,
    arguments: &[&str],
    timeout: Duration,
    result: std::io::Result<Captured>,
) -> Result<Captured, String> {
    match result {
        Ok(captured) if captured.status.is_none() => Err(format!(
            "{} did not end within {} s",
            command_text(name, arguments),
            timeout.as_secs().max(1)
        )),
        Ok(captured) => Ok(captured),
        Err(error) => Err(format!("cannot run {name}: {error}")),
    }
}

/// What a command that exited with an error said, quoted short.
fn exit_detail(name: &str, arguments: &[&str], captured: &Captured) -> String {
    let said = [&captured.stderr[..], &captured.stdout[..]]
        .into_iter()
        .find_map(first_line);

    let status = match captured.status.and_then(|status| status.code()) {
        Some(code) => format!("exited with status {code}"),
        None => "was stopped by a signal".to_owned(),
    };

    let command = command_text(name, arguments);

    match said {
        Some(said) => format!("{command} {status}: {}", clip(&said)),
        None => format!("{command} {status}"),
    }
}

fn command_text(name: &str, arguments: &[&str]) -> String {
    match arguments.first() {
        Some(first) => format!("{name} {first}"),
        None => name.to_owned(),
    }
}

fn clip(text: &str) -> String {
    if text.chars().count() <= DETAIL_CHARS {
        return text.to_owned();
    }

    let kept: String = text.chars().take(DETAIL_CHARS).collect();

    format!("{kept}...")
}

/// The first number-like word of the first line: `2.1.126` from `2.1.126 (Claude Code)`,
/// `0.46.0` from `codex-cli 0.46.0` and `2.39.5` from `git version 2.39.5 (Apple Git-154)`.
fn parse_version(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    [stdout, stderr].into_iter().find_map(|bytes| {
        let line = first_line(bytes)?;

        let numbered = line.split_whitespace().find_map(|word| {
            let word = word.strip_prefix('v').unwrap_or(word);

            word.starts_with(|character: char| character.is_ascii_digit())
                .then(|| word.to_owned())
        });

        Some(numbered.unwrap_or_else(|| clip(&line)))
    })
}

/// The first line that has text.
fn first_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

fn name_of(program: &Path) -> String {
    program
        .file_name()
        .map_or_else(|| text(program), |name| name.to_string_lossy().into_owned())
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn shell(script: &str) -> Vec<String> {
    vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()]
}

fn tool(
    state: ToolState,
    version: Option<String>,
    detail: Option<String>,
    fix: Vec<String>,
) -> ToolCheck {
    ToolCheck {
        state,
        version,
        detail,
        fix,
    }
}

fn ready(version: Option<String>) -> ToolCheck {
    tool(ToolState::Ready, version, None, Vec::new())
}

fn missing(fix: Vec<String>) -> ToolCheck {
    tool(ToolState::Missing, None, None, fix)
}

fn signed_out(version: Option<String>, fix: Vec<String>) -> ToolCheck {
    tool(ToolState::SignedOut, version, None, fix)
}

fn failed(version: Option<String>, detail: String) -> ToolCheck {
    tool(ToolState::Failed, version, Some(detail), Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version_of(stdout: &str) -> Option<String> {
        parse_version(stdout.as_bytes(), b"")
    }

    #[test]
    fn the_version_is_the_first_word_that_starts_with_a_digit() {
        assert_eq!(
            version_of("2.1.126 (Claude Code)\n").as_deref(),
            Some("2.1.126")
        );
        assert_eq!(version_of("codex-cli 0.46.0\n").as_deref(), Some("0.46.0"));
        assert_eq!(
            version_of("git version 2.39.5 (Apple Git-154)\n").as_deref(),
            Some("2.39.5")
        );
        assert_eq!(
            version_of("gh version 2.62.0 (2024-11-14)\nhttps://github.com/cli/cli\n").as_deref(),
            Some("2.62.0")
        );
        assert_eq!(version_of("v1.2.3\n").as_deref(), Some("1.2.3"));
    }

    #[test]
    fn a_line_without_a_number_is_the_version_as_printed() {
        assert_eq!(
            version_of("\n  nightly build \n").as_deref(),
            Some("nightly build")
        );
    }

    #[test]
    fn no_output_gives_no_version() {
        assert_eq!(version_of(""), None);
        assert_eq!(version_of("\n  \n"), None);
    }

    #[test]
    fn stderr_is_used_when_stdout_is_empty() {
        assert_eq!(
            parse_version(b"", b"tool 3.4.5\n").as_deref(),
            Some("3.4.5")
        );
    }

    #[test]
    fn a_long_line_is_cut_short() {
        let long = "x".repeat(500);

        let clipped = clip(&long);

        assert_eq!(clipped.chars().count(), DETAIL_CHARS + 3);
        assert!(clipped.ends_with("..."));
    }
}
