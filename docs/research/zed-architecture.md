# How Zed builds GPUI apps

Research for Swarm, 2026-10-30.
Source: shallow clone of `zed-industries/zed` at commit `70b1b91b73ee12930b4e3375d68657e9591b1921` (2026-10-30).
All paths below are relative to the Zed repo root.

## Conclusion

### What Swarm should copy

- **Crate split by layer.** Model crates (`terminal`, `project`) depend on `gpui` only. View crates (`terminal_view`, `*_ui`) depend on model crates. One app crate (`zed`) wires everything. Map: `swarm-term` = model + element, `swarm-app` = views and wiring.
- **One `init(cx)` per crate**, called in order from `main`. See `crates/zed/src/main.rs` (lines ~479–770).
- **Components as `RenderOnce` + `#[derive(IntoElement)]` builders**, with shared traits (`Clickable`, `Disableable`, `Toggleable`) and a `prelude` module. See `crates/ui/src/prelude.rs`, `crates/ui/src/traits/`.
- **Semantic color tokens.** Components never use raw colors. They call `cx.theme().colors().<token>` or a `Color` enum that maps to tokens. See `crates/ui/src/styles/color.rs`.
- **Themes as JSON families** with a light and a dark variant, plus a `Dynamic { mode, light, dark }` selection that follows the system. See `assets/themes/one/one.json`, `crates/theme_settings/src/settings.rs`.
- **Component previews** via a `Component` trait + `#[derive(RegisterComponent)]` + `inventory`. A preview app lists every component. See `crates/component/`, `crates/ui_macros/`, `crates/component_preview/`.
- **Terminal backend behind a seam.** Zed wraps `alacritty_terminal` in one file and exposes its own `Cell`, `Content`, `Cursor` types. Do the same for `libghostty-vt`. See `crates/terminal/src/alacritty.rs`.
- **Terminal event batching**: handle the first PTY event at once, then batch for 4 ms or 100 events, then one `cx.notify()`. See `crates/terminal/src/terminal.rs` `subscribe()` (~line 1420).
- **Grid painting**: merged background rects, then selection, then batched text runs, then cursor. See `crates/terminal_view/src/terminal_element.rs`.
- **Accessibility from day 1** with GPUI's built-in AccessKit API (`.role()`, `.aria_label()`, …) inside every `swarm-ui` component, like Zed's `ButtonLike`.
- **Rules file style**: Zed's `.rules` (symlinked as `CLAUDE.md`, `AGENTS.md`, `GEMINI.md`) lists traps, not maps. Its GPUI section is a good base for Swarm's own `CLAUDE.md`.

### What Swarm should do differently

- **Do not copy code from `ui`, `theme`, `component`, `ui_macros`, `settings`, `terminal`, `terminal_view`, `editor`, `workspace`.** They are **GPL-3.0-or-later** (`crates/ui/Cargo.toml`, `crates/terminal_view/Cargo.toml`, …). Only `gpui`, `gpui_platform`, `gpui_macos`, `gpui_apple`, `collections`, `refineable`, `util` are Apache-2.0. Copy patterns, write our own code.
- **Do not depend on `ui`/`theme` via git.** They pull in Zed's settings, icons and menu crates and are GPL. Build `swarm-ui` on raw `gpui` (the plan already says so).
- **Small settings model.** Zed has one huge `SettingsContent` struct (`crates/settings_content/src/settings_content.rs`) and a `Settings` trait with a derive. That is too much for Swarm. A typed `Global` per config file is enough.
- **Terminal accessibility.** Zed's terminal element has no a11y role, so VoiceOver cannot read it. Swarm should expose terminal text (see section 5).
- **Turn accessibility on.** Zed ships with it off unless `ZED_EXPERIMENTAL_A11Y=1` (`crates/zed/src/main.rs:87`). Swarm should use the default `Application` (a11y on).
- **Own cursor and selection painting.** Zed's terminal borrows `CursorLayout` and `HighlightedRange` from `editor` (`terminal_element.rs:1`). Swarm has no editor crate, so `swarm-term` needs its own.
- **Menu bar status item**: GPUI has no `NSStatusItem` API (search for `NSStatusItem`/`status_item` in `crates/gpui*` found nothing). Spike 4 needs a small `objc2` bridge.

---

## 1. Repo structure

