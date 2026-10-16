#!/usr/bin/env python3
"""Skip `make graph-index` when nothing it reads has changed.

Why this exists
---------------
`graph-index` walks the whole repository and takes about four minutes. It runs
unconditionally inside `build-all-debug` and `build-all-release`, so rebuilding
for any other reason — a failed later step, a UI-only change, a second run of
the same target — pays that four minutes again for an identical result.

This records a fingerprint of everything the generator could have read, and
lets the Make target skip when the fingerprint is unchanged *and* every output
is still on disk.

What the fingerprint covers
---------------------------
The generator walks the repository, so the fingerprint is the repository:

  * `HEAD` — the committed tree.
  * `git diff HEAD` — every tracked modification, hashed by content. A file
    edited and edited back produces the same digest, which is correct: the
    generator would read the same bytes.
  * untracked files that git would not ignore — path, size and mtime.

Generator sources need no special handling: `scripts/generate_code_graph.py`,
`scripts/codegraph_exclude.txt` and their friends are tracked, so a change to
any of them lands in one of the first two terms.

The outputs are gitignored, so they never appear in the fingerprint and cannot
invalidate themselves.

Direction of error
------------------
This over-triggers and never under-triggers, the same trade `codegen_drift.py`
makes. A docs-only commit regenerates a graph that would not have changed.
That is the safe direction: a stale graph is a wrong answer to every query
made against it, and the check costs 80ms against a 4-minute run.

The one theoretical gap is an untracked file rewritten with identical size and
mtime. Tracked content is exact, and the repository currently carries no
untracked files at all.

Usage
-----
    graph_index_stamp.py check --stamp S --output O [O ...]   # 0 = up to date
    graph_index_stamp.py write --stamp S
"""

from __future__ import annotations

import argparse
import hashlib
import subprocess
import sys
from pathlib import Path

STAMP_VERSION = "graph-index-stamp-v1"


def _git(*args: str) -> bytes:
    """Run a git command, returning stdout. Raises on failure."""
    return subprocess.run(
        ["git", *args],
        check=True,
        capture_output=True,
    ).stdout


def fingerprint() -> str:
    """Digest of everything `graph-index` could read."""
    digest = hashlib.sha256()
    digest.update(STAMP_VERSION.encode())

    # The committed tree. Detached, unborn or otherwise HEAD-less checkouts
    # simply contribute a constant, and the working-tree terms below still
    # carry the real content.
    try:
        digest.update(_git("rev-parse", "HEAD"))
    except subprocess.CalledProcessError:
        digest.update(b"no-head")

    # Tracked modifications, by content rather than by status letter: a file
    # reported "M" twice with different bytes has to produce different digests.
    digest.update(_git("diff", "HEAD"))

    # Untracked files git would not ignore. Ignored paths are excluded, which
    # is what keeps the generator's own 670 MB of output out of its input.
    others = _git("ls-files", "--others", "--exclude-standard", "-z")
    for raw in sorted(others.split(b"\0")):
        if not raw:
            continue
        digest.update(raw)
        try:
            stat = Path(raw.decode("utf-8", "surrogateescape")).stat()
        except OSError:
            # Vanished between listing and stat: record that fact rather than
            # silently agreeing with a stamp taken when it existed.
            digest.update(b"<gone>")
            continue
        digest.update(f"{stat.st_size}:{stat.st_mtime_ns}".encode())

    return digest.hexdigest()


def cmd_check(args: argparse.Namespace) -> int:
    stamp = Path(args.stamp)
    if not stamp.is_file():
        return 1

    missing = [output for output in args.output if not Path(output).exists()]
    if missing:
        # A present stamp with absent outputs means someone cleaned the
        # directory. Regenerate rather than trusting the stamp.
        print(
            f"  graph-index: {len(missing)} output(s) missing "
            f"(first: {missing[0]}) — reindexing",
            file=sys.stderr,
        )
        return 1

    if stamp.read_text().strip() != fingerprint():
        return 1
    return 0


def cmd_write(args: argparse.Namespace) -> int:
    stamp = Path(args.stamp)
    stamp.parent.mkdir(parents=True, exist_ok=True)
    stamp.write_text(fingerprint() + "\n")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    check = sub.add_parser("check", help="exit 0 when the index is current")
    check.add_argument("--stamp", required=True)
    check.add_argument("--output", nargs="*", default=[])
    check.set_defaults(func=cmd_check)

    write = sub.add_parser("write", help="record the current fingerprint")
    write.add_argument("--stamp", required=True)
    write.set_defaults(func=cmd_write)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
