# API Mining Pipeline

## Purpose

API mining converts observed browser network traffic into reusable learned API
capabilities that Magician can inspect, validate, and replay. Two replay rails
exist and are deliberately separate:

- **Rail 1 — Task Recipes**: a task-start cross-origin request DAG keyed by task
  shape, replayed over HTTP before any agent, browser or planning LLM starts.
- **Rail 2 — capabilities/workflows**: per-origin capabilities and
  `WorkflowGraph`s used mid-run at the browser-primitive boundary.

Recipes supplement Rail 2; they do not retire it or guarantee a recipe for
arbitrary traffic. Mining does not serve a value embedded in prose inside a large
server-rendered document.

## Capture And Aggregation

- Network traces come from browser execution surfaces; mining aggregates across
  recent tasks, with promotion thresholds tuned for low-frequency but real
  endpoints.
- Noise suppression: XHR/Fetch only (not documents or "other"), host/path
  filters for telemetry, CDN, analytics and consent, and aligned browser/Rust
  trace buffers so useful samples survive noisy page loads.
- While capture is on, runs use bundled Chrome (other engines omit request
  bodies). Mined traces carry session cookies snapshotted from the live jar at
  drain time (HAR omits them). Oversized bodies keep a bounded prefix, not a
  token preview. Headed/headless HAR auth goes to the encrypted scope store
  before trace redaction; HAR parsing is capped at 64 MiB.

## Capability Formation

The miner clusters compatible traces into per-origin capabilities with URL
templates, methods, confidence, schema hints, side-effect classification and
replay statistics.

- Query strings are part of the template. Sensitive query params and vendor auth
  headers (`x-*-api-key`) are redacted on disk, captured in the encrypted
  captured-auth partition, named in `auth_requirements.query_params`, and
  resolved at replay.
- Search-like body fields (`query`, `q`, `search`, `term`) become placeholders
  even when constant across samples, so the capability is not frozen to the
  first term.
- **One body definition.** A body is read as what it is, not what its
  content-type claims (JSON under a form content-type is common). The tokenizer,
  templatizer and replay re-encoder share one definition; disagreement would
  create parameters no template carries and fail preflight.
- A value inside a body array is not a task input unless explicitly typed;
  binding incidental matches (searchable-field lists, tag filters) would erase
  the literal that distinguishes the task shape, so the recipe would never match.
- `browser__open` emits a synthetic `PageLoad` action context binding a page URL
  (`/search?q=rust`) to the backend calls it triggered, including cross-origin.
  Stable backend placeholders not from the page action (e.g. SDK version
  segments) become action-binding default params.
- **Takeover readiness** requires a binding past the sample threshold whose
  action-param mappings or default params resolve all URL, header and body
  templates. Registry health counts only these; action-context routing skips
  URL-unresolvable candidates before ranking.
- Action-binding correlation reads traces by task id first, execution id as
  fallback (CDP-proxy drain writes task-scoped records). With several correlated
  requests, persistence continues in ranked order until a useful capability
  stores the binding (telemetry sharing a query value cannot block it) and
  evaluates the full candidate set. Template lookup compares decoded query
  keys/values.
- **Raw-JSON document rail.** Top-level documents stay out of the miner, but when
  `browser__open`/`browser__get` returns a JSON object or array over HTTP(S) on a
  non-denylisted URL, it is registered as a read-only GET capability; the
  sequence step stays `executed_via: Browser` but carries capability id, method,
  params, status and body so it can join later browserless replay. The dispatcher
  tracks navigation (`browser__open`, `browser__tab new/open <url>`, simple
  `location.href/assign/replace`) as the current page URL; helper calls without a
  known URL do not start a sequence (no `unknown_origin` pinning).

## Replay Loop

Read-like and write-like operations are classified separately. POSTs whose path
is search/query/lookup/suggest/autocomplete-shaped are read-only unless URL/body
look like checkout, order, payment, cart, admin or mutation work. Unknown side
effects never replay.

