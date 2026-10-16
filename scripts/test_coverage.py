#!/usr/bin/env python3
"""Test-coverage report from the generated code graph.

Reads `docs/codegraph/graph.json` and bucketing production functions
by how many incoming `test_calls` edges they have. Tests themselves
(functions with `test=true`) are excluded from the report — coverage
is about how well *production* code is exercised, not whether tests
exist.

Buckets:
    untested — production fn with ≥1 production caller, 0 test callers
    light    — 1 test caller
    moderate — 2–5 test callers
    well     — 6+ test callers
    only_tested — 0 production callers, ≥1 test callers (function only
                  used by tests — possibly belongs IN the test module)

This is a *structural* coverage view derived from the call graph. It
isn't line/branch coverage (no execution data). It's most useful for
spotting production functions tests never reach — i.e. blind spots a
real coverage tool would also flag.

Usage:
    python3 scripts/test_coverage.py
    python3 scripts/test_coverage.py --crate magician
    python3 scripts/test_coverage.py --bucket untested --limit 100
"""

from __future__ import annotations

import argparse
import json
from collections import defaultdict
from pathlib import Path


# Functions whose lack of test callers is uninteresting (idiomatic
# stdlib-like names that are tested implicitly by any code path
# exercising the type at all).
IDIOM_NAMES = frozenset({
    "new", "default", "clone", "fmt", "deref", "deref_mut",
    "as_ref", "as_mut", "drop", "len", "is_empty", "iter", "iter_mut",
    "next", "size_hint", "hash", "eq", "ne", "partial_cmp", "cmp",
})


def is_idiomatic_method(name: str) -> bool:
    if name in IDIOM_NAMES:
        return True
    for prefix in ("from_", "into_", "to_", "as_", "is_", "has_", "with_",
                   "try_from", "try_into", "set_", "get_"):
        if name.startswith(prefix):
            return True
    return False


def analyze(graph_path: Path, crate_filter: str | None) -> dict:
    graph = json.loads(graph_path.read_text(encoding="utf-8"))
    nodes = graph["nodes"]
    edges = graph["edges"]

    prod_callers: dict[str, int] = defaultdict(int)
    test_callers: dict[str, int] = defaultdict(int)
    for edge in edges:
        kind = edge.get("kind")
        target = edge.get("to")
        if not target:
            continue
        if kind == "calls":
            prod_callers[target] += 1
        elif kind == "test_calls":
            test_callers[target] += 1

    buckets: dict[str, list[dict]] = {
        "untested": [],
        "light": [],
        "moderate": [],
        "well": [],
        "only_tested": [],
    }

    for fn in nodes:
        if fn.get("kind") != "function":
            continue
        if fn.get("test"):
            continue
        if crate_filter and fn.get("crate") != crate_filter:
            continue
        if is_idiomatic_method(fn.get("label") or ""):
            continue
        # Skip the synthetic repo-root crate (loose scripts) — they're
        # not typically test-covered as production code.
        if fn.get("crate") == "repo-root":
            continue

        fn_id = fn["id"]
        prod = prod_callers.get(fn_id, 0)
        tests = test_callers.get(fn_id, 0)
        fn["_prod_callers"] = prod
        fn["_test_callers"] = tests

        if tests == 0 and prod == 0:
            # Production fn with no callers at all → dead-code territory,
            # not really a coverage gap. Skip.
            continue
        if prod == 0 and tests > 0:
            buckets["only_tested"].append(fn)
        elif tests == 0:
            buckets["untested"].append(fn)
        elif tests <= 1:
            buckets["light"].append(fn)
        elif tests <= 5:
            buckets["moderate"].append(fn)
        else:
            buckets["well"].append(fn)

    for key in buckets:
        buckets[key].sort(key=lambda f: (
            -int(f.get("_prod_callers") or 0),  # heavy production-callers first
            str(f.get("path")),
            int(f.get("line") or 0),
        ))

    return buckets


def render(buckets: dict, bucket_filter: str | None, limit: int) -> str:
    out: list[str] = []
    total = sum(len(b) for b in buckets.values())
    summary_line = (
        f"untested={len(buckets['untested'])}  "
        f"light={len(buckets['light'])}  "
        f"moderate={len(buckets['moderate'])}  "
        f"well={len(buckets['well'])}  "
        f"only_tested={len(buckets['only_tested'])}"
    )
    out.append(f"# Test coverage: {total} production functions across buckets")
    out.append(f"  {summary_line}")
    out.append("")
    out.append("Buckets:")
    out.append("  untested    — has production callers, zero test callers (real gap)")
    out.append("  light       — 1 test caller")
    out.append("  moderate    — 2–5 test callers")
    out.append("  well        — 6+ test callers")
    out.append("  only_tested — 0 production callers, ≥1 test callers (may belong in tests/)")
    out.append("")
    out.append("Columns:  `prod=N tcalls=N  fn_name(params…)`")
    out.append("")

    if bucket_filter:
        buckets = {bucket_filter: buckets.get(bucket_filter, [])}

    for bucket_name, fns in buckets.items():
        out.append(f"## {bucket_name} — {len(fns)} function(s)")
        out.append("")
        if not fns:
            out.append("(none)")
            out.append("")
            continue
        by_file: dict[str, list[dict]] = defaultdict(list)
        for fn in fns:
            by_file[str(fn.get("path") or "")].append(fn)
        shown = 0
        for path, group in sorted(
            by_file.items(),
            key=lambda x: (-sum(int(f.get("_prod_callers") or 0) for f in x[1]), x[0]),
        ):
            out.append(f"### `{path}` ({len(group)})")
            for fn in group:
                if shown >= limit:
                    break
                line = fn.get("line") or "?"
                name = fn.get("label") or "?"
                prod = int(fn.get("_prod_callers") or 0)
                tcalls = int(fn.get("_test_callers") or 0)
                public = "pub" if fn.get("public") else "   "
                params = fn.get("params") or ""
                summary = f"  L{line:>5}  prod={prod:<3} tcalls={tcalls:<3}  {public}  `{name}`"
                if params:
                    short = params if len(params) <= 60 else params[:57] + "…"
                    summary += f" `({short})`"
                out.append(summary)
                shown += 1
            out.append("")
            if shown >= limit:
                out.append(f"_(stopping at --limit={limit})_")
                break

    return "\n".join(out)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--graph", default="docs/codegraph/graph.json")
    parser.add_argument("--crate", default=None)
    parser.add_argument(
        "--bucket",
        choices=("untested", "light", "moderate", "well", "only_tested"),
        default=None,
    )
    parser.add_argument("--limit", type=int, default=100)
    args = parser.parse_args()

    script_root = Path(__file__).resolve().parents[1]
    graph_path = (script_root / args.graph).resolve() if not Path(args.graph).is_absolute() else Path(args.graph)
    if not graph_path.exists():
        print(f"graph not found: {graph_path}")
        return 2

    buckets = analyze(graph_path, args.crate)
    print(render(buckets, args.bucket, args.limit))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
