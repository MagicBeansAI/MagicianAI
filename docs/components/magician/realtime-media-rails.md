# Realtime Media + Control Rails

Shared substrate for browser TTS, mobile camera/mic capture, provider TTS/STT,
realtime voice, the native mascot/tray screen/pointer/system-audio bridge, and
the Google Meet participant bot.

**Sampled-artifact review.** `SttProvider` has optional
`review_configuration_hash` and `transcribe_bounded_review` ports; defaults
refuse dispatch. Opting in means pinning the exact model/configuration with
bounded, cancellable, zero-cost preparation. Relentless lists only opted-in
providers, freezes segments, limits and ffmpeg identity, seals sample/receipt
bytes, never resends unknown dispatches (an unknown transcript keeps WAV
identity and levels), and drains all preparation on cancel. Ordinary speech
fallback is unchanged.

Related: archived plan
(`docs/archive/plans/2026-05-15-realtime-media-control-rails.md`),
`fluid-audio-phase0-baseline.md`,
[`fluid-audio-phase9-rollout.md`](fluid-audio-phase9-rollout.md) (engine
defaults, physical gates),
[`tray-bridge-protocol.md`](realtime-media/tray-bridge-protocol.md),
[`mac-notch-orb.md`](../desktop/mac-notch-orb.md),
[`screen-capture-and-ask.md`](screen-capture-and-ask.md),
[`magicllm/README.md`](../magicllm/README.md) (realtime pricing).

Live config: `$MAGICIAN_ROOT_DIR/magician-config.yaml` (seed: repo-root
`magician-config.yaml`). Restart with `make restart-magician` (there is no
`start-magician`).

## Architecture

A client surface registers a `RealtimeSession` (`POST /media/sessions`) on
mount and emits `media.*` events through `RuntimeTransportBroadcaster` (UI
activity card, `/events`, dashboards). Provider plane: `VadProvider`,
`TtsProvider`, `SttProvider`, `StreamingSttProvider`, `DiarizationProvider`
(built at boot) and `magicllm::realtime::RealtimeProvider` (per call from
`realtime_voice`). OpenAI providers need
`OPENAI_API_KEY`. Host-backed local providers register only when
`MAGICIAN_HOST_GATEWAY_URL` (normally `http://127.0.0.1:3017`) answers
`GET /host/speech/status` (`macos_tts` needs the helper; `macos_speech` also
Apple Speech authorization); the desktop tray binds the gateway before
starting Magician.

Realtime voice resolves per call via
`OperationLlmRouter::resolve_realtime_provider` against `realtime_voice`;
host-native clients may pin `realtime_profile` (or `profile`) in
`session.start`. `voice_mode=hands_free` is the local FluidAudio → Chat → TTS
cascade; `voice_mode=realtime` the named vendor profile.

**Which chat a call posts into.** Clients send the chat on screen as
`session.start.chat_session_id` (web from the chat store, iOS and Android from
the open chat). The orchestrator uses it when it is the caller's open chat
with the call's agent (not archived, not a concurrent-voice branch), and the
call takes that chat's `ui_thread_id`. Otherwise, and for callers with no chat
(wake word, war room, thinking map), it falls back to the active chat of
`ui_thread_id`. That fallback alone is ambiguous: a thread with chat tabs has
several active chats. `session.start.new_chat=true` (the desktop Orb) gets a new chat in `ui_thread_id` instead (`CallChat::Fresh`).

**Turn boundaries.** `turn_boundary=server_vad|push_to_talk` maps to provider
turn detection (`server_vad` / `none`) and persists across rotations; the Mac
Orb always uses `server_vad` for Live. Otherwise `ptt.release` commits;
backend-proxied, a release with under
`MIN_PROVIDER_COMMIT_AUDIO_BYTES` (~100 ms) of PCM clears input and emits
`transcript.user.ignored` (`reason: "no_audio"`) instead of
`CommitInputAndRespond`, so the model never replies to an empty turn.

**Readiness.** Capture queues until the backend-proxied provider emits
`TransportReady`; the actor never awaits the provider's bounded channel and
fails closed after 10 s (OpenAI handshake: 8 s). A fatal
Mac Orb Live failure falls back once, non-recursively, to Hands-free.

Local Hands-free TTS keeps a 300 ms lead (`PCM_INITIAL_LEAD_MS`), one segment
ahead; desktop Hands-free and Dictation prebuffer 180 ms
(`LIVE_PTT_PLAYBACK_PREBUFFER_MS`).

**Native mascot.** Magician owns session, ledger, events and bridge socket;
the Tauri host gateway owns the process; the Swift Orb listens on
`127.0.0.1:3027`. Recording/bubble endpoints are render-only (the tray owns
capture, STT, chat posting).

`magician-media` re-exports the seam (`pub use magician::magician_v2::media_seam::*`)
as `media_rails::*`; the defining file is authoritative.

## Backend modules

| Module | What it owns |
| --- | --- |
| `magician-media/src/media_rails/mod.rs` | `RealtimeSession`, `RealtimeSessionRegistry`, `SurfaceType`, `SurfaceCapabilities`, `MediaPermissions`, event constants. |
| `magician/src/magician_v2/media_seam/audio_surface.rs` | Audio surfaces, stage/profile config, provider options, resolved snapshots. |
| `magician/src/magician_v2/media_seam/runtime_config.rs` | Revisioned snapshot, validation, precedence, migration, transactional config writes. |
| `magician-media/src/media_rails/fluid_audio/` | Supervised sidecar, authenticated IPC, VAD/STT/diarization/Kokoro adapters. |
| `magician/src/magician_v2/media_seam/vad_gate.rs` | Meeting/Listening VAD gate: pre-roll, segmented STT, ordered finalization, optional fail-open. |
| `magician/src/magician_v2/media_seam/streaming_stt_types.rs` | `StreamingSttProvider` / `StreamingSttSession` (re-exported as `media_rails::providers::streaming_stt`). |
| `magician-media/src/media_rails/providers/` | Concrete adapters + `MediaProviderRegistry`. |
| `magician-media/src/media_rails/voice_orchestrator.rs` | Per-call realtime state machine (mint, rotate, compact, replay, dispatch). |
| `magician-media/src/media_rails/voice_context_compactor.rs` | Chat ledger → compacted summary. |
| `magician-media/src/media_rails/voice_session_lifecycle.rs` | Process-local lifecycle store. |
| `magician-media/src/media_rails/voice_downstream_fanout.rs` | Voice-session-id → control-WS fan-out. |
| `magician-media/src/media_rails/voice_tool_dispatcher.rs` | `create_task` / `get_task_status` / `stop_task`. |
| `magician-media/src/media_rails/self_echo.rs` | Assistant-utterance tracker + echo guard. |
| `magician-media/src/media_rails/voice_addressing.rs` | Wake-prefix + follow-up window. |
| `magician-media/src/media_rails/hands_free.rs` | Cascaded Hands-free provider. |
| `magician-media/src/media_rails/screen_observe.rs` | Host and client-pushed screen observation. |
| `magician-api/src/media_api.rs` | REST: sessions, providers, preferences, audio-settings, TTS/STT, voice-notes. |
| `magician-api/src/media_ux/session_controls.rs` | `voice_source_surface`, `voice_surface_derivation`, turn-boundary / prefix parsing. |
| `magician-api/src/voice_control_handler.rs` | WebSocket actor per realtime call. |
| `magician-api/src/tray_bridge_handler.rs` | WebSocket actor for tray/mascot bridge. |
| `magician-api/src/meetings_api.rs` | Meetings HTTP. |
| `magicllm/src/realtime/` | `RealtimeProvider` trait, OpenAI/GPT Live/Gemini adapters, voice catalog, factory. |
| `magician-event-taxonomy/src/lib.rs` | `EventCategory::Media` + `media.*` rows. |
| `magician-bin/src/main.rs` | Route registration under `/api/magician/v2`. |

