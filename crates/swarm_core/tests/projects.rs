//! Runs `Projects` against real temporary git repositories.

#![expect(clippy::expect_used, reason = "test helpers panic on failure")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use swarm_core::git;
use swarm_core::store::{NewAgent, Store};
use swarm_core::{Error, Projects, WorkerCheckout};
use swarm_protocol::{
    AgentChange, AgentId, AgentInfo, AgentKind, AgentRole, CreateTask, DiffStat, FileStat,
    ProjectId, TaskInfo,
};

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
        orchestrator: None,
        first_message: None,
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
fn a_blank_configured_base_branch_counts_as_the_default_branch() {
    let repo = Repo::new("[worktrees]\nbase_branch = \"  \"\n");
    let current = repo.git(&["branch", "--show-current"]);
    let projects = projects();

    let project = projects.add(&repo.root).expect("add");
    let prepared = projects
        .create_task(&request(project.id, "Blank base"))
        .expect("task");

    assert_eq!(git::default_base_branch(&repo.root, Some(" \t")), current);
    assert_eq!(project.base_branch, current);
    assert_eq!(prepared.task.base, current);
    assert_eq!(prepared.task.pr_base, current);
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

fn shared_projects() -> (Projects, Arc<Mutex<Store>>) {
    let store = Arc::new(Mutex::new(Store::open_in_memory().expect("store")));

    (Projects::new(Arc::clone(&store)), store)
}

fn register_worker(
    store: &Arc<Mutex<Store>>,
    checkout: &WorkerCheckout,
) -> Result<AgentInfo, Error> {
    store.lock().expect("lock").add_agent(&NewAgent {
        project: checkout.project,
        task: Some(checkout.task),
        role: AgentRole::Worker,
        kind: AgentKind::ClaudeCode,
        model: "sonnet",
        session_id: None,
        name: &checkout.name,
        worktree: &checkout.worktree,
        branch: &checkout.branch,
        ports: &checkout.ports,
    })
}

fn task_with_a_commit(projects: &Projects, repo: &Repo) -> TaskInfo {
    let project = projects.add(&repo.root).expect("add");
    let task = projects
        .create_task(&request(project.id, "Login"))
        .expect("task")
        .task;
    std::fs::write(task.worktree.join("task-file"), "from the task").expect("file");
    git_in(&task.worktree, &["add", "task-file"]);
    git_in(&task.worktree, &["commit", "-q", "-m", "task commit"]);

    task
}

fn git_in(folder: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(folder)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
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

#[test]
fn a_worker_gets_a_branch_from_the_task_branch_and_its_own_worktree() {
    let repo = Repo::new(CONFIG);
    let (projects, store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);

    let prepared = projects
        .create_worker(&task, "API Layer", |checkout| {
            register_worker(&store, checkout)
        })
        .expect("worker");

    assert_eq!(prepared.agent.name, "api-layer");
    assert_eq!(prepared.agent.branch, "swarm/login--api-layer");
    assert_eq!(
        prepared.agent.worktree,
        repo.root.join(".worktrees/login--api-layer")
    );
    assert_eq!(prepared.agent.role, AgentRole::Worker);
    assert!(
        repo.branches()
            .contains(&"swarm/login--api-layer".to_owned())
    );
    assert!(prepared.agent.worktree.join("task-file").is_file());
    assert_eq!(
        std::fs::read_to_string(prepared.agent.worktree.join(".env")).expect("copied"),
        "SECRET=1"
    );
    assert_eq!(prepared.setup.as_deref(), Some("echo ready > setup-ran"));
}

#[test]
fn worker_ports_do_not_overlap_the_task_or_other_workers() {
    let repo = Repo::new("[worktrees]\nports = { range = \"5170-5179\", per_worktree = 3 }\n");
    let (projects, store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);
    assert_eq!(task.ports, [5170, 5171, 5172]);

    let first = projects
        .create_worker(&task, "one", |checkout| register_worker(&store, checkout))
        .expect("first");
    let second = projects
        .create_worker(&task, "two", |checkout| register_worker(&store, checkout))
        .expect("second");

    let ports = |env: &[(String, String)]| {
        env.iter()
            .find(|(name, _)| name == "SWARM_PORTS")
            .map(|(_, value)| value.clone())
    };
    assert_eq!(ports(&first.env).as_deref(), Some("5173,5174,5175"));
    assert_eq!(ports(&second.env).as_deref(), Some("5176,5177,5178"));
}

#[test]
fn a_worker_name_is_unique_within_its_task() {
    let repo = Repo::new("");
    let (projects, store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);
    projects
        .create_worker(&task, "api", |checkout| register_worker(&store, checkout))
        .expect("first");

    let result =
        projects.create_worker(&task, "API!", |checkout| register_worker(&store, checkout));

    assert!(
        matches!(&result, Err(Error::Invalid(message)) if message.contains("already")),
        "{result:?}"
    );
}

#[test]
fn a_worker_name_without_letters_is_refused() {
    let repo = Repo::new("");
    let (projects, store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);

    let result = projects.create_worker(&task, "!!!", |checkout| register_worker(&store, checkout));

    assert!(
        matches!(&result, Err(Error::Invalid(message)) if message.contains("worker name")),
        "{result:?}"
    );
}

#[test]
fn a_failed_registration_leaves_no_worker_behind() {
    let repo = Repo::new(CONFIG);
    let (projects, _store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);

    let result = projects.create_worker(&task, "api", |_| {
        Err(Error::Invalid("cannot record".to_owned()))
    });

    assert!(result.is_err());
    assert!(!repo.branches().contains(&"swarm/login--api".to_owned()));
    assert!(!repo.root.join(".worktrees/login--api").exists());
}

#[test]
fn merging_a_worker_branch_makes_a_merge_commit() {
    let repo = Repo::new("");
    let (projects, store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);
    let worker = projects
        .create_worker(&task, "api", |checkout| register_worker(&store, checkout))
        .expect("worker")
        .agent;
    std::fs::write(worker.worktree.join("api.rs"), "fn api() {}").expect("file");
    git_in(&worker.worktree, &["add", "api.rs"]);
    git_in(&worker.worktree, &["commit", "-q", "-m", "api"]);

    let outcome = git::merge_branch(&task.worktree, &worker.branch).expect("merge");

    assert!(outcome.merged, "{outcome:?}");
    assert!(outcome.conflicts.is_empty());
    assert!(task.worktree.join("api.rs").is_file());
    let parents = git_in(&task.worktree, &["log", "-1", "--format=%p"]);
    assert_eq!(
        parents.split(' ').count(),
        2,
        "a merge commit has two parents"
    );
}

#[test]
fn a_conflicting_merge_is_an_outcome_and_stays_open() {
    let repo = Repo::new("");
    let (projects, store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);
    let worker = projects
        .create_worker(&task, "api", |checkout| register_worker(&store, checkout))
        .expect("worker")
        .agent;
    std::fs::write(worker.worktree.join("README.md"), "worker text").expect("file");
    git_in(&worker.worktree, &["commit", "-q", "-am", "worker change"]);
    std::fs::write(task.worktree.join("README.md"), "task text").expect("file");
    git_in(&task.worktree, &["commit", "-q", "-am", "task change"]);

    let outcome = git::merge_branch(&task.worktree, &worker.branch).expect("merge");

    assert!(!outcome.merged);
    assert_eq!(outcome.conflicts, [PathBuf::from("README.md")]);
    assert!(outcome.output.contains("CONFLICT"), "{}", outcome.output);
    assert!(task.worktree.join("README.md").is_file());
    let status = git_in(&task.worktree, &["status", "--porcelain"]);
    assert!(status.contains("UU README.md"), "{status}");
}

#[test]
fn a_merge_of_a_missing_branch_is_an_error() {
    let repo = Repo::new("");
    let (projects, _store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);

    let result = git::merge_branch(&task.worktree, "swarm/nothing");

    assert!(result.is_err());
}

#[test]
fn a_branch_that_looks_like_an_option_is_not_read_as_one() {
    let repo = Repo::new("");
    let (projects, _store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);

    let result = git::merge_branch(&task.worktree, "--abort");

    assert!(result.is_err());
}

#[test]
fn a_linked_worktree_lists_only_the_git_folders_a_commit_writes() {
    let repo = Repo::new("");
    let (projects, store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);
    let worker = projects
        .create_worker(&task, "api", |checkout| register_worker(&store, checkout))
        .expect("worker")
        .agent;

    let folders = git::writable_git_dirs(&worker.worktree).expect("folders");

    assert_eq!(
        folders,
        [
            repo.root.join(".git/objects"),
            repo.root.join(".git/refs"),
            repo.root.join(".git/logs"),
            repo.root.join(".git/worktrees/login--api")
        ]
    );
    assert!(git::writable_git_dirs(&repo.root).expect("root").is_empty());
}

#[test]
fn a_discarded_worker_leaves_no_branch_folder_or_ports() {
    let repo = Repo::new(CONFIG);
    let (projects, store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);
    let worker = projects
        .create_worker(&task, "api", |checkout| register_worker(&store, checkout))
        .expect("worker")
        .agent;

    projects.discard_worker(&worker);

    assert!(!repo.branches().contains(&"swarm/login--api".to_owned()));
    assert!(!worker.worktree.exists());
    assert!(
        store
            .lock()
            .expect("store")
            .agent(worker.id)
            .expect("read")
            .is_none()
    );
    let again = projects
        .create_worker(&task, "api", |checkout| register_worker(&store, checkout))
        .expect("the name and ports are free again")
        .agent;
    assert_eq!(again.worktree, worker.worktree);
}

#[test]
fn a_discarded_task_leaves_no_branch_folder_or_row() {
    let repo = Repo::new(CONFIG);
    let (projects, store) = shared_projects();
    let project = projects.add(&repo.root).expect("add");
    let task = projects
        .create_task(&request(project.id, "login"))
        .expect("task")
        .task;

    projects.discard_task(&task);

    assert!(!repo.branches().contains(&task.branch));
    assert!(!task.worktree.exists());
    assert!(
        store
            .lock()
            .expect("store")
            .task(task.id)
            .expect("read")
            .is_none()
    );
}

#[test]
fn a_task_made_by_an_agent_records_it() {
    let repo = Repo::new("");
    let (projects, store) = shared_projects();
    let project = projects.add(&repo.root).expect("add");

    let task = projects
        .create_task_by(&request(project.id, "login"), Some(AgentId(9)))
        .expect("task")
        .task;

    assert_eq!(task.created_by, Some(AgentId(9)));
    let stored = store
        .lock()
        .expect("store")
        .task(task.id)
        .expect("read")
        .expect("task");
    assert_eq!(stored.created_by, Some(AgentId(9)));
}

/// The fixture's config and secret are untracked; keep them out of the counts.
fn ignoring_setup_files() -> Repo {
    let repo = Repo::new("");
    std::fs::write(repo.root.join(".git/info/exclude"), ".swarm\n.env\n").expect("exclude");

    repo
}

#[test]
fn the_diff_stat_counts_commits_edits_untracked_and_binary_files() {
    let repo = ignoring_setup_files();
    let base = repo.git(&["rev-parse", "HEAD"]);
    assert_eq!(
        git::diff_stat(&repo.root, &base).expect("clean"),
        DiffStat::default()
    );

    std::fs::write(repo.root.join("committed.txt"), "a\nb\nc\n").expect("file");
    git_in(&repo.root, &["add", "committed.txt"]);
    git_in(&repo.root, &["commit", "-q", "-m", "add a file"]);
    std::fs::write(repo.root.join("README.md"), "hello\nworld\n").expect("edit");
    std::fs::write(repo.root.join("new.txt"), "one\ntwo").expect("untracked");
    std::fs::write(repo.root.join("blob.bin"), [0_u8, 1, 2, 3]).expect("binary");

    let stat = git::diff_stat(&repo.root, &base).expect("stat");

    assert_eq!(
        stat,
        DiffStat {
            files: 4,
            additions: 3 + 2 + 2,
            deletions: 1,
        }
    );
}

fn file_stat(path: &str, additions: u32) -> FileStat {
    FileStat {
        path: path.into(),
        additions,
        deletions: 0,
    }
}

#[test]
fn the_file_list_puts_the_most_changed_files_first() {
    let repo = ignoring_setup_files();
    let base = repo.git(&["rev-parse", "HEAD"]);

    std::fs::write(repo.root.join("README.md"), "hello\nworld\n").expect("edit");
    std::fs::write(repo.root.join("big.txt"), "1\n2\n3\n4\n5\n").expect("untracked");
    std::fs::write(repo.root.join("small.txt"), "1\n").expect("untracked");
    std::fs::write(repo.root.join("blob.bin"), [0_u8, 1]).expect("binary");

    let (stat, files) = git::diff_files(&repo.root, &base).expect("files");
    let listed: Vec<(String, u32, u32)> = files
        .iter()
        .map(|file| {
            (
                file.path.display().to_string(),
                file.additions,
                file.deletions,
            )
        })
        .collect();

    assert_eq!(stat.files, 4);
    assert_eq!(
        listed,
        [
            ("big.txt".to_owned(), 5, 0),
            ("README.md".to_owned(), 2, 1),
            ("small.txt".to_owned(), 1, 0),
            ("blob.bin".to_owned(), 0, 0),
        ]
    );
}

#[test]
fn a_file_removed_from_the_index_but_kept_on_disk_is_one_entry_that_matches_the_totals() {
    let repo = ignoring_setup_files();

    std::fs::write(repo.root.join("README.md"), "a\nb\nc\n").expect("edit");
    git_in(&repo.root, &["add", "README.md"]);
    git_in(&repo.root, &["commit", "-q", "-m", "edit"]);
    let base_after_edit = repo.git(&["rev-parse", "HEAD"]);
    git_in(&repo.root, &["rm", "-q", "--cached", "README.md"]);

    let (stat, files) = git::diff_files(&repo.root, &base_after_edit).expect("files");
    let listed_additions: u32 = files.iter().map(|file| file.additions).sum();
    let listed_deletions: u32 = files.iter().map(|file| file.deletions).sum();

    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(stat.files, 1);
    assert_eq!(listed_additions, stat.additions);
    assert_eq!(listed_deletions, stat.deletions);
    assert_eq!((stat.additions, stat.deletions), (3, 3));
}

#[test]
fn the_file_list_stops_at_the_cap_but_the_totals_count_every_file() {
    let repo = ignoring_setup_files();
    let base = repo.git(&["rev-parse", "HEAD"]);

    for index in 0..(swarm_protocol::FILES_LISTED + 20) {
        std::fs::write(repo.root.join(format!("file-{index:04}.txt")), "x\n").expect("file");
    }
    std::fs::write(repo.root.join("large.txt"), "x\ny\nz\n").expect("file");

    let (stat, files) = git::diff_files(&repo.root, &base).expect("files");

    assert_eq!(files.len(), swarm_protocol::FILES_LISTED);
    assert_eq!(stat.files as usize, swarm_protocol::FILES_LISTED + 21);
    assert_eq!(files[0].path.display().to_string(), "large.txt");
}

#[test]
fn a_tracked_binary_counts_as_a_file_without_lines() {
    let repo = ignoring_setup_files();
    let base = repo.git(&["rev-parse", "HEAD"]);

    std::fs::write(repo.root.join("image.bin"), [0_u8, 255, 0, 255]).expect("binary");
    git_in(&repo.root, &["add", "image.bin"]);
    git_in(&repo.root, &["commit", "-q", "-m", "add a binary"]);

    let stat = git::diff_stat(&repo.root, &base).expect("stat");

    assert_eq!(
        stat,
        DiffStat {
            files: 1,
            additions: 0,
            deletions: 0,
        }
    );
}

#[test]
fn a_large_untracked_file_counts_without_lines() {
    let repo = ignoring_setup_files();
    let base = repo.git(&["rev-parse", "HEAD"]);

    std::fs::write(repo.root.join("big.txt"), "x\n".repeat(600_000)).expect("big");

    let stat = git::diff_stat(&repo.root, &base).expect("stat");

    assert_eq!((stat.files, stat.additions), (1, 0));
}

#[test]
fn a_worker_is_measured_from_where_it_branched() {
    let repo = Repo::new("");
    let (projects, store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);
    let worker = projects
        .create_worker(&task, "api", |checkout| register_worker(&store, checkout))
        .expect("worker")
        .agent;
    let branched = repo.git(&["rev-parse", &task.branch]);

    std::fs::write(worker.worktree.join("work.txt"), "1\n2\n").expect("work");
    git_in(&worker.worktree, &["add", "work.txt"]);
    git_in(&worker.worktree, &["commit", "-q", "-m", "work"]);
    std::fs::write(worker.worktree.join("loose.txt"), "x\n").expect("loose");

    let recorded = store.lock().expect("lock").base_commit(worker.id);
    let changes = projects.agent_changes(&[worker.id, AgentId(9999)]);

    assert_eq!(recorded.expect("base").as_deref(), Some(branched.as_str()));
    assert_eq!(
        changes,
        vec![AgentChange {
            agent: worker.id,
            stat: DiffStat {
                files: 2,
                additions: 3,
                deletions: 0,
            },
            files: vec![file_stat("work.txt", 2), file_stat("loose.txt", 1)],
        }]
    );
}

#[test]
fn a_worker_with_a_missing_worktree_is_left_out() {
    let repo = Repo::new("");
    let (projects, store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);
    let worker = projects
        .create_worker(&task, "api", |checkout| register_worker(&store, checkout))
        .expect("worker")
        .agent;

    std::fs::remove_dir_all(&worker.worktree).expect("remove");

    assert!(projects.agent_changes(&[worker.id]).is_empty());
}

/// Runs `work` on a thread and fails instead of hanging when it never returns.
fn within_seconds<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || sender.send(work()).ok());

    receiver
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("the diff stat hung")
}

