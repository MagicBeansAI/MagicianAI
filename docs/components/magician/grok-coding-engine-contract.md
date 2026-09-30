# Grok Build CLI coding-engine contract

Plan: Grok Build CLI plan.
Grok is a VibeDev coding option next to Pi, Codex, Claude Code, and
Antigravity: honest readiness (version + auth, coalesced refresh), attested ACP
isolation before the profile is selectable, cockpit Stop, same-engine
`session/load` resume, a generation-fenced follow-up FIFO, and honest usage
mapping (unknown cost is not `$0.00`).

Companion: [Pi coding-engine contract](pi-coding-engine-contract.md).
Siblings: [Claude Code coding contract](claude-coding-engine-contract.md),
[Antigravity coding contract](agy-coding-engine-contract.md).

## Frozen names

| Thing | Value |
| --- | --- |
| Rust kind | `CodingEngineKind::GrokAcp` |
| Wire / `engine_str` | `grok_acp` |
| Profile id | `grok-default` |
| Picker label | `Grok` |
| Config block | `coding.grok` |
| Kill switch | `coding.grok.enabled` (schema default **false** when the key is absent; repo-root seed and live config **true**) |
| Operator binary | `coding.grok.binary` (optional; browser/model/tool cannot set it) |
| Client info | `{ "name": "magician", "title": "Magician", "version": CARGO_PKG_VERSION }` |
| Native continuation | ACP `sessionId` on `CodingContinuationRef.native_session_id` |
| Billing V1 | `CodingBillingBasis::External` |

Pi remains the configured and UI default. Auto stays opt-in.

## Launch argv

Build:

```text
grok
  --sandbox workspace
  --disable-web-search
  --no-subagents
  agent
  --no-leader
  --always-approve
  stdio
```

`--sandbox`, `--disable-web-search` and `--no-subagents` are top-level
options (Grok ≥ 1.0.40): `agent` rejects them, and `--no-auto-update` no
longer exists. A wrong argv kills every qualify probe and turn at launch.

Discuss (`plan_only`): `--sandbox read-only` instead of `workspace`.

Never `--leader`. Magician does not relocate `GROK_HOME`. Child env is
rebuilt from `PATH`, `HOME`, `USER`, `TMPDIR`, `LANG`, `TERM`, plus
`GROK_HOME` and `XAI_API_KEY` only when already present in the parent.
`MAGICIAN_*` is dropped. `grok --version` probes use the same
`env_clear` + allowlist rebuild so Magician secrets are not inherited.

Reviewed binary locations (in addition to `PATH`):

- `/opt/homebrew/bin/grok`
- `/usr/local/bin/grok`
- `~/.grok/bin/grok`
- `~/.local/bin/grok`

## ACP

1. `initialize` with `protocolVersion: 1`, Magician `clientInfo`, and
   `clientCapabilities: {}` (no `fs`, no `terminal`).
2. Same-engine follow-up in the same VibeDev root `session/load`s the
   ACP `sessionId`. A settled follow-up is a **new** task/execution, so
   Magician does not read only the current `coding_ledger.json` (that
   file starts empty). After a successful turn, the ACP id is persisted
   on the VibeDev **chain root** (`CodingContinuationContext.root_task_id`)
   at `<scope_root>/coding_engine/chain_continuations/<root>.json`.
   `bind_grok_turn_options` resolves the previous continuation from that
   store, then ancestor-task execution ledgers
   (`vibedev_constraint_lookup_chain` / `ancestor_task_ids`), then the
   current execution ledger. It feeds the last chain continuation (**any
   engine**) to `resume_or_fresh` with requested `GrokAcp`, the chain
   root, and the current invocation generation. Only
   `ContinuationResume::Resume` sets `resume_session_id`. Cross-engine,
   cross-scope, and stale generation start `session/new`. An
   engine-tagged `pending_resume` from a checkpoint rewind wins only
   when its `engine` is `grok_acp`; a Pi/Codex pending id is dropped.
   Otherwise `session/new` with `cwd` = shadow workspace. Both load and
   new send `mcpServers: []` and `_meta.yoloMode: true`. A
   `session/load` JSON-RPC error is `continuation_lost`: Magician
   records it on `CodingEngineRunResult.continuation_fresh_reason` and
   the chain-root record (not only `tracing::warn`), starts
   `session/new`, and does not crash. Pi/Codex native ids are never sent
   to Grok; a Grok id is never bound onto Pi or Codex. Checkpoints store
   `engine` + the ACP `native_session_id` from the continuation, never
   Magician's `grok-{uuid}` `result.session_id`.
