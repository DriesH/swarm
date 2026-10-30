# Append agent prompts, never replace them

Swarm adds its instructions with `--append-system-prompt-file` (Claude Code) and `developer_instructions` (Codex). It never uses `--system-prompt` or `--bare`. This keeps the user's `CLAUDE.md`, `AGENTS.md`, rules, skills, hooks and MCP servers working inside every agent.