#[test]
fn an_untracked_symlink_to_a_fifo_does_not_block() {
    let repo = ignoring_setup_files();
    let base = repo.git(&["rev-parse", "HEAD"]);
    let fifo = repo.root.join("pipe");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo")
            .success()
    );
    std::os::unix::fs::symlink(&fifo, repo.root.join("link-to-pipe")).expect("link");

    let root = repo.root.clone();
    let stat = within_seconds(move || git::diff_stat(&root, &base).expect("stat"));

    assert_eq!((stat.files, stat.additions), (1, 1));
}

#[test]
fn an_untracked_symlink_to_dev_zero_does_not_read_forever() {
    let repo = ignoring_setup_files();
    let base = repo.git(&["rev-parse", "HEAD"]);
    std::os::unix::fs::symlink("/dev/zero", repo.root.join("zeros")).expect("link");

    let root = repo.root.clone();
    let stat = within_seconds(move || git::diff_stat(&root, &base).expect("stat"));

    assert_eq!((stat.files, stat.additions), (1, 1));
}

#[test]
fn a_symlink_counts_as_one_file_with_one_line() {
    let repo = ignoring_setup_files();
    let base = repo.git(&["rev-parse", "HEAD"]);
    std::fs::write(repo.root.join("target.txt"), "a\nb\nc\n").expect("target");
    std::os::unix::fs::symlink("target.txt", repo.root.join("alias")).expect("link");

    let stat = git::diff_stat(&repo.root, &base).expect("stat");

    assert_eq!((stat.files, stat.additions), (2, 3 + 1));
}

