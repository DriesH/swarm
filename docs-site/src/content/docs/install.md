---
title: Install Swarm
description: Requirements, download, and what the first launch checks.
---

This page covers what Swarm needs, how to install it, and what the system check does on first launch.

## Requirements

- A Mac with Apple silicon. There is no Intel build.
- macOS 13 or later.
- Git.
- Claude Code, Codex, or both. Swarm starts agents with these tools and does not ship them.
- Optional: the GitHub CLI (`gh`), for pull requests.

## Install

1. Download the latest release from [GitHub](https://github.com/DriesH/swarm/releases/latest).
2. Open `Swarm.dmg`.
3. Move `Swarm.app` to your Applications folder.
4. Open Swarm.

## The first launch

The first time you open Swarm with no projects, it shows a welcome screen with the **System check**. Each row is one tool. A row says what it found: a version, **installed**, **signed in**, **not installed** or **not signed in**.

| Row | What it looks for | What its button does |
|---|---|---|
| Git | The Xcode Command Line Tools, then `git`. | **Install** runs `xcode-select --install`. |
| Claude Code | The `claude` command, and a Claude Code sign-in in your Keychain. Swarm asks whether the item exists. It never reads it. | **Install** runs the official Claude Code installer. **Sign in** runs `claude` once. |
| Codex | The `codex` command, and the file `~/.codex/auth.json`. | **Install** uses Homebrew if you have it, else the official Codex installer. **Sign in** runs `codex` once. |
| GitHub CLI | The `gh` command. | **Install** uses Homebrew if you have it. Else it downloads `gh` into `~/Library/Application Support/Swarm/bin/`. Nothing is installed system-wide. |
| GitHub account | A signed-in `gh`. | **Sign in** runs `gh auth login --web` for github.com. The button is dimmed while the GitHub CLI is missing. |
| Notifications | The macOS permission for Swarm. | **Allow** shows the macOS question. **Open System Settings** opens the Notifications pane when you turned them off. |

A fix never runs silently. It runs in a terminal under its row, and the first line shows the command. When the command ends, the row checks again. The terminal stays until the row is ready.

The check runs again when the window becomes active. **Check again** runs it by hand. If a check itself fails, the row shows the reason and a **Check again** button.

Without `gh`, Review and **Approve** still work. The pull request button reads **Set up GitHub** instead of **Create PR**.

### Leave the welcome screen

- **Open a project** closes the welcome screen and opens the folder picker.
- **Skip for now** closes it and moves the focus to **Add project**.

Both mean the screen does not show again. The same check stays in **Settings › General**, under **System**. A fix that still runs when you close the welcome screen stops.

## Updates

Swarm checks [GitHub Releases](https://github.com/DriesH/swarm/releases) for a newer version. It checks 30 seconds after launch and then every 6 hours. Turn this off in **Settings › General**. **Check now** and **Swarm › Check for Updates…** always check.

Swarm downloads the new version in the background and checks its signature. The status bar then shows **Update ready**, with **Restart to update** and **Release notes**. Swarm installs the update when you press **Restart to update** or when you quit. The old app goes to the Trash.

An unsigned build, such as one you built yourself, never updates. If Swarm cannot replace itself in its folder, the button reads **Download** and opens the release page.

After an update, the daemon restarts when no agent works. Else Swarm shows **Restart daemon**, and the daemon restarts only when you confirm. A restart stops every agent terminal.

## The background daemon

Swarm starts a background process, the daemon, when you open the app. The daemon owns every agent and every terminal. The app is a window onto it.

- Close the window or quit the app with ⌘Q, and the agents keep running.
- Open the app again and it connects to the daemon again.
- Closing the window leaves Swarm in the menu bar. Its popover shows the running agent count and **Stop all**.
