# LLM Dispatch Queue

**Owner crate:** `magicllm::dispatch` (glue in `magician::magician_v2::dispatch_glue`)
**Design doc (archived):** `docs/archive/plans/2026-05-26-llm-dispatch-queue-and-local-prep.md`

## What it is

A process-wide priority queue and worker pool between every LLM caller and
`MultiLLMRouter`. It replaces ad-hoc local semaphores with one chokepoint that
adds:

- **Priority lanes**: High / Normal / Background, biased drain, with agent/task
  owner round-robin inside each lane.
- **Pre-dispatch cancellation gate**: workers check task state before calling
  the provider.
- **In-flight cancellation race**: `tokio::select!` between the provider call
  and a task-cancel token. `cancel_task` / `fire_cancel` match `task_id`,
  `execution_id` and `root_execution_id`; agentic jobs set `task_id` to the
  persisted task and `execution_id` to the runtime execution, so
  `cancel_execution` still aborts them. Tokens form a tree
  (`task_state_view.rs::CancelTokenTree`: task ⊃ root execution ⊃ execution,
  each a `child_token()` of its parent), so cancelling a task or root fires
  every descendant, and cancelling a child never touches its parent. Never
  register an execution's own token under a shared id (the root id, or a
  `task_id` parent and child share), or failing a child cancels the parent's
  jobs.
- **Idempotency**: caller-supplied keys dedupe concurrent and recent duplicates.
- **Submission deadlines**: an optional `Instant`, tombstoned at pickup.
- **Per-provider concurrency caps, 429 cool-down and circuit breaker.**
- **Provider-aware admission**: saturated work parks in one bounded,
  priority-aware scheduler per provider, never inside the global worker pool,
  and keeps its lane reservation while parked, so waiters and retry sleepers
  cannot escape queue capacity.
- **Reserved interactive workers**: a configurable subset never drains the
  background lane.
- **Retry policy**: hard ceiling `max_attempts_per_dispatch ×
  max_dispatch_cycles`, jittered exponential backoff. The in-crate default is
  3 × 2 = 6 (`RetryConfig::default`); the git seed sets 2 × 1 = 2.
- **Producer routing snapshots**: a snapshot-sensitive job may bind one
  immutable configured-router generation, kept by provider admission, watchdog
  lookup, routing and every retry across config reloads. Unbound jobs use the
  ordinary queue router.
- **Worker watchdog**: drops futures stuck past `2× profile.timeout_secs`.
- **Graceful shutdown**: drains pending work, waits up to T seconds for
  in-flight calls, force-cancels the rest.
- **Local-prep hook**: direct Ollama summarisation of large content blocks.
  Ollama-bound prep parks on a cap-1 coordinator without holding a dispatch
  worker. `llm.dispatch.local_prep.operation` resolves through the normal
  operation mapping, so model, endpoint and context stay profile-owned while
  the direct call avoids queue recursion.
- **Queue viewer**: `GET /api/llm/queue/snapshot` and a workbench panel.
- **Realtime event bus**: `LlmQueueEvent` broadcast.

Worker, lane-entry and retained-byte numbers are a named
`magicllm::dispatch::DispatchCapacityPlan`. Magician's pre-bootstrap resolver
(`runtime.scale`) inherits the selected profile wherever
`runtime.scale.overrides.*` is `null`. The git seed is `profile: current`,
matching `DispatchCapacityPlan::CURRENT` (12 workers, 4 reserved interactive,
400 normal-lane entries). `MAGICIAN_SCALE_PROFILE` overrides the profile at
boot. A leftover `llm.dispatch.workers: 3` or `12` with an explicit
non-`current` profile is a boot error.

