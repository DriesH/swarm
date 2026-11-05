# Feedback: first real task (2026-11-05)

Collected while using Swarm 1.0.0 on a real project. Fix after the list is complete.

## 1. The orchestrator used Claude Code's own subagents instead of `swarm spawn`

- **Seen:** asked for a code review; the orchestrator launched 3 background agents with Claude Code's `Agent` tool (no tiles, no status, not in the Swarm view), then ran `swarm wait --timeout 590`, which only returns on Swarm messages, so it could sit idle up to ~10 minutes while its subagents were already done.
- **Cause:** nothing stops it. `orchestrator.md` never says not to use the built-in tool, and the settings Swarm passes allow the `swarm` commands but deny nothing.
- **Fix:**
  - deny `Agent` and `Workflow` in the `permissions` of the settings Swarm passes to Claude Code orchestrators
  - add to `orchestrator.md`: every helper agent is a worker started with `swarm spawn`, including read-only reviews and research; do not use the built-in subagent or workflow tools
  - check whether Codex has an equivalent tool for its orchestrators
  - workers keep their own subagents
  - docs: say so in the harness reference

## 2. ⌘← and ⌘→ do not jump to the start or end of the line in Claude Code's input

- **Seen:** in an agent terminal, ⌘← and ⌘→ do nothing in Claude Code's input field.
- **Expected:** like macOS terminals (Terminal, iTerm2, Ghostty), ⌘← goes to the start of the line and ⌘→ to the end.
- **Cause:** to check: how `swarm_term_view` encodes ⌘ + arrow keys (probably not sent at all, since ⌘ is kept for app shortcuts).
- **Fix:** to decide after checking what other terminals send (for example ⌃A / ⌃E, or Home / End).

## 3. The focus border of a tile sits inside the tile border: a double border

- **Seen:** a focused user terminal shows its dark focus border inset from the tile's own grey border, with a gap between them, so it reads as two borders.
- **Expected:** one border: the focus border replaces the tile border at the same place and radius.
- **Cause:** to check in `crates/swarm_app/src/views.rs` (tile frame, the focus overlay drawn "above the tile", PLAN "Terminal tiles").

## 4. The resize handle of a user terminal breaks out of the tile

- **Seen:** the drag line at the bottom of a user terminal runs past the tile's rounded corners, outside the container.
- **Expected:** the handle is the tile's bottom border itself: it follows the corner radius and only changes on hover or drag.
- **Cause:** to check: `resize_handle` in `crates/swarm_app/src/views.rs` is drawn at full width under the tile.

## 5. Tiles cannot be dragged on the grid

- **Seen:** a terminal tile cannot be dragged to another place.
- **Status:** not built. Drag, maximize and lock tiles on a grid are parked in `docs/specs/tile-controls.md` (ROADMAP "Parked"). Today you can only minimize a tile and resize a user terminal.
- **To decide:** whether this comes before the next release.

## 6. The user terminal's tile header has two icon and label pairs

- **Seen:** the header reads `[terminal icon] Terminal [user icon] Mine`. Two icon + label pairs next to each other look odd.
- **Expected:** one clear way to say "this is your terminal, agents cannot touch it", without a second icon and label.
- **Ideas:**
  - one combined icon (a terminal with a small person or lock badge) and only the name; "Mine" goes; VoiceOver keeps "Mine, agents have no access" in the label
  - the name with a small text badge (`Mine` as a quiet pill, no icon)
  - show it through the tile itself (for example a different header tint or border style for user terminals) and keep the header to icon + name
- **Also:** the sidebar row of a user terminal has the same pair (Terminal icon and a dimmed User icon); change both the same way. Design on the canvas first.

## 7. Idea: catch pixel bees and wasps while agents work

