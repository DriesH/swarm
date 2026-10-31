//! Starts the real `swarm-daemon` binary and speaks the protocol to it.

#![expect(clippy::expect_used, reason = "test helpers panic on failure")]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Once;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use swarm_core::store::Store;
use swarm_protocol::{
    AgentChange, AgentId, AgentInfo, AgentKind, AgentLifecycle, AgentRole, AgentStatus, CallId,
    CloseCheck, CommentId, CommentState, CreateTask, CreateTerminal, DiffStat, Event, FileStat,
    MessageInfo, MessageKind, ProjectId, ProjectInfo, Request, ReviewSource, TaskDiff, TaskId,
    TaskInfo, TerminalId, TerminalInfo, TerminalKind, VERSION, read, write,
};
use swarm_term::{
    BlockedKind, ExitStatus, Palette, ProgramState, Rgb, Row, Size, TerminalInput, TerminalOutput,
};

static NEXT_HOME: AtomicU32 = AtomicU32::new(0);

const CALL: CallId = CallId(1);

const READ_TIMEOUT: Duration = Duration::from_secs(15);

/// The daemon process; SIGTERM on drop so the shutdown path runs, SIGKILL if it hangs.
struct Daemon {
    home: PathBuf,
    child: Child,
}

impl Daemon {
    fn new_home() -> PathBuf {
        let number = NEXT_HOME.fetch_add(1, Ordering::Relaxed);

        // macOS temp dirs are too long for a socket path.
        PathBuf::from(format!("/tmp/swd-{}-{number}", std::process::id()))
    }

    /// Starts the process without waiting for the socket.
    fn spawn_in(home: PathBuf) -> Self {
        Self::spawn_with(home, None)
    }

    fn spawn_with(home: PathBuf, settings: Option<&Path>) -> Self {
        std::fs::create_dir_all(&home).expect("home");
        let log = std::fs::File::options()
            .create(true)
            .append(true)
            .open(home.join("daemon.log"))
            .expect("log");

        let mut command = Command::new(env!("CARGO_BIN_EXE_swarm-daemon"));
        command.arg("--data-dir").arg(&home);
        if let Some(settings) = settings {
            command.arg("--settings").arg(settings);
        }

        let child = command
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .expect("start the daemon");

        Self { home, child }
    }

    fn wait_until_listening(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(self.socket()).is_err() {
            assert!(Instant::now() < deadline, "the daemon did not listen");
            thread::park_timeout(Duration::from_millis(20));
        }
    }

    fn start_in(home: PathBuf) -> Self {
        let daemon = Self::spawn_in(home);
        daemon.wait_until_listening();

        daemon
    }

    fn start() -> Self {
        Self::start_in(Self::new_home())
    }

    fn start_with_settings(settings: &Path) -> Self {
        swarm_binary();
        let daemon = Self::spawn_with(Self::new_home(), Some(settings));
        daemon.wait_until_listening();

        daemon
    }

    fn agent_socket(&self) -> PathBuf {
        self.home.join("agent.sock")
    }

    fn socket(&self) -> PathBuf {
        self.home.join("daemon.sock")
    }

    fn connect(&self) -> Client {
        let stream = UnixStream::connect(self.socket()).expect("connect");
        stream
            .set_read_timeout(Some(READ_TIMEOUT))
            .expect("timeout");

        Client {
            stream,
            buffer: Vec::new(),
        }
    }

    /// Kills the daemon without letting it clean up.
    fn crash(&mut self) {
        self.child.kill().expect("kill");
        self.child.wait().expect("wait");
    }
}

impl Daemon {
    /// Sends SIGTERM and returns whether the daemon exited within 5 s.
    fn stops_on_sigterm(&mut self) -> bool {
        let pid = i32::try_from(self.child.id()).expect("pid");
        // SAFETY: kill takes plain integers.
        unsafe { libc::kill(pid, libc::SIGTERM) };

        let deadline = Instant::now() + Duration::from_secs(5);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            thread::park_timeout(Duration::from_millis(20));
        }

        !matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let pid = i32::try_from(self.child.id()).unwrap_or(0);
            // SAFETY: kill takes plain integers.
            unsafe { libc::kill(pid, libc::SIGTERM) };

            let deadline = Instant::now() + Duration::from_secs(8);
            while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
                thread::park_timeout(Duration::from_millis(20));
            }
            if matches!(self.child.try_wait(), Ok(None)) {
                self.child.kill().ok();
                self.child.wait().ok();
            }
        }

        std::fs::remove_dir_all(&self.home).ok();
    }
}

struct Client {
    stream: UnixStream,
    buffer: Vec<u8>,
}

impl Client {
    fn send(&mut self, request: &Request) {
        write(&mut self.stream, request).expect("send");
    }

    fn next(&mut self) -> Event {
        read(&mut self.stream, &mut self.buffer).expect("event")
    }

    fn hello(&mut self) {
        self.send(&Request::Hello { version: VERSION });
        assert!(matches!(
            self.next(),
            Event::Hello {
                version: VERSION,
                ..
            }
        ));
    }

    /// Reads events until `pick` returns a value.
    fn wait_for<T>(&mut self, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
        loop {
            let event = self.next();
            if let Some(value) = pick(&event) {
                return value;
            }
        }
    }

    fn create(&mut self, command: &[&str]) -> TerminalId {
        self.send(&Request::Create {
            call: CALL,
            terminal: CreateTerminal {
                kind: TerminalKind::User,
                command: Some(command.iter().map(|part| (*part).to_owned()).collect()),
                cwd: None,
                size: Size {
                    cols: 80,
                    rows: 24,
                    cell_width: 8,
                    cell_height: 16,
                },
                palette: Palette {
                    fg: Rgb(230, 230, 230),
                    bg: Rgb(10, 10, 12),
                    cursor: Rgb(255, 255, 255),
                    ansi: [Rgb(120, 120, 120); 16],
                },
                task: None,
            },
        });

        self.wait_for(|event| match event {
            Event::Created { call: CALL, id } => Some(*id),
            _ => None,
        })
    }

    /// Reads until a screen row equals `text`.
    fn wait_for_row(&mut self, text: &str) {
        self.wait_for_matching_row(|row| row == text);
    }

    /// Reads until a screen row satisfies `matches`.
    fn wait_for_matching_row(&mut self, matches: impl Fn(&str) -> bool) {
        self.wait_for(|event| {
            let Event::Output { output, .. } = event else {
                return None;
            };

            let found = match output {
                TerminalOutput::Snapshot(snapshot) => {
                    snapshot.rows.iter().any(|row| matches(&row_text(row)))
                }
                TerminalOutput::Update(update) => {
                    update.rows.iter().any(|(_, row)| matches(&row_text(row)))
                }
                _ => false,
            };

            found.then_some(())
        });
    }

    /// The text of the next snapshot.
    fn snapshot_rows(&mut self) -> Vec<String> {
        self.wait_for(|event| match event {
            Event::Output {
                output: TerminalOutput::Snapshot(snapshot),
                ..
            } => Some(snapshot.rows.iter().map(row_text).collect()),
            _ => None,
        })
    }
}

fn row_text(row: &Row) -> String {
    let mut line = String::new();
    for span in &row.texts {
        while line.chars().count() < usize::from(span.col) {
            line.push(' ');
        }
        line.push_str(&span.text);
    }

    line.trim_end().to_owned()
}

/// A shell that printed `hello`. The command is split so the typed line differs
/// from its output.
fn shell_that_said_hello(daemon: &Daemon) -> (Client, TerminalId) {
    let mut client = daemon.connect();
    client.hello();
    let id = client.create(&["/bin/sh"]);

    client.send(&Request::Input {
        id,
        input: TerminalInput::Text("echo hel\"\"lo\r".to_owned()),
    });
    client.wait_for_row("hello");

    (client, id)
}

fn pgrep(pattern: &str) -> bool {
    Command::new("pgrep")
        .args(["-f", pattern])
        .stdout(Stdio::null())
        .status()
        .expect("pgrep")
        .success()
}

#[test]
fn a_created_shell_shows_its_output() {
    let daemon = Daemon::start();

    let (_client, _id) = shell_that_said_hello(&daemon);
}

#[test]
fn a_second_client_attaches_to_the_current_screen() {
    let daemon = Daemon::start();
    let (_first, id) = shell_that_said_hello(&daemon);

    let mut second = daemon.connect();
    second.hello();
    second.send(&Request::Attach { id, history: 100 });
    let rows = second.snapshot_rows();

    assert!(rows.iter().any(|row| row == "hello"), "{rows:?}");
}

#[test]
fn a_terminal_survives_its_client() {
    let daemon = Daemon::start();
    let (first, id) = shell_that_said_hello(&daemon);

    drop(first);

    let mut second = daemon.connect();
    second.hello();
    second.send(&Request::List { call: CALL });
    let terminals = second.wait_for(|event| match event {
        Event::Terminals { terminals, .. } => Some(terminals.clone()),
        _ => None,
    });
    assert_eq!(terminals.len(), 1);
    assert_eq!(terminals[0].id, id);
    assert_eq!(terminals[0].exit, None);
}

#[test]
fn kill_ends_the_terminal_and_closes_it() {
    let daemon = Daemon::start();
    let (mut client, id) = shell_that_said_hello(&daemon);

    client.send(&Request::Input {
        id,
        input: TerminalInput::Kill,
    });
    let status = client.wait_for(|event| match event {
        Event::Output {
            output: TerminalOutput::Exited(status),
            ..
        } => Some(status.clone()),
        _ => None,
    });
    let after = client.next();

    assert!(matches!(
        status,
        ExitStatus::Signal(_) | ExitStatus::Code(_)
    ));
    assert_eq!(after, Event::Closed { id });
}

#[test]
fn a_wrong_version_gets_the_daemons_version() {
    let daemon = Daemon::start();
    let mut client = daemon.connect();

    client.send(&Request::Hello {
        version: VERSION + 1,
    });

    assert!(matches!(client.next(), Event::Hello { version, .. } if version == VERSION));
}

#[test]
fn a_request_before_hello_is_refused() {
    let daemon = Daemon::start();
    let mut client = daemon.connect();

    client.send(&Request::List { call: CALL });

    assert!(matches!(
        client.next(),
        Event::Error {
            call: None,
            id: None,
            ..
        }
    ));
}

#[test]
fn attaching_an_unknown_terminal_is_an_error() {
    let daemon = Daemon::start();
    let mut client = daemon.connect();
    client.hello();

    client.send(&Request::Attach {
        id: TerminalId(999),
        history: 10,
    });

    assert!(matches!(
        client.next(),
        Event::Error {
            call: None,
            id: Some(TerminalId(999)),
            ..
        }
    ));
}

#[test]
fn a_missing_command_is_reported_on_its_call() {
    let daemon = Daemon::start();
    let mut client = daemon.connect();
    client.hello();

    client.send(&Request::Create {
        call: CALL,
        terminal: CreateTerminal {
            kind: TerminalKind::Process,
            command: Some(vec!["/nonexistent/program".to_owned()]),
            cwd: None,
            size: Size {
                cols: 80,
                rows: 24,
                cell_width: 8,
                cell_height: 16,
            },
            palette: Palette {
                fg: Rgb(0, 0, 0),
                bg: Rgb(0, 0, 0),
                cursor: Rgb(0, 0, 0),
                ansi: [Rgb(0, 0, 0); 16],
            },
            task: None,
        },
    });

    assert!(matches!(
        client.next(),
        Event::Error {
            call: Some(CALL),
            id: None,
            ..
        }
    ));
}

/// Kills leftovers of the recovery test if it fails before recovery ran.
struct SleepReaper;

impl Drop for SleepReaper {
    fn drop(&mut self) {
        Command::new("pkill")
            .args(["-9", "-f", "trap '' HUP TERM; sleep 300"])
            .status()
            .ok();
    }
}

#[test]
fn recovery_stops_processes_that_survived_a_crash() {
    let _reaper = SleepReaper;
    let mut daemon = Daemon::start();
    let mut client = daemon.connect();
    client.hello();
    client.create(&["/bin/sh", "-c", "trap '' HUP TERM; sleep 300"]);
    assert!(pgrep("sleep 300"));

    daemon.crash();
    let home = daemon.home.clone();
    let restarted = Daemon::start_in(home);

    let mut fresh = restarted.connect();
    fresh.hello();
    fresh.send(&Request::List { call: CALL });
    let terminals = fresh.wait_for(|event| match event {
        Event::Terminals { terminals, .. } => Some(terminals.clone()),
        _ => None,
    });
    assert!(terminals.is_empty());

    let deadline = Instant::now() + Duration::from_secs(5);
    while pgrep("trap '' HUP TERM; sleep 300") && Instant::now() < deadline {
        thread::park_timeout(Duration::from_millis(100));
    }
    assert!(!pgrep("trap '' HUP TERM; sleep 300"));
    drop(restarted);
}

#[test]
fn two_daemons_on_one_folder_leave_one_server() {
    let home = Daemon::new_home();
    let mut first = Daemon::spawn_in(home.clone());
    let mut second = Daemon::spawn_in(home);

    let deadline = Instant::now() + Duration::from_secs(10);
    let winner = loop {
        if let Ok(Some(status)) = first.child.try_wait() {
            assert!(status.success(), "{status}");
            break &second;
        }
        if let Ok(Some(status)) = second.child.try_wait() {
            assert!(status.success(), "{status}");
            break &first;
        }

        assert!(Instant::now() < deadline, "neither daemon exited");
        thread::park_timeout(Duration::from_millis(20));
    };

    winner.wait_until_listening();
    let mut client = winner.connect();
    client.hello();
}

/// Runs inside a daemon terminal (see the test below); does nothing otherwise.
#[test]
fn terminal_client_helper() {
    let Some(socket) = std::env::var_os("SWD_HELPER_SOCKET") else {
        return;
    };

    let mut stream = UnixStream::connect(socket).expect("connect");
    write(&mut stream, &Request::Hello { version: VERSION }).ok();

    let reply: Result<Event, _> = read(&mut stream, &mut Vec::new());
    match reply {
        Ok(Event::Hello { .. }) => println!("peer accepted"),
        _ => println!("peer refused"),
    }
}

#[test]
fn a_client_inside_a_terminal_is_refused() {
    let daemon = Daemon::start();
    let mut client = daemon.connect();
    client.hello();

    let helper = std::env::current_exe().expect("test binary");
    let script = format!(
        "SWD_HELPER_SOCKET='{}' '{}' --exact terminal_client_helper --nocapture",
        daemon.socket().display(),
        helper.display()
    );
    client.create(&["/bin/sh", "-c", &script]);

    client.wait_for_matching_row(|row| row.ends_with("peer refused"));
}

#[test]
fn a_daemon_started_by_the_client_stops_on_sigterm() {
    let home = Daemon::new_home();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .expect("home");

    let paths = swarm_protocol::Paths::in_dir(home.clone());
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_swarm-daemon"));
    let client = swarm_protocol::Client::connect_or_spawn(&paths, &binary).expect("connect");
    let pid = i32::try_from(client.daemon_pid()).expect("pid");
    drop(client);

    // SAFETY: kill takes plain integers.
    unsafe { libc::kill(pid, libc::SIGTERM) };

    let deadline = Instant::now() + Duration::from_secs(5);
    // SAFETY: signal 0 only checks that the process exists.
    while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
        thread::park_timeout(Duration::from_millis(20));
    }

    // SAFETY: as above.
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    if alive {
        // SAFETY: as above.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    if let Err(error) = std::fs::remove_dir_all(&home) {
        eprintln!("cannot remove {}: {error}", home.display());
    }

    assert!(!alive, "the daemon ignored SIGTERM");
}

