#!/usr/bin/env python3
"""Benchmark recording STT providers directly, without running Magician.

The direct transport owns local provider processes and invokes configured cloud
APIs itself. The optional ``magician`` transport remains useful for an end-to-
end comparison, but is not required for provider quality or latency evaluation.
"""

from __future__ import annotations

import argparse
import base64
import datetime as dt
import hashlib
import json
import mimetypes
import os
import platform
import re
import secrets
import socket
import ssl
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path
from typing import Any

import yaml

try:
    import certifi
except ImportError:  # pragma: no cover - system Python may already have a usable trust store
    certifi = None


REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_MANIFEST = REPO_ROOT / "data/magician_v2/media_evals/fixtures.manifest.json"
DEFAULT_RESULTS = REPO_ROOT / "data/magician_v2/media_evals/results"
DEFAULT_RUNTIME_CONFIG = Path.home() / "MagicianNotes/magician-config.yaml"
DEFAULT_CONFIG = DEFAULT_RUNTIME_CONFIG if DEFAULT_RUNTIME_CONFIG.is_file() else REPO_ROOT / "magician-config.yaml"
DEFAULT_ENV_FILES = (
    Path.home() / "MagicianNotes/.env",
    Path.home() / "MagicianNotes/.env.development",
)
DEFAULT_APPLE_HELPER = REPO_ROOT / "magician-macos-speech-helper.bin"
DEFAULT_FLUID_ENGINE = REPO_ROOT / "magician-macos-audio-engine.bin"
LOCAL_ADAPTERS = {"macos_speech_helper", "fluid_audio_recording_stt"}
APPLE_PROVIDER_IDS = {"macos_speech", "macos-speech", "apple_speech", "apple-speech"}
PROTOCOL_VERSION = 1
HTTPS_CONTEXT = ssl.create_default_context(cafile=certifi.where() if certifi else None)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Benchmark Dictation STT providers directly. Online providers are never "
            "called unless --allow-online is supplied."
        )
    )
    parser.add_argument(
        "--transport",
        choices=("direct", "magician"),
        default="direct",
        help="direct invokes providers itself; magician uses the running service API",
    )
    parser.add_argument("--base-url", default="http://127.0.0.1:3002")
    parser.add_argument("--config", type=Path, default=DEFAULT_CONFIG)
    parser.add_argument(
        "--env-file",
        action="append",
        type=Path,
        help="dotenv file used for configured API-key env names; may be repeated",
    )
    parser.add_argument("--apple-helper", type=Path, default=DEFAULT_APPLE_HELPER)
    parser.add_argument("--fluid-engine", type=Path, default=DEFAULT_FLUID_ENGINE)
    parser.add_argument(
        "--fluid-cache-dir",
        type=Path,
        help="benchmark-only FluidAudio model-cache override (useful for an SSD volume)",
    )
    parser.add_argument(
        "--fluid-model-idle-secs",
        type=int,
        help="benchmark-only model idle override used to verify automatic unload",
    )
    parser.add_argument(
        "--idle-observation-secs",
        type=float,
        default=0,
        help="wait after inference and record model residency/RSS (zero disables)",
    )
    parser.add_argument("--language", default="en-US")
    parser.add_argument(
        "--providers",
        default="macos_speech,fluid-qwen3-asr-f32",
        help="comma-separated configured provider IDs",
    )
    parser.add_argument("--fixtures", help="optional comma-separated fixture IDs")
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--warm-runs", type=int, default=5)
    parser.add_argument("--timeout-secs", type=float, default=600)
    parser.add_argument(
        "--allow-online",
        action="store_true",
        help="explicitly consent to uploading selected audio fixtures to online providers",
    )
    parser.add_argument(
        "--cold-only",
        action="store_true",
        help="record one cold request; direct FluidAudio mode loads then unloads the model first",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="inspect adapters, binaries, model cache, and credential presence without inference",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="run provider-free script regressions and exit",
    )
    args = parser.parse_args()
    if args.warm_runs < 1:
        parser.error("--warm-runs must be at least 1")
    if args.timeout_secs <= 0:
        parser.error("--timeout-secs must be positive")
    if args.fluid_model_idle_secs is not None and args.fluid_model_idle_secs < 1:
        parser.error("--fluid-model-idle-secs must be positive")
    if args.idle_observation_secs < 0:
        parser.error("--idle-observation-secs cannot be negative")
    return args


def csv_values(value: str | None) -> list[str]:
    if not value:
        return []
    return [item.strip() for item in value.split(",") if item.strip()]


def normalize_text(value: str) -> str:
    return " ".join(re.findall(r"[^\W_]+(?:'[^\W_]+)?", value.lower(), flags=re.UNICODE))


def edit_distance(left: list[str], right: list[str]) -> int:
    previous = list(range(len(right) + 1))
    for row, left_item in enumerate(left, 1):
        current = [row]
        for column, right_item in enumerate(right, 1):
            current.append(
                min(
                    current[-1] + 1,
                    previous[column] + 1,
                    previous[column - 1] + (left_item != right_item),
                )
            )
        previous = current
    return previous[-1]


def error_rate(reference: str, hypothesis: str, *, characters: bool) -> float | None:
    normalized_reference = normalize_text(reference)
    normalized_hypothesis = normalize_text(hypothesis)
    reference_items = list(normalized_reference.replace(" ", "")) if characters else normalized_reference.split()
    hypothesis_items = list(normalized_hypothesis.replace(" ", "")) if characters else normalized_hypothesis.split()
    if not reference_items:
        return 0.0 if not hypothesis_items else None
    return edit_distance(reference_items, hypothesis_items) / len(reference_items)


