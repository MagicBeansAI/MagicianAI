# MagicLLM Docs

Landing page for `magicllm` documentation. Provider contracts (prompt caching,
token usage and cost, tool transcript replay, tool-choice policy, truncation)
live in [multi-llm.md](multi-llm.md).

## Canonical References

- [MagicLLM Docs](multi-llm.md)
- [LLM dispatch queue](../magician/llm-dispatch-queue.md)
- [MagicLLM Changelog](../../../magicllm/CHANGELOG.md)
- Retired Canvas Mode Execution Board
- [Workspace Architecture](../../ARCHITECTURE_V2.md)

## Dispatch

`magicllm::dispatch` is the process-wide chokepoint between Magician callers and
`MultiLLMRouter`. Lanes use the shared `runtime_core::fair_queue` component; the
Decision Engine reuses it for typed model jobs with separate process-local
queues.

- Each High/Normal/Background lane is a bounded `FairLane`. Pickup prefers High
  over Normal over Background with 8-consecutive-High and
  12-consecutive-foreground bounds; within a lane the next job is the oldest job
  of the next owner (`task_ref.agent_id` if non-empty, else `task_ref.task_id`,
  else `job_id`).
- Worker, lane and retained-byte numbers are a named `DispatchCapacityPlan`,
  resolved from `runtime.scale` before Tokio runtimes are built: `current` →
  `DispatchCapacityPlan::CURRENT` (12 / 4 / 400 normal), `m2_max` → `M2_MAX`
  (24 / 8 / 384; never inferred from CPU count). `null` overrides inherit the
  profile; `MAGICIAN_SCALE_PROFILE` overrides the name. Leftover `workers: 3` or
  `12` with an explicit non-`current` profile is a boot error.
- Magician's YAML seed engine is `provider_isolated`: after local-prep and taking
  the per-provider permit, scheduler workers hand HTTP to a per-attempt executor
  and return. `legacy_worker_pool` is the in-crate default and restart-bound
  rollback (`process_job` holds HTTP).
- `global_cloud_concurrency` (default 16, `0` disables) caps simultaneous
  non-Ollama HTTP, shared by sync workers and streaming jobs; Ollama skips it.
  Optional `llm.dispatch.provider_quota` RPM/TPM buckets (missing/0 unlimited)
  wait after the provider permit and before the cloud cap, streaming included.
- Local-prep that would call Ollama parks on a cap-1 coordinator without
  occupying a dispatch worker (`llm.dispatch.local_prep.yield_worker: false`
  restores head-of-line blocking).
- Every request carries a local-only `LlmTraceContext` (root trace, logical call
  id, typed lineage, deterministic physical attempt ids). `LlmTraceReceipt`
  returns the final attempt and queue job identity without serializing
  principal/workspace/task/chat lineage into provider payloads.
- Terminal-output synthesis can set `TaskRef::survives_terminal_task` so the
  pre-dispatch task-state gate does not cancel the job that materializes a
  terminal deliverable (default false). Explicit cancellation by task, execution
  or root execution still applies. Cancellation diagnostics record the reason
  and matched job count; pickup tombstones include task reference, trace and
  refusal category.

Operating contract: [LLM dispatch queue](../magician/llm-dispatch-queue.md).

## Logical-context Planning

`magicllm::chunking` is the provider-neutral half of the Ollama logical-context
framework. `LLMProfile` can declare a physical context window and a disabled or
enabled `ChunkingConfig`; preflight uses a conservative estimator plus explicit
static, output and safety reserves to decide whether one request fits. These
config-only fields are stripped before provider invocation.

Large structured inputs go through `ChunkDomainAdapter`, not text slicing: the
adapter owns stable source identities, semantic oversized-item splits, map
validation, bounded repair/fallback, reduction and serialization.
`plan_logical_request` packs stable leaves into as many chunks as the physical
budget requires, rejects incomplete or duplicated coverage, and enforces the
logical window without a separate `max_chunks` limit. The crate does not schedule
calls or persist outputs; Magician owns the runner, adapters, profiles and
activation ([Ollama Logical-context Chunking](../magician/ollama-logical-context-chunking.md)).

