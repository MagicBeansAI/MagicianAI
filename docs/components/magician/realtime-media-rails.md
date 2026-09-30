# Realtime Media + Control Rails

Shared substrate for browser TTS, mobile camera/mic capture, provider TTS/STT,
realtime voice, the native mascot/tray screen/pointer/system-audio bridge, and
the Google Meet participant bot.

Related:

- Archived plan:
  `docs/archive/plans/2026-05-15-realtime-media-control-rails.md`
- Pre-FluidAudio baseline:
  `fluid-audio-phase0-baseline.md`
- Engine defaults, measurements, and remaining physical gates:
  [`fluid-audio-phase9-rollout.md`](fluid-audio-phase9-rollout.md)
- Tray/mascot WebSocket frames:
  [`tray-bridge-protocol.md`](realtime-media/tray-bridge-protocol.md)
- Orb rendering pipeline:
  [`mac-notch-orb.md`](../desktop/mac-notch-orb.md)
- Screen capture-and-ask:
  [`screen-capture-and-ask.md`](screen-capture-and-ask.md)
- Realtime pricing:
  [`magicllm/README.md`](../magicllm/README.md)

Live runtime config is `$MAGICIAN_ROOT_DIR/magician-config.yaml` (git-backed
seed: repo-root `magician-config.yaml`). Restart Magician with
`make restart-magician` — there is no `start-magician` target.

## Architecture

```text
client surface (web mobile | web desktop | native mascot | tray | extension)
        |
        v
realtime media session   (POST /media/sessions)
        |
        v
media event envelope     (media.*)  →  RuntimeTransportBroadcaster
        |                                     |
        v                                     v
provider plane                          UI activity card
  ├─ VadProvider
  ├─ TtsProvider                        /events stream
  ├─ SttProvider                        observability dashboards
  ├─ StreamingSttProvider
  ├─ DiarizationProvider
  └─ magicllm::realtime::RealtimeProvider
       (resolved per call from
        magician-config.yaml > realtime_voice)
```

Every surface registers a `RealtimeSession` once on mount, then emits `media.*`
lifecycle events. TTS and STT adapters are process-local and built at boot.
OpenAI-backed providers are gated by `OPENAI_API_KEY`. Host-backed local
providers register only when `MAGICIAN_HOST_GATEWAY_URL` reaches the Tauri
gateway (normally `http://127.0.0.1:3017`) and `GET /host/speech/status`
verifies the capability (`macos_tts` needs the helper; `macos_speech` also
needs Apple Speech authorization). In desktop-managed runs the tray binds the
host gateway before starting Magician so the probe does not race the listener.

Realtime voice is resolved per call by
`OperationLlmRouter::resolve_realtime_provider` against `realtime_voice`.
Host-native clients may pin a profile with `realtime_profile` (or `profile`) in
`session.start`. `voice_mode=hands_free` selects the local FluidAudio → Chat →
TTS cascade; `voice_mode=realtime` selects the named vendor profile. Clients send
`turn_boundary=server_vad|push_to_talk`; the server maps it to provider turn
detection (`server_vad` / `none`) before opening audio and keeps it across
rotations. The Mac Orb always uses `server_vad` for Live (no release gesture).
Clients that omit `turn_boundary` commit with an explicit `ptt.release`.

