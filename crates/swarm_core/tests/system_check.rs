//! The system check against fake tools. Nothing here starts the real `git`, `claude`,
//! `codex`, `gh`, `brew`, `security` or `xcode-select`, and nothing reaches the network.

#![expect(clippy::expect_used, reason = "test helpers panic on failure")]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use swarm_core::github::Github;
use swarm_core::system_check::{AgentCommands, Tools, check};
use swarm_protocol::{SystemCheck, ToolState};

static NEXT_FOLDER: AtomicU32 = AtomicU32::new(0);

/// Command names no real Mac has, so a lookup never finds a real tool.
const CLAUDE: &str = "swarm-test-claude";
const CODEX: &str = "swarm-test-codex";

const GIT_OUTPUT: &str = "git version 2.39.5 (Apple Git-154)";

/// A folder with fake tools in `bin`, a fake home and the fake `xcode-select` and
/// `security`. Removed on drop.
struct World {
    root: PathBuf,
}

impl World {
    fn new(name: &str) -> Self {
        let number = NEXT_FOLDER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "swarm-syscheck-{name}-{}-{number}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("bin")).expect("bin");
        std::fs::create_dir_all(root.join("home")).expect("home");

        let world = Self {
            root: root.canonicalize().expect("real path"),
        };
        world.script("xcode-select", "exit 0");
        world.script("security", "exit 0");

        world
    }

