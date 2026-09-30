# Live Thinking Map — backend spine (canonical domain, reducer, store)

Rust spine: `magician-surfaces/src/thinking_map/`. Domain types live in
`magician::magician_v2::{thinking_map_models, thinking_map_operations}` and are
re-exported from the surfaces crate. REST lives in
`magician-api/src/thinking_maps_api.rs` (mounted via `configure`). Tutor digest
builder: `magician/src/magician_v2/tutor_map_context.rs`. Crash repair is
spawned from `magician-bin/src/main.rs`.

Finalized speech continuously updates a **typed, revisable model of the user's
thinking**: ideas / facts / questions / decisions / options / risks / actions /
metrics / assumptions / evidence / groups are first-class nodes; later speech can
correct, supersede, reconnect, or regroup earlier nodes. The guiding principle is
**"the model proposes bounded operations; deterministic code owns state."** An LLM
never emits or replaces the whole board — it emits a bounded list of typed
operations that a deterministic reducer validates and applies.

The feature is unconditionally routed (no config flag). See the archived
implementation plan
and design.

## Module layout

| File | Responsibility |
|---|---|
| `magician_v2::thinking_map_models` | Domain types + serde wire contracts: `ThinkingMap`, `ThinkingNode`, `ThinkingEdge`, `Clarification`, `RestructureProposal`, and the bounded enums (`NodeKind`, `EpistemicState`, `AssertionOrigin`, `EdgeKind`, `MapLifecycle`, `ThinkingMapSource`, …). Re-exported as `thinking_map::models`. |
| `magician_v2::thinking_map_operations` | `MapOperationEnvelope` + the 22-variant `MapOperation` protocol + `OperationActor`. `MapOperation::is_model_accessible()` exposes the LLM-allowed subset. Re-exported as `thinking_map::operations`. |
| `validation.rs` | Authority matrix (actor eligibility + origin consistency) and structural helpers. Errors carry only ids / enum tags / fixed reason codes — never raw model or user text. |
| `reducer.rs` | `apply_envelope()` — the deterministic, atomic, idempotent apply — plus `semantic_hash()`. |
| `errors.rs` | `ThinkingMapError` (typed validation/authority failures). |
| `store.rs` | `ThinkingMapStore` — scope-owned durable persistence over `ArtifactV2Workspace`. |
| `replay.rs` | Deterministic replay, restore-as-branch, and crash-recovery startup repair. |
| `export.rs` | Pure, deterministic Markdown export (`render_markdown(&map, &opts)`) with provisional/superseded inclusion controls. |
| `context.rs` / `interpreter.rs` / `llm_router_adapter.rs` / `consolidation.rs` | Bounded context, constrained interpreter, `RouterInterpreterLlm`, restructure consolidation. |
| `coordinator.rs` | Ambient session coordinator. |
| `magician_v2::tutor_map_context` | Tutor digest builder + process-local binding registry. Re-exported as `thinking_map::tutor_context`. |

## Domain model

A `ThinkingMap` is a revision-numbered document of `BTreeMap`-keyed nodes, edges,
clarifications, and proposals. Each node carries an **epistemic state**
(`provisional` / `asserted` / `confirmed` / `contradicted` / `rejected` /
`resolved` / `superseded`) and an **assertion origin** (`owner_spoken` /
`participant_spoken` / `owner_edited` / `imported_source` / `model_inferred` /
`system_derived`) — state and origin are independent, and model-inferred content
is always distinguishable from owner-asserted content. Deletions are **tombstones**
(soft, history-preserving), never hard removals.

## Operation protocol & authority

Every change is a `MapOperationEnvelope` — a batch of operations applied
atomically against a known `base_revision`, authored by an `OperationActor`
(`owner` / `participant` / `model` / `trusted_system` / `imported`). Authority is
enforced in two layers:

1. **Actor eligibility** — a fixed matrix decides whether an actor may emit an op.
   Owner-command intent (confirmations, physical positioning, promotion links,
   speaker rename) is owner-only; `set_epistemic_state → confirmed` is owner-only;
   participant and imported content is **data, never command**.
2. **Origin consistency** — a created node/edge's claimed `assertion_origin` must
   match the acting actor (a model cannot mark its inference as owner content).

**Restructure proposals** (`propose_restructure` → owner `confirm_restructure`)
are bounded by their **author's** authority, not the confirming owner's: inner ops
are validated against the proposing actor at propose time and re-applied under the
stored proposer's authority on confirm (never blanket owner). Nested
meta-restructure ops are forbidden.

