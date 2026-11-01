# Phase 8 part 4: VoiceOver, keyboard and performance audit

Status: parked on 2026-11-01. The audit and the performance run are done. No fix is made yet.

## How to pick this up

Work in two rounds, so the app fixes can use the new `swarm_ui` pieces.

1. **Round 1, in parallel:**
   - `swarm_ui` foundation: accessibility flags (disabled, busy, live, modal, read-only, hidden) through a pass-through wrapper element, a live-region component, disabled controls that keep focus, `Label::accessible`, headings, popover menu and menu keys, a one-stop tab bar, a roving list helper, reveal-on-focus scrolling, a shadow token, one list of project tile colors, the text field caret, and the Tab-walk test support.
   - Three app agents, each in its own files, fix what does not need those pieces: the main window and terminals, Review with the Swarm view and the right panels, and the overlays with Settings, the menu bar and first launch.
2. **Round 2:** the same three app agents use the new pieces: live regions, readable text, the dimmed state, list arrow keys, reveal-on-focus, and a Tab-walk test for each window.

Decisions made so far:

- In a terminal, Tab and Shift-Tab go to the shell. ⌃Tab and ⌃⇧Tab move focus out of the terminal, as in macOS text views. Write a failing test first, because the audit says Tab never reaches the shell today.
- Toggle Sidebar gets ⌃⌘S, the macOS shortcut for showing and hiding a sidebar.
- Settings › Keyboard lists both new shortcuts.

## Performance baseline

The run used a release build with the `frame-stats` feature (`crates/swarm_app/README.md`) on 2026-11-01. The build used sample data and a throwaway home folder.

| Measure | Frames | p50 | p90 | p99 | Max | Over 8 ms |
|---|---|---|---|---|---|---|
| Draw | 18 843 | 1.75 ms | 2.78 ms | 4.67 ms | 11.90 ms | 3 |
| Dirty to present | 18 839 | 2.18 ms | 8.12 ms | 33.44 ms | 37.98 ms | 1 920 |

- Draw time meets the 8 ms budget. The 3 slow frames came from the "select project", "select task" and "steady redraw" steps. Find out whether text shaping on the first draw causes them.
- Dirty to present is mostly the wait for the display refresh. Window resize and terminal resize are the slowest steps, with 164 and 184 frames over 33 ms. Check them on a 120 Hz display before you change code.
- A `frame-stats` build prints 73 dead-code warnings, because that feature replaces the normal start-up. Silence them for that feature only.

## Audit findings

The paths are relative to `crates/`. The line numbers are from 2026-11-01 and will move.
GPUI paths are relative to `~/.cargo/git/checkouts/zed-a70e2ad075855582/70b1b91/crates/gpui/`.

### Cross-cutting

**H1. Status and Alert nodes are never spoken.** accesskit_macos announces a node only when it has a value and a `live` setting; Status/Alert roles do not set `live`. Affected: `swarm_ui/src/components/toast.rs:163`, `new_task.rs:290`, `close_task.rs:174/180/207`, `remove_project.rs:273/281/310`, `sidebar.rs:65/77/104/574`, `pull_request_panel.rs:140/443`, `review_view.rs:1264/1633`, `review_view/comments.rs:1490/1576`, `settings/pages.rs:124/336/457/657/683/726`, `settings/prompts_page.rs:512`, `menu_bar/popover.rs:585`. Fix: one `swarm_ui` live-region helper (value + live), used everywhere.

