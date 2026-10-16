"""Generate docs/codegraph/c4.json — the merged render model for the C4
architecture canvas.

Merges the curated architecture model (docs/architecture/architecture.yaml)
with the derived code graph (docs/codegraph/graph.json):

- validates the curated model against the graph (dangling code_refs are
  errors; --strict turns them into a non-zero exit),
- enriches curated nodes with computed stats,
- appends the tiered code ladder (tiers -> crates -> modules),
- writes the compact c4.json the canvas loads (< 1 MB).

Run via `make graph-index` (non-strict) or `make graph-check` (--strict).
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import subprocess
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "scripts"))

import c4_code_ladder  # noqa: E402
import c4_model  # noqa: E402

DEFAULT_GRAPH = REPO_ROOT / "docs" / "codegraph" / "graph.json"
DEFAULT_MODEL = REPO_ROOT / "docs" / "architecture" / "architecture.yaml"
DEFAULT_OUTPUT = REPO_ROOT / "docs" / "codegraph" / "c4.json"


def _git_commit() -> str:
    try:
        result = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
            check=True,
        )
        return result.stdout.strip()
    except Exception:
        return ""


def _load_graph(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def _node_stats(node: dict[str, Any], graph: dict[str, Any]) -> dict[str, int]:
    """Aggregate stats over a curated node's resolved code refs.

    Crate refs contribute crate-wide counts; module refs contribute their
    subtree symbol count. Endpoint counts come from resolved endpoint
    evidence plus referenced-crate endpoint counts.
    """
    nodes_by_id = {n.get("id"): n for n in graph.get("nodes", [])}
    crate_stats: dict[str, dict[str, int]] = {}
    for n in graph.get("nodes", []):
        crate = str(n.get("crate") or "")
        kind = n.get("kind")
        if not crate or kind == "crate":
            continue
        if kind == "module":
            parts = str(n.get("id", "")).split("::")
            if len(parts) >= 2:
                crate = parts[1]
        entry = crate_stats.setdefault(crate, {"symbols": 0, "modules": 0, "endpoints": 0})
        if kind in {
            "function", "struct", "enum", "trait", "const", "static",
            "type_alias", "section",
        }:
            entry["symbols"] += 1
        elif kind == "module":
            entry["modules"] += 1
        elif kind == "endpoint":
            entry["endpoints"] += 1

    symbols = 0
    modules = 0
    endpoints = 0
    seen_crates: set[str] = set()
    seen_modules: set[str] = set()
    for ref in node.get("code_refs_resolved", []):
        target = nodes_by_id.get(ref.get("id", ""))
        if target is None:
            continue
        kind = target.get("kind")
        if kind == "crate":
            name = str(target.get("label") or ref.get("id", "").split("::")[-1])
            if name in seen_crates:
                continue
            seen_crates.add(name)
            stats = crate_stats.get(name, {})
            symbols += stats.get("symbols", 0)
            modules += stats.get("modules", 0)
            endpoints += stats.get("endpoints", 0)
        elif kind == "module":
            module_id = str(target.get("id", ""))
            module_path = module_id.replace("module::", "", 1)
            if module_path in seen_modules:
                continue
            seen_modules.add(module_path)
            for n in graph.get("nodes", []):
                if n.get("kind") in {"function", "struct", "enum", "trait"} and str(
                    n.get("module") or ""
                ).startswith(module_path):
                    symbols += 1
    endpoints += len(node.get("endpoints_resolved", []))
    return {"symbols": symbols, "modules": modules, "endpoints": endpoints}


def build_index(model: dict[str, Any], graph: dict[str, Any], repo_root: Path) -> dict[str, Any]:
    resolved = c4_model.resolve_refs(model, graph)
    nodes = []
    for node in resolved.get("nodes", []):
        enriched = dict(node)
        enriched["stats"] = _node_stats(node, graph)
        nodes.append(enriched)

    return {
        "version": 1,
        "generated_at": dt.datetime.utcnow().replace(microsecond=0).isoformat() + "Z",
        "commit": _git_commit(),
        "system": model.get("system", {}),
        "actors": model.get("actors", []),
        "externals": model.get("externals", []),
        "nodes": nodes,
        "edges": model.get("edges", []),
        "code": c4_code_ladder.build_code_ladder(graph, repo_root),
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Generate the c4 architecture index")
    parser.add_argument("--graph", default=str(DEFAULT_GRAPH), help="Path to graph.json")
    parser.add_argument("--model", default=str(DEFAULT_MODEL), help="Path to architecture.yaml")
    parser.add_argument("--output", default=str(DEFAULT_OUTPUT), help="Output c4.json path")
    parser.add_argument(
        "--strict",
        action="store_true",
        help="Exit non-zero on validation errors or missing artifacts (graph-check mode)",
    )
    args = parser.parse_args(argv)

    graph_path = Path(args.graph)
    model_path = Path(args.model)
    output_path = Path(args.output)

    if not model_path.is_file():
        if args.strict:
            print(f"[c4] architecture model not found: {model_path}", file=sys.stderr)
            return 1
        print(f"[c4] no architecture model at {model_path}; skipping c4 index")
        return 0

    if not graph_path.is_file():
        if args.strict:
            print(f"[c4] graph not found: {graph_path}", file=sys.stderr)
            return 1
        print(f"[c4] no graph at {graph_path}; skipping c4 index (run make graph-index)")
        return 0

    try:
        model = c4_model.load_model(model_path)
    except c4_model.ModelError as exc:
        print(f"[c4] model error: {exc}", file=sys.stderr)
        return 1

    graph = _load_graph(graph_path)
    errors = c4_model.validate_model(model, graph, REPO_ROOT)
    if errors:
        for error in errors:
            print(f"[c4] validation: {error}", file=sys.stderr)
        if args.strict:
            return 1
        print(f"[c4] proceeding with {len(errors)} validation warnings (non-strict)")

    index = build_index(model, graph, REPO_ROOT)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(
        json.dumps(index, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )
    endpoint_warnings = [
        (n["id"], n["endpoints_missing"])
        for n in index["nodes"]
        if n.get("endpoints_missing")
    ]
    for node_id, missing in endpoint_warnings:
        print(f"[c4] endpoint evidence not found for {node_id}: {missing}", file=sys.stderr)
    size_kb = output_path.stat().st_size / 1024
    print(f"[c4] wrote {output_path} ({size_kb:.0f} KB)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
