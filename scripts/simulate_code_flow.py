#!/usr/bin/env python3
"""Static flow simulation over docs/codegraph artifacts.

Phase 1 goals:
- Select a source node (typically an endpoint)
- Start with a mock or user-supplied payload
- Traverse directed flow edges to discover reachable sinks
- Emit trace paths and simple risk findings
"""

from __future__ import annotations

import argparse
import json
from collections import deque
from pathlib import Path
from typing import Any

FLOW_EDGE_KINDS = {
    "handles",
    "calls",
    "calls_api",
    "targets_endpoint",
    "accepts_payload",
    "references",
    "depends_on",
}

SINK_KINDS = {"api_call", "endpoint"}
SANITIZER_TOKENS = {
    "sanitize",
    "sanitizer",
    "validate",
    "validator",
    "redact",
    "mask",
    "hash",
    "escape",
    "encode",
    "normalize",
}
PROJECT_SANITIZER_HINTS = {
    "strip_sensitive",
    "strip_secrets",
    "safe_log",
    "filter_pii",
}


def load_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def index_nodes(graph: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {str(node.get("id")): node for node in graph.get("nodes", []) if node.get("id")}


def build_outgoing_edges(graph: dict[str, Any]) -> dict[str, list[tuple[str, str]]]:
    out: dict[str, list[tuple[str, str]]] = {}
    for edge in graph.get("edges", []):
        src = str(edge.get("from", "")).strip()
        dst = str(edge.get("to", "")).strip()
        kind = str(edge.get("kind", "")).strip()
        if not src or not dst or not kind:
            continue
        out.setdefault(src, []).append((dst, kind))
    return out


def build_incoming_edges(graph: dict[str, Any]) -> dict[str, list[tuple[str, str]]]:
    incoming: dict[str, list[tuple[str, str]]] = {}
    for edge in graph.get("edges", []):
        src = str(edge.get("from", "")).strip()
        dst = str(edge.get("to", "")).strip()
        kind = str(edge.get("kind", "")).strip()
        if not src or not dst or not kind:
            continue
        incoming.setdefault(dst, []).append((src, kind))
    return incoming


def _collect_tags(value: Any, key_hint: str = "") -> set[str]:
    tags: set[str] = set()
    hint = key_hint.lower()

    if any(token in hint for token in ("token", "secret", "password", "api_key", "apikey")):
        tags.add("secret")
    if any(token in hint for token in ("email", "phone", "address", "name", "ssn")):
        tags.add("pii")

    if isinstance(value, dict):
        tags.add("user_input")
        for key, item in value.items():
            tags |= _collect_tags(item, f"{key_hint}.{key}" if key_hint else str(key))
    elif isinstance(value, list):
        tags.add("user_input")
        for item in value:
            tags |= _collect_tags(item, key_hint)
    elif isinstance(value, str):
        tags.add("user_input")
        lowered = value.lower()
        if "bearer" in lowered or "token" in lowered:
            tags.add("secret")
    elif value is not None:
        tags.add("user_input")

    return tags


def infer_payload_tags(payload: Any) -> set[str]:
    return _collect_tags(payload)


def node_label(node: dict[str, Any]) -> str:
    return str(node.get("label") or node.get("id") or "unknown")


def _path_to_sink(
    sink_id: str,
    predecessors: dict[str, tuple[str, str]],
    nodes_by_id: dict[str, dict[str, Any]],
) -> dict[str, Any]:
    node_chain: list[str] = [sink_id]
    edge_chain: list[str] = []

    cursor = sink_id
    while cursor in predecessors:
        prev, edge_kind = predecessors[cursor]
        node_chain.append(prev)
        edge_chain.append(edge_kind)
        cursor = prev

    node_chain.reverse()
    edge_chain.reverse()

    path_nodes = []
    for node_id in node_chain:
        node = nodes_by_id.get(node_id, {"id": node_id, "kind": "unknown", "label": node_id})
        path_nodes.append(
            {
                "id": node_id,
                "kind": str(node.get("kind", "unknown")),
                "label": node_label(node),
                "crate": str(node.get("crate", "")),
                "module": str(node.get("module", "")),
                "path": str(node.get("path", "")),
                "line": int(node.get("line", 0)) if str(node.get("line", "")).strip() else 0,
            }
        )

    return {
        "node_ids": node_chain,
        "edge_kinds": edge_chain,
        "nodes": path_nodes,
        "depth": max(0, len(node_chain) - 1),
    }


def detect_trace_sanitizers(trace: dict[str, Any]) -> list[str]:
    matches: list[str] = []
    seen: set[str] = set()
    for node in trace.get("nodes", []):
        node_label = str(node.get("label", "")).lower()
        node_id = str(node.get("id", "")).lower()
        for token in SANITIZER_TOKENS | PROJECT_SANITIZER_HINTS:
            if token in node_label or token in node_id:
                if token not in seen:
                    seen.add(token)
                    matches.append(token)
    return matches


def trace_confidence_score(trace: dict[str, Any]) -> float:
    node_count = len(trace.get("nodes", []))
    edge_kinds = [str(kind) for kind in trace.get("edge_kinds", [])]
    reverse_count = sum(1 for kind in edge_kinds if kind.startswith("reverse:"))
    unknown_nodes = sum(1 for node in trace.get("nodes", []) if str(node.get("kind", "")) == "unknown")

    score = 0.46
    if node_count > 0:
        first_kind = str(trace.get("nodes", [])[0].get("kind", ""))
        last_kind = str(trace.get("nodes", [])[-1].get("kind", ""))
        if first_kind == "endpoint":
            score += 0.16
        if last_kind in {"api_call", "endpoint"}:
            score += 0.12
    if "handles" in edge_kinds:
        score += 0.1
    if "calls" in edge_kinds:
        score += 0.06
    if "calls_api" in edge_kinds:
        score += 0.1
    if "targets_endpoint" in edge_kinds:
        score += 0.06

    score -= min(0.3, reverse_count * 0.05)
    score -= min(0.2, unknown_nodes * 0.04)
    depth = int(trace.get("depth", max(0, node_count - 1)))
    if depth > 7:
        score -= min(0.18, (depth - 7) * 0.02)

    return max(0.05, min(0.99, round(score, 3)))


def confidence_label(score: float) -> str:
    if score >= 0.78:
        return "high"
    if score >= 0.55:
        return "medium"
    return "low"


def annotate_trace(trace: dict[str, Any]) -> dict[str, Any]:
    score = trace_confidence_score(trace)
    trace["confidence_score"] = score
    trace["confidence_label"] = confidence_label(score)
    trace["sanitizer_hits"] = detect_trace_sanitizers(trace)
    return trace


def evaluate_trace_risk(trace: dict[str, Any], tags: set[str]) -> dict[str, Any]:
    sink = trace["nodes"][-1] if trace.get("nodes") else {"kind": "unknown", "label": "unknown"}
    sink_kind = str(sink.get("kind", "unknown"))
    sanitizer_hits = list(trace.get("sanitizer_hits") or [])
    has_sanitizer = len(sanitizer_hits) > 0
    conf_score = float(trace.get("confidence_score", 0.5))
    conf_label = str(trace.get("confidence_label", confidence_label(conf_score)))

    if sink_kind == "api_call":
        if "secret" in tags and not has_sanitizer:
            severity = "high"
            message = "Secret-tagged payload reached outbound API call without sanitizer in trace."
        elif "pii" in tags and not has_sanitizer:
            severity = "high"
            message = "PII-tagged payload reached outbound API call without sanitizer in trace."
        elif "user_input" in tags and not has_sanitizer:
            severity = "medium"
            message = "User input reached outbound API call without sanitizer in trace."
        else:
            severity = "low"
            message = "Outbound API call reachable; sanitizer or low-risk payload tags detected."
    elif sink_kind == "endpoint":
        severity = "low"
        message = "Flow reached another endpoint (possible internal API hop)."
    else:
        severity = "info"
        message = "Reachable node observed."

    if has_sanitizer and severity in {"high", "medium"}:
        severity = "medium" if severity == "high" else "low"
        message = f"{message} Sanitizer indicators present in trace."

    if conf_label == "low" and severity in {"high", "medium"}:
        message = f"{message} Confidence is low due to weak static linkage."

    return {
        "severity": severity,
        "message": message,
        "sink": sink,
        "depth": trace.get("depth", 0),
        "confidence_score": conf_score,
        "confidence_label": conf_label,
        "sanitizer_hits": sanitizer_hits,
    }


def list_sources(graph: dict[str, Any], payload_index: dict[str, Any]) -> list[dict[str, Any]]:
    nodes_by_id = index_nodes(graph)
    profile_by_source = {
        str(profile.get("source_id")): profile
        for profile in payload_index.get("profiles", [])
        if profile.get("source_id")
    }

    sources: list[dict[str, Any]] = []
    for node in graph.get("nodes", []):
        if str(node.get("kind")) != "endpoint":
            continue
        source_id = str(node.get("id"))
        profile = profile_by_source.get(source_id, {})
        sources.append(
            {
                "id": source_id,
                "label": node_label(node),
                "method": str(node.get("method") or profile.get("method") or ""),
                "route": str(node.get("route") or profile.get("route") or ""),
                "handler": str(node.get("handler") or profile.get("handler") or ""),
                "path": str(node.get("path") or profile.get("path") or ""),
                "line": int(node.get("line") or profile.get("line") or 0),
                "has_mock_payload": bool(profile.get("payload_template")),
            }
        )

    sources.sort(key=lambda item: (item.get("route", ""), item.get("method", ""), item.get("id", "")))
    return sources


def mock_payload_for_source(payload_index: dict[str, Any], source_id: str) -> dict[str, Any]:
    for profile in payload_index.get("profiles", []):
        if str(profile.get("source_id")) == source_id:
            template = profile.get("payload_template")
            if isinstance(template, dict):
                return template
            break
    return {"json": {}, "query": {}, "path": {}}


def simulate_flow(
    graph: dict[str, Any],
    payload_index: dict[str, Any],
    source_id: str,
    payload: Any,
    max_hops: int = 6,
    max_traces: int = 12,
) -> dict[str, Any]:
    nodes_by_id = index_nodes(graph)
    if source_id not in nodes_by_id:
        raise ValueError(f"Unknown source_id: {source_id}")

    outgoing = build_outgoing_edges(graph)
    incoming = build_incoming_edges(graph)
    tags = infer_payload_tags(payload)

    queue: deque[tuple[str, int]] = deque([(source_id, 0)])
    visited: dict[str, int] = {source_id: 0}
    predecessors: dict[str, tuple[str, str]] = {}
    sinks: list[str] = []

    while queue:
        node_id, depth = queue.popleft()
        if depth >= max_hops:
            continue

        for next_id, edge_kind in outgoing.get(node_id, []):
            if edge_kind not in FLOW_EDGE_KINDS:
                continue

            next_depth = depth + 1
            seen_depth = visited.get(next_id)
            if seen_depth is not None and seen_depth <= next_depth:
                continue

            visited[next_id] = next_depth
            predecessors[next_id] = (node_id, edge_kind)
            queue.append((next_id, next_depth))

            next_node = nodes_by_id.get(next_id)
            if not next_node:
                continue
            next_kind = str(next_node.get("kind", ""))
            if next_kind in SINK_KINDS and next_id != source_id:
                sinks.append(next_id)

    unique_sinks: list[str] = []
    sink_seen: set[str] = set()
    for sink_id in sinks:
        if sink_id in sink_seen:
            continue
        sink_seen.add(sink_id)
        unique_sinks.append(sink_id)

    if not unique_sinks:
        terminal_candidates: list[tuple[int, str]] = []
        for node_id, depth in visited.items():
            if node_id == source_id:
                continue
            forward_neighbors = [
                (next_id, edge_kind)
                for next_id, edge_kind in outgoing.get(node_id, [])
                if edge_kind in FLOW_EDGE_KINDS
            ]
            if forward_neighbors:
                continue
            terminal_candidates.append((depth, node_id))

        terminal_candidates.sort(key=lambda item: (item[0], item[1]))
        for _depth, node_id in terminal_candidates:
            if node_id in sink_seen:
                continue
            sink_seen.add(node_id)
            unique_sinks.append(node_id)
            if len(unique_sinks) >= max_traces:
                break

    traces = [
        annotate_trace(_path_to_sink(sink_id, predecessors, nodes_by_id))
        for sink_id in unique_sinks[:max_traces]
    ]
    findings = [evaluate_trace_risk(trace, tags) for trace in traces]
    findings.sort(key=lambda item: ({"high": 0, "medium": 1, "low": 2}.get(item["severity"], 3), item["depth"]))

    upstream_queue: deque[tuple[str, int]] = deque([(source_id, 0)])
    upstream_visited: dict[str, int] = {source_id: 0}
    upstream_predecessors: dict[str, tuple[str, str]] = {}

    while upstream_queue:
        node_id, depth = upstream_queue.popleft()
        if depth >= max_hops:
            continue

        for prev_id, edge_kind in incoming.get(node_id, []):
            if edge_kind not in FLOW_EDGE_KINDS:
                continue
            next_depth = depth + 1
            seen_depth = upstream_visited.get(prev_id)
            if seen_depth is not None and seen_depth <= next_depth:
                continue
            upstream_visited[prev_id] = next_depth
            upstream_predecessors[prev_id] = (node_id, f"reverse:{edge_kind}")
            upstream_queue.append((prev_id, next_depth))

    upstream_candidates: list[tuple[int, int, str]] = []
    for node_id, depth in upstream_visited.items():
        if node_id == source_id:
            continue
        node = nodes_by_id.get(node_id, {})
        kind = str(node.get("kind", ""))
        priority = 0 if kind == "api_call" else 1 if kind == "function" else 2
        upstream_candidates.append((priority, depth, node_id))

    upstream_candidates.sort(key=lambda item: (item[0], item[1], item[2]))
    upstream_sink_ids = [node_id for _, _, node_id in upstream_candidates[:max_traces]]
    upstream_traces = [
        annotate_trace(_path_to_sink(node_id, upstream_predecessors, nodes_by_id))
        for node_id in upstream_sink_ids
    ]

    source_node = nodes_by_id[source_id]
    confidence_counts: dict[str, int] = {"high": 0, "medium": 0, "low": 0}
    for trace in [*traces, *upstream_traces]:
        label = str(trace.get("confidence_label", "low"))
        if label in confidence_counts:
            confidence_counts[label] += 1
    return {
        "source": {
            "id": source_id,
            "label": node_label(source_node),
            "kind": str(source_node.get("kind", "")),
            "method": str(source_node.get("method", "")),
            "route": str(source_node.get("route", "")),
        },
        "mock_payload": mock_payload_for_source(payload_index, source_id),
        "input_payload": payload,
        "tags": sorted(tags),
        "max_hops": max_hops,
        "max_traces": max_traces,
        "visited_node_count": len(visited),
        "reachable_sink_count": len(unique_sinks),
        "traces": traces,
        "upstream_trace_count": len(upstream_traces),
        "upstream_traces": upstream_traces,
        "findings": findings,
        "confidence_summary": confidence_counts,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="Simulate static data-flow paths from a graph source")
    parser.add_argument("--graph", default="docs/codegraph/graph.json", help="Path to graph JSON")
    parser.add_argument(
        "--payload-profiles",
        default="docs/codegraph/payload_profiles.json",
        help="Path to payload profile JSON",
    )
    parser.add_argument("--source-id", required=True, help="Source node id (typically endpoint::...)" )
    parser.add_argument("--max-hops", type=int, default=6, help="Maximum directed traversal hops")
    parser.add_argument("--max-traces", type=int, default=12, help="Maximum downstream/upstream traces to return")
    parser.add_argument(
        "--payload-json",
        default=None,
        help="Inline JSON payload override (defaults to source mock payload)",
    )
    parser.add_argument(
        "--mock-only",
        action="store_true",
        help="Print only the source mock payload and exit",
    )
    args = parser.parse_args()

    graph = load_json(Path(args.graph).resolve())
    payload_index = load_json(Path(args.payload_profiles).resolve())

    if args.mock_only:
        print(json.dumps(mock_payload_for_source(payload_index, args.source_id), indent=2))
        return 0

    if args.payload_json:
        payload = json.loads(args.payload_json)
    else:
        payload = mock_payload_for_source(payload_index, args.source_id)

    result = simulate_flow(
        graph=graph,
        payload_index=payload_index,
        source_id=args.source_id,
        payload=payload,
        max_hops=max(1, int(args.max_hops)),
        max_traces=max(1, int(args.max_traces)),
    )
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
