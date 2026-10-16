"""Tests for the c4 index generator CLI (scripts/generate_c4_index.py)."""

from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import generate_c4_index  # noqa: E402


def _make_graph() -> dict:
    return {
        "version": "1.2.0",
        "nodes": [
            {"id": "crate::magic-supervisor", "kind": "crate", "label": "magic-supervisor", "path": "magic-supervisor"},
            {"id": "crate::magician", "kind": "crate", "label": "magician", "path": "magician"},
            {"id": "module::magician::magician_v2::execution", "kind": "module", "label": "execution", "crate": "magician"},
            {
                "id": "symbol::magician/src/x.rs::function::run::1",
                "kind": "function",
                "label": "run",
                "crate": "magician",
                "module": "magician::magician_v2::execution",
            },
            {
                "id": "endpoint::POST::/executions::magician/src/x.rs::9:0",
                "kind": "endpoint",
                "label": "POST /executions",
                "crate": "magician",
            },
        ],
        "edges": [],
    }


MODEL_YAML = """
version: 1
system:
  label: Magician
  summary: Agent runtime.
  doc: README.md
nodes:
  - id: runtime:supervisor
    label: Magic Supervisor
    summary: Supervises the stack.
    doc: docs/supervisor.md
    code_refs: [crate::magic-supervisor]
  - id: runtime:magician
    label: Magician Server
    summary: Main service.
    doc: docs/magician.md
    code_refs: [crate::magician]
  - id: area:agentic-loop
    parent: runtime:magician
    label: Agentic Execution Loop
    summary: Flat loop.
    doc: docs/execution.md
    code_refs: [module::magician::magician_v2::execution]
    endpoints: ["POST /executions"]
edges:
  - { from: runtime:supervisor, to: runtime:magician, kind: supervises, summary: "Keeps alive." }
externals:
  - { id: external:llm-providers, label: LLM Providers, kind: ai, summary: "LLMs.", touched_by: [runtime:magician] }
actors:
  - { id: actor:user, label: User, summary: "Owner." }
"""


def _make_env(tmp_path: Path) -> tuple[Path, Path]:
    (tmp_path / "README.md").write_text("# Root\n\nRoot summary.\n", encoding="utf-8")
    docs = tmp_path / "docs"
    docs.mkdir()
    for name in ("supervisor.md", "magician.md", "execution.md"):
        (docs / name).write_text(f"# {name}\n\nSummary {name}.\n", encoding="utf-8")
    model_path = tmp_path / "architecture.yaml"
    model_path.write_text(MODEL_YAML, encoding="utf-8")
    graph_path = tmp_path / "graph.json"
    graph_path.write_text(json.dumps(_make_graph()), encoding="utf-8")
    return graph_path, model_path


def test_generates_enriched_c4_json(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.chdir(tmp_path)
    graph_path, model_path = _make_env(tmp_path)
    output = tmp_path / "c4.json"
    rc = generate_c4_index.main(
        [
            "--graph", str(graph_path),
            "--model", str(model_path),
            "--output", str(output),
        ]
    )
    assert rc == 0
    data = json.loads(output.read_text(encoding="utf-8"))
    assert data["version"] == 1
    assert data["system"]["label"] == "Magician"
    nodes = {n["id"]: n for n in data["nodes"]}
    assert nodes["runtime:supervisor"]["code_refs_resolved"][0]["kind"] == "crate"
    assert nodes["runtime:magician"]["stats"]["symbols"] == 1
    assert nodes["runtime:magician"]["stats"]["endpoints"] == 1
    assert nodes["area:agentic-loop"]["stats"]["symbols"] == 1
    assert nodes["area:agentic-loop"]["endpoints_resolved"]
    assert data["edges"][0]["kind"] == "supervises"
    assert data["externals"][0]["id"] == "external:llm-providers"
    assert data["actors"][0]["id"] == "actor:user"
    tiers = {t["id"]: t for t in data["code"]["tiers"]}
    assert "crate::magician" in [c["id"] for c in tiers["orchestrator"]["crates"]]
    assert "crate::magic-supervisor" in [c["id"] for c in tiers["standalone"]["crates"]]


def test_strict_fails_on_dangling_ref(tmp_path: Path, monkeypatch, capsys) -> None:
    monkeypatch.chdir(tmp_path)
    graph_path, model_path = _make_env(tmp_path)
    model_path.write_text(
        MODEL_YAML.replace("crate::magic-supervisor", "crate::ghost"),
        encoding="utf-8",
    )
    output = tmp_path / "c4.json"
    rc = generate_c4_index.main(
        [
            "--graph", str(graph_path),
            "--model", str(model_path),
            "--output", str(output),
            "--strict",
        ]
    )
    assert rc == 1
    assert "crate::ghost" in capsys.readouterr().err
    assert not output.exists()


def test_strict_fails_on_missing_graph(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.chdir(tmp_path)
    _, model_path = _make_env(tmp_path)
    rc = generate_c4_index.main(
        [
            "--graph", str(tmp_path / "nope.json"),
            "--model", str(model_path),
            "--output", str(tmp_path / "c4.json"),
            "--strict",
        ]
    )
    assert rc == 1
