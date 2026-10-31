# swarm_layout

The layout engine for terminal tiles. It decides where each tile goes and which tiles go to the tray.

It is a pure function with no GPUI dependency. The same input always gives the same output. `swarm_app` converts the rects to GPUI pixels.

## Use

```rust
use swarm_layout::{layout, Activity, LayoutStyle, Rect, TileId, TileKind, TileSpec};

let tiles = [
    TileSpec::new(TileId(0), TileKind::Orchestrator, Activity::Working),
    TileSpec::new(TileId(1), TileKind::Worker, Activity::Attention),
];
let result = layout(Rect::new(0.0, 0.0, 1030.0, 790.0), &tiles, &LayoutStyle::default());
// result.placed: (id, rect) pairs. result.tray: ids of tiles in the tray.
```

## Rules

**Who stays on screen.** When space is short, tiles go to the tray in this order, last first:

1. User terminals
2. The orchestrator
3. Pinned tiles
4. Tiles that need attention (blocked or error)
5. Working tiles
6. Running process terminals
7. Done tiles
8. Idle tiles

Tiles with the same priority keep their input order. A minimized tile always goes to the tray.

**User terminals.** A user terminal with a fixed size is an island. The layout never resizes it. Islands go at the top right, side by side from right to left. When a row is full, the next island starts a new row below. An island goes to the tray only when it does not fit in the area. A process terminal whose agent fixed its size is an island too, but the layout grants it the closest size that fits: its size, at least `min_tile`, at most the free area (beside the orchestrator column when there is one). User islands are placed first, then process islands by priority. A user terminal without a fixed size is placed like any other tile, with the highest priority.

**Orchestrator.** The orchestrator gets a full-height column on the left. It is `orchestrator_share` of the content width (65 %), never narrower than `min_orchestrator.width` (320 px), and it leaves room for one `min_tile` column plus the gap on the right when other tiles are on screen. When no other tile is on screen, the column fills all the width left of the islands. The orchestrator goes to the tray only when the area is smaller than `min_orchestrator`.

**Other tiles.** All other tiles are boxes in lanes, in the space that is left: beside the islands (to their left) and below them. A lane is a column at least `min_tile.width` (260 px) wide; the lanes of a region share its width equally, so a wide right side holds more than one column.

- A box is `small_tile_height` high (the app measures it from the terminal font: about 12 rows). Boxes are stacked from the top of a lane. The free space below the last box stays empty; boxes are never stretched. When a lane is full, the next box goes to the next lane. The first box of a lane is cut to the lane when the lane is shorter, but never below `min_tile.height` (160 px).
- A process terminal with a requested height gets that height (at least `min_tile.height`).
- A tile that needs attention grows: its box is as tall as two small boxes and the gap, and it takes the free space below the boxes of its lane. It shrinks back when its activity changes. If that would send a working tile or another tile that needs attention to the tray, the box starts small and only takes the free space. Several boxes that need attention share the free space.
- A lane with a box that needs attention is closed to tiles that rank below working tiles (running processes, done and idle tiles): they go to the next lane or to the tray.
- If a tile does not fit, the tile with the lowest priority goes to the tray.
- Priority picks which tiles are on screen. The order in the lanes is the input order, with agents before process terminals. So a tile does not move when its activity changes.

**Done and idle tiles.** They are on screen only when no other tile in the grid is active (needs attention, working, running or pinned). So they never take space from a tile that is doing work. Pin a done tile to keep it on screen.

**Spacing.** All tiles stay inside the area minus `padding` (10 px). Neighbors are `gap` (10 px) apart. No two tiles overlap.

**Stability.** Adding a tile that goes to the tray changes nothing else. Adding a lower-priority tile never sends a higher-priority tile to the tray.

## Mockup layouts

Workspace (area 1030 × 790): orchestrator, a working worker, a worker that needs attention, two processes. The orchestrator takes 65 % (656 px). The worker that needs attention takes the space the working worker leaves, so the processes, the done worker and the idle worker are in the tray.

```
+----------------------+--------------+
|                      | term-render  |
|                      | (working)    |
|                      +--------------+
| orchestrator         | pty-io       |
|   656 px             | (attention)  |
|                      |              |
|                      |              |
+----------------------+--------------+
tray: cargo test, pnpm dev, ui-tokens (done), bench (idle)
```

Island (area 1060 × 790): the user terminal is 650 × 470 at the top right. The orchestrator column gets the 380 px left of it. Two boxes fit below it.

```
+--------------+---------------------------------+
|              | your terminal (zsh)             |
|              | fixed 650 x 470                 |
| orchestrator |                                 |
|   380 px     +----------------+----------------+
|              | term-render    | cargo test     |
|              | (working)      | (process)      |
+--------------+----------------+----------------+
tray: pty-io (working), ui-tokens (done), bench (idle)
```
