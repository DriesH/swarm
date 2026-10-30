# Spike 1: terminal in GPUI

**Verdict: pass.** `libghostty-vt` + `portable-pty` + a custom GPUI element gives a working, fast terminal that runs `claude` correctly. No fallback to `alacritty_terminal` was needed.

One limit: all input was driven in-process through GPUI's key dispatch (see "How it was tested"). Do a two-minute hands-on check with a real keyboard before closing this spike.

Code: [`spikes/terminal`](../../spikes/terminal) (about 1,100 lines).

## Pass criteria

| Criterion | Result | Evidence |
|---|---|---|
| Typing: letters, Enter, Backspace, arrows, Ctrl-C, Tab | Pass | Line editing with left/right and Backspace, Tab completion (`/usr/li` → `/usr/lib`), Up for history, Ctrl-C stops `sleep 30` and clears a line. |
| 16 / 256 / truecolor, bold, italic, cursor | Pass | All three color modes, plus bold, italic, bold-italic, underline, strikethrough, inverse and faint. Block cursor with the glyph redrawn in inverse. Bar and underline cursors follow DECSCUSR. The cursor is hollow when the window is not focused. |
| Resize resizes the PTY and the grid | Pass | 960×600 → `stty size` 34 120. 640×400 → 22 79. 1200×800 → 46 151. A 150-char line reflows. |
| Fast output stays smooth | Pass | See the performance table. `seq 1 200000` is within 20% of a bare PTY drain with no rendering. Frame CPU stays under 1 ms on average. |
| Scrollback with the mouse wheel | Pass | Scrolling 20 lines up shows the history. Typing jumps back to the bottom. |
| `claude` TUI | Pass | `claude --version` → 2.1.314. The full TUI renders correctly: logo block art, colors, dim text, rules, status line and the prompt cursor. Typing and arrows inside the prompt work, and Ctrl-C exits. No prompt was sent. |

Screenshots (rendered frames, not screen captures):

- `/private/tmp/claude-501/-Users-driesheyninck-personal-swarm/8c80a07b-fb4e-4e28-9cb4-d6d60749af4d/scratchpad/final-colors/snapshot-0.png`: color and style test
- `/private/tmp/claude-501/-Users-driesheyninck-personal-swarm/8c80a07b-fb4e-4e28-9cb4-d6d60749af4d/scratchpad/shots/snapshot-0.png`: `claude` TUI

## Pinned versions

| Thing | Version |
|---|---|
| `gpui`, `gpui_platform` | Zed git rev `70b1b91b73ee12930b4e3375d68657e9591b1921` (2026-10-30). The crate says 0.2.2, but crates.io 0.2.2 is stale. |
| `gpui_platform` features | `font-kit`, `runtime_shaders` (see gotchas) |
| `[patch.crates-io] async-task` | `zed-industries/async-task` rev `015845a1…`, copied from Zed's workspace. Not tested without it. |
| `libghostty-vt` / `-sys` | 0.2.2 (crates.io). Builds Ghostty commit `a887df42` from source. |
| Zig | **0.15.2** (`brew install zig@0.15`, keg-only). Zig 0.17.0 fails. |
| `portable-pty` | 0.9.0 |
| `flume` | 0.12 |
| Rust | 1.99.0 |
| macOS | Darwin 27.0, Apple silicon |

## Performance

Window: 960×600 at 2× scale, 120×34 grid, Menlo 13 px (Geist Mono is not installed).

| Workload | Spike (wall time) | Bare PTY drain, no terminal |
|---|---|---|
| `seq 1 200000` (1.5 MB, one write per line) | 0.27 s | 0.23 s |
| `cat` 20 MB plain text | 0.12 s | 0.10 s |
| `cat` 20 MB with heavy SGR (16, 256, truecolor, bold, italic on every line) | 0.14 s | 0.12 s |

- **Parse speed** (time spent in `vt_write`): about 500–900 MB/s for plain text and 125–140 MB/s for SGR-heavy text.
- **Frame CPU** (layout + paint, GPU time not included):
  - average 0.1–0.5 ms
  - worst 2–4 ms during floods
  - one 10 ms frame when `claude` starts
- **Rows rebuilt per frame**: 1–7 while typing, because only dirty rows and the cursor row are rebuilt. All rows are rebuilt on a full redraw.
- **Paint calls per second** during floods: 150–280. This counts paint calls, not frames shown on screen.

The first design was 4× slower on `seq`: 0.92 s. It sent each PTY read as its own channel message to the UI thread. The fix was a shared buffer:

- The reader thread appends to the buffer and sends one wake-up.
- The UI takes the whole buffer at once.
- The reader blocks above 1 MB, so a fast producer slows down to match the parser.

## Gotchas

1. **`libghostty-vt` 0.2.2 needs Zig 0.15, not 0.17.**
   - Its pinned Ghostty commit fails on 0.17 (`@cImport` and `b.build_root` errors, "requires v0.15.2").
   - The libghostty-rs `main` branch has moved to Zig 0.17 and a `ZIG` env var. Its API also differs a lot from 0.2.2: new `snapshot`, `io` and `search` modules. Expect churn on the next release.
2. **Cargo debuginfo builds Ghostty in Zig Debug mode.**
   - `build.rs` uses `Debug` when Cargo sets `DEBUG=true`, and `debug = "limited"` in `[profile.release]` is enough to trigger that.
   - In Debug mode, `seq 1 200000` did not finish in 30 s: the time went to `verifyIntegrity` and `DebugAllocator`.
   - Fix: never ship debuginfo for that crate, or set `LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast`.
