#!/usr/bin/env python3
"""Inspect and operate Magician-managed local audio models through Magician."""

from __future__ import annotations

import argparse
import json
import sys
import urllib.error
import urllib.parse
import urllib.request
from typing import Any


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Inspect, prewarm, or unload models owned by a Magician audio engine."
    )
    parser.add_argument("action", choices=("status", "prewarm", "unload"))
    parser.add_argument("--base-url", default="http://127.0.0.1:3002")
    parser.add_argument("--engine", default="fluid_audio")
    parser.add_argument("--model", action="append", default=[])
    parser.add_argument("--json", action="store_true", dest="as_json")
    parser.add_argument("--timeout-secs", type=float, default=600)
    args = parser.parse_args()
    if args.timeout_secs <= 0:
        parser.error("--timeout-secs must be positive")
    return args


def request_json(request: urllib.request.Request, timeout: float) -> dict[str, Any]:
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            payload = response.read().decode("utf-8", errors="replace")
            return json.loads(payload) if payload else {}
    except urllib.error.HTTPError as error:
        payload = error.read().decode("utf-8", errors="replace")
        try:
            detail = json.loads(payload)
        except json.JSONDecodeError:
            detail = {"message": payload or str(error)}
        raise RuntimeError(f"audio engine request failed ({error.code}): {detail}") from error
    except urllib.error.URLError as error:
        raise RuntimeError(f"Magician is unavailable: {error.reason}") from error


def status(args: argparse.Namespace) -> dict[str, Any]:
    request = urllib.request.Request(
        f"{args.base_url.rstrip('/')}/api/magician/v2/media/audio-settings",
        method="GET",
    )
    settings = request_json(request, args.timeout_secs)
    engine = settings.get("engines", {}).get(args.engine)
    if not isinstance(engine, dict):
        raise RuntimeError(f"audio engine is not configured: {args.engine}")
    models = [
        model
        for model in settings.get("models", {}).values()
        if isinstance(model, dict) and model.get("engine_id") == args.engine
    ]
    return {"engine": engine, "models": sorted(models, key=lambda row: str(row.get("provider_id")))}


def operate(args: argparse.Namespace) -> dict[str, Any]:
    engine = urllib.parse.quote(args.engine, safe="")
    body = json.dumps({"model_ids": args.model}).encode("utf-8")
    request = urllib.request.Request(
        f"{args.base_url.rstrip('/')}/api/magician/v2/media/audio-engines/{engine}/models/{args.action}",
        data=body,
        method="POST",
        headers={"Content-Type": "application/json"},
    )
    return request_json(request, args.timeout_secs)


def print_status(payload: dict[str, Any]) -> None:
    engine = payload["engine"]
    health = "healthy" if engine.get("healthy") else ("idle" if engine.get("healthy") is None else "degraded")
    print(
        f"{engine.get('label', engine.get('engine_id'))}: {health}; "
        f"resident={engine.get('resident_models', 0)}/{engine.get('max_resident_models', '-')}; "
        f"sessions={engine.get('active_sessions', 0)}; pid={engine.get('process_id') or '-'}"
    )
    for model in payload["models"]:
        print(
            f"  {model.get('provider_id')}: {model.get('state')} "
            f"resident={str(bool(model.get('resident'))).lower()} "
            f"sessions={model.get('active_sessions', 0)}"
        )


def main() -> int:
    args = parse_args()
    try:
        payload = status(args) if args.action == "status" else operate(args)
    except RuntimeError as error:
        print(str(error), file=sys.stderr)
        return 1
    if args.as_json or args.action != "status":
        print(json.dumps(payload, indent=2, sort_keys=True))
    else:
        print_status(payload)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