def load_reference(manifest_dir: Path, fixture: dict[str, Any]) -> tuple[str | None, list[str]]:
    relative = fixture.get("transcript_path")
    if not relative:
        return fixture.get("expected", {}).get("transcript"), []
    path = manifest_dir / relative
    if not path.is_file():
        return None, []
    if path.suffix.lower() == ".json":
        payload = json.loads(path.read_text(encoding="utf-8"))
        if isinstance(payload, dict):
            return str(payload.get("transcript", "")), [str(item) for item in payload.get("entities", [])]
        return None, []
    return path.read_text(encoding="utf-8").strip(), []


def multipart_form(
    fields: dict[str, str],
    *,
    file_field: str,
    filename: str,
    content_type: str,
    audio: bytes,
) -> tuple[bytes, str]:
    boundary = f"magician-{uuid.uuid4().hex}"
    body = bytearray()
    for name, value in fields.items():
        body.extend(f"--{boundary}\r\n".encode())
        body.extend(f'Content-Disposition: form-data; name="{name}"\r\n\r\n'.encode())
        body.extend(value.encode())
        body.extend(b"\r\n")
    body.extend(f"--{boundary}\r\n".encode())
    body.extend(
        f'Content-Disposition: form-data; name="{file_field}"; filename="{filename}"\r\n'.encode()
    )
    body.extend(f"Content-Type: {content_type}\r\n\r\n".encode())
    body.extend(audio)
    body.extend(f"\r\n--{boundary}--\r\n".encode())
    return bytes(body), boundary


def read_json_response(request: urllib.request.Request, timeout: float) -> tuple[int, dict[str, Any]]:
    try:
        with urllib.request.urlopen(request, timeout=timeout, context=HTTPS_CONTEXT) as response:
            raw = response.read().decode("utf-8", errors="replace")
            return response.status, json.loads(raw) if raw else {}
    except urllib.error.HTTPError as error:
        raw = error.read().decode("utf-8", errors="replace")
        try:
            payload = json.loads(raw)
        except json.JSONDecodeError:
            payload = {"error": raw or str(error)}
        return error.code, payload


def transcribe_via_magician(
    base_url: str,
    provider: str,
    audio_path: Path,
    timeout: float,
) -> tuple[int, dict[str, Any]]:
    query = urllib.parse.urlencode({"provider": provider})
    body, boundary = multipart_form(
        {},
        file_field="file",
        filename=audio_path.name,
        content_type=content_type_for(audio_path),
        audio=audio_path.read_bytes(),
    )
    request = urllib.request.Request(
        f"{base_url.rstrip('/')}/api/magician/v2/media/stt/transcribe?{query}",
        data=body,
        method="POST",
        headers={
            "Content-Type": f"multipart/form-data; boundary={boundary}",
            **(
                {"Authorization": f"Bearer {os.environ['MAGICIAN_BEARER_TOKEN'].strip()}"}
                if os.environ.get("MAGICIAN_BEARER_TOKEN", "").strip()
                else {}
            ),
        },
    )
    return read_json_response(request, timeout)


def command_output(command: list[str], fallback: str) -> str:
    try:
        return subprocess.check_output(command, text=True, stderr=subprocess.DEVNULL).strip() or fallback
    except (OSError, subprocess.CalledProcessError):
        return fallback


def host_metadata() -> dict[str, Any]:
    memory_bytes = command_output(["sysctl", "-n", "hw.memsize"], "0")
    try:
        memory_gb = int(memory_bytes) / (1024**3)
    except ValueError:
        memory_gb = 0
    if memory_gb <= 0:
        try:
            memory_gb = os.sysconf("SC_PHYS_PAGES") * os.sysconf("SC_PAGE_SIZE") / (1024**3)
        except (OSError, ValueError):
            memory_gb = 0.1
    return {
        "os": platform.system(),
        "os_version": platform.mac_ver()[0] or platform.release(),
        "architecture": platform.machine(),
        "hardware": command_output(["sysctl", "-n", "hw.model"], platform.machine()),
        "memory_gb": round(memory_gb, 2),
    }


def process_resource_sample(pid: int | None) -> tuple[float, float] | None:
    if not pid:
        return None
    try:
        output = subprocess.check_output(
            ["ps", "-o", "rss=", "-o", "%cpu=", "-p", str(pid)],
            text=True,
            stderr=subprocess.DEVNULL,
        ).strip()
        if not output:
            return None
        rss_kb, cpu_percent = output.split()[-2:]
        return float(rss_kb) / 1024.0, float(cpu_percent)
    except (OSError, ValueError, subprocess.CalledProcessError):
        return None