/// A git repository with one commit, removed on drop.
struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(config: &str) -> Self {
        let number = NEXT_HOME.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("swd-repo-{}-{number}", std::process::id()));
        std::fs::create_dir_all(root.join(".swarm")).expect("folders");
        let root = root.canonicalize().expect("real path");
        std::fs::write(root.join(".swarm/config.toml"), config).expect("config");
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

    /// Commits `count` small files, so a checkout takes measurable time.
    fn commit_many_files(&self, count: u32) {
        std::fs::create_dir_all(self.root.join("bulk")).expect("folder");
        for number in 0..count {
            std::fs::write(self.root.join(format!("bulk/file-{number}.txt")), "x").expect("file");
        }

        self.git(&["add", "bulk"]);
        self.git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "-m",
            "many files",
        ]);
    }

    fn git(&self, arguments: &[&str]) {
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
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

impl Client {
    fn add_project(&mut self, path: &Path) -> ProjectInfo {
        self.send(&Request::AddProject {
            call: CALL,
            path: path.to_owned(),
        });

        self.wait_for(|event| match event {
            Event::ProjectAdded {
                call: CALL,
                project,
            } => Some(project.clone()),
            _ => None,
        })
    }

    fn terminals(&mut self) -> Vec<TerminalInfo> {
        self.send(&Request::List { call: CALL });

        self.wait_for(|event| match event {
            Event::Terminals {
                call: CALL,
                terminals,
            } => Some(terminals.clone()),
            _ => None,
        })
    }

    fn projects(&mut self) -> Vec<ProjectInfo> {
        self.send(&Request::ListProjects { call: CALL });

        self.wait_for(|event| match event {
            Event::Projects {
                call: CALL,
                projects,
            } => Some(projects.clone()),
            _ => None,
        })
    }

    fn create_task(&mut self, task: CreateTask) -> TaskInfo {
        self.send(&Request::CreateTask { call: CALL, task });

        self.wait_for(|event| match event {
            Event::TaskCreated { call: CALL, task } => Some(task.clone()),
            _ => None,
        })
    }
}

#[test]
fn a_task_runs_its_setup_script_in_a_process_terminal() {
    let repo = Repo::new(
        "[worktrees]\nsetup = \"echo ready > setup-ran; echo $SWARM_TASK > task-id; exit 3\"\n",
    );
    let daemon = Daemon::start();
    let mut client = daemon.connect();
    client.hello();

    let project = client.add_project(&repo.root);
    let task = client.create_task(CreateTask {
        project: project.id,
        name: "First task".to_owned(),
        from: None,
        run_setup: true,
        orchestrator: None,
        first_message: None,
    });

    let setup = task.setup_terminal.expect("setup terminal");
    assert_eq!(task.worktree, repo.root.join(".worktrees/first-task"));

    let deadline = Instant::now() + Duration::from_secs(15);
    while client
        .terminals()
        .iter()
        .any(|terminal| terminal.id == setup)
    {
        assert!(Instant::now() < deadline, "the setup did not finish");
        thread::park_timeout(Duration::from_millis(50));
    }

    let ran = std::fs::read_to_string(task.worktree.join("setup-ran")).expect("setup-ran");
    let task_id = std::fs::read_to_string(task.worktree.join("task-id")).expect("task-id");
    assert_eq!(ran.trim(), "ready");
    assert_eq!(task_id.trim(), task.id.0.to_string());

    let deadline = Instant::now() + Duration::from_secs(15);
    let finished = loop {
        let listed = client.projects();
        if listed[0].tasks[0].setup_terminal.is_none() {
            break listed[0].tasks[0].clone();
        }
        assert!(Instant::now() < deadline, "the setup was never recorded");
        thread::park_timeout(Duration::from_millis(50));
    };
    assert_eq!(finished.setup_exit, Some(ExitStatus::Code(3)));
}

#[test]
fn a_slow_task_does_not_block_other_requests() {
    let repo = Repo::new("");
    repo.commit_many_files(2000);
    let daemon = Daemon::start();
    let (mut client, id) = shell_that_said_hello(&daemon);
    let project = client.add_project(&repo.root);

    client.send(&Request::CreateTask {
        call: CallId(2),
        task: CreateTask {
            project: project.id,
            name: "Big".to_owned(),
            from: None,
            run_setup: false,
            orchestrator: None,
            first_message: None,
        },
    });
    client.send(&Request::List { call: CallId(3) });
    client.send(&Request::Input {
        id,
        input: TerminalInput::Text("echo wor\"\"ld\r".to_owned()),
    });

    let mut answered_list = false;
    let mut echoed = false;
    let created_after_the_rest = client.wait_for(|event| match event {
        Event::Terminals {
            call: CallId(3), ..
        } => {
            answered_list = true;
            None
        }
        Event::Output {
            output: TerminalOutput::Update(update),
            ..
        } => {
            echoed |= update.rows.iter().any(|(_, row)| row_text(row) == "world");
            None
        }
        Event::TaskCreated {
            call: CallId(2), ..
        } => Some(answered_list && echoed),
        _ => None,
    });

    assert!(
        created_after_the_rest,
        "list and keystrokes waited for the task"
    );
}

#[test]
fn a_task_for_an_unknown_project_is_reported_on_its_call() {
    let daemon = Daemon::start();
    let mut client = daemon.connect();
    client.hello();

    client.send(&Request::CreateTask {
        call: CALL,
        task: CreateTask {
            project: ProjectId(42),
            name: "x".to_owned(),
            from: None,
            run_setup: false,
            orchestrator: None,
            first_message: None,
        },
    });

    let message = client.wait_for(|event| match event {
        Event::Error {
            call: Some(CALL),
            message,
            ..
        } => Some(message.clone()),
        _ => None,
    });
    assert!(message.contains("42"));
}

#[test]
fn projects_survive_a_daemon_restart() {
    let repo = Repo::new("");
    let mut daemon = Daemon::start();
    let mut client = daemon.connect();
    client.hello();
    let project = client.add_project(&repo.root);
    drop(client);

    daemon.crash();
    let restarted = Daemon::start_in(daemon.home.clone());
    let mut client = restarted.connect();
    client.hello();

    let projects = client.projects();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].id, project.id);
}

#[test]
fn the_daemon_stops_on_sigterm_after_a_terminal_ran() {
    let mut daemon = Daemon::start();
    let (mut client, id) = shell_that_said_hello(&daemon);

    client.send(&Request::Input {
        id,
        input: TerminalInput::Text("exit\r".to_owned()),
    });
    client.wait_for(|event| matches!(event, Event::Closed { .. }).then_some(()));

    assert!(daemon.stops_on_sigterm(), "the daemon ignored SIGTERM");
}

#[test]
fn the_daemon_stops_on_sigterm_while_a_terminal_runs() {
    let mut daemon = Daemon::start();
    let _shell = shell_that_said_hello(&daemon);

    assert!(daemon.stops_on_sigterm(), "the daemon ignored SIGTERM");
}

#[test]
fn a_daemon_started_from_a_thread_that_blocks_signals_stops_on_sigterm() {
    let home = Daemon::new_home();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .expect("home");

    let paths = swarm_protocol::Paths::in_dir(home.clone());
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_swarm-daemon"));

    // GPUI runs background work on GCD threads, which block every signal.
    let pid = thread::spawn(move || {
        // SAFETY: the set is initialised by sigfillset before use.
        unsafe {
            let mut all = std::mem::zeroed::<libc::sigset_t>();
            libc::sigfillset(&raw mut all);
            libc::pthread_sigmask(libc::SIG_BLOCK, &raw const all, std::ptr::null_mut());
        }

        let client = swarm_protocol::Client::connect_or_spawn(&paths, &binary).expect("connect");
        i32::try_from(client.daemon_pid()).expect("pid")
    })
    .join()
    .expect("thread");

    // SAFETY: kill takes plain integers.
    unsafe { libc::kill(pid, libc::SIGTERM) };

    let deadline = Instant::now() + Duration::from_secs(5);
    // SAFETY: signal 0 only checks that the process exists.
    while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
        thread::park_timeout(Duration::from_millis(20));
    }

    // SAFETY: as above.
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    if alive {
        // SAFETY: as above.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    if let Err(error) = std::fs::remove_dir_all(&home) {
        eprintln!("cannot remove {}: {error}", home.display());
    }

    assert!(!alive, "the daemon ignored SIGTERM");
}

/// The `swarm` binary next to the daemon. `cargo test -p swarm_daemon` does not build
/// it, so build it once when it is missing.
fn swarm_binary() -> PathBuf {
    static BUILT: Once = Once::new();

    let path = Path::new(env!("CARGO_BIN_EXE_swarm-daemon")).with_file_name("swarm");
    BUILT.call_once(|| {
        if path.is_file() {
            return;
        }

        let status = Command::new(env!("CARGO"))
            .args(["build", "-p", "swarm_cli"])
            .status()
            .expect("run cargo build");
        assert!(status.success(), "cannot build swarm_cli");
    });
    assert!(path.is_file(), "{} is missing", path.display());

    path
}

/// A fake agent script and the settings file that points at it.
struct Fixture {
    dir: PathBuf,
    out: PathBuf,
}

impl Fixture {
    /// `body` runs as `/bin/sh` with `$OUT` (a folder) and `$SWARM` (the hook binary).
    fn new(body: &str) -> Self {
        let number = NEXT_HOME.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from(format!("/tmp/swd-fx-{}-{number}", std::process::id()));
        let out = dir.join("out");
        std::fs::create_dir_all(&out).expect("fixture folders");

        let script = dir.join("fake-agent.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nOUT='{}'\nSWARM='{}'\n{body}\n",
                out.display(),
                swarm_binary().display()
            ),
        )
        .expect("script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("mode");

        std::fs::write(
            dir.join("settings.toml"),
            format!(
                "[agents.claude_code]\ncommand = \"{0}\"\norchestrator_model = \"opus\"\nworker_model = \"sonnet\"\n\
                 [agents.codex]\ncommand = \"{0}\"\norchestrator_model = \"gpt-reserve\"\nworker_model = \"gpt-6-luna\"\n",
                script.display()
            ),
        )
        .expect("settings");

        Self { dir, out }
    }

    fn settings(&self) -> PathBuf {
        self.dir.join("settings.toml")
    }

    /// Waits for a file the script wrote and returns its text.
    fn read_when_written(&self, name: &str) -> String {
        self.read_when(name, |text| !text.is_empty())
    }

