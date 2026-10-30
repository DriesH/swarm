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
| `swarm-core` | Projects, tasks, agents, worktrees, mailbox, state store (SQLite) |
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

### Agent lifecycle

Every agent belongs to a task. The daemon tracks every agent in SQLite (ADR 0005), so none are lost; the goal is that none are forgotten.

- **Task done or archived**: its agents stop and their worktrees are removed. Unmerged changes ask for confirmation first.
- **Quit**: with running agents, ask once: keep running or stop all. Option to remember the choice.
- **Idle stop**: a done or idle agent stops after 30 minutes (setting). Its session ID is kept, so it resumes with `claude --resume`.
- **Menu bar**: shows the running agent count and **Stop all**, also when the app is closed.
- **Daemon exit**: when no agents run and no app is attached.
- **Limit**: maximum concurrent agents, globally and per task (setting). Spawning past it returns an error to the orchestrator.
- **Daemon crash**: closing the PTYs sends SIGHUP to agents. On restart, the daemon reads the PIDs from SQLite, stops processes it no longer manages, and marks those agents "stopped, resumable".
- **Reboot**: same as a crash; agents come back through resume.

### Review

Full-screen view of the task branch diff against the base branch. Ways in:

- **Review +N −N** button in the task header in every layout: one control for the diff count and the way into Review (⌘⇧R, shown in the command palette, not on the button); it becomes the primary button when all workers are done
- The **+N −N** chip on a worker's pane header opens Review filtered to that worker
- **Review** action on the "ready for review" notification
- **View diff** on a worker in the Swarm view
- Command palette

Two sources:

- **Live** (default while workers run): each worker's worktree diff, committed and uncommitted, refreshed as agents edit. Files being edited get an "Editing" pill. A comment is sent right away through the orchestrator to the worker that owns the file. Approve is disabled.
- **Merged** (default when all workers are done): task branch against the base branch. Comments are batched and sent with **Request changes**.

Live comments are anchored to the code text, not line numbers. If the agent rewrites those lines, the comment is marked outdated.

### Notifications

Native macOS notifications through GPUI's `show_system_notification`. Only the orchestrator notifies the user.

| Trigger | Source |
|---|---|
| Needs a decision | `swarm notify` from the orchestrator |
| Permission or question prompt | Claude Code `Notification` hook: `permission_prompt`, `elicitation_dialog`, `agent_needs_input` |
| Ready for review | orchestrator reports all workers done |
| Something broke | `StopFailure` hook, or an agent process exits |

- Skip the notification when the app is focused on that task.
- One notification per task: the task ID is the tag, so a new one replaces the old one.
- Clicking opens the task: "needs you" opens Focus on the orchestrator, "ready for review" opens Review.
- The same signal shows as a dot on the menu bar icon, the "Needs you" pill in the sidebar, and a dock bounce for urgent cases.
- Settings: each trigger on or off, per-project overrides, sound.
- Needs a real `.app` bundle: GPUI disables notifications under `cargo run`.
- No inline reply in v1: GPUI supports action buttons, not text input.

### Project config

`.swarm/config.toml` per repo: setup and teardown scripts, files to copy into worktrees (`.env`, `CLAUDE.local.md`), port range per worktree, PR checks.

## Phases

0. **Plan, design, spikes** ← now
1. `swarm-ui` component library + app shell + theming
2. Daemon, terminals, projects, worktrees and scripts
3. Harness: socket, CLI, MCP, hooks, orchestrator and prompts
4. Layouts: Focus, Grid, Swarm view
5. Review screen and PR panel
6. Menu bar usage and notifications

## Phase 0 spikes

Each spike is a throwaway program in `spikes/` and ends with a pass or fail note in `docs/spikes/`.

1. **Terminal in GPUI**: `libghostty-vt` + PTY + GPUI renderer running `claude`. Pass: smooth scrolling and typing, correct colors, cursor, resize. Fallback: `alacritty_terminal`.
2. **Daemon reattach**: daemon owns the PTY; quit and reopen the app, the agent is still running with its screen intact.
3. **Agent state**: Claude Code hooks report working / waiting / idle to the daemon. Find the Codex equivalent.
4. **Menu bar and notifications**: status item from a GPUI app; read the usage endpoint; show a system notification with an action from a bundled `.app`.
5. **Codex facts**: skip-permissions flag, resume, passing `developer_instructions`.

## Remaining design screens

- Create-worktree sheet and `.swarm/config.toml` scripts
- Settings, including Prompts and Notifications
- Menu bar usage popover
- New task / start orchestrator
- Command palette (⌘K)