**Engines.** With `engine: provider_isolated` (the YAML seed), after local prep
and once it owns the provider permit, the scheduler worker hands HTTP to a
per-attempt executor and returns, so no global worker slot is held across provider HTTP.
`legacy_worker_pool` is the in-crate default and the restart-bound rollback; it
runs HTTP inside `process_job`. `global_cloud_concurrency` (default 16, `0`
disables) caps simultaneous non-Ollama HTTP, sync and streaming alike, after the
provider permit and RPM/TPM and before `route()` / `route_stream()`; Ollama
skips it, a 429 may shrink it, and cancel/shutdown drop the permit. An empty
`{}` provider quota is unlimited.

## Configuration

The seed `magician-config.yaml` carries (abridged):

```yaml
runtime:
  scale:
    profile: current # current | small_cpu | m2_max | cloud_heavy | auto
    overrides:
      workers: null              # null inherits the selected profile
      reserved_interactive_workers: null
      queue_capacity_high: null
      queue_capacity_normal: null
      queue_capacity_background: null
llm:
  dispatch:
    enabled: true
    engine: provider_isolated
    workers: 12
    reserved_interactive_workers: 4
    global_cloud_concurrency: 16      # not in the seed; code default. 0 disables
    queue_capacity_high: 200
    queue_capacity_normal: 400
    queue_capacity_background: 256
    max_request_bytes: 67108864       # 64 MiB per request
    queue_bytes_high: 268435456        # 256 MiB retained high-priority work
    queue_bytes_normal: 536870912      # 512 MiB retained normal work
    queue_bytes_background: 536870912  # 512 MiB retained background work
    queue_bytes_global: 805306368      # 768 MiB across all pending/waiting work
    queue_depth_warn: 24
    watchdog_factor: 2.0
    idempotency_window_secs: 60
    max_recorded_errors: 6
    provider_concurrency:
      default: 3
      overrides:
        anthropic: 4
        openai: 4
        openrouter: 2
        ollama: 1
    # Missing provider or 0 = unlimited. Burst equals the per-minute rate.
    provider_quota: {}
    retry:
      max_attempts_per_dispatch: 2
      max_dispatch_cycles: 1
      backoff_base_ms: 500
      backoff_cap_ms: 30000
      backoff_jitter_pct: 0.25
      requeue_backoff_base_ms: 5000
      requeue_backoff_cap_ms: 60000
    breaker:
      threshold: 3
      window_secs: 60
      cooldown_secs: 60
      max_cooldown_secs: 900
    local_prep:
      enabled: false
      yield_worker: true
      operation: local_prep
      threshold_chars: 8000
      max_chars_out: 1500
      timeout_secs: 300
      purpose_prompts: {}
    shutdown_timeout_secs: 30
    completed_ring_capacity: 500
    tombstone_ring_capacity: 500
  router:
    operation_mapping:
      local_prep: op-ambient-distill-local
```

**Byte admission.** Entry counts and retained-byte ceilings are independent
limits. A request stays charged while it sits in a lane, waits on
cooldown/backoff, runs local prep or parks for provider capacity, and is released
only once it holds the provider permit right before dispatch. Snapshots show
current and configured retained bytes per lane and globally. Oversized requests
and exhausted budgets return `RequestTooLarge` and `QueueBytesFull`; retries and
fallbacks inside an active physical job are not charged twice. After an attempt
releases its charge, every in-place retry, cycle requeue and rate-limit cooldown
re-enters one fair byte-admission coordinator that waits without holding a
worker, never turns an admitted retry into `QueueBytesFull`, and holds the
acquired charge through any remaining backoff. Saturation in
one lane cannot block another lane's retry; cancellation, deadline or shutdown
drops the reservation exactly once. A request made impossible by a hot-reloaded
smaller ceiling fails explicitly.

The shipped Ollama generation profiles use a 300-second request timeout
(including the Kapso fallback and direct local prep); with
`watchdog_factor: 2.0` a provider future is treated as stuck after 600 seconds.

**Ollama thinking is always disabled for structured/utility calls.** Reasoning
models default to thinking on, which on JSON extraction moves the answer into a
`thinking` field or degenerates into repetition. `magicllm`'s `OllamaProvider`
sends an explicit `think` from the request's reasoning intent (`think: false`
for non-reasoning ops).

