---
title: The Swarm view
description: See a task's agents and their messages as a graph, and inspect one agent.
---

The Swarm view shows one task's agents as a graph, with the messages between them. Pick **Swarm** in the view switch to open it. The **Files**, **Changes** and **Pull request** buttons are off in this view.

## The graph

The orchestrator is a card at the top. Its workers are cards in a row below, each joined to the orchestrator by an edge.

A card shows:

- the agent's kind icon, its name and a status dot
- for the orchestrator: its model and running time
- for a worker: its running time and its additions and deletions
- a short note: what the agent reported, or its status

Click a card to select it. With a card focused, use ← and → to move between workers, ↑ to go to the orchestrator and ↓ to go to the first worker. Press ↩ to select the card and jump to its terminal. The **−** and **+** buttons at the top right of the graph zoom out and in.

### Edges and dots

Messages go between the orchestrator and a worker. Workers never message each other.

- An edge takes its color from the newest message between the two agents. A task message or a brief uses the accent color, a question is amber and dashed, and a report is green. An edge with no messages is plain.
- A label in the middle of the edge names the kind of that message.
- A dot on an edge is a message that is in flight: sent, but not yet delivered. An edge shows up to four dots.

## The inspector

The inspector shows the selected agent. It sits on the right. Drag its left border to resize it between 320 and 640 px. Double-click the border to reset the width. With the border focused, ← and → also resize it.

The header has:

- the kind icon, the name and the status
- **View diff**: opens [Review](/swarm/docs/review/) filtered to this agent
- **Focus**: opens the Terminals view with this agent's tile focused, restored from the tray if needed

Under the header, one line gives the agent's kind and model, its running time, and **Task worktree** or **Own worktree**. The next line gives its branch.

Three tabs follow. The window remembers the tab you picked, not one tab for each agent.

### Messages

The agent's thread: every message it sent or received, oldest first. Each message shows who sent it, who received it, a tag for its kind, and the time. A message that was not delivered yet says **In flight**. The text is Markdown. A long message folds, and **Show more** opens it.

Kinds are brief, task, question, report, comment, pr draft, terminal exited, user input and worker stopped.

### Changes

The agent's changes: total additions and deletions, then one row per file. Click a row, or press ↩ on it, to open that file in Review on the **Live** source, filtered to this agent. When the list is longer than the rows shown, a line says how many files are left out.

### Terminal

The agent's own terminal, live. It scrolls and takes typing like its tile does. For a worker, typing works only while **Allow typing in worker terminals** is on.

Under it, **Process terminals** lists the process terminals this agent started, each with its size and **Running** or **Exited** and the code. Click one to open the Terminals view with its tile focused.
