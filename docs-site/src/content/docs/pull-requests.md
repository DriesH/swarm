---
title: Pull requests
description: Open a pull request from an approved task, follow its status, merge it, and set up GitHub.
---

Swarm opens, tracks and merges a task's pull request through the GitHub CLI, `gh`. You can create a pull request only after you [approve](/swarm/docs/review/#approve) the task, so you read the code first.

## Create a pull request

1. In Review, switch to **Merged** and click **Approve**.
2. Click **Create PR**. The same panel opens from the **Pull request** button in the title bar, or from **Create pull request** in the command palette.
3. Check the **Title** and **Description**. The orchestrator drafts both.
4. Run the **Local checks** if the project has any.
5. Click **Create PR**, or **Create draft PR**.

Swarm pushes the task branch first, exactly the commit you approved. The repository's pre-push hook runs as usual. Then it runs `gh pr create`. If `gh` or the push fails, the panel shows the output. An open pull request from the branch that Swarm did not record is taken over, and a closed one can be replaced.

The panel also has two options.

- **Open as draft**: on by default.
- **Close the task after merge**: off by default.

### What keeps Create PR off

The footer names the reason in a short line.

| Line | Meaning |
|---|---|
| **Not approved** | The task is not approved, or its diff changed since you approved it. The panel then says **Changed since approval** with a **Review** button |
| **Comments pending** | A [review comment](/swarm/docs/review/#comment-on-the-code) is pending |
| **GitHub not set up** | `gh` is missing or not signed in. See [Set up GitHub](#set-up-github) |
| **No title** | The title is empty |
| **Checks not run**, **Checks running**, **Checks failed**, **Checks stopped**, **Checks stale**, **Uncommitted changes** | See Local checks below |

Swarm checks the same rules again before it pushes.

### The draft

When you open the panel for an approved task, Swarm asks the orchestrator for a draft. The orchestrator writes a title and a description and sends them back with `swarm pr draft`. The note **Drafting…** shows while it works, and **Drafted by orchestrator** after.

**Redraft** next to **Drafted by orchestrator** asks for a new draft of the current diff. The button reads **Drafting…** while the orchestrator writes. It shows only while the approval still counts. If you edited the title or the description, the new draft waits behind **New draft** instead of replacing your text.

### Read the description large

The description field is small. The expand button at its top right, or ⇧⌘D, opens the description as a large tile over the app. The tile grows out of the field in 200 ms and shrinks back into it in 150 ms. With **Reduce motion** on in macOS, it shows and goes without movement.

The tile has:

- the pull request icon, **Description**, and **Drafted by orchestrator** when the orchestrator wrote it
- the tabs **Preview** and **Edit**. **Preview** shows the title and the description as formatted Markdown. **Edit** shows the raw text. What you type there is the text of the panel field, so nothing is lost when you close the tile
- **Redraft**, which works as in the panel. A new draft that arrives while the tile is open updates it, unless you edited the fields
- a word count at the bottom and the esc **Close** hint

Esc, the close button, or a click outside the tile closes it. Focus goes back to where it was. VoiceOver reads the tile as a dialog named **Pull request description**.

The description follows a built-in format with four sections: **What**, **Why**, **Notes for reviewers** and **Open tasks**. A project can replace the format with a `pr.md` file in `.swarm/prompts/`. The Prompts page in Settings lists it.

If a draft arrives after you edited the fields, your text stays and **New draft** appears. Click it to use the new draft. Without a running orchestrator, the panel says why there is no draft, and you write the fields yourself.

### Local checks

A project can list commands that must pass before a pull request. They live in `.swarm/config.toml`, in the `before_pr` list under `[checks]`. See [Project config](/swarm/docs/project-config/). The **Local checks** section shows only when the project has checks.

- **Run checks** (or **Run again**) runs the commands one after the other. Each runs in its own terminal in the task worktree, so you can read a failure. After a failure, Swarm skips the rest. **Show output** on a failed check opens its terminal.
- A result belongs to the commit it ran on. A new commit makes it **Stale**. A run also counts only if the worktree had no uncommitted changes the whole time, otherwise the tag **Uncommitted changes** shows. The daemon forgets runs when it restarts.
- With `required = true` under `[checks]`, **Create PR** stays off until every check passes.
- Without it, a failed or missing run only asks first. The button reads **Create PR anyway**, and a dialog **Create PR anyway?** confirms it.

## After the pull request exists

The **Pull request** button in the title bar shows the pull request number and its state, for example **#42 · checks running**. The states are checks running, checks failed, draft, ready to merge, open, merged and closed. The icon and the number in the panel header use GitHub's state colors: green for open, purple for merged, red for closed and grey for draft. The panel shows:

- **Open on GitHub**, the title, and a **Draft**, **Merged** or **Closed** tag
- **GitHub checks**, read through `gh`, with a count such as `3 / 5`
- **Reviewers**, with each state (**Requested**, **Approved**, **Changes requested**, **Commented**). Type GitHub logins, separated by commas or spaces, and click **Request review**.
- **After merge**: **Close the task after merge** and **Merge method**

Swarm reads the status with `gh pr view`: every 10 seconds while GitHub checks run, every 30 seconds otherwise, and every 60 seconds while the panel is hidden.

### Push more commits

New commits after the pull request exists, for example a CI fix, show **New commits**. They go through [Review](/swarm/docs/review/) and the local checks again, under the same rules. Then **Push changes** sends them to the same pull request. It reads **Push anyway** when the checks did not pass and the project does not require them.

### Merge

- **Ready for review** takes a draft out of draft.
- **Merge** merges only the commit Swarm pushed. It stays off while a GitHub check runs or failed, while GitHub says it cannot merge or has not worked it out yet, and while **New commits** wait to be pushed. A line above it gives the reason: **Checks running**, **Checks failed**, **Conflicts**, **Review required**, **Behind base** or **Blocked**. Swarm turns **Merge** on only when GitHub reports the pull request as clean, with hooks, or with a failing check that is not required, and no check Swarm can see runs or failed. The daemon also refuses a merge while a check runs or failed. When the daemon cannot read the checks before the merge, it asks GitHub to merge anyway, and GitHub decides. When the pull request can merge, **Merge** is GitHub's merge green.
- **Merge method** is **Squash** by default. **Merge** and **Rebase** are the other choices.

Swarm checks that GitHub really merged the commit. A merge queue can leave the pull request open, and the panel then shows **Merge queued**. After a merge, Swarm deletes the remote branch.

With **Close the task after merge**, Swarm also closes the task as [Close task](/swarm/docs/review/#close-a-task) does, and then deletes the local task branch. The task's tab then shows the [cleanup screen](/swarm/docs/review/#the-cleanup-screen) until the task is gone. A plain **Close task** keeps a branch that GitHub merged by squash, because git cannot tell it is merged. Here Swarm knows the pull request holds every commit, so it deletes the branch. The task stays open when closing would lose work: uncommitted files, or local commits that are not in the pull request.

If someone closes the pull request without merging, the panel offers **Create PR** to replace it.

## Set up GitHub

Swarm needs `gh`, signed in to github.com, and an `origin` remote on GitHub. The panel says what is missing:

- **No GitHub repository**: `origin` is not on GitHub. There is nothing to set up here.
- **GitHub CLI not installed**
- **Not signed in to GitHub**

For the last two, **Create PR** in Review becomes **Set up GitHub**. The same button sits in the panel. It runs in a visible user terminal:

1. If `gh` is missing, Swarm installs it. With Homebrew, it runs `brew install gh`. Without Homebrew, it downloads the official release and checks it against the published checksums.
2. Swarm runs `gh auth login --web`, which signs you in in your browser.

The panel shows **Installing GitHub CLI…** and **Signing in…**. The terminal closes when a step succeeds. When a step fails, it stays so you can read it, the panel says **Install failed** or **Sign-in failed** with the reason, and **Try again** runs the step again.
