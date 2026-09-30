"""Curated architecture model (docs/architecture/architecture.yaml) support.

Loads the human-owned C4 model, validates it against the generated code
graph, and resolves code references so the c4 index generator can merge
curated intent with derived code truth.

The model is the only place architecture *intent* lives (what supervises
what, which transport a connection uses, when it fires). The validator is
what keeps that intent honest: dangling references fail the build instead
of silently rotting.
"""

from __future__ import annotations

import copy
import re
from pathlib import Path
from typing import Any

import yaml

NODE_ID_RE = re.compile(r"^(runtime|area|external|actor):[a-z0-9][a-z0-9-]*$")

REQUIRED_TOP_LEVEL = ("version", "system", "nodes", "edges", "externals", "actors")


class ModelError(Exception):
    """Raised when the model file cannot be parsed into a valid shape."""


def load_model(path: Path | str) -> dict[str, Any]:
    """Load and shape-check architecture.yaml. Raises ModelError on bad shape."""
    p = Path(path)
    try:
        raw = yaml.safe_load(p.read_text(encoding="utf-8"))
    except yaml.YAMLError as exc:
        raise ModelError(f"invalid YAML in {p}: {exc}") from exc
    if not isinstance(raw, dict):
        raise ModelError(f"{p}: top level must be a mapping")
    for key in REQUIRED_TOP_LEVEL:
        if key not in raw:
            raise ModelError(f"{p}: missing required key '{key}'")
    system = raw["system"]
    if not isinstance(system, dict) or not system.get("label") or not system.get("summary"):
        raise ModelError(f"{p}: system needs label and summary")
    for key in ("nodes", "edges", "externals", "actors"):
        if not isinstance(raw[key], list):
            raise ModelError(f"{p}: '{key}' must be a list")
    return raw


def _known_ids(model: dict[str, Any]) -> set[str]:
    ids: set[str] = set()
    for node in model.get("nodes", []):
        ids.add(node.get("id", ""))
    for ext in model.get("externals", []):
        ids.add(ext.get("id", ""))
    for actor in model.get("actors", []):
        ids.add(actor.get("id", ""))
    ids.discard("")
    return ids


def _doc_exists(repo_root: Path, doc: str) -> bool:
    return bool(doc) and (repo_root / doc).is_file()


def _parse_endpoint_ref(ref: str) -> tuple[str, str] | None:
    """Split 'POST /v2/threads' into ('POST', '/v2/threads'); bare '/x' -> ('', '/x')."""
    text = str(ref).strip()
    if not text.startswith("/"):
        parts = text.split(None, 1)
        if len(parts) == 2 and parts[1].startswith("/"):
            return parts[0].upper(), parts[1].rstrip("/")
    if text.startswith("/"):
        return "", text.rstrip("/")
    return None


def _endpoint_id_parts(endpoint_id: str) -> tuple[str, str] | None:
    """Extract (method, route) from 'endpoint::<METHOD>::<route>::<rel>::<line>:<idx>'."""
    parts = str(endpoint_id).split("::")
    if len(parts) < 3 or parts[0] != "endpoint":
        return None
    return parts[1].upper(), parts[2]


def endpoint_matches(endpoint_id: str, ref: str) -> bool:
    parsed = _parse_endpoint_ref(ref)
    endpoint = _endpoint_id_parts(endpoint_id)
    if parsed is None or endpoint is None:
        return False
    want_method, want_route = parsed
    method, route = endpoint
    if want_method and method != want_method:
        return False
    return route == want_route or route.startswith(want_route + "/")


