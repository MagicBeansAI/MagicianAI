# Antigravity (`agy`) coding-engine contract

Design (archived):
Claude Code and Antigravity.
Companion: [Claude Code coding contract](claude-coding-engine-contract.md).

Antigravity is a live VibeDev coding option next to Pi, Codex, Grok and Claude
Code once it is Ready (see [Ready rule](#ready-rule)). Pi remains the
configured and UI default; Auto stays opt-in.

## Frozen names

| Thing | Value |
| --- | --- |
| Rust kind | `CodingEngineKind::AgyCli` |
| Wire / `engine_str` | `agy_cli` |
| Profile id | `agy-default` |
| Picker label | `Antigravity` |
| Config block | `coding.agy` |
| Kill switch | `coding.agy.enabled` (code default **false**; repo-root seed `magician-config.yaml` **true**) |
| Operator binary | `coding.agy.binary` (optional; browser/model/tool cannot set it) |
| Native continuation | Agy `conversation_id` UUID on `CodingContinuationRef.native_session_id` |
| Magician session id | `agy-{uuid}` (must not leak the native UUID into proposal raw) |
| Billing V1 | `CodingBillingBasis::External` |
| Minimum CLI | `1.1.19` (`agy --version`; newer is compatible) |
| Resume flag | `--conversation <id>`, **never** `--continue` / `-c` |

Frozen against installed `agy` 1.1.19. Older binaries without
`--output-format stream-json` are `incompatible`; Magician does not fall back
to parsing stdout text.

## Launch argv

The prompt is a single `-p` argv; Magician never uses `sh -c`. stdin is
`/dev/null` for print turns (open stdin can hang). `cwd` is the Magician
shadow workspace. Spawn requires `require_outer_fence()`. Piped (non-TTY)
stdout emits NDJSON.

Build:

```text
agy
  -p <prompt>
  --output-format stream-json
  --sandbox
  --disable-slash-commands
  --dangerously-skip-permissions
```

Discuss (`plan_only` / `AgyTurnMode::Discuss`) adds `--mode plan` and drops
`--dangerously-skip-permissions`; init then reports
`permission_mode: request-review`. Build reports `always-proceed`.

Resume: `--conversation <uuid>` when `resume_session_id` is set. Never
`--continue` (that means "most recent", not Magician's id).

Transport is NDJSON (`event`: `init` / `step_update` / `result`), not JSON-RPC
and not ACP.

## Trusted project posture

For a trusted chain (`RepoConfigPosture::Project`),
`agy_contract::launch_args_with` drops `--disable-slash-commands` so the
repo's own commands load. MCP stays as the isolation probe attests: Antigravity
reports no per-project MCP source Magician can scope, so admitting servers
would admit the user's. `request.build_env` (`CARGO_TARGET_DIR`) is applied
after the env allowlist.

## Auth

A logged-in host is detected from a non-empty Antigravity OAuth token file
under the operator home. Check both paths:
`~/.gemini/jetski-standalone-oauth-token` (Agy 1.2.x; the only token name in
the 1.2.9 binary) and the 1.1.19
`~/.gemini/antigravity-cli/antigravity-oauth-token`. Magician's
`GEMINI_API_KEY` / `GOOGLE_API_KEY` do **not** count unless
`coding.agy.use_api_key` (default **false**) is true. The public reason is
`sign in required`; snapshots never name files, home or env keys.

Child env is `env_clear`, then rebuilt from `PATH`, `HOME`, `USER`, `TMPDIR`,
`LANG`, `TERM`, plus `GIT_CEILING_DIRECTORIES=scope_root`.
`GOOGLE_CLOUD_PROJECT` / `VERTEX_LOCATION` / `CLOUD_ML_PROJECT` are copied if
present. `GEMINI_API_KEY`, `GOOGLE_API_KEY` and
`GOOGLE_APPLICATION_CREDENTIALS` are copied only when `use_api_key` is true.
Every `MAGICIAN_*` variable is dropped. Magician does not invent `AGY_HOME` or
relocate Antigravity's config home.

Reviewed binary locations (in addition to `PATH`):

- `/opt/homebrew/bin/agy`
- `/usr/local/bin/agy`
- `~/.local/bin/agy`

## Ready rule

Ready requires all of:

1. `coding.agy.enabled`;
2. exactly one resolved binary;
3. cached `agy --version` ≥ `1.1.19` (background tick only; 5s);
4. auth: a non-empty OAuth token (or `coding.agy.use_api_key`, as above);
5. a cached isolation receipt for this identity **and** version (`init`
   advertises a non-empty `tools` list and a `permission_mode`). Agy identity
   is the canonical path plus mtime and length, so an in-place CLI replace
   cannot inherit a Ready receipt. That is a filesystem stat; it never spawns
   `agy --version` on the request path.

Agy always advertises `search_web` / `call_mcp_tool` / subagent tools in
`init.tools`. Their presence in the catalog does not block Ready; using them
at runtime fails closed (see below).

Public snapshot JSON omits paths, home, emails and env names. Missing lists
stay `unqualified`. A cwd MCP canary appearing in init is `incompatible`.

Refresh: `POST /api/magician/v2/coding/engines/agy_cli/refresh` returns 202
`checking` and reads filesystem and caches only (never `agy --version`, never a
live `-p` probe).

## Isolation probe

The qualify worker must not start a billed model turn every 30s. Once version
and auth would otherwise be Ready, it launches `qualify_launch_args()`
(`--input-format stream-json` **without** `-p` and without a user JSONL event)
in a disposable cwd holding a uniquely named `.mcp.json` canary. Init arrives
without a model turn. The worker drains `event: init`, then kills the process
group **before** dropping stdin; cancellation and Drop also SIGKILL the process
group before stdin closes (`kill_on_drop` is not enough). Isolation attempts
back off 5 minutes per identity. Probe conversation ids are not journaled.
Receipt TTL is 6 hours, and version is part of identity.

A live turn that emits `step_update` with `search_web`, `read_url_content`,
`call_mcp_tool`, `invoke_subagent` / `define_subagent` / `manage_subagents`,
`generate_image`, `send_message`, any `browser_*` name or any name containing
`mcp` fails closed (`AGY_FORBIDDEN_RUNTIME_TOOLS`, `agy_contract.rs`).

Cockpit Stop registers `CodingControlHandle::Agy` and interrupts the NDJSON
drain; Drop kills the process group. A same-engine follow-up binds
`--conversation <uuid>` from the chain continuation. If `--conversation` never
emits `init` because of a protocol or missing-result failure, Magician records
`continuation_lost` and starts a fresh `-p` once. Stop, wall-clock timeout and
no-progress idle (including before init) are **not** continuation-lost and do
not retry a fresh `-p`.

`agy_binary` in tool args is rejected like `claude_binary` / `grok_binary`.

## Usage

Agy `result.usage` is token-shaped (`input_tokens`, `output_tokens`,
`thinking_tokens`, `cache_read_tokens`, `total_tokens`) with **no USD**.
Magician maps tokens and folds `thinking_tokens` into metered output so the
execution cap cannot fail open; `cost_known: false`, never `$0.00`;
`billing_basis: external`.
