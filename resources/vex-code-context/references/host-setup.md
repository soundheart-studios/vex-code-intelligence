# vex reference: host setup

Load this when installing, upgrading, or diagnosing agent wiring.

- Run `vex init --agent claude` or `vex init --agent codex` in the repository
  (repeat `--agent`, or pass `--agent all`, for both). Without `--agent`,
  `vex init` only registers and indexes the project.
- Claude Code reads the `vex` entry in `.mcp.json` and this skill under
  `.claude/skills/`; Codex reads the fenced `[mcp_servers.vex]` block in
  `.codex/config.toml` and this skill under `.agents/skills/`. Both start bare
  `vex mcp` from the repository, so `vex` must be on PATH.
- vex owns only its named MCP entry, its fenced Codex block and this skill
  directory. Other servers and settings survive every re-run.
