# Plan grilling and review stories

## 1. Status and how to pick it up

Agreed on 2026-11-18, for after v1: phase 11 in `docs/ROADMAP.md`. Nothing is built. This doc is the spec. ADR 0013 records why stories are data that the daemon checks.

Line numbers drift. Search by name.

### Read first

- `CLAUDE.md`, `GLOSSARY.md` (Plan, Start orchestrator, Review, Live, Merged, Comment, Approve), `docs/CONVENTIONS.md`.
- ADRs 0002 (messages go through the orchestrator), 0004 (append prompts, never replace), 0005 (daemon owns SQLite state), 0013 (review stories are data that the daemon checks).
- `docs/PLAN.md`: "How work starts", "Prompts", "Review".
- Prompts: `crates/swarm_core/prompts/start.md` and `plan.md`.
- Planning: `crates/swarm_app/src/plan_editor.rs`, `plan_draft` in `crates/swarm_core/src/store.rs`.
- Review data: `TaskDiff`, `FileDiff`, `DiffHunk`, `CommentAnchor` and `ReviewSource` in `crates/swarm_protocol/src/swarm_protocol.rs`; the `review_marks` table in `store.rs`.
- Review UI: `crates/swarm_app/src/review_view.rs`, `review_model.rs`, `review_view/highlight.rs`, `markdown.rs`.
- Harness: `crates/swarm_cli/src/mcp.rs`; `pr_draft` in `crates/swarm_daemon/src/calls.rs` (a call the daemon asks the orchestrator for); the "ready for review" notice in `crates/swarm_daemon/src/agents.rs`.

### Order of work

Each slice ships on its own.

1. **Plan grilling** (section 5). Small: a prompt, one harness call and a split Start tab.
2. **Story data and check** (6.1–6.4). The schema, `story post` and its check, the story writer prompt and the trigger. No UI; test with the CLI.
3. **Story view** (6.5–6.6). **Story | Files** on Merged, flows, the lane rail, steps, code, step checks and keys.
4. **Comments and findings** (6.7–6.8).
5. **Out of date and refresh** (6.9).

## 2. What the user asked for

> "We start with the planning phase like we have today, but instead of having the user write a full markdown file themselves, we make it guided. We grill the idea with mattpococks grilling skills, but tailored to swarm and grab the plan."

> "There is a system that agents can use in the harness that will allow them to group their changes as if it is a "story", so a user can walk through that "story" change per change."

> "The user can still comment on stuff and it will still be send to the agents."

The reference is a review page that a colleague's agent made for one pull request. It has flows by entry point. Each flow is a list of steps along lanes (the parts of the system, such as the client, the web app and storage), and each step has its code, the tests that cover it and a reviewed check. The screenshots are not in the repo.

The key idea: agents do not make the UI. They write the story as data, and Swarm draws it with one fixed, native layout.

## 3. What exists today

- The Start tab opens on the plan editor with the `plan.md` template (What, Requirements, Definition of done). The user writes the plan. **Send plan** starts the Start orchestrator with the plan as its first message. The daemon keeps the draft (`projects.plan_draft`).
- Review shows the task diff by file, from Live or Merged. **Viewed** is per file and kept in `review_marks` with the file's digest, so it resets when the file's diff changes.
- A comment sits on one line, found by its text (`CommentAnchor`). It goes to the orchestrator. On Merged it waits as Pending until **Request changes**.
- Approve works on Merged and holds while the Merged fingerprint stays the same.
- The daemon sends "ready for review" when the orchestrator rests and every worker is done.

## 4. Decisions

| # | Choice | Decision |
|---|---|---|
| 1 | Where grilling happens | In the agent's terminal, with the plan editor next to it |
| 2 | Skip grilling | Possible: **Grill me** and **Send plan** both stay |
| 3 | When grilling ends | The agent writes the plan when it has no questions left; **Write plan** makes it write and stop |
| 4 | When a story is written | Automatically when every worker is done; a setting turns it off |
| 5 | What a step points to | Line ranges at one commit; the symbol is only a label |
| 6 | Coverage | Every changed line is in a step, or in **Other changes** with a reason; else the post fails |
| 7 | One change in more flows | Allowed; the reviewed state of that code is shared |
| 8 | Findings | Yes: the story writer also reports possible problems |
| 9 | Refresh | Only the steps whose code changed are rewritten, at the next "ready for review"; no **Refresh story** button |
| 10 | Story or diff | A **Story \| Files** switch on Merged, opening on Story when one exists |
| 11 | Comments without a line | Step comments, sent like line comments |
| 12 | Step checks and Approve | Step checks show progress only, like Viewed |

