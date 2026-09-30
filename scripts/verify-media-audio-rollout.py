#!/usr/bin/env python3
"""Fail-closed packaging/config audit for the local audio rollout."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

import yaml
import pathlib

# The router's profiles and operation_mapping live in a sibling
# `llm-router.yaml`; reading the config file alone yields neither.
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from magician_config_text import read_config_text  # noqa: E402



REPO_ROOT = Path(__file__).resolve().parent.parent
REPO_CONFIG = REPO_ROOT / "magician-config.yaml"
LIVE_CONFIG = Path.home() / "MagicianNotes/magician-config.yaml"
PACKAGE_RESOLVED = REPO_ROOT / "native/macos-audio-engine/Package.resolved"
TAURI_AUDIO_CONFIG = REPO_ROOT / "desktop/src-tauri/tauri.macos-audio.conf.json"
STAGED_SIDECAR = REPO_ROOT / "magician-macos-audio-engine.bin"
ROLLOUT_MANIFEST = REPO_ROOT / "data/magician_v2/media_evals/phase9-rollout-manifest.json"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Verify audio rollout config and package boundaries.")
    parser.add_argument("--require-staged-sidecar", action="store_true")
    parser.add_argument("--release-app", type=Path, help="optional .app bundle whose resources must contain the sidecar")
    parser.add_argument("--skip-live-config", action="store_true")
    return parser.parse_args()


def read_yaml(path: Path) -> dict[str, Any]:
    # Config files need their sibling router tables; anything else reads plain.
    text = read_config_text(path) if path.name.endswith("config.yaml") or path.name == "magician-config.yaml" else path.read_text(encoding="utf-8")
    payload = yaml.safe_load(text)
    if not isinstance(payload, dict):
        raise ValueError(f"YAML root is not a mapping: {path}")
    return payload


def check(condition: bool, label: str, failures: list[str], checks: list[str]) -> None:
    if condition:
        checks.append(label)
    else:
        failures.append(label)


def configured_fluid_ids(media: dict[str, Any]) -> list[str]:
    ids: list[str] = []
    for stage in ("vad", "recording_stt", "streaming_stt", "diarization", "tts"):
        for provider in media.get(stage, {}).get("providers", []):
            if not isinstance(provider, dict):
                continue
            if provider.get("engine_id") == "fluid_audio" or str(provider.get("adapter", "")).startswith("fluid_audio"):
                ids.append(str(provider.get("id", "")))
    return ids


def verify_rollout_decisions(
    media: dict[str, Any], failures: list[str], checks: list[str]
) -> None:
    manifest = json.loads(ROLLOUT_MANIFEST.read_text(encoding="utf-8"))
    decisions = manifest.get("surface_decisions", {})
    profiles = media.get("surface_profiles", {})
    defaults = profiles.get("default_mapping", {})
    configured_profiles = profiles.get("profiles", {})

    check(manifest.get("schema_version") == 1, "Phase 9 rollout manifest schema is supported", failures, checks)
    for surface in ("dictation", "meeting", "listening", "hands_free"):
        decision = decisions.get(surface, {})
        expected_profile = decision.get("configured_profile")
        check(
            defaults.get(surface) == expected_profile,
            f"{surface} default matches the Phase 9 rollout decision",
            failures,
            checks,
        )
        if expected_profile is None:
            continue
        profile = configured_profiles.get(expected_profile, {})
        check(
            profile.get("surface") == surface,
            f"{surface} rollout profile exists and owns the correct surface",
            failures,
            checks,
        )
        for stage, expected_order in decision.get("stage_orders", {}).items():
            actual_order = profile.get(stage, {}).get("providers", [])
            check(
                actual_order == expected_order,
                f"{surface} {stage} order matches the Phase 9 rollout decision",
                failures,
                checks,
            )


def main() -> int:
    args = parse_args()
    failures: list[str] = []
    checks: list[str] = []

    repo = read_yaml(REPO_CONFIG)
    repo_media = repo.get("media")
    check(isinstance(repo_media, dict), "repository media config exists", failures, checks)
    if not args.skip_live_config:
        check(LIVE_CONFIG.is_file(), "live-runtime config exists", failures, checks)
        if LIVE_CONFIG.is_file():
            live_media = read_yaml(LIVE_CONFIG).get("media")
            check(repo_media == live_media, "repository and live-runtime media config match", failures, checks)

    media = repo_media if isinstance(repo_media, dict) else {}
    fluid = media.get("engines", {}).get("fluid_audio", {})
    fluid_ids = configured_fluid_ids(media)
    prewarm = fluid.get("prewarm", []) if isinstance(fluid, dict) else []
    check(bool(fluid.get("enabled")), "FluidAudio engine is explicitly enabled", failures, checks)
    check(fluid.get("startup") in {"lazy", "external", "disabled"}, "FluidAudio startup policy is explicit", failures, checks)
    check(fluid.get("download_policy") in {"disabled", "on_demand", "prewarm"}, "FluidAudio download policy is explicit", failures, checks)
    check(len(fluid_ids) == len(set(fluid_ids)) and all(fluid_ids), "FluidAudio model ids are unique", failures, checks)
    check(set(prewarm).issubset(fluid_ids), "FluidAudio prewarm ids exist in the provider catalog", failures, checks)
    check(
        any(provider.get("enabled", True) and not str(provider.get("adapter", "")).startswith("fluid_audio")
            for provider in media.get("recording_stt", {}).get("providers", []) if isinstance(provider, dict)),
        "container-safe non-FluidAudio recording STT fallback exists",
        failures,
        checks,
    )
    verify_rollout_decisions(media, failures, checks)

    resolved = json.loads(PACKAGE_RESOLVED.read_text(encoding="utf-8"))
    fluid_pin = next((pin for pin in resolved.get("pins", []) if pin.get("identity") == "fluidaudio"), None)
    check(
        isinstance(fluid_pin, dict) and fluid_pin.get("state", {}).get("version") == "0.12.4",
        "FluidAudio Swift dependency is pinned to 0.12.4",
        failures,
        checks,
    )
    tauri = json.loads(TAURI_AUDIO_CONFIG.read_text(encoding="utf-8"))
    resources = tauri.get("bundle", {}).get("resources", {})
    check(
        resources.get("../../magician-macos-audio-engine.bin") == "magician-macos-audio-engine.bin",
        "macOS desktop overlay packages the staged sidecar",
        failures,
        checks,
    )
    dockerfile = (REPO_ROOT / "Dockerfile").read_text(encoding="utf-8")
    check(
        "magician-macos-audio-engine" not in dockerfile,
        "container image does not package the macOS sidecar",
        failures,
        checks,
    )
    if args.require_staged_sidecar:
        check(STAGED_SIDECAR.is_file(), "development sidecar is staged", failures, checks)
    if args.release_app:
        bundled = args.release_app / "Contents/Resources/magician-macos-audio-engine.bin"
        check(bundled.is_file(), "release app contains the sidecar resource", failures, checks)

    report = {"status": "passed" if not failures else "failed", "checks": checks, "failures": failures}
    print(json.dumps(report, indent=2))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
