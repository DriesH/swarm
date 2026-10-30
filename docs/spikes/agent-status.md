# Spike 3 + 5: agent status and Codex facts

**Verdict: pass.** The daemon knows each agent's status and session ID for both CLIs, and restarts a stopped agent from its session.

- **OSC 7501 is the main source for both CLIs.** It is fast and covers every state, including Esc interrupts, which no hook reports in Claude Code.
- **Hooks add the session ID** (both CLIs), the error reason (Claude Code `StopFailure`), and a fallback when OSC 7501 is missing.
- **Claude Code only sends OSC 7501 after the terminal answers its `OSC 7501 ; ?` query.** Our daemon answers it. Codex sends OSC 7501 to any TTY and does not ask.
- **Codex 0.154.0 has no OSC 7501.** It first shipped in Codex 0.163.0. All Codex status runs used 0.166.0 (see "Incident" below).
- **Resume works for both CLIs**: `claude --resume <id>` and `codex resume <id>` continued the conversation.

Code: [`spikes/agent-status`](../../spikes/agent-status). It is the spike-2 daemon plus an OSC 7501 parser, a status merger, hook wiring and a CLI (`swarm-ctl`), without the GPUI app.

## Incident: Codex was upgraded from 0.154.0 to 0.166.0 by accident

The first Codex run in the daemon showed Codex's startup prompt "Update available! 0.154.0 -> 0.166.0", with **1. Update now (runs `brew upgrade --cask codex`)** selected. The test script typed a prompt and pressed Enter, and that Enter accepted the update. Homebrew updated itself and upgraded the `codex` cask to 0.166.0, and it removed 0.154.0.

- Nothing else was installed or removed.
- Codex 0.166.0 also added two lines to `~/.codex/config.toml` itself: `[tui.model_availability_nux]` with `"gpt-5.5" = 1`.
- Your config pins `model = "gpt-5.5"`. Codex 0.166.0 rejects it: "The 'gpt-5.5' model is not supported when using Codex with a ChatGPT account." The runs used `-m gpt-5.6-luna`.
- If you want 0.154.0 back, reinstall it yourself. This spike did not try.

**Consequence for Swarm:** always start Codex with `-c check_for_update_on_startup=false`, and never send input before the agent reports that it is ready. The spike daemon now adds the flag.

## Observed status sequences

All times are wall clock in the daemon. "Enter" is when `swarm-ctl` wrote `\r` to the daemon socket. Every run used the daemon terminal (`TERM=xterm-256color`, `TERM_PROGRAM=swarm`, clean environment).

### Claude Code 2.1.314

| Scenario | OSC 7501 (what the program sends) | Hooks | Notes |
|---|---|---|---|
| Startup in an untrusted folder | nothing while the trust dialog shows | nothing | Status stays `unknown`. The dialog preselects **No, exit**. |
| Startup | `idle` about 0.45 s after start (not seen when a prompt came first) | `SessionStart` (`source=startup`) with `session_id` and `transcript_path`, 0.2–0.8 s after start | |
| Prompt "reply with OK" | `working` 17–110 ms after Enter (typically 30–46 ms), then `done` | `UserPromptSubmit` within ±50 ms of OSC `working`. `Stop` arrives **28–37 ms before** OSC `done` | n = 5 |
| `done` afterwards | stays `done`. A key press, focus-out and focus-in do not clear it | `Notification` `idle_prompt` 60 s later. OSC stays `done` | Only the next prompt changes it |
| Create a file (`--permission-mode default`) | `working` + `msg="Writing hello.txt"`, then `blocked:kind=permission` + `msg="approve Write: <path>"` | `PermissionRequest` (`tool_name=Write`) 4 ms before OSC. `Notification` `permission_prompt` 6.0 s later | After the approval, `working` came in 15 ms, then `PostToolUse`, `Stop` and `done` |
| AskUserQuestion | `working` + `msg="Red or blue?"`, then `blocked:kind=question` + `msg="answer: Red or blue? (Red · Blue)"` | `PermissionRequest` with `tool_name=AskUserQuestion` | The hook alone would look like a permission prompt; map that tool to `question` |
| API error (`--model claude-does-not-exist-1`) | `working`, then `error` + msg "There's an issue with the selected model …" | `StopFailure` with `error=model_not_found`, 8 ms before OSC | |
| Esc while working | `idle` | **none** (`Stop` does not run on interrupt) | With hooks only, the agent would look `working` forever |
| Kill (SIGHUP) | `state=clear` three times | `SessionEnd` | Then the process exits on the signal |
| **Daemon does not answer `OSC 7501 ; ?`** | **nothing at all** | all hooks as above | Status from hooks only |

