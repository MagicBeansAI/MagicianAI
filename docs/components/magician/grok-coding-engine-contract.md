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

Discuss (`plan_only`): `--sandbox read-only` instead of `workspace`.
`--no-leader` stays whenever sandbox ≠ `off`; never `--leader`.

`--sandbox`, `--disable-web-search` and `--no-subagents` are top-level
options (Grok ≥ 1.0.40): `agent` rejects them, and `--no-auto-update` no
longer exists. A wrong argv kills every qualify probe and turn at launch.

Magician never relocates `GROK_HOME`. Child env is rebuilt from `PATH`,
`HOME`, `USER`, `TMPDIR`, `LANG`, `TERM`, plus `GROK_HOME` when present and
`XAI_API_KEY` only when Magician's credentials apply
([harness credentials](harness-auth.md)); `MAGICIAN_*` is dropped.
`grok --version` probes use the same `env_clear` + allowlist rebuild so
Magician secrets are not inherited.

Reviewed binary locations (in addition to `PATH`):

- `/opt/homebrew/bin/grok`
- `/usr/local/bin/grok`
- `~/.grok/bin/grok`
- `~/.local/bin/grok`

## ACP

1. `initialize` with `protocolVersion: 1`, Magician `clientInfo`, and
   `clientCapabilities: {}` (no `fs`, no `terminal`).
2. **Session.** A settled follow-up is a **new** task/execution, so the
   current `coding_ledger.json` starts empty. After a successful turn the ACP
   id is persisted on the VibeDev **chain root**
   (`CodingContinuationContext.root_task_id`) at
   `<scope_root>/coding_engine/chain_continuations/<root>.json`.
   `bind_grok_turn_options` resolves the previous continuation from that
   store, then ancestor-task execution ledgers
   (`vibedev_constraint_lookup_chain` / `ancestor_task_ids`), then the current
   execution ledger, and feeds the last chain continuation (**any engine**) to
   `resume_or_fresh` with requested `GrokAcp`, the chain root and the current
   invocation generation. Only `ContinuationResume::Resume` sets
   `resume_session_id` and triggers `session/load`; cross-engine, cross-scope
   and stale-generation continuations start `session/new` with `cwd` = shadow
   workspace. An engine-tagged `pending_resume` from a checkpoint rewind wins
   only when its `engine` is `grok_acp`, and is consumed only when the next
   turn's engine matches; a Pi/Codex pending id is dropped. Pi/Codex native
   ids are never sent to Grok, and a Grok id is never bound onto Pi or Codex.
   Both load and new send `mcpServers: []` and `_meta.yoloMode: true`.
   A `session/load` JSON-RPC error is `continuation_lost`: Magician records it
   on `CodingEngineRunResult.continuation_fresh_reason` and the chain-root
   record (not only `tracing::warn`) and starts `session/new`. Other protocol
   errors cancel and fail the turn.
3. **Prompt.** `session/prompt` with a text prompt; write the request, then
   drain JSONL until that id's result, under the outer `request.timeout`
   (hours-scale). While waiting, `tokio::select!` races the read against the
   cancel token, the Grok control Interrupt channel, and a sleep until the next
   `ProgressWatchdog` deadline (`min(remaining bound, time_until_watchdog)`),
   so a silent peer cannot pin the turn. `initialize` / `session/new` /
   `session/load` keep the short RPC timeout. Success requires the JSON-RPC
   result; a dropped stream is not success. Magician then drains a bounded
   follow-up FIFO with further `session/prompt`s on the same ACP session.
4. Map `session/update`:
   - `agent_message_chunk` → `MessageUpdate` with `text_delta` (the
     `coding.message` rail; `Response` is dropped by the handler)
   - `agent_thought_chunk` → `MessageUpdate` with `thinking_delta`
   - `tool_call` → `ToolExecutionStart`
   - `tool_call_update` completed → `ToolExecutionEnd`

   Consecutive same-channel chunks concatenate (`text_delta` with
   `text_delta`, `thinking_delta` with `thinking_delta`) so coalesce drops no
   incremental tokens. Text never mixes into thinking.
