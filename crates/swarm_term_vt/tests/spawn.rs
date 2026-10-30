use std::time::{Duration, Instant};

use swarm_term::{
    ExitStatus, Palette, ProgramState, Rgb, Row, Size, TerminalChannels, TerminalOutput, channels,
};
use swarm_term_vt::{DEFAULT_SCROLLBACK_BYTES, LocalTerminal, SpawnError, SpawnOptions};

fn options(command: &[&str]) -> SpawnOptions {
    SpawnOptions {
        command: Some(command.iter().map(|part| (*part).to_owned()).collect()),
        cwd: None,
        env: Vec::new(),
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
        scrollback_bytes: DEFAULT_SCROLLBACK_BYTES,
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
    line
}

/// Reads output until the process exits; returns the status and every row text seen.
fn run_to_exit(
    terminal: &TerminalChannels,
    timeout: Duration,
) -> Result<(ExitStatus, Vec<String>), String> {
    let deadline = Instant::now() + timeout;
    let mut texts = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let output = terminal
            .output
            .recv_timeout(remaining)
            .map_err(|error| format!("no exit within {timeout:?}: {error}"))?;
        match output {
            TerminalOutput::Snapshot(snapshot) => {
                texts.extend(snapshot.rows.iter().map(row_text));
            }
            TerminalOutput::Update(update) => {
                texts.extend(update.rows.iter().map(|(_, row)| row_text(row)));
            }
            TerminalOutput::Exited(status) => return Ok((status, texts)),
            _ => {}
        }
    }
}

#[test]
fn printf_output_reaches_the_view() {
    let (terminal, emulator) = channels();
    let _local = LocalTerminal::spawn(options(&["/bin/sh", "-c", "printf 'hello\\n'"]), emulator)
        .expect("spawn");

    let (status, texts) = run_to_exit(&terminal, Duration::from_secs(10)).expect("exit");

    assert_eq!(status, ExitStatus::Code(0));
    assert!(texts.iter().any(|text| text.contains("hello")), "{texts:?}");
}

#[test]
fn program_status_reports_arrive_in_order_before_exit() {
    let (terminal, emulator) = channels();
    let script =
        "printf '\\033]7501;state=working:app=test\\033\\\\'; printf '\\033]7501;state=done\\007'";
    let _local =
        LocalTerminal::spawn(options(&["/bin/sh", "-c", script]), emulator).expect("spawn");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = Vec::new();
    loop {
        match terminal.output.recv_deadline(deadline).expect("output") {
            TerminalOutput::ProgramStatus(status) => seen.push(status.state),
            TerminalOutput::Exited(_) => break,
            _ => {}
        }
    }

    assert_eq!(seen, vec![ProgramState::Working, ProgramState::Done]);
}

#[test]
fn exit_code_is_reported() {
    let (terminal, emulator) = channels();
    let _local =
        LocalTerminal::spawn(options(&["/bin/sh", "-c", "exit 7"]), emulator).expect("spawn");

    let (status, _) = run_to_exit(&terminal, Duration::from_secs(10)).expect("exit");

    assert_eq!(status, ExitStatus::Code(7));
}

#[test]
fn kill_ends_a_running_process() {
    let (terminal, emulator) = channels();
    let _local = LocalTerminal::spawn(options(&["/bin/sleep", "30"]), emulator).expect("spawn");

    terminal
        .input
        .send(swarm_term::TerminalInput::Kill)
        .expect("send");
    let (status, _) = run_to_exit(&terminal, Duration::from_secs(10)).expect("exit");

    assert!(matches!(status, ExitStatus::Signal(_)), "{status:?}");
}

/// Pids whose session id is `session`.
fn session_members(session: libc::pid_t) -> Vec<libc::pid_t> {
    // SAFETY: a null buffer with size 0 only asks for the number of processes.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let mut pids: Vec<libc::pid_t> = vec![0; usize::try_from(count).unwrap_or(0) + 64];
    let bytes = libc::c_int::try_from(pids.len() * size_of::<libc::pid_t>()).unwrap_or(0);
    // SAFETY: the buffer holds `bytes` bytes and lives for the whole call.
    let filled = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(usize::try_from(filled).unwrap_or(0));
    // SAFETY: getsid takes a plain integer.
    pids.retain(|pid| *pid > 0 && unsafe { libc::getsid(*pid) } == session);
    pids
}

