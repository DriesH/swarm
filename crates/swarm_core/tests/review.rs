//! Review diffs, approval and closing, against real temporary git repositories.

#![expect(clippy::expect_used, reason = "test helpers panic on failure")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use swarm_core::store::{NewAgent, Store};
use swarm_core::{Error, Projects, WorkerCheckout, git};
use swarm_protocol::{
    AgentId, AgentInfo, AgentKind, AgentLifecycle, AgentRole, CreateTask, DIFF_FILES,
    DIFF_LINE_BYTES, DIFF_LINES, DIFF_LINES_PER_FILE, DiffLineKind, FileChange, FileDiff,
    ReviewSource, TaskDiff, TaskInfo,
};

static NEXT_FOLDER: AtomicU32 = AtomicU32::new(0);

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(config: &str) -> Self {
        let number = NEXT_FOLDER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("swarm-review-it-{}-{number}", std::process::id()));
        std::fs::create_dir_all(root.join(".swarm")).expect("folders");
        let root = root.canonicalize().expect("real path");
        std::fs::write(root.join(".swarm/config.toml"), config).expect("config");
        std::fs::write(root.join("README.md"), "hello\n").expect("readme");

        let repo = Self { root };
        git_in(&repo.root, &["init", "-q"]);
        git_in(&repo.root, &["add", "README.md"]);
        git_in(&repo.root, &["commit", "-q", "-m", "first"]);

        repo
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
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

struct Setup {
    repo: Repo,
    projects: Projects,
    store: Arc<Mutex<Store>>,
    task: TaskInfo,
}

fn setup(config: &str) -> Setup {
    setup_with(config, &[])
}

/// A task whose base already has `files`.
fn setup_with(config: &str, files: &[(&str, &[u8])]) -> Setup {
    let repo = Repo::new(config);
    for (path, content) in files {
        std::fs::write(repo.root.join(path), content).expect("base file");
    }
    git_in(&repo.root, &["add", "-A", ":!.swarm"]);
    git_in(
        &repo.root,
        &["commit", "-q", "--allow-empty", "-m", "base files"],
    );

    let (projects, store) = shared_projects();
    let project = projects.add(&repo.root).expect("add");
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

    Setup {
        repo,
        projects,
        store,
        task,
    }
}

impl Setup {
    fn write(&self, path: &str, content: &str) {
        std::fs::write(self.task.worktree.join(path), content).expect("write");
    }

    fn merged(&self) -> TaskDiff {
        self.projects
            .task_diff(self.task.id, ReviewSource::Merged)
            .expect("diff")
    }

    fn commit_all(&self) {
        git_in(&self.task.worktree, &["add", "-A"]);
        git_in(
            &self.task.worktree,
            &["commit", "-q", "--allow-empty", "-m", "work"],
        );
    }

    fn live(&self) -> TaskDiff {
        self.projects
            .task_diff(self.task.id, ReviewSource::Live)
            .expect("diff")
    }

    fn worker(&self, name: &str, own_worktree: bool) -> AgentInfo {
        let register = |checkout: &WorkerCheckout| register_worker(&self.store, checkout);

        let prepared = if own_worktree {
            self.projects.create_worker(&self.task, name, register)
        } else {
            self.projects
                .create_shared_worker(&self.task, name, register)
        };

        prepared.expect("worker").agent
    }
}

fn commit(agent: &str, folder: &Path, paths: &[&str]) {
    let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();

    git::commit_paths(folder, agent, "feat: work", &paths).expect("commit");
}

fn file<'a>(diff: &'a TaskDiff, path: &str) -> &'a FileDiff {
    diff.files
        .iter()
        .find(|file| file.path == Path::new(path))
        .unwrap_or_else(|| panic!("no file {path:?} in {:?}", paths(diff)))
}

fn paths(diff: &TaskDiff) -> Vec<&Path> {
    diff.files.iter().map(|file| file.path.as_path()).collect()
}

fn lines(file: &FileDiff) -> Vec<(DiffLineKind, &str)> {
    file.hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .map(|line| (line.kind, line.text.as_str()))
        .collect()
}