- **Idea:** a few times a day, while agents work, a pixel creature from the bee and wasp family appears somewhere in the UI. Click it to catch it. Caught creatures go into a collection you can open from the sidebar.
- **Sidebar:** a new region at the bottom of the sidebar with **Settings** and the collection.
- **Creatures:** 20 to start, in three tiers: common, rare, ultra rare. Each needs pixel art in the style of the pixel bee (`crates/swarm_sprite`, the canvas board "Pixel bee").
- **Name:** not "Pokédex" (a Nintendo trademark). Ideas: **Field guide**, **Hive log**, **Bee book**.
- **Rules to settle:** only while at least one agent works; a few per day at most; it never covers terminal text and never takes focus; it leaves after a while if not caught; reachable by keyboard and announced to VoiceOver; follows Reduce Motion and the **Easter eggs** switch; the collection is stored by the daemon or in settings (decide).
- **First list (to discuss):**
  - common (10): honey bee worker, drone, bumblebee, carpenter bee, mason bee, leafcutter bee, sweat bee, mining bee, yellowjacket, paper wasp
  - rare (6): blue-banded bee, green orchid bee, cuckoo wasp, mud dauber, European hornet, velvet ant
  - ultra rare (4): the queen bee (the crowned sprite), giant Asian hornet, tarantula hawk, Wallace's giant bee
- **Next:** design the sidebar region, the creature card and the collection on the canvas; draw the sprites.
- **Art style (update):** the creatures get more detail than the mascot bees, which stay as they are. The style is the GBA-era creature sprite: about 64 × 64 px, a small palette per sprite (about 16 colors), a dark outline that takes the color of the part it borders, two or three shades per surface lit from the top left, and a three-quarter view. Each creature looks close to its real species (body shape, banding, wing veins, colors), drawn as our own art: no characters, poses or palettes copied from an existing game.
- **Latin names:** each creature shows its English name and its Latin name (for example honey bee, *Apis mellifera*).

## 8. A task without a name gets a creature's Latin name

- **Idea:** when **New task** has no name, the placeholder (and the name used when the field stays empty) is a Latin name from the creature list, for example `apis-mellifera` or `vespa-crabro`. Branch: `swarm/apis-mellifera`.
- **To settle:** pick from all 20 or only the ones you caught; avoid a name that is already a branch or an open task (add a number, or pick another).

## 9. The orchestrator renames the task after its brief

- **Idea:** the task starts with a placeholder name (point 8). Once the orchestrator has read its brief, it gives the task a name that says what it is about (for example `clean-up-code`), and Swarm renames the task, its branch and its worktree to match.
- **Needs:** a new harness command, for example `swarm task rename <name>` (orchestrator only, once, right after `swarm confirm` and before it spawns any worker), plus a step in `orchestrator.md`. Swarm renames the branch (`git branch -m`), the tab and the task; the user sees the new name at once.
- **Decided:** rename only the branch, the task and the tab. The worktree folder keeps its placeholder name, because Claude Code and Codex store sessions by folder path and a moved folder could break resume.
- **Risks to check before building:**
  - a branch that was already pushed or has a pull request cannot be renamed this way
  - names must stay unique per project and valid as branch names

## 10. Review: the file header stays on top while you scroll its diff

- **Seen:** in Review, once you scroll into a long file, its header (path, `+53 −32`, the worker, **Viewed**) scrolls away, so you no longer see which file you are reading.
- **Expected:** the header of the file you are in sticks to the top of the diff area until the next file's header pushes it out, as on GitHub. Its controls (fold, **Viewed**, the worker) keep working while it sticks.
- **Cause:** to check: how `review_view` lays out the file cards (one scroll list of cards). Zed's multibuffer has sticky excerpt headers that may show the GPUI pattern (`docs/research/zed-architecture.md`).
- **Also seen in the same screenshot, to check:** a context line shows `+30` in the old line number column (probably the hover comment button drawn over the line number).

## 11. Review: syntax highlighting