**Single chokepoint.** Every Ollama inference goes through `OllamaProvider`, so
token usage, keep-alive, `think` and `format` are shaped in one place.
`dispatch/local_prep.rs` calls `OllamaProvider::invoke` directly (re-queuing
would recurse). The one other exception is the `prewarm` model-load ping
(`num_predict:1`, non-inference, `think: false`). `make check-ollama-chokepoint`
(`scripts/check_ollama_single_chokepoint.py`) fails on any
`/api/generate`/`/api/chat` POST outside that allowlist. See
`docs/archive/plans/2026-07-14-ollama-single-chokepoint.md`.

## Caller usage

```rust
use magicllm::{LlmDispatchQueue, LlmJob, JobOrigin, Priority, TaskRef};

let (job, rx) = LlmJob::new(request, JobOrigin::op("chat_turn"));
let job = job
    .with_priority(Priority::High)
    .with_task(TaskRef::task("task-abc"));
queue.submit(job).await?;
let response = rx.await??;
```

Callers without task scope:

```rust
let response = queue
    .submit_and_wait(request, JobOrigin::op("system_probe"))
    .await?;
```

## Large-payload ownership

`LLMRequest` keeps message history, inline media, tool schemas and
summarisable blocks in immutable `Arc`-backed lanes. Admission moves the owned
envelope; retry, router fallback and streaming handoff clone only the `Arc`
handles. Local prep is the only stage that rewrites message content, using
copy-on-write before the first provider attempt. Provider continuation ids,
turn state, request bodies, attempt accounting and retry selection are
unchanged.

Successful non-idempotent calls move the response straight to the caller with
no cache entry. Idempotent owners and subscribers share `Arc`-backed response
lanes, while each envelope keeps its own `LlmTraceReceipt` (including
`response_reused`). `DispatchedResponse::into_response()` gives an owned
`LLMResponse` by cloning only the small envelope; explicit mutation uses
copy-on-write accessors. The recent idempotency window is bounded by 1,000
entries and 64 MiB of estimated payload; eviction never removes an in-flight
subscriber. Terminal publication must match the registered `JobId`, so an old
key generation cannot remove, notify or overwrite a newer owner. A submission
dropped before lane publication wakes its subscribers without caching the
synthetic cancellation and releases the key for a clean retry.

## Operation Priorities

`OperationLlmRouter` sets priority from `LLMOperation` before submitting:

- `High`: `VoiceController`, `VoiceContextCompaction` and `MeetingResponse`.
- `Normal`: user-facing chat, planning, slot graph, agentic decisions, and
  explicit report synthesis such as `evidence_review`.
- `Background`: memory consolidation and best-effort distillation/eval work,
  including entity extraction, environment knowledge extraction, insight
  distillation, user promotion, archive summaries, episode quality
  classification, conflict review, workflow compilation, screen observation,
  meeting summaries, `distill_evidence`, `ambient_distill`,
  `screen_evidence_distill`, `tier_evidence_distill`, `evidence_claims`,
  `evidence_precision_judge`, `evidence_review_verify`,
  `memory_temperature_utility_review`, `channel_ingest_distill`,
  `channel_classify`, `resurfacing_curate`, and `learning_reflection`.

Memory work therefore does not win pickup over interactive work when provider
concurrency is tight; a call already in flight is never preempted.
Terminal-grounding and generated task-user publication reviews use a call-scoped
Normal override because the user is waiting; offline calls under
`evidence_precision_judge` stay Background.

## Starvation-free background admission

Background loops (distill, classify, ambient distill, comms backfill) submit
bounded work to the background lane. Write-producing memory maintenance goes
further: utility review persists bounded work in a scoped disk queue before
dispatch, and batch consolidation persists the exact source fingerprint in its
retry guard. These drainers yield before admission while foreground work is
live, the background lane is full, or the same provider is already serving or
admitting background work. A busy period moves the durable due time; it is not a model
failure and discards no source write.

