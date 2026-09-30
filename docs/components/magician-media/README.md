# Magician Media

The media plane as a satellite crate over the magician lib (~32k lines): voice
orchestration, fluid audio, screen capture/observe, hands-free and self-echo,
local realtime transcript, the concrete STT/TTS providers (Gemini
generateContent STT, Gemini 3.5 Transcribe file STT, Gemini 3.5 Live Transcribe
streaming STT, google/macos/minimax/openai), and the work-module stores
`scheduling`, `run_state`, `obligation_sweeps`, `run_inbox`, and
`reply_routing`. `magician-api`'s work-modules API consumes all five;
`magician-bin` installs the scheduling reader and the obligation-sweep worker.

The crate uses the monolith's compiler recursion limit (256) when proving `Send`
for delegated chat futures (resolves E0275; no runtime effect).

## Calls and concurrency

Concurrent owner calls negotiate their capability before session startup. They
do not reserve the displayed chat's execution slot, so text can run and queue
while the call stays connected. Legacy vendor calls keep their call-wide
sentinel. Concurrent Tutor/App Copilot takeovers claim an available slot only
for that turn and cannot replace a running text turn. Call teardown does not
clear another modality's slot. See the
[mixed input contract](../unified-ui/concurrent-voice.md#mixing-typed-messages-with-a-live-call).

## Realtime voice engines

- `RealtimeUsage` carries `billed_seconds` beside token buckets for providers
  that bill by the clock (GPT-Live). The orchestrator prices the turn with
  `compute_realtime_cost` at `RealtimePricing.per_second`, so a tokenless session
  still has a cost.
- Voice/meeting emitters of `LLMResponseReceived` always set `search_calls: 0`
  (those lanes never carry server-side search).
- A Settings `realtime_voices` choice is loaded at control-socket connect and
  overlaid on the next Live/Realtime mint and rotation.
- Backend-proxied OpenAI sends a bootstrap `session.update` on connect so the
  upstream socket is not idle while instructions render; a dropped provider
  socket rotates instead of ending the call. Later turns cancel any open
  `response` before `response.create`.
- Gemini 3.1 Flash Live and backend-proxied GPT Realtime stream turn captions as
  they arrive.
- GPT-Live-1 is a selectable backend-proxied full-duplex engine
  (`voice_realtime_gpt_live_1`, `openai_live`) on `/v1/live/sessions`. Its
  `session.instructions` is `voice_live_mouth_system`; GPT Realtime and Gemini
  keep the chat outer-loop plus `voice_modality_addendum`.
- Client delegation runs Magician's tool-using chat turn on the call's owned
  active-run token, following `chat.harness_engine`, so Live can be the mouth for
  Magician or another roster harness. GPT Realtime's `delegate_to_chat` uses the
  same turn. Meeting / Tutor / App Copilot / envoy stay on Magician's LLM.
- Speaker PCM is paced (~40 ms frames, 80 ms preroll) so mobile playback does
  not stutter.
- Meeting join uses wake + `voice_realtime_openai_backend`; Presto session
  instructions are `voice_meeting_presto_system`.
- Gemini 3.5 Live Translate is a speech translator on the same WebSocket: no
  tools, no memory, not selectable in the engine picker. Its transcription config
  stays on `setup` (not inside `generationConfig`), or Google 1007-loops.
- `RealtimeVoiceProfile` fields `thinking_level`, `tool_result_scheduling` and
  `display_order` (Gemini 3.8 Live) are unset on the synthesised hands-free
  profile: that provider has no reasoning knob, blocks on its tools, and is never
  listed in the picker.
- Identified owner surfaces mint a one-shot owner-session credential on media
  registration, but `VoiceOrchestrator::start` keeps that fence only when the
  selected realtime profile is attested in `app_platform.processing.profiles` as
  a backend-proxied route. Browser DirectPeerToPeer (`openai_realtime`) and
  unattested native profiles drop the credential and continue as ordinary
  personal voice rather than fail the call.

### Guided-flow takeover

`submit_tutor_takeover_turn` (`media_rails::voice_orchestrator`) resolves the
chat lane owning the turn's feature mode from
`magician_v2::chat::lane_seam::registered_lanes()`, admits it only if it keeps
the Tutor chat runtime tools (Tutor and App Copilot), and answers the lane's
`admission_surface().as_str()` (`"tutor"` / `"app_copilot"`). Every other mode
fails closed with the takeover's existing error. A new tutor-runtime lane joins
by registering, not by editing the orchestrator.

## Lib-side seam

`magician_v2::media_seam` (~21k lines) holds everything the lib consumes in
production paths:

* the **meeting engine** — session engine, manager, responders (LLM TTS,
  magician-agent, orchestrator voice, realtime), macOS bridge, Linux Pulse
  bridge, passive listener, pushed join, transcript sinks, markers, summarizer,
  memory (consumed by `execution::compiled_providers`);
* the **provider type layer** — `MediaProviderRegistry`, STT/VAD/diarization
  traits and events, cached/configured/OpenAI TTS, streaming STT types
  (re-exported as `media_seam::providers`);
* the **runtime-config cluster** — `AudioRuntimeConfigManager`, preferences
  store, streaming fallback, diarized streaming, and `vad_gate` pipeline
  resolution (`AudioPipelineServices` is installed at boot, so its field types
  must be nameable by the lib);
* vocabulary: meeting thread identity/continuity (`binding`), browser join,
  audio surfaces/profiles, voice fanout, speech segments, host automation,
  dev-server detection, runtime validation.

The crate re-exports it flat (`magician_media::*`) and under
`media_rails::meeting::*` / `media_rails::providers::*`, and owns the concrete
engines on top. `magician-api`, `magician-bin`, `magician-comms`, and
`magician-surfaces` depend on `magician-media`; the lib uses it only as a
dev-dependency; `magician-apps` does not. The seam reads defaults from the
shipped template config.

## Meeting audio

macOS meeting audio is ScreenCaptureKit plus BlackHole 16ch. Linux uses Pulse
(`meeting_bridge_linux`): `pacat` writes `magician_meet_mic`, and `parec` reads
`magician_meet_capture.monitor` when the browser's sink-inputs were moved there,
otherwise the desktop sink monitor. Creating the null-sinks saves and later
restores the previous default sink. Leave moves only that browser's streams off
the capture sink and restores desktop defaults only when no other attendee
remains. The saved microphone is never the virtual mic monitor; the passive
microphone skips `magician_meet_mic.monitor`. Other hosts return the capture
error and construct `NoopBrowserJoin`.

## Meeting control

These live in the media seam because they must be singular across the
first-party API and the meetings app. See
[meetings-surface.md](../magician/meetings-surface.md).

- `media_seam::meeting_control_audit` — per-scope append-only JSONL of every
  capture control, accepted or refused, at
  `<scope>/workdirs/meeting_control_audit/<UTC date>.jsonl`. Appended by the
  `/meetings/{listen,join,stop,pause,resume}` handlers and by the
  `magician.meeting-control` app destination; a row names the door
  (`first_party_api` / `app_control_destination`), verb, outcome and, for the
  app door, installation, signed decision and gesture. It also projects a signed
  control result into a `control_receipt` row. Writes are best-effort: refusing
  to stop a capture because its record failed would invert the risk.
- `media_seam::meeting_calendar` — gws calendar read, account resolution,
  dedupe/chronological merge and short-lived cache, shared by
  `GET /meetings/upcoming` and the `meetings_data` app binder. The cache is keyed
  by the scope's capability auth root so one scope's calendar is never served to
  another. gws is resolved with `runtime_core::process::resolve_program` against
  the child's scoped PATH (stays on `posix_spawn`).
- `media_seam::meeting_capture_reservation` — process-global lock an
  app-initiated start holds across both its liveness probe and the start, so two
  signed starts cannot both see "nothing live". First-party starts do not take
  it: the guarantee is "an app never opens the second capture", not "at most one
  capture exists".
- START requires a host-minted, expiring `AppMeetingStartIntentTicket` from a
  live interactive gesture. Why: a signed display proves the owner saw a window,
  not that a person acted, so a signed start cannot be banked and replayed.
- Meeting status views carry `started_seconds_ago` / `ended_seconds_ago` from a
  monotonic `Instant` (immune to clock changes) and the owning
  `(principal, workspace)`, because the registries are keyed by session id alone.
  Each rail publishes its retained-session window (`ATTENDEE_ENDED_RETAIN_SECS` /
  `PASSIVE_ENDED_RETAIN_SECS`). `ManagedPassive` therefore carries `started_at`
  and `scope`; test constructors must supply both. The test module in
  `meeting_passive.rs` globs `crate::magician_v2::media_seam::*`, not
  `super::*`, so it imports `std::time::Instant` explicitly.

Meeting-memory evidence distillation passes the scoped prompt manager through
the shared tier producer; its sampled decision reference rechecks the meeting
roll-up rows in the user-memory tier.
