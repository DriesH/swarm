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
    assert_eq!(request["params"]["event"]["session_id"], "s1");
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

/// A daemon stand-in: answers each connection with the next reply and returns the
/// requests it got.
fn fake_daemon(
    name: &str,
    replies: Vec<serde_json::Value>,
) -> (PathBuf, thread::JoinHandle<Vec<serde_json::Value>>) {
    let dir = temp_dir(name);
    let socket = dir.join("agent.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for reply in replies {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut line = String::new();
            BufReader::new(stream.try_clone().expect("clone"))
                .read_line(&mut line)
                .expect("line");
            requests.push(serde_json::from_str(&line).expect("json request"));
            writeln!(stream, "{reply}").expect("answer");
        }
        std::fs::remove_dir_all(dir).ok();

        requests
    });

    (socket, handle)
}

fn agent_command(socket: &std::path::Path) -> Command {
    let mut command = swarm();
    command
        .env("SWARM_SOCKET", socket)
        .env("SWARM_AGENT_ID", "3")
        .env("SWARM_TOKEN", "tok")
        .env("SWARM_ROLE", "orchestrator");

    command
}

fn run(mut command: Command, stdin: &str) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start swarm");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write");

    child.wait_with_output().expect("wait")
}

fn worker_json(id: u64, name: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "name": name, "role": "Worker", "kind": "ClaudeCode", "model": "sonnet",
        "status": "working", "message": null, "lifecycle": "Running",
        "branch": "swarm/login--api", "worktree": "/repo/.worktrees/login--api",
    })
}

#[test]
fn send_resolves_a_worker_name_and_sends_the_message() {
    let (socket, daemon) = fake_daemon(
        "send",
        vec![
            serde_json::json!({ "ok": true, "result": [worker_json(9, "api")] }),
            serde_json::json!({ "ok": true, "result": { "message": 41 } }),
        ],
    );
    let mut command = agent_command(&socket);
    command.args(["send", "api", "use", "port", "5173"]);

    let output = run(command, "");
    let requests = daemon.join().expect("daemon");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "sent (message 41)"
    );
    assert_eq!(requests[0]["method"], "agents");
    assert_eq!(requests[0]["agent"], 3);
    assert_eq!(requests[0]["token"], "tok");
    assert_eq!(requests[1]["method"], "send");
    assert_eq!(requests[1]["params"]["to"], 9);
    assert_eq!(requests[1]["params"]["text"], "use port 5173");
}

#[test]
fn spawn_reads_the_brief_from_stdin_and_prints_the_worker() {
    let (socket, daemon) = fake_daemon(
        "spawn",
        vec![serde_json::json!({ "ok": true, "result": worker_json(9, "api") })],
    );
    let mut command = agent_command(&socket);
    command.args(["spawn", "api", "--kind", "codex", "--model", "gpt-6"]);

    let output = run(command, "do the api\nthen test it\n");
    let requests = daemon.join().expect("daemon");

    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.starts_with("started worker api (agent 9)"), "{text}");
    assert!(text.contains("branch: swarm/login--api"), "{text}");
    assert_eq!(requests[0]["method"], "spawn");
    assert_eq!(requests[0]["params"]["name"], "api");
    assert_eq!(requests[0]["params"]["kind"], "Codex");
    assert_eq!(requests[0]["params"]["model"], "gpt-6");
    assert_eq!(requests[0]["params"]["brief"], "do the api\nthen test it\n");
}

#[test]
fn json_prints_the_raw_result() {
    let (socket, daemon) = fake_daemon(
        "json",
        vec![serde_json::json!({ "ok": true, "result": [worker_json(9, "api")] })],
    );
    let mut command = agent_command(&socket);
    command.args(["agents", "--json"]);

    let output = run(command, "");
    daemon.join().expect("daemon");

    let printed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json output");
    assert_eq!(printed[0]["name"], "api");
}

#[test]
fn a_refusal_goes_to_stderr_with_exit_code_one() {
    let (socket, daemon) = fake_daemon(
        "refused",
        vec![
            serde_json::json!({ "ok": false, "error": "workers can only message their orchestrator" }),
        ],
    );
    let mut command = agent_command(&socket);
    command.args(["ask", "why?"]);

    let output = run(command, "");
    daemon.join().expect("daemon");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        "swarm: workers can only message their orchestrator"
    );
}

#[test]
fn a_merge_conflict_prints_the_files_and_exits_with_one() {
    let (socket, daemon) = fake_daemon(
        "conflict",
        vec![serde_json::json!({
            "ok": true,
            "result": { "merged": false, "conflicts": ["README.md"], "output": "CONFLICT (content)" },
        })],
    );
    let mut command = agent_command(&socket);
    command.args(["merge", "9"]);

    let output = run(command, "");
    daemon.join().expect("daemon");

    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("conflict in 1 file:\n  README.md"), "{text}");
}