The utility drainer claims one item at a time with expiring leases, handles at
most two items per minute globally by default, rotates scope order, and commits
or fails each item on its own. Retryable provider and queue failures back off
with a cap, indefinitely. Deterministic output/config failures dead-letter only
that item after eight attempts; changed source evidence or a versioned
reviewer-contract upgrade releases it. A bounded applied-run ledger makes the
temperature-overlay mutation idempotent when lease recovery replays an item. The
generic dispatcher stays in memory, so arbitrary prompts and credentials are
never persisted as a replay body.

Pickup is foreground-biased but bounded: a worker serves Normal work after at
most eight consecutive High pickups, and waiting Background work after at most
twelve consecutive foreground pickups. Within a lane, pickup is owner
round-robin: the next job is the oldest of the next owner (`task_ref.agent_id`
when non-empty, else `task_ref.task_id`, else `job_id`), FIFO within one owner.
Anonymous jobs use unique `job_id` keys and do not clump.

`reserved_interactive_workers` limits workers, not providers. With the shipped
`workers: 12` / `reserved_interactive_workers: 4`, eight workers may start
Background calls and all twelve may serve High/Normal; at least one worker is
always background-capable.

- When a provider semaphore is full, the job enters `waiting_for_provider` and
  the provider's single scheduler waits for capacity; no worker counts as busy.
  Priority and bounded fairness hold inside the scheduler.
- The scheduler hands a provider-bound permit to the selected High/Normal job,
  so a Background job cannot reverse its decision during handoff. Background
  handoffs release the probe permit first, so a Background job waiting for the
  only background-capable worker never holds scarce provider capacity.
- A route change releases a permit bound to the old provider before acquiring
  capacity on the new one. None of this alters provider request bodies,
  caching, continuation ids or turn semantics.

Ollama-bound local prep (`llm.dispatch.local_prep.enabled`, `yield_worker`, and
a summarisable block at or above `threshold_chars`) parks the job on a cap-1
`LocalPrepCoordinator` (High > Normal > Background) without a dispatch worker.
Cheap inlining (disabled, disclosure-bound or below threshold) stays on the
worker. After prep the job is re-enqueued with `local_prep_done`. Snapshots and
`LlmQueueEvent` expose `waiting_for_local_prep`.
`llm.dispatch.local_prep.yield_worker: false` restores in-worker head-of-line
blocking. Rollback for the engine: `llm.dispatch.engine: legacy_worker_pool`
(restart-bound).

## Error model

| `LLMError` variant | Caller action |
|---|---|
| `Cancelled { reason }` | Operational. Log info; don't escalate. |
| `AllRetriesExhausted { attempts, last_error }` | Hard failure. Surface to user; don't wrap in another retry loop. |
| `QueueFull { priority, depth, capacity }` | Backpressure. Caller-side retry, escalate priority, or drop. |
| `ProviderUnavailable` | Circuit breaker open. A router fallback profile may still apply; otherwise a hard failure. |
| `WorkerWatchdog` | Worker dropped a stuck future. Treated as `Timeout` for retry; surfaces only if all retries are exhausted. |
| `DeadlineExceeded` | Submission deadline elapsed before pickup. |
| Provider/Validation/Configuration | Existing semantics, propagated through. |

## Cancellation

Three independent paths tombstone jobs:

1. **Pre-dispatch gate**: the worker reads `TaskStateView::snapshot` right
   before the provider call; cancelled or missing tasks tombstone.
2. **In-flight race**: `tokio::select!` between the provider future and a token
   from `TaskStateView::subscribe_cancel`, fired by
   `task_state_view.fire_cancel(task_id)`.
3. **External API**: `queue.cancel_task(id, reason)` /
   `cancel_chat_session(id, reason)` / `cancel_job(id, reason)` record intent;
   the worker plugs the race at pickup. The provider scheduler is notified at
   once and also polls cancellation and deadlines at a short bounded cadence,
   without a task per waiter.

