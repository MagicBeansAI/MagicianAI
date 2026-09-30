# LLM Dispatch Queue

**Owner crate:** `magicllm::dispatch` (with glue in `magician::magician_v2::dispatch_glue`)
**Design doc (archived):** `docs/archive/plans/2026-05-26-llm-dispatch-queue-and-local-prep.md`

## What it is

A process-wide priority queue + worker pool that sits between every LLM caller and `MultiLLMRouter`. Replaces ad-hoc local semaphores with a single chokepoint that adds:

- **Priority lanes** — High / Normal / Background, biased drain, with
  agent/task owner round-robin inside each lane.
- **Pre-dispatch cancellation gate** — workers consult task state before invoking the provider.
- **In-flight cancellation race** — `tokio::select!` between provider call and a task-cancel token.
  `cancel_task` / `fire_cancel` match `task_id`, `execution_id`, and
  `root_execution_id`. Agentic jobs set `task_id` to the persisted artifact
  task and `execution_id` to the runtime execution; `cancel_execution` fires
  the execution id and must still abort those jobs. The tokens form a tree
  (`task_state_view.rs::CancelTokenTree`): task ⊃ root execution ⊃ execution,
  each level a `child_token()` of the one above, so cancelling a task fires
  every job under it, cancelling a root fires every descendant's, and
  cancelling a child never touches its parent. An execution's own token must
  never be registered under a shared id (the root id, or the persisted
  `task_id` parent and child share), or failing a child cancels the parent's
  jobs.
- **Idempotency** — caller-supplied keys dedupe concurrent + recent duplicates.
- **Submission deadlines** — optional `Instant` deadline tombstoned at pickup.
- **Per-provider concurrency caps + 429 cool-down + circuit breaker.**
- **Provider-aware admission** — saturated work parks in one bounded,
  priority-aware scheduler per provider, never inside the global worker pool.
  The job retains its lane admission reservation while parked, so provider
  waiters and retry sleepers cannot escape configured queue capacity.
- **Reserved interactive workers** — a configurable subset never drains the
  background lane, so one long local maintenance call cannot consume every
  route available to user-facing work.
- **Retry policy** — 3 in-place attempts × 2 dispatch cycles = 6 hard ceiling, with jittered exponential backoff.
- **Producer routing snapshots** — snapshot-sensitive jobs may bind one
  immutable configured-router generation; provider admission, watchdog lookup,
  physical routing, and every retry retain that authority across config reload.
  Jobs without a binding keep the ordinary queue-router behavior.
- **Worker watchdog** — drops futures stuck past `2× profile.timeout_secs`.
- **Graceful shutdown** — drains pending, waits for in-flight up to T seconds, force-cancels remainder.
- **Local-prep hook** — direct Ollama summarisation of large content blocks.
  Ollama-bound prep parks on a cap-1 coordinator and does not occupy a
  dispatch worker; cheap inlining stays on the worker.
  `llm.dispatch.local_prep.operation` resolves through the normal operation
  mapping, so the model, endpoint, and context stay profile-owned while the
  direct call avoids queue recursion.
- **Queue viewer** — `GET /api/llm/queue/snapshot` + workbench panel.
- **Realtime event bus** — `LlmQueueEvent` broadcast for live observability.

Worker, lane-entry, and retained-byte numbers are a named
`magicllm::dispatch::DispatchCapacityPlan`. Magician's pre-bootstrap
resolver (`runtime.scale`) inherits the selected profile when
`runtime.scale.overrides.*` is `null`. The git seed is `profile: current`
and matches `DispatchCapacityPlan::CURRENT` (12 workers, 4 reserved
interactive, 400 normal-lane entries). `MAGICIAN_SCALE_PROFILE` overrides
the profile name at boot. Leftover `llm.dispatch.workers: 3` or `12` plus
an explicit non-`current` profile is a boot error. With
`engine: provider_isolated` (YAML seed), after local-prep and owning the
per-provider permit the scheduler worker hands HTTP to a per-attempt executor
and returns, so the global worker slot is not held across provider HTTP.
`legacy_worker_pool` is the in-crate default and the restart-bound rollback; it
runs HTTP inside `process_job`. `global_cloud_concurrency` (default 16, `0` disables)
caps simultaneous non-Ollama HTTP after the provider permit and RPM/TPM and
before `route()` / `route_stream()`. Sync workers and streaming jobs share
the same cap; Ollama skips it. A 429 on either path may shrink the
effective cap. Cancel/shutdown Drop the cloud permit. Empty `{}` is unlimited.

