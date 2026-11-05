<p align="center">
  <img src="assets/readme/bee.svg" width="104" height="104" alt="">
</p>

<h1 align="center">Swarm</h1>

<p align="center">
  A native macOS app that runs a swarm of Claude Code and Codex agents in terminals.
</p>

<p align="center">
  <a href="https://github.com/DriesH/swarm/releases/latest/download/Swarm.dmg">Download</a> ·
  <a href="https://driesh.github.io/swarm/">Website</a> ·
  <a href="https://driesh.github.io/swarm/docs/">Docs</a> ·
  <a href="CHANGELOG.md">Changelog</a>
</p>

## What it does

Each task gets its own git worktree and an orchestrator agent. The orchestrator splits the work and starts worker agents, each in a real terminal, and you watch them all in one window. When the work is done, you review the combined diff and open a pull request.

## Requirements

- A Mac with Apple silicon and macOS 13 or later
- Git
- Claude Code, Codex, or both
- Optional: the GitHub CLI (`gh`), for pull requests

## Install

Download `Swarm.dmg` from the [latest release](https://github.com/DriesH/swarm/releases/latest), open it and move Swarm to Applications. Swarm updates itself from GitHub Releases.

## Build from source

Swarm is written in Rust with [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui).

```sh
cargo build -p swarm_daemon && cargo run -p swarm_app
```

The app starts the daemon from `target/debug`. More in [`crates/swarm_app/README.md`](crates/swarm_app/README.md).

## Repository

| Folder | What is in it |
|---|---|
| `crates/` | The app, the daemon, the CLI and their libraries |
| `docs/` | Plan, roadmap, decisions (ADRs) and specs |
| `docs-site/` | The docs website (Starlight) |
| `site/` | The product website |
| `script/` | Bundle, sign and release scripts |