    /// The fake agent writes in pieces, so wait for the text the test needs.
    fn read_when(&self, name: &str, done: impl Fn(&str) -> bool) -> String {
        let path = self.out.join(name);
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Ok(text) = std::fs::read_to_string(&path)
                && done(&text)
            {
                return text;
            }
            assert!(Instant::now() < deadline, "{name} was never written");
            thread::park_timeout(Duration::from_millis(30));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn orchestrator_task(project: ProjectId, run_setup: bool) -> CreateTask {
    CreateTask {
        project,
        name: "Agent task".to_owned(),
        from: None,
        run_setup,
        orchestrator: Some(AgentKind::ClaudeCode),
        first_message: None,
    }
}

impl Client {
    /// Reads events until an agent change satisfies `pick`.
    fn wait_for_agent(&mut self, pick: impl Fn(&AgentInfo) -> bool) -> AgentInfo {
        self.wait_for(|event| match event {
            Event::AgentChanged { agent } if pick(agent) => Some(agent.clone()),
            _ => None,
        })
    }
}

fn database(daemon: &Daemon) -> Store {
    Store::open(&daemon.home.join("swarm.db")).expect("open the database")
}

#[test]
fn an_orchestrator_reports_idle_working_done_and_keeps_its_hook_session() {
    let fixture = Fixture::new(
        r#"printf '\033]7501;state=idle\007'
IFS= read -r line
printf '%s\n' "$line" > "$OUT/first-message"
printf '\033]7501;state=working\007'
printf '{"hook_event_name":"SessionStart","session_id":"hook-session-1"}' | "$SWARM" hook
sleep 0.3
printf '\033]7501;state=done\007'
sleep 0.3
exit 0"#,
    );
    let repo = Repo::new("");
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let mut client = daemon.connect();
    client.hello();
    let mut watcher = daemon.connect();
    watcher.hello();
    let project = client.add_project(&repo.root);

    let mut request = orchestrator_task(project.id, false);
    request.first_message = Some("hello agent".to_owned());
    let task = client.create_task(request);

    assert_eq!(task.agents.len(), 1);
    assert_eq!(task.agents[0].status, AgentStatus::Unknown);
    let agent_id = task.agents[0].id;
    let mut statuses = Vec::new();
    let mut revisions = Vec::new();
    let last = client.wait_for(|event| match event {
        Event::AgentChanged { agent } if agent.id == agent_id => {
            statuses.push(agent.status);
            revisions.push(agent.revision);

            (agent.lifecycle == AgentLifecycle::Resumable).then(|| agent.clone())
        }
        _ => None,
    });

    let mut seen: Vec<AgentStatus> = Vec::new();
    for status in statuses {
        if seen.last() != Some(&status) {
            seen.push(status);
        }
    }
    let wanted = [AgentStatus::Idle, AgentStatus::Working, AgentStatus::Done];
    let mut position = 0;
    for status in &seen {
        if position < wanted.len() && *status == wanted[position] {
            position += 1;
        }
    }
    assert_eq!(position, wanted.len(), "statuses were {seen:?}");
    assert_eq!(last.status, AgentStatus::Done);
    assert_eq!(last.terminal, None);
    assert_eq!(last.model, "opus");
    assert!(
        revisions.windows(2).all(|pair| pair[0] < pair[1]),
        "revisions were {revisions:?}"
    );
    assert!(revisions.len() >= 4, "start, idle, working, done, exit");

    assert_eq!(
        fixture.read_when_written("first-message").trim(),
        "hello agent"
    );

    let stored = database(&daemon)
        .agent(agent_id)
        .expect("read")
        .expect("row");
    assert_eq!(stored.session_id.as_deref(), Some("hook-session-1"));
    assert!(stored.session_confirmed);
    assert_eq!(stored.info.revision, last.revision);

    let done = watcher.wait_for_agent(|agent| agent.status == AgentStatus::Done);
    assert_eq!(done.id, agent_id, "every client gets agent changes");

    let listed = client.projects();
    assert_eq!(
        listed[0].tasks[0].agents[0].lifecycle,
        AgentLifecycle::Resumable
    );
    assert_eq!(listed[0].tasks[0].agents[0].revision, last.revision);
}

#[test]
fn the_first_message_waits_for_the_agent_to_be_idle() {
    let fixture = Fixture::new(
        r#"sleep 1
printf '\033]7501;state=idle\007'
IFS= read -r line
printf '%s\n' "$line" > "$OUT/first-message"
sleep 30"#,
    );
    let repo = Repo::new("");
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let mut client = daemon.connect();
    client.hello();
    let project = client.add_project(&repo.root);

    let mut request = orchestrator_task(project.id, false);
    request.first_message = Some("hello agent".to_owned());
    client.create_task(request);
    let running = client.wait_for_agent(|agent| agent.terminal.is_some());
    client.send(&Request::Attach {
        id: running.terminal.expect("terminal"),
        history: 0,
    });

    let mut idle_seen = false;
    let mut typed_early = false;
    while !idle_seen {
        let event = client.next();
        let Event::Output { output, .. } = event else {
            continue;
        };
        match output {
            TerminalOutput::ProgramStatus(status) => {
                idle_seen = status.state == ProgramState::Idle;
            }
            TerminalOutput::Snapshot(snapshot) => {
                typed_early |= snapshot
                    .rows
                    .iter()
                    .any(|row| row_text(row).contains("hello agent"));
            }
            TerminalOutput::Update(update) => {
                typed_early |= update
                    .rows
                    .iter()
                    .any(|(_, row)| row_text(row).contains("hello agent"));
            }
            _ => {}
        }
    }

    assert!(
        !typed_early,
        "the message was typed before the agent was idle"
    );
    assert_eq!(
        fixture.read_when_written("first-message").trim(),
        "hello agent"
    );
}

#[test]
fn the_orchestrator_starts_only_after_the_setup_ended() {
    let fixture = Fixture::new(
        r#"if [ -f setup-done ]; then echo yes > "$OUT/saw-setup"; else echo no > "$OUT/saw-setup"; fi
printf '\033]7501;state=idle\007'
sleep 30"#,
    );
    let repo = Repo::new("[worktrees]\nsetup = \"sleep 1; echo ok > setup-done\"\n");
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let mut client = daemon.connect();
    client.hello();
    let project = client.add_project(&repo.root);

    let task = client.create_task(orchestrator_task(project.id, true));

    assert_eq!(task.agents[0].lifecycle, AgentLifecycle::Starting);
    assert_eq!(fixture.read_when_written("saw-setup").trim(), "yes");
    let running = client.wait_for_agent(|agent| agent.lifecycle == AgentLifecycle::Running);
    assert!(running.terminal.is_some());
}

#[test]
fn a_failed_setup_gives_an_error_and_starts_no_process() {
    let fixture = Fixture::new(r#"echo ran > "$OUT/ran"; sleep 30"#);
    let repo = Repo::new("[worktrees]\nsetup = \"exit 3\"\n");
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let mut client = daemon.connect();
    client.hello();
    let project = client.add_project(&repo.root);

    let task = client.create_task(orchestrator_task(project.id, true));

    let failed = client.wait_for_agent(|agent| agent.status == AgentStatus::Error);
    assert_eq!(failed.id, task.agents[0].id);
    assert_eq!(failed.message.as_deref(), Some("Setup failed"));
    assert_eq!(failed.lifecycle, AgentLifecycle::Stopped);
    assert_eq!(failed.terminal, None);

    thread::park_timeout(Duration::from_millis(500));
    assert!(
        !fixture.out.join("ran").exists(),
        "the agent started anyway"
    );
    let stored = database(&daemon)
        .agent(failed.id)
        .expect("read")
        .expect("row");
    assert_eq!(stored.info.status, AgentStatus::Error);
}

#[test]
fn the_trust_dialog_blocks_the_agent_with_a_message() {
    let fixture = Fixture::new(
        r"printf 'Quick safety check\r\n  1. Yes, I trust this folder\r\n  2. No, exit\r\n'
sleep 30",
    );
    let repo = Repo::new("");
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let mut client = daemon.connect();
    client.hello();
    let project = client.add_project(&repo.root);

    client.create_task(orchestrator_task(project.id, false));

    let blocked =
        client.wait_for_agent(|agent| agent.status == AgentStatus::Blocked(BlockedKind::Other));
    assert_eq!(blocked.message.as_deref(), Some("Trust this folder"));
}

/// Sends one line to the agent socket and returns the parsed answer.
fn agent_request(daemon: &Daemon, line: &[u8]) -> serde_json::Value {
    try_agent_request(daemon, line).expect("json answer")
}

/// As [`agent_request`], but `None` when the socket closed without an answer.
fn try_agent_request(daemon: &Daemon, line: &[u8]) -> Option<serde_json::Value> {
    let mut stream = UnixStream::connect(daemon.agent_socket()).expect("connect");
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .expect("timeout");
    stream.write_all(line).ok()?;
    stream.write_all(b"\n").ok()?;

    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).ok()?;

    serde_json::from_str(&answer).ok()
}

#[test]
fn the_agent_socket_refuses_a_wrong_token_and_app_requests() {
    let fixture = Fixture::new("sleep 30");
    let repo = Repo::new("");
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let mut client = daemon.connect();
    client.hello();
    let project = client.add_project(&repo.root);
    let task = client.create_task(orchestrator_task(project.id, false));
    let AgentId(agent) = task.agents[0].id;
    client.wait_for_agent(|agent| agent.lifecycle == AgentLifecycle::Running);

    let wrong = agent_request(
        &daemon,
        format!(
            r#"{{"method":"hook","agent":{agent},"token":"wrong","params":{{"event":{{"hook_event_name":"Stop"}}}}}}"#
        )
        .as_bytes(),
    );
    let unknown_agent = agent_request(
        &daemon,
        br#"{"method":"hook","agent":999,"token":"x","params":{"event":{}}}"#,
    );
    let listing = agent_request(&daemon, br#"{"method":"list","agent":1,"token":"x"}"#);
    let garbage = agent_request(&daemon, &[0x05, 0x00, 0x00, 0x00, 0x00, 0x01]);

    for answer in [wrong, unknown_agent, listing, garbage] {
        assert_eq!(answer["ok"], false, "{answer}");
    }
    assert_eq!(
        std::fs::metadata(daemon.agent_socket())
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn a_created_agent_leaves_no_process_behind_after_the_daemon_stops() {
    let fixture = Fixture::new("sleep 31");
    let repo = Repo::new("");
    let mut daemon = Daemon::start_with_settings(&fixture.settings());
    let mut client = daemon.connect();
    client.hello();
    let project = client.add_project(&repo.root);
    client.create_task(orchestrator_task(project.id, false));
    client.wait_for_agent(|agent| agent.lifecycle == AgentLifecycle::Running);

    assert!(daemon.stops_on_sigterm(), "the daemon ignored SIGTERM");

    thread::park_timeout(Duration::from_millis(300));
    assert!(!pgrep("sleep 31"));
}

#[test]
fn running_agents_become_resumable_after_a_daemon_crash() {
    let fixture = Fixture::new(
        r#"printf '\033]7501;state=working\007'
printf '{"hook_event_name":"SessionStart","session_id":"hook-session-2"}' | "$SWARM" hook
sleep 30"#,
    );
    let repo = Repo::new("");
    let mut daemon = Daemon::start_with_settings(&fixture.settings());
    let mut client = daemon.connect();
    client.hello();
    let project = client.add_project(&repo.root);
    let task = client.create_task(orchestrator_task(project.id, false));
    client.wait_for_agent(|agent| agent.status == AgentStatus::Working);
    let deadline = Instant::now() + Duration::from_secs(10);
    let before = loop {
        let stored = database(&daemon)
            .agent(task.agents[0].id)
            .expect("read")
            .expect("row");
        if stored.session_confirmed {
            break stored;
        }
        assert!(Instant::now() < deadline, "the session was never confirmed");
        thread::park_timeout(Duration::from_millis(30));
    };
    drop(client);

    daemon.crash();
    let restarted = Daemon::start_in(daemon.home.clone());
    let mut client = restarted.connect();
    client.hello();

    let agent = &client.projects()[0].tasks[0].agents[0];
    assert_eq!(agent.id, task.agents[0].id);
    assert_eq!(agent.lifecycle, AgentLifecycle::Resumable);
    assert_eq!(agent.status, AgentStatus::Unknown);
    assert_eq!(agent.message, None);
    assert_eq!(agent.terminal, None);
    assert!(agent.revision > before.info.revision);
}

#[test]
fn an_agent_that_never_reported_a_session_is_stopped_after_a_daemon_crash() {
    let fixture = Fixture::new(
        r"printf '\033]7501;state=working\007'
sleep 30",
    );
    let repo = Repo::new("");
    let mut daemon = Daemon::start_with_settings(&fixture.settings());
    let mut client = daemon.connect();
    client.hello();
    let project = client.add_project(&repo.root);
    client.create_task(orchestrator_task(project.id, false));
    client.wait_for_agent(|agent| agent.status == AgentStatus::Working);
    drop(client);

    daemon.crash();
    let restarted = Daemon::start_in(daemon.home.clone());
    let mut client = restarted.connect();
    client.hello();

    let agent = &client.projects()[0].tasks[0].agents[0];
    assert_eq!(agent.lifecycle, AgentLifecycle::Stopped);
    assert_eq!(agent.status, AgentStatus::Unknown);
}

#[test]
fn an_agent_that_exits_without_a_session_report_is_stopped() {
    let fixture = Fixture::new("exit 0");
    let repo = Repo::new("");
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let mut client = daemon.connect();
    client.hello();
    let project = client.add_project(&repo.root);
    client.create_task(orchestrator_task(project.id, false));

    let ended = client.wait_for_agent(|agent| {
        agent.lifecycle == AgentLifecycle::Stopped || agent.lifecycle == AgentLifecycle::Resumable
    });

    assert_eq!(ended.lifecycle, AgentLifecycle::Stopped);
}

#[test]
fn a_session_start_hook_types_the_first_message_when_no_osc_report_comes() {
    let fixture = Fixture::new(
        r#"printf '{"hook_event_name":"SessionStart","session_id":"s"}' | "$SWARM" hook
IFS= read -r line
printf '%s\n' "$line" > "$OUT/first-message"
sleep 30"#,
    );
    let repo = Repo::new("");
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let mut client = daemon.connect();
    client.hello();
    let project = client.add_project(&repo.root);

    let mut request = orchestrator_task(project.id, false);
    request.first_message = Some("hello agent".to_owned());
    client.create_task(request);

    assert_eq!(
        fixture.read_when_written("first-message").trim(),
        "hello agent"
    );
}

#[test]
fn the_codex_trust_dialog_blocks_the_agent_with_a_message() {
    let fixture = Fixture::new(
        r"printf 'Folder access\r\n  Trust this folder? Codex can read, edit, and run files here,\r\n> 1. Trust and continue\r\n  2. Quit\r\n'
sleep 30",
    );
    let repo = Repo::new("");
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let mut client = daemon.connect();
    client.hello();
    let project = client.add_project(&repo.root);

    let mut request = orchestrator_task(project.id, false);
    request.orchestrator = Some(AgentKind::Codex);
    client.create_task(request);

    let blocked =
        client.wait_for_agent(|agent| agent.status == AgentStatus::Blocked(BlockedKind::Other));
    assert_eq!(blocked.message.as_deref(), Some("Trust this folder"));
}

#[test]
fn a_missing_command_fails_the_task_before_any_worktree_is_made() {
    let fixture = Fixture::new("sleep 30");
    std::fs::write(
        fixture.settings(),
        "[agents.claude_code]\ncommand = \"/nonexistent/claude\"\norchestrator_model = \"opus\"\nworker_model = \"sonnet\"\n",
    )
    .expect("settings");
    let repo = Repo::new("");
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let mut client = daemon.connect();
    client.hello();
    let project = client.add_project(&repo.root);

    client.send(&Request::CreateTask {
        call: CALL,
        task: orchestrator_task(project.id, false),
    });

    let message = client.wait_for(|event| match event {
        Event::Error {
            call: Some(CALL),
            message,
            ..
        } => Some(message.clone()),
        _ => None,
    });
    assert!(
        message.contains("cannot find /nonexistent/claude"),
        "{message}"
    );
    assert!(
        !repo.root.join(".worktrees/agent-task").exists(),
        "no worktree is made for a command that is missing"
    );
}

#[test]
fn an_agent_folder_and_its_files_are_private() {
    let fixture = Fixture::new("sleep 30");
    let repo = Repo::new("");
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let mut client = daemon.connect();
    client.hello();
    let project = client.add_project(&repo.root);
    let task = client.create_task(orchestrator_task(project.id, false));
    client.wait_for_agent(|agent| agent.lifecycle == AgentLifecycle::Running);

    let folder = daemon
        .home
        .join("agents")
        .join(task.agents[0].id.0.to_string());
    let mode = |path: &Path| {
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    };

    assert_eq!(mode(&folder), 0o700);
    assert_eq!(mode(&folder.join("system-prompt.md")), 0o600);
    assert_eq!(mode(&folder.join("claude-settings.json")), 0o600);
    assert_eq!(mode(&folder.join("mcp.json")), 0o600);
}

/// This test holds more descriptors than macOS gives a process by default.
fn raise_file_limit() {
    let limit = libc::rlimit {
        rlim_cur: 4096,
        rlim_max: 4096,
    };
    // SAFETY: `limit` is a valid `rlimit` that the call reads.
    let raised = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) };
    if raised != 0 {
        let soft = libc::rlimit {
            rlim_cur: 4096,
            rlim_max: libc::RLIM_INFINITY,
        };
        // SAFETY: as above.
        unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const soft) };
    }
}

#[test]
fn the_agent_socket_serves_at_most_256_connections() {
    raise_file_limit();
    let daemon = Daemon::start();
    let held: Vec<UnixStream> = (0..256)
        .map(|_| {
            // The listen queue is shorter than 256 while the daemon accepts.
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match UnixStream::connect(daemon.agent_socket()) {
                    Ok(stream) => break stream,
                    Err(error) => {
                        assert!(Instant::now() < deadline, "connect: {error}");
                        thread::park_timeout(Duration::from_millis(5));
                    }
                }
            }
        })
        .collect();
    thread::park_timeout(Duration::from_millis(300));

    let mut extra = UnixStream::connect(daemon.agent_socket()).expect("connect");
    extra
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    let mut buffer = [0_u8; 1];

    let read = std::io::Read::read(&mut extra, &mut buffer);
    assert!(
        matches!(read, Ok(0)),
        "the extra connection stayed open: {read:?}"
    );

    drop(held);
    let deadline = Instant::now() + Duration::from_secs(5);
    let after = loop {
        let answer = try_agent_request(
            &daemon,
            br#"{"method":"hook","agent":1,"token":"x","params":{"event":{}}}"#,
        );
        match answer {
            Some(answer) => break answer,
            None => assert!(Instant::now() < deadline, "the slots never came back"),
        }
        thread::park_timeout(Duration::from_millis(50));
    };
    assert_eq!(after["error"], "refused");
}

#[test]
fn the_agent_socket_accepts_lines_over_one_mebibyte() {
    let daemon = Daemon::start();
    let padding = "x".repeat(2 << 20);
    let line = format!(
        r#"{{"method":"hook","agent":1,"token":"x","params":{{"event":{{"pad":"{padding}"}}}}}}"#
    );

    let answer = agent_request(&daemon, line.as_bytes());

    assert_eq!(answer["error"], "refused");
}

/// Shell helpers for the fake agents of the harness tests.
const HELPERS: &str = r#"
idle() { printf '\033]7501;state=idle\007'; }
working() { printf '\033]7501;state=working\007'; }
commit() { git -c user.name=Test -c user.email=test@example.com commit -q -m "$1"; }
"#;

fn fixture_with_helpers(body: &str) -> Fixture {
    Fixture::new(&format!("{HELPERS}\n{body}"))
}

/// A repository whose merge commits get an author.
fn repo_for_merges() -> Repo {
    let repo = Repo::new("");
    repo.git(&["config", "user.name", "Test"]);
    repo.git(&["config", "user.email", "test@example.com"]);

    repo
}

/// Starts the task of a harness test: the orchestrator gets "lead the task".
fn start_task(daemon: &Daemon, repo: &Repo) -> (Client, TaskInfo) {
    let mut client = daemon.connect();
    client.hello();
    let project = client.add_project(&repo.root);

    let mut request = orchestrator_task(project.id, false);
    request.first_message = Some("lead the task".to_owned());
    let task = client.create_task(request);

    (client, task)
}

fn git_output(folder: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(folder)
        .args(arguments)
        .output()
        .expect("run git");
    assert!(output.status.success(), "git {arguments:?}");

    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[test]
fn an_orchestrator_and_a_worker_work_a_task_through_the_harness() {
    let fixture = fixture_with_helpers(
        r#"case "$SWARM_ROLE" in
orchestrator)
  idle
  IFS= read -r line
  printf '%s\n' "$line" > "$OUT/orchestrator-first"
  working
  swarm confirm > "$OUT/confirm.out" 2>&1
  swarm spawn api --own-worktree "same files" --brief "build the api" > "$OUT/spawn.out" 2>&1
  swarm wait --timeout 60 > "$OUT/question.out" 2>&1
  swarm send api "use port 5173" > "$OUT/send.out" 2>&1
  swarm wait --timeout 60 > "$OUT/report.out" 2>&1
  swarm merge api > "$OUT/merge.out" 2>&1
  echo $? > "$OUT/merge.code"
  swarm agents > "$OUT/agents.out" 2>&1
  idle
  sleep 30
  ;;
worker)
  idle
  IFS= read -r line
  printf '%s\n' "$line" > "$OUT/worker-brief"
  printf '%s\n' "$PATH" > "$OUT/worker-path"
  working
  swarm ask "which port?" > "$OUT/ask.out" 2>&1
  swarm wait --timeout 60 > "$OUT/answer.out" 2>&1
  echo "port 5173" > api.txt
  git add api.txt
  commit "add api"
  swarm report "api done" > "$OUT/report-sent.out" 2>&1
  idle
  sleep 30
  ;;
esac"#,
    );
    let repo = repo_for_merges();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (mut client, task) = start_task(&daemon, &repo);

    let mut kinds = Vec::new();
    let mut briefed = false;
    let mut worker: Option<AgentInfo> = None;
    client.wait_for(|event| {
        match event {
            Event::MessageSent { message } => kinds.push((message.kind, message.from.is_some())),
            Event::TaskChanged { task } if task.briefed => briefed = true,
            Event::AgentChanged { agent } if agent.role == AgentRole::Worker => {
                worker = Some(agent.clone());
            }
            _ => {}
        }

        (kinds.len() == 4).then_some(())
    });

    assert_eq!(
        kinds.iter().map(|(kind, _)| *kind).collect::<Vec<_>>(),
        [
            MessageKind::Brief,
            MessageKind::Question,
            MessageKind::Task,
            MessageKind::Report
        ]
    );
    assert!(kinds.iter().all(|(_, from_agent)| *from_agent));
    assert!(briefed, "confirm broadcasts TaskChanged");
    let worker = worker.expect("the worker was announced");
    assert_eq!(worker.name, "api");
    assert_eq!(worker.branch, "swarm/agent-task--api");
    assert_eq!(worker.task, Some(task.id));
    assert_eq!(worker.project, task.project);
    assert!(worker.worktree.ends_with(".worktrees/agent-task--api"));
    assert_eq!(worker.kind, AgentKind::ClaudeCode);
    assert_eq!(worker.model, "sonnet");

    assert_eq!(
        fixture.read_when_written("orchestrator-first").trim(),
        "lead the task"
    );
    assert_eq!(
        fixture.read_when_written("worker-brief").trim(),
        "build the api"
    );
    let path = fixture.read_when_written("worker-path");
    let swarm_folder = swarm_binary().parent().expect("folder").to_owned();
    assert!(
        path.starts_with(&format!("{}:", swarm_folder.display())),
        "{path}"
    );

    assert_eq!(fixture.read_when_written("confirm.out").trim(), "ok");
    assert!(
        fixture
            .read_when_written("spawn.out")
            .starts_with("started worker api (agent ")
    );
    let question = fixture.read_when_written("question.out");
    assert!(question.contains("question from api"), "{question}");
    assert!(question.contains("which port?"), "{question}");
    assert!(
        fixture
            .read_when_written("answer.out")
            .contains("use port 5173")
    );
    assert!(fixture.read_when_written("report.out").contains("api done"));

    assert_eq!(fixture.read_when_written("merge.code").trim(), "0");
    assert!(fixture.read_when_written("merge.out").starts_with("merged"));
    assert!(task.worktree.join("api.txt").is_file());
    assert_eq!(
        git_output(&task.worktree, &["log", "--merges", "--format=%s"])
            .lines()
            .count(),
        1
    );
    let agents = fixture.read_when_written("agents.out");
    assert!(
        agents.contains("api") && agents.contains("swarm/agent-task--api"),
        "{agents}"
    );

    assert!(
        !database(&daemon)
            .has_unread(task.agents[0].id)
            .expect("unread"),
        "every message was taken"
    );
}

