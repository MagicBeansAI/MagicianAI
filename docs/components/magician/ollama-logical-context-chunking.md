# Ollama Logical-context Chunking

The framework lets one structured logical operation span multiple bounded
Ollama calls and still return one validated response. Six producers are wired
to the runner; their candidate profiles use the local generation model
(`*local_generation_model`: seed `gemma4:12b`, rewritten per RAM tier by
`make setup-ollama`) with 32K physical context, thinking disabled and MTP
`draft_num_predict: 4`.

Governed app-memory adapters stay on the same physical provider boundary. RemoteAllowed accepted app-memory projections may enter the
destination-owned persistent index only under their current source/grant/policy
partition. LocalOnly app memory uses an ephemeral scorer only while an exact
live local-provider credential is present and reattested before and after I/O;
it never falls back to a remote embedder or reusable chunking/reviewer sink.
The logical chunk planner is unchanged by this.

Logical chunk telemetry follows the shared LLM observability identity
contract. The aggregate logical request owns the parent call id; every physical
map, repair, mapped fallback and reduction request receives a fresh child call
id under the same root trace with `chunk_map`, `chunk_repair`,
`chunk_fallback`, or `chunk_reduce` relation.

## Context contract

`LLMProfile` has provider-neutral typed fields:

```yaml
context_window_tokens: 32768
chunking:
  enabled: true
```

Runtime modes derived from that configuration:

| Profile shape | Preflight behavior | Provider behavior |
|---|---|---|
| no typed context | disabled | unchanged request |
| context + `chunking.enabled: false` | observe | request still runs |
| context + `chunking.enabled: true` | enforce | physical overflow fails before provider dispatch |

Every candidate profile has `chunking.enabled: true` and its operation mapping
points at it; hard physical-context preflight is active for every child, and
recovery is Ollama-only `same_provider_only`. Voice context compaction stays on
its separate baseline mapping.

Release-readiness diagnostics pin the `gemma4:12b` model, 32K physical window,
4K output reserve, JSON output and local endpoint; a different model fails
readiness until reviewed. Only coherent per-operation states pass
readiness/reload validation: dormant (candidate disabled + baseline mapping) or
activated (candidate enabled + candidate mapping). Rollback is per operation:
restore the baseline mapping listed in the activation manifest and set only
that candidate's `chunking.enabled` to `false`.

## Estimation and budgets

`ConservativeOllamaEstimator` deterministically estimates one token per two
UTF-8 bytes. It includes role markers, text and structured content, tool
schemas, and JSON response schemas. This intentionally overestimates ordinary
prose, code, and compact JSON. A configurable safety margin — 2,048 tokens for
chunk policies — protects additional tokenizer/model variance. Unified
tool-result projection reuses the same estimator so projection admission and
MagicLLM preflight cannot disagree at a rounding boundary.

Physical capacity:

```text
estimated input + reserved output + safety margin <= physical context
```

Shared helpers also calculate effective payload capacity and return typed
errors for static, physical, or logical context overflow.

The six local-memory profiles set a 1,200-second logical deadline on top of
their per-call timeout. Each candidate targets the local generation model with a 32K physical window, 256K logical window, 24K
target payload, 4K output reserve, 2K static-overhead reserve, 2K safety
margin, thinking off, JSON output, MTP `draft_num_predict: 4`, and
`same_provider_only` recovery.

## Planner

`magicllm` exposes provider-neutral logical request, budget, item, descriptor,
plan, validation, and reduction types plus the `ChunkDomainAdapter` boundary.
The adapter owns structured source extraction, semantic oversized-item
splitting, physical request rendering, map/final validation, bounded
repair/fallback, and reduction semantics. Adapters that omit a required hook
return a typed unavailable error.

Each original source has a stable root identity and source order. A semantic
split child carries the same root identity, its immediate parent identity, and
an ordered segment path. Planning recursively splits only through the adapter;
there is no generic byte, character, JSON-token, or prose slicing fallback. A
depth guard rejects non-progressing/cyclic adapter behavior without imposing a
separate limit on the derived number of chunks.

The planner performs stable first-fit packing in source order. It appends whole
terminal items until the next item exceeds the effective payload, then starts
the next chunk. Before returning a plan it proves that every terminal identity
appears exactly once and every original root is covered. Missing, unexpected,
or duplicate identities are typed failures, never partial plans.

