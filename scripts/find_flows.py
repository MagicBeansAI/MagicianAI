#!/usr/bin/env python3
"""Trace incoming (or outgoing) flows for any node in the code graph.

Given a target node — by ID or by label/name (any kind: function,
crate, module, file, endpoint, …) — walk the graph upstream
(default) or downstream up to N hops along the call/handle/implement
edges and return the induced subgraph plus a per-hop breakdown.

Used by:
    - HTTP endpoint   `/api/flows`
    - MCP tool        `cgraph_flows`
    - CLI             `python3 scripts/find_flows.py --target X`
    - 2D & 3D viewers via the `/flows` slash command

Flow edge kinds (the ones that mean "X actually calls / handles / implements Y"):
    calls, handles, implements, calls_api, targets_endpoint
"""

from __future__ import annotations

import argparse
import json
from collections import defaultdict, deque
from pathlib import Path


FLOW_EDGE_KINDS = {"calls", "handles", "implements", "calls_api", "targets_endpoint"}

# Default cap on how many nodes the response carries. Above this the
# LLM-facing payload would balloon past typical context budgets.
DEFAULT_LIMIT = 120

# Node kinds for which the `line` attribute is meaningful (filtering
# it out elsewhere strips a token-wasteful zero / repeated-N).
LINE_BEARING_KINDS = {
    "function", "struct", "enum", "trait", "type_alias", "const",
    "static", "section", "endpoint", "api_call",
}


def resolve_target(graph: dict, target: str) -> dict | None:
    """Find a node by id (exact match), by label or by HTTP `route`
    (case-insensitive substring). Targets that look like API paths
    (e.g. `/api/threads/{id}`) are matched against endpoint nodes'
    `route` field first so users can type `/flows /api/...` directly."""
    nodes = graph["nodes"]
    by_id = {n["id"]: n for n in nodes}
    if target in by_id:
        return by_id[target]
    tlow = target.lower().strip()
    if not tlow:
        return None

    # API-path targets: match endpoint nodes by `route`. Exact route
    # match wins; otherwise substring-on-route ranks above label match.
    if tlow.startswith("/"):
        ep_exact = [
            n for n in nodes
            if n.get("kind") == "endpoint" and str(n.get("route", "")).lower() == tlow
        ]
        if ep_exact:
            return ep_exact[0]
        ep_subs = [
            n for n in nodes
            if n.get("kind") == "endpoint" and tlow in str(n.get("route", "")).lower()
        ]
        if ep_subs:
            return ep_subs[0]

    # Exact label match first.
    exact = [n for n in nodes if str(n.get("label", "")).lower() == tlow]
    if exact:
        return exact[0]
    # Substring fallback — search BOTH `label` and `route` so users can
    # paste a partial API path or function name interchangeably.
    subs = [
        n for n in nodes
        if tlow in str(n.get("label", "")).lower()
        or tlow in str(n.get("route", "")).lower()
    ]
    if not subs:
        return None
    rank = {
        "endpoint": 0, "function": 1, "struct": 2, "enum": 2, "trait": 2,
        "api_call": 3, "type_alias": 4, "const": 5, "static": 5,
        "file": 6, "module": 7, "crate": 8, "section": 9,
    }
    subs.sort(key=lambda n: (rank.get(n.get("kind"), 99), str(n.get("label"))))
    return subs[0]