Also agreed: no story on Live, and a fresh worker writes the story, not the orchestrator.

## 5. Plan grilling

### Flow

1. The Start tab (or a Plan tab) opens on the plan editor, as today.
2. The user types an idea, as short as one line, picks the agent and presses **Grill me**. **Send plan** (⌘↩) still sends the text as a finished plan.
3. Swarm starts a **grill agent** in the project root, on the base branch, with the `grill.md` prompt and the user's text as its first message. The main area splits: the grill agent's terminal on the left, the plan editor on the right.
4. The grill agent asks its questions in its terminal, one at a time.
5. It writes the plan with `swarm plan write <file>` (MCP `plan_write`). The call replaces the plan draft and the editor shows it at once. The agent may call it more than once while it learns.
6. **Write plan** in the editor header sends the grill agent a message: write the plan now, then end.
7. After the last write, the grill agent runs `swarm exit`. Its terminal closes and the plan editor takes the main area again.
8. The user edits the plan and presses **Send plan**. A fresh Start orchestrator gets only the plan, not the grilling. This tests that the plan stands on its own.

### Rules

- While the grill agent runs, the plan editor is read-only and **Send plan** is off, so the agent and the user never write over each other.
- Only the grill agent can call `plan write`, and only for its own project. The text is at most 64 KiB.
- The grill agent lives like the Start orchestrator: the daemon keeps it running when the app quits. Closing the tab stops it and keeps the draft.

### The `grill.md` prompt

A new prompt file with the usual layers (built in → `~/.config/swarm/prompts/` → `.swarm/prompts/`). We write our own text in the spirit of Matt Pocock's grilling skill. Check that skill's license before reusing any of its wording.

- Ask one question at a time, with a recommended answer for each.
- Settle first the choices that other choices depend on.
- Read the code to answer what the code can answer.
- Find the features: one change a user can see or use, built and reviewed on its own, as in `start.md`. Each becomes a task. Ask for each feature's definition of done and the checks that must pass.
- Write the plan in the `plan.md` shape, with one part per feature when there is more than one.
- Never write code and never create tasks.

### Harness

| Call | Who | What |
|---|---|---|
| `plan.write {text}` | grill agent | Replaces the project's plan draft. The app shows it at once. |
| `agent.exit` | grill agent | Ends its own session (exists). |

## 6. Review stories

### 6.1 The story

A story is JSON that an agent posts for the Merged diff of a task at one commit. A shortened example:

```json
{
  "title": "Review comments",
  "overview": "Markdown: what the change does as a whole.",
  "lanes": [
    { "id": "app", "label": "App" },
    { "id": "daemon", "label": "Daemon" },
    { "id": "store", "label": "Store" },
    { "id": "orchestrator", "label": "Orchestrator", "outside": true },
    { "id": "worker", "label": "Worker", "outside": true }
  ],
  "flows": [
    {
      "id": "comment-on-line",
      "title": "The user comments on a line",
      "summary": "The comment box opens under the line. …",
      "steps": [
        {
          "id": "keep-anchor",
          "title": "Keep the line's text and its neighbors as the anchor",
          "lane": "app",
          "depth": 1,
          "symbols": ["review_model::anchor_for", "CommentAnchor"],
          "what": "- The anchor keeps the lines before and after, so a moved line can be found again.",
          "code": [
            { "path": "crates/swarm_app/src/review_model.rs", "lines": [826, 838] }
          ],
          "tests": [
            {
              "name": "an anchor holds the line and its neighbors on its side",
              "path": "crates/swarm_app/src/review_model.rs",
              "line": 2624
            }
          ]
        }
      ]
    }
  ],
  "other": [{ "path": "Cargo.lock", "reason": "Lock file" }],
  "findings": [
    {
      "id": "same-text-lines",
      "title": "…",
      "body": "Markdown",
      "step": "keep-anchor",
      "code": [{ "path": "…", "lines": [831, 836] }]
    }
  ]
}
```

