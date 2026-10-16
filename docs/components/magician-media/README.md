# Magician Media

The media plane as a satellite crate over the magician lib (~36k lines, version
0.1.8): voice orchestration, fluid audio, screen capture/observe, hands-free
and self-echo, local realtime transcript, the concrete STT/TTS providers
(Gemini generateContent STT, Gemini 3.5 Transcribe file STT, Gemini 3.5 Live
Transcribe streaming STT, Grok and Sarvam file STT and TTS,
google/macos/minimax/openai), and the work-module stores `scheduling`,
`run_state`, `obligation_sweeps`, `run_inbox` and `reply_routing`.
`magician-api`'s work-modules API consumes all five; `magician-bin` installs
the scheduling reader and the obligation-sweep worker. Negotiation offer
bindings and frozen reply Work provenance persist across cold reads.

The crate uses the monolith's compiler recursion limit (256) to prove `Send`
for delegated chat futures (resolves E0275; no runtime effect). `media_rails`
re-exports `sarvam_tts_default_voice` with the Sarvam STT/TTS providers.

## Sarvam Bulbul TTS

The `sarvam_tts` adapter supports Bulbul v2, v3 and v4 Flash over the existing
REST endpoint. The seed keeps v3 as `sarvam-bulbul` and adds
`sarvam-bulbul-v4-flash` to the same three Dictation/Hands-free surface lists.
V4 defaults to `shubh_enhi_ads` and advertises all 222 published persona IDs
from `providers/sarvam_bulbul_v4.rs`. Known voices from another model
generation normalize to the target model's default; configured catalogs omit
those mismatches. Marathi personas preserve the Marathi language hint for
Devanagari text. Custom future upstream voice IDs still pass through.