- **Workspace**: `Cargo.toml` with `resolver = "2"`, edition 2024, `publish = false` by default. 259 members; 252 folders in `crates/`.
- **Naming**: snake_case. Suffixes show the layer:
  - `*_ui` / `*_view` = views (`git_ui`, `agent_ui`, `terminal_view`, 14 `*_ui` crates).
  - `*_settings` = settings for a domain (`agent_settings`, `theme_settings`).
  - `*_macros` = proc macros (`gpui_macros`, `ui_macros`, `settings_macros`).
  - `gpui_*` = platform backends (`gpui_macos`, `gpui_apple`, `gpui_linux`, `gpui_windows`, `gpui_web`, `gpui_wgpu`, `gpui_platform`).
- **Crate root files** use the crate name, not `lib.rs`: `[lib] path = "src/gpui.rs"` (`crates/gpui/Cargo.toml`). No `mod.rs` files.
- **Dependency direction** (from each `Cargo.toml`):
  - `terminal` → `gpui`, `alacritty_terminal`, `settings`, `theme`, `task`. **No** `ui` or `workspace`.
  - `terminal_view` → `terminal`, `editor`, `ui`, `workspace`, `project`.
  - `ui` → `gpui`, `theme`, `component`, `icons`, `menu`, `ui_macros`. **No** `settings`.
  - `theme` → `gpui`, `refineable`, `syntax_theme`. Settings live in a separate `theme_settings` crate. `theme` reads fonts and density through a `ThemeSettingsProvider` trait object set as a global (`crates/theme/src/theme_settings_provider.rs`), so it does not depend on `settings`.
  - `workspace` → `project`, `ui`, `theme`, `settings`, …
  - `zed` → everything; `main.rs` calls each `init`.
- **Rules for agents**: `.rules` (12 KB). `CLAUDE.md`, `AGENTS.md`, `GEMINI.md` are symlinks to it. Main points:
  - No `unwrap()`; propagate with `?`; never `let _ =` on fallible calls; use `.log_err()`.
  - Comments only explain "why".
  - Prefer existing files over many small files. No `mod.rs`.
  - Full variable names. Shadow clones before `async move` blocks.
  - In GPUI tests use `cx.background_executor().timer(..)`, not `smol::Timer`.
  - Rules should be "traps to avoid, not maps to follow"; crate-specific rules go in that crate's `.rules`.
- **Lints**: `dbg_macro`, `todo`, `redundant_clone` are `deny`; clippy `style` is allowed (`Cargo.toml` `[workspace.lints.clippy]`, ~line 1167).
- **Contributing / UI checklist**: `CONTRIBUTING.md`, `docs/src/development/ui-checklist.md`. The checklist asks for keyboard-only use, light and dark themes, clear focus indicators, and **frames under 8 ms (120 fps)**.
- **Architecture overview**: `docs/src/development.md` "Bird's-eye view".

## 2. GPUI app patterns

Best primary docs: `.rules` (GPUI section), `crates/gpui/README.md`, `crates/gpui/docs/key_dispatch.md`, `crates/gpui/docs/contexts.md`, `crates/gpui/src/_ownership_and_data_flow.rs`, 46 examples in `crates/gpui/examples/`.

**Bootstrap** (`crates/gpui/README.md`, `crates/gpui/examples/a11y.rs`):

```rust
gpui_platform::application().with_assets(Assets).run(|cx: &mut App| {
    settings::init(cx);          // each crate: pub fn init(cx: &mut App)
    terminal_view::init(cx);
    cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
    cx.open_window(WindowOptions::default(), |window, cx| cx.new(|cx| Root::new(window, cx)))?;
});
```

A crate `init` registers settings, actions and observers. Example `crates/terminal_view/src/terminal_view.rs:108`:

```rust
pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(TerminalView::deploy);
    }).detach();
}
```

**Entities and contexts** (`.rules`; `crates/gpui/src/app/context.rs`):
- `Entity<T>` is a handle; `cx.new(|cx| T::new(cx))` creates one.
- `entity.read(cx)`, `entity.update(cx, |this, cx| ...)`. Use the inner `cx`.
- `WeakEntity<T>` to break cycles; its methods return `Result`.
- `App` = root context; `Context<T>` = while updating `T`; `AsyncApp` / `AsyncWindowContext` across `.await`.
- `Window` is passed before `cx`.

**Render vs RenderOnce** (`.rules`):
- `impl Render for T` on an entity = a "view" with state. Re-renders after `cx.notify()`.
- `impl RenderOnce` + `#[derive(IntoElement)]` = stateless component built per frame (all `ui` components).
- Low level: `impl Element` for custom layout and painting (terminal grid, editor).

