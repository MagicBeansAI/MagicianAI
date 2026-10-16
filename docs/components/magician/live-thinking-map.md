# Live Thinking Map — backend spine (canonical domain, reducer, store)

Rust spine: `magician-surfaces/src/thinking_map/`. Domain types live in
`magician::magician_v2::{thinking_map_models, thinking_map_operations}` and are
re-exported from the surfaces crate. REST is
`magician-api/src/thinking_maps_api.rs` (mounted via `configure`). Tutor digest
builder: `magician/src/magician_v2/tutor_map_context.rs`. Crash repair is spawned
from `magician-bin/src/main.rs`.

Finalized speech keeps updating a **typed, revisable model of the user's
thinking**: ideas, facts, questions, decisions, options, risks, actions, metrics,
assumptions, evidence and groups are nodes, and later speech can correct,
supersede, reconnect or regroup them. The rule is **"the model proposes bounded
operations; deterministic code owns state."** An LLM never emits the whole
board; it emits a bounded list of typed operations that a deterministic reducer
validates and applies.

The feature is always routed (no config flag). See the archived
implementation plan
and design.

## Module layout

| File | Responsibility |
|---|---|
| `magician_v2::thinking_map_models` | Domain types and serde wire contracts: `ThinkingMap`, `ThinkingNode`, `ThinkingEdge`, `Clarification`, `RestructureProposal`, and the bounded enums (`NodeKind`, `EpistemicState`, `AssertionOrigin`, `EdgeKind`, `MapLifecycle`, `ThinkingMapSource`, …). Re-exported as `thinking_map::models`. |
| `magician_v2::thinking_map_operations` | `MapOperationEnvelope`, the 22-variant `MapOperation` protocol and `OperationActor`. `MapOperation::is_model_accessible()` gives the LLM-allowed subset. Re-exported as `thinking_map::operations`. |
| `validation.rs` | Authority matrix (actor eligibility + origin consistency) and structural helpers. Errors carry only ids, enum tags and fixed reason codes, never raw model or user text. |
| `reducer.rs` | `apply_envelope()` (deterministic, atomic, idempotent apply) and `semantic_hash()`. |
| `errors.rs` | `ThinkingMapError` (typed validation/authority failures). |
| `store.rs` | `ThinkingMapStore`, scope-owned durable persistence over `ArtifactV2Workspace`. |
| `replay.rs` | Deterministic replay, restore-as-branch and crash-recovery startup repair. |
| `export.rs` | Pure, deterministic Markdown export (`render_markdown(&map, &opts)`). |
| `context.rs` / `interpreter.rs` / `llm_router_adapter.rs` / `consolidation.rs` | Bounded context, constrained interpreter, `RouterInterpreterLlm`, restructure consolidation. |
| `coordinator.rs` | Ambient session coordinator. |
| `magician_v2::tutor_map_context` | Tutor digest builder and process-local binding registry. Re-exported as `thinking_map::tutor_context`. |

## Domain model

A `ThinkingMap` is a revision-numbered document of `BTreeMap`-keyed nodes,
edges, clarifications and proposals. Each node has an **epistemic state**
(`provisional` / `asserted` / `confirmed` / `contradicted` / `rejected` /
`resolved` / `superseded`) and an independent **assertion origin**
(`owner_spoken` / `participant_spoken` / `owner_edited` / `imported_source` /
`model_inferred` / `system_derived`), so model-inferred content is always
distinguishable from owner-asserted content. Deletions are **tombstones**, never
hard removals.

## Operation protocol & authority

Every change is a `MapOperationEnvelope`: a batch applied atomically against a
known `base_revision` by an `OperationActor` (`owner` / `participant` / `model`
/ `trusted_system` / `imported`). Authority has two layers:

1. **Actor eligibility.** A fixed matrix decides who may emit each op.
   Owner-command intent (confirmations, physical positioning, promotion links,
   speaker rename) and `set_epistemic_state → confirmed` are owner-only;
   participant and imported content is **data, never command**.
2. **Origin consistency.** A created node or edge's `assertion_origin` must match
   the actor, so a model cannot mark its inference as owner content.

