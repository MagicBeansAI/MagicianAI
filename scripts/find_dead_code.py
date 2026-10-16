#!/usr/bin/env python3
"""Surface dead-code candidates from the generated code graph.

Reads `docs/codegraph/graph.json`. Production dead-code = a function with
zero incoming `calls` edges from *production* code (test callers are
tracked separately as `test_calls` and shown as informational metadata,
not used in the dead-or-alive decision).

Test classification is structural, not name-based:
    - Rust:    `#[test]` / `#[*::test]` annotations, `#[cfg(test)] mod`
               blocks, files under `tests/`/`benches/`/`examples/`.
    - TS/JS:   files matching `*.test.{ts,tsx,js,jsx,mjs,cjs}` or
               `*.spec.*`, or under `__tests__/` / `__mocks__/`.
    - Python:  files matching `test_*.py` / `*_test.py`, or functions
               whose name starts with `test_` (pytest discovery).
    - Swift:   files matching `*Tests.swift` or living in complete
               `*Tests` / `*UITests` targets (XCTest conventions).

Confidence tiers:
    A — private function in production code, no production callers
    B — private function in test/script/examples tree (function may be
        called by harness, framework discovery, etc.)
    C — public function with no production callers (may be called from
        outside the crate / via FFI / via JS frontend)

Always filtered out (impossible-to-statically-detect callers):
    - Functions with incoming `handles`/`implements` edges (HTTP handler,
      trait impl) or `implements_trait` attr (external trait dispatch)
    - Entry points: `main`, `lib`, `_start`, `build.rs`
    - Tauri command handlers (Rust under `desktop/src-tauri/`) — invoked
      from JS via IPC
    - Functions whose name is on the constructor/converter shortlist
      (`new`, `default`, `from_*`, etc.) — called via type-name
      resolution we can't track without full type inference.

Usage:
    python3 scripts/find_dead_code.py
    python3 scripts/find_dead_code.py --crate magician
    python3 scripts/find_dead_code.py --tier A --limit 100
"""

from __future__ import annotations

import argparse
import json
import re
from collections import Counter, defaultdict
from pathlib import Path


# Word-occurrence index helps separate "graph says no callers" (already
# applied) from "name doesn't appear anywhere in source text either"
# (much stronger dead-code signal — catches FPs from macros, doc
# references, and other mentions our regex extractor misses).
_WORD_RE = re.compile(r"\b[A-Za-z_]\w*\b")
# Code files only — doc/config files (.md/.yaml/.toml/.json) get
# excluded because they shouldn't keep code alive (a doc mention isn't
# a real caller), and including them would double-count function names
# that appear in our own generated artifacts (graph.json itself).
_CODE_SUFFIXES = {".rs", ".py", ".swift", ".ts", ".tsx", ".jsx", ".js",
                  ".mjs", ".cjs", ".svelte", ".vue", ".html"}
_SKIP_DIRS = {".cache", ".build", "build", "dist", "out", "target",
              "node_modules", "_vendor", "gen", ".venv", "venv",
              ".git", "__pycache__", "magician_data_v3"}
_SKIP_FILES = {"graph.json", "stats.json", "payload_profiles.json",
               "contracts.json"}


def build_word_occurrence_counts(root: Path) -> Counter:
    """One-pass count of every identifier (`\\b[A-Za-z_]\\w*\\b`)
    across every source file in the repo. Same pruning as the
    codegraph walker. Used to enrich dead-code candidates with a
    "how often does this name appear in any source file" rank — a
    name appearing exactly once almost always means just its own
    definition (no callers, no doc refs, no macro mentions)."""
    counts: Counter = Counter()
    stack = [root]
    while stack:
        cur = stack.pop()
        try:
            entries = list(cur.iterdir())
        except (PermissionError, OSError):
            continue
        for entry in entries:
            try:
                if entry.is_dir():
                    if entry.name in _SKIP_DIRS or entry.name.startswith("."):
                        continue
                    stack.append(entry)
                elif entry.is_file() and entry.suffix in _CODE_SUFFIXES:
                    if entry.name in _SKIP_FILES:
                        continue
                    try:
                        text = entry.read_text(encoding="utf-8", errors="ignore")
                    except OSError:
                        continue
                    counts.update(_WORD_RE.findall(text))
            except OSError:
                continue
    return counts


# ── Heuristics ───────────────────────────────────────────────────────

