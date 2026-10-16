# Magician macOS Audio Engine

`magician-macos-audio-engine` is Magician's supervised macOS host process for
FluidAudio-backed audio stages. It implements Silero VAD on macOS 14+ and Qwen3
ASR 0.6B recording STT on macOS 15+. Magician owns provider selection and
process lifecycle; the sidecar owns CoreML model residency, audio normalization,
and inference.

## Runtime Contract

- The backend passes a validated JSON configuration through
  `MAGICIAN_AUDIO_ENGINE_CONFIG_JSON`.
- An ephemeral 64-character token in `MAGICIAN_AUDIO_ENGINE_TOKEN` protects all
  loopback HTTP and WebSocket routes.
- `MAGICIAN_AUDIO_ENGINE_ENDPOINT` must be an explicit loopback HTTP origin.
- Every request carries protocol version `1`; incompatible clients fail closed.
- The accepted adapters are `fluid_audio_vad` with
  `FluidInference/silero-vad-coreml`, `fluid_audio_recording_stt` with
  `FluidInference/qwen3-asr-0.6b-coreml`, `fluid_audio_streaming_eou_stt` with
  `FluidInference/parakeet-realtime-eou-120m-coreml`,
  `fluid_audio_streaming_sortformer` with
  `FluidInference/diar-streaming-sortformer-coreml`, and
  `fluid_audio_kokoro_tts` with `FluidInference/kokoro-82m-coreml`; repositories
  and variants are allowlisted by the sidecar config loader.

The sidecar exposes authenticated health, capabilities, model inventory/load/
unload, `/v1/audio/stream`, `/v1/audio/transcriptions`, and `/v1/audio/speech`. The VAD stream accepts PCM16 little-endian or
float32 little-endian input at 8-192 kHz with 1-8 channels and normalizes it to
16 kHz mono before VAD. Recording STT accepts bounded audio file bodies,
normalizes them to 16 kHz mono, and returns transcript, model, language, and
timing metadata; Qwen does not currently expose confidence or word timestamps.
The sidecar does not capture an audio device.

## Lifecycle And Models

Magician starts the sidecar lazily when the configured VAD provider is first
used. It verifies health and protocol compatibility, retries startup within the
configured bound, reaps an idle child, and stops only the process it owns. An
externally managed sidecar requires an auth-token environment variable and is
never killed by Magician.

Model cache location, download/offline policy, prewarm IDs, process/model idle
timeouts, resident-model/session limits, request/frame limits, and retries are
owned by the `media.engines.fluid_audio` block in `magician-config.yaml`.
Offline or disabled-download mode loads only an already compiled CoreML model;
an absent cache fails without a network attempt. Kokoro's SDK-owned voice,
vocabulary, G2P vocabulary/models, and lexicon assets are mirrored into the
configured model cache after online preparation. Portable assets are restored before model
initialization and every synthesis in online and offline modes, so SDK-cache
eviction cannot break out-of-vocabulary words. The shipped config permits four resident models so a
diarized local stream can synthesize while VAD, STT, and speaker models remain
active.

Magician exposes sidecar state and lifecycle actions through its canonical
media API; operators and the Settings UI do not call this private process
directly. Use `make audio-engine-status`,
`make audio-model-prewarm AUDIO_MODEL=<configured-provider-id>`, and
`make audio-model-unload [AUDIO_MODEL=<configured-provider-id>]`. Status and
unload do not start an idle sidecar, and unload rejects active models before a
multi-model batch changes residency.

## Build And Test

```bash
make build-macos-audio-engine-debug
make build-macos-audio-engine-release
make test-macos-audio-engine
```

The normal test target is provider-free and covers protocol, PCM/audio
conversion, VAD event filtering, authentication, model lifecycle, offline cache
behavior, and HTTP/WebSocket integration. Run real CoreML inference explicitly
on supported hardware:

```bash
MAGICIAN_FLUID_AUDIO_HARDWARE_TEST=1 make test-macos-audio-engine
MAGICIAN_FLUID_AUDIO_STT_HARDWARE_TEST=1 \
MAGICIAN_FLUID_AUDIO_STT_FIXTURE=/path/to/consented.wav \
make test-macos-audio-engine
MAGICIAN_FLUID_AUDIO_OFFLINE_HARDWARE_TEST=1 \
MAGICIAN_FLUID_AUDIO_TEST_MODEL_CACHE=/path/to/prepared/cache \
MAGICIAN_FLUID_AUDIO_STT_FIXTURE=/path/to/consented.wav \
make test-macos-audio-engine
```

Desktop development and release targets stage the release executable as
`magician-macos-audio-engine.bin` and include it only in macOS Tauri resources.
The benchmark evidence, rollout defaults, and package/offline gates are recorded
in `docs/components/magician/fluid-audio-phase9-rollout.md`.
