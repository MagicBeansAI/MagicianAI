# Changelog

All notable changes to this project will be documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
aims to follow Semantic Versioning.

---

## [Unreleased]

### 2026-10-04 — 0.2.47

- Per-model prices moved into the pricing file (schema 2: realtime, web-search and TTS/STT media rates); code keeps catch-all defaults; Grok voice bills by session seconds.
- Fixed: `tool_choice: none` maps to `none` on OpenAI Chat, OpenAI Responses and OpenRouter instead of `required`.
- Meter bounded generation resources and propagate cancellation, keeping Anthropic and Gemini structured-output contracts.

### 2026-09-23 — 0.2.44 — xAI Grok and refreshed frontier profiles

- Pricing and request contracts for Claude Opus 5.5, GPT-6 Sol and GPT-6 Luna; Sol and Luna allow the non-reasoning fast path.
- New `provider: xai` serves Grok over the xAI Responses API (`XAI_API_KEY`); requests drop the `metadata` and `reasoning.strategy` fields xAI rejects and map `minimal` effort to `low`. Chat mode is refused.
- xAI profiles use server-side continuation and always send a prompt cache key.
- Grok 4.x pricing rows, with every rate doubled above 200k tokens.
- Validation refuses Grok profiles with reasoning disabled and gates vision support by model.

### 2026-09-21 — 0.2.43 — A caller's prompt-cache key is honored on a markerless request

- Fixed: a caller-supplied prompt-cache key is sent even when the request has no cache breakpoint, so Responses continuation turns hit the cache.

### 2026-09-20 — 0.2.42 — An Anthropic agentic request caches the conversation it will send again

- Fixed: rolling Anthropic requests anchor cache breakpoints on the stable prefix and last completed turn, so agentic loops read from cache instead of rewriting it on every call. OpenRouter's Anthropic passthrough is unchanged.

### 2026-09-19 — 0.2.41 — A GPT-Live session is billed by its clock, and a Gemini reconnect keeps its calls

- Realtime pricing supports per-second billing; GPT Live sessions are metered by duration (pricing fingerprint `v2`).
- Fixed: Gemini Live keeps in-flight tool call names across reconnects, so results after a tool load no longer surface as system errors.

---

Older entries: `docs/archive/changelogs/magicllm.md`
