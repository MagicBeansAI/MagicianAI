# Flat Loop

Behavior contract for the flat per-action loop and the stateless `run_loop`
driver. Design provenance (archived):
flatten,
stateless design,
driver decisions.

## Why

Every pack primitive (duckdb, gmail, metabase, skillshub subprocess skills,
browser, …) is a leaf `<pack>__<action>` the outer LLM calls directly, dispatched
without a nested LLM through `execution/primitive_dispatch/` (pack schema
`type: primitive`). For high-cardinality packs (metabase, generated from Metabase's OpenAPI spec) the
catalog splits into a **hot** tier (full schemas) and a **deferred** tier (bare
names; schema via `tool_search`).

There is no loop-mode toggle. `MagicianConfig` is `#[serde(deny_unknown_fields)]`,
so a leftover `inner_loop:` key hard-errors at load.

Since decision prompt 1.3.11 / system 1.0.8 (system pin now 1.0.9), owner-input
guidance reaches beyond visible browser challenges: investigate context and
evidence, reuse answers, bundle material missing inputs and record reversible
assumptions within the agreed goal. This is guidance, not input authority or
acceptance criteria; synthetic protocol tests do not qualify question selection.

## Resource-bound and Relentless controller runs

Foundation for Relentless's multi-step actor. Not qualified end to end by a
native task run; unit or synthetic protocol passes do not qualify it. The
execution-runtime handoff boxes the executor future before applying
token-meter/resource-guard task locals, so it fits the default worker stack.

- **Resource guard.** A runtime-only `LlmPhysicalResourceGuard` rides
  `AgenticContext`, `CapturedRunTaskLocals`, the execution-runtime hop and the
  native adapter's scoped router, on the existing permit/queue/task harness and
  independent of App disclosure. A missing value keeps ambient authority;
  distinct owners deny. Native turns keep local history; a resource-bound
  execution carrying an opaque provider continuation ID is refused before launch.
- **Catalog.** Admitted scopes keep action/delegation/procedure ceilings through
  owner refresh, get no implicit universal grants, and use the eager flat
  projection under owner/plane/deny policy (an empty shared working set cannot
  hide admitted leaves when discovery has no grant). Exact compiled-pack names
  survive collisions with built-in action names; legacy aliases are omitted.
  Catalog extraction and owner-profile tool-lane derivation share this
  classification. Toggling universal injection neither adds nor removes admitted
  packs. Refresh keeps the admitted `tool` lane of a canonical compiled pack and
  never adds a lane to a frozen App or resource-bound ceiling. Debug traces list
  admitted/direct and pre-policy provider tool names only, no
  prompt/argument/credential values.
- **Actor.** Unsupported child/delegation/pipeline and result controls are
  attenuated by exact dispatch identity, keeping `yield` and `need_user_input`
  (denying the `orchestrator` group would drop them). An invocation file policy
  and isolated compiled-provider resolver are installed before native actions
  are built; wider-policy caches are not reused. Local `files` work and declared
  artifact sets are admitted.
- **Media.** Scoped media preparation owns sampling, local transcription and
  evidence-cache publication through cancellation. `media_edit` runs under a
  runtime-only lease (work area, byte ceiling, deadline, executable pin) that
  model arguments cannot create;
  guarded tools use cancellable ffmpeg, forced input containers, no external
  movie references, and publish completed scratch files only, through the
  durable staged publisher before sealing. Unguarded calls keep background-job
  behavior. `still_to_video`: one PNG/JPEG/WebP/PPM still → silent H.264 MP4
  (finite duration, optional frame rate, even dimensions). `concat` keeps audio
  unless `include_audio: false`; the caller matches dimensions and frame rates.
  Critics see declared samples and optional sampled-speech receipts; unseen
  intervals, music and prosody are unavailable evidence.
- **Public research.** `public_research` is a runtime-only bounded reader
  (`web_fetch`/`web_search`, no browser fallback) whose frozen policy and
  request ledger survive owner-answer continuation. The proposal-only
  `propose_relentless` guide 1.5.0 covers these recipes, explicit shared-runner
  verification and audio provider identity; no second execution harness.
- **Attempts.** `controller_attempt_hash` separates attempt journals and grants
  no routing, pipeline-stage or budget authority. Entry needs a fresh opaque
  resource carrier that pause recovery cannot rebuild; the marker survives pause
  serialization, but executor entry and run setup refuse it without the carrier.
  Pause continuation keeps the frozen total iteration ceiling and accumulated
  cost (no ordinary-lane expanded resume allowance or exhausted-cost reset).
- **Questions and resume.** Pause key `{execution_id}:controller:{attempt_hash}`;
  each question has an ask ID, renewed on a validation re-ask. The pause reader
  is read-only, refuses ambiguous or in-flight/manual/terminal records and
  grants nothing; restoring also needs the pinned checkpoint digest and original
  ledger admission. The resume entry (native adapter and authenticated HITL
  answer route) verifies the pause body and owner-question journal before any
  model call, refuses pending/unknown effects (unknown ones need the existing
  resolver), binds under the original resource guard (serialized state cannot
  create that binding), verifies the typed answer against the controller's
  updated evidence view and reuses the work area and physical ledger.
- **Answers.** Sensitive answers become bounded references before persistence.
  Server-owned input type and form fields constrain the payload
  (unknown/duplicate fields and malformed selections refused, skipped payloads
  discarded); replay is checked before custody so a one-time code registers
  once. Ordinary resume checks external-output ownership before consuming the
  pause.
- **Terminals.** Ordinary status transitions, the HMAC-sealed execution-control
  generation, the app lifecycle fence and terminal-fence transfer apply.
  Attempt terminals defer task publication (incl. terminal outbox replay) to the
  durable external output owner; controller review owns acceptance. Sealed root
  `WaitingForUser` pauses of user-facing tasks reach the terminal question
  publisher without a task outcome; internal candidate/child endings and
  questions from internal evaluation or already-terminal roots are suppressed.
  Validation retries seal their replacement request with the adapter checkpoint;
  replaying it after the replacement pause's body CAS spends no model budget.

## Module: `magician_v2::execution::flat_loop`

### `catalog` — the two-tier catalog

- `ALWAYS_HOT_TOOL_NAMES` (fixed, no per-agent override): `yield`,
  `need_user_input`, `spawn_sub_goal`, `read_result`, `export_result`,
  `tool_search`, `shell`, `read_file`, `write_file`, `edit_file`, `grep`, `glob`,
  `http`, `web_search`, `content_search`. Native control tools are not
  `tool_search`-selectable; search is hot so a first-turn lookup does not fall
  through to `shell`/`http` scraping.
- `conditional_hot_tools(ctx)`: `delegate_to_agent` + `get_agent_details` with
  delegation targets; `activate_skill` with procedure skills;
  `handover_to_agent` with an authorized handover target.
- `CODING_HOT_TOOL_NAMES` (`run_coding_task`, `apply_code_proposal`,
  `run_project_checks`, `list_proposals`) are hot only when granted directly
  (`ctx.direct_capabilities`).
- `## AVAILABLE SPECIALIST TARGETS` lists id, aliases, short description, pack
  names and admitted `delegate_to_agent`/`handover_to_agent` routes; details via
  `get_agent_details(agent_id)`, harness management via `inspect_agent`.
- `build_flat_loop_tools(ctx, index) -> FlatToolCatalog { hot, deferred }`: hot
  tools reuse `agentic::native_catalog` builders (byte-identical schemas);
  `DeferredEntry { name, group, search_hint }` per reachable non-hot leaf.

### `tool_index` — server-side schema store

- `ToolIndex` is built once in `compiled_providers::build_compiled_registry`
  from all loaded packs, installed on `AgentResources.tool_index` (`OnceLock`).
- `build_tool_index(packs)`: compiled/single-leaf pack → one leaf named after
  the pack; a pack with `native_action_schemas` → one `<pack>__<action>` leaf per
  primitive (the action's declared parameter subset, pack-level types unless
  overridden: `tool_index.rs::primitive_parameters_schema`). Multi-action leaves
  get a normalized selector hint (≤180 chars) from the pack description; it feeds
  ranking and renders once per distinct description in the deferred prompt.
  Single-action packs get no hint.
- Lookups: `get`, `leaf_names_for_pack`, `select(names)`, `search(query, max)`
  (weighted regex: exact name-part 12, partial 6, full-name 3, hint 4,
  description 2; `+term` pre-filters).

### `tool_search` handler

`compiled_handlers/tool_search.rs` → pure `search_response(index, query, max)`:
`select:A,B` → full schemas; keyword → ranked matches with `how_to_load` (a
listing is not a load); no index → "inactive" (everything already eager). An empty select (`matches: []`)
says **nothing was loaded** and points to keyword search then `select:<name>`.

`ToolIndex::pack_for_selected_name` (shared with `project_family_selection`):
leaf → itself; pack name → its leaves; unknown `<pack>__<action>` → its pack.
Whole-pack surfaces (chat, voice; `build_surface_tool_index`) collapse leaves
to one pack entry, so `select:browser__open` still works.

`select:` (`flat_loop/working_set.rs`, via
`executor.rs::record_loaded_tools_from_action`) **merges** the authorized leaves
of every named pack into the loaded set, evicting the LRU family only past the
limit (`WorkingSetLimits::autonomous_compatibility`: 4 families per run;
`agent_surface_runtime.working_set.max_loaded_families`, default 8, per
surface; re-selecting or a successful Chat/voice call refreshes recency), and
reports it in the next prompt's `## WORKING SET CHANGE`. A pack
over the tool/schema budget loads only the named leaves.

On revision change (`ToolWorkingSet::reconcile_in_lineage`), each loaded tool's
`tool_identity_digest` (pack/name, parameter schema,
`ToolIndexEntry.pack_definition_digest` (whole pack definition minus prose),
result projection, `ToolIndexEntry.provenance` stamped by the scoped snapshot:
`app:<installation>`/`embedded`/`scope`), recorded at load, decides:
Chat and realtime voice keep a tool only while authorized with an unchanged
digest (without digests, same lineage keeps what is authorized, new lineage
clears); the autonomous loop clears on any revision change. Prepared selects pin
their CAS to the binding instance, revision and generation read under the same
lock as their reconcile.

### `dispatch`

`dispatch_flat_action(tool_name, args, registry, exec_ctx, cancel)`:

- `parse_flat_tool_name` splits `<pack>__<action>` on the first `__`.
- `classify_flat_route` → `FlatRoute`: `CompiledProviderPrimitive`,
  `CliTemplatePrimitive`, `Browser`, `PlainCompiled`, `Unknown`. Leaves of a
  compiled pack with `native_action_schemas` (e.g. `time_math`) are
  `PlainCompiled` with the action injected as `operation` if omitted; an
  undeclared suffix stays `Unknown`.
- Primitives → `primitive_dispatch::dispatch::dispatch_primitive`
  (`CompiledProviderDispatcher` / `CliTemplateDispatcher`,
  `PrimitiveDispatcher::dispatch` → `ActionResult`); bare compiled packs →
  `compiled_dispatch::try_dispatch_compiled_pack`.

`PrimitiveExecCtx` carries only scope/identity, browser side-channels, secrets,
the scope-aware secret resolver (from `ActionExecutors::secret_store_resolver`
via `build_primitive_exec_ctx`), progress sink, event broadcaster and dispatch
authority. Without the resolver, governed static-secret skills refuse to inject
credentials even if the scoped `.env` exists. Spend gating: `execute_maybe_gated`;
CapabilityScheduler retry/concurrency wraps at the executor call site.

### Shipped: flat is the only loop

`ExecutionHistory`, `yield`/`dispose_yield` and cancellation are shared;
`build_catalog_context` expands primitive packs to leaves and
`execute_action` routes flat calls through `flat_loop::dispatch_flat_action`.

## Browser observation and decision

Success is the page reaching its goal state, not tool choice; an `eval` that
fires an element's own handlers is valid. The strict METHOD gate (real
interaction primitive before `goal_reached`, no-page-effect acts as stuck,
before/after effect digest) is **off by default**, behind `strict_browser_interaction_gate_enabled()` (env
`MAGICIAN_STRICT_BROWSER_GATE`), for benchmarking only. Sites:
`executor.rs::goal_reached_rejection_reason`,
`decision.rs::try_synthesize_stuck_auto_yield`, `primitive_dispatch/dispatch.rs`.