**H2. Focus stays on a handle that is not drawn, so the keyboard dies.** (a) Every `swarm_ui` control stops being focusable when disabled (`button.rs:347`, `icon_button.rs:161`, `switch.rs:99`, `checkbox.rs:133`, `dropdown.rs:143`, `text_input.rs:857`) while its handle stays alive. Hits: Run checks (`settings/pages.rs:650`, `pull_request_panel.rs:365`), Request review/Ready for review/Merge (`pull_request_panel.rs:735/804/815`), Close task and Remove project confirm (`close_task.rs:220/227`, `remove_project.rs:324/333`), New task (`new_task.rs:307`), Approve/Request changes (`review_view.rs:203/1253`), reply field and Send (`review_view/comments.rs:1088-1090/1429/1439/1596`), zoom buttons (`swarm_view.rs:1238/1246`), fields turning read-only (`settings/project.rs:1403/1413/1461`). (b) `select_task`, `focus_terminal`, `focus_agent` set a pending focus and `apply_pending_focus` (`workspace.rs:615`) never checks the view: in Swarm and Review, Enter on a tab/project/agent row, a tray chip, ⌘1–⌘9 and palette commands focus an undrawn tile. Effect: no ring, VoiceOver reads the window, Tab/Shift-Tab/Esc dead (GPUI dispatches from node 0, `src/window.rs:6556`, above SwarmRoot and Dialog contexts). Close task / Remove project after a failed re-check stay stuck. `restore_focus_when_lost` (`workspace.rs:359-368`) only acts when nothing has focus; Settings and the menu bar popover have no fallback. Fix: disabled controls stay focusable with `tab_stop(false)`, no click handler, flagged disabled; use `.loading()` for buttons that start work; switch to Terminals before focusing a tile; fallback uses `focus_lost_restore_target` and also exists in Settings and the popover.

**H3. Plain text is not in the tree.** `Label` renders a bare string (`swarm_ui/src/components/label.rs:100`); Markdown uses `StyledText` (`swarm_app/src/markdown.rs:452`). Worst: comment bodies (`review_view/comments.rs:1352-1361`), Swarm view messages read as empty list items (`swarm_view.rs:2191-2193`), Settings › Keyboard rows only say "Command K" (`settings/keyboard_page.rs:157`), "permissions skipped" (`status_bar.rs:128`), PR number/title/state (`pull_request_panel.rs:555-586`). Fix: GPUI's `Text` element (`src/elements/text.rs:189-199`) already has `Role::Label` with the text as value; add opt-in `Label::accessible(id)`; labels inside buttons stay hidden.

**H4. Tab never scrolls the focused control into view.** GPUI `focus_next`/`focus_prev` do not scroll (`src/window.rs:2508-2527`). Settings pages taller than 900 px (`settings.rs:53`, `:895-919`), Swarm graph (`swarm_view.rs:1297-1302`), message thread (`swarm_view.rs:968-1000`). Fix: a scroll container that reveals the focused child (like `swarm_ui/src/components/tab_bar.rs:160-223`).

**M5. Focus falls to the window root (no role, no ring)** (`workspace.rs:733`): closing any overlay via `close_overlay` (`palette.rs:737-741`), toast closing (`eggs.rs:346-353/488/492`), Skip for now on welcome, Toggle Sidebar (`workspace.rs:520`), closing a background tab, removing a focused project row, a tile that goes away (`workspace.rs:555`, `views.rs:314-346/391-417`, `daemon.rs:427`), Delete/Resolve/Fold on a comment (`review_view/comments.rs:1205-1211/1300-1347`), removing a Review filter chip (`review_view.rs:966-970`), removing a Settings row (`settings/project.rs:1361-1379`), Reload (`settings/pages.rs:743`). Fix: focus the opener again, else the next control, never the root.

**M6. Lists are one Tab stop per item with no arrow keys:** task tabs and close buttons (`swarm_ui/src/components/tab_bar.rs:153`), sidebar lists without a List parent (`sidebar.rs:31-58/235/489`), Settings page list (`settings.rs:562`, `settings/project.rs:1033`), Files panel (`panels.rs:479-604`). Fix: one Tab stop per list, arrow keys (like TabStrip and the Changes tree).

**M7. Dialogs are not modal for VoiceOver** (`swarm_ui/src/components/dialog.rs:95`). Fix: modal flag.

**M8. Markdown links are mouse only** (`markdown.rs:467-476`). Fix: focusable `Role::Link`, Enter and Space open it.

**M9. Text fields expose no caret or selection** (`swarm_ui/src/components/text_input.rs:813-824`). Fix: text-run children and a text selection in `FieldText`.

### swarm_ui components

- M: open `PopoverMenu` does not hold Tab (`popover_menu.rs:160-180`); Tab jumps out, menu stays open, Esc lost. Fix: swallow Tab like `context_menu.rs`, or close on focus out.
- L: popover trigger has no expanded state; closing always refocuses the trigger (`popover_menu.rs:88-99`), copy ContextMenu's `contains_focused` check; menus: Space does not pick, Home/End do nothing (`swarm_ui.rs:66-69`); loading button does not say busy (`button.rs:350`); `Kbd` sets label but no value (`kbd.rs:58`); dialog "Close" button sits next to "Close task".
- Raw colors: shadows use `black()` in `menu.rs:399`, `dialog.rs:110`, `toast.rs:188`, `tooltip.rs:43`, app `palette.rs:627`; add a shadow token. Project tile hex values (`swarm_app/src/model.rs:674`) are copied into the contrast test (`project_tile.rs:76`); keep one list.

