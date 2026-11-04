---
title: Your first project
description: Add a git repository, write a plan and send it.
---

This page takes you from an empty window to a first plan sent to Swarm.

## Add a project

A project is a git repository opened in Swarm.

1. Click **Add project**, or click the **+** next to **Projects** in the sidebar, or choose **File › Add Project…**.
2. Pick the repository folder.

Swarm adds the project to the sidebar and opens it.

## Write the plan

A project with no tasks opens on the **Start** tab. Before you send a plan, the tab shows the plan editor and no agent runs.

The editor starts from a template with three headings:

- **What**: the feature you want.
- **Requirements**: what it must do.
- **Definition of done**: how you know it is finished.

Write the plan under these headings. Swarm keeps your draft, so you can quit the app and come back to it.

## Send the plan

1. Pick the **Orchestrator** agent: Claude Code or Codex.
2. Click **Send plan**, or press ⌘↩.

**Send plan** stays off while the plan is empty or still the bare template.

## What happens next

Swarm starts the Start orchestrator in your project folder, with your plan as its first message. The tab now shows its terminal. Answer its questions there.

The Start orchestrator turns the plan into tasks and hands each one to its own orchestrator. Each task gets its own tab. See [How work starts](/swarm/docs/how-work-starts/).

The **Start** tab has no close button, and **Close task** skips it. The Start orchestrator exits after every task orchestrator has confirmed its brief. Then the tab closes. It stays, with the plan editor, if it holds your user terminals or the project has no other tab.

To start more work later, use the **+** in the tab bar. See [How work starts](/swarm/docs/how-work-starts/#start-work-later).
