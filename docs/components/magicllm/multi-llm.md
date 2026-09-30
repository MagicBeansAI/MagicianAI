# magicllm: Multi-Provider Router

## Purpose

`magicllm` is the shared LLM abstraction crate used by Magician. It provides:

- provider-agnostic request/response types
- provider capability metadata
- operation-based routing via `MultiLLMRouter`
- bootstrap helpers to instantiate providers from config

Out of scope: `magictunnel`, MCP proxy workflows, consumer-mode routing branches.

## Main Modules

- `types.rs`: request/response/message/tool payload types
- `provider.rs`: provider trait (`invoke`, `capabilities`, etc.)
- `router.rs`: `MultiLLMRouter` (profile + operation mapping + fallback support)
- `bootstrap.rs`: `ConfiguredRouter` helper to build/register providers
- `config.rs`: `LLMRouterConfig`, `LLMProfile`, `LlmConfig`

Provider registration is driven by configured profiles; only providers
referenced by profiles are instantiated.

## Supported Providers

- OpenAI Responses API (`openai_responses`), OpenAI Chat Completions
  (`openai_chat`), Anthropic Messages (`anthropic_messages`), Gemini (`gemini`),
  MiniMax (`minimax`), DeepSeek (`deepseek`), OpenRouter (`openrouter`), Ollama
  (`ollama`), Yutori N1 (`yutori_n1`).
- **GPT-6.1 Sol** (`gpt-6.1-sol`) uses Responses for tool calls and requires at
  least `low` reasoning (`none`/`minimal` unsupported); migrated fast profiles set
  `low` explicitly. Above 272,000 input tokens the whole request bills at 2x
  input/cache and 1.5x output (`make test-gpt61-sol`).
- **xAI Grok** (`xai`) — the xAI Responses API (`https://api.x.ai/v1/responses`,
  `XAI_API_KEY`) through `OpenAIResponsesProvider` in `ResponsesDialect::Xai`,
  wrapped by `XaiProvider`. Responses only, because that endpoint carries the
  conversation (encrypted reasoning included) via `previous_response_id` and
  `prompt_cache_key` routes it to one cache server; a Chat Completions reasoning
  model misses the cache unless every prior `reasoning_content` is replayed. So
  `openai_api_mode` is refused on `xai` profiles (router validation and at
  request), and an `api_base_url` naming `/chat/` fails at boot. Dialect: xAI
  refuses `metadata` and `reasoning.effort: none` (also refused in profile
  validation); it accepts `reasoning.summary`, `text.verbosity`,
  `tool_choice: required`, `prompt_cache_key`, `previous_response_id` +
  `function_call_output`. The dialect drops `metadata` and `reasoning.strategy`,
  maps `minimal` to `low`, never sends GPT-5.6 `prompt_cache_options` /
  breakpoints, and refuses `server_web_search`. Vision is per model
  (`XaiProvider::model_supports_vision`: Grok 4.5+). Usage has OpenAI's shape.
- **Sarvam AI** (`sarvam`) — `sarvam-105b`, `sarvam-105b-conversations` over
  `https://api.sarvam.ai/v1/chat/completions` (`SARVAM_API_KEY`, sent as the
  `api-subscription-key` header), served by its own `SarvamProvider` (only
  neutral HTTP/SSE helpers shared) so its quirks never touch the OpenAI path.
  - Message `content` must be a string: text-only
    (`model_supports_vision` is false; chat renders images as placeholders).
  - Every request reasons into `reasoning_content`; `reasoning_effort` accepts
    only `low|medium|high`. The provider always sends an effort (unset/`none`/
    `disabled`/`minimal` → `low`, `xhigh`/`max` → `high`) and `max_tokens`
    (reasoning counts against it; unset → 8192), because without them a short
    prompt can spend the whole default budget thinking and return nothing.
  - Tools default to `tool_choice: auto`; `json_schema` output works; tool-result
    turns need no echoed `reasoning_content`. Streaming maps
    `delta.reasoning_content` to `ReasoningStart/Delta/End` (closing when answer
    tokens begin), then a usage-only chunk with `reasoning_tokens`.
  - Extras are allow-listed (`stop`, `seed`, `frequency_penalty`,
    `presence_penalty`, `wiki_grounding`) because Sarvam 400s on unknown fields.
    No `store`, so app-platform processing refuses `sarvam` profiles (like
    `deepseek`/`xai`). The v2 endpoint's hosted models are not wired.