- `lanes`: the parts of the system that the code runs in, in the order they show. `outside` marks a part outside the project; its steps draw as a hollow dot. At most 8 lanes.
- `flows`: one per entry point, such as a request, a job, a command or a user action.
- `steps`: in call order. `depth` is the call depth and sets the indent. `what` is Markdown.
- `code`: a path in the repository and an inclusive line range on the new side. A ref without `lines` is the whole file. Deleted, binary and mode-only files need one.
- `tests`: optional.
- `other`: changes that belong to no flow, each with a short reason.
- `findings`: possible problems. `step` and `code` are optional.
- Ids are unique within their list.
- `swarm story schema` prints the JSON Schema, so the prompt stays short.

### 6.2 `story post` and the check

`swarm story post <file>` (MCP `story_post`) can be called by any agent of the task. The daemon checks the story against the full Merged diff at the current HEAD of the task worktree, without Review's caps. A story that passes is stored with that commit. A story that fails is not stored, and the call returns one line per problem so the agent can fix it and post again:

```
uncovered: crates/swarm_daemon/src/comments.rs 41-46
unknown lane: deamon (step keep-anchor)
outside the file: crates/swarm_app/src/review_model.rs line 9000 (step keep-anchor)
```

The check:

- The JSON matches the schema, ids are unique and every lane exists.
- Every path and line range exists in the file at HEAD, also those of tests.
- **Coverage**: every changed line is covered.
  - An added line is covered when a `code` range holds its new line number.
  - A deleted line is covered when a range holds the new line just after it, or the line just before it at the end of the file.
  - A whole-file ref covers every change in that file.
  - A file in `other` is covered as a whole. Lock and generated files are expected there.

Limits to start with: 1 MiB per story, 30 flows, 50 steps per flow, 8 lanes.

### 6.3 The story writer

- **Trigger**: when the daemon would send "ready for review", the Merged diff has changes and there is no current story, the daemon first sends the orchestrator a `story` message. The orchestrator spawns a fresh worker with the `story.md` prompt: the **story writer**. It reads the Merged diff with git, posts the story and reports done.
- The **ready for review** notice waits until the story is posted, or until the story writer stops without one.
- **Setting**: Settings › General › **Write review stories**, on by default. When it is off, Review shows Files only.
- The story writer gets the diff, the task's brief and the commit messages. A fresh context describes what the code does, not what the workers meant to do.

`story.md` is a new prompt file with the usual layers. It tells the story writer to:

- Make one flow per entry point. Follow the calls and make one step per change that a reader takes in at once.
- Write titles and `what` in plain words: what happens and why it matters, not the code again.
- Name the tests that cover each step.
- Put every change in a step or in `other`, and run `swarm story post` until it passes.
- Report findings (bugs, missing tests, risky choices) and fix none of them.

### 6.4 Storage

- `stories`: task, commit, the Merged fingerprint it was checked against, the JSON, the agent and the time. One current story per task; a new post replaces it.
- `story_marks`: task, step id, a digest of the step's code at its commit, reviewed. It works like `review_marks`.

### 6.5 Story view

On **Merged**, the Review header gets a **Story | Files** switch. It opens on Story when a current story exists. Live has no switch and stays the file diff.

- **Left column** (272 px, like the file tree): the story title, the commit, a progress bar (reviewed steps of all steps), then:
  - **Overview**: the `overview` Markdown.
  - **Findings**, with "N to decide".
  - **Flows**: one row per flow, with its title and "reviewed / steps".
  - **Other changes**, and **Not in story** when the story is out of date.
- **Main area** for the chosen flow: the title, the summary, the lane legend, then the steps.
- **Step row**: number, title, symbols in mono, a **changed** or **unchanged** pill and the reviewed check, with the lane rail on its left. A step is unchanged when its ranges hold no changed line; it is there for context.
- **Open step**: What happens (Markdown), Code, Tests, comments and **Mark as reviewed**.
- **Code block**: a header with the symbol and `path:line`, then the diff rows that Review already uses, with syntax colors. Unchanged lines inside the range come from the file text. Changed lines get the changed bar. Code already reviewed in another step folds to one row, **Reviewed in <flow> #n**, with **Show**.
- **Lane legend**: horizontal chips with a color dot and the label. No rotated labels.
- **Other changes** and **Not in story** show as the file cards of Files, with the reason as a tag.