#[test]
fn a_modified_file_has_hunks_with_context_and_a_section() {
    let setup = setup("");
    setup.write("README.md", "hello\nworld\n");

    let diff = setup.live();

    let readme = file(&diff, "README.md");
    assert_eq!(readme.change, FileChange::Modified);
    assert_eq!((readme.additions, readme.deletions), (1, 0));
    assert_eq!(
        lines(readme),
        [
            (DiffLineKind::Context, "hello"),
            (DiffLineKind::Added, "world")
        ]
    );
    let hunk = &readme.hunks[0];
    assert_eq!(
        (
            hunk.old_start,
            hunk.old_lines,
            hunk.new_start,
            hunk.new_lines
        ),
        (1, 1, 1, 2)
    );
    assert_eq!(readme.worktree, setup.task.worktree);
    assert_eq!(
        (diff.stat.files, diff.stat.additions, diff.stat.deletions),
        (1, 1, 0)
    );
}

#[test]
fn a_deleted_file_and_the_missing_newline_marker() {
    let setup = setup_with("", &[("tail.txt", b"a\nb\n")]);
    std::fs::remove_file(setup.task.worktree.join("tail.txt")).expect("remove");
    setup.write("README.md", "hello");

    let diff = setup.live();

    let removed = file(&diff, "tail.txt");
    assert_eq!(removed.change, FileChange::Deleted);
    assert_eq!((removed.additions, removed.deletions), (0, 2));
    assert_eq!(
        lines(file(&diff, "README.md")),
        [
            (DiffLineKind::Deleted, "hello"),
            (DiffLineKind::Added, "hello"),
            (DiffLineKind::NoNewline, "")
        ]
    );
}

#[test]
fn paths_with_a_space_a_non_ascii_letter_and_a_tab_are_exact() {
    let names = [
        "with space.txt",
        "caf\u{e9}.txt",
        "tab\there.txt",
        "q\"uote.txt",
    ];
    let base: Vec<(&str, &[u8])> = names
        .iter()
        .map(|name| (*name, b"one\n".as_slice()))
        .collect();
    let setup = setup_with("", &base);
    for name in names {
        setup.write(name, "two\n");
    }
    setup.write("new \u{e9}\tfile.txt", "fresh\n");

    let diff = setup.live();

    for name in names {
        assert_eq!(
            lines(file(&diff, name)),
            [(DiffLineKind::Deleted, "one"), (DiffLineKind::Added, "two")],
            "{name:?}"
        );
    }
    let added = file(&diff, "new \u{e9}\tfile.txt");
    assert_eq!(added.change, FileChange::Added);
    assert_eq!(lines(added), [(DiffLineKind::Added, "fresh")]);
}

#[test]
fn untracked_text_and_binary_files_are_added_files() {
    let setup = setup("");
    setup.write("notes.txt", "a\nb\n");
    setup.write("empty.txt", "");
    std::fs::write(setup.task.worktree.join("blob.bin"), [1_u8, 0, 2, 3]).expect("binary");

    let diff = setup.live();

    let notes = file(&diff, "notes.txt");
    assert_eq!(notes.change, FileChange::Added);
    assert_eq!((notes.additions, notes.deletions), (2, 0));
    assert_eq!(notes.hunks.len(), 1);
    assert_eq!(
        (
            notes.hunks[0].old_lines,
            notes.hunks[0].new_start,
            notes.hunks[0].new_lines
        ),
        (0, 1, 2)
    );
    assert_eq!(
        lines(notes),
        [(DiffLineKind::Added, "a"), (DiffLineKind::Added, "b")]
    );
    assert!(file(&diff, "empty.txt").hunks.is_empty());
    let blob = file(&diff, "blob.bin");
    assert!(blob.binary);
    assert!(blob.hunks.is_empty());
    assert_eq!(blob.change, FileChange::Added);
}

#[test]
fn a_tracked_binary_change_has_no_lines() {
    let setup = setup_with("", &[("blob.bin", &[0_u8, 1])]);
    std::fs::write(setup.task.worktree.join("blob.bin"), [0_u8, 2, 3]).expect("binary");

    let diff = setup.live();

    let blob = file(&diff, "blob.bin");
    assert!(blob.binary);
    assert_eq!(blob.change, FileChange::Modified);
    assert!(blob.hunks.is_empty());
}

