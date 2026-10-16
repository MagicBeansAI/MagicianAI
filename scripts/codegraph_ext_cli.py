#!/usr/bin/env python3
"""CLI front-end for codegraph extensions.

Usage:
    python3 scripts/codegraph_ext_cli.py <subcommand> [args…]
    python3 scripts/codegraph_ext_cli.py --list

Each extension exposes its CLI subcommands via the `cli_commands()`
hook (see `scripts/codegraph_ext/__init__.py`). The first non-flag
argument is treated as the subcommand name; everything after is passed
through to the extension's handler.
"""

from __future__ import annotations

import sys
from pathlib import Path

# Make the package importable when this script is run as-is.
sys.path.insert(0, str(Path(__file__).resolve().parent))

from codegraph_ext import load_extensions  # noqa: E402


def main(argv: list[str]) -> int:
    extensions = load_extensions()
    registry: dict[str, callable] = {}
    by_ext: dict[str, list[str]] = {}
    for ext in extensions:
        try:
            cmds = ext.cli_commands()
        except Exception as exc:
            print(f"[ext {ext.name}] cli_commands failed: {exc}", file=sys.stderr)
            continue
        for name, fn in cmds.items():
            if name in registry:
                print(f"[ext {ext.name}] subcommand `{name}` already registered — skipping",
                      file=sys.stderr)
                continue
            registry[name] = fn
            by_ext.setdefault(ext.name, []).append(name)

    if not argv or argv[0] in ("--list", "-l", "--help", "-h"):
        if not registry:
            print("(no extension subcommands registered)")
            return 0
        print("Available extension subcommands:")
        for ext_name, cmds in sorted(by_ext.items()):
            print(f"  [{ext_name}]")
            for c in sorted(cmds):
                print(f"    {c}")
        return 0

    sub = argv[0]
    rest = argv[1:]
    handler = registry.get(sub)
    if handler is None:
        print(f"Unknown subcommand: {sub}", file=sys.stderr)
        print("Run with --list to see what's available.", file=sys.stderr)
        return 2
    try:
        return int(handler(rest) or 0)
    except SystemExit as exc:
        return int(exc.code or 0)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
