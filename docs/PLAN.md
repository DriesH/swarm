# Swarm plan

Swarm is a native macOS app to run a swarm of coding agents (Claude Code and Codex) in real terminals. One orchestrator agent splits a task, spawns workers that share the task worktree (or get their own when it has a reason), and talks to them. You watch, review the combined diff, and open the PR.

Design canvas: https://claude.ai/artifact/6r2WfEmYoTDCWCTJckxVkt (direction **A · Graphite**).

## Goals

- Fast and native: GPUI, 120 fps terminals.
- Component library first: every screen is built from `swarm_ui` parts.
- Light and dark themes from one token set.
- Accessible: every control has a role and label for VoiceOver, everything works from the keyboard, and colors meet WCAG AA contrast. GPUI support: see `docs/research/zed-architecture.md`.
- Built the way Zed builds GPUI apps (crate split, `init(cx)` per crate, actions and keymaps, `ui` + `theme` style components).
- Claude Code and Codex on day 1, with an option to skip permission prompts.
- Agents keep running when the app quits.
- Menu bar icon with Claude and Codex subscription usage.
- A full harness (CLI + MCP) so agents can drive Swarm.
- Calm UI: screens show labels, values and status only. Explanations of how things work live on the docs website.

## Architecture

Cargo workspace:

| Crate | Role |
|---|---|
| `swarm_ui` | Theme tokens (Light / Dark / System), fonts, icons and components on raw GPUI |
| `gallery` | Component gallery app (`swarm-gallery`) |
| `swarm_layout` | Tile layout engine: where each tile goes, what goes to the tray (pure, no GPUI) |
| `swarm_term` | Screen rows, input and the channel pair between emulator and view (plain data, no GPUI, no libghostty) |
| `swarm_term_vt` | Emulator: PTY + `libghostty-vt` on their own threads, turns output into rows. Only the daemon and the gallery link it |
| `swarm_term_view` | GPUI view and grid element that paint the rows and send keys, IME text and resizes |
| `swarm_core` | Projects, tasks, agents, worktrees, messages, state in SQLite |
| `swarm_protocol` | Socket protocol between daemon and app (postcard frames) and the app's client. No libghostty |
| `swarm_daemon` | Background process that owns PTYs and runs `swarm_core` |
| `swarm_settings` | User settings in `~/.config/swarm/settings.toml` (agents, default models, terminal typing), shared by app and daemon |
| `swarm_cli` | The `swarm` CLI agents use (hooks now; spawn, send, ask and MCP next) over the daemon's agent socket (ADR 0008) |
| `swarm_app` | Window, sidebar, tabs, Terminals / Swarm / Review views, panels, settings |
| `swarm_usage` | Reads Claude and Codex subscription usage. No GPUI: the daemon polls, the app's menu bar shows it |

Crate names use snake_case, like Zed. Key decisions live in [`docs/adr/`](adr/). Terms live in [`GLOSSARY.md`](../GLOSSARY.md).

### Agent harness

Every agent terminal gets `SWARM_SOCKET`, `SWARM_AGENT_ID`, `SWARM_TASK` and its own `SWARM_TOKEN`, and finds `swarm` on its `PATH`.

- Orchestrator: `swarm spawn` (the worker shares the task worktree; `--own-worktree "<reason>"` gives it its own worktree and branch), `swarm send`, `swarm agents`, `swarm merge` (only for a worker with its own worktree), `swarm commit`, `swarm notify` (asks the user for attention), `swarm confirm` (the brief is understood), `swarm pr draft` (phase 6, only after the user approved the review).
- Workers: `swarm ask`, `swarm report`, `swarm commit -m <message> <path>...`. `commit` commits only the named files in the caller's worktree, with a lock per worktree, the agent as author (`<name> (Swarm)`) and a `Swarm-Agent` trailer, so Review groups changes by worker (ADR 0009). Absolute paths, `..`, `.git` and an empty path list are refused.
- Every agent: `swarm read` and `swarm wait` take the caller's unread messages. A message the agent has not read is pasted into its terminal the next time it is idle. A Claude Code agent also gets it while it works, because Claude Code queues typed input; never while it is blocked on a prompt or still starting.
- The daemon checks the caller's role on every call; a worker cannot message another worker.
- The same calls are exposed as MCP tools. Task calls (`task.*`) and terminal calls (`terminal.*`) are listed below.
- If the user types into a worker terminal, Swarm tells the orchestrator (ADR 0002).

