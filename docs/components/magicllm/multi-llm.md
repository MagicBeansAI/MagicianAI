# magicllm: Multi-Provider Router

Generation configuration is authored through the
[model catalog](model-catalog.md). References to profiles below describe
generated dispatch contracts. Operators declare model capabilities once rather
than authoring model/effort/output combinations; runtime aliases are rejected. DeepSeek Low now reaches
`output_config.effort: low`; qualified Ollama native effort models send a named
`think` level, while binary models retain true/false.

## Purpose

`magicllm` is Magician's shared LLM crate: provider-agnostic request/response
types, capability metadata, operation routing via `MultiLLMRouter`, and
bootstrap helpers that build providers from config. Out of scope: `magictunnel`,
MCP proxy workflows, consumer-mode routing branches.

## Main Modules

- `types.rs`: request/response/message/tool payload types
- `provider.rs`: provider trait (`invoke`, `capabilities`, …)
- `router.rs`: `MultiLLMRouter` (profile + operation mapping + fallback)
- `bootstrap.rs`: `ConfiguredRouter`, which builds and registers providers
- `config.rs`: `LLMRouterConfig`, `LLMProfile`, `LlmConfig`

Only providers referenced by a configured profile are instantiated.

## Supported Providers

`openai_responses`, `openai_chat`, `anthropic_messages`, `gemini`, `minimax`,
`deepseek`, `openrouter`, `ollama`, `yutori_n1`, plus:

- **GPT-6.1 Sol** (`gpt-6.1-sol`): Responses for tool calls; reasoning at least
  `low` (`none`/`minimal` unsupported; migrated fast profiles set `low`). Above
  272,000 input tokens the whole request bills 2x input/cache, 1.5x output
  (`make test-gpt61-sol`).
