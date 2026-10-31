# Swarm

Swarm runs a group of coding agents on one task. One agent leads, the others do the work, and a person reviews the result.

## Language

**Project**:
A git repository opened in Swarm.
_Avoid_: Workspace, space, repo

**Task**:
One feature inside a project, with its own branch and its own swarm.
_Avoid_: Job, session, thread

**Swarm**:
All agents working on one task.
_Avoid_: Team, fleet, herd

**Agent**:
One Claude Code or Codex process running in its own terminal.
_Avoid_: Bot, session, pane

**Orchestrator**:
The single agent that leads one task's swarm: it splits the task, spawns workers and is the only one that talks to them. The project's **Start orchestrator** instead splits a request into tasks, briefs each task's orchestrator, then exits.
_Avoid_: Dispatcher, lead, manager, conductor

**Worker**:
An agent spawned by the orchestrator to do one part of the task.
_Avoid_: Sub-agent, child, helper

**Agent kind**:
Which CLI an agent runs: Claude Code or Codex.
_Avoid_: Provider, model

**Agent status**:
What an agent is doing right now: unknown, idle, working, done, blocked (on a permission, a question or a login) or error.
_Avoid_: State, phase, waiting

**Agent model**:
The AI model an agent runs, such as opus, sonnet or gpt-reserve. Orchestrators and workers have their own default.
_Avoid_: Engine, LLM

**Message**:
Text sent between the orchestrator and a worker through Swarm.
_Avoid_: Event, ping, prompt

**Brief**:
The first message a worker or a task's orchestrator starts with: what to do and what done means.
_Avoid_: Prompt, instructions, ticket

**Worktree**:
A separate checkout of the project. Each task has a task worktree; a worker gets its own worktree only when the orchestrator has a reason.
_Avoid_: Sandbox, copy, clone

**Task branch**:
The branch that collects the work of all workers in a task; the PR is opened from it.
_Avoid_: Integration branch, feature branch

**Swarm view**:
The view that shows a task's swarm as a graph of agents and the messages between them, with an inspector for the selected agent.
_Avoid_: Graph view, map

**Inspector**:
The panel in the Swarm view that shows the selected agent: its details, messages and a live terminal preview.
_Avoid_: Details pane, sidebar

**In flight**:
A message that was sent but not yet delivered to its recipient.
_Avoid_: Pending, queued, unread

**Review**:
The step where a person reads the combined diff of a task and approves it or requests changes.
_Avoid_: Code review session, check

**Task worktree**:
The worktree that holds a task branch; each tab in the window shows one.
_Avoid_: Space, workspace

**Terminal**:
One running program shown in Swarm: the orchestrator, a worker, a process (dev server, tests) or the user's own shell.
_Avoid_: Pane, shell

**Tile**:
A terminal's place in the workspace layout: its position, size and frame. The layout engine arranges tiles; tiles that don't fit go to the tray.
_Avoid_: Pane, window, panel

**Process terminal**:
A terminal running a command an agent started, such as a dev server or a test run.
_Avoid_: Job, task terminal, background terminal

**User terminal**:
A terminal the user opened that no agent can see or touch.
_Avoid_: God mode terminal, private terminal

**Tray**:
The strip of chips in the status bar where tiles go when they don't fit on screen.
_Avoid_: Dock, minimized bar

**Daemon**:
The background process that keeps agents running when the app is closed.
_Avoid_: Server, service, backend