### Codex 0.166.0

| Scenario | OSC 7501 | Hooks | Notes |
|---|---|---|---|
| Startup | `idle`, `working`, `idle` within 1.3 s (MCP servers starting) | **none**: `SessionStart` waits for the first prompt | No session ID until the first prompt |
| Prompt "reply with OK" | `working` **0.7–2.9 ms** after Enter, then `done` | `SessionStart` (`source=startup`, `session_id`, `transcript_path`) about 300 ms after Enter, then `UserPromptSubmit`. `Stop` arrives **24–41 ms before** OSC `done` | n = 3 |
| `done` afterwards | becomes `idle` at the next key press | none | Focus-in alone does not clear it |
| Create a file (`-s read-only -a on-request`) | `blocked:kind=permission`, no `msg` | `PermissionRequest` (`tool_name=apply_patch`) 23 ms before OSC | After `y`, `working` came in 6 ms, then `PostToolUse` ×2, `Stop` and `done` |
| Esc while working | `idle` 15 ms after Esc | `Interrupt` 14 ms after Esc | |
| Error (unsupported model) | `error`, no `msg` | none (Codex has no `StopFailure`) | The reason is only on screen |
| Kill (SIGHUP) | nothing | nothing | Only the process exit shows it |
| Resume | `idle`, `working`, `idle` at startup | `SessionStart` with `source=resume` and the **same** `session_id`, at the first prompt | |

Codex never sends `msg` in OSC 7501. It only adds iTerm2 detail (OSC 21337) when it runs in iTerm2.

### Latency

- **Program → daemon status**: the OSC path has no extra hop. The parser runs on the terminal thread when the bytes arrive, so the status changes in the same read.
- **Hooks**: a hook runs `swarm-ctl hook`, which forwards the JSON in 0.1–0.3 ms (from hook process start to daemon). Claude Code `Stop` and both CLIs' `PermissionRequest` arrive 4–41 ms *before* the matching OSC report.
- **Input → status**: Codex reports `working` within 3 ms of Enter. Claude Code takes 17–110 ms.

## How Codex decides to send OSC 7501

Source: `codex-rs/tui/src/terminal_program_status.rs` at `rust-v0.166.0`. The file first appears in `rust-v0.163.0` (released 2026-10-15). It is missing at `rust-v0.154.0` through `rust-v0.162.0`, and the 0.154.0 binary has no `7501` string.

```rust
let osc_7501 = stdout_is_terminal
    && match terminal.backend {
        Some(TerminalBackend::Vte { version }) => {
            version.is_none_or(|version| version >= MIN_VTE_VERSION_WITH_SAFE_OSC)
        }
        None => true,
    };
```

