# swarm_app

The Swarm app (`swarm` binary). It shows the window layout with fake data, or the daemon's projects and tasks when a daemon answers.

## Run

```sh
cargo run -p swarm_app
```

## Run with the daemon

Projects, tasks, user terminals and the setup terminal are real when `swarm-daemon` runs. Agents and workers do not exist yet, so a real task has no orchestrator tile.

```sh
cargo build -p swarm_daemon && cargo run -p swarm_app
```

Both binaries land in `target/debug`. The app starts `swarm-daemon` from its own folder when nothing listens, and connects to it. If it cannot connect, the app keeps the fake sample data.

- **Files:** `~/Library/Application Support/Swarm` holds `daemon.sock`, `swarm.db` and `daemon.log`. When that path is too long for a socket, the socket sits in `$TMPDIR/swarm`.
- **Quit the app:** user terminals keep running. Open the app again and they come back.
- **Stop the daemon:** `pkill -x swarm-daemon`. This ends every terminal.
- **Old sample terminals:** user terminals opened with sample data before any project existed keep running in the daemon and have no tile. Stop them with `pkill -x swarm-daemon`.
- **Task key:** real tasks use `task:<id>` as the terminal key. Fake sample tasks use `<project name>/<task name>`.
- **Add project:** the `+` next to Projects opens a folder picker. Errors show in the sidebar until dismissed.
- Snapshot and frame-stats runs never connect.

## Snapshot

Saves `workspace-light.png` and `workspace-dark.png`, then exits:

```sh
SWARM_APP_SNAPSHOT_DIR=/path/to/dir cargo run -p swarm_app --features snapshot
```

## Frame stats

Replays a scripted session in a release build, logs frame times (p50, p90, p99, max, frames over 8 ms), then exits:

```sh
RUST_LOG=info cargo run --release -p swarm_app --features frame-stats
```

Shows GPUI's live frame overlay in the normal app:

```sh
SWARM_FRAME_OVERLAY=1 cargo run --release -p swarm_app --features frame-stats
```

## Window drag

The windows use a transparent titlebar with `app_owns_titlebar_drag`, so macOS does not move the window from the header strip. Only `window_drag::drag_region` areas (empty header space, sidebar and settings top rows) start a move: a left press, then a drag, calls `Window::start_window_move`. A double-click calls `Window::titlebar_double_click`, which follows the macOS double-click setting. Keep buttons, tabs and inputs out of these regions.