fn within_seconds<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || sender.send(work()).ok());

    receiver
        .recv_timeout(Duration::from_secs(20))
        .expect("the diff hung")
}

#[test]
fn a_fifo_and_a_link_to_it_do_not_hang_the_diff() {
    let setup = setup("");
    let fifo = setup.task.worktree.join("pipe");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo")
            .success()
    );
    std::os::unix::fs::symlink(&fifo, setup.task.worktree.join("link")).expect("link");
    std::os::unix::fs::symlink("/dev/zero", setup.task.worktree.join("zeros")).expect("link");

    let (projects, task) = (setup.projects, setup.task.id);
    let diff = within_seconds(move || projects.task_diff(task, ReviewSource::Live).expect("diff"));

    assert_eq!(
        lines(file(&diff, "link")),
        [(
            DiffLineKind::Added,
            setup.task.worktree.join("pipe").to_str().expect("utf8")
        )]
    );
    assert_eq!(diff.stat.files, 2, "git does not list a FIFO");
}

#[test]
fn a_long_line_is_cut_on_a_character_boundary_and_bad_utf8_is_replaced() {
    let setup = setup("");
    let long = "\u{e9}".repeat(DIFF_LINE_BYTES);
    setup.write("long.txt", &format!("{long}\n"));
    let mut bad = b"ok \xff end\n".to_vec();
    bad.extend_from_slice(b"second\n");
    std::fs::write(setup.task.worktree.join("bad.txt"), bad).expect("write");

    let diff = setup.live();

    let line = &file(&diff, "long.txt").hunks[0].lines[0].text;
    assert_eq!(line.len(), DIFF_LINE_BYTES);
    assert!(line.chars().all(|character| character == '\u{e9}'));
    assert_eq!(lines(file(&diff, "bad.txt"))[0].1, "ok \u{fffd} end");
}

#[test]
fn the_per_file_cap_marks_the_file_but_the_totals_stay_full() {
    let setup = setup("");
    let count = DIFF_LINES_PER_FILE + 10;
    setup.write("big.txt", &"x\n".repeat(count));

    let diff = setup.live();

    let big = file(&diff, "big.txt");
    assert!(big.truncated);
    assert_eq!(lines(big).len(), DIFF_LINES_PER_FILE);
    assert_eq!(big.additions, u32::try_from(count).expect("small"));
    assert_eq!(diff.stat.additions, big.additions);
}

#[test]
fn the_total_line_cap_and_the_file_cap_keep_the_totals() {
    let setup = setup("");
    let full = "x\n".repeat(DIFF_LINES_PER_FILE);
    let files = DIFF_LINES / DIFF_LINES_PER_FILE;
    for index in 0..=files {
        setup.write(&format!("big-{index:02}.txt"), &full);
    }
    for index in 0..DIFF_FILES {
        setup.write(&format!("small-{index:04}.txt"), "y\n");
    }

    let diff = setup.live();

    let kept: usize = diff.files.iter().map(|file| lines(file).len()).sum();
    assert_eq!(kept, DIFF_LINES);
    assert!(file(&diff, &format!("big-{files:02}.txt")).truncated);
    assert!(!file(&diff, "big-00.txt").truncated);
    assert_eq!(diff.files.len(), DIFF_FILES);
    assert_eq!(
        diff.stat.files,
        u32::try_from(files + 1 + DIFF_FILES).expect("small")
    );
    let expected = (files + 1) * DIFF_LINES_PER_FILE + DIFF_FILES;
    assert_eq!(diff.stat.additions, u32::try_from(expected).expect("small"));
}

