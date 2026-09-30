# Flat Loop

Behavior contract for the flat per-action loop and the stateless `run_loop`
driver. Design provenance (archived):
flatten,
stateless design,
driver decisions.

## Why

Every pack primitive (duckdb, gmail, metabase, skillshub subprocess skills,
browser, …) is a leaf `<pack>__<action>` the outer LLM calls directly, dispatched
without a nested LLM through `execution/primitive_dispatch/`. The pack schema
value is `type: primitive`.

To keep the prompt buildable for high-cardinality packs (metabase: 104
primitives), the catalog splits into a **hot** tier (full schemas, eager) and a
**deferred** tier (bare names; schema fetched via `tool_search`).

There is no loop-mode toggle; every execution runs the flat loop.
`MagicianConfig` is `#[serde(deny_unknown_fields)]`, so a leftover `inner_loop:`
key hard-errors at load.

## Module: `magician_v2::execution::flat_loop`

### `catalog` — the two-tier catalog

- `ALWAYS_HOT_TOOL_NAMES` (fixed in code, no per-agent override): `yield`,
  `need_user_input`, `spawn_sub_goal`, `read_result`, `tool_search`, `shell`,
  `read_file`, `write_file`, `edit_file`, `grep`, `glob`, `http`, `web_search`,
  `content_search`. Native control tools are not pack-index entries
  `tool_search` can select; search is hot so a first-turn lookup does not fall
  through to `shell`/`http` scraping.
- `conditional_hot_tools(ctx)`: `delegate_to_agent` + `get_agent_details` when
  the agent has delegation targets; `activate_skill` with procedure skills;
  `handover_to_agent` with an authorized handover target.
- `CODING_HOT_TOOL_NAMES`: an agent that *directly* grants `run_coding_task` /
  `apply_code_proposal` / `run_project_checks` / `list_proposals`
  (`ctx.direct_capabilities`) gets them hot, avoiding a first-turn `tool_search`.
  A coordinator that only inherits them via delegation is untouched.
- `## AVAILABLE SPECIALIST TARGETS` is lean: id, aliases, short description,
  pack names, and the admitted `delegate_to_agent`/`handover_to_agent` routes.
  Full guide and leaf names come from `get_agent_details(agent_id)`; harness
  management stays in `inspect_agent`.
- `build_flat_loop_tools(ctx, index) -> FlatToolCatalog { hot, deferred }`. Hot
  tools reuse `agentic::native_catalog` builders (byte-identical schemas).
  Deferred entries are `DeferredEntry { name, group, search_hint }`, one per
  reachable leaf minus the hot set.

### `tool_index` — server-side schema store

- `ToolIndex` is built once in `compiled_providers::build_compiled_registry` from
  all loaded packs and installed on `AgentResources.tool_index` (`OnceLock`).
- `build_tool_index(packs)`: a compiled / single-leaf pack is one leaf named
  after the pack; a pack with `native_action_schemas` is one `<pack>__<action>`
  leaf per primitive (parameters from the action's declared subset, pack-level
  types unless overridden: `tool_index.rs::primitive_parameters_schema`).
- Multi-action leaves inherit a normalized selector hint (max 180 chars) from the
  pack description; it feeds ranking and renders once per distinct description in
  the deferred prompt. Single-action packs get no hint.
- Lookups: `get`, `leaf_names_for_pack`, `select(names)`, `search(query, max)`
  (weighted regex: exact name-part 12, partial 6, full-name 3, hint 4,
  description 2; `+term` pre-filters).

### `tool_search` handler

`compiled_handlers/tool_search.rs` calls pure `search_response(index, query,
max)`: `select:A,B` → full schemas; keyword → ranked matches; no index → an
"inactive" shape (everything already eager).

Name resolution (`ToolIndex::pack_for_selected_name`, shared by `select` and
`project_family_selection`): an exact leaf is itself; a pack name is its leaves;
an unknown `<pack>__<action>` is its pack. On a whole-pack surface (chat, voice;
`build_surface_tool_index`) leaves collapse into one entry named after the pack,
so `select:browser__open` still works there. An empty select says **nothing was
loaded** and points to keyword search then `select:<name>`; keyword results carry
`how_to_load` (a listing is not a load). Why: a model told "now available" over
`matches: []`, or shown a name in a listing, assumed it was callable and fell
back to `http`.

Working-set effect of `select:` (`flat_loop/working_set.rs`, applied by
`executor.rs::record_loaded_tools_from_action`): load the authorized leaves of
every pack named (by leaf or pack name — a pack is one tool with many verbs),
**merge** into the loaded set, evict the oldest family only past the limit
(`WorkingSetLimits::autonomous_compatibility`: 4 families per run;
`agent_surface_runtime.working_set.max_loaded_families`, default 2, per surface),
and report the change in the next prompt's `## WORKING SET CHANGE`. A pack that
would exceed the tool/schema budget loads only the named leaves. Why merge and
whole-pack: a replace-on-select set let an unrelated select evict the browser
family mid-task, leaving the model without `snapshot`.