## Configuration

Add to `magician-config.yaml`:

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
    global_cloud_concurrency: 16      # 0 disables; non-Ollama HTTP (sync + stream)
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

Entry counts and retained-byte ceilings are independent admission boundaries.
A pending request that owns byte admission remains charged while it is in a
lane, delayed by cooldown/backoff, running local preparation, or parked for
provider capacity. Its charge is released only after it owns the matching
provider permit immediately before active dispatch. Queue snapshots expose
current and configured retained bytes per lane and globally. Oversized
requests and exhausted byte budgets return the typed `RequestTooLarge` and
`QueueBytesFull` errors; provider retries/fallbacks inside an active physical
job are not double-charged. After any physical
attempt releases its active charge, every in-place retry, cycle requeue, and
rate-limit cooldown enters one fair byte-admission coordinator. It waits for
transient retained-byte contention without occupying a dispatch worker or
turning an already-admitted retry into `QueueBytesFull`, then holds the acquired
charge through any remaining backoff. Lane-local saturation cannot block an
admissible retry in another lane; cancellation, deadline, or shutdown removes
the parked job and drops any reservation exactly once. A request made
permanently impossible by a hot-reloaded smaller ceiling fails explicitly
instead of waiting forever.

The shipped Ollama generation profiles use a 300-second provider request
timeout, including the Kapso fallback and the direct local-prep path. With the
default `watchdog_factor: 2.0`, queued dispatch workers allow up to 600 seconds
before treating an in-flight provider future as stuck.

**Ollama thinking is always disabled for these structured/utility calls.** Ollama
reasoning models default to thinking-ON, which on a JSON extraction task diverts
the answer into a separate `thinking` field or degenerates into repetition.
`magicllm`'s `OllamaProvider` sends an explicit `think` derived from the
request's reasoning intent (`think: false` for non-reasoning ops).

**Single chokepoint:** every Ollama *inference* goes through `OllamaProvider`,
so token usage, keep-alive, `think` and `format` are shaped in one place.
`dispatch/local_prep.rs` calls `OllamaProvider::invoke` **directly** (re-queuing
would recurse). The one sanctioned exception is the `prewarm` model-load ping
(`num_predict:1`, non-inference), direct with `think: false`. The drift gate
`make check-ollama-chokepoint` (`scripts/check_ollama_single_chokepoint.py`)
fails on any `/api/generate`/`/api/chat` POST outside that allowlist. See
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

Convenience for callers without task scope:

```rust
let response = queue
    .submit_and_wait(request, JobOrigin::op("system_probe"))
    .await?;
```

## Large-payload ownership

`LLMRequest` keeps message history, inline media, tool schemas, and
summarisable blocks in immutable `Arc`-backed lanes. Queue admission moves the
owned envelope. Worker retry, router fallback, and streaming handoff clone only
those Arc handles; provider-specific scalar defaults remain per-attempt.
Local-prep is the only dispatch stage that rewrites message content and uses
copy-on-write before the first provider attempt. This is transport-neutral:
OpenAI continuation ids, Anthropic/Gemini turn state, provider request bodies,
attempt accounting, and retry selection retain their existing semantics.

Successful non-idempotent calls move the normalized provider response directly
to the caller and allocate no recent-result cache entry. Idempotent owners and
subscribers share Arc-backed text, reasoning, message, tool-call, tool-result,
and raw-JSON lanes while each dispatch envelope retains its own
`LlmTraceReceipt` (including `response_reused`). Callers that
need the traditional owned `LLMResponse` use
`DispatchedResponse::into_response()`; that boundary stamps the consumer's
receipt by cloning only the small envelope, without copying any shared provider
lane. Explicit response mutation uses copy-on-write accessors. The recent
idempotency window is
bounded both by 1,000 entries and by 64 MiB of estimated retained payload. Byte
eviction never removes an active in-flight subscriber or cancels its delivery.
Terminal publication is generation-aware: a delayed/repeated completion must
match the registered `JobId`, so an old key generation cannot remove, notify,
or overwrite a newer owner after cache eviction. Registration is also
cancellation-safe before lane publication: if submission is dropped while its
durable ledger intent is pending, the exact owner generation wakes subscribers
without caching the synthetic cancellation and immediately releases the key for
a clean retry.

## Operation Priorities

