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

**User terminals.** A user terminal with a fixed size is an island. The layout never resizes it. Islands go at the top right, side by side from right to left. When a row is full, the next island starts a new row below. An island goes to the tray only when it does not fit in the area. A user terminal without a fixed size is placed like any other tile, with the highest priority. Only user terminals are islands: the layout ignores `fixed` on every other kind.

**Orchestrator.** The orchestrator gets a full-height column on the left. It is `orchestrator_share` of the content width (65 %), never narrower than `min_orchestrator.width` (320 px), and it leaves room for one `min_tile` lane plus the gap when other tiles are on screen. With islands, the lane goes below them when there is height for it, else beside them (so a user terminal as tall as the area still leaves a lane beside it). When no other tile is on screen, the column fills all the width left of the islands. The orchestrator goes to the tray only when the area is smaller than `min_orchestrator`.

**Other tiles.** All other tiles are boxes in lanes, in the space that is left: beside the islands (to their left) and below them. A lane is a column at least `min_tile.width` (260 px) wide; the lanes of a region share its width equally, so a wide right side holds more than one column.

- A box is `small_tile_height` high (the app measures it from the terminal font: about 12 rows). Boxes are stacked from the top of a lane. The free space below the last box stays empty; boxes are never stretched. When a lane is full, the next box goes to the next lane. The first box of a lane is cut to the lane when the lane is shorter, but never below `min_tile.height` (160 px).
- A process terminal (dev server, test run) is the smallest box: `process_tile_height` high (the app measures it from the terminal font: the tile header and about 8 rows), at least `min_tile.height` and never taller than a small box. It is a flow tile like a worker, below the workers in its lane, and never takes width from the orchestrator. The terminal inside follows the tile; an agent cannot ask for a size.
- A tile that needs attention is a tall box: as tall as two small boxes and the gap, never the whole lane. A process terminal that failed grows the same way. It shrinks back when its activity changes. If a tall box would send a working tile or another tile that needs attention to the tray, it stays small.
- A worker with `large` set (it asked for a large tile) is a tall box in the same way, whatever its activity, and does not change its priority. Without `large` it is small again, unless it needs attention. The orchestrator, process terminals and user terminals ignore `large`, and so does a pinned worker. A minimized worker is in the tray.
- Every tile competes for the lanes in priority order, also behind a tall box: they stack below it, or go to the next lane.
- If a tile does not fit, the tile with the lowest priority goes to the tray.
- Priority picks which tiles are on screen. The order in the lanes is the input order, with agents before process terminals. So a tile does not move when its activity changes.

**Spacing.** All tiles stay inside the area minus `padding` (10 px). Neighbors are `gap` (10 px) apart. No two tiles overlap.

**Stability.** Adding a tile that goes to the tray changes nothing else. Adding a lower-priority tile never sends a higher-priority tile to the tray.

## Mockup layouts

Workspace (area 1030 × 790): orchestrator, a working worker, a worker that needs attention, two processes. The orchestrator takes 65 % (656 px). A tall box and a small box do not fit together, so both workers are small boxes; the processes, the done worker and the idle worker are in the tray. In a taller area the worker that needs attention is a tall box and the processes stack below it.

```
+----------------------+--------------+
|                      | term-render  |
|                      | (working)    |
|                      |              |
| orchestrator         +--------------+
|   656 px             | pty-io       |
|                      | (attention)  |
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
