# Tile controls: move, minimize, maximize, lock

## 1. Status and how to pick it up

Parked on 2026-11-02. Nothing is built. This doc is the spec.

Line numbers come from the `phase-8` worktree on that day. They drift. Search by name.

### Read first

- `CLAUDE.md`, `GLOSSARY.md` (Tile, Tray, Terminal, User terminal), `docs/CONVENTIONS.md`.
- `docs/adr/0005-daemon-owns-sqlite-state.md` and `docs/adr/0006-user-terminals-are-off-limits-to-agents.md`.
- `crates/swarm_layout/README.md`, then `crates/swarm_layout/src/swarm_layout.rs` (765 lines) and `src/tests.rs`.
- `crates/swarm_app/src/views.rs`: `tile_spec`, `compute_layout`, `tile`, `agent_frame`, `user_frame`, `resize_handle`.
- `docs/audits/phase-8-part-4.md`, section "Terminals view". Three of its fixes come first: the tile ring follows keyboard focus (`focused_terminal` must be right, because every command acts on it), focus never falls to the window root (M5), and the live-region component (announcements).

### Order of work

Each slice ships on its own, with tests, snapshots and the accessibility checks.

0. **The tile grid.** `swarm_layout` works in whole grid cells (section 4.0), and the resize handle snaps to cells. Every later slice builds on cells.
1. **Minimize polish and maximize.** App only. No engine change.
2. **Tile order, drag, move commands.** One small engine change (respect the input order) and two pure helpers.
3. **Engine: locked tiles.** In `swarm_layout` only, with unit and property tests. The app passes no locked tile yet, so nothing changes on screen.
4. **Lock UI and commands.**
5. **Save tile state in the daemon.**

Slice 0 changes every placed size by a few pixels (the gap goes from 10 to 8, sizes round to cells). Update the 106 layout tests to cell values on purpose, in that slice only, and check that each test still proves the same rule. Slice 3 is the risky one. Do it behind its own switch (section 5.3), so the slice-0 tests stay the proof that nothing else changed.

## 2. What the user asked for

> "I want to be able to drag, minimize, maximize and lock tiles. Locked tiles should not be able to rescale and our rescaling engine needs to be smart enough to work around that and fill the remaining space in a smart way."

## 3. What exists today

### The layout engine

`crates/swarm_layout` is a pure function: `layout(area, tiles, style) -> Layout { placed, tray }` (`swarm_layout.rs:184`). No GPUI.

- **Input.** One `TileSpec` per tile (99-115): `kind` (orchestrator, worker, process, user), `activity` (attention, working, running, done, idle), `fixed` (the size of a user terminal; other kinds ignore it), `pinned`, `minimized`, `large`.
- **`pinned`** means "keep on screen if possible". The app never sets it. This spec replaces it with `locked`.
- **Style.** `LayoutStyle` (133-161): gap 10, padding 10, orchestrator share 0.65, min tile 260 x 160, min orchestrator 320 x 300, small box 286 high, process box 218 high. The app overrides the two heights from the terminal font (`views.rs:147`).
- **Priority** when space is short (`Rank`, 252-277), highest first: user terminals, orchestrator, pinned, attention, working, running processes, done, idle. The lowest leaves for the tray first.
- **Minimized** tiles always go to the tray (189-191).
- **Islands.** A user terminal with a `fixed` size is an island (318-320, 336-440). Islands pack from the top right, right to left, and start a new row below when a row is full. The layout never resizes them (ADR 0006).
- **Orchestrator.** A full-height column on the left: 65 % of the width, at least 320, and room for one 260 lane (`ColumnRange`, 444-498). It goes to the tray only when no room is left, for example in an area smaller than 320 x 300.
- **Other tiles** are boxes in lanes (`Flow`, 525-746):
  - A lane is a column at least 260 wide. The lanes of a region share its width equally (570-589).
  - There are two regions: beside the island block and below it (538-567).
  - Boxes stack from the top of a lane at a fixed height: small 286, process 218, tall (attention, or a worker with `large`) two small boxes plus the gap (312-315, 711-717).
  - Free space below the last box stays empty. Boxes are never stretched.
  - A full lane hands over to the next one. Only the first box of a lane may be cut to the lane, never below 160 high (721-726).
- **Who is shown.** Priority picks the longest start of the priority list that stacks (652-674). Input order picks where, with processes after agents (line 664). So a tile does not move when its activity changes.
- **Tall boxes** apply only when they send no working or attention tile to the tray (`place`, 623-647).
- **Tests.** `cargo test -p swarm_layout` passes 106 tests (2026-11-02). They include property tests over 2000 random cases (`tests.rs:1369-1375`): no overlap, gap kept, fixed sizes kept, minimized in the tray, deterministic, stable.

### The app

