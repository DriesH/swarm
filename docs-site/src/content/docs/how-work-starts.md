---
title: How work starts
description: From a plan to tasks, orchestrators and workers, and how they talk.
---

This page explains who does what once you send a plan: the Start orchestrator, task orchestrators, workers, and the messages between them.

## From plan to tasks

A **task** is one feature of a project. It has its own branch and its own swarm, which is all the agents working on it.

The **Start orchestrator** reads your plan. It counts the features in it. A feature is one change a user can see or use, that can be built and reviewed on its own.

1. With more than one feature, or when it is unsure, it lists the features in its terminal. You talk with it until you agree on the list.
2. It makes exactly one task for each agreed feature.
3. For each task, Swarm makes the task branch and the **task worktree**, a separate checkout of the project. Then it opens a tab and starts that task's orchestrator.
4. The Start orchestrator waits until every task orchestrator confirms its brief. Then it exits.

The task branch name starts with `swarm/`.

## The brief

The **brief** is the first message a task's orchestrator starts with. It states the goal, the scope and what done means. The Start orchestrator keeps your own requirements and definition of done in the brief of the task they belong to, in your words.

## Orchestrators and workers

The **orchestrator** leads one task. It splits the task, starts **workers** and is the only agent that talks to them. Each worker gets a brief of its own.

- Workers share the task worktree by default. They work on parts that touch different files and commit only their own files.
- The orchestrator gives a worker its own worktree and branch only with a stated reason. Two reasons exist: parts that must change the same files at the same time, and an experiment it may throw away. The orchestrator merges that branch back into the task branch.

- Every helper of the orchestrator is a worker started with `swarm spawn`, including read-only reviews and research. Swarm denies the built-in subagent tools of a Claude Code orchestrator.
- When a worker's report is checked and the orchestrator has nothing more for it, the orchestrator runs `swarm stop`. The worker stops like an idle agent, stays resumable and leaves the tile layout. It stays in the [Swarm view](/swarm/docs/swarm-view/), dimmed, with **Resume** in the inspector.
- A task without a name starts with a bee or wasp name. After it confirms its brief, the orchestrator renames the task, its tab and its branch to say what the task is about. The worktree folder keeps its first name. A task with a pull request or a pushed branch keeps its name.

Review sorts the changed files by worktree, then by path. Each file card names the agent of the file's newest commit.

## Start work later

The **+** in the tab bar opens a menu:

- **New task** (⌘T): creates one task without the Start orchestrator.
- **Plan with orchestrator** (⇧⌘T): opens the project's Start tab with the plan editor. It starts no agent. Send the plan to start the Start orchestrator. A project has one Start tab at a time.

The tab reads **Start** while every task of the project came from the Start orchestrator, or the project has no task yet. It reads **Plan** once a task exists that the Start orchestrator did not make, for example one you made with **New task**.

The **New task** sheet has these fields:

| Field | What it does |
|---|---|
| **Name** | Names the task. Optional. The field shows a name to use when you leave it empty: the Latin name of a bee or wasp, such as `apis-mellifera`, that no open task or branch uses. |
| **Branch from** | The branch the task starts from. It lists your local branches, so you can pick up the kept branch of a closed task. |
| **Orchestrator** | Claude Code or Codex. |
| **First message** | Optional. The orchestrator starts with it. |
| **Run setup script** | On by default. Runs the project's setup script in the new worktree. |

Click **Create task**. Swarm makes the worktree and the tab, and starts one orchestrator.

## Messages

A **message** is text sent between the orchestrator and a worker through Swarm.

- Workers never message each other. A worker asks or reports to its orchestrator. Only the orchestrator sends messages to workers.
- You can type into a worker terminal. Swarm tells the orchestrator when you do. The setting **Allow typing in worker terminals** in **Settings › Agents** turns this on or off. It is on by default.

The [Swarm view](/swarm/docs/swarm-view/) shows these messages as edges between agents.

## Your own instructions still apply

Swarm adds its instructions to each agent's own. It does not replace them. Your `CLAUDE.md`, `AGENTS.md`, rules, skills, hooks and MCP servers keep working inside every agent.

## Agent status

Every agent shows a status:

| Status | Meaning |
|---|---|
| Unknown | Swarm has no status for the agent yet. |
| Idle | The agent is not working. |
| Working | The agent is working. |
| Done | The agent finished. |
| Blocked | Waiting on you. The detail says what: **permission**, **question** or **login**. A task with a blocked agent shows **Needs you**. |
| Error | The agent failed or ended with an error. |

## Idle stop and resume

An agent that stays **Idle** or **Done** for the time in **Settings › Agents › Stop idle agents after** (30 minutes by default) stops. Its conversation is kept, so it can resume. Its row in the sidebar then says **resumable**.

Idle stop never stops:

- the Start orchestrator
- an orchestrator while one of its workers runs
- an agent with a process terminal of its own that still runs
- an agent you are typing into
- an agent with messages it has not read yet

A resumable agent starts again with its saved conversation (`claude --resume` or `codex resume`) when:

- a message arrives for it, from its orchestrator, a worker or a review comment. Swarm types the message in once the agent is ready.
- you select **Resume** in its inspector in the [Swarm view](/swarm/docs/swarm-view/).
- you run **Resume agent “name”** from the command palette (⌘K).

Agents that were running when the Mac restarted, or when the daemon crashed, become resumable the same way. They wait for a message or for you.

An agent that has no saved conversation cannot resume. An agent that crashes is not resumed for messages that are already waiting. It resumes with the next message.

## Agent limits

**Settings › Agents** limits how many agents run at the same time: 6 per task and 16 in total by default. Swarm refuses to start or resume an agent past a limit. The agent or message that asked for it gets an error that names the limit, such as `the task has 6 agents running, the limit is 6 (Settings › Agents)`.

## Close a task

Close a task from the close button on its tab, from **File › Close Task**, or from **Close task** in the command palette (⌘K).

A dialog lists what closing stops and deletes: running agents, terminals and uncommitted files. Then the agents and terminals stop and the worktrees are removed.

Swarm deletes the task branch only when it is already merged into the base branch. Every other branch stays. Only uncommitted work is lost.
