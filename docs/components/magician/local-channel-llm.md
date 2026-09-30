# Local channel LLM

Shipped Magician configs select **`privacy.processing.mode: cloud`**. Channel
Assist, memory logical-chunking, meeting summary, app-workflow, and Kapso
envoy fallback therefore run their `when_cloud` OpenAI arms
(`op-*-remote`, mostly GPT-6-Luna via Responses), not the on-device model. The
seed and template comment this explicitly: "selected by this shipped
config; the on-device generation model goes unused." Live
`$MAGICIAN_ROOT_DIR/magician-config.yaml` matches.

**One derivation, every loader.** `privacy.processing.mode` is copied into
`llm.router.locality` by `validate_and_apply_magician_config`, and every
consumer must load through that path — a raw YAML parse would keep the router
`local` while the file says `cloud`. In cloud mode
`magician.bin ollama-launch-config` answers `generation_model_count=0` and sizes
the daemon context from the embedder; `run-ollama.sh`'s fallback
`resolve-ollama-config.rb` also applies `when_cloud` arms and unmaps
`local_prep`. Neither path prewarms the local generation model in cloud mode,
and residency verification is skipped when nothing is prewarmed (the verifier
refuses an empty expected set, which under `set -e` would take down the
supervisor).

`ProcessingLocality`'s **schema default is `local`** (when the `privacy`
section is absent). Opt in with `mode: local` (or omit the section on a config
that has never written it) to pin those operations to one local generation
model: the `runtime.ollama.local_generation.selected` pin (the
`*local_generation_model` anchor; see Kitty).

- Eval script: `scripts/golden-eval-channel.py`
- Channel Assist: [mail-assist.md](mail-assist.md)

Reasoning parity is enforced at three boundaries: configuration rejects a
reasoning-enabled replacement, routing retains the typed non-reasoning
decision, and the Ollama adapter cannot be overridden by raw provider extras.

## Kitty

Auto-setup picks one tier (`36 GB+` → Qwen 27B, `24–35 GB` → Gemma 4 12B,
`16–23 GB` → Woof 4B) and writes `runtime.ollama.local_generation.selected`.
Below 16 GB, local generation is not offered. The seed/template default is
Gemma 4 until that run. Override the pin after install if you want a
different kitty model than the RAM rule.

Settings → **On-device generation** (`GET`/`PUT /api/magician/v2/settings/local-generation`)
is that override in the UI. It rewrites the same YAML anchor the installer
uses, reloads magician-config, and runs `scripts/run-ollama.sh`. If the
chosen model is outside the RAM tier, the panel warns and still lets you
switch. It does not `ollama pull` a missing model.

```bash
make setup-ollama                                      # install + pin matching tier
make setup-local-generation                            # scores + current pin
make setup-local-generation MODEL=woof-4b              # download/create only
make setup-local-generation MODEL=qwen3.8-ud2-mtp SELECT=1  # override pin
```

JSON workers keep `think: false` for every model in this table — thinking
belongs to the browser-agent path, not classify/distill.

| | Qwen 3.8 27B | Gemma 4 12B | Woof 4B |
|---|---|---|---|
| Ollama name | `qwen3.8-ud2-mtp` | `gemma4:12b` | `woof-4b` |
| Disk | 9.8 GB | 7.6 GB | 2.4 GB |
| RAM resident (steady-state) | ~11 GB | ~8 GB | ~3 GB |
| Install | GGUF + Modelfile | `ollama pull` | HF MLX + `ollama create --experimental` |
| Default on | 36 GB+ | 24–35 GB | 16–23 GB |

Catalog: `data/magician_v2/local_generation_catalog.yaml`.

## Model

| | |
|---|---|
| Ollama name | `qwen3.8-ud2-mtp` |
| Weights | Unsloth Dynamic 3.0 `Qwen3.8-27B-UD-Q2_K_XL.gguf` (~9.83 GB) |
| Source | `unsloth/Qwen3.8-27B-GGUF` |
| Resident | ~11.0 GB at `num_ctx` 32768 |
| Architecture | Qwen3.5 / Qwen3.8 dense 27B with embedded MTP (`blk.64`–`blk.68`) |

