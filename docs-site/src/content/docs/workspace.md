---
title: The workspace
description: The sidebar, tabs, terminal tiles, tray, status bar and command palette.
---

This page covers the main window: where your projects and tasks are, how terminals sit on screen, and how to find things fast.

## Sidebar and rail

The sidebar has two lists.

- **Projects**: one row per project, with its task count. A row shows **Needs you** when a task waits for you, a ring while agents work, and **Config error** when the project's config file has a problem. The **+** button adds a project.
- **Agents**: the open task's agents, then your user terminals. A process terminal sits under the agent that started it. A dot shows each status.

Right-click a project row, or press ⇧F10 on it, for **Show in Finder**, **Project settings** and **Remove project…**. See [Review](/swarm/docs/review/#remove-a-project) for the last one.

Press ⌘B, or use the **Toggle sidebar** button in the title bar, to shrink the sidebar to a narrow rail. The rail keeps search, one tile per project, **+**, and one icon per agent, process terminal and user terminal.

## Tabs

Each task has a tab at the top of the window. A tab shows the task name and a ring: working, needs you or idle. The close button on a tab starts [Close task](/swarm/docs/review/#close-a-task).

- The **Start** or **Plan** tab shows before anything runs, with the plan editor, and while the project's Start orchestrator runs. It has no branch and no **Files** or **Changes** button. See [How work starts](/swarm/docs/how-work-starts/).
- Tabs shrink to a minimum width, then scroll. A button then lists every task.
- The **+** button opens the **New tab** menu: **New task** and **Plan with orchestrator**.

The right side of the title bar has the view switch **Terminals | Swarm | Review**, then the **Files** button and the **Changes** button with the task's additions and deletions. After you approve a task, a **Pull request** button joins them. See [Pull requests](/swarm/docs/pull-requests/).

## Terminals view

The Terminals view shows every terminal of the open task as a tile. A tile is a terminal's place on screen. You see four kinds:

| Kind | Who types | Size |
|---|---|---|
| Orchestrator | You | Large, always on screen |
| Worker | You, unless **Allow typing in worker terminals** is off in Settings | Small box; grows when it needs you |
| Process terminal | You | Smallest box |
| User terminal | You only | The size you set |

The layout engine places tiles as follows.

- The orchestrator takes the left column: about two thirds of the width and the full height.
- Workers are boxes stacked from the top of the column to its right. Each box shows about 12 rows. When a column is full, the next box starts another column.
- A worker that is blocked or in error grows to twice the height. It shrinks when it works again.
- Process terminals are the smallest boxes, about 8 rows, below the workers. One that exited with a failure grows the same way.
- An orchestrator can grow or shrink its workers with `swarm tile grow` and `swarm tile shrink`. A worker can change only itself. A tile you moved to the tray yourself ignores these calls.
- Your user terminals keep their size. The other tiles move around them.
- A tile keeps its place when its status changes.

A tile with focus has a 2 px border: the accent color for an agent, the text color for your own terminal. An agent that needs you has an amber border. Click a tile, click its row in the sidebar, or press ⌘1 for the orchestrator and ⌘2 to ⌘9 for workers.

## The tray

When tiles do not fit, the layout engine moves the least important ones to the tray. From most to least important, tiles stay on screen in this order:

1. User terminals
2. The orchestrator
3. Tiles that need attention (blocked or error)
4. Working agents
5. Running process terminals
6. Done agents
7. Idle agents

The tray sits on the left of the status bar. Each chip shows an icon, a name and a status. Click a chip to focus its tile. This restores a tile you moved to the tray yourself. The layout decides about the others. Chips that do not fit sit behind a **+N** chip, which opens a menu of them.

To move a worker or process terminal to the tray yourself, use **Move … to tray** on its tile. It stays there until you restore it. The orchestrator tile has no such button.

## User terminals

A user terminal is your own shell. Open one with the **Terminal** button in the status bar, **File › New Terminal**, ⌥⌘T, or **New terminal** in the palette.

User terminals are off-limits to agents. No agent can see, read, type into, resize or close them, so a secret you type there never reaches an agent. Their tile header shows a user icon and **Mine** after the name.

- **Resize**: drag the bottom edge of the tile. With the handle focused, use ↑ and ↓, or Home and End.
- **Close**: use the close button on the tile or on its sidebar row. If a program still runs, Swarm asks **Close terminal “name”?** and says "The program in it stops." Focus starts on **Cancel**, and Esc cancels. Tick **Do not show this again** to stop the question. You can turn it back on with **Ask before closing a terminal** in Settings › Terminals.
- A terminal whose program has ended closes at once. So does a close that Swarm starts, for example when you close the task.

## Process terminals

An agent starts a process terminal with `swarm terminal spawn`, for a dev server or a test run. Its tile has a **Stop …** button.

- When the process exits with code 0, the tile closes after about 3 seconds.
- When it exits with another code, or a signal ends it, the tile stays with the exit code and grows. Close it yourself.

## Files panel

The **Files** button opens a panel with the files of the task worktree as a folder tree: every file git tracks, and new files git does not ignore. Changed files are marked. The list updates when the task's changes change.

The panel lists at most 20 000 files. A longer list ends with a row such as **30 more files**. A worktree without files shows **No files**.

## Changes panel

The **Changes** button opens a panel with the task's changed files as a folder tree. Each file shows its additions and deletions, and the agent of its newest commit. Click a file to open [Review](/swarm/docs/review/) at that file. The **Review** button opens Review itself.

The panel is available in the Terminals view only. Review has its own file tree.

## Status bar

The left side has the tray and the **Terminal** button. The right side shows, with icons, the task's branch, its agent count, its process count, its user terminal count (when above zero), **permissions skipped** when **Skip permission prompts** is on in **Settings › Agents**, and ⌘K. In Review, the bar also shows how many comments are sent, pending and resolved.

## Command palette

Press ⌘K, or use the search button in the sidebar. Type to filter by name, use ↑ and ↓ to move and ↩ to run. Esc closes it. Shortcuts show next to each result.

| Group | Contents |
|---|---|
| **Suggested** | **Review changes** ⇧⌘R, **Focus orchestrator** ⌘1, **New task** ⌘T, **Plan with orchestrator** ⇧⌘T, and **Create pull request** when the task can have one |
| **Tasks** | Every task of the project, with its status |
| **Agents** | Every worker, with its status and ⌘2 to ⌘9, and **Resume agent “name”** for each resumable agent |
| **Swarm** | **Close task**, **New terminal** ⌥⌘T, **Settings** ⌘, and, in Review, **Next file**, **Previous file**, **Toggle viewed** and **Comment on line** |

## Keys inside a terminal

| Key | What it does |
|---|---|
| Tab, ⇧Tab | Goes to the program in the terminal |
| ⌃Tab, ⌃⇧Tab | Moves focus out of the terminal, to the next or previous place |
| ⌘V | Pastes |

The [Keyboard](/swarm/docs/keyboard/) page lists every shortcut.

## Paste and drop

- Copy text, then press ⌘V. The text goes to the program as a paste.
- Copy an image, then press ⌘V. Swarm sends Ctrl-V, and programs that read the clipboard image, such as Claude Code, take it from there.
- Drop files from Finder onto a tile, or paste files you copied in Finder. Swarm types their paths, quoted, into the program. A program like Claude Code attaches an image path it finds in its input. The tile gets an accent border while you drag over it.

Paste and drop into a worker terminal work only while **Allow typing in worker terminals** is on.