- **DeepSeek** (`provider: deepseek`) reuses the Anthropic-compatible transport.
  The configured model `deepseek-flash` (DeepSeek-V4.1-Flash) takes native image
  input (JPEG/PNG/GIF/WebP) and thinking (default on; `thinking.type`
  enabled|disabled); images bill as input tokens.
- **Harness CLI subscriptions** (`harness-claude_code`, `harness-codex`,
  `harness-grok`, `harness-agy`, `harness-pi`): `invoke` is a one-shot call to an
  installed, user-signed-in CLI (`claude -p --output-format json`,
  `codex exec --json`, `grok`/`agy` streaming variants, Pi print mode).
  - Text-in/text-out only — no tools, MCP, plane or grants, and deliberately
    **no isolation**: riding the user's own subscription is the point.
  - Output parsing prefers each CLI's probe-verified reply path (claude
    `result`; codex last `agent_message`; grok `text`/`data`; agy
    `result.response`) with a tolerant common-field fallback. Limits: 300 s wall
    clock (prompt delivery, reading, exit), 8 MiB output (exceeding fails the
    call and stops the child). Prompts ride stdin for claude/codex/pi and argv
    (≤ 128 KiB) for grok/agy.
  - A nonzero exit, or a Codex `turn.failed`, fails the operation even after
    partial assistant text; partial text is never published as a completed
    background result.
  - Pi runs with an ephemeral session, neutral system prompt, and tools,
    extensions, skills, prompt templates and context files disabled.
    `model: default` uses Pi's own model and credentials (the child env keeps
    `PI_CODING_AGENT_DIR`); a specific model passes via `pi --model`. No token
    usage is returned.
  - The profile `model` rides the CLI model flag, so **size variants are
    profiles** (`op-harness-*-small` pin haiku, grok-4-fast or
    gemini-3.7-flash-low). Codex has no small variant because ChatGPT-account
    Codex refuses alternate models; Pi has none.
  - `codex_app_server` is not a provider (it is a stateful plane/coding engine);
    when it drives chat or execution, secondary non-local operations ride
    `op-harness-codex`, while the parent turn gets `chat.harness_model` /
    `execution.harness_model` directly.
  - Shipped `operation_mapping`s map **zero** operations to harness profiles. A
    Settings override may select one, and the active chat/run harness supplies a
    process-wide affinity profile for eligible non-local operations; operations
    whose default is local (`ollama`) are exempt. Engine → profile:
    `claude_code` → `op-harness-claude`, `pi` → `op-harness-pi`,
    `codex_app_server` → `op-harness-codex`.
  - Harness profiles are identified by their physical `harness-*` provider, not a
    profile-name prefix, and execution-native dispatch refuses any resolved
    profile that does not declare tool-calling, so they can never become an
    agentic-loop provider.
- Aggregate harness receipts carry explicit token/cache/cost availability;
  unreported buckets stay unknown, and CLI USD estimates are distinguished from
  account billing.

## Transport limits

- Provider bodies are read through a 64 MiB ceiling before JSON allocation; JSON
  trees are admitted at 64 container levels and one million nodes. Streaming
  adapters add an 8 MiB event ceiling and a 64 MiB aggregate ceiling.
- SSE parsers walk an immutable buffer with a cursor and drain the consumed
  prefix once per network chunk; single-line `data:` stays borrowed. A shared
  incremental UTF-8 decoder keeps only an incomplete scalar suffix across chunks
  and rejects malformed terminal UTF-8 rather than inserting replacement
  characters. Anthropic consumes `message_stop` before terminating its parser.
  MiniMax skips malformed small compatibility events, but byte/depth/node
  violations stay typed failures.
- Streaming paths check HTTP status before consuming the stream; 4xx/5xx bodies
  are parsed (bounded) into `LLMError::Provider`. Malformed small
  error/tool-argument text keeps its string fallback; input over the ceilings is
  a typed validation error, never copied into a `Value::String` fallback.
