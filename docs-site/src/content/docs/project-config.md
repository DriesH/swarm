---
title: Project config
description: The .swarm/config.toml file of a repository, with every key, its default and its effect.
---

`.swarm/config.toml` in a repository sets how Swarm makes worktrees, which script sets them up and which checks must pass before a pull request. Edit it by hand, or in **Settings › Projects** (the entry of your project).

The file is optional. A missing file, a missing table or a missing key gives the default. Keys that Swarm does not know are ignored.

## Example

```toml
[worktrees]
base_branch = "main"
folder = ".worktrees"
ports = { range = "5170-5199", per_worktree = 3 }
copy = [".env", "CLAUDE.local.md"]
setup = """
cargo fetch
pnpm --dir docs install --frozen-lockfile
"""
teardown = "docker compose down"

[checks]
before_pr = ["cargo test --workspace", "cargo clippy -- -D warnings", "cargo fmt --check"]
required = true
```

## Keys

### `[worktrees]`

| Key | Type | Default | Effect |
|---|---|---|---|
| `base_branch` | string | The branch `origin/HEAD` points to, else the current branch, else `main` | The branch new tasks start from and pull requests target. A blank value counts as unset. |
| `folder` | string | `.worktrees` | The folder, relative to the repository root, that holds the task worktrees. It must stay inside the repository. Swarm adds it to `.git/info/exclude`. |
| `ports` | table | none | Reserves ports for each task. Needs both keys below. |
| `ports.range` | string | – | The ports to hand out, such as `"5170-5199"`. Both ends count. |
| `ports.per_worktree` | number | – | How many ports each task gets: from 1 to the size of the range. |
| `copy` | list of strings | `[]` | Files to copy from the repository root into each new worktree. |
| `setup` | string | empty | A shell script that runs in a new worktree. |
| `teardown` | string | empty | A shell script that runs when you close a task. |

Details:

- **Ports.** A new task gets the lowest free block of `per_worktree` ports. Its agents, its setup script, its teardown script and its checks see them as `SWARM_PORT` (the first port) and `SWARM_PORTS` (all, separated by commas). `SWARM_TASK` holds the task ID. If no free block is left, Swarm refuses to make the task. A worker with its own worktree reserves its own block too, and Swarm refuses to start it when none is free.
- **Copy.** Each entry is a path inside the repository, such as `.env`. Entries that are not files, or do not exist, are skipped. A path with `..` or an absolute path is skipped too, and Settings refuses to save it.
- **Setup.** Runs with `/bin/sh -lc` in the worktree, in a terminal you can watch. The task's agents start when it succeeds. If it fails, the agent shows **Setup failed**. For a new task, the New task dialog has a **Run setup script** box, on by default. A worker with its own worktree runs the script too.
- **Teardown.** Runs with `sh -c` in the task worktree when you close the task, before Swarm removes the worktrees. It stops after 60 seconds. Its output is dropped. A failure does not stop the close.

### `[checks]`

| Key | Type | Default | Effect |
|---|---|---|---|
| `before_pr` | list of strings | `[]` | The [local checks](/swarm/docs/pull-requests/): commands that must pass on the task branch before Swarm opens its pull request. |
| `required` | boolean | `false` | When `true`, a pull request can only be created after every check passes. When `false`, a failure warns you and **Create PR anyway** asks for confirmation. |

Details:

- Each command runs with `/bin/sh -lc` in the task worktree, in its own terminal, one after the other.
- The run stops at the first failure. The later checks are skipped.
- Blank entries are dropped. With no checks, the section is hidden and **Create PR** goes straight through.
- The orchestrator runs the same checks before it reports a task done.

## Edit in Settings

**Settings › Projects ›** your project edits the same file.

| Section | Field | Key |
|---|---|---|
| **Worktrees** | **Base branch** (**Default branch** when unset) | `base_branch` |
| | **Worktree folder** | `folder` |
| | **Port range** | `ports.range` |
| | **Ports per worktree** | `ports.per_worktree` |
| **Setup script** | text field | `setup` |
| **Copy into worktrees** | one row per file, **Add file** | `copy` |
| **Checks before PR** | one row per command, **Add check** | `before_pr` |
| | **Required to pass** | `required` |

There is no field for `teardown`. Edit it in the file. Swarm keeps it.

A save writes only the fields you changed. Comments, key order, unknown keys and your hand edits to other fields stay. Swarm reads the file again when the Settings window gets focus, unless an edit waits to be saved.

A file that does not parse shows its error with **Reload**. A `folder` outside the repository, or a `ports` value that does not fit its rules, makes the file invalid too. Swarm uses the defaults and never overwrites that file. Fix it by hand, then select **Reload**.

**Run checks now** runs the checks on the open task of the project. **Remove project…** takes the project out of Swarm. The code stays on disk.

The page also lists the project's own prompts from `.swarm/prompts/`, each with **Open**. See [Settings](/swarm/docs/settings/#prompts).