## Reducer guarantees (`apply_envelope`)

Pure and deterministic — no wall-clock, no IO, no randomness; the RFC3339
`applied_at` timestamp is injected by the caller.

- **Atomic** — the whole envelope validates against a cloned candidate; a single
  invalid op rejects the entire envelope with zero mutation.
- **Idempotent** — a bounded ledger (last 512) keyed by `envelope_id` and
  `idempotency_key` replays an already-applied envelope as `IdempotentReplay`
  with its original resulting revision; the ledger is excluded from the semantic
  hash.
- **Optimistic-concurrency** — a mismatched `base_revision` returns
  `RevisionConflict`.
- **Invariant-checked** — referential integrity, confidence bounds, tombstone
  cascade to incident edges, acyclicity for `grouped_under` / `depends_on` only,
  parent-chain cycles, global promotion-link uniqueness, and position-lock
  respect.
- **`semantic_hash`** — a stable, order-independent SHA-256 over the durable
  board (revision, lifecycle, nodes, edges, clarifications, proposals),
  excluding ephemeral view state and the idempotency ledger.

## Durable store (`ThinkingMapStore`)

Scope-owned, mirroring the existing `ArtifactV2Workspace` storage stack.

```text
scopes/<principal>/<workspace>/thinking_maps/<map_id>/
  manifest.json         # atomic head record: lifecycle/title/source + latest_revision/sequence/hash
  snapshot.json         # atomic materialization of the current ThinkingMap
  events.jsonl          # append-only authoritative event log (one MapEvent per applied envelope)
  utterance_refs.jsonl  # reserved
  exports/              # reserved
```

`apply_and_persist` loads the current map, runs the reducer, and — only on
`Applied` — **appends + fsyncs the event to `events.jsonl` FIRST**, then writes
the snapshot, then the manifest. The durable log always leads the snapshot: a
crash in that window leaves an event whose `resulting_revision` exceeds the
older snapshot — recoverable by replay, never the reverse. Reducer rejections
map to `Validation` with no disk write; idempotent replays write nothing. All
ids are scope-safety validated (`is_safe_scope_id`). A process-wide keyed async
lock over the normalized storage root, scope, and map id serializes the
load→reduce→persist section across independently constructed store values while
independent maps proceed concurrently.

## Replay, restore, and repair (`replay.rs`)

- **Deterministic replay** (`replay_to_sequence` / `replay_to_utterance` /
  `replay_to_time`) reconstructs any past state by folding the stored envelopes
  through the reducer from the revision-0 base. Each fold step **re-verifies
  the reproduced revision + `semantic_hash` against the value the log
  recorded**, so replay is byte-equivalent or it fails `Corrupt`.
- **Restore-as-branch** (`restore_as_branch`) forks a *new* map at a past
  sequence with a fresh idempotency ledger and `branched_from_*` provenance;
  the source map and its log are never touched.
- **Startup repair** (`startup_repair`) rolls the snapshot/manifest **forward**
  to the last durable event after a crash. Committed events are authoritative
  and are never rolled back, dropped, or truncated; only a non-parseable
  **trailing** line is quarantined. Historical duplicate-sequence groups are
  repaired only when every record in a group carries the same non-empty
  idempotent identity, operation, base revision, and resulting revision.
  Divergent operations, an ambiguous terminal state, excessive branching,
  interior corruption, a sequence gap, or a snapshot ahead of the log are
  reported as `Corrupt`.

`magician-bin` spawns `ThinkingMapStore::startup_repair_all()` during startup.
It enumerates every scope on disk, every map directory in each scope —
**including soft-deleted tombstones** — and runs `startup_repair` on each.

Why it has to run: `apply_and_persist` writes append → snapshot → manifest, so a
crash inside that window leaves a torn trailing line in `events.jsonl` (from
then on `events_after` fails permanently) or a stale
`manifest.latest_sequence` (the next envelope mints a duplicate sequence).

One damaged map never stops the sweep. The pass reports `scopes_scanned /
scopes_unreadable / maps_scanned / maps_repaired / maps_quarantined /
maps_deduplicated / maps_skipped_concurrent_write / maps_failed /
maps_quarantined_ambiguous`. A clean boot logs nothing. `maps_failed > 0` means
those maps still need a human.