- Large request lanes (`messages`, `tools`, response schema, `extra`, context
  reuse, media, summarisation) use Arc-backed copy-on-write, so queue, router,
  fallback and retry clones are shallow until wire materialization or mutation.
  JSON admission verdicts are cached per lane and bound to the exact Arc
  allocation (weak identity), so direct field replacement cannot reuse a stale
  verdict. COW forks and response mutations copy nested JSON with heap frames
  (never recursive `Value::clone`), so deep input cannot overflow worker stacks.
  Successful payloads move into Arc-backed response lanes without cloning. A
  last-owner guard, armed only for structurally rejected lanes, drains nested
  JSON iteratively.

## Ollama

- Requests send `keep_alive` from `runtime.ollama.keep_alive` (default `10m`).
  `MAGICIAN_OLLAMA_KEEP_ALIVE` / `MAGICLLM_OLLAMA_KEEP_ALIVE` override per
  machine; per-profile `metadata.keep_alive` wins for that profile (`0` unload
  immediately, `-1` keep resident).
- Structured generation maps from the neutral request: `JsonObject` →
  `format: "json"`, JSON schema → `format`, `max_output_tokens` →
  `options.num_predict`, `temperature` → `options.temperature`, keeping local
  JSON workers (channel distillation, classification) bounded.
- The router warns that a tool-declaring profile will be rejected at call time
  only when `api_base_url` is not the native `/api/chat` contract (only
  `/api/generate` refuses tools).
- No automatic truncation retry (no stable token-limit reason in the API).

## Routing Model

1. caller sets `request.metadata.operation`
2. router resolves operation -> profile using `LLMRouterConfig.operation_mapping`
3. profile selects provider/model/defaults
4. router validates provider capabilities against request requirements
5. router invokes provider; optional fallback profile can be used on failure

`operation_mapping` is also the operator-facing operation registry. A mapping
object may carry `description` and `group` beside `default`, `when_has_images`
and `when_cloud` (Settings reads them), and `engine: parent | pinned`
(`OperationEngineFollow`): `parent` — the default and the meaning of a flat
string selector — follows the engine that started the flow when it can serve the
operation; `pinned` keeps its own profile (`follows_parent()`). Legacy string and
metadata-free selectors remain compatible. Catalog metadata is validated as
nonempty, bounded and control-character-free at router construction. Keep
operation names stable across prompt templates, orchestration and routing
config.

```yaml
llm_router:
  default_profile: default
  profiles:
    default:
      provider: openai
      model: gpt-5.6-terra
      api_key_env: OPENAI_API_KEY
    vision:
      provider: anthropic
      model: claude-sonnet-4-5
      api_key_env: ANTHROPIC_API_KEY
  operation_mapping:
    query_analysis:
      default: default
      description: Analyze request intent and required capabilities.
      group: Planning & execution
    vision_verify:
      default: vision
      description: Verify a visual claim against an image.
      group: Vision
```

## Protected Physical Disclosure

Magician app workflows attach a non-serializable disclosure guard and physical
resource authorizer to each protected request. `magicllm` treats it as a
stricter transport contract:

- the route is pinned to one exact profile, provider, model, endpoint and
  transport cohort; fallback, queue/provider retry, response reuse and streaming
  are rejected;
- prompt caches, continuation ids and provider-native state (conversations,
  sessions, cached content, storage flags) are stripped before the final bounded
  request digest;
- local preparation is bypassed, built-in HTTP clients never follow redirects,
  and stateful provider adapters force provider storage off;
- disclosure authority is revalidated after queue/resource waits; the move-only
  resource permit keeps its root owner through that check and rechecks expiry
  synchronously before provider I/O;
- every usage counter is narrowed and summed with checked arithmetic; missing,
  ambiguous or overflowing usage never becomes a smaller settlement.

The guarded path is cold and single-attempt. A provider feature needing server
history, internal retry, background execution or a second endpoint must first
gain an explicit disclosure and resource contract.

## Tool Schema Sanitization (Gemini)

Gemini's `function_declarations[].parameters` accepts a strict JSON-Schema
subset and 400s on others. `providers/gemini.rs::sanitize_gemini_schema()`
recursively (properties, array items, nested schemas — offending keys occur
deeply nested) strips `additionalProperties`, `$schema`, `$id`, `$defs`, `$ref`,
`definitions`, `oneOf`, `anyOf`, `allOf`, `not`, `if`/`then`/`else`, `examples`,
`default`, `patternProperties`, `unevaluatedProperties`. `map_tools` sanitizes a
clone; other providers receive the canonical schema.

