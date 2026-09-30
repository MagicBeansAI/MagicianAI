# Chat Profile Routing

## Purpose

Chat profile routing lets the chat UI choose among eligible LLM profiles at send
time without changing the global `chat_completion` default.

## Eligibility Model

The backend exposes chat-eligible profiles through `GET /api/magician/v2/chat/profiles`.
The response has two arrays:

- `profiles`: chat-eligible profiles that are valid for rich chat tool use
- `warnings`: human-readable warnings for hidden incompatible profiles

The list is filtered to profiles that are valid for chat tool use and safe for
rich-chat continuation. In practice that means profiles need chat-safe
`tool_choice: auto`; OpenAI profiles pinned to `openai_api_mode: chat` are
excluded, while Anthropic, Gemini, and OpenAI `auto` / `responses` profiles
remain eligible when their metadata is otherwise compatible. MiniMax profiles
configured for forced native tool output (`tool_choice: any`) are intentionally
hidden from this rich-chat chooser. The current default visible profile is
marked in the response.

## Routing Model

Each send request can include an optional `profile` override. When present, the
chat pipeline routes the LLM call through that exact profile instead of the
operation default.

Current routed paths:

- synchronous chat send
- SSE streaming chat send
- Public envoy chat surfaces use
  `llm.router.operation_mapping.kapso_envoy_chat` when their `source_surface`
  matches the configured public-chat policy. The seed policy covers
  `kapso-envoy-chat` and `telegram-envoy-chat`; this is backend-owned so the
  model can change without changing the bots.

Profile validation happens before execution. If a send request names an invalid
or hidden profile, the API rejects it up front instead of falling back.

The selection is ephemeral. The frontend resets to the default profile on page
reload instead of persisting the choice.

## UI Surface

`/chat` loads the eligible profile list on mount and shows a selector
only when multiple chat-eligible profiles are available. The user sees
`model (provider)` labels with the default profile marked. When the backend
hides incompatible profiles, `/chat` and `/t/[name]` show the returned warning
strings inline near the composer so operators can see why a configured profile
is unavailable.

## OpenAI Default

Active Sol routes use `gpt-6.1-sol` through Responses, including Normal
(compatibility) and Advanced chat, coding, screen understanding/grounding,
review, and the remote archive-summary/reply-draft operations. GPT-6 Luna
remains Instant; GPT-6 Astra remains Frontier.

