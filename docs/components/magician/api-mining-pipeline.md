# API Mining Pipeline

## Purpose

API mining turns observed browser traffic into replayable API capabilities, on
two deliberately separate rails:

- **Rail 1 — Task Recipes**: a task-start cross-origin request DAG keyed by task
  shape, replayed over HTTP before any agent, browser or planning LLM starts.
- **Rail 2 — capabilities/workflows**: per-origin capabilities and
  `WorkflowGraph`s used mid-run at the browser-primitive boundary.

Recipes supplement Rail 2 and are not guaranteed for arbitrary traffic. Values
in prose inside large server-rendered documents are not served.

## Capture And Aggregation

- Traces aggregate across recent tasks; thresholds admit low-frequency real
  endpoints. Only XHR/Fetch is mined; telemetry, CDN, analytics and consent
  hosts/paths are filtered.
- Capture forces bundled Chrome (other engines omit request bodies). Traces carry
  session cookies snapshotted from the live jar at drain time (HAR omits them);
  oversized bodies keep a bounded prefix. HAR auth goes to the encrypted scope
  store before redaction; HAR parsing caps at 64 MiB.

## Capability Formation

- Query strings are templated. Sensitive query params and vendor auth headers
  (`x-*-api-key`) are redacted on disk, kept in the encrypted captured-auth
  partition, named in `auth_requirements.query_params` and resolved at replay.
- Search-like body fields (`query`, `q`, `search`, `term`) always become
  placeholders.
- **One body definition**: a body is parsed as what it is, not what its
  content-type claims, identically by tokenizer, templatizer and replay
  re-encoder. Values inside a body array are not task inputs unless typed.
- `browser__open` emits a synthetic `PageLoad` action context binding a page URL
  (`/search?q=rust`) to the backend calls it triggered, cross-origin included;
  stable backend placeholders (SDK version segments) become default params.
- **Takeover readiness**: a binding past the sample threshold whose param
  mappings or defaults resolve every URL, header and body template. Only these
  count in registry health; action-context routing skips URL-unresolvable
  candidates before ranking. Correlation reads traces by task id, then execution
  id, and walks ranked requests until a useful capability stores the binding
  (telemetry sharing a query value cannot block it); template lookup compares
  decoded query keys/values.
- **Raw-JSON document rail**: a JSON object/array returned by
  `browser__open`/`browser__get` over HTTP(S) on a non-denylisted URL registers
  as a read-only GET capability; its step stays `executed_via: Browser` but
  carries capability id, method, params, status and body for later browserless
  replay.
  Navigation (`browser__open`, `browser__tab new/open <url>`, simple
  `location.href/assign/replace`) sets the page URL; calls with no known URL
  start no sequence (no `unknown_origin` pinning).

## Replay Loop

Search/query/lookup/suggest/autocomplete-shaped POSTs are read-only unless
URL/body look like checkout, order, payment, cart, admin or mutation work.
Unknown side effects never replay.

`OriginPolicyStore::open` is a cheap handle over a process-wide registry keyed by
policy path (one shared `RwLock` per file; one process owns a data root;
`forget_shared_state` resets it for tests). Origin replay modes:

| Mode | Reads | Writes |
|---|---|---|
| `observe_only` | capture/mining only | no |
| `validate_only` | passive validation, no takeover | no |
| `replay_reads` | replace browser steps | browser |
| `replay_writes_with_hitl` | automatic | fail closed to browser until the approval-token path exists |
| `replay_trusted_writes` | automatic | Trusted capabilities only |

Legacy `allow-replay`: `true` → `replay_reads`, `false` → `validate_only`. No
policy: reads replay, writes blocked. A successful `ApiRunner` takeover
best-effort ingests JSON into the scoped projection pipeline without ever failing
the replay (logged as Pending-created, Pending-existing, Ingested,
Not-projectable, Migration-rejected or Failed); see [API Mining Projections](api-mining-projections.md).

### Passive XHR/Fetch validation

