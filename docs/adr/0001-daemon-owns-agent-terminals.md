# The daemon owns agent terminals

Agents must keep running when the app quits or crashes, so a separate background process (the daemon) owns every PTY and all task state. The GPUI app is a client that attaches to it over a unix socket. Running PTYs inside the app would be simpler, but every app restart would kill all agents.

## Consequences

- Terminal emulation state lives in the daemon; the app gets screen snapshots and diffs.
- After a reboot, processes are gone. Agents restart with their saved session ID (`claude --resume`, Codex equivalent).