#[test]
fn the_diff_stat_does_not_touch_the_index() {
    let repo = ignoring_setup_files();
    let base = repo.git(&["rev-parse", "HEAD"]);
    let readme = repo.root.join("README.md");
    let earlier = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&readme)
        .and_then(|file| file.set_modified(earlier))
        .expect("new mtime, same content");
    let index = repo.root.join(".git/index");
    let before = (
        std::fs::read(&index).expect("index"),
        std::fs::metadata(&index)
            .and_then(|meta| meta.modified())
            .expect("mtime"),
    );

    let stat = git::diff_stat(&repo.root, &base).expect("stat");

    let after = (
        std::fs::read(&index).expect("index"),
        std::fs::metadata(&index)
            .and_then(|meta| meta.modified())
            .expect("mtime"),
    );
    assert_eq!(stat, DiffStat::default());
    assert!(before == after, "the index was rewritten");
}

#[test]
fn untracked_files_past_the_count_limit_have_no_line_count() {
    let repo = ignoring_setup_files();
    let base = repo.git(&["rev-parse", "HEAD"]);
    std::fs::create_dir(repo.root.join("many")).expect("folder");
    for number in 0..2100 {
        std::fs::write(repo.root.join(format!("many/{number:05}")), "x\n").expect("file");
    }

    let stat = git::diff_stat(&repo.root, &base).expect("stat");

    assert_eq!((stat.files, stat.additions), (2100, 2000));
}

