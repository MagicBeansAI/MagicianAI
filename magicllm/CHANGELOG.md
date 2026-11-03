# Changelog

All notable changes to this project will be documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
aims to follow Semantic Versioning.

---

## [Unreleased]

### 2026-11-02 — 0.2.56

- `estimate_messages_input_tokens(&[LLMMessage])`: an uncapped input estimate for "does this request fit the window" checks. Real media (image blocks, image URLs, base64 `data:` URLs and inline base64 parts whose MIME type is `image/*`, `audio/*`, `video/*` or `application/pdf`, such as Anthropic image sources and Gemini `inlineData`; a `data:text/plain;base64,` URL counts as text) count at a flat 1,600 tokens (`STREAM_INPUT_TOKENS_PER_MEDIA_BLOCK`, now public); everything else, including long whitespace-free text, at two bytes a token. Serialized bytes count a screenshot as millions of tokens. The stream spend estimate is unchanged.

### 2026-10-30 — 0.2.55

- Tool schemas reach models without top-level `anyOf`/`oneOf`/`allOf`/`not`: `tool_schema::project_whole_call_constraints()` moves those whole-call rules into the tool description for every provider adapter and both Realtime APIs. Anthropic, OpenAI Chat Completions and xAI rejected the whole request on them; Gemini dropped them silently.
- Strip internal tool-grounding aliases from Gemini provider schemas.
- Ollama: a non-2xx response is an error carrying the body's `error` text (for example "prompt is too long"), instead of a 400 with a `message` counting as an answer and one without it reading "missing assistant content".
- Added opt-in per-call cancellation for abandoned auxiliary waits while preserving owner cancellation, sibling isolation and protected-attempt settlement.
- `LlmPhysicalAttemptObservation::is_definitive_rejection()` marks an attempt the provider rejected with a typed HTTP 4xx other than 408/429. The outcome stays `OutcomeUncertain` for every existing consumer; resource owners may release the reservation at zero.
- Added GPT-6.1 Terra to the shared model catalogue with native reasoning controls and effective-dated standard/cache/long-context pricing.

### 2026-10-23 — 0.2.54

- `PromptCacheConfig::conversation_with_ttl` marks a request re-sent with new turns appended: native Anthropic also anchors the last completed turn, and Claude via OpenRouter takes the rolling top-level breakpoint, so the history is cached, not just tools + system. Rolling requests keep the tools' own breakpoint unless all four are taken, so a system-prompt change still reads the tools back. OpenAI, Gemini and non-Claude OpenRouter bodies are unchanged.

### 2026-10-23 — 0.2.53

- Spend gate: every streaming attempt settles exactly once — a stream whose consumer left keeps draining (bounded, then cancelled) to the final usage, and one that ends without it (consumer gone, provider error after output, cancellation) settles an estimate from its text input (each image or other media payload at a flat 1,600 tokens, capped at the context window or 200k) and output so far; $0 local streams are not metered. Embeddings are admitted (`SpendGate::admit_embedding`) and settled on estimated input tokens; $0 local models (`model_is_free_at`) are neither refused nor charged.
- Embeddings go through the spend gate (as system work when untraced); `model_is_free_at` marks $0-priced local models, which are neither refused nor charged.

### 2026-10-23 — 0.2.52

- Anthropic: a replayed `tool_use` always carries an object `input`. A call stored with unparseable arguments (kept as a raw string) is sent as `{"_unparsed_arguments": …}` with a short preview instead of failing every later turn with `tool_use.input: Input should be an object`.
- `SpendGate` seam (`spend_gate.rs`): one installable gate that every router call passes (`route`, `route_stream`, each fallback hop) — `admit` before the provider is contacted, `settle` with the priced cost after. No gate installed means no change. A refusal is `LLMError::SpendEnvelopeExhausted { resets_at_ms }`, terminal for dispatch (new `ErrorClass::SpendEnvelope`, never trips a breaker).
- Pricing: `try_compute_cost_at` returns `None` for an unlisted model instead of `0.0`, and `estimate_unpriced_cost_microusd` charges the highest rate the table knows for that provider on reported tokens (an output-ceiling estimate when usage is absent), so an unpriced model no longer counts as free.

### 2026-10-20 — 0.2.51

- Fixed: an OpenAI Responses stream that ends `response.incomplete` (e.g. at `max_output_tokens`) is no longer a silent success with partial text and no usage: it keeps its status, `incomplete_details` and usage and, like `invoke()`, is asked again once with twice the budget (backed off if rejected). `response.failed` and `error` events end the stream with the provider's message.
- `LLMResponse::stopped_at_output_limit()` reads the output cap in every provider's spelling.
- Streaming responses (OpenAI Responses and Chat, Anthropic, OpenRouter, MiniMax, Sarvam) are bounded by progress, not total length (`providers::StreamWatchdog`): the request timeout bounds the wait for the response head and the first output; after that a stream fails only after 120 s without new output, under a 30-minute ceiling. A long answer that keeps producing is no longer cut at the request timeout.
- Plain `invoke()` calls now stream under the hood with the same progress bound: OpenAI Responses (and xAI), OpenAI Chat, Anthropic (and DeepSeek), OpenRouter, MiniMax, Sarvam, Gemini (`streamGenerateContent?alt=sse`) and Ollama (NDJSON). The stream is folded back into the provider's non-streaming body and parsed by the same code, so text, tool calls, reasoning, usage, finish reason, the max-output-tokens retry and `raw_response` (web-search counts, citations, replay state) are unchanged. A stream cut off before its terminal event fails as `Transport`; a server that ignores the stream flag and answers with JSON is parsed as before. OpenAI Responses `background` requests and Gemini Interactions stay non-streaming. Gemini `invoke_stream` now sends token deltas.

### 2026-10-20 — 0.2.50

- `model_discovery`: per-source runtime model lists (provider list endpoints, CLI listings, rejections) with TTL, last-good retention and catalog drift; the harness CLI provider records model rejections.

---

Older entries: `docs/archive/changelogs/magicllm.md`