`OriginPolicyStore::open` is a cheap handle over a process-wide registry keyed by
policy path (one shared `RwLock` per file; one process owns a data root;
`forget_shared_state` drops it for tests). Origin replay modes:

- `observe_only` — capture/mining only; no passive validation or live replay.
- `validate_only` — passive validation allowed; no live takeover.
- `replay_reads` — read-like capabilities replace browser steps; writes fall back.
- `replay_writes_with_hitl` — reads automatic; writes fail closed to browser
  until the approval-token path exists.
- `replay_trusted_writes` — reads automatic; writes only for Trusted
  capabilities.

The legacy `allow-replay` API maps `true` → `replay_reads`, `false` →
`validate_only`. Origins without a policy replay reads and block writes.

On active browser-to-API takeover, a successful `ApiRunner` replay best-effort
ingests JSON into the scoped projection pipeline; replay stays successful if
projection fails (outcomes logged as Pending-created, Pending-existing,
Ingested, Not-projectable, Migration-rejected, Failed). See
[API Mining Projections](api-mining-projections.md).

### Passive XHR/Fetch validation

With `api_mining.enable_xhr_validation`, a bounded pass replays matched observed
XHR/Fetch traces in the background (never replacing the browser action) and
compares status/body, feeding the same success/failure promotion path. Writes
must pass the router's idempotent URL-pattern gate and the origin must not be
`observe_only`. `api_mining.xhr_validation_max_per_pipeline` caps requests per
run. `GET /api-mining/passive-validation-metrics` reports per-scope counters,
shown apart from active router counters.

### Registry health

The scoped registry audits `registry_index.json` against capability files on
open and rebuilds on stale entries, unindexed files or old schema, so a deleted
or partial capability never looks replayable. The v1.1 index includes
`parent_origin` so overviews group without reloading files.
`GET /api-mining/registry-health` returns indexed vs loadable counts,
stale/unindexed counts, confidence tiers, replayable count, binding and
takeover-ready binding counts (from loadable files, excluding unresolvable
bindings), live `replay_mode` and per-origin inactive reasons — distinguishing
"nothing learned", "not replayable", "policy blocks" and "not bound yet".

Capability Evolution is catalog-only for API-mined replay records: legacy
generated `browser`/`action=api_replay` packs and `magician internal-replay`
skills are suppressed and stale evolved folders removed. Live takeover runs
through the scoped `ApiRouter` + `ApiRunner` at the browser primitive boundary.

## Sequences and workflows (Rail 2)

After every agentic execution the runtime writes a `CapabilitySequence` under
`<scope>/api_mining/<origin_key>/sequences/<id>.json`. Each `SequenceStep` holds
`step_index`, `capability_id` (None for browser-only), `origin`, `concrete_url`,
`method`, request/response fields (`response_body` ≤ 4 KB), `action_binding_id`,
`browser_action_desc`, optional `browser_action` / `browser_arguments`,
`executed_via` (`Browser` | `ApiReplay`) and timing.

When `SequenceStore::count(origin) >= 2`, finalize compiles a linear
`WorkflowGraph` to `workflows/<workflow_id>.json`:

- **Pass 1** — `infer_auto_data_flows` substring-matches later request params
  against earlier responses (skip < 4 chars and `true`/`false`/`null`/`None`;
  dedupe by `(source_step, target_step, target_param)`) → `DataFlow`
  (`AutoMatch`, confidence 0.9).
- **Pass 2** — `compile_with_llm` (`workflow_compilation_system v1.0.1`,
  `LLMOperation::WorkflowCompilation`) validates `step_index`, data-flow order,
  `skip_if.source_step` precedence and `compiled_from_sequence_ids`; one retry.
- **Draft fallback** — longest sequence + auto flows; unmatched params become
  `Literal`.

`WorkflowGraph`: ordered `steps`; `param_sources` =
`Literal | DataFlow | UserInput | SessionAuth`; optional `skip_if`
(`Equals`/`NotEquals`/`IsEmpty`/`IsNotEmpty`); optional `browser_fallback`;
`auth_requirements.login_step_id`; `confidence.workflow_level` starts `Draft`.

