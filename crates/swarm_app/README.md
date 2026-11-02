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
- **Process terminals:** a process terminal an agent starts gets a tile named by its `name`, else its title. It is a small tile below the workers (8 rows) and grows only when it needs attention; the terminal follows its tile, whatever size the agent asked for. A successful exit closes the tile after 3 s; a failed exit stays until you close it.
- **Start and Plan tab:** a project with no tasks opens on its Start tab, which shows the plan editor and starts nothing. You write the plan in Markdown, pick Claude Code or Codex (the default comes from the settings) and press **Send plan** (⌘↩). Only then does the daemon start the Start orchestrator, with the plan as its first message, and the tab shows its terminal. Send plan is off for an empty plan and for the bare template; the text is off while the plan goes out, and a refusal shows under the editor and keeps the text. The daemon keeps the plan draft: the app loads it when the editor first shows (and again when the daemon is back after a failed load), saves it 1 s after the last edit and again on quit, and the template fills an empty draft. Nothing is saved before the draft loaded, while the plan goes out or while the Start orchestrator runs; a refused save shows under the editor. A started plan drops the editor, so the next one loads the draft again. The text box has a `line` border that turns `accent` while the text has focus. The tab reads "Plan" when a task did not come from that orchestrator. It closes when the orchestrator exits, unless it holds user terminals: it then stays, shows the plan editor and keeps them. **+ › Plan with orchestrator** opens it again, on the plan editor.
- **User terminals:** the tile header shows the User icon and "Mine" after the name. The sidebar row shows the Terminal icon, a dimmed User icon and a close button. Both close buttons ask "Close terminal …?" while the program in the terminal runs, and close at once when it ended. "Do not show this again" sets `terminals.confirm_close = false` in `settings.toml`; Settings › Terminals has the switch "Ask before closing a terminal". Closes the app or the daemon starts never ask.
- **Terminals on the Start tab:** a user terminal (and a process terminal, if any) sits in a band under the main area, which is the plan editor or the Start orchestrator's tile. The band is a small tile high, in the same column as the main area. Its tiles share the width and are never narrower than the smallest tile; the rest go to the tray. User terminals come before process terminals, and the terminal you focused last has the first claim on a place, so picking a tile in the tray or the sidebar places it. A window too short to leave the main area the smallest tile's height sends the whole band to the tray. A user terminal keeps the band height there and has no resize handle. `views::StartRegions` holds the layout math for the layout and the plan editor.
- **Tasks and messages:** a task an agent makes gets a tab without taking focus. The model keeps the last 500 messages per task for the Swarm view.
- **Usage:** the sidebar card shows the usage windows of the Claude and Codex subscriptions the daemon reports, each with a pace tick. A window is named by its length: Session under a day, Weekly, Monthly. The menu bar popover and Settings › Usage show a card per agent kind with the reset time and a warning. All three show the plan (only when the CLI reports one) and the age of old numbers. The warning is "Limit in …" when the use so far reaches 100% before the window resets, "Limit likely reached" when numbers read some time ago put that moment in the past, and "Limit reached" at 100%. The rate is the one at the time of the reading; the tick follows the clock. The app asks once after it connects and after each reconnect, then follows the daemon's changes. The views redo their time text every 30 s. The card hides when there is nothing to show.
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

## Menu bar

The menus are defined in `src/app_menus.rs`. The shortcut next to an item comes from `actions::shortcuts`.
