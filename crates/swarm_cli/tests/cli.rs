//! Runs the real `swarm` binary.

#![expect(clippy::expect_used, reason = "test helpers panic on failure")]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn swarm() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_swarm"));
    command.env_remove("SWARM_SOCKET");
    command.env_remove("SWARM_AGENT_ID");
    command.env_remove("SWARM_AGENT_NAME");
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
        "confirm", "pr", "tile", "mcp", "hook",
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
            "tile_grow",
            "tile_shrink",
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
fn tile_grow_resolves_a_worker_name_and_asks_for_a_large_tile() {
    let (socket, daemon) = fake_daemon(
        "tile-grow",
        vec![
            serde_json::json!({ "ok": true, "result": [worker_json(9, "api")] }),
            serde_json::json!({ "ok": true, "result": null }),
        ],
    );
    let mut command = agent_command(&socket);
    command.args(["tile", "grow", "api"]);

    let output = run(command, "");
    let requests = daemon.join().expect("daemon");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "asked for a large tile"
    );
    assert_eq!(requests[0]["method"], "agents");
    assert_eq!(requests[1]["method"], "tile_grow");
    assert_eq!(requests[1]["params"]["agent"], 9);
}

#[test]
fn tile_shrink_by_id_skips_the_name_lookup() {
    let (socket, daemon) = fake_daemon(
        "tile-shrink-id",
        vec![serde_json::json!({ "ok": true, "result": null })],
    );
    let mut command = agent_command(&socket);
    command.args(["tile", "shrink", "9"]);

    let output = run(command, "");
    let requests = daemon.join().expect("daemon");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "asked for a small tile"
    );
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["method"], "tile_shrink");
    assert_eq!(requests[0]["params"]["agent"], 9);
}

#[test]
fn a_worker_asks_for_its_own_tile_without_a_name() {
    let (socket, daemon) = fake_daemon(
        "tile-own",
        vec![serde_json::json!({ "ok": true, "result": null })],
    );
    let mut command = agent_command(&socket);
    command.env("SWARM_ROLE", "worker").args(["tile", "grow"]);

    let output = run(command, "");
    let requests = daemon.join().expect("daemon");

    assert!(output.status.success());
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["method"], "tile_grow");
    assert!(requests[0]["params"]["agent"].is_null());
}

#[test]
fn a_worker_names_itself_in_a_tile_request_without_a_lookup() {
    let (socket, daemon) = fake_daemon(
        "tile-own-name",
        vec![serde_json::json!({ "ok": true, "result": null })],
    );
    let mut command = agent_command(&socket);
    command
        .env("SWARM_ROLE", "worker")
        .env("SWARM_AGENT_NAME", "api")
        .args(["tile", "grow", "api"]);

    let output = run(command, "");
    let requests = daemon.join().expect("daemon");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(requests.len(), 1, "a worker cannot list workers");
    assert_eq!(requests[0]["method"], "tile_grow");
    assert_eq!(requests[0]["params"]["agent"], 3);
}

#[test]
fn a_worker_that_names_another_worker_is_still_refused() {
    let (socket, daemon) = fake_daemon(
        "tile-other-name",
        vec![serde_json::json!({
            "ok": false,
            "error": "only a task's orchestrator can list its workers"
        })],
    );
    let mut command = agent_command(&socket);
    command
        .env("SWARM_ROLE", "worker")
        .env("SWARM_AGENT_NAME", "api")
        .args(["tile", "grow", "ui"]);

    let output = run(command, "");
    let requests = daemon.join().expect("daemon");

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["method"], "agents");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        "swarm: only a task's orchestrator can list its workers"
    );
}

#[test]
fn a_refused_tile_request_goes_to_stderr_with_exit_code_one() {
    let (socket, daemon) = fake_daemon(
        "tile-refused",
        vec![serde_json::json!({ "ok": false, "error": "a worker can only resize its own tile" })],
    );
    let mut command = agent_command(&socket);
    command.args(["tile", "shrink", "12"]);

    let output = run(command, "");
    daemon.join().expect("daemon");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        "swarm: a worker can only resize its own tile"
    );
}