- **Idea:** color the code in Review (and in the inspector's Changes tab) by language.
- **How (proposal):** tree-sitter, as Zed does: the `tree-sitter` and `tree-sitter-highlight` crates plus one grammar crate per language, each with its `highlights.scm` query. Grammars are C code linked into the binary, so the "system libraries only" rule still holds (check binary size and build time per grammar).
- **Performance rules:**
  - parse on the background executor, never on the UI thread
  - parse the whole old and new file once per revision, not the hunks alone (a hunk can start inside a string or comment), and cache the colored spans by file content hash
  - only files that are expanded and near the viewport; folded, generated, lock and large files (for example over 1 MB or 20 000 lines) stay plain
  - the diff draws plain at once and gets its colors when the parse is done, without a layout shift
- **Languages to start:** Rust, TypeScript and JavaScript (with TSX), PHP, Python, Go, JSON, TOML, YAML, Markdown, CSS, HTML, shell. The rest stays plain.
- **Theme:** a small set of syntax tokens (keyword, string, comment, function, type, number, constant) in both themes, each at WCAG AA contrast on the diff's add and delete backgrounds too.
- **To check first:** the Daemon sends hunks today; highlighting needs the full old and new file text (from git, by blob), so the protocol may need a request for it.

## 12. New work after Approve: keep what was reviewed, open what changed

- **Situation:** the task is approved and the PR form is filled (draft from the orchestrator, not created yet). Then the orchestrator keeps working on two more issues.
- **Wanted:** the approval is dropped, the PR draft is dropped, every file of the approved diff counts as viewed and is folded, and when the new work is done, Review opens exactly the files that changed since the approval.
- **What Swarm does today (from the code):**
  - the approval stores a fingerprint of the Merged diff; once the diff changes, the approval no longer counts and **Create PR** is blocked (`review_view.rs` `approve_review`, `model.rs` `approved`)
  - a **Viewed** mark stores the file's digest, so a file whose diff changed is no longer viewed (`review_view.rs` `is_viewed`)
  - but **Approve does not mark the files viewed**, a changed file **stays folded** (the fold set ignores the digest), the marks live **only in the app's memory** (lost on restart), and the PR form keeps the old draft
- **Proposal (automatic, no orchestrator command needed, because Swarm sees the diff change):**
  1. **Approve** marks every file of the approved diff as viewed at its digest and folds it
  2. when the Merged diff changes after an approval: the approval shows as outdated (`Changed since approval`), the PR form drops the draft (or marks it outdated and keeps your own edits; decide), and the orchestrator gets a message that the approval was reset
  3. a file whose digest changed is unviewed and unfolded; a new file is open; files that did not change stay viewed and folded
  4. viewed marks and folds are stored by the daemon per task, so they survive a restart
  5. on the next **Approve**, the orchestrator writes a new PR draft from the whole diff

## 13. Bug: after new work, the Pull request panel and button stay as if still approved

- **Seen:** the task was approved and the PR form was open. The orchestrator changed more files. Review showed the approval as stale and allowed a new Approve, but the **Pull request** panel and the **Pull request** button in the title bar stayed, with **Create draft PR** still clickable. So re-approving changes nothing you can see.
- **Cause (from the code):**
  - the app decides with `approval_holds` (`crates/swarm_app/src/model.rs ~1829`): it compares the approval with the **loaded** Merged diff, and when no Merged diff is loaded it says the approval holds (`is_none_or`). The Merged diff is only reloaded when Review shows it, so the panel and the button keep their old state
  - the daemon does check at creation (`check_approved_at`, `crates/swarm_core/src/swarm_core.rs ~835`): it refuses with "the changes changed since you approved them: approve them again". So **Create draft PR** should fail with that error, but only after you click it
- **Fix:**
  - the daemon tells the app when the Merged diff of an approved task no longer matches the approval (it already computes the fingerprint), so the app does not depend on a loaded diff
  - while the approval does not hold: hide the **Pull request** button, close the panel (or show it as `Changed since approval` with a **Review** button and no **Create PR**), and mark the draft as outdated (see point 12)
  - test: approve, change a file, and the button and panel go away without opening Review

## 14. Done workers stay until idle stop; the orchestrator cannot stop them

- **Seen:** three workers reported done and keep their tiles and their place in the Swarm view.
- **Today (from the code):**
  - a worker reports with `swarm report` and then waits; nothing ends it
  - idle stop ends it after **Stop idle agents after** (30 minutes by default); it becomes resumable and a message brings it back
  - the orchestrator has no command to stop a worker; `swarm exit` only ends the caller itself, and `worker.md` does not tell a worker to exit
- **Proposal:**
  - a new orchestrator command `swarm stop <worker>` (and MCP tool): ends that worker the way idle stop does, so it stays resumable and a later `swarm send` brings it back with its conversation
  - `orchestrator.md`: after you checked a worker's report and merged its work, and you have no follow-up for it, run `swarm stop <worker>`
  - a stopped worker leaves the tile layout (it frees its slot for working agents); the Swarm view keeps it, dimmed, with **Resume** in the inspector, so the history of the task stays visible
  - the tray or sidebar shows stopped workers as resumable, as today

## 15. ⌃C can end an agent; afterwards its tile looks like a normal terminal

- **Seen:** pressing ⌃C in an agent's terminal can quit Claude Code (in Claude Code the first ⌃C clears or interrupts, a second one quits; ⌃D on an empty prompt and `/exit` quit too). Afterwards the tile looked like a normal terminal.
- **To check first:** Swarm starts the agent CLI directly, without a shell (`crates/swarm_core/src/adapters.rs`), so after it quits there should be no shell to type into. Reproduce what the tile shows after the CLI quits: the last screen of Claude Code (with its "resume" hint), a live shell, or something else. A live shell would be a bug of its own.
- **Proposal:**
  - in agent terminals, Swarm passes the first ⌃C (Claude Code and Codex use it to interrupt or clear the input) but holds back a second ⌃C within 2 seconds, so a double ⌃C never quits the agent. Esc still interrupts.
  - the same for ⌃D: hold it back in agent terminals (it is never needed to work with an agent)
  - stopping an agent stays a Swarm action: **Stop** on the tile, the inspector, the palette, or `swarm stop` (point 14)
  - when an agent CLI quits anyway (`/exit`, a crash), the tile closes and the agent shows as resumable, never as a terminal you can type in
  - user and process terminals keep ⌃C and ⌃D as they are

## 16. Ask the orchestrator for a new PR draft

- **Today (from the code):** the app asks the orchestrator for a draft once, when the panel shows an approved task without a stored draft (`pull_request.rs` `request_draft_if_needed`). There is no button to ask again. Workaround: type in the orchestrator's terminal "write a new PR draft"; `swarm pr draft` works any time the task is approved, and the panel offers the newer draft (`newer`).
- **Proposal:** a **Redraft** button next to "Drafted by orchestrator". It sends the `pr draft` message again with the current diff. While it waits, the label reads `Drafting…`; when the new draft comes, it replaces the form, unless you edited title or description, then it asks first. With point 12, a draft that is older than the diff shows `Outdated` and **Redraft** is the main action.

## 17. Read and edit the PR description in a large view

- **Seen:** the description field in the PR panel is small (about 8 lines in a narrow column), too small to read or check a whole description.
- **Proposal:** clicking the description (or an expand button on it, plus a shortcut) opens it as a large tile over the app, animated from the field's place, like the orchestrator's main tile: the Markdown rendered for reading, with an **Edit** switch to the raw text, and the title on top. Esc or the close button animates it back. Follow Reduce Motion (no animation then), focus moves into it and back, and VoiceOver hears it as a dialog. Design on the canvas first.

## 18. Animate the in-app notifications (bottom right)

- **Seen:** the in-app notices at the bottom right appear and disappear without animation.
- **Wanted:** fast and snappy:
  - in: slide in from the right edge, with a short ease-out (about 150–200 ms)
  - out (closed or timed out): fade out and scale down a little (to about 95 %), about 120–150 ms
  - notices below or above move to their new place smoothly when one leaves
- **Rules:** with Reduce Motion on, they appear and leave without movement (a short fade at most). Use GPUI's animation pattern as Zed does (`docs/research/zed-architecture.md`), and find the notice component in `swarm_ui` or `swarm_app` first.
- **Done:** slide in and fade out; the scale-down needs a GPUI transform for elements, which the pinned GPUI does not have.

## 19. Pull request status: GitHub's colors, and no Merge while checks run

- **Seen:** with GitHub checks still running (0 / 6), **Merge** is enabled and drawn as a neutral button. The PR icon and number in the panel header are neutral too.
- **Wanted:**
  - **Merge** is off while any check runs or failed (or GitHub says the PR cannot be merged), with the reason as status text above it (`Checks running`, `Checks failed`, `Conflicts`)
  - when it can merge, **Merge** uses GitHub's green merge button color
  - the PR icon and number use GitHub's state colors: green for open, purple for merged (red for closed, grey for draft), so people know the state at a glance
- **Colors:** take them from GitHub's Primer tokens (open/success emphasis green, done/merged purple, closed/danger red), in a light and a dark value, and add them as theme tokens. Check white text on the green button meets WCAG AA in both themes; pick the darker step if it does not.
- **To check:** whether the daemon refuses a merge while checks run (`crates/swarm_daemon/src/pull_requests.rs`), or only GitHub does.
- **Found:** the panel enables **Merge** from GitHub's `mergeable` flag alone (`crates/swarm_app/src/pull_request_panel.rs ~870`). GitHub reports a PR as mergeable while checks still run when the branch has no required checks, so the panel must also look at the check states it already shows.

## 20. A cleanup screen while a task closes

- **Seen:** after **Merge** with **Close the task after merge**, the task's tab kept showing its normal view while Swarm stopped the agents and removed the worktrees in the background, until the tab disappeared and the Start tab showed. It looked as if nothing was happening.
- **Wanted:** the moment the close starts (after a merge with that option, and for **Close task** after its dialog), the task's whole surface (tiles, Swarm view or Review, below the tab bar) is replaced at once by a cleanup screen:
  - the honeycomb background (as on the website and the Swarm view)
  - the pixel bee mascot flying above the text (wing flap and a slow drift, like the website hero)
  - the label `Cleaning up the swarm`, and under it the current step as status text: `Stopping agents`, `Removing worktrees`, `Deleting the merged branch`
  - when it is done, the tab closes and focus moves as today
- **Rules:** keyboard focus moves to the screen and VoiceOver hears the label and each step once (live region); Reduce Motion shows a still bee; when the close fails, the screen shows the reason with **Retry** and **Back to the task**. The tab bar stays usable, so you can switch to another task meanwhile.
- **Needs:** the daemon reports close steps to the app (check what `close_task` sends today). Design the screen on the canvas first, light and dark.

## Decisions (2026-11-05)

- **5:** tile drag stays parked; it comes as its own phase after this round.
- **12/16:** when new work makes an approved draft outdated, the PR form is cleared; the orchestrator writes a fresh draft on the next Approve. (No "keep as outdated".)
- **8:** a task without a name gets a Latin name from all 20 creatures, skipping names in use.
- **7:** the collection is called **Field guide** and the daemon stores what was caught (SQLite).
- **6:** the user terminal shows the terminal icon with a small honey lock badge and a quiet **Mine** pill, in the tile header and the sidebar row (canvas board "Point 6").
- **17, 20:** the canvas designs are approved as drawn (boards "Point 17" and "Point 20").
- **Sidebar footer (new):** the sidebar bottom gets the usage card, a divider, then **Settings**, **Field guide** (with the caught count) and **Report an issue**; below about 700 px of window height the usage card folds to one line; the rail gets a usage ring and the three footer buttons (board "Sidebar: usage card and footer").
- **Report an issue (new):** a dialog (Bug, Feature request, Feedback) that opens a filled-in GitHub issue form in the browser; three issue forms in `.github/ISSUE_TEMPLATE/`; versions included, the daemon log optional via the clipboard; no token (board "Proposal: report an issue on GitHub").

## Plan

- **Round 1 (code, one branch `feedback-round-1`):** 1, 2, 3, 4, 8, 9, 10, 12, 13, 14, 15, 16, 18, 19.
- **Design first, on the canvas:** 6 (user terminal header), 17 (large description view), 20 (cleanup screen), 7 (sidebar region, creature card, Field guide).
- **Later:** 11 (syntax highlighting, own PR), 7 (creatures, after the designs and the sprites), 5 (tile controls phase).