#[test]
fn untracked_files_past_the_file_limit_are_not_listed() {
    let repo = ignoring_setup_files();
    let base = repo.git(&["rev-parse", "HEAD"]);
    std::fs::create_dir(repo.root.join("many")).expect("folder");
    for number in 0..10_050 {
        std::fs::write(repo.root.join(format!("many/{number:05}")), "").expect("file");
    }

    let stat = git::diff_stat(&repo.root, &base).expect("stat");

    assert_eq!(stat.files, 10_000);
}

#[test]
fn untracked_files_stop_counting_lines_after_the_byte_budget() {
    let repo = ignoring_setup_files();
    let base = repo.git(&["rev-parse", "HEAD"]);
    let megabyte = "x\n".repeat(1 << 19);
    for number in 0..34 {
        std::fs::write(repo.root.join(format!("big-{number:02}.txt")), &megabyte).expect("file");
    }

    let stat = git::diff_stat(&repo.root, &base).expect("stat");

    assert_eq!(stat.files, 34);
    assert_eq!(stat.additions, 32 * (1 << 19));
}

#[test]
fn paths_with_a_tab_and_a_newline_are_counted_once() {
    let repo = ignoring_setup_files();
    let base = repo.git(&["rev-parse", "HEAD"]);
    std::fs::write(repo.root.join("tracked\tname\nhere.txt"), "a\nb\n").expect("tracked");
    git_in(&repo.root, &["add", "--all"]);
    git_in(&repo.root, &["commit", "-q", "-m", "odd name"]);
    std::fs::write(repo.root.join("loose\tname\nhere.txt"), "c\n").expect("loose");

    let stat = git::diff_stat(&repo.root, &base).expect("stat");

    assert_eq!(
        stat,
        DiffStat {
            files: 2,
            additions: 3,
            deletions: 0,
        }
    );
}

