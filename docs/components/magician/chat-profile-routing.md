# Chat model and thinking routing

Chat selects a model and a supported thinking level without changing the global
`chat_completion` assignment. Provider connections, model capabilities/limits,
workload policies and assignments are authored in the
[model catalog](../magicllm/model-catalog.md). Internal generated routes carry
the resolved dispatch settings; operators do not author profile variants.

## Catalog and admission

`GET /api/magician/v2/chat/models?engine=magician|pi` returns one descriptor per
eligible model, its native thinking choices, and `default_selection`. The
retired `/chat/profiles` endpoint is not registered. Ordinary, streaming, queued
and concurrent-voice sends use the same typed admission resolver:

```json
{
  "harness_engine": "magician",
  "model_selection": { "model_id": "openai/gpt-6-luna", "thinking": "none" }
}
```

CLI chat engines retain their native `harness_model`; they do not reinterpret a
Magician model ID. Pi exposes only catalog-qualified models. Its usable context
capacity remains omitted until the engine attests it. Unsupported engines,
models, thinking levels and competing selection fields are refused before a
turn starts. Retired `profile` fields are rejected, including when supplied
beside a valid typed choice. No missing choice is silently replaced with the
first model in the catalog.

Rich-chat admission requires compatible tool behavior. OpenAI tool-bearing,
multimodal chat uses Responses; transports pinned to incompatible Chat behavior
and forced tool-choice modes are excluded. Catalog resolver metadata is inert
and never becomes arbitrary provider payload. Bounded native text operations
retain their reviewed transport and tool-choice constraints.

## Thinking, limits and fallbacks

Auto follows the model/workload or native engine default. It does not begin Off
and escalate during the turn. Explicit levels stay fixed. The retired
fast/thinking pair state machine, injected `request_thinking_mode` tool and
retry escalation are removed. Historical event variants remain readable and
are no longer emitted. Anthropic's native adaptive request mode is a provider
capability, separate from that retired application policy. A future Decision
Engine can choose model/effort through the typed resolver.

Off appears only when the model's advertised levels include `none`. Some models
require reasoning or expose only a minimum effort; a partial provider mode is
not presented as complete Off. See the
[provider Off matrix](../magicllm/model-catalog.md#provider-off-controls).
The model catalog records limits once; shared workload policies derive bounded
output/reasoning settings within those limits. The composer shows context as
information rather than asking the user to allocate token budgets.

Error fallbacks use `assignments.*.fallback`, naming another canonical
assignment with its own model, thinking and workload constraints. The migration
preserves operation/selector contracts and fallback traversal. Error fallback
is independent of thinking escalation; it does not revive adaptive profiles.
See [fallback routing](../magicllm/model-catalog.md#fallback-routing).

Public envoy chat continues to use `llm.router.operation_mapping.kapso_envoy_chat`
when its surface matches the configured policy. The seed includes
`kapso-envoy-chat` and `telegram-envoy-chat`; bots need no model-specific changes.

## Clients and saved selections

Web Chat and VibeDev share compact anchored engine/model dropdowns and native
thinking steps. iOS and Android consume the same descriptors with native
stepped sliders or binary switches. Unsupported controls are absent. Web
lightning and dollar effects read only catalog `feedback` (`fast`, `premium`,
`standard`) across providers and engines. They use theme tokens, float above
the popup and respect reduced motion; they are not price or latency quotes.

Preferences persist model/thinking, and coding preferences also retain engine
identity. A bounded frozen v1 converter imports old saved selectors once,
then validates the exact result against the current catalog. Typed preferences
take priority. Unknown, malformed or unavailable selections remain saved and
require an explicit replacement. The old storage key is removed only after
successful typed persistence. No runtime alias table is involved.

VibeDev reads `GET /coding/models`, which separates model descriptors from
engine readiness. Requests send `coding_choice` as either a fixed
`{kind:"model", engine, model_id, thinking}` or coordinator `{kind:"auto"}`.
Coordinator Auto is distinct from a model's Auto thinking. Crew pins retain
canonical generated handles and workload identity. Packaged Architect and
Principal Software Engineer templates pin Pi's Sol/High choice; Web Researcher
pins `operation/query_analysis`. Materialized definitions require a versioned
owner update; template loading does not overwrite them. Sealed historical
execution digests are never rewritten.

All clients require the typed backend. Old profile endpoints and transport
fallbacks are absent. See migration boundaries.