class ProcessResourceSampler:
    def __init__(self, pid: int | None) -> None:
        self.pid = pid
        self.samples: list[tuple[float, float]] = []
        self.stop_event = threading.Event()
        self.thread: threading.Thread | None = None

    def start(self) -> None:
        if not self.pid:
            return
        initial = process_resource_sample(self.pid)
        if initial is not None:
            self.samples.append(initial)
        self.thread = threading.Thread(target=self._sample, daemon=True)
        self.thread.start()

    def stop(self) -> dict[str, float]:
        self.stop_event.set()
        if self.thread is not None:
            self.thread.join(timeout=1)
        final = process_resource_sample(self.pid)
        if final is not None:
            self.samples.append(final)
        if not self.samples:
            return {}
        rss = [sample[0] for sample in self.samples]
        cpu = [sample[1] for sample in self.samples]
        return {
            "process_rss_baseline_mb": round(rss[0], 2),
            "process_rss_peak_mb": round(max(rss), 2),
            "process_cpu_mean_percent": round(statistics.mean(cpu), 2),
            "process_cpu_peak_percent": round(max(cpu), 2),
        }

    def _sample(self) -> None:
        while not self.stop_event.wait(0.05):
            sample = process_resource_sample(self.pid)
            if sample is not None:
                self.samples.append(sample)