`WorkflowReplayEngine` evaluates `skip_if` (fail-open on missing source),
preflights `ApiRunner::can_replay`, resolves params, calls
`ApiRunner::replay_with_reqwest`, and keeps JSON for downstream JSONPath.
Browser-only steps are `ReplayError::BrowserOnlyStep` on the operator path.
`replay_until_browser_fallback` stops at the first failure with a
`browser_fallback` and returns `MixedReplayResult` + `BrowserFallbackRequest`
(task execution does not consume it). Maturity: Draft → Candidate at 1+
successes; → Validated at 3+; → Trusted at 10+ total with failure rate < 10%.

JSONPath (`workflow_replay/jsonpath.rs`): `$.field[.subfield...]`, numeric
indexes (`$.hits[0].objectID`), `$..field` (first match); numbers/booleans
stringified; null is missing; wildcards, filters and slices return `None`
(`JsonPathMiss`).

Not supported: operator-HTTP consumption of browser fallback for per-origin
workflows, workflow invalidation on capability demotion, numeric `SkipOperator`s,
manual workflow compile.

`run_mining_pipeline` is `tokio::spawn`'d via the orchestrator's `weak_self`
(inline when unset) so a completing coordinator is not delayed; each pass
re-reads the recent-trace corpus, so deferral drops nothing.

## Task Recipes (Rail 1)

### Learning

One browser-backed execution is enough. The finalizer collects values from the
accepted outcome summary, artifact previews and output previews, then works
backward from the responses that carried them. Earlier responses feeding later
path/query/header/body params become typed data flows; values from the task
title/description or typed into the browser become named task inputs. Session
material (auth headers, cookies, auth query values, passwords, tokens) is
represented only by a lookup scheme; volatile signatures/nonces keep their
parameter name but never a value, and unresolved volatiles fail closed to the
browser.

- **Complete answer coverage.** Every reported answer value needs response
  evidence; a partial match does not publish (a browser-only answer must not
  vanish from later runs).
- **Asked-field guard.** A run whose answer sits under a key the task never asked
  for is rejected, unless an asked key carries exactly the reported value (the
  agent only named the field differently).
- **Answer recognition.** Decimals compare by value; currency codes and
  hyphenated tokens do not truncate values; a predicate is not mistaken for the
  answer; a field label attaches to the task's head noun; a URL scheme colon is
  not a field separator; counts report their number, not the unit-glued token.
- Reported values keep field identity: two named fields with the same value get
  distinct bindings; unlabelled echoes are deduplicated. Named zero,
  single-digit and boolean answers need a matching response field, not incidental
  occurrence.
- **Literal-key-safe JSONPath.** Keys containing `.` or brackets cannot be
  reinterpreted; evidence under them is skipped (so the task stays on the
  browser). Recipes with mislearned paths need recompilation.
- **Text dependencies** must reproduce the exact downstream value with exactly
  one regex match; patterns keep only bounded delimiter/assignment shape (or a
  fixed whole-body pattern), never captured page content. Replay applies the
  same unique-match rule (examining at most two matches): a second match is
  schema drift.
- An empty-response mutation joins the closure only when it immediately precedes
  an answer read for the same normalized resource path within 15 s (a collection
  path may pair with exactly one extra dynamic segment).
- **Deterministic first.** The optional `RecipeCompilation` LLM pass sees a
  scalar-free projection (methods, hosts, param names, types, order, unresolved
  positions; no task text or templates) and may only derive backward JSON flows,
  never add/remove/rename inputs or rewrite the template. Unknown steps,
  forward/duplicate dependencies and invalid extractors are rejected; each flow's
  JSONPath must reproduce the exact captured value from the original response
  (proof body ≤ 1 MiB; proof values never enter the model or recipe).
- Distinct inputs are kept when requests reuse a parameter name; later values
  cannot rewrite an emitted placeholder. Redaction markers are never dependency
  evidence (redacted auth resolves from the scoped session; other unresolved
  redactions fail closed). Fresh response-derived headers beat stale captured
  ones.