#[test]
fn an_mcp_tile_grow_passes_the_agent_id_on() {
    let (socket, daemon) = fake_daemon(
        "mcp-tile-grow",
        vec![serde_json::json!({ "ok": true, "result": null })],
    );
    let lines = [
        serde_json::json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": "tile_grow", "arguments": { "agent": 9 } } }),
    ];

    let replies = mcp_session(&socket, "orchestrator", &lines);
    let requests = daemon.join().expect("daemon");

    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["method"], "tile_grow");
    assert_eq!(requests[0]["params"]["agent"], 9);
    assert_eq!(replies[0]["result"]["isError"], false);
    assert_eq!(
        replies[0]["result"]["content"][0]["text"],
        "asked for a large tile"
    );
}

#[test]
fn an_mcp_tile_shrink_of_a_worker_names_no_agent() {
    let (socket, daemon) = fake_daemon(
        "mcp-tile-own",
        vec![serde_json::json!({ "ok": true, "result": null })],
    );
    let lines = [
        serde_json::json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": { "name": "tile_shrink", "arguments": {} } }),
    ];

    mcp_session(&socket, "worker", &lines);
    let requests = daemon.join().expect("daemon");

    assert_eq!(requests[0]["method"], "tile_shrink");
    assert!(requests[0]["params"]["agent"].is_null());
}

#[test]
fn an_mcp_tile_shrink_resolves_a_worker_name() {
    let (socket, daemon) = fake_daemon(
        "mcp-tile-shrink",
        vec![
            serde_json::json!({ "ok": true, "result": [worker_json(9, "api")] }),
            serde_json::json!({ "ok": true, "result": null }),
        ],
    );
    let lines = [
        serde_json::json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": "tile_shrink", "arguments": { "agent": "api" } } }),
    ];

    let replies = mcp_session(&socket, "orchestrator", &lines);
    let requests = daemon.join().expect("daemon");

    assert_eq!(requests[0]["method"], "agents");
    assert_eq!(requests[1]["method"], "tile_shrink");
    assert_eq!(requests[1]["params"]["agent"], 9);
    assert_eq!(
        replies[0]["result"]["content"][0]["text"],
        "asked for a small tile"
    );
}