5. **Lifecycle.** Synthesize `AgentStart` after the session is ready,
   `TurnEnd` when `session/prompt` returns (with mapped usage), and
   `AgentSettled` when the FIFO is drained. Before each FIFO pop and before
   `AgentSettled`, Magician checks cancel token / watchdog / Interrupt and
   retires the handle, so Stop after a successful prompt cannot settle as
   success.
6. **Usage.** Map ACP `_meta.usage`, prompt-result `usage` /
   `total_cost_usd` / `modelUsage.*.costUSD`, and `session/update`
   `usage_update.cost` into `usage_capture` when present. Omitted usage leaves
   the cell empty (not `Some` zeros) and does **not** exhaust the execution
   token meter. **Omitted cost stays unknown** (`cost_known: false`,
   `cost_total` omitted from `coding.turn.finished` and `CodingUsage`); never
   `$0.00`. `billing_basis` is `external`. `run_coding_task` skips
   `emit_llm_call` unless `cost_known`, because `LLMResponseReceived.cost` and
   parquet `llm_calls.cost_usd` are non-null doubles. Reported tokens still
   meter via `account_coding_turn_usage`.
7. **Methods.** Magician may send `initialize`, `session/new`,
   `session/prompt`, `session/cancel` (a JSON-RPC **notification**: no `id`,
   no result wait) and `session/load`; never `x.ai/*`. JSON-RPC ids are string
   or integer. Unknown agent→client **requests** (method + id) fail closed,
   including string ids; `x.ai/*` notifications without id are ignored.
   A response whose id has no request in flight fails the turn as
   `unknown or duplicate response id (id <id>, error: <message>)`
   (`GrokSessionError::UnexpectedResponse`), carrying the id and any error
   text. A response with a **non-numeric** id is ignored: Grok answers its own
   internal requests on the same stream (for example `"skills-reload"`), and
   Magician's ids are always numbers.
8. **Stop.** Cockpit Stop goes through
   `CodingControlHandle::Grok(GrokTurnHandle)` (generation-fenced, same shape
   as Codex): Interrupt unblocks the drain select, Magician sends
   `session/cancel` if a session exists, then process-group kill on Drop. The
   parent cancel token takes the same path. V1 steer queues on the follow-up
   FIFO (cap 8), independent of Grok TUI `follow_up_behavior`. A stale
   generation cannot steer, follow up, or stop a replacement handle.

The ACP session id lives only on `CodingContinuationRef`; checkpoints store
`engine` + that `native_session_id`. Magician `result.session_id` is a
Magician-owned `grok-{uuid}` invocation id, never stored as the continuation
and omitted from proposal raw. Provider output is not proposal authority;
`run_coding_task` stages the shadow-vs-real diff.

## Readiness