# Function names that almost always have callers we can't statically
# resolve (constructor / conversion / predicate idioms, builder methods,
# operator-overload-like accessor names). Marking these as "likely live"
# avoids a huge false-positive class.
LIKELY_LIVE_NAMES: frozenset[str] = frozenset({
    "new", "default", "build", "create", "init", "open", "close",
    "drop", "clone", "deref", "deref_mut", "as_ref", "as_mut",
    "into_inner", "into_iter", "iter", "iter_mut", "len", "is_empty",
    "hash", "eq", "ne", "partial_cmp", "cmp", "fmt", "next",
    "size_hint", "render", "render_self",
})

# Name prefixes that almost always signal "called via dispatch / type
# inference / framework". Function with these prefixes are excluded.
LIKELY_LIVE_PREFIXES: tuple[str, ...] = (
    "from_", "into_", "try_from", "try_into", "to_",
    "as_", "is_", "has_", "with_", "without_",
    "set_", "get_", "or_",
    "deserialize", "serialize",
)

# Stimulus / Svelte controller lifecycle + action handler conventions.
FRAMEWORK_CONTROLLER_METHODS: frozenset[str] = frozenset({
    "connect", "disconnect", "initialize", "register", "unregister",
    "beforeUpdate", "afterUpdate", "destroy",
    # Svelte action lifecycle
    "update", "destroy",
})


def is_entry_point(name: str, path: str) -> bool:
    if name in {"main", "lib", "_start"}:
        return True
    if path.endswith("build.rs"):
        return True
    # Rust leading-underscore is the canonical "this exists on purpose
    # but isn't called from code" marker (warning suppressors, FFI
    # placeholders, type-assertion helpers). Treat as intentional.
    if path.endswith(".rs") and name.startswith("_"):
        return True
    return False


def is_likely_live_idiom(name: str) -> bool:
    if name in LIKELY_LIVE_NAMES:
        return True
    for prefix in LIKELY_LIVE_PREFIXES:
        if name.startswith(prefix):
            return True
    return False


def is_likely_framework_callback(name: str, path: str) -> bool:
    if name in FRAMEWORK_CONTROLLER_METHODS and any(
        path.endswith(suffix) for suffix in (
            "_controller.ts", "_controller.js", "_controller.svelte"
        )
    ):
        return True
    return False


def is_likely_tauri_command(path: str) -> bool:
    # Tauri commands live under `desktop/src-tauri/src/` and are
    # invoked from JS via `invoke('cmd_name', ...)`. We don't parse
    # the JS side's invoke names, so blanket-exclude this tree.
    return path.startswith("desktop/src-tauri/src/")


# ── Analysis ─────────────────────────────────────────────────────────


def analyze(graph_path: Path, crate_filter: str | None, word_counts: Counter) -> dict:
    """Return a tier-bucketed dict of candidate function nodes.

    Production-alive signals: `calls`, `handles`, `implements`.
    `test_calls` is tracked separately (per-node count) and shown as
    informational metadata — not used for dead-or-alive."""
    graph = json.loads(graph_path.read_text(encoding="utf-8"))
    nodes = graph["nodes"]
    edges = graph["edges"]

    has_prod_caller: set[str] = set()
    test_call_count: dict[str, int] = defaultdict(int)
    for edge in edges:
        kind = edge.get("kind")
        target = edge.get("to")
        if not target:
            continue
        if kind in {"calls", "handles", "implements"}:
            has_prod_caller.add(target)
        elif kind == "test_calls":
            test_call_count[target] += 1

    functions = [n for n in nodes if n.get("kind") == "function"]

    buckets: dict[str, list[dict]] = {"A": [], "B": [], "C": []}
    # `occurrences == 1` means the only place the name appears in any
    # indexed source file is its own definition. That's the strongest
    # mechanical signal a function is truly unused — no macro
    # invocations, no doc refs, no string-based dispatch, nothing.

    for fn in functions:
        fn_id = fn["id"]
        name = fn.get("label") or ""
        path = fn.get("path") or ""
        crate = fn.get("crate") or ""

        if crate_filter and crate != crate_filter:
            continue

        # Filters that drop the candidate entirely.
        if fn_id in has_prod_caller:
            continue
        if fn.get("test"):
            # Test functions are not production code — skip them.
            continue
        if fn.get("implements_trait"):
            continue
        if is_entry_point(name, path):
            continue
        if is_likely_live_idiom(name):
            continue
        if is_likely_framework_callback(name, path):
            continue
        if is_likely_tauri_command(path):
            continue

        # Attach informational metadata for the report.
        fn["_occurrences"] = word_counts.get(name, 0)
        fn["_test_callers"] = test_call_count.get(fn_id, 0)

        # Bucket by visibility only — no more name-based test/script
        # heuristics. The `test` attr already excluded those above.
        # Functions tagged as "in a script tree" (e.g. /scripts/*.py)
        # are kept in Tier A/C since scripts are real code that can
        # legitimately go dead. The user can grep-verify.
        public = bool(fn.get("public"))
        tier = "C" if public else "A"

        buckets[tier].append(fn)

    # Sort each bucket: lowest occurrence count first (strongest dead
    # signal), then by path / line for stability.
    for tier in buckets:
        buckets[tier].sort(key=lambda f: (
            int(f.get("_occurrences") or 0),
            str(f.get("path")),
            int(f.get("line") or 0),
        ))

    return buckets


