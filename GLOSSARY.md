# Swarm

Swarm runs a group of coding agents on one task. One agent leads, the others do the work, and a person reviews the result.

## Language

**Project**:
A git repository opened in Swarm.
_Avoid_: Workspace, space, repo

**Task**:
One piece of work inside a project, with its own branch and its own swarm.
_Avoid_: Job, session, thread

**Swarm**:
All agents working on one task.
_Avoid_: Team, fleet, herd

**Agent**:
One Claude Code or Codex process running in its own terminal.
_Avoid_: Bot, session, pane

**Orchestrator**:
The single agent that leads a swarm: it splits the task, spawns workers and is the only one that talks to them.
_Avoid_: Lead, manager, conductor

**Worker**:
An agent spawned by the orchestrator to do one part of the task.
_Avoid_: Sub-agent, child, helper

**Agent kind**:
Which CLI an agent runs: Claude Code or Codex.
_Avoid_: Provider, model

**Agent status**:
What an agent is doing right now: working, waiting, done or idle.
_Avoid_: State, phase

**Message**:
Text sent between the orchestrator and a worker through Swarm.
_Avoid_: Event, ping, prompt

**Worktree**:
A separate checkout of the project where one worker makes its changes.
_Avoid_: Sandbox, copy, clone

**Task branch**:
The branch that collects the work of all workers in a task; the PR is opened from it.
_Avoid_: Integration branch, feature branch

**Review**:
The step where a person reads the combined diff of a task and approves it or requests changes.
_Avoid_: Code review session, check

**Daemon**:
The background process that keeps agents running when the app is closed.
_Avoid_: Server, service, backend
