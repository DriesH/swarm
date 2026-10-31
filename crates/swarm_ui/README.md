# swarm_ui

Design tokens, themes and components for Swarm, built on raw GPUI.

## Use

```rust
use swarm_ui::prelude::*;

// once, at startup
swarm_ui::init(cx);

// in the root view: keep the subscription, wrap the root element
let appearance = swarm_ui::theme::follow_system_appearance(window, cx);
swarm_ui::tab_navigation(div().id("root")).child(/* ... */)
```

Focus an element when the window opens. With nothing focused, GPUI sends Tab nowhere.

- Colors: `cx.theme().colors().fg` and the other tokens in `ThemeColors`. Never use raw colors.
- Theme: `swarm_ui::theme::set_appearance(Appearance::Light | Dark | System, cx)`.
- Fonts: Geist and Geist Mono are bundled from `assets/fonts/` (SIL OFL 1.1, see `assets/fonts/OFL.txt`).

## Components

| Component | Role (VoiceOver) |
|---|---|
| `Icon`, `IconName` | decorative, or `Image` with `.accessible(id, label)` |
| `Label` | none: the interactive parent carries the label |
| `Button` | `Button` |
| `IconButton` | `Button`; the label is a required argument |
| `Switch` | `Switch` with toggled state |
| `SegmentedControl` | `RadioGroup` of `RadioButton`s |
| `Tab` | `Tab` with selected state; label is "name, status" |
| `StatusPill`, `StatusRing`, `StatusDot` | none: status goes into the parent's label |
| `ListRow` | `ListItem` with selected state |
| `TrayChip` | `Button`, "Show name, status" |
| `TrayMore` | `Button`, "4 more tiles" |
| `Panel`, `PanelRow` | `Group` with `.accessible(id, label)` |
| `TileFrame` | `Group`, "name terminal" |

Every interactive component is focusable, works with Enter and Space (GPUI turns them into a click), and shows a 2px accent focus ring when focused from the keyboard.

## Gallery

```sh
cargo run -p gallery
```

Save every page as PNG, in light and dark:

```sh
SWARM_GALLERY_SNAPSHOT_DIR=/tmp/shots cargo run -p gallery --features snapshot
```

The gallery opens and closes one window per page. Screen recording permission is not needed.

## Adding a component

1. Add `src/components/<name>.rs`: a `RenderOnce` + `#[derive(IntoElement)]` builder.
2. Give it a role and label if it is interactive, `.focusable().tab_stop(true)` and `focus_visible(focus_ring)`.
3. Implement `Component` with a preview and add it to `all_components()` in `src/components.rs`.
4. Add an a11y test in `tests/a11y.rs`.

## Decisions

- **Explicit registry, not `inventory`.** `all_components()` is a plain list. It keeps the gallery order stable, needs no proc-macro crate and no link-time tricks. The cost is one line per new component.
- **Token changes from the mockups**, so text meets WCAG AA 4.5:1 on every surface and on its own tinted pill:
  - light `accent` #2f6bf0 → #285acb, `ok`/`add` #1a7f36 → #176f30, `warn` #a8650a → #8c5408, `del` #c9362c → #b63128, `faint` #6b6b74 → #696971
  - dark `faint` #86868f → #878790
  - light `switch_off` #d4d4d8 → #92929d: the off switch was invisible (1.5:1).
- **Claude Code mark** keeps #D97757 in both themes. It is 2.8:1 on the light ground; logos are exempt from WCAG 1.4.11.
- **SegmentedControl** reserves the medium-weight width of every label (an invisible copy), so selecting never shifts the row. One highlight glides to the selected segment in 180 ms (ease-out). Segment positions are measured after layout with `on_children_prepainted` and kept in keyed window state; until then the selected segment paints its own background, so the first frame is already right.
- **Focus ring** is a 2px inset box shadow in `accent`, so it never moves layout. An outer shadow does not work: GPUI fills it behind the element, so transparent controls showed a solid block.
- **Element ids must be unique among siblings.** Two buttons with the same id share one focus handle and both show the ring.

## Traps

- App root views must set `key_context("SwarmRoot")` (`swarm_ui::ROOT_CONTEXT`) for Tab focus to work. Terminal views will bind Tab in their own context.

## Known gaps

- macOS "Reduce motion" is read at startup and each time a window becomes active (`motion::follow_reduce_motion`), not the moment the setting changes. GPUI animations then jump to their end state.
- Plain text is not in the accessibility tree; only interactive elements and labelled groups are.
- GPUI has no `aria-disabled`: disabled buttons are removed from the tab order but VoiceOver does not say "dimmed".
- GPUI's test platform never activates accessibility, so tests check each component's role and properties, not the live AccessKit tree. `Icon` is not covered: its render returns `AnyElement`, which hides the role.
- GPUI logs `window not found` a few times after snapshot windows close. Snapshot mode only; harmless.

## Manual VoiceOver check

Run `cargo run -p gallery`, turn on VoiceOver (Cmd-F5) and check:

- Each sidebar row reads its name and "selected"; Up and Down move between rows.
- The theme control reads as a radio group "Theme" with three options.
- Buttons, icon buttons, tray chips and the "+N" tray chip read their label; Enter and Space press them.
- Switches read "on" or "off" and change with Space.
- Tabs read "name, status" and "selected".
- Tiles read "name terminal" as a group.
