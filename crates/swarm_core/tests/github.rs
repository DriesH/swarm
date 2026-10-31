//! GitHub status, branches, the pre-push hook, pushing and the `gh` calls, against real
//! temporary git repositories and a fake `gh` script. Nothing here starts the real `gh` or
//! reaches the network.

#![expect(clippy::expect_used, reason = "test helpers panic on failure")]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use swarm_core::github::{Github, NewPullRequest, Target};
use swarm_core::store::Store;
use swarm_core::{Error, Projects, git};
use swarm_protocol::{CreateTask, GithubCheckState, MergeMethod, PrLink, ReviewSource};

static NEXT_FOLDER: AtomicU32 = AtomicU32::new(0);

/// A folder under the temp dir, removed on drop.
struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> Self {
        let number = NEXT_FOLDER.fetch_add(1, Ordering::Relaxed);
        let folder = std::env::temp_dir().join(format!(
            "swarm-github-it-{name}-{}-{number}",
            std::process::id()
        ));
        std::fs::create_dir_all(&folder).expect("folder");

        Self(folder.canonicalize().expect("real path"))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn git_in(folder: &Path, arguments: &[&str]) -> String {
    git_dated(folder, arguments, None)
}

fn git_dated(folder: &Path, arguments: &[&str], date: Option<&str>) -> String {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(folder)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(arguments);
    if let Some(date) = date {
        command
            .env("GIT_COMMITTER_DATE", date)
            .env("GIT_AUTHOR_DATE", date);
    }

    let output = command.output().expect("run git");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A repository with one commit on `main`.
fn repo(name: &str) -> Folder {
    let folder = Folder::new(name);
    git_in(folder.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(folder.path().join("README.md"), "hello\n").expect("readme");
    git_in(folder.path(), &["add", "README.md"]);
    git_in(folder.path(), &["commit", "-q", "-m", "first"]);

    folder
}

fn make_executable(path: &Path, body: &str) {
    std::fs::write(path, body).expect("script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("mode");
}

/// A fake `gh` that logs its arguments, a few variables and its stdin, and answers from
/// files next to it: `out.<a>-<b>`, `err.<a>-<b>` and `exit.<a>-<b>`.
struct FakeGh {
    folder: Folder,
}

impl FakeGh {
    fn new(name: &str) -> Self {
        let folder = Folder::new(name);
        make_executable(
            &folder.path().join("gh"),
            r#"#!/bin/sh
dir=$(dirname "$0")
key="$1-$2"
{
  printf 'ARGS'
  for argument in "$@"; do printf '\t%s' "$argument"; done
  printf '\n'
  printf 'REPO\t%s\n' "${GH_REPO-}"
  printf 'PROMPT\t%s\n' "${GH_PROMPT_DISABLED-}"
  printf 'CWD\t%s\n' "$PWD"
} >> "$dir/calls.log"
cat > "$dir/stdin.$key"
if [ "$1" = "--version" ]; then
  echo "gh version 2.62.0 (2024-11-14)"
  echo "https://github.com/cli/cli/releases/tag/v2.62.0"
fi
if [ -f "$dir/out.$key" ]; then cat "$dir/out.$key"; fi
if [ -f "$dir/err.$key" ]; then cat "$dir/err.$key" >&2; fi
if [ -f "$dir/exit.$key" ]; then exit "$(cat "$dir/exit.$key")"; fi
exit 0
"#,
        );

        Self { folder }
    }

    fn path(&self) -> PathBuf {
        self.folder.path().join("gh")
    }

    fn answer(&self, key: &str, out: &str, exit: i32) {
        std::fs::write(self.folder.path().join(format!("out.{key}")), out).expect("out");
        std::fs::write(
            self.folder.path().join(format!("exit.{key}")),
            exit.to_string(),
        )
        .expect("exit");
    }

    fn fail(&self, key: &str, message: &str) {
        std::fs::write(self.folder.path().join(format!("err.{key}")), message).expect("err");
        std::fs::write(self.folder.path().join(format!("exit.{key}")), "1").expect("exit");
    }

    fn calls(&self) -> Vec<Vec<String>> {
        let log = std::fs::read_to_string(self.folder.path().join("calls.log")).unwrap_or_default();

        log.lines()
            .filter_map(|line| line.strip_prefix("ARGS"))
            .map(|line| {
                line.split('\t')
                    .skip(1)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn logged(&self, label: &str) -> Vec<String> {
        let log = std::fs::read_to_string(self.folder.path().join("calls.log")).unwrap_or_default();

        log.lines()
            .filter_map(|line| line.strip_prefix(&format!("{label}\t")))
            .map(str::to_owned)
            .collect()
    }

    fn stdin(&self, key: &str) -> String {
        std::fs::read_to_string(self.folder.path().join(format!("stdin.{key}"))).unwrap_or_default()
    }
}

fn github(gh: &FakeGh, bin: &Path) -> Github {
    Github::with_folders(bin.to_path_buf(), Some(gh.path()), Vec::new())
}

#[test]
fn local_branches_come_newest_commit_first() {
    let repo = repo("branches");
    git_in(repo.path(), &["branch", "old"]);
    git_in(repo.path(), &["switch", "-q", "-c", "middle"]);
    git_dated(
        repo.path(),
        &["commit", "-q", "--allow-empty", "-m", "middle"],
        Some("2030-01-02T10:00:00Z"),
    );
    git_in(repo.path(), &["switch", "-q", "-c", "newest"]);
    git_dated(
        repo.path(),
        &["commit", "-q", "--allow-empty", "-m", "newest"],
        Some("2030-06-01T10:00:00Z"),
    );
    git_in(repo.path(), &["switch", "-q", "main"]);

    let branches = git::local_branches(repo.path()).expect("branches");

    assert_eq!(branches[..2], ["newest", "middle"]);
    assert_eq!(branches.len(), 4);
    assert!(branches.contains(&"main".to_owned()));
    assert!(branches.contains(&"old".to_owned()));
}

#[test]
fn a_repository_without_commits_has_no_branches() {
    let folder = Folder::new("no-branches");
    git_in(folder.path(), &["init", "-q", "-b", "main"]);

    assert_eq!(
        git::local_branches(folder.path()).expect("branches"),
        Vec::<String>::new()
    );
}

#[test]
fn the_pre_push_hook_is_named_by_its_tool() {
    let repo = repo("hook");
    let hooks = repo.path().join(".git/hooks");
    assert_eq!(git::pre_push_hook(repo.path()), None);

    std::fs::write(hooks.join("pre-push"), "#!/bin/sh\nexit 0\n").expect("hook");
    assert_eq!(git::pre_push_hook(repo.path()), None, "not executable");

    make_executable(&hooks.join("pre-push"), "#!/bin/sh\nexit 0\n");
    assert_eq!(git::pre_push_hook(repo.path()).as_deref(), Some("pre-push"));

    make_executable(
        &hooks.join("pre-push"),
        "#!/bin/sh\ncall_lefthook run \"pre-push\" \"$@\"\n",
    );
    assert_eq!(git::pre_push_hook(repo.path()).as_deref(), Some("lefthook"));

    make_executable(
        &hooks.join("pre-push"),
        "#!/bin/sh\n# husky\n. \"$(dirname \"$0\")/h\"\n",
    );
    assert_eq!(git::pre_push_hook(repo.path()).as_deref(), Some("husky"));
}

#[test]
fn a_linked_worktree_finds_the_hook_in_the_shared_git_folder() {
    let repo = repo("hook-worktree");
    make_executable(
        &repo.path().join(".git/hooks/pre-push"),
        "#!/bin/sh\nexit 0\n",
    );
    let worktree = Folder::new("hook-worktree-linked");
    let linked = worktree.path().join("task");
    git_in(
        repo.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "swarm/task",
            linked.to_str().expect("utf8"),
        ],
    );

    assert_eq!(git::pre_push_hook(&linked).as_deref(), Some("pre-push"));
}

#[test]
fn a_hooks_path_replaces_the_git_hooks_folder() {
    let repo = repo("hook-path");
    make_executable(
        &repo.path().join(".git/hooks/pre-push"),
        "#!/bin/sh\nexit 0\n",
    );
    std::fs::create_dir_all(repo.path().join(".husky/_")).expect("folder");
    git_in(repo.path(), &["config", "core.hooksPath", ".husky/_"]);
    assert_eq!(
        git::pre_push_hook(repo.path()),
        None,
        "the husky folder has no hook yet"
    );

    make_executable(
        &repo.path().join(".husky/_/pre-push"),
        "#!/usr/bin/env sh\n. \"$(dirname \"$0\")/h\"\n",
    );
    assert_eq!(git::pre_push_hook(repo.path()).as_deref(), Some("husky"));

    let elsewhere = Folder::new("hooks-elsewhere");
    make_executable(&elsewhere.path().join("pre-push"), "#!/bin/sh\nexit 0\n");
    git_in(
        repo.path(),
        &[
            "config",
            "core.hooksPath",
            elsewhere.path().to_str().expect("utf8"),
        ],
    );
    assert_eq!(git::pre_push_hook(repo.path()).as_deref(), Some("pre-push"));
}

#[test]
fn the_status_of_a_signed_in_gh_names_the_repository_and_the_commands() {
    let repo = repo("status");
    git_in(
        repo.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widgets.git",
        ],
    );
    let gh = FakeGh::new("status-gh");
    let bin = Folder::new("status-bin");

    let status = github(&gh, bin.path()).status(repo.path());

    assert_eq!(status.gh, Some(gh.path()));
    assert_eq!(status.gh_version.as_deref(), Some("2.62.0"));
    assert!(status.signed_in);
    assert_eq!(status.repository.as_deref(), Some("acme/widgets"));
    assert_eq!(
        status.sign_in,
        [
            gh.path().to_string_lossy().as_ref(),
            "auth",
            "login",
            "--web",
            "--hostname",
            "github.com"
        ]
    );
    assert!(!status.install.is_empty());

    let calls = gh.calls();
    assert!(calls.contains(&["--version".to_owned()].to_vec()));
    assert!(
        calls.contains(
            &["auth", "status", "--hostname", "github.com"]
                .map(str::to_owned)
                .to_vec()
        )
    );
    assert!(gh.logged("PROMPT").iter().all(|value| value == "1"));
}

#[test]
fn a_signed_out_gh_is_installed_but_not_signed_in() {
    let repo = repo("signed-out");
    git_in(
        repo.path(),
        &["remote", "add", "origin", "git@github.com:acme/widgets.git"],
    );
    let gh = FakeGh::new("signed-out-gh");
    gh.fail("auth-status", "You are not logged into any GitHub hosts.");
    let bin = Folder::new("signed-out-bin");

    let status = github(&gh, bin.path()).status(repo.path());

    assert_eq!(status.gh, Some(gh.path()));
    assert!(!status.signed_in);
    assert_eq!(status.repository.as_deref(), Some("acme/widgets"));
}

#[test]
fn a_missing_gh_gives_the_install_and_the_sign_in_it_will_have() {
    let repo = repo("no-gh");
    let bin = Folder::new("no-gh-bin");
    let github = Github::with_folders(
        bin.path().to_path_buf(),
        Some(PathBuf::from("/nonexistent/gh")),
        Vec::new(),
    );

    let status = github.status(repo.path());

    assert_eq!(status.gh, None);
    assert_eq!(status.gh_version, None);
    assert!(!status.signed_in);
    assert_eq!(status.repository, None, "no origin");
    assert_eq!(status.install[..2], ["/bin/sh", "-c"]);
    assert_eq!(status.install[4], bin.path().to_string_lossy());
    assert_eq!(status.sign_in[0], bin.path().join("gh").to_string_lossy());
}

#[test]
fn gh_in_the_swarm_bin_folder_is_found_last() {
    let bin = Folder::new("found-bin");
    let elsewhere = Folder::new("found-elsewhere");
    make_executable(&bin.path().join("gh"), "#!/bin/sh\n");

    let alone = Github::with_folders(
        bin.path().to_path_buf(),
        None,
        vec![elsewhere.path().to_path_buf()],
    );
    assert_eq!(alone.find(), Some(bin.path().join("gh")));

    make_executable(&elsewhere.path().join("gh"), "#!/bin/sh\n");
    assert_eq!(alone.find(), Some(elsewhere.path().join("gh")));
}

#[test]
fn a_gitlab_origin_or_no_origin_has_no_repository() {
    let repo = repo("gitlab");
    let gh = FakeGh::new("gitlab-gh");
    let bin = Folder::new("gitlab-bin");
    let github = github(&gh, bin.path());

    assert_eq!(github.status(repo.path()).repository, None);

    git_in(
        repo.path(),
        &["remote", "add", "origin", "git@gitlab.com:acme/widgets.git"],
    );
    assert_eq!(github.status(repo.path()).repository, None);
}

#[test]
fn homebrew_in_a_searched_folder_decides_the_install() {
    let repo = repo("brew");
    let folder = Folder::new("brew-folder");
    make_executable(&folder.path().join("brew"), "#!/bin/sh\n");
    let bin = Folder::new("brew-bin");
    let github = Github::with_folders(
        bin.path().to_path_buf(),
        Some(PathBuf::from("/nonexistent/gh")),
        vec![folder.path().to_path_buf()],
    );

    let status = github.status(repo.path());

    assert_eq!(
        status.install,
        [
            folder.path().join("brew").to_string_lossy().as_ref(),
            "install",
            "gh"
        ]
    );
    assert_eq!(
        status.sign_in[0],
        folder.path().join("gh").to_string_lossy()
    );
}

/// A bare repository as `origin`, and a clone-like working repository that pushes to it.
fn repo_with_origin(name: &str) -> (Folder, Folder) {
    let origin = Folder::new(&format!("{name}-origin"));
    git_in(origin.path(), &["init", "-q", "--bare", "-b", "main"]);
    let repo = repo(name);
    git_in(
        repo.path(),
        &[
            "remote",
            "add",
            "origin",
            origin.path().to_str().expect("utf8"),
        ],
    );
    git_in(repo.path(), &["switch", "-q", "-c", "swarm/login"]);

    (repo, origin)
}

#[test]
fn pushing_sets_the_upstream_on_origin() {
    let (repo, origin) = repo_with_origin("push");

    git::push_branch(repo.path(), "swarm/login", Duration::from_secs(60)).expect("push");

    let remote = git_in(origin.path(), &["branch", "--list", "swarm/login"]);
    assert!(remote.contains("swarm/login"), "{remote}");
    let upstream = git_in(
        repo.path(),
        &["rev-parse", "--abbrev-ref", "swarm/login@{upstream}"],
    );
    assert_eq!(upstream, "origin/swarm/login");
}

#[test]
fn a_failing_pre_push_hook_puts_its_last_lines_in_the_error() {
    let (repo, origin) = repo_with_origin("push-hook");
    make_executable(
        &repo.path().join(".git/hooks/pre-push"),
        "#!/bin/sh\nfor n in $(seq 1 60); do echo \"hook line $n\"; done\necho 'tests failed' >&2\nexit 1\n",
    );

    let error = git::push_branch(repo.path(), "swarm/login", Duration::from_secs(60))
        .expect_err("the hook fails");

    let message = error.to_string();
    assert!(message.starts_with("git push failed:\n"), "{message}");
    assert!(message.contains("tests failed"), "{message}");
    assert!(message.contains("hook line 60"), "{message}");
    assert!(
        !message.contains("hook line 10\n"),
        "only the last lines: {message}"
    );
    assert!(message.lines().count() <= 41, "{}", message.lines().count());
    let remote = git_in(origin.path(), &["branch", "--list", "swarm/login"]);
    assert!(remote.is_empty(), "nothing was pushed");
}

#[test]
fn a_push_to_a_missing_origin_carries_gits_message() {
    let repo = repo("push-missing");
    git_in(repo.path(), &["switch", "-q", "-c", "swarm/login"]);

    let error = git::push_branch(repo.path(), "swarm/login", Duration::from_secs(60))
        .expect_err("no origin");

    assert!(error.to_string().starts_with("git push failed"), "{error}");
}

#[test]
fn a_push_that_takes_too_long_is_stopped() {
    let (repo, _origin) = repo_with_origin("push-slow");
    make_executable(
        &repo.path().join(".git/hooks/pre-push"),
        "#!/bin/sh\necho slow\nsleep 30\n",
    );

    let started = std::time::Instant::now();
    let error = git::push_branch(repo.path(), "swarm/login", Duration::from_millis(500))
        .expect_err("the push hangs");

    assert!(error.to_string().contains("took longer than"), "{error}");
    assert!(error.to_string().contains("slow"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(15));
}

fn target(worktree: &Path) -> Target<'_> {
    Target {
        worktree,
        repository: "acme/widgets",
    }
}

#[test]
fn creating_a_pull_request_passes_the_body_on_stdin_and_reads_the_link() {
    let gh = FakeGh::new("create");
    gh.answer(
        "pr-create",
        "Creating pull request for swarm/login into main in acme/widgets\n\nhttps://github.com/acme/widgets/pull/42\n",
        0,
    );
    let bin = Folder::new("create-bin");
    let worktree = Folder::new("create-worktree");
    let body = "## What\n\n- a `quoted` \"body\" with $(not run) and 'quotes'\n";

    let link = github(&gh, bin.path())
        .create_pull_request(
            &target(worktree.path()),
            &NewPullRequest {
                base: "main",
                head: "swarm/login",
                title: "Add login; rm -rf /",
                body,
                draft: true,
            },
        )
        .expect("create");

    assert_eq!(
        link,
        PrLink {
            number: 42,
            url: "https://github.com/acme/widgets/pull/42".to_owned()
        }
    );
    assert_eq!(
        gh.calls(),
        [[
            "pr",
            "create",
            "--base",
            "main",
            "--head",
            "swarm/login",
            "--title",
            "Add login; rm -rf /",
            "--body-file",
            "-",
            "--draft"
        ]
        .map(str::to_owned)
        .to_vec()]
    );
    assert_eq!(gh.stdin("pr-create"), body);
    assert_eq!(gh.logged("REPO"), ["acme/widgets"]);
    assert_eq!(gh.logged("PROMPT"), ["1"]);
    assert_eq!(gh.logged("CWD"), [worktree.path().to_string_lossy()]);
}

#[test]
fn a_pull_request_that_is_not_a_draft_leaves_out_the_flag() {
    let gh = FakeGh::new("create-ready");
    gh.answer("pr-create", "https://github.com/acme/widgets/pull/7\n", 0);
    let bin = Folder::new("create-ready-bin");
    let worktree = Folder::new("create-ready-worktree");

    github(&gh, bin.path())
        .create_pull_request(
            &target(worktree.path()),
            &NewPullRequest {
                base: "develop",
                head: "swarm/login",
                title: "T",
                body: "",
                draft: false,
            },
        )
        .expect("create");

    let call = &gh.calls()[0];
    assert!(!call.contains(&"--draft".to_owned()));
    assert_eq!(call[3], "develop");
}

#[test]
fn a_failed_create_carries_ghs_message() {
    let gh = FakeGh::new("create-fail");
    gh.fail(
        "pr-create",
        "a pull request for branch \"swarm/login\" into branch \"main\" already exists:\nhttps://github.com/acme/widgets/pull/9",
    );
    let bin = Folder::new("create-fail-bin");
    let worktree = Folder::new("create-fail-worktree");

    let error = github(&gh, bin.path())
        .create_pull_request(
            &target(worktree.path()),
            &NewPullRequest {
                base: "main",
                head: "swarm/login",
                title: "T",
                body: "B",
                draft: false,
            },
        )
        .expect_err("fails");

    let message = error.to_string();
    assert!(message.starts_with("gh pr create failed: "), "{message}");
    assert!(message.contains("already exists"), "{message}");
}

#[test]
fn a_missing_gh_is_an_error_when_a_call_needs_it() {
    let bin = Folder::new("missing-bin");
    let worktree = Folder::new("missing-worktree");
    let github = Github::with_folders(
        bin.path().to_path_buf(),
        Some(PathBuf::from("/nonexistent/gh")),
        Vec::new(),
    );

    let error = github
        .mark_ready(&target(worktree.path()), 3)
        .expect_err("no gh");

    assert!(error.to_string().contains("not installed"), "{error}");
}

const VIEW: &str = r#"{"number":42,"url":"https://github.com/acme/widgets/pull/42","title":"Add login",
    "state":"OPEN","isDraft":true,"mergeable":"MERGEABLE","mergeStateStatus":"DRAFT",
    "statusCheckRollup":[{"__typename":"CheckRun","name":"build","workflowName":"CI",
    "status":"IN_PROGRESS","conclusion":"","startedAt":"2024-05-01T10:00:00Z",
    "completedAt":"0001-01-01T00:00:00Z","detailsUrl":"https://example.com/1"}],
    "reviewRequests":[{"__typename":"User","login":"alice"}],
    "latestReviews":[{"author":{"login":"bob"},"state":"APPROVED"}]}"#;

#[test]
fn the_status_reads_the_json_of_gh_pr_view() {
    let gh = FakeGh::new("view");
    gh.answer("pr-view", VIEW, 0);
    let bin = Folder::new("view-bin");
    let worktree = Folder::new("view-worktree");

    let pull_request = github(&gh, bin.path())
        .pull_request(&target(worktree.path()), 42)
        .expect("view");

    assert_eq!(
        gh.calls(),
        [[
            "pr",
            "view",
            "42",
            "--json",
            "number,url,title,state,isDraft,mergeable,mergeStateStatus,statusCheckRollup,reviewRequests,latestReviews"
        ]
        .map(str::to_owned)
        .to_vec()]
    );
    assert_eq!(pull_request.number, 42);
    assert!(pull_request.draft);
    assert!(!pull_request.mergeable, "a draft is not mergeable");
    assert_eq!(pull_request.checks[0].state, GithubCheckState::Running);
    assert_eq!(pull_request.reviewers.len(), 2);
    assert!(pull_request.read_at > 0);
}

#[test]
fn marking_ready_and_asking_reviewers_have_the_documented_arguments() {
    let gh = FakeGh::new("ready");
    let bin = Folder::new("ready-bin");
    let worktree = Folder::new("ready-worktree");
    let github = github(&gh, bin.path());

    github
        .mark_ready(&target(worktree.path()), 42)
        .expect("ready");
    github
        .add_reviewers(
            &target(worktree.path()),
            42,
            &["alice".to_owned(), "@bob-2".to_owned()],
        )
        .expect("reviewers");

    assert_eq!(
        gh.calls(),
        [
            vec!["pr", "ready", "42"],
            vec!["pr", "edit", "42", "--add-reviewer", "alice,bob-2"],
        ]
    );
}

#[test]
fn a_reviewer_that_is_not_a_login_never_reaches_gh() {
    let gh = FakeGh::new("bad-login");
    let bin = Folder::new("bad-login-bin");
    let worktree = Folder::new("bad-login-worktree");
    let github = github(&gh, bin.path());

    for bad in ["--assignee", "a b", "", "alice,bob", "-x"] {
        let result = github.add_reviewers(&target(worktree.path()), 1, &[bad.to_owned()]);
        assert!(matches!(result, Err(Error::Invalid(_))), "{bad:?}");
    }
    assert!(
        github
            .add_reviewers(&target(worktree.path()), 1, &[])
            .is_err()
    );

    assert!(gh.calls().is_empty());
}

#[test]
fn each_merge_method_has_its_flag_and_deletes_the_branch() {
    let gh = FakeGh::new("merge");
    let bin = Folder::new("merge-bin");
    let worktree = Folder::new("merge-worktree");
    let github = github(&gh, bin.path());

    for method in [MergeMethod::Squash, MergeMethod::Merge, MergeMethod::Rebase] {
        github
            .merge(&target(worktree.path()), 42, method, "swarm/login")
            .expect("merge");
    }

    assert_eq!(
        gh.calls(),
        [
            vec!["pr", "merge", "42", "--squash", "--delete-branch"],
            vec!["pr", "merge", "42", "--merge", "--delete-branch"],
            vec!["pr", "merge", "42", "--rebase", "--delete-branch"],
        ]
    );
}

#[test]
fn a_merge_whose_branch_cleanup_failed_still_counts_when_github_shows_it_merged() {
    let gh = FakeGh::new("merge-cleanup");
    gh.fail(
        "pr-merge",
        "failed to delete local branch swarm/login: cannot delete branch checked out at /repo/.worktrees/login",
    );
    gh.answer(
        "pr-view",
        r#"{"number":42,"url":"u","title":"t","state":"MERGED","isDraft":false,"mergeable":"UNKNOWN","mergeStateStatus":"UNKNOWN","statusCheckRollup":null,"reviewRequests":[],"latestReviews":[]}"#,
        0,
    );
    let bin = Folder::new("merge-cleanup-bin");
    let worktree = Folder::new("merge-cleanup-worktree");

    github(&gh, bin.path())
        .merge(
            &target(worktree.path()),
            42,
            MergeMethod::Squash,
            "swarm/login",
        )
        .expect("merged");

    let calls = gh.calls();
    assert_eq!(calls[0][..2], ["pr", "merge"]);
    assert_eq!(calls[1][..2], ["pr", "view"]);
    assert_eq!(
        calls[2],
        [
            "api",
            "--method",
            "DELETE",
            "repos/acme/widgets/git/refs/heads/swarm/login"
        ]
    );
}

#[test]
fn a_merge_that_failed_and_is_not_merged_is_an_error() {
    let gh = FakeGh::new("merge-fail");
    gh.fail(
        "pr-merge",
        "Pull request is not mergeable: the base branch policy prohibits the merge.",
    );
    gh.answer(
        "pr-view",
        r#"{"number":42,"url":"u","title":"t","state":"OPEN","isDraft":false,"mergeable":"MERGEABLE","mergeStateStatus":"BLOCKED","statusCheckRollup":[],"reviewRequests":[],"latestReviews":[]}"#,
        0,
    );
    let bin = Folder::new("merge-fail-bin");
    let worktree = Folder::new("merge-fail-worktree");

    let error = github(&gh, bin.path())
        .merge(
            &target(worktree.path()),
            42,
            MergeMethod::Squash,
            "swarm/login",
        )
        .expect_err("not merged");

    assert!(error.to_string().contains("policy prohibits"), "{error}");
    assert!(gh.calls().iter().all(|call| call[0] != "api"));
}

#[test]
fn gh_output_is_cut_to_its_last_lines_in_an_error() {
    let gh = FakeGh::new("long-error");
    let lines = (1..=100)
        .map(|number| format!("line {number}"))
        .collect::<Vec<_>>()
        .join("\n");
    gh.fail("pr-ready", &lines);
    let bin = Folder::new("long-error-bin");
    let worktree = Folder::new("long-error-worktree");

    let error = github(&gh, bin.path())
        .mark_ready(&target(worktree.path()), 1)
        .expect_err("fails");

    let message = error.to_string();
    assert!(message.contains("line 100"), "{message}");
    assert!(!message.contains("line 50\n"), "{message}");
}

/// A fake `curl` that serves the release files from `$FIXTURE` and never goes online.
fn fake_curl(folder: &Path) {
    make_executable(
        &folder.join("curl"),
        r#"#!/bin/sh
out=""
format=""
while [ $# -gt 1 ]; do
  case $1 in
    -o) out=$2; shift 2 ;;
    -w) format=$2; shift 2 ;;
    *) shift ;;
  esac
done
url=$1
case $url in
  */releases/latest)
    if [ -n "$format" ]; then printf 'https://github.com/cli/cli/releases/tag/v9.8.7'; fi
    ;;
  https://github.com/cli/cli/releases/download/v9.8.7/*)
    cp "$FIXTURE/$(basename "$url")" "$out"
    ;;
  *)
    echo "unexpected URL $url" >&2
    exit 22
    ;;
esac
"#,
    );
}