**Actions and keymaps** (`crates/gpui/docs/key_dispatch.md`):

```rust
actions!(terminal, [Copy, Paste]);
div().key_context("Terminal")
     .on_action(cx.listener(|this, _: &Copy, window, cx| ...))
```

Keymap JSON binds keys per context, for example `assets/keymaps/default-macos.json` (~line 1375):
`{ "context": "Terminal", "bindings": { "cmd-c": "terminal::Copy" } }`.
Actions with data use `#[derive(Action)]` / `#[gpui::action]`. Code can dispatch with `window.dispatch_action(..)`.

**Focus** (`crates/gpui/src/window.rs:713`):

```rust
pub trait Focusable { fn focus_handle(&self, cx: &App) -> FocusHandle; }
// in new(): let focus_handle = cx.focus_handle();
// in render(): div().track_focus(&self.focus_handle)
// window.focus(&handle, cx); cx.on_focus(..), cx.on_blur(..), .focus_visible(|s| ..)
```

Tab order: `window.focus_next(cx)` / `focus_prev` and `.tab_index(..)` (`examples/a11y.rs`, `examples/tab_stop.rs`).

**Globals**: `struct GlobalTheme {..}; impl Global for GlobalTheme {}`, then `cx.set_global(..)`, `cx.global::<T>()`, `cx.update_global(..)`, `cx.observe_global::<T>(..)` (`crates/theme/src/theme.rs:322`, `crates/gpui/src/app.rs:2513–2576`).

**Settings**: `trait Settings { fn from_settings(content: &SettingsContent) -> Self; }`, `#[derive(RegisterSetting)]`, read with `TerminalSettings::get_global(cx)` (`crates/settings/src/settings_store.rs:69`, `crates/terminal/src/terminal_settings.rs:21`).

**Async** (`.rules`):

```rust
cx.spawn(async move |this, cx| {             // foreground, this: WeakEntity<T>
    let data = cx.background_spawn(async move { heavy() }).await;
    this.update(cx, |this, cx| { this.data = data; cx.notify(); })
}).detach_and_log_err(cx);
```

A dropped `Task` cancels its work. Keep tasks in a field, `.await` them, or `.detach()`.

**Events and observers** (`.rules`; `context.rs:63,93,336,739`):

```rust
impl EventEmitter<Event> for Terminal {}
cx.emit(Event::Wakeup);
self._subscriptions = vec![
    cx.observe(&terminal, |_, _, cx| cx.notify()),
    cx.subscribe_in(&terminal, window, |this, _, event, window, cx| ...),
];
```

Dropping a `Subscription` removes it.

## 3. Component library

**`ui` crate** (`crates/ui/src/`):
- `components/` (≈45 components: `button/`, `label/`, `list/`, `context_menu.rs`, `tooltip.rs`, `modal.rs`, `tab.rs`, `toggle.rs`, `popover_menu.rs`, `scrollbar.rs`, `data_table/`, …).
- `styles/` (`color.rs`, `elevation.rs`, `spacing.rs`, `typography.rs`, `animation.rs`).
- `traits/` (`clickable.rs`, `disableable.rs`, `toggleable.rs`, `fixed.rs`, `styled_ext.rs`, `visible_on_hover.rs`).
- `prelude.rs` re-exports gpui basics plus common components. Every UI file does `use ui::prelude::*`.

**Component shape** (`crates/ui/src/components/button/button.rs:78`):

```rust
#[derive(IntoElement, Documented, RegisterComponent)]
pub struct Button { base: ButtonLike, label: SharedString, /* options */ }
impl Button { pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self }
impl Clickable for Button { .. }   impl Disableable for Button { .. }
impl RenderOnce for Button { fn render(self, window, cx) -> ButtonLike { .. } }
```

- `Button`, `IconButton` and `ToggleButton` wrap one shared `ButtonLike` (`button_like.rs`). It owns style variants (`ButtonStyle::{Filled, Tinted, Outlined, Subtle, ..}`) and state styles (`enabled`, `hovered`, `active`, `disabled`).
- Traits are tiny: `trait Clickable { fn on_click(self, ..) -> Self; fn cursor_style(..) -> Self; }`, `trait Disableable { fn disabled(self, bool) -> Self; }`.
- `StyledExt` adds `h_flex()`, `v_flex()`, `elevation_1/2/3(cx)` on any `Styled` (`traits/styled_ext.rs`).
- Spacing scales with UI density through `DynamicSpacing`, which `ui_macros::derive_dynamic_spacing!` generates (`crates/ui_macros/src/dynamic_spacing.rs`).