#[test]
fn a_merge_base_ref_that_looks_like_an_option_is_not_an_option() {
    let repo = ignoring_setup_files();

    assert!(git::merge_base(&repo.root, "--all").is_err());
}

#[test]
fn a_worker_without_a_base_commit_is_measured_from_the_task_branch() {
    let repo = Repo::new("");
    let (projects, store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);
    let worktree = task.worktree.with_file_name("old-worker");
    git_in(
        &repo.root,
        &[
            "worktree",
            "add",
            "-b",
            "old-worker-branch",
            "--end-of-options",
            worktree.to_str().expect("path"),
            &task.branch,
        ],
    );
    let worker = store
        .lock()
        .expect("lock")
        .add_agent(&NewAgent {
            project: task.project,
            task: Some(task.id),
            role: AgentRole::Worker,
            kind: AgentKind::ClaudeCode,
            model: "sonnet",
            session_id: None,
            name: "old",
            worktree: &worktree,
            branch: "old-worker-branch",
            ports: &[],
        })
        .expect("agent");
    std::fs::write(worktree.join("work.txt"), "1\n2\n").expect("work");

    let changes = projects.agent_changes(&[worker.id]);

    assert_eq!(
        changes,
        vec![AgentChange {
            agent: worker.id,
            stat: DiffStat {
                files: 1,
                additions: 2,
                deletions: 0,
            },
            files: vec![file_stat("work.txt", 2)],
        }]
    );
}