def trace_flows(
    graph: dict,
    target_id: str,
    hops: int = 3,
    direction: str = "in",
    limit: int = DEFAULT_LIMIT,
    fmt: str = "graph",
) -> dict:
    """BFS the call/handle/implement edges in the requested direction
    starting from `target_id`.
        `direction="in"`   — upstream (who calls this?)
        `direction="out"`  — downstream (what does this call?)
        `direction="both"` — union of upstream and downstream — useful
                             for API endpoints where the "flow" is
                             frontend → endpoint → handler → fns.
    Per-hop layers are returned so viewers can color-code by distance
    from the centre."""
    if direction == "both":
        up = trace_flows(graph, target_id, hops, "in", limit=10_000, fmt="graph")
        down = trace_flows(graph, target_id, hops, "out", limit=10_000, fmt="graph")
        # Merge node + edge sets while preserving the smaller hop count
        # for nodes that appear in both directions.
        # Merge upstream + downstream subgraphs. Both sides already
        # produced the new payload shape (`layers`, etc.) via the
        # recursive call. Flatten their `layers` back to nodes, dedupe
        # by id keeping the smaller hop, then re-apply layering + cap.
        merged_nodes: dict[str, dict] = {}
        for layered in (up.get("layers", []), down.get("layers", [])):
            for layer in layered:
                for n in layer:
                    existing = merged_nodes.get(n["id"])
                    if existing is None or n["hop"] < existing["hop"]:
                        merged_nodes[n["id"]] = n
        merged_edges_seen: set[tuple[str, str, str]] = set()
        merged_edges: list[dict] = []
        for e in up.get("edges", []) + down.get("edges", []):
            key = (e["from"], e["to"], e["kind"])
            if key in merged_edges_seen:
                continue
            merged_edges_seen.add(key)
            merged_edges.append(e)

        all_nodes = sorted(merged_nodes.values(),
                           key=lambda p: (p["hop"], str(p.get("crate") or ""), str(p.get("label") or "")))
        truncated = len(all_nodes) > limit
        kept = all_nodes[:limit]
        kept_ids = {n["id"] for n in kept}
        kept_edges = [e for e in merged_edges if e["from"] in kept_ids and e["to"] in kept_ids]
        layers_grouped: list[list[dict]] = []
        if kept:
            max_hop = max(n["hop"] for n in kept)
            layers_grouped = [
                [n for n in kept if n["hop"] == h]
                for h in range(max_hop + 1)
            ]

        base = {
            "target": up.get("target", {}),
            "direction": "both",
            "hops": hops,
            "nodes": kept,
            "layers": layers_grouped,
            "edges": kept_edges,
            "upstream_layer_counts": up.get("layer_counts", []),
            "downstream_layer_counts": down.get("layer_counts", []),
            "layer_counts": up.get("layer_counts", []) + down.get("layer_counts", []),
            "totals": {
                "nodes_returned": len(kept),
                "edges_returned": len(kept_edges),
                "nodes_reachable": len(all_nodes),
            },
        }
        if truncated:
            base["truncated"] = True
            base["truncation_hint"] = (
                f"Returned {len(kept)} of {len(all_nodes)} reachable nodes. "
                f"Re-query with lower hops or a tighter target."
            )
        if fmt == "mermaid":
            base["mermaid"] = _build_mermaid(base)
            return base
        if fmt == "compact":
            base["compact"] = _build_compact(base)
            base.pop("nodes", None)
            base.pop("layers", None)
            base.pop("edges", None)
            return base
        return base

    edges = graph["edges"]
    # adjacency[a] = list of b such that an edge connects them in the
    # walked direction (a → b means "a leads to b in the walk").
    adjacency: dict[str, list[tuple[str, str]]] = defaultdict(list)
    for edge in edges:
        kind = edge.get("kind")
        if kind not in FLOW_EDGE_KINDS:
            continue
        src = edge.get("from")
        dst = edge.get("to")
        if not src or not dst:
            continue
        if direction == "in":
            adjacency[dst].append((src, kind))   # walking upstream
        else:
            adjacency[src].append((dst, kind))   # walking downstream

    visited = {target_id}
    layers: list[list[str]] = []
    edges_in_subgraph: set[tuple[str, str, str]] = set()
    frontier = {target_id}
    for _ in range(max(1, hops)):
        next_frontier: set[str] = set()
        for n in frontier:
            for neighbour, kind in adjacency.get(n, []):
                if direction == "in":
                    edges_in_subgraph.add((neighbour, n, kind))
                else:
                    edges_in_subgraph.add((n, neighbour, kind))
                if neighbour not in visited:
                    next_frontier.add(neighbour)
                    visited.add(neighbour)
        if not next_frontier:
            break
        layers.append(sorted(next_frontier))
        frontier = next_frontier

    nodes_by_id = {n["id"]: n for n in graph["nodes"]}
    distance_by_id = {target_id: 0}
    for h, layer in enumerate(layers, start=1):
        for nid in layer:
            distance_by_id[nid] = h

    # Build the slim node payload (drop redundant defaults so the
    # JSON size shrinks for LLM consumption). Sort by hop ascending,
    # then crate, then label, so the truncation cap at the end keeps
    # the closest-to-target nodes.
    full_nodes: list[dict] = []
    for nid in visited:
        n = nodes_by_id.get(nid)
        if not n:
            continue
        kind = n.get("kind")
        item: dict[str, object] = {
            "id": n["id"],
            "label": n.get("label"),
            "kind": kind,
            "hop": distance_by_id.get(nid, 0),
        }
        if n.get("path"):
            item["path"] = n["path"]
        if kind in LINE_BEARING_KINDS and n.get("line"):
            item["line"] = n["line"]
        if n.get("crate"):
            item["crate"] = n["crate"]
        # Only emit `module` when it actually differs from the crate
        # — saves repeating `<crate>::<filename>` on every fn.
        mod = n.get("module")
        if mod and mod != n.get("crate"):
            item["module"] = mod
        # Only emit `test:true` (false is the default, omit it).
        if n.get("test"):
            item["test"] = True
        full_nodes.append(item)
    full_nodes.sort(key=lambda p: (p["hop"], str(p.get("crate") or ""), str(p.get("label") or "")))

    # Apply node cap. Keep the closest-to-target nodes, set truncated
    # flag + total_reachable so the caller knows there's more.
    truncated = len(full_nodes) > limit
    kept_nodes = full_nodes[:limit]
    kept_ids = {n["id"] for n in kept_nodes}

    # Edges restricted to nodes that survived the cap (otherwise the
    # caller gets dangling references).
    edge_payload = [
        {"from": s, "to": t, "kind": k}
        for (s, t, k) in sorted(edges_in_subgraph)
        if s in kept_ids and t in kept_ids
    ]

    # Group by hop layer for at-a-glance reading.
    layers_grouped: list[list[dict]] = []
    if kept_nodes:
        max_hop = max(n["hop"] for n in kept_nodes)
        layers_grouped = [
            [n for n in kept_nodes if n["hop"] == h]
            for h in range(max_hop + 1)
        ]

    base = {
        "target": _slim_target(nodes_by_id.get(target_id, {"id": target_id})),
        "direction": direction,
        "hops": hops,
        # Flat list (legacy + programmatic consumers) AND grouped
        # layers (LLM-friendly at-a-glance) — same data, two shapes.
        "nodes": kept_nodes,
        "layers": layers_grouped,
        "edges": edge_payload,
        "layer_counts": [len(layer) for layer in layers],
        "totals": {
            "nodes_returned": len(kept_nodes),
            "edges_returned": len(edge_payload),
            "nodes_reachable": len(full_nodes),
        },
    }
    if truncated:
        base["truncated"] = True
        base["truncation_hint"] = (
            f"Returned {len(kept_nodes)} of {len(full_nodes)} reachable nodes. "
            f"Re-query with lower hops or a tighter target."
        )

    if fmt == "mermaid":
        base["mermaid"] = _build_mermaid(base)
        return base
    if fmt == "compact":
        base["compact"] = _build_compact(base)
        # Compact mode → drop the verbose JSON shapes; the `compact`
        # string already conveys layers + edges in one block.
        base.pop("nodes", None)
        base.pop("layers", None)
        base.pop("edges", None)
        return base
    return base


