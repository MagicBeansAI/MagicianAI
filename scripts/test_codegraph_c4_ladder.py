"""Tests for the code-ladder derivation module (scripts/c4_code_ladder.py)."""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import c4_code_ladder  # noqa: E402


def _make_graph() -> dict:
    nodes = [
        {"id": "crate::a", "kind": "crate", "label": "a", "path": "a"},
        {"id": "crate::b", "kind": "crate", "label": "b", "path": "b"},
        {"id": "crate::c", "kind": "crate", "label": "c", "path": "c"},
        {"id": "crate::unified-ui", "kind": "crate", "label": "unified-ui", "path": "ui/unified-ui"},
        {"id": "crate::bot-telegram", "kind": "crate", "label": "bot-telegram", "path": "bots/telegram"},
        {"id": "crate::skill-browser", "kind": "crate", "label": "skill-browser", "path": "skillshub/browser"},
        {"id": "crate::magic-supervisor", "kind": "crate", "label": "magic-supervisor", "path": "magic-supervisor"},
        {"id": "module::a::core", "kind": "module", "label": "a::core", "crate": "a"},
        {"id": "module::a::core::sub", "kind": "module", "label": "a::core::sub", "crate": "a"},
        {"id": "module::b::api", "kind": "module", "label": "b::api", "crate": "b"},
        {"id": "symbol::a/src/core.rs::function::do_a::10", "kind": "function", "label": "do_a", "crate": "a", "module": "a::core"},
        {"id": "symbol::a/src/core.rs::struct::Thing::20", "kind": "struct", "label": "Thing", "crate": "a", "module": "a::core::sub"},
        {"id": "symbol::b/src/api.rs::function::do_b::5", "kind": "function", "label": "do_b", "crate": "b", "module": "b::api"},
        {"id": "file::a/src/core.rs", "kind": "file", "label": "core.rs", "crate": "a"},
        {"id": "file::a/src/lib.rs", "kind": "file", "label": "lib.rs", "crate": "a"},
        {
            "id": "endpoint::POST::/chat/new::b/src/api.rs::5:0",
            "kind": "endpoint",
            "label": "POST /chat/new",
            "crate": "b",
        },
    ]
    edges = [
        {"from": "crate::b", "to": "crate::a", "kind": "depends_on", "weight": 1},
        {"from": "crate::c", "to": "crate::b", "kind": "depends_on", "weight": 1},
    ]
    return {"nodes": nodes, "edges": edges}


def _make_repo(tmp_path: Path) -> Path:
    comp = tmp_path / "docs" / "components" / "a"
    comp.mkdir(parents=True, exist_ok=True)
    (comp / "README.md").write_text(
        "# Crate A\n\nCrate A does input handling.\n\nMore detail here.\n",
        encoding="utf-8",
    )
    cargo = tmp_path / "b"
    cargo.mkdir(parents=True, exist_ok=True)
    (cargo / "Cargo.toml").write_text(
        '[package]\nname = "b"\ndescription = "Crate B from cargo"\n',
        encoding="utf-8",
    )
    return tmp_path


def _tiers_by_id(ladder: dict) -> dict[str, list[str]]:
    return {tier["id"]: [c["id"] for c in tier["crates"]] for tier in ladder["tiers"]}


def test_topological_tiers(tmp_path: Path) -> None:
    ladder = c4_code_ladder.build_code_ladder(_make_graph(), _make_repo(tmp_path))
    tiers = _tiers_by_id(ladder)
    assert tiers["foundation"] == ["crate::a"]
    assert tiers["mid"] == ["crate::b", "crate::c"]


def test_rule_based_tiers(tmp_path: Path) -> None:
    ladder = c4_code_ladder.build_code_ladder(_make_graph(), _make_repo(tmp_path))
    tiers = _tiers_by_id(ladder)
    assert tiers["apps"] == ["crate::unified-ui"]
    assert tiers["bots"] == ["crate::bot-telegram"]
    assert tiers["skills"] == ["crate::skill-browser"]
    assert tiers["standalone"] == ["crate::magic-supervisor"]
    order_index = [c4_code_ladder.TIER_ORDER.index(t["id"]) for t in ladder["tiers"]]
    assert order_index == sorted(order_index)


def test_crate_stats_and_modules(tmp_path: Path) -> None:
    ladder = c4_code_ladder.build_code_ladder(_make_graph(), _make_repo(tmp_path))
    crates = {c["id"]: c for tier in ladder["tiers"] for c in tier["crates"]}
    a = crates["crate::a"]
    assert a["symbols"] == 2
    assert a["files"] == 2
    assert a["modules"] == 2
    assert a["endpoints"] == 0
    b = crates["crate::b"]
    assert b["symbols"] == 1
    assert b["endpoints"] == 1
    modules = {m["id"]: m for m in a["modules_list"]}
    assert modules["module::a::core"]["label"] == "core"
    assert modules["module::a::core"]["symbols"] == 1
    assert modules["module::a::core::sub"]["symbols"] == 1
    assert b["deps"] == ["crate::a"]


def test_description_harvest_prefers_docs_components(tmp_path: Path) -> None:
    ladder = c4_code_ladder.build_code_ladder(_make_graph(), _make_repo(tmp_path))
    crates = {c["id"]: c for tier in ladder["tiers"] for c in tier["crates"]}
    assert crates["crate::a"]["description"] == "Crate A does input handling."
    assert crates["crate::b"]["description"] == "Crate B from cargo"


def test_description_harvest_case_map(tmp_path: Path) -> None:
    repo = _make_repo(tmp_path)
    comp = repo / "docs" / "components" / "magios"
    comp.mkdir(parents=True, exist_ok=True)
    (comp / "README.md").write_text(
        "# Magios\n\niOS companion app.\n",
        encoding="utf-8",
    )
    graph = _make_graph()
    graph["nodes"].append({"id": "crate::Magios", "kind": "crate", "label": "Magios", "path": "magios"})
    ladder = c4_code_ladder.build_code_ladder(graph, repo)
    crates = {c["id"]: c for tier in ladder["tiers"] for c in tier["crates"]}
    assert crates["crate::Magios"]["description"] == "iOS companion app."
    assert crates["crate::Magios"]["tech"] == "swift"


def test_unknown_crate_description_is_none(tmp_path: Path) -> None:
    ladder = c4_code_ladder.build_code_ladder(_make_graph(), _make_repo(tmp_path))
    crates = {c["id"]: c for tier in ladder["tiers"] for c in tier["crates"]}
    assert crates["crate::c"]["description"] is None
