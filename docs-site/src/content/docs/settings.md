---
title: Settings
description: Every page of the Settings window, every control on it and its default.
---

Open Settings with ⌘, **Swarm › Settings…**, or **Settings** at the bottom of the sidebar. The left list has one entry per page. Under **Projects** it has one entry per [project](/swarm/docs/first-project/), for the settings of that project's `.swarm/config.toml`.

The app saves a change about 300 ms after you stop editing. There is no save button.

## Where settings are stored

App settings live in `~/.config/swarm/settings.toml`. Edit it by hand or in the Settings window. When Swarm saves, it changes only its own keys and keeps your comments and any other keys. A missing file or a missing key gives the default.

Swarm reads the file when the app starts. The daemon, which runs your agents (see [Install](/swarm/docs/install/#the-background-daemon)), reads it each time it starts an agent, so a new value applies to agents you start afterwards.

A value that is not valid, such as `skip_permissions = "yes"`, falls back to that key's default. The other keys still load. The page that holds the key shows a warning that names it. The next change you make in Settings writes a corrected file.

A `font_size` outside 11–20 moves to the nearest value in that range.

:::caution
A file that is not valid TOML at all, for example with a missing quote, does not load. The Settings window shows the error and does not overwrite the file. Swarm and its agents use the defaults until you fix it.
:::

## General

| Control | Default | What it does |
|---|---|---|
| **System** | – | Lists Git, Claude Code, Codex, GitHub CLI, GitHub account and Notifications, each with its state. A row that is not ready has a button that fixes it. |
| **Check again** | – | Runs the system check again. |
| **Check for updates automatically** | On | Checks GitHub Releases 3 seconds after launch and every 6 hours. Off leaves only **Check now** and **Swarm › Check for Updates…**. |
| **Check now** | – | Checks for an update now. The button shows a spinner while Swarm checks or downloads. When an update is ready, it becomes **Update now**, or **Download** if Swarm cannot replace itself. |
| **Updates** status | – | One line: **Up to date**, **Checking…**, **Downloading 1.2.0…**, **Update ready: 1.2.0**, **Update failed** with the reason, **Swarm 1.2.0 needs macOS 14.0**, **Updates need a signed build** or **Check failed** with the reason. |
| **Open as draft** | On | New pull requests start with **Open as draft** on. |
| **About** | – | Shows the app version and the credit for the file icons, with links to their repository and licence. |

The system check also runs when you open Settings and when the window becomes active. See [Install](/swarm/docs/install/#updates) for how updates work.

## Agents

One section for each agent kind, **Claude Code** and **Codex**, then a **Workers** section. Each agent kind shows the version the system check found, or **Not found**.

| Control | Default | What it does |
|---|---|---|
| **Command** | `claude` / `codex` | The program Swarm starts. A name without a path is looked up in `PATH` and the usual install folders. |
| **Orchestrator model** | `opus` / `gpt-reserve` | The model for orchestrators, including the Start orchestrator. |
| **Worker model** | `sonnet` / `gpt-6-luna` | The model for workers. An orchestrator can choose another model for one worker. |
| **Orchestrator agent** | Claude Code | The agent kind of new orchestrators: Claude Code or Codex. |
| **Skip permission prompts** | Off | Starts agents without permission prompts: `--dangerously-skip-permissions` for Claude Code, `--dangerously-bypass-approvals-and-sandbox` for Codex. |
| **Allow typing in worker terminals** | On | Lets you type into a worker's terminal. When you do, Swarm tells the orchestrator. |
| **Stop idle agents after** | 30 minutes | **10 minutes**, **30 minutes**, **1 hour** or **Never**. An agent that stays idle or done this long stops, and resumes later. See [Idle stop](/swarm/docs/how-work-starts/#idle-stop-and-resume). |
| **Max agents per task** | 6 | How many agents of one task can run at the same time. |
| **Max agents in total** | 16 | How many agents of all tasks can run at the same time. |

:::caution
With **Skip permission prompts** on, agents run commands and edit files without asking you. The status bar then shows **permissions skipped**.
:::

A limit must be a whole number of at least 1. Another value shows an error on the field and is not saved. When starting an agent would go over a limit, Swarm refuses it. The agent that asked gets an error such as `6 agents are running, the limit is 6 (Settings › Agents)`.

## Terminals

| Control | Default | What it does |
|---|---|---|
| **Font size** | 13 pt (11–20) | The font size of every terminal. Tiles keep their size and show more or fewer rows and columns. |
| **Ask before closing a terminal** | On | Asks first when you close one of your own terminals while a program runs in it. **Do not show this again** in that question turns this switch off. |

## Prompts

Swarm adds a prompt to each agent at start. The page lists the prompts, one row each:

| File | Used for |
|---|---|
| `orchestrator.md` | The orchestrator of a task. |
| `worker.md` | Workers. |
| `start.md` | The Start orchestrator. |
| `plan.md` | The template a new plan starts from. No agent gets it. |
| `pr.md` | The pull request description the orchestrator writes. |

Each prompt comes from one of three layers. The last layer that has the file wins:

1. **Built in**: the text that ships with Swarm.
2. **Yours**: `~/.config/swarm/prompts/<file>`.
3. **Project**: `.swarm/prompts/<file>` in the repository.

A blank file counts as missing. Swarm always adds a fixed block with the agent's identity and commands before the prompt. You cannot edit that block. See the [harness reference](/swarm/docs/harness/).

| Button | Shown on | What it does |
|---|---|---|
| **Customize** | **Built in** | Copies the built-in prompt to `~/.config/swarm/prompts/` and opens the copy in your default editor. |
| **Open** | **Yours** | Opens your copy. |
| **Reset** | **Yours** | Deletes your copy after you confirm. The built-in prompt applies again. |

A **Project** prompt has no buttons here. Open it from the project's page.

## Notifications

How Swarm tells you that a task needs you. [Notifications and usage](/swarm/docs/notifications-and-usage/) explains each kind of notice.

| Control | Default | What it does |
|---|---|---|
| **macOS permission** | – | Shows what macOS allows. **Open System Settings** appears when notifications are not allowed. |
| **Orchestrator needs a decision** | On | Notifies when the orchestrator asks for you. |
| **Permission or question prompt** | On | Notifies when an agent shows a permission or question prompt. |
| **Task ready for review** | On | Notifies when every worker is done and the orchestrator is idle. |
| **Agent stopped or failed** | On | Notifies when an agent fails or ends with an error. |
| **Play sound** | Off | Plays a sound with a notification. |
| **Bounce dock for urgent alerts** | On | Bounces the Dock icon for urgent notices. |
| **Mute while viewing the task** | On | Posts nothing while you look at that task. |
| **Dot on menu bar icon** | On | Shows a dot on the menu bar icon while a task needs you. |
| Project list | **All** | One menu per project: **All**, **Errors only** or **Off**. |
| **Send test notification** | – | Sends a test notification. |

## Appearance

| Control | Default | What it does |
|---|---|---|
| **Theme** | System | **Light**, **Dark** or **System** (follows macOS). |
| **Accent** | Blue | **Blue** or **Honey**. |
| **Easter eggs** | On | Off turns off every easter egg. |
| **Honeycomb behind the Swarm view** | Off | Draws a honeycomb in place of the dots behind the [Swarm view](/swarm/docs/swarm-view/). Needs **Easter eggs** on. |

## Usage

Read-only. Shows every usage window of Claude Code and Codex, with how much is used and when it resets, as the menu bar popover does. See [Notifications and usage](/swarm/docs/notifications-and-usage/).

## Keyboard

Read-only list of every shortcut. You cannot change them. See [Keyboard shortcuts](/swarm/docs/keyboard/).

## Project pages

Each page edits the project's `.swarm/config.toml`. Every field, its default and its effect are in [Project config](/swarm/docs/project-config/).

When the repository has a git pre-push hook, the checks section shows a **Git pre-push hook** row with the tool (`husky`, `lefthook` or `pre-push`) and **Detected**. Swarm does not run the hook itself; git runs it when Swarm pushes.

## Example file

All keys with their defaults:

```toml
[agents]
orchestrator = "claude-code"   # or "codex"
skip_permissions = false
idle_stop_minutes = 30         # 0 = never
max_agents_per_task = 6
max_agents_total = 16

[agents.claude_code]
command = "claude"
orchestrator_model = "opus"
worker_model = "sonnet"

[agents.codex]
command = "codex"
orchestrator_model = "gpt-reserve"
worker_model = "gpt-6-luna"

[terminals]
allow_worker_typing = true
font_size = 13
confirm_close = true

[notifications]
needs_decision = true
prompt = true
ready_for_review = true
broke = true
sound = false
dock_bounce = true
mute_while_viewing = true
menu_bar_dot = true

# Per project, by repository path: "all", "errors_only" or "off".
[notifications.projects]
"/Users/you/code/app" = "errors_only"

[appearance]
theme = "system"               # or "light", "dark"
easter_eggs = true
accent = "blue"                # or "honey"
swarm_view_honeycomb = false

[updates]
auto_check = true

[pull_requests]
open_as_draft = true
```