struct Release {
    fixture: Folder,
    fake_bin: Folder,
}

/// A release of a fake `gh 9.8.7` with the checksums file the real releases have.
fn release(name: &str, checksum_matches: bool) -> Release {
    let fixture = Folder::new(&format!("{name}-release"));
    let fake_bin = Folder::new(&format!("{name}-fake"));
    fake_curl(fake_bin.path());

    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        _ => "amd64",
    };
    let stem = format!("gh_9.8.7_macOS_{arch}");
    let asset = format!("{stem}.zip");

    let source = fixture.path().join("source");
    std::fs::create_dir_all(source.join(&stem).join("bin")).expect("folders");
    make_executable(
        &source.join(&stem).join("bin/gh"),
        "#!/bin/sh\necho gh version 9.8.7\n",
    );
    std::fs::write(source.join(&stem).join("LICENSE"), "license\n").expect("license");
    let zipped = Command::new("zip")
        .current_dir(&source)
        .args(["-q", "-r"])
        .arg(fixture.path().join(&asset))
        .arg(&stem)
        .status()
        .expect("run zip");
    assert!(zipped.success());

    let digest = Command::new("shasum")
        .args(["-a", "256"])
        .arg(fixture.path().join(&asset))
        .output()
        .expect("run shasum");
    let digest = String::from_utf8_lossy(&digest.stdout);
    let digest = digest.split_whitespace().next().expect("digest").to_owned();
    let digest = if checksum_matches {
        digest
    } else {
        "0".repeat(64)
    };
    std::fs::write(
        fixture.path().join("gh_9.8.7_checksums.txt"),
        format!("{digest}  {asset}\n{}  other.zip\n", "1".repeat(64)),
    )
    .expect("checksums");

    Release { fixture, fake_bin }
}

