# Ollama Logical-context Chunking

The framework lets one structured logical operation span several bounded
Ollama calls and still return one validated response. Six producers use it;
their candidate profiles run the local generation model
(`*local_generation_model`: seed `gemma4:12b`, rewritten per RAM tier by
`make setup-ollama`) with a 32K physical context, thinking off and MTP
`draft_num_predict: 4`.

Governed app-memory adapters stay on the same physical provider boundary.
RemoteAllowed app-memory projections may enter the destination-owned
persistent index only under their current source/grant/policy partition.
LocalOnly app memory uses an ephemeral scorer only while an exact live
local-provider credential is present, reattested before and after I/O; it never
falls back to a remote embedder or a reusable chunking or reviewer sink.

Chunk telemetry follows the shared LLM observability identity contract. The
logical request owns the parent call id; every physical map, repair, mapped
fallback and reduction request gets a fresh child call id under the same root
trace with relation `chunk_map`, `chunk_repair`, `chunk_fallback` or
`chunk_reduce`.

## Context contract

`LLMProfile` has provider-neutral typed fields:

```yaml
context_window_tokens: 32768
chunking:
  enabled: true
```

| Profile shape | Preflight | Provider behavior |
|---|---|---|
| no typed context | disabled | unchanged request |
| context + `chunking.enabled: false` | observe | request still runs |
| context + `chunking.enabled: true` | enforce | physical overflow fails before dispatch |

Every candidate profile has `chunking.enabled: true` and its operation mapping
points at it, so hard preflight covers every child, and recovery is Ollama-only
`same_provider_only`. Voice context compaction keeps its own baseline mapping.

The model-catalog migration keeps these contracts in semantic workload
assignments. Candidate handles are `operation/<operation_name>`; shared fast
baselines use `operation/voice_context_compaction` or
`operation/screen_evidence_distill`. The memory-quality comparison retains
`workload/memory_episode_quality/baseline` explicitly even though no configuration
selector names it. No legacy profile alias is needed by readiness or dispatch.
The immutable v1 activation manifest keeps its historical IDs; the offline
evaluation reader translates only that manifest's recorded IDs for comparison
with current assignments. It never rewrites evidence or installs runtime aliases.

Release readiness pins the `gemma4:12b` model, 32K window, 4K output reserve,
JSON output and local endpoint; another model fails readiness until reviewed.
Only two per-operation states pass readiness and reload: dormant (candidate
disabled, baseline mapping) or activated (candidate enabled, candidate
mapping). Rollback is per operation: restore the baseline mapping in the
activation manifest and set only that candidate's `chunking.enabled` to
`false`.

## Estimation and budgets

`ConservativeOllamaEstimator` estimates one token per two UTF-8 bytes across
role markers, text and structured content, tool schemas and JSON response
schemas. It deliberately overestimates. A safety margin (2,048 tokens for chunk
policies) covers tokenizer variance. Tool-result projection uses the same
estimator, so projection admission and MagicLLM preflight cannot disagree at a
rounding boundary.

```text
estimated input + reserved output + safety margin <= physical context
```

Shared helpers compute effective payload capacity and return typed errors for
static, physical or logical overflow.

Each of the six local-memory candidates uses a 32K physical window, 256K
logical window, 24K target payload, 4K output reserve, 2K static-overhead
reserve, 2K safety margin, thinking off, JSON output, MTP
`draft_num_predict: 4`, `same_provider_only` recovery, and a 1,200-second
logical deadline on top of the per-call timeout.

## Planner

`magicllm` exposes provider-neutral logical request, budget, item, descriptor,
plan, validation and reduction types and the `ChunkDomainAdapter` boundary. The
adapter owns source extraction, semantic splitting of oversized items, request
rendering, map and final validation, bounded repair and fallback, and
reduction. A missing required hook returns a typed unavailable error.

