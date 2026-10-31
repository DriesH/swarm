# One task branch, one worktree per worker

Each task gets a task branch. Each worker gets its own worktree on a branch made from the task branch. When a worker is done, the orchestrator merges its branch into the task branch and handles conflicts. Review shows the task branch against the base branch, grouped by the worker that wrote each change. Workers sharing one checkout was rejected because parallel edits collide.

Superseded in part by [ADR 0009](0009-workers-share-the-task-worktree.md): workers share the task worktree by default.