**Restructure proposals** (`propose_restructure` → owner `confirm_restructure`)
are bounded by their author's authority, not the confirming owner's: inner ops
are validated against the proposer at propose time and re-applied under the
stored proposer's authority on confirm. Nested meta-restructure ops are
forbidden.

## Reducer guarantees (`apply_envelope`)

Pure and deterministic: no wall clock, IO or randomness; the caller injects the
RFC3339 `applied_at`.

- **Atomic.** The envelope validates against a cloned candidate; one invalid op
  rejects it with zero mutation.
- **Idempotent.** A bounded ledger (last 512) keyed by `envelope_id` and
  `idempotency_key` returns `IdempotentReplay` with the original resulting
  revision. The ledger is excluded from the semantic hash.
- **Optimistic concurrency.** A mismatched `base_revision` returns
  `RevisionConflict`.
- **Invariant-checked.** Referential integrity, confidence bounds, tombstone
  cascade to incident edges, acyclicity for `grouped_under` / `depends_on` only,
  parent-chain cycles, global promotion-link uniqueness, position locks.
- **`semantic_hash`**: a stable, order-independent SHA-256 over the durable
  board (revision, lifecycle, nodes, edges, clarifications, proposals),
  excluding view state and the idempotency ledger.

## Durable store (`ThinkingMapStore`)

Scope-owned, on the `ArtifactV2Workspace` storage stack.

```text
scopes/<principal>/<workspace>/thinking_maps/<map_id>/
  manifest.json         # atomic head record: lifecycle/title/source + latest_revision/sequence/hash
  snapshot.json         # atomic materialization of the current ThinkingMap
  events.jsonl          # append-only authoritative event log (one MapEvent per applied envelope)
  utterance_refs.jsonl  # reserved
  exports/              # reserved
```

`apply_and_persist` loads the map, runs the reducer and, only on `Applied`,
**appends and fsyncs the event to `events.jsonl` first**, then writes the
snapshot, then the manifest. The log always leads the snapshot, so a crash in
between is recoverable by replay. Reducer rejections map to `Validation` with no
write; idempotent replays write nothing. All ids are checked with
`is_safe_scope_id`. A process-wide keyed async lock over the normalized storage
root, scope and map id serializes load→reduce→persist across store instances,
while different maps proceed concurrently.

## Replay, restore, and repair (`replay.rs`)

- **Deterministic replay** (`replay_to_sequence` / `replay_to_utterance` /
  `replay_to_time`) folds stored envelopes through the reducer from revision 0.
  Each step re-verifies the revision and `semantic_hash` the log recorded, so
  replay is byte-equivalent or fails `Corrupt`.
- **Restore-as-branch** (`restore_as_branch`) forks a new map at a past sequence
  with a fresh idempotency ledger and `branched_from_*` provenance; the source
  map and log are untouched.
- **Startup repair** (`startup_repair`) rolls snapshot and manifest **forward**
  to the last durable event. Committed events are never rolled back, dropped or
  truncated; only a non-parseable **trailing** line is quarantined. A
  duplicate-sequence group is repaired only when every record carries the same
  non-empty idempotent identity, operation, base revision and resulting
  revision. Divergent operations, an ambiguous terminal state, excessive
  branching, interior corruption, a sequence gap or a snapshot ahead of the log
  are reported `Corrupt`.

At startup `magician-bin` spawns `ThinkingMapStore::startup_repair_all()`
(detached, 60s timeout, so binding is never blocked). It walks every scope and
every map directory, **including soft-deleted tombstones**. Without it, a crash
inside append → snapshot → manifest leaves a torn trailing line (after which
`events_after` fails permanently) or a stale `manifest.latest_sequence` (the next
envelope mints a duplicate sequence).

One damaged map never stops the sweep. It reports `scopes_scanned /
scopes_unreadable / maps_scanned / maps_repaired / maps_quarantined /
maps_deduplicated / maps_skipped_concurrent_write / maps_failed /
maps_quarantined_ambiguous`; a clean boot logs nothing, and `maps_failed > 0`
means those maps need a human.