Each source has a stable root identity and order. A split child keeps the root
identity, its parent identity and an ordered segment path. Splitting happens
only through the adapter: there is no generic byte, character, JSON-token or
prose slicing. A depth guard rejects non-progressing or cyclic splits without
capping chunk count.

The planner packs whole items first-fit in source order. Before returning, it
proves every terminal identity appears exactly once and every root is covered;
missing, unexpected or duplicate identities are typed failures, never partial
plans.

## Runner

`LogicalChunkRunner` runs outside dispatch workers. It submits one map, repair,
fallback or reduction child at a time through the normal queue and awaits it,
so it never holds an Ollama provider permit across the operation.

Each child inherits priority, task reference, trace ID, cancellation, absolute
deadline, caller origin, and an idempotency key from logical call, stage,
level, chunk and profile. It uses an exact profile override, disables
reasoning and locks primary calls to Ollama. Provider and timeout gates come
from the concrete request profile.

Profile `timeout_secs` bounds each provider request;
`chunking.logical_timeout_secs` bounds the whole operation. The logical value
must be positive and not shorter than an explicit physical timeout; if omitted,
the physical timeout is the whole deadline.

For each map chunk the runner rejects known truncation finish reasons, then
hands parsing and validation to the adapter. Recovery is one adapter-rendered
repair, then the policy:

- `disabled` fails at once;
- `same_provider_only` fails after the one local repair;
- `deterministic` accepts only adapter output that passes map validation;
- `mapped_profile` retries only that chunk through one explicit profile.

Cancellation and deadlines are terminal and never hidden in a repair or
fallback error. Validated map outputs go to deterministic reduction or to one
or more hierarchical LLM levels, which run in sequence, must reduce
cardinality and have a cycle guard. The adapter validates and serializes the
result into one ordinary `LLMResponse`.

Usage is summed once across successful physical responses. Queue and provider
rows are authoritative for billing; the final logical event carries
`summary_incremental_cost_usd: 0` to prevent double-counting.

## Reading the shipped config

`readiness.rs` and `scripts/eval-ollama-logical-chunking.py` resolve adapters
against the shipped router profiles in `llm-router.yaml`, beside
`magician-config.yaml`. Both read through the splice helpers
(`shipped_repo_config_yaml()` in Rust, `scripts/magician_config_text.py` in
Python), because the config file alone has no profiles and parses into an
empty router, which would report "no adapters" instead of failing. See
[router tables](router-tables-file.md).

## Adapter registry and reload safety

Built-in adapters live in the `magician-chunking` crate and are registered at
boot by `register_builtin_chunk_adapters()`; the lib-side registry starts empty.
Magician owns the process registry and validates enabled profile mappings
against it. Startup disables routing for an enabled policy whose adapter is not
registered, does not advertise the mapped operation, has no final validator,
or whose profile is not Ollama. A chunk-enabled router default is rejected
unless its adapter advertises wildcard support; normal adapters are selected
only through explicit operation mappings.

Hot reload validates before any router or config mutation: direct router
reloads keep their last good state, and the config reload API returns a bad
request before applying.

Registered adapters:

- `memory_episode_quality_v1`
- `memory_utility_review_v1`
- `memory_entities_v1`
- `memory_environment_v1`
- `evidence_distill_v1`
- `memory_archive_v1`

Mapped recovery cannot target another chunk-enabled profile. Child requests
carry a `logical_chunk_child` marker.

### Memory adapters

- **Episode quality** sends one bounded projection per root. Compact output
  omits identities and maps by runtime-owned input order and count. The adapter
  normalizes quality enums, resolves duplicate split-root results, and uses the
  deterministic episode classifier as chunk fallback.
- **Utility review** sends one injected-memory candidate plus the bounded run
  and action-trace projection per root. It reuses the production parser, maps
  invalid or missing judgements to `unknown`, and resolves duplicates by valid
  label, confidence, then content. The map request carries an exact-count JSON
  Schema with allowed labels and a candidate-key enum; rows return in input
  order and identity is restored by position.
