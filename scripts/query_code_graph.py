#!/usr/bin/env python3
"""Query helper for docs/codegraph/graph.json.

Modes:
  --query <text>              Keyword search with neighbor expansion
  --pattern <crate>           Extract the full blueprint of a crate (files, symbols, edges, contracts)
  --endpoints [--crate <c>]   List all API endpoints, optionally filtered by crate
  --how <topic>               Show architecture for a concept (types, endpoints, contracts)

Project-specific concepts (magician's tools/agents/skills/personalities,
Tauri config, etc.) live in extensions — see `scripts/codegraph_ext/`.
"""

from __future__ import annotations

import argparse
import json
from collections import defaultdict, deque
from pathlib import Path
from typing import Any


def load_graph(path: Path | str) -> dict[str, Any]:
    p = Path(path) if not isinstance(path, Path) else path
    return json.loads(p.read_text(encoding="utf-8"))


def load_contracts(graph_path: Path) -> dict[str, Any]:
    contracts_path = graph_path.parent / "contracts.json"
    if contracts_path.exists():
        return json.loads(contracts_path.read_text(encoding="utf-8"))
    return {}


def build_adjacency(edges: list[dict[str, Any]]) -> dict[str, set[str]]:
    adj: dict[str, set[str]] = defaultdict(set)
    for edge in edges:
        src = edge.get("from")
        dst = edge.get("to")
        if not src or not dst:
            continue
        adj[src].add(dst)
        adj[dst].add(src)
    return adj


def build_edge_indices(
    edges: list[dict[str, Any]],
) -> tuple[dict[str, list[dict[str, Any]]], dict[str, list[dict[str, Any]]]]:
    out_idx: dict[str, list[dict[str, Any]]] = defaultdict(list)
    in_idx: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for edge in edges:
        src = edge.get("from")
        dst = edge.get("to")
        if src:
            out_idx[src].append(edge)
        if dst:
            in_idx[dst].append(edge)
    return out_idx, in_idx


def bfs_neighbors(start: str, adjacency: dict[str, set[str]], depth: int) -> set[str]:
    if depth <= 0:
        return {start}
    seen = {start}
    queue: deque[tuple[str, int]] = deque([(start, 0)])
    while queue:
        node, d = queue.popleft()
        if d >= depth:
            continue
        for nxt in adjacency.get(node, set()):
            if nxt in seen:
                continue
            seen.add(nxt)
            queue.append((nxt, d + 1))
    return seen


# ── Mode: keyword search (original) ──────────────────────────────────


def cmd_query(args: argparse.Namespace, graph: dict[str, Any]) -> dict[str, Any]:
    nodes = graph.get("nodes", [])
    edges = graph.get("edges", [])

    query_terms = args.query.lower().strip().split()
    kinds = {k.strip() for k in args.kind.split(",")} if args.kind else None

    matches = []
    for node in nodes:
        kind = str(node.get("kind", ""))
        if kinds and kind not in kinds:
            continue

        hay = " ".join(
            [
                str(node.get("label", "")),
                str(node.get("id", "")),
                str(node.get("path", "")),
                str(node.get("module", "")),
                str(node.get("crate", "")),
            ]
        ).lower()

        if not query_terms:
            continue

        matched_terms = [term for term in query_terms if term in hay]
        if not matched_terms:
            continue

        score = 0
        # Boost score significantly based on the number of matching terms
        score += len(matched_terms) * 1000

        label = str(node.get("label", "")).lower()
        for term in matched_terms:
            if label == term:
                score += 100
            elif label.startswith(term):
                score += 50
            score += max(0, 20 - abs(len(label) - len(term)))
        matches.append((score, node))

    matches.sort(key=lambda x: (-x[0], x[1].get("id", "")))
    selected = [node for _, node in matches[: args.limit]]

    adjacency = build_adjacency(edges)
    edge_index_out, edge_index_in = build_edge_indices(edges)

    graph_path = Path(args.graph).resolve()
    response: dict[str, Any] = {
        "graph": str(graph_path),
        "query": args.query,
        "count": len(selected),
        "results": [],
    }

    for node in selected:
        node_id = node.get("id")
        neighbors = sorted(bfs_neighbors(node_id, adjacency, args.depth)) if node_id else []
        
        # Remove massive fields from node to prevent LLM context bloat
        compact_node = {k: v for k, v in node.items() if k not in ("mock_payload",)}

        response["results"].append(
            {
                "node": compact_node,
                "neighbors_depth": args.depth,
                "neighbor_count": max(0, len(neighbors) - 1),
                "neighbors": neighbors[:50],  # cap list of neighbors
                "out_edge_count": len(edge_index_out.get(node_id, [])),
                "in_edge_count": len(edge_index_in.get(node_id, [])),
            }
        )

    return response


