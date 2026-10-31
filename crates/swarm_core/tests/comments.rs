//! Review comments against real temporary git repositories.

#![expect(clippy::expect_used, reason = "test helpers panic on failure")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use swarm_core::store::{NewAgent, Store};
use swarm_core::{Projects, WorkerCheckout, git};
use swarm_protocol::{
    AgentInfo, AgentKind, AgentRole, COMMENT_BYTES, CommentAnchor, CommentId, CommentState,
    CreateTask, DiffSide, NewComment, ReviewComment, ReviewSource, TaskInfo,
};

static NEXT_FOLDER: AtomicU32 = AtomicU32::new(0);

struct Repo {
    root: PathBuf,
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

struct Setup {
    repo: Repo,
    projects: Projects,
    store: Arc<Mutex<Store>>,
    task: TaskInfo,
    orchestrator: AgentInfo,
}

fn setup(file: &str) -> Setup {
    let number = NEXT_FOLDER.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("swarm-comments-it-{}-{number}", std::process::id()));
    std::fs::create_dir_all(root.join(".swarm")).expect("folders");
    let root = root.canonicalize().expect("real path");
    std::fs::write(root.join(".swarm/config.toml"), "").expect("config");
    std::fs::write(root.join("src.rs"), file).expect("file");
    let repo = Repo { root };
    git_in(&repo.root, &["init", "-q"]);
    git_in(&repo.root, &["add", "src.rs"]);
    git_in(&repo.root, &["commit", "-q", "-m", "first"]);

    let store = Arc::new(Mutex::new(Store::open_in_memory().expect("store")));
    let projects = Projects::new(Arc::clone(&store));
    let project = projects.add(&repo.root).expect("add");
    let task = new_task(&projects, project.id, "Login");
    let orchestrator = add_agent(&store, &task, AgentRole::Orchestrator, "orchestrator");

    Setup {
        repo,
        projects,
        store,
        task,
        orchestrator,
    }
}

fn new_task(projects: &Projects, project: swarm_protocol::ProjectId, name: &str) -> TaskInfo {
    projects
        .create_task(&CreateTask {
            project,
            name: name.to_owned(),
            from: None,
            run_setup: false,
            orchestrator: None,
            first_message: None,
        })
        .expect("task")
        .task
}

fn add_agent(store: &Mutex<Store>, task: &TaskInfo, role: AgentRole, name: &str) -> AgentInfo {
    store
        .lock()
        .expect("lock")
        .add_agent(&NewAgent {
            project: task.project,
            task: Some(task.id),
            role,
            kind: AgentKind::ClaudeCode,
            model: "sonnet",
            session_id: None,
            name,
            worktree: &task.worktree,
            branch: &task.branch,
            ports: &[],
        })
        .expect("agent")
}

impl Setup {
    fn path(&self) -> PathBuf {
        self.task.worktree.join("src.rs")
    }

    fn write(&self, content: &str) {
        std::fs::write(self.path(), content).expect("write");
    }

    fn commit(&self, agent: &str, path: &str) {
        git::commit_paths(
            &self.task.worktree,
            agent,
            "feat: work",
            &[PathBuf::from(path)],
        )
        .expect("commit");
    }