- Equal captured values may share one input; learning emits the placeholder at
  every occurrence in title and description, and matching requires all
  occurrences to agree exactly (changing one falls back to browser learning).
  Repeated-slot recipes cannot use fuzzy matching.
- Compilation needs complete evidence from every listed trace file for the exact
  execution (no fallback to older runs): missing, malformed or unterminated
  records, or exceeded budgets, abort learning rather than dropping a possible
  mutation. Bounds: ≤ 32 trace files, 32 MiB per file, 64 MiB total, 10,000
  events (enforced on actual reads); typed-action recovery ≤ 32 files × 2 MiB,
  256 values × 64 KiB; reported-output mining ≤ 256 KiB per source. The
  dashboard trace reader stays tolerant of corrupt lines.
- Only what the capture can replay compiles: session auth resolves against the
  capture's own predicate, bodies re-encode as forms only when they are forms,
  and a non-GET step with an uncaptured body refuses to compile. Captured
  `Content-Type` and `Accept` (with parameters) are kept as end-to-end metadata
  for HTTP and in-page replay and are part of the request-shape fingerprint;
  `Host`, `Content-Length`, `If-None-Match` are excluded. Recipes that discarded
  representation headers need recompilation.
- **Body representation.** JSON (incl. `+json`) and URL-encoded forms have
  separate substitution paths; `=` in XML, multipart, text or raw GraphQL is not
  a form. Unsupported bodies stay unresolved and fail preflight. Explicit raw
  bodies keep their bytes. Empty bodies stay empty even with a JSON content type.
- Non-JSON answers never persist a captured-body anchor: only a bounded plain
  text body exactly equal to the answer gets a fixed value-free whole-body
  extractor.

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

Recipe, shape-index and task-index publication is one locked atomic transaction.
Replay is serialized per recipe in-process (maturity counters and transport hints
cannot be lost). The append-only run ledger records rail, step outcomes, failure
class, approval request ids, duration and whether a browser continuation taught a
new version — never request values or bodies. It appends O(1) below 4 MiB, then
compacts to the newest complete records (≤ 256 KiB each; API reads ≤ 200 rows).
Persisted run/sequence outcomes are structural only (method, redacted URL
template, status, duration, transport).

### Matching

Task-start lookup ladder:

1. a task-id binding, re-extracting inputs from the current title;
2. a deterministic anchored template match, most-specific first;
3. a shape-only LLM confirmation over the three best token-overlap candidates
   (`match_confirm_threshold`).

Matching binds title and description templates (conflicting values reject
replay); whitespace differences are equivalent. Write-bearing recipes never use
rung 3. Compile-time examples are reused only while the normalized
title/description fingerprint is unchanged (legacy title-only recipes require an
unchanged source fingerprint). Execution-only refinements, substituted launch
goals and runtime plan context bypass replay and learning. A version changed
while lookup waits for its replay lock falls through to normal execution. A miss
falls through unchanged to the agentic browser loop.

Task-start replay passes the same fresh-launch admission as the agentic rail and,
after lookup, lock and approval waits, rechecks the admitted Runtime generation
under the execution lifecycle exclusion (held only for bounded HTTP replay, not
approval waits). Cancelled, paused, replaced or running generations cannot
dispatch.

### Runner

- **Budget.** One total timeout for the DAG (15 s default, clamped to 120 s),
  including after auth healing. Responses stream behind 8 MiB per step and 32 MiB
  per DAG, with header count/byte budgets; up to 64 steps.
- **Whole-graph preflight** before request 1: graph, declared origins, forward
  data-flow and verification edges, answer refs, input names, scalar types and
  sizes (finite JSON-number grammar: `+42`, `01`, `.5` rejected), body-param
  types, URL/header substitutions (invalid bytes, size limits), malformed JSON and
  every known unresolved volatile — so a late failure cannot leave an executed
  prefix. Header names/templates are validated; case-insensitive duplicates are
  rejected; refreshed session auth replaces template headers deterministically.
  Substitution scans only the original template (inserted braces stay literal;
  no recursive expansion); output limits apply while appending.