def validate_model(model: dict[str, Any], graph: dict[str, Any], repo_root: Path) -> list[str]:
    """Return human-readable validation errors (empty list = valid)."""
    errors: list[str] = []

    graph_ids = {node.get("id") for node in graph.get("nodes", [])}
    known = _known_ids(model)
    seen: set[str] = set()

    system_doc = str(model.get("system", {}).get("doc", ""))
    if not _doc_exists(repo_root, system_doc):
        errors.append(f"system doc not found: {system_doc!r}")

    for node in model.get("nodes", []):
        node_id = str(node.get("id", ""))
        if not NODE_ID_RE.match(node_id):
            errors.append(f"node id {node_id!r}: invalid namespace (use runtime:/area:/external:/actor:)")
            continue
        if node_id in seen:
            errors.append(f"duplicate node id: {node_id}")
        seen.add(node_id)

        if not str(node.get("label", "")).strip():
            errors.append(f"{node_id}: missing label")
        if not str(node.get("summary", "")).strip():
            errors.append(f"{node_id}: missing summary")

        doc = str(node.get("doc", ""))
        if not _doc_exists(repo_root, doc):
            errors.append(f"{node_id}: doc not found: {doc!r}")

        if node_id.startswith("area:"):
            parent = str(node.get("parent", ""))
            if not parent:
                errors.append(f"{node_id}: area nodes require a parent")
            elif parent not in known:
                errors.append(f"{node_id}: unknown parent {parent!r}")

        if not node_id.startswith(("external:", "actor:")):
            refs = node.get("code_refs") or []
            if not refs and not node.get("ungrounded"):
                errors.append(f"{node_id}: needs at least one code_refs entry (or explicit ungrounded: true)")
            for ref in refs:
                if ref not in graph_ids:
                    errors.append(f"{node_id}: code_ref not in graph: {ref!r}")

    for ext in model.get("externals", []):
        ext_id = str(ext.get("id", ""))
        if not NODE_ID_RE.match(ext_id) or not ext_id.startswith("external:"):
            errors.append(f"external id {ext_id!r}: must be 'external:<slug>'")
            continue
        if ext_id in seen:
            errors.append(f"duplicate node id: {ext_id}")
        seen.add(ext_id)
        if not str(ext.get("label", "")).strip() or not str(ext.get("summary", "")).strip():
            errors.append(f"{ext_id}: externals need label and summary")
        for toucher in ext.get("touched_by", []) or []:
            if toucher not in known:
                errors.append(f"{ext_id}: touched_by references unknown node {toucher!r}")

    for actor in model.get("actors", []):
        actor_id = str(actor.get("id", ""))
        if not NODE_ID_RE.match(actor_id) or not actor_id.startswith("actor:"):
            errors.append(f"actor id {actor_id!r}: must be 'actor:<slug>'")
            continue
        if actor_id in seen:
            errors.append(f"duplicate node id: {actor_id}")
        seen.add(actor_id)
        if not str(actor.get("label", "")).strip() or not str(actor.get("summary", "")).strip():
            errors.append(f"{actor_id}: actors need label and summary")

    for edge in model.get("edges", []):
        src = str(edge.get("from", ""))
        dst = str(edge.get("to", ""))
        kind = str(edge.get("kind", "")).strip()
        if not kind:
            errors.append(f"edge {src} -> {dst}: missing kind")
        for end in (src, dst):
            if end not in known:
                errors.append(f"edge {src} -> {dst}: unknown node {end!r}")

    return errors


def resolve_refs(model: dict[str, Any], graph: dict[str, Any]) -> dict[str, Any]:
    """Return a deep copy with code_refs/endpoints resolved against the graph.

    Adds per node: code_refs_resolved [{id, kind, label}], missing [],
    endpoints_resolved [], endpoints_missing [].
    """
    resolved = copy.deepcopy(model)
    graph_nodes = {node.get("id"): node for node in graph.get("nodes", [])}
    endpoints = [
        node for node in graph.get("nodes", []) if node.get("kind") == "endpoint"
    ]

    for node in resolved.get("nodes", []):
        resolved_refs = []
        missing = []
        for ref in node.get("code_refs") or []:
            target = graph_nodes.get(ref)
            if target is None:
                missing.append(ref)
            else:
                resolved_refs.append(
                    {
                        "id": ref,
                        "kind": target.get("kind", ""),
                        "label": target.get("label", ""),
                    }
                )
        node["code_refs_resolved"] = resolved_refs
        node["missing"] = missing

        endpoints_resolved = []
        endpoints_missing = []
        for ref in node.get("endpoints") or []:
            match = next(
                (ep["id"] for ep in endpoints if endpoint_matches(ep.get("id", ""), ref)),
                None,
            )
            if match is None:
                endpoints_missing.append(ref)
            else:
                endpoints_resolved.append(match)
        node["endpoints_resolved"] = endpoints_resolved
        node["endpoints_missing"] = endpoints_missing

    return resolved
