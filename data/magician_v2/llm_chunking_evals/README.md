# Logical-context chunking eval data

This directory contains inert, synthetic inputs and baseline evidence for the
generic Ollama logical-context chunking plan. The implementation and live-eval
target is the locally served `qwen3.8-ud2-mtp` model through the Ollama provider;
the recorded OpenAI profiles are only the pre-migration production comparison
and optional policy-permitted canary fallback. Nothing here changes operation
routing or invokes an LLM.

## Phase 0 artifacts

- `phase0-routing-baseline-v1.json` freezes the six candidate operations, their
  planned local profiles, current cloud comparison/retry profiles, prompt
  versions, the actual Ollama target, and relevant configuration fingerprints
  as observed on 2026-07-14. It does not make the cloud profiles targets of the
  new path.
- `fixtures/consolidation-sources-v1.json` defines sanitized source templates
  and deterministic expansion recipes for bounded, multi-chunk, oversized
  single-item, near-logical-limit, and over-logical-limit inputs.
- `consolidation-generation-cases-v1.json` defines generation-quality cases.
  These are intentionally separate from the retrieval/lifecycle suites under
  `data/magician_v2/memory_evals/`.
- `phase0-telemetry-baseline-v1.json` records a read-only snapshot of existing
  cloud dispatch Parquet data for before/after comparison. It is not a live
  evaluation run and does not select a provider.
- `baseline-results.schema.json` is the durable result contract for later
  Ollama-first results and non-authoritative local/cloud comparisons.

The existing-telemetry snapshot can be reproduced without provider calls:

```bash
python3 scripts/capture-ollama-chunking-phase0-baseline.py \
  --from-date 2026-07-08 \
  --to-date 2026-07-14
```

The command reads `llm_dispatch` Parquet and pricing data only. It prints to
stdout unless `--output` is explicitly supplied.

## Compact fixture expansion

Large synthetic fixtures are committed as deterministic recipes instead of
hundreds of kilobytes of repeated prose. A scenario selects one or more
templates, assigns stable source IDs, and pads only the named untrusted text
field using the declared synthetic padding sentence. Until Phase 1 supplies a
model-aware estimator, `chars_div_4_ceiling_v1` means:

```text
estimated_tokens = ceil(UTF-8 character count / 4)
```

Expansion must be deterministic for `(suite_version, scenario_id)`. It must
never read live episodes, prompts, memory tiers, credentials, or user content.
The expanded source-ID set is authoritative for later completeness checks.

## Safety

All names, projects, paths, URLs, identifiers, and secrets are invented. Values
with names such as `SYNTHETIC_SECRET_DO_NOT_EMIT` are deliberate redaction
traps, not credentials. They must never appear in accepted model output.

No normal test command should make provider calls from these fixtures. Later
live evaluation must require the repository's explicit live-eval flag and must
write reports to the configured coverage/report root.

## Phase 7 activation verification reports

`scripts/eval-ollama-logical-chunking.py` expands the committed synthetic
episode templates into one safe case for each registered adapter. `--dry-run`
loads all three config surfaces and invokes the Rust adapter registry plus the
real generic planner without provider calls. The default live mode explicitly
runs both the local Ollama candidates and the current cloud comparison profiles,
defaulting to five repeats per case/provider; `--local-only` suppresses online
comparison calls.

For a quick like-for-like model smoke comparison, `--compact-smoke` keeps one
copy of every committed episode template and removes only the large synthetic
padding. It still exercises the real adapter prompts, schemas, repair path, and
golden checks. `scripts/ollama-llama-server-bridge.py` can expose either a GGUF
loaded by llama-server or an MLX model loaded by an OpenAI-compatible server
such as `mlx_lm.server` through the evaluator's expected Ollama
`/api/generate` contract. Select the latter with `--upstream-kind openai-chat`
and identify its model name or local directory with `--upstream-model`;
`--served-model-label` records the actual candidate without changing the
production-model config assertion. MLX-LM is deliberately marked
`prompt_constrained`: stock `mlx_lm.server` has no equivalent of Ollama's
`format` grammar, so the bridge keeps the production schema instructions in
the prompt but does not pretend they were decoder-enforced. Compare two
detailed reports with `scripts/compare-ollama-model-evals.py`. This compact lane is a fast
compatibility/quality gate, not a substitute for the padded multi-chunk suite
and repeated release evaluation.

