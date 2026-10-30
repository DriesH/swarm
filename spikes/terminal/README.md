# Terminal spike

Throwaway spike: a real terminal in a GPUI window, using `libghostty-vt` for emulation and `portable-pty` for the PTY. Result and notes: [`docs/spikes/terminal.md`](../../docs/spikes/terminal.md).

## Run

`libghostty-vt` 0.2.2 builds Ghostty with Zig and needs **Zig 0.15.x** (0.17 fails). Install it next to your normal Zig:

```sh
brew install zig@0.15   # keg-only, does not replace `zig`
PATH="$(brew --prefix zig@0.15)/bin:$PATH" cargo run --release
```

Always use `--release`. A debug build compiles Ghostty in Zig `Debug` mode, which is too slow to use.

The window runs your `$SHELL` as a login shell. `cmd-v` pastes, `cmd-q` quits.

## Scripted runs

For tests without keyboard focus or accessibility rights, set `SPIKE_SCRIPT` to a file of commands:

```
wait 2000
type ls -la
key enter
key ctrl-c
resize 700 400
scroll 20
snap
quit
```

`snap` saves a PNG of the rendered frame. It needs the `snapshot` feature:

```sh
SPIKE_SCRIPT=test.script SPIKE_SNAPSHOT_DIR=/tmp/shots \
  PATH="$(brew --prefix zig@0.15)/bin:$PATH" cargo run --release --features snapshot
```

The app prints frame and parse stats to stderr once per second.

## What works

- Typing: letters, Enter, Backspace, arrows, Tab, Ctrl-C, history, paste
- 16, 256 and truecolor; bold, italic, underline, strikethrough, inverse, faint
- Block, bar and underline cursor (hollow when unfocused)
- Wide characters and emoji
- Window resize resizes the PTY and the grid (with reflow)
- Scrollback with the mouse wheel
- `claude` TUI

## Not done

Selection and copy, mouse reporting to apps, IME, cursor blink, hyperlinks, Kitty images, procedural box-drawing glyphs.