3. `session/prompt` with a text prompt. Write the JSON-RPC request, then
   drain JSONL until that id's result. The drain uses the outer
   `request.timeout` (hours-scale). While waiting, `tokio::select!` races
   the JSONL read against the cancel token, the Grok control Interrupt
   channel, and a sleep until the next
   `ProgressWatchdog` deadline (or `min(remaining bound,
   time_until_watchdog)`), so a silent peer cannot pin the turn until
   the wall clock. `initialize` / `session/new` / `session/load` keep
   the short RPC timeout. `session/cancel` is a JSON-RPC **notification**
   (method + params, no `id`, no result wait). Success requires that
   JSON-RPC result. A dropped stream is not success. After the prompt
   RPC returns, Magician drains a bounded follow-up FIFO with further
   `session/prompt`s on the same ACP session.
4. Map `session/update`:
   - `agent_message_chunk` → `MessageUpdate` with `text_delta` (the
     `coding.message` rail; `Response` is dropped by the handler)
   - `agent_thought_chunk` → `MessageUpdate` with `thinking_delta`
   - `tool_call` → `ToolExecutionStart`
   - `tool_call_update` completed → `ToolExecutionEnd`
   Consecutive same-channel `MessageUpdate` chunks concatenate
   (`text_delta` with `text_delta`, `thinking_delta` with
   `thinking_delta`) so ACP incremental tokens are not dropped by
   coalesce. Do not mix text into thinking.
5. Synthesize `AgentStart` after the session is ready, `TurnEnd` when
   `session/prompt` returns (with mapped usage), and `AgentSettled`
   when the prompt FIFO is drained. Before each FIFO pop and before
   `AgentSettled`, Magician checks cancel-token / watchdog / Interrupt
   and retires the handle so Stop after a successful `session/prompt`
   cannot settle as success.
   When ACP reports usage, that maps into `usage_capture`. Omitted ACP
   usage leaves the cell empty (not `Some` zeros) and does **not**
   exhaust the execution token meter. Map ACP `_meta.usage`, prompt-result
   `usage` / `total_cost_usd` / `modelUsage.*.costUSD`, and
   `session/update` `usage_update.cost` when present. Token fields may be
   zero; **omitted cost stays unknown** (`cost_known: false`,
   `cost_total` omitted from `coding.turn.finished` and from
   `CodingUsage`). Never report `$0.00` because Grok omitted USD.
   `billing_basis` is `external`. `run_coding_task` skips
   `emit_llm_call` unless `cost_known`: `LLMResponseReceived.cost` and
   parquet `llm_calls.cost_usd` are non-null doubles, so token-only
   unknown cost is not persisted as billed `$0.00`. Reported tokens still
   meter via `account_coding_turn_usage`.
6. Magician may send `initialize`, `session/new`, `session/prompt`,
   `session/cancel`, `session/load`. JSON-RPC ids are string or integer.
   Do not send `x.ai/*`. Unknown agent→client **requests** (method + id)
   fail closed, including string ids. `x.ai/*` notifications without id
   are ignored. `session/load` treats only a JSON-RPC error result as
   `continuation_lost` and then `session/new`; other protocol errors
   cancel and fail the turn.
7. Cockpit Stop goes through `CodingControlHandle::Grok(GrokTurnHandle)`
   (generation-fenced, same shape as Codex). Stop sends Interrupt; the
   drain select unblocks and Magician writes a `session/cancel`
   **notification** if a session exists, then process-group kill on
   Drop. Parent cancel-token still unblocks the same path. V1 steer
   queues on the follow-up FIFO (cap 8); it does not depend on Grok TUI
   `follow_up_behavior`. A stale generation cannot steer, follow-up, or
   stop a replacement handle. Checkpoint pending resume is consumed
   only when the next turn's engine matches.

ACP session id lives only on `CodingContinuationRef`. Magician
`result.session_id` is a Magician-owned `grok-…` invocation id and is
omitted from proposal raw. Provider output is not proposal authority;
`run_coding_task` stages the shadow-vs-real diff.

## Readiness

Ready = `coding.grok.enabled` + exactly one resolved binary +
`grok --version` parseable and ≥ **1.0.5** + either `~/.grok/auth.json`
exists **or** `XAI_API_KEY` is non-empty in the filtered child env + a
cached ACP isolation receipt that **positively evidenced** isolation.
`selectable` is true only for Ready.

Isolation is fail-closed. Magician never treats omitted MCP/tool lists as
proof that user `~/.claude.json` MCP, `~/.grok` MCP, or a repo `.mcp.json`
did not load:

- `mcpServers` must be **present and empty** (array) on `initialize`,
  `session/new`, or a bounded `session/update` drain. Missing
  `mcpServers` is unattested, not isolated: the profile stays
  `unqualified` (hidden, retry). Do not relocate `GROK_HOME` to paper
  over this.
- A tool list must be **present** and contain no `web_search`,
  `search_tool`, MCP-style `server__tool` names, or `use_tool`. Built-in
  Grok tools (`read_file`, `bash`, `search_replace`, `grep`) are allowed.
  Missing tool list → `unqualified`. Present leaky tools →
  `incompatible`.