#[test]
fn the_fingerprint_is_stable_and_follows_every_line_and_binary_change() {
    let setup = setup("");
    setup.write("a.txt", "one\ntwo\n");
    let binary = setup.task.worktree.join("blob.bin");
    std::fs::write(&binary, [0_u8, 1, 2]).expect("binary");
    let first = setup.live();

    assert_eq!(setup.live().fingerprint, first.fingerprint);

    setup.write("a.txt", "one\ntwo!\n");
    let changed_line = setup.live().fingerprint;
    assert_ne!(changed_line, first.fingerprint);

    // Same size, same lines: only the content and the time differ.
    std::fs::write(&binary, [0_u8, 1, 3]).expect("binary");
    let later = SystemTime::now() + Duration::from_secs(5);
    let file = std::fs::File::options()
        .write(true)
        .open(&binary)
        .expect("open");
    file.set_times(std::fs::FileTimes::new().set_modified(later))
        .expect("time");
    assert_ne!(setup.live().fingerprint, changed_line);
}

#[test]
fn a_change_in_a_line_past_the_caps_changes_the_fingerprint() {
    let setup = setup("");
    let mut text = "x\n".repeat(DIFF_LINES_PER_FILE + 5);
    setup.write("big.txt", &text);
    let before = setup.live().fingerprint;

    text.replace_range(text.len() - 2.., "y\n");
    setup.write("big.txt", &text);

    assert_ne!(setup.live().fingerprint, before);
}