`api_mining.enable_xhr_validation` enables a background pass that replays matched
XHR/Fetch traces (never replacing the browser action), compares status/body and
feeds promotion. Writes need the router's idempotent URL-pattern gate;
`observe_only` origins are skipped; `api_mining.xhr_validation_max_per_pipeline`
caps requests per run. `GET /api-mining/passive-validation-metrics` reports
per-scope counters, separate from active router counters.

### Registry health

On open the registry rebuilds `registry_index.json` (v1.1, with
`parent_origin`) on stale entries, unindexed files or old schema, so a deleted or
partial capability never looks replayable.
`GET /api-mining/registry-health` returns indexed/loadable/stale/unindexed,
confidence-tier, replayable, binding and takeover-ready counts (from loadable
files, excluding unresolvable bindings), live
`replay_mode` and per-origin inactive reasons ("nothing learned", "not
replayable", "policy blocks", "not bound yet").

Capability Evolution suppresses legacy `browser`/`action=api_replay` packs and
`magician internal-replay` skills and removes stale evolved folders; live
takeover runs through the scoped
`ApiRouter` + `ApiRunner`.

## Sequences and workflows (Rail 2)

Each agentic execution writes a `CapabilitySequence` to
`<scope>/api_mining/<origin_key>/sequences/<id>.json`. A `SequenceStep` holds
`step_index`, `capability_id` (None for browser-only), `origin`, `concrete_url`,
`method`, request/response fields (`response_body` ≤ 4 KB),
`action_binding_id`, `browser_action_desc`, optional `browser_action` /
`browser_arguments`, `executed_via` (`Browser` | `ApiReplay`) and timing.

When `SequenceStore::count(origin) >= 2`, finalize compiles a linear
`WorkflowGraph` to `workflows/<workflow_id>.json`:

1. `infer_auto_data_flows` substring-matches later params against earlier
   responses (skips < 4 chars and `true`/`false`/`null`/`None`; dedupes by
   `(source_step, target_step, target_param)`) → `DataFlow` (`AutoMatch`, 0.9).
2. `compile_with_llm` (`workflow_compilation_system v1.0.1`,
   `LLMOperation::WorkflowCompilation`) validates `step_index`, flow order,
   `skip_if.source_step` precedence and `compiled_from_sequence_ids`; one retry.
3. Fallback: longest sequence + auto flows, other params `Literal`.

`WorkflowGraph`: ordered `steps`; `param_sources` =
`Literal | DataFlow | UserInput | SessionAuth`; optional `skip_if`
(`Equals`/`NotEquals`/`IsEmpty`/`IsNotEmpty`) and `browser_fallback`;
`auth_requirements.login_step_id`; `confidence.workflow_level` starts `Draft`.

`WorkflowReplayEngine` evaluates `skip_if` (fail-open on missing source),
preflights `ApiRunner::can_replay`, then calls `ApiRunner::replay_with_reqwest`.
Browser-only steps fail as `ReplayError::BrowserOnlyStep` on the operator path;
`replay_until_browser_fallback` returns `MixedReplayResult` +
`BrowserFallbackRequest` at the first failure with a `browser_fallback` (task
execution does not consume it). Maturity: Draft → Candidate at 1+ successes →
Validated at 3+ successes → Trusted at 10+ total replays with failure rate
< 10% (`promote_maturity`, `workflow_replay/engine.rs`).

JSONPath (`workflow_replay/jsonpath.rs`): `$.field[.subfield...]`, indexes
(`$.hits[0].objectID`), `$..field` (first match); scalars stringified; null is
missing; wildcards, filters and slices return `None` (`JsonPathMiss`).

Not supported: operator-HTTP consumption of browser fallback, workflow
invalidation on capability demotion, numeric `SkipOperator`s, manual compile.

`run_mining_pipeline` is `tokio::spawn`'d via the orchestrator's `weak_self`
(inline when unset); passes re-read the recent-trace corpus, so nothing drops.

## Task Recipes (Rail 1)

### Learning

One browser-backed execution suffices. The finalizer takes answer values from
the accepted outcome summary, artifact and output previews and works backward to
the responses that carried them: response-fed path/query/header/body params
become typed data flows; values from the task title/description or typed into
the browser become named inputs. Session material (auth headers, cookies, auth
query values, passwords, tokens) is stored only as a lookup scheme; volatile
signatures/nonces keep a name, never a value, and unresolved volatiles fail
closed to the browser.
A recipe publishes only if:

- **every** reported answer value has response evidence, under a key the task
  asked for (or an asked key carries exactly that value); a partial match does
  not publish. Named zero, single-digit and boolean answers need a matching
  response field; two named fields with the same value get distinct bindings.
  Recognition: decimals compare by value, currency codes/hyphenated tokens do not
  truncate, a URL scheme colon is not a field separator, counts report the
  number;
- JSONPath evidence is literal-key-safe (keys containing `.` or brackets are
  skipped; mislearned recipes need recompilation);
- a text dependency reproduces the exact value with exactly one bounded,
  content-free regex match (replay examines ≤ 2 matches; a second is schema
  drift);
- an empty-response mutation joins only if it immediately precedes an answer
  read on the same normalized resource path within 15 s (a collection path may
  pair with exactly one extra dynamic segment);
- the optional `RecipeCompilation` LLM pass, which sees only a scalar-free
  projection (no task text or templates), derives nothing but backward JSON flows
  whose JSONPath reproduces the captured value (proof body ≤ 1 MiB; proof values
  never reach the model or recipe); it cannot add/remove/rename inputs or rewrite
  templates, and unknown steps, forward/duplicate dependencies and invalid
  extractors are rejected;
- redaction markers are not used as dependency evidence (redacted auth resolves
  from the scoped session; others fail closed); a reused parameter name keeps
  distinct inputs, and fresh response-derived headers beat stale captured ones;
- equal values sharing one input agree at every title/description occurrence
  (repeated-slot recipes cannot fuzzy-match);
- evidence is complete for every listed trace file of that execution (no
  fallback to older runs; missing, malformed or unterminated records or an
  exceeded budget abort learning), within
  ≤ 32 trace files, 32 MiB per file, 64 MiB total, 10,000 events; typed-action
  recovery ≤ 32 files × 2 MiB, 256 values × 64 KiB; reported-output mining
  ≤ 256 KiB per source (the dashboard trace reader stays tolerant of corrupt
  lines);
- the capture is replayable: session auth resolves against its own predicate;
  non-GET steps have captured bodies; JSON (incl. `+json`) and URL-encoded forms
  substitute separately (`=` in XML, multipart, text or raw GraphQL is not a
  form; unsupported bodies fail preflight; raw bodies keep their bytes; empty
  stays empty).

Captured `Content-Type` and `Accept` are replayed and enter the request-shape
fingerprint (recipes that dropped them need recompilation); `Host`,
`Content-Length`, `If-None-Match` are excluded. Non-JSON
answers get no captured-body anchor except a bounded plain-text body exactly
equal to the answer.

### Storage

```text
<scope>/api_mining/
  recipes/<recipe_id>.json
  recipes/<recipe_id>/runs.jsonl
  recipe_index.json
  replay_grants.json
  noise_filter.json
  settings.json
```

Recipe, shape-index and task-index publication is one locked atomic
transaction; replay is serialized per recipe in-process (maturity counters and
transport hints cannot be lost). The append-only run ledger records rail, step
outcomes, failure class, approval request ids, duration and whether a browser
continuation taught a new version, never request values or bodies; it appends
O(1) below 4 MiB, then compacts to the newest complete records (≤ 256 KiB each;
API reads ≤ 200 rows). Persisted run/sequence outcomes are structural only
(method, redacted URL template, status, duration, transport).

### Matching

Lookup ladder: (1) a task-id binding, re-extracting inputs from the current
title; (2) a deterministic anchored template match, most specific first; (3) a
shape-only LLM confirmation over the three best token-overlap candidates
(`match_confirm_threshold`), never for write-bearing recipes. Title and
description templates both bind (conflicts reject; whitespace differences are
equivalent). Compile-time examples are reused only while the normalized
title/description fingerprint is unchanged. Execution-only refinements,
substituted launch goals and runtime plan context bypass replay and learning. A
version changed while lookup waits for its lock, or a miss, falls through to the
agentic browser loop.

Task-start replay passes the agentic rail's fresh-launch admission and rechecks
the admitted Runtime generation after lookup, lock and approval waits under the
execution lifecycle exclusion (held for bounded HTTP replay, not approval
waits); cancelled, paused, replaced or running generations cannot dispatch.

### Runner

- **Budget.** One DAG timeout (15 s default, clamped to 120 s) including auth
  healing; 8 MiB per step, 32 MiB per DAG, header count/byte budgets, ≤ 64 steps.
- **Whole-graph preflight** before request 1 (graph, origins, edges, inputs with
  finite JSON-number grammar — `+42`, `01`, `.5` rejected — substitutions,
  headers, unresolved volatiles), so no late failure leaves an executed prefix.
  Case-insensitive duplicate headers are rejected; substitution scans only the
  original template (no recursive expansion).
- **Session and auth healing.** Replay validates the recipe's
  principal/workspace before resolving credentials. Cookies resolve per concrete
  URL (identically after healing); a value-free per-step cookie requirement
  prevents anonymous HTTP. Stale auth on a **read** step may refresh captured
  auth once per step, at most `api_mining.auth_max_failures` per run, on the same
  transport; writes are never healed. Auth failures do not demote maturity;
  unrecoverable auth continues in the browser at the failed read; refresh
  sessions clean up on cancellation. See
  [Captured Auth For API Replay](auth-capture-and-replay.md#automatic-task-recipe-healing).
- **Transport ladder** (`api_mining.recipes.transport_ladder`): browserless
  surfaces need `reqwest`; a live browser adds `in_page_fetch`; `browser` is the
  continuation rail. Invalid names are dropped; an invalid ladder means
  browser-only. Anti-bot/network failures (incl. HTTP-200 challenge HTML) may
  downgrade a **read** of a structurally read-only recipe to in-page fetch, never
  re-running the failed rung. The warm idle pool carries no cookie jar or scope
  headers; client init fails closed rather than follow redirects. Browser fetch
  accepts the CLI `data.result` envelope, aborts at its deadline and bounds
  streaming.
- **Continuation.** Schema drift, missing input, policy blocks and exhausted
  reads hand value-free step summaries to the browser, which must not repeat
  them.
- Planner aliases bind the current recipe version (recompile retires the old
  alias). Inputs named `recipe_id`, `inputs`, `timeout_secs` or `__*` use a typed
  nested object, never server-injected values.

### Writes

- **Never retried.** A failed or uncertain write is never retried elsewhere or
  handed to a browser. Task-start replay fails the task; a
  planner-invoked recipe returns `write_outcome_uncertain`, `retryable=false`,
  `browser_retry_allowed=false`. Pre-send (credential, dependency,
  request-construction) failures permit browser recovery only when no earlier
  write was sent; missing credentials are not an uncertain effect.
- **Grants.** Every write is preflighted before the first request. A durable
  grant is keyed by recipe id, step id, capability id and a bounded request-shape
  fingerprint (normalized header templates plus JSON path/type shape with exact
  array positions, or form-key shape with duplicate counts; v4 domain, older
  fingerprints ask again). Opaque or unbounded bodies can be approved once, never
  durably; checkout, payment, login, delete and other denylisted floors always
  ask. `approve_once` exists only in-process (JSON cannot assert it); a
  remembered approval does not mint a one-run approval, and a grant stays
  revocable until the write is sent. Grants and the enable fence are rechecked
  before each later send. Denial ends `outcome_type=recipe_write_denied`.
- **Verification.** A write's `verify_with` read must contain at least one
  dynamic request value (task input or earlier response value; not auth,
  timestamps, literals or header-only values like CSRF) from each verified
  write, as an exact type-aware match along one JSON object branch (siblings and
  separate array rows cannot combine; decimals normalized) or a bounded non-JSON
  scan (> 65,536 overlapping matches rejected). Missing, oversized or ambiguous
  proof fails closed as schema drift without retry. If every dynamic value is
  empty the write stops pre-send. A 204 alone is not proof.

### Settlement and receipts

Before sending HTTP, a write-bearing recipe publishes an execution-bound,
scope-HMAC-sealed intent at
`<scope>/restricted/task_recipe_replays/<blake3(task_id, execution_id)>.json`; the
cancellation exclusion covers intent, replay and settlement, never human
approval. A terminal
result (completion, denial or uncertain write) commits receipt → Artifact
outcome → Runtime state, so the active-Runtime startup scan finds interrupted
settlement; recovery restores a saved answer without rerunning. An intent
without a result fails as `recipe_replay_interrupted` with no automatic retry
(verify remote state first). Direct/manual browser launches check the receipt
under admission, so bypassing the matcher cannot re-run a mutation; a `released`
marker permits browser fallback only once the runner proved nothing was sent.
Receipt corruption or publication failure is not a cache miss. Receipts work
with mining disabled or the recipe deleted (I/O ≤ 64 MiB). Direct recipe/tool
calls keep their caller-owned contract.

Statistics-persistence errors or a mid-run disable never replace a completed or
uncertain outcome (`state_persistence_warning` is attached).

### Outcomes and publication

Successful replay ends a task with `outcome_type=recipe_replay` and zero agentic
iterations; a read-only recurring Monitor hands the answer to the agentic rail,
which emits `monitor_run_result` without repeating requests; write-bearing
Monitor recipes stay on the agentic rail.

Candidate-or-better recipes publish scoped `recipe__*` packs; recompiling to
Draft unpublishes them (prior versions stay for audit, never auto-retried after
uncertainty). Empty or unknown-side-effect versions are never published or
matched as read-only. Emitted skills bind `published_only=true` and the exact
version; operator direct replay may exercise a Draft. Direct HTTP replay never
opens HITL: an ungranted write (launch via a task), or an unverifiable sent
write (`success: false`), returns 409. Compilation holds the scope publication
lock, rechecks the master switch around its atomic write and rolls back if it
flipped.

**Verification worker** (opt-in, started only when enabled at boot; waits one
full interval, rereads config and the scope switch each run) replays only Trusted
read-only recipes with recurring-Monitor provenance
(`compiled_from.monitor_revision`) using the Monitor's inputs: reqwest only, 15 s
budget, no browser/auth-healer/HITL/projection/sequence, ≤ 32 recipes per cycle
process-wide, oldest-replayed first. Outcomes record `rail_ended=verification`
and refresh the planner pack.

**Events.** `recipe.replay` events carry a content-free `payload.kind` (start,
step, auth heal, downgrade, approval, fallback, completion, recompilation) from
`RuntimeAgentEventType`; chat coalesces them into one cue per execution. The
API Mining Recipes tab reads the overview and the lazy run-ledger endpoint.

### Relevance and noise

Capabilities are `answer_bearing`, `dependency`, `first_party_api`,
`third_party_api`, `telemetry` (never promoted) or `unclassified`; recipe use
promotes answer/dependency steps and maintains `used_by_recipe_ids`. Unused
endpoints leave the default overview after 30 days (code constant) but stay
inspectable. Telemetry aggregation is cached 5 s, ≤ 128 scopes, invalidated on
purge.

### Master switch

`api_mining.enabled` is the process ceiling; `<scope>/api_mining/settings.json`
can turn a scope off but not override a process off. Every capture, auth
drain, sequence/action persist, compile, task-start replay, in-page
continuation, generated pack and mutating API entry reads the effective state;
the mid-run router binds a disabled implementation while off. Reads and purge stay available. Off: nothing new is captured, repeated
tasks use the browser, and learned data stays until the typed
`delete learned data` operation. Scope toggles evict that scope's planner
catalog; ceiling changes and config reloads evict all.

**Purge.** Origin purge removes every recipe containing the origin (as a unit),
its ledger and bindings, grants, `recipe__*` catalog rows, emitted skills,
projection metadata and that origin's projection rows; it validates every locked
recipe first and fails visibly on corrupt state. Full disable-and-purge also
deletes the mining tree, captured auth, projections and caches and invalidates
the planner snapshot, so re-enable cannot resurrect a tool; it ignores
unaddressable invalid filenames so they cannot veto deletion. Compiler
publication and destructive lifecycle share one scope lock; purge waits for every
per-recipe replay lock (even malformed records).

### Configuration

| Key | Default | Effect |
|---|---:|---|
| `api_mining.enabled` | `true` | process-wide hard ceiling |
| `api_mining.recipes.enabled` | `true` | Task Recipe compile and replay |
| `api_mining.recipes.compile_after_first_run` | `true` | compile after the first browser execution |
| `api_mining.recipes.match_confirm_threshold` | `0.85` | minimum LLM rung confidence |
| `api_mining.recipes.transport_ladder` | `reqwest, in_page_fetch, browser` | read-only downgrade order |
| `api_mining.recipes.unknown_origin_default` | `reads_auto_writes_by_grant` | unknown-origin policy |
| `api_mining.auth_recovery_mode` | `hybrid` | captured-auth recovery |
| `api_mining.auth_max_failures` | `3` | heal cap per recipe run |
| `api_mining.recipe_verification.enabled` | `false` | verification worker; no traffic while off |
| `api_mining.recipe_verification.interval_secs` | `21600` | worker delay and cadence |

### Endpoints

All under `/api/magician/v2`, scoped to the authenticated principal/workspace;
full table in [API Explorer](api-explorer.md).

| Method + path | Purpose |
|---|---|
| `GET /api-mining/overview` | value-free recipes, capabilities, auth, grants, counters |
| `GET /api-mining/registry?relevance=...&include_hidden=...` | filtered registry |
| `GET /api-mining/recipes` | bounded value-free list (input names/types/sources, no examples) |
| `GET /api-mining/recipes/{recipe_id}` | recipe without session values |
| `GET /api-mining/recipes/{recipe_id}/runs?limit=N` | run ledger, newest first; limit capped server-side |
| `POST /api-mining/recipes/{recipe_id}/replay` | read/granted replay; ignores wire approval markers; 409 if approval needed |
| `GET /api-mining/recipe-metrics` | lookup/replay/fallback/approval/grant/heal counters |
| `GET /api-mining/settings` | effective state and deciding layer |
| `PUT /api-mining/settings` | scope override or owner-only ceiling, live |
| `POST /api-mining/settings/disable-and-purge` | typed-confirmation off-then-delete |
| `GET /api-mining/sequences/{origin_key}` | `SequenceMetadata` list (no step bodies) |
| `GET /api-mining/sequences/{origin_key}/{sequence_id}` | full sequence; 404 if missing |
| `GET /api-mining/sequence-metrics` | `started` / `finalized` / `with_browser_only_steps` (process-local) |
| `GET /api-mining/workflows/{origin_key}` | `WorkflowMetadata` list |
| `GET /api-mining/workflows/{origin_key}/{workflow_id}` | full `WorkflowGraph` |
| `GET /api-mining/workflow-metrics` | compile started/succeeded/failed/fell-back-to-draft |
| `POST /api-mining/workflows/{origin_key}/{workflow_id}/replay` | `ReplayInputs { user_inputs }` → `ReplayResult`; persists stats + maturity |
| `GET /api-mining/replay-metrics` | started/succeeded, 3 failure-mode and 3 promotion counters |

## Related Docs

- [API Explorer](api-explorer.md)
- [Captured Auth For API Replay](auth-capture-and-replay.md)
- [V2 API Guide](v2-api-guide.md)
- Sequence capture plan
- Compilation plan
- Replay plan