### `dispatch`

`dispatch_flat_action(tool_name, args, registry, exec_ctx, cancel)`:

- `parse_flat_tool_name` splits `<pack>__<action>` on the first `__`.
- `classify_flat_route` → `FlatRoute`: `CompiledProviderPrimitive`,
  `CliTemplatePrimitive`, `Browser`, `PlainCompiled`, or `Unknown`.
- Primitive leaves go to `primitive_dispatch::dispatch::dispatch_primitive`
  (`CompiledProviderDispatcher` / `CliTemplateDispatcher`, one
  `PrimitiveDispatcher::dispatch`, folded into an `ActionResult`). Bare-name
  compiled packs go to `compiled_dispatch::try_dispatch_compiled_pack`.

`PrimitiveExecCtx` carries only scope/identity, browser session side-channels,
secrets, progress sink, event broadcaster, and dispatch authority. Spend gating
is inherited on the compiled-provider path (`execute_maybe_gated`); reliability
wrapping (CapabilityScheduler retry/concurrency) is at the executor call site.

### Shipped: flat is the only loop

The observe-decide-execute loop, `ExecutionHistory`, `yield`/`dispose_yield`,
and cancellation are shared. The two flat seams: `build_catalog_context` expands
each primitive pack to leaves; `execute_action` routes flat-classified calls
through `flat_loop::dispatch_flat_action`. `yield` stays terminal via
`dispose_yield`.

## Browser observation and decision

A browser task is judged by whether the page reached its goal state, not by which
tool was used; an `eval` that fires an element's own handlers is valid.

The strict METHOD gate (real interaction primitive before `goal_reached`,
no-page-effect acts as stuck failures, before/after effect digest) is **off by
default**, behind `strict_browser_interaction_gate_enabled()` (env
`MAGICIAN_STRICT_BROWSER_GATE`), for primitive benchmarking only. Sites:
`executor.rs::goal_reached_rejection_reason`,
`decision.rs::try_synthesize_stuck_auto_yield`, `primitive_dispatch/dispatch.rs`.

`space_batch_commands` (`primitive_dispatch/browser/dispatch.rs`) waits
`MAGICIAN_AGENT_BROWSER_MUTATION_SETTLE_MS` (default 150 ms) after a mutation
(`click`/`drag`/`fill`/`type`/`press`/`check`/`uncheck`/`select`/`tap`/`upload`)
and `MAGICIAN_AGENT_BROWSER_BATCH_SPACING_MS` (default 50 ms) between read-only
steps and raw `mouse` fragments. Mirrored in the Yutori translator.
`MAGICIAN_AGENT_BROWSER_BATCH_SPACING_MS=0` disables all waits.

The decision seam passes `PageState.viewport_size` through
`native_decision_via_adapter*` so the router injects `extra.viewport`; vision
cohorts (Yutori) denormalize 1000×1000 coordinates to CSS pixels with it.

Yutori is off by default and activated by config only: bind `agentic_decision`
to `vision-yutori-n1.5` as the `when_has_images` arm of a
`default`/`when_has_images` map, then restart.
`OperationLlmRouter::agentic_decision_uses_yutori()` is true when either profile
has provider `yutori`.

Observations are text-first (no screenshot). With Yutori, the policy forces a
`Full` capture each iteration: a read-only `browser__screenshot` gated on a live
browser session (`browser_session_used`); state is rebuilt via
`build_flat_browser_state`. `perform_observation_with_policy` arms pass
`current_state` through unchanged.

Outside a browser state, the decision image is **the screenshot the last action
captured**: a successful pack call whose params carry a non-empty `.png`
`screenshot_out_file` is attached to the next decision
(`decision.rs::last_action_screen_capture`, max 8 MiB). Only the immediately
preceding action counts; a failed capture or missing file attaches nothing. Why:
desktop-automation agents otherwise get their own screenshot back only as a path.

Yutori's bare actions (`left_click`, `mouse_move`, …) are rewritten to
`browser__<action>` at lowering so the allowlist accepts them and they classify
as `Browser`. Native Yutori actions omit `task_state_action`; re-captured state
is stamped with the live viewport. `scroll` emits a cursor-anchored `mouse
wheel` (traps on nested `overflow` containers; the baseline uses
`scrollintoview <selector>`). `Page.handleJavaScriptDialog: No dialog is showing`
is a soft success, not a CDP disconnect. The flat session threads
`with_initial_url`.

## Pause, resume, and provider continuations

- `AgenticPauseState.live_messages` (`#[serde(default)]`; legacy blobs load
  empty). `build_full_pause_state` persists `history.live_messages` plus the
  mid-turn tail (`decision::pending_live_messages`).
