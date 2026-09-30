#!/usr/bin/env python3
"""Evaluate configured offline audio models without starting Magician.

The evaluator owns one loopback-only FluidAudio sidecar and terminates it on
exit. It scores VAD, recording/streaming STT, TTS, and diarization using the
same versioned sidecar contract used by the application. Online adapters are
intentionally rejected; they can be added later behind a separate consented
transport.
"""

from __future__ import annotations

import argparse
import array
import datetime as dt
import importlib.util
import itertools
import json
import math
import os
import re
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid
import wave
from collections import defaultdict
from pathlib import Path
from typing import Any, Iterable

try:
    import websocket
except ImportError:  # pragma: no cover - reported as an actionable CLI error
    websocket = None


REPO_ROOT = Path(__file__).resolve().parent.parent
DATA_DIR = REPO_ROOT / "data/magician_v2/media_evals"
DEFAULT_SUITE = DATA_DIR / "offline-suite.manifest.json"
DEFAULT_RESULTS = DATA_DIR / "results"
DEFAULT_RUNTIME_CONFIG = Path.home() / "MagicianNotes/magician-config.yaml"
DEFAULT_CONFIG = DEFAULT_RUNTIME_CONFIG if DEFAULT_RUNTIME_CONFIG.is_file() else REPO_ROOT / "magician-config.yaml"
DEFAULT_ENGINE = REPO_ROOT / "magician-macos-audio-engine.bin"
PROTOCOL_VERSION = 1
STAGES = ("vad", "stt", "tts", "diarization")
MODES = ("recording", "streaming", "synthesis")
FLUID_ADAPTERS = {
    "vad": {"fluid_audio_vad"},
    "stt_recording": {"fluid_audio_recording_stt"},
    "stt_streaming": {"fluid_audio_streaming_eou_stt"},
    "tts": {"fluid_audio_kokoro_tts"},
    "diarization": {"fluid_audio_streaming_sortformer"},
}


