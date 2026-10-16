#!/usr/bin/env python3
# Postprocess Metabase's full OpenAPI spec into the curated subset our
# data-analyst agent actually uses.
#
# Workflow (driven by `make regen-metabase-cli`):
#   1. refresh-spec.sh fetches the full spec from $MB_URL/api/docs/openapi.json
#      into .cache/metabase-full.json (gitignored).
#   2. This script reads that, drops everything outside the curated path set,
#      transitively pulls the schemas the kept paths reference, rewrites
#      operationIds to clean verbs (card list / dataset query / database
#      metadata ...), and writes spec.json — the file Printing Press generates
#      from.
#
# The curated set started as the old Specli wrapper surface (mb-explore +
# mb-questions + mb-query), then expanded to the operations an analyst agent
# needs for discovery, validation, dashboard/card reuse, controlled creation,
# and downstream handoff. It is intentionally not the full Metabase API.

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

# (path, method) -> (resource, verb). resource becomes the cobra command group
# (`metabase-pp-cli <resource> ...`), verb becomes the subcommand.
RENAMES: dict[tuple[str, str], tuple[str, str]] = {
    # Saved questions (cards) ------------------------------------------------
    ("/api/card", "get"):                                 ("card", "list"),
    ("/api/card", "post"):                                ("card", "create"),
    ("/api/card/{id}", "get"):                            ("card", "get"),
    ("/api/card/{id}", "put"):                            ("card", "update"),
    ("/api/card/{id}", "delete"):                         ("card", "delete"),
    ("/api/card/{id}/copy", "post"):                      ("card", "copy"),
    ("/api/card/{id}/dashboards", "get"):                 ("card", "dashboards"),
    ("/api/card/{id}/query_metadata", "get"):             ("card", "query-metadata"),
    ("/api/card/{id}/series", "get"):                     ("card", "series"),
    ("/api/card/{id}/params/{param-key}/remapping", "get"): ("card", "param-remapping"),
    ("/api/card/{card-id}/params/{param-key}/values", "get"): ("card", "param-values"),
    ("/api/card/{card-id}/params/{param-key}/search/{query}", "get"): ("card", "param-search"),
    ("/api/card/{card-id}/query", "post"):                ("card", "run"),
    ("/api/card/{card-id}/query/{export-format}", "post"): ("card", "export"),
    ("/api/card/collections", "post"):                    ("card", "collections"),
    ("/api/cards/dashboards", "post"):                    ("cards", "dashboards"),
    ("/api/cards/move", "post"):                          ("cards", "move"),

    # Ad-hoc SQL / MBQL ------------------------------------------------------
    ("/api/dataset", "post"):                             ("dataset", "query"),
    ("/api/dataset/{export-format}", "post"):             ("dataset", "export"),
    ("/api/dataset/native", "post"):                      ("dataset", "to-native"),
    ("/api/dataset/query_metadata", "post"):              ("dataset", "query-metadata"),
    ("/api/dataset/parameter/values", "post"):            ("dataset", "parameter-values"),
    ("/api/dataset/parameter/search/{query}", "post"):    ("dataset", "parameter-search"),
    ("/api/dataset/parameter/remapping", "post"):         ("dataset", "parameter-remapping"),
    ("/api/dataset/pivot", "post"):                       ("dataset", "pivot"),

    # Databases (read-only) --------------------------------------------------
    ("/api/database", "get"):                             ("database", "list"),
    ("/api/database/{id}", "get"):                        ("database", "get"),
    ("/api/database/{id}/autocomplete_suggestions", "get"): ("database", "autocomplete-suggestions"),
    ("/api/database/{id}/card_autocomplete_suggestions", "get"): ("database", "card-autocomplete-suggestions"),
    ("/api/database/{id}/metadata", "get"):               ("database", "metadata"),
    ("/api/database/{id}/schema", "get"):                 ("database", "schema-list"),
    ("/api/database/{id}/schemas", "get"):                ("database", "schemas"),
    ("/api/database/{id}/schema/{schema}", "get"):        ("database", "schema"),
    ("/api/database/{id}/fields", "get"):                 ("database", "fields"),
    ("/api/database/{id}/idfields", "get"):               ("database", "idfields"),
    ("/api/database/{id}/usage_info", "get"):             ("database", "usage-info"),

    # Collections ------------------------------------------------------------
    ("/api/collection", "get"):                           ("collection", "list"),
    ("/api/collection", "post"):                          ("collection", "create"),
    ("/api/collection/graph", "get"):                     ("collection", "graph"),
    ("/api/collection/graph", "put"):                     ("collection", "update-graph"),
    ("/api/collection/tree", "get"):                      ("collection", "tree"),
    ("/api/collection/root", "get"):                      ("collection", "root"),
    ("/api/collection/root/items", "get"):                ("collection", "root-items"),
    ("/api/collection/root/dashboard-question-candidates", "get"): ("collection", "root-dashboard-question-candidates"),
    ("/api/collection/root/move-dashboard-question-candidates", "post"): ("collection", "root-move-dashboard-question-candidates"),
    ("/api/collection/trash", "get"):                     ("collection", "trash"),
    ("/api/collection/{id}", "get"):                      ("collection", "get"),
    ("/api/collection/{id}", "put"):                      ("collection", "update"),
    ("/api/collection/{id}/items", "get"):                ("collection", "items"),
    ("/api/collection/{id}/dashboard-question-candidates", "get"): ("collection", "dashboard-question-candidates"),
    ("/api/collection/{id}/move-dashboard-question-candidates", "post"): ("collection", "move-dashboard-question-candidates"),

    # Dashboards -------------------------------------------------------------
    ("/api/dashboard", "get"):                            ("dashboard", "list"),
    ("/api/dashboard", "post"):                           ("dashboard", "create"),
    ("/api/dashboard/save", "post"):                      ("dashboard", "save"),
    ("/api/dashboard/save/collection/{parent-collection-id}", "post"): ("dashboard", "save-in-collection"),
    ("/api/dashboard/params/valid-filter-fields", "get"): ("dashboard", "valid-filter-fields"),
    ("/api/dashboard/{from-dashboard-id}/copy", "post"):  ("dashboard", "copy"),
    ("/api/dashboard/{id}", "get"):                       ("dashboard", "get"),
    ("/api/dashboard/{id}", "put"):                       ("dashboard", "update"),
    ("/api/dashboard/{id}/cards", "put"):                 ("dashboard", "update-cards"),
    ("/api/dashboard/{id}/items", "get"):                 ("dashboard", "items"),
    ("/api/dashboard/{id}/query_metadata", "get"):        ("dashboard", "query-metadata"),
    ("/api/dashboard/{id}/related", "get"):               ("dashboard", "related"),
    ("/api/dashboard/{id}/params/{param-key}/values", "get"): ("dashboard", "param-values"),
    ("/api/dashboard/{id}/params/{param-key}/search/{query}", "get"): ("dashboard", "param-search"),
    ("/api/dashboard/{id}/params/{param-key}/remapping", "get"): ("dashboard", "param-remapping"),
    ("/api/dashboard/{dashboard-id}/dashcard/{dashcard-id}/card/{card-id}/query", "post"): ("dashboard", "dashcard-query"),
    ("/api/dashboard/{dashboard-id}/dashcard/{dashcard-id}/card/{card-id}/query/{export-format}", "post"): ("dashboard", "dashcard-export"),

    # Tables / fields / modeled metadata ------------------------------------
    ("/api/table", "get"):                                ("table", "list"),
    ("/api/table/{id}", "get"):                           ("table", "get"),
    ("/api/table/{id}/query_metadata", "get"):            ("table", "query-metadata"),
    ("/api/table/{id}/fks", "get"):                       ("table", "fks"),
    ("/api/table/{id}/related", "get"):                   ("table", "related"),
    ("/api/table/{table-id}/data", "get"):                ("table", "data"),
    ("/api/table/card__:id/query_metadata", "get"):       ("table", "card-query-metadata"),
    ("/api/table/card__:id/fks", "get"):                  ("table", "card-fks"),

    ("/api/field/table-ids", "post"):                     ("field", "table-ids"),
    ("/api/field/{id}", "get"):                           ("field", "get"),
    ("/api/field/{id}/values", "get"):                    ("field", "values"),
    ("/api/field/{id}/summary", "get"):                   ("field", "summary"),
    ("/api/field/{id}/search/{search-id}", "get"):        ("field", "search"),
    ("/api/field/{id}/related", "get"):                   ("field", "related"),
    ("/api/field/{id}/remapping/{remapped-id}", "get"):   ("field", "remapping"),

    ("/api/native-query-snippet", "get"):                 ("snippet", "list"),
    ("/api/native-query-snippet", "post"):                ("snippet", "create"),
    ("/api/native-query-snippet/{id}", "get"):            ("snippet", "get"),
    ("/api/native-query-snippet/{id}", "put"):            ("snippet", "update"),

    ("/api/metric", "get"):                               ("metric", "list"),
    ("/api/metric/{id}", "get"):                          ("metric", "get"),
    ("/api/metric/{id}/dimension/{dimension-key}/values", "get"): ("metric", "dimension-values"),
    ("/api/metric/{id}/dimension/{dimension-key}/search", "get"): ("metric", "dimension-search"),
    ("/api/metric/{id}/dimension/{dimension-key}/remapping", "get"): ("metric", "dimension-remapping"),

    # Cross-entity search (Metabase's /api/search) ---------------------------
    # Resource is forced to "lookup" via OpenAPI tags so it doesn't collide
    # with PP's framework-level `search` command (which queries the local
    # SQLite mirror — we don't sync, so we don't want that command at all).
    ("/api/search", "get"):                               ("lookup", "items"),
}

