# Swarm plan

Swarm is a native macOS app to run a swarm of coding agents (Claude Code and Codex) in real terminals. One orchestrator agent splits a task, spawns workers in their own worktrees, and talks to them. You watch, review the combined diff, and open the PR.

Design canvas: https://claude.ai/artifact/6r2WfEmYoTDCWCTJckxVkt (direction **A · Graphite**).

## Goals

- Fast and native: GPUI, 120 fps terminals.
- Component library first: every screen is built from `swarm-ui` parts.
- Light and dark themes from one token set.
- Accessible: every control has a role and label for VoiceOver, everything works from the keyboard, and colors meet WCAG AA contrast. GPUI support: see `docs/research/zed-architecture.md`.
- Built the way Zed builds GPUI apps (crate split, `init(cx)` per crate, actions and keymaps, `ui` + `theme` style components).
- Claude Code and Codex on day 1, with an option to skip permission prompts.
- Agents keep running when the app quits.
- Menu bar icon with Claude and Codex subscription usage.
- A full harness (CLI + MCP) so agents can drive Swarm.

## Architecture

Cargo workspace:

| Crate | Role |
|---|---|
| `swarm-ui` | Design tokens, themes and components on raw GPUI |
| `swarm-term` | PTY + `libghostty-vt` + GPUI grid renderer |
| `swarm-core` | Projects, tasks, agents, worktrees, mailbox, state store |
| `swarm-daemon` | Background process that owns PTYs and runs `swarm-core` |
| `swarm-harness` | Unix socket JSON-RPC, `swarm` CLI and MCP server |
| `swarm-app` | Windows, layouts (Focus, Grid, Swarm), Review, PR panel, settings |
| `swarm-usage` | Menu bar icon and usage polling |

Key decisions live in [`docs/adr/`](adr/). Terms live in [`GLOSSARY.md`](../GLOSSARY.md).

### Agent harness

Every agent terminal gets `SWARM_SOCKET`, `SWARM_AGENT_ID` and `SWARM_TASK`.

- Orchestrator: `swarm spawn`, `swarm send`, `swarm read`, `swarm wait`, `swarm merge`, `swarm pr draft`.
- Workers: `swarm ask orchestrator`, `swarm report`.
- The same calls are exposed as MCP tools.

### Agent state

- Claude Code: hooks (`Stop`, `Notification`, `UserPromptSubmit`) report to the daemon.
- Codex: to be decided in spike 3.
- Screen matching is the fallback.

### Prompts

Layers, later wins: built-in → `~/.config/swarm/prompts/` → `.swarm/prompts/`.
Files: `orchestrator.md`, `worker.md`, `review.md`, `pr.md`, `commit.md`.
The harness block is always added first and is not editable.

### Project config

`.swarm/config.toml` per repo: setup and teardown scripts, files to copy into worktrees (`.env`, `CLAUDE.local.md`), port range per worktree, PR checks.

## Phases

0. **Plan, design, spikes** ← now
1. `swarm-ui` component library + app shell + theming
2. Daemon, terminals, projects, worktrees and scripts
3. Harness: socket, CLI, MCP, hooks, orchestrator and prompts
4. Layouts: Focus, Grid, Swarm view
5. Review screen and PR panel
6. Menu bar usage

## Phase 0 spikes

Each spike is a throwaway program in `spikes/` and ends with a pass or fail note in `docs/spikes/`.

1. **Terminal in GPUI**: `libghostty-vt` + PTY + GPUI renderer running `claude`. Pass: smooth scrolling and typing, correct colors, cursor, resize. Fallback: `alacritty_terminal`.
2. **Daemon reattach**: daemon owns the PTY; quit and reopen the app, the agent is still running with its screen intact.
3. **Agent state**: Claude Code hooks report working / waiting / idle to the daemon. Find the Codex equivalent.
4. **Menu bar**: status item from a GPUI app; read the usage endpoint.
5. **Codex facts**: skip-permissions flag, resume, passing `developer_instructions`.

## Remaining design screens

- Create-worktree sheet and `.swarm/config.toml` scripts
- Settings, including Prompts
- Menu bar usage popover
- New task / start orchestrator
- Command palette (⌘K)
