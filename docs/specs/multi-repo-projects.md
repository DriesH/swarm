# Multi-repo projects

## 1. Status and how to pick it up

Parked on 2026-11-04, after v1. Nothing is built. This doc is the spec.

Line numbers drift. Search by name.

### Read first

- `CLAUDE.md`, `GLOSSARY.md` (Project, Task, Worktree, Orchestrator, Worker, Brief), `docs/CONVENTIONS.md`.
- ADRs 0002 (messages go through the orchestrator), 0003 (task branch with worker worktrees), 0005 (daemon owns SQLite state), 0009 (workers share the task worktree).
- `docs/PLAN.md`: "How work starts", "Review", "Pull requests", "Project config".
- `crates/swarm_core/src/store.rs` (the `projects` and `tasks` tables, `NewTask`), `crates/swarm_core/src/git.rs`, `crates/swarm_core/src/config.rs`, `crates/swarm_core/src/adapters.rs` (how agents start), `crates/swarm_daemon/src/tasks.rs`, `crates/swarm_core/src/github.rs`.

### Order of work

Each slice ships on its own.

1. **Related repositories** (section 4). A project lists other repositories that its orchestrators can read. Small, and gives most of the value.
2. **Multi-repo tasks** (section 5). A task has a worktree in each repository of the project. About a phase of work.

Slice 2 needs an ADR, because it changes what a project and a task are.

## 2. What the user asked for

> "I am usually working on Flare and flare clients. Those are usually in different projects. I hate it that I have to spawn 2 different projects in muxy and two agents. It would be so much cooler if I can create a "project" and add different folders under that project. So I would have the flare project and all the clients under it. That way the orchestrator can more clearly get an overview over Flare and over the client SDKs and make better briefs for workers."

The goal is the orchestrator's overview: one agent that sees the server and its client SDKs together, and writes briefs that cover both sides of a change.

## 3. What exists today

- A **project** is one git repository. The `projects` table holds one `path` (unique) and a `name`.
- A **task** belongs to one project and has one `branch`, `base`, `worktree` and `ports` (`tasks` table, `NewTask`).
- Workers share the task worktree, or get their own worktree of the same repository (ADR 0009).
- `.swarm/config.toml` lives in the repository and holds setup, files to copy, ports and checks.
- Review shows one diff: the task branch against its base. A task opens one pull request.
- Agents start in the task worktree. The orchestrator sees only that repository.

## 4. Slice 1: related repositories

### Behavior

- A project gets a list of **related repositories**: other local git repositories, each with a path and a short name. They are not projects of their own in this list (a repository can still also be its own project).
- Settings › Projects shows the list with **Add** and **Remove**. Adding one opens a folder picker and checks that the folder is a git repository.
- The Start orchestrator and every task orchestrator of the project start with access to each related repository, on the branch that is checked out there. They read code, docs, `CLAUDE.md` and `AGENTS.md` there to write better briefs.
- The orchestrator's prompt names the related repositories, their paths and that they are for reading only. A brief can tell a worker to read a file there.
- Workers get the same access, so a brief can point them to code in a related repository.
- Nothing in a related repository changes: no worktree, no branch, no commits. Changes in a client SDK still need a task in that SDK's own project.

### How agents get access

- Claude Code: `--add-dir <path>` for each related repository. Its CLI reference says this grants access "to read and edit files", and that Claude Code does not load most `.claude/` configuration from such a directory. Read-only must come from Swarm: a permission deny rule for edits under those paths in the settings Swarm passes, plus the prompt.
- Codex: check the official Codex docs for `--add-dir` or `writable_roots`, and for a read-only option, before building. Third-party write-ups say `--add-dir` makes a directory writable in `workspace-write` sessions. If Codex has no read-only way, the prompt rule is the only guard; say so in the docs website.
- Store the list in the daemon (a `project_repositories` table), not in `.swarm/config.toml`, because paths are local to one Mac.

### Done when

- [ ] A project lists related repositories in Settings › Projects, and the list survives a restart
- [ ] Orchestrators and workers of the project can read files in each related repository
- [ ] An attempt by a Claude Code agent to edit a file in a related repository is denied
- [ ] The docs website explains related repositories

## 5. Slice 2: multi-repo tasks

### Model

- A **project** becomes a named group of one or more repositories. A project with one repository works as today. The glossary entry changes from "a git repository opened in Swarm" to "a named group of git repositories opened in Swarm".
- A **task** has one branch name and one worktree per repository of the project. The worktrees sit side by side in a task folder: `<task folder>/<repository name>/`.
- The **orchestrator** runs in the task folder, so it sees every repository of the task.
- A **worker** works in one repository by default. `swarm spawn` gets `--repo <name>`; without it the worker gets the orchestrator's choice in the brief, or the first repository. A worker with its own worktree gets it in that repository.
- `.swarm/config.toml` stays per repository. Setup, teardown, files to copy and ports run per repository. Checks before PR run per repository.

### Review and pull requests

- Review groups the diff by repository, then by worker. **Approve** covers every repository of the task.
- **Create PR** opens one pull request per repository that has changes, with the same title and a description that links the other pull requests. Each shows its own checks. Merge is per pull request. The task counts as merged when all are merged.
- **Close task** removes every worktree and deletes each branch that is merged, per repository, after one dialog that lists what is lost per repository.

### Open questions

- Can one repository be in two projects at once? (Probably yes; tasks of both create separate worktrees.)
- Does `swarm commit` need `--repo`, or does it follow the caller's working directory?
- How do process terminals choose their repository (`--cwd` already exists)?
- Does the base branch differ per repository (`main` in one, `develop` in another)? The new task sheet then needs a base per repository.

### Done when

- [ ] An ADR records the new project and task model
- [ ] A project can hold several repositories; a one-repository project behaves as before
- [ ] A task creates a worktree and branch in each repository, and runs each repository's setup
- [ ] The orchestrator sees all repositories; workers work in the repository their brief names
- [ ] Review groups by repository; Approve covers all; Create PR opens one linked pull request per changed repository
- [ ] Close task cleans up every repository after one dialog
