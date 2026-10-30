# Code conventions

Rules for Rust code in this repo. Formatting that `rustfmt` and Clippy enforce is not repeated here: see `rustfmt.toml`, `clippy.toml` and `[workspace.lints]` in `Cargo.toml`.

## Blank lines inside functions

Split a function body into small groups of related lines, with one blank line between groups. A reader should see the steps of the function at a glance.

- Put a blank line between each step: reading inputs, computing a value, building the result.
- Keep lines that belong together in one group, for example a value and the line that adjusts it.
- Put a blank line before and after a multi-line `if`, `match`, `let … else` or loop that computes one value.
- Put a blank line before the final expression or `return` when the function has more than one group.
- Never more than one blank line in a row.

```rust
fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
    let colors = cx.theme().colors();

    let ring: Option<Hsla> = if self.focused {
        Some(colors.accent)
    } else if self.attention {
        Some(colors.warn)
    } else {
        None
    };

    let (line, panel, term) = (colors.line, colors.panel, colors.term);
    let label = self
        .accessible_label
        .unwrap_or_else(|| SharedString::from(format!("{} terminal", self.name)));

    let icon_color = if self.icon == IconName::Terminal || self.icon == IconName::User {
        Color::Muted
    } else {
        Color::Default
    };

    // …
}
```

## Errors

- No `unwrap()`. Use `?`, or handle the error where it happens.
- No `let _ =` on a `Result`. Log it or handle it.

## Comments

- Comment only to explain why, never what. Keep comments to one or two short lines.

## Files and modules

- A crate's root file is named after the crate (`src/swarm_ui.rs`), set with `[lib] path`. No `mod.rs` files.
- Prefer adding to an existing file over creating many small ones.

## Naming

- Use the words from `GLOSSARY.md` in code, UI text and docs.
- Use full variable names (`terminal`, not `t`).

## UI code

- Build screens from `swarm_ui` components and theme tokens. Never use raw colors.
- Every interactive element has an accessibility role and label, is reachable by keyboard, and shows a focus ring.
- UI text shows labels, values and status only.
