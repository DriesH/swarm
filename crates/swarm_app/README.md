# swarm_app

The Swarm app (`swarm` binary). It shows the daemon's projects and tasks. Snapshot and frame-stats builds show fake sample data instead.

## Run

```sh
cargo run -p swarm_app
```

## Run with the daemon

Projects, tasks, agents, user terminals and the setup terminal are real when `swarm-daemon` runs. A real task shows its orchestrator as a live terminal tile; its status ring and the project's "Needs you" marker follow the agents' status.

```sh
cargo build -p swarm_daemon && cargo run -p swarm_app
```

Both binaries land in `target/debug`. The app starts `swarm-daemon` from its own folder when nothing listens, and connects to it. If it cannot connect, the sidebar shows "Daemon not running" with a Retry button, and no project or task appears.

- **Files:** `~/Library/Application Support/Swarm` holds `daemon.sock`, `swarm.db` and `daemon.log`. When that path is too long for a socket, the socket sits in `$TMPDIR/swarm`.
- **Quit the app:** user terminals keep running. Open the app again and they come back.
- **Stop the daemon:** `pkill -x swarm-daemon`. This ends every terminal.
- **Task key:** real tasks use `task:<id>` as the terminal key. Fake sample tasks use `<project name>/<task name>`.
- **Workers:** every agent of a task gets a live tile after the orchestrator. A stopped agent keeps its tile without a terminal.
- **Process terminals:** a process terminal an agent starts gets a tile named by its `name`, else its title. When an agent fixes its size, the tile is sized to those rows and the app never sends `Resize` for it. A successful exit closes the tile after 3 s; a failed exit stays until you close it.
- **Start and Plan tab:** a project with no tasks opens on its Start tab and the app asks the daemon for the Start orchestrator once. The tab reads "Plan" when a task did not come from that orchestrator. It closes when the orchestrator exits. **+ › Plan with orchestrator** opens it again.
- **Tasks and messages:** a task an agent makes gets a tab without taking focus. The model keeps the last 500 messages per task for the Swarm view.
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