- **Session.** State resolves for the concrete URL before cookie-backed headers
  and bodies, identically after healing. The in-run cookie jar merges
  name/domain/path identities, keeps deletion/expiry markers and distinct
  same-name cookies; a successful refresh supersedes earlier updates. A per-step
  value-free cookie requirement stops missing cookies from becoming anonymous
  HTTP. Replay validates the record's principal/workspace before resolving
  credentials.
- **Transport ladder** (`api_mining.recipes.transport_ladder`): browserless
  surfaces need `reqwest`; a live browser adds `in_page_fetch`; `browser` is the
  continuation rail. Invalid names are dropped; an invalid ladder is
  browser-only. Anti-bot/network failures may downgrade a **read** to in-page
  fetch (registered only for structurally read-only recipes; never re-running the
  failed rung). HTTP-200 challenge HTML is eligible. Warm replays share a bounded
  idle pool with no cookie jar or scope headers; client init fails closed rather
  than follow redirects. Browser fetch accepts the CLI `data.result` envelope,
  aborts at its deadline and bounds streaming.
- **Continuation.** Schema drift, missing input, policy blocks and exhausted
  reads hand already-fetched step summaries (value-free) to the browser, which
  must not repeat them.
- **Auth healing.** An HTTP 401 may invoke the captured-auth refresh core once
  per origin and at most `api_mining.auth_max_failures` per run, retrying on the
  same transport; auth failures do not demote maturity; unrecoverable auth
  continues in the browser at the failed read. Refresh sessions clean up on
  cancellation.
- Planner aliases bind the current recipe version (recompile retires the old
  alias). Inputs named `recipe_id`, `inputs`, `timeout_secs` or `__*` use a typed
  nested object and never fall back to server-injected values.

### Writes

- **Never retried.** A failed or uncertain write is never retried on another
  transport or handed to a browser. The task-start rail fails the task; a
  planner-invoked recipe returns `write_outcome_uncertain`, `retryable=false`,
  `browser_retry_allowed=false`.
- **Pre-send vs uncertain.** Credential, dependency and request-construction
  failures happen before dispatch; they permit browser recovery only when no
  earlier write was sent (missing credentials are not an uncertain effect).
- **Grants.** Every write is preflighted before the first request. A durable
  grant is keyed by recipe id, step id, capability id and a request-shape
  fingerprint (normalized header templates plus bounded JSON path/type shape
  with exact array positions, or a form-key shape with duplicate counts; fixed
  body semantics including GraphQL documents). The v4 domain makes older broader
  fingerprints ask again. Opaque or unbounded bodies can be approved once but
  never durably. Checkout, payment, login, delete and other denylisted floors
  always ask. `approve_once` exists only in-process (JSON cannot assert it). A
  remembered approval does not mint a one-run approval; the durable grant stays
  revocable until the write is sent; the live enable fence and grants are
  rechecked before later sends. Denial ends `outcome_type=recipe_write_denied`.
- **Verification.** A write's later `verify_with` read must contain at least one
  dynamic request value from each verified write (task inputs or earlier
  response values; not auth, timestamps, literals or header-only values like
  CSRF). JSON proof is an exact type-aware scalar match along one object branch
  (siblings and separate array rows cannot combine), with decimal normalization
  and a bounded borrowed index; non-JSON proof is one multi-pattern scan with
  scalar boundaries (> 65,536 overlapping matches rejected). Missing, oversized or
  ambiguous proof fails closed as schema drift without retry. If every dynamic
  value is empty, the write stops pre-send. Empty successful POSTs stay available
  for causal analysis; a 204 alone is not telemetry proof.

### Settlement and receipts