## Tool Transcript Replay

- Anthropic Messages: assistant `tool_use` plus `tool_result`, including
  image-bearing results as multimodal blocks. Signed `thinking` /
  `redacted_thinking` blocks are preserved via `ContentBlock::Json` so signatures
  survive replay.
- OpenAI Chat: text-only tool results; inline image replay is unsupported (Chat
  Completions rejects images in prior history).
- OpenAI Responses: multimodal history; native continuation via
  `previous_response_id` plus `function_call_output` deltas.
- Gemini: provider-native assistant parts are preserved so `thoughtSignature`
  survives; multimodal function responses replay with inline images.

OpenAI-backed rich chat profiles should use `openai_api_mode: auto`, staying on
Chat Completions for simple turns and escalating tool-bearing or multimodal
continuations to Responses.

## Reasoning Configuration

`LLMRequest.reasoning` (`ReasoningConfig`) collapses every provider's reasoning
surface into typed fields; each provider reads its subset.

- `effort` (`none` / `low` / `medium` / `high` / `max`): Responses
  `reasoning.effort`; Chat Completions top-level `reasoning_effort`; Anthropic
  via `claude_output_config` when reasoning is enabled; Gemini 3
  `thinkingConfig.thinkingLevel`.
- `max_reasoning_tokens`: Anthropic `thinking.budget_tokens` (default 10000 when
  enabled); Gemini 2.5 `thinkingConfig.thinkingBudget`. **Not forwarded** to
  OpenAI (Responses rejects `reasoning.max_tokens`); use `max_output_tokens`.
- `strategy`: magicllm-internal hint (`extended_thinking`, `deepseek_thinking`)
  for Anthropic-family adapters; never sent.
- `summary`: Responses `reasoning.summary` (`auto` / `concise` / `detailed`;
  defaults to `auto` when reasoning is enabled so summary blocks render as
  "Thought" rows). Ignored elsewhere.
- Which Anthropic models use adaptive thinking (`thinking.type = adaptive` +
  `output_config.effort`) instead of a budget is one rule,
  `providers::anthropic_messages::anthropic_model_uses_adaptive_thinking`
  (Fable, Mythos, Opus 5.x, Sonnet 5, Opus 4.6–4.7, Sonnet 4.6 — which refuse
  `thinking.type = enabled`; Haiku 4.5 still takes it). The Pi harness uses the
  same rule.

**Metadata denylist.** Profile `metadata:` flows into `LLMRequest.extra`, which
can carry router-internal, pricing or fallback hints; strict providers 400 on
unknown fields. Every provider strips `reasoning_strategy`,
`reasoning_max_tokens`, `reasoning_summary`. Anthropic (and DeepSeek by
delegation) also strips `cost_per_million_input_tokens` /
`cost_per_million_output_tokens`; MiniMax filters the same hints itself.

## Tool Choice Policy

Anthropic-family requests default to `tool_choice: {"type": "auto"}` when tools
are present and no override is given (preserves thinking/text before `tool_use`,
avoids implicit forced-tool issues). Profiles set `metadata.tool_choice`
explicitly otherwise: chat uses `auto`; structured execution that must return a
tool may use `{"type": "any"}`.

## Provider-neutral context reuse

Agentic callers describe reuse through typed `LLMRequest.context_reuse`, never
provider checkpoint ids or fingerprints in `extra`. `ContextReuseStrategy`
selects the strongest contract that preserves semantics:

| Transport | Strategy | Wire behavior |
| --- | --- | --- |
| OpenAI Responses | `server_continuation` | Full bootstrap once, then assistant-anchored deltas through `previous_response_id` |
| xAI Responses | `server_continuation` | Same protocol as OpenAI Responses; always on for `provider: xai` (30-day server storage), no metadata opt-in |
| Gemini Interactions | `server_continuation` | Full bootstrap once, then new `user_input` / `function_result` steps through `previous_interaction_id`; tools, system instruction, and generation config are resent each turn |
| OpenAI Chat, Anthropic, Gemini `generateContent`, MiniMax, DeepSeek, OpenRouter, Sarvam | `prefix_cache` | Complete bounded replay with a stable serialized prefix; each adapter uses its provider's automatic or explicit cache contract |
| Ollama, Yutori, unknown/custom providers | `bounded_replay` | Complete local replay with the shared pair-count and character ceilings; no checkpoint or cache capability is invented |