**Ambiguous duplicate history.** When a duplicate group leaves more than one
replay-valid terminal branch, repair keeps the single branch that produced the
snapshot's exact state at its revision (the state the map served). If it stays
ambiguous, the sweep writes `repair-quarantine.json` beside `events.jsonl`,
counts `maps_quarantined_ambiguous`, and stops retrying; the snapshot keeps
serving reads and the log will not replay. Delete the marker to retry after a
manual repair.

The map lock cannot coordinate a second OS process, so `startup_repair`
re-reads the raw log right before its first write and compares it byte for byte
with what it decided on. If the log moved, it writes nothing and reports
`skipped_concurrent_write`.

## Interpreter

`context.rs` + `interpreter.rs` turn a finalized utterance into a bounded,
**model-authored** `MapOperationEnvelope`.

- **Bounded context.** `build_context` selects a capped slice of the board
  (focus node, recent nodes, lexical matches, focus neighbors, open
  clarifications, plus counts by kind/state). Each context node carries its
  `parent_id`. The whole board is never embedded. A client may send
  `focus_node_id` on `/interpret`; the server checks that it names a live node
  and uses it for this request only, without touching another surface's
  selection.
- **Constrained proposal schema.** The LLM never emits raw `MapOperation` JSON;
  it emits a `ProposedOperation` offering only the model-safe subset (add / update / set-kind / set-state / tombstone /
  connect / clarify, plus `move_to_parent`, which only consolidation offers).
  Owner-only operations have no variant. `add_node` takes an optional `parent`:
  the prompt asks for one on every node that elaborates, answers, questions or
  breaks down another (break-open children go under the focus node), and not to
  also `related_to` a node to its own parent.
- **Deterministic translation.** `translate` mints ids, stamps created nodes and
  edges `assertion_origin = model_inferred` and nodes
  `epistemic_state = provisional`, clamps confidence, attaches a `SourceRef` to
  the utterance and resolves intra-batch temp ids. A model
  `set_epistemic_state → confirmed` and self-loop connects are dropped.
  `add_node.parent` becomes `parent_id` only when it names a live node or a temp
  id added earlier in the batch, so new nodes cannot form a parent cycle.
  `move_to_parent` is dropped when an end is unknown or the move would close a
  cycle, and live `interpret()` filters it out entirely: it applies without
  owner review, so it places new nodes but never re-homes existing ones.
- The result is an `actor = Model` envelope, with the reducer's authority check
  as the final gate and `idempotency_key` `interp:<utterance_id>`.
- The LLM sits behind a mockable `InterpreterLlm` trait. The production
  `RouterInterpreterLlm` wraps `OperationLlmRouter` via
  `LLMOperation::Other("thinking_map_interpret")`, mapped by default to the
  dedicated profile `op-thinking-map-interpret` (an isolated OpenAI GPT-6 Luna
  strict-JSON profile, never a shared memory/chat profile).
  `POST /thinking-maps/{id}/interpret` runs it against the global router and
  applies the result. Calls are metered via `OperationLlmTelemetryContext` under
  `thinking_map_interpret`.
