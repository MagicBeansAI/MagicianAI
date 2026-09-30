#!/usr/bin/env python3
"""Uninstall bot bundle symlinks.

For each `<scope>/bots/<bot>/dist/index.js` that is a symlink, remove
it. Leaves untouched: `node_modules/` (whatsapp's real per-scope npm
install), `package.json`, `.env.<account>` files, `bot_configs.yaml`,
or anything else that's a real per-scope file.

Mirror of `install_bot_bundles.py`. Idempotent: re-running on already-
uninstalled bots is a no-op.

Usage:
    uninstall_bot_bundles.py                          # default scope
    uninstall_bot_bundles.py --scope alice/staging
    uninstall_bot_bundles.py --all-scopes
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
DEFAULT_DATA_ROOT = REPO / "magician_data_v3"
DEFAULT_SCOPE = "anonymous/default"


def uninstall_for_scope(scope_root: Path) -> int:
    """Remove every bot's dist/index.js symlink at this scope.
    Returns the count removed."""
    bots_root = scope_root / "bots"
    if not bots_root.is_dir():
        return 0
    removed = 0
    for bot_dir in sorted(bots_root.iterdir()):
        if not bot_dir.is_dir():
            continue
        bundle = bot_dir / "dist" / "index.js"
        if bundle.is_symlink():
            bundle.unlink()
            print(f"    ✓ {bot_dir.name}/dist/index.js (symlink removed)")
            removed += 1
            # Prune dist/ if it's now empty. Leave the bot dir itself
            # because it may have real per-scope state (node_modules/,
            # package.json, .env.<account>).
            try:
                bundle.parent.rmdir()
            except OSError:
                pass
    return removed


def discover_scopes(data_root: Path) -> list[str]:
    out: list[str] = []
    scopes_root = data_root / "scopes"
    if not scopes_root.is_dir():
        return out
    for principal in sorted(scopes_root.iterdir()):
        if not principal.is_dir():
            continue
        for workspace in sorted(principal.iterdir()):
            if workspace.is_dir():
                out.append(f"{principal.name}/{workspace.name}")
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--data-root",
        default=DEFAULT_DATA_ROOT,
        type=Path,
        help=f"Magician data root (default: {DEFAULT_DATA_ROOT})",
    )
    parser.add_argument(
        "--scope",
        default=None,
        help=f"Scope to uninstall from (default: {DEFAULT_SCOPE}; ignored with --all-scopes)",
    )
    parser.add_argument(
        "--all-scopes",
        action="store_true",
        help="Uninstall bot bundles from every scope found under --data-root",
    )
    args = parser.parse_args()

    if args.all_scopes:
        scopes = discover_scopes(args.data_root)
    else:
        scopes = [args.scope or DEFAULT_SCOPE]

    if not scopes:
        print(">>> no scopes found")
        return 0

    total = 0
    for scope in scopes:
        scope_root = args.data_root / "scopes" / scope
        print(f"== {scope} ==")
        count = uninstall_for_scope(scope_root)
        if count == 0:
            print("    (nothing to remove)")
        total += count

    print(f">>> removed {total} bot bundle symlink(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