#[test]
fn a_merge_conflict_is_reported_and_left_for_the_orchestrator() {
    let fixture = fixture_with_helpers(
        r#"case "$SWARM_ROLE" in
orchestrator)
  idle
  IFS= read -r line
  working
  swarm spawn api --own-worktree "same files" --brief "change the readme" > "$OUT/spawn.out" 2>&1
  swarm wait --timeout 60 > "$OUT/report.out" 2>&1
  echo "task text" > README.md
  git add README.md
  commit "task readme"
  swarm merge api > "$OUT/merge.out" 2>&1
  echo $? > "$OUT/merge.code"
  git status --porcelain > "$OUT/status.out"
  sleep 30
  ;;
worker)
  idle
  IFS= read -r line
  working
  echo "worker text" > README.md
  git add README.md
  commit "worker readme"
  swarm report "readme changed" > "$OUT/report-sent.out" 2>&1
  sleep 30
  ;;
esac"#,
    );
    let repo = repo_for_merges();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (_client, _task) = start_task(&daemon, &repo);

    assert_eq!(fixture.read_when_written("merge.code").trim(), "1");
    let merge = fixture.read_when_written("merge.out");
    assert!(
        merge.starts_with("conflict in 1 file:\n  README.md\n"),
        "{merge}"
    );
    assert!(merge.contains("CONFLICT"), "{merge}");
    assert!(
        fixture
            .read_when_written("status.out")
            .contains("UU README.md")
    );
}

#[test]
fn a_message_is_pasted_when_the_agent_becomes_idle() {
    let fixture = fixture_with_helpers(
        r#"case "$SWARM_ROLE" in
orchestrator)
  idle
  IFS= read -r line
  working
  swarm spawn api --brief "report at once" > "$OUT/spawn.out" 2>&1
  while [ ! -f "$OUT/reported" ]; do sleep 0.1; done
  if IFS= read -r -t 1 early; then printf '%s\n' "$early" > "$OUT/early"; fi
  idle
  IFS= read -r header
  IFS= read -r body
  printf '%s\n%s\n' "$header" "$body" > "$OUT/pasted"
  sleep 30
  ;;
worker)
  idle
  IFS= read -r line
  working
  swarm report "all green" > "$OUT/report.out" 2>&1
  echo yes > "$OUT/reported"
  sleep 30
  ;;
esac"#,
    );
    let repo = repo_for_merges();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (_client, _task) = start_task(&daemon, &repo);

    let pasted = fixture.read_when_written("pasted");
    let mut lines = pasted.lines();

    assert!(
        lines
            .next()
            .expect("header")
            .starts_with("[swarm] report from api (agent "),
        "{pasted}"
    );
    assert_eq!(lines.next(), Some("all green"));
    assert!(
        !fixture.out.join("early").exists(),
        "the message was pasted before the agent was idle"
    );
}

#[test]
fn the_daemon_refuses_what_a_role_may_not_do() {
    let fixture = fixture_with_helpers(
        r#"case "$SWARM_ROLE" in
orchestrator)
  idle
  IFS= read -r line
  working
  swarm spawn a --brief "wait for go" > "$OUT/spawn-a.out" 2>&1
  swarm spawn b --brief "stay idle" > "$OUT/spawn-b.out" 2>&1
  swarm send a go > "$OUT/send-go.out" 2>&1
  swarm wait --timeout 60 > "$OUT/wait.out" 2>&1
  swarm ask "q" > "$OUT/orch-ask.out" 2>&1
  swarm report "r" > "$OUT/orch-report.out" 2>&1
  swarm send "$SWARM_AGENT_ID" "to myself" > "$OUT/orch-self.out" 2>&1
  swarm spawn a --brief again > "$OUT/dup.out" 2>&1
  swarm spawn "!!!" --brief again > "$OUT/bad-name.out" 2>&1
  echo done > "$OUT/orchestrator-done"
  sleep 30
  ;;
worker)
  idle
  IFS= read -r line
  working
  if [ "$line" = "wait for go" ]; then
    swarm wait --timeout 60 > "$OUT/a-wait.out" 2>&1
    OTHER=$((SWARM_AGENT_ID + 1))
    swarm send "$OTHER" hi > "$OUT/a-send.out" 2>&1
    swarm spawn x --brief y > "$OUT/a-spawn.out" 2>&1
    swarm merge "$OTHER" > "$OUT/a-merge.out" 2>&1
    swarm notify hi > "$OUT/a-notify.out" 2>&1
    swarm agents > "$OUT/a-agents.out" 2>&1
    swarm confirm > "$OUT/a-confirm.out" 2>&1
    swarm report "refusals recorded" > "$OUT/a-report.out" 2>&1
  fi
  sleep 30
  ;;
esac"#,
    );
    let repo = repo_for_merges();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (_client, _task) = start_task(&daemon, &repo);
    fixture.read_when_written("orchestrator-done");

    let said = |name: &str| fixture.read_when_written(name);
    assert_eq!(
        said("a-send.out").trim(),
        "swarm: workers can only message their orchestrator (use ask or report)"
    );
    assert_eq!(
        said("a-spawn.out").trim(),
        "swarm: only a task's orchestrator can spawn workers"
    );
    assert_eq!(
        said("a-merge.out").trim(),
        "swarm: only a task's orchestrator can merge a worker"
    );
    assert_eq!(
        said("a-notify.out").trim(),
        "swarm: only a task's orchestrator can notify the user"
    );
    assert_eq!(
        said("a-agents.out").trim(),
        "swarm: only a task's orchestrator can list its workers"
    );
    assert_eq!(
        said("a-confirm.out").trim(),
        "swarm: only a task's orchestrator can confirm a brief"
    );
    let not_a_worker =
        "swarm: only workers can ask or report; an orchestrator messages its workers";
    assert_eq!(said("orch-ask.out").trim(), not_a_worker);
    assert_eq!(said("orch-report.out").trim(), not_a_worker);
    assert!(
        said("orch-self.out").contains("is not a worker of your task"),
        "{}",
        said("orch-self.out")
    );
    assert!(said("dup.out").contains("already has a worker named a"));
    assert!(said("bad-name.out").contains("has no letters or digits"));
}

#[test]
fn the_daemon_refuses_a_message_over_64_kib() {
    let fixture = fixture_with_helpers(
        r#"BIG=$(head -c 70000 /dev/zero | tr '\0' x)
case "$SWARM_ROLE" in
orchestrator)
  idle
  IFS= read -r line
  working
  swarm spawn big --brief "$BIG" > "$OUT/big-brief.out" 2>&1
  swarm spawn a --brief "report big" > "$OUT/spawn-a.out" 2>&1
  swarm send a "$BIG" > "$OUT/big-send.out" 2>&1
  echo done > "$OUT/orchestrator-done"
  sleep 30
  ;;
worker)
  idle
  IFS= read -r line
  working
  swarm ask "$BIG" > "$OUT/big-ask.out" 2>&1
  swarm report "$BIG" > "$OUT/big-report.out" 2>&1
  sleep 30
  ;;
esac"#,
    );
    let repo = repo_for_merges();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (_client, _task) = start_task(&daemon, &repo);
    fixture.read_when_written("orchestrator-done");

    let said = |name: &str| fixture.read_when_written(name);
    let refusal = "swarm: message is 69 KiB; the limit is 64 KiB: shorten it or write it to a file and send the path";
    assert_eq!(said("big-brief.out").trim(), refusal);
    assert_eq!(said("big-send.out").trim(), refusal);
    assert_eq!(said("big-ask.out").trim(), refusal);
    assert_eq!(said("big-report.out").trim(), refusal);
}

#[test]
fn a_notice_blocks_the_agent_until_it_works_again() {
    let fixture = fixture_with_helpers(
        r#"idle
IFS= read -r line
swarm notify "Review needed" > "$OUT/notify.out" 2>&1
sleep 2
working
sleep 30"#,
    );
    let repo = repo_for_merges();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (mut client, _task) = start_task(&daemon, &repo);

    let blocked =
        client.wait_for_agent(|agent| agent.status == AgentStatus::Blocked(BlockedKind::Other));
    assert_eq!(blocked.message.as_deref(), Some("Review needed"));

    let working = client.wait_for_agent(|agent| agent.status == AgentStatus::Working);
    assert_eq!(working.message, None);
    assert_eq!(fixture.read_when_written("notify.out").trim(), "ok");
}

#[test]
fn input_from_the_app_clears_a_notice() {
    let fixture = fixture_with_helpers(
        r#"idle
IFS= read -r line
swarm notify "Review needed" > "$OUT/notify.out" 2>&1
sleep 30"#,
    );
    let repo = repo_for_merges();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (mut client, _task) = start_task(&daemon, &repo);

    let blocked =
        client.wait_for_agent(|agent| agent.status == AgentStatus::Blocked(BlockedKind::Other));
    client.send(&Request::Input {
        id: blocked.terminal.expect("terminal"),
        input: TerminalInput::Text("x".to_owned()),
    });

    let cleared = client.wait_for_agent(|agent| agent.status == AgentStatus::Idle);
    assert_eq!(cleared.message, None);
}

#[test]
fn the_mcp_server_serves_an_agent_through_the_daemon() {
    let fixture = fixture_with_helpers(
        r#"idle
IFS= read -r line
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"confirm","arguments":{}}}' \
  '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"agents","arguments":{}}}' \
  '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"report","arguments":{"text":"x"}}}' \
  '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"terminal_list","arguments":{}}}' \
  | swarm mcp > "$OUT/mcp.out" 2> "$OUT/mcp.err"
echo done > "$OUT/mcp.done"
sleep 30"#,
    );
    let repo = repo_for_merges();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (mut client, _task) = start_task(&daemon, &repo);

    fixture.read_when_written("mcp.done");
    let replies: Vec<serde_json::Value> = fixture
        .read_when_written("mcp.out")
        .lines()
        .map(|line| serde_json::from_str(line).expect("json line"))
        .collect();

    assert_eq!(replies.len(), 6);
    let reply = |id: u64| {
        replies
            .iter()
            .find(|reply| reply["id"] == id)
            .expect("a reply for the id")
    };
    let tools: Vec<&str> = reply(2)["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(
        tools,
        [
            "spawn",
            "send",
            "commit",
            "read",
            "wait",
            "merge",
            "notify",
            "agents",
            "confirm",
            "terminal_spawn",
            "terminal_resize",
            "terminal_read",
            "terminal_wait",
            "terminal_kill",
            "terminal_list",
        ]
    );
    assert_eq!(
        reply(6)["result"]["content"][0]["text"],
        "no process terminals"
    );
    assert_eq!(reply(3)["result"]["isError"], false);
    assert_eq!(reply(3)["result"]["content"][0]["text"], "ok");
    assert_eq!(reply(4)["result"]["content"][0]["text"], "no workers");
    assert_eq!(reply(5)["error"]["code"], -32602);
    client.wait_for(|event| match event {
        Event::TaskChanged { task } if task.briefed => Some(()),
        _ => None,
    });
}

#[test]
fn claude_agents_get_the_mcp_file_the_flag_and_the_permission_rules() {
    let fixture = fixture_with_helpers(
        r#"printf '%s\n' "$@" > "$OUT/args"
echo "$SWARM_TOKEN" > "$OUT/token"
sleep 30"#,
    );
    let repo = repo_for_merges();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (_client, task) = start_task(&daemon, &repo);

    let args = fixture.read_when_written("args");
    let folder = daemon
        .home
        .join("agents")
        .join(task.agents[0].id.0.to_string());

    assert!(
        args.contains(&format!(
            "--mcp-config\n{}",
            folder.join("mcp.json").display()
        )),
        "{args}"
    );
    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(folder.join("mcp.json")).expect("file"))
            .expect("json");
    let server = &mcp["mcpServers"]["swarm"];
    assert_eq!(server["command"], swarm_binary().display().to_string());
    assert_eq!(server["env"]["SWARM_ROLE_MCP"], "${SWARM_ROLE}");
    assert_eq!(server["env"]["SWARM_AGENT_ID_MCP"], "${SWARM_AGENT_ID}");
    let settings = std::fs::read_to_string(folder.join("claude-settings.json")).expect("file");
    assert!(settings.contains("Bash(swarm read *)") && settings.contains("mcp__swarm__read"));
    assert!(!settings.contains("Bash(swarm *)") && !settings.contains("mcp__swarm__*"));
    assert!(!settings.contains("terminal spawn") && !settings.contains("terminal_spawn"));

    let token = fixture.read_when_written("token");
    let token = token.trim();
    assert!(!token.is_empty());
    for entry in std::fs::read_dir(&folder).expect("agent folder") {
        let path = entry.expect("entry").path();
        let text = std::fs::read_to_string(&path).expect("file");
        assert!(!text.contains(token), "{} holds the token", path.display());
    }
}

#[test]
fn nothing_is_pasted_before_the_first_message_went_in() {
    let fixture = fixture_with_helpers(
        r#"case "$SWARM_ROLE" in
orchestrator)
  idle
  IFS= read -r line
  working
  swarm spawn api --brief "the brief" > "$OUT/spawn.out" 2>&1
  swarm send api "early message" > "$OUT/send.out" 2>&1
  sleep 30
  ;;
worker)
  sleep 1
  idle
  IFS= read -r first
  printf '%s\n' "$first" > "$OUT/first"
  working
  sleep 1
  idle
  IFS= read -r header
  IFS= read -r body
  printf '%s\n%s\n' "$header" "$body" > "$OUT/second"
  sleep 30
  ;;
esac"#,
    );
    let repo = repo_for_merges();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (_client, _task) = start_task(&daemon, &repo);

    assert_eq!(fixture.read_when_written("first").trim(), "the brief");
    let second = fixture.read_when_written("second");
    assert!(
        second.starts_with("[swarm] task from orchestrator (agent "),
        "{second}"
    );
    assert!(second.ends_with("early message\n"), "{second}");
}

/// The terminal ID in `swarm terminal spawn`'s first line, as a shell snippet.
const TERMINAL_ID: &str = r"sed -n 's/^started terminal \([0-9]*\).*/\1/p'";

impl Client {
    /// Reads events until a message satisfies `pick`.
    fn wait_for_message(&mut self, pick: impl Fn(&MessageInfo) -> bool) -> MessageInfo {
        self.wait_for(|event| match event {
            Event::MessageSent { message } if pick(message) => Some(message.clone()),
            _ => None,
        })
    }

    fn start_orchestrator(&mut self, project: ProjectId) -> AgentInfo {
        self.send(&Request::StartOrchestrator {
            call: CALL,
            project,
            kind: None,
        });

        self.wait_for(|event| match event {
            Event::AgentStarted { call: CALL, agent } => Some(agent.clone()),
            _ => None,
        })
    }
}

fn sized_terminal_repo() -> Repo {
    let repo = Repo::new("[worktrees]\nports = { range = \"5170-5199\", per_worktree = 3 }\n");
    repo.git(&["config", "user.name", "Test"]);
    repo.git(&["config", "user.email", "test@example.com"]);

    repo
}