def _load_recording_benchmark_module() -> Any:
    """Reuse the established sidecar lifecycle and resource sampler."""
    path = REPO_ROOT / "scripts/benchmark-media-recording-stt.py"
    spec = importlib.util.spec_from_file_location("magician_recording_stt_benchmark", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load shared benchmark utilities from {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


baseline = _load_recording_benchmark_module()


def csv_values(value: str | None) -> list[str]:
    return [part.strip() for part in (value or "").split(",") if part.strip()]


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Directly evaluate configured offline VAD, STT, TTS, and diarization models."
    )
    parser.add_argument("--config", type=Path, default=DEFAULT_CONFIG)
    parser.add_argument("--suite", type=Path, default=DEFAULT_SUITE)
    parser.add_argument("--engine", type=Path, default=DEFAULT_ENGINE)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--cache-dir", type=Path, help="override the configured FluidAudio cache")
    parser.add_argument("--stages", default=",".join(STAGES))
    parser.add_argument("--stt-modes", default="recording,streaming")
    parser.add_argument("--fixtures", help="optional comma-separated fixture IDs")
    parser.add_argument(
        "--provider",
        action="append",
        default=[],
        metavar="STAGE=ID[,ID]",
        help="restrict a stage to configured provider IDs; may be repeated",
    )
    parser.add_argument("--run-kinds", default="warm", help="comma-separated warm and/or cold")
    parser.add_argument("--repetitions", type=int, default=1)
    parser.add_argument("--timeout-secs", type=float, default=600)
    parser.add_argument("--chunk-ms", type=int, help="override the suite stream chunk size")
    parser.add_argument(
        "--realtime-streaming",
        action="store_true",
        help="pace PCM at wall-clock speed; default sends as fast as the engine accepts it",
    )
    parser.add_argument(
        "--allow-model-downloads",
        action="store_true",
        help="allow the sidecar's configured download policy; default requires cached models",
    )
    parser.add_argument(
        "--generate-local-fixtures",
        action="store_true",
        help="run the local synthetic speech generator before evaluation",
    )
    parser.add_argument("--include-events", action="store_true")
    parser.add_argument("--fail-on-quality", action="store_true")
    parser.add_argument("--contention-note", default="")
    parser.add_argument("--dry-run", action="store_true", help="validate readiness without starting the sidecar")
    parser.add_argument("--self-test", action="store_true", help="run provider-free evaluator regressions")
    args = parser.parse_args(argv)

    stages = csv_values(args.stages)
    if not stages or any(stage not in STAGES for stage in stages):
        parser.error(f"--stages must contain only {', '.join(STAGES)}")
    args.stages = list(dict.fromkeys(stages))
    modes = csv_values(args.stt_modes)
    if not modes or any(mode not in {"recording", "streaming"} for mode in modes):
        parser.error("--stt-modes must contain recording and/or streaming")
    args.stt_modes = list(dict.fromkeys(modes))
    run_kinds = csv_values(args.run_kinds)
    if not run_kinds or any(kind not in {"cold", "warm"} for kind in run_kinds):
        parser.error("--run-kinds must contain cold and/or warm")
    args.run_kinds = list(dict.fromkeys(run_kinds))
    if args.repetitions < 1:
        parser.error("--repetitions must be at least 1")
    if args.timeout_secs <= 0:
        parser.error("--timeout-secs must be positive")
    if args.chunk_ms is not None and not 20 <= args.chunk_ms <= 2000:
        parser.error("--chunk-ms must be between 20 and 2000")
    args.fixture_ids = set(csv_values(args.fixtures))
    args.provider_filters = parse_provider_filters(args.provider, parser)
    return args


def parse_provider_filters(values: list[str], parser: argparse.ArgumentParser | None = None) -> dict[str, set[str]]:
    filters: dict[str, set[str]] = defaultdict(set)
    valid = {"vad", "stt", "stt_recording", "stt_streaming", "tts", "diarization"}
    for value in values:
        stage, separator, provider_csv = value.partition("=")
        stage = stage.strip()
        providers = csv_values(provider_csv)
        if not separator or stage not in valid or not providers:
            message = f"invalid --provider {value!r}; expected STAGE=ID[,ID]"
            if parser is not None:
                parser.error(message)
            raise ValueError(message)
        filters[stage].update(providers)
    return dict(filters)


def read_json(path: Path) -> dict[str, Any]:
    with path.open(encoding="utf-8") as handle:
        value = json.load(handle)
    if not isinstance(value, dict):
        raise ValueError(f"expected an object in {path}")
    return value


def nested(value: dict[str, Any], path: str) -> Any:
    current: Any = value
    for component in path.split("."):
        if not isinstance(current, dict) or component not in current:
            raise KeyError(f"configuration path {path} is absent")
        current = current[component]
    return current


def provider_catalog(config: dict[str, Any], path: str, adapters: set[str]) -> list[dict[str, Any]]:
    providers = nested(config, path)
    if not isinstance(providers, list):
        raise ValueError(f"configuration path {path} must be a provider list")
    selected = [dict(row) for row in providers if isinstance(row, dict) and row.get("adapter") in adapters]
    for provider in selected:
        for required in ("id", "adapter", "model"):
            if not str(provider.get(required, "")).strip():
                raise ValueError(f"offline provider in {path} is missing {required}")
    return selected


def discover_providers(config: dict[str, Any], suite: dict[str, Any]) -> dict[str, list[dict[str, Any]]]:
    stages = suite["stages"]
    return {
        "vad": provider_catalog(config, stages["vad"]["provider_catalog"], FLUID_ADAPTERS["vad"]),
        "stt_recording": provider_catalog(
            config, stages["stt"]["recording_provider_catalog"], FLUID_ADAPTERS["stt_recording"]
        ),
        "stt_streaming": provider_catalog(
            config, stages["stt"]["streaming_provider_catalog"], FLUID_ADAPTERS["stt_streaming"]
        ),
        "tts": provider_catalog(config, stages["tts"]["provider_catalog"], FLUID_ADAPTERS["tts"]),
        "diarization": provider_catalog(
            config, stages["diarization"]["provider_catalog"], FLUID_ADAPTERS["diarization"]
        ),
    }


def filter_providers(
    providers: dict[str, list[dict[str, Any]]], filters: dict[str, set[str]]
) -> dict[str, list[dict[str, Any]]]:
    generic_stt = filters.get("stt", set())
    known_stt = {
        str(row["id"])
        for key in ("stt_recording", "stt_streaming")
        for row in providers.get(key, [])
    }
    missing_stt = generic_stt - known_stt
    if missing_stt:
        raise ValueError(
            f"unknown configured offline STT provider(s): {', '.join(sorted(missing_stt))}"
        )
    result: dict[str, list[dict[str, Any]]] = {}
    for key, rows in providers.items():
        requested = set(filters.get(key, set()))
        if key.startswith("stt_") and generic_stt:
            requested.update(generic_stt & {str(row["id"]) for row in rows})
        result[key] = [row for row in rows if not requested or str(row["id"]) in requested]
        known = {str(row["id"]) for row in rows}
        missing = set(filters.get(key, set())) - known
        if missing:
            raise ValueError(f"unknown configured offline provider(s) for {key}: {', '.join(sorted(missing))}")
        if key.startswith("stt_") and generic_stt and not (generic_stt & known) and not filters.get(key):
            result[key] = []
    return result


def fixture_index(manifest: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {
        str(row["id"]): row
        for row in manifest.get("fixtures", [])
        if isinstance(row, dict) and row.get("id")
    }


def fixture_path(manifest_path: Path, fixture: dict[str, Any]) -> Path:
    return (manifest_path.parent / str(fixture["path"])).resolve()


def read_pcm16_wav(path: Path) -> tuple[bytes, int, int, int]:
    with wave.open(str(path), "rb") as source:
        channels = source.getnchannels()
        sample_rate = source.getframerate()
        sample_width = source.getsampwidth()
        frames = source.getnframes()
        pcm = source.readframes(frames)
    if sample_width != 2:
        raise ValueError(f"{path} must use 16-bit PCM, got {sample_width * 8}-bit")
    duration_ms = round(frames * 1000 / sample_rate)
    return pcm, sample_rate, channels, duration_ms


def canonical_stream_pcm(path: Path) -> tuple[bytes, int]:
    pcm, sample_rate, channels, duration_ms = read_pcm16_wav(path)
    if sample_rate != 16000 or channels != 1:
        raise ValueError(f"stream fixture {path} must be 16 kHz mono PCM")
    return pcm, duration_ms


def padded_pcm(path: Path, leading_ms: int, trailing_ms: int) -> tuple[bytes, int, list[tuple[int, int]]]:
    pcm, duration_ms = canonical_stream_pcm(path)
    bytes_per_ms = 16000 * 2 / 1000
    leading = bytes(round(leading_ms * bytes_per_ms))
    trailing = bytes(round(trailing_ms * bytes_per_ms))
    total = leading_ms + duration_ms + trailing_ms
    return leading + pcm + trailing, total, [(leading_ms, leading_ms + duration_ms)]


def normalize_text(value: str) -> str:
    return baseline.normalize_text(value)


def word_error_rate(reference: str, hypothesis: str) -> float | None:
    return baseline.error_rate(reference, hypothesis, characters=False)


def character_error_rate(reference: str, hypothesis: str) -> float | None:
    return baseline.error_rate(reference, hypothesis, characters=True)


def critical_entity_recall(reference: str, hypothesis: str) -> float | None:
    """Score invented names and identifiers in fixtures marked for entity QA."""
    entities = {
        token.casefold()
        for token in re.findall(
            r"(?:[$₹€£]\s?\d[\d,]*(?:\.\d+)?)|(?:\b\w*[A-Za-z]\w*\d\w*\b)|(?:\b\d[\d:/.-]*\b)|(?:\b[A-Z][a-z]{2,}\b)",
            reference,
        )
    }
    if not entities:
        return None
    normalized_hypothesis = hypothesis.casefold()
    return sum(1 for entity in entities if entity in normalized_hypothesis) / len(entities)


def intervals_duration(intervals: Iterable[tuple[int, int]]) -> int:
    return sum(max(0, end - start) for start, end in intervals)


def merge_intervals(intervals: Iterable[tuple[int, int]]) -> list[tuple[int, int]]:
    ordered = sorted((max(0, int(start)), max(0, int(end))) for start, end in intervals if end > start)
    merged: list[tuple[int, int]] = []
    for start, end in ordered:
        if merged and start <= merged[-1][1]:
            merged[-1] = (merged[-1][0], max(merged[-1][1], end))
        else:
            merged.append((start, end))
    return merged


def overlap_duration(left: list[tuple[int, int]], right: list[tuple[int, int]]) -> int:
    total = 0
    i = j = 0
    while i < len(left) and j < len(right):
        start = max(left[i][0], right[j][0])
        end = min(left[i][1], right[j][1])
        total += max(0, end - start)
        if left[i][1] <= right[j][1]:
            i += 1
        else:
            j += 1
    return total


def speech_segment_metrics(
    expected: list[tuple[int, int]], predicted: list[tuple[int, int]], duration_ms: int
) -> dict[str, float | None]:
    expected = merge_intervals(expected)
    predicted = merge_intervals(predicted)
    true_positive_ms = overlap_duration(expected, predicted)
    expected_ms = intervals_duration(expected)
    predicted_ms = intervals_duration(predicted)
    false_positive_ms = max(0, predicted_ms - true_positive_ms)
    false_negative_ms = max(0, expected_ms - true_positive_ms)
    precision = true_positive_ms / predicted_ms if predicted_ms else (1.0 if not expected_ms else 0.0)
    recall = true_positive_ms / expected_ms if expected_ms else (1.0 if not predicted_ms else 0.0)
    f1 = 2 * precision * recall / (precision + recall) if precision + recall else 0.0
    boundary_errors: list[float] = []
    if expected and predicted:
        for expected_start, expected_end in expected:
            closest = min(predicted, key=lambda row: abs(row[0] - expected_start) + abs(row[1] - expected_end))
            boundary_errors.extend((abs(closest[0] - expected_start), abs(closest[1] - expected_end)))
    return {
        "speech_precision": precision,
        "speech_recall": recall,
        "speech_f1": f1,
        "false_positive_rate": false_positive_ms / max(1, duration_ms - expected_ms),
        "miss_rate": false_negative_ms / expected_ms if expected_ms else 0.0,
        "false_positive_ms": float(false_positive_ms),
        "missed_speech_ms": float(false_negative_ms),
        "boundary_mean_absolute_error_ms": statistics.fmean(boundary_errors) if boundary_errors else None,
    }


def vad_regions(events: list[dict[str, Any]], duration_ms: int) -> list[tuple[int, int]]:
    regions: list[tuple[int, int]] = []
    active_start: int | None = None
    for event in events:
        event_type = event.get("type")
        if event_type == "speech_started":
            if active_start is None:
                active_start = int(event.get("at_ms", 0))
        elif event_type == "speech_ended" and active_start is not None:
            regions.append((active_start, int(event.get("at_ms", duration_ms))))
            active_start = None
    if active_start is not None:
        regions.append((active_start, duration_ms))
    return merge_intervals(regions)


def wav_signal_metrics(audio: bytes) -> dict[str, float]:
    import io

    with wave.open(io.BytesIO(audio), "rb") as source:
        channels = source.getnchannels()
        sample_rate = source.getframerate()
        sample_width = source.getsampwidth()
        frames = source.getnframes()
        pcm = source.readframes(frames)
    if channels != 1 or sample_width != 2 or sample_rate <= 0:
        raise ValueError("TTS output must be mono 16-bit PCM WAV")
    samples = array.array("h")
    samples.frombytes(pcm)
    if sys.byteorder != "little":
        samples.byteswap()
    duration_ms = len(samples) * 1000 / sample_rate
    if not samples:
        return {
            "audio_duration_ms": 0.0,
            "sample_rate_hz": float(sample_rate),
            "peak_amplitude": 0.0,
            "rms_amplitude": 0.0,
            "clipping_ratio": 0.0,
            "leading_silence_ms": 0.0,
            "trailing_silence_ms": 0.0,
        }
    peak = max(abs(value) for value in samples)
    rms = math.sqrt(statistics.fmean(float(value) * value for value in samples))
    clipping_ratio = sum(1 for value in samples if abs(value) >= 32700) / len(samples)
    active_threshold = max(180, peak * 0.015)
    active = [index for index, value in enumerate(samples) if abs(value) >= active_threshold]
    leading = (active[0] * 1000 / sample_rate) if active else duration_ms
    trailing = ((len(samples) - 1 - active[-1]) * 1000 / sample_rate) if active else duration_ms
    return {
        "audio_duration_ms": duration_ms,
        "sample_rate_hz": float(sample_rate),
        "peak_amplitude": peak / 32768,
        "rms_amplitude": rms / 32768,
        "clipping_ratio": clipping_ratio,
        "leading_silence_ms": leading,
        "trailing_silence_ms": trailing,
    }


def labels_at(segments: list[dict[str, Any]], at_ms: int) -> set[str]:
    return {
        str(segment["speaker_id"])
        for segment in segments
        if int(segment["start_ms"]) <= at_ms < int(segment["end_ms"])
    }


def diarization_error_rate(
    expected: list[dict[str, Any]], predicted: list[dict[str, Any]], duration_ms: int, frame_ms: int = 20
) -> tuple[float | None, dict[str, str]]:
    reference_labels = sorted({str(row["speaker_id"]) for row in expected})
    predicted_labels = sorted({str(row["speaker_id"]) for row in predicted})
    if not reference_labels:
        return None, {}
    targets = reference_labels + [f"__extra_{index}" for index in range(max(0, len(predicted_labels) - len(reference_labels)))]
    if len(targets) < len(predicted_labels):
        targets.extend(f"__extra_{index + len(targets)}" for index in range(len(predicted_labels) - len(targets)))
    mappings = itertools.permutations(targets, len(predicted_labels)) if predicted_labels else [tuple()]
    best_error = math.inf
    best_mapping: dict[str, str] = {}
    for assignment in mappings:
        mapping = dict(zip(predicted_labels, assignment))
        error = 0
        denominator = 0
        for at_ms in range(0, max(duration_ms, frame_ms), frame_ms):
            reference = labels_at(expected, at_ms)
            hypothesis = {mapping[label] for label in labels_at(predicted, at_ms)}
            correct = len(reference & hypothesis)
            confusion = max(0, min(len(reference), len(hypothesis)) - correct)
            misses = max(0, len(reference) - len(hypothesis))
            false_alarms = max(0, len(hypothesis) - len(reference))
            error += confusion + misses + false_alarms
            denominator += len(reference)
        rate = error / denominator if denominator else math.inf
        if rate < best_error:
            best_error = rate
            best_mapping = mapping
    return (best_error if math.isfinite(best_error) else None), best_mapping


def predicted_diarization_segments(events: list[dict[str, Any]]) -> list[dict[str, Any]]:
    latest: dict[tuple[str, int], dict[str, Any]] = {}
    for event in events:
        if event.get("type") != "segment_revised":
            continue
        speaker_id = str(event.get("speaker_id", ""))
        start_ms = int(event.get("start_ms", 0))
        end_ms = int(event.get("end_ms", start_ms))
        if speaker_id and end_ms > start_ms:
            latest[(speaker_id, start_ms)] = {
                "speaker_id": speaker_id,
                "start_ms": start_ms,
                "end_ms": end_ms,
            }
    merged: list[dict[str, Any]] = []
    for speaker_id in sorted({row["speaker_id"] for row in latest.values()}):
        intervals = merge_intervals(
            (int(row["start_ms"]), int(row["end_ms"]))
            for row in latest.values()
            if row["speaker_id"] == speaker_id
        )
        merged.extend(
            {"speaker_id": speaker_id, "start_ms": start, "end_ms": end}
            for start, end in intervals
        )
    return merged


def start_envelope(stage: str, model_id: str, config: dict[str, Any]) -> dict[str, Any]:
    return {
        "type": "start",
        "protocol_version": PROTOCOL_VERSION,
        "stage": stage,
        "model_id": model_id,
        "format": {"sample_rate_hz": 16000, "channels": 1, "sample_format": "pcm_s16_le"},
        "config": config,
    }


def stream_audio(
    sidecar: Any,
    *,
    stage: str,
    model_id: str,
    config: dict[str, Any],
    pcm: bytes,
    chunk_ms: int,
    timeout: float,
    realtime: bool,
) -> tuple[list[dict[str, Any]], float]:
    if websocket is None:
        raise RuntimeError("websocket-client is required: install the repository Python test dependencies")
    sidecar.start()
    endpoint = str(sidecar.endpoint).replace("http://", "ws://", 1).replace("https://", "wss://", 1)
    headers = [
        f"Authorization: Bearer {sidecar.token}",
        f"x-magician-audio-protocol: {PROTOCOL_VERSION}",
    ]
    connection = websocket.create_connection(
        f"{endpoint}/v1/audio/stream",
        header=headers,
        timeout=timeout,
        http_proxy_host=None,
        http_proxy_port=None,
    )
    events: list[dict[str, Any]] = []
    started = time.monotonic()
    try:
        connection.send(json.dumps(start_envelope(stage, model_id, config)))
        ready = json.loads(connection.recv())
        if ready.get("type") != "ready":
            raise RuntimeError(f"stream did not become ready: {ready}")
        ready["arrival_ms"] = (time.monotonic() - started) * 1000
        events.append(ready)
        bytes_per_chunk = max(2, round(16000 * 2 * chunk_ms / 1000))
        bytes_per_chunk -= bytes_per_chunk % 2
        for offset in range(0, len(pcm), bytes_per_chunk):
            sent_at = time.monotonic()
            connection.send_binary(pcm[offset : offset + bytes_per_chunk])
            connection.settimeout(0.001)
            while True:
                try:
                    event = json.loads(connection.recv())
                    event["arrival_ms"] = (time.monotonic() - started) * 1000
                    events.append(event)
                    if event.get("type") == "error":
                        raise RuntimeError(f"stream failed: {event}")
                except websocket.WebSocketTimeoutException:
                    break
            if realtime:
                time.sleep(max(0, chunk_ms / 1000 - (time.monotonic() - sent_at)))
        connection.settimeout(timeout)
        connection.send(json.dumps({"type": "stop"}))
        while True:
            event = json.loads(connection.recv())
            event["arrival_ms"] = (time.monotonic() - started) * 1000
            events.append(event)
            if event.get("type") == "error":
                raise RuntimeError(f"stream failed: {event}")
            if event.get("type") == "finished":
                break
    finally:
        connection.close()
    return events, (time.monotonic() - started) * 1000


def unload_if_resident(sidecar: Any, model_id: str) -> None:
    sidecar.start()
    status, models = sidecar.request("GET", "/models")
    if status >= 400 or not isinstance(models, list):
        return
    row = next((item for item in models if item.get("id") == model_id), None)
    if row and row.get("resident"):
        quoted = baseline.urllib.parse.quote(model_id, safe="")
        status, payload = sidecar.request("POST", f"/models/{quoted}/unload")
        if status >= 400:
            raise RuntimeError(f"failed to unload {model_id}: {payload}")


def prepare_run(sidecar: Any, model_id: str, run_kind: str) -> None:
    if run_kind == "warm":
        sidecar.prepare(model_id, leave_loaded=True)
    else:
        unload_if_resident(sidecar, model_id)


def sampled_call(process_id: int | None, callback: Any) -> tuple[Any, dict[str, float]]:
    sampler = baseline.ProcessResourceSampler(process_id)
    sampler.start()
    try:
        result = callback()
    finally:
        resources = sampler.stop()
    return result, resources


def timing_metrics(wall_time_ms: float, audio_duration_ms: float) -> dict[str, float | None]:
    return {
        "wall_time_ms": wall_time_ms,
        "audio_duration_ms": audio_duration_ms,
        "real_time_factor": wall_time_ms / audio_duration_ms if audio_duration_ms > 0 else None,
    }


def base_measurement(
    stage: str, mode: str, provider: dict[str, Any], fixture_id: str, run_kind: str, run_index: int
) -> dict[str, Any]:
    return {
        "stage": stage,
        "mode": mode,
        "provider_id": str(provider["id"]),
        "adapter": str(provider["adapter"]),
        "model": str(provider["model"]),
        "fixture_id": fixture_id,
        "run_kind": run_kind,
        "run_index": run_index,
        "status": "failed",
        "quality_passed": None,
        "timings": {},
        "resources": {},
        "metrics": {},
    }


def skipped_measurement(
    stage: str,
    mode: str,
    provider: dict[str, Any],
    fixture_id: str,
    run_kind: str,
    run_index: int,
    reason: str,
) -> dict[str, Any]:
    row = base_measurement(stage, mode, provider, fixture_id, run_kind, run_index)
    row.update(status="skipped", skip_reason=reason)
    return row


def execute_measurement(row: dict[str, Any], callback: Any) -> dict[str, Any]:
    try:
        callback(row)
    except Exception as error:
        row["status"] = "failed"
        row["quality_passed"] = None
        row["error"] = str(error)
    return row


class OfflineEvaluator:
    def __init__(
        self,
        args: argparse.Namespace,
        config: dict[str, Any],
        suite: dict[str, Any],
        fixture_manifest_path: Path,
        fixtures: dict[str, dict[str, Any]],
        providers: dict[str, list[dict[str, Any]]],
    ) -> None:
        self.args = args
        self.config = config
        self.suite = suite
        self.fixture_manifest_path = fixture_manifest_path
        self.fixtures = fixtures
        self.providers = providers
        self.chunk_ms = args.chunk_ms or int(suite["canonical_stream_format"]["chunk_ms"])
        engine_config = dict(nested(config, "media.engines.fluid_audio"))
        engine_config["offline"] = not args.allow_model_downloads
        if not args.allow_model_downloads:
            engine_config["download_policy"] = "disabled"
        all_specs: dict[str, dict[str, Any]] = {}
        for rows in providers.values():
            for provider in rows:
                all_specs[str(provider["id"])] = provider
        roundtrip_id = str(suite["stages"]["tts"].get("roundtrip_stt_provider", ""))
        if "tts" in args.stages and roundtrip_id:
            recording_all = provider_catalog(
                config,
                suite["stages"]["stt"]["recording_provider_catalog"],
                FLUID_ADAPTERS["stt_recording"],
            )
            roundtrip = next((row for row in recording_all if str(row["id"]) == roundtrip_id), None)
            if roundtrip is None:
                raise ValueError(f"configured TTS round-trip STT provider {roundtrip_id!r} is absent")
            all_specs[roundtrip_id] = roundtrip
            self.roundtrip_provider = roundtrip
        else:
            self.roundtrip_provider = None
        self._engine_config = engine_config
        self._provider_specs = list(all_specs.values())
        self.sidecar = self._make_sidecar()

    def _make_sidecar(self) -> Any:
        return baseline.FluidAudioSidecar(
            binary=self.args.engine.expanduser().resolve(),
            engine_config=self._engine_config,
            provider_specs=self._provider_specs,
            config_path=self.args.config.expanduser().resolve(),
            cache_dir_override=self.args.cache_dir,
            model_idle_secs_override=None,
            timeout=self.args.timeout_secs,
        )

    def reset_sidecar(self) -> None:
        self.sidecar.close()
        self.sidecar = self._make_sidecar()

    def close(self) -> None:
        self.sidecar.close()

    def selected_cases(self, stage: str, key: str = "cases") -> list[dict[str, Any]]:
        cases = self.suite["stages"][stage].get(key, [])
        return [row for row in cases if not self.args.fixture_ids or str(row["fixture_id"]) in self.args.fixture_ids]

    def run(self) -> list[dict[str, Any]]:
        measurements: list[dict[str, Any]] = []
        if "vad" in self.args.stages:
            measurements.extend(self.run_vad())
        if "stt" in self.args.stages:
            if "recording" in self.args.stt_modes:
                measurements.extend(self.run_recording_stt())
            if "streaming" in self.args.stt_modes:
                measurements.extend(self.run_streaming_stt())
        if "tts" in self.args.stages:
            measurements.extend(self.run_tts())
        if "diarization" in self.args.stages:
            measurements.extend(self.run_diarization())
        return measurements

    def iterations(self) -> Iterable[tuple[str, int]]:
        for run_kind in self.args.run_kinds:
            for run_index in range(1, self.args.repetitions + 1):
                yield run_kind, run_index

    def run_vad(self) -> list[dict[str, Any]]:
        rows: list[dict[str, Any]] = []
        thresholds = self.suite["stages"]["vad"]["thresholds"]
        profile = nested(self.config, self.suite["stages"]["vad"]["session_config_profile"])
        stream_config = {
            "threshold": float(profile["threshold"]),
            "min_speech_ms": int(profile["min_speech_ms"]),
            "min_silence_ms": int(profile["min_silence_ms"]),
            "pre_roll_ms": int(profile["pre_roll_ms"]),
            "hangover_ms": int(profile["hangover_ms"]),
            "max_utterance_ms": int(profile["max_utterance_ms"]),
            "gate_only": True,
        }
        for provider in self.providers["vad"]:
            for case in self.selected_cases("vad"):
                fixture_id = str(case["fixture_id"])
                fixture = self.fixtures.get(fixture_id)
                for run_kind, run_index in self.iterations():
                    if fixture is None:
                        rows.append(skipped_measurement("vad", "streaming", provider, fixture_id, run_kind, run_index, "fixture is not catalogued"))
                        continue
                    path = fixture_path(self.fixture_manifest_path, fixture)
                    if not path.is_file():
                        rows.append(skipped_measurement("vad", "streaming", provider, fixture_id, run_kind, run_index, f"fixture is absent: {path}"))
                        continue
                    row = base_measurement("vad", "streaming", provider, fixture_id, run_kind, run_index)

                    def evaluate(target: dict[str, Any]) -> None:
                        leading = int(case.get("leading_silence_ms", 0))
                        trailing = int(case.get("trailing_silence_ms", 0))
                        if case.get("expected_region") == "padded_fixture_body":
                            pcm, duration_ms, expected = padded_pcm(path, leading, trailing)
                        else:
                            pcm, duration_ms = canonical_stream_pcm(path)
                            expected = [tuple(map(int, region)) for region in case.get("expected_speech_regions_ms", [])]
                        prepare_run(self.sidecar, str(provider["id"]), run_kind)
                        (events, wall_time_ms), resources = sampled_call(
                            self.sidecar.process_id(),
                            lambda: stream_audio(
                                self.sidecar,
                                stage="vad",
                                model_id=str(provider["id"]),
                                config=stream_config,
                                pcm=pcm,
                                chunk_ms=self.chunk_ms,
                                timeout=self.args.timeout_secs,
                                realtime=self.args.realtime_streaming,
                            ),
                        )
                        timings = timing_metrics(wall_time_ms, duration_ms)
                        metrics = speech_segment_metrics(expected, vad_regions(events, duration_ms), duration_ms)
                        boundary = metrics["boundary_mean_absolute_error_ms"]
                        quality = (
                            float(metrics["speech_f1"] or 0) >= float(thresholds["minimum_f1"])
                            and float(metrics["false_positive_rate"] or 0) <= float(thresholds["maximum_false_positive_rate"])
                            and float(metrics["miss_rate"] or 0) <= float(thresholds["maximum_miss_rate"])
                            and (boundary is None or float(boundary) <= float(thresholds["maximum_boundary_mean_absolute_error_ms"]))
                        )
                        target.update(status="passed" if quality else "failed", quality_passed=quality, timings=timings, resources=resources, metrics=metrics)
                        if self.args.include_events:
                            target["events"] = events

                    rows.append(execute_measurement(row, evaluate))
            self.reset_sidecar()
        return rows

    def run_recording_stt(self) -> list[dict[str, Any]]:
        rows: list[dict[str, Any]] = []
        thresholds = self.suite["stages"]["stt"]["thresholds"]
        for provider in self.providers["stt_recording"]:
            for case in self.selected_cases("stt", "recording_cases"):
                fixture_id = str(case["fixture_id"])
                fixture = self.fixtures.get(fixture_id)
                for run_kind, run_index in self.iterations():
                    if fixture is None:
                        rows.append(skipped_measurement("stt", "recording", provider, fixture_id, run_kind, run_index, "fixture is not catalogued"))
                        continue
                    path = fixture_path(self.fixture_manifest_path, fixture)
                    if not path.is_file():
                        rows.append(skipped_measurement("stt", "recording", provider, fixture_id, run_kind, run_index, f"fixture is absent: {path}"))
                        continue
                    reference, _ = baseline.load_reference(self.fixture_manifest_path.parent, fixture)
                    expected_speech = bool(fixture.get("expected", {}).get("speech", True))
                    row = base_measurement("stt", "recording", provider, fixture_id, run_kind, run_index)

                    def evaluate(target: dict[str, Any]) -> None:
                        _, _, _, duration_ms = read_pcm16_wav(path)
                        prepare_run(self.sidecar, str(provider["id"]), run_kind)

                        def transcribe() -> tuple[int, dict[str, Any], float]:
                            started = time.monotonic()
                            status, payload = self.sidecar.transcribe(provider, path, str(case.get("language", "en")))
                            return status, payload, (time.monotonic() - started) * 1000

                        (status, payload, wall_time_ms), resources = sampled_call(
                            self.sidecar.process_id(), transcribe
                        )
                        timings = timing_metrics(wall_time_ms, duration_ms)
                        if status >= 400:
                            raise RuntimeError(f"recording STT returned {status}: {payload}")
                        transcript = str(payload.get("text", "")).strip()
                        wer = word_error_rate(reference or "", transcript) if expected_speech and reference is not None else None
                        cer = character_error_rate(reference or "", transcript) if expected_speech and reference is not None else None
                        quality = (
                            (not expected_speech and (not thresholds.get("non_speech_must_be_empty") or not transcript))
                            or (
                                expected_speech
                                and bool(transcript)
                                and (wer is None or wer <= float(thresholds["maximum_word_error_rate"]))
                                and (cer is None or cer <= float(thresholds["maximum_character_error_rate"]))
                            )
                        )
                        metrics = {
                            "transcript": transcript,
                            "word_error_rate": wer,
                            "character_error_rate": cer,
                            "critical_entity_recall": critical_entity_recall(reference or "", transcript)
                            if fixture.get("expected", {}).get("score_named_entities")
                            else None,
                            "engine_processing_ms": payload.get("processing_duration_ms"),
                        }
                        target.update(status="passed" if quality else "failed", quality_passed=quality, timings=timings, resources=resources, metrics=metrics)

                    rows.append(execute_measurement(row, evaluate))
            self.reset_sidecar()
        return rows

    def run_streaming_stt(self) -> list[dict[str, Any]]:
        rows: list[dict[str, Any]] = []
        thresholds = self.suite["stages"]["stt"]["thresholds"]
        for provider in self.providers["stt_streaming"]:
            for case in self.selected_cases("stt", "streaming_cases"):
                fixture_id = str(case["fixture_id"])
                fixture = self.fixtures.get(fixture_id)
                for run_kind, run_index in self.iterations():
                    if fixture is None:
                        rows.append(skipped_measurement("stt", "streaming", provider, fixture_id, run_kind, run_index, "fixture is not catalogued"))
                        continue
                    path = fixture_path(self.fixture_manifest_path, fixture)
                    if not path.is_file():
                        rows.append(skipped_measurement("stt", "streaming", provider, fixture_id, run_kind, run_index, f"fixture is absent: {path}"))
                        continue
                    reference, _ = baseline.load_reference(self.fixture_manifest_path.parent, fixture)
                    row = base_measurement("stt", "streaming", provider, fixture_id, run_kind, run_index)

                    def evaluate(target: dict[str, Any]) -> None:
                        pcm, duration_ms = canonical_stream_pcm(path)
                        prepare_run(self.sidecar, str(provider["id"]), run_kind)
                        (events, wall_time_ms), resources = sampled_call(
                            self.sidecar.process_id(),
                            lambda: stream_audio(
                                self.sidecar,
                                stage="streaming_stt",
                                model_id=str(provider["id"]),
                                config={
                                    "language": case.get("language"),
                                    "eou_debounce_ms": int(
                                        self.suite["stages"]["stt"]["streaming_session_config"]["eou_debounce_ms"]
                                    ),
                                },
                                pcm=pcm,
                                chunk_ms=self.chunk_ms,
                                timeout=self.args.timeout_secs,
                                realtime=self.args.realtime_streaming,
                            ),
                        )
                        timings = timing_metrics(wall_time_ms, duration_ms)
                        finals = [str(event.get("text", "")).strip() for event in events if event.get("type") == "transcript_final"]
                        partials = [str(event.get("text", "")).strip() for event in events if event.get("type") == "transcript_partial"]
                        transcript = " ".join(text for text in finals if text).strip() or max(partials, key=len, default="")
                        wer = word_error_rate(reference or "", transcript) if reference is not None else None
                        cer = character_error_rate(reference or "", transcript) if reference is not None else None
                        quality = bool(transcript) and (wer is None or wer <= float(thresholds["maximum_word_error_rate"])) and (cer is None or cer <= float(thresholds["maximum_character_error_rate"]))
                        first_partial = next((event.get("arrival_ms") for event in events if event.get("type") == "transcript_partial"), None)
                        first_final = next((event.get("arrival_ms") for event in events if event.get("type") == "transcript_final"), None)
                        timings.update(first_partial_ms=first_partial, first_final_ms=first_final)
                        metrics = {
                            "transcript": transcript,
                            "word_error_rate": wer,
                            "character_error_rate": cer,
                            "critical_entity_recall": critical_entity_recall(reference or "", transcript)
                            if fixture.get("expected", {}).get("score_named_entities")
                            else None,
                        }
                        target.update(status="passed" if quality else "failed", quality_passed=quality, timings=timings, resources=resources, metrics=metrics)
                        if self.args.include_events:
                            target["events"] = events

                    rows.append(execute_measurement(row, evaluate))
            self.reset_sidecar()
        return rows

    def synthesize(self, provider: dict[str, Any], text: str) -> tuple[bytes, dict[str, str], float]:
        self.sidecar.start()
        payload = json.dumps(
            {
                "input": text,
                "voice": provider.get("voice"),
                "response_format": "wav",
                "speed": 1.0,
            }
        ).encode()
        request = urllib.request.Request(
            f"{self.sidecar.endpoint}/v1/audio/speech",
            data=payload,
            method="POST",
            headers={
                "Authorization": f"Bearer {self.sidecar.token}",
                "x-magician-audio-protocol": str(PROTOCOL_VERSION),
                "x-magician-audio-model": str(provider["id"]),
                "Content-Type": "application/json",
            },
        )
        started = time.monotonic()
        try:
            with urllib.request.urlopen(request, timeout=self.args.timeout_secs) as response:
                audio = response.read()
                headers = {key.lower(): value for key, value in response.headers.items()}
        except urllib.error.HTTPError as error:
            detail = error.read().decode("utf-8", errors="replace")
            raise RuntimeError(f"TTS returned {error.code}: {detail}") from error
        return audio, headers, (time.monotonic() - started) * 1000

    def run_tts(self) -> list[dict[str, Any]]:
        rows: list[dict[str, Any]] = []
        thresholds = self.suite["stages"]["tts"]["thresholds"]
        if self.roundtrip_provider is None:
            raise ValueError("TTS evaluation requires an offline round-trip STT provider")
        for provider in self.providers["tts"]:
            for case in self.selected_cases("tts"):
                fixture_id = str(case["fixture_id"])
                text = str(case["text"])
                for run_kind, run_index in self.iterations():
                    row = base_measurement("tts", "synthesis", provider, fixture_id, run_kind, run_index)

                    def evaluate(target: dict[str, Any]) -> None:
                        prepare_run(self.sidecar, str(provider["id"]), run_kind)
                        unload_if_resident(self.sidecar, str(self.roundtrip_provider["id"]))
                        (audio, headers, wall_time_ms), resources = sampled_call(
                            self.sidecar.process_id(), lambda: self.synthesize(provider, text)
                        )
                        signal = wav_signal_metrics(audio)
                        self.sidecar.prepare(str(self.roundtrip_provider["id"]), leave_loaded=True)
                        with tempfile.NamedTemporaryFile(suffix=".wav") as generated:
                            generated.write(audio)
                            generated.flush()
                            status, payload = self.sidecar.transcribe(
                                self.roundtrip_provider, Path(generated.name), str(case.get("language", "en"))
                            )
                        timings = timing_metrics(wall_time_ms, float(signal["audio_duration_ms"]))
                        if status >= 400:
                            raise RuntimeError(f"TTS round-trip STT returned {status}: {payload}")
                        transcript = str(payload.get("text", "")).strip()
                        wer = word_error_rate(text, transcript)
                        cer = character_error_rate(text, transcript)
                        quality = (
                            signal["audio_duration_ms"] >= float(thresholds["minimum_duration_ms"])
                            and signal["clipping_ratio"] <= float(thresholds["maximum_clipping_ratio"])
                            and (wer is None or wer <= float(thresholds["maximum_roundtrip_word_error_rate"]))
                            and signal["leading_silence_ms"] <= float(thresholds["maximum_leading_silence_ms"])
                            and signal["trailing_silence_ms"] <= float(thresholds["maximum_trailing_silence_ms"])
                        )
                        metrics: dict[str, Any] = dict(signal)
                        metrics.update(
                            roundtrip_transcript=transcript,
                            roundtrip_word_error_rate=wer,
                            roundtrip_character_error_rate=cer,
                            engine_processing_ms=float(headers.get("x-magician-audio-processing-ms", 0) or 0),
                            voice=headers.get("x-magician-audio-voice", str(provider.get("voice", ""))),
                        )
                        target.update(status="passed" if quality else "failed", quality_passed=quality, timings=timings, resources=resources, metrics=metrics)

                    rows.append(execute_measurement(row, evaluate))
            self.reset_sidecar()
        return rows

    def run_diarization(self) -> list[dict[str, Any]]:
        rows: list[dict[str, Any]] = []
        thresholds = self.suite["stages"]["diarization"]["thresholds"]
        for provider in self.providers["diarization"]:
            for case in self.selected_cases("diarization"):
                fixture_id = str(case["fixture_id"])
                fixture = self.fixtures.get(fixture_id)
                annotation_path = (self.fixture_manifest_path.parent / str(case["annotation_path"])).resolve()
                for run_kind, run_index in self.iterations():
                    if fixture is None:
                        rows.append(skipped_measurement("diarization", "streaming", provider, fixture_id, run_kind, run_index, "fixture is not catalogued"))
                        continue
                    path = fixture_path(self.fixture_manifest_path, fixture)
                    if not path.is_file() or not annotation_path.is_file():
                        missing = path if not path.is_file() else annotation_path
                        rows.append(skipped_measurement("diarization", "streaming", provider, fixture_id, run_kind, run_index, f"fixture input is absent: {missing}"))
                        continue
                    row = base_measurement("diarization", "streaming", provider, fixture_id, run_kind, run_index)

                    def evaluate(target: dict[str, Any]) -> None:
                        pcm, duration_ms = canonical_stream_pcm(path)
                        annotation = read_json(annotation_path)
                        expected = annotation.get("segments", [])
                        if not isinstance(expected, list) or not expected:
                            raise ValueError(f"diarization annotation has no segments: {annotation_path}")
                        prepare_run(self.sidecar, str(provider["id"]), run_kind)
                        (events, wall_time_ms), resources = sampled_call(
                            self.sidecar.process_id(),
                            lambda: stream_audio(
                                self.sidecar,
                                stage="diarization",
                                model_id=str(provider["id"]),
                                config={"expected_speakers": int(case.get("expected_speakers", 2))},
                                pcm=pcm,
                                chunk_ms=self.chunk_ms,
                                timeout=self.args.timeout_secs,
                                realtime=self.args.realtime_streaming,
                            ),
                        )
                        timings = timing_metrics(wall_time_ms, duration_ms)
                        predicted = predicted_diarization_segments(events)
                        der, mapping = diarization_error_rate(expected, predicted, duration_ms)
                        expected_count = len({str(row["speaker_id"]) for row in expected})
                        predicted_count = len({str(row["speaker_id"]) for row in predicted})
                        speaker_count_error = abs(expected_count - predicted_count)
                        quality = der is not None and der <= float(thresholds["maximum_diarization_error_rate"]) and speaker_count_error <= int(thresholds["maximum_speaker_count_error"])
                        metrics = {
                            "diarization_error_rate": der,
                            "expected_speaker_count": expected_count,
                            "predicted_speaker_count": predicted_count,
                            "speaker_count_error": speaker_count_error,
                            "speaker_mapping": json.dumps(mapping, sort_keys=True),
                            "predicted_segment_count": len(predicted),
                        }
                        target.update(status="passed" if quality else "failed", quality_passed=quality, timings=timings, resources=resources, metrics=metrics)
                        if self.args.include_events:
                            target["events"] = events

                    rows.append(execute_measurement(row, evaluate))
            self.reset_sidecar()
        return rows


def mean_metric(rows: list[dict[str, Any]], section: str, key: str) -> float | None:
    values = [row.get(section, {}).get(key) for row in rows]
    numeric = [float(value) for value in values if isinstance(value, (int, float)) and math.isfinite(float(value))]
    return statistics.fmean(numeric) if numeric else None


def summarize_models(measurements: list[dict[str, Any]]) -> list[dict[str, Any]]:
    grouped: dict[tuple[str, str, str, str], list[dict[str, Any]]] = defaultdict(list)
    for row in measurements:
        if row["status"] != "skipped":
            grouped[(row["stage"], row["mode"], row["provider_id"], row["run_kind"])].append(row)
    summaries: list[dict[str, Any]] = []
    quality_keys = {
        ("vad", "streaming"): ("speech_f1", "false_positive_rate", "miss_rate", "boundary_mean_absolute_error_ms"),
        ("stt", "recording"): ("word_error_rate", "character_error_rate"),
        ("stt", "streaming"): ("word_error_rate", "character_error_rate"),
        ("tts", "synthesis"): ("roundtrip_word_error_rate", "clipping_ratio", "leading_silence_ms", "trailing_silence_ms"),
        ("diarization", "streaming"): ("diarization_error_rate", "speaker_count_error"),
    }
    for (stage, mode, provider_id, run_kind), rows in sorted(grouped.items()):
        quality_rows = [row for row in rows if row["quality_passed"] is not None]
        summaries.append(
            {
                "stage": stage,
                "mode": mode,
                "provider_id": provider_id,
                "run_kind": run_kind,
                "measurement_count": len(rows),
                "pass_rate": (
                    sum(bool(row["quality_passed"]) for row in quality_rows) / len(quality_rows)
                    if quality_rows
                    else None
                ),
                "quality": {key: mean_metric(rows, "metrics", key) for key in quality_keys[(stage, mode)]},
                "performance": {
                    "wall_time_ms": mean_metric(rows, "timings", "wall_time_ms"),
                    "real_time_factor": mean_metric(rows, "timings", "real_time_factor"),
                    "process_rss_peak_mb": mean_metric(rows, "resources", "process_rss_peak_mb"),
                    "process_cpu_mean_percent": mean_metric(rows, "resources", "process_cpu_mean_percent"),
                },
                "pareto_preferred": False,
            }
        )
    loss_key = {
        ("vad", "streaming"): ("speech_f1", True),
        ("stt", "recording"): ("word_error_rate", False),
        ("stt", "streaming"): ("word_error_rate", False),
        ("tts", "synthesis"): ("roundtrip_word_error_rate", False),
        ("diarization", "streaming"): ("diarization_error_rate", False),
    }
    for summary in summaries:
        peers = [
            row
            for row in summaries
            if (row["stage"], row["mode"], row["run_kind"])
            == (summary["stage"], summary["mode"], summary["run_kind"])
        ]
        key, maximize = loss_key[(summary["stage"], summary["mode"])]

        def quality_loss(row: dict[str, Any]) -> float:
            value = row["quality"].get(key)
            if value is None:
                return math.inf
            return 1 - float(value) if maximize else float(value)

        own_quality = quality_loss(summary)
        own_speed = summary["performance"].get("real_time_factor")
        if own_speed is None or not math.isfinite(own_quality):
            continue
        dominated = any(
            peer is not summary
            and quality_loss(peer) <= own_quality
            and peer["performance"].get("real_time_factor") is not None
            and float(peer["performance"]["real_time_factor"]) <= float(own_speed)
            and (
                quality_loss(peer) < own_quality
                or float(peer["performance"]["real_time_factor"]) < float(own_speed)
            )
            for peer in peers
        )
        summary["pareto_preferred"] = not dominated
    return summaries


def selected_provider_ids(providers: dict[str, list[dict[str, Any]]], args: argparse.Namespace) -> list[str]:
    keys: list[str] = []
    if "vad" in args.stages:
        keys.append("vad")
    if "stt" in args.stages:
        keys.extend(f"stt_{mode}" for mode in args.stt_modes)
    if "tts" in args.stages:
        keys.append("tts")
    if "diarization" in args.stages:
        keys.append("diarization")
    return [str(row["id"]) for key in keys for row in providers[key]]


def readiness_report(
    args: argparse.Namespace,
    suite: dict[str, Any],
    fixture_manifest_path: Path,
    fixtures: dict[str, dict[str, Any]],
    providers: dict[str, list[dict[str, Any]]],
    config: dict[str, Any],
) -> dict[str, Any]:
    engine_config = nested(config, "media.engines.fluid_audio")
    configured_cache = Path(str(engine_config.get("model_cache_dir", "models/audio/fluidaudio"))).expanduser()
    cache = args.cache_dir.expanduser().resolve() if args.cache_dir else (
        configured_cache if configured_cache.is_absolute() else args.config.expanduser().resolve().parent / configured_cache
    )
    stage_case_keys = {
        "vad": ["cases"],
        "stt": [f"{mode}_cases" for mode in args.stt_modes],
        "tts": ["cases"],
        "diarization": ["cases"],
    }
    missing_fixtures: list[str] = []
    for stage in args.stages:
        for key in stage_case_keys[stage]:
            for case in suite["stages"][stage].get(key, []):
                fixture_id = str(case["fixture_id"])
                if args.fixture_ids and fixture_id not in args.fixture_ids:
                    continue
                if stage == "tts":
                    continue
                fixture = fixtures.get(fixture_id)
                if fixture is None or not fixture_path(fixture_manifest_path, fixture).is_file():
                    if not case.get("optional"):
                        missing_fixtures.append(fixture_id)
                annotation = case.get("annotation_path")
                if annotation and not (fixture_manifest_path.parent / str(annotation)).is_file() and not case.get("optional"):
                    missing_fixtures.append(f"{fixture_id}:annotation")
    provider_groups: list[str] = []
    if "vad" in args.stages:
        provider_groups.append("vad")
    if "stt" in args.stages:
        provider_groups.extend(f"stt_{mode}" for mode in args.stt_modes)
    if "tts" in args.stages:
        provider_groups.append("tts")
    if "diarization" in args.stages:
        provider_groups.append("diarization")
    missing_provider_groups = [key for key in provider_groups if not providers.get(key)]
    return {
        "magician_started": False,
        "engine_binary": str(args.engine.expanduser().resolve()),
        "engine_binary_present": args.engine.expanduser().is_file(),
        "cache_directory": str(cache.resolve()),
        "cache_directory_present": cache.is_dir(),
        "network_policy": "configured_download_policy" if args.allow_model_downloads else "cached_models_only",
        "providers": selected_provider_ids(providers, args),
        "missing_provider_groups": missing_provider_groups,
        "missing_required_fixtures": sorted(set(missing_fixtures)),
        "ready": (
            args.engine.expanduser().is_file()
            and (args.allow_model_downloads or cache.is_dir())
            and not missing_fixtures
            and not missing_provider_groups
        ),
    }


def validate_result(report: dict[str, Any]) -> None:
    schema = read_json(DATA_DIR / "offline-results.schema.json")
    try:
        import jsonschema
    except ImportError as error:  # pragma: no cover
        raise RuntimeError("jsonschema is required to validate offline eval output") from error
    jsonschema.Draft202012Validator(schema, format_checker=jsonschema.FormatChecker()).validate(report)


def print_summary(summaries: list[dict[str, Any]]) -> None:
    print("stage          mode       kind  provider                         pass     quality      rtf      rss_mb  pareto")
    for row in summaries:
        quality = next((value for value in row["quality"].values() if value is not None), None)
        pass_rate = row["pass_rate"]
        rtf = row["performance"].get("real_time_factor")
        rss = row["performance"].get("process_rss_peak_mb")
        print(
            f"{row['stage']:<14} {row['mode']:<10} {row['run_kind']:<5} {row['provider_id']:<32} "
            f"{pass_rate if pass_rate is not None else float('nan'):>6.2f} "
            f"{quality if quality is not None else float('nan'):>11.3f} "
            f"{rtf if rtf is not None else float('nan'):>8.3f} "
            f"{rss if rss is not None else float('nan'):>10.1f}  "
            f"{'yes' if row['pareto_preferred'] else 'no'}"
        )


def run_self_test() -> int:
    assert normalize_text("Hello, WORLD!") == "hello world"
    assert word_error_rate("one two three", "one two") == 1 / 3
    assert critical_entity_recall(
        "Riya approved invoice AB123 on 21/07",
        "riya approved invoice ab123 on 21/07",
    ) == 1.0
    perfect = speech_segment_metrics([(100, 400)], [(100, 400)], 500)
    assert perfect["speech_f1"] == 1
    assert perfect["boundary_mean_absolute_error_ms"] == 0
    quiet = speech_segment_metrics([], [], 500)
    assert quiet["speech_f1"] == 1
    expected = [
        {"speaker_id": "a", "start_ms": 0, "end_ms": 500},
        {"speaker_id": "b", "start_ms": 500, "end_ms": 1000},
    ]
    predicted = [
        {"speaker_id": "speaker_2", "start_ms": 0, "end_ms": 500},
        {"speaker_id": "speaker_1", "start_ms": 500, "end_ms": 1000},
    ]
    der, mapping = diarization_error_rate(expected, predicted, 1000)
    assert der == 0
    assert mapping == {"speaker_1": "b", "speaker_2": "a"}
    envelope = start_envelope("vad", "local-vad", {"threshold": 0.5})
    assert envelope["protocol_version"] == 1 and envelope["format"]["sample_rate_hz"] == 16000
    sample_measurements = [
            {
                **base_measurement("stt", "recording", {"id": "a", "adapter": "x", "model": "a"}, "f", "warm", 1),
                "status": "passed",
                "quality_passed": True,
                "metrics": {"word_error_rate": 0.1, "character_error_rate": 0.05},
                "timings": {"wall_time_ms": 100, "real_time_factor": 0.2},
                "resources": {},
            },
            {
                **base_measurement("stt", "recording", {"id": "b", "adapter": "x", "model": "b"}, "f", "warm", 1),
                "status": "passed",
                "quality_passed": True,
                "metrics": {"word_error_rate": 0.2, "character_error_rate": 0.1},
                "timings": {"wall_time_ms": 150, "real_time_factor": 0.3},
                "resources": {},
            },
        ]
    summaries = summarize_models(sample_measurements)
    assert next(row for row in summaries if row["provider_id"] == "a")["pareto_preferred"]
    assert not next(row for row in summaries if row["provider_id"] == "b")["pareto_preferred"]
    validate_result(
        {
            "schema_version": 1,
            "run_id": "self-test",
            "captured_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "host": {},
            "git_revision": "self-test",
            "config_fingerprint": "self-test",
            "suite_manifest": "offline-suite.manifest.json",
            "execution": {
                "transport": "owned_fluid_audio_sidecar",
                "network_policy": "cached_models_only",
                "magician_started": False,
                "stages": ["stt"],
                "run_kinds": ["warm"],
                "repetitions": 1,
            },
            "measurements": sample_measurements,
            "model_summaries": summaries,
            "notes": [],
        }
    )
    print("Offline audio evaluator self-test passed")
    return 0


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if args.self_test:
        return run_self_test()
    if args.generate_local_fixtures:
        subprocess.run([str(REPO_ROOT / "scripts/generate-media-local-speech-fixtures.sh")], check=True)
    config = baseline.read_yaml(args.config.expanduser().resolve())
    suite = read_json(args.suite.expanduser().resolve())
    fixture_manifest_path = (args.suite.expanduser().resolve().parent / str(suite["fixture_manifest"])).resolve()
    fixture_manifest = read_json(fixture_manifest_path)
    fixtures = fixture_index(fixture_manifest)
    providers = filter_providers(discover_providers(config, suite), args.provider_filters)
    readiness = readiness_report(args, suite, fixture_manifest_path, fixtures, providers, config)
    if args.dry_run:
        print(json.dumps(readiness, indent=2, sort_keys=True))
        return 0 if readiness["ready"] else 2
    if not readiness["engine_binary_present"]:
        raise RuntimeError(f"FluidAudio engine binary is absent: {readiness['engine_binary']}")

    evaluator = OfflineEvaluator(args, config, suite, fixture_manifest_path, fixtures, providers)
    try:
        measurements = evaluator.run()
    finally:
        evaluator.close()
    summaries = summarize_models(measurements)
    captured_at = dt.datetime.now(dt.timezone.utc)
    report = {
        "schema_version": 1,
        "run_id": f"offline-audio-{captured_at.strftime('%Y%m%dT%H%M%SZ')}-{uuid.uuid4().hex[:8]}",
        "captured_at": captured_at.isoformat(),
        "host": baseline.host_metadata(),
        "git_revision": baseline.command_output(["git", "rev-parse", "HEAD"], "unknown"),
        "config_fingerprint": baseline.media_config_fingerprint(config),
        "suite_manifest": baseline.relative_or_absolute(args.suite.expanduser().resolve()),
        "execution": {
            "transport": "owned_fluid_audio_sidecar",
            "network_policy": "configured_download_policy" if args.allow_model_downloads else "cached_models_only",
            "magician_started": False,
            "stages": args.stages,
            "run_kinds": args.run_kinds,
            "repetitions": args.repetitions,
            **({"contention_note": args.contention_note} if args.contention_note else {}),
        },
        "measurements": measurements,
        "model_summaries": summaries,
        "notes": [
            "Synthetic fixtures are plumbing and repeatability baselines, not substitutes for consented human speech.",
            "TTS intelligibility is measured by an explicitly named offline round-trip STT model and therefore couples both models.",
            "Use an idle machine for comparable performance numbers; --contention-note records unavoidable contention.",
        ],
    }
    validate_result(report)
    output = args.output or DEFAULT_RESULTS / f"{report['run_id']}.json"
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print_summary(summaries)
    print(f"Wrote {output}")
    quality_failures = [row for row in measurements if row["quality_passed"] is False]
    runtime_failures = [row for row in measurements if row["status"] == "failed" and row["quality_passed"] is None]
    if runtime_failures or (args.fail_on_quality and quality_failures):
        return 2
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (KeyError, ValueError, RuntimeError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(2)