- `tile_specs` (`views.rs:222-256`) builds the input in this order: orchestrator, workers, processes, user terminals. `tile_spec` (199-218) sets `minimized` from `Workspace::minimized`, `large` from the agent and `fixed` from `Workspace::user_sizes` (user terminals only).
- `compute_layout` (258-268) runs on every render (`workspace.rs:883`).
- State is in memory and not saved: `minimized: HashSet<TerminalId>` (`workspace.rs:91`), `user_sizes` (105), `resizing` (106).
- Each tile is an absolutely positioned slot (`tile`, `views.rs:585-630`). Mouse down on it focuses the terminal.
- Header actions are `IconButton`s in a `TileFrame` (`swarm_ui/src/components/tile_frame.rs`).
- **Resize.** Only user terminals have a handle, on the bottom edge (`resize_handle`, 868-907). It has the Splitter role, the label "Resize your terminal", and ↑/↓ nudge it by 20 px (`KEY_STEP`, line 28). `resize_user_terminal` (419-437) clamps to 260 x 160 and the area minus 20 px.
- **Tray.** Chips in the status bar (`status_bar.rs:184-238`) and a "+N" menu (241-269). A chip click calls `restore`, then `focus_terminal`.

### What minimize does today

- `Workspace::minimize` (`workspace.rs:566`) adds the terminal to `minimized`. The engine puts it in the tray. Nothing else changes. The terminal keeps running. `focused_terminal` still points at the tile, which is no longer drawn.
- The button "Move name to tray" is on workers and process terminals (`views.rs:725-734`, `772-780`). The orchestrator has none. User terminals have none, only Close (854-860).
- `restore` (571) removes the tile from `minimized`. `focus_terminal` (576) also removes it, so ⌘1 to ⌘9, the palette and the Inspector's Focus un-minimize a tile.
- A tile that does not fit is in the tray but not in `minimized`. A chip click on it changes nothing you can see.
- `tile.grow` and `tile.shrink` for a minimized worker are dropped, not remembered (`views.rs:301`, tests 1658-1687).

### What the lock icon means today

User terminals showed a small padlock after "You" (`views.rs:816-820`, label "Agents have no access"), a privacy mark (ADR 0006). On 2026-11-02 the user replaced it with the person icon and the label "Mine" (accessible label "Mine, agents have no access"). So the padlock is free for the tile lock (open question 1).

### Requests from agents

`tile.grow` and `tile.shrink` (PLAN.md line 170) become `Event::TileRequest` (`swarm_protocol.rs:1319`, sent in `swarm_daemon/src/calls.rs:529`). `Workspace::tile_requested` (`views.rs:289`) sets `Agent::tile_large` (`model.rs:237`), and that becomes `TileSpec::large`. Only workers react.

### The Start tab

The engine is not used. `start_layout` (`views.rs:271-284`) puts one centered tile, up to 880 wide. The plan editor lies over it (574-581). There is no tray and no resize.

## 4. Behaviour to build

### 4.0 The tile grid

Every tile position and size is a whole number of **grid cells**. A grid cell is 8 × 8 logical pixels. Tiles move and resize in whole cells only: no tile ever sits between two cells.

**Why a grid.**

- The engine does integer math. Overlap, gap and fit checks become exact. The float `EPSILON` (`swarm_layout.rs:11`) goes away.
- Snapping is free: a drag or a resize rounds the pointer to the nearest cell.
- A locked tile keeps its exact pixels, because a cell has a fixed size. (The grid in approach D of section 5.2 scaled with the area. This one does not.)
- Saved state is small integers that never drift.

**Why 8 px.**

- 8 logical px is 8 device px at 1x and 16 at 2x. Tile edges always fall on device pixels, so frames stay sharp.
- A terminal area of 1200 × 880 is 150 × 110 cells. A step of one cell is fine enough for a drag or a resize to feel smooth.
- 4 px doubles every number with no step you can see. 16 px makes resizes jump and wastes space in the gaps.
- Change it in one place: `const CELL: f32 = 8.0` in `swarm_layout`. Nothing else may hard-code 8.

**Grid rules.**

- **G1.** The grid starts at the top-left corner of the content area: the terminal area minus the padding. The content area is `floor((area − 2 × padding) / CELL)` cells in each direction. The 0 to 7 px left over go to the right and bottom edges, so the left and top edges stay put when the window changes size.
- **G2.** Gap and padding are 1 cell (8 px; today both are 10 px).
- **G3.** Fixed sizes in the style are in cells: min tile 33 × 20 (264 × 160 px), min orchestrator 40 × 38 (320 × 304 px).
- **G4.** Sizes that come from the terminal font (the small box of 12 rows, the process box of 8 rows) round **up** to whole cells, so the rows always fit.
- **G5.** `swarm_layout` takes the area in pixels, converts it to cells once, does all its work in `u16` cells, and turns the result back into pixel rects (`origin + cells × CELL`). The app never rounds.
- **G6.** A drag shows the drop place snapped to cells (D5). A resize follows the pointer in whole cells, rounded to the nearest one. The resize keys step 1 cell, and 5 cells with ⇧ (today 20 px, `KEY_STEP`).
- **G7.** User terminal sizes (`user_sizes`) and lock sizes and anchors are stored in cells.
- **G8.** While a drag or a resize runs, the content area shows the grid as faint dots (theme token `dot`). It hides when the drag or resize ends. It is decoration: no role, no label.
- **G9.** The terminal inside a tile still fits whole rows and columns into the tile's body, as today (`tile_grid`, `views.rs:155`). The body's last few pixels below the last row stay empty, as today.