- `restore_context_from_pause` → `AgenticContext::resume_live_messages`, with bare
  `synth_iter_*` tool ids namespaced `r{generation}_…` (generation = resumed
  `iteration_offset`; numbering restarts at 1). `seed_resumed_history` seeds
  exactly once (`mem::take`).
- Ids that must not repeat across a pause use the **global** iteration
  (`iteration_offset` + local): tool-result call id
  `{execution_id}:iteration:{n}:record:{k}:{tool}` and
  `synthetic:{kind}:{execution_id}:{n}`. Why: the raw-result store refuses a
  different payload under an existing id, so local counts made resumed results
  unreadable (`materialization_or_projection_failed`).
- The continuation wrapper transfers live messages, provider response id, image
  cohort, and compaction resume projection into the first resumed segment only;
  refinement clears all four. The watermark seeds to `iterations.len()` so
  `sync_live_messages` folds only post-resume records. A provider continuation
  checkpoint is persisted when available; the adapter falls back to cold
  reconstruction if it is rejected.
- With a restored conversation, the `[Prior actions …]` summary is replaced by a
  compact resume marker (`build_exact_resumed_goal`, `build_resumed_goal`,
  `execute_agentically_continue`); empty `live_messages` keeps the
  summary-into-goal fallback. Exact resume reuses frozen `preserved_initial_tools`.
- A delegation checkpoint owns its environment snapshot. `WaitingForChildren`
  envelopes keep the legacy `last_state` field as an `Uninitialized` sentinel with
  the real state in an optional exact checkpoint. A resumed parent that delegates
  again settles from `WaitingChildren`; the FSM accepts `WaitingChildren →
  WaitingChildren` (`resume_delegation_checkpoint`) — refusing it strands the next
  round's handoff descriptor.

Provider continuations are revision-aware: a full bootstrap fingerprints the
dynamic observation, task state, compact plan, delegation results, capability
projection, active procedure, call-frequency/file summaries, and supplemental
knowledge. A continued turn sends only changed sections; clearing a section emits
a tombstone. Fingerprints commit only after a successful response. Owner, trust,
capability, or routing-authority changes discard the checkpoint and force a full
bootstrap; nested/refinement runs keep independent ledgers.

## Terminal evidence and derived sink

- `goal_reached_has_sufficient_evidence` ignores iterations the harness
  contradicted (`NoEffect` / `BROWSER_NO_EFFECT_MARKER`, `Mismatch`/`NoChange`
  verification; `iteration_carries_negative_harness_signal`). `Unknown`/absent
  verification is acceptable. Micro-goal completions stay gated
  (`observed_evidence_ref` + `step_completion_supported_by_iteration`).
  `evidence_refs` are free-form and not existence-checked.
- The give-up gate (`premature_giveup_rejection_reason`) rejects premature
  `Failed`/`RetryTransient` yields with budget and recoverable work left.
  Exempt: stuck-auto-yields, permanent blockers, 3-strike honour, budget margin.
- A rejected terminal claim is an `IterationRecord` folded as a whole
  (assistant, user) pair; an emitted-but-unrun tool_call folds as a
  `{"status":"deferred"}` `ToolResult` to keep tool_use/tool_result pairing.
- Before the precision judge reviews a factual draft, opened-page records group
  by canonical URL (then durable source id, content hash, record hash); repeated
  reads collapse to the latest success. Publisher canonical URLs win; fragments
  and tracking params are dropped, semantic query params kept. The 24 KB judge
  packet is split deterministically across sources; discovery snippets and failed
  reads are excluded. Three rejected drafts fail closed.

## Stateless run_loop driver

Stateless is the default. `MAGICIAN_EXECUTION_DRIVER=inprocess|stateless`; blank
or unknown → `stateless` (logged); `inprocess` is the explicit rollback. Resolved
once per run in `run_loop::driver_for_run`. `GET /health/execution-driver` exposes
per-arm counts (`driver_run_counts()`), the resolved arm, and the raw env value.

Production cold composition comes only from Artifact/runtime recovery (including
sealed delegated-child bindings) or a validated durable PlanGraph. A loop-state
row alone carries no authority; generic `HostFleet` discovery is not a production
composer. Direct admission refuses work with neither source before `Executing`.

- `driver_inproc::run_iteration` advances **one iteration** → `IterationStep`.
- `driver_worker::advance_once` advances **one phase** (claim → load → phase →
  commit → release) → 17-variant `Advanced`. Keep them separate: merging deletes
  the per-phase commit.
- `executor.rs::StatelessArm` is the claim loop over `InProcessWorkerHost` (trait
  in `driver_worker.rs`, impl in `executor.rs` because phases take
  `TrustDispatchGuard` and `AgenticToolLineageState`). Every `Advanced` variant is
  handled by name; a source gate in `run_loop/mod.rs` refuses a catch-all.
- Normal setup rebuilds services for the invocation key and identity adoption
  proves they match committed authority. Lease heartbeats keep the pin live during
  long phase awaits.

