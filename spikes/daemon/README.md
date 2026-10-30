# Daemon spike

Throwaway spike: a daemon owns agent terminals, and a GPUI app attaches to them, detaches and reattaches with the screen intact. Result and notes: [`docs/spikes/daemon.md`](../../docs/spikes/daemon.md).

## Run

`libghostty-vt` 0.2.2 needs **Zig 0.15.x** (0.17 fails). `.cargo/config.toml` already sets `LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast`.

```sh
brew install zig@0.15   # keg-only, does not replace `zig`
PATH="$(brew --prefix zig@0.15)/bin:$PATH" cargo build --release
./target/release/swarm-app
```

The app starts `swarm-daemon` if it is not running, attaches to the first terminal, or creates one that runs your login shell. `cmd-q` quits the app; the terminal keeps running. Start the app again to reattach.

- Socket, log and agent records: `$TMPDIR/swarm-spike/` (override with `SWARM_SPIKE_DIR`).
- `SWARM_NEW=1` always creates a new terminal. `SWARM_CMD="claude"` sets its command.
- Stop the daemon with `pkill -x swarm-daemon`. Its terminals get SIGHUP.

## Architecture

```
swarm-app (GPUI)  ──unix socket, postcard frames──  swarm-daemon
  rows → element                                    client thread (read) + writer thread
  local scrollback cache                            one thread per terminal:
  keys as logical events                              PTY reader → libghostty Terminal → row diff
```

- `crates/proto`: wire types and framing. No GPUI, no libghostty.
- `crates/daemon`: the daemon, plus `swarm-bench` (attach cost for a big screen).
- `crates/app`: the GPUI client. It does not link libghostty, so it builds without Zig.

## Scripted runs

Same as spike 1: `SPIKE_SCRIPT=file` drives the app in-process. Scripted windows open without keyboard focus. Extra commands: `mark`, `waitfor <text>`, `latency <n> <gap ms>`, `report`, `dump <path>`. Build with `--features swarm-app/snapshot` for `snap`.