### Agent state

- **OSC 7501 (Program Status Protocol)** is the main source. Claude Code emits it since 2.1.295 and Codex emits it (`codex-rs/tui/src/terminal_program_status.rs`). States: `idle`, `working`, `done`, `blocked` (`kind=permission|question|auth`), `error`, plus `clear`. The daemon reads it from the PTY stream. `libghostty-vt` 0.2.2 has no API for it, so the daemon parses this OSC itself before feeding bytes to the terminal. Claude Code only sends it after the terminal answers its support query, so the daemon answers that query. Codex (0.163.0+) sends it to any terminal. Details: `docs/spikes/agent-status.md`.
- Claude Code hooks add what OSC 7501 lacks: session ID (for resume), `StopFailure` reasons.
- Screen matching is the last fallback.
- Process terminals that emit OSC 7501 (e.g. a test runner) show their state the same way.

### How work starts

**Opening a project.** The project opens on a **Start** tab with an **orchestrator**: a Claude Code (or Codex) session in the project root on the base branch.

1. The user says what they want.
2. The Start orchestrator counts the features in the request. A feature is one change a user can see or use that can be built and reviewed on its own. With more than one feature, or when it is unsure, it lists them in its terminal and talks with the user until they agree on the list.
3. It makes exactly one task per agreed feature. A task is focused on one feature.
4. For each task it calls `task.create`: Swarm makes the task worktree and branch, runs the setup script, opens a tab and starts that task's orchestrator with the Start orchestrator's brief as its first message.
5. When every orchestrator confirms its brief, the Start orchestrator exits and the Start tab closes. Focus moves to the first task tab.

**When the Start tab shows.**

- A project with no active tasks opens on the Start tab.
- A project with active tasks opens on the tab used last. There is no Start tab.
- The Start tab closes itself once every task orchestrator has confirmed its brief. If the request is unclear, the Start orchestrator asks first and the tab stays open until it is done.

**Starting new work later.** The **+** in the tab bar opens a menu with two choices:

- **New task**: a sheet with name, branch from (default: base branch; any local branch, so a kept branch of a closed task can be picked up again), orchestrator agent, optional first message, run setup script. Swarm creates the worktree and tab and starts one orchestrator.
- **Plan with orchestrator**: opens a Plan tab for a bigger request that should be split into several tasks. It is the Start tab under another name, because the project already has tasks. Only one Start or Plan tab can be open per project.

Harness calls for the Start orchestrator:

| Call | What |
|---|---|
| `task.create {name, from?, brief, agent?}` | Creates task worktree, branch, tab and orchestrator; sends `brief` as its first message. Returns the task ID. |
| `task.list` | Lists tasks in the project with status. |
| `task.wait {ids, until: briefed\|done}` | Waits until orchestrators confirm their brief, or finish. |
| `agent.exit` | Ends the caller's own session (used by the Start orchestrator when done). |

### Workspace layout