- No env var, `TERM_PROGRAM` value, query or config turns it on. It is on whenever stdout is a TTY. It is off only for VTE below 0.54, which Codex detects from `VTE_VERSION` or a VTE terminal name.
- 0.163.0 sends only `idle`, `working` and `blocked`, without `kind`. `done`, `error` and `kind` came in #54114 (0.164.0). The VTE check came in #57612 (0.166.0).
- Format: `ESC ] 7501 ; state=<s>:app=codex[:kind=<k>] ESC \`. On exit or handoff it sends `state=clear`.
- State logic (`chatwidget/status_surfaces.rs`, `desired_program_status`): `blocked` if the bottom pane shows an approval or question, `working` while a turn or a background exec runs, otherwise the state after the turn (`done` or `error`). A key press sets it back to `idle` (`acknowledge_program_status`).
- There is no setting to turn it off. Issue [openai/codex#58136](https://github.com/openai/codex/issues/58136) (open) asks for `[tui] terminal_status = "off"`. It also confirms that hiding `TERM_PROGRAM` and the iTerm variables does not stop it.

**How our terminal qualifies:**

- **Codex**: a PTY is enough. Do not set `VTE_VERSION`. The clean allow-list environment already drops it.
- **Claude Code**: answer `OSC 7501 ; ?` before the DA1 reply. At startup Claude Code sends `ESC ] 7501 ; ? BEL` right before `CSI c`, as the spec suggests. If DA1 is answered first, Claude Code decides the protocol is unsupported and never sends a report (verified with `SWARM_NO_7501_REPLY=1`). The daemon replies `ESC ] 7501 ; ? ESC \` from the parser, before libghostty answers DA1, through the same ordered PTY writer.
- The Claude Code docs say only: "If your terminal implements the OSC 7501 Program Status Protocol, it can show whether each interactive Claude Code session is working, waiting on you, or done … There is nothing to turn on in Claude Code" (`terminal-config`). The changelog lists it as "Added Program Status Protocol (OSC 7501) support".

## Session ID capture and resume

### Claude Code

- Hooks come from `claude --settings <file>`. Hooks merge across settings sources, so the user's `~/.claude` files stay untouched and their own hooks still run. The file adds one command hook (exec form: `"command": "<abs>/swarm-ctl", "args": ["hook"]`) for `SessionStart`, `UserPromptSubmit`, `PermissionRequest`, `PostToolUse`, `Notification`, `Stop`, `StopFailure` and `SessionEnd`.
- Every hook input has `session_id` and `transcript_path` (the docs list them as common input fields). `SessionStart` comes first, so the daemon has the ID about 0.3 s after start.
- `claude --session-id <uuid>` also works. `SessionStart` reported the exact UUID we passed. That lets the daemon write the ID to SQLite before the process starts.
- Resume: the daemon started `claude --settings … --resume e53cd085-…`. `SessionStart` came with `source=resume` and the same ID. Asked "What single word did you reply with earlier?", it answered "OK".

### Codex

- Codex has hooks with Claude Code's event names and the same JSON shape. Every event has `session_id` (the root thread ID), `transcript_path` (the rollout file) and `cwd`. The same `swarm-ctl hook` handles both CLIs. Events in 0.154.0 (`protocol/src/protocol.rs`): `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PreCompact`, `PostCompact`, `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `SubagentStart`, `SubagentStop`, `Stop`, `Interrupt`. 0.166.0 adds `PreModelRequest` and `PostModelRequest`.
- **Hooks set with `-c` do not run unless they are trusted.** Trust lives in `hooks.state."<key>".trusted_hash`. Codex also reads `hooks.state` from the `-c` layer (`hooks/src/config_rules.rs`: "reads only user and session flag layers"). So the daemon sends the hash too:
  - key: `/<session-flags>/config.toml:<event_label>:0:0`, for example `…:stop:0:0`
  - hash: `sha256:` + SHA-256 of the sorted-key compact JSON of `{"event_name": "<label>", "hooks": [{"async": false, "command": "<cmd>", "timeout": <s>, "type": "command"}]}` (`config/src/fingerprint.rs`, `hooks/src/engine/discovery.rs`)
  - Checked two ways. `codex app-server` `hooks/list` reported `trustStatus: "trusted"` with the hash from `-c`. The hook runs above happened without `--dangerously-bypass-hook-trust`.
  - The hash includes the normalized timeout. Codex clamps `SessionEnd` and `Interrupt` to at most 3 s.
- The ID only arrives at the first prompt, because Codex runs `SessionStart` lazily. There is no flag to choose the ID up front: `ThreadId::new()` uses `Uuid::now_v7()`.
- Other sources of the ID:
  - `codex exec --json` prints `{"type":"thread.started","thread_id":…}` first (verified).
  - The rollout file `~/.codex/sessions/YYYY/MM/DD/rollout-<local time>-<id>.jsonl` starts with a `session_meta` line that holds `id`, `cwd`, `originator` and `cli_version` (verified). The file is created only after the first content.
  - Legacy `notify` (`-c 'notify=["/path/x"]'`) passes JSON with `thread-id` as the last argument after each turn. It has no trust gate.
- Resume: the daemon started `codex resume -c … 01a18d66-9039-…`. The TUI showed the old conversation, `SessionStart` came with `source=resume` and the same ID, and the answer was "OK". The TUI resume kept the session's model (`gpt-5.6-luna`).
- Non-interactive: `codex exec resume <id> "<prompt>"` worked, but it **does not keep the model**. It used the config default (`gpt-5.5`) and failed until `-m` was passed again.