Backend-proxied providers emit `TransportReady` only once their socket/cascade
is consuming. The voice-control actor queues capture until then, dispatches
config and resume without awaiting the provider's bounded channel, and fails
closed on a 10 s readiness deadline (OpenAI's handshake has its own 8 s). On the
Mac Orb a fatal Live startup/transport failure triggers one non-recursive
Hands-free fallback.

Local Hands-free TTS keeps a 300 ms PCM lead over playback
(`PCM_INITIAL_LEAD_MS`) and synthesizes one segment ahead; desktop Hands-free and
Dictation play through a 180 ms prebuffer (`LIVE_PTT_PLAYBACK_PREBUFFER_MS`).

Native mascot lifecycle is split: Magician owns the media session, chat ledger,
events and bridge socket; the Tauri host gateway owns the visible host process.
The Swift Orb listens on loopback `127.0.0.1:3027` for tray commands.
Recording/bubble endpoints are render-only; the tray owns mic capture, STT and
chat posting.

`magician-media` re-exports the seam (`pub use magician::magician_v2::media_seam::*`)
so `media_rails::*` names still compile. The defining file is the source of
truth for a type, not the re-export.

## Backend modules

| Module | What it owns |
| --- | --- |
| `magician-media/src/media_rails/mod.rs` | `RealtimeSession`, `RealtimeSessionRegistry`, `SurfaceType`, `SurfaceCapabilities`, `MediaPermissions`, lifecycle event constants. |
| `magician/src/magician_v2/media_seam/audio_surface.rs` | Audio surfaces, stage/profile config, provider options, capabilities, resolved session snapshots. |
| `magician/src/magician_v2/media_seam/runtime_config.rs` | Revisioned runtime snapshot, configured profiles, validation, precedence, one-time legacy migration, transactional live-config updates. |
| `magician-media/src/media_rails/fluid_audio/` | Supervised sidecar lifecycle, authenticated/versioned IPC, model loading, VAD / recording STT / streaming STT / diarization / Kokoro TTS adapters. |
| `magician/src/magician_v2/media_seam/vad_gate.rs` | Meeting/Listening profile adoption, bounded pre-roll, speech-segmented STT sessions, ordered finalization, optional fail-open fallback. |
| `magician/src/magician_v2/media_seam/streaming_stt_types.rs` | `StreamingSttProvider` / `StreamingSttSession` traits (re-exported as `media_rails::providers::streaming_stt`). |
| `magician-media/src/media_rails/providers/` | Concrete VAD/STT/streaming STT/diarization/TTS adapters + `MediaProviderRegistry`. |
| `magician-media/src/media_rails/voice_orchestrator.rs` | Per-call realtime voice state machine (mint, rotate, compact, replay, dispatch). |
| `magician-media/src/media_rails/voice_context_compactor.rs` | Chat-ledger → compacted summary via PromptManager + OperationLlmRouter. |
| `magician-media/src/media_rails/voice_session_lifecycle.rs` | Process-local lifecycle store (rotation count, summary cache). |
| `magician-media/src/media_rails/voice_downstream_fanout.rs` | Voice-session-id → control-WS downstream fan-out. |
| `magician-media/src/media_rails/voice_tool_dispatcher.rs` | Voice tool dispatcher (`create_task` / `get_task_status` / `stop_task`). |
| `magician-media/src/media_rails/self_echo.rs` | Assistant-utterance tracker + transcript echo guard. |
| `magician-media/src/media_rails/voice_addressing.rs` | Wake-prefix + follow-up window admission. |
| `magician-media/src/media_rails/hands_free.rs` | Cascaded Hands-free provider. |
| `magician-media/src/media_rails/screen_observe.rs` | Host and client-pushed screen observation. |
| `magician-api/src/media_api.rs` | REST: sessions, providers, preferences, audio-settings, synthesize / transcribe, voice-notes. |
| `magician-api/src/media_ux/session_controls.rs` | `voice_source_surface`, `voice_surface_derivation`, turn-boundary / prefix parsing. |
| `magician-api/src/voice_control_handler.rs` | Bidirectional WebSocket actor per realtime voice call. |
| `magician-api/src/tray_bridge_handler.rs` | WebSocket actor for native tray and mascot bridge surfaces. |
| `magician-api/src/meetings_api.rs` | Meetings HTTP (listen / join / ingest / pause / resume). |
| `magicllm/src/realtime/` | `RealtimeProvider` trait, topology/control/event types, OpenAI Realtime / GPT Live / Gemini Live adapters, speakable voice catalog, factory. |
| `magician-event-taxonomy/src/lib.rs` | `EventCategory::Media` + `media.*` taxonomy rows. |
| `magician-bin/src/main.rs` | Route registration under `/api/magician/v2`. |

Meeting engine modules live under `magician/src/magician_v2/media_seam/` (see
[Meeting participant bot](#meeting-participant-bot)).

## Surfaces

`SurfaceType` (serde `snake_case`): `web_mobile`, `web_desktop`,
`mascot_macos` / `mascot_windows` / `mascot_linux`, `tray_macos` /
`tray_windows` / `tray_linux`, `extension`, `esp_terminal`, `meeting_bot`,
`unknown` (serde default). `EspTerminal` is the ESP32-C6 push-to-talk desk
terminal in `magesp/`. An unrecognized device string resolves to `Unknown`
rather than failing.

## HTTP surface

All under `/api/magician/v2/media` unless noted, scope-isolated via the
workspace-bound bearer.

| Method | Path | Purpose |
| --- | --- | --- |
| POST   | `/sessions` | Register a realtime surface |
| GET    | `/sessions` | List active surfaces in scope |
| GET    | `/sessions/{id}` | Read a surface snapshot |
| PATCH  | `/sessions/{id}` | Update capabilities / permissions |
| POST   | `/sessions/{id}/heartbeat` | Refresh liveness |
| DELETE | `/sessions/{id}` | Disconnect (graceful) or revoke |
| POST   | `/sessions/{id}/events` | Publish a whitelisted client event |
| GET    | `/providers` | `{ tts, tts_fallbacks?, stt, stt_fallbacks?, realtime_voice? }`. TTS rows carry default format, voices/formats, truthful streaming capability. `realtime_voice` is set when `resolve_realtime_provider(VoiceController)` returned a usable provider at boot; `VoiceCallButton` gates on it. Selectable profiles advertise topology, mode, YAML default voice, speakable voice catalog, transcription/fallback models, availability. |
| GET    | `/audio-settings` | Safe global audio catalog, profiles/defaults, engine availability, revision. |
| PUT    | `/audio-settings` | Revision-checked typed patch; persists live config and swaps the resolver snapshot. FluidAudio disable invalidates leases, closes owned streams, clears its TTS cache, stops an owned sidecar and asks an external sidecar to unload models (without killing it). Enable verifies host support and restores adapters without restart. |
| POST   | `/audio-engines/{engine_id}/models/{action}` | Prewarm/unload actions advertised by Settings. |
| GET    | `/surfaces/{surface}/resolved` | Effective backend-owned profile for a scope, with optional profile and stage-option hints. |
| GET    | `/preferences` | Schema-v3 scoped preferences: `auto_speak`, `voice_mode`, `require_voice_prefix`, `surface_profiles`, `surface_stage_options`, `realtime_voices`. |
| PUT    | `/preferences` | Update; options absent from the catalog are rejected. `realtime_voices` pins a voice per Live/Realtime profile (`default`/empty clears). Emits `media.preferences.updated`. |
| POST   | `/tts/synthesize` | Provider TTS, text → audio. Optional `provider` uses chain semantics. |
| POST   | `/tts/synthesize_message` | Streamed TTS for pre-parsed `<speech>` segments. |
| POST   | `/tts/cache/clear` | Drop all cached TTS responses. |
| GET    | `/tts/cache/stats` | Per-provider hit / miss / eviction counters. |
| POST   | `/stt/transcribe` | Multipart audio → transcript. Optional `provider` reorders the chain for this request. |
| POST   | `/stt/transcribe/stream` | Streaming recording-mode STT. |
| POST   | `/voice-notes` | Multipart audio + chat metadata → STT → normal chat turn. |
| POST   | `/voice-notes/events` | Scoped, allowlisted tray voice-note lifecycle events. |
| GET    | `/voice/{voice_session_id}/control` | Bidirectional control WebSocket for realtime voice. |
| GET    | `/bridge/{session_id}/ws` | WebSocket bridge for native tray and mascot surfaces. |

Meetings (`/api/magician/v2/meetings`) and client-pushed screen observation
(`/api/magician/v2/screen/observe/…`) are listed with those rails below.

**Voice notes.** `POST /voice-notes` returns the transcript, assistant preview,
and `assistant_speech_segments` when the reply carried `<speech>` blocks
(host-native recording uses them to request `/tts/synthesize_message`; browsers
use persisted `speech_segments`). After STT the chat turn runs as an owned
closure on the execution runtime; cancellation or panic emits
`media.voice_note.chat_submit_failed` + `media.voice_note.error` and returns
`chat_submit_failed`. `unsupported_language` / `empty_transcript` 422 bodies
include the created `chat_session_id`, `error`, `message`, optional
`audio_artifact_id`. macOS "No speech detected" maps to `SttError::NoSpeech`,
which the STT chain treats as terminal (no fall-through to Whisper).
Predominantly non-Latin transcripts are rejected before chat ("English (and
Hinglish) only"). Lifecycle events: `media.voice_note.{received,
recording_started, recording_stopped, recording_failed, transcription_started,
transcription_completed, transcription_failed, transcribed, chat_submit_started,
chat_submit_completed, chat_submit_failed, submitted, error}` (`transcribed` and
`submitted` are compatibility names). Magician stays the authority for STT, chat
insertion, artifacts and audit.

VAD-gated Meeting and Listening streams also emit backend-only diagnostics
(`media.audio.profile.{resolved,degraded}`, `media.audio.vad.{speech_started,
speech_ended}`, `media.audio.provider.fallback`, `media.audio.frames.dropped`)
with identifiers, timestamps and counts — never PCM or transcript text.

### `surface_type` decides who the agent thinks it is talking to

`voice_source_surface` maps a session's `surface_type` to the chat turn's
`source_surface`, which chat maps to an `InvocationSurface` that decides which
agents may answer.

- `meeting_bot` registers as itself and resolves to the room surface; only an
  agent listing `meeting` in its invocation policy may serve it.
- `unknown` (the default for an omitted type) and `extension` also resolve to
  the room, with derivation reason `unrecognised_registration_fail_closed`.
- Identified owner surfaces (`web_*`, `tray_*`, `mascot_*`, `esp_terminal`)
  keep owner voice (`realtime_voice` / `global_live_ptt` / `mascot`).

Claiming an owner surface requires a server-owned `VerifiedRequestIdentity`
(Cloudflare Access assertion, paired-device credential, or trusted loopback)
from `verify_access_middleware`; an unverified caller resolves to `unknown`
however it labels itself. Narrowing to `meeting_bot` is always admitted. The
middleware passes non-loopback peers through without identity when Cloudflare is
unconfigured or below `Require` — exactly the deployments this guards.

The call's surface is minted once at start and survives rotation, reconnect and
resume: `rotate()` mutates named fields in place rather than rebuilding
`CallState`.

### The surface decides which agent the call binds

Owner voice binds `VOICE_CHAT_AGENT_ID` (`personal-assistant`). A room binds
`ChatService::room_agent_id` (`EnvoyConfig::envoy_agent_id`, default `envoy`)
into `CallState::chat_agent_id` once at start; the chat-session binding and
`authorize_external_tool_call` both read it. With no ambassador configured the
surface gate refuses (`agent_not_surface_eligible`) rather than seating the
personal assistant in a room.

Every voice path that names an agent must use `CallState::chat_agent_id` or
`call_agent_id()` — never the constant. `call_agent_id()` takes the lock itself
and must not be called under one.

### Retrieval and prompt authority follow the surface

Retrieval and the memory-authority instruction are selected from the call's
resolved surface, not the chat thread name. `voice_invocation_surface()` is the
one mapping, so prompt layer and turn classifier cannot disagree. The
memory-authority instruction keys on `surface.audience()`; both retrieval sites
(session start, per turn) pass the surface into
`chat_context_retrieval_request`. `get_or_create_active_session` reuses a session
only on an exact agent match, so a room cannot attach to the owner's session.

**What a room may see.** The room's chat session is bound to the per-meeting
thread where the transcript lands. Retrieval adds only the condensed view — the
rolling summary and parsed decisions/action items — for the meeting bound to the
call's own thread. The gate is `surface.audience()`, so an owner call on a
meeting thread gets nothing. It rides `RealtimeTurnContext::meeting_context`
(merged via `injectable_context()`), refreshed per turn and injected only when a
digest on `CallState` changes. Cross-meeting reach, prior meetings, attachments
and every memory tier are excluded.

Teardown files takeaways under `meeting:<thread-id>` into USER tier
`user.research_findings` with **no agent** (shared user root). Isolation between
rooms comes from `user_memory_isolation: fully_isolated`, which redirects the
ambassador's user-memory reads into `agents/<agent>/memory/user_memory/`.

A same-day rejoin names the same thread (id from url + title + date), so the
room keeps its prior transcript and `initial_resume_context` compacts it. Known
gaps: a next-day rejoin (or past midnight) is a different thread, and a session
archived between drop and rejoin strands the transcript.

**A room reads no agent memory.** `surface_may_read_agent_memory` is false for
`Meeting` at both the chat prompt and realtime retrieval. `PublicEnvoy` keeps its
memory, made safe by `fully_isolated`. `memory_tiers: []` on the ambassador is
not that seal: it is `kind: personal`, so `apply_defaults()` injects the six-tier
personal set.

**Seeing the boundary.** `session.ready` carries `boundary`
`{surface, audience, agent_id, elevatable}` from live `CallState`; `elevatable`
is constant `false` and no client field can request another surface. A missing
block means "unknown", never "owner". `session.ready.agent` carries the primary
agent's presentation identity so captions follow renames.

**Addressing.** Preference `require_voice_prefix` defaults `true`. At call start
Magician advertises `Hey <alias>` phrases for the primary agent and an 8 s
follow-up window in `session.ready.addressing`, and admits only transcripts that
start with a phrase (case/punctuation tolerant, token-boundary exact). A
prefix-only utterance arms the next one. Rejected speech interrupts racing
provider output and is not persisted. Direct browser providers mirror the gate;
backend admission is authoritative. `session.start` may override per call (iOS
sends `false` for manually started calls).

**Self-echo.** `media_rails::self_echo` tracks recent assistant utterances and
estimates a playback window from provider→client PCM bytes (24 kHz mono PCM16);
direct peer-to-peer captions get a conservative 15 s window. Checked before the
address gate: a transcript is dropped when ≥80% of its tokens are in a tracked
utterance and it arrives inside that window plus 2 s; one- or two-token
transcripts only when they exactly equal the utterance's tail. Interruption
collapses the window. Rejections use `transcript.user.ignored` reason `self_echo`.

**Telemetry.** `media.voice.surface.resolved` fires once at ready with
`voice_session_id`, `source_surface`, `surface`, `audience`, `derivation_reason`,
`agent_id`, `binding`, `tools_admitted`, `per_turn_context_enabled` — content-free,
thread hashed. `derivation_reason` separates "room because meeting bot" from
"room because unidentified". Long-lived lifecycle names (`Media`,
`user_relevant=false`): `media.config.updated`, `media.voice.surface.resolved`,
`media.voice.session.{minted,rotated,compaction,reconnect_attempt,reconnect_failed}`
(`reconnect_failed` is Warn, others Info). `rotated` carries the post-rotation
surface/audience/agent/binding; `CallState::apply_rotation` is the only
cross-rotation mutator. Dispatch refusals count via `ExternalToolRefusal` at
session/agent mismatch, absent-from-snapshot and trust denial; the message names
only what the caller supplied so it is not an enumeration oracle.

Active voice sessions do not survive a process restart (in-memory lifecycle
store; `CallState` built fresh in `start()`).

**Answering.** Owner-only personal-assistant voice may answer from authorized
scoped context and must not invent a blanket privacy refusal; shared meeting
sessions keep their private-memory prohibition. Fast memory, hybrid memory and
procedures run as independent stages under one absolute realtime deadline;
completed evidence survives a sibling timeout; barge-in discards values bound to
the cancelled turn. The result is owned by the linked Chat lifecycle (or a
bounded ephemeral voice owner), never the provider session.

Realtime tool calls use Chat's post-redaction result materializer and projector;
capability-owned `spoken_fields` select narration. OpenAI Realtime caps
historical `item.call_id` at 32 chars, so replay of a completed call/output pair
uses a deterministic alias (`openai_realtime_replay_call_id`). Gemini Live cannot
inject historical function exchanges silently; a cold rotation without a resume
handle omits that replay.

### Audio profile migration boundary

Dictation, Meeting, Listening and Hands-free resolve only through configured
surface profiles and stage options. Pre-profile installations materialize
versioned migration profiles into `magician-config.yaml` once. Preferences use
schema v3; legacy provider/observe-mode fields are read only while migrating.
Corrupt or future-version documents fail visibly; requests with removed fields
are rejected.

Web TTS defers provider, model and voice to the resolved Dictation profile;
browser speech is used only when no backend TTS provider exists. Recording
endpoints resolve Dictation when `provider` is empty/`auto`/`default`; a
concrete provider wins for that request. Native callers may pass `profile` plus
repeated `stage_option=stage:option_id`; TTS callers use `audio_profile` and
`audio_stage_options`. An explicit profile is a request-local isolation
boundary: scoped stage overrides are ignored, explicit request options still
reorder its fallback chain. Hands-free uses the immutable
`resolved_audio_profile` captured at registration. Realtime voice stays governed
by the LLM operation router.

Configured surface defaults (see
[`fluid-audio-phase9-rollout.md`](fluid-audio-phase9-rollout.md)):

| Surface | Configured default |
| --- | --- |
| Dictation | `dictation-default-v1` (`dictation-fluid-qwen3-v1` is opt-in) |
| Meeting | `meeting-vad-gated-v1` |
| Listening | `listening-vad-gated-v1` |
| Hands-free | `hands-free-local-fluid-v1` (capability-gated) |

Hands-free is advertised only when every required stage has an available
provider. Physical echo-loop and barge-in testing is a release gate.

## Realtime Voice Topologies

- `direct_peer_to_peer` — OpenAI browser live call. Magician mints the provider
  session and sends instructions/tools/resume context over the control
  WebSocket; the browser opens the vendor WebRTC peer. Audio does not cross
  Magician.
- `backend_proxied` — host-native live PTT and server-stream providers. The
  control WebSocket carries three lanes: binary PCM both ways, backend controls
  (clear input, commit-and-respond, tool result, system-message injection, end),
  and provider semantic events.

Settings stores a per-profile voice in `realtime_voices`, overlaid on the next
session mint. Web can select both topologies; iOS filters out direct-WebRTC
profiles. `realtime_voice.operation_mapping.voice_controller` and
`default_profile` point at `voice_realtime_default` (`gpt-realtime-2.1-mini`,
`openai_realtime`, `server_vad`). Host-native PTT and the meeting responder use
`voice_realtime_openai_backend` (`gpt-realtime-2.1`, `transcription_model:
local`). `voice_realtime_openai_backend_mini` is defined but unmapped, for
explicit selection.

The macOS tray live-PTT client registers `tray_macos`, opens
`/media/voice/{voice_session_id}/control` with
`realtime_profile=voice_realtime_openai_backend`, streams 24 kHz PCM while the
hotkey is held, sends `ptt.release` to commit, and plays backend PCM. It can mute
assistant audio locally without touching provider state.

### Local transcription for backend-proxied calls

Backend-proxied OpenAI profiles may set `transcription_model: local`. Before
`session.ready`, voice control opens a call-scoped stream from the Hands-free
STT profile (FluidAudio Parakeet → macOS Speech → OpenAI streaming) and tees
every 24 kHz frame to it and to the model. Attachment sends a
`ConfigureSession` setting provider input transcription to `null`; failed
attachment keeps `transcription_fallback_model`; a mid-call local failure
restores that model, and the provider must acknowledge before queued PTT commit
or audio is released.

Local `Partial`s relay as `transcript.user.partial` and never enter the chat
ledger. Finals stay staged until the authoritative boundary (PTT release, or
upstream `speech.stopped` for server VAD), which flushes and reopens local STT
before `CommitReady`; only then does the final pass `admit_user_transcript` and
become `transcript.user`. `transcript.user.cleared` silently removes an
unfinished row; `transcript.user.ignored` is reserved for semantic rejection
(prefix, self-echo). Backend-proxied PTT uses the Hands-free streaming-STT chain
without the profile VAD: release, not provider EOU, is authoritative. FluidAudio
`finish()` awaits session teardown (bounded 2 s after a 15 s ack) so a reopen
cannot collide; segmented VAD streams await the prior segment's completion
barrier before opening the next.

Content-free `media.voice.local_transcript.{state,queue,turn,fallback}` events
(`state`/`turn` Info, `queue`/`fallback` Warn). Chain rotations publish via
`magician::metrics::streaming_stt_fallback` and `media.audio.provider.fallback`.
Tests: `make test-realtime-local-transcript`; opt-in
`make test-realtime-local-transcript-live REALTIME_LOCAL_TRANSCRIPT_SESSION_EVIDENCE=…`.

### Vendor profiles

Gemini Live uses the same frontend/control-WebSocket contract: Magician keeps
`GEMINI_API_KEY` server-side, opens Google's Live WebSocket, and translates
events (JSON in text or binary frames, one parser). Assistant mode streams user
`inputTranscription` as `transcript.user.partial` and `outputTranscription` as
cumulative `transcript.assistant.delta`, so bubbles grow during the turn;
backend-proxied GPT Realtime uses the same envelopes and direct GPT accumulates
deltas in the browser. On every Gemini family `usageMetadata` rides
`turnComplete`, one step after `generationComplete`, so a response closes only
once usage is known (closing earlier prices the turn at $0).

- `voice_realtime_gemini_38_live` — `gemini-3.8-live`. Every tool declaration is
  `behavior: NON_BLOCKING`; `functionResponse.scheduling` comes from
  `tool_result_scheduling` (`when_idle` default, `interrupt`, `silent`). Its
  setup validator requires `items` on every array in a function declaration, or
  Google closes 1007 before `setupComplete` and the call rotates forever; the
  Gemini sanitizer and the pack loader both complete such arrays with
  `items: {}`. Reasoning reports `thoughtsTokenCount`, billed as text output.
- `voice_realtime_gemini_38_live_thinking` — `gemini-3.8-live-extended-thinking`.
  Adds `thinking_level` (`low`/`medium`/`high`; `minimal` rejected) as
  `generationConfig.thinkingConfig.thinkingLevel`. It acknowledges ("let me
  check…"), closes that with `turnComplete` + `interactionStatus: IN_PROGRESS`,
  runs the tool, then answers with `IDLE`. Magician forwards an
  `interaction.status` envelope (`in_progress`/`idle`) after the utterance's
  `response.done`, and clients render "Working…" (web `assistantWorking`,
  Android `RealtimeVoiceState.assistantWorking`, iOS
  `RealtimeVoiceClient.assistantWorking`). Each generation is its own billed
  turn. The factory refuses `thinking_level` on other models and
  `tool_result_scheduling` on 3.1, so the picker shows the profile unavailable
  instead of the socket closing mid-call. Gemini reasons inside the audio
  model; Magician is the brain only via the non-blocking `delegate_to_chat`.
- `voice_realtime_gemini_live` — `gemini-3.1-flash-live-preview`: blocking tool
  calls, no thinking knob, no interaction status. `display_order: 100` lists it
  last (catalog sorts by `display_order`, unset = 0, then id).
- `voice_realtime_grok` — Grok Voice (`grok-voice-latest`, provider
  `grok_voice`) on `wss://api.x.ai/v1/realtime`: backend-proxied PCM, flatter
  `session.update`, OpenAI-style function events; outside the OpenAI
  turn-context gate.
- `voice_realtime_gpt_live_1` — `gpt-live-1` (`provider: openai_live`),
  backend-proxied to `wss://api.openai.com/v1/live/sessions`. Live owns
  full-duplex turn-taking; Magician is the delegated backend
  (`delegation.type = client` → `delegate_to_chat` →
  `session.commentary.append`). `session.instructions` is the isolated
  `voice_live_mouth_system` prompt; Magician still renders the voice catalog so
  `delegate_to_chat` can authorize, but Live receives no tool list, no
  `response.create`, and is outside the Realtime turn-context gate. The brain is
  Magician's tool-using chat turn on the call's active-run token, following
  `chat.harness_engine` (GPT Realtime's `delegate_to_chat` uses the same turn).
  Delegation persists only the current utterance, strips `<speech>` tags, tracks
  each `delegation_id`, and streams the ack as `thinking.append`; Magician speech
  chunks are injected into the Live socket. Speaker PCM is prerolled 80 ms and
  paced ~40 ms so mobile does not stutter. **Billing is per second**: after the
  run loop ends (so client closes are metered) the adapter emits one
  `ResponseDone` with `RealtimeUsage.billed_seconds`; the provider's
  `session.usage.updated` / `session.closed.usage` figure is used when it
  arrives, else the held connection time, labelled `Estimated`. On session end
  the control handler sends `End` and waits `PROVIDER_CLOSING_REPORT_GRACE` for
  the closing report before closing the call. A speech start
  (`speech_start_interrupts_a_response`) cancels only when assistant audio is
  still playing — Live has no per-turn terminal, so its response id stays set all
  session. Pressing to talk remains a deliberate barge-in. The voice conformance
  lane (`make test-voice-harness-live`) gates this as `voice_call_row`. GPT Realtime
  and Gemini keep `CHAT_OUTER_LOOP` plus `voice_modality_addendum`.
- `voice_realtime_gemini_translate_en` — `gemini-3.5-live-translate-preview`,
  translation mode: no assistant instructions, tools, prefix gating or
  retrieval. `selectable: false` (speech-to-speech translator, not an
  assistant).

Gemini tests: `make test-gemini-live-models` (provider-free) and opt-in billable
`make test-gemini-live-models-live` (each shipped model through the production
provider; report `coverage/evals/gemini-live-models/latest/report.jsonl`).

Changing profiles starts a fresh upstream session on the same chat thread.
Google native resume handles are preferred over transcript replay; `goAway`
surfaces as `session.expiring` and triggers rotation. `turn_detection_mode: none`
uses Magician PTT (`activityStart`/`activityEnd`).

Web calls keep the selected PTT/open-mic mode. Direct WebRTC updates turn
detection in place; backend-proxied providers rotate with a new
`session.turn_boundary` setup and roll back the override if replacement fails.
iOS stores Realtime/Hands-free, profile and PTT choices on-device only. A
Live/Hands-free session is a transient audio-focus owner: non-call narration is
cancelled and off-call reply speech suppressed; the persisted auto-speak choice
is never changed.

OpenAI Realtime uses `POST /v1/realtime/client_secrets` (ephemeral `ek_…`,
1-minute TTL); the retired beta `/v1/realtime/sessions` returns
`beta_api_shape_disabled`. Voice-only `delegate_to_chat` resolves the
`chat_completion` profile and, for an adaptive composite, pins its
`thinking_profile`. The delegated call advertises no tools.

## Frontend modules

| Module | What it owns |
| --- | --- |
| `ui/unified-ui/src/lib/media/types.ts` | TS mirror of backend types. |
| `ui/unified-ui/src/lib/media/session.ts` | Auto-register on mount, heartbeat, post events. |
| `ui/unified-ui/src/lib/media/providers.ts` | Snapshot of `/media/providers`. |
| `ui/unified-ui/src/lib/media/preferences.ts` | Backend-hydrated schema-v3 preferences store, incl. `realtime_voices`. |
| `ui/unified-ui/src/lib/media/LiveCallVoicePanel.svelte` | Settings → Engine voices for GPT Realtime, Gemini Live, GPT Live 1. |
| `ui/unified-ui/src/lib/media/tts/browserTts.ts` | `window.speechSynthesis` wrapper. |
| `ui/unified-ui/src/lib/media/tts/providerChoices.ts` | Normalizes the backend TTS provider preference. |
| `ui/unified-ui/src/lib/media/tts/providerTts.ts` | Backend `/synthesize` / `/synthesize_message` → `<audio>`. |
| `ui/unified-ui/src/lib/media/stt/sttClient.ts` | Backend `/transcribe`. |
| `ui/unified-ui/src/lib/media/voice/realtimeVoiceClient.ts` | Voice client + `voiceCallStore`; control WS plus WebRTC peer for `direct_peer_to_peer`. |
| `ui/unified-ui/src/routes/(app)/debug/voice/+page.svelte` | Operator view for active sessions and voice event tails. |
| `ui/unified-ui/src/lib/media/voice/TrayLiveVoiceBanner.svelte` | Composer banner for active desktop live PTT. |
| `ui/unified-ui/src/lib/media/capture/{camera,mic}.ts` | Camera (`<input capture>`) and mic (`MediaRecorder`) helpers. |
| `ui/unified-ui/src/lib/magician/components/chat/{SpeakButton,AutoSpeakToggle,CameraCaptureButton,MicCaptureButton,VoiceCallButton}.svelte` | Composer + bubble affordances. |

## Provider configuration

Reply TTS providers are declared in `media.tts.providers`; recorded STT
providers in `media.recording_stt.providers`. With none configured or verified,
clients fall back to browser TTS / staged-attachment audio.

### TTS provider selection

Request `provider` is a one-request override. `auto`/`default`/empty resolves the
scoped Dictation profile's TTS stage. A concrete provider moves to the front of
the registry chain. Live voice calls are unaffected.

### TTS — local macOS (`macos_tts`)

Registered first only when `GET /host/speech/status` reports local TTS. It calls
`POST /host/speech/synthesize`; the gateway runs
`magician-macos-speech-helper synthesize` (`AVSpeechSynthesizer.write`) and
returns WAV. Voice hints may be Apple identifiers, names or locales; unknown
hints use the current locale voice. Output is always `audio/wav`.

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
      - id: minimax
        adapter: minimax_t2a_v2
        model: speech-02-hd
        voice: female-yujie-jingpin
        format: mp3
        api_key_env: MINIMAX_API_KEY
        group_id_env: MAGICIAN_MINIMAX_GROUP_ID
      - id: gemini-tts-3-1-flash
        adapter: gemini_tts
        model: gemini-3.1-flash-tts-preview
        voice: Kore
        format: wav
        api_key_env: GEMINI_API_KEY
      - id: grok-tts
        adapter: grok_tts
        model: grok-tts
        voice: eve
        format: mp3
        api_key_env: XAI_API_KEY
```

| Adapter | Endpoint shape | Config notes |
| --- | --- | --- |
| `openai_speech` | OpenAI-compatible `POST /v1/audio/speech` | `model`, `voice`, `format`, `api_key_env`, optional `base_url`. |
| `minimax_t2a_v2` | MiniMax native T2A V2 | `model`, `voice`, `format`, `api_key_env`, `group_id_env` or `group_id`, optional `base_url`. |
| `gemini_tts` | Gemini `models/{model}:generateContent`, `responseModalities: ["AUDIO"]` | `gemini-3.1-flash-tts-preview` uses `prebuiltVoiceConfig.voiceName`; `gemini-3.8-flash-tts` / `-lite-tts` use `voiceConfig.voice` and keep the transcript verbatim. `wav` wraps 24 kHz PCM. |
| `grok_tts` | xAI `POST /v1/tts`, raw bytes | `voice` is `voice_id` (`eve` default); language `auto`; `mp3`/`wav`/`pcm`. |
| `fluid_audio_kokoro_tts` | Sidecar `/v1/audio/speech` | `FluidInference/kokoro-82m-coreml`, `streaming=false`; used only when requested or placed first by a surface preference. |

`enabled: false` keeps a definition unadvertised; entries missing credentials are
omitted. If the list is absent or fully skipped, Magician uses the legacy
env-only path (`MAGICIAN_TTS_BASE_URL` / `MAGICIAN_TTS_API_KEY`, then
`OPENAI_API_KEY`, then placeholder `local`).

On chain rotation the handler strips request `voice`/`model` after the first
attempt so fallbacks use their defaults; expression hints (`emotion`, `style`,
`pace`, `voice_mode`, `emphasis`) pass through. Response headers:
`X-Tts-Provider`, `X-Tts-Model`, `X-Tts-Chain-Attempts`, `X-Tts-Fallback: true`
(non-primary only). Rotation traces on `magician::tts::fallback`.

### TTS synthesis cache

Each provider is wrapped in `CachedTtsProvider` (key: text + every delivery hint
+ voice/model/format). Errors are never cached. Process-local FIFO.

| Knob | Default | Notes |
| --- | --- | --- |
| `MAGICIAN_TTS_CACHE_CAPACITY` | `200` | Per-provider entries; `0` disables. |
| `MAGICIAN_TTS_SEGMENT_TIMEOUT_SECS` | `60` | Per-segment timeout on `/tts/synthesize_message`; timeout rotates providers. |

### Streamed segment synth — `POST /tts/synthesize_message`

Takes the full `<speech>` segment list and streams NDJSON so the client plays
segment 1 while later ones synthesize. `Upstream`/`Transport` errors rotate,
`BadRequest` short-circuits; a segment that exhausts all providers emits an
`error` envelope and the next continues.

### Recording STT (`media.recording_stt`)

Adapters: `openai_transcriptions` (`/v1/audio/transcriptions`),
`gemini_generate_content` (inline audio), `gemini_transcribe` (Gemini 3.5
Transcribe via `POST /v1beta/interactions`), `grok_stt` (`POST /v1/stt`,
`XAI_API_KEY`), `google_cloud_speech_v2` (`GOOGLE_CLOUD_ACCESS_TOKEN` + project),
`fluid_audio_recording_stt` (Qwen3 ASR 0.6B sidecar, opt-in via
`dictation-fluid-qwen3-v1`, 30 s one-shot, `NoSpeech` on empty). With none
configured the legacy env path applies (`MAGICIAN_STT_MODEL` over
`OPENAI_API_KEY` or `MAGICIAN_STT_API_KEY`, `MAGICIAN_STT_BASE_URL`).

`provider=auto`/omitted: macOS Speech first only when the host verifier
registered it, then cloud fallbacks. Selection affects only recorded clips
(`/stt/transcribe`, `/stt/transcribe/stream`, `/voice-notes`).

Benchmark Dictation STT against consented fixtures (Magician and supervisor
stopped for the default direct transport):

```bash
make benchmark-media-recording-stt \
  MEDIA_STT_BENCHMARK_ARGS='--fixtures speech-clean-en --providers macos_speech,fluid-qwen3-asr-f32'
```

`make generate-media-local-speech-fixtures` makes synthetic plumbing fixtures
only (not adoption-quality). `--allow-online` is required for online providers.
Results land in ignored `data/magician_v2/media_evals/results/`.

### STT fallback — macOS Speech

```text
Magician STT adapter
  -> MAGICIAN_HOST_GATEWAY_URL /host/speech/transcribe
  -> magician-macos-speech-helper
  -> Apple's SFSpeechRecognizer
```

Registers only when `/host/speech/status` reports the helper and Speech
authorization. Browser recordings for `macos_speech` are converted to WAV before
upload. Helper failures and stalls are provider failures; the chain rotates.

### Streaming STT (meeting "ears")

Recordings use one-shot `SttProvider`; meetings use `StreamingSttProvider`.
OpenAI adapters segment the stream and transcribe each window:

| Adapter | Model | Speaker labels | Default window |
| --- | --- | --- | --- |
| `openai_streaming_stt::OpenAiStreamingSttProvider` | `gpt-transcribe` | no | 4 s (`MEET_BOT_STT_SEGMENT_SECS`) |
| `openai_diarize_stt::OpenAiDiarizeStreamingSttProvider` | `gpt-4o-transcribe-diarize` | yes, per window | 8 s |

`POST /meetings/listen`, `POST /meetings/join` and screen observation accept
`audio_profile` and `audio_stage_options`. Multi-speaker tracks attach the
configured diarization stage when STT does not attribute speakers. VAD, STT and
diarization are independently selectable; diarization failure only removes
labels. Optional VAD failures fail open to macOS device-VAD or provider RMS;
silence never reaches STT while the gate is healthy.

`MacOsSpeechSttProvider` pushes PCM into the meet-audio helper's
`--mode transcribe` (helper: `MAGICIAN_MACOS_MEET_AUDIO_BIN`, default
`./magician-macos-meet-audio.bin`). `GeminiLiveTranscribeSttProvider`
(`gemini_live_transcribe`) opens `gemini-3.5-transcribe-live`, resamples 24 kHz
to 16 kHz, maps interim/final transcriptions to partials/finals; catalogued for
Hands-free (`hands-free-local-fluid-v1`), sessions up to 10 minutes, no
diarization, needs `GEMINI_API_KEY`.

## Voice cost accounting

Each realtime turn lands in the `llm_calls` ledger as
`operation = "voice_controller"`, priced by
`magicllm::pricing::compute_realtime_cost(model, &RealtimeUsage)`. Unknown cost is
recorded as `0.0` with `usage_reported` false. Never write `f64::NAN` — it
survives into Parquet and poisons `SUM`. Silent `$0` happens when the model has no
realtime pricing row or the provider gave no modality split.

The canonical fact prices from the model-keyed realtime table whatever the
vendor; only a `Custom` provider is held out (`llm_pricing_identity`). Aliases
`openai_realtime`, `openai_realtime_backend` and `openai_live` are OpenAI's.

Usage writes are spawned on the runtime, never on the voice actor's context: a
client closing right after `session.end` stops the actor in under a millisecond
and would cancel them.

Duration-billed models (a `per_second` rate, no token rates;
`realtime_is_duration_billed_at`) report no tokens, so recomputing would record a
false `$0.00`. `pricing_for_runtime_event` keeps the producer's figure labelled
`Estimated` (`Unknown` if not finite), and `llm_reprice` skips them as
`rows_unpriceable_skipped`. TTS (`gemini_tts.rs`) emits no usage row.

## Using local / self-hosted models

| Channel | Local server | Default URL |
| --- | --- | --- |
| TTS | macOS AVSpeechSynthesizer via the desktop host gateway | `MAGICIAN_HOST_GATEWAY_URL=http://127.0.0.1:3017` plus `/host/speech/status` `tts_available=true` |
| TTS | OpenAI-compatible (Kokoro-FastAPI, openedai-speech, LocalAI) | config `base_url` on an `openai_speech` entry |
| STT | OpenAI-compatible (faster-whisper-server, LocalAI) | `MAGICIAN_STT_BASE_URL` or config `base_url` |
| Realtime voice | no production-grade local server | — |

With a base URL and no API key the adapter sends `Bearer local`. Example: add an
`openai_speech` entry with `base_url: http://localhost:8880/v1/audio/speech`,
then `make restart-magician`.

With no provider: TTS uses `window.speechSynthesis`; STT has no browser fallback
(the mic can still attach audio as an artifact); the realtime voice button hides.

## Tray bridge wire protocol

See [`tray-bridge-protocol.md`](realtime-media/tray-bridge-protocol.md) for the
per-frame contract. The tray's live PTT client registers `tray_macos`, opens
`/media/voice/{session_id}/control`, heartbeats, posts diagnostics through
`/media/sessions/{id}/events` (`media.voice.bridge.*`, `media.voice.client_*`,
`media.voice.controller_command`), and maps tray controls to `input.clear`,
`response.interrupt` and `session.end`.

## Debugging

`/debug/voice`: active scoped sessions, tray live PTT and stale-heartbeat counts,
`media.voice_note*` / `media.voice*` tails. Voice-originated chat turns carry
`voice_origin=true`; browser realtime uses `source_surface=realtime_voice`,
desktop paths their tray surfaces. The chat ledger is the source of truth.

## Meeting participant bot

Google Meet participant bot, native on macOS (ScreenCaptureKit capture,
on-device wake, CoreAudio inject). It auto-joins in its own signed-in browser,
keys a per-meeting chat thread, and streams a display-only live transcript into
it. Spoken-reply instructions are `voice_meeting_presto_system` (compiled
fallback if render fails). Design history:
original plan,
capability/auto-join,
teardown/memory,
auto-join,
barge-in/profile,
passive listener.

Two modes share one thread resolver (`derive_meeting_thread_id` /
`resolve_meeting_thread`): attendee (`MeetingSession` + `MeetingSessionManager`)
and passive listener (`PassiveMeetingSession` + `PassiveMeetingSessionManager`).
Thread id is `meeting-<label>-<YYYY-MM-DD>`: label = slug of calendar `title`,
else Meet code, else a stable blake3 hash of the URL; date from the tool's `date`
param else the join date. `MEET_BOT_THREAD` forces one thread. Joining once with a
title and once with only the link yields two threads.

### HTTP

Under `/api/magician/v2/meetings` (`magician-api/src/meetings_api.rs`):

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/meetings` | Active sessions (both rails) + recent `meeting-*` threads. |
| GET | `/meetings/active` | Live captures only. |
| GET | `/meetings/upcoming` | Next ~12 h of calendar events via `gws` (`MEET_BOT_CALENDAR_ACCOUNTS` or `gws_accounts` ∩ `<scope>/auth/gws-<name>`). 60 s cache. |
| POST | `/meetings/listen` | Passive listen. `{"capture":"client",…}` starts client-pushed ingest. |
| POST | `/meetings/join` | Attendee join (same path as the compiled `meeting` tool). |
| GET | `/meetings/{id}` | Status (`paused`, `latest_summary`, thread, title). |
| POST | `/meetings/{id}/stop` | Stop. Client ingest revokes immediately (`202`), finishing STT/summary/memory in the background. |
| POST | `/meetings/{id}/pause` / `/resume` | `set_paused`: drop chunks before STT ("Mute Magician"). |
| POST | `/meetings/{id}/audio` | Client PCM ingest (`channel=primary\|mic`, 16 kHz mono PCM16 LE, 2 MiB cap). `410 Gone` means "stop capturing". |

UI: `/meetings` 308-redirects to `/observe`; `/meetings/[thread]` is the
transcript detail route.

### Modules

| Module | What it owns |
| --- | --- |
| `media_seam/meeting_session.rs` | `MeetingConfig`, `derive_meeting_thread_id`. |
| `media_seam/meeting_session_engine.rs` | `MeetingSession` state machine (`Idle` / `Joining` / `Listening` / `Responding` / `Left` / `Failed`), wake gate, barge-in, rolling summary, teardown. |
| `media_seam/meeting_manager.rs` | Process-global `MeetingSessionManager`: `spawn` / `join` / `status` / `leave` / `list`. Ended sessions retained 300 s. |
| `media_seam/meeting_passive.rs` | Passive listener: display audio + optional mic, no browser or responder. Auto-stop after 5 min system silence (`MEET_LISTEN_AUTO_STOP_SECS`, `0` disables); 4 h max (`MEET_LISTEN_MAX_SECS`). |
| `media_seam/meeting_pushed.rs` | Client-pushed `PushedAudioSource` + ingest registry. Idle stop 120 s (`MAGICIAN_MEETING_CLIENT_IDLE_STOP_SECS`); first-chunk grace 600 s. |
| `media_seam/meeting_memory.rs` | `MeetingMemoryWriter`; teardown merge into `user.research_findings` (`MEET_BOT_MEMORY_MAX_MEETINGS`, default 20). |
| `media_seam/meeting_transcript_sink.rs` | Display-only posts to `POST /chat/sessions/{id}/transcript`. Bounded queue (256); `MEET_BOT_STREAM_TRANSCRIPT=0` disables. |
| `media_seam/meeting_markers.rs` | Crash markers at `<scope>/workdirs/meeting_capture_markers/<session_id>.json`, drained by a boot sweep. |
| `media_seam/browser_join.rs` | `BrowserJoin` / `NoopBrowserJoin` / macOS `AgentBrowserMeetJoiner`. |
| `media_seam/meeting_bridge_macos.rs` | `ScreenCaptureAudioSource` + `CoreAudioSink` via `magician-macos-meet-audio`. |
| `media_seam/meeting_responder.rs` | `MeetingResponder` trait. |
| `media_seam/meeting_orchestrator_voice_responder.rs` | Default responder. |
| `media_seam/meeting_magician_agent_responder.rs` | REST agent → OpenAI TTS (`agent-tts`). |
| `media_seam/meeting_llm_tts_responder.rs` | `meeting_response` operation → TTS (`llm-tts`). |
| `media_seam/meeting_realtime_responder.rs` | Bot-local realtime, no agent tools (`realtime-direct`). |
| `media_seam/meeting_summarizer.rs` | `meeting_summary` operation (local Ollama / Gemma); a missing router fails explicitly. |
| `execution/compiled_providers.rs` | `MeetingCapabilityProvider`. |
| `execution/embedded_pack_defs/meeting.yaml` | Compiled `meeting` pack (`join` / `status` / `leave` / `list`), Google Meet only. |
| `magician/examples/meet_bot.rs` | Terminal harness: `cargo run -p magician --example meet_bot`. |

(Paths under `magician/src/magician_v2/`.) Scope precedence:
`MEET_BOT_PRINCIPAL` / `MEET_BOT_WORKSPACE` pins, then the agent's scope, then
`anonymous` / `default`.

### Join, capture, barge-in, teardown

`MeetingSession::start()` joins before opening capture, retargets capture to the
launched PID when returned, polls every 5 s for removal (fires the unified cancel
on `!is_in_meeting()`), and leaves on teardown. If STT fails to open after join,
it hangs up before bailing.

`AgentBrowserMeetJoiner` (macOS):

- Saves the default input and flips it to `BlackHole 16ch` (SwitchAudioSource),
  restoring on every failure path and on `leave()`.
- Launches a headed agent-browser session with
  `content_acquisition.browser.engine` (same resolver as the `browser` tool); an
  unresolvable engine disables automatic join.
- Profile: `MEET_BOT_PROFILE_DIR`, else `<scope workdir>/meet-bot-profile`, else
  a unique temp dir. Applied via `AGENT_BROWSER_PROFILE`, never a raw
  `--user-data-dir` (that desyncs DevTools-port discovery). A stable profile
  means one meeting at a time. A pre-launch sanitizer deletes the tab-restore
  cache.
- Admission is a wall-clock-bounded self-healing loop. Default capture is
  **whole-display audio** because SCK's app-filtered capture is silent for the
  cloak Chromium engine. Display audio includes the bot's own voice, so the
  session is **half-duplex** (capture drops chunks while the sink plays) and
  **barge-in is unavailable**. `MEET_BOT_TARGET_BUNDLE_ID` or
  `MEET_BOT_CAPTURE=bundle` selects app-filtered capture where SCK attribution
  works.

Barge-in (app-filtered capture only): TTS goes up BlackHole, so STT activity
during a reply is a human. `speaking: AtomicBool` is true only while a reply (not
a cue) plays; `interrupt` holds a `CancellationToken`; the listen loop calls
`maybe_barge_in()` on Partial and non-empty Final. `CoreAudioSink::play_pcm`
sets `kill_on_drop(true)` on the inject helper.

Wake gate: an addressed final, or an addressed partial settled ~1.5 s. A repeat
of the just-answered utterance is ignored for ~8 s. Replies are non-blocking; a
wake during a reply gets a spoken "still busy". Cues ("On it") use the session's
TTS voice and are serialized with the answer.

Teardown order: hang up, bounded tail drain (STT flush), final `resummarize()` +
memory write, then `Left`. `MeetingSessionManager::leave` awaits teardown
(bounded ~10 s) and returns the final summary when it lands in time.

Memory write: `meeting_summary_to_fields` parses `{ summary, decisions[],
action_items[] }`; `ScopedMeetingMemoryWriter` merges into
`user.research_findings` keyed `meeting:<thread-id>` and files a best-effort
`Observed` MemoryFact learning candidate. Default writer is
`NoopMeetingMemoryWriter`.

Helper and setup: `./magician-macos-meet-audio.bin` (override
`MAGICIAN_MACOS_MEET_AUDIO_BIN`) is staged by `make build-macos-speech-helper` and
the `build-all-*` targets. `make setup-meet-bot` (also run by `make install`)
installs BlackHole 16ch and `switchaudio-osx`. Capture needs the **Screen
Recording** TCC grant. `--mode inject` plays PCM to BlackHole; `--target-pid`
filters by `processID` (precedence over `--target-bundle-id`). On Linux
(`meeting_bridge_linux.rs`) the same join uses Pulse: `pacat` writes
`magician_meet_mic`, `parec` reads `magician_meet_capture.monitor` or the default
sink monitor, and Xvfb supplies `DISPLAY` when absent. Loading the null-sinks
restores the previous default sink; leave restores desktop devices only when no
other attendee remains, never makes the virtual mic the desktop microphone, and
the passive mic never records `magician_meet_mic.monitor`.

### Responders

`default_meeting_responder` returns `OrchestratorVoiceResponder` unless
`MEET_BOT_RESPONDER` is `orchestrator` (default) / `agent-tts` / `llm-tts` /
`realtime-direct` / `noop`.

`OrchestratorVoiceResponder` is a headless client of
`/media/voice/{id}/control`: registers `surface_type: meeting_bot`, starts
`voice_realtime_openai_backend` (`turn_detection: none`), and on wake streams the
question audio (`ptt.engage` → 24 kHz PCM → `ptt.release`) after silently seeding
the changed rolling summary as `user.text` with `inject: true`. Text-in is the
fallback when no audio was captured. Env: `MEET_BOT_MAGICIAN_URL` (default
`http://127.0.0.1:3002/api/magician/v2`), `MEET_BOT_BEARER_TOKEN` /
`MAGICIAN_BEARER_TOKEN`, `MEET_BOT_REALTIME_PROFILE`. Meeting join stays on this
wake + `turn_detection: none` path rather than GPT-Live-1 until a Live-1 meeting
profile is proven not to barge in. `persists_to_chat()` is true for
orchestrator-voice and agent-tts, so the transcript sink skips addressed turns.

### Passive listener

Does not join. Captures display audio plus, opt-in, the user's mic
(`--mode capture-mic`, Microphone TCC only): system track on the diarize
provider, mic track on the plain provider labelled `You`. Mic defenses: OS voice
gate (macOS 14+), RMS gate (`MEET_STT_SILENCE_RMS`, ≈ −49 dBFS), and echo
suppression (mic final ≥60% token-covered by recent system finals, ≥35% if system
audio played ≤4 s ago). Raw audio is never persisted. Idempotent per thread: a
second client listen reuses the live one; colliding with a live host capture
returns `409 already_observed`. Shutdown gracefully stops captures on both rails.

### Client-pushed meeting capture

`POST /meetings/listen` with `{"capture":"client",…}` builds the same pipeline
with `PushedAudioSource`s and returns `"capture_source":"client"` plus a
short-lived `"upload_token"`. Ingest:
`POST /meetings/{id}/audio?channel=primary|mic&seq=N`, one raw `audio/pcm` chunk.
`primary` is diarized room audio; `mic` is the hard-"You" track, used only when
`mic:true`. Newest-drop under backpressure. Unknown/ended/mismatched → uniform
`410 Gone`; bad channel `400`; oversized `413`. Single-tenant: do not expose
ingest on the LAN.

### Client-pushed screen observation

`magician-media/src/media_rails/screen_observe.rs`: `FrameSource::Host`
(host-gateway capture) or `FrameSource::Pushed` (newest-wins slot). HTTP
(`magician-api/src/screen_api.rs`):

- `POST /screen/observe/client/start` `{ thread, title?, purpose?, mode?, cadence_s?, max_minutes?, deep_observation? }` → `{ observe_id, upload_token, thread, status }`.
- `POST /screen/observe/frame?observe_id&principal&workspace` with `X-Upload-Token` and a raw JPEG/PNG body (≤ 4 MiB) → `202`, or `410 observation_gone` for unknown session / scope mismatch / bad token.

Client observations have their own multi-session registry; host observation
stays single. Pass the meeting thread so screen notes weave into the meeting
transcript.

## Voice-created tasks render their chat card

After `create_task`, `orchestrate_pipeline` or `delegate_to_agent`,
`dispatch_external_tool_call` hands every result `task_id` to
`subscribe_external_spawned_tasks`, which admits ids whose
`manifest.chat_session_id` is this session and not terminal, drops any ephemeral
voice-turn fanout, and re-subscribes with `chat-task-<task_id>-<session_id>` so
steps land on the persistent `TaskStatusUpdate` card. These tools get
`effective_cancel = None`, so hanging up does not cancel background work; inline
tools keep the call-wide token.

When such a task finishes, `task.completed` is pushed to the live call, routed by
`chat_session_id` (`VoiceDownstreamFanout::link_chat_session`). `failed` /
`cancelled` announce from `persist_execution_outcome`; `completed` waits for
`finalize_terminal_execution` and speaks `speech_live` from `voice_speech.json`
(32 KiB ceiling), else `voice_summary_excerpt`. Off-call, the card carries
`speech_tts` and `maybeSpeakTaskCompletion` reads it when auto-speak is on and no
call is active.

`replay_turn_from_message` emits a turn for terminal `TaskStatusUpdate` cards so
resume sees request → ack → completion and does not re-fire.
`dispatch_chat_tool_call` blocks an identical-goal task in the same session
(`duplicate_blocked`; `allow_duplicate: true` after user confirmation).
`ChatLlmService::neutralized_stale_task_call_ids` rewrites earlier-turn
`orchestrate_pipeline` / `create_task` / `create_monitor` calls into a plain assistant note.

## Wake-started voice and call lifecycle

- `fireWake` starts `voice_mode=hands_free` with continuous capture; the local
  cascade owns VAD boundaries using the Hands-free profile.
- In vendor Realtime, `setPushToTalkMode` toggles PTT vs provider VAD. Direct
  WebRTC updates in place; backend-proxied rotates (activity mode is
  setup-bound) while the browser keeps the same call and socket via
  `audio.rebind`.
- OpenAI Realtime `error` events are request-scoped; only a terminal WebRTC
  failure requests reconnect. An utterance that never created a response emits
  no `response.cancel`. Rebinds preserve `connectedAt`.

## VibeDev preview dev server

`magician/src/magician_v2/media_seam/dev_server_manager.rs` runs a project's preview (`npm run dev` /
`vite`) as its own process-group leader (`process_group(0)`).
`DevServerSession::kill()` signals the whole group (`SIGTERM`, ~400 ms grace,
`SIGKILL`) before reaping — used by `stop()` and idle eviction (`reap()`).

## What's intentionally not done

Per-agent voice routing: every session resolves to `voice_realtime_default`.
Branching `operation_mapping` (e.g. by persona) would be config-only.