**Theme colors**: `cx.theme()` comes from the `ActiveTheme` trait on `App` (`crates/theme/src/theme.rs:148`), which reads the `GlobalTheme` global. `Theme` has `colors()` (`ThemeColors`, 143 `Hsla` fields such as `text`, `text_muted`, `border_variant`, `element_hover`), `status()`, `syntax()`, `players()`, `accents()`, `system()` (`crates/theme/src/styles/`). `ThemeColors` derives `Refineable`, so a JSON theme can override some fields only (`styles/colors.rs:13`). The `ui::Color` enum maps names to tokens (`Color::Muted => cx.theme().colors().text_muted`, `styles/color.rs:90`).

**Themes**:
- JSON families: one file holds a `"themes"` list, each with `"appearance": "dark" | "light"` and a flat `"style"` map (`"border.variant": "#363c46ff"`). See `assets/themes/one/one.json`, with schema `https://zed.dev/schema/themes/v0.2.0.json`.
- Built-in fallback in code: `crates/theme/src/fallback_themes.rs`.
- Loading: `theme_settings::init(LoadThemes::All(Box::new(Assets)), cx)` (`crates/zed/src/main.rs:670`), registry in `crates/theme/src/registry.rs`.
- Light/dark: `ThemeSelection::Dynamic { mode, light, dark }` with `mode` = light / dark / system (`crates/theme_settings/src/settings.rs:150`). System changes come through `cx.observe_window_appearance(..)` (`context.rs:441`).

**Previews / storybook**:
- `component` crate: `trait Component { fn scope(); fn description(); fn preview(window, cx) -> AnyElement; .. }` plus a registry filled through `inventory` (`crates/component/src/component.rs:21–47,170`).
- `#[derive(RegisterComponent)]` in `ui_macros` registers a type at link time (`crates/ui_macros/src/ui_macros.rs:40`).
- `example_group_with_title(..)` and `single_example(..)` build preview pages (`button.rs:557`).
- `component_preview` crate shows all components in a workspace tab. A standalone runner exists: `crates/component_preview/examples/component_preview.rs`, which calls `component::init()`.
- For Swarm: copy the idea (trait + `inventory` + a `swarm-ui` example binary), not the code (GPL).

## 4. Terminal

**Emulator**: `alacritty_terminal` from a Zed fork, pinned by git rev (`Cargo.toml:537`, `zed-industries/alacritty`). All alacritty use is in `crates/terminal/src/alacritty.rs`. It maps to Zed types (`Cell`, `Color`, `Content`, `Cursor`, `RenderableCells`) and a `TerminalBackendEvent` enum (`terminal.rs:725`). This is the seam to copy for `libghostty-vt`.

**PTY to UI thread** (`crates/terminal/src/terminal.rs`):
1. Alacritty's `EventLoop` reads the PTY on its own thread. It sends events through `ZedListener` into a `futures::mpsc::unbounded` channel (`alacritty.rs` `ZedListener`, `terminal.rs:1019,1225`).
2. `Terminal::subscribe` (`terminal.rs:1420`) runs a foreground `cx.spawn` loop:
   - handle the first event at once (low latency);
   - then collect events for 4 ms or until 100 events; many `Wakeup`s collapse into one;
   - apply the batch in one `terminal.update(..)`, then `yield_now()`.
3. `process_event` emits `Event::Wakeup`. `TerminalView` subscribes and calls `cx.notify()` (`terminal_view.rs:1213–1229`).
4. In `prepaint`, the element calls `terminal.sync(window, cx)` (`terminal_element.rs:1335`). This locks the term once and builds a `Content` snapshot (`terminal.rs:2498`).
5. Input goes back through `write_to_pty` (`PtyWrite` events, `PtySender`).

**Grid element** (`crates/terminal_view/src/terminal_element.rs`, a custom `impl Element` with `request_layout` / `prepaint` / `paint`):
- `layout_grid` (line 438) walks cells once and builds:
  - **`BatchedTextRun`** (line 97): neighbor cells with the same font, color, background, underline and strikethrough join into one string. Painted with `window.text_system().shape_line(text, size, &[style], Some(cell_width))` then `.paint(..)`. Forcing cell width keeps the grid aligned.
  - **Background regions**: per-cell backgrounds merged into larger rects (`merge_background_regions`, line 360), painted with `window.paint_quad(fill(..))`.
  - **Block elements** (`█▌░`, sextants): drawn as rects on a sub-cell grid, not as glyphs (`BLOCK_SUBCELL_*`, line ~180).