`space_batch_commands` (`primitive_dispatch/browser/dispatch.rs`, mirrored for
Yutori) waits `MAGICIAN_AGENT_BROWSER_MUTATION_SETTLE_MS` (default 150 ms) after
`click`/`drag`/`fill`/`type`/`press`/`check`/`uncheck`/`select`/`tap`/`upload`
and `MAGICIAN_AGENT_BROWSER_BATCH_SPACING_MS` (default 50 ms) between read-only
steps and raw `mouse` fragments; `MAGICIAN_AGENT_BROWSER_BATCH_SPACING_MS=0`
disables all waits.

Yutori is off by default: bind `agentic_decision` to `vision-yutori-n1.5` as the
`when_has_images` arm of a `default`/`when_has_images` map and restart
(`OperationLlmRouter::agentic_decision_uses_yutori()` checks either profile for
provider `yutori`). Then each iteration forces a `Full` capture (read-only
`browser__screenshot`, gated on `browser_session_used`, rebuilt by
`build_flat_browser_state`), and `PageState.viewport_size` flows through
`native_decision_via_adapter*` as `extra.viewport` to denormalize 1000×1000
coordinates to CSS pixels. Otherwise observations are text-first (no
screenshot) and `perform_observation_with_policy` passes `current_state`
through. Bare Yutori actions (`left_click`, `mouse_move`, …) lower to
`browser__<action>` (so the allowlist accepts them and they classify as
`Browser`) and omit `task_state_action`; re-captured state carries the live
viewport. `scroll` emits a cursor-anchored `mouse wheel` (traps on nested
`overflow` containers; baseline: `scrollintoview <selector>`).
`Page.handleJavaScriptDialog: No dialog is showing` is a soft success, not a CDP
disconnect. The flat session threads `with_initial_url`.

