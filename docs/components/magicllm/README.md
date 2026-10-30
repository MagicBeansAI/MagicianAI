# MagicLLM Docs

[Model catalog and thinking selection](model-catalog.md) covers generation
configuration, shared budgets, client controls and saved selection migration.

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
`MultiLLMRouter`. Lanes use the shared `runtime_core::fair_queue`; the Decision
Engine reuses it for typed model jobs with separate process-local queues.
Operating contract: [LLM dispatch queue](../magician/llm-dispatch-queue.md).

- Each High/Normal/Background lane is a bounded `FairLane`. Pickup prefers High
  over Normal over Background, with 8-consecutive-High and
  12-consecutive-foreground bounds. Within a lane the next job is the oldest of
  the next owner (`task_ref.agent_id` if non-empty, else `task_ref.task_id`,
  else `job_id`).
- Worker, lane and retained-byte numbers are a named `DispatchCapacityPlan`,
  resolved from `runtime.scale` before Tokio runtimes start: `current` →
  `DispatchCapacityPlan::CURRENT` (12 / 4 / 400 normal), `m2_max` → `M2_MAX`
  (24 / 8 / 384; never inferred from CPU count). `null` overrides inherit the
  profile; `MAGICIAN_SCALE_PROFILE` overrides the name. A leftover `workers: 3`
  or `12` with an explicit non-`current` profile is a boot error.
- The YAML seed engine is `provider_isolated`: after local prep and the
  per-provider permit, workers hand HTTP to a per-attempt executor and return.
  `legacy_worker_pool` is the in-crate default and restart-bound rollback
  (`process_job` holds HTTP).
- `global_cloud_concurrency` (default 16, `0` disables) caps simultaneous
  non-Ollama HTTP across sync and streaming jobs. Optional
  `llm.dispatch.provider_quota` RPM/TPM buckets (missing/0 = unlimited) wait
  after the provider permit and before the cloud cap, streaming included.
- Local prep that would call Ollama parks on a cap-1 coordinator without
  holding a worker (`llm.dispatch.local_prep.yield_worker: false` restores
  head-of-line blocking).
- Every request carries a local-only `LlmTraceContext` (root trace, logical call
  id, typed lineage, deterministic attempt ids). `LlmTraceReceipt` returns the
  final attempt and queue job identity; lineage never enters provider payloads.
- Terminal-output synthesis can set `TaskRef::survives_terminal_task` (default
  false) so the pre-dispatch task-state gate does not cancel the job that writes
  a terminal deliverable. Explicit cancellation by task, execution or root
  execution still applies. Cancellation diagnostics record reason and matched
  job count; pickup tombstones carry task reference, trace and refusal
  category.