### Recovery, launch, and admission

Operator cutover: legacy-writer runbook.

Principle: every recovery path requires typed, sealed, exact authority; anything
ambiguous stays durable debt and defers without mutation. Only a typed not-found
is absence; scope, lineage, I/O, decode, or integrity errors fail closed.

**Legacy writers and the scope seal.** Interrupted recovery uses an exact
per-base reverse catalog. Older writers may omit it, so `Absent` and
`RecoverableExact` are mutation-free deferrals until legacy writers are drained, a
deployment-bound catalog walk completes, and each principal/workspace scope is
sealed. Pre-seal walks are positive discovery only. The same gate covers the
first stateless launch of a new runtime shell. Post-seal, startup classifies
active-run authority in index-only chunks of ≤20,000 base execution ids.

**Root states.** Canonical roots persist `Planning` as never-started. Final entry
needs the exact Runtime generation and absence of pause, manual-resume,
live-control, and base-segment authority (authoritative only after the seal).
`Runnable` is not a fresh-root marker: a new delegated child may use its
creator's one-shot nonce; every interrupted `Runnable`/`Executing` run needs exact
or pre-seed recovery authority.

**Accepted launch.** Direct message execution, explicit starts, and
create-with-initial-message never return 202 for a detached future. Before
acceptance, a scope-HMAC-sealed envelope binds Runtime generation,
Artifact/root/owner ids, launch mode, message, and bounded options, with a
renewable `Pending -> Claimed -> Consumed` lease that owns crash recovery through
`Planning`/`PlanningComplete` and the pre-seed/first-loop window. The envelope
stays Pending until a waiting/terminal Runtime state or positive loop authority
owns restart; future completion is not a handoff. Planning mode prewrites one
deterministic inbound turn; a conversation store without exact turn insertion
refuses rather than append a duplicate. Startup deletes Consumed envelopes (≤20,000
per scope) only when HMAC, hashed path, identity, and a fenced re-read agree.
Legacy/taskless PlanGraph rows recover via typed base-execution authority, never
an invented intent.

**Startup queues.** Recovery work runs on bounded, deduplicated,
deadline-indexed queues registered with the process-wide execution-job registry
— no sleeping future or semaphore waiter per row:
- legacy/taskless active PlanGraphs: ≤32 workers, backoff 250 ms–30 s until a
  peer lease expires, pre-seed grace passes, launch succeeds, or the Runtime
  leaves `Runnable`/`Executing`;
- planning commits: sealed Runtime/turn/goal receipt + journal-first per-task
  recovery-catalog marker, 16 workers; a one-time canonical walk bootstraps
  markers for pre-catalog rows (the list cache is never restart authority);
- AskLoop resume catalog: 128-entry pages via durable cursor, 16-worker shared
  deadline coordinator; fixed-roster outer-stage cursors, accepted-launch
  rechecks, and delegated-child attachment repair each use their own queue.

**AskLoop continuations** use a scope/task/plan/execution/question-bound
`Prepared -> Committed -> settled` receipt, durable before an answer, resume,
guardrail cancel, or pause is consumed; replay is idempotent and only Committed
dispatches under a renewable claim. Clarification-suspended planning turns are
non-terminal receipts (recovery keeps `WaitingUser`). Non-TaskPlan continuations
use a sealed Runtime resume catalog binding status revision, pause/state id,
source turn digest, and any completed PlanGraph; a graph created by replanning
must first be HMAC-bound before it may publish `PlanningComplete`.

**Delegated parents.** No-progress state is sealed and bounded (oversized child
vectors rejected before reads; agent/goal multiset fingerprinted). Completion
requires the exact active-child roster, validated Runtime/Artifact lineage, and a
persisted result summary; then one conversation-store settlement makes the parent
runnable and clears its child group. Children publish input before the sealed
launch-ready binding, then the parent commits `WaitingChildren` before scheduling.
The handoff descriptor is scope-HMAC-sealed over principal, workspace, task,
parent, and a capped child roster.

**Interrupted runs** classify as live, one exact recoverable segment,
settlement-pending, absent, or uncertain. Live/pending/uncertain defer without
mutation; absence applies only to the fenced pre-seed window. Candidates take a
durable admission token before async composition and revalidate token, Runtime
generation, and segment/revision under the lifecycle exclusion before the first
mutation. The exact timer alone owns Sleeping admission. Terminal settlement
discovery uses a checksummed priority catalog (published before terminal CAS,
retired after cursor settlement) with fallback to authoritative paging and a
periodic full audit; the catalog never replaces the journal/receipt source.

**Placement resumes** are backed by a complete checkpoint plus sealed
`PlacementRetry`, rebinding source segment/due generation at Artifact and runtime
admission. A newer exact `LoopState` forces owner/tool/trust recomposition.