- Stateful modes are opt-ins: OpenAI needs `metadata.openai_api_mode: responses`;
  Gemini needs `metadata.gemini_api_mode: interactions` (`generateContent`
  stays default so it never stores state). Interactions profiles must keep
  `metadata.streaming` false until its SSE adapter exists (validation fails
  closed).
- Continuation ids are valid only inside an exact
  provider/model/base-URL/API-mode cohort. Model or endpoint switch, image-shape
  change, compacted history without a provable assistant boundary, pause/resume,
  the periodic six-turn rebase, or any physical fallback drops the id and sends
  a clean bounded bootstrap. `transport_cohort_fingerprint` enforces this across
  reloads and fallbacks; a fallback's response id never enters the primary's
  slot; server checkpoints are never combined with replayed pre-checkpoint
  history.
- Responses also accepts a pre-sliced delta starting with a native
  `function_call_output` (its `function_call` lives in the referenced response);
  a user recovery instruction may follow. Without a local assistant boundary or
  native tool result, the adapter bootstraps cleanly.
- `stable_prefix_fingerprint` hashes model, exact tool order, system content and
  user text before `CACHE_BREAKPOINT_SENTINEL`. It is local
  observability/invalidation data, never sent. Encoding is heap-framed straight
  into the digest (stack-safe, no request-sized buffer). The configured router
  computes it only after queue admission; the operation router does not rehash.
- OpenRouter receives an opaque execution-scoped `session_id` for cache-sticky
  upstream routing without exposing principal, workspace or content.

## Prompt Caching

`LLMRequest.prompt_cache`; behavior is intentionally per provider.

- **Anthropic Messages.** Ordinary requests keep explicit stable system/user
  breakpoints. Agentic `prefix_cache` (rolling) requests anchor where the prompt
  is stable: the last tool, the system block, and the last block of the **last
  completed turn** (`anchor_last_completed_turn`). Why: a decision loop replaces
  its trailing observation prompt each iteration, so only the conversation up to
  the last completed turn is a prefix of the next request. When the final user
  message's last text block ends with the sentinel, that block is anchored too
  (the caller repeats it verbatim), and tools defer to the system anchor to stay
  within Anthropic's four breakpoints. A fourth breakpoint sits 16 content blocks
  earlier (`anchor_lookback_block`) because Anthropic looks back only ~20 blocks
  from a breakpoint. Sentinels are stripped; `prompt_cache = Disabled` or
  `extra.cache_control` opts out.