fn run_install(release: &Release, bin: &Path) -> std::process::Output {
    let github = Github::with_folders(bin.to_path_buf(), None, Vec::new());
    let install = github.install_command();
    assert_eq!(install[0], "/bin/sh");

    Command::new(&install[0])
        .args(&install[1..])
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", release.fake_bin.path().display()),
        )
        .env("FIXTURE", release.fixture.path())
        .output()
        .expect("run the install")
}

#[test]
fn the_install_script_checks_the_download_and_installs_only_gh() {
    let release = release("install", true);
    let home = Folder::new("install-home");
    let bin = home.path().join("bin");

    let output = run_install(&release, &bin);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut entries: Vec<String> = std::fs::read_dir(&bin)
        .expect("bin folder")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    entries.sort();
    assert_eq!(entries, ["gh"], "nothing else is left in the folder");
    let version = Command::new(bin.join("gh"))
        .output()
        .expect("run the installed gh");
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        "gh version 9.8.7"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Installed gh 9.8.7"));
}

#[test]
fn a_checksum_mismatch_stops_the_install_and_leaves_nothing() {
    let release = release("install-bad", false);
    let home = Folder::new("install-bad-home");
    let bin = home.path().join("bin");

    let output = run_install(&release, &bin);

    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("Checksum mismatch"), "{error}");
    assert!(!bin.join("gh").exists());
    let leftovers = std::fs::read_dir(&bin).expect("bin folder").count();
    assert_eq!(leftovers, 0, "the temporary folder is removed");
}