**Lane rail**: each row paints its own part with `canvas` and `PathBuilder`: the line through the row in its lane's color, the dot, and an elbow at the top when the lane changes from the step before. Because each row paints its own part, the steps can be a virtualized `list` with rows of different heights. Lane colors come from 8 new theme tokens that meet contrast in both themes.

**Keys**, the same as in Files where they overlap: `j` / `k` next or previous step, `o` open or close the step, `v` reviewed, `[` / `]` previous or next flow, `c` line comment on the line cursor, `⇧C` step comment.

**Accessibility**:

- Each step row is a list item with a label such as "Step 2 of 7, Keep the line's text and its neighbors as the anchor, App, changed, not reviewed". The lane is in the label, so color is never the only sign.
- The rail is decoration and hidden from VoiceOver.
- Every check and button has a role and a label and works from the keyboard.

### 6.6 Reviewed

- **Mark as reviewed** sets the step's check and marks its code lines as reviewed.
- The same lines in another step fold as reviewed. That step's own check stays the user's choice.
- A step's check resets when its code changes.
- Step checks do not affect Approve.

### 6.7 Comments

- **Line comments**: a line in a step's code takes a comment the same way as in Files: a `CommentAnchor`, the same states, and Pending on Merged until **Request changes**. A line comment shows in both Story and Files.
- **Step comments**: **Comment** on an open step (`⇧C`). A new anchor kind holds the step id, its title and its code refs, so the orchestrator knows what the comment is about. It goes to the orchestrator and names the workers that own the step's files. It has the same states and the same Request changes batching.
- A comment on the story itself (no step) uses the same anchor kind without a step id.
- When a new story drops a step, that step's comments show as **Outdated** in the left column.

### 6.8 Findings

- **Findings** lists each finding with its title, body, code and step.
- **Raise it** creates a Pending step comment with the finding's text, on its step or, without a step, on the story. The user can edit it before sending.
- **Not an issue** dismisses the finding. Dismissed findings stay listed and folded, and can be raised later.
- "N to decide" counts the findings that are not raised or dismissed yet.

### 6.9 Out of date and refresh

- A story belongs to one commit. When the Merged diff changes, the daemon maps each range from that commit to the new HEAD with `git diff` between the two commits:
  - A range whose lines did not change moves to its new line numbers.
  - A step with a changed line in its ranges is **stale**. It gets a **Changed** tag and its check resets.
  - Changed lines that no range and no `other` entry covers go to **Not in story**.
- The Review header shows **Story out of date**.
- While the setting is on, the next "ready for review" sends the orchestrator a `story refresh` message with the stale step ids and the uncovered ranges. A fresh story writer gets the old story, rewrites only those parts and posts a full story. Steps with the same id and the same code keep their checks.
- There is no **Refresh story** button. Changes to Merged after a story almost always come from agents, and their work ends in a new "ready for review". A commit the user makes by hand shows as stale steps and **Not in story** until the next refresh, so nothing is hidden.
- Live does not touch the story. Mapping runs only when Merged changes.

## 7. Docs

- ADR 0013: review stories are data that the daemon checks.
- `GLOSSARY.md`: Grill agent, Story, Flow, Step, Lane, Finding and Story writer. Plan, Review and Comment are updated.
- `docs/PLAN.md`: "How work starts", "Prompts" and "Review".
- `docs/ROADMAP.md`: phase 11.
- Still to write: docs website pages on grilling and stories. UI copy stays labels only.

## 8. Open details

- Designs on the canvas (direction A, light and dark): the split Start tab, the Story view and the lane colors.
- A timeout for the ready-for-review wait when the story writer hangs. Start with 20 minutes.
- Very large diffs: the story writer may need more than one pass.
- Later: a story summary in the PR description, and lanes set in `.swarm/config.toml`.