- **Progress narration** (`InterpretStage` + `InterpretProgressSink`).
  `interpret()` narrates `preparing` (with the context's live-node count) →
  `loading_context` → `facilitating` → `parsing` → `shaping`. The REST handler
  publishes on the transport bus `ThinkingMapInterpretProgress { map_id, utterance_id, stage, detail?,
  node_count? }` beside `ThinkingMapUpdated` and emits the terminal `idle` when
  the run settles. Best-effort; `utterance_id` lets a client claim narration for
  its own run. The ambient coordinator passes `NoProgress`. The stage vocabulary
  is closed. Consumers: web (strip above the brainstorm composer), iOS
  (`ThinkingMapIntelligenceProgress`), Android (`ThinkingProgress` +
  `IntelligenceStrip`).

## Ambient session coordinator (`coordinator.rs`)

`ThinkingMapSessionCoordinator` subscribes read-only to the
`RuntimeTransportBroadcaster` and keeps a registry
`source_session_id → (map_id, principal, workspace)`. For each
`ChatMessageReceived` with `direction == User` in a **registered** session it
runs `interpret` (`continue_thinking`) and `apply_and_persist`s the ops,
idempotent by chat-message id. Errors are logged and swallowed; the loop never
dies. Attach and detach with `POST /thinking-maps/{id}/sessions`
(`{source_session_id}`) and
`DELETE /thinking-maps/{id}/sessions/{source_session_id}`. It is built once in
the server bin and shared to the API via `app_data`.

- **Session matching (`resolve_binding`).** A registered id matches the event's
  `session_id` first, then the message's `presence_session_id`. The voice
  orchestrator sets `session_id` to the chat session it derives and stamps the
  media/voice session on `presence_session_id`, so a voice client that only
  knows its media session id can attach that. The matched id becomes the
  utterance's thread id.
- **Coverage (`is_mappable_turn`).** Voice and chat-dictation turns
  (`direction: User`) map.
  Live meeting-transcript lines are `direction: System` messages tagged
  `source_surface: "meeting-transcript"`
  (`ChatService::persist_meeting_transcript_line`, speaker prefixed), and those
  map too once attached. Other System messages and Assistant turns never map.
- **Realtime push (`ThinkingMapUpdated`).** Every applied envelope (owner
  `/operations`, `PATCH`, `/interpret`, a consolidation decision, the ambient
  auto-map) emits `ThinkingMapUpdated { map_id, principal, workspace, revision }`
  (`emit_map_updated` in `thinking_maps_api.rs`; the coordinator skips
  idempotent replays). It is a change notice, not data: clients re-fetch the map.
  Scope-filtered in `event_visible_to_scope`; taxonomy `(Observability, Info)`
  in both tables (`make event-taxonomy-codegen`). The web detail page
  `pollNow()`s on a matching notice over the v2 WebSocket; iOS subscribes during
  Listen mode too (`ThinkingMapRealtime` over `/realtime/ws`) with a 20s safety
  poll.

## Governed promotion

`POST /thinking-maps/{id}/nodes/{node_id}/promote` `{target: task|memory,
confirm?}` turns a node into a durable object.

- Tombstoned, rejected, superseded or contradicted content never promotes.
  Owner-asserted content promotes by default. Anything else (model-inferred,
  provisional, participant or imported) needs `confirm: true`, recorded as an
  owner assertion (`set_epistemic_state → asserted`) in the same envelope.
- Idempotent: an existing link of that kind returns the existing object
  (`promoted: false`), backed by the reducer's promotion-link uniqueness.
- `task` creates through the v3 task service (absent → 503
  `promotion_unavailable`) with map/node/origin provenance; the description
  carries `Thinking map node source: <map_id>:<node_id>` for Today
  reconciliation. `memory` creates a review-gated learning candidate targeting
  `user.knowledge`.
- The map records the link via `link_promoted_object` and emits
  `ThinkingMapUpdated`. REST and Today share `promote_node_to_target`.

## Today source-action adapter (`thinking_map_action`)

Acknowledged, unlinked action nodes surface on **Today** with a **Create task**
action (`feed/action_adapter.rs` + the projection in
`magician-api/src/feed_api.rs`).

- **Projection (read-side).** Every active map in the scope is scanned. A node
  qualifies when it is action-kind, not tombstoned, and owner-asserted (origin
  `owner_spoken`/`owner_edited`, state `asserted`/`confirmed`). Candidates land in
  Follow-ups (`source_kind: thinking_map_action`) with navigation to
  `/thinking-maps/{map}?node={node}`.
- **Create task** is `POST /today/items/{id}/actions/create_task`, calling
  `promote_node_to_target`. A retry or an already-promoted node returns the
  existing task (`reused_task: true`); a node whose state regressed gets a 409
  (`confirmation_required`/`not_promotable`).
- Once a node has a `task` promoted_ref, the candidate is suppressed for every
  task status. Reconciliation also drops candidates whose task exists but whose
  link commit was interrupted (description marker, explicit `linked_task_id`, or
  the same `thinking-map-{map_id}` thread plus normalized title).
- Store read failures yield an empty projection.

## Tutor adapter

Grounds Personal Tutor in a selected map or cluster without taking over Tutor
narration or storyboard.

- **Digest builder.** `build_tutor_map_context(map, selected_node, budget_chars)`
  is pure and deterministic. With a selected node it renders that node's cluster
  (the node, one-hop neighbors and the edges among them); otherwise a
  recency-ordered overview. Bounded on every axis. `model_inferred` content is
  tagged `[AI-suggested]`, participant/imported content is tagged too, and
  tombstoned/rejected/superseded content never renders as current truth. The
  digest declares itself reference material, not instructions.
- **Binding registry.** `POST /thinking-maps/{id}/tutor-context`
  `{session_id, node_id?}` snapshots the digest into a process-local registry
  (30 min TTL) keyed by `(principal, workspace, chat_session_id)`;
  re-registering overwrites, and
  `DELETE /thinking-maps/{id}/tutor-context/{session_id}` clears (idempotent).
- **Tutor wiring.** `TutorRunStore::start_run` stamps the digest onto the run's
  optional `TutorRun.thinking_map_context` (skipped when absent). The forced
  preflight serializes it into its `start_tutor_run` tool result; the
  LLM-invoked tool returns it top-level (`thinking_map_context`) next to
  `visual_entity_context`.

The web "Ask Tutor" inspector button drives this flow (see
`docs/components/unified-ui/thinking-map.md`).

## Live semantic eval (`scripts/eval-thinking-map-live.py`)

Runs fixture utterances against the running server's real `/interpret` (live
LLM) on scratch maps that are soft-deleted afterwards. It measures envelope
validity, node precision and keyword recall (a diagnostic proxy: an in-place
correction adds no nodes), correction-target accuracy, duplicate/churn,
unsupported assertions, authority violations, clarification restraint, latency,
and 5-repeat stability on the release-critical subset. **Gates (non-zero exit):
correction-target accuracy ≥95%, zero authority violations, envelope validity
≥95%.** Run `make eval-thinking-map-live` (~20 mini-class LLM calls;
`TM_EVAL_RUNS=1` for an 8-call smoke); the report is
`coverage/evals/thinking-map/latest.html`.

