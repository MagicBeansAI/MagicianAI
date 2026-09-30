# Changelog

All notable changes to this project will be documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
aims to follow Semantic Versioning.

---
## [Unreleased]

- Add a Sarvam chat provider with text-only reasoning, tools, JSON output, prefix reuse, and pricing.
- Add GPT-6.1 Sol pricing and reasoning contracts while retaining historical GPT-6 Sol rates.

_Current development version: `0.2.46`._

- Anthropic rolling requests cache the final turn's marked text and a lookback block (tools defer to the system anchor); a sentinel ending a block marks the whole block on OpenAI and Anthropic; `anthropic_model_uses_adaptive_thinking` is public for harnesses.

- Share the fair dispatch scheduler with Decision Model calls.

- Grok Voice is a selectable live engine on the backend realtime socket.

- Harden CLI harness process resolution when a supplied environment changes
  `PATH`, so a missing executable fails cleanly before spawn.

### 2026-09-23 — 0.2.44 — xAI Grok and refreshed frontier profiles

- Added published pricing and request-contract coverage for Claude Opus 5.5,
  GPT-6 Sol, and GPT-6 Luna. GPT-6 Sol and Luna accept the non-reasoning fast
  path; GPT-6 Astra continues to require reasoning.

`provider: xai` serves Grok over the xAI Responses API
(`https://api.x.ai/v1/responses`, key `XAI_API_KEY`). A config-only profile
pointing the OpenAI provider at `api.x.ai` would have failed its first call:
live probes showed xAI refuses the `metadata` field every OpenAI request
carries ("Argument not supported") and `reasoning.effort: none`.

- `XaiProvider` wraps `OpenAIResponsesProvider` in `ResponsesDialect::Xai`,
  which drops `metadata` and `reasoning.strategy`, maps `minimal` to `low`,
  never sends GPT-5.6 cache options, refuses `server_web_search`, and reports
  `LLMProviderKind::Xai` (pricing, telemetry, context reuse). Responses only:
  `openai_api_mode` is refused on `xai` profiles and at the request, and a
  `/chat/` base URL fails at boot.
- Context reuse is `server_continuation` for every `xai` profile — chained
  `previous_response_id` + `function_call_output` turns verified live.
- xAI caches per server, so every request carries a `prompt_cache_key`
  (caller-supplied, else the execution `session_key`, else the stable-prefix
  fingerprint): ~79 % of continued turns hit the cache with it, ~71 % without,
  over six 8-turn runs.
- Pricing rows for Grok 4.7 / 4.6 / 4.5 / 4.3 / 4.20 / build, with the 200k
  tier doubling every rate; `grok_4_7_matches_the_providers_own_billed_cost`
  pins the table to xAI's own `cost_in_usd_ticks` for a live call.
- Profile validation refuses a disabled-reasoning `xai` profile (Grok
  reasoning cannot be turned off) and gates `supports_vision` on
  `XaiProvider::model_supports_vision` (Grok 4.5+).

### 2026-09-21 — 0.2.43 — A caller's prompt-cache key is honored on a markerless request

`openai_prompt_cache::plan` returned the empty plan whenever the request
carried no `CACHE_BREAKPOINT_SENTINEL`, dropping a caller-supplied
`extra.prompt_cache_key` with it. A Responses-chain continuation turn is
exactly that request — a delta with no full prompt — so every
continuation went out unkeyed while bootstrap and rebootstrap turns went
out under a digest key that changed with the history in front of the
sentinel. Run 17 (2026-09-21) billed five ~105K-token rebootstraps at 0%
cached with an identical 70K system+tools prefix warm under another key.

- No sentinel + caller key → the key is sent, no explicit breakpoint,
  the caller's validated options or none. No sentinel + no key → the
  empty plan, as before.
- Test: a markerless delta with a caller key is routed without a
  breakpoint; the same key on a full prompt keeps its breakpoint.

### 2026-09-20 — 0.2.42 — An Anthropic agentic request caches the conversation it will send again

A Fable 5.1 decision loop wrote its whole 30–50k-token prompt into the
Anthropic cache on every one of nine decisions and read nothing back
(`cache_creation_tokens ≈ prompt_tokens`, `cached_tokens=0`): ~$0.55 a
decision, a $5 run budget gone in eight iterations, while the same loop on
OpenAI read ~97% of each prompt from cache.

- Rolling (`prefix_cache`) requests used one top-level `cache_control` and no
  block breakpoints. That caches the prefix ending at the final message and
  pays off only when the next request is this one plus appended turns; an
  agentic loop re-renders its final observation prompt every iteration, so the
  cached prefix never recurred. The provider now anchors the last tool, the
  system block, and the last block of the last completed turn
  (`anchor_last_completed_turn`; `thinking` blocks are skipped) — the parts the
  next request repeats — and treats user sentinels as strip-only in rolling
  mode. Non-rolling requests are unchanged. Test:
  `anthropic_agentic_rolling_prefix_anchors_the_stable_prefix_and_the_last_completed_turn`.
- OpenRouter's Anthropic passthrough keeps its top-level rolling breakpoint
  (different wire format; not measured here).

### 2026-09-19 — 0.2.41 — A GPT-Live session is billed by its clock, and a Gemini reconnect keeps its calls

- `RealtimeUsage` gains `billed_seconds` and `RealtimePricing` a `per_second`
  rate, for a provider billed by the clock rather than by tokens. `gpt-live-1`
  is such a row: $0.05/min at zero token rates, because Live's backend model is
  the host's own delegated chat turn and is metered there. `openai_live`
  reports the session's seconds as one billable response after its run loop
  ends — ending a call sends `session.close` and stops reading, so the server's
  closing frame usually never arrived and the session went unmetered — taking
  the provider's `session.usage.updated` / `session.closed` figure when it sent
  one and the connection it held open when it did not. `realtime_is_duration_billed_at`
  lets a consumer tell the two kinds apart; the realtime pricing fingerprint is
  now `v2`, covering the per-second rate.
- `gemini_live` keeps the names of its in-flight tool calls per voice session
  rather than per connection. A `tool_search` that loads a deferred tool
  reconnects the session (Gemini cannot change a live tool catalog), and the
  result of the call that triggered it is delivered to the new connection —
  where the name was gone, so the response named the call id instead of the
  function and the model answered "a system error occurred". Bounded, and
  forgotten when the conversation is not resumable.

### 2026-09-17 — 0.2.40 — Gemini 3.8 Live and 3.8 Live Extended Thinking

- `gemini_live` serves `gemini-3.8-live` (new default) and `gemini-3.8-live-extended-thinking` beside 3.1 with a per-model wire contract (`NON_BLOCKING` tools + `scheduling`, `thinkingLevel` on the extended model only, `interactionStatus` → `RealtimeProviderEvent::InteractionStatus`), profile `thinking_level` / `tool_result_scheduling` / `display_order` validated in the factory, both ids priced at Flash Live rates with thoughts billed as output, `ResponseDone` closed on the `turnComplete` that carries usage (every Gemini voice turn had priced at $0), arrays without `items` completed with `items: {}` so the 3.8 setup validator no longer closes 1007, and an opt-in live eval (`make test-gemini-live-models-live`).

---

Older entries: `docs/archive/changelogs/magicllm.md`
