#!/usr/bin/env python3
"""Verify exact Ollama model/context residency from an ``/api/ps`` response."""

from __future__ import annotations

import json
import os
import sys


DEFAULT_HOST = "registry.ollama.ai"
DEFAULT_NAMESPACE = "library"
DEFAULT_TAG = "latest"


def canonical_model_name(value: str) -> str:
    """Apply Ollama's host, namespace, and tag defaults to one model name."""

    if not isinstance(value, str) or not value or value.strip() != value:
        raise ValueError("model name must be a non-empty trimmed string")
    if "@" in value:
        raise ValueError("digest-qualified model names are not supported by /api/ps residency")

    without_scheme = value
    if "://" in without_scheme:
        scheme, without_scheme = without_scheme.split("://", 1)
        if scheme not in {"http", "https"}:
            raise ValueError(f"unsupported model registry scheme: {scheme}")

    last_slash = without_scheme.rfind("/")
    last_colon = without_scheme.rfind(":")
    if last_colon > last_slash:
        base, tag = without_scheme[:last_colon], without_scheme[last_colon + 1 :]
    else:
        base, tag = without_scheme, DEFAULT_TAG
    if not tag:
        raise ValueError("model tag must not be empty")

    parts = base.split("/")
    if len(parts) == 1:
        host, namespace, model = DEFAULT_HOST, DEFAULT_NAMESPACE, parts[0]
    elif len(parts) == 2:
        host, namespace, model = DEFAULT_HOST, parts[0], parts[1]
    elif len(parts) == 3:
        host, namespace, model = parts
    else:
        raise ValueError("model name must contain model, namespace/model, or host/namespace/model")
    if not host or not namespace or not model:
        raise ValueError("model host, namespace, model, and tag must not be empty")

    # Ollama compares parsed name components case-insensitively.
    return f"{host}/{namespace}/{model}:{tag}".casefold()


def parse_expected_models(value: str) -> list[tuple[str, int]]:
    expected: list[tuple[str, int]] = []
    for line in value.splitlines():
        if not line:
            continue
        try:
            name, context = line.split("\t", 1)
            parsed_context = int(context)
        except (ValueError, TypeError) as exc:
            raise ValueError("expected models must be MODEL<TAB>CONTEXT lines") from exc
        if parsed_context <= 0:
            raise ValueError("expected model context must be positive")
        canonical_model_name(name)
        expected.append((name, parsed_context))
    if not expected:
        raise ValueError("at least one expected model is required")
    return expected


def verify_residency(process_state: str, expected_models: str) -> list[str]:
    try:
        state = json.loads(process_state)
    except Exception as exc:
        return [f"invalid /api/ps response: {exc}"]
    if not isinstance(state, dict) or not isinstance(state.get("models"), list):
        return ["invalid /api/ps response: models must be an array"]

    try:
        expected = parse_expected_models(expected_models)
    except ValueError as exc:
        return [f"invalid expected model contract: {exc}"]

    loaded: dict[str, list[object]] = {}
    for item in state["models"]:
        if not isinstance(item, dict):
            continue
        name = item.get("model") or item.get("name")
        if not isinstance(name, str):
            continue
        try:
            canonical = canonical_model_name(name)
        except ValueError:
            continue
        loaded.setdefault(canonical, []).append(item.get("context_length"))

    problems: list[str] = []
    for display_name, context in expected:
        canonical = canonical_model_name(display_name)
        contexts = loaded.get(canonical)
        if not contexts:
            problems.append(f"{display_name} is not resident")
        elif context not in contexts:
            rendered = ", ".join(str(value) for value in contexts)
            problems.append(
                f"{display_name} context is {rendered}, expected {context}"
            )
    return problems


def main() -> int:
    problems = verify_residency(
        os.environ.get("PROCESS_STATE", ""),
        os.environ.get("EXPECTED_MODELS_TSV", ""),
    )
    if problems:
        print("; ".join(problems))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
