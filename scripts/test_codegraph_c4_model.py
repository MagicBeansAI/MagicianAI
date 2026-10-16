"""Tests for the curated architecture model validator (scripts/c4_model.py)."""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))

import c4_model  # noqa: E402


def _make_graph() -> dict:
    return {
        "nodes": [
            {"id": "crate::magic-supervisor", "kind": "crate", "label": "magic-supervisor"},
            {"id": "crate::magician", "kind": "crate", "label": "magician"},
            {
                "id": "module::magician::magician_v2::execution",
                "kind": "module",
                "label": "magician_v2::execution",
            },
            {
                "id": "endpoint::POST::/v2/threads::magician/src/api.rs::10:0",
                "kind": "endpoint",
                "label": "POST /v2/threads",
            },
        ],
        "edges": [],
    }


def _make_model() -> dict:
    return {
        "version": 1,
        "system": {
            "label": "Magician",
            "summary": "Agent runtime.",
            "doc": "README.md",
        },
        "nodes": [
            {
                "id": "runtime:supervisor",
                "label": "Magic Supervisor",
                "summary": "Supervises the local stack.",
                "doc": "docs/supervisor.md",
                "runs": {"port": 8081},
                "code_refs": ["crate::magic-supervisor"],
            },
            {
                "id": "area:agentic-loop",
                "parent": "runtime:magician",
                "label": "Agentic execution loop",
                "summary": "Flat outer agentic loop.",
                "doc": "docs/execution.md",
                "code_refs": ["module::magician::magician_v2::execution"],
                "endpoints": ["POST /v2/threads"],
            },
            {
                "id": "runtime:magician",
                "label": "Magician server",
                "summary": "Main service.",
                "doc": "docs/magician.md",
                "runs": {"port": 3002},
                "code_refs": ["crate::magician"],
            },
        ],
        "edges": [
            {
                "from": "runtime:supervisor",
                "to": "runtime:magician",
                "kind": "supervises",
                "transport": "child process",
                "summary": "Launches and health-checks.",
            }
        ],
        "externals": [
            {
                "id": "external:llm-providers",
                "label": "LLM providers",
                "kind": "ai",
                "summary": "Multi-provider LLMs.",
                "touched_by": ["runtime:magician"],
            }
        ],
        "actors": [
            {"id": "actor:user", "label": "User", "summary": "Human owner."}
        ],
    }


def _make_repo(tmp_path: Path) -> Path:
    for rel in (
        "README.md",
        "docs/supervisor.md",
        "docs/execution.md",
        "docs/magician.md",
    ):
        target = tmp_path / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text("First paragraph.\n\nSecond paragraph.\n", encoding="utf-8")
    return tmp_path


def test_valid_model_passes(tmp_path: Path) -> None:
    errors = c4_model.validate_model(_make_model(), _make_graph(), _make_repo(tmp_path))
    assert errors == []


def test_dangling_code_ref_reported(tmp_path: Path) -> None:
    model = _make_model()
    model["nodes"][0]["code_refs"] = ["crate::does-not-exist"]
    errors = c4_model.validate_model(model, _make_graph(), _make_repo(tmp_path))
    assert any("crate::does-not-exist" in err for err in errors)
    assert any("runtime:supervisor" in err for err in errors)


def test_missing_doc_reported(tmp_path: Path) -> None:
    model = _make_model()
    model["nodes"][0]["doc"] = "docs/missing.md"
    errors = c4_model.validate_model(model, _make_graph(), _make_repo(tmp_path))
    assert any("docs/missing.md" in err for err in errors)


def test_duplicate_node_id_reported(tmp_path: Path) -> None:
    model = _make_model()
    model["nodes"].append(dict(model["nodes"][0]))
    errors = c4_model.validate_model(model, _make_graph(), _make_repo(tmp_path))
    assert any("duplicate" in err.lower() for err in errors)


def test_edge_to_unknown_node_reported(tmp_path: Path) -> None:
    model = _make_model()
    model["edges"][0]["to"] = "runtime:ghost"
    errors = c4_model.validate_model(model, _make_graph(), _make_repo(tmp_path))
    assert any("runtime:ghost" in err for err in errors)


def test_bad_namespace_reported(tmp_path: Path) -> None:
    model = _make_model()
    model["nodes"][0]["id"] = "foo:bar"
    errors = c4_model.validate_model(model, _make_graph(), _make_repo(tmp_path))
    assert any("foo:bar" in err and "namespace" in err.lower() for err in errors)


def test_area_requires_parent(tmp_path: Path) -> None:
    model = _make_model()
    del model["nodes"][1]["parent"]
    errors = c4_model.validate_model(model, _make_graph(), _make_repo(tmp_path))
    assert any("area:agentic-loop" in err and "parent" in err.lower() for err in errors)


def test_node_requires_code_refs_or_ungrounded(tmp_path: Path) -> None:
    model = _make_model()
    model["nodes"][0]["code_refs"] = []
    errors = c4_model.validate_model(model, _make_graph(), _make_repo(tmp_path))
    assert any("runtime:supervisor" in err and "code_refs" in err for err in errors)
    model["nodes"][0]["ungrounded"] = True
    errors = c4_model.validate_model(model, _make_graph(), _make_repo(tmp_path))
    assert errors == []


def test_system_doc_must_exist(tmp_path: Path) -> None:
    model = _make_model()
    model["system"]["doc"] = "docs/nope.md"
    errors = c4_model.validate_model(model, _make_graph(), _make_repo(tmp_path))
    assert any("docs/nope.md" in err for err in errors)


def test_resolve_refs_maps_ids_and_routes() -> None:
    graph = _make_graph()
    resolved = c4_model.resolve_refs(_make_model(), graph)
    by_id = {node["id"]: node for node in resolved["nodes"]}
    supervisor = by_id["runtime:supervisor"]
    assert [ref["id"] for ref in supervisor["code_refs_resolved"]] == [
        "crate::magic-supervisor"
    ]
    assert supervisor["missing"] == []
    loop = by_id["area:agentic-loop"]
    assert [ref["id"] for ref in loop["code_refs_resolved"]] == [
        "module::magician::magician_v2::execution"
    ]
    assert loop["endpoints_resolved"] == [
        "endpoint::POST::/v2/threads::magician/src/api.rs::10:0"
    ]
    assert loop["endpoints_missing"] == []


def test_resolve_refs_reports_missing_route(tmp_path: Path) -> None:
    model = _make_model()
    model["nodes"][1]["endpoints"] = ["GET /nope"]
    resolved = c4_model.resolve_refs(model, _make_graph())
    by_id = {node["id"]: node for node in resolved["nodes"]}
    assert by_id["area:agentic-loop"]["endpoints_missing"] == ["GET /nope"]


def test_load_model_roundtrip(tmp_path: Path) -> None:
    repo = _make_repo(tmp_path)
    (repo / "arch.yaml").write_text(
        "version: 1\n"
        "system:\n"
        "  label: Magician\n"
        "  summary: Agent runtime.\n"
        "  doc: README.md\n"
        "nodes: []\n"
        "edges: []\n"
        "externals: []\n"
        "actors: []\n",
        encoding="utf-8",
    )
    model = c4_model.load_model(repo / "arch.yaml")
    assert model["system"]["label"] == "Magician"


def test_load_model_rejects_bad_shape(tmp_path: Path) -> None:
    bad = tmp_path / "bad.yaml"
    bad.write_text("version: 1\nsystem: {}\n", encoding="utf-8")
    with pytest.raises(c4_model.ModelError):
        c4_model.load_model(bad)