**Wake queue and controls.** The wake queue reloads/publishes under
cross-process locks with per-execution activation fencing. Dispatchers hold the
row lock only for the stale-claim check (the lock is non-reentrant). Steer/cancel
state is durable and phase-consumed; steering binds the sealed runtime-control
UUID. Control, pause, steer, and exact resume resolve the execution's epoch
before the serving process's driver setting, so an `inprocess` peer cannot
misroute a stateless run. Manual tree pause publishes one sealed roster; manual
tree resume is a fixed-roster transaction (preparing members roll back,
committed roll forward, status-only revision prevents Paused ABA). A terminal
inbox fence routes late accepted input to another Decide.

**Terminal generations and ceilings.** A resumable body is persisted before the
terminal `LoopState` CAS as a hidden exact generation (HMAC over key, revision,
body digest, successor segment, stage/attempt, diff-approval disposition). Public
resume/continue/cancel get a retryable settlement-pending response and cannot
mutate it; pause activation is the final publication step. An expired running
segment ends as a non-resumable deadline failure (not cancel, not pause).
Exhausted work/token/cost ceilings and runtime-scope loss conclude inside the
claimed phase. Apply rechecks cancel, pause, deadline, ceilings, and scope after
intents commit and before first dispatch.

**Shutdown and lifecycle exclusions.** Detached jobs register at the
execution-runtime boundary; shutdown closes admission and drains futures before
sinks are torn down, without terminalizing durable executions. Wakes (scheduled
automation, V3 task recovery, child completion, exact execution) stay leased
until a generation-bound ack; task handlers renew and self-cancel on replacement.
Watchers reserve a handler slot before leasing. Agent-wide then exact-execution
lifecycle exclusions (cross-process, that order) cover terminal publication,
cancel, public controls, tree pause/resume, deletion, owner handoff, and every
admission. Sleep and `WaitingChildren` parks mint no terminal receipts.

**Deliberate limits.** Journal-to-canonical persistence is at-least-once; the live
broadcast after it is best-effort (no journal key or consumer ack, so lag can drop
and recovery can duplicate). Process loss during Decide may repeat provider cost
(no durable provider job id). Process-local secrets, broker credentials, and
Primitive runtime proofs are live-host-only and fail closed. Journal hot paths
use a verified derived tail index, falling back to the full parser. Flock
acquisition verifies the descriptor still names the published inode.

### Run-loop inventory

- **`run_loop/state.rs`** — `RunIdentity` (copy-on-write behind
  `Arc<Mutex<Arc<..>>>`) and `LoopState`. Ten fields are **required on the
  wire** (absence is a parse failure) because each defaults to the *permissive*
  value (absent `max_spawned_tasks` = no cap; absent `journal_seq` orphans the
  whole log).
- **`run_loop/outcome.rs`** — `Phase` (Prepare, Observe, Decide, Resolve, Apply,
  Epilogue) and `BoundaryOutcome`; no `Terminal`/`Pause`/`Park` variant, a
  `break` never ends a run. Source-scanning tests pin exits per phase. Decide is
  not effect-free (billed provider call; may dispatch `browser__screenshot`).
- **`run_loop/phases/`** — six functions returning
  `PhaseStep::Exit(BoundaryOutcome)`.
- **`run_loop/journal.rs`, `effects.rs`, `store/`** — append-only journal, effect
  ledger keyed by `effect_id`, `LoopStateStore` (filesystem + in-memory). Commits
  are CAS on `Revision`; the filesystem impl uses `link(2)`.
- **`executor.rs::build_run_setup`** — owner profile, registry scoping, prompt
  context, tool index, capability snapshot, ephemeral-secret sync, deadline
  watchdog → `RunSetup::Ready(Box<RunBindings>)` or `RunSetup::Concluded`
  (`SleepUntil`). Order is load-bearing.
- **Loop-state key** — `loop_state_execution_id(execution_id, resume_generation,
  nesting, refinement_pass)` via `loop_state_address`; trivial components
  omitted. Needed because `handle_spawn_sub_goal_decision`, the in-context arm of
  `handle_delegate_to_agent_decision`, and `execute_agentically_with_refinement`
  re-enter with the same execution id and offset. `ctx.depth` is not the
  discriminator (a delegated child is top-level at `depth == 1`).
- **Outbox** — `journal_and_emit` splits a `RuntimeTransportEvent` into
  `JournalBody::Event` (byte-exact split of its `#[serde(tag, content)]`
  encoding), buffers for post-commit projection, carries
  `RecordedEventRouting`. The buffer is keyed by `loop_state_address`;
  `routing_for` takes the **bare** execution id (nested segments' events belong
  to the shared execution).
- **`run_loop/worker_runner.rs`** — `sweep`: scan, read cursor, borrow a host,
  claim, `advance_once` to a boundary, release. `classify` is total over all 17
  variants. `sweep` refuses a mid-iteration cursor before claiming; `after_park`
  re-checks after a park exit because `leave_park` commits without moving the
  cursor. Boot asks each row's Artifact/runtime or PlanGraph owner to compose the
  host; a generic fleet over arbitrary keys is forbidden.