- **OpenAI Chat / Responses.** Automatic caching is the fallback. GPT-5.6+
  requests whose system/user text contains the sentinel with non-empty halves
  also get a native explicit block breakpoint, a deterministic key over model +
  operation + tool/response schemas + exact stable prefix (streamed into BLAKE3
  with heap frames), and `prompt_cache_options.mode: "implicit"` (keeping
  OpenAI's automatic latest-message breakpoint). `Disabled` suppresses these;
  GPT-5.5 and older never receive them. `cache_write_tokens` normalizes into
  `TokenUsage.cache_creation_tokens`. A caller-supplied
  `extra.prompt_cache_key` wins and is honored even without a sentinel (routing
  key only), which is what a Responses-chain continuation needs. The decision
  loop sends its stable prompt as its own message right after the system prompt,
  ending with the sentinel, so the derived key covers system + tools + stable
  prompt and is identical for every full send of a run.
  `OperationLlmRouter::stable_chain_cache_key` exists but is not sent. See the
  [OpenAI prompt caching contract](https://developers.openai.com/api/docs/guides/prompt-caching).
- **Gemini.** Implicit caching on 2.5+ `generateContent`; explicit reuse via
  `prompt_cache.cached_content -> cachedContent`. Interactions profiles use
  server continuation.
- **MiniMax.** Automatic prefix caching on the bounded replay; cache reads
  normalized when reported.
- **DeepSeek.** Ignores `cache_control`, so none is sent; relies on automatic
  prefix cache. `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens` normalize
  into total/cached input.
- **xAI.** Automatic per-server prefix caching. Every request carries a
  `prompt_cache_key` — caller's, else `context_reuse.session_key`, else the
  stable-prefix fingerprint — so a conversation reaches the server holding its
  cache; `Disabled` and single-attempt (no-storage) requests send none.
- **Sarvam.** No cache controls; the bounded replay keeps a byte-stable prefix
  and `prompt_tokens_details.cached_tokens` is read when reported. Sentinel
  stripped.
- **OpenRouter.** Execution-scoped `session_id`; Anthropic-family ordinary
  requests keep the inline system anchor, rolling requests use top-level
  `cache_control`; other routes rely on upstream caching.
- **Ollama / Yutori N1.** Bounded replay; no prompt caching advertised.

## Cache Breakpoint Sentinel

`CACHE_BREAKPOINT_SENTINEL = "<!--MAGICIAN_CACHE_BREAKPOINT-->"` marks the
stable/volatile boundary of a user message.

- **Anthropic direct + OpenRouter Anthropic-family**: split the user message into
  two blocks with `cache_control: {"type":"ephemeral"}` on the first. In
  multi-block content the first sentinel-bearing text block is replaced in place
  by `[prefix, suffix]`, and `cache_control` attaches only when that block is at
  index 0 (a preceding volatile block would defeat the anchor).
- **All others**: strip the sentinel and send one string; halves are rejoined
  with a single `\n` when both are non-empty.
- A sentinel that **ends** a block marks the whole block: OpenAI puts the
  explicit breakpoint on it, and Anthropic attaches `cache_control` to that block
  instead of emitting an empty second block (which the API rejects).
- Every role's text is stripped of sentinels (the split is user-only); the
  Anthropic system-prompt path strips at its attach site.
- `split_on_cache_sentinel(text) -> (String, Option<String>)` exposes the parser;
  it is byte-safe against multi-byte UTF-8 before the sentinel.

## Token Usage & Cost

`TokenUsage` keeps cache buckets separate:

- `cached_tokens` — cache **read**.
- `cache_creation_tokens` — cache **write** (Anthropic, GPT-5.6+); `None` where
  providers report no write telemetry.
- `prompt_tokens` includes both; subtract them for uncached input.

`magicllm::pricing::compute_cost(provider, model, usage) -> f64` prices usage
against the active `PricingTable`; `compute_cost_at` resolves effective-dated
rows at the call's own time. The built-in table covers OpenAI (current and
historical effective-dated GPT rows, with 272K long-context multipliers where
applicable), Anthropic, DeepSeek, MiniMax, OpenRouter (mirroring vendor
baselines), Gemini, xAI Grok (200k-prompt tier doubles every rate), Sarvam
(rupee list converted at ₹88/USD), and realtime models. It accounts for
uncached input, cache reads, cache writes, output and long-context multipliers
without double billing. Lookups use longest-prefix matching (a more specific
dated revision wins at its effective boundary). Unknown pairs and free local
providers return `0.0`, so Magician's corrective reprice skips them rather than
fabricating a zero cost. `PricingTable::with_rows` layers deployment rates over
the base; the live `llm_pricing.json` overlay is read at startup (restart after
replacing it).

Decision Model rows live in the same registry: `decision:typesafe` / `jev-1.13`
([model card](https://docs.typesafe.ai/models)) and zero-API-cost local
`decision:laya-{mlx,onnx}` / `decision:kev-{mlx,onnx}`. Custom System One routes
never inherit TypeSafe pricing. Decision receipts are priced at start time,
preserve unknown cache buckets, and contribute once to the ordinary cost ledger;
`/llm` exposes a separate breakdown.

## Truncation Handling

Non-streaming requests get one bounded recovery from documented token-budget
truncation: Anthropic `finish_reason = max_tokens`, OpenAI Chat / OpenRouter
`length`, Responses token-related `incomplete_details.reason`, Gemini
`MAX_TOKENS`. The retry raises `max_output_tokens` only from a trustworthy
baseline (the caller's explicit limit, or reported completion usage); if the
larger request is rejected as excessive, one backed-off retry follows. Streaming
is excluded because partial output is already user-visible.