DEFAULT_SERVER = "https://metabase.example.com"


def collect_refs(node, acc: set[str]) -> None:
    """Walk a JSON tree, collecting every $ref string."""
    if isinstance(node, dict):
        ref = node.get("$ref")
        if isinstance(ref, str):
            acc.add(ref)
        for v in node.values():
            collect_refs(v, acc)
    elif isinstance(node, list):
        for v in node:
            collect_refs(v, acc)


def relax_required_params_with_defaults(paths: dict) -> None:
    """Mark params/body-fields with defaults as not required.

    Metabase marks many boolean query params (e.g. `include-analytics`) as
    required even though they have sensible defaults; PP and Specli both
    enforce required flags at the CLI level, which would force the agent to
    pass tautological values on every call. Drop `required: true` whenever
    `default:` is present.
    """
    for path_obj in paths.values():
        for op in path_obj.values():
            if not isinstance(op, dict):
                continue
            for param in op.get("parameters", []):
                schema = param.get("schema", {})
                if param.get("required") and "default" in schema:
                    param["required"] = False
            body_schema = (
                op.get("requestBody", {})
                .get("content", {})
                .get("application/json", {})
                .get("schema", {})
            )
            required_list = body_schema.get("required", [])
            props = body_schema.get("properties", {})
            if required_list and props:
                body_schema["required"] = [
                    name for name in required_list
                    if "default" not in props.get(name, {})
                ]
                if not body_schema["required"]:
                    del body_schema["required"]