- Present nonempty MCP, hooks, or plugins → `incompatible`.
- The qualify cwd may plant a uniquely named `.mcp.json` canary. Never
  plant it in the user's home. If that server name appears,
  `incompatible` (cwd MCP loaded).

Otherwise the snapshot uses the Codex vocabulary:

- `disabled` — kill switch
- `missing` — no binary
- `checking` — HTTP refresh status (202), not a dishonest Ready
- `ambiguous_installation` — two canonical paths, no `coding.grok.binary`
- `unqualified` — binary found, version not yet known, **or** version +
  auth are OK but isolation is not yet attested, **or** ACP
  `initialize` / `session/new` / a short `session/update` drain omitted
  MCP or tool lists. Missing lists keep `grok-default` hidden.
- `incompatible` — version < 1.0.5 or unparseable, **or** ACP advertised
  MCP servers, hooks, plugins, web search, MCP-style (`server__tool`) tools,
  **or** the MCP gateway tools `search_tool` / `use_tool`. Grok's user guide
  documents those two as the way the model finds and calls tools on enabled
  MCP servers — including `~/.grok` config, installed plugins and
  xAI-managed connectors that Magician's empty `mcpServers` does not cover.
  Grok 1.0.41 advertises both in every session with no per-session flag to
  withhold them, so the Grok CLI engine stays blocked (reason: "Grok
  advertises its MCP gateway tools…"); Grok models remain usable through Pi
  profiles. These are gateway tools, not web search — Grok's real
  `web_search` / `web_fetch` disappear under `--disable-web-search`.
- `auth_required` — version OK, no auth file and no key. Public `reason`
  is `sign in required` (no home path, no `auth.json`, no key)

Request paths read the snapshot. GET `/api/magician/v2/coding/profiles`
calls `observe_grok_readiness` + `project_coding_profiles` (filesystem +
cached version/auth/isolation overlay, same shape as Codex observe). It
never runs `grok --version`, never ACP-initializes, and never waits.
`run_coding_task` observes again at dispatch and binds only when the
executable identity matches the Ready snapshot and a matching isolation
receipt is cached.

`POST /api/magician/v2/coding/engines/grok_acp/refresh` returns **202**
with `status: checking`. It coalesces 2s and observes filesystem +
cached version/auth/attestation only (same shape as Codex
`codex_app_server/refresh`). The handler never calls `grok --version`
and never ACP-initializes.

A background tick (started next to the Codex qualify worker) may probe
`--version` (5s, cached) and overlay version **and** auth. If those would
be Ready, the same tick speaks ACP `initialize` + `session/new` with
`mcpServers: []` in a disposable cwd, drains a bounded window of
`session/update` so advertised tools can become evidence, then a
`session/cancel` notification and process-group kill (`process_group(0)`
+ `killpg`). If those messages still omit MCP
and tool lists, readiness stays `unqualified` and the profile stays
hidden. Magician does not relocate `GROK_HOME`. The probe session id is
not journaled as a VibeDev continuation. Identity change drops the
version cache, auth overlay, and isolation receipt so a new binary
cannot inherit Ready. Until Ready, `grok-default` is present with
`selectable: false`. `run_coding_task` never waits on a probe. Catalog
activation (`append_ready_grok_catalog_entry`) requires `selectable`
(Ready).

`GET /coding/profiles` includes `grok-default`; Auto may pick Grok only when
that row is Ready. Staying `unqualified` until ACP emits `mcpServers` (present
and empty) **and** a tool list is fail-closed by design, not a discovery bug.

Public `reason` strings (no paths, homes, `auth.json`, or email):

| State | Reason |
| --- | --- |
| `disabled` | Grok Build CLI is disabled |
| `missing` | Grok is not installed (or configured binary was not found) |
| `checking` | checking Grok Build CLI readiness (HTTP 202 overlay) |
| `ambiguous_installation` | multiple compatible Grok installations were found; set `coding.grok.binary` |
| `unqualified` | CLI version not yet known, **or** isolation not yet attested, **or** ACP omitted MCP/tool lists |
| `incompatible` | version too old / unparseable, **or** advertised MCP / hooks / plugins / web search |
| `auth_required` | sign in required |
| `ready` | Grok Build CLI is ready |

Initialize payload is Magician `clientInfo` and `clientCapabilities: {}`
(no client `fs` or `terminal`). `--no-leader` stays on the launch argv
whenever sandbox ≠ `off` (Build `workspace`, Discuss `read-only`).

## Non-negotiables

- One coding pipeline. No client-selected `grok_binary`.
- No silent fallback after possible dispatch.
- Outer Magician fence required (no Pi unsandboxed fallback).
- Unreported cost is unknown, not zero.
- Existing Pi/Codex fixtures stay compatible.