# ── Mode: pattern — full crate blueprint ─────────────────────────────


def cmd_pattern(args: argparse.Namespace, graph: dict[str, Any]) -> dict[str, Any]:
    """Extract the full blueprint of a crate: files, symbols, edges, contracts."""
    crate_name = args.pattern
    nodes = graph.get("nodes", [])
    edges = graph.get("edges", [])
    node_map = {n["id"]: n for n in nodes}

    # All nodes in this crate
    crate_nodes = [n for n in nodes if n.get("crate") == crate_name]
    crate_ids = {n["id"] for n in crate_nodes}

    if not crate_nodes:
        return {"error": f"No crate found with name '{crate_name}'", "available_crates": sorted({n.get("crate", "") for n in nodes if n.get("crate")})}

    # Group by kind
    by_kind: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for n in crate_nodes:
        by_kind[n["kind"]].append({"label": n["label"], "path": n.get("path", ""), "line": n.get("line")})

    # Internal edges (both endpoints in crate)
    internal_edges: dict[str, int] = defaultdict(int)
    for e in edges:
        if e.get("from") in crate_ids and e.get("to") in crate_ids:
            internal_edges[e["kind"]] += 1

    # Cross-crate edges (one endpoint outside)
    cross_edges: list[dict[str, Any]] = []
    for e in edges:
        src_in = e.get("from") in crate_ids
        dst_in = e.get("to") in crate_ids
        if src_in != dst_in:
            # One is in, one is out
            outside_id = e["to"] if src_in else e["from"]
            outside_node = node_map.get(outside_id, {})
            cross_edges.append({
                "kind": e["kind"],
                "direction": "outbound" if src_in else "inbound",
                "external_node": outside_node.get("label", outside_id),
                "external_kind": outside_node.get("kind", "?"),
                "external_crate": outside_node.get("crate", "?"),
            })

    # Load contracts for this crate's files
    contracts = load_contracts(Path(args.graph).resolve())
    crate_files = {n.get("path", "") for n in crate_nodes if n.get("kind") == "file"}

    relevant_contracts: dict[str, list[Any]] = {}
    for section in ("ts_interfaces", "ts_type_aliases", "python_tool_params", "python_credentials"):
        items = contracts.get(section, [])
        matched = [item for item in items if item.get("file", "") in crate_files]
        if matched:
            relevant_contracts[section] = matched

    return {
        "crate": crate_name,
        "summary": {kind: len(items) for kind, items in by_kind.items()},
        "files": sorted([n["label"] for n in by_kind.get("file", [])]),
        "symbols": {
            kind: sorted([s["label"] for s in items])
            for kind, items in by_kind.items()
            if kind not in ("crate", "module", "file")
        },
        "internal_edges": dict(internal_edges),
        "cross_crate_edges": cross_edges,
        "contracts": relevant_contracts,
    }


# ── Mode: endpoints ──────────────────────────────────────────────────


def cmd_endpoints(args: argparse.Namespace, graph: dict[str, Any]) -> dict[str, Any]:
    """List all API endpoints with their handlers and payload types."""
    nodes = graph.get("nodes", [])
    edges = graph.get("edges", [])
    node_map = {n["id"]: n for n in nodes}

    crate_filter = args.crate if hasattr(args, "crate") and args.crate else None

    endpoints = [
        n for n in nodes
        if n["kind"] == "endpoint" and (not crate_filter or n.get("crate") == crate_filter)
    ]

    edge_out, edge_in = build_edge_indices(edges)

    result: list[dict[str, Any]] = []
    for ep in sorted(endpoints, key=lambda n: n.get("label", "")):
        ep_id = ep["id"]
        handler = None
        payloads: list[str] = []
        callers: list[dict[str, Any]] = []

        for e in edge_out.get(ep_id, []):
            if e["kind"] == "handles":
                h = node_map.get(e["to"], {})
                handler = {"name": h.get("label"), "path": h.get("path"), "line": h.get("line")}
            elif e["kind"] == "accepts_payload":
                p = node_map.get(e["to"], {})
                payloads.append(p.get("label", e["to"]))

        # Who calls this endpoint?
        for e in edge_in.get(ep_id, []):
            if e["kind"] == "targets_endpoint":
                caller = node_map.get(e["from"], {})
                callers.append({
                    "label": caller.get("label"),
                    "crate": caller.get("crate"),
                    "kind": caller.get("kind"),
                })

        result.append({
            "method": ep.get("method", "?"),
            "route": ep.get("route", ep.get("label", "")),
            "crate": ep.get("crate", ""),
            "handler": handler,
            "payload_types": payloads,
            "called_by": callers,
        })

    return {"endpoint_count": len(result), "endpoints": result}