Outside browser state, the decision image is the non-empty `.png`
`screenshot_out_file` in the params of the immediately preceding successful pack
call (`decision.rs::last_action_screen_capture`, max 8 MiB); a failed capture or
missing file attaches nothing.

## Pause, resume, and provider continuations

- `AgenticPauseState.live_messages` (`#[serde(default)]`; legacy blobs load
  empty) stores `history.live_messages` plus the mid-turn tail
  (`decision::pending_live_messages`) via `build_full_pause_state`.
- `restore_context_from_pause` → `AgenticContext::resume_live_messages`; bare
  `synth_iter_*` ids become `r{generation}_…` (generation = resumed
  `iteration_offset`; numbering restarts at 1).
  `seed_resumed_history` seeds once (`mem::take`).
- Pause-stable ids use the **global** iteration (`iteration_offset` + local):
  `{execution_id}:iteration:{n}:record:{k}:{tool}` and
  `synthetic:{kind}:{execution_id}:{n}`; the raw-result store refuses a
  different payload under an existing id, so local counts made resumed results
  unreadable (`materialization_or_projection_failed`).
- Live messages, provider response id, image cohort and compaction resume
  projection pass to the first resumed segment only (refinement clears them);
  the watermark seeds to `iterations.len()` so `sync_live_messages` folds only
  post-resume records. A rejected provider
  continuation checkpoint falls back to cold reconstruction.
- A restored conversation gets a compact resume marker instead of the
  `[Prior actions …]` summary (`build_exact_resumed_goal`, `build_resumed_goal`,
  `execute_agentically_continue`); empty `live_messages` falls back to the
  summary. Exact resume reuses frozen `preserved_initial_tools`.
- A delegation checkpoint owns its environment snapshot. `WaitingForChildren`
  envelopes keep legacy `last_state` as an `Uninitialized` sentinel with the
  real state in an optional exact checkpoint; the FSM accepts
  `WaitingChildren → WaitingChildren` (`resume_delegation_checkpoint`) so a
  resumed parent can delegate again (refusing it strands the next round's
  handoff descriptor).

Provider continuations send only changed sections of a fingerprinted bootstrap
(observation, task state, plan, delegation results, capability projection,
procedure, summaries, supplemental knowledge; cleared → tombstone); fingerprints
commit after success. Owner, trust, capability or routing-authority changes
force a full bootstrap; nested/refinement runs keep their own ledgers.

