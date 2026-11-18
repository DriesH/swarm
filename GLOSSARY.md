# Swarm

Swarm runs a group of coding agents on one task. One agent leads, the others do the work, and a person reviews the result.

## Language

**Project**:
A git repository opened in Swarm.
_Avoid_: Workspace, space, repo

**Home**:
The place above Projects for quick work outside a project. Its tabs are scratches.
_Avoid_: Hub, scratchpad, dashboard

**Scratch**:
One Home tab: terminals that start in the scratch folder, with no branch or worktree. It has its own agents, which work alone.
_Avoid_: Scratchpad, playground, sandbox, session

**Make project**:
Adding the folder of a scratch's terminal as a project, with a git repository and a first commit when it has none. No files move.
_Avoid_: Promote, convert, graduate

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
The single agent that leads one task's swarm: it splits the task, spawns workers and is the only one that talks to them. The project's **Start orchestrator** instead splits the user's plan into tasks, briefs each task's orchestrator, then exits.
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

**Plan**:
What the Start orchestrator gets: what the feature is, its requirements and its definition of done. The user writes it, or a grill agent writes it from the user's answers. A plan not sent yet is a **plan draft**.
_Avoid_: Prompt, spec, request

**Grill agent**:
The agent that asks the user questions about an idea, one at a time, and writes the plan from the answers. It ends before the plan is sent.
_Avoid_: Planner, interviewer

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
The panel in the Swarm view that shows the selected agent: its details, messages, changes and its live terminal.
_Avoid_: Details pane, sidebar

**In flight**:
A message that was sent but not yet delivered to its recipient.
_Avoid_: Pending, queued, unread

**Review**:
The part of a task where a person reads its changes, by file or as a story, and approves them or requests changes.
_Avoid_: Code review session, check

**Live**:
The Review source that shows the work in progress: the task worktree and each worker's own worktree that is not merged yet.
_Avoid_: Current, working copy

**Merged**:
The Review source that shows only the commits of the task branch: what the pull request would hold.
_Avoid_: Final, combined

**Approve**:
The user's sign-off on the Merged changes of a task. It holds only while those changes stay the same, and a pull request needs it.
_Avoid_: Accept, sign off, LGTM

**Comment**:
A note the user leaves in Review, on one line, on a step or on a story. It goes through the orchestrator to the worker that owns the code. It becomes outdated when its line or step is gone.
_Avoid_: Review note, annotation, feedback

**Story**:
A walk through the Merged changes of a task, written by an agent: flows of steps that together cover every changed line.
_Avoid_: Walkthrough, tour, narrative

**Flow**:
One path through the code in a story, from one entry point such as a request, a job or a command.
_Avoid_: Scenario, path

**Step**:
One part of a flow: a few lines of code in one lane, with what they do and the tests that cover them.
_Avoid_: Stage, node, item

**Lane**:
One part of the system that the steps of a story run in, such as a web app, a queue worker or a storage service.
_Avoid_: Column, track, swimlane

**Finding**:
A possible problem in a task's changes that the story writer reports and the user raises or dismisses.
_Avoid_: Issue, warning, flag

**Story writer**:
The worker that writes a task's story after the other workers are done.
_Avoid_: Narrator, explainer

**Local checks**:
The project's own commands (tests, linters) that must pass on a task's branch before Swarm opens its pull request.
_Avoid_: CI, pre-flight, tests

**Close task**:
Ending a task: its agents stop and its worktrees are removed. Its branch stays unless it is already merged into the base branch.
_Avoid_: Delete task, archive, kill

**Remove project**:
Taking a project out of Swarm: its open tasks close, its agents and terminals stop, and Swarm forgets the project. The code in the project folder stays on disk.
_Avoid_: Delete project, close project

**Task worktree**:
The worktree that holds a task branch; each tab in the window shows one.
_Avoid_: Space, workspace

**Terminal**:
One running program shown in Swarm: the orchestrator, a worker, a process (dev server, tests) or the user's own shell.
_Avoid_: Pane, shell

**Tile**:
A terminal's place in the workspace layout: its position, size and frame. The layout engine arranges tiles; tiles that don't fit go to the tray.
_Avoid_: Pane, window, panel

**Grid cell**:
The 8 px unit that every tile's place and size is made of. Tiles move and resize in whole cells only.
_Avoid_: Snap unit, step, pixel grid

**Process terminal**:
A terminal running a command an agent started, such as a dev server or a test run.
_Avoid_: Job, task terminal, background terminal

**User terminal**:
A terminal the user opened that no agent can see or touch.
_Avoid_: God mode terminal, private terminal

**Tray**:
The strip of chips in the status bar where tiles go when they don't fit on screen. Chips that don't fit the bar sit behind a "+N" chip that opens a menu.
_Avoid_: Dock, minimized bar

**Notice**:
A moment that needs the user: a decision, a blocked agent, work ready for review or a failure. The app may show it as a system notification.
_Avoid_: Alert, toast

**Daemon**:
The background process that keeps agents running when the app is closed.
_Avoid_: Server, service, backend

**Usage window**:
A span of time in which a subscription allows a limited amount of use. Swarm shows how much of it is used and when it resets. Its length gives its name: Session, Weekly or Monthly.
_Avoid_: Quota, rate limit, allowance

**Session**:
The usage window shorter than a day, for example the 5-hour window of Claude Code. It is not a Task and not an Agent.
_Avoid_: 5h window, block

**Weekly**:
The usage window that lasts seven days.
_Avoid_: 7d window, week limit

**Monthly**:
The usage window that lasts thirty days.
_Avoid_: 30d window, month limit

**Pace**:
Where even use would be by now in a usage window: the share of the window that has passed. The usage bar marks it. A window warns when use at its average rate would reach 100% before the window resets.
_Avoid_: Burn rate, trend