- **Sidebar**: projects (with "Needs you" and busy markers and a tab count), then the agents, processes and user terminals of the open tab.
- **Tabs**: one tab per task, at the top of the window, plus the Start or Plan tab while its orchestrator runs. That tab has no worktree, so it shows no branch, Files or Changes. A tab shows the task name and its status ring. Tabs keep their width while there is room, then shrink to a minimum (the name is cut with an ellipsis) and then scroll sideways; a button with the task count then lists every task. The header controls on the right never move. Each task runs in its own task worktree; workers share it unless they have their own worktree, and worker worktrees are not tabs. **+** opens the New tab menu.
- **Header, right side**: view switch **Terminals | Swarm | Review**, then panel toggles **Files**, **Changes +N −N** and, after approval, **Pull request** (later the live PR state, e.g. **#42 · checks running**).
- **Right panel**: Files (file tree of the task worktree, changed files marked), Changes (changed files grouped by worker, with a **Review** button), or Pull request.
- **Terminal tiles**: separate cards with 10 px gaps and rounded corners. Focus is a 2 px accent border, "needs attention" a 2 px amber border; both are drawn above the tile and never move the layout. Only the orchestrator tile has an input box.
- **Status bar**: the tray chips and **+ Terminal** on the left; branch, agent count, process count, user terminal count (each with an icon), "permissions skipped" and ⌘K on the right.

### Command palette

⌘K opens a palette over the window: one search field, then grouped results.

- **Suggested**: the most likely next actions for the open task (Review changes ⌘⇧R, Focus orchestrator ⌘1, New task ⌘T, Plan with orchestrator ⌘⇧T).
- **Tasks**: switch tab; shows each task's status.
- **Agents**: focus an agent's tile (⌘1–⌘9); shows each agent's status.
- **Swarm**: New terminal ⌘⌥T, Settings ⌘, and every other action.
- Typing filters all groups. Shortcuts are shown here, not on buttons. Esc closes. The field is a combobox and the results a listbox for VoiceOver.

### Swarm view

- Graph: the orchestrator on top, workers below. Edges are colored by message type (task, question, report); dots on an edge are messages in flight.
- Inspector for the selected agent, on the right, resizable (320–640 px) by dragging its left border:
  - Header: kind icon, name, status, **View diff** (Review filtered to that agent) and **Focus** (switches to Terminals with that tile focused). Under it one line with kind and model, running time and shared or own worktree, then the branch.
  - Tabs **Messages**, **Changes** and **Terminal**, with counts on the first two.
  - Messages: the agent's thread, newest at the bottom. Each message shows sender, recipient, a kind tag and the time or "In flight". The text is rendered as Markdown (paragraphs, headings, lists, inline code, links); long messages fold with **Show more**.
  - Changes: total additions and deletions, then each changed file; a file opens Review at that file.
  - Terminal: the agent's own terminal, live: it scrolls and takes typing like its tile (a worker only when typing in worker terminals is on). Then its process terminals with size and state.
- No timeline: it was tried in phase 5 and removed because it added little.

### Settings

App settings: **General** (system check), **Agents** (per CLI: command, version, default model, skip permission prompts; orchestrator agent, typing in worker terminals, idle stop, agent limits), **Terminals**, **Prompts**, **Notifications**, **Appearance** (Light / Dark / System, accent, easter eggs), **Usage**, **Keyboard**.

Per project, under **Projects**: everything in `.swarm/config.toml` (worktrees, setup script, files to copy, ports, checks before PR).

- **Projects** reads and writes `.swarm/config.toml` through the daemon, which owns the file format. A save sends the fields as the page loaded them and the fields as edited; the daemon re-reads the file and writes only the fields that differ between the two, with `toml_edit`, so comments, formatting, keys Swarm does not show and hand edits to other fields stay. The page reads the file again when the Settings window gets focus, unless an edit waits to be saved. A file that does not parse shows its error and is never overwritten. **Run checks now** runs the checks on the project's open task.
- **Terminals**: the font size (11–20 pt) applies to every terminal at once; tiles keep their size and get more or fewer rows and columns.
- **Prompts**: each prompt shows the layer the agents get. **Customize** copies the built-in prompt to `~/.config/swarm/prompts/` and opens it in the default editor; **Open** opens the copy; **Reset** removes it after a confirm. A project's own prompts in `.swarm/prompts/` show on its Projects page.
- **Usage** shows every window of both agent kinds, like the menu bar popover. **Keyboard** lists the shortcuts, read-only.

### Brand and icons

- Logo: a line-drawn bee. The menu bar uses it as a template image so macOS tints it.
- App icon: the pixel bee, a 24 × 24 black and yellow sprite on cream with a faint honeycomb (canvas boards "App icon" and "Pixel bee"). The same sprite is the mascot for the easter eggs and the website.
- Agent icons: the real Claude Code and Codex icons (Lobe Icons, MIT). Claude Code keeps its orange in both themes; Codex uses the text color.
- Fonts: Geist and Geist Mono.

### Terminals

Four kinds:

| Kind | Who types | Who resizes | Who kills |
|---|---|---|---|
| **Orchestrator** | user | layout: always large | user |
| **Worker** | output view; user can type in v1 (setting, may be off by default later) | layout: small; grows when blocked or on `tile.grow` | orchestrator, user |
| **Process** (dev server, tests, any command an agent starts) | nobody by default | the agent that started it | that agent, orchestrator, user |
| **User** | user only | user only | user only |

- **User terminals are off-limits to agents** (ADR 0006): agents cannot read, type into, resize or kill them. The layout treats them as fixed islands.
- **Process terminals**: started with `swarm terminal spawn`. When the process exits or is killed, the terminal closes after a short delay (to show the exit code); a failed exit stays until the user closes it and the owner agent and the orchestrator get a `terminal.exited` event with who stopped it (`agent`, `user`, `process`) and the exit code or signal.
- **Layout engine**: tiles fill the space. Priority: user terminals (fixed size), orchestrator, terminals that need attention (`blocked`, `error`), working agents, process terminals, done or idle agents. Each tile has a minimum size; tiles that don't fit move to the **tray**: chips on the left of the status bar, each with a restore icon; chips that don't fit the bar sit behind a "+N" chip that opens a menu of them. A tile that needs attention is promoted out of the tray. The user can pin or minimize any tile.
- When the user resizes a user terminal, the other tiles reflow around it.
- **Tile sizes**: the orchestrator always gets the main slot on the left, about two thirds of the width (65 %), the full height, and never goes to the tray. Workers are small boxes stacked from the top of the column on the right: tall enough to see what they are doing (about 12 rows), never the full height. A blocked worker grows by itself and shrinks when it works again. Process terminals (dev servers, test runs) are the smallest tiles (about 8 rows), below the workers; they grow only when they need attention (a failed exit). Only user terminals keep a fixed size. The orchestrator can grow or shrink any of its workers with `tile.grow` / `tile.shrink`; a worker only itself (phase 8). A tile the user pinned, minimized or resized ignores these calls until the user resets it.
- **Font size**: one global setting (Settings › Terminals, 11–20 pt, default 13) for every terminal. Every tile keeps its size; the terminal inside gets more or fewer columns and rows.

### Terminal harness (CLI + MCP)

| Call | Who | What |
|---|---|---|
| `terminal.spawn {cmd, cwd?, size?, name?}` | any agent | Starts a process terminal in a small tile. `size` is accepted for compatibility and ignored; the terminal follows its tile. Returns an ID. |
| `terminal.resize {id, size}` | owner agent | Returns the terminal's current size. The layout decides the size; the request is ignored. |
| `terminal.read {id, lines?}` | owner agent, orchestrator | Reads the screen or the last lines of output. |
| `terminal.wait {id, until: exit\|output-match, pattern?}` | owner agent, orchestrator | Waits for exit or for a line such as `ready in`. |
| `terminal.kill {id, signal?}` | owner agent, orchestrator | Stops the process; the terminal closes. |
| `terminal.list` | any agent | Lists terminals the caller may see (never user terminals). |
| `tile.grow {agent?}` / `tile.shrink {agent?}` | orchestrator for its workers, worker for itself | Makes a worker's tile large or small again. Changes the layout only, never sends a message. |
| event `terminal.exited {id, by, code\|signal}` | to owner + orchestrator | Sent when a process terminal ends for any reason. |

### Prompts

Layers, later wins: built-in → `~/.config/swarm/prompts/` → `.swarm/prompts/`.
Files: `orchestrator.md`, `worker.md`, `start.md`, `pr.md`.
The harness block is always added first and is not editable.

### Agent lifecycle

Every agent belongs to a task. The daemon tracks every agent in SQLite (ADR 0005), so none are lost; the goal is that none are forgotten.

- **Close task**: from the tab's close button or ⌘K. A dialog shows what closing stops and deletes: running agents, user terminals, uncommitted files, workers with their own worktree that are not merged. Then the agents and all terminals of the task stop, the teardown script runs, and the task worktree and workers' own worktrees are removed. A branch already merged into the base branch is deleted; every other branch stays, so only uncommitted work (and ignored files) is lost. Branches merged by squash on GitHub do not count as merged and stay. After the agents stop, the daemon checks again and refuses when more would be lost than the dialog showed. A close interrupted by a crash finishes on the next daemon start. A closed task is never listed again and its ports are free.
- **Quit**: with running agents, ask once: keep running or stop all. Option to remember the choice.
- **Idle stop**: a done or idle agent stops after 30 minutes (setting). Its session ID is kept, so it resumes with `claude --resume <id>` or `codex resume <id>`.
- **Menu bar**: shows the running agent count and **Stop all**, also when the app is closed.
- **Daemon exit**: when no agents run and no app is attached.
- **Limit**: maximum concurrent agents, globally and per task (setting). Spawning past it returns an error to the orchestrator.
- **Daemon crash**: closing the PTYs sends SIGHUP to agents. On restart, the daemon reads the PIDs from SQLite, stops processes it no longer manages, and marks those agents "stopped, resumable".
- **Reboot**: same as a crash; agents come back through resume.

### Review

A third view next to Terminals and Swarm in the header switch (**Terminals | Swarm | Review**). It fills the main area; the sidebar and tabs stay. Left: the changed files as a folder tree. Right: the diff with inline comments. The Changes panel does not open in Review, because the tree already lists the files; Files and Pull request still do. Ways in:

- **Review** in the view switch (⌘⇧R)
- **Review** button and any file in the Changes panel (the **Changes +N −N** header button opens that panel)
- **Review** action on the "ready for review" notification
- **View diff** on a worker in the Swarm view (Review filtered to that worker)
- Command palette

Two sources:

- **Live** (default while workers run): each worker's worktree diff, committed and uncommitted, refreshed as agents edit. For workers that share the task worktree, only their `swarm commit` commits are attributed to them; uncommitted work is not. Files being edited get an "Editing" pill. A comment is sent right away through the orchestrator to the worker that owns the file. Approve is disabled.
- **Merged** (default when all workers are done): the commits of the task branch since it left the base branch, so exactly what the pull request holds. Uncommitted work shows only in Live. Comments are batched and sent with **Request changes**.

Live comments are anchored to the code text, not line numbers. If the agent rewrites those lines, the comment is marked outdated.

- Layout: the file tree on the left (272 px) with "N files" and "N / N viewed". On the right a header with the agents' status, **Live | Merged**, **Unified | Split** and **Approve**, then one card per file. The agent that changed a file shows in its card header.
- File tree: like Zed's project panel. Folders first, then files, each sorted by name; chevrons, indent guides, and a folder with a single subfolder shown as one row (`crates/swarm_term/src`). A file row shows its name, +/− counts, open comments and the Viewed check. Arrow keys move and open or close folders. The Changes panel in the Terminals view uses the same tree.
- Folded files: lock files, generated files (`linguist-generated` in `.gitattributes`, minified files, source maps) and files with more than 500 changed lines come without their lines, like GitHub. The card is one row with a tag (**Lock file**, **Generated**, **Large diff**) and **Show diff**, which loads that one file. Totals, the fingerprint and Approve still cover every line.
- **Viewed** is per file and resets when the file's diff changes. A viewed file folds. Keys: `j` / `k` next or previous file, `v` viewed.
- The source starts as Live while a worker of the task works, else Merged; the user's pick then stays for that task.
- **Approve** works only on Merged. The daemon stores a fingerprint of the Merged diff (a hash of every changed line, file mode and blob). When the diff changes, the approval no longer counts and Review shows **Approve** again.
- **Comments**: the **+** on a line's gutter, or `c` on the line cursor (moved with ↑ / ↓), opens a comment box under the line; ⌘↩ adds it and Esc cancels. On Live the comment goes at once; on Merged it waits as **Pending** until **Request changes · N** sends all pending comments, packed into messages of at most 64 KiB. A comment holds at most 16 KiB, so the orchestrator can pass it on with its header. **Approve** stays off while comments are pending, and the daemon refuses it too. Sending is refused while the task's orchestrator is stopped.
- Comments go to the task orchestrator as `comment` messages (ADR 0002). Each names the worker that owns the file (its own worktree, or the newest `swarm commit` of the file), and the orchestrator passes it on or handles it.
- A thread shows the state (**Pending**; **In flight** while the orchestrator has not got it; **Delivered** with its time; **Resolved**), the route (`→ orchestrator → term-render`) and the user's replies. **Reply** opens a field with its own **Send**; a reply follows its thread: it is sent at once unless the thread is still pending. **Resolve** (a check) marks the thread done and folds it. A pending comment can be deleted.
- Anchors: a comment keeps the line's text and its neighbors. Each time the comments are listed, the daemon finds the line again in the file as it is now, preferring a match with the same neighbors and the one nearest to where it was. When the text is gone, the comment is **Outdated** and shows at the top of its file card.
- Agents do not reply to comments in v1; they answer through the orchestrator's normal messages.
- Large diffs: at most 1,000 files, 5,000 lines per file and 50,000 lines in total are sent; the totals still count everything. The list is virtualized.

### Pull requests

A PR can only be created after the task is approved in Review. This makes the user read the code first.

1. **Review → Approve.** The Approve button turns into **Create PR**.
2. **Create PR** opens the PR panel on the right (a header toggle next to Files and Changes): title and description drafted by the orchestrator (`pr.md` prompt), **local checks** from `.swarm/config.toml` (run in the task worktree; the PR can only be created when they pass), open as draft. Uses `gh`; pushes the branch first.
3. **After creation** the header button shows the live PR state, e.g. **#42 · checks running** or **#42 · ready to merge**. Its panel shows **GitHub checks** (CI on the pushed branch, read through `gh`), reviewers, Ready for review, Merge, and after-merge options (remove agent worktrees, close the tab).

Other ways in: ⌘K "Create pull request"; `swarm pr draft` from the orchestrator opens the panel with its draft (still only after approval).

- **Draft:** opening the panel sends the orchestrator a `pr draft` message. It writes the title and description from the `pr.md` prompt (built in: What, Why, Notes for reviewers, Open tasks; a project can replace it in `.swarm/prompts/pr.md`) and sends them back with `swarm pr draft`. Without a running orchestrator the fields stay empty and the user writes them.
- **Local checks:** run one after the other, each in its own process terminal in the task worktree, so a failure can be read. After a failure the rest are skipped. A result belongs to the commit it ran on, and counts only when the worktree had no uncommitted changes from start to end; a new commit makes it stale. Without configured checks the section is hidden.
- **Create PR** reads the task's HEAD once and pushes exactly that commit (the repository's pre-push hook runs as usual), then runs `gh pr create` into the task's PR base: the branch the task started from when `origin` has it, else the project's base branch. The daemon refuses it when HEAD is not the task branch, the approval does not match the Merged diff at that commit, a comment is pending, or required checks did not pass cleanly on that commit. A push or `gh` failure shows its output. An open pull request from the branch that Swarm did not record is taken over; a closed one can be replaced.
- **Update PR:** new commits after the pull request exists (for example a CI fix) go through Review and the checks again, then **Push changes** sends them under the same rules.
- **Merge** merges only the commit Swarm pushed (`--match-head-commit`), checks that GitHub really merged it (a merge queue leaves it open), then deletes the remote branch. Squash by default, with merge and rebase as other choices.
- **After merge:** one option, **Close the task after merge**, which closes the task as Close task does. It stays open when that would lose work: uncommitted files or local commits that are not in the pull request.
- **Status** is read with `gh pr view` while the panel shows: every 10 s while GitHub checks run, every 30 s otherwise.
- **Set up GitHub:** without `gh`, **Create PR** becomes **Set up GitHub**. It installs `gh` in a visible user terminal (Homebrew when it exists, else the official release, checked against its published checksums, into Swarm's own bin folder), then runs `gh auth login --web` the same way. Agents get the same `gh` on their `PATH`.
- **New task from any branch:** the New task sheet's "branch from" lists every local branch, so a closed task's kept branch can be picked up again.

### Notifications

Native macOS notifications through GPUI's `show_system_notification`. Only the orchestrator notifies the user.

| Trigger | Source |
|---|---|
| Needs a decision | `swarm notify` from the orchestrator |
| Permission or question prompt | Claude Code `Notification` hook: `permission_prompt`, `elicitation_dialog`, `agent_needs_input` |
| Ready for review | the orchestrator rests and no worker is busy |
| Something broke | `StopFailure` hook, or an agent process exits |

- Skip the notification when the app is focused on that task.
- One notification per task: the task ID is the tag, so a new one replaces the old one.
- Clicking opens the task: "needs you" opens the Terminals view with the agent the notice came from focused, "ready for review" opens the Review view. A notice for a task the app no longer shows only brings the window back.
- The same signal shows as a dot on the menu bar icon, the "Needs you" pill in the sidebar, and a dock bounce for urgent cases.
- Settings: each trigger on or off, per-project overrides, sound.
- Needs a real `.app` bundle: GPUI disables notifications under `cargo run`.
- No inline reply in v1: GPUI supports action buttons, not text input.
- The daemon decides when: it broadcasts a notice (kind, task, agent, short text). The app decides whether and how: settings, the focused task, sound, Dock bounce. So the menu bar and a closed window get the same notices.
- Prompt notices come from any agent of the task. Ready for review fires once when the orchestrator rests and no worker is busy (working, blocked or starting), whichever happens last; it fires again only after a worker worked again. Something broke is a `StopFailure` hook, an OSC 7501 error (Codex has no such hook), an agent that fails to start, or an agent process that ends with an error, never a stop the user or `swarm exit` asked for. One error spell gives one notice.
- Settings live in `settings.toml` under `[notifications]`: each trigger, sound, Dock bounce for urgent ones (needs a decision, prompt), mute while viewing the task, the menu bar dot, and per project **All**, **Errors only** (something broke) or **Off**. A value there that is not valid falls back to its default, so a typo never stops agents from starting.
- The sound plays only when macOS shows the notification: the permission is allowed and the app's own sound setting in System Settings is on. The app reads both at start, when it becomes active and when the Settings page opens.
- **Send test notification** posts one right away. The permission row reads the real authorization and offers **Open System Settings** when it is denied.

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
- The daemon runs the checks, so they see the tools the agents get. Git is checked with `xcode-select -p` first, because `/usr/bin/git` without the Command Line Tools opens Apple's install dialog; the fix is `xcode-select --install`.
- Fixes, from the official docs: Claude Code installs with `curl -fsSL https://claude.ai/install.sh | bash`; Codex with `brew install --cask codex` when Homebrew exists, else `curl -fsSL https://chatgpt.com/codex/install.sh | sh`. Both sign in by running the CLI once (`claude`, `codex`). Signed in means the `Claude Code-credentials` Keychain item exists (checked without reading the secret), and `~/.codex/auth.json` exists.
- The welcome screen shows once, on the first launch: the system check, **Skip for now** and **Open a project**. A fix runs in a terminal under its row, and the row checks again when that terminal ends. **Settings › General** shows the same check.
- The welcome screen shows while the daemon reports no project and `welcome_done` in `settings.toml` is unset. **Skip for now** and **Open a project** both set the flag. A fix that is still running when the screen closes is stopped, like a closed tile. The first action that is not ready takes the keyboard focus; a dimmed button (the GitHub account while the GitHub CLI is missing) does nothing.
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

### Websites

- **Product website**: one page (designed on the canvas, Website page): hero with a workspace preview, three steps with app screenshots, features, harness, download.
- **Docs website**: every explanation that the app leaves out by design, plus the harness reference.

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

See [`ROADMAP.md`](ROADMAP.md) for the phases, their scope and what "done" means for each.

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
- Set `[profile.dev.package."*"] opt-level = 2` so `cargo run` draws and scrolls smoothly: dependencies (GPUI) are optimized, workspace crates are not. A clean `swarm_app` dev build went from 27 s to 70 s; rebuilds of workspace crates stay as fast.

**Terminal and daemon**

- Batch PTY output: one shared buffer and one wake-up; send the first change at once, then at most one update per 8 ms.
- Ghostty's `max_scrollback` is in bytes, not lines.
- Preload about 1,000 scrollback lines on attach; fetch older lines on scroll (about 6.5 µs per line to read).
- Support the Kitty keyboard protocol (fish 4 turns it on).
- Claude Code draws on the alternate screen and asks for mouse reporting (fullscreen rendering, the default for new users since May 2026), so the wheel must reach the program. The view has no scrollback there: it sends `TerminalInput::Scroll`, and the emulator encodes mouse wheel events in the format the program asked for, else arrow keys. Arrow keys are typing, so the view sets `keys` from the typing gate and the emulator sends them only when it is true; mouse wheel events do not need it. The main screen scrolls in the view.
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
- Codex's `workspace-write` sandbox blocks unix sockets and makes `.git` read-only. Codex agents use the `swarm` MCP tools (MCP servers run outside the sandbox, and Codex passes them only the env names listed in `env_vars`), and get the main repository's `.git` and their own `.git/worktrees/<name>` as writable roots so they can commit.
- Both CLIs ask once per project whether to trust the folder; they key trust on the main repository root, so worktrees share the answer. Swarm never pre-trusts. While the dialog shows, the agent is blocked with "Trust this folder".

**Swarm view**

- Count an agent's changes with `git diff-index --numstat -z <base> --` and `GIT_OPTIONAL_LOCKS=0`. Porcelain `git diff` refreshes the index and takes `index.lock`, which can fail the agent's own commit.
- Untracked files can be symlinks to FIFOs or `/dev/zero`: check `symlink_metadata`, read regular files only, and cap the bytes read.
- The daemon broadcasts "delivered" while it holds the store lock, so it never overtakes "sent". The app still keeps deliveries for messages it has not seen yet, because history arrives on another channel.

**Usage and menu bar**

- Claude usage: read `rate_limits` from the Claude Code status line data of agents Swarm runs. Fall back to `/api/oauth/usage` at most every 5 minutes, honor `Retry-After` and store it on disk. The endpoint shares a budget with the user's own sessions and returns 429 often.
- The daemon polls usage, not the app: it outlives the app and keeps one schedule. It polls only while an app is connected, keeps the last good numbers on disk, and broadcasts each change.
- Swarm sets the status line of its Claude agents to `swarm statusline`. It sends `rate_limits` to the daemon while it runs the user's own status line command from `~/.claude/settings.json` with the same input and prints its output, so the user keeps their status line.
- The fallback request goes through `/usr/bin/curl` with the token in a config on stdin, never in arguments: no TLS stack in the daemon, HTTPS only, no redirects. curl runs with `-q`, an empty environment and `--noproxy '*'`, so no config file or proxy variable changes where the request goes.
- Claude token (fallback only): Keychain item `Claude Code-credentials`, read through `/usr/bin/security`; never logged or stored. When `security` does not end in time, a Keychain dialog is probably open: the daemon does not run it again for one hour, and keeps that time on disk.
- Codex usage: `codex app-server` → `account/rateLimits/read`. Codex keeps its own token.
- Menu bar popover is a GPUI window under an `NSStatusItem`. Compute its position before opening it.
- With no main window open, switch to no Dock icon; the menu bar keeps working.
- The menu bar icon is the line bee as a template image, the highest usage percent across all windows as its title (` 42%`), and a dot drawn into a second template image while an agent needs you.
- The popover shows usage per agent kind (each window with its percent and reset time), the tasks that need you (a click opens that task), the running agent count, **Stop all** (asks once in place), **Open Swarm** and **Quit**. Esc closes it, Tab moves through it.
- The workspace outlives its window: closing the window keeps the workspace and its daemon connection, so the menu bar stays live, and **Open Swarm** opens a new window on the same workspace.
- **Stop all** stops every running agent as the user's choice: agents with a session become resumable, and no orchestrator is told. An agent that the user stopped cannot start workers or tasks, also during the seconds its process takes to end.
- The Dock icon goes only when the status item exists. A Dock click or a second launch of the app opens the main window again.

**Notifications**

- Need a real `.app` bundle with a bundle ID.
- macOS may deny permission without showing a prompt. The system check must detect "denied" and offer a button that opens System Settings › Notifications.