#[test]
#[expect(clippy::too_many_lines, reason = "one scripted scenario")]
fn an_agent_runs_a_command_in_a_sized_terminal_and_stops_it() {
    let fixture = fixture_with_helpers(&format!(
        r#"idle
IFS= read -r line
working
swarm terminal spawn --name "dev server" --size 100x30 -- sh -c 'echo ready in 5ms; sleep 60' > "$OUT/spawn.out" 2>&1
ID=$({TERMINAL_ID} < "$OUT/spawn.out")
swarm terminal spawn -- sh -c 'sleep 0.3; exit 3' > "$OUT/spawn-short.out" 2>&1
SHORT=$({TERMINAL_ID} < "$OUT/spawn-short.out")
swarm terminal wait "$ID" --output "ready in" --timeout 20 > "$OUT/wait.out" 2>&1
swarm terminal read "$ID" > "$OUT/read.out" 2>&1
swarm terminal list > "$OUT/list.out" 2>&1
swarm terminal resize "$ID" 120x40 > "$OUT/resize.out" 2>&1
swarm terminal wait "$SHORT" --exit --timeout 20 > "$OUT/short.out" 2>&1
swarm terminal kill "$ID" > "$OUT/kill.out" 2>&1
swarm terminal wait "$ID" --exit --timeout 20 > "$OUT/exit.out" 2>&1
swarm terminal read "$ID" > "$OUT/read-after.out" 2>&1
swarm wait --timeout 20 > "$OUT/message-1.out" 2>&1
swarm wait --timeout 3 > "$OUT/message-2.out" 2>&1
echo "$ID $SHORT" > "$OUT/ids"
sleep 30"#
    ));
    let repo = sized_terminal_repo();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (mut client, task) = start_task(&daemon, &repo);
    let owner = task.agents[0].id;

    let ids = fixture.read_when_written("ids");
    let ids: Vec<u64> = ids
        .split_whitespace()
        .map(|id| id.parse().expect("terminal id"))
        .collect();
    let (dev, short) = (ids[0], ids[1]);

    assert!(
        fixture
            .read_when_written("spawn.out")
            .contains("size: 100x30")
    );
    assert_eq!(
        fixture.read_when_written("wait.out").trim(),
        "found: ready in 5ms"
    );
    assert_eq!(fixture.read_when_written("read.out").trim(), "ready in 5ms");
    let list = fixture.read_when_written("list.out");
    assert!(
        list.contains(&format!(
            "{dev}  dev server  100x30  running  agent {}",
            owner.0
        )),
        "{list}"
    );
    assert_eq!(
        fixture.read_when_written("resize.out").trim(),
        format!("terminal {dev} is now 120x40")
    );
    assert_eq!(
        fixture.read_when_written("short.out").trim(),
        format!("terminal {short} ended, code 3")
    );
    assert!(
        fixture
            .read_when_written("exit.out")
            .starts_with(&format!("terminal {dev} ended, signal"))
    );
    assert_eq!(
        fixture.read_when_written("read-after.out").trim(),
        "ready in 5ms",
        "the output stays readable after the exit"
    );

    let messages = format!(
        "{}{}",
        fixture.read_when_written("message-1.out"),
        std::fs::read_to_string(fixture.out.join("message-2.out")).unwrap_or_default()
    );
    assert!(
        messages.contains(&format!(
            "dev server (terminal {dev}) stopped by orchestrator, signal"
        )),
        "{messages}"
    );
    assert!(
        messages.contains(&format!(
            "sh -c sleep 0.3; exit 3 (terminal {short}) exited, code 3"
        )),
        "{messages}"
    );
    assert!(
        messages.contains("terminal exited from swarm:"),
        "{messages}"
    );

    let mut changes = Vec::new();
    let mut exited = Vec::new();
    client.wait_for(|event| {
        match event {
            Event::TerminalChanged { terminal } => changes.push(terminal.clone()),
            Event::MessageSent { message } if message.kind == MessageKind::TerminalExited => {
                exited.push(message.clone());
            }
            _ => {}
        }

        (!exited.is_empty()).then_some(())
    });
    let first = changes
        .iter()
        .find(|terminal| terminal.name.as_deref() == Some("dev server"))
        .expect("the spawn was announced");
    assert_eq!(first.kind, TerminalKind::Process);
    assert_eq!(first.owner, Some(owner));
    assert_eq!(
        first.fixed_size.map(|size| (size.cols, size.rows)),
        Some((100, 30))
    );
    assert_eq!(first.task.as_deref(), Some(task.id.terminal_key().as_str()));
    assert!(
        exited
            .iter()
            .all(|message| message.from.is_none() && message.to == owner)
    );

    let stored = database(&daemon)
        .process(TerminalId(dev))
        .expect("read")
        .expect("the owner is stored");
    assert_eq!(stored.owner, owner);
    assert_eq!(stored.name.as_deref(), Some("dev server"));
    assert_eq!((stored.size.cols, stored.size.rows), (120, 40));
}

#[test]
fn a_process_terminal_gets_the_task_env_but_not_the_token() {
    let fixture = fixture_with_helpers(
        r#"idle
IFS= read -r line
working
swarm terminal spawn -- sh -c 'env | grep "^SWARM_" | sort > "$1"' sh "$OUT/env.out" > "$OUT/spawn.out" 2>&1
sleep 30"#,
    );
    let repo = sized_terminal_repo();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (_client, task) = start_task(&daemon, &repo);

    let env = fixture.read_when_written("env.out");

    assert!(env.contains(&format!("SWARM_TASK={}", task.id.0)), "{env}");
    assert!(env.contains("SWARM_PORT="), "{env}");
    assert!(env.contains("SWARM_PORTS="), "{env}");
    assert!(!env.contains("SWARM_TOKEN"), "{env}");
}

#[test]
fn the_app_stopping_a_process_terminal_tells_its_owner_and_the_orchestrator() {
    let fixture = fixture_with_helpers(
        r#"case "$SWARM_ROLE" in
orchestrator)
  idle
  IFS= read -r line
  working
  swarm spawn api --brief "run the server" > "$OUT/spawn.out" 2>&1
  until swarm terminal list 2>&1 | grep -q running; do sleep 0.2; done
  ID=$(swarm terminal list | awk '{print $1}')
  swarm terminal read "$ID" > "$OUT/orchestrator-read.out" 2>&1
  echo "$ID" > "$OUT/id"
  swarm wait --timeout 30 > "$OUT/orchestrator-message.out" 2>&1
  sleep 30
  ;;
worker)
  idle
  IFS= read -r line
  working
  swarm terminal spawn --name "dev server" -- sh -c 'echo serving; sleep 60' > "$OUT/spawn.out" 2>&1
  swarm wait --timeout 30 > "$OUT/worker-message.out" 2>&1
  sleep 30
  ;;
esac"#,
    );
    let repo = sized_terminal_repo();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (mut client, _task) = start_task(&daemon, &repo);

    let id = fixture.read_when_written("id");
    let id = TerminalId(id.trim().parse().expect("terminal id"));
    assert_eq!(
        fixture.read_when_written("orchestrator-read.out").trim(),
        "serving",
        "the orchestrator may read the worker's terminal"
    );
    client.send(&Request::Input {
        id,
        input: TerminalInput::Kill,
    });

    let wanted = format!("dev server (terminal {}) stopped by the user, ", id.0);
    let orchestrator = fixture.read_when_written("orchestrator-message.out");
    let worker = fixture.read_when_written("worker-message.out");
    assert!(orchestrator.contains(&wanted), "{orchestrator}");
    assert!(worker.contains(&wanted), "{worker}");
}

#[test]
fn the_app_cannot_resize_a_terminal_an_agent_sized() {
    let fixture = fixture_with_helpers(
        r#"idle
IFS= read -r line
working
swarm terminal spawn --size 100x30 -- sh -c 'while true; do stty size; sleep 0.2; done' > "$OUT/spawn.out" 2>&1
sed -n 's/^started terminal \([0-9]*\).*/\1/p' "$OUT/spawn.out" > "$OUT/id"
until swarm terminal read "$(cat "$OUT/id")" --lines 100 | grep -q typed-after-resize; do sleep 0.2; done
sleep 1
swarm terminal read "$(cat "$OUT/id")" --lines 100 > "$OUT/read.out" 2>&1
sleep 30"#,
    );
    let repo = sized_terminal_repo();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (mut client, _task) = start_task(&daemon, &repo);

    let id = TerminalId(fixture.read_when_written("id").trim().parse().expect("id"));
    client.send(&Request::Input {
        id,
        input: TerminalInput::Resize(Size {
            cols: 50,
            rows: 10,
            cell_width: 8,
            cell_height: 16,
        }),
    });
    // The terminal echoes this only after the daemon handled the resize before it.
    client.send(&Request::Input {
        id,
        input: TerminalInput::Text("typed-after-resize".to_owned()),
    });

    let read = fixture.read_when_written("read.out");
    assert!(read.contains("30 100"), "{read}");
    assert!(!read.contains("10 50"), "{read}");
}

#[test]
#[expect(clippy::too_many_lines, reason = "one scripted scenario")]
fn agents_never_reach_user_and_agent_terminals_or_each_others_terminals() {
    let fixture = fixture_with_helpers(
        r#"case "$SWARM_ROLE" in
orchestrator)
  idle
  IFS= read -r line
  working
  mkdir sub
  ln -s /tmp link
  swarm spawn helper --brief "check the refusals" > "$OUT/spawn.out" 2>&1
  swarm terminal spawn --name mine -- sleep 60 > "$OUT/mine.out" 2>&1
  sed -n 's/^started terminal \([0-9]*\).*/\1/p' "$OUT/mine.out" > "$OUT/mine-id"
  swarm terminal spawn --cwd / -- true > "$OUT/cwd-root.out" 2>&1
  swarm terminal spawn --cwd .. -- true > "$OUT/cwd-parent.out" 2>&1
  swarm terminal spawn --cwd link -- true > "$OUT/cwd-link.out" 2>&1
  swarm terminal spawn --cwd nowhere -- true > "$OUT/cwd-missing.out" 2>&1
  swarm terminal spawn --cwd sub --name inside -- sleep 60 > "$OUT/cwd-inside.out" 2>&1
  until [ -s "$OUT/ids" ]; do sleep 0.1; done
  read USER AGENT < "$OUT/ids"
  n=0
  for ID in $USER $AGENT; do
    n=$((n + 1))
    swarm terminal read "$ID" > "$OUT/read-$n.out" 2>&1
    swarm terminal wait "$ID" --exit --timeout 1 > "$OUT/wait-$n.out" 2>&1
    swarm terminal kill "$ID" > "$OUT/kill-$n.out" 2>&1
    swarm terminal resize "$ID" small > "$OUT/resize-$n.out" 2>&1
  done
  until [ -s "$OUT/worker-id" ]; do sleep 0.1; done
  WORKER_TERMINAL=$(cat "$OUT/worker-id")
  swarm terminal resize "$WORKER_TERMINAL" small > "$OUT/orchestrator-resize.out" 2>&1
  swarm terminal read "$WORKER_TERMINAL" > "$OUT/orchestrator-read.out" 2>&1
  swarm terminal list > "$OUT/list.out" 2>&1
  echo done > "$OUT/orchestrator-done"
  sleep 30
  ;;
worker)
  idle
  IFS= read -r line
  working
  until [ -s "$OUT/mine-id" ]; do sleep 0.1; done
  MINE=$(cat "$OUT/mine-id")
  swarm terminal list > "$OUT/worker-list.out" 2>&1
  swarm terminal resize "$MINE" small > "$OUT/worker-resize.out" 2>&1
  swarm terminal read "$MINE" > "$OUT/worker-read.out" 2>&1
  swarm terminal wait "$MINE" --exit --timeout 1 > "$OUT/worker-wait.out" 2>&1
  swarm terminal kill "$MINE" > "$OUT/worker-kill.out" 2>&1
  swarm terminal spawn --name own -- sleep 60 > "$OUT/own.out" 2>&1
  sed -n 's/^started terminal \([0-9]*\).*/\1/p' "$OUT/own.out" > "$OUT/worker-id"
  sleep 30
  ;;
esac"#,
    );
    let repo = sized_terminal_repo();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (mut client, task) = start_task(&daemon, &repo);

    let orchestrator = client
        .wait_for_agent(|agent| agent.role == AgentRole::Orchestrator && agent.terminal.is_some());
    let user_terminal = {
        let mut app = daemon.connect();
        app.hello();
        app.send(&Request::Create {
            call: CALL,
            terminal: CreateTerminal {
                kind: TerminalKind::User,
                command: Some(vec!["/bin/sleep".to_owned(), "60".to_owned()]),
                cwd: None,
                size: Size {
                    cols: 80,
                    rows: 24,
                    cell_width: 8,
                    cell_height: 16,
                },
                palette: Palette {
                    fg: Rgb(230, 230, 230),
                    bg: Rgb(10, 10, 12),
                    cursor: Rgb(255, 255, 255),
                    ansi: [Rgb(120, 120, 120); 16],
                },
                task: Some(task.id.terminal_key()),
            },
        });
        let id = app.wait_for(|event| match event {
            Event::Created { call: CALL, id } => Some(*id),
            _ => None,
        });

        (app, id)
    };
    let agent_terminal = orchestrator.terminal.expect("the agent terminal");
    std::fs::write(
        fixture.out.join("ids"),
        format!("{} {}\n", user_terminal.1.0, agent_terminal.0),
    )
    .expect("ids");

    fixture.read_when_written("orchestrator-done");
    let said = |name: &str| fixture.read_when_written(name).trim().to_owned();

    for (number, id) in [(1, user_terminal.1), (2, agent_terminal)] {
        let missing = format!("swarm: no process terminal {} in your task", id.0);
        for action in ["read", "wait", "kill", "resize"] {
            assert_eq!(said(&format!("{action}-{number}.out")), missing);
        }
    }
    for name in ["cwd-root.out", "cwd-parent.out", "cwd-link.out"] {
        assert!(
            said(name).contains("is outside your worktree"),
            "{name}: {}",
            said(name)
        );
    }
    assert!(said("cwd-missing.out").contains("cannot use the folder"));
    assert!(said("cwd-inside.out").starts_with("started terminal "));

    let mine = said("mine-id");
    let list = said("list.out");
    let ids: Vec<&str> = list
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert!(ids.contains(&mine.as_str()), "{list}");
    assert!(
        !ids.contains(&user_terminal.1.0.to_string().as_str()),
        "{list}"
    );
    assert!(
        !ids.contains(&agent_terminal.0.to_string().as_str()),
        "{list}"
    );

    assert!(said("worker-list.out").contains("mine"));
    assert_eq!(
        said("worker-resize.out"),
        format!("swarm: only the agent that started terminal {mine} can resize it")
    );
    for (name, action) in [
        ("worker-read.out", "read"),
        ("worker-wait.out", "wait for"),
        ("worker-kill.out", "kill"),
    ] {
        assert_eq!(
            said(name),
            format!(
                "swarm: only the agent that started terminal {mine} or the task's orchestrator can {action} it"
            )
        );
    }
    let worker_terminal = said("worker-id");
    assert_eq!(
        said("orchestrator-resize.out"),
        format!("swarm: only the agent that started terminal {worker_terminal} can resize it")
    );
    assert!(!said("orchestrator-read.out").starts_with("swarm:"));

    let alive = client.terminals();
    let running = |id: &str| alive.iter().any(|terminal| terminal.id.0.to_string() == id);
    assert!(running(&mine), "the worker could not kill it");
    assert!(running(&worker_terminal));
    assert!(alive.iter().any(|terminal| terminal.id == user_terminal.1));
}

#[test]
#[expect(clippy::too_many_lines, reason = "one scripted scenario")]
fn the_start_orchestrator_hands_two_tasks_to_their_orchestrators_and_exits() {
    let fixture = fixture_with_helpers(
        r#"case "$SWARM_ROLE" in
start_orchestrator)
  printf '{"hook_event_name":"SessionStart","session_id":"start-session"}' | "$SWARM" hook
  swarm task create "Login page" --brief "build the login page" > "$OUT/one.out" 2>&1
  swarm task create "Billing" --kind claude --brief "build billing" > "$OUT/two.out" 2>&1
  ONE=$(sed -n 's/^created task \([0-9]*\).*/\1/p' "$OUT/one.out")
  TWO=$(sed -n 's/^created task \([0-9]*\).*/\1/p' "$OUT/two.out")
  swarm task wait "$ONE" "$TWO" --until briefed --timeout 60 > "$OUT/wait.out" 2>&1
  echo $? > "$OUT/wait.code"
  swarm task list > "$OUT/list.out" 2>&1
  swarm task create "Nope" --brief "x" --from "no-such-branch" > "$OUT/bad-branch.out" 2>&1
  swarm exit > "$OUT/exit.out" 2>&1
  sleep 30
  ;;
orchestrator)
  idle
  IFS= read -r line
  printf '%s\n' "$line" > "$OUT/brief-$SWARM_TASK"
  working
  swarm confirm > "$OUT/confirm-$SWARM_TASK.out" 2>&1
  sleep 30
  ;;
