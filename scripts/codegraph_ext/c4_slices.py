"""C4 architecture-canvas slice extension.

Serves the lazy detail band of docs/codegraph/c4.html: children of a
crate or module (files, symbols, submodules), page-capped, straight from
graph.json. The page falls back to explorer deep links when this API is
absent (plain static hosting), so the route is an enhancement, not a
dependency.

Route: GET /api/c4/slice?crate=<name>&parent=<graph-id>&depth=1&cursor=0&limit=24
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs

from codegraph_ext import CodegraphExtension

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
GRAPH_JSON_PATH = REPO_ROOT / "docs" / "codegraph" / "graph.json"

SYMBOL_KINDS = {
    "function", "struct", "enum", "trait", "const", "static", "type_alias", "section",
}

DEFAULT_LIMIT = 24
MAX_LIMIT = 100

# In-process graph cache keyed on mtime so the dev server's staleness
# watcher (auto-rebuild) stays correct across regenerations.
_CACHE: dict[str, Any] = {"key": None, "graph": None}


def _load_graph() -> dict[str, Any]:
    try:
        mtime = GRAPH_JSON_PATH.stat().st_mtime_ns
    except OSError:
        return {"nodes": [], "edges": []}
    key = f"{GRAPH_JSON_PATH}:{mtime}"
    if _CACHE["key"] == key and _CACHE["graph"] is not None:
        return _CACHE["graph"]
    try:
        graph = json.loads(GRAPH_JSON_PATH.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {"nodes": [], "edges": []}
    _CACHE["key"] = key
    _CACHE["graph"] = graph
    return graph


class C4SlicesExtension(CodegraphExtension):
    name = "c4_slices"

    def __init__(self) -> None:
        # Tests inject a fixture graph here instead of touching disk.
        self._graph_override: dict[str, Any] | None = None

    def _graph(self) -> dict[str, Any]:
        return self._graph_override if self._graph_override is not None else _load_graph()

    def http_routes(self) -> dict[str, Any]:
        return {"/api/c4/slice": self._handle_slice}

    def _handle_slice(self, parsed_url: Any) -> dict[str, Any]:
        params = parse_qs(parsed_url.query)
        crate = (params.get("crate", [""])[0] or "").strip()
        parent = (params.get("parent", [""])[0] or "").strip()
        depth = max(1, min(3, int(params.get("depth", ["1"])[0] or 1)))
        cursor = max(0, int(params.get("cursor", ["0"])[0] or 0))
        limit = max(1, min(MAX_LIMIT, int(params.get("limit", [str(DEFAULT_LIMIT)])[0] or DEFAULT_LIMIT)))

        graph = self._graph()
        nodes = graph.get("nodes", [])
        result: dict[str, Any] = {"files": [], "symbols": [], "children": [], "truncated": 0, "next_cursor": None}
        if not parent:
            return result

        if parent.startswith("crate::"):
            crate_name = crate or parent[len("crate::"):]
            prefix = f"module::{crate_name}::"
            for node in nodes:
                if node.get("kind") != "module":
                    continue
                node_id = str(node.get("id", ""))
                if not node_id.startswith(prefix):
                    continue
                rest = node_id[len(prefix):]
                if "::" in rest:
                    continue  # top-level modules only
                result["children"].append({
                    "id": node_id,
                    "label": str(node.get("label") or rest),
                    "symbols": 0,
                })
            result["children"].sort(key=lambda c: c["id"])
            return result

        if not parent.startswith("module::"):
            return result

        module_path = parent[len("module::"):]
        child_prefix = f"module::{module_path}::"

        for node in nodes:
            kind = node.get("kind")
            node_id = str(node.get("id", ""))
            if kind == "module" and node_id.startswith(child_prefix):
                rest = node_id[len(child_prefix):]
                if depth >= 1 and "::" not in rest:
                    result["children"].append({
                        "id": node_id,
                        "label": str(node.get("label") or rest),
                        "symbols": 0,
                    })
            elif kind == "file" and str(node.get("module") or "") == module_path:
                result["files"].append({
                    "id": node_id,
                    "label": str(node.get("label") or node_id),
                })
            elif kind in SYMBOL_KINDS and str(node.get("module") or "") == module_path:
                result["symbols"].append({
                    "id": node_id,
                    "label": str(node.get("label") or node_id),
                    "kind": str(kind),
                })

        result["children"].sort(key=lambda c: c["id"])
        result["files"].sort(key=lambda f: f["id"])
        result["symbols"].sort(key=lambda s: s["id"])

        total = len(result["symbols"])
        window = result["symbols"][cursor:cursor + limit]
        result["truncated"] = max(0, total - cursor - len(window))
        result["symbols"] = window
        result["next_cursor"] = cursor + limit if result["truncated"] > 0 else None
        return result