#[test]
fn the_start_orchestrator_does_not_get_the_tile_tools() {
    let lines = [
        serde_json::json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": "tile_grow", "arguments": {} } }),
    ];

    let replies = mcp_session(
        std::path::Path::new("/tmp/none.sock"),
        "start_orchestrator",
        &lines,
    );

    assert!(
        replies[0]["error"]["message"]
            .as_str()
            .expect("message")
            .contains("tile_grow")
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

    let start = Instant::now();
    mcp_session(&socket, "worker", &lines);

    assert!(
        start.elapsed() < Duration::from_secs(10),
        "{:?}",
        start.elapsed()
    );
    daemon.join().expect("the daemon saw the connection close");
    std::fs::remove_dir_all(dir).ok();
}

/// A Claude Code config folder with the given `settings.json`.
fn claude_config(name: &str, settings: &str) -> PathBuf {
    let dir = temp_dir(name);
    std::fs::write(dir.join("settings.json"), settings).expect("settings");

    dir
}

fn status_line_settings(command: &str) -> String {
    serde_json::json!({"statusLine": {"type": "command", "command": command, "padding": 0}})
        .to_string()
}

fn run_status_line(mut command: Command, input: &[u8]) -> Output {
    let mut child = command
        .arg("statusline")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start swarm");
    let mut stdin = child.stdin.take().expect("stdin");
    // The command may stop reading early when the input is too big.
    stdin.write_all(input).ok();
    drop(stdin);

    child.wait_with_output().expect("wait")
}

const RATE_LIMITS_INPUT: &str = r#"{"model":{"id":"opus"},"rate_limits":{"five_hour":{"used_percentage":23.5,"resets_at":1793383200},"seven_day":{"used_percentage":41.2,"resets_at":1793900000}}}"#;

#[test]
fn the_status_line_chains_to_the_users_command_and_prints_its_output() {
    let config = claude_config("chain", &status_line_settings("cat; printf ' | mine'"));
    let mut command = swarm();
    command.env("CLAUDE_CONFIG_DIR", &config);

    let output = run_status_line(command, RATE_LIMITS_INPUT.as_bytes());
    std::fs::remove_dir_all(&config).expect("cleanup");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("{RATE_LIMITS_INPUT} | mine"),
        "the user's command gets the same input"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn the_status_line_reads_the_home_folder_without_a_config_dir() {
    let home = temp_dir("home");
    std::fs::create_dir_all(home.join(".claude")).expect("folder");
    std::fs::write(
        home.join(".claude/settings.json"),
        status_line_settings("echo from-home"),
    )
    .expect("settings");
    let mut command = swarm();
    command.env_remove("CLAUDE_CONFIG_DIR").env("HOME", &home);

    let output = run_status_line(command, b"{}");
    std::fs::remove_dir_all(&home).expect("cleanup");

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "from-home\n");
}

#[test]
fn the_status_line_prints_nothing_without_a_user_status_line() {
    let config = claude_config("none", r#"{"model":"opus"}"#);
    let mut command = swarm();
    command.env("CLAUDE_CONFIG_DIR", &config);

    let output = run_status_line(command, RATE_LIMITS_INPUT.as_bytes());
    std::fs::remove_dir_all(&config).expect("cleanup");

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn the_status_line_does_not_run_swarm_statusline_as_the_users_command() {
    let swarm_path = env!("CARGO_BIN_EXE_swarm");
    for (name, command) in [
        ("direct", format!("'{swarm_path}' statusline")),
        (
            "wrapped",
            format!("sh -c '\"{swarm_path}\" statusline; echo wrapped'"),
        ),
    ] {
        let config = claude_config(name, &status_line_settings(&command));
        let mut run = swarm();
        run.env("CLAUDE_CONFIG_DIR", &config);

        let started = Instant::now();
        let output = run_status_line(run, b"{}");
        std::fs::remove_dir_all(&config).expect("cleanup");

        assert!(output.status.success(), "{name}");
        assert!(started.elapsed() < Duration::from_secs(2), "{name}");
        // The wrapper prints its own `echo`; the inner Swarm must print nothing.
        let expected = if name == "wrapped" { "wrapped\n" } else { "" };
        assert_eq!(String::from_utf8_lossy(&output.stdout), expected, "{name}");
    }
}

#[test]
fn a_slow_user_command_is_cut_off_and_still_exits_zero() {
    let config = claude_config("slow", &status_line_settings("echo early; exec sleep 30"));
    let mut command = swarm();
    command.env("CLAUDE_CONFIG_DIR", &config);

    let started = Instant::now();
    let output = run_status_line(command, b"{}");
    std::fs::remove_dir_all(&config).expect("cleanup");

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(started.elapsed() < Duration::from_secs(8));
}

#[test]
fn input_over_256_kib_is_dropped_silently() {
    let config = claude_config("big", &status_line_settings("echo mine"));
    let mut command = swarm();
    command.env("CLAUDE_CONFIG_DIR", &config);
    let input = vec![b' '; (256 << 10) + 1];

    let output = run_status_line(command, &input);
    std::fs::remove_dir_all(&config).expect("cleanup");

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn input_that_is_not_json_still_reaches_the_users_command() {
    let config = claude_config("garbage", &status_line_settings("cat"));
    let mut command = swarm();
    command.env("CLAUDE_CONFIG_DIR", &config);

    let output = run_status_line(command, b"not json");
    std::fs::remove_dir_all(&config).expect("cleanup");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"not json");
}

#[test]
fn the_status_line_sends_the_rate_limits_to_the_daemon() {
    let dir = temp_dir("limits");
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
    let config = claude_config("limits-config", &status_line_settings("echo mine"));
    let mut command = swarm();
    command
        .env("CLAUDE_CONFIG_DIR", &config)
        .env("SWARM_SOCKET", &socket)
        .env("SWARM_AGENT_ID", "7")
        .env("SWARM_TOKEN", "secret");

    let output = run_status_line(command, RATE_LIMITS_INPUT.as_bytes());
    let line = server.join().expect("server");
    std::fs::remove_dir_all(&dir).expect("cleanup");
    std::fs::remove_dir_all(&config).expect("cleanup");

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "mine\n");
    let request: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert_eq!(request["method"], "rate_limits");
    assert_eq!(request["agent"], 7);
    assert_eq!(request["token"], "secret");
    assert_eq!(
        request["params"]["windows"],
        serde_json::json!([
            {"label": "5h", "used_percent": 24, "resets_at": 1_793_383_200_u64},
            {"label": "7d", "used_percent": 41, "resets_at": 1_793_900_000_u64},
        ])
    );
}

/// Whether the file exists before `window` has passed.
fn appears_within(path: &std::path::Path, window: Duration) -> bool {
    let deadline = Instant::now() + window;

    while !path.exists() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::park_timeout(Duration::from_millis(10));
    }

    true
}

#[test]
fn the_users_command_runs_before_the_daemon_answers() {
    let dir = temp_dir("parallel");
    let socket = dir.join("agent.sock");
    let marker = dir.join("started");
    let listener = UnixListener::bind(&socket).expect("bind");
    let seen = marker.clone();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut line = String::new();
        BufReader::new(stream.try_clone().expect("clone"))
            .read_line(&mut line)
            .expect("line");

        // The answer waits for the user's command; the client gives up after a second.
        let started = appears_within(&seen, Duration::from_millis(800));
        stream.write_all(b"{\"ok\":true}\n").ok();

        started
    });
    let user_command = format!("touch '{}'; echo mine", marker.display());
    let config = claude_config("parallel-config", &status_line_settings(&user_command));
    let mut command = swarm();
    command
        .env("CLAUDE_CONFIG_DIR", &config)
        .env("SWARM_SOCKET", &socket)
        .env("SWARM_AGENT_ID", "7")
        .env("SWARM_TOKEN", "secret");

    let output = run_status_line(command, RATE_LIMITS_INPUT.as_bytes());
    let started_while_waiting = server.join().expect("server");
    std::fs::remove_dir_all(&dir).expect("cleanup");
    std::fs::remove_dir_all(&config).expect("cleanup");

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "mine\n");
    assert!(
        started_while_waiting,
        "the user's command waited for the daemon"
    );
}

