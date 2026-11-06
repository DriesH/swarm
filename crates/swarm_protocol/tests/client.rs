//! Client tests against a fake daemon on a temp socket.

#![allow(clippy::expect_used, reason = "test helpers outside #[test] functions")]

use std::io::{BufReader, BufWriter, Write};
use std::os::unix::fs::DirBuilderExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use swarm_protocol::{
    AgentChange, AgentId, AgentInfo, AgentKind, AgentLifecycle, AgentRole, AgentStatus, CallId,
    Client, ClientError, CloseFailure, CloseStep, CreateTask, CreateTerminal, DiffStat, Event,
    FileStat, GridSize, MessageEvent, MessageId, MessageInfo, MessageKind, PlanDraft, PrEvent,
    ProjectId, ProjectInfo, Request, TaskBranch, TaskBranchList, TaskId, TaskInfo, TerminalId,
    TerminalInfo, TerminalKind, VERSION, read, write,
};
use swarm_term::{Palette, Rgb, Size, TerminalInput, TerminalOutput};

const WAIT: Duration = Duration::from_secs(5);

static NEXT_SOCKET: AtomicU32 = AtomicU32::new(0);

/// The daemon's end of one connection.
struct Fake {
    reader: BufReader<UnixStream>,
    writer: BufWriter<UnixStream>,
    buffer: Vec<u8>,
}

impl Fake {
    fn request(&mut self) -> Option<Request> {
        read(&mut self.reader, &mut self.buffer).ok()
    }

    fn send(&mut self, event: &Event) {
        write(&mut self.writer, event).expect("fake daemon can write");
        self.writer.flush().expect("fake daemon can flush");
    }

    fn handshake(&mut self, version: u32) {
        assert_eq!(self.request(), Some(Request::Hello { version: VERSION }));
        self.send(&Event::Hello {
            version,
            pid: 1,
            build: "9.9.9".to_owned(),
        });

        if version == VERSION {
            self.challenge("fake-nonce");
        }
    }

    /// Sends a challenge and checks the client answers it with the same nonce.
    fn challenge(&mut self, nonce: &str) {
        self.send(&Event::Challenge {
            nonce: nonce.to_owned(),
        });
        assert_eq!(
            self.request(),
            Some(Request::Prove {
                nonce: nonce.to_owned()
            })
        );
    }

    /// Reads requests until the client hangs up.
    fn drain(&mut self) {
        while self.request().is_some() {}
    }
}

/// Serves one connection with `script` and returns the socket path.
fn serve(script: impl FnOnce(&mut Fake) + Send + 'static) -> PathBuf {
    let folder = PathBuf::from(format!(
        "/tmp/swc-{}-{}",
        std::process::id(),
        NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&folder)
        .expect("create the private folder");
    let socket = folder.join("d.sock");
    let listener = UnixListener::bind(&socket).expect("bind the fake socket");
    thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let mut fake = Fake {
                reader: BufReader::new(stream.try_clone().expect("clone the stream")),
                writer: BufWriter::new(stream),
                buffer: Vec::new(),
            };
            script(&mut fake);
        }

        std::fs::remove_dir_all(&folder).ok();
    });

    socket
}

fn create_request() -> CreateTerminal {
    CreateTerminal {
        kind: TerminalKind::User,
        command: None,
        cwd: None,
        size: Size {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        },
        palette: Palette {
            fg: Rgb(0, 0, 0),
            bg: Rgb(255, 255, 255),
            cursor: Rgb(0, 0, 0),
            ansi: [Rgb(0, 0, 0); 16],
        },
        task: Some("swarm/terminal-renderer".into()),
    }
}

/// A fake that answers `Create` with terminal 7 and then runs `after`.
fn serve_created(after: impl FnOnce(&mut Fake) + Send + 'static) -> PathBuf {
    serve(move |fake| {
        fake.handshake(VERSION);
        let Some(Request::Create { call, .. }) = fake.request() else {
            panic!("expected a Create request");
        };
        fake.send(&Event::Created {
            call,
            id: TerminalId(7),
        });
        after(fake);
    })
}

