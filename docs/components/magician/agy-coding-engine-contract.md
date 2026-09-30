# Antigravity (`agy`) coding-engine contract

Design (archived):
Claude Code and Antigravity.
Antigravity is a live VibeDev coding option next to Pi, Codex, Grok, and
Claude Code when `coding.agy.enabled` is on, exactly one reviewed `agy`
binary is found, the CLI version meets `1.1.19`, Antigravity OAuth is
signed in (Magician's Gemini/Google keys do **not** count unless
`coding.agy.use_api_key` is true), and a background isolation
receipt matches the current identity + version.

Companion: [Claude Code coding contract](claude-coding-engine-contract.md).

## Frozen names

| Thing | Value |
| --- | --- |
| Rust kind | `CodingEngineKind::AgyCli` |
| Wire / `engine_str` | `agy_cli` |
| Profile id | `agy-default` |
| Picker label | `Antigravity` |
| Config block | `coding.agy` |
| Kill switch | `coding.agy.enabled` (template default **false**; repo-root / live seed **true**) |
| Operator binary | `coding.agy.binary` (optional; browser/model/tool cannot set it) |
| Native continuation | Agy `conversation_id` UUID on `CodingContinuationRef.native_session_id` |
| Magician session id | `agy-{uuid}` (must not leak the native UUID into proposal raw) |
| Billing V1 | `CodingBillingBasis::External` |
| Minimum CLI | `1.1.19` (`agy --version`; newer is compatible) |
| Resume flag | `--conversation <id>` — **never** `--continue` / `-c` |

Pi remains the configured and UI default. Auto stays opt-in.

Frozen against installed `agy` 1.1.19. Older binaries without
`--output-format stream-json` are `incompatible`, not "parse stdout text".

## Launch argv (A1)

Prompt is a single `-p` argv. Magician never uses `sh -c`. stdin is
`/dev/null` for print turns (open stdin historically hangs). `cwd` is the
Magician shadow workspace. Spawn requires `require_outer_fence()`. Piped
(non-TTY) stdout emits NDJSON.

Build:

```text
agy
  -p <prompt>
  --output-format stream-json
  --sandbox
  --disable-slash-commands
  --dangerously-skip-permissions
```

Discuss (`plan_only` / `AgyTurnMode::Discuss`): `--mode plan` and
**without** `--dangerously-skip-permissions`. Init then reports
`permission_mode: request-review`. Build skip-permissions reports
`always-proceed`.

Resume: `--conversation <uuid>` when `resume_session_id` is set. Never
`--continue` (that is "most recent", not Magician's id).

Transport is NDJSON (`event`: `init` / `step_update` / `result`), not
JSON-RPC and not ACP.

## Auth

A logged-in host is detected from a non-empty Antigravity OAuth token
file under the operator home: `~/.gemini/jetski-standalone-oauth-token`
(Agy 1.2.x; the only token name in the 1.2.9 binary) or the 1.1.19
`~/.gemini/antigravity-cli/antigravity-oauth-token`; both paths must be
checked. Magician's `GEMINI_API_KEY` /
`GOOGLE_API_KEY` do **not** count unless `coding.agy.use_api_key` is
true. Public reason is `sign in required`. Snapshots never name files,
home, or env keys.

`coding.agy.use_api_key` default **false**. Only when true may Gemini /
Google credential env vars be copied from the parent allowlist.

Child env is `env_clear` then rebuilt from `PATH`, `HOME`, `USER`,
`TMPDIR`, `LANG`, `TERM`, plus `GIT_CEILING_DIRECTORIES=scope_root`.
`GOOGLE_CLOUD_PROJECT` / `VERTEX_LOCATION` / `CLOUD_ML_PROJECT` may be
copied if already present. `GEMINI_API_KEY`, `GOOGLE_API_KEY`, and
`GOOGLE_APPLICATION_CREDENTIALS` are copied only when
`use_api_key` is true. Every `MAGICIAN_*` variable is dropped. Do not
invent `AGY_HOME`. Do not relocate Antigravity config home.

Reviewed binary locations (in addition to `PATH`):

- `/opt/homebrew/bin/agy`
- `/usr/local/bin/agy`
- `~/.local/bin/agy`

## Ready rule

Ready requires all of:

1. `coding.agy.enabled`
2. exactly one resolved binary
3. cached `agy --version` ≥ `1.1.19` (background tick only; 5s)
4. auth: non-empty OAuth token (default). Magician's Gemini/Google keys
   do not count unless `coding.agy.use_api_key` is true
5. a cached isolation receipt for this identity **and** version (`init`
   advertises a non-empty `tools` list and a `permission_mode`). Agy
   identity is the canonical path plus mtime and length so an in-place
   CLI replace cannot inherit a Ready receipt. That is a filesystem
   stat; it never spawns `agy --version` on the request path.

Agy always advertises `search_web` / `call_mcp_tool` / subagent tools in
`init.tools`. Catalog presence is **not** Ready-incompatible. Runtime
use of those tools on a `step_update` fails closed.

Public snapshot JSON omits paths, home, emails, and env names.
Auth-required reason is `sign in required`. Missing lists stay
`unqualified`. A cwd MCP canary appearing in init is `incompatible`.

Refresh: `POST /api/magician/v2/coding/engines/agy_cli/refresh` returns
202 `checking` and observes filesystem + caches only (never
`agy --version`, never a live `-p` probe).

## Isolation probe (A3)

The qualify worker must not start a billed model turn every 30s. After
version + auth would otherwise be Ready it launches
`qualify_launch_args()` (`--input-format stream-json` **without** `-p`
and without a user JSONL event) in a disposable cwd that holds a
uniquely named `.mcp.json` canary. Init arrives without a model turn.
The worker drains `event: init`, then kills the process group **before**
dropping stdin. Cancellation / Drop also SIGKILLs the process group
before stdin is closed (`kill_on_drop` is not enough). Isolation
attempts back off 5 minutes per identity.
Probe conversation ids are not journaled. Receipt TTL is 6 hours.
Version is part of identity: a new CLI cannot inherit a Ready receipt.

A live turn that emits `step_update` with `search_web`,
`read_url_content`, `call_mcp_tool`, subagent, `browser_*`,
`generate_image`, or `send_message` still fails closed.

Cockpit Stop registers `CodingControlHandle::Agy` and interrupts the
NDJSON drain; Drop kills the process group. Same-engine follow-up binds
`--conversation <uuid>` from the chain continuation. If `--conversation`
never emits `init`, Magician records `continuation_lost` and starts a
fresh `-p` once. Stop/timeout before init is **not** continuation-lost.

`agy_binary` in tool args is rejected the same way as `claude_binary` /
`grok_binary`.

## Usage (A5)

Agy `result.usage` is token-shaped (`input_tokens`, `output_tokens`,
`thinking_tokens`, `cache_read_tokens`, `total_tokens`) and has **no
USD**. Map tokens; fold `thinking_tokens` into metered output so the
execution cap cannot fail open; `cost_known: false`; never `$0.00`.
`billing_basis: external`.

Same-engine `--conversation` that never emits `init` is
`continuation_lost` only for protocol/missing-result failures. Stop,
wall-clock timeout, and no-progress idle **do not** retry a fresh `-p`.