esac"#,
    );
    let repo = sized_terminal_repo();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let mut client = daemon.connect();
    client.hello();
    let mut watcher = daemon.connect();
    watcher.hello();
    let project = client.add_project(&repo.root);
    assert_eq!(project.start, None);

    let start = client.start_orchestrator(project.id);

    assert_eq!(start.role, AgentRole::StartOrchestrator);
    assert_eq!(start.task, None);
    assert_eq!(start.name, "start");
    assert_eq!(start.worktree, repo.root);
    assert_eq!(start.branch, project.base_branch);
    assert_eq!(start.kind, AgentKind::ClaudeCode);
    assert_eq!(start.project, project.id);

    let again = client.start_orchestrator(project.id);
    assert_eq!(
        again.id, start.id,
        "a running Start orchestrator is returned"
    );
    assert_eq!(
        client.projects()[0].start.as_ref().map(|agent| agent.id),
        Some(start.id)
    );

    let mut created = Vec::new();
    let mut briefed = Vec::new();
    let mut briefs = Vec::new();
    let mut agent_changes = 0;
    watcher.wait_for(|event| {
        match event {
            Event::TaskChanged { task } => {
                if task.created_by == Some(start.id) && !created.contains(&task.id) {
                    created.push(task.id);
                }
                if task.briefed && !briefed.contains(&task.id) {
                    briefed.push(task.id);
                }
            }
            Event::MessageSent { message } if message.kind == MessageKind::Brief => {
                briefs.push(message.clone());
            }
            Event::AgentChanged { .. } => agent_changes += 1,
            _ => {}
        }

        (briefed.len() == 2).then_some(())
    });

    assert_eq!(created.len(), 2, "TaskChanged announces each new task");
    assert_eq!(briefs.len(), 2);
    assert!(briefs.iter().all(|message| message.from == Some(start.id)));
    assert!(agent_changes > 0);

    let stopped = client
        .wait_for_agent(|agent| agent.id == start.id && agent.lifecycle == AgentLifecycle::Stopped);
    assert_eq!(stopped.role, AgentRole::StartOrchestrator);

    let wait = fixture.read_when_written("wait.out");
    assert_eq!(wait.matches("briefed").count(), 2, "{wait}");
    assert!(
        !wait.contains("not briefed") && !wait.contains("timed out"),
        "{wait}"
    );
    assert_eq!(fixture.read_when_written("wait.code").trim(), "0");
    let list = fixture.read_when_written("list.out");
    assert!(
        list.contains("Login page") && list.contains("Billing"),
        "{list}"
    );
    assert!(fixture.read_when_written("exit.out").contains("ok"));
    assert!(
        fixture
            .read_when_written("bad-branch.out")
            .starts_with("swarm: "),
        "a failing task_create is an error"
    );

    let listed = client.projects();
    assert_eq!(
        listed[0].start, None,
        "the stopped Start orchestrator is gone"
    );
    assert_eq!(listed[0].tasks.len(), 2);
    for task in &listed[0].tasks {
        assert_eq!(task.created_by, Some(start.id));
        assert!(task.briefed);
        assert_eq!(task.agents.len(), 1);
        assert_eq!(task.agents[0].role, AgentRole::Orchestrator);
        assert_eq!(
            fixture
                .read_when_written(&format!("brief-{}", task.id.0))
                .trim(),
            if task.name == "Billing" {
                "build billing"
            } else {
                "build the login page"
            }
        );
    }

    let stored = database(&daemon)
        .agent(start.id)
        .expect("read")
        .expect("row");
    assert!(stored.session_confirmed, "the session was real");
    assert_eq!(
        stored.info.lifecycle,
        AgentLifecycle::Stopped,
        "an agent that exits is stopped, not resumable"
    );

    let restarted = client.start_orchestrator(project.id);
    assert_ne!(restarted.id, start.id, "a stopped one is replaced");
}

#[test]
fn only_the_start_orchestrator_may_use_the_task_calls() {
    let fixture = fixture_with_helpers(
        r#"idle
IFS= read -r line
working
swarm task create x --brief y > "$OUT/create.out" 2>&1
swarm task list > "$OUT/list.out" 2>&1
swarm task wait 1 --until done --timeout 1 > "$OUT/wait.out" 2>&1
sleep 30"#,
    );
    let repo = sized_terminal_repo();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (_client, _task) = start_task(&daemon, &repo);

    assert_eq!(
        fixture.read_when_written("create.out").trim(),
        "swarm: only the Start orchestrator can create tasks"
    );
    assert_eq!(
        fixture.read_when_written("list.out").trim(),
        "swarm: only the Start orchestrator can list the tasks"
    );
    assert_eq!(
        fixture.read_when_written("wait.out").trim(),
        "swarm: only the Start orchestrator can wait for tasks"
    );
}

#[test]
fn the_orchestrator_hears_when_the_user_types_into_a_worker_but_only_once_in_a_while() {
    let fixture = fixture_with_helpers(
        r#"case "$SWARM_ROLE" in
orchestrator)
  idle
  IFS= read -r line
  working
  swarm spawn api --brief "work" > "$OUT/spawn.out" 2>&1
  swarm wait --timeout 60 > "$OUT/first.out" 2>&1
  echo ok > "$OUT/got-first"
  swarm wait --timeout 4 > "$OUT/second.out" 2>&1
  echo ok > "$OUT/done"
  sleep 30
  ;;
worker)
  idle
  IFS= read -r line
  working
  sleep 60
  ;;
esac"#,
    );
    let repo = sized_terminal_repo();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (mut client, _task) = start_task(&daemon, &repo);

    let orchestrator = client
        .wait_for_agent(|agent| agent.role == AgentRole::Orchestrator && agent.terminal.is_some());
    let worker =
        client.wait_for_agent(|agent| agent.role == AgentRole::Worker && agent.terminal.is_some());
    let enter = || {
        TerminalInput::Key(swarm_term::Key {
            name: "enter".to_owned(),
            modifiers: swarm_term::Modifiers::default(),
            text: None,
            repeat: false,
        })
    };

    client.send(&Request::Input {
        id: orchestrator.terminal.expect("terminal"),
        input: TerminalInput::Text("not for the orchestrator\r".to_owned()),
    });
    client.send(&Request::Input {
        id: worker.terminal.expect("terminal"),
        input: TerminalInput::Resize(Size {
            cols: 90,
            rows: 20,
            cell_width: 8,
            cell_height: 16,
        }),
    });
    client.send(&Request::Input {
        id: worker.terminal.expect("terminal"),
        input: enter(),
    });
    client.send(&Request::Input {
        id: worker.terminal.expect("terminal"),
        input: TerminalInput::Text("hello".to_owned()),
    });
    client.send(&Request::Input {
        id: worker.terminal.expect("terminal"),
        input: enter(),
    });

    let message = client.wait_for_message(|message| message.kind == MessageKind::UserInput);
    assert_eq!(message.from, None);
    assert_eq!(message.to, orchestrator.id);
    assert_eq!(message.text, "The user typed into api's terminal.");

    fixture.read_when_written("got-first");
    client.send(&Request::Input {
        id: worker.terminal.expect("terminal"),
        input: TerminalInput::Paste("more text\n".to_owned()),
    });
    fixture.read_when_written("done");

    let first = fixture.read_when_written("first.out");
    assert!(first.contains("user input from swarm:"), "{first}");
    assert_eq!(first.matches("The user typed").count(), 1, "{first}");
    assert!(!first.contains("orchestrator"), "{first}");
    assert_eq!(
        fixture.read_when_written("second.out").trim(),
        "no new messages",
        "a second message within 30 seconds is held back"
    );
}

fn ported_repo(range: &str, setup: &str) -> Repo {
    let repo = Repo::new(&format!(
        "[worktrees]\nports = {{ range = \"{range}\", per_worktree = 3 }}\n{setup}"
    ));
    repo.git(&["config", "user.name", "Test"]);
    repo.git(&["config", "user.email", "test@example.com"]);

    repo
}

#[test]
fn the_orchestrator_hears_that_a_worker_ended_and_its_ports_come_back() {
    let fixture = fixture_with_helpers(
        r#"case "$SWARM_ROLE" in
orchestrator)
  idle
  IFS= read -r line
  working
  swarm spawn first --brief "end at once" > "$OUT/spawn-1.out" 2>&1
  swarm wait --timeout 30 > "$OUT/ended.out" 2>&1
  swarm spawn second --brief "end at once" > "$OUT/spawn-2.out" 2>&1
  sleep 30
  ;;
worker)
  exit 0
  ;;
esac"#,
    );
    let repo = ported_repo("5170-5175", "");
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (_client, _task) = start_task(&daemon, &repo);

    let ended = fixture.read_when_written("ended.out");
    let second = fixture.read_when_written("spawn-2.out");

    assert!(
        ended.contains("[swarm] worker stopped from swarm:\nworker first stopped, code 0"),
        "{ended}"
    );
    assert!(second.starts_with("started worker"), "{second}");
}

#[test]
fn the_orchestrator_hears_that_a_worker_failed_to_start() {
    let fixture = fixture_with_helpers(
        r#"idle
IFS= read -r line
working
swarm spawn api --own-worktree "same files" --brief "never starts" > "$OUT/spawn.out" 2>&1
swarm wait --timeout 30 > "$OUT/failed.out" 2>&1
sleep 30"#,
    );
    let repo = ported_repo("5170-5199", "setup = \"exit 3\"\n");
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (_client, _task) = start_task(&daemon, &repo);

    let failed = fixture.read_when_written("failed.out");

    assert!(
        failed.contains("worker api failed: Setup failed"),
        "{failed}"
    );
}

#[test]
fn the_terminals_of_an_agent_stop_when_it_ends() {
    let fixture = fixture_with_helpers(
        r#"case "$SWARM_ROLE" in
orchestrator)
  idle
  IFS= read -r line
  working
  swarm spawn api --brief "start a server and end" > "$OUT/spawn.out" 2>&1
  swarm wait --timeout 30 > "$OUT/message-1.out" 2>&1
  swarm wait --timeout 30 > "$OUT/message-2.out" 2>&1
  kill -0 "$(cat "$OUT/pid")" 2>/dev/null
  echo $? > "$OUT/alive"
  sleep 30
  ;;
worker)
  idle
  IFS= read -r line
  working
  swarm terminal spawn --name dev -- sh -c 'echo $$ > "$1"; exec sleep 60' sh "$OUT/pid" > "$OUT/terminal.out" 2>&1
  until [ -s "$OUT/pid" ]; do sleep 0.1; done
  exit 0
  ;;
esac"#,
    );
    let repo = ported_repo("5170-5199", "");
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (_client, _task) = start_task(&daemon, &repo);

    let alive = fixture.read_when_written("alive");
    let messages = format!(
        "{}{}",
        fixture.read_when_written("message-1.out"),
        fixture.read_when_written("message-2.out")
    );

    assert_ne!(alive.trim(), "0", "the process still runs");
    assert!(messages.contains("dev (terminal "), "{messages}");
    assert!(messages.contains("stopped, its owner ended"), "{messages}");
    assert!(messages.contains("worker api stopped"), "{messages}");
}

#[test]
fn a_kill_names_the_agent_that_did_it() {
    let fixture = fixture_with_helpers(
        r#"case "$SWARM_ROLE" in
orchestrator)
  idle
  IFS= read -r line
  working
  swarm spawn api --brief "start a server and stop it" > "$OUT/spawn.out" 2>&1
  swarm wait --timeout 30 > "$OUT/message.out" 2>&1
  sleep 30
  ;;
worker)
  idle
  IFS= read -r line
  working
  swarm terminal spawn --name dev -- sleep 60 > "$OUT/terminal.out" 2>&1
  ID=$(sed -n 's/^started terminal \([0-9]*\).*/\1/p' < "$OUT/terminal.out")
  swarm terminal kill "$ID" > "$OUT/kill.out" 2>&1
  sleep 30
  ;;
esac"#,
    );
    let repo = ported_repo("5170-5199", "");
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (_client, _task) = start_task(&daemon, &repo);

    let message = fixture.read_when_written("message.out");

    assert!(message.contains("stopped by worker api"), "{message}");
}

#[test]
fn a_burst_of_output_scrolls_past_without_losing_lines() {
    let fixture = fixture_with_helpers(
        r#"idle
IFS= read -r line
working
swarm terminal spawn --size small -- sh -c 'seq 1 500 | sed "s/^/line /"; sleep 60' > "$OUT/spawn.out" 2>&1
ID=$(sed -n 's/^started terminal \([0-9]*\).*/\1/p' < "$OUT/spawn.out")
swarm terminal wait "$ID" --output "line 250" --timeout 20 > "$OUT/wait.out" 2>&1
swarm terminal read "$ID" --lines 500 > "$OUT/read.out" 2>&1
sleep 30"#,
    );
    let repo = ported_repo("5170-5199", "");
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (_client, _task) = start_task(&daemon, &repo);

    let wait = fixture.read_when_written("wait.out");
    let read = fixture.read_when_written("read.out");

    assert_eq!(wait.trim(), "found: line 250");
    let lines: Vec<&str> = read.lines().collect();
    let expected: Vec<String> = (1..=500).map(|number| format!("line {number}")).collect();
    assert_eq!(lines, expected);
}

#[test]
fn a_burst_right_before_the_exit_keeps_every_line() {
    let fixture = fixture_with_helpers(
        r#"idle
IFS= read -r line
working
swarm terminal spawn --size small -- sh -c 'seq 1 500 | sed "s/^/line /"' > "$OUT/spawn.out" 2>&1
ID=$(sed -n 's/^started terminal \([0-9]*\).*/\1/p' < "$OUT/spawn.out")
swarm terminal wait "$ID" --exit --timeout 20 > "$OUT/wait.out" 2>&1
swarm terminal read "$ID" --lines 500 > "$OUT/read.out" 2>&1
sleep 30"#,
    );
    let repo = ported_repo("5200-5229", "");
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (_client, _task) = start_task(&daemon, &repo);

    fixture.read_when_written("wait.out");
    let read = fixture.read_when_written("read.out");

    let lines: Vec<&str> = read.lines().collect();
    let expected: Vec<String> = (1..=500).map(|number| format!("line {number}")).collect();
    assert_eq!(lines, expected);
}

/// The worker reports at once; the orchestrator is pasted the report once it is idle.
fn pasted_report_fixture() -> Fixture {
    fixture_with_helpers(
        r#"case "$SWARM_ROLE" in
orchestrator)
  idle
  IFS= read -r line
  working
  swarm spawn api --brief "report at once" > "$OUT/spawn.out" 2>&1
  while [ ! -f "$OUT/reported" ]; do sleep 0.1; done
  idle
  IFS= read -r header
  IFS= read -r body
  printf '%s\n%s\n' "$header" "$body" > "$OUT/pasted"
  sleep 30
  ;;
worker)
  idle
  IFS= read -r line
  working
  swarm report "all green" > "$OUT/report.out" 2>&1
  echo yes > "$OUT/reported"
  sleep 30
  ;;
esac"#,
    )
}

impl Client {
    fn list_messages(&mut self, project: ProjectId, task: Option<TaskId>) -> Vec<MessageInfo> {
        self.send(&Request::ListMessages {
            call: CALL,
            project,
            task,
        });

        self.wait_for(|event| match event {
            Event::Messages {
                call: CALL,
                messages,
            } => Some(messages.clone()),
            _ => None,
        })
    }
}

#[test]
fn a_delivered_message_is_announced_to_the_app() {
    let fixture = pasted_report_fixture();
    let repo = repo_for_merges();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (mut client, _task) = start_task(&daemon, &repo);

    let report = client.wait_for_message(|message| message.kind == MessageKind::Report);
    let (ids, delivered_at) = client.wait_for(|event| match event {
        Event::MessagesDelivered { ids, delivered_at } if ids.contains(&report.id) => {
            Some((ids.clone(), *delivered_at))
        }
        _ => None,
    });

    assert_eq!(report.delivered_at, None);
    assert_eq!(ids, vec![report.id]);
    assert!(delivered_at >= report.sent_at);
}