For runtime comparisons of the same weights, the bridge can also pass through
to native Ollama with `--upstream-kind ollama`. `--metrics-jsonl` stores only
prompt/generation token counts and durations plus request wall time. Ollama and
llama-server expose native phase timings. For MLX-LM the bridge internally asks
for an OpenAI stream, collapses it back into the one non-streaming Ollama reply
expected by Magician, and records observed TTFT plus the post-first-token stream
window. MLX prompt/decode throughput is therefore an explicitly labelled
estimate, not native backend phase telemetry. MLX-LM also owns context capacity
and model lifetime at server startup: Ollama's per-request `num_ctx` and
`keep_alive` fields cannot be reproduced. The bridge records the requested
context but does not silently claim to apply it. Set
`LOCAL_CHUNK_EVAL_CONTEXT_TOKENS` to the actual MLX server/model limit so the
saved report makes the runtime control explicit. Ollama `think: false` is
forwarded as Qwen's best-effort `enable_thinking: false` chat-template argument;
reports distinguish that from Ollama's native reasoning flag.
`scripts/compare-local-runtime-evals.py` joins those records to the detailed
quality reports so output length and failed golden checks remain visible beside
tokens-per-second measurements. It accepts any two saved lanes (Ollama,
llama-server, or MLX-LM); runs do not need to be resident concurrently.

An MLX-LM lane uses three processes while leaving Magician's production Ollama
provider untouched:

```sh
# Terminal 1: stock MLX-LM OpenAI-compatible server.
/Volumes/build/magician/tools/mlx-lm/bin/mlx_lm.server \
  --model /Volumes/build/magician/models/mlx/Ternary-Bonsai-27B-mlx-2bit \
  --host 127.0.0.1 --port 18080 --temp 0 --max-tokens 4096

# Terminal 2: evaluation-only compatibility boundary. It must listen at the
# Ollama URL selected by the isolated eval config (11434 by default).
python3 scripts/ollama-llama-server-bridge.py \
  --host 127.0.0.1 --port 11434 \
  --upstream http://127.0.0.1:18080/v1/chat/completions \
  --upstream-kind openai-chat \
  --upstream-model /Volumes/build/magician/models/mlx/Ternary-Bonsai-27B-mlx-2bit \
  --served-model qwen-27b-bonsai-1.75 \
  --metrics-jsonl coverage/evals/mlx-bonsai/metrics.jsonl

# Terminal 3: real adapters/fixtures, with honest runtime metadata in report.json.
# Replace 32768 with config.json's max_position_embeddings when it differs.
make ollama-chunking-shadow-eval \
  OLLAMA_CHUNK_EVAL_MODEL=qwen-27b-bonsai-1.75 \
  OLLAMA_CHUNK_EVAL_REPORT_DIR=coverage/evals/mlx-bonsai \
  LOCAL_CHUNK_EVAL_RUNTIME=mlx-lm \
  LOCAL_CHUNK_EVAL_CONTEXT_TOKENS=32768 \
  LOCAL_CHUNK_EVAL_SERVED_MODEL_LABEL=prism-ml/Ternary-Bonsai-27B-mlx-2bit
```

After running any other lane into its own directory, compare the saved artifacts
without keeping either runtime loaded:

```sh
python3 scripts/compare-local-runtime-evals.py \
  --left-report coverage/evals/gemma-ollama/report.json \
  --left-metrics coverage/evals/gemma-ollama/metrics.jsonl \
  --left-label "Gemma 4 12B · Ollama" \
  --right-report coverage/evals/mlx-bonsai/report.json \
  --right-metrics coverage/evals/mlx-bonsai/metrics.jsonl \
  --right-label "Bonsai 27B · MLX-LM" \
  --output-dir coverage/evals/ollama-vs-mlx \
  --title "Ollama vs MLX-LM"
```

Changing only the two left/right artifact sets supports Ollama/Ollama,
llama-server/llama-server, MLX/MLX, and every cross-runtime pairing. No
model-specific MLX shim is needed beyond this shared compatibility bridge.

Stop native Ollama or bind the isolated eval config/bridge to a different free
port before using the default `11434`; the bridge is not a production daemon.
Run `python3 scripts/ollama-llama-server-bridge.py --self-test` before a live
lane to verify translation and streaming telemetry without loading a model.

The evaluator writes `fixture-suite.json`, `runner-report.json`, `report.json`,
`results.jsonl`, and a clickable `report.html` beneath the requested output
directory. Live output bodies are never stored; JSON/HTML contain only hashes,
top-level schema keys, validity verdicts, latency, token/chunk telemetry, and
the frozen Phase 0 comparison. The runner has no persistence dependency and
reports `durable_writes: 0`.

The exact activation/rollback state is versioned in
`activation-manifest-v1.yaml`. It is a release/evaluation contract, not a
runtime config file: the readiness evaluator checks that its candidate values,
configured and rollback mappings, SLOs, evidence, canary scope, and approval
state agree with the release being qualified. The service itself loads the
repository-seed/live `magician-config.yaml` surfaces and never loads this
manifest. String-valued YAML states such as `reasoning: "off"` stay quoted so
YAML 1.1 readers cannot reinterpret them as booleans.

The production cutover is configured, while approval, canary evidence, and
rollback verification remain open. The evaluator itself still has no
persistence handle and cannot write authoritative memory.