`OperationLlmRouter` assigns dispatch priority from `LLMOperation` before
submitting to the queue:

- `High`: realtime voice session/control and voice context compaction.
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

This keeps memory consolidation/distillation from winning queue pickup against
interactive work when provider concurrency is tight. It does not preempt a
provider call that is already in flight.
Terminal-grounding and generated task-user publication reviews use a
call-scoped Normal override because the user is waiting for those exact
verdicts; offline calls under the same `evidence_precision_judge` operation keep
the Background default.

## Starvation-free background admission

General background LLM loops (distill, classify, ambient distill, and comms
backfill) submit bounded work to the background lane. Write-producing memory
maintenance has a stronger boundary: utility review persists bounded work in a
scoped disk queue before dispatch, and batch consolidation persists the exact
source fingerprint in its retry guard. Those drainers yield before admission
while foreground work is live, the bounded background lane is full, or the same
configured provider is already serving or admitting background work. A busy period therefore
moves the durable due time; it is not counted as a model failure and does not
discard the source write.

The utility drainer claims one item at a time with expiring leases, processes at
most two items globally per minute by default, rotates scope order between
passes, and commits/fails each item independently. Retryable provider and queue
failures back off indefinitely with a cap. Repeated deterministic output/config
failures dead-letter only the affected item after eight attempts; changed source
evidence or a versioned reviewer-contract upgrade releases that dead letter. A
bounded applied-run ledger makes the
temperature-overlay mutation idempotent when lease recovery replays an item
after a process crash. The generic dispatcher intentionally remains
in memory so arbitrary prompts and credentials are not persisted as a replay
body.

Queue pickup remains foreground-biased, but the bias is bounded: a worker serves
normal work after at most eight consecutive high-priority pickups and serves
waiting background work after at most twelve consecutive foreground pickups.
This preserves interactive preference without allowing a continuously active
lane or maintenance producer to starve another lane indefinitely. Calls already
in flight are not preempted.

Within each priority lane, pickup is owner round-robin rather than plain FIFO.
The next job is the oldest job of the next owner: `task_ref.agent_id` when
`Some` and non-empty, else `task_ref.task_id`, else `job_id`. FIFO is preserved
within one owner so a single agent cannot fill a lane and starve others at the
same priority. Anonymous jobs use unique `job_id` keys and do not clump. High
still beats Normal/Background; the consecutive-high (8) and
consecutive-foreground (12) bounds are unchanged.

`reserved_interactive_workers` is a worker-admission boundary, not a provider
limit. With the shipped `workers: 12` / `reserved_interactive_workers: 4`, eight
workers may start Background calls while all twelve may serve High/Normal calls.
At least one worker remains background-capable for every configuration. When a
provider semaphore is full, the picked job transitions to the explicit
`waiting_for_provider` state and the provider's single scheduler waits for
capacity; no worker is counted busy during that wait. High/Normal/Background
priority and bounded fairness are preserved inside that scheduler. The
scheduler transfers a provider-bound permit with the selected High/Normal job,
so a Background job cannot race again and reverse the scheduler's priority
decision during worker handoff. Background handoffs deliberately release the
probe permit before returning to their lane: a Background job may be waiting
for the only background-capable worker, and must not hold scarce provider
capacity while an interactive worker is ready. Route changes reject and release
a permit bound to the old provider before acquiring capacity on the new route.
This behavior is provider-neutral and does not alter provider request bodies,
caching, continuation ids, or turn semantics.

Ollama-bound local-prep (`llm.dispatch.local_prep.enabled` and
`yield_worker` and at least one summarisable block at or above
`threshold_chars`) parks the job on a cap-1 `LocalPrepCoordinator`
(High > Normal > Background) and does not occupy a dispatch worker.
Cheap inlining (disabled, disclosure-bound, or below threshold) still
runs on the worker. After prep, the job is re-enqueued with
`local_prep_done` and proceeds to provider admission. Queue snapshots
and `LlmQueueEvent` expose `waiting_for_local_prep`.
`llm.dispatch.local_prep.yield_worker: false` restores in-worker HOL.
Cancellation, shutdown, disclosure, fall-through, and telemetry are
unchanged. Magician YAML seed is `provider_isolated`. Rollback:
`llm.dispatch.engine: legacy_worker_pool` (restart-bound).

## Error model