## Runner

Magician's `LogicalChunkRunner` is intentionally outside dispatch workers. It
submits one map, repair, fallback, or reduction child at a time through the
normal queue and awaits that child before constructing the next. The runner
therefore never owns an Ollama provider permit across the logical operation.

Each child inherits priority, task reference, trace ID, cancellation, absolute
deadline, caller origin, and an idempotency key derived from logical call,
stage, level, chunk, and profile. It uses an exact profile override, explicitly
disables reasoning, and locks primary calls to Ollama. Provider and timeout
gates resolve from the concrete request profile rather than only the operation
default.

Physical and logical deadlines are separate. Profile `timeout_secs` bounds each
individual provider request; `chunking.logical_timeout_secs` bounds the complete
map/repair/reduce operation. The logical value must be positive and cannot be
shorter than an explicitly configured physical timeout. If omitted, the
physical timeout is the whole-operation deadline.

For every map chunk the runner rejects known truncation finish reasons, then
delegates parsing and semantic validation to the adapter. Recovery is bounded
to one adapter-rendered repair followed by the configured policy:

- `disabled` fails immediately;
- `same_provider_only` fails after the one local repair opportunity;
- `deterministic` accepts only adapter-produced output that passes map
  validation; and
- `mapped_profile` retries only that chunk through one explicit profile.

Cancellation and deadlines are terminal and are never hidden inside a repair
or fallback error. Validated map outputs then enter either deterministic
reduction or one or more adapter-budgeted hierarchical LLM levels.
Hierarchical levels execute sequentially, must reduce output cardinality, and
have a hard cycle guard. The adapter validates and serializes the complete
value into one ordinary `LLMResponse`.

Usage is summed once across successful physical map, repair, fallback, and
reduction responses. Queue/provider rows are authoritative for billing. The final logical event contains
`summary_incremental_cost_usd: 0`, explicitly preventing double-counting.

## Reading the shipped config

`readiness.rs` and `scripts/eval-ollama-logical-chunking.py` both resolve chunk
adapters against the shipped router profiles, which live in `llm-router.yaml`
beside `magician-config.yaml` rather than inside it. Both read through the
splice helpers — `shipped_repo_config_yaml()` in Rust,
`scripts/magician_config_text.py` in Python — because the config file alone has
no profiles and parses cleanly into an empty router, so a direct read reports
"no adapters" rather than failing. See
[router tables](router-tables-file.md).

## Adapter registry and reload safety

Built-in domain adapters live in the `magician-chunking` crate and are
registered at boot via `register_builtin_chunk_adapters()`; the lib-side
registry starts empty so it never depends on the satellite crate. Magician
owns the process adapter registry and validates enabled profile mappings
against it. Startup disables LLM routing for an enabled policy when the
adapter is not registered, does not advertise the mapped operation, does not
declare an available final validator, or the profile is not an Ollama profile.

A chunk-enabled router default is also rejected unless its adapter explicitly
advertises wildcard operation support; normal domain adapters must be selected
through explicit operation mappings.

Hot reload validation happens before router or other runtime config mutation.
Direct router reloads reject the invalid candidate and retain their last good
state; the config reload API returns a bad request before applying it.

Six registered adapters:

- `memory_episode_quality_v1`
- `memory_utility_review_v1`
- `memory_entities_v1`
- `memory_environment_v1`
- `evidence_distill_v1`
- `memory_archive_v1`

Mapped recovery cannot target another chunk-enabled profile. Child requests
carry a `logical_chunk_child` marker.

### Memory adapters

Episode quality uses one bounded episode-quality projection per logical root.
The compact output omits identities and maps exactly by runtime-owned input
order and count. The adapter normalizes the existing quality enums,
deterministically resolves duplicate split-root results, and reuses the
current deterministic episode classifier when a chunk fallback is requested.

Utility review uses one injected-memory candidate plus the current bounded run
and action-trace projection per root. It reuses the production utility parser,
normalizes invalid or missing judgements to `unknown`, and deterministically
resolves duplicate split-root results by valid label, confidence, then
canonical content. Its map request carries an exact-count JSON Schema with the
allowed utility labels and runtime-owned candidate-key enum. The model must
return rows in input order; after cardinality validation, the adapter restores
candidate identity positionally before parsing.