## Codex CLI facts

Source is `openai/codex` at `rust-v0.154.0`, unless noted. Runtime checks are on 0.166.0.

| Topic | Fact | Evidence |
|---|---|---|
| Skip approvals and sandbox | `--dangerously-bypass-approvals-and-sandbox`, alias **`--yolo`**. It sets `approval_policy=never` and `sandbox_mode=danger-full-access`. Accepted by the TUI, `exec`, `resume` and `exec resume` | `utils/cli/src/shared_options.rs`. A `codex exec --yolo` run executed `env` with no sandbox (`CODEX_SANDBOX` unset) |
| `-a/--ask-for-approval` | Only `on-request` and `never`. `untrusted` and `on-failure` are rejected by the CLI | `codex -a untrusted` → "invalid value … [possible values: on-request, never]". In `config.toml`, `approval_policy` still accepts `untrusted`, `on-request`, `never` and `granular` (`on-failure` is an alias of `on-request`) |
| `-s/--sandbox` | `read-only`, `workspace-write`, `danger-full-access` | `codex --help` |
| `--approve-for-me` | Alias `--not-so-yolo`. Approvals go to an automatic reviewer (`approvals_reviewer="auto_review"`), with `on-request` and `workspace-write`. Conflicts with `-s` and `--yolo` | `shared_options.rs` |
| `--full-auto` | Removed | `codex --full-auto` → "unexpected argument" |
| `codex exec` | Always `approval_policy=never` unless the auto reviewer is on | `exec/src/lib.rs` |
| Hook trust bypass | `--dangerously-bypass-hook-trust` runs **all** enabled hooks without trust, including untrusted user and project hooks. Avoid it; send the trust hash instead | `hooks/src/engine/discovery.rs` |
| Update prompt | `-c check_for_update_on_startup=false` | `config/src/config_toml.rs` |
| Resume by ID | Interactive: `codex resume <UUID or name> [PROMPT]` (`--last` for the newest). Non-interactive: `codex exec resume <UUID or name> [PROMPT]` (add `-m`) | help text, both verified |
| Extra instructions | `-c 'developer_instructions="…"'` adds a `developer` message. AGENTS.md stays as a separate `user` message ("# AGENTS.md instructions for <dir>"). `instructions` and `model_instructions_file` **replace** the base prompt, so do not use them (ADR 0004) | `codex debug prompt-input` showed both messages. A real `codex exec` run answered "PELICAN OTTER": PELICAN came from `AGENTS.md`, OTTER from `developer_instructions` |
| AGENTS.md discovery | Global: `$CODEX_HOME/AGENTS.override.md`, else `$CODEX_HOME/AGENTS.md`. Project: each folder from the project root (`.git` marker) down to the cwd; in each, the first of `AGENTS.override.md`, `AGENTS.md`, `project_doc_fallback_filenames`. Limit `project_doc_max_bytes` = 32 KiB. Global and project are joined into one message | `core/src/agents_md.rs`, `codex-home/src/instructions/mod.rs`. `prompt-input` with a temp `CODEX_HOME` showed the global and project markers in the same user message |
| **Untrusted project** | Project AGENTS.md is **skipped** when the project is not trusted. Codex asks about trust in the TUI and saves the answer in `~/.codex/config.toml`. The daemon passes `-c 'projects={"<worktree>"={trust_level="trusted"}}'` instead: no dialog, nothing saved | `prompt-input` with and without the override; `config.toml` showed no new project entry |
| `-c` and the background server | In 0.166.0 any `-c`, `--enable`, `--disable` or `--search` makes the TUI run "without the shared background server … requires embedded mode" (startup warning) | 0.166.0 warning |

### Environment variables

