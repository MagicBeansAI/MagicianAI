# Magician Core

Layer-1 extraction of the `magician` monolith (`crate-split-layer1`).

## Purpose

`magician-core` is the first crate split out of the `magician` application
crate. It owns **pure-logic modules** — code with no dependencies on the rest
of `magician_v2` — so edits there no longer recompile the whole application,
and later layers can build on a stable foundation.

`json_traversal` is a re-export of `magicvault-primitives`, which also
supplies synchronous durable publication and retry primitives; the original
import paths stay valid. The async durable-write adapter and blocking-pool
admission stay here, including the permit acquired before rename, so there is
no second durable writer or JSON walker and no credential/backend dependencies
in `magician-core`. The upstream revision is pinned in root `Cargo.toml` and
`Cargo.lock`.

Durable publication invariants (the pinned primitives, `0.1.2` in
`Cargo.lock`, and the async writer move together): staging is created exclusively with the requested Unix mode, exact
permissions are applied before bytes and covered by the pre-rename fsync,
pre-existing files/symlinks are never adopted, and cleanup removes only a
staging file created by that call. Bare filenames use `.` for parent sync.
Covered by `make test-magicvault-compatibility`.

`config_extras` keeps separate boot-time CUA and Mac host-automation
availability flags. CUA providers on Windows/Linux must not enable Mac-only
skill catalogs; see [CUA setup](../scripts/cua-setup.md).

## Extraction contract

- Only modules whose entire internal-dependency set is `magician-core` itself
  (or other extracted modules) may move here.
- A moved module keeps its path: `magician_v2` re-exports it
  (`pub use magician_core::<mod>;`) so every `crate::magician_v2::<mod>::…`
  consumer compiles unchanged.
- `pub(crate)` items are widened to `pub` to cross the crate boundary; a move
  commit changes no behavior.
- A move commit must leave `cargo check -p magician --lib` green and the moved
  module's unit tests passing in the new crate.

## Owned modules

Prompt metadata and version commentary use Magican for the product identity.
The backend crate and API namespace remain `magician`; persisted hashes and
service IDs are not renamed.

- `json_traversal` — stack-safe traversal, size/depth metrics, bounded and
  canonical JSON serialization, and blake3 hashing for external JSON/tool
  payloads. `pretty_serialized_len` is `pub` because `magician` tests import
  it.
- `durable_io` — `write_bytes_durably(_with_mode)`, the sync
  `write_bytes_durably_sync`/`write_bytes_durably_with_mode_sync` pair,
  `publish_staged_file_durably_sync`, `sync_parent_dir_blocking`, transient-I/O
  retry wrappers and `warn_cleanup_failed`; `artifact_v2::io` re-exports them.
  Async parent-dir fsync acquires a blocking-admission permit *before* rename,
  then `File::sync_all` through `spawn_blocking_with_admission`. Join and fsync
  errors surface as `io::Error`. Streaming JSON writers
  (`write_json_*_atomic_stream`) use `spawn_blocking_admitted`.
- `blocking_admission` — bounded `spawn_blocking` admission. Default 16 permits
  on `current` (4 `small_cpu`, 32 `m2_max`/`cloud_heavy`; `auto` picks
  `small_cpu` at ≤ 4 CPUs, else `current`; `magician/src/runtime_plan.rs`).
  `configure_blocking_admission(0)` is unlimited.
  `MAGICIAN_BLOCKING_ADMISSION=off` (also `pass_through` / `0` / `false`)
  skips the semaphore and still counts in-flight. Does not set Tokio
  `max_blocking_threads`. Acquire on the async runtime, then
  `spawn_blocking`; never acquire from a blocking thread. Magician feed
  and UI-thread DuckDB plus list-index SQLite take a per-scope async gate
  before this permit; task flocks and workspace JSON readers admit
  without a process-wide async mutex. Restart-bound; watch the
  `blocking_admission` gauge (`make test-runtime-performance-live-eval`).
  Never wait for a file lock inside a permit: `spawn_blocking_admitted_flock`
  runs an attempt that uses `try_lock_*` and, while `is_lock_contended`,
  retries after a sleep outside the pool. A blocking `lock_exclusive()` would
  hold its permit for as long as another holder keeps the lock, so a few
  waiters could fill the pool and stall every admitted file operation.
- `idle_resource_cache` — weakly registered process-local owners with a
  60-second idle grace and eight idle owners per cache. Borrowed Arcs protect
  active operations. Retirement closes outside the cache mutex and serializes
  reopen for the same key. The host owns one periodic collector;
  `make test-workspace-resource-cache` checks expiry, capacity, in-flight
  protection and slow-close concurrency.