#[test]
fn a_release_without_a_checksum_for_the_download_stops_the_install() {
    let release = release("install-missing", true);
    std::fs::write(
        release.fixture.path().join("gh_9.8.7_checksums.txt"),
        format!("{}  other.zip\n", "1".repeat(64)),
    )
    .expect("checksums");
    let home = Folder::new("install-missing-home");
    let bin = home.path().join("bin");

    let output = run_install(&release, &bin);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no checksum"));
    assert!(!bin.join("gh").exists());
}

fn project_with_task() -> (Projects, swarm_protocol::TaskInfo, Folder) {
    let repo = repo("approved");
    let store = Arc::new(Mutex::new(Store::open_in_memory().expect("store")));
    let projects = Projects::new(store);
    let project = projects.add(repo.path()).expect("project");
    let task = projects
        .create_task(&CreateTask {
            project: project.id,
            name: "Login".to_owned(),
            from: None,
            run_setup: false,
            orchestrator: None,
            first_message: None,
        })
        .expect("task")
        .task;

    (projects, task, repo)
}

#[test]
fn a_task_counts_as_approved_only_for_the_changes_that_were_approved() {
    let (projects, task, _repo) = project_with_task();
    std::fs::write(task.worktree.join("login.rs"), "fn login() {}\n").expect("file");
    git_in(&task.worktree, &["add", "login.rs"]);
    git_in(&task.worktree, &["commit", "-q", "-m", "login"]);

    let error = projects
        .check_approved(task.id)
        .expect_err("not approved yet");
    assert!(error.to_string().contains("not approved"), "{error}");

    let fingerprint = projects
        .task_diff(task.id, ReviewSource::Merged)
        .expect("diff")
        .fingerprint;
    projects.approve(task.id, fingerprint).expect("approve");
    assert_eq!(
        projects.check_approved(task.id).expect("approved").id,
        task.id
    );

    std::fs::write(task.worktree.join("login.rs"), "fn login() { todo() }\n").expect("edit");
    git_in(&task.worktree, &["commit", "-q", "-am", "more"]);
    let error = projects.check_approved(task.id).expect_err("changed");
    assert!(error.to_string().contains("changed since"), "{error}");
}

