# Offline Audio Model Evaluations

Magician has a provider-direct evaluation suite for the four offline audio
processes: voice activity detection (VAD), speech-to-text (STT), text-to-speech
(TTS), and speaker diarization. The evaluator does not start Magician, the
supervisor, the web UI, or any online provider. It owns a loopback-only
FluidAudio sidecar and terminates that process after each provider.

The suite is intended for two jobs:

- catch protocol, metric, fixture, and model-output regressions; and
- compare configured local models before changing a surface default.

## Fast tests

The provider-free tests are part of `make test` and can be run alone:

```bash
make test-media-offline-audio-eval
```

They cover WER/CER, VAD segment overlap, waveform measurements, diarization
speaker-label permutation, provider selection, Pareto ranking, and result-schema
validation. They do not start a sidecar or load a CoreML model.
The test and benchmark Make targets create or reconcile the repository's pinned,
isolated test-suite Python environment before running, so schema validation and
sidecar WebSocket support do not depend on packages installed in system Python.

## Readiness

Generate or reuse the ignored local synthetic fixtures, then validate the
engine, cache, provider catalog, and required inputs without inference:

```bash
make benchmark-media-offline-audio \
  MEDIA_OFFLINE_AUDIO_EVAL_ARGS='--dry-run \
    --cache-dir /Volumes/build/magician/models/audio/fluidaudio'
```

The evaluator defaults to cached models only. `--allow-model-downloads` is an
explicit exception that enables the configured FluidAudio download policy; it
does not enable cloud inference. A readiness result is false when the engine,
cache, required fixture, annotation, or selected configured local provider is
missing.

## Running evaluations

Run the complete offline suite:

```bash
make benchmark-media-offline-audio \
  MEDIA_OFFLINE_AUDIO_EVAL_ARGS='--cache-dir /Volumes/build/magician/models/audio/fluidaudio'
```

Run one process at a time:

```bash
make benchmark-media-vad MEDIA_OFFLINE_AUDIO_EVAL_ARGS='--cache-dir /Volumes/build/magician/models/audio/fluidaudio'
make benchmark-media-stt MEDIA_OFFLINE_AUDIO_EVAL_ARGS='--cache-dir /Volumes/build/magician/models/audio/fluidaudio'
make benchmark-media-tts MEDIA_OFFLINE_AUDIO_EVAL_ARGS='--cache-dir /Volumes/build/magician/models/audio/fluidaudio'
make benchmark-media-diarization MEDIA_OFFLINE_AUDIO_EVAL_ARGS='--cache-dir /Volumes/build/magician/models/audio/fluidaudio'
```

The STT stage evaluates both configured recording and streaming local models.
Use `--stt-modes recording` or `--stt-modes streaming` to narrow it. Restrict a
stage to one or more configured IDs with repeatable arguments such as
`--provider vad=fluid-silero-v6` or
`--provider stt=fluid-qwen3-asr-f32,fluid-parakeet-eou-en`.

Warm inference is the default. For a model-selection run on an otherwise idle
machine, capture both load and steady-state behavior:

```bash
make benchmark-media-offline-audio MEDIA_OFFLINE_AUDIO_EVAL_ARGS='\
  --cache-dir /Volumes/build/magician/models/audio/fluidaudio \
  --run-kinds cold,warm \
  --repetitions 5 \
  --fail-on-quality'
```

`--realtime-streaming` paces streaming fixtures at their real duration for
end-user event latency. The default sends PCM as fast as accepted and is better
for compute throughput and real-time-factor comparison. Do not compare timing,
CPU, or RSS from runs performed under materially different machine load. Record
unavoidable contention with `--contention-note`.

Results are schema-validated and written under the ignored
`data/magician_v2/media_evals/results/` directory. Every provider starts in a
fresh owned sidecar, preventing retained allocations from a previous model from
polluting its RSS measurement.

This does partially measure CoreML performance: end-to-end cold/warm latency,
real-time factor, sampled process CPU, and RSS include model preparation and
inference through FluidAudio. It does not attribute work to CPU, GPU, and ANE or
replace Instruments energy/thermal profiling. Default changes therefore still
need controlled on-device benchmark runs on the target hardware.

## Metrics

VAD reports speech precision/recall/F1, false-positive time, missed-speech time,
and boundary mean absolute error. Silence and deterministic room-noise controls
are scored alongside padded synthetic speech.

STT reports strict WER/CER, wall time, real-time factor, CPU, RSS, and streaming
partial/final latency. Numeral formatting and transliteration remain visible as
errors instead of being normalized away.

TTS reports duration, peak/RMS amplitude, clipping, leading/trailing silence,
synthesis real-time factor, CPU, and RSS. Intelligibility uses a named offline
recording-STT provider for round-trip WER/CER. That metric is coupled to the
judge model and must not be interpreted as a standalone listening score.

Diarization reports permutation-invariant diarization error rate, expected and
predicted speaker counts, speaker-count error, and resource/performance data.
The repeatable synthetic two-speaker fixture is a plumbing baseline; an optional
consented overlap fixture remains a release-quality gate.

The report marks models on the quality/performance Pareto frontier. This is a
comparison aid, not an automatic routing decision: a model must first satisfy
the stage quality gates, then be compared on the relevant surface workload.
Synthetic speech cannot approve a default change by itself. Consented human
speech, accents, code switching, noise, overlap, device capture, and physical
echo/barge-in remain required where applicable.

## Contracts

- Suite definition: `data/magician_v2/media_evals/offline-suite.manifest.json`
- Shared fixture catalog: `data/magician_v2/media_evals/fixtures.manifest.json`
- Result schema: `data/magician_v2/media_evals/offline-results.schema.json`
- Evaluator: `scripts/media_offline_audio_eval.py`
- Provider-free tests: `scripts/test_media_offline_audio_eval.py`

Future online-provider evaluators should emit the same measurement contract but
must use a separate explicitly consented transport. They must never make
`--allow-model-downloads` double as permission to upload fixture audio.