3. **The `libghostty-vt-sys` build clones Ghostty from GitHub at build time.** Offline or CI builds need `GHOSTTY_SOURCE_DIR` or `GHOSTTY_ZIG_SYSTEM_DIR`.
4. **Without the `gpui_platform` `font-kit` feature, GPUI draws no text on macOS.**
   - It only logs a warning, and you see that warning only if a `log` logger is installed.
   - Quads and underlines still render, so the problem looks like a broken renderer.
5. **The GPUI build needs the Metal Toolchain**, which is missing here (`xcodebuild -downloadComponent MetalToolchain`). `runtime_shaders` works around it by compiling shaders at startup. For the product, install the toolchain and drop `runtime_shaders`.
6. **`Window::render_to_image` (test-support) throws an NSException on macOS 14+.**
   - It calls `nextDrawable` while `CAMetalDisplayLink` is attached.
   - Workaround: minimize the window first. That detaches the link, and the last scene is still there.
   - `screencapture` failed because there is no screen recording permission.
7. **The window could not be focused from the background.**
   - `osascript` keystrokes went to the frontmost app instead.
   - During a first attempt, a path to a harmless color script, Enter and Cmd-S were typed into the frontmost app (Muxy) by mistake. All later input was driven in-process.
8. **`WindowHandle<V>::update` leases the root view.**
   - If you dispatch a keystroke inside it, the view's key handler panics with "already being updated".
   - Use `AnyWindowHandle::update`.
9. **GPUI renamed `ScrollWheelEvent` to `ScrollEvent` and `on_scroll_wheel` to `on_scroll`.** The old names are deprecated.
10. **`shape_line(force_width)` puts glyphs one cell apart**, so a wide character (CJK, emoji) must end a text batch. Zed does the same.
11. **There is no GPUI-to-ghostty key map.**
    - It is a hand-written table.
    - Shifted symbols arrive as their own key name (for example `!`). They are sent as `Unidentified` plus UTF-8 text.
    - End did not move the cursor in plain zsh. This is probably zsh having no binding for `CSI F`. Not investigated.
12. **Environment leaks into agents.**
    - Started from a Claude Code session, the PTY inherited `CLAUDE_CODE_CHILD_SESSION`, and `claude` showed "Transcript saving is off".
    - The daemon must spawn agents with a clean environment.
13. **`$SHELL` wins over the passwd shell.**
    - `portable-pty` uses `$SHELL` first (here `/bin/zsh`). The passwd login shell (fish) is only a fallback.
    - A launchd daemon may have a different `$SHELL`. Pick the shell on purpose.
14. **Box-drawing glyphs do not connect at a 1.3 line height.** Draw box and block characters procedurally, as Ghostty and Zed do.

## What to change for the real `swarm-term`

Keep:

- The element design:
  - one `shape_line` per same-style run, with `force_width = cell width`
  - background spans merged per row
  - the cursor painted last
- The dirty-row cache. It uses `RowIteration::dirty` and `Snapshot::dirty`, and also rebuilds the cursor row.
- The renderer reads plain row data (`RowLayout`), not libghostty, so it does not need to know where that data comes from.
- Coalesced reads with backpressure.
- Answers to DA1 and XTVERSION (`on_device_attributes`, `on_xtversion`), and `on_pty_write` for query replies.

Change:

- **Build:**
  - Pin Zig 0.15 for CI and developers.
  - Add `LIBGHOSTTY_VT_SYS_OPTIMIZE = "ReleaseFast"` to `.cargo/config.toml` `[env]`, so dev builds are usable too.
  - Vendor the Ghostty source.
- **Input:**
  - Add a GPUI `InputHandler` for IME and dead keys.
  - Add the libghostty mouse encoder for apps that request mouse tracking, and its selection and gesture API for copy.
  - Use the libghostty `paste` module.
  - Report focus (mode 1004).
- **Rendering:**
  - Draw box and block glyphs procedurally.
  - Add cursor blink, hyperlinks (OSC 8) and minimum contrast.
  - Use Geist Mono once it ships with the app.
- **Metadata for agent state:** `on_title_changed` and `on_pwd_changed` give cheap signals.

### Fit with the daemon (ADR 0001)

All libghostty types are `!Send`, so each `Terminal` stays on one thread.

- **Daemon:** one thread per agent terminal owns:
  - the PTY reader
  - `Terminal` and `vt_write`
  - the `RenderState`

  The daemon turns dirty rows into plain row data: text runs with fg, bg and flags, plus cursor and colors. It sends that row data to the app as diffs, and sends a full grid when the app attaches. This matches the ADR's "screen snapshots and diffs".
- **App:**
  - keeps the last grid per terminal
  - paints it with the element from this spike, with almost no change
  - sends keys to the daemon

  Key encoding needs the terminal modes (`set_options_from_terminal`), so encode keys in the daemon. The app sends logical key events (key, mods, text).
- **Scrollback:** the app sends viewport scroll requests. The daemon calls `scroll_viewport` and sends the new rows.
- **Alternative for spike 2:**
  - The app runs its own libghostty `Terminal`, fed by the raw byte stream.
  - On attach, the daemon sends a VT dump of the current state. `fmt::Formatter` can emit VT with modes, cursor and palette.
  - This is less protocol, but parsing happens twice and fidelity on attach is unproven.
  - Spike 2 should try the diff approach first.

## How it was tested

- **Input:** a script runner in the app (`SPIKE_SCRIPT`) sends keystrokes with `Window::dispatch_keystroke`. This is the same path a real key press takes after macOS turns it into a GPUI `Keystroke`. The macOS `NSEvent` → `Keystroke` step itself was not tested.
- **Screenshots:** `snap` saves the rendered frame with `render_to_image` (`snapshot` feature).
- **Timing:** wall times come from `/usr/bin/time -p` inside the terminal. Frame and parse numbers come from the spike's stats output.
- **Baseline:** a Python `pty.fork` loop that drains output and discards it.
