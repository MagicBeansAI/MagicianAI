"""Project-specific extension: index the Tauri desktop app's config
details that the core walker only sees as opaque JSON files.

What it surfaces:

- One `tauri_app` node per `tauri.conf.json` carrying the rich app
  metadata (productName, identifier, version, build URLs, bundle
  targets, plugin names, window labels, etc.).
- One `tauri_capability` node per `<src-tauri>/capabilities/*.json`
  carrying the capability id, description, window scopes, and the
  flat list of permission strings it grants.
- One `tauri_icon` node per file under `<src-tauri>/icons/`.
- `defines` edges from each new node to its underlying file node so
  the graph stays internally consistent.

Why this is project-specific: only repositories with a Tauri desktop
crate have these files. The core walker does index them as plain
`file` nodes but doesn't look inside — the rich detail (productName,
permissions, etc.) would be invisible without this extension.

Exposed via:
- MCP tool   : `cgraph_tauri(kind="" | "app" | "capability" | "icon")`
- HTTP route : `/api/tauri?kind=...`
- CLI subcmd : `python3 scripts/codegraph_ext_cli.py tauri --kind capability`
- Slash      : `/tauri [kind]`
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any, Callable, Iterator
from urllib.parse import parse_qs

from codegraph_ext import CodegraphExtension


# Allow-list of attributes the extension is willing to pull out of
# `tauri.conf.json`. Anything else is considered out of scope to
# keep secret-leak risk low (CSP rules, etc., are public but we
# don't need them in the graph).
SAFE_APP_KEYS = ("productName", "identifier", "version")
SAFE_BUILD_KEYS = ("frontendDist", "devUrl", "beforeDevCommand", "beforeBuildCommand")
SAFE_BUNDLE_KEYS = ("active", "targets", "publisher", "category",
                    "shortDescription", "icon")


def _flatten_perms(node: Any, out: list[str]) -> None:
    """Tauri capability permissions can be plain strings or objects
    like `{"identifier": "...", "allow": [...]}`. Collect the
    identifiers either way."""
    if isinstance(node, str):
        out.append(node)
    elif isinstance(node, dict):
        ident = node.get("identifier")
        if isinstance(ident, str):
            out.append(ident)
    elif isinstance(node, list):
        for item in node:
            _flatten_perms(item, out)


class TauriConfigExtension(CodegraphExtension):
    name = "tauri_config"

    # ── Hook: extra graph nodes ─────────────────────────────────────

    def discover_nodes(self, root: Path, graph_data: dict[str, Any]) -> Iterator[dict]:
        for conf in root.rglob("tauri.conf.json"):
            try:
                data = json.loads(conf.read_text(encoding="utf-8"))
            except (OSError, ValueError):
                continue
            app_dir = conf.parent
            app_rel = str(conf.relative_to(root))
            app_id = data.get("identifier") or app_dir.name
            label = data.get("productName") or app_id

            # ── Top-level app node ──────────────────────────────────
            app_node: dict[str, Any] = {
                "id": f"tauri::app::{app_id}",
                "kind": "tauri_app",
                "label": label,
                "path": app_rel,
            }
            for k in SAFE_APP_KEYS:
                if data.get(k):
                    app_node[k] = data[k]
            build = data.get("build") or {}
            build_summary = {k: build[k] for k in SAFE_BUILD_KEYS if k in build}
            if build_summary:
                app_node["build"] = build_summary
            bundle = data.get("bundle") or {}
            bundle_summary = {k: bundle[k] for k in SAFE_BUNDLE_KEYS if k in bundle}
            if bundle_summary:
                app_node["bundle"] = bundle_summary
            windows_raw = (data.get("app") or {}).get("windows") or []
            window_summaries = []
            for w in windows_raw:
                if not isinstance(w, dict):
                    continue
                window_summaries.append({
                    "label": w.get("label") or "?",
                    "title": w.get("title") or "",
                    "url":   w.get("url") or "",
                    "width": w.get("width"),
                    "height": w.get("height"),
                })
            if window_summaries:
                app_node["windows"] = window_summaries
            plugins = data.get("plugins") or {}
            if plugins:
                app_node["plugins"] = sorted(plugins.keys())
            yield app_node

            # ── Capability nodes (peer JSONs in capabilities/) ──────
            cap_dir = app_dir / "capabilities"
            if cap_dir.is_dir():
                for cap_path in sorted(cap_dir.glob("*.json")):
                    try:
                        cap = json.loads(cap_path.read_text(encoding="utf-8"))
                    except (OSError, ValueError):
                        continue
                    cap_id = cap.get("identifier") or cap_path.stem
                    perms: list[str] = []
                    _flatten_perms(cap.get("permissions") or [], perms)
                    node: dict[str, Any] = {
                        "id": f"tauri::capability::{app_id}::{cap_id}",
                        "kind": "tauri_capability",
                        "label": cap_id,
                        "path": str(cap_path.relative_to(root)),
                        "app_identifier": app_id,
                    }
                    if cap.get("description"):
                        node["description"] = str(cap["description"])[:200]
                    if cap.get("windows"):
                        node["windows"] = cap["windows"]
                    if cap.get("contexts"):
                        node["contexts"] = cap["contexts"]
                    if perms:
                        node["permissions"] = perms
                    yield node

            # ── Icon nodes ──────────────────────────────────────────
            icons_dir = app_dir / "icons"
            if icons_dir.is_dir():
                for icon_path in sorted(icons_dir.iterdir()):
                    if not icon_path.is_file():
                        continue
                    yield {
                        "id": f"tauri::icon::{app_id}::{icon_path.name}",
                        "kind": "tauri_icon",
                        "label": icon_path.name,
                        "path": str(icon_path.relative_to(root)),
                        "app_identifier": app_id,
                        "size_bytes": icon_path.stat().st_size,
                    }

    def discover_edges(self, root: Path, graph_data: dict[str, Any]) -> Iterator[dict]:
        for conf in root.rglob("tauri.conf.json"):
            try:
                data = json.loads(conf.read_text(encoding="utf-8"))
            except (OSError, ValueError):
                continue
            app_id = data.get("identifier") or conf.parent.name
            yield {
                "from": f"tauri::app::{app_id}",
                "to": f"file::{conf.relative_to(root)}",
                "kind": "defines",
            }
            cap_dir = conf.parent / "capabilities"
            if cap_dir.is_dir():
                for cap_path in sorted(cap_dir.glob("*.json")):
                    try:
                        cap = json.loads(cap_path.read_text(encoding="utf-8"))
                    except (OSError, ValueError):
                        continue
                    cap_id = cap.get("identifier") or cap_path.stem
                    yield {
                        "from": f"tauri::capability::{app_id}::{cap_id}",
                        "to": f"file::{cap_path.relative_to(root)}",
                        "kind": "defines",
                    }
                    # App ↔ capability relationship
                    yield {
                        "from": f"tauri::app::{app_id}",
                        "to": f"tauri::capability::{app_id}::{cap_id}",
                        "kind": "contains",
                    }
            icons_dir = conf.parent / "icons"
            if icons_dir.is_dir():
                for icon_path in sorted(icons_dir.iterdir()):
                    if not icon_path.is_file():
                        continue
                    yield {
                        "from": f"tauri::icon::{app_id}::{icon_path.name}",
                        "to": f"file::{icon_path.relative_to(root)}",
                        "kind": "defines",
                    }

    # ── Shared query helper ─────────────────────────────────────────

    _KIND_BY_FILTER = {
        "app": "tauri_app",
        "capability": "tauri_capability",
        "icon": "tauri_icon",
    }

    def _graph_path(self) -> Path:
        return Path(__file__).resolve().parents[2] / "docs" / "codegraph" / "graph.json"

    def _query(self, kind: str = "", limit: int = 200) -> dict:
        try:
            graph = json.loads(self._graph_path().read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return {"total": 0, "tauri": [], "counts": {}, "kind_filter": kind or None}
        nodes = [n for n in graph.get("nodes", [])
                 if (n.get("kind") or "").startswith("tauri_")]
        counts: dict[str, int] = {}
        for n in nodes:
            counts[n["kind"]] = counts.get(n["kind"], 0) + 1
        if kind:
            wanted = self._KIND_BY_FILTER.get(kind, kind)
            nodes = [n for n in nodes if n.get("kind") == wanted]
        nodes.sort(key=lambda n: (n.get("kind", ""), n.get("label", "")))
        capped = nodes[:limit]
        return {
            "total": len(nodes),
            "returned": len(capped),
            "kind_filter": kind or None,
            "counts": counts,
            "tauri": [{k: v for k, v in n.items()} for n in capped],
            "truncated": len(nodes) > len(capped),
        }

    # ── Hook: MCP tool ──────────────────────────────────────────────

    def mcp_tools(self) -> dict[str, dict[str, Any]]:
        def cgraph_tauri(kind: str = "", limit: int = 200) -> str:
            return json.dumps(self._query(kind=kind, limit=limit), indent=2, default=str)
        cgraph_tauri.__doc__ = (
            "List Tauri app metadata, capabilities, and icons discovered under "
            "any `tauri.conf.json` in the repo. Pass `kind=\"app\"`, "
            "`kind=\"capability\"`, or `kind=\"icon\"` to filter; default returns all."
        )
        return {"cgraph_tauri": {"fn": cgraph_tauri, "description": cgraph_tauri.__doc__}}

    # ── Hook: HTTP route ────────────────────────────────────────────

    def http_routes(self) -> dict[str, Callable]:
        return {"/api/tauri": self._http}

    def _http(self, parsed_url) -> dict:
        params = parse_qs(getattr(parsed_url, "query", "") or "")
        kind = (params.get("kind", [""])[0]).strip()
        try:
            limit = int(params.get("limit", ["200"])[0])
        except ValueError:
            limit = 200
        return self._query(kind=kind, limit=limit)

    # ── Hook: CLI subcommand ────────────────────────────────────────

    def cli_commands(self) -> dict[str, Callable]:
        return {"tauri": self._cli}

    def _cli(self, argv: list[str]) -> int:
        parser = argparse.ArgumentParser(prog="codegraph_ext_cli tauri",
                                         description="Inspect Tauri config nodes.")
        parser.add_argument("--kind", default="",
                            choices=("", "app", "capability", "icon"))
        parser.add_argument("--limit", type=int, default=200)
        args = parser.parse_args(argv)
        print(json.dumps(self._query(kind=args.kind, limit=args.limit), indent=2, default=str))
        return 0

    # ── Hook: slash-command spec (viewer-side palette) ──────────────

    def slash_commands(self) -> list[dict[str, Any]]:
        return [{
            "cmd": "/tauri",
            "arg": "[app|capability|icon]",
            "description": "Inspect Tauri config — productName / windows / permissions / icons",
            "kinds": ["tauri_app", "tauri_capability", "tauri_icon"],
            "http_route": "/api/tauri",
            "arg_param": "kind",
            "summary": {
                "header_fields": [
                    {"label": "Total",  "from": "total"},
                    {"label": "Counts", "from": "counts", "format": "kv"},
                ],
                "list_field": "tauri",
                "row_template": "{kind}  {label}  @ {path}",
            },
        }]