Ready = `coding.grok.enabled` + exactly one resolved binary +
`grok --version` parseable and ≥ **1.0.5** + either `~/.grok/auth.json`
exists **or** (Magician's credentials apply and) `XAI_API_KEY` is non-empty + a
cached ACP isolation receipt that **positively evidenced** isolation.
`selectable` is true only for Ready; until then `grok-default` is listed with
`selectable: false`, and Auto may pick Grok only when the row is Ready.

Isolation is fail-closed. Omitted MCP/tool lists are never proof that user
`~/.claude.json` MCP, `~/.grok` MCP, or a repo `.mcp.json` did not load:

- `mcpServers` must be **present and empty** (array) on `initialize`,
  `session/new`, or a bounded `session/update` drain. Missing `mcpServers` is
  unattested: `unqualified` (hidden, retry).
- A tool list must be **present** and contain no `web_search` or MCP-style
  `server__tool` names. Built-in Grok tools (`read_file`, `bash`,
  `search_replace`, `grep`) are allowed; `search_tool` / `use_tool` are the
  disclosed gateway below. Missing list → `unqualified`; present leaky tools →
  `incompatible`.
- Present nonempty MCP, hooks, or plugins → `incompatible`.
- The qualify cwd may plant a uniquely named `.mcp.json` canary (never in the
  user's home). If that server name appears, `incompatible` (cwd MCP loaded).

Staying `unqualified` until ACP emits both lists is fail-closed by design, not
a discovery bug.

**MCP gateway: disclosed, not blocked.** The gateway tools `search_tool` /
`use_tool` are how the model finds and calls tools on enabled MCP servers
(`~/.grok` config, installed plugins and xAI-managed connectors that
Magician's empty `mcpServers` does not cover), and Grok advertises both in
every session with no flag to withhold them. The attestation records them
(`GrokAttestation.mcp_gateway`) instead of failing; the receipt is `ready` with
`GROK_REASON_READY_GATEWAY`. The qualify worker lists what they reach
(`grok mcp list --json`, `grok plugin list --json`, names only, 15 s each)
into `GrokGatewayReach { mcp_servers, plugins, listed }`, carried on the
profile row as `gateway` and shown under Grok in the VibeDev engine picker
before the owner chooses it (account connectors are named as not listable; a
failed listing says "unknown", never "none"). These are not web search: Grok's
real `web_search` / `web_fetch` disappear under `--disable-web-search`.

States use the Codex vocabulary. Public `reason` strings carry no paths,
homes, `auth.json`, or email:

| State | Meaning | Reason |
| --- | --- | --- |
| `disabled` | kill switch | Grok Build CLI is disabled |
| `missing` | no binary | Grok is not installed (or configured binary was not found) |
| `checking` | HTTP refresh status (202), not a dishonest Ready | checking Grok Build CLI readiness (HTTP 202 overlay) |
| `ambiguous_installation` | two canonical paths, no `coding.grok.binary` | multiple compatible Grok installations were found; set `coding.grok.binary` |
| `unqualified` | version not yet known, or version + auth OK but isolation not attested, or ACP `initialize` / `session/new` / a short `session/update` drain omitted MCP or tool lists | CLI version not yet known, **or** isolation not yet attested, **or** ACP omitted MCP/tool lists |
| `incompatible` | version < 1.0.5 or unparseable, or advertised MCP servers, hooks, plugins, web search, or `server__tool` tools | version too old / unparseable, **or** advertised MCP / hooks / plugins / web search |
| `auth_required` | version OK, no auth file and no key | sign in required |
| `ready` | all gates pass | Grok Build CLI is ready |

### Observation and qualify

Request paths read the snapshot. GET `/api/magician/v2/coding/models`
calls `observe_grok_readiness` + `project_coding_profiles` (filesystem +
cached version/auth/isolation overlay, same shape as Codex observe) and never
runs `grok --version`, ACP-initializes, or waits. `run_coding_task` observes
again at dispatch, binds only when the executable identity matches the Ready
snapshot and a matching isolation receipt is cached, and never waits on a
probe.

`POST /api/magician/v2/coding/engines/grok_acp/refresh` returns **202** with
`status: checking`. It coalesces 2s and observes filesystem + cached
version/auth/attestation only (same shape as Codex
`codex_app_server/refresh`); the handler never runs `grok --version` or
ACP-initializes.

A background tick (started next to the Codex qualify worker) may probe
`--version` (5s, cached) and overlay version **and** auth. If those would be
Ready, the same tick speaks ACP `initialize` + `session/new` with
`mcpServers: []` in a disposable cwd, drains a bounded window of
`session/update` so advertised tools can become evidence, then sends a
`session/cancel` notification and kills the process group
(`process_group(0)` + `killpg`). The probe session id is not journaled as a
VibeDev continuation. An identity change drops the version cache, auth
overlay, and isolation receipt so a new binary cannot inherit Ready. Catalog
activation (`append_ready_grok_catalog_entry`) requires `selectable` (Ready).

## Trusted projects (P5)

Grok is selectable in trusted projects too, with the same gateway disclosure:
the earlier project-config-only rule is relaxed for Grok by the decision to
disclose the gateway rather than block it. The adapter applies
`request.build_env` like the others.

## Non-negotiables

- One coding pipeline. No client-selected `grok_binary`.
- No silent fallback after possible dispatch.
- Outer Magician fence required (no Pi unsandboxed fallback).
- Unreported cost is unknown, not zero.
- Existing Pi/Codex fixtures stay compatible.
