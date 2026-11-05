---
title: Harness reference
description: The environment, the swarm command, the MCP tools and the messages that every Swarm agent gets. For people who write prompts.
---

The harness is what Swarm gives each agent so it can work in a swarm: environment variables, the `swarm` command and the same calls as MCP tools. This page lists all of it. If you write your own [prompts](/swarm/docs/settings/#prompts), start here.

Swarm adds a fixed harness block to every agent's prompt before the role text. It names the agent, its task, its role and its commands. You cannot edit that block.

## Environment

Every agent terminal gets these variables, and finds `swarm` on its `PATH`.

| Variable | Value |
|---|---|
| `SWARM_SOCKET` | The address `swarm` connects to. |
| `SWARM_AGENT_ID` | The agent's number. |
| `SWARM_AGENT_NAME` | The agent's name. |
| `SWARM_TASK` | The task's number. The Start orchestrator has no task, so it does not get this. |
| `SWARM_TOKEN` | The agent's own secret. Swarm checks it on every call. |
| `SWARM_ROLE` | `orchestrator`, `worker` or `start_orchestrator`. |
| `SWARM_PORT`, `SWARM_PORTS` | The first port, and all ports separated by commas, of the task. Set only when the project reserves [ports](/swarm/docs/project-config/). A worker with its own worktree reserves its own block, and `spawn` fails when no block is free. |

## Who may call what

Swarm checks the caller's role on every call. A call from the wrong role fails with a message that says who may make it.

| Role | Commands |
|---|---|
| **Orchestrator** (leads one task) | `spawn`, `send`, `agents`, `merge`, `stop`, `notify`, `confirm`, `task rename`, `pr draft`, `tile grow` and `tile shrink` for its workers, `commit`, `read`, `wait`, `terminal` |
| **Worker** | `ask`, `report`, `tile grow` and `tile shrink` for itself, `commit`, `read`, `wait`, `terminal` |
| **Start orchestrator** (splits your plan into tasks, then exits) | `task create`, `task list`, `task wait`, `read`, `wait`, `terminal` |

Every agent can run `swarm exit` in its shell. Only the Start orchestrator gets the matching MCP tool, `agent_exit`.

- A worker cannot message another worker. It talks to its orchestrator with `ask` and `report`.
- The orchestrator is the only agent that talks to its workers.
- Every helper agent of an orchestrator is a worker started with `swarm spawn`. For a Claude Code orchestrator or Start orchestrator, Swarm denies the built-in `Agent` and `Workflow` tools. A Codex orchestrator gets the same rule in its prompt only. Workers keep their own subagents.
- Every agent can run process terminals. See [Terminals](#terminals) for who may read and stop them.

:::note
User terminals, the ones you open yourself, are off-limits to agents. No agent can list, read, type into, resize or kill them.
:::

## Run the commands

Run `swarm` in the agent's shell, or call the matching MCP tool. Claude Code can do either. A Codex agent started without **Skip permission prompts** cannot reach Swarm from its shell, because of the Codex sandbox. It uses the MCP tools. See [MCP tools](#mcp-tools).

Rules for every command:

- `--json` prints the raw result as JSON. It works anywhere on the line.
- A text argument of `-` reads the text from stdin.
- A command exits with a non-zero status when it fails. `merge` with conflicts, `task wait` that times out and `terminal wait` that times out also exit non-zero.
- A message is at most 64 KiB.
- `swarm --help` and `swarm <command> --help` print the details.

## Messages

| Command | Who | Arguments and flags | What it does |
|---|---|---|---|
| `swarm spawn <name>` | Orchestrator | `--brief TEXT` · `--kind claude\|codex` · `--model M` · `--own-worktree REASON` | Starts a worker. `name` becomes part of its branch. The brief is its first message; without `--brief` it is read from stdin and must not be empty. `--kind` defaults to your own kind. `--model` defaults to the worker model in [Settings](/swarm/docs/settings/#agents). The worker shares the task worktree. With `--own-worktree` it gets its own worktree and branch, and you must give a reason. |
| `swarm send <agent> <text>...` | Orchestrator | `agent` is a worker's ID or name | Sends a message to one of your workers. |
| `swarm ask <text>...` | Worker | – | Asks your orchestrator a question. The answer comes back as a message. |
| `swarm report <text>...` | Worker | – | Sends your orchestrator a progress report or your final result. |
| `swarm notify <text>...` | Orchestrator | – | Asks the user for attention, with a reason. |
| `swarm read` | Everyone | – | Takes your unread messages. |
| `swarm wait` | Everyone | `--timeout SECS` (default 100, at most 600) | Waits for a message, then takes your unread messages. Returns nothing at the timeout. |

Swarm types a message you have not read into your terminal when you are idle. `read` and `wait` take messages first.

## Work with git

| Command | Who | Arguments and flags | What it does |
|---|---|---|---|
| `swarm commit -m <message> <path>...` | Orchestrator, worker | `-m`, `--message` · one or more paths | Commits the named files, and nothing else, in your worktree. The author is `<name> (Swarm)`, and a `Swarm-Agent` trailer names you. Use it instead of `git commit`. Paths are relative to your worktree. |
| `swarm merge <worker>` | Orchestrator | a worker's ID or name | Merges the branch of a worker that has its own worktree into the task branch. On conflicts it lists the files, leaves the merge open and exits non-zero. A worker in the task worktree has nothing to merge. |
| `swarm stop <worker>` | Orchestrator | a worker's ID or name | Ends a worker you have no follow-up for. It stops the way [idle stop](/swarm/docs/how-work-starts/#idle-stop-and-resume) does: it stays resumable, you get no `worker stopped` message, and a later `send` brings it back. Refused for a worker with its own worktree that is not merged. |
| `swarm agents` | Orchestrator | – | Lists your workers with status, branch and worktree. |
| `swarm confirm` | Orchestrator | – | Says that you understood your brief. |
| `swarm pr draft --title <title>` | Orchestrator | `--body TEXT` or `--body-file PATH` (`-` reads stdin); one of the two is required | Sends the pull request title and description that a `pr draft` message asks for. The title is one line of at most 256 characters. Refused until the user has approved the task. |

## Tiles

| Command | Who | Arguments | What it does |
|---|---|---|---|
| `swarm tile grow [agent]` | Orchestrator, worker | An orchestrator names one of its workers. A worker leaves `agent` out. | Asks for a large tile. |
| `swarm tile shrink [agent]` | Orchestrator, worker | Same | Asks for a small tile again. |

The layout decides. It ignores the request for a tile the user minimized. No agent gets a message.

## Tasks

`task create`, `task list` and `task wait` are for the Start orchestrator. `task rename` is for a task's orchestrator. `exit` works for every agent.

| Command | Arguments and flags | What it does |
|---|---|---|
| `swarm task rename <name>` | – | Renames your task, its tab and its branch (`git branch -m`). The worktree folder keeps its name. You can rename more than once. Refused when the task has a pull request, when the branch was pushed, when the name is taken by an open task or a branch, and when the name has no letters or digits. |
| `swarm task create [name]` | `--brief TEXT` · `--from BRANCH` · `--kind claude\|codex` | Makes a task: a branch, a task worktree, a tab and an orchestrator that starts on the brief. Without a name the task is named after a bee or wasp, such as `apis-mellifera`. The brief states the goal, the scope and what done means. Without `--brief` it is read from stdin and must not be empty. `--from` defaults to the base branch. `--kind` defaults to the orchestrator agent in Settings. |
| `swarm task list` | – | Lists the tasks of the project. |
| `swarm task wait <id>...` | `--until briefed\|done` (required) · `--timeout SECS` (default 100, at most 600) | Waits until every task is `briefed` (its orchestrator confirmed the brief) or `done`. Prints `timed out` and exits non-zero if not. |
| `swarm exit` | – | Ends your agent. Call it when your work is done. |

## Terminals

Process terminals run a command an agent started, such as a dev server or a test run.

| Command | Arguments and flags | What it does |
|---|---|---|
| `swarm terminal spawn -- <command>...` | `--name N` · `--cwd DIR` · `--size S` | Runs the command in a small tile and prints its ID. No shell is added: pass `sh -c` yourself when you need one. `--name` is a label of at most 60 characters. `--cwd` is a folder inside your worktree and defaults to the worktree. `--size` is ignored. |
| `swarm terminal list` | – | Lists the process terminals of your task. |
| `swarm terminal read <id>` | `--lines N` | Reads the screen, or the last `N` lines (at most 2000). |
| `swarm terminal wait <id>` | `--exit` or `--output TEXT` (one is required) · `--timeout SECS` (default 100, at most 600) | Waits for the process to exit, or for a line that contains `TEXT`. A line already on screen or in the scrollback counts. |
| `swarm terminal kill <id>` | `--signal TERM\|INT\|KILL\|HUP` (default `TERM`) | Stops the process. |
| `swarm terminal resize <id> <size>` | `size` is `small`, `medium`, `large` or `COLSxROWS` | Prints the terminal's size. The tile sets the size, so the request changes nothing. |

Who may use a process terminal:

- Any agent can start one and list the ones in its task.
- Only the agent that started it, or the orchestrator of its task, can read it, wait for it and stop it.
- Only the agent that started it can resize it.

## Commands Swarm sets up

| Command | What it does |
|---|---|
| `swarm mcp` | Runs the MCP server on stdin and stdout. Swarm configures it for each agent. |
| `swarm hook` | Forwards the hook events of Claude Code and Codex to Swarm. Swarm sets it up. |
| `swarm statusline` | Sends the usage limits from Claude Code's status line to Swarm, then runs your own status line. Swarm sets it up. |

## MCP tools

The `swarm` MCP server lists only the tools your role may call. A tool has the name of its command, with `_` between the words: `swarm terminal spawn` is `terminal_spawn`, and `swarm exit` is `agent_exit`. Parameters marked with * are required.

| Tool | Who | Parameters |
|---|---|---|
| `spawn` | Orchestrator | `name`\*, `brief`\*, `kind` (`claude` or `codex`), `model`, `own_worktree` (the reason) |
| `send` | Orchestrator | `to`\* (ID or name), `text`\* |
| `agents` | Orchestrator | – |
| `merge` | Orchestrator | `worker`\* |
| `stop` | Orchestrator | `worker`\* |
| `notify` | Orchestrator | `text`\* |
| `confirm` | Orchestrator | – |
| `task_rename` | Orchestrator | `name`\* |
| `pr_draft` | Orchestrator | `title`\*, `body`\* |
| `ask` | Worker | `question`\* |
| `report` | Worker | `text`\* |
| `commit` | Orchestrator, worker | `message`\*, `paths`\* (list of strings) |
| `tile_grow`, `tile_shrink` | Orchestrator, worker | `agent` (an orchestrator names a worker; a worker leaves it out) |
| `task_create` | Start orchestrator | `brief`\*, `name`, `from`, `kind` |
| `task_list` | Start orchestrator | – |
| `task_wait` | Start orchestrator | `ids`\* (list of integers), `until`\* (`briefed` or `done`), `timeout_secs` |
| `agent_exit` | Start orchestrator | – |
| `read` | Everyone | – |
| `wait` | Everyone | `timeout_secs` |
| `terminal_spawn` | Everyone | `command`\* (list of strings), `name`, `size` (ignored), `cwd` |
| `terminal_list` | Everyone | – |
| `terminal_read` | Everyone | `id`\*, `lines` |
| `terminal_wait` | Everyone | `id`\*, `output` (without it, waits for the exit), `timeout_secs` |
| `terminal_kill` | Everyone | `id`\*, `signal` |
| `terminal_resize` | Everyone | `id`\*, `size`\* (ignored) |

## Limits and resumable agents

- `spawn` and `task create` fail when a start would go over an [agent limit](/swarm/docs/how-work-starts/#agent-limits). The error is `the task has N agents running, the limit is M (Settings › Agents)` or `N agents are running, the limit is M (Settings › Agents)`.
- A worker that [idle stop](/swarm/docs/how-work-starts/#idle-stop-and-resume) or `swarm stop` stopped is resumable. `swarm agents` still lists it. Neither sends a `worker stopped` message.
- `send`, `ask` and `report` to a resumable agent resume it first. Swarm types the message in once the agent is ready. When the resume is refused, for example by a limit, the command fails with the reason.

## Messages from Swarm

Swarm sends these messages itself. They arrive like any other message, through `read`, `wait` or the terminal, and each has a kind.

| Kind | Sent to | When |
|---|---|---|
| `brief` | A worker or a task's orchestrator | It starts. This is its first message. |
| `task` | A worker | Its orchestrator used `send`. |
| `question`, `report` | The orchestrator | A worker used `ask` or `report`. |
| `terminal exited` | The agent that started a process terminal, and the orchestrator of its task | The terminal ended. The text names the terminal and its ID, says who stopped it (the process itself, an agent or the user) and gives the exit code or signal. The owner and the orchestrator both hear of it. If the owner's process ended first, only the orchestrator hears. If you stopped the owner, nobody hears. |
| `user input` | The orchestrator | You typed into a worker's terminal. |
| `worker stopped` | The orchestrator | A worker failed to start, or its process ended. |
| `comment` | The orchestrator | You left [review comments](/swarm/docs/review/). The orchestrator passes each one to the worker that owns the file. |
| `pr draft` | The orchestrator | You asked for a [pull request](/swarm/docs/pull-requests/). It answers with `swarm pr draft`. |
| `approval reset` | The orchestrator | The task changed after you [approved](/swarm/docs/review/#approve) it. The approval and the draft are gone. |