## Terminal evidence and derived sink

- `goal_reached_has_sufficient_evidence` ignores harness-contradicted iterations
  (`NoEffect` / `BROWSER_NO_EFFECT_MARKER`, `Mismatch`/`NoChange`;
  `iteration_carries_negative_harness_signal`); `Unknown`/absent verification
  is fine.
  Micro-goals stay gated (`observed_evidence_ref` +
  `step_completion_supported_by_iteration`). `evidence_refs` are not
  existence-checked.
- `premature_giveup_rejection_reason` rejects `Failed`/`RetryTransient` yields
  with budget and recoverable work left, except stuck-auto-yields, permanent
  blockers, 3-strike honour and budget margin.
- A rejected terminal claim folds as a whole (assistant, user) `IterationRecord`
  pair; an unrun tool_call folds as a `{"status":"deferred"}` `ToolResult`.
- The precision judge sees opened pages grouped by canonical URL (then durable
  source id, content hash, record hash; publisher canonical wins;
  fragments/tracking params dropped, semantic query params kept), latest
  successful read only, in a packet of at most 24,000 characters (`MAX_TERMINAL_GROUNDING_SOURCE_CHARS`) split deterministically across sources
  (no discovery snippets or failed reads). Three rejected drafts (`MAX_TERMINAL_GROUNDING_REJECTED_DRAFTS`) fail closed.

## Stateless run_loop driver

Stateless is the default. `MAGICIAN_EXECUTION_DRIVER=inprocess|stateless`;
blank or unknown → `stateless` (logged); `inprocess` is the rollback, resolved
once per run in `run_loop::driver_for_run`. `GET /health/execution-driver`
reports per-arm counts (`driver_run_counts()`), the resolved arm and the raw env.

Cold composition comes only from Artifact/runtime recovery (incl. sealed
delegated-child bindings) or a validated durable PlanGraph; a loop-state row
carries no authority and `HostFleet` is not a production composer. Direct
admission refuses work with neither before `Executing`.

- `driver_inproc::run_iteration` → one iteration (`IterationStep`);
  `driver_worker::advance_once` → one phase (claim → load → phase → commit →
  release; 17-variant `Advanced`). Do not merge them (that deletes the
  per-phase commit).
- `driver_worker::advance_in_session` runs a phase under a `LeaseSession`:
  reuse above half TTL, renew (new fence) below, claim afresh if none or lost.
  Kept only after `Committed`/`RecoveryRewound`; released on every other outcome
  before the terminal-outbox wake. Every phase loads, verifies the journal and
  commits under the fence; a taken-over lease fails the next fenced write.
  `StatelessArm` holds one session per iteration (`advance_iteration` releases
  it on every exit), `WorkerRunner::take_turn` one per key turn. Claim-time
  housekeeping (temporary reaping, ending repair) runs on each fresh claim.
  `[LOOP-TIMING]` reports `lease=claimed|renewed|reused` and `lease_ms` per phase.
- Apply: gate → intents → `dispatch_apply` → `record_batch_outcomes` →
  `WorkerHost::fold_after_dispatch` (folds a settled pre-decided call once every
  effect outcome is durable) → boundary.
- `executor.rs::StatelessArm` claims over `InProcessWorkerHost` (trait in
  `driver_worker.rs`; impl in `executor.rs` for `TrustDispatchGuard` and
  `AgenticToolLineageState`). A source gate in `run_loop/mod.rs` refuses a
  catch-all over `Advanced`.
- Normal setup rebuilds services for the invocation key; identity adoption
  proves they match committed authority. Lease heartbeats keep the pin live
  during long phase awaits.

### Recovery, launch, and admission

Authority comes from the per-scope reverse-binding catalog; the legacy-writer
seal is retired for the runtime writer floor
(runbook note).
Every recovery path needs typed, sealed, exact authority; ambiguity stays
durable debt and defers without mutation. Only a typed not-found is absence;
scope, lineage, I/O, decode or integrity errors fail closed.