- Paint order inside `with_content_mask` (lines 1584–1731): background rects → selection and search highlights (`HighlightedRange`, rounded) → text runs → block rects → cursor (`CursorLayout::paint`).
- Origin is snapped to device pixels (`snap_px`, ~line 1590) for sharp text.
- **IME / text input**: `TerminalInputHandler` implements `InputHandler`, registered with `window.handle_input(&focus, handler, cx)` (lines 1601–1633, 1764). This is needed for dead keys, emoji and CJK input.
- The element logs counts and timings of runs and rects (~line 620). Useful for our 120 fps target.

## 5. Accessibility (critical)

**Verdict: yes.** GPUI on `main` can expose role, label, value, description, states and actions to VoiceOver today, through **AccessKit**. It merged in May 2026 and is still young. Zed itself keeps it off by default, and Zed's terminal and editor are not accessible yet.

Evidence:
- Dependencies: `accesskit 0.24`, `accesskit_macos 0.26` (`Cargo.toml:531–532`, `crates/gpui/Cargo.toml`, `crates/gpui_macos/Cargo.toml:27–28`).
- Guide: `crates/gpui/src/_accessibility.rs`. Internals: `crates/gpui/src/window/a11y.rs` (tree built during prepaint, sent as an AccessKit `TreeUpdate` each frame). Demo: `crates/gpui/examples/a11y.rs`.
- macOS adapter: `accesskit_macos::SubclassingAdapter::for_window` in `crates/gpui_macos/src/window.rs:2406–2456`. AccessKit maps to NSAccessibility, so VoiceOver sees it.
- Element API (`crates/gpui/src/elements/div.rs:1350–1520`, on `StatefulInteractiveElement`, so the element needs an `.id()`):
  - `.role(Role::Button)`, `.aria_label(..)`, `.aria_description(..)`, `.aria_value(..)`, `.aria_placeholder(..)`
  - `.aria_selected`, `.aria_expanded`, `.aria_toggled`, `.aria_numeric_value` (+ min, max, step), `.aria_level`, `.aria_position_in_set`, `.aria_size_of_set`, `.aria_orientation`, `.aria_keyshortcuts`
  - `.aria_active_descendant()` for menus and lists; `.accessibility_id(..)` maps to `AXIdentifier` (useful for UI tests)
  - `.on_a11y_action(AccessibleAction::Increment, ..)`. `.on_click` adds the `Click` action for free.
  - `.a11y_synthetic_children(..)` adds nodes that are not elements (text runs).
- Custom elements: `Element::a11y_role()`, `write_a11y_info(&self, node: &mut accesskit::Node)`, `a11y_synthetic_children(..)` (`crates/gpui/src/element.rs:113–137`). `write_a11y_info` gives full `accesskit::Node` access, so `set_live(..)` (live regions) is reachable even without a GPUI builder. `Role::Terminal` exists in accesskit 0.24 (docs.rs).
- Rules: a node needs both an id **and** a role. Ids must be unique per frame and stable across frames. The `text!` macro derives ids from its source location, so text built in a loop needs `.with_id(index)` (`_accessibility.rs`).
- Opt out: `Application::new_inaccessible(..)` (`crates/gpui/src/app.rs:224`). Testing: `window.debug_a11y_tree_json()` and `is_a11y_active()` (`window.rs:~7100`), `TestAppContext::disable_accessibility` (`app/test_context.rs:196`).
- Zed use: `crates/zed/src/main.rs:87–93` enables it only with `ZED_EXPERIMENTAL_A11Y=1`. Components that already use it: `ui/.../button_like.rs:774` (`Role::Button` by default plus aria fields), `context_menu.rs`, `list_item.rs`, `toggle.rs`, `dropdown_menu.rs`, `tree_view_item.rs`, `settings_ui`. **Not found** in `terminal_view` or the editor element: no `a11y_role` or `.role(` there.
- No GPUI API for live-region announcements ("aria-live") or explicit VoiceOver announcements: **not found** in `div.rs` or `a11y.rs`.

