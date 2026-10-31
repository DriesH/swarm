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
    AgentId, AgentInfo, AgentKind, AgentLifecycle, AgentStatus, CallId, CreateTask, CreateTerminal,
    Event, ProjectInfo, Request, TaskInfo, TerminalId, TerminalInfo, TerminalKind, VERSION, read,
    write,
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
            project: swarm_protocol::ProjectId(42),
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
        let path = self.out.join(name);
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Ok(text) = std::fs::read_to_string(&path)
                && !text.is_empty()
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

fn orchestrator_task(project: swarm_protocol::ProjectId, run_setup: bool) -> CreateTask {
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
    let mut stream = UnixStream::connect(daemon.agent_socket()).expect("connect");
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .expect("timeout");
    stream.write_all(line).expect("write");
    stream.write_all(b"\n").expect("write");

    let mut answer = String::new();
    BufReader::new(stream)
        .read_line(&mut answer)
        .expect("answer");

    serde_json::from_str(&answer).expect("json answer")
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
            r#"{{"method":"hook","agent":{agent},"token":"wrong","event":{{"hook_event_name":"Stop"}}}}"#
        )
        .as_bytes(),
    );
    let unknown_agent = agent_request(
        &daemon,
        br#"{"method":"hook","agent":999,"token":"x","event":{}}"#,
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
fn a_missing_command_gives_a_short_message() {
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

    client.create_task(orchestrator_task(project.id, false));

    let failed = client.wait_for_agent(|agent| agent.status == AgentStatus::Error);
    assert_eq!(failed.message.as_deref(), Some("Command not found"));
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
}

#[test]
fn the_agent_socket_serves_at_most_64_connections() {
    let daemon = Daemon::start();
    let held: Vec<UnixStream> = (0..64)
        .map(|_| UnixStream::connect(daemon.agent_socket()).expect("connect"))
        .collect();

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
    let after = agent_request(
        &daemon,
        br#"{"method":"hook","agent":1,"token":"x","event":{}}"#,
    );
    assert_eq!(after["error"], "refused");
}

#[test]
fn the_agent_socket_accepts_lines_over_one_mebibyte() {
    let daemon = Daemon::start();
    let padding = "x".repeat(2 << 20);
    let line =
        format!(r#"{{"method":"hook","agent":1,"token":"x","event":{{"pad":"{padding}"}}}}"#);

    let answer = agent_request(&daemon, line.as_bytes());

    assert_eq!(answer["error"], "refused");
}