- **Entity and environment** extraction use the consolidation episode
  projection. Reducers upsert by normalized identity, redact secrets, drop
  transient trace debris, merge evidence deterministically and run the default
  tier validator. They fail rather than return partial content after repair is
  exhausted.

Episode roots split at whole evidence fields; utility roots at whole
action-trace entries. No adapter slices bytes, characters, JSON tokens or
sentences. Source-coverage metadata exists only during validation
and reduction and is removed before serialization.

### Evidence and archive adapters

Evidence distillation sends one consolidation projection per episode plus
bounded provenance, pending-action, source-output and strategy fields, split
only at whole evidence sections. Map output is keyed by opaque segment
identity; the runtime stamps the root, normalizes the `EvidenceProposal`,
applies the salience gate and redacts secrets. Several segment proposals for
one episode use one grounded reduction request; separate episodes merge
without a model call. Physical output is capped at 1,024 tokens.

Archive planning sorts complete episodes by time, then groups by normalized
task or workflow plus UI-thread or root-execution session, at most six per
group, splitting only between episodes. Opaque reversible identities carry
membership and timestamp ranges; parsers ignore model-written membership and
stamp runtime values. When intermediate entries exceed the payload budget, the
adapter batches them deterministically through hierarchical levels. Root
partitions stay exact, each level must reduce cardinality, and a reduction
cannot soften a failure.

### Durable archive sweep checkpoints

No incomplete map or reduction output can reach canonical memory. The batch
consolidator adds a durable bounded-snapshot transaction for
`memory_archive_summary` rules targeting the `archive` tier with
`append_period` merging and an `episodes(unprocessed=true)` cursor:

- trigger eligibility uses the full bounded snapshot;
- the grouping plan is persisted before the first model call;
- one invocation gets one complete group (at most six episodes);
- groups may commit independently, but the cursor advances only after every
  group in the snapshot commits;
- a cap-sized snapshot schedules a follow-up; an empty follow-up restores the
  normal interval;
- scheduled and named invocations share one planner, retry, write, checkpoint
  and cursor path.

A five-minute durable continuation floor stops repeated calls from spinning
Ollama's generation lane. Archive summarization skips the LLM episode-quality
classifier, since membership is unconditional and the adapter already gets the
deterministic quality signal.

Retry guards fingerprint only the active group. Provider, queue, deadline and
output failures retry with capped exponential backoff; only missing local
configuration or prompt references quarantine until config changes.

Archive entries carry a runtime-owned `source_episode_ids` field, and
`append_period` uses period plus those IDs as its replay key. Telemetry emits
`memory_consolidation_microbatch_checkpointed` per committed group and
`memory_consolidation_microbatch_drained` when the snapshot is done.
`make ollama-chunking-shadow-eval` requires exact once-only membership and the
six-episode bound in every archive run.

## Readiness tooling

The offline evaluator (`scripts/eval-ollama-logical-chunking.py`) defaults to
`qwen3.8-ud2-mtp`. Use `--local-model gemma4:12b` or
`OLLAMA_CHUNK_EVAL_MODEL=gemma4:12b` for the shipped candidate.

`GET /api/magician/v2/llm/chunking/adapters` returns the registered contract:
adapter ID and version, supported operations and final validator availability.
It cannot enable a profile, call a provider or write a result. The service
binary also registers the `logical-chunk-eval` subcommand, which is plan-only
unless `--execute` is given.

```bash
python3 scripts/eval-ollama-logical-chunking.py --dry-run \
  --output-dir coverage/evals/ollama-logical-chunking

python3 scripts/eval-ollama-logical-chunking.py --runs 5 \
  --output-dir coverage/evals/ollama-logical-chunking
```