### Sidebar, tab bar, status bar

- M: arrowing onto "Review" in the View control moves focus into the diff (`tab_bar.rs:140-153`, `workspace.rs:514`). Process rows always say "running" (`sidebar.rs:423/436`).
- L: Changes toggle does not read its counts (`tab_bar.rs:90`); PR toggle reads "#42 · open" (`pull_request.rs:271`); All tasks menu drops each task's status (`swarm_ui/src/components/tab_bar.rs:408`); error Dismiss and Retry send focus to a container with no ring (`sidebar.rs:88/119`); Toggle Sidebar has no shortcut and no button to bring the sidebar back; the tray is a List of Buttons (`status_bar.rs:218-236`); "Projects" and "Agents" headings not in the tree.

### Terminals view

- H: the tile ring does not follow keyboard focus (`views.rs:664/757/791`); set `focused_terminal` when a terminal gets focus. Tab never reaches the shell: the terminal has no key context (`swarm_term_view.rs:677`), so the SwarmRoot Tab binding wins. Fix: a Terminal key context that sends Tab and Shift-Tab to the shell, plus ⌃Tab / ⌃⇧Tab to move focus out (the macOS convention for text views), listed in Settings › Keyboard. Confirm the bug with a failing test first.
- M: process tiles always say "Working", even after a failure (`views.rs:714-718/755-757`); user tile label does not say "disconnected" (`views.rs:815`); user terminal resize handle has no value and no keys (`views.rs:859-897`), use `ResizeHandle`.
- L: labels repeat the same name for every user terminal (`views.rs:838/866`); the right panel has no landmark.

### Swarm view

- M: inspector Changes list (up to 500 rows) cannot be scrolled by keyboard (`swarm_view.rs:1782-1797`); switching views by shortcut loses focus (`workspace.rs:507-517`).
- L: selected card is a Button with selected state, which macOS does not report on buttons (`swarm_view.rs:1506-1508`); "Focus", "View diff", "Show more" do not name their target (`swarm_view.rs:1640-1652/2211`); message rows inside `list()` risk duplicate ids when drawn again.

### Right panels

- H: two sibling nodes share id "pull-request-status" (`pull_request_panel.rs:135`, used at `:864` and `:867`); with VoiceOver on a debug build panics, release drops the second. Fix: unique ids.
- M: PR busy states and results not announced: "Merging…" (`:809`), Merged/Closed/Draft tags (`:555-557`), errors shown as a Log not an Alert (`:495/604/824`). Files panel breaks the tree pattern; M and A marks are letter and color only; "Filter files" looks like a field but is not one (`panels.rs:479-604`).
- L: trees have no Home and End (`file_tree.rs:316-320`).

### Review

- M: diff body shows no focus when Review opens; cursor ring stays after focus leaves (`review_view.rs:1363-1371/1839-1846/1957-1960`). "Loading changes" and "No changes" are silent and the focus target is not drawn (`review_view.rs:827-834/862-864/1346-1354`). Sending a reply says nothing (`review_view/comments.rs:1088-1090`). Split view has no key to change side (`review_view/comments.rs:675-682`). Header status and totals are plain text (`review_view.rs:1190-1197/1310-1335`). File header label leaves out Added/Deleted, the agent and the comment count (`review_view.rs:1491-1503/1617-1650`).
- L: note rows and hunk headers not in the tree; split rows away from the cursor lose the change kind (`review_view.rs:2021-2031`); each line has two gutter buttons with the same label (`review_view.rs:1862/1888`); diff rows are list items under a Group; "Request changes · 2" reads the dot; progress bar has no value (`review_view.rs:922`); after j or k, Down jumps back to the old file (`review_view.rs:748-766`), treat as a bug; the naming test only checks bounds (`review_view.rs:3746`).

### Overlays and first launch