    fn worker(&self, name: &str, own_worktree: bool) -> AgentInfo {
        let register = |checkout: &WorkerCheckout| {
            self.store.lock().expect("lock").add_agent(&NewAgent {
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
        };
        let prepared = if own_worktree {
            self.projects.create_worker(&self.task, name, register)
        } else {
            self.projects
                .create_shared_worker(&self.task, name, register)
        };

        prepared.expect("worker").agent
    }

    fn anchor(
        &self,
        line: u32,
        text: &str,
        before: Option<&str>,
        after: Option<&str>,
    ) -> CommentAnchor {
        CommentAnchor {
            worktree: self.task.worktree.clone(),
            path: PathBuf::from("src.rs"),
            side: DiffSide::New,
            line,
            text: text.to_owned(),
            before: before.map(str::to_owned),
            after: after.map(str::to_owned),
        }
    }

    fn comment(&self, source: ReviewSource, anchor: CommentAnchor, body: &str) -> NewComment {
        NewComment {
            task: self.task.id,
            parent: None,
            source,
            anchor,
            body: body.to_owned(),
        }
    }

    fn reply(&self, parent: CommentId, body: &str) -> NewComment {
        NewComment {
            parent: Some(parent),
            ..self.comment(
                ReviewSource::Live,
                self.anchor(1, "ignored", None, None),
                body,
            )
        }
    }

    fn add(&self, source: ReviewSource, line: u32, text: &str, body: &str) -> ReviewComment {
        let comment = self.comment(source, self.anchor(line, text, None, None), body);

        self.projects.add_comment(&comment).expect("add").comment
    }

    fn send(&self, comment: &ReviewComment) -> String {
        let outgoing = self
            .projects
            .outgoing(self.task.id, &[comment.id], false)
            .expect("outgoing");
        self.projects
            .mark_comments_sent(&outgoing.owners)
            .expect("sent");

        outgoing.text
    }

    fn listed(&self) -> Vec<ReviewComment> {
        self.projects.list_comments(self.task.id).expect("list")
    }
}

const FILE: &str = "one\ntwo\nthree\nfour\n";

#[test]
fn live_is_sent_at_once_and_merged_waits() {
    let setup = setup(FILE);

    let live = setup
        .projects
        .add_comment(&setup.comment(ReviewSource::Live, setup.anchor(2, "two", None, None), "a"))
        .expect("live");
    let merged = setup
        .projects
        .add_comment(&setup.comment(
            ReviewSource::Merged,
            setup.anchor(2, "two", None, None),
            "b",
        ))
        .expect("merged");

    assert!(live.send_now);
    assert!(!merged.send_now);
    assert_eq!(live.comment.state, CommentState::Pending);
}

#[test]
fn the_message_names_the_worker_that_owns_the_file() {
    let setup = setup(FILE);
    let api = setup.worker("api", false);
    setup.write("one\ntwo\nthree\nfour\nfive\n");
    setup.commit("api", "src.rs");
    let comment = setup.add(ReviewSource::Live, 2, "two", "Cache it.");

    let outgoing = setup
        .projects
        .outgoing(setup.task.id, &[comment.id], false)
        .expect("outgoing");

    assert_eq!(
        outgoing.text,
        format!("#{} for api · src.rs:2\n> two\nCache it.", comment.id.0)
    );
    assert_eq!(outgoing.owners, [(comment.id, Some(api.id))]);
    assert_eq!(outgoing.orchestrator, setup.orchestrator.id);

    let sent = setup
        .projects
        .mark_comments_sent(&outgoing.owners)
        .expect("sent");
    assert_eq!(sent[0].state, CommentState::Sent);
    assert_eq!(sent[0].to, Some(api.id));
    assert!(sent[0].sent_at.is_some());
}

#[test]
fn a_file_in_an_own_worktree_belongs_to_that_worker() {
    let setup = setup(FILE);
    let ui = setup.worker("ui", true);
    std::fs::write(ui.worktree.join("src.rs"), "one\ntwo\nthree\nfour\n").expect("file");
    let mut anchor = setup.anchor(1, "one", None, None);
    anchor.worktree = ui.worktree.clone();
    let added = setup
        .projects
        .add_comment(&setup.comment(ReviewSource::Live, anchor, "Rename."))
        .expect("add");

    let outgoing = setup
        .projects
        .outgoing(setup.task.id, &[added.comment.id], false)
        .expect("outgoing");

    assert_eq!(outgoing.owners, [(added.comment.id, Some(ui.id))]);
    assert!(outgoing.text.contains(" for ui · "));
}

#[test]
fn nobody_owns_uncommitted_and_plain_commit_files() {
    let setup = setup(FILE);
    setup.worker("api", false);
    let comment = setup.add(ReviewSource::Live, 2, "two", "Why?");

    let outgoing = setup
        .projects
        .outgoing(setup.task.id, &[comment.id], false)
        .expect("outgoing");

    assert_eq!(outgoing.owners, [(comment.id, None)]);
    assert!(
        outgoing
            .text
            .starts_with(&format!("#{} for you · src.rs:2", comment.id.0))
    );
}

#[test]
fn a_file_the_orchestrator_committed_is_the_orchestrators() {
    let setup = setup(FILE);
    setup.write("one\ntwo\nthree\nfour\nfive\n");
    setup.commit("orchestrator", "src.rs");
    let comment = setup.add(ReviewSource::Live, 2, "two", "Why?");

    let outgoing = setup
        .projects
        .outgoing(setup.task.id, &[comment.id], false)
        .expect("outgoing");

    assert_eq!(outgoing.owners, [(comment.id, None)]);
}

#[test]
fn the_old_side_and_a_reply_are_marked_in_the_message() {
    let setup = setup(FILE);
    let mut anchor = setup.anchor(3, "three", None, None);
    anchor.side = DiffSide::Old;
    let parent = setup
        .projects
        .add_comment(&setup.comment(ReviewSource::Live, anchor, "Why removed?"))
        .expect("add")
        .comment;
    let first = setup.send(&parent);
    let reply = setup
        .projects
        .add_comment(&setup.reply(parent.id, "Still unclear."))
        .expect("reply")
        .comment;

    let second = setup.send(&reply);

    assert!(first.starts_with(&format!(
        "#{} for you · src.rs:3 (old)\n> three\n",
        parent.id.0
    )));
    assert_eq!(
        second,
        format!(
            "#{} reply to #{} for you · src.rs:3 (old)\n> three\nStill unclear.",
            reply.id.0, parent.id.0
        )
    );
}

#[test]
fn all_pending_comments_go_in_one_message() {
    let setup = setup(FILE);
    let first = setup.add(ReviewSource::Merged, 1, "one", "A");
    let second = setup.add(ReviewSource::Merged, 4, "four", "B");
    let reply = setup
        .projects
        .add_comment(&setup.reply(first.id, "C"))
        .expect("reply");
    assert!(!reply.send_now, "the parent waits");

    let pending = setup
        .projects
        .pending_comments(setup.task.id)
        .expect("pending");
    assert_eq!(pending, [first.id, second.id, reply.comment.id]);
    let outgoing = setup
        .projects
        .outgoing(setup.task.id, &pending, true)
        .expect("outgoing");

    let blocks: Vec<&str> = outgoing.text.split("\n\n").collect();
    assert_eq!(blocks.len(), 4);
    assert_eq!(blocks[0], "3 comments:");
    assert!(blocks[1].starts_with(&format!("#{} for you · src.rs:1\n> one\nA", first.id.0)));
    assert!(blocks[3].starts_with(&format!(
        "#{} reply to #{} for ",
        reply.comment.id.0, first.id.0
    )));

    setup
        .projects
        .mark_comments_sent(&outgoing.owners)
        .expect("sent");
    assert!(
        setup
            .projects
            .pending_comments(setup.task.id)
            .expect("pending")
            .is_empty()
    );
}

#[test]
fn a_reply_follows_its_parent_and_takes_its_anchor() {
    let setup = setup(FILE);
    let pending = setup.add(ReviewSource::Merged, 2, "two", "A");
    let sent = setup.add(ReviewSource::Live, 3, "three", "B");
    setup.send(&sent);

    let waits = setup
        .projects
        .add_comment(&setup.reply(pending.id, "x"))
        .expect("reply");
    let goes = setup
        .projects
        .add_comment(&setup.reply(sent.id, "y"))
        .expect("reply");

    assert!(!waits.send_now);
    assert!(goes.send_now);
    assert_eq!(goes.comment.anchor.text, "three");
    assert_eq!(goes.comment.anchor.line, 3);
    assert_eq!(goes.comment.parent, Some(sent.id));

    setup
        .projects
        .resolve_comment(sent.id, true)
        .expect("resolve");
    let after_resolve = setup
        .projects
        .add_comment(&setup.reply(sent.id, "z"))
        .expect("reply");
    assert!(after_resolve.send_now);
    let parent = setup.projects.list_comments(setup.task.id).expect("list");
    assert_eq!(
        parent
            .iter()
            .find(|comment| comment.id == sent.id)
            .map(|c| c.state),
        Some(CommentState::Resolved),
        "a reply reopens nothing"
    );
}

#[test]
fn a_bad_comment_is_refused() {
    let setup = setup(FILE);
    let reply_parent = setup.add(ReviewSource::Live, 2, "two", "A");
    setup.send(&reply_parent);
    let reply = setup
        .projects
        .add_comment(&setup.reply(reply_parent.id, "r"))
        .expect("reply");
    let other = new_task(&setup.projects, setup.task.project, "Other");
    let in_other = setup.projects.add_comment(&NewComment {
        task: other.id,
        ..setup.comment(ReviewSource::Merged, setup.anchor(1, "x", None, None), "o")
    });
    assert!(in_other.is_err(), "the worktree belongs to another task");

    let mut cases: Vec<(&str, NewComment)> = Vec::new();
    let base = |anchor| setup.comment(ReviewSource::Live, anchor, "ok");

    let mut elsewhere = setup.anchor(1, "x", None, None);
    elsewhere.worktree = setup.repo.root.clone();
    cases.push(("other worktree", base(elsewhere)));
    for path in ["../x", "/etc/passwd", "a/../../x", "", ".git/config"] {
        let mut anchor = setup.anchor(1, "x", None, None);
        anchor.path = PathBuf::from(path);
        cases.push((path, base(anchor)));
    }
    cases.push((
        "empty",
        setup.comment(ReviewSource::Live, setup.anchor(1, "x", None, None), "  \n"),
    ));
    cases.push((
        "long",
        setup.comment(
            ReviewSource::Live,
            setup.anchor(1, "x", None, None),
            &"x".repeat(COMMENT_BYTES + 1),
        ),
    ));
    cases.push(("reply to reply", setup.reply(reply.comment.id, "no")));
    cases.push(("no parent", setup.reply(CommentId(999), "no")));
    cases.push((
        "closed task",
        NewComment {
            task: swarm_protocol::TaskId(999),
            ..base(setup.anchor(1, "x", None, None))
        },
    ));

    for (name, case) in cases {
        assert!(setup.projects.add_comment(&case).is_err(), "{name}");
    }

    let at_limit = setup.comment(
        ReviewSource::Live,
        setup.anchor(1, "x", None, None),
        &"x".repeat(COMMENT_BYTES),
    );
    assert!(setup.projects.add_comment(&at_limit).is_ok());

    let other_parent = NewComment {
        task: other.id,
        ..setup.reply(reply_parent.id, "cross")
    };
    assert!(
        setup.projects.add_comment(&other_parent).is_err(),
        "parent in another task"
    );
}

#[test]
fn only_a_pending_comment_is_deleted_with_its_replies() {
    let setup = setup(FILE);
    let parent = setup.add(ReviewSource::Merged, 1, "one", "A");
    let reply = setup
        .projects
        .add_comment(&setup.reply(parent.id, "B"))
        .expect("reply")
        .comment;
    let sent = setup.add(ReviewSource::Live, 2, "two", "C");
    setup.send(&sent);

    assert!(setup.projects.delete_comment(sent.id).is_err());

    let (task, deleted) = setup.projects.delete_comment(parent.id).expect("delete");

    assert_eq!(task, setup.task.id);
    assert_eq!(deleted, [reply.id, parent.id]);
    let left: Vec<CommentId> = setup.listed().iter().map(|comment| comment.id).collect();
    assert_eq!(left, [sent.id]);
}

#[test]
fn only_a_sent_first_comment_is_resolved() {
    let setup = setup(FILE);
    let pending = setup.add(ReviewSource::Merged, 1, "one", "A");
    let sent = setup.add(ReviewSource::Live, 2, "two", "B");
    setup.send(&sent);
    let reply = setup
        .projects
        .add_comment(&setup.reply(sent.id, "C"))
        .expect("reply")
        .comment;
    setup.send(&reply);

    assert!(setup.projects.resolve_comment(pending.id, true).is_err());
    assert!(setup.projects.resolve_comment(reply.id, true).is_err());
    assert!(
        setup
            .projects
            .resolve_comment(CommentId(999), true)
            .is_err()
    );

    assert_eq!(
        setup
            .projects
            .resolve_comment(sent.id, true)
            .expect("resolve")
            .state,
        CommentState::Resolved
    );
    assert_eq!(
        setup
            .projects
            .resolve_comment(sent.id, false)
            .expect("reopen")
            .state,
        CommentState::Sent
    );
}

#[test]
fn the_anchor_moves_when_lines_are_inserted_above() {
    let setup = setup(FILE);
    let comment = setup.add(ReviewSource::Live, 3, "three", "A");

    setup.write("new\nnew\none\ntwo\nthree\nfour\n");
    let listed = setup.listed();

    assert_eq!(listed[0].id, comment.id);
    assert_eq!(listed[0].anchor.line, 5);
    assert!(!listed[0].outdated);
    let stored = setup
        .store
        .lock()
        .expect("lock")
        .comment(comment.id)
        .expect("read")
        .expect("row");
    assert_eq!(stored.anchor.line, 5, "the new line is saved");
}

#[test]
fn equal_lines_are_told_apart_by_their_neighbors() {
    let setup = setup("x\nb\nx\nc\nx\n");
    let comment = setup
        .projects
        .add_comment(&setup.comment(
            ReviewSource::Live,
            setup.anchor(3, "x", Some("b"), Some("c")),
            "A",
        ))
        .expect("add")
        .comment;

    setup.write("x\nc\nx\nb\nx\nc\nx\n");

    assert_eq!(setup.listed()[0].anchor.line, 5, "{comment:?}");
}

#[test]
fn equal_lines_without_neighbors_pick_the_nearest() {
    let setup = setup("x\na\nx\nb\nx\n");
    setup.add(ReviewSource::Live, 3, "x", "A");

    setup.write("x\nq\nx\nq\nq\nx\n");

    assert_eq!(setup.listed()[0].anchor.line, 3);
}

#[test]
fn a_gone_line_is_outdated_and_comes_back() {
    let setup = setup(FILE);
    setup.add(ReviewSource::Live, 3, "three", "A");

    setup.write("one\ntwo\nfour\n");
    let gone = setup.listed();
    assert!(gone[0].outdated);
    assert_eq!(gone[0].anchor.line, 3, "the line stays where it was");

    setup.write("zero\none\ntwo\nthree\nfour\n");
    let back = setup.listed();
    assert!(!back[0].outdated);
    assert_eq!(back[0].anchor.line, 4);

    std::fs::remove_file(setup.path()).expect("remove");
    assert!(setup.listed()[0].outdated, "a missing file is outdated");
}

#[test]
fn the_old_side_reads_the_base_version() {
    let setup = setup(FILE);
    let mut anchor = setup.anchor(3, "three", None, None);
    anchor.side = DiffSide::Old;
    setup
        .projects
        .add_comment(&setup.comment(ReviewSource::Live, anchor, "A"))
        .expect("add");

    setup.write("zero\none\ntwo\nfour\n");
    let listed = setup.listed();

    assert!(!listed[0].outdated);
    assert_eq!(listed[0].anchor.line, 3, "the base version did not change");

    let mut gone = setup.anchor(2, "missing", None, None);
    gone.side = DiffSide::Old;
    setup
        .projects
        .add_comment(&setup.comment(ReviewSource::Live, gone, "B"))
        .expect("add");
    assert!(setup.listed()[1].outdated);
}

#[test]
fn merged_reads_head_and_not_the_working_tree() {
    let setup = setup(FILE);
    setup.add(ReviewSource::Merged, 3, "three", "A");

    setup.write("zero\none\ntwo\nthree\nfour\n");
    let uncommitted = setup.listed();
    assert_eq!(
        uncommitted[0].anchor.line, 3,
        "uncommitted work does not count"
    );

    setup.commit("api", "src.rs");
    assert_eq!(setup.listed()[0].anchor.line, 4);

    setup.write("one\ntwo\nfour\n");
    let still = setup.listed();
    assert!(!still[0].outdated, "HEAD still has the line");
}

#[test]
fn a_comment_in_an_own_worktree_follows_that_file() {
    let setup = setup(FILE);
    let ui = setup.worker("ui", true);
    let mut anchor = setup.anchor(2, "two", None, None);
    anchor.worktree = ui.worktree.clone();
    setup
        .projects
        .add_comment(&setup.comment(ReviewSource::Live, anchor, "A"))
        .expect("add");

    std::fs::write(ui.worktree.join("src.rs"), "a\nb\none\ntwo\n").expect("file");

    assert_eq!(setup.listed()[0].anchor.line, 4);
}

#[test]
fn a_path_through_a_linked_folder_is_not_read() {
    let setup = setup(FILE);
    let outside = setup.repo.root.join("outside");
    std::fs::create_dir_all(&outside).expect("folder");
    std::fs::write(outside.join("secret.rs"), "needle\n").expect("file");
    std::os::unix::fs::symlink(&outside, setup.task.worktree.join("link")).expect("link");
    let mut anchor = setup.anchor(1, "needle", None, None);
    anchor.path = PathBuf::from("link/secret.rs");
    setup
        .projects
        .add_comment(&setup.comment(ReviewSource::Live, anchor, "A"))
        .expect("add");

    assert!(setup.listed()[0].outdated);
}

#[test]
fn a_closed_task_has_no_listed_comments() {
    let setup = setup(FILE);
    let comment = setup.add(ReviewSource::Merged, 1, "one", "A");
    assert_eq!(setup.listed().len(), 1);

    setup.projects.close_task(setup.task.id).expect("close");

    assert!(setup.listed().is_empty());
    assert!(
        setup
            .projects
            .pending_comments(setup.task.id)
            .expect("pending")
            .is_empty()
    );
    assert!(setup.projects.delete_comment(comment.id).is_err());
    let row = setup
        .store
        .lock()
        .expect("lock")
        .comment(comment.id)
        .expect("read");
    assert!(row.is_some(), "the row stays in the database");
    assert!(
        setup
            .projects
            .list_comments(swarm_protocol::TaskId(999))
            .is_err()
    );
}

#[test]
fn a_task_without_an_orchestrator_cannot_send() {
    let setup = setup(FILE);
    let other = new_task(&setup.projects, setup.task.project, "Bare");
    let anchor = CommentAnchor {
        worktree: other.worktree.clone(),
        ..setup.anchor(1, "one", None, None)
    };
    let added = setup
        .projects
        .add_comment(&NewComment {
            task: other.id,
            ..setup.comment(ReviewSource::Merged, anchor, "A")
        })
        .expect("add");

    assert!(
        setup
            .projects
            .outgoing(other.id, &[added.comment.id], false)
            .is_err()
    );
}