fn wait_until(condition: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + WAIT;

    while Instant::now() < deadline {
        if condition() {
            return true;
        }

        thread::yield_now();
    }

    false
}

#[test]
fn connect_completes_the_handshake() {
    let socket = serve(|fake| {
        fake.handshake(VERSION);
        fake.drain();
    });

    let client = Client::connect(&socket).expect("connect");

    assert!(client.is_connected());
}

#[test]
fn connect_answers_the_challenge_with_its_nonce() {
    let (proved, proof) = flume::bounded(1);
    let socket = serve(move |fake| {
        assert_eq!(fake.request(), Some(Request::Hello { version: VERSION }));
        fake.send(&Event::Hello {
            version: VERSION,
            pid: 1,
            build: "9.9.9".to_owned(),
        });
        fake.send(&Event::Challenge {
            nonce: "abc123".to_owned(),
        });

        proved.send(fake.request()).expect("test is listening");
        fake.drain();
    });

    let client = Client::connect(&socket).expect("connect");

    assert_eq!(
        proof.recv_timeout(WAIT).expect("the client answered"),
        Some(Request::Prove {
            nonce: "abc123".to_owned()
        })
    );
    assert!(client.is_connected());
}

#[test]
fn connect_fails_when_the_daemon_skips_the_challenge() {
    let socket = serve(|fake| {
        assert_eq!(fake.request(), Some(Request::Hello { version: VERSION }));
        fake.send(&Event::Hello {
            version: VERSION,
            pid: 1,
            build: "9.9.9".to_owned(),
        });
        fake.send(&Event::Done { call: CallId(1) });
        fake.drain();
    });

    let result = Client::connect(&socket);

    assert!(matches!(result, Err(ClientError::Daemon(_))));
}

#[test]
fn connect_refuses_another_protocol_version() {
    let socket = serve(|fake| fake.handshake(VERSION + 1));

    let result = Client::connect(&socket);

    assert!(matches!(
        result,
        Err(ClientError::Version { daemon, .. }) if daemon == VERSION + 1
    ));
}

#[test]
fn a_protocol_mismatch_carries_the_daemons_build() {
    let socket = serve(|fake| fake.handshake(VERSION + 1));

    let result = Client::connect(&socket);

    assert!(matches!(
        result,
        Err(ClientError::Version { build: Some(build), .. }) if build == "9.9.9"
    ));
}

#[test]
fn the_client_knows_the_build_of_a_daemon_it_talks_to() {
    let socket = serve(|fake| {
        fake.handshake(VERSION);
        fake.drain();
    });

    let client = Client::connect(&socket).expect("connect");

    assert_eq!(client.daemon_build(), "9.9.9");
}

/// The first reply of a daemon from before the build field: only `Hello`, two fields.
#[derive(serde::Serialize, serde::Deserialize)]
enum LegacyEvent {
    Hello { version: u32, pid: u32 },
}

#[test]
fn a_daemon_from_before_the_build_field_reports_its_protocol_version() {
    let socket = serve(|fake| {
        assert_eq!(fake.request(), Some(Request::Hello { version: VERSION }));
        write(
            &mut fake.writer,
            &LegacyEvent::Hello {
                version: VERSION - 1,
                pid: 1,
            },
        )
        .expect("fake daemon can write");
        fake.writer.flush().expect("fake daemon can flush");
    });

    let result = Client::connect(&socket);

    assert!(matches!(
        result,
        Err(ClientError::Version { daemon, build: None }) if daemon == VERSION - 1
    ));
}

#[test]
fn an_older_client_still_reads_the_version_and_pid_of_the_new_hello() {
    let hello = Event::Hello {
        version: VERSION,
        pid: 7,
        build: "1.2.3".to_owned(),
    };
    let frame = postcard::to_stdvec(&hello).expect("encode");

    let old: LegacyEvent = postcard::from_bytes(&frame).expect("an older client decodes it");

    assert!(matches!(
        old,
        LegacyEvent::Hello { version, pid: 7 } if version == VERSION
    ));
}