def _slim_target(node: dict) -> dict:
    """Strip the target node to just the fields a caller actually
    consults (id, label, kind, route/method for endpoints, path)."""
    keep = {"id", "label", "kind", "path", "line", "crate", "module"}
    out = {k: v for k, v in node.items() if k in keep}
    if node.get("kind") == "endpoint":
        if node.get("method"):
            out["method"] = node["method"]
        if node.get("route"):
            out["route"] = node["route"]
        if node.get("handler"):
            out["handler"] = node["handler"]
    return out


def _build_compact(base: dict) -> str:
    """One-line-per-node text representation. Each line:
        hop=<n> <kind> <label> @ <path>[:line]
    Edges listed at the end as `<from-label> --<kind>--> <to-label>`.
    Designed to be paste-into-LLM friendly — typically ~100 chars/line."""
    by_id = {}
    lines = []
    target = base.get("target", {})
    lines.append(f"target: {target.get('label', '?')} [{target.get('kind', '?')}]")
    if target.get("path"):
        loc = target["path"]
        if target.get("line"):
            loc += f":{target['line']}"
        lines.append(f"  at {loc}")
    if target.get("route"):
        lines.append(f"  route: {target.get('method', '?')} {target['route']}")
    lines.append(f"direction: {base.get('direction', '?')}  hops: {base.get('hops', '?')}")
    totals = base.get("totals", {})
    lines.append(
        f"reach: {totals.get('nodes_returned', 0)} returned"
        f" of {totals.get('nodes_reachable', 0)} reachable"
        f"  · {totals.get('edges_returned', 0)} edges"
    )
    if base.get("truncated"):
        lines.append(f"  truncated: {base.get('truncation_hint', '')}")
    lines.append("")
    for layer in base.get("layers", []):
        if not layer:
            continue
        lines.append(f"hop {layer[0]['hop']}:")
        for n in layer:
            by_id[n["id"]] = n
            loc = n.get("path", "?")
            if n.get("line"):
                loc += f":{n['line']}"
            extras = []
            if n.get("test"):
                extras.append("test")
            extra = f"  ({', '.join(extras)})" if extras else ""
            lines.append(f"  - {n.get('kind', '?')}: {n.get('label', '?')}  @ {loc}{extra}")
    if base.get("edges"):
        lines.append("")
        lines.append("edges:")
        for e in base["edges"]:
            src = by_id.get(e["from"], {}).get("label", e["from"])
            dst = by_id.get(e["to"], {}).get("label", e["to"])
            lines.append(f"  {src} --{e['kind']}--> {dst}")
    return "\n".join(lines)