- H: in the ⌘K palette VoiceOver never reads the highlighted result (`palette.rs:540`): GPUI honors `aria_active_descendant` only under a focused ancestor, and the focused input is a sibling of the results list. Fix: render the results inside the focused combobox element.
- M: busy states not announced: "Creating task" (`new_task.rs:283`), Close task (no busy text), "Stopping agents" (`menu_bar/popover.rs:442/449`), system check "checking" and header count (`system_check.rs:367/400/1132`). Failed Stop all and system check errors are not alerts (`menu_bar/popover.rs:441/448`, `system_check.rs:785`). Palette rows leave out status, changes and shortcut (`palette.rs:532`).
- L: nothing announced when "Checking" turns into the result; palette has no "No results" status; toast is last in Tab order and lasts 20 s (`eggs.rs:27/407-414`); popover window has no title (`menu_bar.rs:259-270`); "Enter Full Screen" never changes to "Exit Full Screen" (`app_menus.rs:72`); the "Create anyway" dialog (`pull_request.rs:558`) likely has the H2/M5 focus problems.

### Settings window

- H: H2, H3 and H4 above (Keyboard rows, Run checks now, scrolling).
- M: both agent cards use the same field names "Command", "Orchestrator model", "Worker model" (`settings.rs:522-533`); opening Settings at a page leaves focus on the Agents row (`settings.rs:1261`), call `focus_nav`; read-only values not in the tree (`settings/general_page.rs:48-75`, `settings/pages.rs:268/444/667-712`); ⌘W only works in an inner context (`settings.rs:858/982`), bind it on the root.
- L: project notification picker label is only the project name (`settings/pages.rs:404`); prompt layer tags do not name the prompt (`settings/prompts_page.rs:464-478`); page titles are not headings (`settings/pages.rs:92`); links use the Button role; ⌘⇧T and ⌘1–⌘9 do nothing from Settings (`app_menus.rs:139-151`).

### Settings › Keyboard vs the keymap

Bound but not listed: ⇧F10 context menu (`swarm_ui.rs:68`), ⌘↵ post a comment or reply (`text_input.rs:109`), ⌘W close Settings (`settings.rs:982`), ↑↓ between Settings pages (`settings.rs:983-984`), arrows and ↵ on Swarm view cards (`swarm_view.rs:478-486/1530`), ↑↓ to resize the tray or a tile (`views.rs:888-890`). Review ↑↓ is listed but only works while the diff has focus.

### GPUI facts

- No `aria_disabled` builder (`src/elements/div.rs:1357-1550`), and the writer never sets it (`div.rs:3591-3663`). Workaround: `a11y_synthetic_children(|b| b.parent_node().set_disabled())` reaches the element's own node (`src/element.rs:475-499`); accesskit_macos then reports it not enabled ("dimmed"). The same hook gives `set_live`, `set_modal`, `set_busy`, `set_hidden`, `set_read_only`, `set_required`. Tests cannot call that hook, so build a small pass-through wrapper element in `swarm_ui` that adds the flags in `write_a11y_info`, so tests can read them.
- Plain text: GPUI `Text` (`text!` or `Text::new(id, text)`) has `Role::Label` with the text as value.
- Still missing in GPUI: builders for live/disabled/modal/busy, and an active descendant outside the focused element.

### Tab-walk test design

Public APIs: `window.focused(cx)`, `FocusHandle::dispatch_action` (`src/window.rs:637-645`), `root.contains_focused(window, cx)`.

1. `swarm_ui` feature `test-support = ["gpui/test-support"]`, enabled in `swarm_app` dev-dependencies.
2. Test-only action `NameStop` and global `LastStop(Option<String>)`.
3. One helper `accessible(element, role, label)` that sets role and label and, in test builds, adds `on_action(NameStop)` writing `"{role:?} {label}"` to `LastStop`. Every component and app-level focusable uses it.
4. Helpers: `focused_stop(cx, root) -> String` ("nothing", "not drawn", "unnamed" or the name) and `tab_walk(cx, root, key)` that presses the key until focus is back at the start, failing if focus did not move or after 200 steps.
5. Per window: exact Tab order, Shift-Tab is the reverse, no "unnamed"/"nothing"/"not drawn", the walk stays inside an open dialog, after an overlay closes focus is on the opener.
Limit: an unnamed focusable inside a named one reports the parent's name.