Before a write-bearing recipe sends HTTP it publishes an execution-bound,
scope-HMAC-sealed intent under `restricted/task_recipe_replays/`. The
cancellation exclusion covers intent publication, bounded replay and settlement
(never human approval). A terminal result commits to the receipt, then the
Artifact outcome, then Runtime's terminal state, so interrupted settlement is
found by the active-Runtime startup scan; completion, denial and uncertain
writes all settle Runtime.

Recovery restores a saved answer without rerunning. An intent without a result
fails as `recipe_replay_interrupted` with no automatic retry (verify remote state
first). Direct/manual browser launches check this receipt under admission, so
bypassing the matcher cannot re-run the mutation. A `released` marker permits
browser fallback only after the runner proved no mutation was sent. Receipt
corruption or publication failure is not a cache miss. Receipts work with mining
disabled or the recipe deleted; reads/writes are bounded to 64 MiB. Direct
recipe/tool calls keep their caller-owned contract.

Statistics-persistence errors or a mid-run disable never replace a
completed/uncertain outcome with a generic retryable error; responses attach
`state_persistence_warning`.

### Outcomes and publication

Successful replay ends an ordinary task with `outcome_type=recipe_replay` and
zero agentic iterations. A read-only recurring Monitor hands the replay answer to
the agentic interpretation rail, which must still emit the run's
`monitor_run_result` artifact (without repeating requests); write-bearing Monitor
recipes stay on the agentic rail.

Candidate-or-better recipes publish scoped `recipe__*` compiled packs. Recompiling
to Draft unpublishes stale packs and skills; prior versions stay for audit but are
never auto-retried after uncertainty. Empty or unknown-side-effect versions are
never published or matched as read-only. Emitted skills bind
`published_only=true` and the exact version; operator direct replay may exercise a
Draft. Direct HTTP replay never opens HITL: an ungranted write returns 409 (launch
via a task), and an unverifiable sent write returns 409 with `success: false`.
Compilation holds the scope publication lock and rechecks the master switch
before and after its atomic write, rolling back if it flipped.

**Verification worker** (opt-in, started only when enabled at boot): waits one
full interval, rereads config and the scope switch each run, and replays only
Trusted read-only recipes with recurring-Monitor provenance
(`compiled_from.monitor_revision`) using the Monitor's inputs, reqwest only, a
15 s budget, no browser/auth-healer/HITL/projection/sequence. ≤ 32 recipes per
cycle process-wide, oldest-replayed first; outcomes are recorded with
`rail_ended=verification` and the planner pack refreshed.

**Events.** Canonical `recipe.replay` events carry a content-free `payload.kind`
(start, step completion/failure, auth heal, transport downgrade, approval,
fallback, completion, recompilation), derived from shared
`RuntimeAgentEventType` identifiers. Chat coalesces them per execution into one
cue. The API Mining Recipes tab reads the overview and lazy run-ledger endpoint.

### Relevance and noise

Capabilities are `answer_bearing`, `dependency`, `first_party_api`,
`third_party_api`, `telemetry` or `unclassified`. Structural classification runs
while mining; recipe use promotes answer and dependency steps and maintains
`used_by_recipe_ids`. Telemetry is counted but never promoted. Unused endpoints
age out of the default overview after 30 days (code constant) but remain
inspectable. Raw-trace telemetry aggregation is cached 5 s, bounded to 128 scopes,
invalidated on purge.

### Master switch

`api_mining.enabled` is the process ceiling; `<scope>/api_mining/settings.json`
may turn a scope off but cannot override a process-level off. Every capture,
auth drain, sequence/action persist, compile, task-start replay, in-page
continuation, generated pack and mutating API entry reads the effective state;
the mid-run router binds a disabled implementation while off. Reads and purge
stay available. Off means nothing new is captured and repeated tasks use the
browser; learned data stays until the typed `delete learned data` operation.
Scope toggles evict that scope's planner catalog; process-ceiling changes and
config reloads evict all.

