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
    DIFF_LINE_BYTES, DIFF_LINES, DIFF_LINES_PER_FILE, DiffLineKind, FOLD_LINES, FileChange,
    FileDiff, Fold, ReviewSource, TaskDiff, TaskInfo,
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
        write_in(&self.task.worktree, path, content);
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

fn write_in(folder: &Path, path: &str, content: &str) {
    let full = folder.join(path);
    std::fs::create_dir_all(full.parent().expect("parent")).expect("folder");
    std::fs::write(full, content).expect("write");
}

/// `count` different lines.
fn numbered(count: usize) -> String {
    use std::fmt::Write as _;

    let mut text = String::new();
    for number in 0..count {
        writeln!(text, "line {number}").expect("a string grows");
    }

    text
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
    let loaded = setup
        .projects
        .file_hunks(
            setup.task.id,
            ReviewSource::Live,
            &setup.task.worktree,
            Path::new("big.txt"),
        )
        .expect("file")
        .expect("the diff holds it");

    assert!(loaded.truncated);
    assert_eq!(lines(&loaded).len(), DIFF_LINES_PER_FILE);
    assert_eq!(loaded.additions, u32::try_from(count).expect("small"));
    assert_eq!(diff.stat.additions, loaded.additions);
}

#[test]
fn the_total_line_cap_and_the_file_cap_keep_the_totals() {
    let setup = setup("");
    // The most a file may have and still come with its lines.
    let per_file = usize::try_from(FOLD_LINES).expect("small");
    let full = "x\n".repeat(per_file);
    let files = DIFF_LINES / per_file;
    for index in 0..=files {
        setup.write(&format!("big-{index:03}.txt"), &full);
    }
    for index in 0..DIFF_FILES {
        setup.write(&format!("small-{index:04}.txt"), "y\n");
    }

    let diff = setup.live();

    let kept: usize = diff.files.iter().map(|file| lines(file).len()).sum();
    assert_eq!(kept, DIFF_LINES);
    assert!(file(&diff, &format!("big-{files:03}.txt")).truncated);
    assert!(!file(&diff, "big-000.txt").truncated);
    assert_eq!(diff.files.len(), DIFF_FILES);
    assert_eq!(
        diff.stat.files,
        u32::try_from(files + 1 + DIFF_FILES).expect("small")
    );
    let expected = (files + 1) * per_file + DIFF_FILES;
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
            pr_base: "main",
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
fn closing_removes_the_worktrees_keeps_unmerged_branches_and_frees_the_ports() {
    let setup = setup("[worktrees]\nports = { range = \"5170-5175\", per_worktree = 3 }\n");
    let ui = setup.worker("ui", true);
    setup.write("done.txt", "committed\n");
    setup.commit_all();
    std::fs::write(ui.worktree.join("b.rs"), "1\n").expect("file");
    commit("ui", &ui.worktree, &["b.rs"]);
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

#[test]
fn closing_deletes_only_the_branches_merged_into_the_base_branch() {
    let setup = setup("");
    let idle = setup.worker("idle", true);
    let busy = setup.worker("busy", true);
    std::fs::write(busy.worktree.join("c.rs"), "1\n").expect("file");
    commit("busy", &busy.worktree, &["c.rs"]);

    let check = setup.projects.close_check(setup.task.id).expect("check");
    assert_eq!(
        check.merged_branches,
        [setup.task.branch.clone(), idle.branch.clone()]
    );

    setup.projects.close_task(setup.task.id).expect("close");

    let branches = git_in(&setup.repo.root, &["branch", "--format=%(refname:short)"]);
    assert!(
        !branches.lines().any(|line| line == setup.task.branch),
        "{branches}"
    );
    assert!(
        !branches.lines().any(|line| line == idle.branch),
        "{branches}"
    );
    assert!(
        branches.lines().any(|line| line == busy.branch),
        "{branches}"
    );
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

/// Files that come folded and files next to them that do not.
fn write_folding_files(setup: &Setup) {
    setup.write(".gitattributes", "generated/** linguist-generated\n");
    setup.write("Cargo.lock", &numbered(600));
    setup.write("web/package-lock.json", &numbered(50));
    setup.write("generated/schema.txt", &numbered(20));
    setup.write("dist/app.min.js", &numbered(5));
    setup.write("big.txt", &numbered(501));
    setup.write("edge.txt", &numbered(500));
    setup.write("normal.txt", &numbered(3));
}

fn assert_folding(diff: &TaskDiff) {
    let folded = |path: &str, fold: Fold, count: u32| {
        let folded = file(diff, path);

        assert_eq!(folded.fold, Some(fold), "{path}");
        assert!(folded.hunks.is_empty(), "{path} has no lines");
        assert!(!folded.truncated, "{path} is not cut");
        assert_eq!(folded.additions, count, "{path}");
        assert_eq!(folded.deletions, 0, "{path}");
        assert_ne!(folded.digest, 0, "{path}");
    };
    folded("Cargo.lock", Fold::Lock, 600);
    folded("web/package-lock.json", Fold::Lock, 50);
    folded("generated/schema.txt", Fold::Generated, 20);
    folded("dist/app.min.js", Fold::Generated, 5);
    folded("big.txt", Fold::Large, 501);

    for (path, count) in [("edge.txt", 500), ("normal.txt", 3), (".gitattributes", 1)] {
        let shown = file(diff, path);

        assert_eq!(shown.fold, None, "{path}");
        assert_eq!(lines(shown).len(), count, "{path}");
        assert!(!shown.truncated, "{path}");
    }
}

#[test]
fn lock_generated_minified_and_large_files_come_without_lines() {
    let setup = setup("");
    write_folding_files(&setup);

    let live = setup.live();
    assert_folding(&live);
    assert_eq!(live.stat.files, 8);
    assert_eq!(live.stat.additions, 600 + 50 + 20 + 5 + 501 + 500 + 3 + 1);

    // Merged reads the attributes of the commit.
    setup.commit_all();
    assert_folding(&setup.merged());
}

#[test]
fn a_changed_tracked_file_comes_folded_by_its_counts() {
    let big = numbered(600);
    let setup = setup_with(
        "",
        &[
            ("Cargo.lock", numbered(600).as_bytes()),
            ("big.txt", big.as_bytes()),
        ],
    );
    setup.write("Cargo.lock", &numbered(599));
    setup.write("big.txt", &big.replace("line 3\n", "changed\n"));
    setup.write("grown.txt", &numbered(10));

    let live = setup.live();

    assert_eq!(file(&live, "Cargo.lock").fold, Some(Fold::Lock));
    assert_eq!(file(&live, "Cargo.lock").deletions, 1);
    assert_eq!(file(&live, "big.txt").fold, None, "2 changed lines");
    assert!(!lines(file(&live, "big.txt")).is_empty());

    setup.write("big.txt", &numbered(601).replace("line", "other"));
    let live = setup.live();

    let big = file(&live, "big.txt");
    assert_eq!(big.fold, Some(Fold::Large));
    assert!(big.hunks.is_empty());
    assert_eq!((big.additions, big.deletions), (601, 600));
}

#[test]
fn a_false_attribute_turns_the_folds_off() {
    let setup = setup("");
    setup.write(
        ".gitattributes",
        "*.min.js linguist-generated=false\nCargo.lock -linguist-generated\nweb/** linguist-generated=true\nweb/yarn.lock linguist-generated=false\n",
    );
    setup.write("app.min.js", "x\n");
    setup.write("other.min.js", "x\n");
    setup.write("Cargo.lock", "x\n");
    setup.write("web/yarn.lock", "x\n");
    setup.write("web/notes.txt", "x\n");
    setup.write("web/package-lock.json", "x\n");

    let assert_folds = |diff: &TaskDiff| {
        assert_eq!(file(diff, "app.min.js").fold, None);
        assert_eq!(file(diff, "other.min.js").fold, None);
        assert_eq!(file(diff, "Cargo.lock").fold, None);
        assert_eq!(file(diff, "web/yarn.lock").fold, None);
        assert_eq!(file(diff, "web/notes.txt").fold, Some(Fold::Generated));
        assert_eq!(
            file(diff, "web/package-lock.json").fold,
            Some(Fold::Lock),
            "a lock file stays a lock file"
        );
    };

    assert_folds(&setup.live());

    setup.commit_all();
    assert_folds(&setup.merged());
}

#[test]
fn merged_reads_the_attributes_of_the_commit_and_live_those_of_the_worktree() {
    let setup = setup("");
    setup.write(".gitattributes", "generated.txt linguist-generated\n");
    setup.write("generated.txt", "x\n");
    setup.commit_all();
    setup.write(".gitattributes", "");

    assert_eq!(
        file(&setup.merged(), "generated.txt").fold,
        Some(Fold::Generated)
    );
    assert_eq!(
        file(&setup.live(), "generated.txt").fold,
        None,
        "the worktree no longer marks it"
    );

    setup.write(".gitattributes", "generated.txt linguist-generated\n");
    assert_eq!(
        file(&setup.live(), "generated.txt").fold,
        Some(Fold::Generated)
    );
}

#[test]
fn binary_files_and_files_left_out_are_never_folded() {
    let setup = setup("");
    std::fs::write(setup.task.worktree.join("yarn.lock"), [0_u8, 1, 2]).expect("binary");
    std::fs::write(
        setup.task.worktree.join("Cargo.lock"),
        "x".repeat((1 << 20) + 10),
    )
    .expect("big");

    let live = setup.live();

    let binary = file(&live, "yarn.lock");
    assert!(binary.binary);
    assert_eq!(binary.fold, None);
    let left_out = file(&live, "Cargo.lock");
    assert!(left_out.truncated);
    assert_eq!(left_out.fold, None);
}

#[test]
fn a_change_deep_inside_a_folded_file_changes_its_digest_and_the_fingerprint() {
    let base = numbered(1000);
    let setup = setup_with("", &[("Cargo.lock", base.as_bytes())]);
    setup.write("Cargo.lock", &base.replace("line 700\n", "line 700b\n"));
    setup.write("big.txt", &numbered(700));
    let before = setup.live();

    setup.write("Cargo.lock", &base.replace("line 701\n", "line 701b\n"));
    let lock_changed = setup.live();
    setup.write(
        "big.txt",
        &numbered(700).replace("line 650\n", "line 650b\n"),
    );
    let big_changed = setup.live();

    assert_eq!(file(&before, "Cargo.lock").fold, Some(Fold::Lock));
    assert_ne!(
        file(&lock_changed, "Cargo.lock").digest,
        file(&before, "Cargo.lock").digest
    );
    assert_eq!(
        file(&lock_changed, "big.txt").digest,
        file(&before, "big.txt").digest
    );
    assert_ne!(lock_changed.fingerprint, before.fingerprint);
    assert_ne!(
        file(&big_changed, "big.txt").digest,
        file(&lock_changed, "big.txt").digest
    );
    assert_ne!(big_changed.fingerprint, lock_changed.fingerprint);
}

#[test]
fn an_approval_ends_when_a_folded_file_changes() {
    let base = numbered(1000);
    let setup = setup_with("", &[("Cargo.lock", base.as_bytes())]);
    setup.write("Cargo.lock", &base.replace("line 700\n", "line 700b\n"));
    setup.commit_all();
    let reviewed = setup.merged();
    assert_eq!(file(&reviewed, "Cargo.lock").fold, Some(Fold::Lock));
    setup
        .projects
        .approve(setup.task.id, reviewed.fingerprint)
        .expect("approve");
    setup
        .projects
        .check_approved(setup.task.id)
        .expect("approved");

    setup.write("Cargo.lock", &base.replace("line 700\n", "line 700c\n"));
    setup.commit_all();

    assert!(setup.projects.check_approved(setup.task.id).is_err());
}

/// Every fingerprint and digest must be what they were before files could fold.
fn assert_same_without_folds(read: impl Fn() -> TaskDiff) {
    let folded = read();
    let plain = git::unfolded(&read);

    assert_eq!(folded.fingerprint, plain.fingerprint);
    assert_eq!(folded.stat, plain.stat);
    assert_eq!(folded.files.len(), plain.files.len());

    for (folded, plain) in folded.files.iter().zip(&plain.files) {
        assert_eq!(folded.path, plain.path);
        assert_eq!(folded.digest, plain.digest, "{}", folded.path.display());
        assert_eq!(
            folded.additions,
            plain.additions,
            "{}",
            folded.path.display()
        );
        assert_eq!(
            folded.deletions,
            plain.deletions,
            "{}",
            folded.path.display()
        );
        assert_eq!(plain.fold, None, "folding is off");
    }
    assert!(folded.files.iter().any(|file| file.fold.is_some()));
    assert!(
        !lines(file(&plain, "Cargo.lock")).is_empty(),
        "the switch brings the lines back"
    );
}

#[test]
fn folding_changes_no_digest_and_no_fingerprint() {
    let base = numbered(600);
    let setup = setup_with(
        "",
        &[
            ("Cargo.lock", base.as_bytes()),
            ("old-big.txt", base.as_bytes()),
        ],
    );
    write_folding_files(&setup);
    setup.write("Cargo.lock", &base.replace("line 300\n", "line 300b\n"));
    setup.write("old-big.txt", &numbered(700));
    std::fs::write(setup.task.worktree.join("blob.bin"), [0_u8, 1, 2]).expect("binary");

    assert_same_without_folds(|| setup.live());

    setup.commit_all();
    assert_same_without_folds(|| setup.merged());
}

#[test]
fn folding_changes_nothing_for_a_worker_with_its_own_worktree() {
    let setup = setup("");
    let ui = setup.worker("ui", true);
    write_in(&ui.worktree, "Cargo.lock", &numbered(600));
    write_in(&ui.worktree, "ui.rs", &numbered(3));
    setup.write("Cargo.lock", &numbered(700));

    assert_same_without_folds(|| setup.live());
    let live = setup.live();
    assert!(
        live.files
            .iter()
            .filter(|file| file.path == Path::new("Cargo.lock"))
            .all(|file| file.fold == Some(Fold::Lock)),
        "both locks are folded"
    );
    assert_eq!(
        live.files
            .iter()
            .filter(|file| file.path == Path::new("Cargo.lock"))
            .count(),
        2
    );
}

fn file_hunks(
    setup: &Setup,
    source: ReviewSource,
    worktree: &Path,
    path: &str,
) -> Option<FileDiff> {
    setup
        .projects
        .file_hunks(setup.task.id, source, worktree, Path::new(path))
        .expect("file hunks")
}

#[test]
fn file_hunks_loads_the_lines_of_a_folded_file_in_live_and_merged() {
    let setup = setup("");
    write_folding_files(&setup);
    setup.write("huge.txt", &numbered(DIFF_LINES_PER_FILE + 10));

    for source in [ReviewSource::Live, ReviewSource::Merged] {
        if source == ReviewSource::Merged {
            setup.commit_all();
        }
        let diff = setup
            .projects
            .task_diff(setup.task.id, source)
            .expect("diff");

        for (path, count, fold) in [
            ("Cargo.lock", 600, Fold::Lock),
            ("generated/schema.txt", 20, Fold::Generated),
            ("dist/app.min.js", 5, Fold::Generated),
            ("big.txt", 501, Fold::Large),
        ] {
            let folded = file(&diff, path);
            let loaded = file_hunks(&setup, source, &setup.task.worktree, path)
                .unwrap_or_else(|| panic!("{path} in {source:?}"));

            assert_eq!(loaded.fold, Some(fold), "{path} keeps its tag");
            assert_eq!(lines(&loaded).len(), count, "{path}");
            assert!(!loaded.truncated, "{path}");
            assert_eq!(
                loaded,
                FileDiff {
                    hunks: loaded.hunks.clone(),
                    ..folded.clone()
                },
                "{path} differs from the diff only in its lines"
            );
        }

        let huge = file_hunks(&setup, source, &setup.task.worktree, "huge.txt").expect("huge");
        assert_eq!(huge.fold, Some(Fold::Large));
        assert!(huge.truncated, "the caps still apply");
        assert_eq!(lines(&huge).len(), DIFF_LINES_PER_FILE);
        assert_eq!(huge.digest, file(&diff, "huge.txt").digest);

        let plain = file_hunks(&setup, source, &setup.task.worktree, "normal.txt").expect("file");
        assert_eq!(plain.fold, None);
        assert_eq!(plain, *file(&diff, "normal.txt"));
    }
}

#[test]
fn file_hunks_of_a_tracked_file_has_the_hunks_of_the_diff() {
    let base = numbered(100);
    let setup = setup_with("", &[("Cargo.lock", base.as_bytes())]);
    setup.write("Cargo.lock", &base.replace("line 50\n", "line 50b\n"));

    let diff = setup.live();
    let loaded = file_hunks(
        &setup,
        ReviewSource::Live,
        &setup.task.worktree,
        "Cargo.lock",
    )
    .expect("file");

    assert_eq!(file(&diff, "Cargo.lock").hunks, []);
    assert_eq!(loaded.fold, Some(Fold::Lock));
    assert_eq!(loaded.change, FileChange::Modified);
    assert_eq!((loaded.additions, loaded.deletions), (1, 1));
    assert_eq!(loaded.hunks.len(), 1);
    assert_eq!(loaded.digest, file(&diff, "Cargo.lock").digest);
    assert!(
        lines(&loaded).contains(&(DiffLineKind::Added, "line 50b")),
        "{:?}",
        lines(&loaded)
    );
}

#[test]
fn file_hunks_reads_a_file_of_a_workers_own_worktree() {
    let setup = setup("");
    let ui = setup.worker("ui", true);
    write_in(&ui.worktree, "Cargo.lock", &numbered(600));
    write_in(&ui.worktree, "ui.rs", &numbered(3));
    setup.write("task.rs", &numbered(2));

    let live = setup.live();
    let own = file(&live, "Cargo.lock");
    assert_eq!(own.worktree, ui.worktree);
    let loaded = file_hunks(&setup, ReviewSource::Live, &ui.worktree, "Cargo.lock")
        .expect("the worker's file");

    assert_eq!(loaded.agent, Some(ui.id));
    assert_eq!(loaded.fold, Some(Fold::Lock));
    assert_eq!(lines(&loaded).len(), 600);
    assert_eq!(loaded.digest, own.digest);
    assert_eq!(loaded.worktree, ui.worktree);
    assert!(
        file_hunks(
            &setup,
            ReviewSource::Live,
            &setup.task.worktree,
            "Cargo.lock"
        )
        .is_none(),
        "the task worktree has no such file"
    );
    assert!(
        file_hunks(&setup, ReviewSource::Live, &setup.task.worktree, "ui.rs").is_none(),
        "the worker's other file neither"
    );
    assert!(
        file_hunks(&setup, ReviewSource::Merged, &ui.worktree, "Cargo.lock").is_none(),
        "Merged holds the task worktree only"
    );

    commit("ui", &ui.worktree, &["Cargo.lock", "ui.rs"]);
    let committed =
        file_hunks(&setup, ReviewSource::Live, &ui.worktree, "Cargo.lock").expect("committed");
    assert_eq!(committed.agent, Some(ui.id));
    assert_eq!(lines(&committed).len(), 600);
}

#[test]
fn file_hunks_names_the_same_agent_as_the_diff() {
    let setup = setup("");
    let api = setup.worker("api", false);
    setup.write("Cargo.lock", &numbered(600));
    setup.write("loose.lock", &numbered(2));
    commit("api", &setup.task.worktree, &["Cargo.lock"]);
    setup.write("dirty.txt", &numbered(2));

    let live = setup.live();
    let task_worktree = &setup.task.worktree;

    assert_eq!(file(&live, "Cargo.lock").agent, Some(api.id));
    for path in ["Cargo.lock", "loose.lock", "dirty.txt"] {
        let loaded = file_hunks(&setup, ReviewSource::Live, task_worktree, path).expect(path);

        assert_eq!(loaded.agent, file(&live, path).agent, "{path}");
    }
    assert_eq!(
        file_hunks(&setup, ReviewSource::Live, task_worktree, "dirty.txt")
            .expect("dirty")
            .agent,
        None
    );
}

#[test]
fn file_hunks_answers_none_for_paths_the_diff_does_not_hold() {
    let setup = setup("");
    setup.write("a.txt", "one\n");
    setup.write("ab.txt", "two\n");
    setup.write("a*.txt", "star\n");
    setup.write("folder/inner.txt", "inner\n");
    setup.write("Cargo.lock", &numbered(600));
    setup.commit_all();
    setup.write("untracked.txt", "three\n");
    let worktree = &setup.task.worktree;

    let unchanged = file_hunks(&setup, ReviewSource::Live, worktree, "README.md");
    assert!(unchanged.is_none(), "README.md did not change");
    for path in [
        "missing.txt",
        "../a.txt",
        "folder/../a.txt",
        "./a.txt",
        "/etc/passwd",
        "",
        "folder",
        "folder/",
        "*.txt",
        "a?.txt",
        ":(top)a.txt",
        ".git/config",
        "a.txt\0",
    ] {
        for source in [ReviewSource::Live, ReviewSource::Merged] {
            assert!(
                file_hunks(&setup, source, worktree, path).is_none(),
                "{path:?} in {source:?}"
            );
        }
    }

    let star = file_hunks(&setup, ReviewSource::Merged, worktree, "a*.txt").expect("star");
    assert_eq!(star.path, Path::new("a*.txt"));
    assert_eq!(lines(&star), [(DiffLineKind::Added, "star")]);
    let inner = file_hunks(&setup, ReviewSource::Merged, worktree, "folder/inner.txt");
    assert_eq!(
        lines(&inner.expect("inner")),
        [(DiffLineKind::Added, "inner")]
    );

    assert!(
        file_hunks(&setup, ReviewSource::Merged, worktree, "untracked.txt").is_none(),
        "Merged holds commits only"
    );
    let untracked =
        file_hunks(&setup, ReviewSource::Live, worktree, "untracked.txt").expect("untracked");
    assert_eq!(untracked.change, FileChange::Added);
    assert_eq!(lines(&untracked), [(DiffLineKind::Added, "three")]);
    assert!(
        file_hunks(
            &setup,
            ReviewSource::Live,
            &worktree.join("folder"),
            "inner.txt"
        )
        .is_none(),
        "a folder is no section"
    );
    assert!(
        file_hunks(&setup, ReviewSource::Live, Path::new("/"), "a.txt").is_none(),
        "an outside folder is no section"
    );
}

#[test]
fn file_hunks_reads_a_path_literally() {
    let setup = setup("");
    let names = [
        "a*.txt",
        "ab.txt",
        "[x].txt",
        "x.txt",
        ":colon.txt",
        "-dash.txt",
    ];
    for name in names {
        setup.write(name, &format!("{name}\n"));
    }
    setup.commit_all();

    for name in names {
        let loaded = file_hunks(&setup, ReviewSource::Merged, &setup.task.worktree, name)
            .unwrap_or_else(|| panic!("{name}"));

        assert_eq!(loaded.path, Path::new(name));
        assert_eq!(lines(&loaded), [(DiffLineKind::Added, name)]);
    }
}

#[test]
fn file_hunks_loads_an_untracked_lock_file_and_a_deleted_one() {
    let base = numbered(600);
    let setup = setup_with("", &[("yarn.lock", base.as_bytes())]);
    std::fs::remove_file(setup.task.worktree.join("yarn.lock")).expect("remove");
    setup.write("Cargo.lock", &numbered(700));

    let live = setup.live();

    let deleted = file_hunks(
        &setup,
        ReviewSource::Live,
        &setup.task.worktree,
        "yarn.lock",
    )
    .expect("deleted");
    assert_eq!(deleted.change, FileChange::Deleted);
    assert_eq!(deleted.fold, Some(Fold::Lock));
    assert_eq!((deleted.additions, deleted.deletions), (0, 600));
    assert_eq!(lines(&deleted).len(), 600);
    assert_eq!(deleted.digest, file(&live, "yarn.lock").digest);
    let added = file_hunks(
        &setup,
        ReviewSource::Live,
        &setup.task.worktree,
        "Cargo.lock",
    )
    .expect("untracked");
    assert_eq!(added.change, FileChange::Added);
    assert_eq!(lines(&added).len(), 700);
}

#[test]
fn file_hunks_for_an_unknown_task_is_an_error() {
    let setup = setup("");

    let unknown = setup.projects.file_hunks(
        swarm_protocol::TaskId(9999),
        ReviewSource::Live,
        &setup.task.worktree,
        Path::new("a.txt"),
    );

    assert!(matches!(unknown, Err(Error::Invalid(_))), "{unknown:?}");
}