## Streaming Support

`LLMProvider::invoke_stream(request, tx)` streams `StreamDelta`s; providers
without streaming fall back to invoke + Done. `ConfiguredRouter::route_stream()`
applies the same resolution as `route()` and streams only when the profile has
`metadata.streaming: true`.

Streaming providers: OpenAI Chat, OpenAI Responses, Anthropic, DeepSeek (via
Anthropic-compatible Messages), MiniMax (OpenAI-compatible
`/v1/chat/completions`), and OpenAI Meta (delegates by API mode).

Every completed response and terminal provider error carries `LlmRouteIdentity`
for the profile/provider/model that actually ran, including fallbacks. A
provider-emitted streaming error delta stays visible but cannot replace the
router's typed terminal error in queue metadata. If fallback selection/preflight
fails after a real attempt, the error keeps that last physical route. Breaker,
cooldown and health observations are charged to the effective terminal provider;
pre-provider failures use the initially resolved provider.

## Harness CLI provider: the child environment

`providers/harness_cli.rs` shells out to an installed coding CLI (`claude`,
`codex`, `grok`, `agy`, `pi`) for one-shot completions. Every spawn clears the
environment and passes only `PATH`, `HOME`, `USER`, `TMPDIR` and `LANG` (CLIs
read their credentials from `HOME`). Two things must never reach the child:

- **Provider keys** (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, …):
  a CLI finding one authenticates and bills as that key instead of the operator's
  own login. Key-based harness auth is explicit configuration only.
- **Nested-session markers** (`CLAUDECODE`, `CLAUDE_CODE_SESSION_ID`,
  `CODEX_COMPANION_SESSION_ID`): a nested launch that inherits them refuses or
  hangs, surfacing misleading provider errors (e.g. quota or wall-clock).

Claude, Codex and Pi read the prompt on stdin (keeps prompts out of argv, no size
limit). Grok and agy take it as the value of `-p`, so the prompt goes immediately
after that flag and format flags elsewhere (placed last, both CLIs exit 2). A CLI
exiting without output has its stderr tail carried in the error; for Pi, any
nonzero exit is an error. `execution/plane/engines/claude_code.rs::apply_env_allowlist`
applies the same policy to the turn path, plus a caller-supplied allowlist.

## Server-side Web Search

A request asks the provider to search by setting `server_web_search` in
`LLMRequest.extra` — `true` or an options object (`max_uses`, `allowed_domains`,
`blocked_domains`, `user_location`; each transport maps what it supports, e.g.
OpenRouter clamps `max_uses` to 1..=10). Mistyped values are rejected, never
degraded to off.

- **OpenAI Responses**: `web_search` server tool (and no `tool_choice`).
- **Anthropic Messages**: `web_search_20250305` server tool; Anthropic-compatible
  transports (DeepSeek) fail closed.
- **Gemini**: `googleSearch` grounding tool (generateContent and Interactions).
- **OpenRouter**: `web` search plugin.
- **OpenAI Chat, DeepSeek, MiniMax, Ollama, Yutori N1**:
  `LLMError::UnsupportedCapability`. `LLMCapability.web_search` advertises the
  same matrix.

`server_web_search` plus function tools fails closed at body build on every
supporting transport (including raw `extra["tools"]` on OpenRouter). Citations and
counts come from the retained `raw_response` via
`magicllm::server_web_search::{extract_citations, web_search_call_count}` (dedup
by URL; OpenRouter prefers billed `usage.cost_details.web_search_requests_count`).
Cost includes per-call search charges (`pricing::server_web_search_cost_per_call_at`
→ `compute_cost_with_server_web_search_at`). Profiles advertising
`server_web_search` are validated at load (a supporting transport; OpenAI must not
pin `openai_api_mode: chat`). The Magician caller is the `web_answer` compiled tool
(`op-web-answer` profile); the free DuckDuckGo `web_search` list tool stays the
no-cost lane.