#[test]
fn a_usage_error_exits_with_two_and_names_the_problem() {
    let output = run(agent_command(std::path::Path::new("/tmp/none.sock")), "");
    assert_eq!(output.status.code(), Some(2));

    let mut command = agent_command(std::path::Path::new("/tmp/none.sock"));
    command.args(["send", "api"]);
    let output = run(command, "");

    assert_eq!(output.status.code(), Some(2));
    assert!(!output.stderr.is_empty());
}

#[test]
fn a_command_outside_an_agent_says_what_is_missing() {
    let mut command = swarm();
    command.arg("read");

    let output = run(command, "");

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("SWARM_SOCKET"));
}

#[test]
fn help_lists_every_command() {
    let output = swarm().arg("--help").output().expect("run");

    let text = String::from_utf8_lossy(&output.stdout);
    for command in [
        "spawn", "send", "ask", "report", "commit", "notify", "read", "wait", "merge", "agents",
        "confirm", "pr", "mcp", "hook",
    ] {
        assert!(text.contains(command), "{command} is missing:\n{text}");
    }
}

/// `swarm mcp` with a fake daemon: writes the lines, closes stdin, returns the replies.
fn mcp_session(
    socket: &std::path::Path,
    role: &str,
    lines: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    let mut command = agent_command(socket);
    command.env("SWARM_ROLE", role).arg("mcp");
    let input = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";

    let output = run(command, &input);
    assert!(output.status.success());

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("json line"))
        .collect()
}

#[test]
fn the_mcp_server_lists_the_tools_of_the_role_over_stdio() {
    let lines = [
        serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } } }),
        serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    ];

    let replies = mcp_session(std::path::Path::new("/tmp/none.sock"), "worker", &lines);

    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0]["result"]["protocolVersion"], "2025-06-18");
    let names: Vec<&str> = replies[1]["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(
        names,
        [
            "ask",
            "report",
            "commit",
            "read",
            "wait",
            "terminal_spawn",
            "terminal_resize",
            "terminal_read",
            "terminal_wait",
            "terminal_kill",
            "terminal_list",
        ]
    );
}

#[test]
fn an_mcp_tool_call_reaches_the_daemon_and_returns_text() {
    let (socket, daemon) = fake_daemon(
        "mcp-call",
        vec![serde_json::json!({ "ok": true, "result": { "message": 5 } })],
    );
    let lines = [
        serde_json::json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": { "name": "report", "arguments": { "text": "all done" } } }),
    ];

    let replies = mcp_session(&socket, "worker", &lines);
    let requests = daemon.join().expect("daemon");

    assert_eq!(requests[0]["method"], "report");
    assert_eq!(requests[0]["params"]["text"], "all done");
    assert_eq!(replies[0]["id"], 7);
    assert_eq!(replies[0]["result"]["isError"], false);
    assert_eq!(
        replies[0]["result"]["content"][0]["text"],
        "sent (message 5)"
    );
}

#[test]
fn commit_sends_the_message_and_paths_and_prints_the_commit() {
    let (socket, daemon) = fake_daemon(
        "commit",
        vec![serde_json::json!({ "ok": true, "result": { "commit": "abc123" } })],
    );
    let mut command = agent_command(&socket);
    command.args(["commit", "-m", "feat: api", "src/api.rs"]);

    let output = run(command, "");
    let requests = daemon.join().expect("daemon");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "committed abc123"
    );
    assert_eq!(requests[0]["method"], "commit");
    assert_eq!(requests[0]["params"]["message"], "feat: api");
    assert_eq!(
        requests[0]["params"]["paths"],
        serde_json::json!(["src/api.rs"])
    );
}

#[test]
fn spawn_sends_the_reason_for_its_own_worktree() {
    let worker = serde_json::json!({
        "id": 4, "name": "api", "role": "Worker", "kind": "ClaudeCode", "model": "sonnet",
        "status": "unknown", "message": null, "lifecycle": "Starting",
        "branch": "swarm/login--api", "worktree": "/repo/.worktrees/login--api",
    });
    let (socket, daemon) = fake_daemon(
        "spawn-own",
        vec![serde_json::json!({ "ok": true, "result": worker })],
    );
    let mut command = agent_command(&socket);
    command.args([
        "spawn",
        "api",
        "--brief",
        "go",
        "--own-worktree",
        "same files",
    ]);

    let output = run(command, "");
    let requests = daemon.join().expect("daemon");

    assert!(output.status.success(), "{output:?}");
    assert_eq!(requests[0]["method"], "spawn");
    assert_eq!(requests[0]["params"]["own_worktree"], "same files");
}

