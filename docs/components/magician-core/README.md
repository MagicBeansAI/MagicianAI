# Magician Core

Layer-1 extraction of the `magician` monolith (`crate-split-layer1`).

`config_extras` keeps separate boot-time CUA and Mac host-automation availability
flags. CUA providers on Windows/Linux must not enable Mac-only skill catalogs;
see [CUA setup](../scripts/cua-setup.md).

## Purpose

The MagicVault extraction makes `json_traversal` a re-export of
`magicvault-primitives`; synchronous durable publication and retry primitives
also come from that crate. The original import paths remain valid. The async
durable-write adapter and blocking-pool admission remain here, including the
permit acquired before rename. This prevents a second durable writer or JSON
walker and avoids pulling credential/backend dependencies into `magician-core`.
The upstream revision is pinned in root `Cargo.toml` and `Cargo.lock`.

Durable publication invariants (primitives `0.1.1` and the async writer move
together): staging is created exclusively with the requested Unix mode, exact
descriptor permissions are applied before bytes and covered by the pre-rename
fsync, pre-existing files/symlinks are never adopted, and cleanup removes only a
staging file created by that call. Bare filenames use `.` for parent sync.
Covered by `make test-magicvault-compatibility`.

`magician-core` is the first crate split out of the `magician` application
crate. It owns **pure-logic modules** — code with no dependencies on the
rest of `magician_v2` — so that edits there no longer recompile the whole
application, and so later layers can build on a stable foundation.

## Extraction contract

- Only modules whose entire internal-dependency set is `magician-core` itself
  (or other already-extracted modules) are eligible to move here.
- Each moved module keeps its historical path: `magician_v2` re-exports it
  (`pub use magician_core::<mod>;`) so every existing
  `crate::magician_v2::<mod>::…` consumer compiles unchanged.
- `pub(crate)` items are widened to `pub` to cross the crate boundary; no
  behavior changes are allowed in a move commit.
- A move commit must leave `cargo check -p magician --lib` green and the moved
  module's unit tests passing in the new crate.

## Owned modules

Prompt metadata and version commentary use Magican for the product identity.
The backend crate and technical API namespace remain `magician`; this is a
presentation-boundary change, not a rename of persisted hashes or service IDs.

- `json_traversal` — stack-safe traversal, size/depth metrics, bounded and
  canonical JSON serialization, and blake3 hashing for externally supplied
  JSON/tool payloads. `pretty_serialized_len` is a real `pub` item because
  tests in `magician` import it.
- `durable_io` — `write_bytes_durably(_with_mode)`, the sync
  `write_bytes_durably_sync`/`write_bytes_durably_with_mode_sync` pair,
  `publish_staged_file_durably_sync`, `sync_parent_dir_blocking`, transient-I/O
  retry wrappers, and `warn_cleanup_failed`. `artifact_v2::io` re-exports
  them. Async parent-dir fsync acquires a blocking-admission permit *before*
  rename, then `File::sync_all` through `spawn_blocking_with_admission`.
  Join and fsync errors surface as `io::Error`. Streaming JSON writers
  (`write_json_*_atomic_stream`) use `spawn_blocking_admitted`.
- `blocking_admission` — bounded Magician-owned `spawn_blocking` admission.
  Default 16 permits on `current`/`auto` (4 `small_cpu`, 32
  `m2_max`/`cloud_heavy`). `configure_blocking_admission(0)` is unlimited.
  `MAGICIAN_BLOCKING_ADMISSION=off` (also `pass_through` / `0` / `false`)
  skips the semaphore and still counts in-flight. Does not set Tokio
  `max_blocking_threads`. Acquire on the async runtime, then
  `spawn_blocking`; never acquire from a blocking thread. Magician feed
  and UI-thread DuckDB plus list-index SQLite take a per-scope async gate
  before this permit; task flocks and workspace JSON readers admit
  without a process-wide async mutex. Restart-bound; watch the
  `blocking_admission` gauge (`make test-runtime-performance-live-eval`).
- `prompts` — prompt name/version constants, types (re-exported from
  `runtime_core`), JSON + trait storage, and `PromptManager`.
  Re-exported from `magician_v2::prompts`.
  `VOICE_LIVE_MOUTH_SYSTEM` (`voice_live_mouth_system` v1.0.0) is GPT-Live-1
  session instructions only; GPT Realtime / Gemini keep
  `VOICE_MODALITY_ADDENDUM` on the chat outer-loop.
  `VOICE_MEETING_PRESTO_SYSTEM` (`voice_meeting_presto_system` v1.0.0) is
  meeting-join Presto session instructions.
  - Carries the `test-support` feature — see *The `cfg(test)` trap* below.