def transitive_schemas(spec: dict, kept_paths: dict) -> dict:
    """Resolve every $ref reachable from kept_paths and return that schema subset."""
    all_schemas = spec.get("components", {}).get("schemas", {}) or {}
    seen, queue = set(), set()
    collect_refs(kept_paths, queue)
    while queue:
        ref = queue.pop()
        name = ref.split("/")[-1]
        if name in seen or name not in all_schemas:
            continue
        seen.add(name)
        more: set[str] = set()
        collect_refs(all_schemas[name], more)
        queue |= more - seen
    return {k: all_schemas[k] for k in sorted(seen)}


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--input", required=True, help="full Metabase OpenAPI spec (json)")
    p.add_argument("--output", required=True, help="curated spec destination")
    p.add_argument("--server", default=DEFAULT_SERVER,
                   help=f"server URL placeholder for the spec (default: {DEFAULT_SERVER}). "
                        "Real value is supplied at CLI runtime via MB_URL.")
    args = p.parse_args()

    src = json.loads(Path(args.input).read_text())

    kept_paths: dict[str, dict] = {}
    matched: set[tuple[str, str]] = set()
    for (path, method), (resource, verb) in RENAMES.items():
        op = src.get("paths", {}).get(path, {}).get(method)
        if op is None:
            print(f"WARN: missing in upstream spec: {method.upper()} {path}", file=sys.stderr)
            continue
        op = json.loads(json.dumps(op))  # deep copy
        op["operationId"] = f"{resource}-{verb}"
        op["tags"] = [resource]  # forces PP's resource grouping
        kept_paths.setdefault(path, {})[method] = op
        matched.add((path, method))

    relax_required_params_with_defaults(kept_paths)
    schemas = transitive_schemas(src, kept_paths)

    out = {
        "openapi": src.get("openapi", "3.0.3"),
        "info": {
            # Title becomes the slug for PP's framework-collision rename
            # (e.g. /api/search → "search" collides with PP's own search
            # command, gets renamed to "<slug>-search"). Keep title short
            # so the renamed command name stays usable.
            "title": "Metabase",
            "version": "1.0.0",
            "description": (
                "Curated read-mostly subset for the Magician data-analyst agent. "
                "Driven by Printing Press; regenerate via `make regen-metabase-cli`."
            ),
        },
        "servers": [{"url": args.server}],
        "tags": [
            {"name": "card",       "description": "Saved questions (run, export, manage)"},
            {"name": "dataset",    "description": "Ad-hoc SQL / MBQL queries"},
            {"name": "database",   "description": "Database discovery and schema introspection"},
            {"name": "collection", "description": "Collection hierarchy and controlled organization changes"},
            {"name": "dashboard",  "description": "Dashboard discovery, execution, and controlled updates"},
            {"name": "table",      "description": "Table metadata and relationship discovery"},
            {"name": "field",      "description": "Field metadata, values, summaries, and remapping"},
            {"name": "snippet",    "description": "Native SQL snippets"},
            {"name": "metric",     "description": "Metric discovery and dimension values"},
            {"name": "lookup",     "description": "Cross-entity search"},
        ],
        "paths": kept_paths,
        "components": {
            "schemas": schemas,
            "securitySchemes": src.get("components", {}).get("securitySchemes", {}),
        },
    }

    Path(args.output).write_text(json.dumps(out, indent=2) + "\n")

    ops = sum(len(m) for m in kept_paths.values())
    print(f"wrote {args.output}: {len(kept_paths)} paths / {ops} operations / {len(schemas)} schemas")
    if missing := set(RENAMES) - matched:
        print(f"WARN: {len(missing)} renames did not match upstream paths", file=sys.stderr)
        for path, method in sorted(missing):
            print(f"  {method.upper()} {path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