Wire orchestrator transitions to both `TaskStateView::fire_cancel` and
`queue.cancel_task(...)` so cancellation reaches queued and in-flight jobs
instantly.

Auxiliary callers can opt into `submit_and_wait_cancellable(job, receiver)`:
dropping that wait cancels a private child of the owner's token, across queue,
provider waits and retries, without cancelling siblings. Native delivery repairs
and LLM claim reviews use this path, including their logical chunks. Protected
attempts still drain to their existing settlement boundary before cancellation.

Streaming and terminal-path invariants:

- Streams use the same per-lane admission permits as sync jobs, and their
  provider wait races cancellation, deadline and both shutdown modes. A stream
  first scheduled after graceful shutdown begins is tombstoned before provider
  resolution.
- Forwarding to a full receiver races force shutdown and cancellation; after the
  tombstone the terminal error delta is best-effort, so an abandoned consumer
  cannot hold capacity.
- A delivered provider `Error` owns the terminal failure (one scheduler turn for
  the adapter's richer error, then abort); a delivered `Done` commits success.
  A racing cancel/shutdown rewrites neither.
- Health, breaker and 429 cooldown (Retry-After or bounded fallback) are charged
  once, to the effective fallback provider.
- Terminal helpers release byte reservations and lane/provider admission,
  publish registry/event state, release idempotency ownership and deliver the
  caller result *before* awaiting a possibly slow durable ledger sink, also when
  admission rejects a job before lane publication.
- Streaming ledger `wait_ms` is lane pickup time; provider contention stays in
  `provider_wait_ms`.
- Sync, sync-to-stream fallback and native-stream `Done` responses pass
  aggregate response admission before observation or retention: 64 MiB per
  arbitrary JSON lane (compact wire), 192 MiB across lanes with bounded depth and
  node count, and a separate 192 MiB cap on the whole normalized response
  (rechecked after the trace receipt is attached). Rejected JSON is dismantled
  iteratively, and an invalid response cannot pin a hung adapter.

## Result posting

Every job's terminal outcome flows through three channels:

1. **`oneshot::Sender`** on the `LlmJob`, for the submitter's inline await.
2. **Task ledger**: `LlmCallLedgerEvent`
   (Submitted/AttemptStart/AttemptDone/Requeued/Completed/Failed/Tombstoned)
   appended to `<data_root>/llm_dispatch/<task_id>.jsonl`. Durable; read by the
   executor projection on restart.
3. **Realtime event bus**: `LlmQueueEvent` broadcast, for the viewer and any
   host realtime fanout.

Startup orphan recovery scans the historical ledger in bounded background
maintenance after the queue starts, never before the Actix HTTP server binds; a
slow ledger read only delays synthetic tombstones for the previous process's
jobs. Immediate shutdown also bounds tombstone persistence for drained lanes,
provider waiters, delayed requeues and streams, so a blocked ledger sink cannot
keep shutdown alive.

## Observability

`GET /api/llm/queue/snapshot` returns the `QueueSnapshot` JSON: workers, total
admitted depth per lane (including work parked for provider capacity, local
prep or retry delay), provider-wait counts by lane, `waiting_for_local_prep`
(serde default 0), and the last N completed/failed/tombstoned jobs. Each job
reports lane `wait_ms`, provider-permit `provider_wait_ms` and provider
`execution_ms` separately, so local-model semaphore saturation is not mistaken
for slow inference. The `/llm/queue` table shows all three, and terminal
dispatch Parquet rows keep the provider-wait field.

Each job carries the runtime-activity row it was submitted from as
`JobOrigin.activity_id`, stored in the `activity_id` column of terminal dispatch
Parquet rows. It joins the live view to this store: take `activity_id` from an
`ActivityStarted` event (websocket or the scope's `events.jsonl`) and filter
`analytics/llm_dispatch/dt=*` on it, or go the other way from a slow call. It is
a decimal correlation counter only; no credential, prompt or user content rides
on it. `magicllm` never mints it; the submitting call stamps it from its tracing
context, and a call outside any instrumented span records `NULL`.

The memory utility runner logs active, due, retrying, dead-letter, deferred and
oldest-pending health each pass, from the leased disk state. The same state is
returned by `GET /api/magician/v2/memory/temperature/status` and shown on
`/memory`.

Realtime: `queue.subscribe_events()` streams `LlmQueueEvent`.
`WaitingForProvider` and `WaitingForLocalPrep` are distinct from `Requeued`.
`Dispatched` is emitted once per logical job, after the provider permit is held
and right before the first provider attempt; per-attempt detail is in the
`AttemptStart`/`AttemptDone` ledger events. Streaming jobs follow the same
lifecycle: the first provider-capacity miss emits one `WaitingForProvider`, and
the terminal event keeps that lane `wait_ms`.

UI: `LlmQueuePanel.svelte` under `unified-ui/src/lib/magician/components/`,
mounted at `/llm/queue` (command palette **Jump to → LLM dispatch queue**, kbd
`G Q`, and the **Manage** section).

## Realtime event fanout

`LlmQueueEvent` has its own bus (`queue.subscribe_events()`), independent of
`RuntimeAgentEventType`; the dev-workbench viewer subscribes directly. To put the
events on the `RuntimeTransportEvent` channel (for example, to reach client SSE),
wire `spawn_realtime_fanout(queue, forward)`:

```rust
use magician::magician_v2::dispatch_glue::spawn_realtime_fanout;

let queue = ...; // from start_dispatch_queue
let broadcaster = event_broadcaster.clone();
let _handle = spawn_realtime_fanout(Arc::clone(&queue), move |event| {
    // Map LlmQueueEvent -> RuntimeTransportEvent and emit on broadcaster.
    // See magician_v2::dispatch_glue::realtime_fanout for the integration point.
});
```

There are no `llm.queue.*` entries in `RuntimeAgentEventType`; adding them needs
matching variants in `magician-event-taxonomy` and
`magician_v2::realtime_events` plus `make event-taxonomy-codegen`. Local prep
has no automatic marker detection: callers list blocks explicitly in
`LLMRequest.summarisable_blocks` (`SummarisableBlock`, `dispatch/local_prep.rs`).
Terminal jobs are persisted as Parquet rows by
`magician_v2::analytics::llm_dispatch_rows`, and in-process gauges live in
`magicllm/src/dispatch/metrics.rs`.

## Boot integration

```rust
use magician::magician_v2::dispatch_glue::start_dispatch_queue;

let (queue, task_state_view) = start_dispatch_queue(
    Arc::clone(&router),
    Arc::clone(&artifact_service),
    data_root.clone(),
    dispatch_config,
).await;

// On orchestrator task cancellation:
task_state_view.fire_cancel(&task_id);
queue.cancel_task(&task_id, "user_cancelled");

// On process shutdown:
queue.shutdown(Duration::from_secs(30)).await;
```

`OperationLlmRouter::set_dispatch_queue` and
`MultiLLMService::set_dispatch_queue` install the queue at boot. The queue
routes through `OperationLlmRouter::live_dispatch_router`, which looks up the
current `ConfiguredRouter` on every call, so a config reload
(`/settings/magician-config/reload`, or any `reload_from_config`) applies without
a restart. This matters because when a picked profile is an operation's default,
Magician sends no override and the queue's router re-resolves the mapping, so a
stale router would dispatch the old profile.
`priority_for_operation` routes `OperationLlmRouter` submissions through the
queue, and chat streaming (`generate_chat_completion_streaming*`) submits through
`submit_stream` on the High lane when `llm.dispatch.enabled`. Without an
installed queue, direct `route` / `route_stream` remain. Callers with a task
scope use `LlmJob::new(request, origin).with_task(...)` so the cancellation
gates engage.