#[test]
fn the_pull_request_and_the_after_merge_choice_are_recorded_on_the_task() {
    let (projects, task, _repo) = project_with_task();
    let link = PrLink {
        number: 7,
        url: "https://github.com/acme/widgets/pull/7".to_owned(),
    };

    let saved = projects
        .set_pull_request(task.id, &link, true)
        .expect("save");
    assert_eq!(saved.pull_request, Some(link.clone()));
    assert!(saved.close_after_merge);

    let flipped = projects
        .set_close_after_merge(task.id, false)
        .expect("flag");
    assert_eq!(flipped.pull_request, Some(link));
    assert!(!flipped.close_after_merge);
    assert!(
        projects
            .set_close_after_merge(swarm_protocol::TaskId(999), true)
            .is_err()
    );
}

#[test]
fn a_checks_config_with_a_problem_is_an_error_not_an_empty_list() {
    let (projects, task, repo) = project_with_task();
    assert!(
        projects
            .checks_config(task.project)
            .expect("none")
            .before_pr
            .is_empty()
    );

    std::fs::create_dir_all(repo.path().join(".swarm")).expect("folder");
    std::fs::write(
        repo.path().join(".swarm/config.toml"),
        "[checks]\nbefore_pr = [\"cargo test\"]\nrequired = true\n",
    )
    .expect("config");
    let checks = projects.checks_config(task.project).expect("checks");
    assert_eq!(checks.before_pr, ["cargo test"]);
    assert!(checks.required);

    std::fs::write(repo.path().join(".swarm/config.toml"), "[checks\n").expect("broken");
    assert!(projects.checks_config(task.project).is_err());
}

#[test]
fn a_merged_branch_is_deleted_from_the_project_and_a_missing_one_is_fine() {
    let (projects, task, repo) = project_with_task();
    git_in(
        repo.path(),
        &[
            "worktree",
            "remove",
            "--force",
            task.worktree.to_str().expect("utf8"),
        ],
    );
    assert!(git::branch_exists(repo.path(), &task.branch).expect("exists"));

    projects
        .delete_merged_branch(task.project, &task.branch)
        .expect("delete");
    projects
        .delete_merged_branch(task.project, &task.branch)
        .expect("again");

    assert!(!git::branch_exists(repo.path(), &task.branch).expect("gone"));
}
