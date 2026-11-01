# Roadmap

Each phase ends with a pull request into `main`. A phase is done when every box in "Done when" is checked. Details of how things work live in [`PLAN.md`](PLAN.md); this file says what gets built when.

| Phase | Name | Status |
|---|---|---|
| 0 | Plan, design, spikes | Done ([PR #1](https://github.com/DriesH/swarm/pull/1)) |
| 1 | Foundation: workspace, `swarm_ui`, gallery | Done ([PR #2](https://github.com/DriesH/swarm/pull/2)) |
| 2 | App shell with fake data | Done ([PR #3](https://github.com/DriesH/swarm/pull/3)) |
| 3 | Daemon, terminals and worktrees | Done ([#4](https://github.com/DriesH/swarm/pull/4), [#5](https://github.com/DriesH/swarm/pull/5), [#6](https://github.com/DriesH/swarm/pull/6)) |
| 4 | Agents and harness | In progress |
| 5 | Swarm view | In progress |
| 6 | Review and pull requests | In progress |
| 7 | Menu bar, usage and notifications | In progress |
| 8 | Polish and first release | Not started |
| 9 | After first release | Not started |

## 0. Plan, design, spikes

**Goal:** know what to build and prove the risky parts work.

**Scope:** `PLAN.md`, `GLOSSARY.md`, ADRs 0001–0006, Zed research, design canvas (direction A, light and dark), spikes 1–4.

**Done when:**

- [x] Plan, glossary and ADRs written
- [x] Designs for workspace, review, PR, settings, notifications and welcome screen
- [x] Spikes: GPUI terminal, daemon reattach, agent status, menu bar
- [ ] Notification buttons tested (waits on a one-time macOS permission; moved to phase 7)

## 1. Foundation: workspace, `swarm_ui`, gallery

**Goal:** a component library that every screen is built from.

**Scope:** Cargo workspace with pinned GPUI and best-practice lints, `swarm_ui` crate (theme tokens, Light / Dark / System, Geist fonts, components), `gallery` app.

**Done when:**

- [x] Workspace builds; `cargo test`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check` pass
- [x] Theme tokens match direction A in light and dark; System follows macOS live
- [x] Contrast tests pass (WCAG AA) for every text token in both themes
- [x] Components: icon, label, button, icon button, switch, segmented control, tab, status pill and dot, list row, tray chip, panel, tile frame
- [x] Every interactive component has a VoiceOver role and label, keyboard focus and a visible focus ring
- [x] Gallery shows every component in both themes and is fully keyboard navigable

## 2. App shell with fake data

**Goal:** every workspace screen from the canvas works as a real GPUI app, driven by fake data.

**Scope:** `swarm_app` crate: window, sidebar (projects, agents), task tabs and the **+** menu, Terminals / Swarm / Review view switch, Files and Changes panels (static), status bar with tray, tile layout engine (priorities, minimum sizes, tray, user-terminal islands, resize), settings window (Agents, Notifications, Project), command palette, actions and keymaps.

**Done when:**

- [x] Start, Workspace, user terminal island and New task screens match the designs
- [x] Layout engine moves tiles to the tray when they don't fit and keeps user terminals at their size
- [x] Everything works from the keyboard; ⌘K opens the palette; each window focuses an element when it opens (Tab does nothing until something has focus)
- [ ] VoiceOver can reach and name every control
- [x] Frames stay under 8 ms with fake content

## 3. Daemon, terminals and worktrees

**Goal:** real terminals that survive the app, inside real worktrees.

**Scope:** `swarm_term` (`libghostty-vt` behind our own types, GPUI grid element, batching, scrollback, Kitty keyboard, IME), `swarm_daemon` (PTYs, socket protocol, attach and reattach, clean environment, crash recovery), `swarm_core` (SQLite state, projects, tasks, worktrees, `.swarm/config.toml`: setup script, copy files, ports), user terminals, agent lifecycle rules.

**Done when:**

- [x] Open a project; **New task** creates branch, worktree and tab and runs the setup script
- [x] A user terminal works; quitting and reopening the app shows it unchanged
- [x] Killing the daemon leaves no leftover processes after restart
- [x] Terminal speed matches spike 1 (`seq 1 200000` about 0.3 s)
- [x] The gallery shows a real terminal preview: `swarm_term`'s grid inside a `TileFrame`, running a short colour and text test

## 4. Agents and harness

**Goal:** both ways of starting work run end to end with real agents.

**Scope:** Claude Code and Codex adapters (flags, skip permissions, resume, session IDs, hooks, update check off), OSC 7501 status, `swarm_cli` (agent socket, `swarm` CLI, MCP server: `spawn`, `send`, `ask`, `report`, `task.*`, `terminal.*`, `agent.exit`), orchestrator-only messaging, prompt layers, Start orchestrator, process terminals with sizes, kill and `terminal.exited` events.

**Done when:**

- [ ] Situation 1: open a project, describe two features, get two tabs with briefed orchestrators, Start tab closes
- [ ] Situation 2: **+ › New task** starts a fresh orchestrator
- [ ] Orchestrator spawns Claude and Codex workers; their status shows live
- [ ] An agent starts `pnpm dev` in a sized terminal and stops it; a user stop sends `terminal.exited` to the orchestrator
- [ ] Workers cannot message each other; agents cannot see user terminals

## 5. Swarm view

**Goal:** see the swarm and its messages at a glance.

**Scope:** graph of orchestrator and workers, message edges, and a resizable agent inspector with Messages, Changes and Terminal tabs.

**Done when:**

- [ ] Graph and inspector update live from the daemon
- [ ] Inspector shows worktree, branch, changes and messages; **Focus** jumps to the agent's tile

## 6. Review and pull requests

**Goal:** read the code, then ship it.

**Scope:** Changes panel, Review view (Live and Merged, by agent or folder, unified or split), inline comments routed through the orchestrator, outdated comments, Approve, Close task, local checks from `.swarm/config.toml`, `gh` PR create, PR status (GitHub checks, reviewers, merge), system check for `gh`.

Three parts: 1. Review view, Approve, Changes panel and Close task. 2. Comments. 3. Pull requests, and **New task** from any branch.

**Done when:**

- [ ] Review shows Live and Merged diffs that update while agents work, and Approve stops counting when the Merged diff changes
- [ ] Close task stops the agents, removes the worktrees and deletes only branches merged into the base branch, after a dialog that shows what is lost
- [ ] **New task** can branch from any local branch, so a closed task's branch can be picked up again

- [ ] A comment on live code reaches the worker that wrote it
- [ ] **Create PR** appears only after **Approve** and only when required checks pass
- [ ] PR status shows GitHub checks and can merge; after merge, worktrees and tab close as configured
- [ ] Without `gh`, **Set up GitHub** installs and signs in through a visible user terminal

## 7. Menu bar, usage and notifications

**Goal:** know what the swarm is doing without opening the window.

**Scope:** `swarm_usage` (Claude usage from status line data with the endpoint as fallback, Codex usage through `codex app-server`), menu bar status item with a GPUI popover, running agent count and **Stop all**, system notifications with actions, dock bounce, notification settings.

Three parts: 1. Usage in the daemon and the sidebar card. 2. Menu bar and popover. 3. Notifications.

**Done when:**

- [ ] Menu bar shows usage and a "needs you" dot, also with the window closed
- [ ] Notifications appear for the four triggers, replace each other per task, and their buttons open the right view
- [ ] Usage polling honors `Retry-After`

## 8. Polish and first release

**Goal:** a signed app that someone else can install.

**Scope:** welcome screen and system check, remaining settings pages (Terminals with the font size, Prompts, and Projects, which edits each repository's `.swarm/config.toml`), `tile.grow` / `tile.shrink` (the tile sizes themselves came early, after phase 5), file-type icons in the style of vscode-icons for files and folders (Changes tab, Changes and Files panels, Review), easter eggs, full VoiceOver and keyboard audit, performance pass, app bundle signing and notarization, updates, docs website and product website.

**Done when:**

- [ ] First launch guides through the system check
- [ ] `tile.grow` / `tile.shrink` change a worker's tile, and a user-sized tile ignores them
- [ ] Changing the terminal font size in Settings resizes every terminal at once
- [ ] Settings › Projects edits `.swarm/config.toml` and keeps the file's comments and the keys it does not show
- [ ] Files and folders show file-type icons; the icon set's license terms are met (vscode-icons: CC BY-SA 4.0 for the icons, attribution in About, branded icons excluded)
- [ ] VoiceOver audit passes on every screen (checklist in `crates/swarm_ui/README.md`)
- [ ] Reduce Motion reaches GPUI and stops spinners and easter-egg animations
- [ ] Disabled controls and plain text are announced by VoiceOver (needs GPUI support or a contribution upstream)
- [ ] Signed and notarized build installs on a clean Mac
- [ ] The daemon's app socket accepts only the signed Swarm app: it reads the caller's audit token from the socket and checks its code signature, so a process an agent detached from its parent cannot get in (ADR 0008)
- [ ] Docs website and product website are live

## 9. After first release

**Goal:** Swarm is ready when the Mac starts.

**Scope:** **Launch at login** in Settings: starts the daemon at login (with `SMAppService` and a launch agent in the app bundle), so agents and the menu bar work before the window opens. Off by default.

**Done when:**

- [ ] With the setting on, the daemon runs after a restart of the Mac without opening the app
- [ ] Turning it off removes the login item
- [ ] The setting shows the state macOS reports, also when the user changed it in System Settings › General › Login Items

## Designs still to make

- Settings › Terminals and Prompts (phase 8)
- Menu bar usage popover (phase 7)
- Settings: Launch at login (phase 9)