Meeting modules: see [Meeting participant bot](#meeting-participant-bot).

## Surfaces

`SurfaceType` (serde `snake_case`): `web_mobile`, `web_desktop`,
`mascot_macos` / `mascot_windows` / `mascot_linux`, `tray_macos` /
`tray_windows` / `tray_linux`, `extension`, `esp_terminal` (`EspTerminal`, the
ESP32-C6 PTT terminal in `magesp/`), `meeting_bot`, `unknown` (default). An
unrecognized string resolves to `Unknown`.

## HTTP surface

Under `/api/magician/v2/media`, scope-isolated by the workspace bearer.

| Method | Path | Purpose |
| --- | --- | --- |
| POST   | `/sessions` | Register a surface |
| GET    | `/sessions` | List active surfaces in scope |
| GET    | `/sessions/{id}` | Snapshot |
| PATCH  | `/sessions/{id}` | Update capabilities / permissions |
| POST   | `/sessions/{id}/heartbeat` | Refresh liveness |
| DELETE | `/sessions/{id}` | Disconnect or revoke |
| POST   | `/sessions/{id}/events` | Publish a whitelisted client event |
| GET    | `/providers` | `{ tts, tts_fallbacks?, stt, stt_fallbacks?, realtime_voice? }`; TTS rows carry default format, voices/formats, truthful streaming capability. `realtime_voice` is set when `resolve_realtime_provider(VoiceController)` succeeded at boot (gates `VoiceCallButton`); profiles list topology, mode, voices, models, availability. |
| GET    | `/audio-settings` | Audio catalog, profiles/defaults, engine availability, revision. |
| PUT    | `/audio-settings` | Revision-checked; persists only the changed `media:` values (comments kept) and swaps the snapshot. FluidAudio disable drops leases, streams and TTS cache, stops an owned sidecar (external: unload models, not killed). Enable verifies host support; no restart. |
| POST   | `/audio-engines/{engine_id}/models/{action}` | Prewarm/unload. |
| GET    | `/surfaces/{surface}/resolved` | Effective profile for a scope, with optional hints. |
| GET    | `/preferences` | Schema-v3: `auto_speak`, `voice_mode`, `require_voice_prefix`, `surface_profiles`, `surface_stage_options`, `realtime_voices`. |
| PUT    | `/preferences` | Off-catalog options rejected; `realtime_voices` pins a voice per profile (`default`/empty clears). Emits `media.preferences.updated`. |
| POST   | `/tts/synthesize` | Text → audio; optional `provider` uses chain semantics. |
| POST   | `/tts/synthesize_message` | Streamed TTS for `<speech>` segments. |
| POST   | `/tts/cache/clear` | Drop TTS cache. |
| GET    | `/tts/cache/stats` | Hit / miss / eviction counters. |
| POST   | `/stt/transcribe` | Multipart audio → transcript; `provider` reorders the chain. |
| POST   | `/stt/transcribe/stream` | Streaming recording-mode STT. |
| POST   | `/voice-notes` | Audio + chat metadata → STT → chat turn. |
| POST   | `/voice-notes/events` | Allowlisted tray voice-note events. |
| GET    | `/voice/{voice_session_id}/control` | Realtime voice control WebSocket. |
| GET    | `/bridge/{session_id}/ws` | Tray/mascot bridge WebSocket. |

Meetings (`/api/magician/v2/meetings`) and `/api/magician/v2/screen/observe/…`
are below.

**Voice notes.** Without a `chat_session_id` a note joins the thread's open chat, or starts a new one with `new_chat=true`.
`POST /voice-notes` returns transcript, preview and
`assistant_speech_segments` when the reply had `<speech>` (host-native feeds them
to `/tts/synthesize_message`; browsers use persisted `speech_segments`). Chat cancel/panic emits
`media.voice_note.chat_submit_failed` + `media.voice_note.error` and returns
`chat_submit_failed`. `unsupported_language` / `empty_transcript` 422s include
`chat_session_id`, `error`, `message`, optional `audio_artifact_id`. macOS "No
speech detected" is `SttError::NoSpeech` (terminal: no fall-through to Whisper).
A transcript whose letters are more than 20% non-Latin is rejected before
chat (`unsupported_language`, "English (and Hinglish) only";
`transcript_is_supported_language`). Magician stays the authority for STT, chat insertion, artifacts, audit.
Events:
`media.voice_note.{received,
recording_started, recording_stopped, recording_failed, transcription_started,
transcription_completed, transcription_failed, transcribed, chat_submit_started,
chat_submit_completed, chat_submit_failed, submitted, error}` (`transcribed`,
`submitted` are compatibility names).

VAD-gated Meeting/Listening emit content-free diagnostics:
`media.audio.profile.{resolved,degraded}`, `media.audio.vad.{speech_started,
speech_ended}`, `media.audio.provider.fallback`, `media.audio.frames.dropped`.

### `surface_type` decides who the agent thinks it is talking to

`voice_source_surface` maps `surface_type` to the turn's `source_surface`,
then an `InvocationSurface` that decides which agents may answer.

| Registration | Resolves to |
|---|---|
| `meeting_bot` | The room; only agents listing `meeting` in their invocation policy |
| `unknown` (default), `extension` | The room, reason `unrecognised_registration_fail_closed` |
| `web_*`, `tray_*`, `mascot_*`, `esp_terminal` | Owner voice (`realtime_voice` / `global_live_ptt` / `mascot`) |

Meet registers `meeting_bot` (seats Envoy). A WhatsApp call registers
`web_desktop` and seats `personal-assistant` only when the caller's number
matches the configured owner number; otherwise `meeting_bot`. Public calls keep
that agent's tools (`delegate_to_chat` runs as it; no owner Chrome for search),
and get no learned procedures, no `delegate_to_agent` / `handover_to_agent`,
and not the voice slot that speaks cockpit diff approvals.

An owner surface requires a server-owned `VerifiedRequestIdentity` (Cloudflare
Access, paired device, or trusted loopback) from `verify_access_middleware`;
unverified callers resolve to `unknown` whatever they claim, even when
Cloudflare is unconfigured or below `Require`. Narrowing to `meeting_bot` is
always admitted. The surface survives rotation, reconnect and resume
(`rotate()` mutates `CallState` in place).

### The surface decides which agent the call binds

Owner voice binds `VOICE_CHAT_AGENT_ID` (`personal-assistant`); a room binds
`ChatService::room_agent_id` (`EnvoyConfig::envoy_agent_id`, default `envoy`)
into `CallState::chat_agent_id`, which the chat binding and
`authorize_external_tool_call` read. No envoy → refused before any upstream
session. Use `CallState::chat_agent_id` or `call_agent_id()` (takes the lock;
never call under one), never the constant.

### Retrieval and prompt authority follow the surface

`voice_invocation_surface()` (not the thread name) drives retrieval;
memory authority keys on `surface.audience()`, and both retrieval sites pass
the surface to `chat_context_retrieval_request`.
`get_or_create_active_session` reuses only on exact agent match (a room cannot
attach to the owner's session).

**What a room may see.** Only its own meeting's summary, decisions and action
items, via `RealtimeTurnContext::meeting_context` (`injectable_context()`),
re-injected when the `CallState` digest changes; nothing else (owner calls on
a meeting thread get none). Teardown files
takeaways under `meeting:<thread-id>` in USER tier `user.research_findings`
with no agent; `user_memory_isolation: fully_isolated` redirects the
ambassador's reads to `agents/<agent>/memory/user_memory/`. Same-day rejoin
reuses the thread (`initial_resume_context`); gaps: next-day rejoin is a new
thread, and archiving before rejoin strands the transcript.

**A room reads no agent memory.** `surface_may_read_agent_memory` is false for
`Meeting` (prompt and retrieval). `PublicEnvoy` keeps its memory under
`fully_isolated`. `memory_tiers: []` is not a seal: the ambassador is
`kind: personal`, so `apply_defaults()` injects six tiers.

**Boundary.** `session.ready` carries `boundary`
`{surface, audience, agent_id, elevatable}` (`elevatable` always `false`; no
client field can request another surface; absent = unknown, never owner) and
`session.ready.agent` (presentation identity, so captions follow renames).

**Addressing.** `require_voice_prefix` defaults `true`: `session.ready.addressing`
advertises `Hey <alias>` and an 8 s follow-up window; only prefixed transcripts
(case/punctuation tolerant, token-exact) are admitted; prefix-only arms the
next. Rejected speech interrupts output, is not persisted; direct browser providers
mirror the gate, backend admission is authoritative. `session.start` may override (iOS: `false` for manual calls).

**Self-echo.** `media_rails::self_echo` estimates playback windows from
provider→client PCM (24 kHz mono PCM16; direct P2P: 15 s). Checked before the
address gate: drop when ≥80% of tokens match a tracked utterance within the
window + 2 s (one/two-token transcripts: exact tail match); interruption
collapses the window. Reason `self_echo`.

**Telemetry.** `media.voice.surface.resolved` (once at ready; content-free,
thread hashed): `voice_session_id`, `source_surface`, `surface`, `audience`,
`derivation_reason`, `agent_id`, `binding`, `tools_admitted`,
`per_turn_context_enabled`. Lifecycle (`Media`, `user_relevant=false`):
`media.config.updated`, `media.voice.surface.resolved`,
`media.voice.session.{minted,rotated,compaction,reconnect_attempt,reconnect_failed}`
(last is Warn); `rotated` carries post-rotation surface/audience/agent/binding.
`CallState::apply_rotation` is the only cross-rotation mutator. Refusals
(session/agent mismatch, absent from snapshot, trust denial) count via
`ExternalToolRefusal` and name only caller input (no enumeration oracle). Sessions do not survive restart (`CallState` built in `start()`).

**Answering.** Owner voice answers from authorized scoped context without
blanket privacy refusals; meetings keep the private-memory prohibition. Memory
and procedure stages share one realtime deadline (finished evidence survives
a sibling timeout; barge-in discards the cancelled turn's); Chat, not the
provider session, owns the result. Tools use Chat's post-redaction
materializer; `spoken_fields` select narration. OpenAI Realtime caps
`item.call_id` at 32 chars, so replay uses
`openai_realtime_replay_call_id`. Gemini Live cold rotation without a resume
handle omits function replay.

### Audio profile migration boundary

Dictation, Meeting, Listening and Hands-free resolve only through surface
profiles. Old installs migrate once into `magician-config.yaml`; preferences
are schema v3 (legacy provider/observe-mode fields read only while migrating).
Corrupt/future-version documents fail visibly; removed fields are rejected.

Web TTS and recording endpoints (empty/`auto`/`default` `provider`) follow
Dictation (browser speech only with no backend TTS); a concrete provider wins
for that request. Native callers pass `profile` + repeated `stage_option=stage:option_id`
(TTS: `audio_profile`, `audio_stage_options`); an explicit profile ignores
scoped overrides (explicit request options still reorder its chain). Hands-free freezes `resolved_audio_profile` at
registration; realtime follows the LLM router.

Defaults (see [`fluid-audio-phase9-rollout.md`](fluid-audio-phase9-rollout.md)):

| Surface | Configured default |
| --- | --- |
| Dictation | `dictation-default-v1` (`dictation-fluid-qwen3-v1` opt-in) |
| Meeting | `meeting-vad-gated-v1` |
| Listening | `listening-vad-gated-v1` |
| Hands-free | `hands-free-local-fluid-v1` (capability-gated) |

Hands-free is advertised only when every stage has a provider. Physical
echo-loop and barge-in testing is a release gate.

## Realtime Voice Topologies

- `direct_peer_to_peer`: OpenAI browser call; Magician mints and sends
  instructions/tools over the control WS, audio goes browser↔vendor WebRTC.
- `backend_proxied`: host-native and server-stream providers; control WS
  carries PCM, backend controls and provider events.

`realtime_voices` overlays a per-profile voice on the next mint. iOS filters
out direct-WebRTC profiles. `realtime_voice.operation_mapping.voice_controller`
and `default_profile` → `voice_realtime_default` (`gpt-realtime-2.1-mini`,
`openai_realtime`, `server_vad`). Host-native PTT and the meeting responder use
`voice_realtime_openai_backend` (`gpt-realtime-2.1`, `transcription_model:
local`); `voice_realtime_openai_backend_mini` is unmapped.

### Local transcription for backend-proxied calls

With `transcription_model: local`, a call-scoped Hands-free STT stream
(FluidAudio Parakeet → macOS Speech → OpenAI streaming) gets every 24 kHz
frame alongside the model, and `ConfigureSession` sets provider transcription
`null`. On failure `transcription_fallback_model` is restored (acknowledged
before queued audio is released).

Partials relay as `transcript.user.partial` (never persisted). Finals wait
for the boundary (PTT release or `speech.stopped`) and a local STT flush
before `CommitReady`, then pass `admit_user_transcript` as `transcript.user`. `transcript.user.cleared` removes an unfinished row;
`transcript.user.ignored` is semantic rejection only. PTT skips profile VAD.
FluidAudio `finish()` awaits teardown (2 s after a 15 s ack); VAD segments
await the previous segment's barrier.

Events `media.voice.local_transcript.{state,queue,turn,fallback}`
(`queue`/`fallback` Warn); rotations via
`magician::metrics::streaming_stt_fallback` and `media.audio.provider.fallback`.
Tests: `make test-realtime-local-transcript`; live
`make test-realtime-local-transcript-live REALTIME_LOCAL_TRANSCRIPT_SESSION_EVIDENCE=…`.

### Vendor profiles

Gemini Live: same control-WS contract, `GEMINI_API_KEY` server-side;
`inputTranscription` → `transcript.user.partial`, `outputTranscription` →
cumulative `transcript.assistant.delta`. Responses close on `turnComplete`
(which carries `usageMetadata`), not `generationComplete`, or they price at $0.

| Profile | Model | Contract |
| --- | --- | --- |
| `voice_realtime_gemini_38_live` | `gemini-3.8-live` | Tools `behavior: NON_BLOCKING`; `functionResponse.scheduling` from `tool_result_scheduling` (`when_idle` default, `interrupt`, `silent`). Every array needs `items` (else 1007 before `setupComplete` and endless rotation); sanitizer and pack loader add `items: {}`. `thoughtsTokenCount` billed as text output. |
| `voice_realtime_gemini_38_live_thinking` | `gemini-3.8-live-extended-thinking` | `thinking_level` (`low`/`medium`/`high`; `minimal` rejected) → `generationConfig.thinkingConfig.thinkingLevel`. Ack ends with `turnComplete` + `interactionStatus: IN_PROGRESS`, answer with `IDLE`; Magician forwards `interaction.status` (`in_progress`/`idle`) after `response.done` for "Working…" (web `assistantWorking`, Android `RealtimeVoiceState.assistantWorking`, iOS `RealtimeVoiceClient.assistantWorking`). Each generation billed. Factory refuses `thinking_level` elsewhere and `tool_result_scheduling` on 3.1 (shown unavailable). Brain only via `delegate_to_chat`. |
| `voice_realtime_gemini_live` | `gemini-3.1-flash-live-preview` | Blocking tools, no thinking or status. `display_order: 100` sorts it last (unset 0, then id). |
| `voice_realtime_grok` | `grok-voice-latest` (`grok_voice`) | `wss://api.x.ai/v1/realtime`, backend-proxied, flatter `session.update`, OpenAI-style function events, outside the turn-context gate. |
| `voice_realtime_gpt_live_1` | `gpt-live-1` (`provider: openai_live`) | See below. |
| `voice_realtime_gemini_translate_en` | `gemini-3.5-live-translate-preview` | Translation: no instructions, tools, prefix gating or retrieval; `selectable: false`. |

**GPT Live 1** (`wss://api.openai.com/v1/live/sessions`). Live owns
full-duplex turn-taking; Magician is the delegated backend
(`delegation.type = client` → `delegate_to_chat` →
`session.commentary.append`); `session.instructions` is the isolated
`voice_live_mouth_system` prompt. No tool list (Magician still builds the
catalog snapshot so `delegate_to_chat` can authorize, via
`VoiceOrchestrator::session_policy_snapshot`, without rendering the chat
prompt or waiting on its memory retrieval), no `response.create`, outside the
turn-context gate. The brain is the chat turn per
`chat.harness_engine`, persisting only the current utterance (`<speech>`
stripped, `delegation_id` tracked, ack via `thinking.append`). PCM: 80 ms
preroll, ~40 ms pacing. **Billing is per second**: after the run loop ends,
one `ResponseDone` carries `RealtimeUsage.billed_seconds` (from
`session.usage.updated` / `session.closed.usage`, else held time, `Estimated`);
on end the handler sends `End` and waits `PROVIDER_CLOSING_REPORT_GRACE`.
`speech_start_interrupts_a_response` cancels only during playback (PTT is a
deliberate barge-in). Gated by `make test-voice-harness-live`
(`voice_call_row`). GPT Realtime and Gemini keep `CHAT_OUTER_LOOP` plus
`voice_modality_addendum`.

Gemini tests: `make test-gemini-live-models`; billable
`make test-gemini-live-models-live` (report
`coverage/evals/gemini-live-models/latest/report.jsonl`).

Profile change = fresh upstream session, same thread. Google resume handles
beat replay; `goAway` → `session.expiring` → rotation. `turn_detection_mode: none` uses Magician PTT
(`activityStart`/`activityEnd`). Direct WebRTC changes turn detection in place;
backend-proxied rotates with a new `session.turn_boundary` and rolls back on
failure. Web keeps the PTT/open-mic choice; iOS keeps these choices on-device.
Live/Hands-free owns audio focus: non-call narration cancelled, off-call reply
speech suppressed, persisted auto-speak never changed.

OpenAI Realtime mints via `POST /v1/realtime/client_secrets` (`ek_…`, 1-minute
TTL); beta `/v1/realtime/sessions` returns `beta_api_shape_disabled`.
Voice-only `delegate_to_chat` resolves `chat_completion` (pinning an adaptive
composite's `thinking_profile`) and advertises no tools.

## Frontend modules

| Module | What it owns |
| --- | --- |
| `ui/unified-ui/src/lib/media/types.ts` | TS mirror of backend types. |
| `ui/unified-ui/src/lib/media/session.ts` | Register on mount, heartbeat, events. |
| `ui/unified-ui/src/lib/media/providers.ts` | `/media/providers` snapshot. |
| `ui/unified-ui/src/lib/media/preferences.ts` | Schema-v3 preferences store incl. `realtime_voices`. |
| `ui/unified-ui/src/lib/media/LiveCallVoicePanel.svelte` | Engine voices (GPT Realtime, Gemini Live, GPT Live 1). |
| `ui/unified-ui/src/lib/media/WhatsAppCallSettings.svelte` | WhatsApp call engine (composer Call catalog; next call). |
| `ui/unified-ui/src/lib/media/MeetCallSettings.svelte` | Meet call engine; Observe Join sends `realtime_profile`. |
| `ui/unified-ui/src/lib/media/tts/browserTts.ts` | `window.speechSynthesis` wrapper. |
| `ui/unified-ui/src/lib/media/tts/providerChoices.ts` | TTS provider preference. |
| `ui/unified-ui/src/lib/media/tts/providerTts.ts` | `/synthesize` / `/synthesize_message` → `<audio>`. |
| `ui/unified-ui/src/lib/media/stt/sttClient.ts` | `/transcribe`. |
| `ui/unified-ui/src/lib/media/voice/realtimeVoiceClient.ts` | `voiceCallStore`; control WS + WebRTC for `direct_peer_to_peer`. |
| `ui/unified-ui/src/routes/(app)/debug/voice/+page.svelte` | Operator view of sessions and event tails. |
| `ui/unified-ui/src/lib/media/voice/TrayLiveVoiceBanner.svelte` | Desktop live-PTT banner. |
| `ui/unified-ui/src/lib/media/capture/{camera,mic}.ts` | `<input capture>` and `MediaRecorder` helpers. |
| `ui/unified-ui/src/lib/magician/components/chat/{SpeakButton,AutoSpeakToggle,CameraCaptureButton,MicCaptureButton,VoiceCallButton}.svelte` | Composer + bubble affordances. |

## Provider configuration

TTS: `media.tts.providers`; recorded STT: `media.recording_stt.providers`.
With none verified, clients use browser TTS / staged audio.

### TTS provider selection

Request `provider` overrides one request; `auto`/`default`/empty uses the
scoped Dictation TTS stage; a concrete provider moves to the chain front. Live
calls are unaffected.

### TTS — local macOS (`macos_tts`)

First in the chain when `GET /host/speech/status` reports TTS. `POST /host/speech/synthesize` runs `magician-macos-speech-helper synthesize`
(`AVSpeechSynthesizer.write`); output is always `audio/wav`. Voice hints may be
identifiers, names or locales; unknown → locale voice.

### TTS — configured remote providers (`media.tts`)

```yaml
media:
  tts:
    providers:
      - id: openai
        adapter: openai_speech
        model: gpt-4o-mini-tts
        voice: alloy
        format: mp3
        api_key_env: OPENAI_API_KEY
```

| Adapter | Seed entry (`model` / `voice` / key) | Endpoint | Notes |
| --- | --- | --- | --- |
| `openai_speech` | `gpt-4o-mini-tts` / `alloy` / `OPENAI_API_KEY` | `POST /v1/audio/speech` | `model`, `voice`, `format`, `api_key_env`, optional `base_url`. |
| `minimax_t2a_v2` | `speech-02-hd` / `female-yujie-jingpin` / `MINIMAX_API_KEY` | MiniMax T2A V2 | Plus `group_id_env` (`MAGICIAN_MINIMAX_GROUP_ID`) or `group_id`, optional `base_url`. |
| `gemini_tts` | `gemini-3.1-flash-tts-preview` / `Kore` / `GEMINI_API_KEY`, `wav` | `models/{model}:generateContent`, `responseModalities: ["AUDIO"]` | `gemini-3.1-flash-tts-preview`: `prebuiltVoiceConfig.voiceName`; `gemini-3.8-flash-tts` / `-lite-tts`: `voiceConfig.voice`, verbatim transcript. `wav` wraps 24 kHz PCM. |
| `grok_tts` | `grok-tts` / `eve` / `XAI_API_KEY` | xAI `POST /v1/tts` | `voice` = `voice_id`; language `auto`; `mp3`/`wav`/`pcm`. |
| `sarvam_tts` | `bulbul:v3` / `shubh` / `SARVAM_API_KEY`, `language_codes: [en-IN, hi-IN, ...]` | Sarvam `POST /text-to-speech` (`api-subscription-key`), base64 `audios` | Language from Indic script, else first `language_codes` (`en-IN`). `voice` = `speaker` (`anushka` on v2; cross-generation speaker swapped). Over 2500 chars (1500 on `bulbul:v2`) split at sentences, so only `mp3`/`wav`/`pcm`. No whisper/emotion. |
| `fluid_audio_kokoro_tts` | — | Sidecar `/v1/audio/speech` | `FluidInference/kokoro-82m-coreml`, `streaming=false`; only when requested or preferred first. |

#### Sarvam Bulbul v4 Flash

`sarvam-bulbul-v4-flash` is a separate selectable `sarvam_tts` binding alongside
`sarvam-bulbul` (v3). It uses `bulbul:v4-flash`, default persona
`shubh_enhi_ads`, the existing `SARVAM_API_KEY`, and the same REST transport,
24 kHz output, `mp3`/`wav`/`pcm` formats and 2,500-character chunking as v3.
The adapter advertises all 222 published v4 personas from
`magician-media/src/media_rails/providers/sarvam_bulbul_v4.rs`; an empty
configured `voices` list uses that catalog. A nonempty list restricts the
advertised choices. Known speakers from another Bulbul generation normalize
to the selected model's default, including saved v3 short names on v4 and
v4 personas on v2/v3. Unknown new upstream voice IDs still pass through.
Marathi personas supply the language hint needed to distinguish Marathi
from Hindi when both use Devanagari.

The v4 seed advertises the nine languages with published personas: English,
Hindi, Bengali, Gujarati, Kannada, Marathi, Punjabi, Tamil and Telugu.
`enhi` personas cover English–Hindi mixtures. Sarvam's guide says Malayalam
and Odia language codes are accepted but no corresponding v4 personas are
published yet; v3 remains available with its full eleven-language coverage.
This is a voice-coverage distinction, not a claim that v4 rejects those API
language codes. See the [voice catalog](https://docs.sarvam.ai/api/api-guides-tutorials/text-to-speech/voices)
and [v4 guide](https://docs.sarvam.ai/api/api-guides-tutorials/text-to-speech/best-practices).

This integration remains non-streaming; provider metadata does not claim
streaming or additional emotion/prosody controls. Run
`make cargo ARGS="test -p magician-media --lib sarvam_tts"` for the adapter
contracts, and with `SARVAM_API_KEY` exported run
`make cargo ARGS="test -p magician-media --lib live_v4_personas_generate_decodable_audio_and_v3_remains_available -- --ignored --nocapture"`
for four billable synthesis calls: v4 English/Hindi/Bengali and a v3 baseline.
The live check validates PCM WAV decoding and the declared sample rate;
`SARVAM_TTS_V4_LIVE_DIR` optionally retains samples for listening. It does
not grade pronunciation or establish cross-language voice quality.

#### Speaking voice selection

`GET /media/providers` advertises voice IDs for each TTS binding under
`stages.tts[].capabilities.voices`, its configured default in `variant`, and
realtime choices under `realtime_voice_profiles[].voices`. Catalogs include
OpenAI (13 voices for `gpt-4o-mini-tts`, the smaller legacy set for `tts-1`),
Gemini's 30 studio voices (3.1) and all 2,089 public prebuilt voices (3.8),
MiniMax's 332 system voices plus its configured legacy default, Grok's 28
voices, Sarvam v3's 37 speakers and v4's 222 personas, FluidAudio/Kokoro's
20 supported American English voices, and installed host macOS voices. An
OpenAI-compatible model with an unknown voice set advertises its configured
catalog/default rather than assuming OpenAI's voice IDs. Account/custom voice
IDs can be supplied through existing `media.tts.providers[].voices` config.

`PUT /media/preferences` accepts partial `tts_voices: {binding_id: voice_id}`
and `realtime_voices: {profile_id: voice_id}` maps. Voice IDs are validated
against the binding's catalog before the scope's preferences are saved. Empty,
`auto`, or `default` removes only that entry. This optional addition keeps
schema version 3 and reads older preferences with empty voice maps. The
scope-owned load/mutate/save lock preserves concurrent patches to other fields.

Single-utterance and segmented TTS requests apply the saved binding voice when
`voice` is absent and its model matches the binding. An explicit request voice
override wins; a different request model does not inherit the binding's saved voice. Fallback attempts discard
the primary request voice/model and use the fallback binding's own saved voice.
Preferences are bound outside shared caches; the effective voice remains in the
cache key. A voice removed from a binding's current catalog is ignored during
playback and can be reset in the UI. Hands-free captures these choices at call
creation. Realtime choices apply to the next call/session mint; an active call
is not switched mid-utterance.

| Client | Where to select speaking voices |
| --- | --- |
| Web / Desktop web UI | Settings → Text to speech voices / Live call → Engine voices; composer pipeline controls → Speaking voice; composer Call panel → Speaking voice. Compact themed selectors match the LLM profile picker; popups stay anchored to the field, with TTS preview and catalog search. |
| iOS | Settings → Voice and audio → Speaking voices; composer Replies / Dictation / Hands-free pipeline → Speech model → Speaking voice for that selected TTS binding; Live → Speaking voices for the chosen realtime model. Compact rows open searchable choice pages within the existing sheet/navigation stack. This iPhone voices have a separate installed-voice picker. |
| Android | Settings → Voice and audio; composer Replies / Dictation / Hands-free pipeline → Speech model → Speaking voice for that selected TTS binding; Live → Speaking voices for the chosen realtime model. Compact rows open searchable lazy lists in the current bottom sheet (or a standalone sheet from Settings). This phone voices have a separate Android engine voice picker. |
| Native desktop tray Settings | Conversation Routing → Choose speaking voices opens shared Web Settings. Saved voices also apply to Orb calls. |

Backend TTS/realtime choices are scoped and shared across clients. Phone-native
voices are stored on the phone and applied by AVSpeechSynthesizer / Android
TextToSpeech. Browser-native voices remain in `BrowserVoiceControls` and keep
their existing device-local selection. No voice cloning or design action is
performed by these controls.

`enabled: false` hides an entry; entries missing credentials are omitted. With
no usable list, the legacy env path applies (`MAGICIAN_TTS_BASE_URL` /
`MAGICIAN_TTS_API_KEY`, then `OPENAI_API_KEY`, then placeholder `local`).
Fallback attempts drop request `voice`/`model` but pass `emotion`, `style`,
`pace`, `voice_mode`, `emphasis`. Headers: `X-Tts-Provider`, `X-Tts-Model`, `X-Tts-Voice`,
`X-Tts-Chain-Attempts`, `X-Tts-Fallback: true` (non-primary). Trace:
`magician::tts::fallback`.

### TTS synthesis cache

`CachedTtsProvider` per provider (key: text, delivery hints, voice/model/
format); process-local FIFO; errors not cached.

| Knob | Default | Notes |
| --- | --- | --- |
| `MAGICIAN_TTS_CACHE_CAPACITY` | `200` | Entries per provider; `0` disables. |
| `MAGICIAN_TTS_SEGMENT_TIMEOUT_SECS` | `60` | Per-segment on `/tts/synthesize_message`; timeout rotates. |

### Streamed segment synth — `POST /tts/synthesize_message`

Streams NDJSON per `<speech>` segment so playback starts early.
`Upstream`/`Transport` rotate, `BadRequest` short-circuits; an exhausted
segment emits `error` and the next continues.

### Recording STT (`media.recording_stt`)

| Adapter | Notes |
| --- | --- |
| `openai_transcriptions` | `/v1/audio/transcriptions` |
| `gemini_generate_content` | Inline audio |
| `gemini_transcribe` | Gemini 3.5 Transcribe, `POST /v1beta/interactions` |
| `grok_stt` | `POST /v1/stt`, `XAI_API_KEY` |
| `sarvam_stt` | Saaras, `POST /speech-to-text`, `SARVAM_API_KEY`; auto-detects 23 Indian languages (unsupported hint → auto). 30 s cap: WAV split into ≤28 s pieces at the quietest point after 18 s; long webm/m4a converted to 16 kHz WAV via ffmpeg (none → next provider) |
| `google_cloud_speech_v2` | `GOOGLE_CLOUD_ACCESS_TOKEN` + project |
| `fluid_audio_recording_stt` | Qwen3 ASR 0.6B sidecar via `dictation-fluid-qwen3-v1`; 30 s one-shot; `NoSpeech` on empty |

Legacy env path with none configured: `MAGICIAN_STT_MODEL` over
`OPENAI_API_KEY` or `MAGICIAN_STT_API_KEY`, `MAGICIAN_STT_BASE_URL`.
`provider=auto` tries macOS Speech first only when registered. Selection
affects only `/stt/transcribe`, `/stt/transcribe/stream`, `/voice-notes`.

Benchmark against consented fixtures (Magician and supervisor stopped):

```bash
make benchmark-media-recording-stt \
  MEDIA_STT_BENCHMARK_ARGS='--fixtures speech-clean-en --providers macos_speech,fluid-qwen3-asr-f32'
```

`make generate-media-local-speech-fixtures` makes synthetic plumbing fixtures
only; online providers need `--allow-online`. Results:
`data/magician_v2/media_evals/results/`.

### STT fallback — macOS Speech

Adapter → `MAGICIAN_HOST_GATEWAY_URL` `/host/speech/transcribe` →
`magician-macos-speech-helper` → `SFSpeechRecognizer`. Registers only with
helper + Speech authorization (`/host/speech/status`).
Browser clips are converted to WAV; helper failures/stalls rotate the chain.

### Streaming STT (meeting "ears")

Meetings use `StreamingSttProvider`; OpenAI adapters transcribe windows:

| Adapter | Model | Speaker labels | Default window |
| --- | --- | --- | --- |
| `openai_streaming_stt::OpenAiStreamingSttProvider` | `gpt-transcribe` | no | 4 s (`MEET_BOT_STT_SEGMENT_SECS`) |
| `openai_diarize_stt::OpenAiDiarizeStreamingSttProvider` | `gpt-4o-transcribe-diarize` | yes, per window | 8 s |

`POST /meetings/listen`, `POST /meetings/join` and screen observation accept
`audio_profile` and `audio_stage_options`. VAD, STT and diarization are
independently selectable; multi-speaker tracks attach the diarization stage when
STT does not attribute speakers, and its failure only drops labels; optional VAD fails open to macOS device-VAD or provider RMS; a healthy
gate never sends silence to STT.

`MacOsSpeechSttProvider` feeds the meet-audio helper's `--mode transcribe`
(`MAGICIAN_MACOS_MEET_AUDIO_BIN`, default `./magician-macos-meet-audio.bin`).
`GeminiLiveTranscribeSttProvider` (`gemini_live_transcribe`):
`gemini-3.5-transcribe-live`, 24→16 kHz, partials/finals, Hands-free
(`hands-free-local-fluid-v1`), no diarization, `GEMINI_API_KEY`.

## Voice cost accounting

Realtime turns land in `llm_calls` as `operation = "voice_controller"`, priced by
`magicllm::pricing::compute_realtime_cost(model, &RealtimeUsage)`. Unknown cost
is `0.0` with `usage_reported` false, never `f64::NAN` (poisons Parquet `SUM`);
silent `$0` = no pricing row or no modality split. The model-keyed table
applies to any vendor except `Custom` (`llm_pricing_identity`). `openai_realtime`, `openai_realtime_backend`,
`openai_live` are OpenAI aliases. Usage writes spawn on the runtime, not the
voice actor (a close after `session.end` would cancel them).

Duration-billed models (`per_second`, no token rates;
`realtime_is_duration_billed_at`) would recompute to a false `$0.00`, so they keep the producer's figure as `Estimated`
(`Unknown` if not finite) via `pricing_for_runtime_event`; `llm_reprice` skips
them as `rows_unpriceable_skipped`. Grok voice (`grok-voice`, $0.08/min) is
too: its wire dialect reports the connection's seconds once at close (xAI's
$0.004 per text message is not modelled).

### TTS and STT metering

`media_seam::metering` writes one `llm_calls` row per TTS/STT call
(`voice.tts` / `voice.stt`, response kind `voice_tts`, `voice_stt` or
`voice_stt_streaming`), priced from the pricing file's `media_rates` as
`Estimated`; local Fluid and macOS engines record an exact $0 (vendor `local`).
TTS wrappers sit inside the cache, so hits are not billed. TTS bills characters
plus output length (WAV/PCM exact, MP3 from the first frame's bitrate); STT the
vendor's duration or the upload's WAV/PCM/MP3 length; streaming STT the PCM
pushed. Media API handlers set the tenant with `with_media_meter_scope`;
sessions bind their chains with `scope_tts_chain` / `scope_streaming_stt`. A
missing rate or an unmeasured unit records cost unknown, never $0.

Limitations (possible fix): compressed STT uploads without a vendor duration
cost unknown (decode the length); units are not stored, so media rows cannot
be repriced (add usage columns); VBR MP3 is estimated (parse Xing/VBRI);
vendor usage fields are ignored (prefer them); Relentless audio review is not
scoped, so not metered (thread the run scope); Gemini Flash as STT and Chirp 3
have no media rows, so they record a pricing miss.

## Using local / self-hosted models

| Channel | Local server | Default URL |
| --- | --- | --- |
| TTS | macOS AVSpeechSynthesizer via host gateway | `MAGICIAN_HOST_GATEWAY_URL=http://127.0.0.1:3017` + `tts_available=true` |
| TTS | Kokoro-FastAPI, openedai-speech, LocalAI | `base_url` on `openai_speech` |
| STT | faster-whisper-server, LocalAI | `MAGICIAN_STT_BASE_URL` or `base_url` |
| Realtime voice | none production-grade | — |

No API key → `Bearer local` (e.g. `base_url: http://localhost:8880/v1/audio/speech`,
then `make restart-magician`). No provider: TTS falls back to
`window.speechSynthesis`, STT has none (the mic can still attach audio as an
artifact), the call button hides.

## Tray bridge wire protocol

Frames: [`tray-bridge-protocol.md`](realtime-media/tray-bridge-protocol.md).
The tray PTT client registers `tray_macos`, opens
`/media/voice/{voice_session_id}/control` with `realtime_profile=voice_realtime_openai_backend`, heartbeats, streams 24 kHz PCM while
the hotkey is held, commits with `ptt.release`, plays backend PCM (assistant
audio can be muted locally without touching provider state), posts diagnostics to
`/media/sessions/{id}/events` (`media.voice.bridge.*`, `media.voice.client_*`,
`media.voice.controller_command`), and maps controls to `input.clear`,
`response.interrupt`, `session.end`.

## Debugging

`/debug/voice` shows sessions, tray PTT, stale heartbeats and `media.voice_note*` /
`media.voice*` tails. Voice turns carry `voice_origin=true` (browser:
`source_surface=realtime_voice`); the chat ledger is authoritative.

## Meeting participant bot

Native macOS Meet bot (ScreenCaptureKit capture, CoreAudio inject): joins in
its own signed-in browser and streams a display-only transcript to a
per-meeting thread. Prompt: `voice_meeting_presto_system`
(compiled fallback). History:
plan,
capability/auto-join,
teardown/memory,
auto-join,
barge-in/profile,
passive listener.

Attendee (`MeetingSession` + `MeetingSessionManager`) and passive
(`PassiveMeetingSession` + `PassiveMeetingSessionManager`) share
`derive_meeting_thread_id` / `resolve_meeting_thread`:
`meeting-<label>-<YYYY-MM-DD>`, label = `title` slug, Meet code or URL blake3;
date = tool `date` or today. `MEET_BOT_THREAD` forces one thread; title and
link-only joins differ.

### HTTP

Under `/api/magician/v2/meetings` (`magician-api/src/meetings_api.rs`):

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/meetings` | Active sessions + recent `meeting-*` threads. |
| GET | `/meetings/active` | Live captures. |
| GET | `/meetings/upcoming` | Next ~12 h via `gws` (`MEET_BOT_CALENDAR_ACCOUNTS` or `gws_accounts` ∩ `<scope>/auth/gws-<name>`); 60 s cache. |
| POST | `/meetings/listen` | Passive; `{"capture":"client",…}` for client ingest. |
| POST | `/meetings/join` | Attendee join (as the `meeting` tool); `https://web.whatsapp.com/` answers, `https://web.whatsapp.com/send?phone=<digits>` places a call. |
| GET | `/meetings/{id}` | Status (`paused`, `latest_summary`, thread, title); WhatsApp answer mode: `IncomingCall` then `Listening`. |
| POST | `/meetings/{id}/stop` | Stop; client ingest: `202`, finishes in background. |
| POST | `/meetings/{id}/pause` / `/resume` | `set_paused`: drop chunks before STT. |
| POST | `/meetings/{id}/audio` | Client PCM (`channel=primary\|mic`, 16 kHz mono PCM16 LE, 2 MiB cap); `410 Gone` = stop. |

UI: `/meetings` 308 → `/observe`; `/meetings/[thread]` is the transcript.

### Modules

Paths under `magician/src/magician_v2/`.

| Module | What it owns |
| --- | --- |
| `media_seam/meeting_session.rs` | `MeetingConfig`, `derive_meeting_thread_id`. |
| `media_seam/meeting_session_engine.rs` | `MeetingSession` (`Idle` / `Joining` / `Listening` / `Responding` / `Left` / `Failed`), wake gate, barge-in, summary, teardown. |
| `media_seam/meeting_manager.rs` | `MeetingSessionManager`: `spawn` / `join` / `status` / `leave` / `list`; ended kept 300 s. |
| `media_seam/meeting_passive.rs` | Passive listener; auto-stop after 5 min silence (`MEET_LISTEN_AUTO_STOP_SECS`, `0` off); max 4 h (`MEET_LISTEN_MAX_SECS`). |
| `media_seam/meeting_pushed.rs` | `PushedAudioSource` + ingest registry; idle stop 120 s (`MAGICIAN_MEETING_CLIENT_IDLE_STOP_SECS`); first-chunk grace 600 s. |
| `media_seam/meeting_memory.rs` | `MeetingMemoryWriter` → `user.research_findings` (`MEET_BOT_MEMORY_MAX_MEETINGS`, default 20). |
| `media_seam/meeting_transcript_sink.rs` | Posts to `POST /chat/sessions/{id}/transcript`; queue 256; `MEET_BOT_STREAM_TRANSCRIPT=0` disables. |
| `media_seam/meeting_markers.rs` | Crash markers `<scope>/workdirs/meeting_capture_markers/<session_id>.json`, boot sweep. |
| `media_seam/browser_join.rs` | `BrowserJoin` / `NoopBrowserJoin` / `AgentBrowserMeetJoiner` (macOS, Linux); WhatsApp branch. |
| `media_seam/whatsapp_web_call.rs` | WhatsApp URL intent + snapshot classifier (incoming = Accept/Answer and Decline/Reject). |
| `media_seam/whatsapp_call_ring.rs` | Reducer for a call in everyday Chrome: Approve starts voice once, Ended stops, mid-flight profile changes ignored. |
| `media_seam/meeting_bridge_macos.rs` | `ScreenCaptureAudioSource` + `CoreAudioSink` (`magician-macos-meet-audio`). |
| `media_seam/meeting_responder.rs` | `MeetingResponder` trait. |
| `media_seam/meeting_orchestrator_voice_responder.rs` | Default responder. |
| `media_seam/meeting_magician_agent_responder.rs` | REST agent → TTS (`agent-tts`). |
| `media_seam/meeting_llm_tts_responder.rs` | `meeting_response` → TTS (`llm-tts`). |
| `media_seam/meeting_realtime_responder.rs` | Bot-local realtime, no tools (`realtime-direct`). |
| `media_seam/meeting_summarizer.rs` | `meeting_summary` (local Ollama / Gemma); missing router fails explicitly. |
| `execution/compiled_providers.rs` | `MeetingCapabilityProvider`. |
| `execution/embedded_pack_defs/meeting.yaml` | `meeting` pack (`join` / `status` / `leave` / `list`); `web.whatsapp.com` → WhatsApp call, else Meet. |
| `magician/examples/meet_bot.rs` | `cargo run -p magician --example meet_bot`. |

Scope: `MEET_BOT_PRINCIPAL` / `MEET_BOT_WORKSPACE`, then agent scope, then
`anonymous` / `default`.

### Join, capture, barge-in, teardown

`MeetingSession::start()` joins, then captures (retargeted to the launched
PID), polls every 5 s (cancel on `!is_in_meeting()`), and hangs up if STT
fails.

`AgentBrowserMeetJoiner` (macOS):

- Switches default input to `BlackHole 16ch` (SwitchAudioSource), restored on
  every failure and `leave()`.
- Headed agent-browser via `content_acquisition.browser.engine` (unresolvable
  → no auto-join).
- Profile `MEET_BOT_PROFILE_DIR`, else `<scope workdir>/meet-bot-profile`, else
  temp, via `AGENT_BROWSER_PROFILE` (never `--user-data-dir`, which breaks
  DevTools-port discovery). One meeting at a time per profile; tab-restore
  cache deleted pre-launch.
- Admission is a wall-clock-bounded self-healing loop.
- WhatsApp: bare URL accepts once Accept + Decline show (`IncomingCall` on
  `GET /meetings/{id}`);
  `send?phone=` clicks Continue to chat → Call → Voice (never Video). Place
  timeout 90 s; answer `MAGICIAN_WHATSAPP_CALL_WAIT_SECS` (default 600,
  30–1800). A QR screen fails fast.
- `POST /meetings/whatsapp-ring` bypasses this joiner: Join attaches voice to
  the open Chrome tab (`NoopBrowserJoin`, `answer_all`, Chrome-bundle capture,
  voice session token as bearer); Listen is passive with mic off; Idle starts
  nothing. Profile = selected realtime id (default
  `voice_realtime_openai_backend`, from the selectable catalog).
- Default capture is **whole-display audio** (SCK app-filtered capture is
  silent for the cloak Chromium engine); it includes the bot's own voice, so the
  session is **half-duplex** (chunks dropped while the sink plays) and
  **barge-in is unavailable**. `MEET_BOT_TARGET_BUNDLE_ID` or
  `MEET_BOT_CAPTURE=bundle` selects app-filtered capture.

**Turn-taking.** WhatsApp and Meet stream with `turn_boundary: server_vad`;
capture is withheld during playback plus a tail. `CoreAudioSink::play_pcm` sets `kill_on_drop(true)`. The local
wake loop runs only without a live session (tests, `noop`, `realtime-direct`):
addressed final or ~1.5 s-settled partial = turn, repeats suppressed ~8 s,
optional short busy cue.
A live call whose realtime session fails stays transcript-only.

Teardown: hang up, STT drain, `resummarize()` + memory write, `Left`;
`MeetingSessionManager::leave` waits ~10 s for the summary. `meeting_summary_to_fields` parses `{ summary, decisions[],
action_items[] }`; `ScopedMeetingMemoryWriter` merges into
`user.research_findings` keyed `meeting:<thread-id>` and files a best-effort
`Observed` MemoryFact candidate. Default writer: `NoopMeetingMemoryWriter`.

Setup: `./magician-macos-meet-audio.bin` (`MAGICIAN_MACOS_MEET_AUDIO_BIN`)
staged by `make build-macos-speech-helper` and `build-all-*`;
`make setup-meet-bot` (run by `make install`) installs BlackHole 16ch and
`switchaudio-osx`. Capture needs **Screen Recording** TCC. `--mode inject`
plays to BlackHole; `--target-pid` (by `processID`) beats `--target-bundle-id`. Linux
(`meeting_bridge_linux.rs`): `pacat` → `magician_meet_mic`, `parec` from
`magician_meet_capture.monitor` (or default monitor), Xvfb if no `DISPLAY`;
loading the null-sinks puts the previous default sink back. Devices are restored only after the last attendee; the virtual mic never
becomes the desktop mic, and the passive mic never records
`magician_meet_mic.monitor`.

### Responders

`default_meeting_responder` → `OrchestratorVoiceResponder` unless
`MEET_BOT_RESPONDER` is `realtime-direct` or `noop` (`agent-tts`, `llm-tts`,
`dictation`, `hands-free` use the same realtime session). It is a headless
client of `/media/voice/{id}/control` (seating as above; default
`voice_realtime_openai_backend`); a silent `user.text` seed (`inject: true`)
grounds without speaking. Env: `MEET_BOT_MAGICIAN_URL` (default
`http://127.0.0.1:3002/api/magician/v2`), `MEET_BOT_BEARER_TOKEN` /
`MAGICIAN_BEARER_TOKEN`, `MEET_BOT_REALTIME_PROFILE`. `persists_to_chat()` is
true, so the sink skips addressed turns.

### Passive listener

No join. Display audio plus opt-in mic (`--mode capture-mic`, Microphone TCC):
system track diarized, mic track labelled `You`. Mic defenses: OS voice gate
(macOS 14+), RMS gate (`MEET_STT_SILENCE_RMS`, ≈ −49 dBFS), echo suppression
(≥60% token overlap with recent system finals, ≥35% within 4 s of system
audio). Raw audio is never persisted. Idempotent per thread (a second client listen
reuses the live one); collision with a host capture → `409 already_observed`.
Shutdown stops captures on both rails.

### Client-pushed meeting capture

`POST /meetings/listen` `{"capture":"client",…}` returns
`"capture_source":"client"` and a short-lived `"upload_token"`. Ingest
`POST /meetings/{id}/audio?channel=primary|mic&seq=N`, one `audio/pcm` chunk;
`primary` diarized, `mic` = "You" (needs `mic:true`); newest dropped under
backpressure. Unknown/ended/mismatched → `410 Gone`; bad channel `400`;
oversized `413`. Single-tenant: do not expose ingest on the LAN.

### Client-pushed screen observation

`magician-media/src/media_rails/screen_observe.rs`: `FrameSource::Host` or
`FrameSource::Pushed` (newest wins); HTTP in `magician-api/src/screen_api.rs`:

- `POST /screen/observe/client/start` `{ thread, title?, purpose?, mode?, cadence_s?, max_minutes?, deep_observation? }` → `{ observe_id, upload_token, thread, status }`.
- `POST /screen/observe/frame?observe_id&principal&workspace` + `X-Upload-Token`, raw JPEG/PNG ≤ 4 MiB → `202`, or `410 observation_gone` (unknown session, scope mismatch, bad token).

Client observations are multi-session (host: single); passing the meeting
thread weaves notes into its transcript.

## Voice-created tasks render their chat card

`create_task`, `orchestrate_pipeline` and `delegate_to_agent` results go
through `dispatch_external_tool_call` → `subscribe_external_spawned_tasks`
(non-terminal, matching `manifest.chat_session_id`; ephemeral voice-turn fanout
dropped), re-subscribed as
`chat-task-<task_id>-<session_id>` onto the `TaskStatusUpdate` card. They get
`effective_cancel = None`: hang-up does not cancel them (inline tools keep the
call-wide token).

`task.completed` reaches the live call via
`VoiceDownstreamFanout::link_chat_session` (`chat_session_id`). `failed` / `cancelled` announce
from `persist_execution_outcome`; `completed` waits for
`finalize_terminal_execution` and speaks `speech_live` from `voice_speech.json`
(32 KiB cap), else `voice_summary_excerpt`. Off-call, `maybeSpeakTaskCompletion`
reads the card's `speech_tts` when auto-speak is on and no call is active.

`replay_turn_from_message` replays terminal cards (no re-fire on resume).
`dispatch_chat_tool_call` blocks identical-goal tasks in the same session (`duplicate_blocked`;
`allow_duplicate: true` after confirmation).
`ChatLlmService::neutralized_stale_task_call_ids` turns earlier
`orchestrate_pipeline` / `create_task` / `create_monitor` calls into notes.

## Wake-started voice and call lifecycle

`fireWake` starts `voice_mode=hands_free` (cascade owns VAD).
`setPushToTalkMode` toggles PTT (backend-proxied rotates but keeps the socket
via `audio.rebind`). OpenAI `error` events are request-scoped; only terminal
WebRTC failure reconnects; no `response.cancel` without a response; rebinds
keep `connectedAt`.

## VibeDev preview dev server

`magician/src/magician_v2/media_seam/dev_server_manager.rs` runs `npm run dev` /
`vite` as a process-group leader (`process_group(0)`); `DevServerSession::kill()`
signals the group (`SIGTERM`, ~400 ms, `SIGKILL`) for `stop()` and `reap()`.

## What's intentionally not done

Per-agent voice routing: every session resolves to `voice_realtime_default`;
branching `operation_mapping` would be config-only.

Voice catalog audit (2026-10-15): xAI `GET /v1/tts/voices` returned 28 IDs; Gemini `GET /v1beta/voices?type=prebuilt` returned 2,089 IDs across three pages; MiniMax `POST /v1/get_voice` (`voice_type: system`) returned 332 IDs. Exact MiniMax Unicode voice IDs are preserved. Gemini 3.8 includes the extended library; 3.1 keeps its 30 studio voices. Gemini Live exposes 30 studio voices for 3.1 and all 2,089 prebuilt IDs for 3.8, Grok Live the same 28-voice roster as TTS. FluidAudio is limited to the 20 American English voices supported by pinned FluidAudio 0.12.4; untested foreign-language voices are not offered by the English binding.

Setup-only live API checks confirmed `Sulafat` on Gemini 3.1/3.8 Live and `en-us-advisor-1` on 3.8; 3.1 rejected the extended ID with close code 1007. Catalog discovery, saved voice validation, and session setup therefore use the model-specific roster. xAI `altair` and Gemini 3.8 `en-us-advisor-1` also passed real WAV synthesis checks. Sarvam’s live v4 validation lists 224 personas; the two `_as_` voices returned “request beta access” on this account. They remain outside the default 222 selectable personas; explicit Assamese language/voice config opts in after beta access is granted.

Registered TTS adapters supply the selectable voice catalog and normalized model/default voice. The config-only fallback contributes unavailable bindings without overwriting an available adapter with an omitted or stale `voices` allow-list.
