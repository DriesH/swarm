//! Runs `Projects` against real temporary git repositories.

#![expect(clippy::expect_used, reason = "test helpers panic on failure")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use swarm_core::git;
use swarm_core::store::Store;
use swarm_core::{Error, Projects};
use swarm_protocol::{CreateTask, ProjectId};

static NEXT_FOLDER: AtomicU32 = AtomicU32::new(0);

const CONFIG: &str = r#"
[worktrees]
ports = { range = "5170-5175", per_worktree = 3 }
copy = [".env", "missing.txt", "../escape"]
setup = "echo ready > setup-ran"
"#;

/// A repository with one commit and a config, removed on drop.
struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(config: &str) -> Self {
        let number = NEXT_FOLDER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("swarm-core-it-{}-{number}", std::process::id()));
        std::fs::create_dir_all(root.join(".swarm")).expect("folders");
        let root = root.canonicalize().expect("real path");
        std::fs::write(root.join(".swarm/config.toml"), config).expect("config");
        std::fs::write(root.join(".env"), "SECRET=1").expect("env");
        std::fs::write(root.join("README.md"), "hello").expect("readme");

        let repo = Self { root };
        repo.git(&["init", "-q"]);
        repo.git(&["add", "README.md"]);
        repo.git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "-m",
            "first",
        ]);

        repo
    }

    fn git(&self, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(arguments)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn commit_all(&self, message: &str) {
        self.git(&["add", "-A", "-f", "--", ".", ":!.swarm"]);
        self.git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            message,
        ]);
    }

    fn branches(&self) -> Vec<String> {
        self.git(&["branch", "--format=%(refname:short)"])
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

/// An empty folder outside any repository, removed on drop.
struct Outside(PathBuf);

impl Outside {
    fn new() -> Self {
        let number = NEXT_FOLDER.fetch_add(1, Ordering::Relaxed);
        let folder =
            std::env::temp_dir().join(format!("swarm-core-out-{}-{number}", std::process::id()));
        std::fs::create_dir_all(&folder).expect("folder");

        Self(folder.canonicalize().expect("real path"))
    }
}

impl Drop for Outside {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn projects() -> Projects {
    let store = Store::open_in_memory().expect("store");

    Projects::new(Arc::new(Mutex::new(store)))
}

fn request(project: ProjectId, name: &str) -> CreateTask {
    CreateTask {
        project,
        name: name.to_owned(),
        from: None,
        run_setup: true,
    }
}

fn exclude_lines(repo: &Repo) -> usize {
    std::fs::read_to_string(repo.root.join(".git/info/exclude"))
        .expect("exclude")
        .lines()
        .filter(|line| *line == "/.worktrees/")
        .count()
}

fn is_dir(path: &Path) -> bool {
    path.is_dir()
}

#[test]
fn adding_a_project_names_it_after_the_folder() {
    let repo = Repo::new(CONFIG);
    let projects = projects();

    let project = projects.add(&repo.root).expect("add");

    assert_eq!(
        project.name,
        repo.root.file_name().expect("name").to_string_lossy()
    );
    assert_eq!(project.path, repo.root);
    assert_eq!(project.config_error, None);
}

#[test]
fn adding_from_a_subfolder_finds_the_root() {
    let repo = Repo::new(CONFIG);
    std::fs::create_dir_all(repo.root.join("src/deep")).expect("folder");
    let projects = projects();

    let project = projects.add(&repo.root.join("src/deep")).expect("add");

    assert_eq!(project.path, repo.root);
}

#[test]
fn adding_twice_returns_the_same_project() {
    let repo = Repo::new(CONFIG);
    let projects = projects();

    let first = projects.add(&repo.root).expect("first");
    let second = projects.add(&repo.root).expect("second");

    assert_eq!(first.id, second.id);
    assert_eq!(projects.list().expect("list").len(), 1);
}

#[test]
fn a_folder_outside_git_is_refused() {
    let folder = std::env::temp_dir().join(format!("swarm-core-nogit-{}", std::process::id()));
    std::fs::create_dir_all(&folder).expect("folder");
    let projects = projects();

    let result = projects.add(&folder);

    std::fs::remove_dir_all(&folder).expect("cleanup");
    assert!(matches!(result, Err(Error::Git { .. })));
}

#[test]
fn a_broken_config_is_reported() {
    let repo = Repo::new("[worktrees\n");
    let projects = projects();

    let project = projects.add(&repo.root).expect("add");

    assert!(project.config_error.is_some());
}

#[test]
fn the_base_branch_is_the_current_branch() {
    let repo = Repo::new(CONFIG);
    let current = repo.git(&["branch", "--show-current"]);

    let base = git::default_base_branch(&repo.root, None);

    assert_eq!(base, current);
}

#[test]
fn a_task_gets_a_branch_a_worktree_and_copied_files() {
    let repo = Repo::new(CONFIG);
    let projects = projects();
    let project = projects.add(&repo.root).expect("add");

    let prepared = projects
        .create_task(&request(project.id, "Fix the login"))
        .expect("task");

    let worktree = repo.root.join(".worktrees/fix-the-login");
    assert_eq!(prepared.task.branch, "swarm/fix-the-login");
    assert_eq!(prepared.task.worktree, worktree);
    assert!(repo.branches().contains(&"swarm/fix-the-login".to_owned()));
    assert!(is_dir(&worktree));
    assert_eq!(
        std::fs::read_to_string(worktree.join(".env")).expect("copied"),
        "SECRET=1"
    );
    assert!(!repo.root.join("..").join("escape").exists());
}

#[test]
fn the_worktree_folder_is_excluded_once() {
    let repo = Repo::new(CONFIG);
    let projects = projects();
    let project = projects.add(&repo.root).expect("add");

    projects
        .create_task(&request(project.id, "one"))
        .expect("first");
    projects
        .create_task(&request(project.id, "two"))
        .expect("second");

    assert_eq!(exclude_lines(&repo), 1);
    assert_eq!(repo.git(&["status", "--porcelain", "--", ".worktrees"]), "");
}

#[test]
fn the_task_keeps_its_ports_and_environment() {
    let repo = Repo::new(CONFIG);
    let projects = projects();
    let project = projects.add(&repo.root).expect("add");

    let prepared = projects
        .create_task(&request(project.id, "one"))
        .expect("task");

    assert_eq!(prepared.task.ports, [5170, 5171, 5172]);
    assert_eq!(prepared.setup.as_deref(), Some("echo ready > setup-ran"));
    assert!(
        prepared
            .env
            .contains(&("SWARM_TASK".to_owned(), prepared.task.id.0.to_string()))
    );
    assert!(
        prepared
            .env
            .contains(&("SWARM_PORT".to_owned(), "5170".to_owned()))
    );
    assert!(
        prepared
            .env
            .contains(&("SWARM_PORTS".to_owned(), "5170,5171,5172".to_owned()))
    );
}

#[test]
fn the_second_task_gets_the_next_port_block() {
    let repo = Repo::new(CONFIG);
    let projects = projects();
    let project = projects.add(&repo.root).expect("add");
    projects
        .create_task(&request(project.id, "one"))
        .expect("first");

    let second = projects
        .create_task(&request(project.id, "two"))
        .expect("second");

    assert_eq!(second.task.ports, [5173, 5174, 5175]);
}

#[test]
fn the_same_name_gets_a_numbered_branch() {
    let repo = Repo::new(CONFIG);
    let projects = projects();
    let project = projects.add(&repo.root).expect("add");
    projects
        .create_task(&request(project.id, "Same"))
        .expect("first");

    let second = projects
        .create_task(&request(project.id, "same"))
        .expect("second");

    assert_eq!(second.task.branch, "swarm/same-2");
    assert!(is_dir(&repo.root.join(".worktrees/same-2")));
}

#[test]
fn no_setup_is_returned_when_not_asked_for() {
    let repo = Repo::new(CONFIG);
    let projects = projects();
    let project = projects.add(&repo.root).expect("add");
    let mut task = request(project.id, "one");
    task.run_setup = false;

    let prepared = projects.create_task(&task).expect("task");

    assert_eq!(prepared.setup, None);
}

#[test]
fn a_task_can_start_from_another_branch() {
    let repo = Repo::new(CONFIG);
    repo.git(&["branch", "release"]);
    let projects = projects();
    let project = projects.add(&repo.root).expect("add");
    let mut task = request(project.id, "one");
    task.from = Some("release".to_owned());

    let prepared = projects.create_task(&task).expect("task");

    assert_eq!(prepared.task.base, "release");
}

#[test]
fn tasks_are_listed_under_their_project() {
    let repo = Repo::new(CONFIG);
    let projects = projects();
    let project = projects.add(&repo.root).expect("add");
    projects
        .create_task(&request(project.id, "one"))
        .expect("task");

    let listed = projects.list().expect("list");

    assert_eq!(listed[0].tasks.len(), 1);
}

#[test]
fn a_full_port_range_fails_before_any_checkout() {
    let repo = Repo::new(CONFIG);
    let projects = projects();
    let project = projects.add(&repo.root).expect("add");
    projects
        .create_task(&request(project.id, "one"))
        .expect("first");
    projects
        .create_task(&request(project.id, "two"))
        .expect("second");

    let result = projects.create_task(&request(project.id, "three"));

    assert!(matches!(result, Err(Error::NoFreePorts)));
    assert!(!repo.branches().contains(&"swarm/three".to_owned()));
    assert!(!repo.root.join(".worktrees/three").exists());
    assert_eq!(projects.list().expect("list")[0].tasks.len(), 2);
}

#[test]
fn ports_are_unique_across_projects() {
    let first_repo = Repo::new(CONFIG);
    let second_repo = Repo::new(CONFIG);
    let projects = projects();
    let first = projects.add(&first_repo.root).expect("add");
    let second = projects.add(&second_repo.root).expect("add");
    projects
        .create_task(&request(first.id, "one"))
        .expect("first");

    let prepared = projects
        .create_task(&request(second.id, "one"))
        .expect("second");

    assert_eq!(prepared.task.ports, [5173, 5174, 5175]);
}

#[test]
fn a_failure_after_the_worktree_leaves_nothing_behind() {
    let repo = Repo::new(CONFIG);
    std::fs::remove_file(repo.root.join(".env")).expect("remove");
    repo.git(&["checkout", "-q", "-b", "release"]);
    std::fs::create_dir_all(repo.root.join(".env")).expect("folder");
    std::fs::write(repo.root.join(".env/inner"), "x").expect("file");
    repo.commit_all("folder named env");
    repo.git(&["checkout", "-q", "-"]);
    std::fs::remove_dir_all(repo.root.join(".env")).ok();
    std::fs::write(repo.root.join(".env"), "SECRET=1").expect("env");
    let projects = projects();
    let project = projects.add(&repo.root).expect("add");
    let mut task = request(project.id, "one");
    task.from = Some("release".to_owned());

    let result = projects.create_task(&task);

    assert!(matches!(result, Err(Error::Io { .. })), "{result:?}");
    assert!(!repo.branches().contains(&"swarm/one".to_owned()));
    assert!(!repo.root.join(".worktrees/one").exists());
    assert!(projects.list().expect("list")[0].tasks.is_empty());
}

#[test]
fn a_broken_config_stops_task_creation() {
    let repo = Repo::new("[worktrees\n");
    let projects = projects();
    let project = projects.add(&repo.root).expect("add");

    let result = projects.create_task(&request(project.id, "one"));

    let Err(Error::Invalid(message)) = result else {
        panic!("expected a config error, got {result:?}");
    };
    assert!(message.contains("config"), "{message}");
    assert!(!repo.root.join(".worktrees").exists());
}

#[test]
fn a_start_point_that_looks_like_an_option_is_refused() {
    let repo = Repo::new(CONFIG);
    let projects = projects();
    let project = projects.add(&repo.root).expect("add");
    let mut task = request(project.id, "one");
    task.from = Some("--no-checkout".to_owned());

    let result = projects.create_task(&task);

    assert!(matches!(result, Err(Error::Invalid(_))), "{result:?}");
    assert!(!repo.branches().contains(&"swarm/one".to_owned()));
    assert!(!repo.root.join(".worktrees/one").exists());
}

#[test]
fn a_start_point_that_is_not_a_commit_is_refused() {
    let repo = Repo::new(CONFIG);
    let projects = projects();
    let project = projects.add(&repo.root).expect("add");
    let mut task = request(project.id, "one");
    task.from = Some("no-such-branch".to_owned());

    let result = projects.create_task(&task);

    assert!(matches!(result, Err(Error::Invalid(_))), "{result:?}");
    assert!(!repo.root.join(".worktrees/one").exists());
}

#[test]
fn a_worktree_folder_that_is_a_symlink_out_of_the_project_is_refused() {
    let repo = Repo::new(CONFIG);
    let outside = Outside::new();
    std::os::unix::fs::symlink(&outside.0, repo.root.join(".worktrees")).expect("link");
    repo.commit_all("link");
    let projects = projects();
    let project = projects.add(&repo.root).expect("add");

    let result = projects.create_task(&request(project.id, "one"));

    assert!(matches!(result, Err(Error::Invalid(_))), "{result:?}");
    assert_eq!(std::fs::read_dir(&outside.0).expect("read").count(), 0);
    assert!(!repo.branches().contains(&"swarm/one".to_owned()));
}

#[test]
fn a_copy_does_not_write_through_a_symlinked_folder() {
    let repo = Repo::new(CONFIG);
    let outside = Outside::new();
    repo.git(&["checkout", "-q", "-b", "release"]);
    std::os::unix::fs::symlink(&outside.0, repo.root.join("data")).expect("link");
    repo.commit_all("link");
    repo.git(&["checkout", "-q", "-"]);
    std::fs::create_dir_all(repo.root.join("data")).expect("folder");
    std::fs::write(repo.root.join("data/key.env"), "x").expect("file");
    let config = CONFIG.replace("[\".env\"", "[\"data/key.env\"");
    std::fs::write(repo.root.join(".swarm/config.toml"), config).expect("config");
    let projects = projects();
    let project = projects.add(&repo.root).expect("add");
    let mut task = request(project.id, "one");
    task.from = Some("release".to_owned());

    projects.create_task(&task).expect("task");

    assert_eq!(std::fs::read_dir(&outside.0).expect("read").count(), 0);
}

#[test]
fn a_copy_does_not_write_through_a_symlinked_file() {
    let repo = Repo::new(CONFIG);
    let outside = Outside::new();
    let target = outside.0.join("target");
    std::fs::write(&target, "old").expect("target");
    std::fs::remove_file(repo.root.join(".env")).expect("remove");
    repo.git(&["checkout", "-q", "-b", "release"]);
    std::os::unix::fs::symlink(&target, repo.root.join(".env")).expect("link");
    repo.commit_all("link");
    repo.git(&["checkout", "-q", "-"]);
    std::fs::write(repo.root.join(".env"), "SECRET=1").expect("env");
    let projects = projects();
    let project = projects.add(&repo.root).expect("add");
    let mut task = request(project.id, "one");
    task.from = Some("release".to_owned());

    projects.create_task(&task).expect("task");

    assert_eq!(std::fs::read_to_string(&target).expect("read"), "old");
}

#[test]
fn an_unknown_project_is_an_error() {
    let projects = projects();

    let result = projects.create_task(&request(ProjectId(99), "one"));

    assert!(matches!(result, Err(Error::UnknownProject(99))));
}

#[test]
fn a_name_without_letters_is_an_error() {
    let repo = Repo::new(CONFIG);
    let projects = projects();
    let project = projects.add(&repo.root).expect("add");

    let result = projects.create_task(&request(project.id, "???"));

    assert!(matches!(result, Err(Error::Invalid(_))));
}