# `cmd_capabilities` was retired — the legacy `capabilities` Rust crate
# no longer exists. Equivalent (and richer) discovery lives in
# `scripts/codegraph_ext/magician_skills.py` and is exposed as
# `cgraph_skills` / `/api/skills` / `/skills <type>`.


# ── Mode: how — architecture explain ─────────────────────────────────


def _explain_concept(
    topic: str,
    graph: dict[str, Any],
    graph_path: Path,
) -> dict[str, Any]:
    """Explain an architecture concept by gathering nodes, edges, endpoints, and contracts."""
    nodes = graph.get("nodes", [])
    edges = graph.get("edges", [])
    node_map = {n["id"]: n for n in nodes}
    edge_out, edge_in = build_edge_indices(edges)
    topic_lower = topic.lower()

    # 1. Find all matching nodes (structs, traits, enums, functions, endpoints)
    interesting_kinds = {"struct", "trait", "enum", "endpoint", "function", "type_alias"}
    matched_nodes: list[dict[str, Any]] = []
    for n in nodes:
        if n["kind"] not in interesting_kinds:
            continue
        hay = f"{n.get('label', '')} {n.get('module', '')} {n.get('path', '')}".lower()
        if topic_lower in hay:
            matched_nodes.append(n)

    if not matched_nodes:
        return {"error": f"No nodes found matching '{topic}'", "mode": "explain"}

    # Sort: structs/traits/enums first, then endpoints, then functions
    kind_rank = {"trait": 0, "struct": 1, "enum": 2, "endpoint": 3, "type_alias": 4, "function": 5}
    matched_nodes.sort(key=lambda n: (kind_rank.get(n["kind"], 99), n.get("label", "")))

    # 2. Group by kind
    by_kind: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for n in matched_nodes:
        by_kind[n["kind"]].append(n)

    # 3. Find key files (deduplicated)
    key_files: list[dict[str, Any]] = []
    seen_files: set[str] = set()
    for n in matched_nodes:
        path = n.get("path", "")
        if path and path not in seen_files:
            seen_files.add(path)
            key_files.append({"path": path, "crate": n.get("crate", ""), "module": n.get("module", "")})

    # 4. Find related endpoints
    related_endpoints: list[dict[str, Any]] = []
    for n in matched_nodes:
        if n["kind"] == "endpoint":
            handler = None
            for e in edge_out.get(n["id"], []):
                if e["kind"] == "handles":
                    h = node_map.get(e["to"], {})
                    handler = {"name": h.get("label"), "path": h.get("path"), "line": h.get("line")}
            related_endpoints.append({
                "method": n.get("method", "?"),
                "route": n.get("route", n.get("label", "")),
                "handler": handler,
            })

    # 5. Find cross-references (who calls/uses these nodes)
    matched_ids = {n["id"] for n in matched_nodes}
    callers: list[dict[str, Any]] = []
    callees: list[dict[str, Any]] = []
    seen_caller_pairs: set[tuple[str, str]] = set()
    for nid in matched_ids:
        for e in edge_in.get(nid, []):
            if e["kind"] in ("calls", "calls_api", "handles", "references"):
                src = node_map.get(e["from"], {})
                pair = (e["from"], e["kind"])
                if pair not in seen_caller_pairs and e["from"] not in matched_ids:
                    seen_caller_pairs.add(pair)
                    callers.append({
                        "label": src.get("label"), "kind": src.get("kind"),
                        "crate": src.get("crate"), "edge": e["kind"],
                    })
        for e in edge_out.get(nid, []):
            if e["kind"] in ("calls", "calls_api", "targets_endpoint"):
                dst = node_map.get(e["to"], {})
                pair = (e["to"], e["kind"])
                if pair not in seen_caller_pairs and e["to"] not in matched_ids:
                    seen_caller_pairs.add(pair)
                    callees.append({
                        "label": dst.get("label"), "kind": dst.get("kind"),
                        "crate": dst.get("crate"), "edge": e["kind"],
                    })

    # 6. Gather contracts
    contracts = load_contracts(graph_path)
    relevant_contracts: dict[str, list[Any]] = {}

    for section in ("struct_shapes", "enum_shapes", "constants", "validation_rules",
                     "error_types", "kind_gates", "default_guards", "whitelists",
                     "ts_interfaces", "python_tool_params"):
        items = contracts.get(section, [])
        matched = [
            item for item in items
            if topic_lower in json.dumps(item, default=str).lower()
        ]
        if matched:
            relevant_contracts[section] = matched[:15]  # cap per section

    # 7. Crate distribution — where does this concept live?
    crate_dist: dict[str, dict[str, int]] = defaultdict(lambda: defaultdict(int))
    for n in matched_nodes:
        crate_dist[n.get("crate", "unknown")][n["kind"]] += 1

    # 8. File clusters — group functions by file for primary implementation files
    file_clusters: dict[str, list[str]] = defaultdict(list)
    for n in matched_nodes:
        if n["kind"] == "function":
            file_clusters[n.get("path", "unknown")].append(n["label"])
    # Sort by density (most functions = most relevant file)
    primary_files = sorted(file_clusters.items(), key=lambda x: -len(x[1]))[:10]

    return {
        "mode": "explain",
        "topic": topic,
        "summary": {kind: len(items) for kind, items in by_kind.items()},
        "crate_distribution": {
            crate: dict(kinds) for crate, kinds in sorted(crate_dist.items())
        },
        "key_types": [
            {"label": n["label"], "kind": n["kind"], "path": n.get("path", ""), "line": n.get("line"),
             "crate": n.get("crate", ""), "module": n.get("module", "")}
            for n in matched_nodes
            if n["kind"] in ("struct", "trait", "enum", "type_alias")
        ][:20],
        "key_functions": [
            {"label": n["label"], "path": n.get("path", ""), "line": n.get("line"),
             "crate": n.get("crate", "")}
            for n in matched_nodes
            if n["kind"] == "function"
        ][:30],
        "primary_files": [
            {"path": path, "function_count": len(fns), "functions": fns[:8]}
            for path, fns in primary_files
        ],
        "endpoints": related_endpoints,
        "key_files": key_files[:15],
        "used_by": callers[:20],
        "depends_on": callees[:20],
        "contracts": relevant_contracts,
    }