#[test]
fn a_mode_only_change_and_a_type_change_do_not_break_the_diff() {
    let setup = setup_with("", &[("run.sh", b"echo\n"), ("kind", b"text\n")]);
    std::fs::set_permissions(
        setup.task.worktree.join("run.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("mode");
    std::fs::remove_file(setup.task.worktree.join("kind")).expect("remove");
    std::os::unix::fs::symlink("README.md", setup.task.worktree.join("kind")).expect("link");

    let diff = setup.live();

    let script = file(&diff, "run.sh");
    assert_eq!(script.change, FileChange::Modified);
    assert!(script.hunks.is_empty());
    assert_eq!(script.mode_change, Some((0o100_644, 0o100_755)));
    let kind = file(&diff, "kind");
    assert_eq!(kind.change, FileChange::Modified);
    assert_eq!(
        lines(kind),
        [
            (DiffLineKind::Deleted, "text"),
            (DiffLineKind::Added, "README.md"),
            (DiffLineKind::NoNewline, "")
        ]
    );
}

#[test]
fn a_submodule_change_does_not_break_the_diff() {
    let setup = setup("");
    let inner = Repo::new("");
    git_in(
        &setup.task.worktree,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            inner.root.to_str().expect("utf8"),
            "sub",
        ],
    );
    setup.write("README.md", "hello\nmore\n");

    let diff = setup.live();

    assert_eq!(file(&diff, "sub").change, FileChange::Added);
    assert_eq!(file(&diff, "README.md").additions, 1);
}

#[test]
fn files_are_attributed_by_the_newest_trailer_commit() {
    let setup = setup("");
    let api = setup.worker("api", false);
    let ui = setup.worker("ui", false);
    setup.write("api.rs", "1\n");
    setup.write("shared.rs", "1\n");
    commit("api", &setup.task.worktree, &["api.rs", "shared.rs"]);
    setup.write("ui.rs", "1\n");
    setup.write("shared.rs", "2\n");
    commit("ui", &setup.task.worktree, &["ui.rs", "shared.rs"]);
    setup.write("loose.rs", "1\n");
    git_in(&setup.task.worktree, &["add", "loose.rs"]);
    git_in(
        &setup.task.worktree,
        &["commit", "-q", "-m", "plain commit"],
    );
    setup.write("dirty.rs", "1\n");

    let diff = setup.live();

    assert_eq!(file(&diff, "api.rs").agent, Some(api.id));
    assert_eq!(file(&diff, "ui.rs").agent, Some(ui.id));
    assert_eq!(file(&diff, "shared.rs").agent, Some(ui.id));
    assert_eq!(file(&diff, "loose.rs").agent, None);
    assert_eq!(file(&diff, "dirty.rs").agent, None);
}

#[test]
fn a_name_two_agents_share_names_nobody() {
    let setup = setup("");
    let orchestrator = {
        let store = setup.store.lock().expect("lock");
        store
            .add_agent(&NewAgent {
                project: setup.task.project,
                task: Some(setup.task.id),
                role: AgentRole::Orchestrator,
                kind: AgentKind::ClaudeCode,
                model: "opus",
                session_id: None,
                name: "orchestrator",
                worktree: &setup.task.worktree,
                branch: &setup.task.branch,
                ports: &[],
            })
            .expect("orchestrator")
    };
    let twin = setup.worker("orchestrator", false);
    assert_ne!(orchestrator.id, twin.id);
    setup.write("a.rs", "1\n");
    commit("orchestrator", &setup.task.worktree, &["a.rs"]);

    assert_eq!(file(&setup.merged(), "a.rs").agent, None);
}

#[test]
fn live_adds_an_unmerged_worker_and_drops_it_once_merged() {
    let setup = setup("");
    let ui = setup.worker("ui", true);
    std::fs::write(ui.worktree.join("ui.rs"), "1\n").expect("file");
    setup.write("task.rs", "1\n");

    let live = setup.live();

    let own = file(&live, "ui.rs");
    assert_eq!(own.worktree, ui.worktree);
    assert_eq!(own.agent, Some(ui.id));
    assert_eq!(file(&live, "task.rs").worktree, setup.task.worktree);
    assert!(
        !paths(&setup.merged()).contains(&Path::new("ui.rs")),
        "Merged shows the task worktree only"
    );

    commit("ui", &ui.worktree, &["ui.rs"]);
    assert!(
        paths(&setup.live()).contains(&Path::new("ui.rs")),
        "committed, not merged"
    );

    let outcome = git::merge_branch(&setup.task.worktree, &ui.branch).expect("merge");
    assert!(outcome.merged);
    let live = setup.live();
    assert_eq!(
        live.files
            .iter()
            .filter(|f| f.path == Path::new("ui.rs"))
            .count(),
        1
    );
    assert_eq!(file(&live, "ui.rs").worktree, setup.task.worktree);
    assert_eq!(
        file(&live, "ui.rs").agent,
        Some(ui.id),
        "the trailer still names it"
    );
}

#[test]
fn a_merged_worker_with_uncommitted_changes_stays_in_live() {
    let setup = setup("");
    let ui = setup.worker("ui", true);
    std::fs::write(ui.worktree.join("ui.rs"), "1\n").expect("file");
    commit("ui", &ui.worktree, &["ui.rs"]);
    git::merge_branch(&setup.task.worktree, &ui.branch).expect("merge");
    std::fs::write(ui.worktree.join("more.rs"), "1\n").expect("file");

    let live = setup.live();

    assert_eq!(file(&live, "more.rs").worktree, ui.worktree);
}

#[test]
fn approving_needs_the_current_fingerprint() {
    let setup = setup("");
    setup.write("a.txt", "1\n");
    setup.commit_all();
    let reviewed = setup.merged().fingerprint;
    setup.write("a.txt", "2\n");
    setup.commit_all();

    let stale = setup.projects.approve(setup.task.id, reviewed);
    let Err(Error::Invalid(message)) = stale else {
        panic!("a stale fingerprint must be refused: {stale:?}");
    };
    assert_eq!(message, "the changes changed since you reviewed them");
    assert_eq!(
        setup.projects.task(setup.task.id).expect("task").approved,
        None
    );

    let current = setup.merged().fingerprint;
    let task = setup
        .projects
        .approve(setup.task.id, current)
        .expect("approve");

    assert_eq!(task.approved, Some(current));
    assert_eq!(
        setup.projects.task(setup.task.id).expect("task").approved,
        Some(current)
    );
}

#[test]
fn a_fingerprint_with_the_top_bit_set_survives_the_database() {
    let (_projects, store) = shared_projects();
    let store = store.lock().expect("lock");
    let project = store
        .add_project(Path::new("/repo"), "repo")
        .expect("project");
    let task = store
        .add_task(&swarm_core::store::NewTask {
            project: project.id,
            name: "t",
            branch: "swarm/t",
            base: "main",
            worktree: Path::new("/repo/.worktrees/t"),
            ports: &[],
        })
        .expect("task");

    store.set_approved(task.id, u64::MAX - 6).expect("approve");

    let stored = store.task(task.id).expect("read").expect("task");
    assert_eq!(stored.approved, Some(u64::MAX - 6));
}

#[test]
fn the_close_check_counts_running_agents_changes_and_unmerged_workers() {
    let setup = setup("");
    let ui = setup.worker("ui", true);
    let api = setup.worker("api", true);
    let shared = setup.worker("docs", false);
    setup.write("loose.rs", "1\n");
    std::fs::write(ui.worktree.join("a.rs"), "1\n").expect("file");
    std::fs::write(ui.worktree.join("b.rs"), "1\n").expect("file");
    std::fs::write(api.worktree.join("c.rs"), "1\n").expect("file");
    commit("ui", &ui.worktree, &["a.rs"]);
    commit("api", &api.worktree, &["c.rs"]);
    git::merge_branch(&setup.task.worktree, &api.branch).expect("merge");
    {
        let store = setup.store.lock().expect("lock");
        for (agent, lifecycle) in [
            (&ui, AgentLifecycle::Running),
            (&shared, AgentLifecycle::Running),
            (&api, AgentLifecycle::Stopped),
        ] {
            let mut changed = agent.clone();
            changed.lifecycle = lifecycle;
            changed.revision += 1;
            store.save_agent(&changed).expect("save");
        }
    }

    let check = setup.projects.close_check(setup.task.id).expect("check");

    assert_eq!(check.running_agents, 2);
    assert_eq!(check.uncommitted_files, 2);
    assert_eq!(check.unmerged_workers, ["ui"]);
}

#[test]
fn closing_removes_the_worktrees_keeps_the_branches_and_frees_the_ports() {
    let setup = setup("[worktrees]\nports = { range = \"5170-5175\", per_worktree = 3 }\n");
    let ui = setup.worker("ui", true);
    setup.write("a.txt", "uncommitted\n");
    let branches = git_in(&setup.repo.root, &["branch", "--format=%(refname:short)"]);
    let used_before = setup
        .store
        .lock()
        .expect("lock")
        .all_task_ports()
        .expect("ports");
    assert!(!used_before.is_empty());
    {
        let store = setup.store.lock().expect("lock");
        let mut running = ui.clone();
        running.lifecycle = AgentLifecycle::Running;
        running.revision += 1;
        store.save_agent(&running).expect("save");
    }

    let closed = setup.projects.close_task(setup.task.id).expect("close");

    assert!(closed.task.closed);
    assert_eq!(closed.stopped.len(), 1);
    assert_eq!(closed.stopped[0].lifecycle, AgentLifecycle::Stopped);
    assert!(!setup.task.worktree.exists());
    assert!(!ui.worktree.exists());
    assert_eq!(
        git_in(&setup.repo.root, &["branch", "--format=%(refname:short)"]),
        branches
    );
    assert!(
        !git_in(&setup.repo.root, &["worktree", "list", "--porcelain"]).contains(".worktrees"),
        "the worktrees are pruned"
    );
    assert_eq!(
        setup
            .store
            .lock()
            .expect("lock")
            .all_task_ports()
            .expect("ports"),
        Vec::<u16>::new()
    );
    let project = setup.projects.list().expect("list").remove(0);
    assert!(project.tasks.is_empty());
    assert!(setup.projects.task(setup.task.id).is_err());
    assert!(
        setup
            .projects
            .task_diff(setup.task.id, ReviewSource::Merged)
            .is_err()
    );
    assert!(setup.projects.close_check(setup.task.id).is_err());
    assert!(setup.projects.close_task(setup.task.id).is_err(), "twice");
    let again = setup.projects.close_task(TaskIdOf::unknown());
    assert!(again.is_err());
}

struct TaskIdOf;

impl TaskIdOf {
    fn unknown() -> swarm_protocol::TaskId {
        swarm_protocol::TaskId(9999)
    }
}

#[test]
fn closing_runs_the_teardown_script_with_the_task_environment_and_survives_its_failure() {
    let marker = std::env::temp_dir().join(format!("swarm-teardown-{}", std::process::id()));
    let config = format!(
        "[worktrees]\nteardown = \"echo $SWARM_TASK > {}; exit 3\"\n",
        marker.display()
    );
    let setup = setup(&config);

    let closed = setup.projects.close_task(setup.task.id).expect("close");

    assert!(closed.task.closed);
    let written = std::fs::read_to_string(&marker).expect("marker");
    std::fs::remove_file(&marker).ok();
    assert_eq!(written.trim(), setup.task.id.0.to_string());
}

#[test]
fn a_closed_agent_that_could_resume_is_stopped() {
    let setup = setup("");
    let ui = setup.worker("ui", false);
    {
        let store = setup.store.lock().expect("lock");
        let mut resumable = ui.clone();
        resumable.lifecycle = AgentLifecycle::Resumable;
        resumable.revision += 1;
        store.save_agent(&resumable).expect("save");
    }

    let closed = setup.projects.close_task(setup.task.id).expect("close");

    let ids: Vec<AgentId> = closed.stopped.iter().map(|agent| agent.id).collect();
    assert_eq!(ids, [ui.id]);
}

#[test]
fn merged_holds_commits_only_and_live_holds_the_working_tree() {
    let setup = setup("");
    setup.write("done.txt", "1\n");
    setup.commit_all();
    let before = setup.merged().fingerprint;
    setup.write("done.txt", "1\n2\n");
    setup.write("fresh.txt", "x\n");

    let merged = setup.merged();

    assert_eq!(paths(&merged), [Path::new("done.txt")]);
    assert_eq!(merged.stat.additions, 1);
    assert_eq!(merged.fingerprint, before, "uncommitted work is not in it");
    let live = setup.live();
    assert_eq!(live.stat.files, 2);
    assert_eq!(file(&live, "done.txt").additions, 2);
}

#[test]
fn the_missing_newline_at_the_end_changes_the_fingerprint() {
    let setup = setup("");
    setup.write("a.txt", "one\ntwo\n");
    setup.commit_all();
    let with_newline = setup.merged().fingerprint;

    setup.write("a.txt", "one\ntwo");
    setup.commit_all();
    let merged = setup.merged();

    assert_ne!(merged.fingerprint, with_newline);
    assert_eq!(
        lines(file(&merged, "a.txt")),
        [
            (DiffLineKind::Added, "one"),
            (DiffLineKind::Added, "two"),
            (DiffLineKind::NoNewline, "")
        ]
    );

    let untracked = {
        setup.write("b.txt", "x");
        setup.live()
    };
    assert_eq!(
        lines(file(&untracked, "b.txt")),
        [(DiffLineKind::Added, "x"), (DiffLineKind::NoNewline, "")]
    );
}

#[test]
fn a_mode_only_change_changes_the_merged_fingerprint() {
    let setup = setup_with("", &[("run.sh", b"echo\n")]);
    setup.write("other.txt", "1\n");
    setup.commit_all();
    let before = setup.merged().fingerprint;

    std::fs::set_permissions(
        setup.task.worktree.join("run.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("mode");
    setup.commit_all();
    let merged = setup.merged();

    assert_ne!(merged.fingerprint, before);
    assert_eq!(
        file(&merged, "run.sh").mode_change,
        Some((0o100_644, 0o100_755))
    );
}

#[test]
fn a_committed_binary_change_changes_the_fingerprint_without_a_new_mtime() {
    let setup = setup("");
    let binary = setup.task.worktree.join("blob.bin");
    std::fs::write(&binary, [0_u8, 1, 2]).expect("binary");
    setup.commit_all();
    let first = setup.merged().fingerprint;
    let modified = std::fs::metadata(&binary)
        .and_then(|meta| meta.modified())
        .expect("mtime");

    std::fs::write(&binary, [0_u8, 1, 3]).expect("binary");
    std::fs::File::options()
        .write(true)
        .open(&binary)
        .and_then(|file| file.set_times(std::fs::FileTimes::new().set_modified(modified)))
        .expect("time");
    setup.commit_all();

    let merged = setup.merged();
    assert_ne!(merged.fingerprint, first);
    assert!(file(&merged, "blob.bin").binary);
}

#[test]
fn a_partly_merged_worker_shows_only_what_the_task_branch_lacks() {
    let setup = setup("");
    let ui = setup.worker("ui", true);
    std::fs::write(ui.worktree.join("one.rs"), "1\n").expect("file");
    commit("ui", &ui.worktree, &["one.rs"]);
    git::merge_branch(&setup.task.worktree, &ui.branch).expect("merge");
    std::fs::write(ui.worktree.join("two.rs"), "1\n").expect("file");
    commit("ui", &ui.worktree, &["two.rs"]);

    let live = setup.live();

    let ones: Vec<_> = live
        .files
        .iter()
        .filter(|file| file.path == Path::new("one.rs"))
        .collect();
    assert_eq!(ones.len(), 1);
    assert_eq!(ones[0].worktree, setup.task.worktree);
    assert_eq!(file(&live, "two.rs").worktree, ui.worktree);
    assert_eq!(live.stat.files, 2);
}

#[test]
fn uncommitted_work_in_the_task_worktree_has_no_agent_in_live() {
    let setup = setup("");
    let api = setup.worker("api", false);
    setup.write("kept.rs", "1\n");
    setup.write("edited.rs", "1\n");
    commit("api", &setup.task.worktree, &["kept.rs", "edited.rs"]);
    setup.write("edited.rs", "1\n2\n");

    let live = setup.live();

    assert_eq!(file(&live, "edited.rs").agent, None);
    assert_eq!(file(&live, "kept.rs").agent, Some(api.id));
}

#[test]
fn untracked_files_over_a_cap_are_left_out_not_binary() {
    let setup = setup("");
    let big = setup.task.worktree.join("big.txt");
    std::fs::write(&big, "x".repeat((1 << 20) + 10)).expect("big");
    setup.write("nul.bin", "a\0b");

    let live = setup.live();

    let left_out = file(&live, "big.txt");
    assert!(left_out.truncated);
    assert!(!left_out.binary);
    assert!(left_out.hunks.is_empty());
    assert!(file(&live, "nul.bin").binary);
}

#[test]
fn untracked_files_past_the_listing_cap_still_count_and_are_hashed() {
    let names: Vec<String> = (0..5).map(|index| format!("t{index}.txt")).collect();
    let base: Vec<(&str, &[u8])> = names
        .iter()
        .map(|name| (name.as_str(), b"one\n".as_slice()))
        .collect();
    let setup = setup_with("", &base);
    for name in &names {
        setup.write(name, "two\n");
    }
    let many = setup.task.worktree.join("many");
    std::fs::create_dir_all(&many).expect("folder");
    for index in 0..10_002 {
        std::fs::write(many.join(format!("f{index:05}.txt")), "x\n").expect("file");
    }

    let before = setup.live();

    assert_eq!(before.stat.files, 5 + 10_002);
    assert_eq!(
        before.stat.additions,
        5 + 10_000,
        "tracked files do not use up the untracked allowance"
    );

    std::fs::write(many.join("f10001.txt"), "xyz\n").expect("file");
    assert_ne!(
        setup.live().fingerprint,
        before.fingerprint,
        "a file past the cap is hashed by size"
    );
}

#[test]
fn a_closing_task_is_finished_by_recovery() {
    let setup = setup("");
    let ui = setup.worker("ui", true);
    {
        let store = setup.store.lock().expect("lock");
        let mut running = ui.clone();
        running.lifecycle = AgentLifecycle::Running;
        running.revision += 1;
        store.save_agent(&running).expect("save");
        store.set_agent_session(ui.id, "session").expect("session");
        store.confirm_agent_session(ui.id).expect("confirm");
        store.begin_closing(setup.task.id).expect("closing");
        store.recover_agents().expect("recover");
    }
    let agents = setup.store.lock().expect("lock").list_agents(setup.task.id);
    assert_eq!(
        agents.expect("agents")[0].lifecycle,
        AgentLifecycle::Stopped,
        "no agent of a closing task resumes"
    );
    assert!(setup.task.worktree.exists());

    setup.projects.recover_closing();

    assert!(!setup.task.worktree.exists());
    assert!(!ui.worktree.exists());
    assert!(setup.projects.task(setup.task.id).is_err());
    assert!(
        setup
            .store
            .lock()
            .expect("lock")
            .closing_tasks()
            .expect("closing")
            .is_empty()
    );
}
