# Model catalog and thinking selection

Generation configuration lives in `llm-router.yaml` under `llm.router.catalog`.
It is spliced into the main YAML before parsing so the local model can use the
`runtime` section's `*local_generation_model` anchor. Users select a model and
thinking level; concrete `LLMProfile` objects are generated dispatch contracts.

Native provider connections declare conservative `metadata.model_family` groups
for independent Quality review. The family is local routing metadata, not a
provider parameter. Dynamic local-runner models without a declared family remain
ineligible for bounded Quality admission until their identity and pricing qualify.

## Configuration

`openai/gpt-6.1-terra` uses Responses for tools and supports thinking `none`
through `max` (default `medium`); its standard/cache/long-context rates live in
`magician_data_v3/llm_pricing.template.json`, with deployment overrides in
`<MAGICIAN_ROOT_DIR>/llm_pricing.json`. See the
[OpenAI model contract](https://developers.openai.com/api/docs/models/gpt-6.1-terra).

| Section | Responsibility |
| --- | --- |
| `connections` | Provider, credential environment-variable name, endpoint, optional account output ceiling and transport defaults |
| `models` | Model ID, label, physical limits, capabilities, accepted thinking controls, request shape (`wire`) and optional Pi qualification (`coding: true`) |
| `policies` | Shared output allowance, timeout, answer reserve, optional default effort and context admission ceiling |
| `assignments` | Model, thinking, optional policy, capability restrictions and operation-owned tool/format/chunking requirements |

A model declares its steps and limits once:

```yaml
thinking:
  mode: effort
  levels: [low, medium, high, xhigh, max]
  default: medium
limits:
  context_window_tokens: 1050000
  min_output_tokens: 16
  max_output_tokens: 128000
  shared_context_window: true
```

Optional `default_output_tokens` uses the workload policy when absent.
`max_input_tokens` expresses a separate provider input ceiling. Model output
capacity, account limits and workload defaults are separate: setting a ceiling
does not require the model to emit that many tokens. Opaque harness defaults
can omit `limits`; clients display no invented context capacity for them.

## Thinking and request resolution

- `effort`: ordered native levels become discrete slider steps. Only qualified
  Off controls contain `none`.
- `toggle`: the native choices are `none` and `on`.
- `budget`: legacy adapters derive token budgets from the chosen level and
  actual output allowance. Their `budget` declares `min_tokens`, `max_tokens`
  and `step_tokens`; assignments need no reasoning-token number.
- `none`: the model has no adjustable control. It may still reason internally.

`auto` resolves workload/model defaults directly. Explicit choices keep their
level; there is no injected escalation tool or Off-first retry policy. Changing
effort preserves transport. The runtime has no adaptive-profile pairs or alias resolver.

Unsupported explicit levels, impossible legacy budgets, cross-provider
connection overrides and output allowances outside model/account limits fail
before I/O. Explicit caller bounds are never expanded. Smaller caller output
allowances receive newly derived legacy thinking budgets with answer reserve.
Resolver metadata never reaches provider payloads. `model_family` is one of the
router-only keys `route`/`route_stream` strip from the request extra right
before dispatch, so a caller that copies a profile's params into the extra
(chat streaming) cannot forward it; OpenAI rejects it as an unknown parameter.
Internal reasoning strategies
and explicit reasoning-token budgets are not OpenAI Responses fields; its adapter
sends supported effort and summary fields for sync, streaming and fallback calls.
See the [official reasoning contract](https://developers.openai.com/api/docs/guides/reasoning).
A model's ordinary `auto`
tool default permits an explicit caller tool requirement; operation-owned tool
restrictions remain authoritative.

## Request shape (`wire`)

Adapters shape a catalogued model's request from catalog data, not its name:
the router sets `LLMRequest.model_wire` (never serialized) on every attempt,
fallback hops included.

```yaml
wire: {sampling: false, thinking_off_effort: low}
```

| Fact | Source | Effect |
| --- | --- | --- |
| Thinking shape | `wire.thinking` (`adaptive` / `budget`), else `thinking.mode`: `effort` → `adaptive`, `budget`/`toggle` → `budget` | Claude: `thinking.type: adaptive` + `output_config.effort`, or `enabled` + `budget_tokens` |
| Off accepted | `thinking.levels` contains `none` | Claude: `thinking.type: disabled`; otherwise Off fails before I/O |
| `wire.thinking_off_effort` | Effort level; requires a `none` level | Sent with Off for models that reject Off at an inherited `xhigh`/`max` |
| `wire.sampling` (default `true`) | `false`: the model rejects sampling | Claude, Gemini and OpenAI Chat/Responses omit `temperature`/`top_p` |

An uncatalogued id has no `model_wire` and falls back to each adapter's
model-name rules. `shipped_wire_facts_match_the_name_rules_they_replaced` pins
the seed.

## Migration and verification

```sh
python3 scripts/migrate_llm_catalog.py --config /old/magician-config.yaml --output-dir /new
make cargo ARGS="run -p magicllm --example inspect_model_catalog -- /new/magician-config.yaml"
```

The first converter reads historical authored profiles and writes an intermediate
catalog without changing its inputs. `scripts/retire_llm_profile_config.py` then
rewrites configuration selectors and app trust bindings together, removes aliases
and authored coding rows, retains reachable assignment/fallback chains, and
accepts semantic workload renames. Use `--keep-assignment` for source-owned
workloads, such as the memory-quality shadow baseline, that are not config roots.
The emitted pair preserves operation request/fallback contracts, comments,
anchors, enum tags and unrelated settings. Repository and live configurations
must be converted separately because their selected local models can differ.

Runtime YAML rejects `llm.router.profiles`, `llm.router.adaptive_profiles`,
`catalog.aliases` and `coding.profiles`, and requires a nonempty model catalog.
The Rust runtime no longer contains adaptive composite types or resolution.
Existing saved choices are converted once by the offline owner/client migration;
sealed historical records remain unchanged. Activation also requires matching
clients, owned-pin migration and a qualified backend; a config preview alone
never authorizes that switch. See state ownership and recovery.

`scripts/magician_config_text.py` generates a read-only dispatch view for
setup/evaluation readers. It rejects retired authored profile tables. Production
uses the Rust resolver. The inspector
accepts an optional second output path for route JSON used in parity checks.
Unrelated main-config writers omit the catalog, concrete profiles, generated
routes and operation mappings; ownership stays with the sibling file.

## Fallback routing

The resolver generates internal `LLMProfile` dispatch contracts from the catalog.
Users do not author token/effort profile variants. Fallback traversal uses canonical
assignment references and requires no legacy aliases.

Fallbacks are explicit assignment references: `assignments.*.fallback` names
another assignment. The target owns its model, thinking, workload policy and
operation restrictions; token allowances are resolved normally. Target existence
and acyclic chains are validated before dispatch. Generated model choices do not
inherit an operation's fallback permission.

```yaml
assignments:
  extraction:
    model: openai/gpt-6-luna
    thinking: low
    fallback: extraction_retry
  extraction_retry:
    model: openai/gpt-6.1-sol
    thinking: medium
    policy: background
```

The migrator moves old `metadata.fallback_profile` values to `fallback` and
resolves their targets to canonical assignment IDs. It rejects conflicting
old/new declarations, unknown targets and cycles. New catalog metadata rejects
`fallback_profile`; old-config reading belongs to the migration tooling. Resolved
internal requests translate the typed fallback policy to the dispatcher's private
route metadata. Authored configuration and client selections contain no legacy
fallback-profile names or aliases.

Fallback is an error path for eligible unpinned non-streaming calls. An exact
profile/model route pin blocks switching; a provider pin blocks crossing providers,
and a physically resource-bound call permits one attempt. The streaming router
does not traverse fallback chains. Capability checks, per-request limits, loop
detection and cold continuation state apply to fallback attempts.

## Client contract

`GET /chat/models?engine=magician|pi` returns one descriptor per eligible model
plus `default_selection`. Model descriptors publish opaque `choices` handles;
requests carry `model_selection` and an explicit engine. The retired profile
endpoint and profile request field are absent from the backend. Web, iOS and Android show model selection,
a stepped thinking control or switch, and read-only context information.

`GET /llm/routing` includes the model catalog and each route's selection.
`PUT /llm/routing/{operation}` accepts
`{"selection":{"model_id":"…","thinking":"auto"}}`; retired `profile`
bodies are rejected. Background Auto resolves to a concrete default route. Descriptors also publish
`operation_choices` handles for agent pins; the crew editors use those handles
and offer Follow global routing. Current pins use canonical model/thinking
selections; unavailable explicit saved choices remain visible and require an
explicit replacement instead of silently resetting.
Parent/pinned rules and local/cloud precedence continue to apply. CLI models
remain available for operation assignment with installation status.

`GET /coding/models` returns qualified models, engine readiness and the default
selection. Each model marked `coding: true` generates Pi choices for its supported
native levels. Requests carry `coding_choice` with kind, engine, model ID and
thinking, or `{kind: auto}` for the configured default. The handles belong to coding admission and differ from chat
handles. Web VibeDev, the `@vibedev` composer and native new-run sheets use
these descriptors, as do coding pins in all three crew editors. Coding-engine
readiness remains independent of the model catalog.

Native coding CLIs derive their model choices from the same catalog connections
(`harness-claude_code`, `harness-codex`, `harness-grok`, `harness-agy`). Optional
`coding_thinking` stores their ordered native effort values and default once per
model, independently of one-shot provider thinking controls. These fields and
limits are resolver metadata, never arbitrary provider request fields. Native
choices inherit an eligible engine's existing readiness and billing definition;
model and effort participate in the sealed definition digest and reach the CLI
as separate arguments. Default placeholders retain the owner's configured model.
The chat engine roster and VibeDev now read this shared set of model names.
For a CLI's Default model, Auto preserves the engine owner's effort setting;
clients show Engine default because its current value is not attested.

New models need one qualified catalog record, with no authored effort variants.
The owned configuration loader rejects `coding.profiles`. Generated
coding rows participate in run admission and pin validation but are omitted
from serialized configuration. Migration moves the global coding default to
the same model's Auto choice; saved explicit selectors retain their native
effort. Pi Auto selects the model default for the run. Pi's SDK owns its request
token allowances; Magician passes model and thinking without token profiles.
Coding descriptors omit context limits because Pi's SDK can impose a narrower
window than the provider API catalog; the picker does not advertise an
unattested engine capacity.

Limits and accepted levels are versioned, reviewed catalog data. Providers do
not expose prices or controls through a common discovery API, so metadata is not
refreshed automatically. Add a model once, qualify its controls and update that catalog record
when its provider changes. This avoids one configuration per output/effort
combination. Existing context preflight/chunking rules still apply; the catalog
does not silently truncate conversation input or claim an exact tokenizer count.

## Runtime availability

Availability is runtime; the catalog stays the metadata (prices, `wire`,
limits, capabilities). `magicllm::model_discovery` caches, per source (a
connection's provider, or `provider@host` with its own base URL), the models
the source offers now:

- provider model-list endpoints where a key is set (OpenAI, Anthropic, Gemini,
  xAI, DeepSeek, OpenRouter, OpenAI-compatible base URLs, Ollama `/api/tags`),
  every 6 hours;
- harness CLIs: `agy models`, `grok models` (Magician's Grok home), Codex's own
  `models_cache.json`, every 15 minutes; Claude Code has no listing;
- an error that refuses a model by name (coding runs, plane one-shot turns, the
  harness provider): that model is hidden for up to 6 hours and the source is
  refreshed early; accepted ids named in the error become its listing.

The `coding_engine::model_discovery` worker (started at boot) does the fetching
in the background; a failed refresh keeps the last good list, and request
handlers only read the cache. Effects: a catalogued model its source no longer
offers carries `availability.state: "unavailable"` and pickers hide it (`unknown`
until a listing exists; `default` is always available); a model a native CLI
offers that the catalog lacks becomes an uncatalogued coding choice
(`availability.catalogued: false`, the CLI's default request shape, Auto only,
no price). `/coding/models` and `/llm/routing` add `model_sources` and
`catalog_drift`; `/llm/routing` also lists `uncatalogued_models` (shown, not
routable). Each drifted source raises one Attention notice per scope
(`service_health`, `health_issue: model_catalog_drift`) naming missing and new
models; it resolves when the catalog catches up and stays dismissed.

Provider references:
[OpenAI](https://developers.openai.com/api/docs/guides/reasoning),
[Claude](https://platform.claude.com/docs/en/build-with-claude/extended-thinking),
[Gemini](https://ai.google.dev/gemini-api/docs/generate-content/thinking),
[DeepSeek](https://api-docs.deepseek.com/guides/thinking_mode/),
[Ollama](https://docs.ollama.com/capabilities/thinking),
[OpenRouter](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens).

## Provider Off controls

The catalog and provider HTTP fixtures define the accepted Off controls.
Unsupported Off choices are rejected before I/O. Requalify a model record
when its provider contract changes.

| Provider/model | Off control |
| --- | --- |
| OpenAI Luna and qualified 5.6 models | `reasoning.effort: none` (Responses), `reasoning_effort: none` (Chat) |
| OpenAI GPT-6.1 Sol / GPT-6 Astra | No Off; Low is the lowest supported level |
| Claude Haiku 4.5 / Opus 4.7 / Opus 5 / Haiku 5.5 | `thinking.type: disabled`; Opus 5 and Haiku 5.5 also get Low effort (`wire.thinking_off_effort`; they reject Off at `xhigh`/`max`) |
| Claude Fable 5.1 / Opus 5.5 | Mandatory reasoning; no Off |
| Claude Sonnet 5.5 | No complete Off; native `between_tools` only removes up-front thinking |
| Gemini 4 Argon / 3.1 Pro / 3.5 Flash-Lite | No Off; Minimal on Flash-Lite is not an Off guarantee |
| DeepSeek Flash | `thinking.type: disabled` |
| MiniMax M3 | `thinking.type: disabled` |
| MiniMax M3.1 Flash | Mandatory thinking; Low, Medium, High, Extra High and Max effort; Auto uses Max |
| Sarvam 105B | Explicit JSON `reasoning_effort: null`; Auto retains configured Low |
| xAI Grok 4.7 | Mandatory reasoning; Low is the lowest supported level |
| Qualified local Ollama models | Boolean `think: false`; embedding models have no thinking control |
| Native CLI engines | Only qualified native effort flags are exposed; API Off support does not imply a CLI Off flag |

Claude's native adaptive request mode remains supported independently of the
retired Magician adaptive profile-pair feature.

MiniMax M3.1 Flash replaces M2.7 in the selectable catalog. Its OpenAI-compatible
transport sends `reasoning_effort` and retains separate `reasoning_content` in
both plain and streamed responses. MiniMax streaming always requests usage,
including on plain calls delivered internally as streams, so token/cache counts
and spend are metered. M3 keeps its binary thinking control.
The M3.1 record uses a 1M shared context window and a conservative 65,536-token
output cap; MiniMax documents the context size but does not publish a separate
output ceiling in its SDK reference. Thinking and visible output share that cap.
Its flat rates are $0.15 input, $0.60 output and $0.015 cached input per million
tokens, with no M3-style long-context multiplier. M2.7's historical pricing rows
remain available for previously recorded usage. See
[MiniMax model controls](https://platform.minimax.io/docs/api-reference/text-openai-api)
and [pricing](https://platform.minimax.io/docs/pricing/overview).

`models.*.feedback` is `fast`, `standard` (default), or `premium`. Both Web
composers read this hint for lightning/dollar feedback across all providers and
native engines. Unknown records default to Standard; it never reaches provider
payloads and does not represent measured latency or current prices.

References: [Claude thinking support](https://platform.claude.com/docs/en/build-with-claude/thinking-troubleshooting),
[Sarvam nullable Off](https://docs.sarvam.ai/api/api-guides-tutorials/chat-completion/how-to/adjust-the-models-thinking-level),
[MiniMax thinking control](https://platform.minimax.io/docs/api-reference/text-openai-api),
[Grok reasoning](https://docs.x.ai/developers/model-capabilities/text/reasoning).
