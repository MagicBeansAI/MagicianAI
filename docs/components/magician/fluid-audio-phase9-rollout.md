# FluidAudio Phase 9 Rollout

Phase 9 makes the alternate local audio engine operable. Machine-readable
authority: `data/magician_v2/media_evals/phase9-rollout-manifest.json`.
Ignored per-run transcripts and measurements stay under
`data/magician_v2/media_evals/results/`.

## Default Decisions

| Surface | Configured default | Decision |
| --- | --- | --- |
| Dictation | `dictation-default-v1` | Keep Apple Speech and the existing online fallback behavior. `dictation-fluid-qwen3-v1` remains opt-in until consented natural and code-switched speech passes. |
| Meeting | `meeting-vad-gated-v1` | Use FluidAudio Silero as an optional fail-open gate while preserving the established OpenAI diarized, Apple, then OpenAI streaming order. |
| Listening | `listening-vad-gated-v1` | Use the same optional local VAD gate and retain Apple-first streaming STT. |
| Hands-free | `hands-free-local-fluid-v1` | Keep the local cascaded profile as a capability-gated preview. It is not general availability until physical echo and barge-in gates pass. |

`make verify-media-audio-rollout` fails when the configured profiles or stage
orders drift from these decisions. It also checks config parity, the exact
FluidAudio SDK pin, macOS bundle resources, and the non-Apple container
boundary.

Benchmark history lives in the rollout manifest. The cross-stage suite is
`make benchmark-media-offline-audio` and per-stage
`benchmark-media-{vad,stt,tts,diarization}`; see
[Offline Audio Model Evaluations](fluid-audio-offline-evals.md). Fixtures are
synthetic TTS and do not justify a Dictation default change. Natural
Indian-accented speech, English/Hindi code-switching, noisy rooms, overlapping
speakers, and physical echo remain release gates in the manifest.

Configured model-idle is 300 s; process idle is 900 s. Package.swift pins
FluidAudio `0.12.4`. Magician mirrors that SDK's split Kokoro cache (voice,
vocabulary, G2P, lexicon) into the configured model cache after online
preparation, and restores portable assets before initialization and every
synthesis in online and offline modes.

## Runtime Operations

Settings > Voice and audio > Advanced shows the canonical engine/model state:
health, ownership, PID, starts/restarts, resident-model and active-session
counts, idle policy, model lifecycle, and the last engine error. Its load and
unload buttons use the same backend controls as the operator commands:

```bash
make audio-engine-status
make audio-model-prewarm AUDIO_MODEL=fluid-qwen3-asr-f32
make audio-model-unload AUDIO_MODEL=fluid-qwen3-asr-f32
make audio-model-unload
```

The API is
`POST /api/magician/v2/media/audio-engines/{engine_id}/models/{prewarm|unload}`
with `{"model_ids":["..."]}`. Empty prewarm uses the configured prewarm list;
empty unload selects every configured model. Requests validate the entire batch
before changing residency, and unload returns `409` when any selected model has
an active session. Status and unload on an idle engine never start the sidecar.

The shipped desktop configuration uses lazy process ownership plus background
startup prewarm for Silero VAD, Parakeet EOU, and Kokoro TTS. Magician startup is
not blocked by model preparation; downloaded assets remain cached, and resident
models still follow the configured five-minute idle unload policy. Prewarm IDs
are validated against every FluidAudio-backed provider catalog (VAD, streaming
STT, diarization, recording STT, and TTS), not only the recording catalogs.
Engine/model loading events also use the shared typed runtime event identifiers,
so the taxonomy and the sidecar emit sites cannot drift independently.

`PUT /api/magician/v2/media/audio-settings` is the live FluidAudio engine
switch. Disable persists `media.engines.fluid_audio.enabled: false`, marks
every FluidAudio stage option and model unavailable, invalidates in-flight
operation leases, closes Magician-owned VAD/STT/diarization streams, clears
FluidAudio TTS cache entries, and stops a sidecar owned by Magician.
Externally managed sidecar processes are never killed; after its streams close,
Magician performs a bounded unload sweep for configured models with no remaining
active sessions. Re-enabling first verifies current host support, restores the
already-registered provider chains without a Magician restart, and starts
configured prewarm work only after the settings response has been assembled.
The manager serializes startup separately from process state, so disable and
status operations do not wait behind sidecar health polling.

Supervised local launches put Magician and its owned FluidAudio sidecar in one
isolated Unix process group. Normal stop/restart and detected native Magician
crashes therefore terminate the sidecar as part of the same lifecycle; a
surviving `3029` listener cannot poison the next launch with `EADDRINUSE`.
`make stop-macos-audio-engine` provides a narrow compatibility sweep for
pre-existing orphans and refuses to terminate a listener whose command is not
`magician-macos-audio-engine.bin`.

Lifecycle events are metadata-only process telemetry on the realtime transport
and in audio-engine status; they are not materialized into a user-scoped
progress feed and include no raw audio or transcript:
`media.audio.engine.{started,stopped,unhealthy}` and
`media.audio.model.{loading,loaded,unloaded}`.

## Packaging And Platforms

- Local development stages `magician-macos-audio-engine.bin` through
  `make build-macos-audio-engine-debug`.
- macOS release builds use the dedicated Tauri overlay and package the sidecar
  in the app resources.
- Magician lazily starts the sidecar, authenticates it with an ephemeral token,
  and stops only the process it owns. Externally managed engines are never
  killed.
- Containers and non-Apple hosts do not package or advertise the macOS sidecar.
  Existing non-FluidAudio recording providers remain available, so an absent
  local engine is a capability omission rather than a startup failure.
- Offline mode is fail-closed: only already staged and compiled models can load.
  Signed-app bundle and staged-cache first-run verification is a release gate.

Run the provider-free and packaging checks with:

```bash
make test-macos-audio-engine
make verify-media-audio-rollout
make verify-media-audio-rollout MEDIA_AUDIO_ROLLOUT_ARGS=--require-staged-sidecar
```

Hardware inference is opt-in and may download models:

```bash
MAGICIAN_FLUID_AUDIO_HARDWARE_TEST=1 make test-macos-audio-engine
MAGICIAN_FLUID_AUDIO_STT_HARDWARE_TEST=1 \
  MAGICIAN_FLUID_AUDIO_STT_FIXTURE=/path/to/consented.wav \
  make test-macos-audio-engine
MAGICIAN_FLUID_AUDIO_TTS_HARDWARE_TEST=1 make test-macos-audio-engine
MAGICIAN_FLUID_AUDIO_OFFLINE_HARDWARE_TEST=1 \
  MAGICIAN_FLUID_AUDIO_TEST_MODEL_CACHE=/path/to/prepared/cache \
  MAGICIAN_FLUID_AUDIO_STT_FIXTURE=/path/to/consented.wav \
  make test-macos-audio-engine
```