- **`run_loop/reconciler.rs`** — stalled-park detection over cursored
  `scan_parked`: `Unwakeable` (proved) vs `Stalled` (suspected).
  `Recovery::RetireProvedParks` carries a `RetirableGround` set; the live caller
  opts into `DeadlinePassedWhileParked` only. `EveryChildHasFinished` is unsound
  under the refinement key axis (addresses children by bare id);
  `NoChildrenNamed` is declined (`check_park` refuses empty lists);
  `ChildMayStillBeContinued` never retires (see rules). A cursor beyond the
  committed watermark refuses retirement. A committed `Children` park stays inert
  while `.resolve_wake(` is unwired (source-scanned).
  `RetirementRefused::GroundNotOptedIn` ≠ `GroundIsNotActedOn`.
- **Store** — `commit` publishes the ending beside the state (`ended.json` /
  `ExecutionSlot.ended`), never at `append_journal` (a `RunEnded` above the
  watermark is an orphan). Filesystem commits prepublish a
  current-plus-proposed revision binding before the snapshot CAS. `scan_runnable(worker,
  limit, after)` pages in order; an over-window directory reserves its last entry
  as next-page sentinel. Cold scans withhold a terminal's exact revision; claim
  repair republishes a missing marker from the journal. Invisible to store
  filters: `max_phase_attempts`, `max_iterations`, and indeterminate effects. The
  process-local holdoff is suppression, not correctness.
- **Cold entry** — `LoopState::foreign_pickup`: missing Decide carry rewinds to
  Observe; Resolve/Apply restore the pre-Resolve capsule or re-observe; Prepare
  commits an iteration-bound Epilogue checkpoint (history baseline, wall-clock
  start). Browser decisions re-observe. Process-local secrets suppress checkpoint
  publication.
- **Exact recovery authority** — an `EffectIndeterminate` Adopt/Refire is keyed by
  both source segment and `EffectId`. Coding reattach requires a live, unsettled
  invocation matching engine, native session, scope root, project root, root task,
  and invocation generation. Sealed fixed-roster work's outer owner (roster +
  revisioned progress) settles first; the active stage's absolute deadline
  encloses the whole exact-resume future.
- Observation ids: `obs_{seq}_{label}_{uuid}`.
- `execute_direct_path_on_scheduler_root` moves `history`, `current_state`,
  `loop_protective` through one scheduled-state cell; neither cancellation path
  can persist empty bindings as a resumable checkpoint.
- **Placement** — `Placement::for_commit` renews the pin on **every** commit
  (`PIN_TTL_MS` = 15 min after last commit); a live run stays with its driver.
  Cooperative handoff and a shorter no-secret TTL were refused
  (open-questions
  §1). `portable_ceiling_is_cdp_only` runs before pinning. `Placement::Portable`
  means **not yet committed**, not movable; keep the variant.
  `advance_under_lease` checks PARK before DEADLINE. The terminal originates in
  the journal.

### Event projection and outbox

Exactly one delivery path per event: a record the journal accepted is
projector-only; a record the outbox refused (or any record on the `inprocess`
arm, which has no drain) is emitted inline. The producer branches on the journal
op's acceptance bit. Both paths doubles a timeline; neither blanks a surface.
Pinned by `accepted_records_project_and_refused_records_emit_inline` and
`every_driver_selects_exactly_one_outbox_delivery_path`.
`outbox::arm_has_a_drain` matches `ExecutionDriver` exhaustively (`Inprocess` →
`false`, and the module will not buffer there). `run_phase`'s entry drain
discards leftovers from an uncommitted attempt.

- The trait default `emits_projected_events() == false` is for hosts with no
  transports; the production host opts in.
- `HostEventSink::emit_routed` preserves the producer's canonical vs
  transport-only decision; `emit_named` carries `plan.step.started` and
  `tool.result.projected`. `emit_event` and `routing_for` share
  `artifact_v2::canonical_runtime_fact_of`.
- `WorkerHost::emit_projected_named_event` defaults to **refusal**;
  `ProjectorCursor::project` stops with its mark below a refused record. The
  `HOST_IMPL` gate in `run_loop::mod` asserts both rails.

Terminal delivery: a boot-immediate periodic lifecycle projector scans committed
debt in cursored pages, re-verifies each key under lease, replays the recorded
route and canonical scope, and leaves debt on any failure. A non-empty cursor
continues immediately; the 30 s cadence applies only at end or after a failed
page. The canonical sink's receipt resolves after durable append; the cursor
advances only after every receipt in the page succeeds **and** lifecycle
acceptance. Discovery = event projection debt ∪ exact-terminal settlement debt.