**Purge.** Origin purge removes every cross-origin recipe containing the origin
(as a unit), its ledger and bindings, origin grants, `recipe__*` catalog rows,
emitted skill folders, projection metadata and that origin's projection rows.
Full disable-and-purge removes all of those before deleting the mining tree,
clears captured auth and projections, drops process caches and invalidates the
planner snapshot, so re-enable cannot resurrect a tool. Compiler publication and
destructive lifecycle share one scope lock; purge waits for every per-recipe
replay lock (even malformed records). Origin purge validates every locked recipe
strictly first and fails visibly on corrupt state; full purge ignores
unaddressable invalid filenames so they cannot veto deletion.

### Configuration

| Key | Default | Effect |
|---|---:|---|
| `api_mining.enabled` | `true` | process-wide hard ceiling |
| `api_mining.recipes.enabled` | `true` | Task Recipe compile and replay |
| `api_mining.recipes.compile_after_first_run` | `true` | compile in the first browser execution finalizer |
| `api_mining.recipes.match_confirm_threshold` | `0.85` | minimum LLM rung confidence |
| `api_mining.recipes.transport_ladder` | `reqwest, in_page_fetch, browser` | read-only downgrade order |
| `api_mining.recipes.unknown_origin_default` | `reads_auto_writes_by_grant` | unknown-origin recipe policy |
| `api_mining.auth_recovery_mode` | `hybrid` | portable captured-auth recovery strategy |
| `api_mining.auth_max_failures` | `3` | total heal cap per recipe run |
| `api_mining.recipe_verification.enabled` | `false` | opt in to bounded reqwest verification of Trusted, read-only, monitor-owned recipes; no background traffic while off |
| `api_mining.recipe_verification.interval_secs` | `21600` | full initial delay and cadence for the optional verification worker |

### Endpoints

All under `/api/magician/v2`, scoped by the authenticated principal/workspace.
Full route table: [API Explorer](api-explorer.md).

| Method + path | Purpose |
|---|---|
| `GET /api-mining/overview` | value-free recipe metadata, site-grouped relevant capabilities, auth metadata, grants, and counters |
| `GET /api-mining/registry?relevance=...&include_hidden=...` | relevance-filtered capability registry |
| `GET /api-mining/recipes` | bounded, value-free recipe metadata list (input names/types/sources; no captured examples) |
| `GET /api-mining/recipes/{recipe_id}` | complete recipe without session values |
| `GET /api-mining/recipes/{recipe_id}/runs?limit=N` | newest-first run ledger, limit capped server-side |
| `POST /api-mining/recipes/{recipe_id}/replay` | direct read/granted replay; ignores wire approval markers and returns 409 for an approval requirement |
| `GET /api-mining/recipe-metrics` | lookup, replay, fallback, approval, grant, and auth-heal counters |
| `GET /api-mining/settings` | effective/process/scope state and deciding layer |
| `PUT /api-mining/settings` | update the scope override or owner-only process ceiling live |
| `POST /api-mining/settings/disable-and-purge` | typed-confirmation off-then-delete operation |
| `GET /api-mining/sequences/{origin_key}` | `SequenceMetadata` list (no step bodies) |
| `GET /api-mining/sequences/{origin_key}/{sequence_id}` | full sequence; 404 if missing |
| `GET /api-mining/sequence-metrics` | `started` / `finalized` / `with_browser_only_steps` (process-local) |
| `GET /api-mining/workflows/{origin_key}` | `WorkflowMetadata` list |
| `GET /api-mining/workflows/{origin_key}/{workflow_id}` | full `WorkflowGraph` |
| `GET /api-mining/workflow-metrics` | compile started/succeeded/failed/fell-back-to-draft |
| `POST /api-mining/workflows/{origin_key}/{workflow_id}/replay` | `ReplayInputs { user_inputs }` → `ReplayResult`; persists stats + maturity |
| `GET /api-mining/replay-metrics` | started/succeeded, three failure-mode counters, three promotion counters |

## Related Docs

- [API Explorer](api-explorer.md)
- [Captured Auth For API Replay](auth-capture-and-replay.md)
- [V2 API Guide](v2-api-guide.md)
- Sequence capture plan
- Compilation plan
- Replay plan
