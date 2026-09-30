#!/usr/bin/env python3
"""MCP server for the Magician code graph.

Exposes the code graph query tools via Model Context Protocol so that
Claude Code, Codex, and other MCP clients can query the codebase
structure, capabilities, endpoints, and architecture.

Start:
  python3 docs/codegraph/mcp_server.py

Configure in Codex project config (.codex/config.toml):
  [mcp_servers.codegraph]
  command = "python3"
  args = ["docs/codegraph/mcp_server.py"]
  required = true

Configure in clients that run the server from the repo root:
  "codegraph": {
    "type": "stdio",
    "command": "python3",
    "args": ["docs/codegraph/mcp_server.py"]
  }

If a client cannot run the server from the repo root, use an absolute script
path only as a local override rather than committing machine-specific paths.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from mcp.server.fastmcp import FastMCP

CODEGRAPH_DIR = Path(__file__).resolve().parent
REPO_ROOT = CODEGRAPH_DIR.parents[1]
SCRIPTS_DIR = REPO_ROOT / "scripts"
DEFAULT_GRAPH = CODEGRAPH_DIR / "graph.json"

sys.path.insert(0, str(SCRIPTS_DIR))
import query_code_graph as gq  # noqa: E402
import find_dead_code as dc  # noqa: E402
import test_coverage as tc  # noqa: E402
import find_flows as flows_mod  # noqa: E402
import audit_code_graph as audit_mod  # noqa: E402

# ── Codegraph extensions: load + register their MCP tools ─────────
try:
    from codegraph_ext import load_extensions
    _extensions = load_extensions()
except Exception as exc:
    print(f"[mcp_server] extension loader failed: {exc}", file=sys.stderr)
    _extensions = []

mcp = FastMCP(
    "codegraph",
    instructions=(
        "Code graph intelligence for the Magician workspace. "
        "Query codebase structure, capabilities, endpoints, and architecture concepts. "
        "Use 'cgraph_how' for blueprints and architecture explanations. "
        "Use 'cgraph_detail' to understand a crate's contents. "
        "Use 'cgraph_skills' (magician_skills extension) to list tools / agents / procedure-skills / personalities. "
        "Use 'cgraph_endpoints' to see API routes. "
        "Use 'cgraph_search' for keyword search across all nodes."
    ),
)

# Cache the graph in memory — reload on file change
_graph_cache: dict = {}
_graph_mtime: float = 0.0


def _load_graph() -> dict:
    global _graph_cache, _graph_mtime
    try:
        mt = DEFAULT_GRAPH.stat().st_mtime
    except OSError:
        mt = 0.0
    if mt != _graph_mtime or not _graph_cache:
        _graph_cache = gq.load_graph(DEFAULT_GRAPH)
        _graph_mtime = mt
    return _graph_cache


def _ns(**kwargs) -> argparse.Namespace:
    defaults = dict(
        graph=str(DEFAULT_GRAPH),
        how=None, query=None, pattern=None,
        endpoints=True, capabilities=True,
        kind=None, limit=20, depth=2, crate=None,
    )
    defaults.update(kwargs)
    return argparse.Namespace(**defaults)


@mcp.tool()
def cgraph_how(topic: str) -> str:
    """Show how something works in the codebase.

    For capability names (telegram, tavily_search, etc.) → returns a blueprint:
    files to create, functions to implement, SDK interfaces, YAML template.

    For architecture concepts (approval, memory_tier, agent, etc.) → returns
    an architecture explanation: key types, primary files, endpoints, contracts,
    crate distribution, and cross-references.

    Examples:
      cgraph_how("telegram")     → blueprint for adding a channel adapter
      cgraph_how("approval")     → approval flow: structs, functions, endpoints, contracts
      cgraph_how("memory_tier")  → memory tier architecture: types, enums, retention modes
      cgraph_how("agent")        → agent system: 334 structs, 7 endpoints, full contracts
    """
    graph = _load_graph()
    result = gq.cmd_how(_ns(how=topic), graph)
    return json.dumps(result, indent=2, default=str)


@mcp.tool()
def cgraph_detail(crate: str) -> str:
    """Show the full contents of a crate: files, symbols, internal edges,
    cross-crate connections, and relevant contracts.

    Examples:
      cgraph_detail("bot-sdk")        → 6 files, 20 functions, 5 classes
      cgraph_detail("python-tools")   → 11 Python tool files with API calls
      cgraph_detail("capabilities")   → 45 YAML packs as trait nodes
      cgraph_detail("magician")       → the main Rust crate
    """
    graph = _load_graph()
    result = gq.cmd_pattern(_ns(pattern=crate), graph)
    return json.dumps(result, indent=2, default=str)


# `cgraph_capabilities` retired — the legacy `capabilities` Rust crate
# no longer exists. The magician_skills extension now exposes the
# equivalent (and richer) `cgraph_skills` tool via the
# `scripts/codegraph_ext/` plugin system.


@mcp.tool()
def cgraph_endpoints(crate: str = "") -> str:
    """List all API endpoints with handlers, payload types, and callers.

    Optionally filter by crate name.

    Examples:
      cgraph_endpoints()            → all 68 endpoints
      cgraph_endpoints("magician")  → only magician crate endpoints
    """
    graph = _load_graph()
    result = gq.cmd_endpoints(_ns(crate=crate or None), graph)
    return json.dumps(result, indent=2, default=str)


@mcp.tool()
def cgraph_search(query: str, kind: str = "", depth: int = 2, limit: int = 20) -> str:
    """Search for nodes in the code graph by keyword.

    Matches against node labels, IDs, paths, modules, and crates.
    Returns matched nodes with their neighbors at the specified depth.

    Args:
      query: Search terms (case-insensitive substring match)
      kind: Optional node kind filter (function, struct, endpoint, trait, etc.)
      depth: Neighbor expansion depth (default 2)
      limit: Max results (default 20)
    """
    graph = _load_graph()
    result = gq.cmd_query(
        _ns(query=query, kind=kind or None, depth=depth, limit=limit),
        graph,
    )
    return json.dumps(result, indent=2, default=str)


@mcp.tool()
def cgraph_dead_code(crate: str = "", tier: str = "", limit: int = 50) -> str:
    """List functions with no production callers (dead-code candidates).

    Production-alive signals are `calls`, `handles`, and `implements`
    edges. `test_calls` edges (calls from test code, identified
    structurally by `#[test]` / pytest convention / `*.test.ts` /
    `*Tests.swift`, or Swift `*Tests` / `*UITests` targets) are tracked separately and surfaced as
    `tcalls=N` info — they do NOT keep a function alive for
    dead-code purposes.

    Tiers:
      A — private fn (no production callers, not a test)
      C — public fn (may be called externally — review with caution)

    Args:
      crate: optional crate-name filter (e.g. "magician")
      tier:  optional, one of "A" / "C"
      limit: max candidates returned (default 50)
    """
    word_counts = dc.build_word_occurrence_counts(REPO_ROOT)
    buckets = dc.analyze(DEFAULT_GRAPH, crate or None, word_counts)
    return dc.render_report(buckets, tier or None, limit)


@mcp.tool()
def cgraph_test_coverage(crate: str = "", bucket: str = "", limit: int = 100) -> str:
    """Structural test-coverage report from the call graph.

    For every production function, counts incoming `test_calls`
    edges and buckets the result:
      untested    — has production callers, zero test callers (real gap)
      light       — 1 test caller
      moderate    — 2–5 test callers
      well        — 6+ test callers
      only_tested — 0 production callers, ≥1 test callers (may belong in tests/)

    Idiomatic methods (`new`, `default`, `from_*`, `as_*`, etc.) are
    excluded — they're tested implicitly by any code path that
    exercises the type.

    Args:
      crate:  optional crate filter
      bucket: optional, one of "untested" / "light" / "moderate" / "well" / "only_tested"
      limit:  max functions returned (default 100)
    """
    buckets = tc.analyze(DEFAULT_GRAPH, crate or None)
    return tc.render(buckets, bucket or None, limit)


@mcp.tool()
def cgraph_flows(
    target: str,
    hops: int = 3,
    direction: str = "in",
    limit: int = 120,
    format: str = "compact",
) -> str:
    """Trace incoming / outgoing / both-direction flows for any node
    (function, crate, module, file, endpoint, struct, …).

    Walks `calls`, `handles`, `implements`, `calls_api`, and
    `targets_endpoint` edges up to `hops` deep starting from the
    resolved target.

    Args:
      target:    Label, partial label, exact node id, or API path
                 (e.g. `/api/magician/v3/tasks` — matches endpoint route).
      hops:      BFS depth (default 3).
      direction: "in"   = who calls/handles this (upstream);
                 "out"  = what this reaches (downstream);
                 "both" = end-to-end (useful for API endpoints).
      limit:     Max nodes in the response (default 120). When the
                 reachable set is larger, the response carries
                 `truncated: true` + `truncation_hint`.
      format:    "compact" = one-line-per-node text body in `.compact`
                            (LLM-friendly, ~100 chars/line);
                 "graph"   = full JSON `layers`/`edges`;
                 "mermaid" = mermaid flowchart source in `.mermaid`.

    `compact` is the default for LLM callers — paste-friendly text
    sized for normal context windows. Use `graph` when you need to
    walk the structure programmatically; use `mermaid` to render or
    relay the diagram.
    """
    graph = _load_graph()
    node = flows_mod.resolve_target(graph, target)
    if not node:
        return json.dumps({"error": "no_match", "target": target})
    result = flows_mod.trace_flows(
        graph, node["id"],
        hops=hops, direction=direction,
        limit=limit, fmt=format,
    )
    return json.dumps(result, indent=2, default=str)


@mcp.tool()
def cgraph_audit(verbose: bool = False) -> str:
    """Audit code-graph coverage against the repo filesystem.

    Re-walks the repo with the same exclusion rules `generate_code_graph.py`
    uses, then diffs filesystem ground-truth against the file nodes in
    `graph.json`. Per-extension breakdown, manifest-dir coverage, and
    sample missing / unexpected file lists.

    Args:
      verbose: If False (default), the per-extension `missing` and
               `unexpected` arrays are truncated to the first 20
               entries each (with total counts preserved) — keeps the
               payload LLM-context-friendly. Set True to get every
               file path explicitly.

    Returns:
      JSON with keys: `by_extension`, `manifests`,
      `unique_manifest_dirs`, `crate_node_count`,
      `uncovered_manifest_dirs`, `totals`, `ok`.
    """
    try:
        result = audit_mod.compute_audit(REPO_ROOT, DEFAULT_GRAPH)
    except FileNotFoundError as exc:
        return json.dumps({"error": "graph_not_found", "message": str(exc)})
    except Exception as exc:
        return json.dumps({"error": "audit_failed", "message": str(exc)})

    if not verbose:
        # Trim long file-list arrays so a clean repo's payload stays
        # tiny and a dirty one still fits without flooding context.
        CAP = 20
        for row in result.get("by_extension", []):
            missing = row.get("missing", [])
            unexpected = row.get("unexpected", [])
            row["missing_total"] = len(missing)
            row["unexpected_total"] = len(unexpected)
            row["missing"] = missing[:CAP]
            row["unexpected"] = unexpected[:CAP]
        un = result.get("uncovered_manifest_dirs", [])
        result["uncovered_manifest_dirs_total"] = len(un)
        result["uncovered_manifest_dirs"] = un[:CAP]
    return json.dumps(result, indent=2, default=str)


# Register every extension-provided tool. Each tool descriptor is
# `{"fn": callable, "description": str}`; the callable's signature
# drives FastMCP's argument schema.
for _ext in _extensions:
        for _tool_name, _tool_spec in _ext.mcp_tools().items():
            try:
                _fn = _tool_spec.get("fn")
                _desc = _tool_spec.get("description", "")
                if not callable(_fn):
                    continue
                # Preserve the signature so FastMCP infers args properly.
                try:
                    _fn.__doc__ = _fn.__doc__ or _desc
                except AttributeError:
                    pass
                mcp.tool(name=_tool_name, description=_desc)(_fn)
            except Exception as exc:
                pass  # Skip silently to avoid breaking MCP stdio handshake


if __name__ == "__main__":
    mcp.run()