| Area | Contract |
|---|---|
| Legacy writers / scope seal | Interrupted recovery uses an exact per-base reverse catalog that older writers may omit, so `Absent` and `RecoverableExact` are mutation-free deferrals until legacy writers drain, a deployment-bound walk completes and each principal/workspace scope is sealed (also gates a new shell's first stateless launch). Pre-seal walks are positive discovery only. Post-seal startup classifies active-run authority in index-only chunks of ≤20,000 base execution ids. |
| Root states | `Planning` = never-started; entry needs the exact Runtime generation and no pause, manual-resume, live-control or base-segment authority (authoritative only after the seal). `Runnable` is not a fresh-root marker: a new delegated child may use its creator's one-shot nonce; interrupted `Runnable`/`Executing` runs need exact or pre-seed authority. |
| Accepted launch | Message execution, explicit starts and create-with-initial-message never 202 a detached future. Before acceptance a scope-HMAC-sealed envelope binds Runtime generation, Artifact/root/owner ids, launch mode, message and bounded options, with a renewable `Pending -> Claimed -> Consumed` lease that owns crash recovery through `Planning`/`PlanningComplete` and the pre-seed/first-loop window. It stays Pending until a waiting/terminal Runtime state or positive loop authority owns restart; future completion is not a handoff. Planning prewrites one deterministic inbound turn (a store without exact insertion refuses rather than duplicate). Consumed envelopes (≤20,000 per scope) are deleted only after HMAC, hashed path, identity and a fenced re-read agree. Legacy/taskless PlanGraph rows recover via typed base-execution authority, never an invented intent. |
| Startup queues | Bounded, deduplicated, deadline-indexed, in the execution-job registry (no sleeping future or semaphore waiter per row). Legacy/taskless active PlanGraphs: ≤32 workers, backoff 250 ms–30 s until a peer lease expires, pre-seed grace passes, launch succeeds or the Runtime leaves `Runnable`/`Executing`. Planning commits: sealed Runtime/turn/goal receipt + journal-first per-task recovery-catalog marker, 16 workers; a one-time canonical walk bootstraps markers for pre-catalog rows (the list cache is never restart authority). AskLoop resume catalog: 128-entry pages, durable cursor, 16-worker shared deadline coordinator. Fixed-roster outer-stage cursors, accepted-launch rechecks and delegated-child attachment repair each have their own queue. |
| AskLoop continuations | Scope/task/plan/execution/question-bound `Prepared -> Committed -> settled` receipt, durable before an answer, resume, guardrail cancel or pause is consumed; idempotent replay, only Committed dispatches (under a renewable claim). Clarification-suspended planning turns are non-terminal (recovery keeps `WaitingUser`). Non-TaskPlan continuations use a sealed Runtime resume catalog binding status revision, pause/state id, source turn digest and any completed PlanGraph; a replanned graph must be HMAC-bound before `PlanningComplete`. |
| Delegated parents | No-progress state is sealed and bounded (oversized child vectors rejected before reads; agent/goal multiset fingerprinted). Completion needs the exact active-child roster, validated Runtime/Artifact lineage and a persisted result summary; one conversation-store settlement then makes the parent runnable and clears its child group. Children publish input before the sealed launch-ready binding; the parent commits `WaitingChildren` before scheduling. Handoff descriptors are scope-HMAC-sealed over principal, workspace, task, parent and a capped child roster. |
| Interrupted runs | Live / exact recoverable segment / settlement-pending / absent / uncertain; live/pending/uncertain defer without mutation, absence applies only in the fenced pre-seed window. A durable admission token taken before async composition is revalidated (token, Runtime generation, segment/revision) under the lifecycle exclusion before first mutation. Only the exact timer admits Sleeping. A checksummed priority catalog (published before terminal CAS, retired after cursor settlement; fallback to authoritative paging plus periodic full audit) speeds terminal settlement discovery; the journal/receipt stays the source. |
| Placement resumes | Complete checkpoint + sealed `PlacementRetry`, rebinding source segment/due generation at Artifact and runtime admission; a newer exact `LoopState` forces owner/tool/trust recomposition. |
| Wake queue and controls | Reload/publish under cross-process locks, per-execution activation fencing; dispatchers hold the (non-reentrant) row lock only for the stale-claim check. Steer/cancel are durable, phase-consumed and bound to the sealed runtime-control UUID. Control, pause, steer and exact resume resolve the execution's epoch, not the serving process's driver, so an `inprocess` peer cannot misroute a stateless run. Tree pause publishes one sealed roster; tree resume is a fixed-roster transaction (preparing members roll back, committed roll forward, status-only revision prevents Paused ABA). A terminal inbox fence routes late accepted input to another Decide. A content-free cumulative accepted-redirect count (never reset by consumption or control-epoch activation) lets Relentless qualification refuse operator-assisted native trials; ordinary steer/control is unchanged. |
| Terminal generations and ceilings | A resumable body is persisted before the terminal `LoopState` CAS as a hidden generation (HMAC over key, revision, body digest, successor segment, stage/attempt, diff-approval disposition); public resume/continue/cancel get retryable settlement-pending and cannot mutate it; pause activation publishes last. An expired running segment is a non-resumable deadline failure (not cancel, not pause). Exhausted work/token/cost ceilings and runtime-scope loss conclude inside the claimed phase; Apply rechecks cancel, pause, deadline, ceilings and scope after intents commit, before first dispatch. |
| Shutdown and lifecycle exclusions | Detached jobs register at the execution-runtime boundary; shutdown closes admission and drains them before sinks close, without terminalizing durable executions. Wakes (scheduled automation, V3 task recovery, child completion, exact execution) stay leased until a generation-bound ack; task handlers renew and self-cancel on replacement; watchers reserve a handler slot before leasing. Agent-wide then exact-execution cross-process exclusions (that order) cover terminal publication, cancel, public controls, tree pause/resume, deletion, owner handoff and every admission. Sleep and `WaitingChildren` parks mint no terminal receipts. |

**Deliberate limits.** Journal-to-canonical persistence is at-least-once; the
live broadcast after it is best-effort (no journal key or consumer ack, so lag
can drop and recovery can duplicate).
Process loss during Decide may repeat provider cost (no durable provider job
id). Process-local secrets, broker credentials and Primitive runtime proofs are
live-host-only and fail closed. Journal hot paths use a verified derived tail
index (full-parser fallback); flock acquisition verifies the descriptor still
names the published inode.

### Run-loop inventory

| Module | Owns / rule |
|---|---|
| `run_loop/state.rs` | `RunIdentity` (copy-on-write, `Arc<Mutex<Arc<..>>>`) and `LoopState`; ten fields are **wire-required** (absence is a parse failure) because their defaults are permissive (absent `max_spawned_tasks` = no cap; absent `journal_seq` orphans the log). |
| `run_loop/outcome.rs` | `Phase` (Prepare, Observe, Decide, Resolve, Apply, Epilogue), `BoundaryOutcome`; no `Terminal`/`Pause`/`Park` variant, so a `break` never ends a run; source-scanning tests pin exits per phase. Decide is not effect-free (billed; may dispatch `browser__screenshot`). |
| `run_loop/phases/` | Six functions returning `PhaseStep::Exit(BoundaryOutcome)`. |
| `run_loop/journal.rs`, `effects.rs`, `store/` | Append-only journal, effect ledger keyed by `effect_id`, `LoopStateStore` (filesystem + in-memory); CAS on `Revision`, filesystem via `link(2)`. |
| `executor.rs::build_run_setup` | Owner profile, registry scoping, prompt context, tool index, capability snapshot, ephemeral-secret sync, deadline watchdog (order load-bearing) → `RunSetup::Ready(Box<RunBindings>)` or `RunSetup::Concluded` (`SleepUntil`). |
| Loop-state key | `loop_state_execution_id(execution_id, resume_generation, nesting, refinement_pass)` via `loop_state_address` (trivial components omitted), since `handle_spawn_sub_goal_decision`, in-context `handle_delegate_to_agent_decision` and `execute_agentically_with_refinement` re-enter with the same execution id and offset. Not `ctx.depth` (a delegated child is top-level at `depth == 1`). Controller attempts add their validated hash as an axis; the exact base/segment binding stays the durable ownership authority. |
| Outbox | `journal_and_emit` splits a `RuntimeTransportEvent` into `JournalBody::Event` (byte-exact `#[serde(tag, content)]` split), buffers for post-commit projection per `loop_state_address` with `RecordedEventRouting`; `routing_for` takes the **bare** execution id (nested segments' events belong to the shared execution). |
| `run_loop/worker_runner.rs` | `sweep`: scan, claim, `advance_in_session` to a boundary, release; refuses a mid-iteration cursor. `classify` is total over 17 variants; `after_park` re-checks because `leave_park` does not move the cursor. Boot asks each row's Artifact/runtime or PlanGraph owner to compose the host; a generic fleet over arbitrary keys is forbidden. |
| `run_loop/reconciler.rs` | `scan_parked`: `Unwakeable` (proved) vs `Stalled` (suspected). `Recovery::RetireProvedParks` takes a `RetirableGround` set; live caller opts into `DeadlinePassedWhileParked` only. `EveryChildHasFinished` is unsound under the refinement axis (addresses children by bare id); `NoChildrenNamed` declined (`check_park` refuses empty lists); `ChildMayStillBeContinued` never retires; a cursor past the committed watermark refuses retirement. A committed `Children` park is inert while `.resolve_wake(` is unwired. `RetirementRefused::GroundNotOptedIn` ≠ `GroundIsNotActedOn`. |
| Store | `commit` publishes the ending beside the state (`ended.json` / `ExecutionSlot.ended`), never at `append_journal` (a `RunEnded` above the watermark is an orphan). Filesystem commits prepublish a current-plus-proposed revision binding before the snapshot CAS. `scan_runnable(worker, limit, after)` pages in order; an over-window directory reserves its last entry as next-page sentinel. Cold scans withhold a terminal's exact revision; claim repair republishes a missing marker from the journal. Store filters cannot see `max_phase_attempts`, `max_iterations` or indeterminate effects; the process-local holdoff is suppression, not correctness. |
| Cold entry | `LoopState::foreign_pickup`: missing Decide carry → Observe; Resolve/Apply restore the pre-Resolve capsule or re-observe; Prepare commits an iteration-bound Epilogue checkpoint (history baseline, wall-clock start); browser decisions re-observe. Process-local secrets suppress checkpoint publication. |
| Exact recovery | `EffectIndeterminate` Adopt/Refire keyed by source segment and `EffectId`. Coding reattach needs a live unsettled invocation matching engine, native session, scope root, project root, root task and invocation generation. A sealed fixed-roster outer owner (roster + revisioned progress) settles first; the active stage's absolute deadline encloses the whole exact-resume future. |
| Misc | Observation ids `obs_{seq}_{label}_{uuid}`. `execute_direct_path_on_scheduler_root` keeps `history`, `current_state`, `loop_protective` in one scheduled-state cell so neither cancellation path persists empty bindings as a resumable checkpoint. |
| Placement | `Placement::for_commit` renews the pin on **every** commit (`PIN_TTL_MS` = 15 min after last commit); a live run stays with its driver (cooperative handoff and a shorter no-secret TTL were refused: open-questions §1). `portable_ceiling_is_cdp_only` runs before pinning. `Placement::Portable` means **not yet committed**, not movable; keep the variant. `advance_under_lease` checks PARK before DEADLINE. The terminal originates in the journal. |

### Event projection and outbox

One delivery path per event: journal-accepted records are projector-only;
outbox-refused records (and everything on `inprocess`, which has no drain) emit
inline; the producer branches on the journal op's acceptance bit (both paths
double a timeline; neither blanks a surface). Pinned by `accepted_records_project_and_refused_records_emit_inline`
and `every_driver_selects_exactly_one_outbox_delivery_path`;
`outbox::arm_has_a_drain` matches `ExecutionDriver` exhaustively (`Inprocess` →
`false`; the module will not buffer there). `run_phase`'s entry drain discards
leftovers from an uncommitted attempt.

- `emits_projected_events() == false` by default (hosts with no transports);
  production opts in.
- `HostEventSink::emit_routed` preserves canonical vs transport-only;
  `emit_named` carries `plan.step.started` and `tool.result.projected`.
  `emit_event` and `routing_for` share `artifact_v2::canonical_runtime_fact_of`.
- `WorkerHost::emit_projected_named_event` defaults to **refusal**;
  `ProjectorCursor::project` stops below a refused record (`HOST_IMPL` in
  `run_loop::mod` asserts both).
- A live non-terminal pass that emitted, deduped, dropped and refused nothing
  skips the mark write (`MarkDurability::LiveBoundary`; the next pass saves it).
  Every mark write is fsynced (`save_projector_cursor_fenced`; an unsynced rename
  can leave an unparsable mark after power loss); a corrupt mark fails the run's
  projection closed.

Terminal delivery: a periodic lifecycle projector (immediate at boot) scans
committed debt in cursored pages, re-verifies each key under lease, replays the
recorded route and canonical scope, and leaves debt on any failure. A non-empty
cursor continues immediately; the 30 s cadence applies only at end or after a
failed page. The canonical sink's receipt resolves after durable append; the
cursor advances only after every receipt in the page succeeds **and** lifecycle
acceptance. Discovery = event projection debt ∪ exact-terminal
settlement debt.

Receipts: ordinary final, HITL (incl. `PausedByUser`) and taskless direct
endings commit a scope-HMAC-sealed receipt before the terminal `LoopState` CAS;
fixed-roster terminals keep full stage output in a protected sidecar (receipt
binds digest, length, media type, segment, stage/attempt, terminal sequence);
fixed-roster pauses/terminals reduce through their outer progress CAS first.
Truncated progress records exact omitted counts and renders visible omission
markers; delegated-child failure receipts re-enter child policy.
`HandedOff` is successor retirement, never a root outcome; an eventless
reconciler `CannotProceed` may carry no receipt. Convergence is replayable and
idempotent, not one transaction.

Both lifecycle owners wait on a one-shot readiness latch (config,
Artifact/runtime services, delegation dispatch composed), so boot recovery never
runs under partial authority. Steering acks bind the runtime execution id and
exact segment. Canonical persistence and settlement use separate lease windows. Content-derived `EventKey`s dedupe retries and become
`source_event_ref`; the recorded `ui_thread_id` routes `app:<installation_id>`
facts without trusting transport content. Event-id recovery uses a derived exact-id
index, falling back to a chunked journal scan (not bound by the recipe log's
8 MiB replay cap). Refused rails: `phases::outbox` *WHAT IS NOT JOURNALLED*;
spawned closures and trait-object calls are blind spots of the source scan.

### Effects, Apply, and coding reattach

`Apply` = `gate_apply` → intents → `dispatch_apply` → outcomes.
`phases::apply::dispatch` requires an `ApplyIntents` receipt only a driver can
mint; `driver_worker::record_batch_intents` / `record_batch_outcomes` bracket
it. `resolve_effects` covers the committed batch
(`splits_apply_gate_from_dispatch` defaults `false`) ∪
`EffectLedger::unsettled`.

`phases::apply::declared_retry_safety` is the single per-**action**
retry-safety site (not `is_parallelizable_read_only_action`: "may run beside
another" ≠ "may run twice"; nor pack `reliability`). `RetrySafety::Reattachable` applies to `run_coding_task`;
`reattach_ref` is the coding **invocation id** from `prepare_coding_invocation`
(durable before start), not `native_session_id`; `WorkerHost::reattach_state`
resolves it.

`apply::dispatch` takes an `effects::EffectPlan` (a value, not a capability;
verdicts index-parallel to gate admission; naming an unadmitted effect is
refused); `apply::plan_the_batch` matches `EffectAction` exhaustively (a `_` arm
would re-fire a live effect):

| action | dispatch | settled |
|---|---|---|
| `Refire` | fires (retry-safety or positive evidence, plus reproduced fingerprint) | as reported |
| `Adopt` | no | nothing (row has outcome) |
| `AlreadyFired` | no | `Succeeded` |
| `Reattach` | stamps `__coding_invocation_id` + `__coding_resume_session_id`, resumes session | as reported |

`run_coding_task` validates engine, session, scope, project root, root task and
generation before binding the resume slot. Settled invocations never resume; a
row with no durable result body needs an operator Adopt/Refire. Engines report their native session as soon as known
(`ledger::attach_live_invocation_session`):

| engine | reported at | uncovered window |
|---|---|---|
| Codex | `thread/start` returns | spawn → handshake |
| Grok | `session/new`/`session/load` returns | spawn → handshake |
| Pi | `prompt` RPC ack | spawn → prompt ack |
| Claude | `system/init` frame | spawn → first frame |
| Agy | `init` frame; factory Unconstructable | (not live) |

`mark_invocation_may_have_started` runs just before the engine is touched (Codex
adds `accept_invocation_turn` from `turn/start`), so
`CodingDispatchState::automatic_retry_allowed` separates *never started* from
*started, unknown*. Decide has no in-flight marker (provider exposes no job id).

Cold Apply journals a rewind to Resolve, restores the pre-Resolve capsule and re-resolves
under current authority. `PhaseReport.pending` marks the complete batch before
dispatch; failures keep the successful prefix without advancing the cursor.

### Rules

- Guard a value where it is interpreted (`reconcile_outward_effect`), not where
  produced (`act_path`).
- `OutwardActStatus::Failed` has two writers (`refuse_outward`; receipt sweep
  after a hard bounce); `dispatched_at` discriminates.
- `serde` yields `None` for a missing `Option<T>` regardless of
  `#[serde(default)]`; wire-required needs `deserialize_with`.
- A pin needs a bound renewed by proof of liveness; otherwise a scan offers a
  live run's key between phases and its next claim gets `Advanced::LeaseHeld`.
- `Placement::Portable` was drawn around the wrong resource (a local Chrome
  between phases vs. a chat thread's browser session between executions);
  redrawing it is new design
  (open-questions §2).
- Folding a forked `LoopProtectiveState` reads `loop_recovery_context: None` as
  "cleared", so callers filter defaulted members
  (`a_defaulted_member_erases_the_base_which_is_why_the_caller_filters`).
- A forked detector cannot be merged exactly (each member is `base + at most
  one record_action_result`); the fold errs toward detecting cycles later,
  never earlier.
- Count emitters transitively over every vocabulary, not at depth 1.
- `TerminalKind::is_resumable()` is not "can continue": `BudgetExhausted` and
  `MaxIterationsReached` carry a "Continue or cancel?" pause but return `false`.
- Guard both ends of a two-commit transition (`leave_park`).
- One execution id can address several runs; enumerate every re-entry path on
  an inherited context before choosing a discriminator.
- One switch over grounds of unequal soundness is a defect; opt in per ground.

## Ungranted-tool dispatch

`classify_flat_route` uses the **per-agent scoped** `CapabilityRegistry`, so a
miss means "not granted": it falls to the Compiled/`Unknown` arm and fails with
`ungranted_capability_error`
(`flat_loop/dispatch.rs`) naming tool and agent: delegate or finish; do **not**
retry. Snapshot absence ≠ empty snapshot: a detached harness or
startup-registry fallback without a scoped revision keeps its owner/legacy
catalog (no shared surface cache, no empty ceiling); a real effective-policy
snapshot, even tool-free, is authoritative and fail-closed. Trust is checked
before approval and catalog-miss recovery: confirmation cannot legalize a
trust-denied action, which fails the run with the canonical trust-denial
reason; non-denied calls outside the active catalog take the corrective path.

## Subprocess PATH

Scope-tool subprocesses prepend tool-bin dirs via the single source of truth
`CapabilityScopePaths::subprocess_bin_path(extra_bin, parent_path)` (existing
`[extra_bin?, venv_bin, node_modules_bin, node_bin]`; nothing found → PATH
untouched). Callers: `cli_template/dispatcher.rs` (per-skill `bin/`),
`agentic/preflight.rs::run_probe`, `compiled_providers.rs::run_auth_command`,
`bots/mod.rs` (magician-learning), `meetings_api.rs`, `skills_api.rs`,
`pack_provider.rs::execute_command`, `skills/runner.rs::run_ephemeral`. New
spawn sites reuse it.

## Keep-tail context window

decide() sees the last 25 assistant/user pairs (`OUTER_VERBATIM_MAX_ITERATIONS`),
then oldest-first eviction to `OUTER_VERBATIM_TOKEN_BUDGET` = 40,000 tokens
(ceil(UTF-8 bytes / 2)); the newest pair is never dropped.
`history.live_messages` is append-only and round-trips resume unchanged;
`last_response_id` clears only when the sent prefix changed. There is no second
model; the unused `op-agentic-ledger-compaction-local` profile must not be
rebound to decide(). Pi's own compaction and analytics parquet compaction are
unrelated.

<a name="llm-led-ledger-compaction-available-default-off"></a>

### LLM-led ledger compaction (removed)

Removed; do not revive on the active-task path. `llm-router.yaml` still maps
`agentic_ledger_compaction`, but nothing dispatches it. Historical
design.

## In-turn parallel reads

Leading read-only follow-ups (file reads and named packs incl. `grep`, `glob`,
`web_search`; not HTTP GET or DuckDB) may `join_all` in one turn, merged in
provider order, if the next turn's full gate would pass them. **Serial**:
absolute paths outside sandbox allowed roots (plus session overrides), `..`
escapes, unauthorized tools (two sandbox/tool-auth parks cannot share a pause
key). Calls failing the agent allowlist, carrying criticality or the model's
confirmation flag, or matching an approval rule stay out of the slice; the
sequential tail stops before them and they return through the full gate. If
the first candidate fails or any sibling fails, parks or cancels, the mutating
tail does **not** start. Writes, shell and
browser stay serial `--bail`.

## Web answer lane

- `web_search`: free DuckDuckGo HTML results; parallel-read candidate.
- `web_answer`: one function-tool-free request via `op-web-answer`
  (`server_web_search` metadata → server-side search with citations); billed
  per search plus tokens; **serial**.
- research agent: multi-source synthesis.

magicllm `server_web_search` never mixes with function tools on one request; unsupported transports
fail closed. Without the `web_answer: op-web-answer` mapping and metadata,
`web_answer` fails closed rather than answering ungrounded. Spend gate
$0.05/call (`spend.cost_per_action` in `embedded_pack_defs/web_answer.yaml`); `search_calls` ride `LLMResponseReceived` into settlement,
telemetry and lakehouse recompute.

## Relentless proposals

`propose_relentless` (handler-backed compiled capability for ordinary active
tasks) proposes a typed local
text acceptance contract through durable owner requests. It does not dispatch an
actor/critic, create a task, or grant budget/profile/delivery authority. Scope,
agent and execution are runtime-stamped and checked; the owner adopts the exact
proposal and contract hash in the Relentless workspace. Multi-step actors also
need the physical resource meter; the proposal capability alone does not enable
that adapter.

MCP affordance cards use the durable atomic byte writer (no shared fixed temp
file under concurrent discovery) and are optional discovery guidance.