Make targets: `make ollama-chunking-readiness` and
`make ollama-chunking-shadow-eval` (preferred, for build-cache routing). The
live lane gets only committed synthetic fixtures and no memory store,
repository or persistence callback. Reports keep no generated content; the CLI
records `queue_impact_measured: false`. The evaluator honors
`CARGO_TARGET_DIR`, else the repository's `target` path; it does not assume
`/Volumes/build` exists.

The activation manifest is
[`activation-manifest-v1.yaml`](../../../data/magician_v2/llm_chunking_evals/activation-manifest-v1.yaml),
a versioned release and eval contract, not runtime configuration.

To add a domain: implement the full `ChunkDomainAdapter`, register it with only
its exact operation names, add a disabled Ollama candidate profile without
changing a mapping, add synthetic fixtures and the candidate to the readiness
inventory, then add the operation, rollback mapping, SLOs and observation query
to a new reviewed activation-manifest revision. Activation is a separate step.

## Production router

`OperationLlmRouter::generate_for_chunkable_operation_with_lazy_fallback` is
the production boundary for registered structured operations (the
borrowed-prompt method is a wrapper). It captures the mapping, profile, chunk
policy and configured-router handle under one read lock and releases it before
running caller code or awaiting dispatch. Queue jobs and every child carry that
captured router through pickup and retry.

A disabled policy calls the fallback producer exactly once with a borrowed view
of the input. An enabled policy drops the producer, then:

1. requires an explicit operation mapping and uses the global dispatch queue
   when installed (queue-less CLI and dispatch-disabled paths use the same
   pinned router directly);
2. derives physical, logical, payload, output and safety budgets from the
   profile;
3. runs `LogicalChunkRunner` with a clone of the adapter registry outside the
   worker (or through the bounded direct lane without a queue);
4. carries priority, task identity, timeout, the profile lock and aggregate
   token accounting;
5. returns the same `SimplifiedLLMResponse` existing producers consume.

The six producers are memory-temperature utility review, episode
memory-quality classification, entity extraction, environment-knowledge
extraction, evidence distillation, and archive/recent-activity summarization.

Episode-quality and evidence map calls use compact positional wire schemas but
keep the durable response contracts; runtime identities are never left to the
model. The Ollama provider gives an explicit request JSON Schema precedence
over a profile's `format: json`.

Consolidation producers send the reviewed episode projections as the
authoritative identity, order and input. Entity and environment adapters get
the owning agent tier's real schema. When an operation is rolled back to a
chunk-disabled profile, the lazy producer renders the same projection and
keeps the legacy interpolation, schema suffix, clarification and local-prep
contracts byte-for-byte.

The archive adapter returns an internal source-grounded contract that Magician
deterministically projects, just before tier validation, into the `archive.summaries` collection
or the `recent_activity` object. Recent-activity actions and tool names come
from the source episodes, bounded to tier limits and redacted.

## Validation and wire safety

Router startup rejects: zero physical context; a typed Ollama context that
differs from `metadata.options.num_ctx`; enabled chunking without a physical
context, adapter, logical window or target payload; enabled chunking on an
online provider; invalid payload or safety budgets; malformed mapped fallback.

Typed `context_window_tokens` and `chunking` never enter provider extras, and
legacy metadata keys with those names are stripped at the router. Requests
that fit are built as before.

## Observability

Every typed Ollama preflight emits `llm_context_preflight` with profile, model,
estimator, source bytes, estimated input, reserved output, safety margin,
required tokens, physical window, mode and `would_overflow`. Successful calls
with Ollama usage also emit `llm_context_estimator_observation` with actual
prompt tokens and signed estimator error. Neither counts as a provider call or
billing row.

Design:
archived Generic Ollama Logical-context Chunking plan.

Conversation memory projections keep complete sanitized source parts. Memory,
evidence and archive adapter version 1.1.0 split oversized conversation
evidence while keeping role, source revision, root membership and Unicode
offsets; retries keep the same observation identity. Compatibility excerpts
are separate evidence children. Activation and profile gates are unchanged.
See [conversational recall](memory-recall.md).
