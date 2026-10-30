# Swarm

Native macOS app (Rust + GPUI) that runs a swarm of Claude Code and Codex agents in terminals.

- Plan: `docs/PLAN.md`. Phases and "done when": `docs/ROADMAP.md`
- Code conventions: `docs/CONVENTIONS.md`. Follow them in every Rust change.
- Terms: `GLOSSARY.md`. Use its words in code, UI copy and docs.
- Decisions: `docs/adr/`. Read before changing architecture.
- Design reference: direction A on the canvas linked in `docs/PLAN.md`.

## Rules

- Build screens from `swarm_ui` components and theme tokens; add a component when one is missing.
- Accessibility is required: every interactive element gets a role and label, is reachable by keyboard, and meets WCAG AA contrast.
- UI text shows labels, values and status only. Explanations of how something works belong on the docs website.
- Follow Zed's GPUI patterns: `docs/research/zed-architecture.md`.
- Spikes in `spikes/` are throwaway. Record the result in `docs/spikes/<name>.md`.