Receipts: ordinary final, HITL (including `PausedByUser`), and taskless direct
endings commit a scope-HMAC-sealed receipt before the terminal `LoopState` CAS.
Fixed-roster terminals keep full stage output in a protected sidecar; the receipt
binds digest, length, media type, segment, stage/attempt, and terminal sequence.
Truncated progress records exact omitted counts and renders visible omission
markers. Delegated-child failure receipts re-enter child policy. Fixed-roster
pauses/terminals reduce through their outer progress CAS first. `HandedOff` is
successor retirement, never a root outcome. An eventless reconciler
`CannotProceed` may carry no receipt. This is replayable, idempotent
cross-layer convergence, not one transaction.

Both lifecycle owners wait on a one-shot readiness latch (config,
Artifact/runtime services, delegation dispatch composed) so boot recovery never
runs under partial authority. Steering acks bind the runtime execution id and
exact segment. Canonical persistence and settlement use separate lease windows.
Content-derived `EventKey`s dedupe phase retries; the key travels into canonical
admission as `source_event_ref`, and the recorded `ui_thread_id` is stamped into
the payload so observers route `app:<installation_id>` facts without trusting
transport content. Event-id recovery uses a derived exact-id index with fallback
to a chunked journal scan (not bound by the recipe log's 8 MiB replay cap). See
`phases::outbox` *WHAT IS NOT JOURNALLED* for refused rails; spawned closures and
trait-object calls are blind spots of the source scan.

### Effects, Apply, and coding reattach

`Apply` = `gate_apply` → intents → `dispatch_apply` → outcomes.
`phases::apply::dispatch` requires an `ApplyIntents` receipt only a driver can
mint; `driver_worker::record_batch_intents` / `record_batch_outcomes` write the
effect ledger around it. `resolve_effects` covers the **union** of the committed
batch (for hosts without the split seam; `splits_apply_gate_from_dispatch`
defaults `false`) and `EffectLedger::unsettled` (for a worker that died).

`phases::apply::declared_retry_safety` is the single retry-safety site, per
**action**. Not derived from `is_parallelizable_read_only_action` ("may run
beside another" ≠ "may run twice") nor the per-pack `reliability` block.
`RetrySafety::Reattachable` applies to `run_coding_task`; its `reattach_ref` is
the coding **invocation id** from `prepare_coding_invocation` (durable before the
job starts), not the `native_session_id`. `WorkerHost::reattach_state` resolves it.

`apply::dispatch` takes an `effects::EffectPlan` (a value, not a capability;
verdicts index-parallel to gate admission; naming an unadmitted effect is a
refusal). `apply::plan_the_batch` matches `EffectAction` exhaustively, since a `_`
arm would re-fire a live effect:

| action | dispatch | settled |
|---|---|---|
| `Refire` | fires (licence: retry-safety declaration or positive evidence, plus a reproduced fingerprint) | whatever dispatch reports |
| `Adopt` | does not fire | nothing — row already has the outcome |
| `AlreadyFired` | does not fire | `Succeeded` |
| `Reattach` | stamps `__coding_invocation_id` + `__coding_resume_session_id`, resumes the native session | whatever the resumed dispatch reports |

`run_coding_task` validates engine, session, scope, project root, root task, and
generation before binding the resume slot; settled invocations are never resumed;
a row with no durable result body needs an operator Adopt/Refire.

Engines report their native session as soon as known
(`ledger::attach_live_invocation_session`):

| engine | reported at | uncovered window |
|---|---|---|
| Codex | `thread/start` returns | spawn → handshake |
| Grok | `session/new`/`session/load` returns | spawn → handshake |
| Pi | `prompt` RPC ack | spawn → prompt ack |
| Claude | `system/init` frame | spawn → first frame |
| Agy | `init` frame; factory Unconstructable | (not live) |

`mark_invocation_may_have_started` runs just before the engine is touched; Codex
also records `accept_invocation_turn` from `turn/start`, so
`CodingDispatchState::automatic_retry_allowed` distinguishes *never started* from
*started, unknown*. Decide has no in-flight marker (provider exposes no job id).

Cold Apply journals a rewind to Resolve, restores the pre-Resolve capsule,
re-runs Resolve under current authority, then builds the plan.
`PhaseReport.pending` is the complete-batch activation marker published before
dispatch; failures preserve the successful prefix without advancing the cursor.

### Rules

- Guard a value where it is interpreted, not where it is produced
  (`reconcile_outward_effect` reads "no record" as licence to re-send, so the
  check lives there, not in `act_path`).
- `OutwardActStatus::Failed` has two writers (`refuse_outward` before dispatch;
  receipt sweep after a hard bounce); `dispatched_at` discriminates.
- `serde` yields `None` for a missing `Option<T>` regardless of
  `#[serde(default)]`; a wire-required `Option` needs `deserialize_with`.
- A pin needs a bound renewed by proof of liveness; otherwise a scan offers a
  live run's key between phases and its next claim gets `Advanced::LeaseHeld`.
- `Placement::Portable` was drawn around the wrong resource (a local Chrome
  between phases vs. a chat thread's browser session between executions);
  redrawing it is new design
  (open-questions §2).
- A defaulted value is not neutral when absence is read as evidence: folding a
  forked `LoopProtectiveState` reads `loop_recovery_context: None` as "cleared",
  so the caller filters defaulted members (pinned by
  `a_defaulted_member_erases_the_base_which_is_why_the_caller_filters`).
- A forked detector cannot be merged exactly (each member is `base + at most one
  record_action_result`); the fold errs toward detecting a cycle later, never
  earlier.
- Count emitters transitively over every vocabulary, not at depth 1.
- `TerminalKind::is_resumable()` is not "can continue": `BudgetExhausted` and
  `MaxIterationsReached` carry a live "Continue or cancel?" pause but map to
  `false`.
- A transition spanning two commits (`leave_park`) needs guards at both ends.
- One execution id can address several runs; enumerate every re-entry path on an
  inherited context before choosing a discriminator.
- One switch over grounds of unequal soundness is a defect; opt in per ground.

## Ungranted-tool dispatch

`classify_flat_route` uses the **per-agent scoped** `CapabilityRegistry`, so a
miss means "not granted". The call falls to the Compiled/`Unknown` arm and fails
with a grant-framed error (`flat_loop/dispatch.rs`,
`ungranted_capability_error`) naming tool and agent: delegate to an agent that
has it or finish with available tools — do **not** retry.

Snapshot absence ≠ empty snapshot. A detached harness or startup-registry
fallback without a scoped revision keeps its owner/legacy catalog and must not
install the shared surface cache or an empty ceiling. A real effective-policy
snapshot, even tool-free, is authoritative and fail-closed.

Trust is checked before approval and catalog-miss recovery: a trust-denied action
cannot be made legal by confirmation, and a provider call to one fails the run
with the canonical trust-denial reason. Non-denied calls outside the active
catalog use the corrective path above.

## Subprocess PATH

Subprocesses running scope tools must prepend the scope's tool-bin dirs via the
single source of truth
`CapabilityScopePaths::subprocess_bin_path(extra_bin, parent_path)` (prepends
existing `[extra_bin?, venv_bin, node_modules_bin, node_bin]`). Callers: CLI
dispatcher (`cli_template/dispatcher.rs`, per-skill `bin/`), preflight probe
(`agentic/preflight.rs::run_probe`), pack auth
(`compiled_providers.rs::run_auth_command`), bot GWS auth/daemon
(`bots/mod.rs`, magician-learning), `meetings_api.rs`, `skills_api.rs`,
`pack_provider.rs::execute_command`, `skills/runner.rs::run_ephemeral`. Fail-safe:
no scope paths or no existing bins leaves PATH untouched. New spawn sites reuse it.

## Keep-tail context window

The decide() snapshot is keep-tail only: last 25 assistant/user pairs
(`OUTER_VERBATIM_MAX_ITERATIONS`), then oldest-first eviction to
`OUTER_VERBATIM_TOKEN_BUDGET` = 40,000 tokens (estimate: ceil(UTF-8 bytes / 2)).
The newest pair is never dropped. `history.live_messages` is append-only and
round-trips resume unchanged; `last_response_id` is cleared only when the sent
prefix changed. There is no second model; the unused
`op-agentic-ledger-compaction-local` profile must not be rebound to decide().
Pi's own compaction and analytics parquet compaction are unrelated.

### LLM-led ledger compaction (available, default off)

Removed; do not revive on the active-task path. Historical
design.

## In-turn parallel reads

Consecutive leading read-only follow-ups (file reads and named packs; not HTTP
GET or DuckDB) may `join_all` in one turn; histories merge in provider order.
Pathless named packs (`grep`, `glob`, `web_search`, …) still join. Absolute paths
outside sandbox allowed roots (plus session overrides), `..` escapes, and
unauthorized tools stay **serial** so two sandbox/tool-auth parks cannot share a
pause key. If the first candidate fails or any sibling parks, cancels, or fails,
the mutating tail does **not** start. Writes, shell, and browser stay serial
`--bail`.

## Web answer lane

- `web_search` — free DuckDuckGo HTML result list; parallel-read candidate.
- `web_answer` — one function-tool-free LLM request via `op-web-answer`, whose
  `server_web_search` metadata makes the transport search server-side and return
  citations. Billed per search on top of tokens. **Serial** (a provider call).
- research agent — multi-source investigation and synthesis.

Invariants (magicllm `server_web_search`): never mixed with function tools on one
request; unsupported transports fail closed. `web_answer` is enabled by the
`web_answer: op-web-answer` mapping; without it (or without the metadata) the tool
fails closed rather than answering ungrounded. The pack declares a counted spend
gate (~$0.05/call), and per-call `search_calls` ride `LLMResponseReceived` so
settlement, telemetry, and lakehouse recompute include search charges.