fn with_identity(repo: &Repo) {
    repo.git(&["config", "user.name", "Dries"]);
    repo.git(&["config", "user.email", "dries@example.com"]);
}

fn shared_worker(
    projects: &Projects,
    store: &Arc<Mutex<Store>>,
    task: &TaskInfo,
    name: &str,
) -> AgentInfo {
    projects
        .create_shared_worker(task, name, |checkout| register_worker(store, checkout))
        .expect("shared worker")
        .agent
}

#[test]
fn a_shared_worker_works_in_the_task_worktree_without_a_second_setup() {
    let repo = Repo::new(CONFIG);
    let (projects, store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);
    let branches_before = repo.branches();
    let tip = repo.git(&["rev-parse", &task.branch]);

    let shared = projects
        .create_shared_worker(&task, "API Layer", |checkout| {
            register_worker(&store, checkout)
        })
        .expect("shared worker");
    let own = projects
        .create_worker(&task, "ui", |checkout| register_worker(&store, checkout))
        .expect("own worker");

    assert_eq!(shared.agent.name, "api-layer");
    assert_eq!(shared.agent.worktree, task.worktree);
    assert_eq!(shared.agent.branch, task.branch);
    assert_eq!(shared.setup, None, "the task worktree ran the setup");
    assert!(
        own.setup.is_some(),
        "an own worktree still runs the setup script"
    );
    assert_ne!(own.agent.worktree, task.worktree);
    assert_ne!(own.agent.branch, task.branch);
    let recorded = store.lock().expect("lock").base_commit(shared.agent.id);
    assert_eq!(recorded.expect("base").as_deref(), Some(tip.as_str()));
    let new_branches: Vec<String> = repo
        .branches()
        .into_iter()
        .filter(|branch| !branches_before.contains(branch))
        .collect();
    assert_eq!(new_branches, std::slice::from_ref(&own.agent.branch));
}

