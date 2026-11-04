---
title: Review
description: Read a task's diff, comment on it, approve it, and close the task or remove the project.
---

Review is where you read a task's combined diff, send comments to the swarm and approve the result. You must approve a task before you can open a pull request.

## Open Review

Any of these opens Review:

- **Review** in the view switch, or ⇧⌘R
- The **Review** button in the **Changes** panel
- A file in the **Changes** panel (opens Review at that file)
- **View diff** on an agent in the [Swarm view](/swarm/docs/swarm-view/) (filtered to that agent)
- A "ready for review" notification
- **Review changes** in the command palette

The sidebar and tabs stay. The left side lists the changed files as a folder tree, with the number of files and how many you marked as viewed. The right side has a header, then one card per file.

The header shows:

- how many agents work, or **All agents done**
- where the diff comes from, and the total additions and deletions
- **Live | Merged**
- **Unified | Split**
- **Request changes** (on Merged only) and **Approve**

## Live and Merged

Review reads the diff from one of two sources.

| Source | Shows | Use it to |
|---|---|---|
| **Live** | Work in progress: the task worktree, committed or not, and each worker's own worktree that is not merged yet. It updates as agents edit. | Watch work and send quick comments |
| **Merged** | Only the commits on the task branch since it left the base branch. This is what the pull request will hold. Uncommitted work does not show. | Approve |

The first time Review shows a task, it picks **Live** while a worker works, and **Merged** otherwise. Swarm saves that source for the task. When you pick another source, the new one stays.

## Who changed a file

Workers in the shared task worktree commit their own files with `swarm commit`. That command records the worker as the commit author and adds a `Swarm-Agent` trailer. Review reads the trailer, so each file card names the agent of the newest such commit that touched the file. Review sorts the cards by worktree, then by path.

- A worker with its own worktree owns every file in it.
- Uncommitted work in the shared worktree, and commits made without `swarm commit`, show no worker.

**View diff** in the Swarm view filters Review to one agent. A chip at the top of the file tree names the agent. Click it to remove the filter.

## Read the diff

- A card shows the file, **Added** or **Deleted** when that applies, additions and deletions, the worker and the open comments.
- Lock files, generated files (marked `linguist-generated` in `.gitattributes`, minified files, source maps) and files with more than 500 changed lines come folded. The card shows a tag, **Lock file**, **Generated** or **Large diff**, and **Show diff**, which loads that one file. Totals and approval still count every line.
- Tick **Viewed** on a card to fold it and count it as read. The mark resets when that file's diff changes.
- Keys: `j` and `k` go to the next and previous file, `v` toggles **Viewed**. ↑ and ↓ move the line cursor in the diff.

## Comment on the code

A comment is a note on one line. It goes to the task's orchestrator, which passes it to the worker that owns the file, or handles it when no worker owns the file.

1. Hover a line and click **+** in its gutter. Or move the line cursor with ↑ and ↓ and press `c`.
2. Type the comment. It can hold up to 16 KiB.
3. Press ⌘↩ or click the button. Esc cancels.

What happens next depends on the source.

- On **Live**, the button is **Comment**. The comment goes at once.
- On **Merged**, the button is **Add comment**. The comment waits as **Pending**. **Request changes · N** sends all pending comments together.

Swarm refuses to send while the task's orchestrator is stopped.

A thread shows its state: **Pending**, **In flight** (the orchestrator has not received it), **Delivered** with its time, or **Resolved**.

- **Reply** adds to the thread. A reply goes at once, unless the thread is still pending. Then the button is **Add reply**.
- **Resolve** marks the thread done and folds it. **Reopen** brings it back.
- **Delete** removes a pending comment.

A comment keeps the text of its line and the lines around it. When the file changes, Swarm finds the line again. When the text is gone, the thread gets an **Outdated** tag at the top of its file card.

## Approve

**Approve** works only on **Merged**. It stays off while a comment is pending, and Swarm refuses it too.

Swarm stores a fingerprint of the Merged diff when you approve. If the diff changes, for example after a new commit, the approval no longer counts and **Approve** shows again. A pull request needs an approval that still counts.

After you approve, the button changes with the GitHub status. See [Pull requests](/swarm/docs/pull-requests/).

- It reads **Create PR** when GitHub is ready.
- It reads **Set up GitHub** when `gh` is missing or not signed in.
- It reads **Approved**, with a check mark, when the project has no GitHub repository or the GitHub status is not known yet.

## Close a task

Close a task when it is done or you drop it. Use the close button on its tab, **File › Close Task**, or **Close task** in the palette. The Start and Plan tabs cannot close.

The dialog, titled **Close** and the task name, says **Checking** while it asks the daemon, then lists what closing does:

- **Stops** the running agents and your user terminals
- **Deletes** the uncommitted files, in the task worktree and in workers' own worktrees
- **Deletes merged branch** … for the task branch and worker branches that are merged into the base branch
- **Keeps branch** … when the task branch is not merged
- **Not merged:** … the workers with their own worktree whose commits are not in the task branch

Choose **Close task** to go on. Swarm then stops the agents and all terminals, runs the project's teardown script if it has one, and removes the task worktree and the workers' own worktrees. Uncommitted work is lost. Every branch that is not merged into the base branch stays. A branch that GitHub merged by squash does not count as merged, so a normal **Close task** keeps it. Only **Close the task after merge** in the [pull request panel](/swarm/docs/pull-requests/#merge) deletes that branch.

Before it deletes anything, the daemon checks again. If closing would now lose more than the dialog showed, it refuses and the dialog checks again. A close that a crash interrupted finishes at the next daemon start.

## Remove a project

Remove a project to take it out of Swarm. Right-click its row in the sidebar and choose **Remove project…**, or use the button of the same name on the project's settings page.

The dialog, titled **Remove project** and the project name in quotes, says Swarm removes only its own tasks, agents and worktrees, and that the code in the project folder stays on disk. It lists what happens and leaves out each line that is zero:

- **Closes** the open tasks
- **Stops** agents and terminals
- **Deletes** uncommitted files, by task
- **Deletes** merged branches

If the check fails, the dialog shows why and **Remove** stays off. While Swarm removes the project, the button reads **Removing…** and Cancel and Esc do nothing.

Swarm closes each open task as Close task does, stops the Start orchestrator, and forgets the project. The daemon checks again first. It refuses when a task would lose more than the dialog showed, or when a task opened since.
