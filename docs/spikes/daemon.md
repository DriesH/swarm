# Spike 2: daemon reattach

**Verdict: pass.** A separate `swarm-daemon` owns the PTYs and the `libghostty-vt` terminals. The GPUI app attaches over a unix socket, quits or gets killed, and reattaches in under 2 ms with the screen, colors, cursor and scrollback intact. `claude` keeps running and stays interactive. Throughput matches spike 1, and the socket adds about 0.1 ms to a keystroke echo.

Limits:

- All input was driven in-process (script runner), as in spike 1. Do a short hands-on check with a real keyboard.
- Key-to-paint latency could not be compared with spike 1: spike 1 did not measure it, and in this spike it is dominated by GPUI frame pacing of a background window (see criterion 4).

Code: [`spikes/daemon`](../../spikes/daemon) (about 3,200 lines in three crates: `proto`, `swarm-daemon`, `swarm-app`).

## Pass criteria

| # | Criterion | Result | Evidence |
|---|---|---|---|
| 1 | Process survives app quit and `kill -9` | Pass | A colored counter loop kept counting after Cmd-Q (8 → 182) and after `kill -9` of the app (922 → 961). The daemon runs in its own session (`setsid`), with PPID 1 after the app exits. |
| 2 | Reattach shows the current screen at once | Pass | Attach takes 0.2–2 ms. The window opens at the terminal's size, and the first frame already has the content: no blank frame, no replay. Colors, bold, underline and the cursor are intact. `top` (alternate screen) reattaches the same way. Scrollback works 500 and 20,000 lines up. |
| 3 | `claude` survives quit and reopen | Pass | The TUI was identical after reattach. Typing into the prompt worked and Ctrl-C cleared it; no prompt was sent. The "Transcript saving is off" warning from spike 1 is gone. The shell environment has only 10 variables (see "Clean environment"). |
| 4 | Keystroke latency | Pass | Key sent → update with the echo received: **p50 0.24–0.36 ms, p99 0.48 ms** (n = 200 per run; one run had p99 2.4 ms). The same path inside the daemon, without the socket, is p50 0.15–0.28 ms, so the socket and decoding add about 0.1 ms. That is far below one frame (8.3 ms at 120 Hz). |
| 5 | Throughput | Pass | See the table below. |
| 6 | Two clients on one terminal | Pass | Two app processes attached to the same terminal. Each saw the other's input and output live. |
| 7 | Daemon crash | Pass | See "Daemon crash and recovery". |

### Throughput (criterion 5)

Wall time from `/usr/bin/time -p` inside the terminal, three runs each. "App sees the end" is the time from Enter until the app's screen shows a marker printed after the command.

| Workload | Shell time | App sees the end | Spike 1 |
|---|---|---|---|
| `seq 1 200000` | 0.27–0.28 s | 291–310 ms | 0.27 s |
| `cat` 20 MB plain text | 0.12–0.15 s | 147–180 ms | 0.12–0.13 s |

During `seq`, the daemon sent at most one update per 8 ms, and two `seq` runs together sent under 50 KB. Updates are small because only changed rows are sent.

### Attach cost (criterion 5)

Measured with `swarm-bench`: a 200×60 terminal filled with styled 200-column lines (color and bold on every line), attached five times per setting.

| Snapshot | Size | Attach time (median) |
|---|---|---|
| Screen only | 12 KB | 0.2 ms |
| Screen + 1,000 scrollback lines | 223 KB | 9.9 ms |
| Screen + 10,000 scrollback lines | 2.1 MB | 69 ms |

- Almost all of the time is spent reading rows out of `libghostty-vt` (65 ms for 10,000 lines), not encoding or the socket. That is about 6.5 µs per 200-column line.
- In the real runs, snapshots were much smaller: 1.3 KB for `claude`, 5.8 KB for `top`, 35 KB for a 120×34 shell with 942 lines of history.
- The app now asks for 1,000 lines on attach and fetches older lines when you scroll to them.

## Design

### Wire format

