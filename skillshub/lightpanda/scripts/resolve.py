#!/usr/bin/env python3
"""Resolve a locally installed Lightpanda binary for agent-browser.

This resolver never starts Lightpanda's own Agent/LLM mode. It selects the
agent-browser CDP engine and supplies the browser executable path.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path


def _die(message: str) -> None:
    sys.stderr.write(f"lightpanda: {message}\n")
    raise SystemExit(1)


def _parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Resolve Lightpanda config for Magician's agent-browser runtime."
    )
    parser.add_argument(
        "--headed",
        action="store_true",
        help="rejected because Lightpanda is headless-only",
    )
    return parser.parse_args(argv)


def _candidate_paths() -> list[Path]:
    values: list[str] = []
    configured = os.environ.get("LIGHTPANDA_EXECUTABLE_PATH", "").strip()
    if configured:
        values.append(configured)
    discovered = shutil.which("lightpanda")
    if discovered:
        values.append(discovered)
    skill_local = Path(__file__).resolve().parents[1] / "bin" / "lightpanda"
    values.extend(
        [
            str(skill_local),
            "/opt/homebrew/bin/lightpanda",
            "/usr/local/bin/lightpanda",
            "/usr/bin/lightpanda",
        ]
    )
    seen: set[str] = set()
    candidates: list[Path] = []
    for raw in values:
        path = Path(raw).expanduser().resolve()
        key = str(path)
        if key not in seen:
            seen.add(key)
            candidates.append(path)
    return candidates


def _resolve_binary() -> tuple[Path, str]:
    for candidate in _candidate_paths():
        if not candidate.is_file() or not os.access(candidate, os.X_OK):
            continue
        try:
            result = subprocess.run(
                [str(candidate), "version"],
                capture_output=True,
                text=True,
                timeout=10,
                check=False,
            )
        except (OSError, subprocess.SubprocessError):
            continue
        if result.returncode != 0:
            continue
        version = (result.stdout or result.stderr).strip() or "unknown"
        return candidate, version
    _die(
        "executable not found. Run `make -C skillshub setup-lightpanda` or set "
        "LIGHTPANDA_EXECUTABLE_PATH."
    )


def main(argv: list[str]) -> int:
    args = _parse_args(argv)
    if args.headed:
        _die(
            "headed mode is unsupported because Lightpanda has no graphical renderer; "
            "use cloak-browser, bundled Chrome, or CDP"
        )
    binary, version = _resolve_binary()
    json.dump(
        {
            "binary_path": str(binary),
            "version": version,
            "headed": False,
            "args": [],
            "env": {
                "AGENT_BROWSER_ENGINE": "lightpanda",
                "LIGHTPANDA_DISABLE_TELEMETRY": "true",
                "LIGHTPANDA_DISABLE_CORE_DUMP": "1",
            },
        },
        sys.stdout,
        indent=2,
    )
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
