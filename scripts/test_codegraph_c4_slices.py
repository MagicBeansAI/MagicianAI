"""Tests for the c4 slice extension (scripts/codegraph_ext/c4_slices.py)."""

from __future__ import annotations

import json
import sys
from pathlib import Path
from urllib.parse import urlparse

sys.path.insert(0, str(Path(__file__).resolve().parent))

from codegraph_ext.c4_slices import C4SlicesExtension  # noqa: E402


def _make_graph() -> dict:
    nodes = [
        {"id": "crate::magician", "kind": "crate", "label": "magician"},
        {"id": "module::magician::magician_v2", "kind": "module", "label": "magician_v2", "crate": "magician"},
        {"id": "module::magician::magician_v2::execution", "kind": "module", "label": "execution", "crate": "magician"},
        {"id": "module::magician::magician_v2::execution::lanes", "kind": "module", "label": "lanes", "crate": "magician"},
        {"id": "file::magician/src/execution/mod.rs", "kind": "file", "label": "mod.rs", "crate": "magician", "module": "magician::magician_v2::execution"},
        {"id": "file::magician/src/execution/lanes.rs", "kind": "file", "label": "lanes.rs", "crate": "magician", "module": "magician::magician_v2::execution::lanes"},
        {"id": "symbol::magician/src/execution/mod.rs::function::run_loop::10", "kind": "function", "label": "run_loop", "crate": "magician", "module": "magician::magician_v2::execution"},
        {"id": "symbol::magician/src/execution/mod.rs::struct::LoopState::40", "kind": "struct", "label": "LoopState", "crate": "magician", "module": "magician::magician_v2::execution"},
        {"id": "symbol::magician/src/execution/lanes.rs::function::lane_step::5", "kind": "function", "label": "lane_step", "crate": "magician", "module": "magician::magician_v2::execution::lanes"},
        {"id": "symbol::magician/src/execution/mod.rs::function::helper_a::60", "kind": "function", "label": "helper_a", "crate": "magician", "module": "magician::magician_v2::execution"},
        {"id": "symbol::magician/src/execution/mod.rs::function::helper_b::70", "kind": "function", "label": "helper_b", "crate": "magician", "module": "magician::magician_v2::execution"},
    ]
    edges = []
    return {"nodes": nodes, "edges": edges}


def _handler(graph: dict):
    ext = C4SlicesExtension()
    ext._graph_override = graph
    routes = ext.http_routes()
    assert "/api/c4/slice" in routes
    return routes["/api/c4/slice"]


def _call(handler, query: str) -> dict:
    return handler(urlparse(f"http://x/api/c4/slice?{query}"))


def test_module_slice_returns_files_and_symbols() -> None:
    handler = _handler(_make_graph())
    result = _call(handler, "crate=magician&parent=module::magician::magician_v2::execution&depth=1")
    assert [f["label"] for f in result["files"]] == ["mod.rs"]
    labels = [s["label"] for s in result["symbols"]]
    assert "run_loop" in labels and "LoopState" in labels
    # Deeper module symbols are excluded at depth 1.
    assert "lane_step" not in labels
    assert [c["id"] for c in result["children"]] == ["module::magician::magician_v2::execution::lanes"]


def test_slice_pagination_and_truncation() -> None:
    handler = _handler(_make_graph())
    # Four symbols live at this module: helper_a, helper_b, run_loop, LoopState.
    page1 = _call(handler, "crate=magician&parent=module::magician::magician_v2::execution&depth=1&limit=2")
    assert len(page1["symbols"]) == 2
    assert page1["truncated"] == 2
    assert page1["next_cursor"] == 2
    page2 = _call(handler, "crate=magician&parent=module::magician::magician_v2::execution&depth=1&limit=2&cursor=2")
    assert len(page2["symbols"]) == 2
    assert page2["next_cursor"] is None


def test_crate_slice_returns_top_modules() -> None:
    handler = _handler(_make_graph())
    result = _call(handler, "crate=magician&parent=crate::magician")
    assert [c["id"] for c in result["children"]] == ["module::magician::magician_v2"]
    assert result["files"] == []


def test_unknown_crate_returns_empty() -> None:
    handler = _handler(_make_graph())
    result = _call(handler, "crate=ghost&parent=crate::ghost")
    assert result == {"files": [], "symbols": [], "children": [], "truncated": 0, "next_cursor": None}
