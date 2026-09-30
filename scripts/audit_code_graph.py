#!/usr/bin/env python3
"""Audit the generated code graph for coverage gaps.

Re-walks the repo with the same pruning rules as
`generate_code_graph.py` (`DISCOVERY_EXCLUDE_DIRS` /
`DISCOVERY_EXCLUDE_FILES` from `codegraph_exclude.txt`), then diffs
"files-on-disk-that-should-be-indexed" against the `file` nodes
emitted in `docs/codegraph/graph.json`.

Reports, per extension:
    - on disk         (files matching the suffix, after pruning)
    - in graph        (file nodes carrying that suffix)
    - missing         (on disk but not in graph)
    - unexpected      (in graph but no longer on disk)

Also reports project-level coverage:
    - manifest count   (Cargo.toml / package.json / Package.swift / project.yml /
                       pyproject.toml / setup.py / Chrome manifest.json)
    - crate nodes      (graph crate count)
    - any manifest dirs without a matching crate node

Usage:
    python3 scripts/audit_code_graph.py
    python3 scripts/audit_code_graph.py --graph docs/codegraph/graph.json
    python3 scripts/audit_code_graph.py --show-missing 50

Exit code:
    0  full coverage (or only known-design gaps)
    1  unexpected gaps detected (with --strict)
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Iterator


# ── Exclude list (same loader as generate_code_graph.py) ─────────────

SCRIPT_DIR: Path = Path(__file__).resolve().parent
EXCLUDE_FILE: Path = SCRIPT_DIR / "codegraph_exclude.txt"


def _load_exclude(path: Path) -> tuple[frozenset[str], frozenset[str]]:
    """Parse the shared `[dirs]` / `[files]` exclude file."""
    section: str | None = None
    buckets: dict[str, set[str]] = {"dirs": set(), "files": set()}
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        line = raw_line.split("#", 1)[0].strip()
        if not line:
            continue
        if line.startswith("[") and line.endswith("]"):
            section = line[1:-1].strip().lower()
            if section not in buckets:
                raise ValueError(f"{path}: unknown section [{section}]")
            continue
        if section is None:
            raise ValueError(f"{path}: entry {line!r} before any section header")
        buckets[section].add(line)
    return frozenset(buckets["dirs"]), frozenset(buckets["files"])


EXCLUDE_DIRS, EXCLUDE_FILES = _load_exclude(EXCLUDE_FILE)


# Mirrors `generate_code_graph.py::SECRET_FILE_RE`. Filenames matching
# this pattern are deliberately skipped during graph generation
# (credentials, env files, *_secret.* config). The audit must mirror
# the rule or these intentional skips show up as bogus "missing".
SECRET_FILE_RE = re.compile(
    r"(?i)(^|/)(\.env(\..*)?|secrets?\.(?:ya?ml|json|toml)|"
    r"credentials?\.(?:json|ya?ml|toml)|"
    r".*[._-]secret[._-]?.*\.(?:ya?ml|json|toml)|"
    r"keyring|tokens?\.json)$"
)


# ── Tracked extensions (must match generate_code_graph.py phases) ────

SOURCE_EXTENSIONS: tuple[str, ...] = (
    ".rs", ".py", ".swift",
    ".ts", ".tsx", ".jsx", ".js", ".mjs", ".cjs",
    ".svelte", ".vue", ".html", ".css", ".scss",
)
METADATA_EXTENSIONS: tuple[str, ...] = (".md", ".yaml", ".yml", ".toml", ".json")
ALL_TRACKED_EXTENSIONS: tuple[str, ...] = SOURCE_EXTENSIONS + METADATA_EXTENSIONS

MANIFEST_FILES: tuple[str, ...] = (
    "Cargo.toml", "package.json", "Package.swift", "project.yml",
    "pyproject.toml", "setup.py", "manifest.json",
)


# ── Walk ─────────────────────────────────────────────────────────────


def walk_repo_files(root: Path) -> Iterator[Path]:
    """Yield every file under `root`, applying the same exclusion rules
    that `generate_code_graph.py` uses during indexing:
        - prune `EXCLUDE_DIRS` and leading-dot dirs
        - skip basenames in `EXCLUDE_FILES`
        - skip leading-dot files (`.mcp.json` etc.) — the orphan walker
          filters these via its `part.startswith('.')` check
        - skip `.d.ts` TypeScript declaration files (deduped by the
          Node scanner since they re-export their `.ts` symbols)
        - skip filenames matching `SECRET_FILE_RE` (credentials etc.)
    Mirroring these rules keeps the audit from flagging deliberately
    excluded files as gaps."""
    stack: list[Path] = [root]
    while stack:
        cur = stack.pop()
        try:
            entries = list(cur.iterdir())
        except (PermissionError, OSError):
            continue
        for entry in entries:
            try:
                if entry.is_dir():
                    if entry.name in EXCLUDE_DIRS or entry.name.startswith("."):
                        continue
                    stack.append(entry)
                elif entry.is_file():
                    if entry.name in EXCLUDE_FILES:
                        continue
                    if entry.name.startswith("."):
                        continue
                    if entry.name.endswith(".d.ts"):
                        continue
                    if SECRET_FILE_RE.search(str(entry.relative_to(root))):
                        continue
                    yield entry
            except OSError:
                continue


# ── Audit ────────────────────────────────────────────────────────────


def compute_audit(root: Path, graph_path: Path) -> dict:
    """Pure-data audit: returns a structured payload describing
    coverage gaps without printing anything. Shared by the CLI
    pretty-printer (`audit()`), the HTTP `/api/audit` handler, and
    the MCP `cgraph_audit` tool."""
    if not graph_path.exists():
        raise FileNotFoundError(f"graph not found at {graph_path}")
    graph = json.loads(graph_path.read_text(encoding="utf-8"))

    nodes = graph.get("nodes", [])
    file_nodes_by_path: dict[str, dict] = {}
    crate_node_paths: set[str] = set()
    for node in nodes:
        kind = node.get("kind")
        path = node.get("path")
        if kind == "file" and path:
            file_nodes_by_path[path] = node
        elif kind == "crate" and path:
            crate_node_paths.add(path)

    indexed_file_paths: set[str] = set(file_nodes_by_path.keys())

    def manifest_dir_has_indexed_files(manifest_dir: str) -> bool:
        if manifest_dir == ".":
            return bool(indexed_file_paths)
        prefix = manifest_dir + "/"
        for fp in indexed_file_paths:
            if fp == manifest_dir or fp.startswith(prefix):
                return True
        return False

    on_disk_by_ext: dict[str, set[str]] = {ext: set() for ext in ALL_TRACKED_EXTENSIONS}
    manifest_dirs: dict[str, set[str]] = {m: set() for m in MANIFEST_FILES}

    for file_path in walk_repo_files(root):
        rel = str(file_path.relative_to(root))
        suffix = file_path.suffix
        if suffix in on_disk_by_ext:
            on_disk_by_ext[suffix].add(rel)
        if file_path.name in manifest_dirs:
            manifest_dirs[file_path.name].add(str(file_path.parent.relative_to(root)))

    in_graph_by_ext: dict[str, set[str]] = {ext: set() for ext in ALL_TRACKED_EXTENSIONS}
    for path in file_nodes_by_path:
        suffix = Path(path).suffix
        if suffix in in_graph_by_ext:
            in_graph_by_ext[suffix].add(path)

    by_extension: list[dict] = []
    total_missing: list[str] = []
    total_unexpected: list[str] = []
    for ext in ALL_TRACKED_EXTENSIONS:
        on_disk = on_disk_by_ext[ext]
        in_graph = in_graph_by_ext[ext]
        missing = sorted(on_disk - in_graph)
        unexpected = sorted(in_graph - on_disk)
        total_missing.extend(missing)
        total_unexpected.extend(unexpected)
        by_extension.append({
            "ext": ext,
            "on_disk": len(on_disk),
            "in_graph": len(in_graph),
            "missing": missing,
            "unexpected": unexpected,
        })

    total_manifest_dirs = set().union(*manifest_dirs.values()) if manifest_dirs else set()
    uncovered_dirs = sorted(
        d for d in total_manifest_dirs if not manifest_dir_has_indexed_files(d)
    )

    return {
        "by_extension": by_extension,
        "manifests": {
            name: sorted(manifest_dirs[name]) for name in MANIFEST_FILES
        },
        "unique_manifest_dirs": len(total_manifest_dirs),
        "crate_node_count": len(crate_node_paths),
        "uncovered_manifest_dirs": uncovered_dirs,
        "totals": {
            "missing": len(total_missing),
            "unexpected": len(total_unexpected),
            "uncovered_manifest_dirs": len(uncovered_dirs),
        },
        "ok": not (total_missing or total_unexpected or uncovered_dirs),
    }


def audit(root: Path, graph_path: Path, show_missing: int) -> int:
    """CLI pretty-printer over `compute_audit`. Prints a per-ext
    table + manifest summary + samples of missing / unexpected files.
    Returns exit code (0 = clean, 1 = gaps, 2 = error)."""
    try:
        result = compute_audit(root, graph_path)
    except FileNotFoundError as exc:
        print(f"audit failed: {exc}", file=sys.stderr)
        return 2

    by_extension = result["by_extension"]
    total_missing: list[str] = []
    total_unexpected: list[str] = []
    for row in by_extension:
        total_missing.extend(row["missing"])
        total_unexpected.extend(row["unexpected"])
    uncovered_dirs = result["uncovered_manifest_dirs"]

    print()
    print(f"Audit: graph={graph_path.relative_to(root) if graph_path.is_relative_to(root) else graph_path}")
    print(f"       root={root}")
    print()
    header = f"  {'ext':<8} {'on disk':>10} {'in graph':>10} {'missing':>10} {'unexpected':>12}"
    print(header)
    print("  " + "─" * (len(header) - 2))

    coverage_issues = 0
    for row in by_extension:
        if row["missing"] or row["unexpected"]:
            coverage_issues += len(row["missing"]) + len(row["unexpected"])
        flag = "" if (not row["missing"] and not row["unexpected"]) else "  ⚠"
        print(
            f"  {row['ext']:<8} {row['on_disk']:>10} {row['in_graph']:>10} "
            f"{len(row['missing']):>10} {len(row['unexpected']):>12}{flag}"
        )

    # ── Manifest-level audit ──
    print()
    print("  manifests discovered (project markers):")
    for name in MANIFEST_FILES:
        dirs = result["manifests"].get(name) or []
        print(f"    {name:<20} {len(dirs):>5}")
    print(f"    {'(unique dirs)':<20} {result['unique_manifest_dirs']:>5}")
    print(f"  crate nodes in graph:  {result['crate_node_count']:>5}")

    if uncovered_dirs:
        print()
        print(f"  ⚠ {len(uncovered_dirs)} manifest dir(s) have no matching crate node:")
        for d in uncovered_dirs[:show_missing]:
            print(f"      {d}")
        if len(uncovered_dirs) > show_missing:
            print(f"      … {len(uncovered_dirs) - show_missing} more")

    # ── Detail: missing / unexpected files ──
    if total_missing:
        print()
        print(f"  ⚠ {len(total_missing)} file(s) on disk are missing from the graph:")
        for path in total_missing[:show_missing]:
            print(f"      {path}")
        if len(total_missing) > show_missing:
            print(f"      … {len(total_missing) - show_missing} more")

    if total_unexpected:
        print()
        print(f"  ⚠ {len(total_unexpected)} file(s) in graph no longer exist on disk:")
        for path in total_unexpected[:show_missing]:
            print(f"      {path}")
        if len(total_unexpected) > show_missing:
            print(f"      … {len(total_unexpected) - show_missing} more")

    if coverage_issues == 0 and not uncovered_dirs:
        print()
        print("  ✓ full coverage — every tracked file on disk is in the graph")
        return 0

    print()
    print(
        f"  summary: {len(total_missing)} missing, {len(total_unexpected)} unexpected, "
        f"{len(uncovered_dirs)} uncovered manifest dirs"
    )
    return 1 if total_missing or uncovered_dirs else 0


# ── CLI ──────────────────────────────────────────────────────────────


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Audit code graph coverage against repo filesystem."
    )
    parser.add_argument("--root", default=None, help="Repo root (defaults to script's parent dir)")
    parser.add_argument(
        "--graph", default="docs/codegraph/graph.json", help="Path to graph.json"
    )
    parser.add_argument(
        "--show-missing",
        type=int,
        default=20,
        help="Max sample size to print for missing/unexpected file lists",
    )
    parser.add_argument(
        "--strict",
        action="store_true",
        help="Exit non-zero when any gaps are detected (default: warn only)",
    )
    args = parser.parse_args()

    root = (Path(args.root).resolve() if args.root else SCRIPT_DIR.parent)
    graph_path = (root / args.graph).resolve() if not Path(args.graph).is_absolute() else Path(args.graph)

    rc = audit(root, graph_path, args.show_missing)
    return rc if args.strict else 0


if __name__ == "__main__":
    raise SystemExit(main())