def read_yaml(path: Path) -> dict[str, Any]:
    payload = yaml.safe_load(path.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise ValueError(f"config is not a YAML mapping: {path}")
    return payload


def media_config_fingerprint(config: dict[str, Any]) -> str:
    media = config.get("media", {})
    return hashlib.sha256(
        json.dumps(media, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def parse_dotenv(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    if not path.is_file():
        return values
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line[7:].lstrip()
        if "=" not in line:
            continue
        name, value = line.split("=", 1)
        name = name.strip()
        value = value.strip()
        if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name):
            continue
        if len(value) >= 2 and value[0] == value[-1] and value[0] in {'"', "'"}:
            value = value[1:-1]
        values[name] = value
    return values


def load_runtime_environment(paths: list[Path]) -> dict[str, str]:
    values: dict[str, str] = {}
    for path in paths:
        values.update(parse_dotenv(path.expanduser()))
    values.update(os.environ)
    return values


def recording_provider_specs(config: dict[str, Any]) -> dict[str, dict[str, Any]]:
    providers = config.get("media", {}).get("recording_stt", {}).get("providers", [])
    result: dict[str, dict[str, Any]] = {}
    for provider in providers:
        if isinstance(provider, dict) and str(provider.get("id", "")).strip():
            result[str(provider["id"])] = dict(provider)
    for provider_id in APPLE_PROVIDER_IDS:
        result.setdefault(
            provider_id,
            {
                "id": provider_id,
                "label": "macOS Speech",
                "adapter": "macos_speech_helper",
                "model": "macos_speech",
            },
        )
    return result


def provider_is_online(spec: dict[str, Any]) -> bool:
    return str(spec.get("adapter", "")) not in LOCAL_ADAPTERS


def content_type_for(path: Path) -> str:
    guessed, _ = mimetypes.guess_type(path.name)
    return guessed or "application/octet-stream"


def free_loopback_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


class FluidAudioSidecar:
    def __init__(
        self,
        *,
        binary: Path,
        engine_config: dict[str, Any],
        provider_specs: list[dict[str, Any]],
        config_path: Path,
        cache_dir_override: Path | None,
        model_idle_secs_override: int | None,
        timeout: float,
    ) -> None:
        self.binary = binary
        self.engine_config = engine_config
        self.provider_specs = provider_specs
        self.config_path = config_path
        self.cache_dir_override = cache_dir_override
        self.model_idle_secs_override = model_idle_secs_override
        self.timeout = timeout
        self.process: subprocess.Popen[bytes] | None = None
        self.log_file: Any = None
        self.token = secrets.token_hex(32)
        self.endpoint: str | None = None

    def resolved_cache_dir(self) -> Path:
        if self.cache_dir_override is not None:
            return self.cache_dir_override.expanduser().resolve()
        configured = Path(str(self.engine_config.get("model_cache_dir", "models/audio/fluidaudio"))).expanduser()
        return configured if configured.is_absolute() else self.config_path.parent / configured

    def sidecar_config(self) -> dict[str, Any]:
        models = []
        for provider in self.provider_specs:
            models.append(
                {
                    "id": str(provider["id"]),
                    "adapter": str(provider["adapter"]),
                    "repository": str(provider["model"]),
                    "variant": provider.get("variant"),
                    "revision": provider.get("revision"),
                    "sha256": provider.get("sha256"),
                    "voice": provider.get("voice"),
                    "voices": provider.get("voices"),
                    "formats": provider.get("formats"),
                    "idle_secs": int(
                        self.model_idle_secs_override
                        or provider.get("idle_secs")
                        or self.engine_config.get("default_model_idle_secs", 300)
                    ),
                }
            )
        return {
            "protocol_version": PROTOCOL_VERSION,
            "model_cache_dir": str(self.resolved_cache_dir()),
            "download_policy": str(self.engine_config.get("download_policy", "on_demand")),
            "registry_url": self.engine_config.get("registry_url"),
            "offline": bool(self.engine_config.get("offline", False)),
            "process_idle_secs": max(
                int(self.engine_config.get("idle_process_secs", 900)), int(self.timeout) + 60
            ),
            "max_resident_models": max(1, int(self.engine_config.get("max_resident_models", 2))),
            "max_streaming_sessions": max(1, int(self.engine_config.get("max_streaming_sessions", 2))),
            "max_request_bytes": int(self.engine_config.get("max_request_bytes", 25 * 1024 * 1024)),
            "max_frame_bytes": int(self.engine_config.get("max_frame_bytes", 1024 * 1024)),
            "prewarm": [],
            "models": models,
        }

    def start(self) -> None:
        if self.process is not None:
            return
        if not self.binary.is_file():
            raise RuntimeError(f"FluidAudio engine binary is absent: {self.binary}")
        self.endpoint = f"http://127.0.0.1:{free_loopback_port()}"
        environment = os.environ.copy()
        environment.update(
            {
                "MAGICIAN_AUDIO_ENGINE_TOKEN": self.token,
                "MAGICIAN_AUDIO_ENGINE_ENDPOINT": self.endpoint,
                "MAGICIAN_AUDIO_ENGINE_CONFIG_JSON": json.dumps(self.sidecar_config()),
            }
        )
        self.log_file = tempfile.TemporaryFile(mode="w+b")
        self.process = subprocess.Popen(
            [str(self.binary)],
            cwd=REPO_ROOT,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=self.log_file,
            stderr=subprocess.STDOUT,
        )
        deadline = time.monotonic() + max(10.0, float(self.engine_config.get("health_timeout_secs", 3)))
        last_error = "sidecar did not answer health"
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise RuntimeError(
                    f"FluidAudio sidecar exited with {self.process.returncode}: {self._log_tail()}"
                )
            try:
                status, payload = self.request("GET", "/health", timeout=1.0)
                if status == 200 and payload.get("status") == "ok" and payload.get("protocol_version") == PROTOCOL_VERSION:
                    return
                last_error = f"health returned {status}: {payload}"
            except Exception as error:
                last_error = str(error)
            time.sleep(0.1)
        raise RuntimeError(f"FluidAudio sidecar startup timed out: {last_error}; {self._log_tail()}")

    def request(
        self,
        method: str,
        path: str,
        *,
        data: bytes | None = None,
        headers: dict[str, str] | None = None,
        timeout: float | None = None,
    ) -> tuple[int, dict[str, Any]]:
        if self.endpoint is None:
            raise RuntimeError("FluidAudio sidecar has not been started")
        request_headers = {
            "Authorization": f"Bearer {self.token}",
            "x-magician-audio-protocol": str(PROTOCOL_VERSION),
        }
        request_headers.update(headers or {})
        request = urllib.request.Request(
            f"{self.endpoint}{path}",
            data=data,
            method=method,
            headers=request_headers,
        )
        return read_json_response(request, timeout or self.timeout)

    def prepare(self, model_id: str, *, leave_loaded: bool) -> None:
        self.start()
        quoted = urllib.parse.quote(model_id, safe="")
        try:
            status, payload = self.request("POST", f"/models/{quoted}/load")
        except Exception as error:
            exit_code = self.process.poll() if self.process is not None else None
            process_state = f"exited with {exit_code}" if exit_code is not None else "closed the request"
            raise RuntimeError(
                f"FluidAudio sidecar {process_state} while loading {model_id}: "
                f"{error}; {self._log_tail()}"
            ) from error
        if status >= 400:
            raise RuntimeError(f"FluidAudio model preparation failed ({status}): {payload}")
        if leave_loaded:
            return
        status, payload = self.request("POST", f"/models/{quoted}/unload")
        if status >= 400:
            raise RuntimeError(f"FluidAudio model unload failed ({status}): {payload}")

    def transcribe(self, spec: dict[str, Any], audio_path: Path, language: str) -> tuple[int, dict[str, Any]]:
        self.start()
        headers = {
            "Content-Type": content_type_for(audio_path),
            "x-magician-audio-model": str(spec["id"]),
        }
        if language:
            headers["x-magician-audio-language"] = language
        try:
            status, payload = self.request(
                "POST",
                "/v1/audio/transcriptions",
                data=audio_path.read_bytes(),
                headers=headers,
            )
        except Exception as error:
            exit_code = self.process.poll() if self.process is not None else None
            process_state = f"exited with {exit_code}" if exit_code is not None else "closed the request"
            raise RuntimeError(
                f"FluidAudio sidecar {process_state}: {error}; {self._log_tail()}"
            ) from error
        if status < 400:
            payload["extras"] = {
                "provider": spec["id"],
                "model": payload.get("model"),
                "variant": payload.get("variant"),
                "audio_duration_ms": payload.get("audio_duration_ms"),
                "processing_duration_ms": payload.get("processing_duration_ms"),
                "confidence": payload.get("confidence"),
            }
        return status, payload

    def close(self) -> None:
        process = self.process
        self.process = None
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        if self.log_file is not None:
            self.log_file.close()
            self.log_file = None

    def process_id(self) -> int | None:
        return self.process.pid if self.process is not None and self.process.poll() is None else None

    def observe_idle(self, model_id: str, wait_secs: float) -> dict[str, Any]:
        time.sleep(wait_secs)
        process_id = self.process_id()
        sample = process_resource_sample(process_id)
        state: dict[str, Any] | None = None
        if process_id is not None:
            status, models = self.request("GET", "/models")
            if status < 400 and isinstance(models, list):
                state = next(
                    (row for row in models if isinstance(row, dict) and row.get("id") == model_id),
                    None,
                )
        return {
            "provider_id": model_id,
            "wait_secs": wait_secs,
            "process_running": process_id is not None,
            "process_rss_mb": round(sample[0], 2) if sample is not None else 0,
            "model_state": state.get("state") if state else "process_stopped",
            "resident": bool(state and state.get("resident")),
            "passed": process_id is not None and state is not None and not bool(state.get("resident")),
        }

    def _log_tail(self) -> str:
        if self.log_file is None:
            return "no sidecar log"
        self.log_file.flush()
        self.log_file.seek(0)
        text = self.log_file.read().decode("utf-8", errors="replace")
        return " | ".join(text.splitlines()[-8:]) or "empty sidecar log"


class DirectProviders:
    def __init__(self, args: argparse.Namespace, config: dict[str, Any], environment: dict[str, str]) -> None:
        self.args = args
        self.config = config
        self.environment = environment
        self.specs = recording_provider_specs(config)
        self.fluid: FluidAudioSidecar | None = None

    def spec(self, provider_id: str) -> dict[str, Any]:
        try:
            return self.specs[provider_id]
        except KeyError as error:
            raise ValueError(f"provider is not configured for direct benchmarking: {provider_id}") from error

    def prepare(self, provider_id: str, *, cold_only: bool) -> None:
        spec = self.spec(provider_id)
        adapter = str(spec.get("adapter", ""))
        if adapter == "macos_speech_helper":
            self._check_apple_status()
        elif adapter == "fluid_audio_recording_stt":
            self._fluid_sidecar().prepare(provider_id, leave_loaded=not cold_only)
        elif provider_is_online(spec):
            self._require_secret(spec)

    def transcribe(self, provider_id: str, audio_path: Path) -> tuple[int, dict[str, Any]]:
        spec = self.spec(provider_id)
        adapter = str(spec.get("adapter", ""))
        if adapter == "macos_speech_helper":
            return self._transcribe_apple(spec, audio_path)
        if adapter == "fluid_audio_recording_stt":
            return self._fluid_sidecar().transcribe(spec, audio_path, self.args.language)
        if adapter == "openai_transcriptions":
            return self._transcribe_openai(spec, audio_path)
        if adapter == "gemini_generate_content":
            return self._transcribe_gemini(spec, audio_path)
        raise ValueError(f"direct benchmark does not implement adapter {adapter!r} for {provider_id}")

    def warm_local_provider(self, provider_id: str, audio_path: Path) -> None:
        spec = self.spec(provider_id)
        if spec.get("adapter") != "fluid_audio_recording_stt":
            return
        status, payload = self._fluid_sidecar().transcribe(spec, audio_path, self.args.language)
        if status >= 400:
            raise RuntimeError(f"FluidAudio warmup failed ({status}): {payload}")

    def probe(self, provider_id: str) -> dict[str, Any]:
        spec = self.spec(provider_id)
        adapter = str(spec.get("adapter", ""))
        row: dict[str, Any] = {
            "provider_id": provider_id,
            "adapter": adapter,
            "model": str(spec.get("model", "")),
            "online": provider_is_online(spec),
        }
        if adapter == "macos_speech_helper":
            row["binary"] = str(self.args.apple_helper)
            row["binary_present"] = self.args.apple_helper.is_file()
            row["authorization"] = self._apple_status_payload()
        elif adapter == "fluid_audio_recording_stt":
            sidecar = self._fluid_sidecar()
            cache = sidecar.resolved_cache_dir()
            variant = str(spec.get("variant") or "f32")
            expected = cache / "qwen3-asr-0.6b-coreml" / variant
            row.update(
                {
                    "binary": str(self.args.fluid_engine),
                    "binary_present": self.args.fluid_engine.is_file(),
                    "model_cache": str(cache),
                    "model_cache_present": expected.is_dir(),
                }
            )
        else:
            env_name = str(spec.get("api_key_env") or spec.get("auth_token_env") or "")
            row["credential_env"] = env_name
            row["credential_present"] = bool(env_name and self.environment.get(env_name, "").strip())
        return row

    def close(self) -> None:
        if self.fluid is not None:
            self.fluid.close()

    def resource_pid(self, provider_id: str) -> int | None:
        spec = self.spec(provider_id)
        if spec.get("adapter") != "fluid_audio_recording_stt" or self.fluid is None:
            return None
        return self.fluid.process_id()

    def observe_idle(self, provider_id: str, wait_secs: float) -> dict[str, Any] | None:
        spec = self.spec(provider_id)
        if spec.get("adapter") != "fluid_audio_recording_stt" or self.fluid is None:
            return None
        return self.fluid.observe_idle(provider_id, wait_secs)

    def _fluid_sidecar(self) -> FluidAudioSidecar:
        if self.fluid is None:
            engine_config = self.config.get("media", {}).get("engines", {}).get("fluid_audio", {})
            fluid_specs = [
                spec
                for spec in self.specs.values()
                if spec.get("adapter") == "fluid_audio_recording_stt"
            ]
            self.fluid = FluidAudioSidecar(
                binary=self.args.fluid_engine,
                engine_config=engine_config,
                provider_specs=fluid_specs,
                config_path=self.args.config,
                cache_dir_override=self.args.fluid_cache_dir,
                model_idle_secs_override=self.args.fluid_model_idle_secs,
                timeout=self.args.timeout_secs,
            )
        return self.fluid

    def _apple_status_payload(self) -> dict[str, Any]:
        if not self.args.apple_helper.is_file():
            return {"authorized": False, "status": "binary_absent"}
        completed = subprocess.run(
            [str(self.args.apple_helper), "status"],
            capture_output=True,
            text=True,
            timeout=min(self.args.timeout_secs, 30),
            check=False,
        )
        if completed.returncode != 0:
            return {"authorized": False, "status": completed.stderr.strip() or "status_failed"}
        try:
            payload = json.loads(completed.stdout)
            return payload if isinstance(payload, dict) else {"authorized": False, "status": "invalid_status"}
        except json.JSONDecodeError:
            return {"authorized": False, "status": "invalid_status_json"}

    def _check_apple_status(self) -> None:
        status = self._apple_status_payload()
        if not status.get("authorized"):
            raise RuntimeError(f"Apple Speech is not authorized: {status.get('status', 'unknown')}")

    def _transcribe_apple(self, spec: dict[str, Any], audio_path: Path) -> tuple[int, dict[str, Any]]:
        completed = subprocess.run(
            [
                str(self.args.apple_helper),
                "transcribe",
                "--file",
                str(audio_path),
                "--locale",
                self.args.language,
            ],
            capture_output=True,
            text=True,
            timeout=self.args.timeout_secs,
            check=False,
        )
        if completed.returncode != 0:
            return 503, {"error": completed.stderr.strip() or "Apple Speech helper failed"}
        payload = json.loads(completed.stdout)
        payload.setdefault("model", str(spec.get("model", "macos_speech")))
        return 200, payload

    def _require_secret(self, spec: dict[str, Any]) -> str:
        env_name = str(spec.get("api_key_env") or spec.get("auth_token_env") or "").strip()
        if not env_name:
            raise RuntimeError(f"provider {spec['id']} has no configured credential env name")
        value = self.environment.get(env_name, "").strip()
        if not value:
            raise RuntimeError(f"provider {spec['id']} requires {env_name}, but it is not set")
        return value

    def _transcribe_openai(self, spec: dict[str, Any], audio_path: Path) -> tuple[int, dict[str, Any]]:
        fields = {"model": str(spec["model"]), "response_format": "json"}
        if self.args.language:
            fields["language"] = self.args.language.split("-", 1)[0]
        prompt = str(spec.get("prompt", "")).strip()
        if prompt:
            fields["prompt"] = prompt
        body, boundary = multipart_form(
            fields,
            file_field="file",
            filename=audio_path.name,
            content_type=content_type_for(audio_path),
            audio=audio_path.read_bytes(),
        )
        endpoint = str(spec.get("base_url") or "https://api.openai.com/v1/audio/transcriptions")
        request = urllib.request.Request(
            endpoint,
            data=body,
            method="POST",
            headers={
                "Authorization": f"Bearer {self._require_secret(spec)}",
                "Content-Type": f"multipart/form-data; boundary={boundary}",
            },
        )
        status, payload = read_json_response(request, min(self.args.timeout_secs, 300))
        if status < 400:
            payload["transcript"] = str(payload.get("text", ""))
            payload["model"] = str(spec["model"])
        return status, payload

    def _transcribe_gemini(self, spec: dict[str, Any], audio_path: Path) -> tuple[int, dict[str, Any]]:
        prompt = str(spec.get("prompt") or "Generate a clean verbatim transcript of the speech. Return only the transcript text.")
        if self.args.language:
            prompt += f"\nLanguage hint: {self.args.language}."
        body: dict[str, Any] = {
            "contents": [
                {
                    "role": "user",
                    "parts": [
                        {"text": prompt},
                        {
                            "inline_data": {
                                "mime_type": content_type_for(audio_path),
                                "data": base64.b64encode(audio_path.read_bytes()).decode("ascii"),
                            }
                        },
                    ],
                }
            ]
        }
        if isinstance(spec.get("generation_config"), dict):
            body["generationConfig"] = spec["generation_config"]
        base_url = str(spec.get("base_url") or "https://generativelanguage.googleapis.com/v1beta")
        model = urllib.parse.quote(str(spec["model"]).removeprefix("models/"), safe="-._")
        request = urllib.request.Request(
            f"{base_url.rstrip('/')}/models/{model}:generateContent",
            data=json.dumps(body).encode(),
            method="POST",
            headers={
                "Content-Type": "application/json",
                "x-goog-api-key": self._require_secret(spec),
            },
        )
        status, payload = read_json_response(request, min(self.args.timeout_secs, 300))
        if status < 400:
            parts = []
            for candidate in payload.get("candidates", []):
                for part in candidate.get("content", {}).get("parts", []):
                    if str(part.get("text", "")).strip():
                        parts.append(str(part["text"]).strip())
            payload["transcript"] = "\n".join(parts)
            payload["model"] = str(spec["model"])
        return status, payload


def skipped_measurement(
    provider: str,
    model: str,
    fixture_id: str,
    run_kind: str,
    reason: str,
) -> dict[str, Any]:
    return {
        "surface": "dictation",
        "provider_id": provider,
        "model": model,
        "fixture_id": fixture_id,
        "run_kind": run_kind,
        "status": "not_run_by_request",
        "skip_reason": reason,
    }


def failed_measurement(
    provider: str,
    model: str,
    fixture_id: str,
    run_kind: str,
    reason: str,
) -> dict[str, Any]:
    return {
        "surface": "dictation",
        "provider_id": provider,
        "model": model,
        "fixture_id": fixture_id,
        "run_kind": run_kind,
        "status": "failed",
        "error": reason,
    }


def run_measurement(
    manifest_dir: Path,
    provider: str,
    model: str,
    fixture: dict[str, Any],
    run_kind: str,
    transcribe: Any,
    resource_pid: int | None = None,
) -> dict[str, Any]:
    fixture_id = str(fixture["id"])
    audio_path = manifest_dir / str(fixture["path"])
    expected = fixture.get("expected", {})
    reference, entities = load_reference(manifest_dir, fixture)
    started = time.perf_counter()
    sampler = ProcessResourceSampler(resource_pid)
    sampler.start()
    try:
        status_code, payload = transcribe(provider, audio_path)
        elapsed_ms = round((time.perf_counter() - started) * 1000, 2)
        transcript = str(payload.get("transcript", "")).strip() if status_code < 400 else ""
        no_speech = not expected.get("speech", True)
        accepted_no_speech = no_speech and (status_code < 400 or status_code == 422)
        passed = (accepted_no_speech and not transcript) or (
            not no_speech and status_code < 400 and bool(transcript)
        )
        extras = payload.get("extras") if isinstance(payload.get("extras"), dict) else {}
        entity_score = None
        if entities:
            normalized_hypothesis = normalize_text(transcript)
            entity_score = sum(
                normalize_text(entity) in normalized_hypothesis for entity in entities
            ) / len(entities)
        row = {
            "surface": "dictation",
            "provider_id": provider,
            "model": str(payload.get("model") or extras.get("model") or model),
            "fixture_id": fixture_id,
            "run_kind": run_kind,
            "status": "passed" if passed else "failed",
            "final_result_ms": elapsed_ms,
            "wall_time_ms": elapsed_ms,
            "word_error_rate": error_rate(reference, transcript, characters=False) if reference is not None else None,
            "character_error_rate": error_rate(reference, transcript, characters=True) if reference is not None else None,
            "entity_exact_match": entity_score,
            "transcript": transcript or None,
            "error": None if passed else str(
                payload.get("message") or payload.get("error") or f"HTTP {status_code}"
            ),
        }
        row.update(sampler.stop())
        return row
    except Exception as error:  # benchmark rows must survive one provider failure
        row = {
            "surface": "dictation",
            "provider_id": provider,
            "model": model,
            "fixture_id": fixture_id,
            "run_kind": run_kind,
            "status": "failed",
            "wall_time_ms": round((time.perf_counter() - started) * 1000, 2),
            "error": str(error),
        }
        row.update(sampler.stop())
        return row


def relative_or_absolute(path: Path) -> str:
    try:
        return str(path.resolve().relative_to(REPO_ROOT))
    except ValueError:
        return str(path.resolve())


def print_summary(measurements: list[dict[str, Any]]) -> None:
    provider_ids = list(dict.fromkeys(str(row["provider_id"]) for row in measurements))
    print("\nProvider comparison")
    print("provider                         pass fail skip p50_ms    mean_wer")
    for provider_id in provider_ids:
        rows = [row for row in measurements if row["provider_id"] == provider_id]
        passed = [row for row in rows if row["status"] == "passed"]
        failed = [row for row in rows if row["status"] == "failed"]
        skipped = [row for row in rows if row["status"] in {"skipped", "not_run_by_request"}]
        latencies = [float(row["wall_time_ms"]) for row in passed if row.get("wall_time_ms") is not None]
        word_errors = [float(row["word_error_rate"]) for row in passed if row.get("word_error_rate") is not None]
        latency = f"{statistics.median(latencies):.1f}" if latencies else "-"
        mean_wer = f"{statistics.mean(word_errors):.4f}" if word_errors else "-"
        print(
            f"{provider_id[:31]:31} {len(passed):4} {len(failed):4} "
            f"{len(skipped):4} {latency:9} {mean_wer}"
        )


def run_self_test() -> int:
    assert csv_values(" a,b ,, c ") == ["a", "b", "c"]
    assert error_rate("one two", "one too", characters=False) == 0.5
    assert error_rate("", "", characters=True) == 0.0
    body, boundary = multipart_form(
        {"model": "test-model"},
        file_field="file",
        filename="test.wav",
        content_type="audio/wav",
        audio=b"audio",
    )
    assert boundary.encode() in body and b'test-model' in body and b'filename="test.wav"' in body
    config = read_yaml(REPO_ROOT / "magician-config.yaml")
    specs = recording_provider_specs(config)
    assert specs["macos_speech"]["adapter"] == "macos_speech_helper"
    assert specs["fluid-qwen3-asr-f32"]["adapter"] == "fluid_audio_recording_stt"
    assert provider_is_online(specs["openai"])
    assert not provider_is_online(specs["fluid-qwen3-asr-f32"])
    assert parse_dotenv(Path("/definitely/absent")) == {}
    print("Direct recording-STT benchmark self-test passed")
    return 0


def main() -> int:
    args = parse_args()
    if args.self_test:
        return run_self_test()

    config = read_yaml(args.config)
    env_paths = args.env_file or list(DEFAULT_ENV_FILES)
    environment = load_runtime_environment(env_paths)
    specs = recording_provider_specs(config)
    providers = csv_values(args.providers)
    if not providers:
        raise SystemExit("at least one --providers value is required")

    if args.transport == "direct":
        direct = DirectProviders(args, config, environment)
        if args.dry_run:
            try:
                print(json.dumps({"transport": "direct", "providers": [direct.probe(provider) for provider in providers]}, indent=2))
                return 0
            finally:
                direct.close()
    elif args.dry_run:
        print(json.dumps({"transport": "magician", "base_url": args.base_url, "providers": providers}, indent=2))
        return 0

    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    manifest_dir = args.manifest.parent
    requested_fixtures = set(csv_values(args.fixtures))
    fixtures = [
        fixture for fixture in manifest["fixtures"]
        if not requested_fixtures or fixture["id"] in requested_fixtures
    ]
    if requested_fixtures and len(fixtures) != len(requested_fixtures):
        found = {str(fixture["id"]) for fixture in fixtures}
        raise SystemExit(f"unknown fixture IDs: {', '.join(sorted(requested_fixtures - found))}")
    if args.cold_only and (len(providers) != 1 or len(fixtures) != 1):
        raise SystemExit("--cold-only requires exactly one provider and one fixture")

    run_id = (
        dt.datetime.now(dt.timezone.utc).strftime("recording-stt-%Y%m%dT%H%M%SZ-")
        + uuid.uuid4().hex[:8]
    )
    output = args.output or (DEFAULT_RESULTS / f"{run_id}.json")
    measurements: list[dict[str, Any]] = []
    idle_observations: list[dict[str, Any]] = []
    preparation_errors: dict[str, str] = {}
    direct = DirectProviders(args, config, environment) if args.transport == "direct" else None
    warmup_fixture = next(
        (
            manifest_dir / str(fixture["path"])
            for fixture in fixtures
            if (manifest_dir / str(fixture["path"])).is_file()
        ),
        None,
    )
    try:
        for provider in providers:
            spec = specs.get(provider)
            model = str(spec.get("model", "")) if spec else ""
            if spec is None and direct is not None:
                preparation_errors[provider] = f"provider is not configured for direct benchmarking: {provider}"
                continue
            if (
                not args.allow_online
                and (
                    args.transport == "magician"
                    or spec is None
                    or provider_is_online(spec)
                )
            ):
                continue
            if direct is not None:
                try:
                    direct.prepare(provider, cold_only=args.cold_only)
                    if not args.cold_only and warmup_fixture is not None:
                        direct.warm_local_provider(provider, warmup_fixture)
                except Exception as error:
                    preparation_errors[provider] = str(error)

        for provider in providers:
            spec = specs.get(provider)
            model = str(spec.get("model", "")) if spec else ""
            for fixture in fixtures:
                run_kinds = ["cold"] if args.cold_only else ["warm"] * args.warm_runs
                audio_path = manifest_dir / str(fixture["path"])
                for run_kind in run_kinds:
                    if (
                        not args.allow_online
                        and (
                            args.transport == "magician"
                            or spec is None
                            or provider_is_online(spec)
                        )
                    ):
                        row = skipped_measurement(
                            provider,
                            model,
                            str(fixture["id"]),
                            run_kind,
                            (
                                "Magician transport can use configured online fallbacks and requires --allow-online"
                                if args.transport == "magician"
                                else "online provider upload requires --allow-online"
                            ),
                        )
                    elif not audio_path.is_file():
                        row = skipped_measurement(
                            provider,
                            model,
                            str(fixture["id"]),
                            run_kind,
                            f"fixture is absent: {audio_path}",
                        )
                    elif provider in preparation_errors:
                        row = failed_measurement(
                            provider,
                            model,
                            str(fixture["id"]),
                            run_kind,
                            preparation_errors[provider],
                        )
                    else:
                        if direct is not None:
                            transcriber = direct.transcribe
                        else:
                            transcriber = lambda provider_id, path: transcribe_via_magician(
                                args.base_url,
                                provider_id,
                                path,
                                args.timeout_secs,
                            )
                        row = run_measurement(
                            manifest_dir,
                            provider,
                            model,
                            fixture,
                            run_kind,
                            transcriber,
                            direct.resource_pid(provider) if direct is not None else None,
                        )
                    measurements.append(row)
                    print(f"{provider} {fixture['id']} {run_kind}: {row['status']}", flush=True)
        if direct is not None and args.idle_observation_secs > 0:
            for provider in providers:
                observation = direct.observe_idle(provider, args.idle_observation_secs)
                if observation is None:
                    continue
                idle_observations.append(observation)
                for row in measurements:
                    if row.get("provider_id") == provider:
                        row["process_rss_after_idle_mb"] = observation["process_rss_mb"]
    finally:
        if direct is not None:
            direct.close()

    report = {
        "schema_version": 1,
        "run_id": run_id,
        "captured_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "host": host_metadata(),
        "git_revision": command_output(["git", "rev-parse", "HEAD"], "unknown"),
        "config_fingerprint": media_config_fingerprint(config),
        "fixture_manifest": relative_or_absolute(args.manifest),
        "notes": [
            f"Transport: {args.transport}.",
            "Direct mode invokes provider executables/APIs without Magician or its supervisor.",
            "Direct FluidAudio warm mode runs one untimed inference after model preparation.",
            "Direct FluidAudio cold mode prepares the cached model and unloads it before timing.",
            "Online audio uploads require the explicit --allow-online flag.",
            "WER/CER are strict text metrics; numeral formatting and cross-script transliteration count as edits.",
        ],
        "idle_observations": idle_observations,
        "measurements": measurements,
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print_summary(measurements)
    print(f"Wrote {output}")
    return 1 if any(row["status"] == "failed" for row in measurements) else 0


if __name__ == "__main__":
    sys.exit(main())