### 4.1 Shared rules

- **C1.** Controls work on the tiles of the open tab in the Terminals view. The Start tab has none.
- **C2.** A tile is **movable** when it is placed, not locked, not maximized and not the orchestrator.
- **C3.** The tile controls sit at the right end of the tile header, like window controls. Left to right: Lock, Minimize, Maximize, then Close (user terminal) or Stop (process terminal). Close is always the rightmost. A button that can never apply is not drawn, and the others keep their order. The orchestrator gets Maximize only (open question 2).
- **C4.** Every command acts on `focused_terminal`.
- **C5.** All controls are also commands with shortcuts (section 6).
- **C6.** Terminals keep running in every state. Only a drawn tile resizes its terminal (the terminal element sends the resize while it paints, `swarm_term_view/src/element.rs:477`). Hidden and tray tiles keep their last grid.

### 4.2 Drag (move a tile)

A drop on another tile changes the **order** of tiles: the engine places tiles by order, so the new order decides the new place. A drop on empty space puts the tile there, snapped to the grid, and locks it, because a lock is how a tile keeps a place (D12, open question 11).

- **D1.** A drag starts on the tile header, away from the buttons, after the pointer moves more than 2 px (GPUI's `DRAG_THRESHOLD`, `div.rs:52`). Less than that is a click. The body never starts a drag: the terminal owns the mouse there.
- **D2.** A movable tile can be dragged, and so can a locked tile (it moves to a new place, D13). The orchestrator, maximized tiles, tray tiles and the Start tab cannot. They show no grab cursor.
- **D3.** Two classes: user terminals move only among user terminals (their rows are separate, see section 5). All other tiles move only among other tiles.
- **D4.** Valid drop targets: another movable tile of the same class (reorder), the tray (minimize), and empty space where the tile fits at its size (place and lock, D12). Invalid: locked tiles, the orchestrator, and empty space where the tile does not fit.
- **D5.** While dragging, the dragged tile shows at 50 % opacity. A valid tile target, or the tray, gets a 2 px accent outline. Over empty space, an accent outline at the tile's size shows where it lands, snapped to the grid (G6). An invalid place shows nothing. No ghost image follows the pointer.
- **D6.** The layout does not change during the drag. The order changes once, on drop.
- **D7.** Drop on tile T: the dragged tile D takes T's place in the order, and the tiles in between shift by one. Example: `[a b c d]`, drop `a` on `c`, gives `[b c a d]`. Drop `d` on `b` gives `[a d b c]`. Hidden tiles (tray, minimized) count in the order.
- **D8.** Cancel by Esc, or by release on D itself, outside the tile area, or on an invalid place. Nothing changes. Esc needs a key context that exists only during a drag, so terminals keep their Esc. Call `cx.stop_active_drag(window)` (`gpui/src/app.rs:2985`).
- **D9.** After a drop or a cancel, keyboard focus is on the dragged tile's terminal.
- **D10.** New tiles join the order after the last tile of their kind. The first tile of a kind goes after the last tile of the kinds before it (worker, process, user terminal). Closed tiles leave the order.
- **D11.** Minimize, restore, lock and unlock never change the order. A restored or unlocked tile returns to its index.
- **D12.** Drop on empty space: the tile's top-left corner goes to the cell nearest to where the dragged tile's top-left corner is, and the tile keeps its size in cells. It is locked there (section 4.5). It must not overlap a locked tile or an island and must keep the gap; else the place is invalid (D4). The other tiles refill the rest.
- **D13.** Dragging a locked tile moves its lock to the new snapped place. Its size does not change. A drop on another tile is invalid for a locked tile.

GPUI APIs checked in the pinned source (`crates/gpui/src/elements/div.rs`): `on_drag` (639), `on_drop` (592), `can_drop` (603), `drag_over` (1238), `on_drag_move` (360). `can_drop` also switches off the `drag_over` style, so invalid targets show no outline (3542-3558).

### 4.3 Minimize (to the tray)

- **M1.** Every tile except the orchestrator has Minimize. User terminals get the button they lack today.
- **M2.** Minimize puts the tile in the tray as a chip. The chip keeps the status dot. The terminal keeps running and keeps its grid.
- **M3.** The other tiles refill the space by the engine's rules.
- **M4.** Restore: click the chip or the "+N" menu item (as today), or any focus command (⌘1 to ⌘9, palette, Inspector Focus). The tile returns to its index in the order and gets focus.
- **M5.** If a restored tile still does not fit, it stays in the tray (not in `minimized`) and VoiceOver reads "name does not fit".
- **M6.** A minimized tile stays minimized when it needs attention. Its chip shows the status.
- **M7.** Focus after Minimize: the next movable tile in the order, else the previous one, else the orchestrator. Never the window root.
- **M8.** Dropping a tile on the tray is Minimize.
- **M9.** Minimizing a maximized tile ends the maximize first.
- **M10.** `tile.grow` and `tile.shrink` for a minimized worker are dropped (unchanged).

### 4.4 Maximize

- **X1.** A header button toggles Maximize and Restore. Double-click on the header does the same.
- **X2.** The tile fills the content area: the terminal area minus the padding (`maximized_rect`, section 5.1).
- **X3.** Other tiles are not drawn. They keep running and keep their grid. The layout engine still runs without the maximize, so Restore gives back exactly the layout before. The tray is as the engine made it.
- **X4.** One maximized tile per tab. The state is per tab. It survives switching to the Swarm or Review view and back, and switching tabs.
- **X5.** Maximize ends when: the user restores it; another tile gets focus (⌘1 to ⌘9, palette, sidebar, chip, Inspector Focus, a new terminal); the tile is minimized, stopped or closed. The tile that got focus appears in the normal layout (as `tmux` does when you leave a zoomed pane).
- **X6.** Esc does not end Maximize. Programs in the terminal use Esc.
- **X7.** The maximized terminal resizes to the new grid. It goes back on Restore. The Swarm view's terminal tab already does this (test `the_terminal_takes_the_size_of_the_tab_and_goes_back_to_its_tile`, `swarm_view.rs:3537`).
- **X8.** The orchestrator can be maximized.
- **X9.** A locked tile can be maximized. Restore returns it to its locked place. Lock is refused while the tile is maximized (L8). Unlock works.
- **X10.** Focus stays on the tile. If the button had focus, it keeps it (same id, label flips).
- **X11.** Hidden tiles that need attention are visible in the sidebar and in notices, not in the tray (open question 7).

### 4.5 Lock

- **L1.** Header toggle "Lock name", on every tile except the orchestrator. It shows the pressed state.
- **L2.** Lock freezes the tile's rect in the normal layout: its size and its place. The place is stored as a corner of the content area plus an offset (section 5.1).
- **L3.** A locked tile has no resize handle, and no ↑/↓ resize keys. It can be dragged to a new place (D13), never resized.
- **L4.** `tile.grow` and `tile.shrink` for a locked worker are dropped, like for a minimized one. Attention and `large` do not make it taller. The attention ring still shows.
- **L5.** It shows as locked: the Lock button is pressed (accent background), the tile's accessible label ends with ", locked", and the tray chip shows a padlock.
- **L6.** Unlock: the tile goes back to the flow at its index in the order. A user terminal with a size stays an island of that size.
- **L7.** Priority: a locked tile ranks with user terminals, above everything else. When space is short, everything else leaves first, in the usual order.
- **L8.** Lock is refused, and VoiceOver reads "Can't lock name, no room", when the tile is not placed (in the tray), is maximized, or when the lock would send the orchestrator to the tray.
- **L9.** The window changes size:
  - The place follows the stored corner. The tile slides back inside the content area if the corner moved. The size never changes.
  - If it is larger than the content area, it goes to the tray with its lock. It comes back when the area is large enough.
  - If two locked tiles then touch (closer than the gap), the one earlier in the input order stays. The other goes to the tray.
- **L10.** Lock survives minimize, maximize, restore, tab and view switches, and a font size change (pixels do not change).
- **L11.** Closing or stopping a tile removes its lock.

### 4.6 How the controls combine

| Case | Rule |
|---|---|
| Locked and minimized | In the tray with a padlock chip. Restore puts it at its locked rect. |
| Locked and maximized | Fills the area. Restore returns it to its locked rect. |
| Drag a tile onto a locked tile | Invalid target (D4). Nothing happens. |
| Drag a tile onto empty space | It lands on the snapped cell and is locked there (D12). |
| Drag a locked tile | It moves to the new snapped place and stays locked (D13). The move commands step it 1 cell (section 6). |
| Window shrinks below the locked tiles | L9. Locked tiles beat the rest. The rest go to the tray by priority. |
| A new tile arrives and locked tiles fill the area | The new tile goes to the tray by priority. |
| Lock a tile, then move others | Only the order changes. The locked rect does not move. |
| Maximize a tile while another is maximized | Not reachable by pointer. By command, the first one restores, then the second maximizes. |
| Minimize the focused, maximized tile | Maximize ends, then the tile goes to the tray (M9). |
| Start tab | No controls. Commands do nothing. |

## 5. Layout engine design

### 5.1 Inputs and outputs

```rust
pub struct TileSpec {
    // today's fields, without `pinned`
    /// The user locked the tile: it keeps this size and place.
    pub locked: Option<Lock>,
}

/// Logical pixels per grid cell (section 4.0).
pub const CELL: f32 = 8.0;

/// A size or a distance in whole grid cells.
pub struct Cells {
    pub width: u16,
    pub height: u16,
}

pub struct Lock {
    pub size: Cells,
    pub anchor: Anchor,
}

/// The distance from one corner of the content area to the same corner of the tile.
pub struct Anchor {
    pub corner: Corner,
    pub offset: Cells,
}

pub enum Corner { TopLeft, TopRight, BottomLeft, BottomRight }

pub enum Direction { Left, Right, Up, Down }

impl Lock {
    /// Locks `rect`, snapped to the grid. The corner is the one nearest to the tile's center.
    pub fn from_rect(rect: Rect, content: Rect) -> Self;
}

pub fn maximized_rect(area: Rect, style: &LayoutStyle) -> Rect;
pub fn move_before(order: &mut Vec<TileId>, moved: TileId, target: TileId);
pub fn neighbour(layout: &Layout, from: TileId, direction: Direction, movable: &[TileId])
    -> Option<TileId>;
```

- A corner anchor keeps a tile at the right edge when the window grows. A fixed top-left offset would not (open question 3).
- `layout` keeps its signature. The input order is final: remove the "processes after agents" sort in `select` (`swarm_layout.rs:664`). The app builds the default order (workers, then processes). The test `processes_go_after_agents` (`tests.rs:1076`) moves to the app.
- `neighbour` picks, among movable tiles of the same class, the nearest one whose center lies on the requested side. Ties break by distance on the other axis, then by input order. Locked tiles are skipped.

### 5.2 Approaches compared

| | A. Free rectangles, then today's lanes | B. Bin packing (MaxRects, skyline) | C. Constraint solve | D. Grid with spans |
|---|---|---|---|---|
| Idea | Cut the free space into non-overlapping rects around fixed rects. Run the existing lane stacking in each rect. | Keep a list of overlapping free rects. Put each tile in the best one. | Fixed tiles are equalities, flexible tiles have minimum sizes and a preference. | Snap everything to a coarse grid. Locked tiles take spans. Others auto-flow. |
| Deterministic | Yes. Fixed tie-breaks. | Yes, but small input changes cascade. | Yes in theory. Ties and infeasible cases need extra rules. | Yes. |
| Stable between frames | Good. Order drives the place (today's rule). | Poor. Order of insertion decides everything. | Fair. Which tile sits next to which is not decided by the solver. | Good. |
| Speed, ~20 tiles | Very fast. Today's 106 tests, with property runs of 2000 random cases each, take 0.06 s. | Fast. | Fast enough, but needs a solver crate. | Fast. |
| Testable | Yes. Same style of tests as now. | Yes, but results are hard to predict by hand. | Hard. Float results, no obvious expected values. | Yes. |
| Fits this code | Reuses `lanes_in`, `stack`, `select`, `wanted_height`. | Replaces all of `Flow`. Boxes lose aligned columns. | Replaces all of it. | Locked sizes round up to cells. With a coarse grid that scales with the area, a locked tile no longer keeps its exact pixels. |
| Main risk | Hard to fill every hole. | Ragged look. | Over-constrained input has no answer. | Fails "keeps its size". |

D's coarse grid with spans is not the fine grid of section 4.0. The fine grid has a fixed cell of 8 px, so a locked tile keeps its pixels. It is the unit that A works in, not a layout method of its own.

### 5.3 Recommendation: A, on the fine grid

Use free rectangles and keep the lane stacking. Every rect, gap and size in the steps below is in whole cells (section 4.0), so all checks are exact integer checks.

**Why.** The lane rules (small, tall and process boxes, priority picks who, order picks where, tall only if it costs nothing) already do what the product wants. They only assume one obstacle: the island block at the top right. A is the smallest change that removes that assumption.

**Two paths.** With no locked tile, run today's code unchanged ("path A"). With at least one locked tile, run the new code ("path B"). When path B is proven, try to merge the paths. The 106 tests must pass unchanged after the merge.

**Path B, step by step.**

1. `content` = area minus padding. Minimized tiles go to the tray.
2. **Locked rects.** In input order, turn each lock into a rect from its anchor. Slide it inside `content`. A tile larger than `content`, or closer than the gap to an earlier locked rect, goes to the tray.
3. **Islands.** Pack user terminals with a size from the top right, as today, but skip any position closer than the gap to a locked rect. The scan goes right to left, then down. An island with no position goes to the tray.
4. **Free regions.** Obstacles are the locked rects and the islands.
   - Cut `content` into vertical strips at the left and right edge of each obstacle (grown by the gap).
   - In each strip, the free vertical intervals are what lies between obstacles.
   - Merge neighbour strips that have the same interval.
   - Drop rects smaller than `min_tile`.
   - Sort the rects by top, then left.
   - Strips, not bands, because lanes are columns: a column beside a locked tile should stay one tall lane. Today's code cuts bands (beside and below the island block), so the two give different regions for an island. That is why path A stays until the merge is proven.
5. **Orchestrator.** Pick the region of at least `min_orchestrator` with the largest area. Ties: leftmost, then topmost. Its column starts at the region's left edge and has the region's full height. The width is `share` of the region, at least `min_orchestrator.width`, and leaves one `min_tile` lane beside it when other tiles need it. With nothing else to show, it takes the whole region. The rest of the region (if at least `min_tile` wide) goes back to the free list. No such region: the orchestrator goes to the tray.
6. **Flow.** Lanes come from all free regions in sorted order. Then it is today's `Flow::place`: select the longest start of the priority list that stacks, stack it in input order, choose tall or small as today.
7. Everything that found no room goes to the tray.

### 5.4 What "smart fill" means

- **S1. Fixed first.** Locked tiles and islands never move or resize. Nothing overlaps them. The gap stays.
- **S2. Orchestrator next.** It takes the biggest region at its share, as the main slot (step 5).
- **S3. Order decides the place.** Tiles flow through the regions in reading order, in the user's order. Priority decides who stays on screen. Activity never moves a tile.
- **S4. Attention gets size.** A tile that needs attention, or a worker with `large`, is a tall box when that costs no working or attention tile its place. A lane shorter than a tall box cuts the first box in it down to the lane, never below `min_tile.height`.
- **S5. No big holes.** While a tile waits in the tray, no lane keeps free space as tall as the smallest waiting box (a process box 218, a small box 286). This holds today inside one lane. Across lanes it needs two changes (open question 5): first-fit across all lanes for non-first boxes, and cut a box to the space left, never below `min_tile.height`. Free space stays empty when no tile waits. Boxes do not stretch (open question 6). A region narrower than `min_tile.width` cannot hold a tile, so it does not count.
- **S6. Stable.** Same input, same output. Locked rects depend only on the lock and the area. Adding a tile that goes to the tray changes nothing. Adding a lower-priority tile never sends a higher one to the tray. Unlocking a tile does not change the order.
- **S7. Width.** The lanes of a region share its width equally (as today).

### 5.5 When locked tiles leave no room

- Everything that is not locked leaves first, in the usual priority order (lowest first). The orchestrator outranks all of those tiles, as today, but not the locked tiles.
- To keep the orchestrator on screen, Lock is refused when it would remove the orchestrator's region (L8). A window that shrinks later can still do it. Then the orchestrator goes to the tray, and so do the locked tiles that do not fit (L9).
- A tile in the tray that is locked keeps its rect. It returns when there is room.

## 6. Keyboard and VoiceOver

### Commands

All shortcuts are proposals. Check each against `actions::shortcuts()` and the terminal's own keys when you build it. `⌘M` is Minimize window, so Minimize tile cannot use it. `⇧⌘↩` is also iTerm2's maximize pane.

| Action | Keys | Palette and menu | VoiceOver says | Focus after |
|---|---|---|---|---|
| `MoveTileLeft`, `Right`, `Up`, `Down` | ⌃⌘←, ⌃⌘→, ⌃⌘↑, ⌃⌘↓ | "Move tile left" and so on. Tile menu. | "name moved, position 2 of 5". At an edge: "Can't move left". Locked: "name is locked". | The moved tile's terminal |
| `MinimizeTile` | ⇧⌘M | "Minimize tile" | "name moved to tray" | Next movable tile, else the previous, else the orchestrator |
| `MaximizeTile` (toggle) | ⇧⌘↩ | "Maximize tile" / "Restore tile" | "name maximized" / "name restored" | Same tile |
| `LockTile` (toggle) | ⇧⌘L | "Lock tile" / "Unlock tile" | "name locked" / "name unlocked" / "Can't lock name, no room" | Same tile |
| Tray chip (Enter, Space) | none | none | "name restored" or "name does not fit" | The restored tile |
| Pointer drag: drop / Esc | none | none | "name moved, position 2 of 5" / "Move cancelled" | The dragged tile |

- Position counts the movable tiles of the same class, in order.
- For a locked tile, the move commands step its place by 1 cell (⇧ for 5 cells) instead of changing the order. VoiceOver says the new place in cells from the top-left, for example "name moved to column 12, row 4".
- Move uses `neighbour` and `move_before` (section 5.1).
- The palette shows these rows only in the Terminals view, only when they apply (as "Comment on line" does for Review, `palette.rs` test `comment_on_line_is_in_the_palette_only_while_review_is_open`). Add a group "Tile".
- Add a **Tile** menu in `app_menus.rs`, between View and Window. Item names follow the state ("Lock Tile" / "Unlock Tile"). The audit already lists a static "Enter Full Screen" as a bug (`app_menus.rs:72`).
- `shortcuts()` feeds the Settings › Keyboard page, so the new keys show there under "Terminals".
- Every drag has a keyboard command. The drag area has no role. It is for the mouse only.

### Roles and labels

| Element | Role | Label | State |
|---|---|---|---|
| Lock button | Button | "Lock name" | toggled (`IconButton::toggle_state`, `icon_button.rs:120`) |
| Maximize button | Button | "Maximize name" / "Restore name" | none |
| Minimize button | Button | "Move name to tray" (today) | none |
| Tile | Group | "name terminal" + ", locked" and/or ", maximized" | none |
| Tray chip | Button | "Show name, status" + ", locked" | none |
| Resize handle | Splitter | "Resize your terminal" (↑/↓ step 1 cell, ⇧ 5 cells, G6) | gone when locked |

- Header buttons are tab stops, in the header order of C3 (left to right, Close last).
- Announcements go through the live region from the audit's round 1. Do not build a second mechanism.
- Focus never lands on a view that is not drawn, and never on the window root (audit H2 and M5).
- Contrast: the pressed Lock button and the drop outline use theme tokens (`active`, `accent`) and meet WCAG AA like the other controls. Add them to the contrast test.

## 7. State

| What | Scope | Lives in | Saved |
|---|---|---|---|
| `order: Vec<TerminalId>` | per tab | app memory | slice 5 |
| `locked: HashMap<TerminalId, Lock>` (cells) | per terminal | app memory | slice 5 |
| `minimized: HashSet<TerminalId>` | per terminal | app memory (today) | slice 5 |
| `user_sizes` (cells, from slice 0) | per terminal | app memory (today) | slice 5 |
| `maximized: Option<TerminalId>` | per tab | app memory | no |

- **Today** none of this survives quitting the app. It does survive closing the window, because the workspace lives on without a window (`workspace.rs:80`). Slices 1 to 4 keep that.
- **Maximize is not saved.** It is a temporary view. The app opens with all tiles shown.
- **Saving needs the daemon.** ADR 0005: only the daemon reads and writes SQLite, and the app asks over the socket. The plan draft is the model: `SavePlanDraft` (`swarm_protocol.rs:947`). Agent terminals already survive an app restart (ADR 0001), so their layout should too.
- **Key.** The app's `TerminalId` (u32) is a local counter and is not stable. The daemon's terminal id (u64, `swarm_protocol.rs:65`) is. Save by daemon terminal id and task.
- **Shape.** One table `tile_state(task, terminal, position, minimized, lock)` with a migration, and two calls: read the state of a task, and replace it. The app sends a save after each change, with a short delay to merge bursts. Rows go when their terminal goes.
- **No daemon, no save.** The app works with in-memory state, as with the plan draft.

## 8. Tests

### Layout unit tests (`crates/swarm_layout/src/tests.rs`)

- `every_placed_rect_sits_on_whole_cells`
- `the_content_area_is_whole_cells_and_the_rest_goes_right_and_bottom`
- `font_sized_boxes_round_up_to_whole_cells`
- `gap_and_padding_are_one_cell`
- `a_locked_tile_keeps_its_size_in_every_area`
- `a_locked_tile_keeps_its_corner_when_the_area_grows`
- `a_locked_tile_slides_back_inside_when_the_area_shrinks`
- `a_locked_tile_larger_than_the_area_goes_to_the_tray`
- `of_two_locked_tiles_that_touch_the_earlier_one_stays`
- `no_tile_overlaps_a_locked_tile_and_the_gap_stays`
- `tiles_fill_the_free_column_beside_a_locked_tile`
- `tiles_fill_the_free_space_below_two_locked_tiles`
- `the_orchestrator_takes_the_largest_free_region_at_its_share`
- `the_orchestrator_takes_all_of_its_region_when_nothing_else_is_shown`
- `the_orchestrator_goes_to_the_tray_when_no_region_is_large_enough`
- `locked_tiles_beat_every_other_tile_when_space_is_short`
- `a_minimized_locked_tile_is_in_the_tray_and_returns_to_its_rect`
- `unlocking_a_tile_puts_it_back_at_its_index`
- `input_order_is_final_a_process_above_a_worker_stays_above` (replaces `processes_go_after_agents`)
- `move_before_*` (down, up, first, last, same tile, unknown tile)
- `neighbour_*` (each direction, skips locked tiles, none at the edge, ties)
- `lock_from_rect_picks_the_nearest_corner`
- `maximized_rect_is_the_area_minus_the_padding`
- With no locked tile, path B is not used and every old test passes unchanged.

### Property tests (add `locked` to `random_tile`)

- Every placed rect starts and ends on a cell edge, for any area in pixels, also areas that are not a multiple of 8.
- A locked tile's size never changes. Its origin never changes when the area is the same.
- Placed tiles never overlap and keep the gap, locked ones included.
- Everything lies inside the content area.
- Deterministic.
- No free space as tall as the smallest waiting box while a tile waits (S5).
- Adding a tile that goes to the tray changes nothing else.
- Adding a lower-priority tile never sends a higher one to the tray.
- Goal for the merge of the two paths: path B with a locked tile in the top-right corner gives the layout path A gives for an island of the same size. If they differ, decide which result is better and change the test on purpose.

### App tests (`views.rs` style, `TestAppContext`)

- **Grid.** A resize by pointer lands on whole cells; ↑/↓ step 1 cell and ⇧ 5. The dot grid shows only while a drag or a resize runs.
- **Drag.** Drop on a tile reorders. Drop on empty space places the tile on the snapped cell and locks it; where it does not fit, nothing changes. Drop on a locked tile and on the orchestrator changes nothing. Dragging a locked tile moves it in whole cells. Drop on the tray minimizes. Esc and release outside cancel. Less than 2 px is a click. A user terminal cannot drop on a worker. The order survives a restore.
- **Minimize.** A user terminal gets the button. Focus goes to the next movable tile. Restore from the "+N" menu keeps the order index. A tile that does not fit says so.
- **Maximize.** Rect equals `maximized_rect`. No other `tile-slot-*` is drawn. The terminal gets the new grid, and the old one on Restore. Restore gives back the exact earlier layout. Focus on another tile ends it. One per tab. Switching view and tab keeps it. Minimize, stop and close end it. Double-click toggles it. Esc does not end it.
- **Lock.** The resize handle is gone. `tile_requested` is dropped. An attention status does not grow the tile. Lock is refused for tray tiles, for maximized tiles and when the orchestrator would lose its place. Unlock returns the tile to its index. The window shrinks: the locked tile slides back and the others go to the tray. Closing the tile drops its lock.
- **Keyboard.** Dispatch each action and assert order or state, the VoiceOver text and the focus. Edge cases: no neighbour, locked, Start tab, tile in the tray.
- **Accessibility** (`crates/swarm_ui/tests/a11y.rs`, `a11y_components.rs`): roles and labels of the Lock and Maximize buttons and the toggled state; the new tile and chip labels; a Tab-walk through a tile header.
- **Daemon** (slice 5): save, reload and delete of tile state. A restart restores the order and locks.

### Snapshots (`snapshot.rs`, `SWARM_APP_SNAPSHOT_DIR`)

Add, each in light and dark: `workspace-locked`, `workspace-locked-corner` (a locked tile bottom right, others filling around it), `workspace-maximized`, `workspace-dragging` (outline on the target), `workspace-tray-locked` (padlock chip). Add the Lock and Maximize buttons and the locked chip to the `TileFrame` and `TrayChip` previews in the gallery.

## 9. Open questions

Each has a recommended answer.

1. **The padlock used to mean "agents have no access" on user terminals.** Decided (2026-11-02): user terminals now show the person icon and "Mine", so the tile lock uses the padlock (closed and open). The icon set has no Unlock or Maximize icon yet. Add them.
2. **Can the orchestrator move, lock or minimize?** Recommend: it can only maximize. It keeps the main slot, and PLAN.md says it stays on screen.
3. **Where does a locked tile go when the window changes size?** Recommend: the nearest corner anchor (section 5.1). Other options: a fixed top-left offset, or a share of the area. The first breaks right-edge tiles when the window grows. The second makes the size change.
4. **Locked tiles would starve the orchestrator. Refuse the lock or let the orchestrator go?** Recommend: refuse at lock time (L8). A window that shrinks later can still tray the orchestrator.
5. **May a box be cut short, or placed in an earlier lane, to use a hole?** Recommend: yes. Cut a non-first box down to the space left, never below `min_tile.height`, and use first-fit across lanes, so a working tile does not go to the tray while a hole sits free. This changes tests that expect a tray result with room left (search `goes_to_the_tray` in `tests.rs`). Apply it to both paths.
6. **Do boxes stretch to fill leftover space when no tile waits?** Recommend: no. PLAN.md says workers are never the full height. Maximize is the tool for a big tile.
7. **While a tile is maximized, show the other tiles as chips?** Recommend: no. The sidebar and the notices already show status. A chip would also mix hidden and tray tiles.
8. **Does focusing another tile end Maximize, or move it to that tile?** Recommend: end it. It is predictable and matches `tmux` zoom.
9. **Save the state across an app restart?** Recommend: yes, in slice 5, through the daemon.
10. **Shortcuts.** Recommend the keys in section 6, checked against the Keyboard page when you build.
11. **Should a drag drop a tile on a free spot of the user's choosing?** Decided with the grid (2026-11-02): yes. Tile moves snap to the grid, so a free drop has simple rules: the tile lands on the snapped cell and is locked there (D12). A drop on another tile still reorders.
12. **Does resizing a user terminal lock it?** Recommend: no. A sized user terminal stays an island: fixed size, place chosen by the engine. Lock fixes the place too, and removes the resize handle.
13. **Is 8 px the right cell?** Recommend: yes (section 4.0). Revisit only if 8 px resize steps feel coarse in use; 4 px is the fallback, never smaller.

## Docs to update when this is built

- `GLOSSARY.md`: add **Grid cell** (the 8 px unit every tile's place and size is made of), **Lock** (a locked tile keeps its size and place) and **Maximize** (a tile fills the area). Change **Tray**: tiles also go there when the user minimizes them.
- `docs/PLAN.md` lines 155 to 157: "pin" becomes "lock"; a locked tile ignores `tile.grow` and `tile.shrink`. Add the order, the maximize rule and the keys.
- `docs/ROADMAP.md`: add a done-when for each control.
- `crates/swarm_layout/README.md`: locked tiles, the free-region fill rules, the order rule.
- `crates/swarm_ui/README.md`: the new component states and icons.
