# The daemon owns state in SQLite

Task and agent state lives in one SQLite database at `~/Library/Application Support/Swarm/swarm.db`, and only the daemon reads or writes it. The app gets state over the daemon socket. One writer keeps a single source of truth and avoids locking between processes. Zed also keeps its state in SQLite; we use `rusqlite` (bundled) because Zed's `sqlez` is not under a license we can copy.

## What goes where

- **SQLite**: projects, tasks, agents (kind, status, PID, session ID, worktree, branch), messages, review comments, notification history.
- **Files**: user settings and prompts in `~/.config/swarm/`, project config and prompts in `.swarm/` in the repo. People edit and share these by hand.
- **Daemon memory**: live terminal screens and scrollback.
- **Not ours**: agent conversations stay in Claude Code and Codex storage; we keep only the session ID.

## Consequences

- WAL mode, migrations run at daemon start.
- An agent row, with its PID, is written before its process starts, so the daemon can find leftover processes after a crash.
