# Agent status spike

Throwaway spike: the daemon reports each agent's status (`idle`, `working`, `done`, `blocked{kind}`, `error`, `unknown`) and its session ID for Claude Code and Codex. Result and notes: [`docs/spikes/agent-status.md`](../../docs/spikes/agent-status.md).

Built on the spike-2 daemon (`spikes/daemon`), without the GPUI app. A small CLI, `swarm-ctl`, drives it.

## Build

`libghostty-vt` 0.2.2 needs **Zig 0.15.x**. `.cargo/config.toml` sets `LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast`.

```sh
PATH="$(brew --prefix zig@0.15)/bin:$PATH" cargo build --release
PATH="$(brew --prefix zig@0.15)/bin:$PATH" cargo test --release   # OSC 7501 parser tests
```

## Run

```sh
export SWARM_SPIKE_DIR=/tmp/sas          # records, logs, Claude settings file
export SWARM_SOCKET=/tmp/sas.sock        # macOS socket paths must be under 104 bytes
./target/release/swarm-daemon &
./target/release/swarm-ctl watch &       # one line per status report
id=$(./target/release/swarm-ctl create --cwd ~/some/repo -- claude)
./target/release/swarm-ctl send $id 'Reply with the word OK and nothing else.'
./target/release/swarm-ctl wait $id done 60
./target/release/swarm-ctl list
./target/release/swarm-ctl kill $id
./target/release/swarm-ctl resume $id    # new terminal: claude --resume <session id>
```

`swarm-ctl` commands: `create`, `resume`, `send`, `keys` (raw bytes, `\r`, `\e`, `\xNN`), `screen`, `list`, `watch`, `wait`, `kill`, and `hook` (used by the agents' hooks).

Debug switches for the daemon:

- `SWARM_RAW_LOG=1` writes every PTY byte to `$SWARM_SPIKE_DIR/term-<id>.raw`.
- `SWARM_NO_7501_REPLY=1` does not answer the `OSC 7501 ; ?` query, to test what a program does without it.

## How it works

- `crates/daemon/src/osc7501.rs`: streaming OSC 7501 parser (revision 0.4 of the spec). It runs on the terminal thread before the bytes go to libghostty, keeps state across reads, and answers the feature query.
- `crates/daemon/src/agent.rs`: merges OSC 7501, hook reports and process exit into one status per terminal and streams changes to watchers. Also builds the Claude Code settings file and the Codex `-c` hook flags.
- Claude Code is started with `--settings <file>` that adds command hooks. Codex is started with `-c hooks.<Event>=…` plus `-c hooks.state=…` with the trust hash of each hook, so the hooks run without `--dangerously-bypass-hook-trust`. Both run `swarm-ctl hook`, which sends the hook JSON to `$SWARM_SOCKET` with `$SWARM_AGENT_ID`.
- The agent record (`agents.json`) keeps kind, cwd and session ID, so `resume` can restart a stopped agent.