# ── Output ───────────────────────────────────────────────────────────


def render_report(buckets: dict[str, list[dict]], tier_filter: str | None, limit: int) -> str:
    out: list[str] = []
    total = sum(len(b) for b in buckets.values())
    near_certain = sum(
        1 for b in buckets.values() for fn in b if int(fn.get("_occurrences") or 0) <= 1
    )
    out.append(f"# Dead-code candidates: {total} (★ = only one occurrence in any source file)")
    out.append("")
    out.append("Test classification is structural (annotation- and path-based — see script docstring).")
    out.append("Test callers are tracked separately as `tcalls=N` (informational; does not affect dead-or-alive).")
    out.append("")
    out.append("Tiers:")
    out.append("  A — private fn (no production callers, not a test)")
    out.append("  C — public fn (may be called externally — review with caution)")
    out.append("")
    out.append("Per-line columns:")
    out.append("  `occ=N`   — name occurrences across all code files (def + everywhere else).")
    out.append("  `tcalls=N` — incoming `test_calls` edges (functions called only by tests).")
    out.append("  ★         — occ=1 (name appears only at its definition; strongest dead signal).")
    out.append("")

    for tier in ("A", "C"):
        if tier_filter and tier != tier_filter:
            continue
        candidates = buckets[tier]
        tier_near_certain = sum(1 for c in candidates if int(c.get("_occurrences") or 0) <= 1)
        only_tested = sum(1 for c in candidates if int(c.get("_test_callers") or 0) > 0)
        out.append(
            f"## Tier {tier} — {len(candidates)} candidates "
            f"(★={tier_near_certain}; only-test-callers={only_tested})"
        )
        out.append("")
        if not candidates:
            out.append("(none)")
            out.append("")
            continue

        by_file: dict[str, list[dict]] = defaultdict(list)
        for fn in candidates:
            by_file[str(fn.get("path") or "")].append(fn)

        shown = 0
        def file_sort_key(item):
            _, fns = item
            return (min((int(f.get("_occurrences") or 0)) for f in fns), str(item[0]))
        for path, fns in sorted(by_file.items(), key=file_sort_key):
            out.append(f"### `{path}` ({len(fns)})")
            for fn in fns[:limit]:
                line = fn.get("line") or "?"
                name = fn.get("label") or "?"
                occ = int(fn.get("_occurrences") or 0)
                tcalls = int(fn.get("_test_callers") or 0)
                params = fn.get("params") or ""
                tag = "★" if occ <= 1 else " "
                summary = f"  {tag} L{line:>5}  occ={occ:<3}  tcalls={tcalls:<3}  `{name}`"
                if params:
                    short = params if len(params) <= 70 else params[:67] + "…"
                    summary += f"  `({short})`"
                out.append(summary)
                shown += 1
                if shown >= limit:
                    break
            if len(fns) > limit:
                out.append(f"     … {len(fns) - limit} more in this file")
            out.append("")
            if shown >= limit:
                out.append(f"_(stopping at --limit={limit}; pass higher limit to see more)_")
                break

    return "\n".join(out)


# ── CLI ──────────────────────────────────────────────────────────────


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--graph", default="docs/codegraph/graph.json")
    parser.add_argument("--crate", default=None, help="Restrict to one crate (e.g. magician)")
    parser.add_argument("--tier", choices=("A", "C"), default=None)
    parser.add_argument("--limit", type=int, default=50, help="Max candidates shown per report")
    args = parser.parse_args()

    script_root = Path(__file__).resolve().parents[1]
    graph_path = (script_root / args.graph).resolve() if not Path(args.graph).is_absolute() else Path(args.graph)
    if not graph_path.exists():
        print(f"graph not found: {graph_path}")
        return 2

    word_counts = build_word_occurrence_counts(script_root)
    buckets = analyze(graph_path, args.crate, word_counts)
    print(render_report(buckets, args.tier, args.limit))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