- `prompts` — prompt name/version constants, types (re-exported from
  `runtime_core`), JSON + trait storage and `PromptManager`; re-exported from
  `magician_v2::prompts`. A new prompt JSON is inert until its version
  constant moves; earlier prompt files stay available. Current pins:
  - tutor: `visual_storyboard_runtime` 1.2.0, `personal_tutor_policy` 1.1.0;
  - agentic decision: `agentic_decision` 1.3.11 and `agentic_decision_system`
    1.0.9 (pass large values by reference with `{"$ref"}` / `{"$file"}`
    arguments and `export_result` instead of paging and retyping them; run
    saved artifacts through their own system before copying their source;
    count errors inside a successful result as failures; investigate context
    first, ask only for unavailable facts, preferences or authority, bundle
    questions, reuse answers and record reversible assumptions). This applies
    to ordinary tasks and Relentless; real question-selection quality still
    needs model/runtime qualification;
  - `chat_outer_loop_system` 0.0.7 (need-based tool use, a "Finishing a
    request" section);
  - slot extraction: `SLOT_EXTRACTION` (1.0.0, the `legacy` planning path) and
    `SLOT_EXTRACTION_ASSESSED` (1.1.0: adds request-level
    `extraction_confidence` / `extraction_reasoning`, used by
    `planning.analysis_path` `shadow` and `new`);
  - progress reviewer: `PROGRESS_REVIEW` (`progress_review` 1.0.0) and
    `PROGRESS_REVIEW_SYSTEM` (`progress_review_system` 1.0.1);
  - preference alignment: `ACTION_ALIGNMENT_JUDGE`
    (`action_alignment_judge` 2.0.0), returning `alignment_decision.v2`,
    checked by the host validator in
    `magician_v2::preference_alignment::contract`;
  - voice: `VOICE_LIVE_MOUTH_SYSTEM` (`voice_live_mouth_system` v1.0.0) is
    GPT-Live-1 session instructions only (GPT Realtime / Gemini keep
    `VOICE_MODALITY_ADDENDUM` on the chat outer loop);
    `VOICE_MEETING_PRESTO_SYSTEM` (`voice_meeting_presto_system` v1.0.0) is
    meeting-join Presto session instructions.

  It carries the `test-support` feature (see *The `cfg(test)` trap* below).
- `attention_funnel` — the canonical attention-routing vocabulary:
  candidates, route decisions, source families, trace events. Depends only on
  serde; re-exported from `magician_v2`.
- `attention_funnel_store` — append-only rusqlite observability store for
  route events. SQLite work takes an async gate before a process-wide blocking
  permit so funnel jobs cannot starve parent-dir fsync. Production opens
  `open_at` on the cataloged `host_database_path(..., AttentionFunnel)` file;
  `open(base_root)` joins `attention_funnel.db` for tests. `open_in_temp` is
  not `cfg(test)`-gated because dependent modules' tests use it.
  `attention_lane_facade` stays in `magician_v2`: its imports pull
  `channel_assist`, `feed` and `resurfacing`, so it is not a leaf yet.
- `history` — legacy lane inference for threads.
- `config_extras`, `hitl`, `gws_cli` — std + serde helpers (gws_cli shells out
  to the Google Workspace CLI), re-exported from `magician_v2`.
- `slot_graph` kernel (types + enrichment) and `confidence` — the persisted
  slot vocabulary (`SlotRecord`, `SlotType`, provenance, `ProvisionalSlot`),
  the enrichment pipeline and the confidence scorer.
  `magician_v2::slot_graph` re-exports them, so `super::types` paths in the
  remaining slot_graph files still resolve. Extraction, elicitation, rewriter,
  repository and adapters stay in `magician_v2`, coupled to
  `analytics::operation_llm_telemetry`, `ask_loop::clarifier` and
  `state_tracker`.
- `local_resource_governor` — `snapshot(memory_overlay_max_wait_ms)` takes the
  gauge value as a parameter (the API layer reads it from
  `magician_vector_index`), keeping LanceDB/Arrow out. Snapshot
  `schema_version` is `8`. `admit_agent_loop` is the hard cap (default 50;
  `0` is observe-only). Per-agent outstanding default 8. The RSS tripwire is
  probe-injected (`configure_rss_probe`); no `sysinfo` dependency.
  Delegated-child permits default to `[4, 4, 2, 1]`;
  `configure_per_agent_outstanding_limit` and
  `configure_delegated_child_permits` (once, at startup, from the runtime
  scale plan) raise the per-agent limit and the first two delegation levels.
  A child that finds every slot taken logs `[DELEGATION] child waiting for a
  delegated-child slot`. `make test-admission-300-task-eval-harness` is the
  `/evals` harness lane for
  `admit_agent_loop_300_task_admission_count_harness`, not the live soak.
  Magician-bin calls `configure_live_agent_limit` from the resolved
  `runtime.scale` plan before building runtimes.
- Decision metrics — the pure incremental reducer, fixed-size latency
  histograms and bounded scoped SQLite summaries.
  `make test-decision-metrics-store` verifies receipt deduplication, cohort
  separation, explicit review, retention and indexed large-data reads without
  compiling the host.

## Layer-2 analysis (slot_graph proper)

Moving extraction/elicitation/rewriter/repository requires inverting or
co-moving operation-LLM telemetry types (entangled with `query_analysis`,
`realtime_events` and `slot_graph`), the ask-loop clarifier and
`state_tracker`. The seam exists: `dyn LlmService`/`dyn RewriteModel`, with
`adapters.rs` gluing them to `OperationLlmRouter`.

## The `cfg(test)` trap, in both its forms

Across a crate boundary, `cfg(test)` is true only while **magician-core
compiles its own tests**; for a downstream crate's tests magician-core is an
ordinary dependency and it is false.

**Visibility.** `pretty_serialized_len` and `open_in_temp` must be un-gated
to stay importable.

**Behaviour.** `required_prompt_manager` falls back to the on-disk prompt
store when the process-global `PromptManager` was never installed, because a
test binary never runs the startup builder. Gated on `cfg(test)` alone, the
fallback would not apply to `magician`'s tests (they fail with `global
PromptManager is not initialized`), and nothing fails to compile.

`test-support` restores it: `#[cfg(any(test, feature = "test-support"))]`,
enabled from `magician`'s `[dev-dependencies]` only. A release build must
still fail loudly on an uninstalled manager; with `resolver = "2"` a
dev-dependency feature does not unify into a normal build. `cargo tree -p
magician -e features` shows the normal edge resolving `default` alone and the
dev edge carrying `test-support`.

**When moving a module, check every `cfg(test)` in it for both forms.** A
gate that silently supplies something is harder to spot than one that hides a
symbol, because the crate still builds.