#[test]
fn kill_ends_processes_that_ignore_sighup() {
    let (terminal, emulator) = channels();
    let local = LocalTerminal::spawn(
        options(&[
            "/bin/sh",
            "-c",
            "trap '' HUP; (trap '' HUP; sleep 30) & sleep 30",
        ]),
        emulator,
    )
    .expect("spawn");
    let session = libc::pid_t::try_from(local.pid().expect("pid")).expect("pid");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = session_members(session);
    while seen.len() < 3 && Instant::now() < deadline {
        std::thread::park_timeout(Duration::from_millis(50));
        seen = session_members(session);
    }
    assert!(seen.len() >= 3, "{seen:?}");

    terminal
        .input
        .send(swarm_term::TerminalInput::Kill)
        .expect("send");
    let (status, _) = run_to_exit(&terminal, Duration::from_secs(5)).expect("exit");

    let deadline = Instant::now() + Duration::from_secs(2);
    while !session_members(session).is_empty() && Instant::now() < deadline {
        std::thread::park_timeout(Duration::from_millis(50));
    }
    assert!(matches!(status, ExitStatus::Signal(_)), "{status:?}");
    assert_eq!(session_members(session), Vec::<libc::pid_t>::new());
}

#[test]
fn exit_is_reported_while_a_background_job_holds_the_pty() {
    let (terminal, emulator) = channels();
    let _local = LocalTerminal::spawn(options(&["/bin/sh", "-c", "sleep 5 & exit 0"]), emulator)
        .expect("spawn");

    let (status, _) = run_to_exit(&terminal, Duration::from_secs(2)).expect("exit");

    assert_eq!(status, ExitStatus::Code(0));
}

#[test]
fn missing_command_is_reported() {
    let (_terminal, emulator) = channels();

    let result = LocalTerminal::spawn(options(&["/nonexistent/program"]), emulator);

    assert!(matches!(result, Err(SpawnError::CommandNotFound(_))));
}

#[test]
fn dropping_the_view_ends_the_process() {
    let (terminal, emulator) = channels();
    let local = LocalTerminal::spawn(options(&["/bin/sleep", "30"]), emulator).expect("spawn");
    let output = terminal.output.clone();
    drop(terminal);

    drop(local);

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut exited = false;
    while let Ok(message) = output.recv_deadline(deadline) {
        if matches!(message, TerminalOutput::Exited(_)) {
            exited = true;
            break;
        }
    }
    assert!(exited);
}

#[test]
#[ignore = "speed test, run with --release"]
fn seq_200000_speed() {
    let (terminal, emulator) = channels();
    let start = Instant::now();
    let _local =
        LocalTerminal::spawn(options(&["/usr/bin/seq", "1", "200000"]), emulator).expect("spawn");

    let (status, _) = run_to_exit(&terminal, Duration::from_secs(60)).expect("exit");

    println!("seq 1 200000 took {:?}", start.elapsed());
    assert_eq!(status, ExitStatus::Code(0));
}

#[test]
fn snapshot_request_answers_with_the_current_screen() {
    let (terminal, emulator) = channels();
    let _local = LocalTerminal::spawn(
        options(&["/bin/sh", "-c", "printf 'hello'; sleep 30"]),
        emulator,
    )
    .expect("spawn");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen_text = false;
    while !seen_text {
        match terminal.output.recv_deadline(deadline).expect("output") {
            TerminalOutput::Update(update) => {
                seen_text = update
                    .rows
                    .iter()
                    .any(|(_, row)| row_text(row).contains("hello"));
            }
            TerminalOutput::Snapshot(snapshot) => {
                seen_text = snapshot
                    .rows
                    .iter()
                    .any(|row| row_text(row).contains("hello"));
            }
            _ => {}
        }
    }

    terminal
        .input
        .send(swarm_term::TerminalInput::Snapshot { history: 10 })
        .expect("send");
    let snapshot = loop {
        if let TerminalOutput::Snapshot(snapshot) =
            terminal.output.recv_deadline(deadline).expect("snapshot")
        {
            break snapshot;
        }
    };

    assert!(
        snapshot
            .rows
            .iter()
            .any(|row| row_text(row).contains("hello"))
    );
    terminal
        .input
        .send(swarm_term::TerminalInput::Kill)
        .expect("send");
}