#[test]
fn a_quick_users_command_still_waits_for_the_daemon_to_take_the_call() {
    let dir = temp_dir("takes");
    let socket = dir.join("agent.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut line = String::new();
        BufReader::new(stream.try_clone().expect("clone"))
            .read_line(&mut line)
            .expect("line");
        thread::park_timeout(Duration::from_millis(400));
        stream.write_all(b"{\"ok\":true}\n").ok();

        line
    });
    let config = claude_config("takes-config", &status_line_settings("echo mine"));
    let mut command = swarm();
    command
        .env("CLAUDE_CONFIG_DIR", &config)
        .env("SWARM_SOCKET", &socket)
        .env("SWARM_AGENT_ID", "7")
        .env("SWARM_TOKEN", "secret");

    let started = Instant::now();
    let output = run_status_line(command, RATE_LIMITS_INPUT.as_bytes());
    let waited = started.elapsed();
    let line = server.join().expect("server");
    std::fs::remove_dir_all(&dir).expect("cleanup");
    std::fs::remove_dir_all(&config).expect("cleanup");

    assert!(output.status.success());
    assert!(line.contains("rate_limits"), "{line}");
    assert!(
        waited >= Duration::from_millis(400),
        "it did not wait for the daemon: {waited:?}"
    );
    assert!(waited < Duration::from_secs(5), "{waited:?}");
}

#[test]
fn the_status_line_sends_nothing_without_rate_limits() {
    let dir = temp_dir("no-limits");
    let socket = dir.join("agent.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let mut command = swarm();
    command
        .env("CLAUDE_CONFIG_DIR", "/nonexistent")
        .env("SWARM_SOCKET", &socket)
        .env("SWARM_AGENT_ID", "7")
        .env("SWARM_TOKEN", "secret");

    let output = run_status_line(command, br#"{"model":{"id":"opus"}}"#);
    let connection = listener.accept();
    std::fs::remove_dir_all(&dir).expect("cleanup");

    assert!(output.status.success());
    let error = connection.expect_err("nobody should have connected");
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
}

#[test]
fn the_status_line_survives_a_daemon_that_is_not_there() {
    let config = claude_config("no-daemon", &status_line_settings("echo mine"));
    let mut command = swarm();
    command
        .env("CLAUDE_CONFIG_DIR", &config)
        .env("SWARM_SOCKET", "/tmp/swc-does-not-exist.sock")
        .env("SWARM_AGENT_ID", "7")
        .env("SWARM_TOKEN", "secret");

    let output = run_status_line(command, RATE_LIMITS_INPUT.as_bytes());
    std::fs::remove_dir_all(&config).expect("cleanup");

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "mine\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn help_lists_the_statusline_command() {
    let output = swarm().arg("--help").output().expect("run");

    assert!(String::from_utf8_lossy(&output.stdout).contains("statusline"));
}
