# LLM Observability Contracts

This directory owns the machine-readable contracts for the LLM trace, outcome,
acceptance, and training-data program.

- `coverage-ledger-v1.json` classifies every current production model-invocation
  boundary as queued, direct, streaming, realtime/media, external, or excluded.
- `operation-families-v1.json` assigns every shipped operation mapping to one
  v1 quality family and names the runtime surface authoritative for immediate
  and delayed validation. Agentic ledger compaction belongs to the planning and
  decision family because its schema-validated patch is proposed by the model
  and enforced by the deterministic execution reducer. The coverage audit
  reads the operation keys from the repository seed (the former fallback
  template, which it compared against, was removed on 2026-09-24).
- `sink-contract-v1.json` records the current buffering, durability, scope, and
  known loss points of the `llm_calls` and `llm_dispatch` Parquet sinks.
- `phase2-fact-contract-v1.json` freezes the content-free call/attempt/gap
  vocabulary, schema versions, lifecycle revisions, and journal idempotency key
  used by the Phase 2 recorder, including the 16 KiB serialized-fact ceiling
  and bounded operation/capability/routing categories. Folded cache, reasoning
  and realtime modality buckets are validated before acceptance.
- `phase2-durability-contract-v1.json` freezes Phase 2B buffer priorities and
  capacities, append/materialize/commit ordering, restart repair behavior,
  exact critical-gap accounting, and bounded shutdown guarantees. Phase 2F
  activates this pipeline by default. One cross-process writer lease, a 64 MiB
  segment ceiling, bounded prefix reads, and defensive owner-drop drain protect
  restart recovery.
- `phase2-materialization-contract-v1.json` freezes Phase 2C dataset ownership,
  UTC partitions, immutable deterministic objects, typed content-free columns,
  publish/verify/watermark ordering, replay conflict behavior and per-dataset
  checksum watermarks. Phase 2D owns compaction and governed latest-revision
  reads; Phase 2F owns runtime activation and recovery.
- `phase2-governed-read-contract-v1.json` freezes Phase 2D's content-free fact
  registry, source-bound compaction manifest, raw-or-compacted partition
  selection, revision coalescing, historical typed projection, strict scope
  boundary and bounded shared-read contract. Phase 2E owns consumer wiring.
- `phase2-product-access-contract-v1.json` freezes Phase 2E's shared REST and
  `internal_data` access, runtime-authoritative scope and bounded envelopes.
- `phase2-activation-contract-v1.json` freezes Phase 2F's default-on producer,
  journal authority, non-invention and explicit-gap behavior, restart/shutdown
  recovery, compatibility-mirror deduplication, retention streams and strict
  content-free reconciliation tolerances.
  Canonical runtime cost is recomputed from validated usage and effective-dated
  pricing; producer mismatches remain explicit diagnostics.
- `phase3-sanitized-content-contract-v1.json` freezes Phase 3 ownership,
  capture-event boundaries, policy precedence, fail-closed sanitization,
  restricted durability, one-use grants, audited bounded reads, restart-safe
  deletion and independent retention. Encrypted raw-local capture remains
  disabled and restricted records stay outside ordinary fact SQL.
- `phase4-tool-lineage-contract-v1.json` freezes Phase 4's content-free model
  proposal, validation, authorization, approval, execution, result,
  consumption, branch, delegation and rollback lifecycle. It also fixes the
  producing-call/execution identity, explicit-gap behavior, governed trace
  reads and conservative side-effect semantics.

`scripts/audit-llm-trace-coverage.py` fails when source markers disappear,
operation assignments duplicate or omit a shipped mapping, validators point to
missing source, a direct-router/provider/realtime discovery probe finds a new
unowned production path, or an entry lacks an explicit capture/training posture. The
ledger describes logical caller boundaries; provider adapters are attempts
inside a logical call and must not be counted as independent training examples.