#[test]
fn a_shared_worker_keeps_the_task_ports() {
    let repo = Repo::new(CONFIG);
    let (projects, store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);

    let worker = shared_worker(&projects, &store, &task, "api");

    let env = projects.agent_env(&worker).expect("env");
    assert!(env.contains(&("SWARM_PORT".to_owned(), task.ports[0].to_string())));
}

#[test]
fn two_shared_workers_cannot_share_a_name() {
    let repo = Repo::new("");
    let (projects, store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);
    shared_worker(&projects, &store, &task, "api");

    let result =
        projects.create_shared_worker(&task, "API", |checkout| register_worker(&store, checkout));

    assert!(matches!(result, Err(Error::Invalid(_))), "{result:?}");
}

#[test]
fn discarding_a_shared_worker_keeps_the_task_worktree_and_branch() {
    let repo = Repo::new("");
    let (projects, store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);
    let worker = shared_worker(&projects, &store, &task, "api");

    projects.discard_worker(&worker);

    assert!(task.worktree.join("task-file").is_file());
    assert!(repo.branches().contains(&task.branch));
    let row = store.lock().expect("lock").agent(worker.id).expect("read");
    assert!(row.is_none());
}

#[test]
fn a_commit_takes_only_the_named_files_with_the_worker_as_author() {
    let repo = Repo::new("");
    with_identity(&repo);
    let (projects, store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);
    let worker = shared_worker(&projects, &store, &task, "api");
    std::fs::write(task.worktree.join("api.rs"), "fn api() {}\n").expect("file");
    std::fs::write(task.worktree.join("ui.rs"), "fn ui() {}\n").expect("file");
    git_in(&task.worktree, &["add", "ui.rs"]);

    let commit = git::commit_paths(
        &task.worktree,
        &worker.name,
        "feat: api",
        &[PathBuf::from("api.rs")],
    )
    .expect("commit");

    let head = git_in(&task.worktree, &["rev-parse", "HEAD"]);
    assert_eq!(commit, head);
    let author = git_in(
        &task.worktree,
        &[
            "log",
            "-1",
            "--format=%an|%ae|%cn|%(trailers:key=Swarm-Agent,valueonly)",
        ],
    );
    assert_eq!(author, "api (Swarm)|dries@example.com|Dries|api");
    let files = git_in(
        &task.worktree,
        &["show", "--name-only", "--format=", "HEAD"],
    );
    assert_eq!(files, "api.rs");
    let staged = git_in(&task.worktree, &["diff", "--cached", "--name-only"]);
    assert_eq!(staged, "ui.rs", "another worker's staged file stays staged");
}

#[test]
fn a_commit_refuses_paths_that_leave_the_worktree_or_enter_git() {
    let repo = Repo::new("");
    with_identity(&repo);
    let (projects, _store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);
    let outside = Outside::new();
    std::fs::write(outside.0.join("secret"), "x").expect("file");
    std::os::unix::fs::symlink(&outside.0, task.worktree.join("link")).expect("symlink");

    let refused = |paths: &[&str]| {
        let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();

        git::commit_paths(&task.worktree, "api", "msg", &paths)
            .expect_err("refused")
            .to_string()
    };

    assert!(refused(&[]).contains("name the files"));
    assert!(refused(&["/etc/passwd"]).contains("relative"));
    assert!(refused(&[""]).contains("relative"));
    assert!(refused(&["../outside.txt"]).contains("inside your worktree"));
    assert!(refused(&["src/../../x"]).contains("inside your worktree"));
    assert!(refused(&[".git/config"]).contains(".git"));
    assert!(refused(&["sub/.GIT/hooks/pre-commit"]).contains(".git"));
    assert!(refused(&["link/secret"]).contains("outside your worktree"));
    assert_eq!(
        git_in(&task.worktree, &["log", "--format=%s", "-1"]),
        "task commit"
    );
}

#[test]
fn a_commit_without_a_message_or_without_changes_says_why() {
    let repo = Repo::new("");
    with_identity(&repo);
    let (projects, _store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);

    let empty = git::commit_paths(&task.worktree, "api", "  ", &[PathBuf::from("task-file")])
        .expect_err("empty message");
    let unchanged = git::commit_paths(&task.worktree, "api", "msg", &[PathBuf::from("task-file")])
        .expect_err("nothing to commit");
    let missing = git::commit_paths(&task.worktree, "api", "msg", &[PathBuf::from("nope.rs")])
        .expect_err("no such file");

    assert!(empty.to_string().contains("message is empty"));
    assert!(
        unchanged.to_string().contains("git commit failed"),
        "{unchanged}"
    );
    assert!(missing.to_string().contains("git add failed"), "{missing}");
}