## Restructure consolidation (`consolidation.rs`)

`consolidate` reads the bounded whole-board digest and asks the model for a
reorganization using the same model-safe `ProposedOperation` subset. The ops are
translated (ids minted, `model_inferred` stamped) and wrapped in a
`RestructureProposal` by the **Model** actor, staged via `propose_restructure`
as **pending**. Nothing changes until the owner confirms; `confirm_restructure`
then re-applies the ops under the stored proposer's authority. Consolidation is
offered `move_to_parent` and asked to fix flat trees (a top-level node that
belongs under another, often tied to a hub only by `related_to`), so older flat
maps are re-homed through an owner-confirmed Reorganize, not a data migration.
It reuses `InterpreterLlm`, `translate`, `build_context` and
`RouterInterpreterLlm`, metered under `thinking_map_interpret`.

## REST API (GA)

`magician-api/src/thinking_maps_api.rs` serves the store under
`/api/magician/v2/thinking-maps`. Routes are always mounted.

| Method + path | Purpose | Success |
|---|---|---|
| `POST /thinking-maps` | create a map (`{title, source?, map_id?}`) | 201 map |
| `GET /thinking-maps` | list visible summaries, `updated_at` desc with `map_id` tie-break; deleted tombstones omitted | 200 `[MapSummary]` |
| | Each `MapSummary` may carry `node_preview` (`{nodes:[{node_id, parent_id?, kind, suggested, title}], edges:[{from, to}]}`): the first 10 live nodes and their parent→child edges. Omitted for empty maps. | |
| `GET /thinking-maps?limit=N&offset=M[&lifecycle=deleted]` | one server page (`limit` clamped 1..=200); no lifecycle returns the visible list, an exact lifecycle selects that view before `total` and slicing | 200 `{maps, total, offset, limit}` |
| `GET /thinking-maps/{id}` | load current map | 200 map / 404 |
| `DELETE /thinking-maps/{id}?confirm=permanent` | permanently remove an already soft-deleted map and clear its ambient/tutor bindings; fails closed without confirmation or for a non-deleted map | 200 `{deleted, map_id, detached_sessions, cleared_tutor_contexts}` / 400 / 409 / 404 |
| `POST /thinking-maps/{id}/operations` | apply an owner envelope | 200 `applied` \| `idempotent_replay` |
| `GET /thinking-maps/{id}/events?after_seq=N` | event-log tail | 200 `[MapEvent]` |
| `GET /thinking-maps/{id}/replay?at_seq=N` | deterministic replay | 200 map |
| `GET /thinking-maps/{id}/export/markdown?include_provisional=&include_superseded=` | deterministic Markdown export | 200 `text/markdown` |
| `POST /thinking-maps/{id}/restore` | restore-as-branch | 201 branch |
| `POST /thinking-maps/{id}/interpret` | interpret an utterance (intent `continue_thinking`/`break_open`, optional `focus_node_id`), then apply | 200 `applied`\|`idempotent_replay`\|`no_operations`; invalid focus 400 |
| `POST /thinking-maps/{id}/consolidate` | LLM proposes a reorganization, staged as a pending proposal | 200 `applied`\|`no_operations` |
| `POST /thinking-maps/{id}/proposals/{pid}/decision` | owner confirms or rejects a pending proposal | 200 `applied` |
| `PATCH /thinking-maps/{id}` | owner-only rename / lifecycle change | 200 `applied` |
| `POST /thinking-maps/{id}/tutor-context` | bind a map to a chat session for Tutor (`{session_id, node_id?}`) | 200 binding + digest |
| `DELETE /thinking-maps/{id}/tutor-context/{session_id}` | clear the binding (idempotent) | 200 `{cleared}` |
| `POST /thinking-maps/{id}/nodes/{node_id}/promote` | governed promotion to task or memory | 200 |
| `POST /thinking-maps/{id}/sessions` | attach an ambient source session | 200 |
| `DELETE /thinking-maps/{id}/sessions/{source_session_id}` | detach | 200 `{detached}` |