#[test]
fn pr_draft_sends_the_title_and_the_body_from_stdin() {
    let (socket, daemon) = fake_daemon(
        "pr-draft",
        vec![serde_json::json!({ "ok": true, "result": null })],
    );
    let mut command = agent_command(&socket);
    command.args(["pr", "draft", "--title", "Add login", "--body-file", "-"]);

    let output = run(command, "## What\n\n- login\n");
    let requests = daemon.join().expect("daemon");

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "pull request draft sent"
    );
    assert_eq!(requests[0]["method"], "pr_draft");
    assert_eq!(requests[0]["agent"], 3);
    assert_eq!(requests[0]["params"]["title"], "Add login");
    assert_eq!(requests[0]["params"]["body"], "## What\n\n- login\n");
}

#[test]
fn a_refused_pr_draft_goes_to_stderr_with_exit_code_one() {
    let (socket, daemon) = fake_daemon(
        "pr-draft-refused",
        vec![serde_json::json!({ "ok": false, "error": "the task is not approved" })],
    );
    let mut command = agent_command(&socket);
    command.args(["pr", "draft", "--title", "T", "--body", "B"]);

    let output = run(command, "");
    daemon.join().expect("daemon");

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("the task is not approved"));
}

#[test]
fn an_mcp_pr_draft_sends_the_title_and_body() {
    let (socket, daemon) = fake_daemon(
        "mcp-pr-draft",
        vec![serde_json::json!({ "ok": true, "result": null })],
    );
    let lines = [
        serde_json::json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": "pr_draft", "arguments": {
            "title": "Add login", "body": "## What\n\n- login" } } }),
    ];

    let replies = mcp_session(&socket, "orchestrator", &lines);
    let requests = daemon.join().expect("daemon");

    assert_eq!(requests[0]["method"], "pr_draft");
    assert_eq!(requests[0]["params"]["title"], "Add login");
    assert_eq!(requests[0]["params"]["body"], "## What\n\n- login");
    assert_eq!(replies[0]["result"]["isError"], false);
    assert_eq!(
        replies[0]["result"]["content"][0]["text"],
        "pull request draft sent"
    );
}

#[test]
fn a_worker_does_not_get_the_pr_draft_tool() {
    let lines = [
        serde_json::json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": "pr_draft", "arguments": { "title": "T", "body": "B" } } }),
    ];

    let replies = mcp_session(std::path::Path::new("/tmp/none.sock"), "worker", &lines);

    assert!(
        replies[0]["error"]["message"]
            .as_str()
            .expect("message")
            .contains("pr_draft")
    );
}

#[test]
fn an_mcp_commit_sends_the_paths_to_the_daemon() {
    let (socket, daemon) = fake_daemon(
        "mcp-commit",
        vec![serde_json::json!({ "ok": true, "result": { "commit": "abc123" } })],
    );
    let lines = [
        serde_json::json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": "commit", "arguments": {
            "message": "feat: api", "paths": ["src/api.rs", "src/lib.rs"] } } }),
    ];

    let replies = mcp_session(&socket, "worker", &lines);
    let requests = daemon.join().expect("daemon");

    assert_eq!(requests[0]["method"], "commit");
    assert_eq!(
        requests[0]["params"]["paths"],
        serde_json::json!(["src/api.rs", "src/lib.rs"])
    );
    assert_eq!(replies[0]["result"]["isError"], false);
    assert_eq!(
        replies[0]["result"]["content"][0]["text"],
        "committed abc123"
    );
}

#[test]
fn an_mcp_spawn_passes_own_worktree_on() {
    let worker = serde_json::json!({
        "id": 4, "name": "api", "role": "Worker", "kind": "ClaudeCode", "model": "sonnet",
        "status": "unknown", "message": null, "lifecycle": "Starting",
        "branch": "swarm/login--api", "worktree": "/repo/.worktrees/login--api",
    });
    let (socket, daemon) = fake_daemon(
        "mcp-spawn-own",
        vec![serde_json::json!({ "ok": true, "result": worker })],
    );
    let lines = [
        serde_json::json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": "spawn", "arguments": {
            "name": "api", "brief": "go", "own_worktree": "an experiment" } } }),
    ];

    mcp_session(&socket, "orchestrator", &lines);
    let requests = daemon.join().expect("daemon");

    assert_eq!(requests[0]["params"]["own_worktree"], "an experiment");
}

