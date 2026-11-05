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
- Code has syntax colors for Rust, TypeScript, TSX, JavaScript, PHP, Python, Go, JSON, TOML, YAML, Markdown, CSS, HTML and shell. Swarm colors the whole file, not only the changed lines, so a string or comment that starts above a hunk is colored right. A card shows plain text first and gets its colors a moment later; the text does not move. Swarm reads only the files you can see, at most four at a time. A file you scroll past stops loading, and loads again when you scroll back. Folded files, files over 1 MB or 20,000 lines, and other file types stay plain. The colors meet WCAG AA contrast on added and deleted lines too.
- The header of the file you read stays at the top of the diff until the next file's header pushes it out. **Viewed**, the fold button and the other controls keep working there.
- Tick **Viewed** on a card to fold it and count it as read. The mark and the fold reset when that file's diff changes. Swarm stores both per task, so they survive a restart.
- Keys: `j` and `k` go to the next and previous file, `v` toggles **Viewed**. ↑ and ↓ move the line cursor in the diff.

## Comment on the code

A comment is a note on one line. It goes to the task's orchestrator, which passes it to the worker that owns the file, or handles it when no worker owns the file.

1. Hover a line and click **+** in its gutter. Or move the line cursor with ↑ and ↓ and press `c`.
2. Type the comment. It can hold up to 16 KiB.
3. Press ⌘↩ or click the button. Esc cancels.

What happens next depends on the source.

- On **Live**, the button is **Comment**. The comment goes at once.
- On **Merged**, the button is **Add comment**. The comment waits as **Pending**. **Request changes · N** sends all pending comments together.

When the task's orchestrator is resumable, sending a comment [resumes it](/swarm/docs/how-work-starts/#idle-stop-and-resume) first. Swarm refuses to send when the orchestrator cannot resume, or when resuming it would go over an [agent limit](/swarm/docs/how-work-starts/#agent-limits). The error says why.

A thread shows its state: **Pending**, **In flight** (the orchestrator has not received it), **Delivered** with its time, or **Resolved**.

- **Reply** adds to the thread. A reply goes at once, unless the thread is still pending. Then the button is **Add reply**.
- **Resolve** marks the thread done and folds it. **Reopen** brings it back.
- **Delete** removes a pending comment.

A comment keeps the text of its line and the lines around it. When the file changes, Swarm finds the line again. When the text is gone, the thread gets an **Outdated** tag at the top of its file card.

## Approve

**Approve** works only on **Merged**. It stays off while a comment is pending, and Swarm refuses it too.

Swarm stores a fingerprint of the Merged diff when you approve. **Approve** also marks every file of that diff as viewed and folds it.

If the diff changes, for example after a new commit, the approval no longer counts and **Approve** shows again. Swarm notices within a few seconds, also when Review is not open. Then:

- The **Pull request** button leaves the title bar. If the panel is open, it says **Changed since approval** and has a **Review** button. **Create PR** is gone.
- The pull request draft is cleared.
- The task's orchestrator gets an `approval reset` message. On your next approval it writes a new draft from the whole diff.
- Files that did not change stay viewed and folded. A file that changed is open and unviewed. A new file is open.

A pull request needs an approval that still counts.

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

Before it deletes anything, the daemon checks again. If closing would now lose more than the dialog showed, it refuses and the cleanup screen shows the reason. Choose **Retry**: the **Close** dialog opens again with a new check, so you see the new list before you confirm. A close that a crash interrupted finishes at the next daemon start.

### The cleanup screen

The moment a close starts, the dialog goes and the whole surface of the task, below the tab bar, becomes the cleanup screen. This holds for **Close task** and for a merge with **Close the task after merge**. The screen has the honeycomb, the pixel bee and the title **Cleaning up the swarm**. The bee flaps and drifts. With **Reduce motion** on in macOS it stays still.

Under the title, the steps show as the daemon reports them. Each has a mark: a check mark when done, a spinner while it runs, a ring when next.

- **Stopping agents**
- **Removing worktrees**
- **Deleting the merged branch**, only when a merged branch goes. It shows when the daemon starts that step

The task's tab shows a spinner, and the tab bar stays usable: you can open another task meanwhile. When the close ends, the tab goes and focus moves to the tab next to it, or to the Start tab.

Keyboard focus moves to the screen. VoiceOver reads each step once as it starts. If the connection to the daemon is lost while a close runs, the screen reads **Cleanup stopped** with `Not connected to the daemon`.

When the close fails, the screen reads **Cleanup stopped** and shows the reason. Three buttons follow:

- **Retry** sends the same close again. A close that began with a merge has no check you confirmed, so Swarm checks first. If closing would delete uncommitted files or leave workers unmerged, the **Close** dialog opens and nothing is sent until you confirm
- **Back to the task** leaves the screen. The task stays open
- **Report an issue** opens the [report dialog](/swarm/docs/troubleshooting/#report-an-issue) with the reason filled in

## Remove a project

Remove a project to take it out of Swarm. Right-click its row in the sidebar and choose **Remove project…**, or use the button of the same name on the project's settings page.

The dialog, titled **Remove project** and the project name in quotes, says Swarm removes only its own tasks, agents and worktrees, and that the code in the project folder stays on disk. It lists what happens and leaves out each line that is zero:

- **Closes** the open tasks
- **Stops** agents and terminals
- **Deletes** uncommitted files, by task
- **Deletes** merged branches

If the check fails, the dialog shows why and **Remove** stays off. While Swarm removes the project, the button reads **Removing…** and Cancel and Esc do nothing.

Swarm closes each open task as Close task does, stops the Start orchestrator, and forgets the project. The daemon checks again first. It refuses when a task would lose more than the dialog showed, or when a task opened since.
