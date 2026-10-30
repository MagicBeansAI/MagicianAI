"""Typed model/thinking choices for public-chat evaluation drivers."""
from __future__ import annotations

import argparse
from collections.abc import Callable, Sequence
from typing import Any


def add_chat_model_arguments(parser: argparse.ArgumentParser, stages: Sequence[str]) -> None:
    for stage in stages:
        parser.add_argument(f"--{stage}-model", default="", help=f"catalog model ID for {stage} turns (default: server default)")
        parser.add_argument(f"--{stage}-thinking", default="auto", help=f"native thinking level for --{stage}-model (default: auto)")
        parser.set_defaults(**{f"{stage}_model_selection": None})


def resolve_chat_model_selections(
    args: argparse.Namespace, stages: Sequence[str], fetch_catalog: Callable[[], dict[str, Any]]
) -> None:
    for stage in stages:
        if not getattr(args, f"{stage}_model").strip() and getattr(args, f"{stage}_thinking") != "auto":
            raise ValueError(f"--{stage}-thinking requires --{stage}-model")
    catalog = fetch_catalog() if any(getattr(args, f"{stage}_model").strip() for stage in stages) else {}
    models = {model["id"]: model for model in catalog.get("models", [])}
    for stage in stages:
        model_id = getattr(args, f"{stage}_model").strip()
        thinking = getattr(args, f"{stage}_thinking")
        selection = None
        if model_id:
            model = models.get(model_id)
            if model is None or thinking not in model.get("choices", {}):
                raise ValueError(f"Unavailable {stage} model/thinking choice: {model_id!r}, {thinking!r} (see GET /chat/models?engine=magician)")
            selection = {"model_id": model_id, "thinking": thinking}
        setattr(args, f"{stage}_model_selection", selection)


def chat_model_request_fields(selection: dict[str, str] | None) -> dict[str, Any]:
    return {"harness_engine": "magician", "model_selection": selection} if selection else {}