#[test]
fn a_path_that_looks_like_an_option_or_a_pattern_is_a_plain_file() {
    let repo = Repo::new("");
    with_identity(&repo);
    let (projects, _store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);
    std::fs::write(task.worktree.join("-x"), "dash").expect("file");
    std::fs::write(task.worktree.join("a.rs"), "a").expect("file");
    std::fs::write(task.worktree.join("b.rs"), "b").expect("file");

    git::commit_paths(&task.worktree, "api", "msg", &[PathBuf::from("-x")]).expect("dash");
    let globbed = git::commit_paths(&task.worktree, "api", "msg", &[PathBuf::from("*.rs")]);

    assert!(globbed.is_err(), "a glob must not match files");
    let files = git_in(
        &task.worktree,
        &["show", "--name-only", "--format=", "HEAD"],
    );
    assert_eq!(files, "-x");
}

#[test]
fn commits_at_once_in_one_worktree_all_succeed() {
    let repo = Repo::new("");
    with_identity(&repo);
    let (projects, _store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);
    let names = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta"];
    for name in names {
        std::fs::write(task.worktree.join(format!("{name}.rs")), name).expect("file");
    }

    let results: Vec<Result<String, Error>> = std::thread::scope(|scope| {
        let handles: Vec<_> = names
            .iter()
            .map(|name| {
                let worktree = &task.worktree;
                scope.spawn(move || {
                    git::commit_paths(
                        worktree,
                        name,
                        &format!("feat: {name}"),
                        &[PathBuf::from(format!("{name}.rs"))],
                    )
                })
            })
            .collect();

        handles
            .into_iter()
            .map(|handle| handle.join().expect("thread"))
            .collect()
    });

    for result in &results {
        assert!(result.is_ok(), "{result:?}");
    }
    let subjects = git_in(&task.worktree, &["log", "--format=%s", "-6"]);
    assert_eq!(subjects.lines().count(), 6);
    assert!(git_in(&task.worktree, &["status", "--porcelain"]).is_empty());
}

#[test]
fn a_shared_worker_is_measured_by_its_own_commits_only() {
    let repo = Repo::new("");
    with_identity(&repo);
    let (projects, store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);
    let api = shared_worker(&projects, &store, &task, "api");
    let ui = shared_worker(&projects, &store, &task, "ui");
    std::fs::write(task.worktree.join("api.rs"), "1\n2\n3\n").expect("file");
    std::fs::write(task.worktree.join("ui.rs"), "1\n").expect("file");
    std::fs::write(task.worktree.join("loose.rs"), "x\ny\n").expect("file");
    git::commit_paths(
        &task.worktree,
        "api",
        "feat: api",
        &[PathBuf::from("api.rs")],
    )
    .expect("api");
    git::commit_paths(&task.worktree, "ui", "feat: ui", &[PathBuf::from("ui.rs")]).expect("ui");

    let changes = projects.agent_changes(&[api.id, ui.id]);

    let change = |agent, path: &str, additions| AgentChange {
        agent,
        stat: DiffStat {
            files: 1,
            additions,
            deletions: 0,
        },
        files: vec![FileStat {
            path: PathBuf::from(path),
            additions,
            deletions: 0,
        }],
    };
    assert_eq!(
        changes,
        vec![change(api.id, "api.rs", 3), change(ui.id, "ui.rs", 1)]
    );
}

#[test]
fn a_commit_made_before_the_worker_started_is_not_counted() {
    let repo = Repo::new("");
    with_identity(&repo);
    let (projects, store) = shared_projects();
    let task = task_with_a_commit(&projects, &repo);
    std::fs::write(task.worktree.join("old.rs"), "1\n").expect("file");
    git::commit_paths(
        &task.worktree,
        "api",
        "feat: old",
        &[PathBuf::from("old.rs")],
    )
    .expect("old");
    let api = shared_worker(&projects, &store, &task, "api");

    let changes = projects.agent_changes(&[api.id]);

    assert_eq!(
        changes,
        vec![AgentChange {
            agent: api.id,
            stat: DiffStat::default(),
            files: Vec::new(),
        }]
    );
}