Entity and environment extraction use the existing bounded consolidation
episode projection. Their reducers upsert by normalized domain identity,
redact secrets, filter transient trace debris, merge reusable evidence
deterministically, and run the existing default tier validator. They fail
rather than returning partial deterministic content after repair exhaustion.

Oversized episode roots split at whole evidence fields; utility roots split at
whole action-trace entries. No adapter slices bytes, characters, JSON tokens,
or sentences. Internal source-coverage metadata exists only during validation
and reduction and is removed before the ordinary response is serialized.

### Evidence and archive adapters

Evidence distillation uses one existing bounded consolidation projection per
episode and adds bounded provenance, pending-action, source-output, and
strategy fields. An oversized episode splits only at whole evidence sections.
Map output is keyed by opaque segment identity, while the runtime stamps the
episode root, normalizes the existing `EvidenceProposal`, applies the current
salience gate, and redacts secrets. Several segment proposals for one episode
use one grounded hierarchical reduction request; independent episode proposals
merge without an extra model call. Evidence uses a 1,024-token physical output
cap.

Archive planning first sorts complete episodes chronologically, then groups by
normalized task/workflow plus UI-thread or root-execution session. Groups are
capped at six episodes and oversized groups split only between complete
episodes. Opaque reversible identities — not model prose — carry exact episode
membership and source timestamp ranges. Map and reduction parsers ignore any
model-authored membership/timestamps and stamp the runtime-owned values.

When intermediate archive entries exceed the effective payload budget, the
adapter constructs deterministic batches and asks for concise summaries through
one or more hierarchical levels. Source-root partitions must remain exact and
each later physical level must reduce cardinality. Failure outcomes cannot be
softened by a reduction response.

### Durable archive sweep checkpoints

The archive adapter preserves an all-or-nothing logical-call contract: no
incomplete map or reduction output can reach canonical memory. The batch
consolidator adds a durable bounded-snapshot transaction for
`memory_archive_summary` rules targeting the `archive` tier with
`append_period` merging and an `episodes(unprocessed=true)` cursor source:

- trigger eligibility is evaluated against the full bounded source snapshot;
- the adapter's deterministic workflow/session grouping plan is persisted
  before the first model call;
- one invocation receives one complete adapter group, capped at six episodes;
- interleaved groups may commit independently, but the source cursor advances
  only after every group in the snapshot commits;
- a cap-sized snapshot schedules a conservative follow-up snapshot, while an
  empty follow-up restores the normal interval cadence; and
- scheduled and explicitly named batch invocations share this same planner,
  retry, write, checkpoint, and cursor path.

A five-minute durable continuation lower bound prevents repeated named or
scheduler calls from spinning through Ollama's generation lane. Archive
summarization skips the separate LLM episode-quality classifier: archive
membership is unconditional, and the adapter already receives the deterministic
per-episode quality signal.

Retry guards fingerprint only the exact active group. Provider, queue,
logical-deadline, and model/schema-output failures remain retryable with
capped exponential backoff; only missing local configuration or prompt
references quarantine until configuration changes.

Archive tier entries carry an explicit runtime-owned `source_episode_ids`
field. `append_period` uses period plus those IDs as its replay key.
Lifecycle telemetry emits `memory_consolidation_microbatch_checkpointed` for
committed groups and `memory_consolidation_microbatch_drained` when the
bounded snapshot/cadence is complete. `make ollama-chunking-shadow-eval`
requires exact once-only archive episode membership and the six-episode group
bound in every local/cloud archive run.

## Readiness tooling

The offline evaluator (`scripts/eval-ollama-logical-chunking.py`) defaults to
`qwen3.8-ud2-mtp`. Use `--local-model gemma4:12b` or
`OLLAMA_CHUNK_EVAL_MODEL=gemma4:12b` to evaluate the shipped candidate.

The runtime exposes the stable registered contract at
`GET /api/magician/v2/llm/chunking/adapters`. The response contains adapter
ID/version, supported operations, and final validator availability. It cannot
enable a profile, invoke a provider, or write a result.

The service binary registers this read-only route and the `logical-chunk-eval`
subcommand. Registration does not execute the chunk runner; the CLI remains
plan-only unless `--execute` is supplied.

```bash
python3 scripts/eval-ollama-logical-chunking.py --dry-run \
  --output-dir coverage/evals/ollama-logical-chunking

python3 scripts/eval-ollama-logical-chunking.py --runs 5 \
  --output-dir coverage/evals/ollama-logical-chunking
```