The audit also runs a production-code operation-key scan over the Rust crates
(`LLMOperation::Other("…")` literals and `*OPERATION*` string constants,
test regions excluded): an operation key that exists only in code never enters
the config/ledger diff above and silently rides the router default profile, so
its absence from `operation-families-v1.json` is an audit failure, not a
silent gap. This surfaced — and now fences — the four formerly-unmapped
operations `memory_attach_stage2`, `progress_review`, `agent_general`, and
`page_understanding_vision`, all now explicitly mapped in the `llm-router.yaml`
seed (to `gpt6luna-responses-toolsany` / `gpt61sol-responses-toolsany`).

The `embedding` family (`embedding_output_v1`) covers the routed embedding
operations `embed_documents` and `embed_query`, bound to the
`op-embedding-local` profile (dedicated embedding daemon). Provider identity
for embeddings is router config, not a Rust constructor: the seam lives in
`magician-vector-index/src/embedding_router.rs` and resolves through magicllm's
`ConfiguredRouter::embed_for_operation`, with a byte-identical ad-hoc-provider
fallback when no router is installed. Switching the embedding provider is a
profile edit plus a full re-embed (`embedding_contract_id` rotates).

`scripts/eval-llm-observability-phase0.py` reads Parquet in place and writes a
content-free population/volume report. It never persists prompts, responses,
contact data, tool arguments, or tool results.

`scripts/test_llm_trace_phase2b.py` checks the durability contract against the
Rust source without invoking a model or provider. The focused Rust module tests
exercise real fsynced journals in temporary scoped workspaces.

`scripts/test_llm_trace_phase2c.py` checks the materialization contract against
the Rust source without invoking a model or provider. Its focused Rust module
tests write and query real temporary Parquet datasets through DuckDB.

`scripts/test_llm_trace_phase2d.py` checks the governed-read contract against
the registry, compactor and shared-service source. Focused Rust tests exercise
real temporary Parquet compaction, stale-manifest fallback, revision
coalescing, provider-attempt enrichment, historical schema defaults and scope
isolation.

`scripts/test_llm_trace_phase2f.py` checks default activation, graceful drain,
mapping/non-invention, mirror deduplication, retention and complete authored
recovery coverage without invoking a provider. The focused Rust tests exercise
real temporary journals and Parquet materialization. The shutdown contract also
pins cancellation and joining of the singular storage-maintenance runtime
before final canonical and compatibility batches are drained.

`scripts/eval-llm-observability-phase2f.py` is the live, read-only exit audit.
It compares canonical and compatibility calls by stable id, reconciles tokens,
cost, latency and attempt counts (including explicit gaps), joins the auxiliary
scoped `llm_dispatch` timing stream by both job and call id, checks pricing
provenance, separates unclassified process-level transport loss from scoped LLM
fact gaps, and probes the governed overview. It never selects or reports model
content and emits linked JSON/HTML reports under the configured coverage root.

`scripts/test_llm_trace_phase3.py` freezes the provider-free Phase 3 ownership,
privacy, restricted-storage and authorization boundaries. Focused Rust tests
exercise exact borrowed observations, secret/dynamic-key/media sanitization,
bounded one-use reveals, durable audit, restart-safe deletion, journal
namespace isolation, exact replay and independent retention.

`scripts/eval-llm-observability-phase3.py` is a local live privacy audit. It
reads a bounded restricted cohort but emits only counts and machine categories
to JSON/HTML. With a setup token and call id it also proves a real grant can be
used exactly once. It runs after Phase 2F as the final child of opt-in aggregate
live evals; metadata-only installations report an explicit skip.

`scripts/test_llm_trace_phase4.py` verifies the frozen lineage contract and
its runtime, storage, read, UI and report wiring without calling a provider.
`scripts/eval-llm-observability-phase4.py` performs the bounded live,
aggregate-only lineage audit; cohorts without eligible tool executions return
an explicit skip rather than a false pass.