## DeepSeek V4

`LLMProviderKind::DeepSeek` / `DeepSeekProvider` over the Anthropic-compatible
Messages transport (default base `https://api.deepseek.com/anthropic/v1/messages`):
V4 Flash/Pro text, thinking, streaming, JSON output and tool calls. Vision is
per model (`DeepSeekProvider::model_supports_vision`): `deepseek-flash`
(V4.1 Flash) advertises image input; V4 Pro stays text-only.

## Realtime voice

- **OpenAI Realtime (backend-proxied)** bootstraps `session.update` on WebSocket
  connect (the socket is not idle while instructions render) and surfaces a
  dropped socket as `TransportClosed`. A later spoken turn cancels any open
  `response` before `response.create`. Transcriptions from
  `input_audio_transcription.delta` / `audio_transcript.delta` are forwarded as
  growing captions (DirectPeerToPeer accumulates them in the browser).
- **Gemini Live** is a realtime assistant (same tool catalog and function-call
  dispatch) over three model families. `gemini_live_model_contract` derives the
  wire contract from the model id: `gemini-3.8-live` (code default) and
  `gemini-3.8-live-extended-thinking` declare tools `NON_BLOCKING` and send
  `scheduling` on each function response (profile `tool_result_scheduling`,
  default `when_idle`); only extended-thinking accepts `thinking_level` and
  reports `interactionStatus` (`RealtimeProviderEvent::InteractionStatus`, since
  there `turnComplete` only closes an utterance); both bill `thoughtsTokenCount`
  as text output. `gemini-3.1-flash-live-preview` keeps blocking tools.
  `usageMetadata` rides `turnComplete`, one message after `generationComplete`,
  so `ResponseDone` closes there (or on `generationComplete` once usage is known)
  and always carries usage. The factory refuses a level or scheduling hint the
  contract lacks (`NotConfigured`). Live transcriptions forward as growing
  caption snapshots. Opt-in live check: `make test-gemini-live-models-live`.
- **Gemini 3.5 Live Translate** is speech-to-speech translation on the same path:
  no tools, memory or chat turns; `selectable: false`. `setup` keeps
  `translationConfig` in `generationConfig` and transcription on `setup` itself
  (nested transcription is a 1007).
- **Grok Voice** (`provider: grok_voice`, profile `voice_realtime_grok`,
  `wss://api.x.ai/v1/realtime`, `grok-voice-latest`) reuses the OpenAI Realtime
  socket and function-tool events with Grok's flatter `session.update`.
- **GPT-Live-1** (`provider: openai_live`, profile `voice_realtime_gpt_live_1`,
  `wss://api.openai.com/v1/live/sessions`) is not a Realtime model swap: no
  `response.create` loop and no in-session function catalog. Live owns
  full-duplex turn-taking; Magician is the delegated backend
  (`delegation.type = client` → Magician's tool-using chat turn or the
  `chat.harness_engine` mouth → `session.commentary.append`). Instructions are
  `voice_live_mouth_system`, with built-in `OPENAI_LIVE_DEFAULT_INSTRUCTIONS` as
  render fallback. Speaker PCM prerolls 80 ms and emits ~40 ms frames.
- The per-turn memory/procedure injection gate
  (`provider_supports_turn_context_gate`) is OpenAI Realtime-only; Grok Voice,
  Gemini Live and GPT-Live do not share it.
- `magicllm::realtime::voices` is the speakable-voice catalog per transport;
  `create_session` overlays the Settings choice on the YAML/provider default.
- The Live transport delegates through its backend interface; engine selection
  belongs to the host application. TTS spend is not on the LLM cost path
  (`media_rails/providers/gemini_tts.rs` emits no usage into `llm_calls`).

## Invariants

`openai_api_mode` controls Chat vs Responses explicitly, locked-profile
comparisons block fallback drift, and provider tool-calling capability metadata
is fail-closed rather than inferred.