def cmd_how(args: argparse.Namespace, graph: dict[str, Any]) -> dict[str, Any]:
    """Architecture explanation for a concept.

    The legacy "capability blueprint" branch was dropped along with the
    `capabilities` Rust crate — `cmd_how` now always returns the
    architecture-explain view. Skill / tool / agent blueprints live
    behind `cgraph_skills` (magician_skills extension)."""
    reference = args.how.lower().strip()
    return _explain_concept(reference, graph, Path(args.graph).resolve())


# ── Main ─────────────────────────────────────────────────────────────


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Query the generated code graph",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  %(prog)s --query "enroll"                    Search for nodes matching "enroll"
  %(prog)s --pattern bot-telegram              Full blueprint of the telegram adapter
  %(prog)s --endpoints                         List all API endpoints
  %(prog)s --endpoints --crate magician        Endpoints in the magician crate only
  %(prog)s --how telegram                      Architecture explain for a concept

For magician-specific lists (tools / agents / skills / personalities /
Tauri config), use the extension CLIs:
  python3 scripts/codegraph_ext_cli.py skills --type tool
  python3 scripts/codegraph_ext_cli.py tauri  --kind capability
        """,
    )
    parser.add_argument("--graph", default="docs/codegraph/graph.json", help="Path to graph JSON")

    # Modes
    parser.add_argument("--query", default=None, help="Keyword search")
    parser.add_argument("--pattern", "--detail", default=None, help="Extract full crate blueprint")
    parser.add_argument("--endpoints", action="store_true", help="List API endpoints")
    parser.add_argument("--how", default=None, help="Architecture explain for a concept")

    # Search options
    parser.add_argument("--kind", default=None, help="Node kind filter for --query")
    parser.add_argument("--limit", type=int, default=20, help="Max results for --query")
    parser.add_argument("--depth", type=int, default=1, help="Neighbor depth for --query")
    parser.add_argument("--crate", default=None, help="Crate filter for --endpoints")

    args = parser.parse_args()

    graph_path = Path(args.graph).resolve()
    graph = load_graph(graph_path)

    if args.how:
        result = cmd_how(args, graph)
    elif args.endpoints:
        result = cmd_endpoints(args, graph)
    elif args.pattern:
        result = cmd_pattern(args, graph)
    elif args.query:
        result = cmd_query(args, graph)
    else:
        parser.print_help()
        return 1

    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
