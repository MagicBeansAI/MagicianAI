#!/usr/bin/env python3
"""One-time cleanup for legacy Desk feed assistant-message rows.

The runtime no longer materializes ChatMessageReceived events into feed
`agent_message` rows. This script removes rows created by the old projection
from existing scoped feed DuckDB files. It is intentionally explicit instead
of being part of normal feed reads.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Remove legacy item_type='agent_message' rows from scoped feed DuckDB stores."
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=Path("magician_data_v3"),
        help="Path to magician_data_v3 root. Defaults to ./magician_data_v3.",
    )
    parser.add_argument(
        "--principal",
        help="Only clean this scope principal segment.",
    )
    parser.add_argument(
        "--workspace",
        help="Only clean this scope workspace segment. Use with --principal.",
    )
    parser.add_argument(
        "--apply",
        action="store_true",
        help="Actually delete rows. Without this flag the script only reports counts.",
    )
    return parser.parse_args()


def iter_feed_dbs(root: Path, principal: str | None, workspace: str | None) -> list[Path]:
    scopes_root = root / "scopes"
    if principal and workspace:
        candidate = scopes_root / principal / workspace / "ui" / "feed" / "feed.duckdb"
        return [candidate] if candidate.exists() else []
    if workspace and not principal:
        raise SystemExit("--workspace requires --principal")
    if principal:
        return sorted((scopes_root / principal).glob("*/ui/feed/feed.duckdb"))
    return sorted(scopes_root.glob("*/*/ui/feed/feed.duckdb"))


def main() -> int:
    args = parse_args()
    db_paths = iter_feed_dbs(args.root, args.principal, args.workspace)
    if not db_paths:
        print(f"No scoped feed DuckDB files found under {args.root}")
        return 0

    try:
        import duckdb
    except ImportError:
        print(
            "error: Python package 'duckdb' is required. Install it in the environment "
            "used for maintenance scripts, then rerun.",
            file=sys.stderr,
        )
        return 2

    total_removed = 0
    total_seen = 0
    mode = "apply" if args.apply else "dry-run"
    for db_path in db_paths:
        try:
            conn = duckdb.connect(str(db_path), read_only=not args.apply)
            try:
                count = conn.execute(
                    "SELECT COUNT(*) FROM feed_items WHERE item_type = 'agent_message'"
                ).fetchone()[0]
                total_seen += count
                if args.apply and count:
                    conn.execute("DELETE FROM feed_items WHERE item_type = 'agent_message'")
                    conn.execute("CHECKPOINT")
                    total_removed += count
                print(f"{mode}: {db_path}: agent_message rows={count}")
            finally:
                conn.close()
        except Exception as exc:  # noqa: BLE001 - maintenance script should continue per DB.
            print(f"warning: failed to inspect {db_path}: {exc}", file=sys.stderr)

    if args.apply:
        print(f"Removed {total_removed} legacy agent_message feed row(s).")
    else:
        print(f"Dry run found {total_seen} legacy agent_message feed row(s).")
        print("Rerun with --apply to delete them.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