Every handler resolves an explicit `(principal, workspace)` via
`resolve_required_scope`; a missing scope is `400 missing_scope`. The operations
endpoint always stamps `actor = OperationActor::Owner { principal }` from that
scope and forces `map_id` to the path. `ThinkingMapStoreError` maps to HTTP:
`NotFound`→404, `AlreadyExists`→409, `Validation`→400, a stale `base_revision`
→ **409 `revision_conflict`**, `InvalidId`→400, `Corrupt`/`Io`→500. Bodies leak
nothing beyond `to_string()`.

## Export

`thinking_map/export.rs` renders Markdown with the pure
`render_markdown(&map, &opts) -> String`, deterministic because it reads only the
stored map and walks `BTreeMap`s. Layout: `# title` and a
source/lifecycle/revision line; a **Board** tree over the `parent_id` hierarchy
(indented bullets with label, a `kind` tag, an origin/state tag for
non-owner-asserted content, `✦ AI-suggested` for model-inferred, the epistemic
state when not plain `asserted`, and `detail_markdown` as a nested blockquote);
then **Connections**, **Open clarifications** and **Pending proposals**, each
omitted when empty. A node whose parent is filtered out shows at the root, and
`parent_id` cycles render each member once.

`ExportOptions`: `include_provisional` (default **true**), `include_superseded`
(default **false**); **tombstoned content is never exported**, and edges render
only when both ends are visible. Over HTTP the export returns
`text/markdown; charset=utf-8`, and unknown or empty query flags fall back to the
defaults. On the web, the canvas toolbar's **Export** menu
(`ui/unified-ui/src/lib/thinkingMaps/ThinkingMapCanvas.svelte`) offers **SVG**,
**PNG** and **Markdown** (`exportMarkdown(id, opts)` in
`$lib/thinkingMaps/api.ts`).

## Testing

Run each through `make cargo ARGS="…"`: `test -p magician-surfaces --lib
thinking_map` (spine, including `thinking_map::export`),
`test -p magician-api thinking_maps_api` (REST),
`test -p magician --lib tutor_map_context` and `test -p magician --lib tutor`
(Tutor digest and `start_run` attachment).
