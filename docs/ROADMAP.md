# Roadmap

Each phase ends with a pull request into `main`. A phase is done when every box in "Done when" is checked. Details of how things work live in [`PLAN.md`](PLAN.md); this file says what gets built when.

| Phase | Name | Status |
|---|---|---|
| 0 | Plan, design, spikes | Done ([PR #1](https://github.com/DriesH/swarm/pull/1)) |
| 1 | Foundation: workspace, `swarm_ui`, gallery | In progress |
| 2 | App shell with fake data | Not started |
| 3 | Daemon, terminals and worktrees | Not started |
| 4 | Agents and harness | Not started |
| 5 | Swarm view | Not started |
| 6 | Review and pull requests | Not started |
| 7 | Menu bar, usage and notifications | Not started |
| 8 | Polish and first release | Not started |

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

- [ ] Workspace builds; `cargo test`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check` pass
- [ ] Theme tokens match direction A in light and dark; System follows macOS live
- [ ] Contrast tests pass (WCAG AA) for every text token in both themes
- [ ] Components: icon, label, button, icon button, switch, segmented control, tab, status pill and dot, list row, tray chip, panel, tile frame
- [ ] Every interactive component has a VoiceOver role and label, keyboard focus and a visible focus ring
- [ ] Gallery shows every component in both themes and is fully keyboard navigable

## 2. App shell with fake data

**Goal:** every workspace screen from the canvas works as a real GPUI app, driven by fake data.

**Scope:** `swarm_app` crate: window, sidebar (projects, agents), task tabs and the **+** menu, Terminals / Swarm / Review view switch, Files and Changes panels (static), status bar with tray, tile layout engine (priorities, minimum sizes, tray, user-terminal islands, resize), settings window (Agents, Notifications, Project), command palette, actions and keymaps.

**Done when:**

- [ ] Start, Workspace, user terminal island and New task screens match the designs
- [ ] Layout engine moves tiles to the tray when they don't fit and keeps user terminals at their size
- [ ] Everything works from the keyboard; ⌘K opens the palette
- [ ] VoiceOver can reach and name every control
- [ ] Frames stay under 8 ms with fake content

## 3. Daemon, terminals and worktrees

**Goal:** real terminals that survive the app, inside real worktrees.

**Scope:** `swarm_term` (`libghostty-vt` behind our own types, GPUI grid element, batching, scrollback, Kitty keyboard, IME), `swarm_daemon` (PTYs, socket protocol, attach and reattach, clean environment, crash recovery), `swarm_core` (SQLite state, projects, tasks, worktrees, `.swarm/config.toml`: setup script, copy files, ports), user terminals, agent lifecycle rules.

**Done when:**

- [ ] Open a project; **New task** creates branch, worktree and tab and runs the setup script
- [ ] A user terminal works; quitting and reopening the app shows it unchanged
- [ ] Killing the daemon leaves no leftover processes after restart
- [ ] Terminal speed matches spike 1 (`seq 1 200000` about 0.3 s)

## 4. Agents and harness

**Goal:** both ways of starting work run end to end with real agents.

**Scope:** Claude Code and Codex adapters (flags, skip permissions, resume, session IDs, hooks, update check off), OSC 7501 status, `swarm_harness` (socket API, `swarm` CLI, MCP server: `spawn`, `send`, `ask`, `report`, `task.*`, `terminal.*`, `agent.exit`), orchestrator-only messaging, prompt layers, Start orchestrator, process terminals with sizes, kill and `terminal.exited` events.

**Done when:**

- [ ] Situation 1: open a project, describe two features, get two tabs with briefed orchestrators, Start tab closes
- [ ] Situation 2: **+ › New task** starts a fresh orchestrator
- [ ] Orchestrator spawns Claude and Codex workers; their status shows live
- [ ] An agent starts `pnpm dev` in a sized terminal and stops it; a user stop sends `terminal.exited` to the orchestrator
- [ ] Workers cannot message each other; agents cannot see user terminals

## 5. Swarm view

**Goal:** see the swarm and its messages at a glance.

**Scope:** graph of orchestrator and workers, message edges, timeline, agent inspector with live terminal preview.

**Done when:**

- [ ] Graph and timeline update live from the daemon
- [ ] Inspector shows worktree, branch, changes and messages; **Focus** jumps to the agent's tile

## 6. Review and pull requests

**Goal:** read the code, then ship it.

**Scope:** Changes panel, Review view (Live and Merged, by agent or folder, unified or split), inline comments routed through the orchestrator, outdated comments, Approve, local checks from `.swarm/config.toml`, `gh` PR create, PR status (GitHub checks, reviewers, merge), system check for `gh`.

**Done when:**

- [ ] A comment on live code reaches the worker that wrote it
- [ ] **Create PR** appears only after **Approve** and only when required checks pass
- [ ] PR status shows GitHub checks and can merge; after merge, worktrees and tab close as configured
- [ ] Without `gh`, **Set up GitHub** installs and signs in through a visible user terminal

## 7. Menu bar, usage and notifications

**Goal:** know what the swarm is doing without opening the window.

**Scope:** `swarm_usage` (status item, GPUI popover, Claude usage from status line data with the endpoint as fallback, Codex usage through `codex app-server`), running agent count and **Stop all**, system notifications with actions, dock bounce, notification settings.

**Done when:**

- [ ] Menu bar shows usage and a "needs you" dot, also with the window closed
- [ ] Notifications appear for the four triggers, replace each other per task, and their buttons open the right view
- [ ] Usage polling honors `Retry-After`

## 8. Polish and first release

**Goal:** a signed app that someone else can install.

**Scope:** welcome screen and system check, remaining settings pages (Terminals, Prompts), easter eggs, full VoiceOver and keyboard audit, performance pass, app bundle signing and notarization, updates, docs website and product website.

**Done when:**

- [ ] First launch guides through the system check
- [ ] VoiceOver audit passes on every screen
- [ ] Signed and notarized build installs on a clean Mac
- [ ] Docs website and product website are live

## Designs still to make

- Settings › Terminals and Prompts (phase 8)
- Menu bar usage popover (phase 7)
- Command palette (phase 2)
