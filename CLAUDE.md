@AGENTS.md

## Claude Code

Claude Code does not auto-load `AGENTS.md`. The import above is the project
rules. This file only adds Claude-specific paths:

- Docs-freshness hook: `.claude/settings.json` → `scripts/claudecode_docs_hook.sh`
  (`PostToolUse` / `Stop` / `SubagentStop`)
- Slash commands: `.claude/commands/` (`/sync-api-docs`,
  `/sync-websocket-events`, `/sync-all-docs`)
- Machine-local overrides: `.claude/settings.local.json` (gitignored)