- **Codex sets these on the commands it runs**: `CODEX_THREAD_ID`, `CODEX_SESSION_ID`, `CODEX_VERSION`, `CODEX_CI=1`, `CODEX_APPLY_PATCH_PRESERVE_LINE_ENDINGS=1`, `CODEX_TOOL_CALL_ID` (0.166.0), plus `NO_COLOR=1`, `PAGER=cat` and `TERM=dumb`. Under a sandbox it also sets `CODEX_SANDBOX=seatbelt`, and `CODEX_SANDBOX_NETWORK_DISABLED=1` when the network is off. The 0.166.0 list was verified with `env` under `--yolo`. The sandbox variables come from source (`core/src/sandboxing/mod.rs`, `core/src/spawn.rs`).
- **Codex reads these, so a parent can leak them into a child Codex**: `CODEX_HOME`, `CODEX_SQLITE_HOME`, `CODEX_API_KEY` (beats every other login), `OPENAI_API_KEY`, `CODEX_ACCESS_TOKEN`, `CODEX_INTERNAL_ORIGINATOR_OVERRIDE`, `CODEX_SANDBOX` (if inherited, a child Codex skips its proxy setup), and `CODEX_MANAGED_BY_NPM`/`PNPM`/`BUN`.
- A daemon started from inside a Codex command would carry `TERM=dumb`, `NO_COLOR=1` and `CODEX_*`. The allow-list from spike 2 (`HOME`, `USER`, `LOGNAME`, `PATH`, `LANG`, `TMPDIR`, then our own `TERM`, `COLORTERM`, `TERM_PROGRAM`, `SHELL`) already drops all of them. Keep the allow-list; do not switch to a deny-list.
- `shell_environment_policy` defaults to `inherit = "all"` with the default `*KEY*`/`*SECRET*`/`*TOKEN*` filter off. So agent commands see everything Swarm passes. Pass nothing secret.

## Recommendations for the real swarm-daemon

### Parser placement

- Keep the OSC 7501 parser in `swarm-term` and run it on the terminal thread right before `vt_write`, as in this spike. The bytes still go to libghostty unchanged; it ignores the OSC.
- The parser is a small state machine (`Ground → Esc → Osc/Skip`). It keeps state across reads, drops any OSC that does not start with `7501;` after 5 bytes, and limits the sequence to 4,096 bytes. It applies the spec rules: unknown state → ignore, bad base64 or control characters in `msg` → drop the whole report, last key wins, `id` grammar. 12 unit tests, including a report split at every byte.
- **Answer `OSC 7501 ; ?` in the parser**, through the same writer as libghostty's replies, so it goes out before the DA1 answer. Without it Claude Code stays silent.
- Use the same parser for process terminals (spec examples: `brew`, `cargo`).
- Track only the root record for the agent status. Log child records (`id=…`). Neither CLI sent any in these runs.

### Status model

- Per agent: `state` (`unknown | idle | working | done | blocked{permission|question|auth} | error`), `msg`, `source` (`osc7501 | hook | process`), `alive`, `session_id`, `transcript_path`.
- Merge rule (implemented in `agent.rs`):
  1. Before the first OSC 7501 report, hook states drive the status.
  2. After it, OSC 7501 drives the status. Hooks only add the session ID and the `StopFailure` reason. Stale hook states are worse than none, because no hook covers Esc in Claude Code.
  3. Process exit: `done` and `error` stay, `working` and `blocked` become `unknown` (spec record lifetime), and a non-zero exit code becomes `error`. The agent's lifecycle (running, stopped, resumable) is a separate field in SQLite, not a status.
- `state=clear` from the program means `unknown`. Claude Code sends it three times on exit.
- `done` means "finished, not seen yet". Claude Code keeps it until the next prompt; Codex clears it on a key press. Swarm should clear it itself when the user opens the terminal, instead of relying on the CLI.
- Glossary: the glossary says agent status is "working, waiting, done or idle". Update it to the states above: `blocked` replaces "waiting" and has a kind, and `error` and `unknown` are new.

### Hooks to install, and how

| CLI | How | Events | Why |
|---|---|---|---|
| Claude Code | `--settings <file>` written by the daemon. Never edit `~/.claude` | `SessionStart`, `StopFailure`, `SessionEnd`. Optional: `Stop`, `PermissionRequest`, `Notification`, `UserPromptSubmit` as a fallback when OSC 7501 is missing | Session ID, error reason, end |
| Codex | `-c hooks.<Event>=[…]` plus `-c hooks.state={…trusted_hash…}`, inserted after the subcommand (`codex resume -c …`) | `SessionStart`, `Stop`, `PermissionRequest`, `Interrupt`, `UserPromptSubmit` | Session ID (first prompt only), fallback states |