#[test]
fn listing_messages_returns_the_conversation_with_delivery_times() {
    let fixture = pasted_report_fixture();
    let repo = repo_for_merges();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (mut client, task) = start_task(&daemon, &repo);

    fixture.read_when_written("pasted");
    let report = client.wait_for_message(|message| message.kind == MessageKind::Report);
    client.wait_for(|event| match event {
        Event::MessagesDelivered { ids, .. } if ids.contains(&report.id) => Some(()),
        _ => None,
    });

    let messages = client.list_messages(task.project, Some(task.id));
    let kinds: Vec<MessageKind> = messages.iter().map(|message| message.kind).collect();

    assert_eq!(kinds, [MessageKind::Brief, MessageKind::Report]);
    assert!(messages.windows(2).all(|pair| pair[0].id < pair[1].id));
    assert!(
        messages
            .iter()
            .all(|message| message.delivered_at.is_some())
    );
    assert!(
        client.list_messages(task.project, None).is_empty(),
        "the Start orchestrator has no messages"
    );
    assert!(client.list_messages(ProjectId(999), None).is_empty());
}

#[test]
fn agent_changes_measure_a_worker_against_its_fork_point() {
    let fixture = fixture_with_helpers(
        r#"case "$SWARM_ROLE" in
orchestrator)
  idle
  IFS= read -r line
  working
  swarm spawn api --own-worktree "same files" --brief "write files" > "$OUT/spawn.out" 2>&1
  sleep 30
  ;;
worker)
  idle
  IFS= read -r line
  working
  printf 'one\n' > api.txt
  git add api.txt
  commit "add api"
  printf 'a\nb\n' > loose.txt
  echo yes > "$OUT/wrote"
  sleep 30
  ;;
esac"#,
    );
    let repo = repo_for_merges();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (mut client, _task) = start_task(&daemon, &repo);

    let worker = client.wait_for_agent(|agent| agent.role == AgentRole::Worker);
    fixture.read_when_written("wrote");
    client.send(&Request::AgentChanges {
        call: CALL,
        agents: vec![worker.id, AgentId(9999)],
    });
    let changes = client.wait_for(|event| match event {
        Event::AgentChanges {
            call: CALL,
            changes,
        } => Some(changes.clone()),
        _ => None,
    });

    assert_eq!(
        changes,
        vec![AgentChange {
            agent: worker.id,
            stat: DiffStat {
                files: 2,
                additions: 3,
                deletions: 0,
            },
            files: vec![
                FileStat {
                    path: "loose.txt".into(),
                    additions: 2,
                    deletions: 0,
                },
                FileStat {
                    path: "api.txt".into(),
                    additions: 1,
                    deletions: 0,
                },
            ],
        }]
    );
}

#[test]
fn workers_share_the_task_worktree_and_commit_their_own_files() {
    let fixture = fixture_with_helpers(
        r#"case "$SWARM_ROLE" in
orchestrator)
  idle
  IFS= read -r line
  working
  swarm spawn api --brief "write api.txt" > "$OUT/spawn-api.out" 2>&1
  swarm spawn ui --brief "write ui.txt" > "$OUT/spawn-ui.out" 2>&1
  swarm spawn lonely --own-worktree "  " --brief "x" > "$OUT/spawn-blank.out" 2>&1
  swarm wait --timeout 60 > "$OUT/report-1.out" 2>&1
  swarm wait --timeout 60 > "$OUT/report-2.out" 2>&1
  swarm merge api > "$OUT/merge.out" 2>&1
  echo $? > "$OUT/merge.code"
  swarm agents > "$OUT/agents.out" 2>&1
  echo done > "$OUT/orchestrator-done"
  sleep 30
  ;;
worker)
  idle
  IFS= read -r line
  working
  pwd -P > "$OUT/pwd-$SWARM_AGENT_ID"
  case "$line" in
  "write api.txt")
    echo api > api.txt
    swarm commit -m "feat: api" api.txt > "$OUT/commit-api.out" 2>&1
    swarm commit -m "bad" /etc/passwd > "$OUT/absolute.out" 2>&1
    swarm commit -m "bad" ../escape > "$OUT/dotdot.out" 2>&1
    swarm commit -m "bad" .git/config > "$OUT/dotgit.out" 2>&1
    ;;
  *)
    echo ui > ui.txt
    swarm commit -m "feat: ui" ui.txt > "$OUT/commit-ui.out" 2>&1
    ;;
  esac
  swarm report "done" > "$OUT/report-sent-$SWARM_AGENT_ID.out" 2>&1
  idle
  sleep 30
  ;;
esac"#,
    );
    let repo = repo_for_merges();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (_client, task) = start_task(&daemon, &repo);
    fixture.read_when_written("orchestrator-done");

    let said = |name: &str| fixture.read_when_written(name);
    assert!(said("spawn-api.out").contains(&format!("worktree: {}", task.worktree.display())));
    assert!(said("spawn-api.out").contains(&format!("branch: {}", task.branch)));
    assert!(said("spawn-blank.out").contains("state a reason for --own-worktree"));
    assert!(said("commit-api.out").starts_with("committed "));
    assert!(said("commit-ui.out").starts_with("committed "));
    assert!(said("absolute.out").contains("relative"));
    assert!(said("dotdot.out").contains("inside your worktree"));
    assert!(said("dotgit.out").contains(".git"));
    assert_eq!(said("merge.code").trim(), "1");
    assert!(said("merge.out").contains("works in the task worktree; nothing to merge"));

    let real_worktree = task.worktree.canonicalize().expect("real path");
    let working_in: Vec<PathBuf> = std::fs::read_dir(&fixture.out)
        .expect("out folder")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("pwd-"))
        .map(|entry| PathBuf::from(std::fs::read_to_string(entry.path()).expect("pwd").trim()))
        .collect();
    assert_eq!(working_in, [real_worktree.clone(), real_worktree]);
    assert!(!task.worktree.with_file_name("agent-task--api").exists());

    let mut commits: Vec<String> = git_output(
        &task.worktree,
        &[
            "log",
            "-2",
            "--format=%s|%an|%ae|%(trailers:key=Swarm-Agent,valueonly)",
        ],
    )
    .lines()
    .filter(|line| !line.is_empty())
    .map(str::to_owned)
    .collect();
    commits.sort();
    assert_eq!(
        commits,
        [
            "feat: api|api (Swarm)|test@example.com|api",
            "feat: ui|ui (Swarm)|test@example.com|ui",
        ]
    );
    assert!(task.worktree.join("api.txt").is_file());
    assert!(task.worktree.join("ui.txt").is_file());
}

impl Client {
    /// The diff of a task, or the daemon's error text.
    fn task_diff(
        &mut self,
        task: TaskId,
        source: ReviewSource,
        known: Option<u64>,
    ) -> Result<Option<TaskDiff>, String> {
        self.send(&Request::TaskDiff {
            call: CALL,
            task,
            source,
            known,
        });

        self.wait_for(|event| match event {
            Event::TaskDiff { call: CALL, diff } => Some(Ok(diff.clone())),
            Event::Error {
                call: Some(CALL),
                message,
                ..
            } => Some(Err(message.clone())),
            _ => None,
        })
    }

    fn check_close(&mut self, task: TaskId) -> CloseCheck {
        self.send(&Request::CheckClose { call: CALL, task });

        self.wait_for(|event| match event {
            Event::CloseCheck { call: CALL, check } => Some(check.clone()),
            _ => None,
        })
    }

    /// Sends a request that ends with `Done`. Returns every event up to it, or the
    /// error text.
    fn until_done(&mut self, request: &Request) -> Result<Vec<Event>, String> {
        self.send(request);

        let mut seen = Vec::new();
        loop {
            match self.next() {
                Event::Done { call: CALL } => return Ok(seen),
                Event::Error {
                    call: Some(CALL),
                    message,
                    ..
                } => return Err(message),
                other => seen.push(other),
            }
        }
    }

    /// Closes the task with the check the user would just have seen.
    fn close_task(&mut self, task: TaskId) -> Result<Vec<Event>, String> {
        let confirmed = self.check_close(task);

        self.close_task_confirmed(task, confirmed)
    }

    fn close_task_confirmed(
        &mut self,
        task: TaskId,
        confirmed: CloseCheck,
    ) -> Result<Vec<Event>, String> {
        self.until_done(&Request::CloseTask {
            call: CALL,
            task,
            confirmed,
        })
    }

    /// A user terminal that belongs to the task.
    fn create_task_terminal(&mut self, task: TaskId, command: &[&str]) -> TerminalId {
        self.send(&Request::Create {
            call: CALL,
            terminal: CreateTerminal {
                kind: TerminalKind::User,
                command: Some(command.iter().map(|part| (*part).to_owned()).collect()),
                cwd: None,
                size: Size {
                    cols: 80,
                    rows: 24,
                    cell_width: 8,
                    cell_height: 16,
                },
                palette: Palette {
                    fg: Rgb(230, 230, 230),
                    bg: Rgb(10, 10, 12),
                    cursor: Rgb(255, 255, 255),
                    ansi: [Rgb(120, 120, 120); 16],
                },
                task: Some(task.terminal_key()),
            },
        });

        self.wait_for(|event| match event {
            Event::Created { call: CALL, id } => Some(*id),
            _ => None,
        })
    }
}

fn commit_all(folder: &Path) {
    git_output(folder, &["add", "-A"]);
    git_output(
        folder,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "-m",
            "work",
        ],
    );
}

/// A task without agents, in a repository with a file to review.
fn reviewed_task(daemon: &Daemon, repo: &Repo) -> (Client, TaskInfo) {
    let mut client = daemon.connect();
    client.hello();
    let project = client.add_project(&repo.root);
    let mut request = orchestrator_task(project.id, false);
    request.orchestrator = None;
    let task = client.create_task(request);
    std::fs::write(task.worktree.join("review.txt"), "one\ntwo\n").expect("file");
    commit_all(&task.worktree);

    (client, task)
}

#[test]
fn a_task_diff_has_files_and_answers_none_for_a_known_fingerprint() {
    let repo = Repo::new("");
    let daemon = Daemon::start();
    let (mut client, task) = reviewed_task(&daemon, &repo);

    let diff = client
        .task_diff(task.id, ReviewSource::Merged, None)
        .expect("diff")
        .expect("a diff");

    assert_eq!(diff.task, task.id);
    assert_eq!(diff.source, ReviewSource::Merged);
    assert_eq!(diff.files.len(), 1);
    assert_eq!(diff.files[0].path, Path::new("review.txt"));
    assert_eq!(diff.files[0].additions, 2);
    assert_eq!(
        client
            .task_diff(task.id, ReviewSource::Merged, Some(diff.fingerprint))
            .expect("known"),
        None
    );
    let live = client
        .task_diff(task.id, ReviewSource::Live, Some(diff.fingerprint))
        .expect("live")
        .expect("another source has another fingerprint");
    assert_eq!(live.source, ReviewSource::Live);
    assert!(
        client
            .task_diff(TaskId(999), ReviewSource::Merged, None)
            .is_err()
    );

    std::fs::write(task.worktree.join("review.txt"), "one\nthree\n").expect("file");
    commit_all(&task.worktree);
    let changed = client
        .task_diff(task.id, ReviewSource::Merged, Some(diff.fingerprint))
        .expect("changed")
        .expect("the diff changed");
    assert_ne!(changed.fingerprint, diff.fingerprint);
}

#[test]
fn a_second_diff_request_is_busy_while_the_first_runs() {
    let repo = Repo::new("");
    repo.commit_many_files(300);
    let daemon = Daemon::start();
    let (mut client, task) = reviewed_task(&daemon, &repo);
    for number in 0..300 {
        std::fs::write(task.worktree.join(format!("new-{number}.txt")), "x\n").expect("file");
    }

    for call in [CallId(1), CallId(2)] {
        client.send(&Request::TaskDiff {
            call,
            task: task.id,
            source: ReviewSource::Live,
            known: None,
        });
    }

    let (mut diffs, mut busy) = (0, 0);
    while diffs + busy < 2 {
        match client.next() {
            Event::TaskDiff { call, .. } => {
                assert_eq!(call, CallId(1));
                diffs += 1;
            }
            Event::Error {
                call: Some(CallId(2)),
                message,
                ..
            } => {
                assert!(message.starts_with("busy"), "{message}");
                busy += 1;
            }
            _ => {}
        }
    }
    assert_eq!((diffs, busy), (1, 1));

    let after = client.task_diff(task.id, ReviewSource::Live, None);
    assert!(after.is_ok(), "the guard was released");
}

#[test]
fn the_live_and_merged_diffs_of_a_task_run_side_by_side() {
    let repo = Repo::new("");
    repo.commit_many_files(300);
    let daemon = Daemon::start();
    let (mut client, task) = reviewed_task(&daemon, &repo);
    for number in 0..300 {
        std::fs::write(task.worktree.join(format!("new-{number}.txt")), "x\n").expect("file");
    }

    for (call, source) in [
        (CallId(1), ReviewSource::Live),
        (CallId(2), ReviewSource::Merged),
    ] {
        client.send(&Request::TaskDiff {
            call,
            task: task.id,
            source,
            known: None,
        });
    }

    let mut answered = Vec::new();
    while answered.len() < 2 {
        match client.next() {
            Event::TaskDiff { call, .. } => answered.push(call),
            Event::Error { message, .. } => panic!("refused: {message}"),
            _ => {}
        }
    }
    answered.sort_by_key(|call| call.0);
    assert_eq!(answered, [CallId(1), CallId(2)]);
}

#[test]
fn approving_needs_the_current_fingerprint_and_reaches_every_client() {
    let repo = Repo::new("");
    let daemon = Daemon::start();
    let (mut client, task) = reviewed_task(&daemon, &repo);
    let mut watcher = daemon.connect();
    watcher.hello();
    let fingerprint = client
        .task_diff(task.id, ReviewSource::Merged, None)
        .expect("diff")
        .expect("a diff")
        .fingerprint;

    let stale = client.until_done(&Request::ApproveTask {
        call: CALL,
        task: task.id,
        fingerprint: fingerprint ^ 1,
    });
    assert_eq!(
        stale.expect_err("refused"),
        "the changes changed since you reviewed them"
    );

    let events = client
        .until_done(&Request::ApproveTask {
            call: CALL,
            task: task.id,
            fingerprint,
        })
        .expect("approved");
    let own = events.iter().find_map(|event| match event {
        Event::TaskChanged { task } => Some(task.clone()),
        _ => None,
    });
    assert_eq!(
        own.expect("TaskChanged before Done").approved,
        Some(fingerprint)
    );
    let seen = watcher.wait_for(|event| match event {
        Event::TaskChanged { task } => Some(task.clone()),
        _ => None,
    });
    assert_eq!(seen.approved, Some(fingerprint));
    assert_eq!(
        client.projects()[0].tasks[0].approved,
        Some(fingerprint),
        "the approval is stored"
    );
}

const CLOSE_AGENT: &str = r#"case "$SWARM_ROLE" in
orchestrator)
  idle
  IFS= read -r line
  working
  swarm spawn api --brief "wait" > "$OUT/spawn.out" 2>&1
  swarm terminal spawn --name dev -- sh -c 'echo $$ > "$1"; exec sleep 300' sh "$OUT/pid" > "$OUT/terminal.out" 2>&1
  until [ -s "$OUT/pid" ]; do sleep 0.1; done
  echo ready > "$OUT/ready"
  sleep 300
  ;;
worker)
  idle
  IFS= read -r line
  working
  sleep 300
  ;;
esac"#;