| `LLMError` variant | Caller action |
|---|---|
| `Cancelled { reason }` | Operational. Log info; don't escalate. |
| `AllRetriesExhausted { attempts, last_error }` | Hard failure. Surface to user; don't wrap in another retry loop. |
| `QueueFull { priority, depth, capacity }` | Backpressure. Caller-side retry/escalate priority/drop. |
| `ProviderUnavailable` | Circuit breaker open. Router fallback profile may still kick in via the dispatch layer; otherwise treat as hard failure. |
| `WorkerWatchdog` | Worker forcibly dropped a stuck future. Treated as `Timeout` for retry; surfaces only if all retries exhausted. |
| `DeadlineExceeded` | Submission deadline elapsed before pickup. |
| Provider/Validation/Configuration | Existing semantics, propagated through. |

## Cancellation

Three independent paths land jobs in the tombstone:

1. **Pre-dispatch gate** — worker reads `TaskStateView::snapshot` right before the provider call; cancelled/missing tasks tombstone.
2. **In-flight race** — `tokio::select!` between the provider future and a cancellation token sourced from `TaskStateView::subscribe_cancel`. When the orchestrator cancels a task, it calls `task_state_view.fire_cancel(task_id)` to fire the token.
3. **External API** — `queue.cancel_task(id, reason)` / `cancel_chat_session(id, reason)` / `cancel_job(id, reason)` records intent; the worker plugs the channel race at pickup time. The provider scheduler is notified immediately and also polls task cancellation/deadlines at a short bounded cadence, without creating one task per waiter.

Streaming and terminal-path invariants:

- Streams use the same per-lane lifetime admission permits as sync jobs; their
  provider-capacity wait races cancellation, submission deadline and both
  shutdown modes. A stream first scheduled after graceful shutdown begins is
  tombstoned before provider resolution.
- Consumer backpressure is supported, but forwarding to a full receiver races
  force shutdown and cancellation; after the tombstone is published its terminal
  error delta is best-effort, so an abandoned consumer cannot retain capacity.
- A delivered provider `Error` owns the terminal failure (one scheduler turn for
  the adapter to return its richer error, then abort); a delivered `Done` is the
  success commit. Neither can be rewritten by a racing cancel/shutdown.
- Health, breaker and 429 cooldown (Retry-After or bounded fallback) are
  attributed exactly once to the effective fallback provider.
- Terminal helpers drop byte reservations and lane/provider admission, publish
  registry/event state, release idempotency ownership and make the caller
  result visible *before* awaiting a possibly slow durable ledger sink. This
  also applies when admission rejects a job before lane publication.
- Streaming ledger `wait_ms` is the lane-pickup duration; provider semaphore
  contention stays in `provider_wait_ms`.
- Before observation or retention, sync, sync-to-stream fallback and
  native-stream `Done` responses cross aggregate response admission: 64 MiB per
  arbitrary JSON lane (compact wire), 192 MiB across lanes plus bounded depth
  and node count, and a separate 192 MiB retained-memory cap for the whole
  normalized response (rechecked after attaching the trace receipt). Rejected
  JSON is dismantled iteratively, and an invalid terminal response cannot
  retain a hung adapter.

Wire orchestrator transitions to both `TaskStateView::fire_cancel` AND `queue.cancel_task(...)` so cancellation propagates instantly to both queued and in-flight jobs.

## Result posting

Every job's terminal outcome flows through three channels:

1. **`oneshot::Sender`** on the `LlmJob` — inline await for the submitter.
2. **Task ledger** — `LlmCallLedgerEvent` (Submitted/AttemptStart/AttemptDone/Requeued/Completed/Failed/Tombstoned) appended to `<data_root>/llm_dispatch/<task_id>.jsonl`. Durable; consumed by executor projection on restart.
3. **Realtime event bus** — `LlmQueueEvent` broadcast. Subscribed by the viewer + any host realtime fanout.

Startup orphan recovery scans the historical task ledger in bounded background
maintenance after the queue has started. It must not sit on the critical path
before the Actix HTTP server binds; a slow or blocked ledger read should at most
delay synthetic tombstones for previous-process orphaned jobs, never make the
whole backend unavailable.
Immediate shutdown also bounds tombstone persistence for drained lanes,
provider waiters, delayed requeues, and streams: if a task-ledger sink is
blocked, the graceful deadline/force-shutdown token terminates that write
instead of keeping queue shutdown alive indefinitely.

## Observability