[GPT-6.1 Sol](https://developers.openai.com/api/docs/models/gpt-6.1-sol)
requires reasoning (`low | medium | high | xhigh | max`) and Responses for
tool calls, so Sol profiles use at least `low`. Grounding and remote reply
budgets are 4K to leave room for reasoning. No sampling controls are sent.
The model supports 1,050,000 context tokens and up to 128,000 output tokens;
individual profiles retain their smaller task budgets.

Canonical IDs use `gpt61sol`; `gpt6sol` IDs are aliases so saved selections
resolve, and the `rnone-out16k` alias targets
`gpt61sol-responses-vision-toolsany-rlow-out16k`. Historical model IDs and
pricing rows are kept for past-call accounting.

### Luna carries more than nano-class

GPT-6 Luna carries the memory pipeline, output synthesis, and the bounded half
of the pre-plan flow at each operation's reasoning tier.

**The line is drawn on workload shape.** Luna handles bounded extraction,
tagging, classification, and high-volume background work. GPT-6.1 Sol handles
operations whose context or judgment requirements have not yet been
requalified against GPT-6 Luna:

| stays on Sol | why |
| --- | --- |
| `agentic_decision` | averages 68K input tokens in recorded dispatch |
| `task_decomposition` | produces the plan; a bad one wastes the run |
| `atomic_composition` | `effort: high` long-form generation |
| `workflow_compilation` | context grows with the number of sequences compiled |
| `evidence_precision_judge` | a judge on the cheapest model fails invisibly |

**`LLMOperation` annotates each operation `(nano)`, `(small)`, `(medium)` or
`(strong)`** (`query_analysis/operation_llm_router.rs`). That annotation is the
intended tier. **Nothing enforces that the bound profile matches it**, so it
can drift silently (e.g. `parameter_default_inference`,
`parameter_safety_check`, `discovery_extraction` are declared `(small)` and
are bound to Luna only because the seed says so; nothing checks it).

Tool-bearing,
multimodal, and streaming rich-chat profiles remain pinned to the Responses API.
Effective-dated GPT-5.4 pricing is retained only for historical analytics and is
not an eligible runtime profile.

## Claude Opus 5.5

Opus 5 remains selectable; the advanced Anthropic default and Fable's fallback
use Opus 5.5 (ahead of Opus 5 on Anthropic's published comparisons, and cheaper:
$4/$20 per million input/output tokens, $0.20 cache reads). Its profiles use
adaptive thinking and `tool_choice: auto`, because Opus 5.5 rejects
disabled/manual-budget thinking and forced `any`/named tool choice.

## The Gemini tiers

Three, chosen by cost against how mechanical the work is:

| model | input / 1M | output / 1M | used for |
| --- | --- | --- | --- |
| `gemini-3.1-pro-preview` | $2.00 | $12.00 | deep work; still the newest Pro, still preview |
| `gemini-3.8-flash` | $0.75 | $3.75 | general Flash work |
| `gemini-3.5-flash-lite` | $0.30 | $2.50 | high-volume background: memory extraction, archive summary, episode quality, learning reflection |

3.8 Flash's price is introductory and **doubles to $1.50 / $7.50 on
2027-01-01**. Both rows are in the effective-dated table, so the increase lands
on its own rather than needing to be remembered.

Keeping the lite tier is a cost decision, not inertia: the twelve profiles on
it are the most frequently called paths in the system, and folding them into
3.8 Flash would cost 2.5x the input and 1.5x the output on exactly that
traffic. 3.5 Flash-Lite is Stable with no announced shutdown.

The lite profiles are named `gemini35-flash-lite` and must point at
`gemini-3.5-flash-lite`; Gemini 3.x Flash needs its own pricing rows, or calls
bill through the `gemini-` catch-all at 2.5-Flash rates.

Not Gemini API models, despite the name: `gemini-3.7-flash-{low,medium,high}`
are `harness-agy` CLI selectors riding a subscription, and are not per-token
billed.

## Adaptive Profiles (chat-only)

Adaptive profiles pair two standard profiles — a `fast_profile` with extended
reasoning off, and a `thinking_profile` with extended reasoning on — and let
the LLM self-escalate between them mid-turn via a `request_thinking_mode`
tool. The chat-inline runtime (`process_chat_inline_turn`) starts every turn
on `fast_profile`, exposes the escalation tool + a fast-mode instruction
block, and on the first `request_thinking_mode` call swaps to
`thinking_profile`, drops the escalation tool, strips the fast-mode prompt,
and re-runs the turn from the user's original message with full reasoning
budget. Adaptive state is per-turn: every new user message starts fresh on
the fast variant.

**Why "chat-only".** The escalation interceptor only exists inside
`process_chat_inline_turn`. Non-chat operations (autonomous loops, memory,
slot extraction, etc.) that resolve to an adaptive composite transparently
collapse it to its `fast_profile` via
`magicllm::router::resolve_adaptive_to_fast`. The global `default_profile`
is intentionally NOT an adaptive composite — pointing it there would be
visually misleading without any behavioral upside.

### Schema

```yaml
profiles:
  # standard profile entries (LLMProfile shape)
  ...

adaptive_profiles:
  chat-openai-adaptive-normal:
    description: >-
      OpenAI adaptive — GPT-6.1 Sol with vision + tools. ...
    tier: normal
    fast_profile: chat-gpt61sol-responses-vision-toolsauto-fast
    thinking_profile: chat-gpt61sol-responses-vision-toolsauto-thinking
  chat-openai-adaptive-frontier:
    description: GPT-6 Astra, escalates to high reasoning when needed.
    tier: frontier
    fast_profile: chat-gptastra-responses-vision-toolsauto-fast
    thinking_profile: chat-gptastra-responses-vision-toolsauto-thinking
```

Placement is the discriminator: a name in `profiles` is a standard profile,
a name in `adaptive_profiles` is a composite. No `kind` field on standard
profiles.

### Config-load validation

`magicllm::LLMRouterConfig::validate_adaptive_profiles` rejects:

- a `fast_profile` or `thinking_profile` that doesn't exist in `profiles`
- a reference that points at another adaptive composite (no nesting)
- identical fast/thinking pair (escalation would be a no-op)

`MultiLLMService::warn_on_misconfigured_adaptive_profiles` runs at startup
and logs a warning when either side of an adaptive pair fails the
chat-eligibility check (`tool_choice: auto`, and for OpenAI not
`openai_api_mode: chat`). Fast-side failures hide the composite from the
picker; thinking-side failures would otherwise surface mid-turn as a router
error — the startup warning catches them earlier.

### Shipped composites

| Composite | Provider | Fast | Thinking (high reasoning) |
|---|---|---|---|
| `chat-openai-adaptive-frontier` | OpenAI | gpt-6-astra, effort=low, 4K out | gpt-6-astra, effort=high, 8K shared reasoning/output |
| `chat-openai-adaptive-advanced` | OpenAI | gpt-6.1-sol, effort=low, 4K out | gpt-6.1-sol, effort=high, 8K shared reasoning/output |
| `chat-openai-adaptive-normal` | OpenAI | gpt-6.1-sol, effort=low, 4K out | gpt-6.1-sol, effort=high, 8K shared reasoning/output |
| `chat-openai-adaptive-instant` ★ default | OpenAI | gpt-6-luna, 4K out | gpt-6-luna, effort=high, 8K shared reasoning/output |
| `chat-anthropic-adaptive-frontier` | Anthropic | claude-fable-5-1, effort=low | claude-fable-5-1, effort=high |
| `chat-anthropic-adaptive-advanced` | Anthropic | claude-opus-5-5, effort=low | claude-opus-5-5, effort=high |
| `chat-anthropic-adaptive-opus5` | Anthropic | claude-opus-5, effort=low | claude-opus-5, effort=high |
| `chat-anthropic-adaptive-instant` | Anthropic | claude-haiku-4-5 | claude-haiku-4-5 with extended thinking |
| `chat-anthropic-adaptive-normal` | Anthropic | claude-sonnet-4-6, 32K out | claude-sonnet-4-6, 32K thinking budget, 64K out |
| `chat-gemini-adaptive-instant` | Gemini | gemini-3.5-flash-lite, 32K out | gemini-3.5-flash-lite, thinkingLevel=high, 8K out |
| `chat-gemini-adaptive-advanced` | Gemini | gemini-3.1-pro-preview, 4K out | gemini-3.1-pro-preview, thinkingLevel=high, 32K reasoning, 64K out |
| `chat-deepseek-adaptive-instant` | DeepSeek | `deepseek-flash` (V4.1 Flash), thinking disabled, vision | same model, thinking enabled, vision |
| `chat-deepseek-adaptive-advanced` | DeepSeek | `deepseek-flash` (V4.1 Flash), thinking disabled, vision | same model, thinking enabled at `effort: max`, vision |
| `chat-minimax-adaptive` | MiniMax | MiniMax-M2.7, 32K out | MiniMax-M2.7, effort=high, 64K out (no separate reasoning budget) |
| `chat-minimax-adaptive-advanced` | MiniMax | MiniMax-M3 | MiniMax-M3 with thinking enabled |
| `chat-xai-adaptive-advanced` | xAI | grok-4.7, effort=low, 8K out | grok-4.7, effort=high, 16K out |
| `chat-sarvam-adaptive` | Sarvam | sarvam-105b, effort=low, 8K out | sarvam-105b, effort=high, 24K out |

Provider quirks:

- **OpenAI GPT-5.6 and GPT-6 Luna** Responses API takes
  `effort: none | low | medium | high | xhigh | max`; omitted effort defaults
  to `medium`, so fast profiles must disable reasoning explicitly.
- **OpenAI GPT-6.1 Sol and GPT-6 Astra** take `effort: low | medium | high | xhigh | max`
  and rejects `none` / `minimal`. Their fast profiles therefore use
  `effort: low` instead of turning reasoning off. Tool calling requires the
  Responses API. Sampling params (`temperature`, `top_p`) are omitted.
- **Anthropic Claude Fable 5.1** (`claude-fable-5-1`) is adaptive-thinking
  always-on. `thinking.type=enabled` with `budget_tokens` 400s; Magician
  sends `thinking.type=adaptive` plus `output_config.effort`. Forced
  `tool_choice` `any` / `tool` also 400s, so Fable profiles stay on `auto`.
- **Anthropic Claude Opus 5.5** (`claude-opus-5-5`) keeps adaptive thinking
  always on and rejects forced tool choice. The advanced fast profile sends
  `effort: low`; the retained Opus 5 pair follows the same profile shape.
- **Anthropic** Extended Thinking ignores `effort`; only
  `max_reasoning_tokens` (→ `budget_tokens`) controls the budget. Sonnet
  4.6 supports up to 64K thinking budget + 64K output.
- **Gemini** maps `effort: low | medium | high | max` to
  `thinkingLevel: low | medium | high`. Gemini 3.5 Flash-Lite and 3.6 Flash
  reject legacy sampling controls, so the provider omits `temperature` and
  `topP` for those model generations. 3.1 Pro remains the advanced tier and
  supports `thinkingLevel: high` with up to 32K configured reasoning tokens.
- **DeepSeek** uses `effort` + `strategy: deepseek_thinking` +
  `metadata.thinking.type: enabled|disabled`. Reasoning budget is internal —
  no `max_reasoning_tokens` knob. `deepseek-flash` (V4.1 Flash) accepts
  images (not video) and thinking together; thinking defaults ON, so
  Instant-fast sends `thinking.type: disabled`. Retired `deepseek-v4-flash`,
  `deepseek-v4-flash-vision-exp`, and (from 2026-09-14) `deepseek-v4-pro`
  names are not configured.
- **MiniMax** treats reasoning as part of the output token pool — no
  separate budget knob; thinking variant just bumps `max_output_tokens`
  to 64K so the reasoning trace + final answer both fit.

### Runtime escalation contract

1. Adaptive detected → `effective_profile_name = fast_profile`, escalation
   tool injected, `FAST_MODE_INSTRUCTION_BLOCK` appended to system prompt.
2. LLM calls `request_thinking_mode` →
   - emit `RuntimeTransportEvent::ThinkingModeActivated`
   - `effective_profile_name = thinking_profile`
   - drop escalation tool from spec list
   - strip fast-mode block from system prompt
   - `iterations = iterations.saturating_sub(1)` (escalation doesn't
     count toward `CHAT_INLINE_MAX_TOOL_LOOP`)
   - discard the fast-mode assistant turn (no persist, no in-memory push)
   - `continue` the loop — next call uses thinking profile
3. Turn ends → emit `ThinkingModeCompleted` so UI can clear the
   "Thinking mode" chip.

If the LLM pairs `request_thinking_mode` with other tool calls in the same
response, ALL calls are discarded; the thinking turn re-plans from the
user's original message with full context. A warning logs the dropped
call names.

If the chat-runtime tool surface already exposes a tool named
`request_thinking_mode` (collision guard for future drift), the runtime
disables adaptive for the turn and falls through to a plain fast-profile
turn rather than corrupting the tool surface.

### Realtime events

`RuntimeTransportEvent::ThinkingModeActivated` and `…Completed` carry
`chat_turn_id`, `adaptive_profile`, `fast_profile`, `thinking_profile`,
and (Activated only) an optional `reason` string the LLM passed in.
Mirrored to `ui/unified-ui/src/lib/realtime/event-taxonomy.ts` so the
frontend's `chatTurnEventsStore` picks them up through the standard
per-turn SSE pipe. `thinkingModeForTurn(chat_turn_id)` derives the chip
state by replaying events for one turn id.

### UI surface

- The profile picker (`/chat` and `/t/[name]`) groups adaptive
  composites in their own "Adaptive" section above the standard list,
  with an "Adaptive" badge + the composite's `description`.
- When the selected profile is adaptive, the `FloatingComposer` trigger
  shows the badge.
- While a turn is escalated, the composer renders a pulsing "Thinking
  mode" chip driven by `thinkingModeForTurn(inFlightChatTurnId)`. The
  chip auto-clears on `ThinkingModeCompleted`.