V4 advertises the nine languages with published personas; v3 keeps Malayalam
and Odia coverage. Those two language codes are accepted by v4's API, but
have no published v4 personas yet. The adapter remains non-streaming and
keeps v3's 2,500-character chunking and 24 kHz `mp3`/`wav`/`pcm` output.
See [configuration, tests and voice selection](../magician/realtime-media-rails.md#sarvam-bulbul-v4-flash).
Voice pickers in web/iOS/Android Settings and composer panels consume the
backend catalog, including local FluidAudio/Kokoro. Scoped `tts_voices` choices
are applied to single and segmented speech and captured for Hands-free calls.
Fallbacks use their own saved voice; explicit request overrides take precedence.
OpenAI, Gemini and MiniMax now advertise their built-in catalogs as well.
Installed macOS voices arrive through the host speech status probe.

## Calls and concurrency

The macOS Speech STT provider opts into the bounded artifact-review port. Its
configuration hash binds the exact model and literal loopback gateway URL; the
path permits no proxy, redirect, credentials, query or fragment. A separate
streamed client enforces the caller's response-byte ceiling and cancellation,
with no provider fallback. Normal voice transcription is unchanged, and other
providers are not qualified for this zero-cost review port. Relentless caches
sampled WAV bytes and exact speech receipts for review and restart;
partial or unknown speech cannot establish a perceptual pass. No test covers
the bounded review path yet (`macos_speech.rs` tests only the normal
transcribe call).

Concurrent owner calls negotiate their capability before session startup. They
do not reserve the displayed chat's execution slot, so text can run and queue
while the call stays connected. Legacy vendor calls keep their call-wide
sentinel. Concurrent Tutor/App Copilot takeovers claim a free slot only for
that turn and cannot replace a running text turn. Call teardown does not clear
another modality's slot. See the
[mixed input contract](../unified-ui/concurrent-voice.md#mixing-typed-messages-with-a-live-call).
`delegate_to_chat` refuses an intent that quotes a sentence of the
concurrent-voice instructions (`intent_echoes_concurrent_voice_instructions`):
only a provider replying with no user turn produces one, and admitting it would
start work and speak the prompt back as a result.

## Realtime voice engines

- The realtime turn-context render is boxed (`Box::pin`) where the voice
  orchestrator and the chat service's select await it, so its future does not
  sit on the caller's stack.
- `RealtimeUsage` carries `billed_seconds` beside token buckets for providers
  that bill by the clock (GPT-Live). The orchestrator prices the turn with
  `compute_realtime_cost` at `RealtimePricing.per_second`, so a tokenless
  session still has a cost.
- Voice and meeting emitters of `LLMResponseReceived` always set
  `search_calls: 0` (those lanes never carry server-side search).
- A Settings `realtime_voices` choice is loaded at control-socket connect and
  overlaid on the next Live/Realtime mint and rotation.
- Backend-proxied OpenAI sends a bootstrap `session.update` on connect so the
  upstream socket is not idle while instructions render; a dropped provider
  socket rotates instead of ending the call. Later turns cancel any open
  `response` before `response.create`.
- Gemini 3.1 Flash Live and backend-proxied GPT Realtime stream turn captions
  as they arrive.
- GPT-Live-1 is a selectable backend-proxied full-duplex engine
  (`voice_realtime_gpt_live_1`, `openai_live`) on `/v1/live/sessions`. Its
  `session.instructions` is `voice_live_mouth_system`; GPT Realtime and Gemini
  keep the chat outer loop plus `voice_modality_addendum`.
- Client delegation runs Magician's tool-using chat turn on the call's owned
  active-run token, following `chat.harness_engine`, so Live can be the mouth
  for Magician or another roster harness. GPT Realtime's `delegate_to_chat`
  uses the same turn. Meeting, Tutor, App Copilot and envoy stay on Magician's
  LLM.
- GPT-Live speaker PCM is paced into 40 ms frames after an 80 ms preroll
  (`magicllm::realtime::openai_live`) so mobile playback does not stutter.
- Meeting join uses wake + `voice_realtime_openai_backend`; Presto session
  instructions are `voice_meeting_presto_system`.
- Gemini 3.5 Live Translate is a speech translator on the same WebSocket: no
  tools, no memory, not selectable in the engine picker. Its transcription
  config stays on `setup` (not inside `generationConfig`), or Google
  1007-loops.
- `RealtimeVoiceProfile` fields `thinking_level`, `tool_result_scheduling` and
  `display_order` (Gemini 3.8 Live) are unset on the synthesised hands-free
  profile: that provider has no reasoning knob, blocks on its tools and is
  never listed in the picker.
- Identified owner surfaces mint a one-shot owner-session credential on media
  registration, but `VoiceOrchestrator::start` keeps that fence only when the
  selected realtime profile is attested in `app_platform.processing.profiles`
  as a backend-proxied route. Browser DirectPeerToPeer (`openai_realtime`) and
  unattested native profiles drop the credential and continue as ordinary
  personal voice instead of failing the call.

### Guided-flow takeover

`submit_tutor_takeover_turn` (`media_rails::voice_orchestrator`) resolves the
chat lane owning the turn's feature mode from
`magician_v2::chat::lane_seam::registered_lanes()`, admits it only if it keeps
the Tutor chat runtime tools (Tutor and App Copilot), and answers the lane's
`admission_surface().as_str()` (`"tutor"` / `"app_copilot"`). Every other mode
fails closed with the takeover's existing error. A new tutor-runtime lane joins
by registering, not by editing the orchestrator.

## Lib-side seam

`magician_v2::media_seam` (~27k lines) holds everything the lib consumes in
production paths:

* the **meeting engine**: session engine, manager, responders (LLM TTS,
  magician-agent, orchestrator voice, realtime), macOS bridge, Linux Pulse
  bridge, passive listener, pushed join, transcript sinks, markers,
  summarizer, memory (consumed by `execution::compiled_providers`);
* the **provider type layer**: `MediaProviderRegistry`, STT/VAD/diarization
  traits and events, cached/configured/OpenAI TTS, streaming STT types
  (re-exported as `media_seam::providers`);
* the **runtime-config cluster**: `AudioRuntimeConfigManager`, preferences
  store, streaming fallback, diarized streaming, and `vad_gate` pipeline
  resolution (`AudioPipelineServices` is installed at boot, so its field types
  must be nameable by the lib);
* vocabulary: meeting thread identity/continuity (`binding`), browser join,
  audio surfaces/profiles, voice fanout, speech segments, host automation,
  dev-server detection, runtime validation.

The crate re-exports it flat (`magician_media::*`) and under
`media_rails::meeting::*` / `media_rails::providers::*`, and owns the concrete
engines on top. `magician-api`, `magician-bin`, `magician-comms` and
`magician-surfaces` depend on `magician-media`; the lib uses it only as a
dev-dependency; `magician-apps` does not. The seam reads defaults from the
shipped template config.

## Meeting audio

macOS meeting audio is ScreenCaptureKit plus BlackHole 16ch. Linux uses Pulse
(`meeting_bridge_linux`): `pacat` writes `magician_meet_mic`, and `parec` reads
`magician_meet_capture.monitor` when the browser's sink-inputs were moved
there, otherwise the desktop sink monitor. Creating the null-sinks saves and
later restores the previous default sink. Leave moves only that browser's
streams off the capture sink and restores desktop defaults only when no other
attendee remains. The saved microphone is never the virtual mic monitor; the
passive microphone skips `magician_meet_mic.monitor`. Other hosts return the
capture error and construct `NoopBrowserJoin`.

A `web.whatsapp.com` join uses the same browser and audio path.
`https://web.whatsapp.com/` waits for an incoming voice call;
`https://web.whatsapp.com/send?phone=<8-15 digits>` places one. The incoming
signal is an Accept (or Answer) button together with a Decline (or Reject)
button; `GET /meetings/{id}` then reports `IncomingCall` until End call is on
screen. The answer wait is `MAGICIAN_WHATSAPP_CALL_WAIT_SECS` (default 600,
clamped 30–1800); a placed call has 90 seconds. See
[meetings.md](../magician/meetings.md).

`GET`/`POST /meetings/whatsapp-ring` handles a call already open in everyday
Chrome. Join uses `NoopBrowserJoin` (no new window, no system-mic flip) and
streams the call to the realtime session.

- **Who answers.** Google Meet seats Envoy. A WhatsApp call seats the personal
  assistant only when the other party is a phone number matching the
  configured owner number; a contact name seats Envoy.
- **Public calls.** A public call keeps that agent's own tools, and
  `delegate_to_chat` runs as the same agent. Search cannot use the owner's
  signed-in Chrome. An untrusted call gets no learned procedures, does not
  advertise `delegate_to_agent` or `handover_to_agent`, and does not take the
  scope voice slot that speaks cockpit diff approvals.
- **Controls.** Cancel on a call Magician has joined, or is still starting,
  also stops that session. Dictation and hands-free are not used. Listen uses
  the passive rail with `capture_mic` false and the same Chrome bundle. Idle
  starts neither session.
- **Profile and auth.** `realtime_profile` is the selected id from the
  selectable realtime catalog (the list `GET /media/providers` returns); the
  stored default is `voice_realtime_openai_backend`, and an id the catalog
  does not list stays on that default. The request bearer is passed to the
  voice session for Join and never written into the process environment.

The ring reducer lives in `whatsapp_call_ring.rs`.

## Meeting control

These live in the media seam because they must be singular across the
first-party API and the meetings app. See
[meetings-surface.md](../magician/meetings-surface.md).

- `media_seam::meeting_control_audit`: per-scope append-only JSONL of every
  capture control, accepted or refused, at
  `<scope>/workdirs/meeting_control_audit/<UTC date>.jsonl`. Appended by the
  `/meetings/{listen,join,stop,pause,resume}` handlers and the
  `magician.meeting-control` app destination; a row names the door
  (`first_party_api` / `app_control_destination`), verb, outcome and, for the
  app door, installation, signed decision and gesture. A signed control result
  is also projected into a `control_receipt` row. Writes are best-effort:
  refusing to stop a capture because its record failed would invert the risk.
- `media_seam::meeting_calendar`: gws calendar read, account resolution,
  dedupe/chronological merge and short-lived cache, shared by
  `GET /meetings/upcoming` and the `meetings_data` app binder. The cache is
  keyed by the scope's capability auth root, so one scope's calendar is never
  served to another. gws is resolved with
  `runtime_core::process::resolve_program` against the child's scoped PATH
  (stays on `posix_spawn`).
- `media_seam::meeting_capture_reservation`: a process-global lock an
  app-initiated start holds across its liveness probe and the start, so two
  signed starts cannot both see "nothing live". First-party starts do not take
  it: the guarantee is "an app never opens the second capture", not "at most
  one capture exists".
- START requires a host-minted, expiring `AppMeetingStartIntentTicket` from a
  live interactive gesture. A signed display proves the owner saw a window,
  not that a person acted, so a signed start cannot be banked and replayed.
- Meeting status views carry `started_seconds_ago` / `ended_seconds_ago` from
  a monotonic `Instant` (immune to clock changes) and the owning
  `(principal, workspace)`, because the registries are keyed by session id
  alone. Each rail publishes its retained-session window
  (`ATTENDEE_ENDED_RETAIN_SECS` / `PASSIVE_ENDED_RETAIN_SECS`). `ManagedPassive`
  therefore carries `started_at` and `scope`; test constructors must supply
  both. The test module in `meeting_passive.rs` globs
  `crate::magician_v2::media_seam::*`, not `super::*`, so it imports
  `std::time::Instant` explicitly.

Meeting-memory evidence distillation passes the scoped prompt manager through
the shared tier producer; its sampled decision reference rechecks the meeting
roll-up rows in the user-memory tier.

## Scheduling integration

The negotiation store stays provider-neutral. Typed calendar free/busy and
create/update/delete actions use the existing skill runtime and outward gate;
Work's hold/release routes require scoped confirmed resource effects,
including the accepted interval and counterparty. `bind_offer` associates an
actual send and an explicitly authorized reply agent. Signed inbound adapters
route only explicit parent-message references; one deterministic ordinary Work
item per source performs language interpretation under normal grants, review
and budgets. Unverified senders require task approval. Captured or unknown
effects cannot book or release a hold. The Work Operations panel pages
scheduling and delivery status.

Implementation and qualification are tracked in
the integration plan.
Live calendar and outbound communications are not yet qualified.

`work-modules` in-app calls keep their typed contract and use the host-owned
local bridge with a per-call scoped credential, revoked on completion or
cancellation. The shared route table excludes submit/sweep and accepts no
caller-selected scope or endpoint; no bearer reaches a child or model.

A worker binds only its own agent as the reply receiver; the owner route can
explicitly select a different eligible agent. Ambiguous associations and
missing receivers produce an approval-required Work item with the original
source, rather than dropping it or choosing an ask from prose.

Reply bindings keep an originating task reference and brief digest. Matched
Work inherits that approved brief and goal, plus the completed task reference
when available; changed or unavailable source briefs require review. The brief
stays in the task store and is not copied into the negotiation book.

The 2026-10-15 catalog audit expands Grok TTS to all 28 API-listed voices and Gemini 3.8 TTS to all 2,089 public prebuilt IDs (three pages, studio aliases deduplicated); Gemini 3.1 keeps its 30 studio voices. MiniMax’s 332 IDs match `get_voice` exactly, including Unicode punctuation. Root/live allow-lists expose 37 Bulbul v3 speakers and the pinned FluidAudio SDK’s 20 supported American English voices.

Registered TTS adapters supply the selectable voice catalog and normalized model/default voice. The config-only fallback contributes unavailable bindings without overwriting an available adapter with an omitted or stale `voices` allow-list.