Endpoint: `GET /api/llm/queue/snapshot` returns the full `QueueSnapshot` JSON
(workers + total admitted depth per lane + provider-wait counts by lane +
`waiting_for_local_prep` (serde default 0) + last-N completed/failed/tombstoned).
Depth includes work parked for provider capacity, local-prep, or retry delay,
not only the channel's physical occupancy. Each job reports
lane `wait_ms`, provider-permit `provider_wait_ms`, and provider `execution_ms`
separately, so local-model semaphore saturation is not misdiagnosed as slow
inference. The `/llm/queue` table renders all three timings, and terminal
dispatch Parquet rows retain the same provider-wait field for later analysis.

Each job also carries the live runtime-activity row it was submitted from, as
`JobOrigin.activity_id`, persisted to the `activity_id` column of the terminal
dispatch Parquet rows. That column is the join key between the watchable view
of the runtime and this retrospective store: take `activity_id` off an
`ActivityStarted` event (websocket, or the scope's `events.jsonl`) and filter
`analytics/llm_dispatch/dt=*` on it to get the queue outcomes that activity
produced; go the other way to find the live row a slow or failed call came
from. It is a decimal correlation counter and nothing else — no credential,
prompt body, or user content rides on it. `magicllm` never mints the value;
the submitting call stamps it from its own tracing context, so a call made
outside any instrumented span records `NULL` rather than a placeholder.

The memory utility runner logs active, due, retrying, dead-letter, deferred, and
oldest-pending queue health for every pass. The same state is returned by
`GET /api/magician/v2/memory/temperature/status` and rendered on `/memory`.
These counters are derived from the leased disk state used for execution, not
from an in-memory estimate.

Realtime: subscribe to `queue.subscribe_events()` for live `LlmQueueEvent` stream.
`WaitingForProvider` and `WaitingForLocalPrep` are distinct from `Requeued`;
`Dispatched` is emitted only
once per logical job, after the matching provider permit has been obtained and
immediately before its first actual provider attempt. Per-attempt retry detail
continues through `AttemptStart`/`AttemptDone` ledger events. Streaming jobs use
the same lifecycle: their first provider-capacity miss emits exactly one
`WaitingForProvider` event. The event records the lane pickup time, and the
terminal event preserves that same `wait_ms`; provider contention accumulates
only in `provider_wait_ms`.

UI: `LlmQueuePanel.svelte` under `unified-ui/src/lib/magician/components/`,
mounted at `/llm/queue` (command palette **Jump to → LLM dispatch queue**, kbd
`G Q`, and the **Manage** section).

## Realtime event fanout

`LlmQueueEvent` has its own broadcast bus (`queue.subscribe_events()`)
independent of `RuntimeAgentEventType`. The dev-workbench viewer subscribes
directly. If you want the events on the existing `RuntimeTransportEvent`
channel (e.g. to fan out to existing client SSE), wire the supplied
`spawn_realtime_fanout(queue, forward)` helper:

```rust
use magician::magician_v2::dispatch_glue::spawn_realtime_fanout;

let queue = ...; // from start_dispatch_queue
let broadcaster = event_broadcaster.clone();
let _handle = spawn_realtime_fanout(Arc::clone(&queue), move |event| {
    // Map LlmQueueEvent -> RuntimeTransportEvent and emit on broadcaster.
    // See magician_v2::dispatch_glue::realtime_fanout for the integration point.
});
```

There are no `llm.queue.*` entries in `RuntimeAgentEventType`; adding them
needs matching variants in `magician-event-taxonomy` and
`magician_v2::realtime_events` plus `make event-taxonomy-codegen`. Marker
detection (`ContentBlock::SummarisableContext`), DuckDB analytics rows and
provider gauges are not built.

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
`MultiLLMService::set_dispatch_queue` install the process queue at boot.
The queue routes through `OperationLlmRouter::live_dispatch_router`, which
looks up the operation router's current `ConfiguredRouter` on every call, so
a config reload (`/settings/magician-config/reload`, or any
`reload_from_config`) reaches it without a restart. Why: when a picked profile
is an operation's default, Magician sends no profile override and the queue's
router re-resolves the mapping itself, so a stale router would dispatch the old
profile. `priority_for_operation` then routes `OperationLlmRouter` submissions through
the queue, and chat streaming (`generate_chat_completion_streaming*`) submits
through `submit_stream` on the High lane when `llm.dispatch.enabled`. Direct
`route` / `route_stream` remain when the queue is not installed. Callers with
a task scope use `LlmJob::new(request, origin).with_task(...)` so the
cancellation gates engage.
