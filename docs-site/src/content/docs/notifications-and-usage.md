---
title: Notifications and usage
description: When Swarm tells you a task needs you, the menu bar item, and how Swarm shows subscription usage.
---

This page covers the notifications Swarm sends, the menu bar item, and the usage meters for Claude and Codex.

## Notifications

A notice is a moment that needs you. The daemon decides when one happens. The app decides whether to show it as a macOS notification. Notices reach the app while its window is closed, too.

| Notification | When | Clicking it opens |
|---|---|---|
| **Needs a decision** | The orchestrator asks you something with `swarm notify` | The Terminals view, with that agent focused |
| **Agent blocked** | Any agent, the Start orchestrator too, shows a permission, question or login prompt | The Terminals view, with that agent focused |
| **Ready for review** | The task has at least one worker, the orchestrator is at rest and no worker is busy (working, blocked or starting). It fires once, and again only after a worker has worked again. It stays quiet right after the orchestrator used `swarm notify`. | [Review](/swarm/docs/review/) |
| **Agent failed** | An agent fails to start, reports a failure, or its process ends with an error. A stop you asked for does not count. One error spell gives one notice. | The Terminals view, with that agent focused |

The notification body gives the project, the task and a short text, for example `swarm · terminal-renderer: Allow cargo test?`. The **Open** action does the same as a click. A notification for a task Swarm no longer shows only brings the window back.

Prompt and failure notices can come from any agent. Only `swarm notify` is orchestrator-only.

A task has one notification at a time. A new one replaces the old one.

When a task is ready for review, a notice also shows at the bottom right of the window. It slides in from the right. When you close it, press **Review**, or wait 20 seconds, it fades out. With **Reduce motion** on in macOS, it appears and leaves without movement.

The same signal shows elsewhere: the **Needs you** pill on the project row, the ring on the task tab, and the dot on the menu bar icon.

### When Swarm stays quiet

Swarm skips a notification when:

- the trigger is off in **Settings › Notifications › Notify me when**
- the project is set to **Off**, or to **Errors only** and the notice is not a failure
- you are looking at the task: Swarm is active, its window is key, the task is open, and **Mute while viewing the task** is on

### Sound and Dock bounce

- **Play sound** is off by default. The sound plays only when macOS also shows the notification and the app's own sound setting in System Settings is on.
- **Bounce dock for urgent alerts** is on by default. It applies to **Needs a decision** and **Agent blocked**, and only while the Swarm window is not active.

### Permission

macOS must allow notifications for Swarm. **Settings › Notifications** has a **macOS permission** row: **Allowed**, **Not allowed**, **Not asked** or **Unavailable**. **Unavailable** means Swarm does not run from an app bundle. When notifications are not allowed, **Open System Settings** takes you to the right pane. **Send test notification** shows one at once.

Each project can be set to **All**, **Errors only** or **Off** on the same page.

## Menu bar

Swarm puts a bee in the macOS menu bar. Next to it is the highest used share of any usage window, for example `42%`. A dot shows while a task needs you, if **Dot on menu bar icon** is on. The wings flap while agents work.

Click the bee for the popover:

- **N agents running**
- A usage card for each agent kind, the same as in the sidebar
- **Needs you**: each waiting task, with the project, the task name and the reason. Click one to open the task.
- **Stop all**: asks **Stop N agents?**, then **Stop** stops every running agent. Cancel keeps them running.
- **Open Swarm** and **Quit**

## Usage

Swarm shows how much of your Claude and Codex subscriptions you have used. The numbers appear in the sidebar, in the menu bar popover and on **Settings › Usage**.

Each agent kind has a section with its plan name when the CLI reports one (Codex does, for example **Plus**) and one meter for each usage window. A usage window is a span of time in which a subscription allows a limited amount of use. Swarm names it by its length:

| Name | Length |
|---|---|
| **Session** | Under one day, for example the 5-hour window of Claude Code |
| **Weekly** | 7 days |
| **Monthly** | 30 days |

Any other length shows the CLI's label with "window", for example "3d window".

### Reading a meter

- The bar fills to the percent used.
- A tick on the bar marks the pace: where even use would be by now, based on how much of the window has passed.
- **Resets in 2h 6m** says when the window starts again. A window past its reset shows 0%.
- **Limit in 3d 21h** warns that at your average rate so far, you reach 100% before the window resets. The warning also changes the meter's color. It starts once a tenth of the window has passed.
- **Limit likely reached** means the average rate puts 100% already in the past, but the last reading is below it.
- **Limit reached** means the window is at 100%.

When the numbers are older than 10 minutes, the section shows **Updated 14m ago**. **Not logged in** and **Usage unavailable** explain a missing reading. A CLI that is not installed has no section.

### Where the numbers come from

- **Claude**: from the status line data that Claude Code gives to the agents Swarm runs. Swarm never reads your sign-in token. So the numbers update only while a Claude agent runs in Swarm. With none running, the last numbers stay and grow old.
- **Codex**: Swarm asks the Codex app server every 5 minutes while the app is open. Codex reads and refreshes its own token.