#[test]
fn an_mcp_tool_call_that_the_daemon_refuses_is_an_error_result() {
    let (socket, daemon) = fake_daemon(
        "mcp-refused",
        vec![
            serde_json::json!({ "ok": false, "error": "only a task's orchestrator can merge a worker" }),
        ],
    );
    let lines = [
        serde_json::json!({ "jsonrpc": "2.0", "id": 8, "method": "tools/call",
        "params": { "name": "merge", "arguments": { "worker": 4 } } }),
    ];

    let replies = mcp_session(&socket, "orchestrator", &lines);
    daemon.join().expect("daemon");

    assert_eq!(replies[0]["result"]["isError"], true);
    assert_eq!(
        replies[0]["result"]["content"][0]["text"],
        "only a task's orchestrator can merge a worker"
    );
}

fn terminal_json(id: u64, running: bool) -> serde_json::Value {
    serde_json::json!({
        "id": id, "name": "dev server", "command": ["pnpm", "dev"], "owner": 3,
        "size": { "cols": 100, "rows": 24 }, "running": running,
        "exit": if running { serde_json::Value::Null } else { "code 0".into() },
    })
}

#[test]
fn terminal_spawn_sends_the_command_and_prints_the_id() {
    let (socket, daemon) = fake_daemon(
        "terminal-spawn",
        vec![serde_json::json!({ "ok": true, "result": terminal_json(7, true) })],
    );
    let mut command = agent_command(&socket);
    command.args([
        "terminal",
        "spawn",
        "--name",
        "dev server",
        "--size",
        "medium",
        "--",
        "pnpm",
        "dev",
    ]);

    let output = run(command, "");
    let requests = daemon.join().expect("daemon");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "started terminal 7 (dev server)\nsize: 100x24"
    );
    assert_eq!(requests[0]["method"], "terminal_spawn");
    assert_eq!(
        requests[0]["params"]["command"],
        serde_json::json!(["pnpm", "dev"])
    );
    assert_eq!(requests[0]["params"]["size"], "medium");
    assert_eq!(requests[0]["params"]["name"], "dev server");
}

#[test]
fn terminal_wait_that_times_out_exits_with_one() {
    let (socket, daemon) = fake_daemon(
        "terminal-wait",
        vec![serde_json::json!({ "ok": true, "result": {
            "reached": false, "terminal": terminal_json(7, true), "line": null,
        } })],
    );
    let mut command = agent_command(&socket);
    command.args([
        "terminal",
        "wait",
        "7",
        "--output",
        "ready in",
        "--timeout",
        "1",
    ]);

    let output = run(command, "");
    let requests = daemon.join().expect("daemon");

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "timed out");
    assert_eq!(
        requests[0]["params"]["until"],
        serde_json::json!({ "output": "ready in" })
    );
    assert_eq!(requests[0]["params"]["timeout_secs"], 1);
}

#[test]
fn task_wait_lists_the_tasks_and_task_create_reads_the_brief_from_stdin() {
    let task = serde_json::json!({
        "id": 5, "name": "Login", "branch": "swarm/login", "briefed": true,
        "orchestrator": worker_json(9, "orchestrator"),
    });
    let (socket, daemon) = fake_daemon(
        "tasks",
        vec![
            serde_json::json!({ "ok": true, "result": task }),
            serde_json::json!({ "ok": true, "result": [task] }),
        ],
    );
    let mut create = agent_command(&socket);
    create.args(["task", "create", "Login"]);
    let created = run(create, "build login\n");
    let mut wait = agent_command(&socket);
    wait.args(["task", "wait", "5", "--until", "briefed"]);
    let waited = run(wait, "");
    let requests = daemon.join().expect("daemon");

    assert!(created.status.success());
    assert!(
        String::from_utf8_lossy(&created.stdout)
            .starts_with("created task 5 (Login)\nbranch: swarm/login")
    );
    assert_eq!(requests[0]["params"]["brief"], "build login\n");
    assert!(waited.status.success());
    assert!(String::from_utf8_lossy(&waited.stdout).contains("5  Login  swarm/login  briefed"));
    assert_eq!(requests[1]["params"]["until"], "briefed");
    assert_eq!(requests[1]["params"]["ids"], serde_json::json!([5]));
}

#[test]
fn the_mcp_server_exits_when_stdin_closes_during_a_wait() {
    let dir = temp_dir("mcp-eof");
    let socket = dir.join("agent.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let daemon = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut line = String::new();
        BufReader::new(stream.try_clone().expect("clone"))
            .read_line(&mut line)
            .expect("request");
        // Never answers. The client must hang up.
        let mut rest = Vec::new();
        std::io::Read::read_to_end(&mut stream, &mut rest).expect("until the hang-up");
    });
    let lines = [
        serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "wait", "arguments": { "timeout": 600 } } }),
    ];

    let start = std::time::Instant::now();
    mcp_session(&socket, "worker", &lines);

    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "{:?}",
        start.elapsed()
    );
    daemon.join().expect("the daemon saw the connection close");
    std::fs::remove_dir_all(dir).ok();
}