Each frame is a little-endian `u32` length followed by one [postcard](https://docs.rs/postcard) message. The message types are serde enums in `crates/proto`.

Why postcard:

- It uses serde derives, so the protocol is plain Rust types with no schema files or code generation.
- It is compact. Integers are varints, and colors are 3 bytes. A full styled 200×60 screen is 12 KB, and a typing echo is one row.
- Its wire format is documented and stable since 1.0.
- The length prefix allows `read_exact` framing on a blocking socket, and one encoded frame can be shared by every client.

Versioning is a `Hello { version }` exchange. The real protocol should add explicit message tags or a version per message before it is frozen.

### What goes over the wire

The daemon does all terminal emulation and sends rows that are ready to paint:

- `Row` = background spans (`col`, `len`, RGB) + text spans (`col`, cells, text, RGB, style flags). Palette, inverse and default colors are already resolved.
- `Screen` = grid size, default colors, cursor (position and shape), alternate-screen flag, and the scrollback range. It is small and is sent whole with every update.
- `Snapshot` on attach = `Screen` + all viewport rows + the last N scrollback lines.
- `Update` = `Screen` + only the rows that changed. The daemon keeps the last sent row per line and compares, so a full redraw inside Ghostty does not always mean a full resend.
- Keys go up as logical events (GPUI key name, modifiers, text). The daemon encodes them, because the encoding depends on terminal modes (Kitty keyboard flags, cursor-key mode).

### Scrollback

- Every scrollback line has an absolute number that never changes. `Screen.history` says which numbers exist now: `start..start + len`.
- The app caches lines by number and scrolls locally, so scrolling needs no round trip once lines are cached. Missing lines are requested with `FetchHistory` and show as blank until they arrive (about 1 ms).
- The daemon counts new scrollback lines with a `TrackedGridRef` on the first active row. After each batch of output, the new screen position of that row tells how many lines scrolled into history, even when old lines were pruned. This was checked across 1.2 million lines of output: line numbers stayed correct.
- On resize, Ghostty reflows the scrollback, so the daemon tells clients to drop their cache (`history_reset`).
- On the alternate screen, the primary scrollback is frozen and local scrolling is off.

### Threading

`libghostty-vt` types are `!Send`, so each terminal lives on one thread.

| Thread | Owns | Talks over |
|---|---|---|
| Accept loop (main) | the listener | — |
| Client reader (one per connection) | the socket read half | sends commands to terminal threads |
| Client writer (one per connection) | the socket write half | receives encoded frames (`Arc<Vec<u8>>`) |
| Terminal (one per terminal) | `Terminal`, `RenderState`, iterators, key encoder, PTY master, subscriber list | one `flume` channel of commands |
| PTY reader (one per terminal) | the PTY read side | shared buffer + one wake-up message (spike 1 design) |
| PTY writer (one per terminal) | the PTY write side | channel of byte buffers |

Flushing rule: the first change after a quiet period goes out at once (low echo latency). Later changes are batched to one update per 8 ms.

## Daemon crash and recovery (criterion 7)

### (a) What survives a `kill -9` of the daemon

Setup: terminal A ran `fish` → `claude` (with two MCP servers as child processes). Terminal B ran `fish` with four background jobs. After `kill -9` of the daemon, within 3 s:

| Process | Result |
|---|---|
| `fish` (both) | exited (SIGHUP when the PTY master closed) |
| `claude` and its MCP children (`npm exec …`, `node …`) | exited |
| `sleep 1001 &` (plain background job) | exited (fish sends SIGHUP to its jobs when it hangs up) |
| `nohup sleep 1002 &` | **survived** |
| `sh -c 'trap "" HUP; exec sleep 1003' &` | **survived** |
| `sh -c 'trap "" HUP TERM; exec sleep 1004' &` | **survived** |

So SIGHUP cleans up normal agents, but anything that ignores SIGHUP lives on as an orphan of launchd.

### (b) Recovery

- Before a process starts, the daemon writes a record with status `starting` (ADR 0005). After the spawn, it adds the PID and the process start time (from `proc_pidinfo(PROC_PIDTBSDINFO)`). The spike uses a JSON file; the real daemon will use SQLite.
- At startup, before it serves anything, the daemon checks every `starting` or `running` record:
  - If the recorded PID is alive but has a different start time, the PID was reused by an unrelated process. Recovery skips it.
  - Otherwise it finds every live process whose session ID (`getsid`) equals the recorded PID. portable-pty runs each child with `setsid`, so that is the agent's whole process tree, also after the leader died. It only counts processes that started after the leader.
  - It sends SIGTERM, waits up to 3 s, then sends SIGKILL to anything still alive with the same PID and start time.
  - It marks the record `stopped`.

Result:

- The three survivors were found through their session, even though their session leader (`fish`) was gone. Two stopped on SIGTERM. The TERM-ignoring one got SIGKILL after 3 s.
- A fake record that pointed to an unrelated `sleep 3000` (started outside the daemon, wrong start time) was skipped, and that process kept running.

Problem found: recovery ran before the socket was bound, so the app waited 3.2 s to connect. The real daemon should bind first and run recovery in the background.

## Clean environment

The daemon starts agents with a cleared environment plus an allow-list: `HOME`, `USER`, `LOGNAME`, `PATH`, `LANG`, `TMPDIR`. It sets `SHELL` (the passwd login shell), `TERM=xterm-256color`, `COLORTERM=truecolor` and `TERM_PROGRAM=swarm`. Started from a Claude Code session, the shell had exactly these 10 variables and no `CLAUDE_CODE_*`. An allow-list is safer than removing known variables, because new agent markers appear over time.

## Gotchas

1. **Ghostty's `max_scrollback` is in bytes, not lines.** The C header and the Rust docs say "lines", but `Screen.zig` and `PageList.zig` treat it as a byte budget, rounded to whole pages. Spike 1 passed `10_000`, so it had almost no scrollback. This spike uses 32 MB. A 200-column line costs about 2 KB.
2. **"Flush when output goes quiet" floods the socket.** `seq` writes one line per `write`, so the buffer is empty after almost every read. The first version sent 3,467 updates in one second (239 ms of encoding). The fix: send the first change at once, then at most one update per 8 ms.
3. **Fish 4 turns on the Kitty keyboard protocol.** GPUI reports shifted symbols such as `%` or `!` as their own key name, which maps to ghostty's `Unidentified` key. With Kitty flags on, the encoder sent nothing for those keys, so they were lost. Fix: send the text directly for an unidentified key without Ctrl or Alt. Spike 1 did not see this, because zsh does not enable the Kitty protocol.
4. **Reading scrollback is the expensive part.** The only fast row API is `RenderState`, which covers the viewport. The daemon scrolls the viewport over the history lines, reads them, and scrolls back. That costs about 6.5 µs per styled 200-column line, and the next update is a full Ghostty redraw (the row diff hides most of it). `grid_ref` per cell is documented as slower.
5. **There is one `RenderState` per terminal.** A second one would consume the dirty flags, and the main one would miss changes.
6. **New scrollback is not pushed to clients.** If you are scrolled up while output streams, lines that just entered history are blank for one round trip plus one frame until the fetch returns.
7. **Client queues are unbounded.** A stuck client makes the daemon's memory grow. The real daemon needs a cap: when a client falls behind, drop its queue and send a new snapshot.
8. **A scripted window can receive real typing.** Once, a stray `f` and a space appeared in the terminal while a test window had focus. Neither the script nor the app's key log produced them, so they were most likely typed by the user into the window that had just taken focus. Scripted windows now open without focus (`WindowOptions { focus: false }`) and the app is not activated.
9. **Key-to-paint is mostly GPUI frame pacing.** The window was in the background. Key → paint was p50 5–18 ms and p99 32 ms, while key → update was 0.4 ms. During floods, the background window painted 13–33 fps. Measure again with a focused, visible window.

## Recommendations for the real crates

### What lives where

| Crate | Contents |
|---|---|
| `swarm-term` (no GPUI) | The `libghostty-vt` wrapper, the row encoder, scrollback numbering, key encoding and the wire types for screens. Only the daemon links it. |
| `swarm-term-view` (or a module in `swarm-ui`) | The GPUI element and the client-side screen model: rows, scrollback cache, cursor. It does not link libghostty, so app builds need no Zig. |
| `swarm-daemon` | PTYs, terminal threads, agent records (SQLite), recovery, the socket server. |
| `swarm-harness` | The JSON-RPC control API for the CLI and MCP (spawn, send, read). |

Keep two protocols:

- **Control:** JSON-RPC (harness), readable by people and by agents.
- **Screen stream:** binary postcard frames, as in this spike.

They can share one socket with a frame type byte, or use two sockets. Two sockets is simpler.

### Protocol shape

- Requests: `Hello`, `List`, `Create`, `Attach { id, history }`, `Detach`, `Key`, `Paste`, `Resize`, `FetchHistory { from, count }`, `Kill`. Scrolling stays in the client; there is no scroll request.
- Events: `Snapshot`, `Update` (changed rows + small `Screen` header), `History`, `Exited`, plus title, working directory and bell events for agent status.
- Attach with about one screen of history (a few hundred lines). Fetch the rest on demand.
- Push new scrollback lines to clients that are scrolled up (they send "viewing history"), to fix gotcha 6.
- Add a mouse event request and a selection request when copy and mouse reporting are built. Both need the daemon's terminal state.

### Resume after a reboot

1. Spawn Claude Code with `claude --session-id <uuid>`. Swarm creates the UUID, so it is in SQLite before the process starts. `claude --help` (v2.1.314) lists both `--session-id <uuid>` and `--resume <session-id>`.
2. Store per agent: kind, session ID, worktree, command line, PID, start time and status.
3. After a crash or a reboot, recovery marks the agent `stopped, resumable`. Starting it again runs `claude --resume <session-id>` in the same worktree, in a new terminal. The screen and scrollback are not restored; Claude Code redraws the conversation itself.
4. Codex needs the same facts (spike 5).

## How it was tested

- **Input:** the spike-1 script runner (`SPIKE_SCRIPT`) sends keystrokes with `Window::dispatch_keystroke`. No input went through the OS, and scripted windows did not take focus.
- **Screen content:** the script's `dump` writes the visible text, including scrolled-back lines, to a file.
- **Latency:** the app timestamps each key it sends and the first update with changed rows after it. The daemon logs the same interval from key received to update queued.
- **Throughput:** `/usr/bin/time -p` inside the terminal, plus the script's `waitfor` for the app-side end time.
- **Attach cost:** `swarm-bench` (headless client).

Saved frames (rendered by the app, not screen captures), in `/private/tmp/claude-501/-Users-driesheyninck-personal-swarm/8c80a07b-fb4e-4e28-9cb4-d6d60749af4d/scratchpad/daemon-shots/`:

- `snapshot-86424-0.png`: counter loop right after reattach (colors, bold, underline, cursor)
- `snapshot-87652-0.png`: `top` right after reattach
- `snapshot-91941-0.png`: scrolled 20,000 lines back after reattach
- `snapshot-92284-0.png`: `claude` before quitting the app
- `snapshot-92613-0.png`: `claude` right after reattach
- `snapshot-92613-1.png`: text typed into the `claude` prompt after reattach