Equivalent Make targets: `make ollama-chunking-readiness` and
`make ollama-chunking-shadow-eval`. The live lane receives only committed
synthetic fixtures and no memory store, repository, or persistence callback.
Reports do not retain generated content. The focused CLI records
`queue_impact_measured: false`.

The activation manifest is
[`activation-manifest-v1.yaml`](../../../data/magician_v2/llm_chunking_evals/activation-manifest-v1.yaml).
It is a versioned release/evaluation contract, not a runtime configuration
source. Magician itself loads only the normal runtime configuration surfaces.

To register another domain: implement the full `ChunkDomainAdapter` contract,
register it and advertise only the exact supported operation names, add a
disabled Ollama candidate profile without changing an operation mapping, add
synthetic fixtures and the candidate tuple to the readiness/eval inventory,
then add the operation, rollback mapping, SLOs, and observation query to a new
reviewed activation-manifest revision. Activation stays a separate phase.

## Production router

`OperationLlmRouter::generate_for_chunkable_operation_with_lazy_fallback` is
the allocation-safe production boundary for registered structured operations
(the borrowed-prompt method is a compatibility wrapper). The router
captures the configuration mapping, selected profile, chunk policy, and
matching configured-router handle under one read lock, then releases the lock
before invoking caller code or awaiting dispatch. Queue jobs and every logical
map/repair/fallback/reduce child carry that captured router authority through
pickup and retry.

A disabled chunk policy invokes the fallback producer exactly once with a
borrowed view of the logical input. An enabled policy drops the uncalled
producer before planning, then:

1. requires an explicit operation mapping and uses the global dispatch queue
   when installed; queue-less CLI/dispatch-disabled paths use the same pinned
   configured router directly;
2. derives the physical, logical, payload, output, and safety budgets from the
   selected profile;
3. clones the process adapter registry and invokes `LogicalChunkRunner` outside
   the worker (or through the bounded direct compatibility lane when no queue
   is installed);
4. carries operation priority, task identity, timeout, exact profile lock, and
   aggregate token accounting; and
5. returns the same `SimplifiedLLMResponse` consumed by existing producers.

The six integrated producer paths are memory-temperature utility review,
episode memory-quality classification, entity extraction, environment-knowledge
extraction, evidence distillation, and archive/recent-activity summarization.

Episode-quality and evidence map calls use compact positional model-wire
schemas while preserving the existing durable response contracts. Runtime
identities are never delegated to the model. The Ollama provider gives an
explicit request JSON Schema precedence over a profile's generic
`format: json`.

For consolidation producers, the structured request carries the already-
reviewed episode projections as the authoritative identity/order and model
input. Entity and environment adapters receive the owning agent tier's actual
schema rather than assuming the personal-assistant default. When an operation
is rolled back to a chunk-disabled profile, the lazy producer pretty-renders
that same projection and preserves the legacy interpolation, schema suffix,
clarification, and local-prep contracts byte-for-byte.

The archive adapter returns a source-grounded internal archive contract.
Immediately before existing tier validation, Magician deterministically
projects that result into either the historical `archive.summaries` collection
or the historical `recent_activity` object. Recent-activity actions and tool
names come from the authoritative source episodes, are bounded to the tier
limits, and are redacted before validation.

## Validation and wire safety

Router startup rejects: zero physical context; a typed Ollama context that
differs from `metadata.options.num_ctx`; enabled chunking without a physical
context, adapter, logical window, or target payload; enabled chunking on an
online provider; invalid payload/safety budgets; malformed mapped fallback
configuration.

Typed `context_window_tokens` and `chunking` data never enter provider extras.
Legacy metadata keys with those names are also stripped at the router
boundary. Requests that fit retain their existing provider request
construction.

## Observability

Every typed Ollama preflight emits a structured `llm_context_preflight` event
with profile/model, estimator, source bytes, estimated input, reserved output,
safety margin, required tokens, physical window, mode, and `would_overflow`.
Successful calls with Ollama usage also emit
`llm_context_estimator_observation`, including actual prompt tokens and signed
estimator error. These are observation events only; they are not counted as
extra provider calls or billing rows.

Design:
archived Generic Ollama Logical-context Chunking plan.