def _build_mermaid(base: dict) -> str:
    """Mermaid flowchart source for the flow. Mirrors the JS builder
    in `app.js::buildCommandMermaidSource('flows', …)` so server-side
    consumers get the exact diagram the 2D viewer renders."""
    target = base.get("target") or {}
    layers = base.get("layers") or []
    edges = base.get("edges") or []
    lines = ["flowchart LR"]
    alias_by_id: dict[str, str] = {}
    counter = 0

    def safe(s):
        return str(s or "?").replace('"', "'")[:60]

    def shape(node):
        k = node.get("kind") or ""
        lbl = safe(node.get("label") or node.get("id") or "?")
        if k == "endpoint":
            return f'(["{lbl}"])'
        if k == "function":
            return f'("{lbl}")'
        if k == "api_call":
            return f'>"{lbl}"]'
        if k in ("struct", "enum", "trait"):
            return '{{"' + lbl + '"}}'
        if k == "file":
            return f'[/"{lbl}"/]'
        if k in ("crate", "module"):
            return '[\\"' + lbl + '"\\]'
        return f'["{lbl}"]'

    def alias(node_id):
        nonlocal counter
        if node_id not in alias_by_id:
            counter += 1
            alias_by_id[node_id] = f"n{counter}"
        return alias_by_id[node_id]

    target_id = target.get("id")
    for layer in layers:
        for n in layer:
            lines.append(f"  {alias(n['id'])}{shape(n)}")
            cls = "flow-target" if n.get("id") == target_id else f"flow-hop-{min(3, n.get('hop') or 1)}"
            lines.append(f"  class {alias(n['id'])} {cls}")
    for e in edges:
        if e["from"] in alias_by_id and e["to"] in alias_by_id:
            lines.append(f"  {alias_by_id[e['from']]} -.->|{safe(e['kind'])}| {alias_by_id[e['to']]}")
    lines.append("  classDef flow-target fill:#fff3f3,stroke:#b71c1c,stroke-width:2.5px,color:#3a0606,font-weight:bold")
    lines.append("  classDef flow-hop-1 fill:#e3eefc,stroke:#0a6cd0,stroke-width:1.5px,color:#0a2a55")
    lines.append("  classDef flow-hop-2 fill:#eef3fa,stroke:#3a6a8a,stroke-width:1.2px,color:#1a3a55")
    lines.append("  classDef flow-hop-3 fill:#f4f6f9,stroke:#6a7a8a,stroke-width:1px,color:#2a3a4a")
    return "\n".join(lines)


# ── CLI ──────────────────────────────────────────────────────────────


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--graph", default="docs/codegraph/graph.json")
    parser.add_argument("--target", required=True,
                        help="Node label, partial label, or node id")
    parser.add_argument("--hops", type=int, default=3)
    parser.add_argument("--direction", choices=("in", "out", "both"), default="in")
    parser.add_argument("--limit", type=int, default=DEFAULT_LIMIT,
                        help="Max nodes returned (default 120)")
    parser.add_argument("--format", choices=("graph", "compact", "mermaid"),
                        default="graph", dest="fmt")
    args = parser.parse_args()

    script_root = Path(__file__).resolve().parents[1]
    gpath = (script_root / args.graph).resolve() if not Path(args.graph).is_absolute() else Path(args.graph)
    if not gpath.exists():
        print(f"graph not found: {gpath}")
        return 2

    graph = json.loads(gpath.read_text(encoding="utf-8"))
    node = resolve_target(graph, args.target)
    if not node:
        print(f"No node matched '{args.target}'")
        return 1

    result = trace_flows(
        graph, node["id"],
        hops=args.hops, direction=args.direction,
        limit=args.limit, fmt=args.fmt,
    )
    if args.fmt == "compact":
        print(result.get("compact", ""))
    elif args.fmt == "mermaid":
        print(result.get("mermaid", ""))
    else:
        print(json.dumps(result, indent=2, default=str))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
