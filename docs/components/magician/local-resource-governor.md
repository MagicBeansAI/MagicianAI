# Local Resource Governor

The local resource governor (`magician-core/src/local_resource_governor.rs`,
re-exported with the sysinfo RSS probe by
`magician/src/magician_v2/local_resource_governor.rs`; there is no
`LocalResourceGovernor` type) is the process-local admission controller and
read model for resource pressure during high-agent-count runs.

Live agent loops are **hard-capped** by `admit_agent_loop` before the agentic
execute future is constructed. The default cap is 50.
`configure_live_agent_limit(0)` is observe-only: loops are counted and
would-throttle is recorded, but admission never rejects. `magician-bin`
applies `configure_live_agent_limit` from
`EffectiveRuntimePlan.live_agent_limit`. The per-agent outstanding limit and
RSS tripwire have no YAML knobs; they are set through
`configure_per_agent_outstanding_limit` and `configure_rss_tripwire`. The host
RSS probe is installed after config load via `install_process_rss_probe()`.

Delegated-child permits stay compile-time `[4, 4, 2, 1]` by depth. That
split-by-level table is deadlock prevention and is not part of this gate.

## Snapshot API

Endpoint: `GET /api/local-resource-governor/snapshot`

The response includes:

- `mode: "hard_admission"` and `observe_only: false` when the live-agent cap is
  greater than zero; `mode: "observe_only"` and `observe_only: true` when the
  cap is `0`
- `admitted` (currently active loops), `rejected` (live-agent cap),
  `rss_blocked`, and `per_agent_rejects`
- current and high-water resource gauges
- limits and `ok` / `watch` / `pressure` severity
- pressure advice rows for resources in watch or pressure state
- would-throttle counters for signals that crossed a soft limit
- the latest observed agent-loop identity, when available

The snapshot `schema_version` is `8`.

## Instrumented Signals

The governor covers the local pressure points that matter most for 30-50
concurrent agents (200-300 is the scale target):

- active agent execution loops, with hard admission, per-agent outstanding
  caps (default 8), and an RSS tripwire with hysteresis when a probe is
  installed
- scoped goal-trigger queue admissions, duplicates, and queue-full drops
- pending goal triggers waiting behind active cycles
- canonical runtime event sink backlog
- canonical event append-lock wait high-water
- memory temperature overlay write-lock wait high-water
- direct and streaming `MultiLLMService` routes that bypass the LLM dispatch
  queue
- injected retrieval HOL gauges: journal lock waiters, embedding waiters,
  oldest embedding-write wait, Lance in-flight/waiters, journal snapshot
  hits/hydrates, hybrid result cache hits/misses/entries/waiters, Lance
  table-pool idle/hits/misses, query-vector cache entries, and query
  embedding batch waiters/fill
- Magician-owned blocking admission (`blocking_admission` gauge): in-flight,
  high-water, waiters, wait-ms, and admitted total. Default 16 permits on
  `current`. In-flight includes a held permit between durable rename and
  parent-dir fsync. Feed/UI-thread DuckDB and list-index SQLite wait on a
  per-scope async gate before taking a permit. `configure_blocking_admission(0)`
  and `MAGICIAN_BLOCKING_ADMISSION=off` skip the semaphore and still count
  in-flight. Tokio `max_blocking_threads` is not reduced.

Non-agent-loop gauges are operator-facing diagnostics. Agent-loop
admission is the first hard scheduler input.

## UI

The Unified UI mounts the viewer at `/runtime/resources`. It polls the snapshot
every two seconds and is reachable from the command palette as **Local
resources**. In local web development, Vite explicitly proxies the governor's
top-level `/api/local-resource-governor/*` namespace to Magician on port 3002;
the route predates the newer `/api/magician/v2/*` namespace and is not covered
by that broader proxy prefix.

## Operator Use

Run the local system with the intended agent fan-out, then watch:

- active agent loops approaching the hard live-agent cap (default 50; `0`
  restores observe-only)
- per-agent rejects when one agent fills its outstanding cap (default 8)
  while other agents still admit
- RSS tripwire blocks (hysteresis: reject at/above high, recover only at/below
  low). The probe is Magician process RSS only; Ollama RSS is out of process.
- trigger queue pressure growing even when active loop count looks healthy
- event backlog or append-lock wait spikes during bursty task completion
- memory overlay lock wait spikes during maintenance or consolidation
- direct LLM-route volume that still bypasses dispatch-queue cancellation and
  provider-level backpressure

The live-agent cap is installed at boot from `runtime.scale` via
`configure_live_agent_limit`. Seed profile `current` keeps 50.
`MAGICIAN_SCALE_PROFILE` and `runtime.scale` are restart-bound.
Blocking admission is installed the same way via
`configure_blocking_admission` (seed 16). Watch `blocking_admission`
in-flight and wait during a 300-task soak. `MAGICIAN_BLOCKING_ADMISSION=off`
is the observe-only kill switch.

## 300-task admission-count harness (not the live soak)

`admit_agent_loop_300_task_admission_count_harness` in
`magician-core/src/local_resource_governor.rs` is a compile-ready count
contract: admit 300 with limit 300, reject 301, drop every guard, count
returns to 0. It is **not** the owner-gated 300-task soak (idle / mixed-
provider / cancellation waves on live hardware).