**Relentless.** Its native text adapter captures an immutable
`BoundedOperationRoute` behind the local-only `single_physical_attempt` fence,
so queue and provider retries cannot create a second paid request.
`metadata.model_family` is an operator-owned independence declaration, stripped
before invocation. Context/output/timeout and the pricing row are pinned;
missing bounds or changed pricing require readmission. Other opaque metadata,
tool/search flags, multiple candidates and cached state are refused. Gemini's
output ceiling includes thinking tokens per its
[token-limit contract](https://ai.google.dev/gemini-api/docs/generate-content/thinking#token-limits-and-max_output_tokens).
See [Relentless](../magician/relentless.md).

**Resource-bound execution** uses the same physical authorizer and move-only
permit through a runtime-only `LlmPhysicalResourceGuard`, captured before queue
submission and kept across owned futures. Clones of one authority are
idempotent; distinct nested authorities deny. Serialization omits the authority
and its single-attempt fence, so resumed owners must re-attest. App and generic
resource meters together deny before I/O.

The router quotes the resolved profile, model, transport cohort and lineage
before dispatch. A pre-I/O denial releases the permit; usage settles it; missing
usage or provider errors stay uncertain. A typed HTTP client rejection
(`ProviderStatus` 4xx other than 408/429) is still uncertain but carries
`is_definitive_rejection()` with zero counters, so an owner may release the
reservation; timeouts, 5xx and untyped provider messages carry no such flag.
Queue cancellation drains settlement
first, and protected rate limits never trigger a second attempt. Local prep
keeps the raw content without calling Ollama. Streaming,
embeddings, continuations, fallbacks, logical chunking and server tools with
unqualified fee ceilings are refused under this contract.
`make test-llm-resource-guard` exercises it with the real router and queue and
provider-free fixtures.

**Gemini JSON output.** JSON object/schema formats reach `generateContent` as
`generationConfig.responseMimeType` and `responseJsonSchema`
([contract](https://ai.google.dev/gemini-api/docs/generate-content/structured-output));
Text requests keep their format. `make test-gemini-structured-output` checks the
wire against a local mock and proves one request per bounded invocation.

## Logical-context Planning

`magicllm::chunking` is the provider-neutral half of Ollama logical-context
chunking. `LLMProfile` can declare a physical context window and a
`ChunkingConfig`; preflight uses a conservative estimator plus static, output
and safety reserves to decide whether a request fits. These config-only fields
are stripped before invocation.

Large structured inputs go through `ChunkDomainAdapter`, not text slicing: the
adapter owns stable source identities, oversized-item splits, map validation,
bounded repair/fallback, reduction and serialization. `plan_logical_request` packs
leaves into as many chunks as the budget needs, rejects incomplete or
duplicated coverage, and enforces the logical window (no `max_chunks`). The
crate does not schedule calls or persist output; Magician owns the runner, adapters, profiles and activation
([Ollama Logical-context Chunking](../magician/ollama-logical-context-chunking.md)).

## Spend gate

`spend_gate::install_spend_gate` installs one process-wide `SpendGate` that every router call passes: `admit` before any provider is contacted (a refusal is `LLMError::SpendEnvelopeExhausted`, terminal for dispatch and never a breaker failure), `settle` with the priced cost afterwards. With no gate installed nothing changes. Pricing returns `None` for an unlisted model (`try_compute_cost_at`) and `estimate_unpriced_cost_microusd` charges the provider's highest known rate, so an unpriced model is not free. The owner envelope that uses it: [agentic budgets](../magician/execution/AGENTIC_EXECUTION_DESIGN.md#budgets).

## Streaming Support

`LLMProvider::invoke_stream(request, tx)` streams `StreamDelta`s; providers
without streaming fall back to invoke + Done. `ConfiguredRouter::route_stream()`
resolves like `route()` and streams only when the profile has
`metadata.streaming: true`.

Streaming providers: OpenAI Chat, OpenAI Responses, Anthropic, DeepSeek (via
Anthropic-compatible Messages), MiniMax (OpenAI-compatible
`/v1/chat/completions`), Gemini (`:streamGenerateContent?alt=sse`), OpenRouter,
Sarvam, Ollama (NDJSON) and OpenAI Meta (delegates by API mode).

Plain `invoke()` streams too (`providers/plain_stream.rs`): the provider sends
its streaming request, `StreamWatchdog` bounds the body by progress (request
timeout until the first output, then 120 s without new output, 30-minute
ceiling), and the provider's own non-streaming body is rebuilt from the stream
for the unchanged parser and max-tokens retry. A long answer that keeps
producing is not cut at the request timeout; a stream that ends before its
terminal event is a `Transport` error. Stays non-streaming: Responses
`background: true` and Gemini Interactions. An error status or a JSON reply is
parsed as before.

Every completed response and terminal error carries `LlmRouteIdentity` for the
route that actually ran, fallbacks included. A provider's streaming error delta
stays visible but cannot replace the router's typed terminal error in queue
metadata. If fallback selection/preflight fails after a real attempt, the
error keeps that last physical route. Breaker,
cooldown and health are charged to the effective terminal provider;
pre-provider failures use the initially resolved one.

## Anthropic structured outputs

Native Anthropic Messages maps `LLMResponseFormat::JsonSchema` to
`output_config.format` in all three invocation paths, keeping the schema and
thinking effort; extra fields cannot replace it. `JsonObject` without a schema
is rejected before I/O, as are structured formats on unqualified
Anthropic-compatible transports. Callers must stay inside the
[supported JSON Schema subset](https://platform.claude.com/docs/en/build-with-claude/structured-outputs)
and validate meaning locally; the transport never strips constraints silently.
Run `make test-anthropic-structured-output`.

## Harness CLI provider: the child environment

`providers/harness_cli.rs` runs an installed coding CLI (`claude`, `codex`,
`grok`, `agy`, `pi`) for one-shot completions. Every spawn clears the
environment and passes only `PATH` (`/usr/bin:/bin:/usr/sbin:/sbin` when unset
or empty), `HOME`, `USER`, `TMPDIR` and `LANG`, plus `PI_CODING_AGENT_DIR` for
Pi (`apply_child_env`; CLIs read credentials from `HOME`). Not passed, among
everything else:

- **Provider keys** (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, …):
  a CLI that finds one bills that key instead of the operator's login.
  Key-based harness auth is explicit configuration only.
- **Nested-session markers** of a coding harness Magician itself runs under
  (e.g. Claude Code's `CLAUDECODE`): a nested launch that inherits them refuses
  or hangs until its wall clock.

Claude, Codex and Pi read the prompt on stdin. Grok and agy take it as the
value of `-p`, so the prompt goes right after that flag (format flags placed
last make both exit 2). A CLI that exits without output carries its stderr
tail in the error; for Pi any nonzero exit is an error.
`execution/plane/engines/claude_code.rs::apply_env_allowlist` applies the same
policy to the turn path, plus a caller-supplied allowlist.

## Server-side Web Search

Set `server_web_search` in `LLMRequest.extra` to `true` or an options object
(`max_uses`, `allowed_domains`, `blocked_domains`, `user_location`; each
transport maps what it supports, e.g. OpenRouter clamps `max_uses` to 1..=10).
Mistyped values are rejected, never treated as off.

| Transport | Mapping |
|---|---|
| OpenAI Responses | `web_search` server tool (no `tool_choice`) |
| Anthropic Messages | `web_search_20250305`; Anthropic-compatible transports (DeepSeek) fail closed |
| Gemini | `googleSearch` grounding (generateContent and Interactions) |
| OpenRouter | `web` plugin |
| OpenAI Chat, DeepSeek, MiniMax, Ollama, Yutori N1 | `LLMError::UnsupportedCapability`; `LLMCapability.web_search` advertises the same matrix |

`server_web_search` plus function tools fails closed at body build (including
raw `extra["tools"]` on OpenRouter). Citations and counts come from
`raw_response` via
`magicllm::server_web_search::{extract_citations, web_search_call_count}`
(dedup by URL; OpenRouter prefers billed
`usage.cost_details.web_search_requests_count`). Cost adds per-call charges
(`pricing::server_web_search_cost_per_call_at`, file `web_search_rates` →
`compute_cost_with_server_web_search_at`). Profiles advertising
`server_web_search` are validated at load (a supporting transport; OpenAI must not pin
`openai_api_mode: chat`). The Magician caller is the `web_answer` compiled tool
(`op-web-answer`); the free DuckDuckGo `web_search` tool stays the no-cost lane.

## DeepSeek V4

`LLMProviderKind::DeepSeek` / `DeepSeekProvider` over Anthropic-compatible
Messages (default base `https://api.deepseek.com/anthropic/v1/messages`): V4
Flash/Pro text, thinking, streaming, JSON output and tool calls. Vision is per
model (`DeepSeekProvider::model_supports_vision`): `deepseek-flash` (V4.1
Flash) takes images; V4 Pro is text-only.

## Realtime voice

Provider WebSockets use an explicit rustls crypto backend with WebPKI certificate
verification, so dependency feature unification cannot panic during voice startup.

- **OpenAI Realtime (backend-proxied)** sends `session.update` on connect and
  surfaces a dropped socket as `TransportClosed`. A new spoken turn cancels any
  open `response` before `response.create`. `input_audio_transcription.delta` /
  `audio_transcript.delta` forward as growing captions (DirectPeerToPeer builds
  them in the browser).
- **Gemini Live** is a realtime assistant (same tool catalog and dispatch).
  `gemini_live_model_contract` derives the wire contract from the model id:
  - `gemini-3.8-live` (code default) and `gemini-3.8-live-extended-thinking`
    declare tools `NON_BLOCKING` and send `scheduling` on each function response
    (profile `tool_result_scheduling`, default `when_idle`); both bill
    `thoughtsTokenCount` as text output.
  - Only extended-thinking accepts `thinking_level` and reports
    `interactionStatus` (`RealtimeProviderEvent::InteractionStatus`, since there
    `turnComplete` only closes an utterance).
  - `gemini-3.1-flash-live-preview` keeps blocking tools.
  - `usageMetadata` rides `turnComplete`, one message after
    `generationComplete`, so `ResponseDone` closes there (or on
    `generationComplete` once usage is known) and always carries usage. The
    factory refuses a level or scheduling hint the contract lacks
    (`NotConfigured`). Live transcriptions forward as growing caption
    snapshots. Opt-in live check: `make test-gemini-live-models-live`.
- **Gemini 3.5 Live Translate**: speech-to-speech translation on the same path;
  no tools, memory or chat turns; `selectable: false`. `setup` keeps
  `translationConfig` in `generationConfig` and transcription on `setup` itself
  (nested transcription is a 1007).
- **Grok Voice** (`provider: grok_voice`, profile `voice_realtime_grok`,
  `wss://api.x.ai/v1/realtime`, `grok-voice-latest`) reuses the OpenAI Realtime
  socket and tool events with Grok's flatter `session.update`.
- **GPT-Live-1** (`provider: openai_live`, profile `voice_realtime_gpt_live_1`,
  `wss://api.openai.com/v1/live/sessions`) has no `response.create` loop and no
  in-session tool catalog. Live owns full-duplex turn-taking; Magician is the
  delegated backend (`delegation.type = client` → Magician's chat turn or the
  `chat.harness_engine` mouth → `session.commentary.append`). Instructions are
  `voice_live_mouth_system`, falling back to `OPENAI_LIVE_DEFAULT_INSTRUCTIONS`.
  Speaker PCM prerolls 80 ms and emits ~40 ms frames.
- The per-turn memory/procedure injection gate
  (`provider_supports_turn_context_gate`) is OpenAI Realtime-only.
- `magicllm::realtime::voices` is the voice catalog per transport;
  `create_session` overlays the Settings choice on the YAML/provider default.
- Engine selection belongs to the host application. TTS spend is not on the LLM
  cost path (`media_rails/providers/gemini_tts.rs` emits no usage into
  `llm_calls`).

## Invariants

`openai_api_mode` controls Chat vs Responses explicitly, locked-profile
comparisons block fallback drift, and provider tool-calling capability metadata
is fail-closed, never inferred.

Realtime voice catalogs were audited on 2026-10-15: Grok Voice exposes all 28 IDs returned by xAI’s `GET /v1/tts/voices` (the roster is shared with TTS); Gemini 3.1 Live exposes 30 studio voices and Gemini 3.8 Live exposes all 2,089 public prebuilt IDs. OpenAI Realtime keeps its distinct 10-voice roster, and GPT Live its 31-voice roster. Provider-specific canonicalization and validation use these same catalogs before session creation.

Gemini 3.8’s extended catalog is shared with TTS through `speech_voices`; `voices_for_realtime_model` and `canonical_realtime_voice_for_model` keep discovery, preference validation, and session creation consistent. Gemini 3.1’s named studio roster remains compatible with its transport. OpenAI Live’s 31 built-ins come from the create-session API reference, including the additional regional voices; OpenAI Realtime retains its distinct 10 built-ins.
