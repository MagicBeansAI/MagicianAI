# AI Docs Hook Setup

This project supports reminder hooks for Claude Code, Codex, Grok, Gemini
CLI, Antigravity (`agy`), and ZCode so agents prompt for docs updates when
code changes are detected. Project rules live in `AGENTS.md`; Claude imports
that file from `CLAUDE.md`, and Gemini CLI loads it via
`.gemini/settings.json` `context.fileName`.

## Claude Code Hook

Claude hook config is repo-managed in:

- [`.claude/settings.json`](../../.claude/settings.json)

Reference template:

- [claude-settings.docs-hook.example.json](claude-settings.docs-hook.example.json)

If you need machine-specific behavior, keep it in `.claude/settings.local.json`.

Claude hook config uses:

- `PostToolUse` for `Write|Edit|MultiEdit`
- `Stop`
- `SubagentStop`

All three invoke:

- [`scripts/claudecode_docs_hook.sh`](../../scripts/claudecode_docs_hook.sh)

The hook runs:

```bash
python3 scripts/docs_guard.py --working-tree --remind-only --source claudecode
```

It does not block the agent; it prints a reminder prompt when docs likely need updates.

The blocking `docs_guard.py` path (pre-commit and CI, not `--remind-only`)
also runs `scripts/skillshub_runtime_root_linter.py` and
`scripts/check_typed_storage_boundaries.py`. Those are source ratchets, not
doc-pairing reminders.

`.githooks/pre-commit` runs `scripts/signing_guard.sh` before the docs guard.
It refuses a staged `*.pbxproj` hunk that adds a non-empty
`DEVELOPMENT_TEAM`: the Apple team lives in the gitignored
`magios/Signing.local.xcconfig`, and Xcode's Signing tab would otherwise
commit one operator's team for every clone (`SIGNING_GUARD_DISABLE=1`
bypasses it once). Details: `docs/components/scripts/README.md`.

## Codex Hook (`codexhook`)

You can use either:

1. Wrapper command: [`scripts/codexhook`](../../scripts/codexhook)

```bash
scripts/codexhook
```

This launches `codex` and runs a docs reminder pre-flight check.

2. Codex notify hook (continuous reminders after notifications):

Point `notify` to:

- [`scripts/codexhook_docs_notify.sh`](../../scripts/codexhook_docs_notify.sh)

Add this to `~/.codex/config.toml`:

```toml
# Codex keys this table by the clone's absolute path — substitute yours.
[projects."/absolute/path/to/magician"]
notify = ["bash", "-lc", "cd /absolute/path/to/magician && scripts/codexhook_docs_notify.sh"]
```

That command runs:

```bash
python3 scripts/docs_guard.py --working-tree --remind-only --source codexhook
```

## Grok Hook

Grok hook config is repo-managed in:

- [`.grok/hooks/docs-remind.json`](../../.grok/hooks/docs-remind.json)

It uses `PostToolUse` (writes/edits), `Stop`, and `SubagentStop`. All three
invoke:

- [`scripts/grok_docs_hook.sh`](../../scripts/grok_docs_hook.sh)

The hook runs:

```bash
python3 scripts/docs_guard.py --working-tree --remind-only --source grok
```

It does not block the agent. Grok project hooks are skipped until the folder
is trusted (`/hooks-trust` or `grok --trust`). Claude-compat scanning of
`.claude/settings.json` can also fire the Claude reminder in a Grok session;
this file is the Grok-native path so Grok does not depend on that compat
layer.

## Gemini CLI Hook

Gemini CLI hook config is repo-managed in:

- [`.gemini/settings.json`](../../.gemini/settings.json)

`AfterTool` (`write_file|replace`) and `AfterAgent` invoke:

- [`scripts/docs_remind_hook.sh`](../../scripts/docs_remind_hook.sh) `gemini`

Gemini CLI loads `AGENTS.md` through `context.fileName` in the same file.
Project hooks are fingerprinted; a command change is treated as a new
untrusted hook until you accept it.

## Antigravity (`agy`) Hook

Antigravity does not read `.gemini/settings.json` MCP or hooks.

- Hooks: [`.agents/hooks.json`](../../.agents/hooks.json)
- MCP: [`.agents/mcp_config.json`](../../.agents/mcp_config.json)

`PostToolUse` (`write_to_file|replace_file_content|multi_replace_file_content`)
and `Stop` invoke `scripts/docs_remind_hook.sh agy`. That script prints `{}`
on stdout (agy requires JSON) and sends the reminder to stderr. `agy` also
loads `AGENTS.md` as workspace context.

## ZCode Hook

ZCode hook config is repo-managed in:

- [`.zcode/config.json`](../../.zcode/config.json)