**Ambiguous duplicate history.** When a duplicate group leaves
more than one replay-valid terminal branch, the repair first uses the persisted
snapshot as the tie-break: it keeps the single branch that produced the
snapshot's exact state at the snapshot's revision, because that is the state
the map served. If the history stays ambiguous, the sweep writes
`repair-quarantine.json` beside `events.jsonl`, counts the map under
`maps_quarantined_ambiguous`, and does not retry it on later boots; the
snapshot keeps serving reads and the log will not replay. Delete the marker to
retry after a manual repair.

The sweep is spawned detached (60s timeout) so it never blocks the server from
binding. The process-wide map lock covers every `ThinkingMapStore` instance
over the same normalized root. It cannot coordinate a second OS process, so
`startup_repair` also re-reads the raw log and compares it byte-for-byte with
what it used for the decision immediately before its first write. If the log
moved, it writes nothing and reports `skipped_concurrent_write`.

## Interpreter

`context.rs` + `interpreter.rs` turn a *finalized* speech utterance into a
bounded, **model-authored** `MapOperationEnvelope`.

- **Bounded context** — `build_context` selects a small, capped slice of the
  board (focus node, recent nodes, lexical matches, focus neighbors, open
  clarifications, plus an aggregate digest of counts by kind/state). The whole
  board is never embedded. Interactive clients may send `focus_node_id` on
  `/interpret`; the server validates that it names a live node and applies it
  to this request's context snapshot. It does not depend on, or overwrite,
  another surface's shared-view selection.
- **Constrained proposal schema** — the LLM never emits raw internal
  `MapOperation` JSON. It emits a `ProposedOperation` whose schema offers ONLY
  the model-safe subset (add / update / set-kind / set-state / tombstone /
  connect / clarify). Owner-only operations have no variant.
- **Deterministic translation** — `translate` mints ids, stamps every created
  node/edge `assertion_origin = model_inferred` and nodes `epistemic_state =
  provisional`, clamps confidence, attaches a `SourceRef` to the utterance, and
  resolves intra-batch temp-ids. A model `set_epistemic_state → confirmed` is
  dropped; self-loop connects are dropped.
- The result is an `actor = Model` envelope the reducer authority-validates as
  the final gate. Its `idempotency_key` is `interp:<utterance_id>`.
- The LLM call is behind a mockable `InterpreterLlm` trait. The production
  adapter `RouterInterpreterLlm` wraps `OperationLlmRouter` via
  `LLMOperation::Other("thinking_map_interpret")`, routed to a dedicated
  config profile `op-thinking-map-interpret`, mapped by default (an isolated OpenAI
  GPT-6 Luna strict-JSON profile — never a shared memory/chat profile).
  `POST /thinking-maps/{id}/interpret` runs the interpreter against the global
  router and applies the result. LLM calls are metered via
  `OperationLlmTelemetryContext` under the `thinking_map_interpret` operation
  label.
- **Progress narration** (`InterpretStage` + `InterpretProgressSink`) —
  `interpret()` narrates `preparing` (with the bounded context's live-node
  count) → `loading_context` → `facilitating` → `parsing` → `shaping`. The
  REST handler implements the sink over the transport bus as
  `ThinkingMapInterpretProgress { map_id, utterance_id, stage, detail?,
  node_count? }` events beside `ThinkingMapUpdated`, and emits the terminal
  `idle` itself once the run settles. Best-effort by contract. `utterance_id`
  is what lets a client claim narration for the run *it* started. The ambient
  coordinator passes `NoProgress` deliberately. The stage vocabulary is closed
  and bound to real steps. Consumers: web (strip above the brainstorm
  composer), iOS (`ThinkingMapIntelligenceProgress`), Android
  (`ThinkingProgress` + `IntelligenceStrip`).

## Ambient session coordinator (`coordinator.rs`)

`ThinkingMapSessionCoordinator` subscribes read-only to the
`RuntimeTransportBroadcaster` and holds a registry `source_session_id →
(map_id, principal, workspace)`. For each `ChatMessageReceived` event with
`direction == User` in a **registered** session, it extracts the text, runs
`interpret` (`continue_thinking`) against the global router, and
`apply_and_persist`s the model-authored ops — idempotent by chat-message id.
Errors are logged and swallowed; the loop never dies. Attach/detach via
`POST /thinking-maps/{id}/sessions` (`{source_session_id}`) and
`DELETE /thinking-maps/{id}/sessions/{source_session_id}`. Constructed +
spawned once in the server bin; shared to the API via `app_data`.