fn process_alive(pid: i32) -> bool {
    // SAFETY: kill with signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[test]
fn closing_a_task_stops_everything_removes_worktrees_and_survives_a_restart() {
    let fixture = fixture_with_helpers(CLOSE_AGENT);
    let repo = ported_repo("5170-5175", "");
    repo.git(&["branch", "-q", "unrelated"]);
    let mut daemon = Daemon::start_with_settings(&fixture.settings());
    let (mut client, task) = start_task(&daemon, &repo);
    fixture.read_when_written("ready");
    let worker = client.wait_for_agent(|agent| {
        agent.role == AgentRole::Worker && agent.status == AgentStatus::Working
    });
    let pid: i32 = fixture
        .read_when_written("pid")
        .trim()
        .parse()
        .expect("pid");
    std::fs::write(task.worktree.join("loose.txt"), "uncommitted\n").expect("file");
    let user_pid_file = fixture.out.join("user-pid");
    let user_script = format!("echo $$ > {}; exec sleep 300", user_pid_file.display());
    client.create_task_terminal(task.id, &["/bin/sh", "-c", &user_script]);
    let user_pid: i32 = fixture
        .read_when_written("user-pid")
        .trim()
        .parse()
        .expect("pid");

    let check = client.check_close(task.id);
    assert_eq!(check.running_agents, 2);
    assert_eq!(check.user_terminals, 1);
    assert_eq!(check.uncommitted_files, 1);
    assert!(check.unmerged_workers.is_empty());
    assert!(process_alive(pid));

    let events = client.close_task(task.id).expect("closed");

    assert!(!process_alive(pid), "the process terminal was stopped");
    assert!(!process_alive(user_pid), "the user terminal was stopped");
    let stopped: Vec<AgentId> = events
        .iter()
        .filter_map(|event| match event {
            Event::AgentChanged { agent } if agent.lifecycle == AgentLifecycle::Stopped => {
                Some(agent.id)
            }
            _ => None,
        })
        .collect();
    assert!(stopped.contains(&task.agents[0].id), "{stopped:?}");
    assert!(stopped.contains(&worker.id), "{stopped:?}");
    let closed_at = events
        .iter()
        .position(|event| matches!(event, Event::TaskChanged { task } if task.closed))
        .expect("TaskChanged with closed");
    let last_stop = events
        .iter()
        .rposition(|event| matches!(event, Event::AgentChanged { .. }))
        .expect("agent change");
    assert!(
        last_stop < closed_at,
        "agents are announced before the task"
    );

    assert!(!task.worktree.exists());
    let branches = git_output(&repo.root, &["branch", "--format=%(refname:short)"]);
    assert!(
        !branches.lines().any(|branch| branch == task.branch),
        "the task branch has no commits of its own, so it is merged and deleted: {branches}"
    );
    assert!(branches.lines().any(|branch| branch == "unrelated"));
    assert!(client.projects()[0].tasks.is_empty());
    let messages = client.list_messages(task.project, Some(task.id));
    assert!(
        messages.iter().all(|message| !matches!(
            message.kind,
            MessageKind::WorkerStopped | MessageKind::TerminalExited
        )),
        "a close tells nobody: {messages:?}"
    );
    assert!(
        client
            .close_task_confirmed(task.id, CloseCheck::default())
            .is_err(),
        "closed already"
    );
    assert!(
        client
            .task_diff(task.id, ReviewSource::Merged, None)
            .is_err(),
        "a closed task is unknown"
    );

    let ports_free = client.create_task(CreateTask {
        orchestrator: None,
        ..orchestrator_task(task.project, false)
    });
    assert_eq!(ports_free.ports, task.ports, "the ports came back");
    drop(client);

    daemon.crash();
    let restarted = Daemon::start_in(daemon.home.clone());
    let mut client = restarted.connect();
    client.hello();
    let tasks = &client.projects()[0].tasks;
    assert_eq!(tasks.len(), 1, "only the new task is listed");
    assert_ne!(tasks[0].id, task.id);
    for agent in [task.agents[0].id, worker.id] {
        let stored = database(&restarted)
            .agent(agent)
            .expect("read")
            .expect("row");
        assert_eq!(stored.info.lifecycle, AgentLifecycle::Stopped);
    }
}

#[test]
fn a_stubborn_process_is_killed_and_nothing_new_starts_while_the_task_closes() {
    let fixture = fixture_with_helpers(
        r#"idle
IFS= read -r line
working
trap 'swarm spawn late --brief "too late" > "$OUT/late-spawn.out" 2>&1; swarm terminal spawn -- sleep 1 > "$OUT/late-terminal.out" 2>&1; echo 1 > "$OUT/trapped"' TERM
echo ready > "$OUT/ready"
while :; do sleep 0.2; done"#,
    );
    let repo = ported_repo("5170-5175", "");
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (mut client, task) = start_task(&daemon, &repo);
    fixture.read_when_written("ready");
    let started = Instant::now();

    let events = client.close_task(task.id).expect("closed");

    assert!(
        started.elapsed() >= Duration::from_secs(4),
        "SIGTERM was ignored"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        Event::AgentChanged { agent }
            if agent.id == task.agents[0].id && agent.lifecycle == AgentLifecycle::Stopped
    )));
    assert!(
        fixture
            .read_when_written("late-spawn.out")
            .contains("the task is closing")
    );
    assert!(
        fixture
            .read_when_written("late-terminal.out")
            .contains("the task is closing")
    );
    assert!(
        client
            .terminals()
            .iter()
            .all(|terminal| terminal.task.as_deref() != Some(task.id.terminal_key().as_str()))
    );
}

#[test]
fn a_restart_does_not_resume_agents_of_a_task_that_was_closed_mid_run() {
    let fixture = Fixture::new(
        r#"printf '\033]7501;state=working\007'
printf '{"hook_event_name":"SessionStart","session_id":"hook-session-3"}' | "$SWARM" hook
sleep 300"#,
    );
    let repo = Repo::new("");
    let mut daemon = Daemon::start_with_settings(&fixture.settings());
    let mut client = daemon.connect();
    client.hello();
    let project = client.add_project(&repo.root);
    let task = client.create_task(orchestrator_task(project.id, false));
    let agent = task.agents[0].id;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !database(&daemon)
        .agent(agent)
        .expect("read")
        .expect("row")
        .session_confirmed
    {
        assert!(Instant::now() < deadline, "the session was never confirmed");
        thread::park_timeout(Duration::from_millis(30));
    }
    drop(client);

    daemon.crash();
    {
        // A crash in the middle of a close: the row says closed, the agent still runs.
        let connection = rusqlite::Connection::open(daemon.home.join("swarm.db")).expect("open");
        connection
            .execute("UPDATE tasks SET closed_at = 1", [])
            .expect("close");
    }
    let restarted = Daemon::start_in(daemon.home.clone());

    let stored = database(&restarted)
        .agent(agent)
        .expect("read")
        .expect("row");
    assert_eq!(stored.info.lifecycle, AgentLifecycle::Stopped);
}

#[test]
fn closing_refuses_when_more_would_be_lost_than_shown_and_a_retry_works() {
    let repo = Repo::new("");
    let daemon = Daemon::start();
    let (mut client, task) = reviewed_task(&daemon, &repo);
    let shown = client.check_close(task.id);
    std::fs::write(task.worktree.join("late.txt"), "new\n").expect("file");

    let refused = client.close_task_confirmed(task.id, shown);

    assert_eq!(
        refused.expect_err("refused"),
        "more would be lost than shown; check again"
    );
    assert!(task.worktree.exists());
    assert_eq!(client.projects()[0].tasks.len(), 1);

    client
        .close_task(task.id)
        .expect("closed after a new check");
    assert!(!task.worktree.exists());
}

#[test]
fn a_restart_finishes_a_close_that_stopped_half_way() {
    let fixture = Fixture::new(
        r#"printf '\033]7501;state=working\007'
printf '{"hook_event_name":"SessionStart","session_id":"hook-session-4"}' | "$SWARM" hook
sleep 300"#,
    );
    let repo = Repo::new("");
    let mut daemon = Daemon::start_with_settings(&fixture.settings());
    let mut client = daemon.connect();
    client.hello();
    let project = client.add_project(&repo.root);
    let task = client.create_task(orchestrator_task(project.id, false));
    let agent = task.agents[0].id;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !database(&daemon)
        .agent(agent)
        .expect("read")
        .expect("row")
        .session_confirmed
    {
        assert!(Instant::now() < deadline, "the session was never confirmed");
        thread::park_timeout(Duration::from_millis(30));
    }
    drop(client);

    daemon.crash();
    {
        let connection = rusqlite::Connection::open(daemon.home.join("swarm.db")).expect("open");
        connection
            .execute("UPDATE tasks SET closing_at = 1", [])
            .expect("closing");
    }
    let restarted = Daemon::start_in(daemon.home.clone());
    let mut client = restarted.connect();
    client.hello();

    let deadline = Instant::now() + Duration::from_secs(15);
    while task.worktree.exists() || !client.projects()[0].tasks.is_empty() {
        assert!(Instant::now() < deadline, "the close was never finished");
        thread::park_timeout(Duration::from_millis(50));
    }
    let stored = database(&restarted)
        .agent(agent)
        .expect("read")
        .expect("row");
    assert_eq!(
        stored.info.lifecycle,
        AgentLifecycle::Stopped,
        "not resumed"
    );
}

fn comment_at(
    task: &TaskInfo,
    source: ReviewSource,
    line: u32,
    text: &str,
    body: &str,
) -> swarm_protocol::NewComment {
    swarm_protocol::NewComment {
        task: task.id,
        parent: None,
        source,
        anchor: swarm_protocol::CommentAnchor {
            worktree: task.worktree.clone(),
            path: PathBuf::from("api.txt"),
            side: swarm_protocol::DiffSide::New,
            line,
            text: text.to_owned(),
            before: None,
            after: None,
        },
        body: body.to_owned(),
    }
}

fn changed(events: &[Event]) -> Vec<swarm_protocol::ReviewComment> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::CommentChanged { comment } => Some(comment.clone()),
            _ => None,
        })
        .collect()
}

fn comment_messages(events: &[Event]) -> Vec<MessageInfo> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::MessageSent { message } if message.kind == MessageKind::Comment => {
                Some(message.clone())
            }
            _ => None,
        })
        .collect()
}

impl Client {
    fn list_comments(&mut self, task: TaskId) -> Vec<swarm_protocol::ReviewComment> {
        self.send(&Request::ListComments { call: CALL, task });

        self.wait_for(|event| match event {
            Event::Comments {
                call: CALL,
                comments,
            } => Some(comments.clone()),
            _ => None,
        })
    }

    fn add_comment(&mut self, comment: swarm_protocol::NewComment) -> Result<Vec<Event>, String> {
        self.until_done(&Request::AddComment {
            call: CALL,
            comment,
        })
    }
}

const COMMENT_ORCHESTRATOR: &str = r#"case "$SWARM_ROLE" in
orchestrator)
  printf '\033]7501;state=idle\007'
  IFS= read -r line
  printf '\033]7501;state=working\007'
  swarm confirm > "$OUT/confirm.out" 2>&1
  swarm spawn api --brief "build the api" > "$OUT/spawn.out" 2>&1
  swarm wait --timeout 60 > "$OUT/report.out" 2>&1
  printf '\033]7501;state=idle\007'
  while IFS= read -r l; do printf '%s\n' "$l" >> "$OUT/inbox"; done
  ;;
worker)
  printf '\033]7501;state=idle\007'
  IFS= read -r line
  printf '\033]7501;state=working\007'
  printf 'one\ntwo\n' > api.txt
  swarm commit -m "feat: api" api.txt > "$OUT/commit.out" 2>&1
  swarm report "done" > "$OUT/report-sent.out" 2>&1
  printf '\033]7501;state=idle\007'
  sleep 30
  ;;
esac"#;

#[test]
fn a_live_comment_reaches_the_orchestrator_and_names_the_worker() {
    let fixture = Fixture::new(COMMENT_ORCHESTRATOR);
    let repo = repo_for_merges();
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let (mut client, task) = start_task(&daemon, &repo);
    let report = client.wait_for_message(|message| message.kind == MessageKind::Report);
    let worker = report.from.expect("the worker wrote the report");

    let events = client
        .add_comment(comment_at(&task, ReviewSource::Live, 2, "two", "Cache it."))
        .expect("added");

    let messages = comment_messages(&events);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].from, None);
    assert_eq!(messages[0].to, task.agents[0].id);
    let id = changed(&events)[0].id;
    assert_eq!(
        messages[0].text,
        format!("#{} for api · api.txt:2\n> two\nCache it.", id.0)
    );
    let sent = changed(&events);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].state, CommentState::Sent);
    assert_eq!(sent[0].to, Some(worker));
    assert!(sent[0].sent_at.is_some());

    let delivered = events.iter().any(|event| {
        matches!(event, Event::MessagesDelivered { ids, .. } if ids.contains(&messages[0].id))
    });
    if !delivered {
        client.wait_for(|event| match event {
            Event::MessagesDelivered { ids, .. } if ids.contains(&messages[0].id) => Some(()),
            _ => None,
        });
    }
    let inbox = fixture.read_when("inbox", |text| text.contains("Cache it."));
    assert!(inbox.contains("Cache it."), "{inbox}");

    assert_eq!(client.list_comments(task.id)[0].id, id);
}

#[test]
fn merged_comments_wait_and_are_sent_together() {
    let fixture = Fixture::new("sleep 60");
    let repo = Repo::new("");
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let mut client = daemon.connect();
    client.hello();
    let project = client.add_project(&repo.root);
    let task = client.create_task(orchestrator_task(project.id, false));
    std::fs::write(task.worktree.join("api.txt"), "one\ntwo\n").expect("file");
    commit_all(&task.worktree);

    let first = client
        .add_comment(comment_at(&task, ReviewSource::Merged, 1, "one", "A"))
        .expect("first");
    let second = client
        .add_comment(comment_at(&task, ReviewSource::Merged, 2, "two", "B"))
        .expect("second");

    assert!(comment_messages(&first).is_empty());
    assert_eq!(changed(&first)[0].state, CommentState::Pending);
    let (one, two) = (changed(&first)[0].id, changed(&second)[0].id);

    let reply = client
        .until_done(&Request::AddComment {
            call: CALL,
            comment: swarm_protocol::NewComment {
                parent: Some(one),
                ..comment_at(&task, ReviewSource::Live, 9, "ignored", "R")
            },
        })
        .expect("reply");
    assert!(comment_messages(&reply).is_empty(), "the parent waits");
    assert_eq!(changed(&reply)[0].anchor.text, "one");
    let reply_id = changed(&reply)[0].id;

    let events = client
        .until_done(&Request::SendComments {
            call: CALL,
            task: task.id,
        })
        .expect("sent");

    let messages = comment_messages(&events);
    assert_eq!(messages.len(), 1);
    assert!(
        messages[0].text.starts_with("3 comments:\n\n"),
        "{}",
        messages[0].text
    );
    assert_eq!(
        changed(&events)
            .iter()
            .map(|comment| comment.id)
            .collect::<Vec<_>>(),
        [one, two, reply_id]
    );
    assert!(
        changed(&events)
            .iter()
            .all(|comment| comment.state == CommentState::Sent)
    );
    assert!(
        client
            .until_done(&Request::SendComments {
                call: CALL,
                task: task.id,
            })
            .is_err(),
        "nothing waits"
    );

    let resolved = client
        .until_done(&Request::ResolveComment {
            call: CALL,
            id: one,
            resolved: true,
        })
        .expect("resolved");
    assert_eq!(changed(&resolved)[0].state, CommentState::Resolved);
    assert!(
        client
            .until_done(&Request::ResolveComment {
                call: CALL,
                id: reply_id,
                resolved: true,
            })
            .is_err()
    );
    assert!(
        client
            .until_done(&Request::DeleteComment {
                call: CALL,
                id: two,
            })
            .is_err(),
        "sent comments stay"
    );

    client.close_task(task.id).expect("closed");
    assert!(client.list_comments(task.id).is_empty());
}

#[test]
fn deleting_a_pending_comment_deletes_its_replies_and_tells_everyone() {
    let fixture = Fixture::new("sleep 60");
    let repo = Repo::new("");
    let daemon = Daemon::start_with_settings(&fixture.settings());
    let mut client = daemon.connect();
    client.hello();
    let mut watcher = daemon.connect();
    watcher.hello();
    let project = client.add_project(&repo.root);
    let task = client.create_task(orchestrator_task(project.id, false));
    let parent = changed(
        &client
            .add_comment(comment_at(&task, ReviewSource::Merged, 1, "one", "A"))
            .expect("parent"),
    )[0]
    .id;
    let reply = changed(
        &client
            .until_done(&Request::AddComment {
                call: CALL,
                comment: swarm_protocol::NewComment {
                    parent: Some(parent),
                    ..comment_at(&task, ReviewSource::Merged, 1, "one", "R")
                },
            })
            .expect("reply"),
    )[0]
    .id;

    client
        .until_done(&Request::DeleteComment {
            call: CALL,
            id: parent,
        })
        .expect("deleted");

    let mut gone: Vec<CommentId> = Vec::new();
    watcher.wait_for(|event| {
        if let Event::CommentDeleted {
            task: id,
            id: comment,
        } = event
        {
            assert_eq!(*id, task.id);
            gone.push(*comment);
        }

        (gone.len() == 2).then_some(())
    });
    assert_eq!(gone, [reply, parent]);
    assert!(client.list_comments(task.id).is_empty());
}
