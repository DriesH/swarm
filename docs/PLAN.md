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

- **OSC 7501 (Program Status Protocol)** is the main source. Claude Code emits it since 2.1.295 and Codex emits it (`codex-rs/tui/src/terminal_program_status.rs`). States: `idle`, `working`, `done`, `blocked` (`kind=permission|question|auth`), `error`, plus `clear`. The daemon reads it from the PTY stream. `libghostty-vt` 0.2.2 has no API for it, so the daemon parses this OSC itself before feeding bytes to the terminal. Claude Code only sends it after the terminal answers its support query, so the daemon answers that query. Codex (0.163.0+) sends it to any terminal. Details: `docs/spikes/agent-status.md`.
- Claude Code hooks add what OSC 7501 lacks: session ID (for resume), `StopFailure` reasons.
- Screen matching is the last fallback.
- Process terminals that emit OSC 7501 (e.g. a test runner) show their state the same way.

### How work starts

**Opening a project.** The project opens on a **Start** tab with an **orchestrator**: a Claude Code (or Codex) session in the project root on the base branch.

1. The user says what they want.
2. The Start orchestrator splits it into tasks. One request can become one task or several.
3. For each task it calls `task.create`: Swarm makes the task worktree and branch, runs the setup script, opens a tab and starts that task's orchestrator with the Start orchestrator's brief as its first message.
4. When every orchestrator confirms its brief, the Start orchestrator exits and the Start tab closes. Focus moves to the first task tab.

**When the Start tab shows.**

- A project with no active tasks opens on the Start tab.
- A project with active tasks opens on the tab used last. There is no Start tab.
- The Start tab closes itself once every task orchestrator has confirmed its brief. If the request is unclear, the Start orchestrator asks first and the tab stays open until it is done.

**Starting new work later.** The **+** in the tab bar opens a menu with two choices:

- **New task**: a sheet with name, branch from (default: base branch; option: current tab's branch), orchestrator agent, optional first message, run setup script. Swarm creates the worktree and tab and starts one orchestrator.
- **Plan with orchestrator**: opens a Start tab for a bigger request that should be split into several tasks. Only one Start tab can be open per project.

Harness calls for the Start orchestrator:

| Call | What |
|---|---|
| `task.create {name, from?, brief, agent?}` | Creates task worktree, branch, tab and orchestrator; sends `brief` as its first message. Returns the task ID. |
| `task.list` | Lists tasks in the project with status. |
| `task.wait {ids, until: briefed\|done}` | Waits until orchestrators confirm their brief, or finish. |
| `agent.exit` | Ends the caller's own session (used by the Start orchestrator when done). |

### Workspace layout

- **Sidebar**: projects, then the agents and terminals of the open tab.
- **Tabs**: one tab per task, at the top of the window, plus the Start tab while its orchestrator runs. A tab shows the task name and its status. Each task runs in its own task worktree; worker worktrees are not tabs.
- **Right panel**: **Files** (file tree of the task worktree, changed files marked) or **Changes** (changed files grouped by worker, with a **Review** button). The Changes toggle shows the diff count.
- **Views**: Terminals (default), Swarm graph or Review.

### Terminals

Four kinds:

| Kind | Who types | Who resizes | Who kills |
|---|---|---|---|
| **Orchestrator** | user | layout | user |
| **Worker** | output view; user can type in v1 (setting, may be off by default later) | its agent | orchestrator, user |
| **Process** (dev server, tests, any command an agent starts) | nobody by default | the agent that started it | that agent, orchestrator, user |
| **User** | user only | user only | user only |

- **User terminals are off-limits to agents** (ADR 0006): agents cannot read, type into, resize or kill them. The layout treats them as fixed islands.
- **Process terminals**: started with `swarm terminal spawn`. When the process exits or is killed, the terminal closes after a short delay (to show the exit code) and the owner agent and the orchestrator get a `terminal.exited` event with who stopped it (`agent`, `user`, `process`) and the exit code or signal.
- **Layout engine**: tiles fill the space. Priority: user terminals (fixed size), orchestrator, terminals that need attention (`blocked`, `error`), working agents, process terminals, done or idle agents. Each tile has a minimum size; tiles that don't fit move to the **tray**: chips on the left of the status bar, each with a restore icon. A tile that needs attention is promoted out of the tray. The user can pin or minimize any tile.
- When the user resizes a user terminal, the other tiles reflow around it.

### Terminal harness (CLI + MCP)

| Call | Who | What |
|---|---|---|
| `terminal.spawn {cmd, cwd?, size?, name?}` | any agent | Starts a process terminal. `size` is `{cols, rows}` or `small` / `medium` / `large`. Returns an ID. |
| `terminal.resize {id, size}` | owner agent | Asks for a new size. The layout grants the closest size that fits and returns it. |
| `terminal.read {id, lines?}` | owner agent, orchestrator | Reads the screen or the last lines of output. |
| `terminal.wait {id, until: exit\|output-match, pattern?}` | owner agent, orchestrator | Waits for exit or for a line such as `ready in`. |
| `terminal.kill {id, signal?}` | owner agent, orchestrator | Stops the process; the terminal closes. |
| `terminal.list` | any agent | Lists terminals the caller may see (never user terminals). |
| event `terminal.exited {id, by, code\|signal}` | to owner + orchestrator | Sent when a process terminal ends for any reason. |

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

A third view next to Terminals and Swarm in the header switch (**Terminals | Swarm | Review**). It fills the main area; the sidebar and tabs stay. Left: changed files grouped by worker or folder. Right: the diff with inline comments. Ways in:

- **Review** in the view switch (⌘⇧R)
- **Review** button and any file in the Changes panel (the **Changes +N −N** header button opens that panel)
- **Review** action on the "ready for review" notification
- **View diff** on a worker in the Swarm view (Review filtered to that worker)
- Command palette

Two sources:

- **Live** (default while workers run): each worker's worktree diff, committed and uncommitted, refreshed as agents edit. Files being edited get an "Editing" pill. A comment is sent right away through the orchestrator to the worker that owns the file. Approve is disabled.
- **Merged** (default when all workers are done): task branch against the base branch. Comments are batched and sent with **Request changes**.

Live comments are anchored to the code text, not line numbers. If the agent rewrites those lines, the comment is marked outdated.

### Pull requests

A PR can only be created after the task is approved in Review. This makes the user read the code first.

1. **Review → Approve.** The Approve button turns into **Create PR**.
2. **Create PR** opens the PR panel on the right (a header toggle next to Files and Changes): title and description drafted by the orchestrator (`pr.md` prompt), **local checks** from `.swarm/config.toml` (run in the task worktree; the PR can only be created when they pass), open as draft. Uses `gh`; pushes the branch first.
3. **After creation** the header button shows the live PR state, e.g. **#42 · checks running** or **#42 · ready to merge**. Its panel shows **GitHub checks** (CI on the pushed branch, read through `gh`), reviewers, Ready for review, Merge, and after-merge options (remove agent worktrees, close the tab).

Other ways in: ⌘K "Create pull request"; `swarm pr draft` from the orchestrator opens the panel with its draft (still only after approval).

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

### System check

Swarm depends on tools it does not ship. It checks them on first launch (welcome screen) and in **Settings › General › System**:

| Tool | Check | Fix |
|---|---|---|
| Git | `git --version` | Point to Xcode Command Line Tools install |
| Claude Code | `claude --version`, signed in | Install / sign in in a user terminal |
| Codex | `codex --version`, signed in | Install / sign in in a user terminal |
| GitHub CLI | `gh --version` | Install: `brew install gh` if Homebrew exists, else download the official release into `~/Library/Application Support/Swarm/bin/` (Swarm-only, nothing system-wide) |
| GitHub account | `gh auth status` | `gh auth login --web` in a user terminal |
| Notifications | macOS authorization status | Ask for permission |

- Never install silently: every fix is a button, and installs run in a visible user terminal (agents can't see it, ADR 0006).
- Missing optional tools disable only their features: no `gh` means Review and Approve work, and **Create PR** becomes **Set up GitHub**.
- PR features show only when `origin` is on GitHub. GitLab (`glab`) later.
- Agents get the same `gh` on their `PATH`.

### Easter eggs

Small, rare and never in the way. All respect **Reduce Motion**, stay silent for VoiceOver, and can be turned off in Settings › Appearance.

- **Buzzing menu bar bee**: the wings flicker while agents work; still when idle.
- **Honeycomb**: a faint hex pattern on the welcome screen and empty states, and as an option for the Swarm view background.
- **Welcome bee**: a single small bee on the welcome screen.
- **"Honey's ready"**: the toast when all workers of a task finish shows a tiny honeycomb filling up.
- **Queen bee**: after the 100th merged PR, the Start orchestrator gets a crown in the sidebar for a day.
- **`swarm buzz`**: hidden CLI command that prints an ASCII bee and the swarm's live stats.
- **⌘K "buzz"**: a few bees fly across the window once.
- **Honey accent**: an amber accent color in Settings › Appearance.

### Project config

`.swarm/config.toml` per repo, edited by hand or in **Settings › Projects › <project>**:

```toml
[worktrees]
base_branch = "main"
folder = ".worktrees"
ports = { range = "5170-5199", per_worktree = 3 }
copy = [".env", "CLAUDE.local.md"]
setup = """
cargo fetch
pnpm --dir docs install --frozen-lockfile
"""
teardown = ""

[checks]
before_pr = ["cargo test --workspace", "cargo clippy -- -D warnings", "cargo fmt --check"]
required = true
```

**Checks before PR**

- Run in the task worktree when the user clicks Create PR; shown as "Local checks" in the PR panel.
- `required = true`: the PR can only be created when all pass. `false`: failures warn and **Create PR anyway** asks for confirmation.
- No checks configured: the section is hidden and Create PR goes straight through.
- An existing git `pre-push` hook (husky, lefthook, plain) is detected and shown; Swarm does not duplicate it. It still runs during the push, and its output is shown if it fails.
- The orchestrator runs the same checks before it reports a task done.
- **Run checks now** in project settings runs them on the current tab's worktree.

## Phases

0. **Plan, design, spikes** ← now
1. `swarm-ui` component library + app shell + theming
2. Daemon, terminals, projects, worktrees and scripts
3. Harness: socket, CLI, MCP, hooks, orchestrator and prompts
4. Layouts: Focus, Grid, Swarm view
5. Review screen and PR panel
6. Menu bar usage and notifications

## Phase 0 spikes

Reports in `docs/spikes/`. Code in `spikes/` (throwaway).

| Spike | Result | Report |
|---|---|---|
| 1. Terminal in GPUI (`libghostty-vt` + PTY + GPUI element) | Pass | `terminal.md` |
| 2. Daemon owns PTYs, app reattaches, crash recovery | Pass | `daemon.md` |
| 3. Agent status (OSC 7501 + hooks), resume, Codex facts | Pass | `agent-status.md` |
| 4. Menu bar, usage, notifications | Mostly pass; notification actions untested (permission) | `menubar.md` |

## Lessons for the real build

**Build**

- `libghostty-vt` 0.2.2 needs Zig 0.15 (not 0.17). Set `LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast` in `.cargo/config.toml`, or Ghostty builds in Debug mode and is far too slow.
- `gpui_platform` needs the `font-kit` feature or no text is drawn. Use `runtime_shaders` when the Metal toolchain is missing.
- Pin `gpui` and `gpui_platform` to one Zed git rev for the whole workspace.

**Terminal and daemon**

- Batch PTY output: one shared buffer and one wake-up; send the first change at once, then at most one update per 8 ms.
- Ghostty's `max_scrollback` is in bytes, not lines.
- Preload about 1,000 scrollback lines on attach; fetch older lines on scroll (about 6.5 µs per line to read).
- Support the Kitty keyboard protocol (fish 4 turns it on).
- Open the daemon socket before crash recovery, so the app never waits.
- Start agents with an allow-listed environment; strip inherited `CLAUDE_CODE_*` and similar variables.
- macOS socket paths must stay under 104 bytes.
- Apps started from Finder get a minimal `PATH`: look up `claude`, `codex`, `gh` and `git` in known install folders.

**Agents**

- OSC 7501 is the main status source. Claude Code only sends it after the terminal answers its support query; the daemon answers. Codex (0.163.0+) always sends it.
- Hooks miss events (Esc in Claude Code fires none; Codex has no error hook), so they only add session IDs and failure reasons.
- Claude Code: pick the session ID up front with `--session-id`; resume with `--resume <id>`. Hooks via `--settings <file>`, never by editing the user's settings.
- Codex: `--dangerously-bypass-approvals-and-sandbox` to skip approvals; `-c developer_instructions=…` adds to AGENTS.md; `codex resume <id>` (pass `-m` again for `codex exec resume`); the session ID only arrives at the first prompt; hooks passed with `-c` need their trust hash passed too.
- Start Codex with `-c check_for_update_on_startup=false`. Never type into an agent before it reports `idle`: startup prompts swallow input (a spike accepted a Codex update this way).

**Usage and menu bar**

- Claude usage: read `rate_limits` from the Claude Code status line data of agents Swarm runs. Fall back to `/api/oauth/usage` at most every 5 minutes, honor `Retry-After` and store it on disk. The endpoint shares a budget with the user's own sessions and returns 429 often.
- Claude token (fallback only): Keychain item `Claude Code-credentials`, read through `/usr/bin/security`; never logged or stored.
- Codex usage: `codex app-server` → `account/rateLimits/read`. Codex keeps its own token.
- Menu bar popover is a GPUI window under an `NSStatusItem`. Compute its position before opening it.
- With no main window open, switch to no Dock icon; the menu bar keeps working.

**Notifications**

- Need a real `.app` bundle with a bundle ID.
- macOS may deny permission without showing a prompt. The system check must detect "denied" and offer a button that opens System Settings › Notifications.

## Remaining design screens

- Create-worktree sheet and `.swarm/config.toml` scripts
- Settings: Terminals, Prompts, Worktrees pages (Agents and Notifications are done)
- Menu bar usage popover
- New task / start orchestrator
- Command palette (⌘K)
