#!/usr/bin/env python3
"""Live eval for the MiniMax CLI (mmx-cli) skill wrappers.

Codifies the manual per-skill smoke test into a repeatable, report-producing
contract eval. It exercises the five `*-via-minimax` skill wrappers against the
real MiniMax API through the bundled `mmx` binary and asserts:

  - the `mmx` CLI resolves and is at least the region-flag-aware line (>=1.0.17),
  - every wrapper passes `--region` explicitly (mmx 1.0.17+ auto-detects the key
    region and fails inconclusively under --non-interactive, so this flag is
    load-bearing — a static guard keeps it from silently regressing),
  - web-search and vision return the exact envelopes the toolchain parses,
  - image / music / video accept our flag set + region (dry-run by default so
    the eval never spends real generation quota; pass --real-media for a real
    single-image generation).

Cost: web-search + vision make one cheap real call each; generative skills are
dry-run unless --real-media is set. Requires `MINIMAX_API_KEY` in the env
(same secret the skills read); region defaults to `global` (override with
`MINIMAX_REGION` — a MiniMax-CN key needs `cn`).

Modes:
  --self-test   provider-free: runs the pure validators over canned CLI output
                and the static wrapper guard. No network, no key. (CI-safe.)
  --dry-run     print the plan only.
  (default)     live: real cheap calls + generative dry-runs.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass, asdict
from html import escape
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import zlib
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[1]
SKILLSHUB = REPO_ROOT / "skillshub"
MIN_CLI_VERSION = (1, 0, 17)  # region auto-detect (and the --region flag) land here

WRAPPERS = {
    "web-search": SKILLSHUB / "web-search-via-minimax/scripts/minimax_websearch.py",
    "vision": SKILLSHUB / "analyze-image-via-minimax/scripts/minimax_vision.py",
    "image": SKILLSHUB / "image-generation-via-minimax/scripts/minimax_image.py",
    "music": SKILLSHUB / "music-generation-via-minimax/scripts/minimax_music.py",
    "video": SKILLSHUB / "video-generation-via-minimax/scripts/minimax_video.py",
}


@dataclass(frozen=True)
class Gate:
    name: str
    passed: bool
    detail: str
    latency_ms: float = 0.0


class EvalFailure(RuntimeError):
    pass


# ── Pure validators (shared by live mode and --self-test) ───────────────────

def parse_version(text: str) -> tuple[int, int, int] | None:
    for token in (text or "").replace("mmx", "").split():
        parts = token.strip().lstrip("v").split(".")
        if len(parts) >= 3 and all(p.isdigit() for p in parts[:3]):
            return tuple(int(p) for p in parts[:3])  # type: ignore[return-value]
    return None


def validate_version(version_text: str, latency_ms: float = 0.0) -> Gate:
    parsed = parse_version(version_text)
    ok = parsed is not None and parsed >= MIN_CLI_VERSION
    want = ".".join(str(n) for n in MIN_CLI_VERSION)
    return Gate("cli_version", ok, f"mmx={version_text.strip() or 'missing'} (>= {want})", latency_ms)


def validate_wrappers_pass_region() -> Gate:
    """Static guard: mmx-cli 1.0.17+ needs --region; every wrapper must pass it."""
    missing = [
        name
        for name, path in WRAPPERS.items()
        if not path.exists() or '"--region"' not in path.read_text(encoding="utf-8")
    ]
    return Gate(
        "wrappers_pass_region",
        not missing,
        "all 5 wrappers pass --region" if not missing else f"missing --region in: {missing}",
    )


def validate_search(stdout: str, latency_ms: float = 0.0) -> Gate:
    try:
        payload = json.loads(stdout)
    except json.JSONDecodeError as error:
        return Gate("web_search_live", False, f"non-JSON output: {error}", latency_ms)
    results = payload.get("results")
    ok = (
        isinstance(results, list)
        and len(results) > 0
        and isinstance(results[0], dict)
        and bool(results[0].get("title"))
        and bool(results[0].get("url"))
    )
    n = len(results) if isinstance(results, list) else 0
    return Gate("web_search_live", ok, f"results={n}, first has title+url={ok}", latency_ms)


def validate_vision(stdout: str, latency_ms: float = 0.0) -> Gate:
    try:
        payload = json.loads(stdout)
    except json.JSONDecodeError as error:
        return Gate("vision_describe_live", False, f"non-JSON output: {error}", latency_ms)
    desc = payload.get("description")
    ok = isinstance(desc, str) and len(desc.strip()) > 0 and "error" not in payload
    return Gate("vision_describe_live", ok, f"description_len={len(desc or '')}", latency_ms)


def validate_dry_run(name: str, stdout: str, expect_model: str | None, latency_ms: float = 0.0) -> Gate:
    try:
        payload = json.loads(stdout)
    except json.JSONDecodeError as error:
        return Gate(name, False, f"non-JSON output: {error}", latency_ms)
    if isinstance(payload.get("error"), dict):
        # A region-detection failure surfaces here; that is exactly the regression.
        return Gate(name, False, f"cli error: {payload['error'].get('message')}", latency_ms)
    request = payload.get("request", payload)
    model = request.get("model") if isinstance(request, dict) else None
    ok = isinstance(request, dict) and bool(request)
    if expect_model is not None:
        ok = ok and model == expect_model
    return Gate(name, ok, f"model={model} (region accepted, plan returned)", latency_ms)


def validate_image(stdout: str, real: bool, latency_ms: float = 0.0) -> Gate:
    try:
        payload = json.loads(stdout)
    except json.JSONDecodeError as error:
        return Gate("image_generate", False, f"non-JSON output: {error}", latency_ms)
    if real:
        images = payload.get("images")
        ok = isinstance(images, list) and len(images) > 0 and bool(images[0].get("path"))
        return Gate("image_generate", ok, f"real gen, images={len(images or [])}, model={payload.get('model')}", latency_ms)
    return validate_dry_run("image_generate", stdout, "image-01", latency_ms)


# ── Live execution ──────────────────────────────────────────────────────────

def resolve_mmx() -> str:
    for candidate in (os.environ.get("MMX_BIN"), str(SKILLSHUB / "node_modules/.bin/mmx"), shutil.which("mmx")):
        if candidate and Path(candidate).exists():
            return candidate
    raise EvalFailure(
        "mmx binary not found — run `make setup-skillshub-deps` (or npm install in skillshub/)."
    )


def make_test_png(path: Path) -> None:
    w = h = 48
    rows = [b"".join(b"\xd0\x20\x20" if x < w // 2 else b"\x20\x40\xd0" for x in range(w)) for _ in range(h)]

    def chunk(typ: bytes, data: bytes) -> bytes:
        c = typ + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c) & 0xFFFFFFFF)

    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(b"".join(b"\x00" + r for r in rows)))
        + chunk(b"IEND", b"")
    )
    path.write_bytes(png)


def run(argv: list[str], env: dict[str, str], timeout: float) -> tuple[int, str, str]:
    completed = subprocess.run(argv, capture_output=True, text=True, timeout=timeout, env=env)
    return completed.returncode, completed.stdout, completed.stderr


def run_live(api_key: str, region: str, mmx: str, real_media: bool, timeout: float) -> list[Gate]:
    base_env = dict(os.environ)
    base_env["MINIMAX_API_KEY"] = api_key
    base_env["MINIMAX_REGION"] = region
    base_env["PATH"] = f"{Path(mmx).parent}{os.pathsep}{base_env.get('PATH', '')}"
    gates: list[Gate] = []

    # 1. CLI version
    started = time.perf_counter()
    _, ver_out, _ = run([mmx, "--version"], base_env, timeout=20)
    gates.append(validate_version(ver_out, (time.perf_counter() - started) * 1000))

    # 2. Static region guard (no network)
    gates.append(validate_wrappers_pass_region())

    # 3. web-search wrapper (real)
    started = time.perf_counter()
    env = {**base_env, "_TOOL_QUERY": "MiniMax M2 model", "_TOOL_MAX_RESULTS": "3", "_TOOL_TIMEOUT_SECS": "30"}
    _, out, err = run([sys.executable, str(WRAPPERS["web-search"])], env, timeout + 40)
    gates.append(validate_search(out or err, (time.perf_counter() - started) * 1000))

    # 4. vision wrapper (real) on a generated test image
    with tempfile.TemporaryDirectory() as tmp:
        img = Path(tmp) / "probe.png"
        make_test_png(img)
        started = time.perf_counter()
        env = {**base_env, "_TOOL_IMAGE_REF": str(img), "_TOOL_QUESTION": "What colors and layout? One sentence.", "_TOOL_API_TIMEOUT_SECS": "60"}
        _, out, err = run([sys.executable, str(WRAPPERS["vision"])], env, timeout + 70)
        gates.append(validate_vision(out or err, (time.perf_counter() - started) * 1000))

    # 5. image — real single generation if --real-media, else dry-run
    started = time.perf_counter()
    if real_media:
        with tempfile.TemporaryDirectory() as tmp:
            env = {**base_env, "_TOOL_PROMPT": "a small red circle on white, minimalist", "_TOOL_OUTPUT_PATH": str(Path(tmp) / "img.png"), "_TOOL_N": "1", "_TOOL_API_TIMEOUT_SECS": "150"}
            _, out, err = run([sys.executable, str(WRAPPERS["image"])], env, 190)
            gates.append(validate_image(out or err, real=True, latency_ms=(time.perf_counter() - started) * 1000))
    else:
        _, out, _ = run([mmx, "image", "generate", "--dry-run", "--region", region, "--api-key", api_key,
                         "--output", "json", "--quiet", "--non-interactive", "--prompt", "a small red circle"], base_env, timeout)
        gates.append(validate_image(out, real=False, latency_ms=(time.perf_counter() - started) * 1000))

    # 6. music — dry-run (region + flags)
    started = time.perf_counter()
    _, out, _ = run([mmx, "music", "generate", "--dry-run", "--region", region, "--api-key", api_key,
                     "--output", "json", "--quiet", "--non-interactive", "--prompt", "upbeat acoustic",
                     "--instrumental", "--model", "music-2.6"], base_env, timeout)
    gates.append(validate_dry_run("music_generate_dry_run", out, "music-2.6", (time.perf_counter() - started) * 1000))

    # 7. video — dry-run (region + flags)
    started = time.perf_counter()
    _, out, _ = run([mmx, "video", "generate", "--dry-run", "--region", region, "--api-key", api_key,
                     "--output", "json", "--quiet", "--non-interactive", "--prompt", "a cat walking"], base_env, timeout)
    gates.append(validate_dry_run("video_generate_dry_run", out, None, (time.perf_counter() - started) * 1000))
    return gates


def synthetic_gates() -> list[Gate]:
    """Provider-free: exercise every validator over canned CLI output + real guard."""
    return [
        validate_version("mmx 1.0.18"),
        validate_wrappers_pass_region(),  # reads real wrapper files; no network
        validate_search(json.dumps({"results": [{"title": "MiniMax", "url": "https://x", "snippet": "s"}]})),
        validate_vision(json.dumps({"description": "red left, blue right", "voice_summary": "..."})),
        validate_image(json.dumps({"request": {"model": "image-01", "prompt": "p"}}), real=False),
        validate_dry_run("music_generate_dry_run", json.dumps({"request": {"model": "music-2.6"}}), "music-2.6"),
        validate_dry_run("video_generate_dry_run", json.dumps({"request": {"model": "MiniMax-Hailuo-2.3"}}), None),
    ]


def write_report(output_dir: Path, gates: list[Gate], mode: str) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    payload = {
        "schema_version": 1,
        "eval": "minimax-cli",
        "mode": mode,
        "passed": all(gate.passed for gate in gates),
        "gate_count": len(gates),
        "gates": [asdict(gate) for gate in gates],
    }
    (output_dir / "report.json").write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    rows = "".join(
        "<tr><td>{}</td><td class='{}'>{}</td><td>{}</td><td>{:.1f} ms</td></tr>".format(
            escape(gate.name),
            "pass" if gate.passed else "fail",
            "PASS" if gate.passed else "FAIL",
            escape(gate.detail),
            gate.latency_ms,
        )
        for gate in gates
    )
    html = f"""<!doctype html><html><head><meta charset="utf-8"><title>MiniMax CLI live eval</title>