#[test]
fn connect_reports_a_daemon_it_cannot_read() {
    let socket = serve(|fake| {
        assert_eq!(fake.request(), Some(Request::Hello { version: VERSION }));

        // An older daemon's error frame: a variant this version cannot decode.
        fake.writer
            .write_all(&[2, 0, 0, 0, 9, 7])
            .expect("fake daemon can write");
        fake.writer.flush().expect("fake daemon can flush");
    });

    let result = Client::connect(&socket);

    assert!(matches!(result, Err(ClientError::Incompatible)));
}

#[test]
fn list_returns_what_the_daemon_sends() {
    let info = TerminalInfo {
        id: TerminalId(3),
        kind: TerminalKind::User,
        task: Some("swarm/terminal-renderer".into()),
        owner: None,
        name: None,
        fixed_size: None,
        title: Some("zsh".into()),
        cwd: None,
        exit: None,
    };
    let sent = vec![info.clone()];
    let socket = serve(move |fake| {
        fake.handshake(VERSION);
        let Some(Request::List { call }) = fake.request() else {
            panic!("expected a List request");
        };
        fake.send(&Event::Terminals {
            call,
            terminals: sent,
        });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");

    let terminals = client.list().expect("list");

    assert_eq!(terminals, vec![info]);
}

#[test]
fn list_reports_a_daemon_error() {
    let socket = serve(|fake| {
        fake.handshake(VERSION);
        let Some(Request::List { call }) = fake.request() else {
            panic!("expected a List request");
        };
        fake.send(&Event::Error {
            call: Some(call),
            id: None,
            message: "no".into(),
        });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");

    let result = client.list();

    assert!(matches!(result, Err(ClientError::Daemon(message)) if message == "no"));
}

#[test]
fn create_routes_output_to_the_returned_channels() {
    let socket = serve_created(|fake| {
        fake.send(&Event::Output {
            id: TerminalId(7),
            output: TerminalOutput::Title("hello".into()),
        });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");

    let (id, channels) = client.create(create_request()).expect("create");
    let output = channels.output.recv_timeout(WAIT);

    assert_eq!(id, TerminalId(7));
    assert_eq!(output, Ok(TerminalOutput::Title("hello".into())));
}

#[test]
fn input_from_the_channels_reaches_the_daemon() {
    let (seen_sender, seen) = flume::unbounded();
    let socket = serve_created(move |fake| {
        while let Some(request) = fake.request() {
            if matches!(request, Request::Input { .. }) {
                seen_sender.send(request).expect("test is listening");
            }
        }
    });
    let client = Client::connect(&socket).expect("connect");
    let (_, channels) = client.create(create_request()).expect("create");

    channels
        .input
        .send(TerminalInput::Text("ls".into()))
        .expect("send input");

    assert_eq!(
        seen.recv_timeout(WAIT),
        Ok(Request::Input {
            id: TerminalId(7),
            input: TerminalInput::Text("ls".into()),
        })
    );
}

#[test]
fn closed_ends_the_output() {
    let socket = serve_created(|fake| {
        fake.send(&Event::Closed { id: TerminalId(7) });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");
    let (_, channels) = client.create(create_request()).expect("create");

    let output = channels.output.recv_timeout(WAIT);

    assert_eq!(output, Err(flume::RecvTimeoutError::Disconnected));
    assert!(client.is_connected());
}

#[test]
fn dropping_the_server_ends_all_outputs() {
    let socket = serve_created(|_| {});
    let client = Client::connect(&socket).expect("connect");
    let (_, channels) = client.create(create_request()).expect("create");

    let output = channels.output.recv_timeout(WAIT);

    assert_eq!(output, Err(flume::RecvTimeoutError::Disconnected));
    assert!(wait_until(|| !client.is_connected()));
}

#[test]
fn requests_fail_after_the_server_is_gone() {
    let (stop, stopped) = flume::bounded::<()>(1);
    let socket = serve(move |fake| {
        fake.handshake(VERSION);
        stopped.recv().ok();
    });
    let client = Client::connect(&socket).expect("connect");
    stop.send(()).expect("server is waiting");
    assert!(wait_until(|| !client.is_connected()));

    let result = client.list();

    assert!(matches!(result, Err(ClientError::Disconnected)));
}

#[test]
fn attach_sends_the_request_and_routes_the_snapshot_stream() {
    let socket = serve(|fake| {
        fake.handshake(VERSION);
        assert_eq!(
            fake.request(),
            Some(Request::Attach {
                id: TerminalId(4),
                history: 1_000
            })
        );
        fake.send(&Event::Output {
            id: TerminalId(4),
            output: TerminalOutput::Bell,
        });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");

    let channels = client.attach(TerminalId(4), 1_000).expect("attach");

    assert_eq!(channels.output.recv_timeout(WAIT), Ok(TerminalOutput::Bell));
}

#[test]
fn dropping_the_channels_detaches() {
    let (seen_sender, seen) = flume::unbounded();
    let socket = serve(move |fake| {
        fake.handshake(VERSION);
        while let Some(request) = fake.request() {
            seen_sender.send(request).expect("test is listening");
        }
    });
    let client = Client::connect(&socket).expect("connect");
    let channels = client.attach(TerminalId(4), 10).expect("attach");

    drop(channels);

    let requests: Vec<Request> = std::iter::from_fn(|| seen.recv_timeout(WAIT).ok())
        .take(2)
        .collect();
    assert_eq!(
        requests.last(),
        Some(&Request::Detach { id: TerminalId(4) })
    );
}

#[test]
fn connect_or_spawn_reports_a_daemon_that_never_starts() {
    let directory = PathBuf::from(format!("/tmp/swc-data-{}", std::process::id()));
    let paths = swarm_protocol::Paths {
        socket: directory.join("daemon.sock"),
        database: directory.join("swarm.db"),
        log: directory.join("daemon.log"),
        data: directory.clone(),
    };

    let result = Client::connect_or_spawn(&paths, std::path::Path::new("/usr/bin/true"));

    assert!(matches!(
        result,
        Err(ClientError::DaemonDidNotStart { log }) if log == paths.log
    ));
    std::fs::remove_dir_all(&directory).ok();
}

fn info(id: u64) -> TerminalInfo {
    TerminalInfo {
        id: TerminalId(id),
        kind: TerminalKind::User,
        task: None,
        owner: None,
        name: None,
        fixed_size: None,
        title: None,
        cwd: None,
        exit: None,
    }
}

#[test]
fn a_late_reply_does_not_answer_the_next_call() {
    let socket = serve(|fake| {
        fake.handshake(VERSION);
        let Some(Request::List { call: first }) = fake.request() else {
            panic!("expected the first List");
        };
        let Some(Request::List { call: second }) = fake.request() else {
            panic!("expected the second List");
        };
        fake.send(&Event::Terminals {
            call: first,
            terminals: vec![info(1)],
        });
        fake.send(&Event::Terminals {
            call: second,
            terminals: vec![info(2)],
        });
        fake.drain();
    });
    let client = Client::connect(&socket)
        .expect("connect")
        .with_reply_timeout(Duration::from_millis(100));
    assert!(matches!(client.list(), Err(ClientError::Timeout)));

    let terminals = client.list().expect("second list");

    assert_eq!(terminals, vec![info(2)]);
}

#[test]
fn a_late_created_leads_to_a_kill() {
    let (timed_out, release) = flume::bounded::<()>(1);
    let (seen_sender, seen) = flume::unbounded();
    let socket = serve(move |fake| {
        fake.handshake(VERSION);
        let Some(Request::Create { call, .. }) = fake.request() else {
            panic!("expected a Create request");
        };
        release.recv().ok();
        fake.send(&Event::Created {
            call,
            id: TerminalId(9),
        });
        while let Some(request) = fake.request() {
            seen_sender.send(request).expect("test is listening");
        }
    });
    let client = Client::connect(&socket)
        .expect("connect")
        .with_reply_timeout(Duration::from_millis(100));

    let result = client.create(create_request());
    timed_out.send(()).expect("server is waiting");

    assert!(matches!(result, Err(ClientError::Timeout)));
    assert_eq!(
        seen.recv_timeout(WAIT),
        Ok(Request::Input {
            id: TerminalId(9),
            input: TerminalInput::Kill,
        })
    );
}

#[test]
fn connect_refuses_a_socket_folder_others_can_open() {
    let folder = PathBuf::from(format!("/tmp/swc-open-{}", std::process::id()));
    std::fs::DirBuilder::new()
        .mode(0o755)
        .create(&folder)
        .expect("create the folder");

    let result = Client::connect(&folder.join("d.sock"));

    assert!(matches!(
        result,
        Err(ClientError::UnsafeSocketDir { dir, .. }) if dir == folder
    ));
    std::fs::remove_dir_all(&folder).ok();
}

fn task_info() -> TaskInfo {
    TaskInfo {
        id: TaskId(4),
        project: ProjectId(2),
        name: "login".into(),
        branch: "swarm/login".into(),
        base: "main".into(),
        worktree: PathBuf::from("/tmp/wt"),
        ports: vec![3000],
        setup_terminal: Some(TerminalId(9)),
        setup_exit: None,
        agents: Vec::new(),
        created_by: None,
        briefed: false,
        approved: None,
        approval_stale: false,
        closed: false,
        pr_base: "main".into(),
        pull_request: None,
        close_after_merge: false,
        branch_source: swarm_protocol::BranchSource::Derived,
    }
}

fn project_info() -> ProjectInfo {
    ProjectInfo {
        id: ProjectId(2),
        name: "demo".into(),
        path: PathBuf::from("/tmp/demo"),
        base_branch: "main".into(),
        config_error: None,
        tasks: vec![task_info()],
        start: None,
    }
}

#[test]
fn list_projects_returns_what_the_daemon_sends() {
    let sent = vec![project_info()];
    let socket = serve(move |fake| {
        fake.handshake(VERSION);
        let Some(Request::ListProjects { call }) = fake.request() else {
            panic!("expected a ListProjects request");
        };
        fake.send(&Event::Projects {
            call,
            projects: sent,
        });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");

    let projects = client.list_projects().expect("list projects");

    assert_eq!(projects, vec![project_info()]);
}

#[test]
fn add_project_sends_the_path_and_returns_the_project() {
    let socket = serve(|fake| {
        fake.handshake(VERSION);
        let Some(Request::AddProject { call, path }) = fake.request() else {
            panic!("expected an AddProject request");
        };
        assert_eq!(path, PathBuf::from("/tmp/demo/src"));
        fake.send(&Event::ProjectAdded {
            call,
            project: project_info(),
        });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");

    let project = client
        .add_project(PathBuf::from("/tmp/demo/src"))
        .expect("add project");

    assert_eq!(project, project_info());
}

#[test]
fn add_project_reports_a_daemon_error() {
    let socket = serve(|fake| {
        fake.handshake(VERSION);
        let Some(Request::AddProject { call, .. }) = fake.request() else {
            panic!("expected an AddProject request");
        };
        fake.send(&Event::Error {
            call: Some(call),
            id: None,
            message: "not a git repository".into(),
        });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");

    let result = client.add_project(PathBuf::from("/tmp"));

    assert!(
        matches!(result, Err(ClientError::Daemon(message)) if message == "not a git repository")
    );
}

fn create_task_request() -> CreateTask {
    CreateTask {
        project: ProjectId(2),
        name: "login".into(),
        branch: TaskBranch::New {
            name: None,
            from: None,
        },
        run_setup: true,
        orchestrator: Some(AgentKind::Codex),
        first_message: Some("start with the tests".into()),
    }
}

#[test]
fn create_task_sends_the_request_and_returns_the_task() {
    let socket = serve(|fake| {
        fake.handshake(VERSION);
        let Some(Request::CreateTask { call, task }) = fake.request() else {
            panic!("expected a CreateTask request");
        };
        assert_eq!(task, create_task_request());
        fake.send(&Event::TaskCreated {
            call,
            task: task_info(),
        });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");

    let task = client
        .create_task(create_task_request())
        .expect("create task");

    assert_eq!(task, task_info());
}

#[test]
fn create_task_reports_a_daemon_error() {
    let socket = serve(|fake| {
        fake.handshake(VERSION);
        let Some(Request::CreateTask { call, .. }) = fake.request() else {
            panic!("expected a CreateTask request");
        };
        fake.send(&Event::Error {
            call: Some(call),
            id: None,
            message: "branch exists".into(),
        });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");

    let result = client.create_task(create_task_request());

    assert!(matches!(result, Err(ClientError::Daemon(message)) if message == "branch exists"));
}

#[test]
fn task_branches_sends_the_request_and_returns_the_list() {
    let list = TaskBranchList {
        branches: Vec::new(),
        project_folder: PathBuf::from("/tmp/project"),
        project_folder_changes: 2,
        base_branch: "main".into(),
    };
    let reply = list.clone();
    let socket = serve(move |fake| {
        fake.handshake(VERSION);
        let Some(Request::TaskBranches { call, project }) = fake.request() else {
            panic!("expected a TaskBranches request");
        };
        assert_eq!(project, ProjectId(2));
        fake.send(&Event::TaskBranches {
            call,
            branches: reply,
        });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");

    let branches = client.task_branches(ProjectId(2)).expect("task branches");

    assert_eq!(branches, list);
}

fn agent_info(status: AgentStatus) -> AgentInfo {
    AgentInfo {
        id: AgentId(1),
        project: ProjectId(2),
        task: Some(TaskId(4)),
        role: AgentRole::Orchestrator,
        kind: AgentKind::ClaudeCode,
        model: "opus".into(),
        name: "orchestrator".into(),
        worktree: PathBuf::from("/tmp/wt"),
        branch: "swarm/login".into(),
        terminal: Some(TerminalId(7)),
        status,
        message: None,
        lifecycle: AgentLifecycle::Running,
        revision: 1,
        created_at: 1_000,
        started_at: Some(2_000),
        ended_at: None,
    }
}

#[test]
fn agent_changes_reach_the_agent_events() {
    let socket = serve(|fake| {
        fake.handshake(VERSION);
        fake.send(&Event::AgentChanged {
            agent: agent_info(AgentStatus::Working),
        });
        fake.send(&Event::AgentChanged {
            agent: agent_info(AgentStatus::Done),
        });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");
    let events = client.agent_events();

    let first = events.recv_timeout(WAIT).expect("first event");
    let second = events.recv_timeout(WAIT).expect("second event");

    assert_eq!(
        (first.status, second.status),
        (AgentStatus::Working, AgentStatus::Done)
    );
}

#[test]
fn start_orchestrator_sends_the_request_and_returns_the_agent() {
    let socket = serve(|fake| {
        fake.handshake(VERSION);
        let Some(Request::StartOrchestrator {
            call,
            project,
            kind,
            plan,
        }) = fake.request()
        else {
            panic!("expected a StartOrchestrator request");
        };
        assert_eq!(
            (project, kind, plan),
            (ProjectId(2), Some(AgentKind::Codex), None)
        );
        fake.send(&Event::AgentStarted {
            call,
            agent: agent_info(AgentStatus::Unknown),
        });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");

    let agent = client
        .start_orchestrator(ProjectId(2), Some(AgentKind::Codex), None)
        .expect("start orchestrator");

    assert_eq!(agent, agent_info(AgentStatus::Unknown));
}

#[test]
fn start_orchestrator_sends_the_plan() {
    let socket = serve(|fake| {
        fake.handshake(VERSION);
        let Some(Request::StartOrchestrator { call, plan, .. }) = fake.request() else {
            panic!("expected a StartOrchestrator request");
        };
        assert_eq!(plan.as_deref(), Some("## What\n\nlogin"));
        fake.send(&Event::AgentStarted {
            call,
            agent: agent_info(AgentStatus::Unknown),
        });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");

    let agent = client
        .start_orchestrator(ProjectId(2), None, Some("## What\n\nlogin".into()))
        .expect("start orchestrator");

    assert_eq!(agent, agent_info(AgentStatus::Unknown));
}

#[test]
fn start_orchestrator_reports_a_daemon_error() {
    let socket = serve(|fake| {
        fake.handshake(VERSION);
        let Some(Request::StartOrchestrator { call, .. }) = fake.request() else {
            panic!("expected a StartOrchestrator request");
        };
        fake.send(&Event::Error {
            call: Some(call),
            id: None,
            message: "no such project".into(),
        });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");

    let result = client.start_orchestrator(ProjectId(9), None, None);

    assert!(matches!(result, Err(ClientError::Daemon(message)) if message == "no such project"));
}

#[test]
fn plan_draft_returns_the_draft_and_template() {
    let draft = PlanDraft {
        text: Some("## What\n\nlogin".into()),
        template: "## What\n".into(),
    };
    let reply = draft.clone();
    let socket = serve(move |fake| {
        fake.handshake(VERSION);
        let Some(Request::PlanDraft { call, project }) = fake.request() else {
            panic!("expected a PlanDraft request");
        };
        assert_eq!(project, ProjectId(2));
        fake.send(&Event::PlanDraft { call, draft: reply });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");

    let found = client.plan_draft(ProjectId(2)).expect("plan draft");

    assert_eq!(found, draft);
}

#[test]
fn plan_draft_reports_a_daemon_error() {
    let socket = serve(|fake| {
        fake.handshake(VERSION);
        let Some(Request::PlanDraft { call, .. }) = fake.request() else {
            panic!("expected a PlanDraft request");
        };
        fake.send(&Event::Error {
            call: Some(call),
            id: None,
            message: "no such project".into(),
        });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");

    let result = client.plan_draft(ProjectId(9));

    assert!(matches!(result, Err(ClientError::Daemon(message)) if message == "no such project"));
}

#[test]
fn save_plan_draft_sends_the_text_and_waits_for_done() {
    let socket = serve(|fake| {
        fake.handshake(VERSION);
        let Some(Request::SavePlanDraft {
            call,
            project,
            text,
        }) = fake.request()
        else {
            panic!("expected a SavePlanDraft request");
        };
        assert_eq!((project, text), (ProjectId(2), Some("draft".to_owned())));
        fake.send(&Event::Done { call });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");

    let result = client.save_plan_draft(ProjectId(2), Some("draft".into()));

    assert!(result.is_ok());
}

#[test]
fn save_plan_draft_without_text_clears_the_draft() {
    let socket = serve(|fake| {
        fake.handshake(VERSION);
        let Some(Request::SavePlanDraft { call, text, .. }) = fake.request() else {
            panic!("expected a SavePlanDraft request");
        };
        assert_eq!(text, None);
        fake.send(&Event::Done { call });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");

    let result = client.save_plan_draft(ProjectId(2), None);

    assert!(result.is_ok());
}

#[test]
fn close_steps_and_a_failed_close_reach_the_pull_request_listener() {
    let socket = serve(|fake| {
        fake.handshake(VERSION);
        fake.send(&Event::CloseProgress {
            task: TaskId(4),
            step: CloseStep::RemovingWorktrees,
        });
        fake.send(&Event::CloseFailed {
            task: TaskId(4),
            reason: CloseFailure::Other("git: busy".to_owned()),
        });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");

    let step = client.pr_events().recv_timeout(WAIT).expect("step");
    let failed = client.pr_events().recv_timeout(WAIT).expect("failed");

    assert_eq!(
        step,
        PrEvent::CloseStep {
            task: TaskId(4),
            step: CloseStep::RemovingWorktrees
        }
    );
    assert_eq!(
        failed,
        PrEvent::CloseFailed {
            task: TaskId(4),
            reason: CloseFailure::Other("git: busy".to_owned())
        }
    );
}

#[test]
fn task_terminal_and_message_broadcasts_reach_their_listeners() {
    let brief = brief_message();
    let socket = serve(move |fake| {
        fake.handshake(VERSION);
        fake.send(&Event::TaskChanged { task: task_info() });
        fake.send(&Event::TerminalChanged {
            terminal: TerminalInfo {
                kind: TerminalKind::Process,
                fixed_size: Some(GridSize { cols: 80, rows: 12 }),
                ..info(5)
            },
        });
        fake.send(&Event::MessageSent { message: brief });
        fake.send(&Event::MessagesDelivered {
            ids: vec![MessageId(1)],
            delivered_at: 5,
        });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");

    let task = client.task_events().recv_timeout(WAIT).expect("task");
    let terminal = client
        .terminal_events()
        .recv_timeout(WAIT)
        .expect("terminal");
    let sent = client.message_events().recv_timeout(WAIT).expect("sent");
    let delivered = client
        .message_events()
        .recv_timeout(WAIT)
        .expect("delivered");

    assert_eq!(task, task_info());
    assert_eq!(terminal.fixed_size, Some(GridSize { cols: 80, rows: 12 }));
    assert_eq!(sent, MessageEvent::Sent(brief_message()));
    assert_eq!(
        delivered,
        MessageEvent::Delivered {
            ids: vec![MessageId(1)],
            delivered_at: 5,
        }
    );
}

fn brief_message() -> MessageInfo {
    MessageInfo {
        id: MessageId(1),
        project: ProjectId(2),
        task: Some(TaskId(4)),
        from: None,
        to: AgentId(1),
        kind: MessageKind::Brief,
        text: "go".into(),
        sent_at: 0,
        delivered_at: None,
    }
}

#[test]
fn list_messages_sends_the_task_and_returns_the_messages() {
    let socket = serve(|fake| {
        fake.handshake(VERSION);
        let Some(Request::ListMessages {
            call,
            project,
            task,
        }) = fake.request()
        else {
            panic!("expected a ListMessages request");
        };
        assert_eq!((project, task), (ProjectId(2), Some(TaskId(4))));
        fake.send(&Event::Messages {
            call,
            messages: vec![brief_message()],
        });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");

    let messages = client
        .list_messages(ProjectId(2), Some(TaskId(4)))
        .expect("list messages");

    assert_eq!(messages, vec![brief_message()]);
}

#[test]
fn agent_changes_returns_each_diff_stat() {
    let stat = DiffStat {
        files: 2,
        additions: 41,
        deletions: 8,
    };
    let socket = serve(move |fake| {
        fake.handshake(VERSION);
        let Some(Request::AgentChanges { call, agents }) = fake.request() else {
            panic!("expected an AgentChanges request");
        };
        assert_eq!(agents, vec![AgentId(1), AgentId(3)]);
        fake.send(&Event::AgentChanges {
            call,
            changes: vec![change(stat)],
        });
        fake.drain();
    });
    let client = Client::connect(&socket).expect("connect");

    let changes = client
        .agent_changes(vec![AgentId(1), AgentId(3)])
        .expect("agent changes");

    assert_eq!(changes, vec![change(stat)]);
}

fn change(stat: DiffStat) -> AgentChange {
    AgentChange {
        agent: AgentId(1),
        stat,
        files: vec![FileStat {
            path: PathBuf::from("src/pty.rs"),
            additions: 39,
            deletions: 8,
        }],
    }
}

#[test]
fn a_resize_input_reaches_the_daemon() {
    let (seen_sender, seen) = flume::unbounded();
    let socket = serve(move |fake| {
        fake.handshake(VERSION);
        while let Some(request) = fake.request() {
            seen_sender.send(request).expect("test is listening");
        }
    });
    let client = Client::connect(&socket).expect("connect");
    let channels = client.attach(TerminalId(4), 10).expect("attach");
    let size = Size {
        cols: 80,
        rows: 12,
        cell_width: 7,
        cell_height: 17,
    };

    channels
        .input
        .send(TerminalInput::Resize(size))
        .expect("send resize");
    channels
        .input
        .send(TerminalInput::Focus(true))
        .expect("send focus");

    let mut requests = Vec::new();
    while requests.last()
        != Some(&Request::Input {
            id: TerminalId(4),
            input: TerminalInput::Focus(true),
        })
    {
        requests.push(seen.recv_timeout(WAIT).expect("a request"));
    }

    assert_eq!(
        requests[1..],
        [
            Request::Input {
                id: TerminalId(4),
                input: TerminalInput::Resize(size)
            },
            Request::Input {
                id: TerminalId(4),
                input: TerminalInput::Focus(true)
            },
        ]
    );
}
