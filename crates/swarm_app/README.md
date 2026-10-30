# swarm_app

The Swarm app (`swarm` binary). Right now it shows the window layout with fake data.

## Run

```sh
cargo run -p swarm_app
```

## Snapshot

Saves `workspace-light.png` and `workspace-dark.png`, then exits:

```sh
SWARM_APP_SNAPSHOT_DIR=/path/to/dir cargo run -p swarm_app --features snapshot
```

## Window drag

The windows use a transparent titlebar with `app_owns_titlebar_drag`, so macOS does not move the window from the header strip. Only `window_drag::drag_region` areas (empty header space, sidebar and settings top rows) start a move: a left press, then a drag, calls `Window::start_window_move`. A double-click calls `Window::titlebar_double_click`, which follows the macOS double-click setting. Keep buttons, tabs and inputs out of these regions.