<style>body{{font:15px system-ui;margin:2rem;color:#17202a}}table{{border-collapse:collapse;width:100%}}th,td{{border-bottom:1px solid #ddd;padding:.65rem;text-align:left}}.pass{{color:#16803b;font-weight:700}}.fail{{color:#c0392b;font-weight:700}}</style></head>
<body><h1>MiniMax CLI (mmx-cli) live eval</h1><p>Mode: {escape(mode)} · {len(gates)} contract gates</p><table><thead><tr><th>Gate</th><th>Result</th><th>Evidence</th><th>Latency</th></tr></thead><tbody>{rows}</tbody></table></body></html>"""
    (output_dir / "report.html").write_text(html, encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--timeout-secs", type=float, default=45.0)
    parser.add_argument("--real-media", action="store_true", help="do a real single-image generation (spends quota)")
    parser.add_argument("--self-test", action="store_true", help="provider-free validator + wrapper guard; no network")
    parser.add_argument("--dry-run", action="store_true", help="print the plan only")
    args = parser.parse_args()

    if args.self_test:
        gates, mode = synthetic_gates(), "self-test"
    elif args.dry_run:
        gates, mode = [Gate("request_plan", True, "version + region guard + 5 skill contracts (search/vision real, media dry-run)")], "dry-run"
    else:
        mode = "live"
        api_key = os.environ.get("MINIMAX_API_KEY", "").strip()
        region = os.environ.get("MINIMAX_REGION", "").strip() or "global"
        if not api_key:
            gates = [Gate("live_contract", False, "MINIMAX_API_KEY not set in env")]
        else:
            try:
                gates = run_live(api_key, region, resolve_mmx(), args.real_media, args.timeout_secs)
            except (EvalFailure, subprocess.TimeoutExpired) as error:
                gates = [Gate("live_contract", False, str(error))]

    if args.output_dir:
        write_report(args.output_dir, gates, mode)
        print(f"Report: {(args.output_dir.resolve() / 'report.html').as_uri()}")
    for gate in gates:
        print(f"{'PASS' if gate.passed else 'FAIL'} {gate.name}: {gate.detail}")
    return 0 if all(gate.passed for gate in gates) else 1


if __name__ == "__main__":
    sys.exit(main())