**Session matching (`resolve_binding`).** A registered id is matched against
the event's `session_id` **first**, then falls back to the message's
`presence_session_id`. The voice orchestrator emits `ChatMessageReceived` with
`session_id` = the *chat* session id it derives server-side but stamps the
originating *media/voice* session id onto `presence_session_id`. A voice
client that only knows its media session id attaches **that** id and is still
matched. The id that matched becomes the utterance's thread id.

**Coverage (`is_mappable_turn`):** voice / chat-dictation sessions emit
`ChatMessageReceived` with `direction: User` and are auto-mapped. Live
meeting-transcript lines are display-only `direction: System` messages tagged
`source_surface: "meeting-transcript"`
(`ChatService::persist_meeting_transcript_line`, speaker prefixed into the
text) — the coordinator maps exactly those System turns too. The opt-in is the
attach itself; unregistered meeting sessions stay ignored, other System
messages never map, and Assistant turns never map.

**Realtime push (`ThinkingMapUpdated`).** Every successfully applied envelope
— owner `/operations`, `PATCH`, `/interpret`, a consolidation decision, and
the coordinator's ambient auto-map — emits a `ThinkingMapUpdated { map_id,
principal, workspace, revision }` onto the transport bus (`emit_map_updated`
in `thinking_maps_api.rs`; the coordinator emits after `apply_and_persist`,
skipping idempotent replays). It is a lightweight change NOTICE, not a data
channel: clients re-fetch the authoritative map on receipt. Scope-filtered
strictly in `event_visible_to_scope`; taxonomy `(Observability, Info)` in
BOTH tables (`make event-taxonomy-codegen`). The web detail page subscribes
via the v2 WebSocket and `pollNow()`s on a matching notice; iOS subscribes
during Listen mode too (`ThinkingMapRealtime` over `/realtime/ws`), keeping
only a slow 20s safety poll behind the push.

## Governed promotion

`POST /thinking-maps/{id}/nodes/{node_id}/promote` `{target: task|memory,
confirm?}` turns a node into a durable Magician object: tombstoned/rejected/
superseded/contradicted content NEVER promotes; owner-asserted content
promotes by default; anything else (model_inferred/provisional,
participant/imported) requires `confirm: true`, which is recorded as an owner
assertion (`set_epistemic_state → asserted`) in the same envelope. IDEMPOTENT
— an existing link of that kind returns the existing object
(`promoted: false`); the reducer's global promotion-link uniqueness backs
this. `task` creates via the v3 task service (absent → 503
`promotion_unavailable`) with map/node/origin provenance in the description;
`memory` creates a review-gated learning candidate targeting `user.knowledge`.
The map records the link via `link_promoted_object` and emits
`ThinkingMapUpdated`. The whole flow lives in `promote_node_to_target` (REST
and Today share it). The created task's description carries
`Thinking map node source: <map_id>:<node_id>` used by Today reconciliation.

## Today source-action adapter (`thinking_map_action`)

Acknowledged, unlinked action nodes surface on **Today** with a state-aware
**Create task** action (`feed/action_adapter.rs` + the projection in
`magician-api/src/feed_api.rs`).

- **Projection (read-side only):** every ACTIVE map in the scope is scanned;
  a node qualifies only when it is action-kind, non-tombstoned, and
  owner-asserted (origin `owner_spoken`/`owner_edited` AND state
  `asserted`/`confirmed`). Candidates land in Follow-ups
  (`source_kind: thinking_map_action`) with map/node provenance and
  canonical navigation (`/thinking-maps/{map}?node={node}`).
- **Create task = the promote flow:**
  `POST /today/items/{id}/actions/create_task` calls the same
  `promote_node_to_target`. A retry (or a candidate already promoted
  out-of-band) resolves the existing task (`reused_task: true`); a node whose
  state regressed since projection gets an explicit 409
  (`confirmation_required`/`not_promotable`).
- **Suppression for EVERY task status:** once a node carries a `task`
  promoted_ref, the candidate is suppressed permanently. Meeting-style
  reconciliation additionally drops candidates whose task exists but whose
  link commit was interrupted (matched by the description marker, explicit
  `linked_task_id`, or same `thinking-map-{map_id}` thread + normalized
  title).
- Store read failures degrade to an empty projection.

## Tutor adapter

Selected-map/cluster grounding for Personal Tutor **without touching Tutor
narration/storyboard ownership**.

- **Digest builder** — `build_tutor_map_context(map, selected_node,
  budget_chars)` is pure + deterministic: with a selected node it renders
  that node's CLUSTER (the node + one-hop neighbors + the edges among them);
  without one (or when the id is unknown/tombstoned) a recency-ordered
  whole-map overview. Bounded on every axis. `model_inferred` content is
  tagged `[AI-suggested]`; participant/imported content is tagged too;
  tombstoned/rejected/superseded content NEVER renders as current truth. The
  digest explicitly declares itself reference material, not instructions.
- **Binding registry** — `POST /thinking-maps/{id}/tutor-context`
  `{session_id, node_id?}` snapshots the digest into a process-local,
  TTL-bounded (30 min) registry keyed by `(principal, workspace,
  chat_session_id)`; re-register overwrites;
  `DELETE /thinking-maps/{id}/tutor-context/{session_id}` clears
  (idempotent).
- **Tutor wiring** — `TutorRunStore::start_run` consults the registry and
  stamps the digest onto the new run's optional
  `TutorRun.thinking_map_context` field (serde-skipped when absent). Both
  tutor start paths deliver it: the forced preflight serializes the run into
  its `start_tutor_run` tool result, and the LLM-invoked tool surfaces it
  top-level (`thinking_map_context`) next to `visual_entity_context`.

The web client's "Ask Tutor" inspector button drives this flow (see
`docs/components/unified-ui/thinking-map.md`).

## Live semantic eval (`scripts/eval-thinking-map-live.py`)

Fixture utterances against the RUNNING server's real `/interpret` (live LLM),
SHADOW-ONLY on scratch maps that are soft-deleted afterwards. Measures
envelope validity, node precision + keyword recall (recall is a diagnostic
proxy: an in-place correction legitimately adds zero new nodes),
correction-target accuracy, duplicate/churn, unsupported assertions, authority
violations, clarification restraint, latency, and 5-repeat stability on the
release-critical subset. **Gates (non-zero exit): correction-target accuracy
≥95%, zero authority violations, envelope validity ≥95%.** Run:
`make eval-thinking-map-live` (~20 mini-class LLM calls; `TM_EVAL_RUNS=1` for
an 8-call smoke); report at `coverage/evals/thinking-map/latest.html`.

## Restructure consolidation (`consolidation.rs`)

`consolidate` reads the **bounded whole-board digest** and asks the model to
propose a reorganization using the **same model-safe `ProposedOperation`
subset**. The proposed ops are **not applied directly**: they're translated
(minting ids, stamping `model_inferred`) and wrapped in a
`RestructureProposal` authored by the **Model** actor, staged via
`propose_restructure` as a **pending** proposal. Nothing on the board changes
until the **owner confirms** — a separate deterministic `confirm_restructure`
re-applies the inner ops under the stored proposer's (Model) authority.
Reuses the interpreter's `InterpreterLlm` trait, `translate`, `build_context`,
and the `RouterInterpreterLlm` adapter (same `thinking_map_interpret`
op/profile; consolidation LLM cost is metered under that label).

## REST API (GA)

`magician-api/src/thinking_maps_api.rs` exposes the store over HTTP under
`/api/magician/v2/thinking-maps`. Routes are **unconditionally mounted**.

| Method + path | Purpose | Success |
|---|---|---|
| `POST /thinking-maps` | create a map (`{title, source?, map_id?}`) | 201 map |
| `GET /thinking-maps` | list visible scope summaries, most-recently-updated first (`updated_at` desc, `map_id` tie-break); deleted tombstones are omitted | 200 `[MapSummary]` |
| | Each `MapSummary` also carries an OPTIONAL `node_preview` — `{nodes:[{node_id, parent_id?, kind, suggested, title}], edges:[{from, to}]}` — a bounded thumbnail (first 10 live nodes + their parent→child branch edges). Omitted for empty maps. | |
| `GET /thinking-maps?limit=N&offset=M[&lifecycle=deleted]` | one server page (`limit` clamped 1..=200); absent lifecycle returns the visible list, while an exact lifecycle selects that view before `total` and slicing | 200 `{maps, total, offset, limit}` |
| `GET /thinking-maps/{id}` | load current map | 200 map / 404 |
| `DELETE /thinking-maps/{id}?confirm=permanent` | permanently remove an already-soft-deleted map directory and clear its ambient/tutor bindings; fails closed for a missing confirmation or non-deleted lifecycle | 200 `{deleted, map_id, detached_sessions, cleared_tutor_contexts}` / 400 / 409 / 404 |
| `POST /thinking-maps/{id}/operations` | apply an owner envelope | 200 `applied` \| `idempotent_replay` |
| `GET /thinking-maps/{id}/events?after_seq=N` | event-log tail | 200 `[MapEvent]` |
| `GET /thinking-maps/{id}/replay?at_seq=N` | deterministic replay | 200 map |
| `GET /thinking-maps/{id}/export/markdown?include_provisional=&include_superseded=` | deterministic Markdown export | 200 `text/markdown` |
| `POST /thinking-maps/{id}/restore` | restore-as-branch | 201 branch |
| `POST /thinking-maps/{id}/interpret` | interpret an utterance via the LLM (intent `continue_thinking`/`break_open`, optional live `focus_node_id`), then apply | 200 `applied`\|`idempotent_replay`\|`no_operations`; invalid focus 400 |
| `POST /thinking-maps/{id}/consolidate` | LLM proposes a board reorganization → staged as a pending owner-confirmable proposal | 200 `applied`\|`no_operations` |
| `POST /thinking-maps/{id}/proposals/{pid}/decision` | owner confirm/reject a pending restructure proposal | 200 `applied` |
| `PATCH /thinking-maps/{id}` | owner-only rename / lifecycle change | 200 `applied` |
| `POST /thinking-maps/{id}/tutor-context` | register a map→tutor grounding binding for a chat session (`{session_id, node_id?}`) | 200 binding + digest |
| `DELETE /thinking-maps/{id}/tutor-context/{session_id}` | clear the binding (idempotent) | 200 `{cleared}` |
| `POST /thinking-maps/{id}/nodes/{node_id}/promote` | governed promotion to task or memory | 200 |
| `POST /thinking-maps/{id}/sessions` | attach an ambient source session | 200 |
| `DELETE /thinking-maps/{id}/sessions/{source_session_id}` | detach | 200 `{detached}` |

Every handler resolves an explicit `(principal, workspace)` scope via
`resolve_required_scope`; a missing scope is `400 missing_scope`. The
operations endpoint always stamps `actor = OperationActor::Owner { principal }`
from the resolved scope and forces `map_id` to the path parameter. Typed
`ThinkingMapStoreError` maps to HTTP: `NotFound`→404, `AlreadyExists`→409,
`Validation`→400, a stale `base_revision` distinctly as **409
`revision_conflict`**, `InvalidId`→400, `Corrupt`/`Io`→500; bodies never leak
internal detail beyond `to_string()`.

## Export

`thinking_map/export.rs` renders a map to Markdown via the pure
`render_markdown(&map, &opts) -> String`. The render is **deterministic**: it
reads only the stored map, and every collection it walks is a `BTreeMap`.
Layout: `# title` + a source/lifecycle/revision line, a **Board** tree walked
over the `parent_id` hierarchy (indented bullets carrying the label, a `kind`
tag, an origin/state tag for non-owner-asserted content — `✦ AI-suggested` for
model-inferred — plus the epistemic state when it isn't the plain `asserted`
baseline, and `detail_markdown` as a nested blockquote), then **Connections**,
**Open clarifications**, and **Pending proposals**, each omitted when empty. A
node whose parent is filtered out surfaces at the root, and `parent_id` cycles
still render each member exactly once.

Inclusion rules (`ExportOptions`): `include_provisional` (default **true**);
`include_superseded` (default **false**); **tombstoned content is never
exported**. Edges render only when both endpoints are visible.

Over HTTP: `GET /thinking-maps/{id}/export/markdown?include_provisional=&include_superseded=`
returns `text/markdown; charset=utf-8`; query flags parse leniently
(absent/empty/unknown values fall back to the defaults). On the web, the
canvas toolbar's **Export** dropdown
(`ui/unified-ui/src/lib/thinkingMaps/ThinkingMapCanvas.svelte`) offers **SVG**,
**PNG**, and **Markdown** (`exportMarkdown(id, opts)` in
`$lib/thinkingMaps/api.ts`).

## Testing

`cargo test -p magician-surfaces --lib thinking_map` (spine, including
`thinking_map::export`), `cargo test -p magician-api thinking_maps_api` (REST),
`cargo test -p magician --lib tutor_map_context` and `--lib tutor` (Tutor
digest and `start_run` attachment).