Rebuild after a fresh Ollama or a deleted model. Weights live on the SSD
outside git:

```bash
curl -L --continue-at - \
  -o /Volumes/build/magician/models/Qwen3.8-27B-UD-Q2_K_XL.gguf \
  https://huggingface.co/unsloth/Qwen3.8-27B-GGUF/resolve/main/Qwen3.8-27B-UD-Q2_K_XL.gguf
ollama create qwen3.8-ud2-mtp -f scripts/ollama-qwen38-ud2-mtp.Modelfile
```

`scripts/ollama-qwen38-ud2-mtp.Modelfile` is `FROM` that GGUF plus
`PARAMETER draft_num_predict 4`, `temperature 0.1`, and `num_ctx 32768`.
A raw `ollama create` from the GGUF without that Modelfile will not turn
MTP on.

`scripts/eval-swift-qwen38.sh` evaluates the UkisAI Swift-Qwen3.8-27B variant
(a thinking-token diet, not a drop-in replacement; its gains show only on the
`think: true` browser-agent path). It must not rewrite
`runtime.ollama.local_generation.selected`.

### Woof 4B (16–23 GB auto-pick)

Underdog Woof 1.1 is a 4-bit MLX Qwen3.5, advertised as a text-only DOM
browser executor. It is not an `ollama pull` tag. Auto-setup selects it
on 16–23 GB machines.

```bash
make setup-local-generation MODEL=woof-4b SELECT=1
```

Weights default to `/Volumes/build/magician/models/mlx/Underdog-Woof-4B-1.1`
when that volume exists, else `$MAGICIAN_ROOT_DIR/models/mlx/…`. Override
with `MAGICIAN_MODELS_DIR`. `scripts/ollama-woof-4b.Modelfile` is the
template; the setup script fills `FROM`. Needs Ollama 0.32+ (`--experimental`
safetensors / MLX import).

The managed launcher verifies the loaded context through `/api/ps` after
its prewarm request. Ollama applies `registry.ollama.ai`, `library`, and
`latest` defaults to omitted model-name parts, so the verifier
canonicalizes both identities before comparing them. Run
`make test-ollama-residency` for the provider-free contract;
`make run-ollama` performs the real installed-model check.

## What must be enabled

These apply when `privacy.processing.mode` is `local` (or the section is
absent):

1. **MTP speculative decoding.** `PARAMETER draft_num_predict 4` in the
   Ollama Modelfile (and `metadata.options.draft_num_predict: 4` on the
   Magician profiles). Without it decode is ~43 tok/s; with it ~69–80 tok/s.
2. **`think: false`.** Qwen 3.8 thinks by default. Magician's Ollama
   provider sends `think: false` when the profile has no `reasoning:`
   block. Do not add a reasoning block to these JSON workers.
3. **`format: json`** on classify, distill, and reply-draft.
4. **Daemon:** `runtime.ollama.flash_attention: true` and
   `kv_cache_type: q8_0` (already the seed).
5. **Do not shrink `num_ctx` for speed** on this hybrid model; decode speed
   does not depend on it.

Every local Ollama profile in `llm-router.yaml` takes its model from the
`*local_generation_model` anchor (seed `gemma4:12b`; `make setup-ollama`
rewrites it per RAM tier) with `metadata.options.draft_num_predict: 4` plus
`num_ctx: 32768`. JSON
workers keep `format: json` and no `reasoning:` block.
`op-app-workflow-local` sets `think: false` and `/api/chat`.

The Apple on-device model (FoundationModels via `apfel`) is not integrated; see
the evaluation record.

## Profiles

These operations map to the local generation model as the **`default:`**
(local) arm. Under shipped `mode: cloud` each resolves its `when_cloud` arm
instead:

