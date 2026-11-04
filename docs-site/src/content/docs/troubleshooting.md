---
title: Troubleshooting
description: Fixes for system check failures, the daemon, notifications and usage numbers.
---

Each entry names what you see and what to do.

## System check

### A tool shows "not installed" but it is installed

Swarm looks for `claude`, `codex`, `git` and `gh` in your `PATH`, then in `/opt/homebrew/bin`, `/usr/local/bin`, `~/.local/bin` and `~/.claude/local`. An app opened from Finder has a short `PATH`. A tool in another folder, such as one a version manager installed, is not found.

For Claude Code and Codex, set the full path:

1. Open **Settings › Agents**.
2. Put the full path of the program in **Command**, under **Claude Code** or **Codex**.
3. Click **Check again** in **Settings › General**.

### A row shows a reason and **Check again**

The check itself failed, for example because a command did not end within 5 seconds. Click **Check again**. The reason stays in the row until the check passes.

### Git: **Install** opens an Apple dialog

Git needs the Xcode Command Line Tools. **Install** runs `xcode-select --install`, which opens the macOS installer. Click **Check again** when it ends.

### Claude Code or Codex shows "not signed in"

- Claude Code counts as signed in when the Keychain has the item `Claude Code-credentials`. Click **Sign in**, finish the sign-in in the terminal, and the row checks again.
- Codex counts as signed in when the file `~/.codex/auth.json` exists. `CODEX_HOME` does not count, because agents do not get it.

### GitHub account says "needs GitHub CLI"

The account row waits for the GitHub CLI. Install the GitHub CLI first. Its **Sign in** button then works.

## The daemon

### The sidebar says **Daemon not running**

The app cannot reach the daemon. Click **Retry**. The app starts the daemon from its own folder. If the daemon does not start, its log is `~/Library/Application Support/Swarm/daemon.log`.

### **Retry** keeps failing after an update

An older daemon can still run after you update the app. The new app cannot talk to it. Stop it in a terminal, then click **Retry**:

```sh
pkill -x swarm-daemon
```

:::caution
Stopping the daemon ends every running agent.
:::

## Notifications

### No notifications show

Check these in order:

1. In **Settings › Notifications**, read the **macOS permission** row. **Not asked** means macOS has not asked you yet. Click **Allow** under **Notifications** in **Settings › General**. **Not allowed** means you turned them off. Click **Open System Settings** and turn them on.
2. **Unavailable** means Swarm does not run from the app bundle. macOS then gives no answer.
3. Check the switch for the kind of notification under **Notify me when**.
4. Check the project's setting under **Projects**: **All**, **Errors only** or **Off**.
5. **Mute while viewing the task** is on by default. Swarm shows nothing for a task while you look at it.
6. Click **Send test notification** to test the path from Swarm to macOS.

### No sound

**Play sound** in **Settings › Notifications** is off by default. The sound plays only when macOS shows the notification and the sound setting for Swarm in System Settings is on.

## Usage

### Claude usage shows old numbers

Swarm gets Claude usage only from Claude Code agents that Swarm runs. With no Claude Code agent running, it shows the last numbers it has. After 10 minutes the line **Updated … ago** shows how old they are. A usage window whose reset time has passed shows 0%.

Start a Claude Code agent to get new numbers.