- **Gemini 4 Argon** (`gemini-4-argon`, `GEMINI_API_KEY`): selectable
  `chat-gemini4-argon-vision-toolsauto-r{low,medium,high}` profiles and
  `gemini4-argon-vision-toolsany-rhigh` use the `generateContent` adapter (streamed
  over `:streamGenerateContent?alt=sse`, plain calls included), vision, function calling, a shared 1,048,576-token
  context and a 32,768-token output cap (the limit reported by our API model
  lookup on 2026-10-15, matching [Google's non-streaming REST limit](https://ai.google.dev/gemini-api/docs/generate-content/latest-model)).
  Timeout: 900 seconds. Both GenerateContent and
  Interactions map reasoning effort to thinking levels; absent/disabled
  reasoning uses `low`, as with Pro. Sampling parameters and explicit
  `cachedContent` references are omitted for Gemini 4; implicit caching is
  provider-managed. GenerateContent usage includes `thoughtsTokenCount` in
  billable completion tokens and exposes it as the reasoning subset.
  Pricing is $2/$10 per million input/output tokens with $0.10 cached reads
  through 2026-11-30, then $4/$20 with $0.20 cached reads, effective-dated in
  the pricing file. Existing operation mappings
  retain their selected profiles. Long decode/background continuation is not
  enabled by these profiles. See [Google's migration guide](https://ai.google.dev/gemini-api/docs/latest-model).
  Run `make cargo ARGS="test -p magicllm --test gemini4_argon"` for the
  profile/wire contracts. With `GEMINI_API_KEY` exported, run
  `make cargo ARGS="test -p magicllm --test gemini4_argon -- --ignored --nocapture"`
  for five live calls: low/medium/high text, forced function call, and signed
  tool-result replay. The live lane fails if the key is missing.
- **xAI Grok** (`xai`, `XAI_API_KEY`): `https://api.x.ai/v1/responses` via
  `OpenAIResponsesProvider` in `ResponsesDialect::Xai`, wrapped by
  `XaiProvider`. Responses only (`previous_response_id` carries encrypted
  reasoning; `prompt_cache_key` pins one cache server): `openai_api_mode` is
  refused on `xai` profiles (router validation and at request) and an
  `api_base_url` naming `/chat/` fails at boot. xAI refuses `metadata` and
  `reasoning.effort: none` (the latter also refused in profile validation); the
  dialect drops `metadata` and `reasoning.strategy`, maps `minimal` to `low`,
  never sends GPT-5.6 `prompt_cache_options` or breakpoints, and refuses
  `server_web_search`. Supported: `reasoning.summary`, `text.verbosity`,
  `tool_choice: required`, `prompt_cache_key`, `previous_response_id` +
  `function_call_output`. Vision per model
  (`XaiProvider::model_supports_vision`: Grok 4.5+); usage has OpenAI's shape.
- **Sarvam AI** (`sarvam`): `sarvam-105b`, `sarvam-105b-conversations` over
  `https://api.sarvam.ai/v1/chat/completions` (`SARVAM_API_KEY` as the
  `api-subscription-key` header), in its own `SarvamProvider` (shares only
  neutral HTTP/SSE helpers). Text-only (string `content`;
  `model_supports_vision` false; chat renders images as placeholders). The provider
  sends `reasoning_effort` (`low|medium|high`, or JSON `null` for explicit Off)
  and `max_tokens` (reasoning counts; unset → 8192). Unset effort retains Low;
  `none`/`disabled` sends null, `minimal` maps to Low, and `xhigh`/`max` to High.
  A bounded allowance limits the combined thinking and answer budget. Tools default to
  `tool_choice: auto`; `json_schema` works; tool-result turns need no echoed
  `reasoning_content`. Streaming maps `delta.reasoning_content` to
  `ReasoningStart/Delta/End` (closed when answer tokens begin), then a
  usage-only chunk with `reasoning_tokens`. Extras are allow-listed (`stop`,
  `seed`, `frequency_penalty`, `presence_penalty`, `wiki_grounding`) since
  Sarvam 400s on unknown fields. No `store`, so app-platform processing refuses
  `sarvam`, `deepseek` and `xai` profiles. v2 hosted models are not wired.
- **DeepSeek** (`provider: deepseek`): Anthropic-compatible transport.
  Structured JSON output is unavailable on this transport and is rejected before dispatch.
  `deepseek-flash` (DeepSeek-V4.1-Flash) takes JPEG/PNG/GIF/WebP images (billed
  as input) and thinking (default on; `thinking.type` enabled|disabled).
- **Harness CLI subscriptions** (`harness-claude_code`, `harness-codex`,
  `harness-grok`, `harness-agy`, `harness-pi`): one call to an installed,
  signed-in CLI (`claude -p --output-format json`, `codex exec --json`,
  `grok`/`agy` streaming, Pi print mode).
  - Text in, text out: no tools, MCP, plane or grants, and deliberately **no
    isolation** (it rides the user's own subscription).
  - Replies read from claude `result`, codex last `agent_message`, grok
    `text`/`data`, agy `result.response`, else common fields. Limits: 300 s wall
    clock (prompt delivery, reading, exit), 8 MiB output (exceeding fails and
    stops the child); argv prompts (grok/agy) ≤ 128 KiB, others on stdin.
  - A nonzero exit or Codex `turn.failed` fails the operation even after partial
    text, which is never published as a completed background result (Pi 1.2+
    exits 1 on an output-limit cut-off).
  - Pi runs ephemeral, with a neutral system prompt and no tools, extensions,
    skills, prompt templates or context files; `model: default` uses Pi's own
    model and credentials (keeps `PI_CODING_AGENT_DIR`), else `pi --model`. No
    token usage. A run's background operations come here only when the CLI is
    on its own sign-in ([harness credentials](../magician/harness-auth.md)).
  - Qualified catalog models and native thinking controls determine CLI flags;
    Default retains the engine owner's configured model/effort. No authored
    size/effort profile variants are required.
  - Shipped operation mappings use local/API assignments. Settings and engine
    affinity can select installed harness models. Affinity finds a unique
    catalog model with the matching `harness-*` provider and physical model
    `default`; missing or ambiguous defaults do not resolve. Operator-defined
    catalog IDs work without a name-based compatibility table.
    `codex_app_server` is a plane engine, not a provider: its parent turn gets
    `chat.harness_model` / `execution.harness_model` directly, while secondary
    non-local operations use the catalog’s matching Codex harness default. See
    [LLM routing overrides](../magician/llm-routing-overrides.md#engine-affinity).
  - Harness profiles are identified by their `harness-*` provider. Dispatch
    refuses tool-less profiles for execution-native turns, so they never become
    agentic-loop providers.
  - Receipts mark token/cache/cost availability; unreported buckets stay
    unknown, and CLI USD estimates stay apart from account billing.

## Transport limits

- Bodies: 64 MiB before JSON allocation; JSON trees: 64 container levels, one
  million nodes; streaming: 8 MiB per event, 64 MiB aggregate. Violations are
  typed failures; over-ceiling input is never copied into a `Value::String`
  fallback, while malformed small error/tool-argument text keeps its string
  fallback.
- SSE uses a shared incremental UTF-8 decoder that rejects malformed terminal
  UTF-8 instead of inserting replacement characters. Anthropic consumes
  `message_stop` before ending; MiniMax skips malformed small compatibility
  events.
- Streaming checks HTTP status first; 4xx/5xx bodies parse (bounded) into
  `LLMError::Provider`.
- Large request lanes (`messages`, `tools`, schema, `extra`, context reuse,
  media, summarisation) are Arc-backed copy-on-write, so queue, router, fallback
  and retry clones stay shallow; admission verdicts bind to the exact Arc
  allocation (weak identity), so field replacement cannot reuse a stale one.
  Successful payloads move into response lanes without cloning. Nested JSON is
  copied and dropped with heap frames (no recursive `Value::clone`), so deep
  input cannot overflow worker stacks.

## Ollama

- `keep_alive` from `runtime.ollama.keep_alive` (default `10m`);
  `MAGICIAN_OLLAMA_KEEP_ALIVE` / `MAGICLLM_OLLAMA_KEEP_ALIVE` override per
  machine; profile `metadata.keep_alive` wins (`0` unloads, `-1` resident).
- `JsonObject` → `format: "json"`, schema → `format`, `max_output_tokens` →
  `options.num_predict`, `temperature` → `options.temperature`.
- Tool-declaring profiles warn only off native `/api/chat` (`/api/generate`
  refuses tools). No truncation retry.

## Routing Model

The caller sets `request.metadata.operation`;
`LLMRouterConfig.operation_mapping` maps it to a profile (provider, model,
defaults); capabilities are checked against the request; an optional fallback
profile runs on failure.

`operation_mapping` is also the operator-facing registry. An entry may carry
`default`, `when_has_images`, `when_cloud`, `description`, `group`, and
`engine: parent | pinned` (`OperationEngineFollow`, `follows_parent()`):
`parent` (default, and what a flat string means) follows the flow's starting
engine when it can serve the operation; `pinned` keeps its profile. Metadata
must be nonempty, bounded and control-character-free at router construction.
Legacy string and metadata-free selectors still work. Keep operation names
stable across prompts, orchestration and config.

```yaml
# llm-router.yaml is spliced under llm.router.
    catalog:
      connections:
        openai: {provider: openai, api_key_env: OPENAI_API_KEY}
      models:
        openai/luna:
          connection: openai
          model: gpt-6-luna
          label: Luna
          limits: {context_window_tokens: 400000, max_output_tokens: 128000}
          thinking: {mode: effort, levels: [none, low, medium, high], default: low}
          capabilities: {tools: true}
    operation_mapping:
      query_analysis:
        default: model/openai/luna/auto
        description: Analyze request intent and required capabilities.
        group: Planning & execution
```

## Protected Physical Disclosure

App workflows attach a non-serializable disclosure guard and physical resource
authorizer to protected requests, which then:

- pin one exact profile, provider, model, endpoint and transport cohort, with no
  fallback, retry, response reuse or streaming;
- strip prompt caches, continuation ids and provider-native state before the
  final bounded request digest;
- bypass local preparation, never follow redirects, and force provider storage
  off;
- recheck disclosure authority after queue/resource waits (the move-only permit
  keeps its root owner) and permit expiry synchronously before I/O;
- sum usage with checked arithmetic; missing, ambiguous or overflowing usage
  never becomes a smaller settlement.

The guarded path is cold and single-attempt. Server history, internal retry,
background execution or a second endpoint each need an explicit disclosure and
resource contract first.

## Whole-Call Rules (every provider)

A tool's canonical schema may carry top-level `anyOf` / `oneOf` / `allOf` /
`not` to say which argument combinations make a valid call (a pack's
`parameter_constraints`). The rules are an internal contract: candidate
construction, proposal validation and dispatch read them from the canonical
`LLMToolSpec`. Models never get them as schema. Anthropic, OpenAI Chat
Completions and xAI answer the whole request with a 400 on a top-level
combinator, Gemini has none, and OpenAI Responses accepts but does not enforce
one outside strict mode (measured 2026-10-30).

`tool_schema::project_whole_call_constraints()` is the one projection: it
removes those keys from the root and restates them in the tool description as
`Valid argument combinations (JSON Schema): {...}`. Every adapter's tool mapper
(including both Realtime APIs) sends that projection, and so does Magician's
plane MCP catalog (`project_schema()`), so a harness agent sees the same
definition. The same path keeps a third-party schema with root combinators from
failing a request. Nested combinators (inside a property) pass through.

## Tool Schema Sanitization (Gemini)

Gemini 400s on schema outside its `function_declarations[].parameters` subset.
After the whole-call projection,
`providers/gemini.rs::sanitize_gemini_schema()` recursively strips
`additionalProperties`, `$schema`, `$id`, `$defs`, `$ref`, `definitions`,
`oneOf`, `anyOf`, `allOf`, `not`, `if`/`then`/`else`, `examples`, `default`,
`patternProperties`, `unevaluatedProperties` from the clone `map_tools` sends;
other providers get the canonical schema minus its root rules.

## Tool Transcript Replay

| Provider | Replay |
| --- | --- |
| Anthropic Messages | `tool_use` + `tool_result`, multimodal images; signed `thinking` / `redacted_thinking` kept via `ContentBlock::Json` |
| OpenAI Chat | Text-only tool results (Chat Completions rejects images in prior history) |
| OpenAI Responses | Multimodal; `previous_response_id` + `function_call_output` deltas |
| Gemini | Native parts kept so `thoughtSignature` survives; inline images |

Rich OpenAI chat profiles should use `openai_api_mode: auto` (Chat for simple
turns, Responses for tool-bearing or multimodal continuations).
Explicit Chat mode supports text tool calls; images and Responses checkpoints
still require Responses.

**Call/result pairing.** The OpenAI Responses and Anthropic adapters answer
every tool call exactly once per request: duplicates are dropped, and an
unanswered call gets `providers::MISSING_TOOL_RESULT_TEXT` (after the call's
tool items on Responses; in the next user turn on Anthropic). Responses also
remembers (per process, bounded) each response's `function_call` ids: a
continuation delta starts after the assistant message holding them, later
caller-added assistant turns go in as `function_call` + output pairs, and an
unanswered remembered call is answered first. An unknown response id keeps the
last-assistant anchor.

## Reasoning Configuration

`LLMRequest.reasoning` (`ReasoningConfig`); each provider reads its subset.

| Field | Wire |
| --- | --- |
| `effort` (`none`…`max`) | Responses `reasoning.effort`; Chat `reasoning_effort`; Anthropic `claude_output_config` when enabled; Gemini 3 `thinkingConfig.thinkingLevel` |
| `max_reasoning_tokens` | Anthropic `thinking.budget_tokens` (default 10000); Gemini 2.5 `thinkingConfig.thinkingBudget`; never OpenAI (rejects `reasoning.max_tokens`; use `max_output_tokens`) |
| `strategy` | Internal hint (`extended_thinking`, `deepseek_thinking`); never sent |
| `summary` | Responses `reasoning.summary` (`auto`/`concise`/`detailed`; `auto` when on, rendering "Thought" rows); ignored elsewhere |

Catalogued models choose adaptive thinking (`thinking.type = adaptive` +
`output_config.effort`) versus `enabled` + `budget_tokens`, Off support and
sampling from catalog data (`LLMRequest.model_wire`; see
[model catalog](model-catalog.md#request-shape-wire)). Uncatalogued Claude ids
fall back to name rules (adaptive for Fable, Mythos, Opus 5.x, Sonnet 5,
Haiku 5.x, Opus 4.6–4.7 and Sonnet 4.6); Pi's harness config uses that fallback
through `anthropic_model_uses_adaptive_thinking`.

**Metadata denylist.** Profile `metadata:` flows into `LLMRequest.extra`, and
strict providers 400 on unknown fields; every provider strips
`reasoning_strategy`, `reasoning_max_tokens`, `reasoning_summary`, and
Anthropic/DeepSeek/MiniMax also strip `cost_per_million_input_tokens` /
`cost_per_million_output_tokens`.

## Tool Choice Policy

Anthropic-family requests with tools default to `tool_choice: {"type": "auto"}`
(thinking/text may precede `tool_use`); `metadata.tool_choice` overrides, e.g.
`{"type": "any"}` for structured execution. `{"type": "none"}` keeps tools
advertised but forbids use (for a final answer over a transcript holding tool
calls, since Anthropic rejects tool blocks sent without `tools`): Anthropic and
DeepSeek pass it through; OpenAI Chat/Responses, OpenRouter, MiniMax and Sarvam
send `"none"`; Gemini `NONE`.

## Provider-neutral context reuse

Callers set typed `LLMRequest.context_reuse`, never provider ids in `extra`;
`ContextReuseStrategy` picks the strongest semantics-preserving contract:

| Transport | Strategy | Wire |
| --- | --- | --- |
| OpenAI Responses | `server_continuation` | Bootstrap, then deltas via `previous_response_id` |
| xAI Responses | `server_continuation` | Same; always on for `provider: xai` (30-day storage), no metadata opt-in |
| Gemini Interactions | `server_continuation` | Bootstrap, then `user_input` / `function_result` via `previous_interaction_id`; tools, system instruction, generation config resent each turn |
| OpenAI Chat, Anthropic, Gemini `generateContent`, MiniMax, DeepSeek, OpenRouter, Sarvam | `prefix_cache` | Bounded replay, stable prefix |
| Ollama, Yutori, custom | `bounded_replay` | Bounded local replay; no cache claimed |

- Opt-in: `metadata.openai_api_mode: responses`,
  `metadata.gemini_api_mode: interactions` (requires `metadata.streaming` false;
  validation fails closed). `generateContent` stays the Gemini default and
  stores no state.
- A continuation id is valid only within one provider/model/base-URL/API-mode
  cohort (`transport_cohort_fingerprint`, across reloads and fallbacks). Model,
  endpoint or image-shape change, unprovable compaction, pause/resume, the
  six-turn rebase, or any fallback forces a clean bootstrap; a fallback's
  response id never fills the primary's slot, and server checkpoints never mix
  with replayed history.
- A Responses delta may start with a native `function_call_output`; without that
  or an assistant boundary the adapter bootstraps. A user recovery instruction
  may follow it.
- `stable_prefix_fingerprint` (model, tool order, system, user text before
  `CACHE_BREAKPOINT_SENTINEL`) is local invalidation data, never sent; it is
  heap-framed into the digest, computed by the configured router only after
  queue admission, and not rehashed by the operation router.
- OpenRouter gets an opaque execution-scoped `session_id`, exposing no
  principal, workspace or content.

## Prompt Caching

`LLMRequest.prompt_cache`, per provider (`prompt_cache = Disabled` opts out):

- **Anthropic.** Ordinary requests: stable system/user breakpoints. Rolling
  `prefix_cache` requests anchor the last tool, system, the **last completed
  turn** (`anchor_last_completed_turn`; a decision loop replaces its trailing
  prompt each iteration), a sentinel-ending final user block, and a block 16
  earlier (`anchor_lookback_block`; Anthropic looks back ~20 blocks), within
  four breakpoints; the tools give
  up theirs to the system anchor only when all four are taken. Sentinels are
  stripped; `extra.cache_control` also opts out. A conversation prompt cache
  (`PromptCacheConfig::conversation_with_ttl`, set by chat) rolls without a
  reuse plan, so chat history is read back instead of billed in full. Chat
  must not carry a reuse plan: it would replace the OpenAI Responses chain id
  in `extra.openai_previous_response_id`. Other providers ignore the flag.
- **OpenAI.** Automatic caching. On GPT-5.6+ (never 5.5 or older), a sentinel
  with a non-empty stable half, followed by text or later blocks/messages, adds
  an explicit breakpoint and `prompt_cache_options.mode: "implicit"` (OpenAI's
  automatic latest-message breakpoint stays). `cache_write_tokens` →
  `TokenUsage.cache_creation_tokens`. `prompt_cache_key` =
  `extra.prompt_cache_key`, else `context_reuse.session_key`, else a BLAKE3
  digest of model, operation, schemas and stable prefix. Caller and session keys
  are sent even without a sentinel, so full and delta turns share a cache. The
  decision loop sends its stable prompt as its own sentinel-ending message after
  the system prompt. `OperationLlmRouter::stable_chain_cache_key` is not sent.
  ([contract](https://developers.openai.com/api/docs/guides/prompt-caching))
- **Gemini.** Implicit on 2.5+ `generateContent`;
  `prompt_cache.cached_content -> cachedContent`; Interactions use server
  continuation.
- **DeepSeek.** No `cache_control` sent (automatic prefix cache);
  `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens` normalize into
  cached/total input.
- **xAI.** `prompt_cache_key` always sent (else `context_reuse.session_key`,
  else stable-prefix fingerprint) except `Disabled` and single-attempt requests.
- **Sarvam.** No controls; byte-stable replay prefix;
  `prompt_tokens_details.cached_tokens` read when reported.
- **OpenRouter.** Anthropic-family requests keep the inline system anchor
  (rolling and conversation ones use top-level `cache_control`); others get
  no `cache_control` and rely on upstream.
- **MiniMax** automatic (reads normalized when reported); **Ollama / Yutori N1**
  none advertised.

## Cache Breakpoint Sentinel

`CACHE_BREAKPOINT_SENTINEL = "<!--MAGICIAN_CACHE_BREAKPOINT-->"` marks a user
message's stable/volatile boundary.

- Anthropic (direct and via OpenRouter) splits it into two blocks with
  `cache_control: {"type":"ephemeral"}` on the first (`[prefix, suffix]` in
  multi-block content; only at index 0). A sentinel ending a block marks the
  whole block (no empty second block, which the API rejects); OpenAI puts its
  explicit breakpoint there.
- Others strip it; halves rejoin with one `\n` when both are non-empty. Every
  role is stripped (the split is user-only).
- `split_on_cache_sentinel(text) -> (String, Option<String>)` is byte-safe.
- `CACHE_SEGMENT_SENTINEL = "<!--MAGICIAN_CACHE_SEGMENT-->"` ends a repeated
  run; under GPT-5.6+ explicit caching OpenAI sends one item per segment
  (`split_cache_segments`) and breakpoints the last
  `MAX_CACHE_SEGMENT_BREAKPOINTS` (2) finished segments, so the previous one
  still reads. `split_on_cache_sentinel` and `strip_cache_segments` drops
  markers elsewhere; unmarked text stays byte-identical. Only an OpenAI planner
  on its own engine emits segments
  ([structured-decision.md](../magician/structured-decision.md)).

## Token Usage & Cost

`TokenUsage`: `cached_tokens` (read), `cache_creation_tokens` (write: Anthropic,
GPT-5.6+; `None` without telemetry); `prompt_tokens` includes both (subtract for
uncached input).

`magicllm::pricing::compute_cost(provider, model, usage) -> f64` prices against
the active `PricingTable`; `compute_cost_at` uses rows effective at the call's
time. Built-in rows cover OpenAI (current and historical GPT rows, 272K
multipliers), Anthropic, DeepSeek, MiniMax, OpenRouter, Gemini, xAI (200k tier
doubles rates), Sarvam (₹88/USD) and realtime models, pricing uncached input,
cache reads/writes and output without double billing; lookup is longest-prefix
(a dated revision wins at its boundary). Unknown pairs and free local providers
return `0.0`, so corrective reprice skips them rather than fabricating a zero
cost. Rates are rows in `llm_pricing.template.json` (compiled in; see
`pricing_file`); `llm_pricing.json` layers on top at startup.

Decision rows: `decision:typesafe` / `jev-1.13` and `jev-1.14`
([model card](https://docs.typesafe.ai/models)) and zero-cost
`decision:laya-{mlx,onnx}` / `decision:kev-{mlx,onnx}`; custom System One routes
never inherit TypeSafe pricing. Decision receipts are priced at start time, keep
unknown cache buckets and count once in the cost ledger; `/llm` shows them
separately.

## Truncation Handling

Plain (`invoke()`) requests get one recovery from truncation (Anthropic
`finish_reason = max_tokens`, OpenAI Chat / OpenRouter `length`, Responses
token-related `incomplete_details.reason`, Gemini `MAX_TOKENS`). It raises
`max_output_tokens` only from the caller's explicit limit or reported completion
usage; if rejected as excessive, one backed-off retry follows. Plain calls
stream under the hood and run this on the rebuilt body. Caller-visible streams
(`invoke_stream`) are excluded (partial output is already visible), except
OpenAI Responses, which retries a cut-off stream and streams the retry only when
the first attempt showed no text.