    fn bin(&self) -> PathBuf {
        self.root.join("bin")
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    /// A script next to the tools that logs its arguments, one call per line, to
    /// `<name>.calls`. The body runs after that.
    fn script(&self, name: &str, body: &str) -> PathBuf {
        self.write(&self.root.join(name), name, body)
    }

    fn tool(&self, name: &str, body: &str) -> PathBuf {
        self.write(&self.bin().join(name), name, body)
    }

    fn write(&self, path: &Path, name: &str, body: &str) -> PathBuf {
        let log = self.root.join(format!("{name}.calls"));
        std::fs::write(
            path,
            format!("#!/bin/sh\necho \"$*\" >> '{}'\n{body}\n", log.display()),
        )
        .expect("script");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("mode");

        path.to_owned()
    }

    /// How often the script ran, and with what arguments.
    fn calls(&self, name: &str) -> Vec<String> {
        std::fs::read_to_string(self.root.join(format!("{name}.calls")))
            .map(|text| text.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    fn tools(&self) -> Tools {
        Tools {
            xcode_select: self.root.join("xcode-select"),
            security: self.root.join("security"),
            path: Some(self.bin().into_os_string()),
            folders: Vec::new(),
            home: Some(self.home()),
            timeout: Duration::from_secs(10),
        }
    }

    fn with_xcode_select(&self, body: &str) {
        self.script("xcode-select", body);
    }

    /// Git, `claude` and `codex` that run and answer.
    fn working_tools(&self) {
        self.tool("git", &format!("echo '{GIT_OUTPUT}'"));
        self.tool(CLAUDE, "echo '2.1.126 (Claude Code)'");
        self.tool(CODEX, "echo 'codex-cli 0.46.0'");
    }

    fn codex_auth(&self) {
        let folder = self.home().join(".codex");
        std::fs::create_dir_all(&folder).expect(".codex");
        std::fs::write(folder.join("auth.json"), "{}").expect("auth.json");
    }

    /// A fake `gh` that answers `--version` and `auth status`.
    fn gh(&self, auth_body: &str) -> PathBuf {
        self.tool(
            "gh",
            &format!(
                "case \"$1\" in\n--version) echo 'gh version 2.62.0 (2024-11-14)'; \
                 echo 'https://github.com/cli/cli/releases/tag/v2.62.0';;\n\
                 auth) {auth_body};;\nesac"
            ),
        )
    }

    /// `gh` and Homebrew are looked up in `folders`; the install folder is unused.
    fn github(&self, gh: Option<PathBuf>, folders: Vec<PathBuf>) -> Github {
        Github::with_folders(self.root.join("swarm-bin"), gh, folders)
    }

    /// Homebrew in its own folder, so a lookup in that folder finds only Homebrew.
    fn brew(&self) -> (PathBuf, Vec<PathBuf>) {
        let folder = self.root.join("brew-bin");
        std::fs::create_dir_all(&folder).expect("brew folder");
        let brew = folder.join("brew");
        std::fs::write(&brew, "#!/bin/sh\nexit 0\n").expect("brew");
        std::fs::set_permissions(&brew, std::fs::Permissions::from_mode(0o755)).expect("mode");

        (brew, vec![folder])
    }
}

/// Runs the check on tools that must not reach beyond the world.
fn run_check(world: &World, tools: &Tools, github: &Github) -> SystemCheck {
    assert_contained(world, tools);

    check(
        tools,
        &AgentCommands {
            claude: CLAUDE,
            codex: CODEX,
        },
        github,
    )
}

/// Panics when the tools can start or find a program outside the folder of the world, like
/// the real `git` in `/opt/homebrew/bin`.
fn assert_contained(world: &World, tools: &Tools) {
    let reachable = tools
        .search_folders()
        .into_iter()
        .chain([tools.xcode_select.clone(), tools.security.clone()])
        .chain(tools.home.clone());

    for path in reachable {
        assert!(
            path.starts_with(&world.root),
            "{} is outside {}",
            path.display(),
            world.root.display()
        );
    }
}

impl Drop for World {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

fn path_text(path: &Path) -> &str {
    path.to_str().expect("utf-8")
}

#[test]
fn git_is_ready_with_its_version() {
    let world = World::new("git-ready");
    world.working_tools();

    let result = run_check(&world, &world.tools(), &world.github(None, Vec::new()));

    assert_eq!(result.git.state, ToolState::Ready);
    assert_eq!(result.git.version.as_deref(), Some("2.39.5"));
    assert!(result.git.fix.is_empty());
    assert_eq!(world.calls("xcode-select"), ["-p"]);
}

#[test]
fn git_is_missing_and_never_run_when_the_command_line_tools_are_not_installed() {
    let world = World::new("git-missing");
    world.with_xcode_select("exit 2");
    world.tool(
        "git",
        "echo ran > \"$(dirname \"$0\")/git-ran\"; echo 'git version 1'",
    );

    let result = run_check(&world, &world.tools(), &world.github(None, Vec::new()));

    assert_eq!(result.git.state, ToolState::Missing);
    assert_eq!(result.git.fix, strings(&["xcode-select", "--install"]));
    assert!(result.git.version.is_none());
    assert!(
        world.calls("git").is_empty() && !world.bin().join("git-ran").exists(),
        "git started although xcode-select -p failed"
    );
}

#[test]
fn git_fails_when_it_cannot_say_its_version() {
    let world = World::new("git-broken");
    world.tool("git", "echo 'bad install' >&2; exit 3");

    let result = run_check(&world, &world.tools(), &world.github(None, Vec::new()));

    assert_eq!(result.git.state, ToolState::Failed);
    let detail = result.git.detail.expect("detail");
    assert!(detail.contains("status 3"), "{detail}");
    assert!(detail.contains("bad install"), "{detail}");
}

#[test]
fn git_fails_when_xcode_select_cannot_run() {
    let world = World::new("xcode-missing");
    world.working_tools();
    let mut tools = world.tools();
    tools.xcode_select = world.root.join("no-such-xcode-select");

    let result = run_check(&world, &tools, &world.github(None, Vec::new()));

    assert_eq!(result.git.state, ToolState::Failed);
    let detail = result.git.detail.expect("detail");
    assert!(
        detail.starts_with("cannot run no-such-xcode-select"),
        "{detail}"
    );
    assert!(world.calls("git").is_empty());
}

#[test]
fn claude_is_ready_when_the_keychain_has_its_item() {
    let world = World::new("claude-ready");
    world.working_tools();

    let result = run_check(&world, &world.tools(), &world.github(None, Vec::new()));

    assert_eq!(result.claude.state, ToolState::Ready);
    assert_eq!(result.claude.version.as_deref(), Some("2.1.126"));
    assert!(result.claude.fix.is_empty());
    assert_eq!(
        world.calls("security"),
        ["find-generic-password -s Claude Code-credentials"],
        "the secret must never be read (no -w, no -g)"
    );
}

#[test]
fn claude_is_signed_out_when_the_keychain_item_is_not_found() {
    let world = World::new("claude-out");
    world.working_tools();
    world.script("security", "exit 44");

    let result = run_check(&world, &world.tools(), &world.github(None, Vec::new()));

    assert_eq!(result.claude.state, ToolState::SignedOut);
    assert_eq!(result.claude.version.as_deref(), Some("2.1.126"));
    assert_eq!(
        result.claude.fix,
        strings(&[path_text(&world.bin().join(CLAUDE))])
    );
}

#[test]
fn claude_fails_when_the_keychain_check_breaks_for_another_reason() {
    let world = World::new("claude-keychain");
    world.working_tools();
    world.script("security", "echo 'keychain is locked' >&2; exit 36");

    let result = run_check(&world, &world.tools(), &world.github(None, Vec::new()));

    assert_eq!(result.claude.state, ToolState::Failed);
    assert_eq!(result.claude.version.as_deref(), Some("2.1.126"));
    let detail = result.claude.detail.expect("detail");
    assert!(detail.contains("status 36"), "{detail}");
}

#[test]
fn claude_is_missing_with_the_official_installer_as_its_fix() {
    let world = World::new("claude-missing");
    world.working_tools();
    let mut tools = world.tools();
    tools.path = Some(world.root.join("empty").into_os_string());

    let result = run_check(&world, &tools, &world.github(None, Vec::new()));

    assert_eq!(result.claude.state, ToolState::Missing);
    assert_eq!(
        result.claude.fix,
        strings(&[
            "/bin/sh",
            "-c",
            "curl -fsSL https://claude.ai/install.sh | bash"
        ])
    );
    assert!(world.calls("security").is_empty(), "nothing to sign in to");
}

#[test]
fn claude_that_cannot_say_its_version_fails_without_a_keychain_call() {
    let world = World::new("claude-broken");
    world.working_tools();
    world.tool(CLAUDE, "exit 1");

    let result = run_check(&world, &world.tools(), &world.github(None, Vec::new()));

    assert_eq!(result.claude.state, ToolState::Failed);
    assert!(world.calls("security").is_empty());
}

#[test]
fn codex_is_ready_when_its_auth_file_exists() {
    let world = World::new("codex-ready");
    world.working_tools();
    world.codex_auth();

    let result = run_check(&world, &world.tools(), &world.github(None, Vec::new()));

    assert_eq!(result.codex.state, ToolState::Ready);
    assert_eq!(result.codex.version.as_deref(), Some("0.46.0"));
    assert!(result.codex.fix.is_empty());
}

#[test]
fn codex_is_signed_out_without_an_auth_file() {
    let world = World::new("codex-out");
    world.working_tools();
    std::fs::create_dir_all(world.home().join(".codex")).expect(".codex");

    let result = run_check(&world, &world.tools(), &world.github(None, Vec::new()));

    assert_eq!(result.codex.state, ToolState::SignedOut);
    assert_eq!(result.codex.version.as_deref(), Some("0.46.0"));
    assert_eq!(
        result.codex.fix,
        strings(&[path_text(&world.bin().join(CODEX))])
    );
}

#[test]
fn an_auth_file_that_is_a_folder_does_not_count() {
    let world = World::new("codex-folder");
    world.working_tools();
    std::fs::create_dir_all(world.home().join(".codex/auth.json")).expect("folder");

    let result = run_check(&world, &world.tools(), &world.github(None, Vec::new()));

    assert_eq!(result.codex.state, ToolState::SignedOut);
}

#[test]
fn a_sign_in_fix_names_the_tool_where_the_folders_after_path_found_it() {
    let world = World::new("fix-path");
    world.working_tools();
    world.script("security", "exit 44");
    std::fs::create_dir_all(world.home().join(".codex")).expect(".codex");
    let mut tools = world.tools();
    tools.path = Some(world.root.join("empty").into_os_string());
    tools.folders = vec![world.bin()];

    let result = run_check(&world, &tools, &world.github(None, Vec::new()));

    assert_eq!(result.claude.state, ToolState::SignedOut);
    assert_eq!(
        result.claude.fix,
        strings(&[path_text(&world.bin().join(CLAUDE))])
    );
    assert_eq!(result.codex.state, ToolState::SignedOut);
    assert_eq!(
        result.codex.fix,
        strings(&[path_text(&world.bin().join(CODEX))])
    );
}

#[test]
fn a_tool_in_no_searched_folder_is_missing() {
    let world = World::new("not-searched");
    world.working_tools();
    let mut tools = world.tools();
    tools.path = Some(world.root.join("empty").into_os_string());

    let result = run_check(&world, &tools, &world.github(None, Vec::new()));

    assert_eq!(result.git.state, ToolState::Missing);
    assert_eq!(result.claude.state, ToolState::Missing);
    assert_eq!(result.codex.state, ToolState::Missing);
    assert!(world.calls("git").is_empty());
}

#[test]
fn the_tools_of_a_test_reach_nothing_outside_its_folder() {
    let world = World::new("contained");

    assert_contained(&world, &world.tools());
}

#[test]
#[should_panic(expected = "is outside")]
fn the_system_tools_reach_beyond_the_folder_of_a_test() {
    let world = World::new("not-contained");

    assert_contained(&world, &Tools::system());
}

#[test]
fn codex_without_a_home_folder_fails() {
    let world = World::new("codex-no-home");
    world.working_tools();
    let mut tools = world.tools();
    tools.home = None;

    let result = run_check(&world, &tools, &world.github(None, Vec::new()));

    assert_eq!(result.codex.state, ToolState::Failed);
    assert!(result.codex.detail.is_some());
}

#[test]
fn a_missing_codex_installs_with_homebrew_when_homebrew_exists() {
    let world = World::new("codex-brew");
    world.working_tools();
    let (brew, folders) = world.brew();
    let mut tools = world.tools();
    tools.path = Some(world.root.join("empty").into_os_string());

    let result = run_check(&world, &tools, &world.github(None, folders));

    assert_eq!(result.codex.state, ToolState::Missing);
    assert_eq!(
        result.codex.fix,
        strings(&[brew.to_str().expect("utf-8"), "install", "--cask", "codex"])
    );
}

#[test]
fn a_missing_codex_uses_the_official_script_without_homebrew() {
    let world = World::new("codex-no-brew");
    let mut tools = world.tools();
    tools.path = Some(world.root.join("empty").into_os_string());

    let result = run_check(&world, &tools, &world.github(None, Vec::new()));

    assert_eq!(result.codex.state, ToolState::Missing);
    assert_eq!(
        result.codex.fix,
        strings(&[
            "/bin/sh",
            "-c",
            "curl -fsSL https://chatgpt.com/codex/install.sh | sh"
        ])
    );
}

#[test]
fn gh_and_the_account_are_ready_when_gh_is_signed_in() {
    let world = World::new("gh-ready");
    world.working_tools();
    let gh = world.gh("exit 0");

    let result = run_check(&world, &world.tools(), &world.github(Some(gh), Vec::new()));

    assert_eq!(result.gh.state, ToolState::Ready);
    assert_eq!(result.gh.version.as_deref(), Some("2.62.0"));
    assert_eq!(result.github_account.state, ToolState::Ready);
    assert!(result.github_account.fix.is_empty());
    let mut calls = world.calls("gh");
    calls.sort();
    assert_eq!(calls, ["--version", "auth status --hostname github.com"]);
}

#[test]
fn the_account_is_signed_out_with_the_sign_in_command() {
    let world = World::new("gh-out");
    world.working_tools();
    let gh = world.gh("echo 'You are not logged in' >&2; exit 1");

    let result = run_check(
        &world,
        &world.tools(),
        &world.github(Some(gh.clone()), Vec::new()),
    );

    assert_eq!(result.gh.state, ToolState::Ready);
    assert_eq!(result.github_account.state, ToolState::SignedOut);
    assert_eq!(
        result.github_account.fix,
        strings(&[
            gh.to_str().expect("utf-8"),
            "auth",
            "login",
            "--web",
            "--hostname",
            "github.com"
        ])
    );
}

#[test]
fn a_missing_gh_installs_with_homebrew_and_leaves_the_account_missing() {
    let world = World::new("gh-missing-brew");
    world.working_tools();
    let (brew, folders) = world.brew();

    let result = run_check(&world, &world.tools(), &world.github(None, folders));

    assert_eq!(result.gh.state, ToolState::Missing);
    assert_eq!(
        result.gh.fix,
        strings(&[brew.to_str().expect("utf-8"), "install", "gh"])
    );
    assert_eq!(result.github_account.state, ToolState::Missing);
    assert!(result.github_account.fix.is_empty());
}

#[test]
fn a_missing_gh_without_homebrew_downloads_the_release() {
    let world = World::new("gh-missing");
    world.working_tools();

    let result = run_check(&world, &world.tools(), &world.github(None, Vec::new()));

    assert_eq!(result.gh.state, ToolState::Missing);
    assert_eq!(&result.gh.fix[..2], ["/bin/sh", "-c"]);
    assert!(result.gh.fix[2].contains("github.com/cli/cli"));
    assert_eq!(result.github_account.state, ToolState::Missing);
    assert!(result.github_account.fix.is_empty());
}

#[test]
fn a_command_that_does_not_end_fails_the_check_and_leaves_the_others_alone() {
    let world = World::new("hang-version");
    world.working_tools();
    world.tool(CLAUDE, "exec sleep 30");
    world.codex_auth();
    let mut tools = world.tools();
    tools.timeout = Duration::from_secs(1);

    let start = Instant::now();
    let result = run_check(&world, &tools, &world.github(None, Vec::new()));

    assert!(start.elapsed() < Duration::from_secs(10));
    assert_eq!(result.claude.state, ToolState::Failed);
    let detail = result.claude.detail.expect("detail");
    assert!(detail.contains("did not end within 1 s"), "{detail}");
    assert_eq!(result.git.state, ToolState::Ready);
    assert_eq!(result.codex.state, ToolState::Ready);
}

#[test]
fn a_keychain_check_that_does_not_end_fails_with_the_version_kept() {
    let world = World::new("hang-security");
    world.working_tools();
    world.script("security", "exec sleep 30");
    let mut tools = world.tools();
    tools.timeout = Duration::from_secs(1);

    let result = run_check(&world, &tools, &world.github(None, Vec::new()));

    assert_eq!(result.claude.state, ToolState::Failed);
    assert_eq!(result.claude.version.as_deref(), Some("2.1.126"));
    let detail = result.claude.detail.expect("detail");
    assert!(detail.contains("security"), "{detail}");
}

#[test]
fn a_gh_sign_in_check_that_does_not_end_fails_the_account_only() {
    let world = World::new("hang-gh");
    world.working_tools();
    let gh = world.gh("exec sleep 30");
    let mut tools = world.tools();
    tools.timeout = Duration::from_secs(1);

    let result = run_check(&world, &tools, &world.github(Some(gh), Vec::new()));

    assert_eq!(result.gh.state, ToolState::Ready);
    assert_eq!(result.github_account.state, ToolState::Failed);
    let detail = result.github_account.detail.expect("detail");
    assert!(detail.contains("did not end within 1 s"), "{detail}");
}

#[test]
fn the_tools_are_checked_at_the_same_time() {
    let world = World::new("parallel");
    world.with_xcode_select("sleep 1");
    world.tool("git", &format!("sleep 1; echo '{GIT_OUTPUT}'"));
    world.tool(CLAUDE, "sleep 1; echo '2.1.126 (Claude Code)'");
    world.tool(CODEX, "sleep 1; echo 'codex-cli 0.46.0'");
    world.script("security", "sleep 1");
    world.codex_auth();
    let gh = world.tool(
        "gh",
        "sleep 1; case \"$1\" in --version) echo 'gh version 2.62.0 (2024-11-14)';; esac",
    );
    let github = world.github(Some(gh), Vec::new());

    let start = Instant::now();
    let result = run_check(&world, &world.tools(), &github);
    let elapsed = start.elapsed();

    assert_eq!(result.git.state, ToolState::Ready);
    assert_eq!(result.claude.state, ToolState::Ready);
    assert_eq!(result.codex.state, ToolState::Ready);
    assert_eq!(result.gh.state, ToolState::Ready);
    assert_eq!(result.github_account.state, ToolState::Ready);
    // One after the other these are seven seconds of sleeping.
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
}