`hooks.enabled` is true. `PostToolUse` (`Write|Edit`) and `Stop` invoke
`scripts/docs_remind_hook.sh zcode`. ZCode reads workspace `AGENTS.md` as
project instructions. Per-user session plans under `.zcode/` stay gitignored.

## Manual reminder

```bash
make docs-remind
```

runs `python3 scripts/docs_guard.py --working-tree --remind-only --source make`.
It never fails the build; the blocking gate is still the pre-commit/CI
`--staged` path.

## Disable Switch

Set `DOCS_HOOK_DISABLE=1` to disable the Claude, Codex, Grok, Gemini, agy,
and ZCode reminder hooks temporarily.

## Prerequisites Script

`scripts/install-prerequisites.sh` installs all required tools for local development. It also installs `cloudflared` for Cloudflare Tunnel–based external access (the Kapso webhook + optional dev UI; opt-in via `MAGICIAN_ENABLE_FUNNEL=1`, see `scripts/ensure-magician-tunnel.sh`). The previous Tailscale funnel was retired in favour of a stable cloudflared named tunnel on the operator's own zone.

## iOS Test Artifacts

`make test-ios` runs its temporary work under
`$(CARGO_TARGET_DIR)/ios-tests` (the Makefile's build dir: an external build
volume when one is mounted, else `./target`). The direct UI and realtime local
transcript targets also keep their Xcode derived data on the configured build
location. Keep these paths on the configured Cargo target location so simulator test artifacts do not
consume the system temporary volume.

## Marketing Pages Packaging

- `make build-marketing-site` builds the static unified-UI payload into
  `marketing-site/`, removes the offline wake model, installs the Cloudflare
  Pages `_headers`, supplies a real top-level `404.html`, and materializes the
  legacy service-worker retirement script under each historic worker filename.
- `make publish-marketing-site` runs that build, publishes the
  `magician-marketing` Pages project, and attaches `next.magican.ai` as its sole
  custom marketing domain.
- The expired Unified UI `0.0.774` `Clear-Site-Data` recovery header is gone.
  The worker tombstones, top-level 404, no-store HTML shell, and immutable
  content-hashed assets remain as the permanent cache-safety contract.

## Event Taxonomy Codegen + Drift Gate

The Rust event taxonomy (`magician/src/magician_v2/realtime_events.rs::taxonomies!` macro + the `AGUI_EVENT_TAXONOMY` const) is the single source of truth for category/severity/user-relevant per event type. The UI mirror at `ui/unified-ui/src/lib/realtime/event-taxonomy.ts` is **generated**, never hand-edited.

### Targets

- **`make event-taxonomy-codegen`** — runs `cargo run -p magician-event-taxonomy --bin event_taxonomy_dump`, writes the regenerated TS to `ui/unified-ui/src/lib/realtime/event-taxonomy.ts`, then calls `scripts/codegen_drift.py stamp --label event-taxonomy` to embed a `// SOURCE_HASH: <sha256>` marker over the Rust source files. The generator lives in a thin workspace crate so it avoids compiling the full `magician` service. Run **manually** after editing `realtime_events.rs`, `magician-event-taxonomy/src/lib.rs`, or `magician-event-taxonomy/src/bin/event_taxonomy_dump.rs`.
- **`make event-taxonomy-check`** — runs `scripts/codegen_drift.py check --label event-taxonomy`. Pure file-IO (~10ms): recomputes the hash and compares it to the embedded marker. Wired as a prerequisite to `make test`, `make build-all-debug`, and `make build-all-release` so a forgotten codegen fails the build with a clear remediation message instead of letting the TS mirror silently diverge.

### `scripts/event_taxonomy_check.py`

The checker is now the shared `scripts/codegen_drift.py` drift gate. Two subcommands:

- `python3 scripts/codegen_drift.py stamp --label <label> --regen <cmd> --output <ts-file> <rust-source>...` — compute SHA-256 over the listed Rust files and embed it at the top of the TS file as a `// SOURCE_HASH: <hex>` line.
- `python3 scripts/codegen_drift.py check --label <label> --regen <cmd> --output <ts-file> <rust-source>...` — recompute, read the marker, exit non-zero if they differ.

Hashing the entire Rust source files (not just the macro body) is intentional — over-triggers occasionally (an unrelated comment edit forces a re-codegen) but never under-triggers, which is the only direction that matters for correctness.

### Operator workflow

1. Edit `realtime_events.rs` (add a variant, change a taxonomy row).
2. `make build-all-debug` → fails with the `event-taxonomy` drift gate flagging stale `event-taxonomy.ts`.
3. `make event-taxonomy-codegen` → regenerates the TS mirror with a fresh hash.
4. Re-run the build → passes. Commit both the Rust change and the regenerated TS file together.