- `attention_funnel` — the canonical attention-routing vocabulary:
  candidates, route decisions, source families, trace events. Depended on
  nothing but serde; re-exported from `magician_v2`.
- `attention_funnel_store` — append-only rusqlite observability store for
  route events. SQLite work takes an async gate before a process-wide
  blocking permit so funnel jobs cannot starve parent-dir fsync. Production
  opens `open_at` on the cataloged `host_database_path(..., AttentionFunnel)`
  file; `open(base_root)` joins `attention_funnel.db` for tests.
  `open_in_temp` is not `cfg(test)`-gated because dependent modules'
  test code constructs stores through it. `attention_lane_facade` deliberately stays in
  `magician_v2`: its grouped import pulls `channel_assist`, `feed`, and
  `resurfacing`, so it is not a leaf until those stores are addressed.
- `history` — legacy lane inference for threads.
- `config_extras`, `hitl`, `gws_cli` — std + serde helpers (gws_cli shells
  out to the Google Workspace CLI). All re-exported from `magician_v2`.
- `slot_graph` kernel (types + enrichment) and `confidence` — the persisted
  slot vocabulary (`SlotRecord`, `SlotType`, provenance, `ProvisionalSlot`),
  the enrichment pipeline, and the confidence scorer. `magician_v2::slot_graph`
  re-exports the moved modules, so `super::types` paths inside the remaining
  slot_graph files still resolve. The heavy half of slot_graph (extraction,
  elicitation, rewriter, repository, adapters) stays in `magician_v2`: it is
  coupled to `analytics::operation_llm_telemetry`, `ask_loop::clarifier`, and
  `state_tracker`.
- `local_resource_governor` — `snapshot(memory_overlay_max_wait_ms)` takes
  the gauge value as a parameter (the API layer reads it from
  `magician_vector_index`), keeping LanceDB/Arrow out of magician-core.
  Snapshot `schema_version` is `8`. `admit_agent_loop` is the hard cap
  (default 50; `0` is observe-only). Per-agent outstanding default 8. RSS
  tripwire is probe-injected (`configure_rss_probe`); no `sysinfo`
  dependency. Delegated-child permits remain `[4, 4, 2, 1]`.
  `make test-admission-300-task-eval-harness` is the `/evals` harness lane
  for `admit_agent_loop_300_task_admission_count_harness`, not the
  live soak. Magician-bin calls `configure_live_agent_limit`
  from the resolved `runtime.scale` plan before constructing runtimes.

## Layer-2 analysis (slot_graph proper)

Moving extraction/elicitation/rewriter/repository requires inverting or
co-moving operation-LLM telemetry types (entangled with `query_analysis`,
`realtime_events`, and `slot_graph`), the ask-loop clarifier, and
`state_tracker`. The seam already exists — `dyn LlmService`/`dyn RewriteModel`
with `adapters.rs` gluing them to `OperationLlmRouter`.

## The `cfg(test)` trap, in both its forms

Extraction changes what `cfg(test)` *means*. Inside the monolith a module
compiled as part of the crate under test, so `cfg(test)` was true for it
whenever `magician`'s tests were built. Across a crate boundary it is true only
while **magician-core compiles its own tests**; for a downstream crate's tests
magician-core is an ordinary dependency, and it is false.

**Visibility.** `pretty_serialized_len` and `open_in_temp` must be un-gated to
stay importable. There is a second, quieter form:

**Behaviour.** `required_prompt_manager` falls back to the on-disk prompt store
when the process-global `PromptManager` was never installed, because a test
binary never runs the startup builder. Gated on `cfg(test)` alone, that fallback
would not apply to `magician`'s tests (they fail with `global PromptManager is
not initialized`), and nothing fails to compile.

`test-support` restores it: `#[cfg(any(test, feature = "test-support"))]`,
enabled from `magician`'s `[dev-dependencies]` only. A release build must
still fail loudly on an uninstalled manager; with `resolver = "2"` a
dev-dependency feature does not unify into a normal build. `cargo tree -p
magician -e features` shows the normal edge resolving `default` alone and the
dev edge carrying `test-support`.

**When moving a module, check every `cfg(test)` in it for both forms.** A gate
that silently supplies something is harder to spot than one that hides a symbol,
because the crate still builds.