GitHub history (`gh search`, zed-industries/zed):
- #56065 "gpui: Accesskit support", merged 2026-05-27 (replaces #51097). First step for tracking issue **#41138** "Windows: Screen reader accessibility missing completely" (open, label `area:accessibility`).
- Then: #57954 `Application::inaccessible()`, #59429 a11y in Settings UI, #60397 landmarks and menus, #61926 accessibility identifiers, #64143 macOS adapter release fix (all merged).
- Open: #65740 "Roll back accessibility nodes when a prepaint transaction fails". It fixes a debug panic (`Duplicate a11y node id`) and dropped nodes when a `list` re-prepaints for autoscroll. This matters if Swarm uses `list`/`uniform_list` with roles. Open: #65186 crash closing windows on Touch Bar Macs (a11y adapter related).
- No open PR for terminal or editor accessibility found.

**Options for Swarm**

| Option | What | Effort |
|---|---|---|
| A. Use the built-in API (do this) | Every `swarm-ui` component sets role, label and states, like `ButtonLike`. Add a11y checks to the preview app. Run with VoiceOver. | Low: ~1 day for the base, then a few lines per component. |
| B. Accessible terminal | `swarm-term` element returns `Role::Terminal`. Add synthetic `TextRun` children for visible lines (or the last N lines) and cursor or selection through `A11ySubtreeBuilder`. Optionally a `Live` region for agent status. | Medium: ~3–5 days, plus tuning so it does not flood VoiceOver at 120 fps (update only changed lines, stable ids per row). |
| C. Keyboard-first design | Every action has a key binding and a key context. Visible focus (`focus_visible`). Tab order. Command palette. | Low; needed anyway (Zed's UI checklist asks for it). |
| D. Agent status announcements | Not in GPUI. Use `write_a11y_info` + `set_live` on a status element. If that is not enough, post `NSAccessibilityAnnouncementRequested` through a small `objc2` call. | Low to medium: ~1–2 days. Unverified that AccessKit's macOS adapter turns live regions into VoiceOver speech. |
| E. Own NSAccessibility bridge | Bypass AccessKit. | High (weeks). Not needed now that AccessKit is in GPUI. |

Risks: the API is new and changes often. Pin a gpui rev and watch #65740 and follow-up PRs. Zed does not dogfood it by default yet, so expect bugs. Report them upstream.

## 6. Versioning

- **crates.io is stale**: `gpui` 0.2.2, published 2025-10-22. `crates/gpui/Cargo.toml` still says `version = "0.2.2"`, `publish = true`. `gpui_platform` is **not** on crates.io (crates.io API returned nothing) and is `publish.workspace = true` → `false`. The README's `version = "*"` lines therefore do not work for `gpui_platform` from crates.io.
- **Use a git rev pin**:

  ```toml
  [workspace.dependencies]
  gpui = { git = "https://github.com/zed-industries/zed", rev = "70b1b91b73ee12930b4e3375d68657e9591b1921" }
  gpui_platform = { git = "https://github.com/zed-industries/zed", rev = "70b1b91b73ee12930b4e3375d68657e9591b1921", features = ["font-kit"] }
  ```

  Start with `gpui_platform::application()`. On macOS, `font-kit` is required, or text has no glyphs (`crates/gpui/README.md`).
- **`gpui_platform` split**: `gpui` is platform-free. `gpui_platform` picks `gpui_macos` / `gpui_linux` / `gpui_windows` / `gpui_web` (`crates/gpui_platform/src/gpui_platform.rs`). `gpui_macos` uses `gpui_apple` for Metal.
- **Patches do not carry over**: Zed's root `[patch.crates-io]` (`Cargo.toml:1036`) swaps `async-task`, `calloop`, `notify` and others for Zed forks. Cargo ignores a dependency's patches. If the build or runtime differs, copy the `async-task` patch into Swarm's root `Cargo.toml`. Not verified whether it is required.
- **macOS needs**: full Xcode (not only the CLI tools). `gpui_apple/build.rs` runs `xcrun -sdk macosx metal` and `metallib` to compile `shaders.metal`. Use the `runtime_shaders` feature to compile shaders at runtime instead. Commands: `xcode-select --install`, `sudo xcode-select --switch /Applications/Xcode.app/Contents/Developer` (`crates/gpui/README.md`, `docs/src/development/macos.md`). Zed release bundles need the macOS 26 SDK / Xcode 26 and run on macOS 12+.
- **Toolchain** (`rust-toolchain.toml`): `channel = "1.98.1"`, components `rustfmt`, `clippy`, `rust-analyzer`, `rust-src`. Swarm should pin the same channel as its gpui rev, and bump both together.
- **Upgrades**: GPUI is pre-1.0 with frequent breaking changes (README). Bump the rev on purpose and check `[GPUI]` release-note lines in Zed PRs (`.rules` requires them for gpui changes).