- `channel_ingest_distill` / `resurfacing_deep_summary` → `op-channel-distill-local`
- `channel_classify` / `channel_pattern_synthesis` → `op-channel-classify-local`
- `channel_reply_draft` → `op-channel-reply-draft-local`
- `ambient_distill` / `local_prep` / `resurfacing_curate` → `op-ambient-distill-local`
- `agentic_ledger_compaction` → `op-agentic-ledger-compaction-local`
- `memory_episode_quality_classification` → `op-memory-episode-quality-local-chunked`
- `memory_temperature_utility_review` → `op-memory-utility-review-local-chunked`
- `memory_entity_extraction` → `op-memory-entity-extraction-local-chunked`
- `memory_environment_knowledge_extraction` → `op-memory-environment-extraction-local-chunked`
- `distill_evidence` → `op-memory-evidence-distillation-local-chunked`
- `memory_archive_summary` → `op-memory-archive-summary-local-chunked`
- `meeting_summary` / `meeting_response` → `op-meeting-summary-local`
- app-workflow local processing → `op-app-workflow-local`
- `kapso_envoy_chat_fallback` → `op-kapso-envoy-chat-fallback-local`

Each local profile sets `model: *local_generation_model` and
`metadata.options.draft_num_predict: 4`. Magician's Ollama provider merges
`metadata.options` into `/api/generate` and `/api/chat` `options`. JSON
workers also set `format: json` and have no `reasoning:` block so the
provider sends `think: false`. `op-app-workflow-local` sets `think: false`
explicitly because it uses `/api/chat` with tools.

Config surfaces:

- Live: `$MAGICIAN_ROOT_DIR/magician-config.yaml` (default `~/MagicianNotes`)
- Repo seed: `magician-config.yaml` (router tables in `llm-router.yaml`)

Changing a profile's model pin needs a Magician restart. Switching locality
does not (see below).

## Locality policy: the customer's local-vs-cloud switch

`privacy.processing.mode` is the single switch. Schema default is
`local`; **every shipped config writes `cloud`**.

- **`local`** — the on-device local-generation profiles serve every
  local-eligible operation (the `default:` arms above). Local arms disable
  reasoning.
- **`cloud`** — each mapping's `when_cloud` arm selects its remote
  OpenAI Responses counterpart. The six chunked arms keep
  `chunking.enabled: false`. Most remote profiles disable reasoning and
  omit a `reasoning` block, so the adapter sends `effort: none`, matching
  Ollama's `think: false`. Exceptions: `memory_archive_summary` and
  `channel_reply_draft` use GPT-6.1 Sol with `effort: low` and a 4K shared
  output budget, because that model rejects `none`.
  The local generation model goes unused in cloud mode: the generation prewarm set
  resolves empty, `OLLAMA_CONTEXT_LENGTH` is left untouched, and the model
  evicts on its existing 10-minute keep-alive. The embedding daemon on
  `127.0.0.1:11435` and its model are untouched in both modes — embeddings
  never carry a `when_cloud` arm.

Selection is runtime, not restart: `PUT /api/magician/v2/settings/privacy`
writes the section durably and triggers the standard config reload, and
the guards (`require_permitted_provider` in `llm_dispatch_seam`) verify
the mode-effective binding — Ollama under `local`, the `when_cloud` arm
under `cloud`; unbound stays OFF in both. Every guard refusal preserves
the distill backlog. Remote arms carry no Ollama-only metadata
(`format`/`options`/`keep_alive`). The pinned dispatches request
`response_format: json` on remote arms so JSON enforcement does not
depend on provider metadata.

Load-time rules under `cloud`: `llm.dispatch.local_prep` is disabled and
its mapping arm removed (there is no local model to pre-summarize with),
and `app_platform.processing.remote_processing_enabled` is *derived* from
the mode rather than set independently.

Both paths preserve the direct path's timing contracts: embedding
deadlines stay millisecond-precise and the routed health probe keeps the
10-second `health_timeout` bound.

See `docs/archive/plans/2026-08-25-llm-locality-policy-design.md` for the
full design, including the deliberately excluded items (per-domain
granularity, daemon start/stop, embedding locality).
