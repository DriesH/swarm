//! Runs the real `swarm` binary.

#![expect(clippy::expect_used, reason = "test helpers panic on failure")]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::thread;

fn swarm() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_swarm"));
    command.env_remove("SWARM_SOCKET");
    command.env_remove("SWARM_AGENT_ID");
    command.env_remove("SWARM_TOKEN");

    command
}

fn run_hook(mut command: Command, input: &str) -> Output {
    let mut child = command
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start swarm");

    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("write the hook");

    child.wait_with_output().expect("wait")
}

fn temp_dir(name: &str) -> PathBuf {
    // macOS temp dirs are too long for a socket path.
    let dir = PathBuf::from(format!("/tmp/swc-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");

    dir
}

#[test]
fn help_lists_the_hook_command() {
    let output = swarm().arg("--help").output().expect("run");

    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(text.contains("hook"), "{text}");
}

#[test]
fn a_hook_without_a_socket_is_silent_and_succeeds() {
    let mut command = swarm();
    command
        .env("SWARM_SOCKET", "/tmp/swc-does-not-exist.sock")
        .env("SWARM_AGENT_ID", "1")
        .env("SWARM_TOKEN", "abc");

    let output = run_hook(command, r#"{"hook_event_name":"Stop"}"#);

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn a_hook_without_any_environment_is_silent_and_succeeds() {
    let output = run_hook(swarm(), r#"{"hook_event_name":"Stop"}"#);

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn a_hook_with_broken_json_is_silent_and_succeeds() {
    let output = run_hook(swarm(), "not json");

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn a_hook_sends_one_json_line_with_the_agent_and_token() {
    let dir = temp_dir("line");
    let socket = dir.join("agent.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut line = String::new();
        BufReader::new(stream.try_clone().expect("clone"))
            .read_line(&mut line)
            .expect("line");
        stream.write_all(b"{\"ok\":true}\n").expect("answer");

        line
    });

    let mut command = swarm();
    command
        .env("SWARM_SOCKET", &socket)
        .env("SWARM_AGENT_ID", "7")
        .env("SWARM_TOKEN", "secret");
    let output = run_hook(command, r#"{"hook_event_name":"Stop","session_id":"s1"}"#);
    let line = server.join().expect("server");
    std::fs::remove_dir_all(&dir).expect("cleanup");

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    let request: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert_eq!(request["method"], "hook");
    assert_eq!(request["agent"], 7);
    assert_eq!(request["token"], "secret");
    assert_eq!(request["event"]["session_id"], "s1");
}

fn padded_event(total: usize) -> String {
    let prefix = r#"{"hook_event_name":"Stop","pad":""#;
    let suffix = r#""}"#;
    let pad = "x".repeat(total - prefix.len() - suffix.len());

    format!("{prefix}{pad}{suffix}")
}

/// Runs the hook against a listener and returns the line it received, if any.
fn forwarded(name: &str, input: &str) -> Option<usize> {
    let dir = temp_dir(name);
    let socket = dir.join("agent.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().ok()?;
        let mut line = String::new();
        BufReader::new(stream.try_clone().ok()?)
            .read_line(&mut line)
            .ok()?;
        stream.write_all(b"{\"ok\":true}\n").ok();

        Some(line.len())
    });

    let mut command = swarm();
    command
        .env("SWARM_SOCKET", &socket)
        .env("SWARM_AGENT_ID", "7")
        .env("SWARM_TOKEN", "secret");
    let mut child = command
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start swarm");
    let mut stdin = child.stdin.take().expect("stdin");
    // The hook may stop reading early when the input is too big.
    stdin.write_all(input.as_bytes()).ok();
    drop(stdin);
    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    // Unblocks `accept` when the hook never connected.
    UnixStream::connect(&socket).ok();
    let received = server.join().expect("server");
    std::fs::remove_dir_all(&dir).expect("cleanup");

    received
}

#[test]
fn a_hook_event_of_two_mebibytes_is_forwarded() {
    let line = forwarded("big-ok", &padded_event(2 << 20)).expect("forwarded");

    assert!(line > 2 << 20);
}

#[test]
fn a_hook_event_over_two_mebibytes_is_dropped_silently() {
    let received = forwarded("big-drop", &padded_event((2 << 20) + 1));

    assert_eq!(received, Some(0), "the hook sent something");
}