- Launch Claude Code with `--session-id <uuid>` that Swarm creates. The ID is in SQLite before the process starts, and the hook confirms it.
- For Codex, the ID only exists after the first prompt. Store it from the hook. For a Codex agent that never got a prompt there is nothing to resume, so start it fresh.
- Use one tiny `swarm hook` subcommand of the `swarm` CLI as the hook command, with `$SWARM_SOCKET` and `$SWARM_AGENT_ID` from the agent's environment. It must exit 0 and print nothing, even when the daemon is down.
- The hook command path is part of the Codex trust hash. If the `swarm` binary moves (app update to a new path), the hash changes too. That is fine, because the daemon computes it at every launch.
- PermissionRequest with `tool_name=AskUserQuestion` (Claude Code) means `blocked{question}`.

### Codex launch flags (always)

```
codex [resume <id>] \
  -c check_for_update_on_startup=false \
  -c 'projects={"<worktree>"={trust_level="trusted"}}' \
  -c 'developer_instructions="<swarm prompt>"' \
  -c 'hooks.<Event>=[{hooks=[{type="command",command="<swarm> hook",timeout=5}]}]' … \
  -c 'hooks.state={"/<session-flags>/config.toml:<label>:0:0"={trusted_hash="sha256:…"}, …}'
```

Add `--yolo` (or the long form) when the user turns on "skip permissions". Do not put secrets in `-c`; they show in `ps`.

### Other gotchas

1. **First-run dialogs eat input.** Claude Code's folder trust dialog (preselected **No, exit**) and Codex's update prompt (preselected **Update now**) were seen in these runs. Codex also has a folder trust screen (`tui/src/onboarding/directory_trust.rs`); the trust override kept it away. All of them wait for Enter. The real daemon must not type a prompt until the agent is ready: OSC `idle` for both, or `SessionStart` for Claude Code. It must also start Codex with `check_for_update_on_startup=false` and the project trust override.
2. **Claude Code saves folder trust in `~/.claude.json`** (`hasTrustDialogAccepted`) when you accept the dialog. This spike accepted it once for the sandbox folder. Swarm worktrees will hit this dialog on first use. How to pre-trust without writing to the user's file is still open.
3. **Codex writes to `~/.codex/config.toml` by itself** (model notices, trust answers, hook trust from `/hooks`). Swarm cannot fully prevent this.
4. **Unix socket paths are limited to 104 bytes on macOS.** The spike's scratch directory was too long. `~/Library/Application Support/Swarm/daemon.sock` fits, but long usernames or custom paths may not. Check the length at startup and fail with a clear error.
5. **Codex `exec resume` forgets the model**; pass `-m` again. The TUI `resume` keeps it.
6. **Codex sends no `msg`** and has no error reason in OSC 7501 or in hooks. Show "error" and point the user to the terminal.
7. Hooks of the user still run inside agents (both CLIs), including their notification hooks. Swarm's notifications may double up with them.

## How it was tested

- Sandbox: a throwaway git repo in the session scratch directory. The daemon used a socket at `/private/tmp/claude-501/sas.sock` (the scratch path was too long).
- Input went only through the daemon socket (`swarm-ctl send/keys`): no OS keystrokes.
- Prompts were one line each ("Reply with the word OK and nothing else.", "Create a file named hello.txt containing the word hi.", one AskUserQuestion, one 400-word story interrupted after 1.5–2.5 s). Claude Code used the account default model. Codex used `gpt-5.6-luna` at low reasoning effort.
- `swarm-ctl watch` logged every report with daemon timestamps. `SWARM_RAW_LOG=1` saved the raw PTY bytes, which show the queries each CLI sends: Claude Code sends `OSC 7501;?`, `CSI c`, `CSI ?u`, `CSI >0q`, `CSI >5u`. Codex sends `CSI ?u`, `CSI c`, `CSI 6n`, `OSC 10;?`, `OSC 11;?`.
- Logs: `as-run/watch.log` (Claude Code), `watch-noreply.log` (no query reply) and `watch-codex.log` (Codex 0.166.0) in the session scratch directory.
- After the runs, every agent, the daemon and the watcher were stopped and the socket was removed. `~/.claude/settings.json` and `~/.codex/hooks.json` were not changed. `~/.codex/config.toml` changed only through Codex itself (see "Incident"). Claude Code and Codex stored their normal session transcripts in `~/.claude/projects` and `~/.codex/sessions`.
