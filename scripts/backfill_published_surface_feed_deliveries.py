#!/usr/bin/env python3
"""Backfill active published surfaces into scoped Desk feed delivery rows.

Runtime publication now emits `published_surface.changed` events that the feed
materializer stores as `data_delivery` cards. Existing published surfaces from
before that projection can be backfilled explicitly with this script; normal
feed reads intentionally do not re-create rows the user removed.
"""

from __future__ import annotations

import argparse
import json
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Backfill active published surfaces as data_delivery feed rows."
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=Path("magician_data_v3"),
        help="Path to magician_data_v3 root. Defaults to ./magician_data_v3.",
    )
    parser.add_argument("--principal", help="Only backfill this principal segment.")
    parser.add_argument(
        "--workspace",
        help="Only backfill this workspace segment. Use with --principal.",
    )
    parser.add_argument(
        "--apply",
        action="store_true",
        help="Actually write rows. Without this flag the script only reports work.",
    )
    return parser.parse_args()


def iter_scope_dirs(root: Path, principal: str | None, workspace: str | None) -> list[Path]:
    scopes_root = root / "scopes"
    if principal and workspace:
        candidate = scopes_root / principal / workspace
        return [candidate] if candidate.exists() else []
    if workspace and not principal:
        raise SystemExit("--workspace requires --principal")
    if principal:
        return sorted(path for path in (scopes_root / principal).glob("*") if path.is_dir())
    return sorted(path for path in scopes_root.glob("*/*") if path.is_dir())


def parse_rfc3339_millis(value: str | None) -> int:
    if not value:
        return int(datetime.now(tz=timezone.utc).timestamp() * 1000)
    normalized = value.replace("Z", "+00:00")
    try:
        return int(datetime.fromisoformat(normalized).timestamp() * 1000)
    except ValueError:
        return int(datetime.now(tz=timezone.utc).timestamp() * 1000)


def read_surface(path: Path) -> dict[str, Any] | None:
    try:
        with path.open("r", encoding="utf-8") as handle:
            value = json.load(handle)
    except Exception as exc:  # noqa: BLE001 - maintenance should continue per record.
        print(f"warning: failed to read {path}: {exc}", file=sys.stderr)
        return None
    if not isinstance(value, dict):
        print(f"warning: skipping non-object surface record {path}", file=sys.stderr)
        return None
    return value


def delivery_item(surface: dict[str, Any]) -> dict[str, Any]:
    surface_id = str(surface["surface_id"])
    created_at = parse_rfc3339_millis(surface.get("published_at"))
    updated_at = max(parse_rfc3339_millis(surface.get("updated_at")), created_at)
    placement = surface.get("placement") if isinstance(surface.get("placement"), dict) else {}
    ui_thread_id = surface.get("ui_thread_id")
    if not ui_thread_id and placement.get("placement_kind") == "thread":
        ui_thread_id = placement.get("placement_id")
    if isinstance(ui_thread_id, str) and ui_thread_id.strip().startswith("system:"):
        ui_thread_id = None
    metadata = {
        "surface_id": surface_id,
        "surface_kind": surface.get("surface_kind"),
        "surface_status": surface.get("status"),
        "logical_surface_id": surface.get("logical_surface_id"),
        "route": surface.get("route"),
        "document_key": surface.get("document_key"),
        "source_output_id": surface.get("source_output_id"),
        "execution_id": surface.get("source_execution_id"),
        "media_type": surface.get("media_type"),
        "materialized_render_kind": surface.get("materialized_render_kind"),
        "materialized_document_key": surface.get("materialized_document_key"),
        "published_at": surface.get("published_at"),
        "updated_at": surface.get("updated_at"),
        "placement": placement,
    }
    return {
        "principal": surface.get("principal"),
        "workspace": surface.get("workspace"),
        "id": f"data_delivery:{surface_id}",
        "item_type": "data_delivery",
        "task_id": surface.get("task_id"),
        "ui_thread_id": ui_thread_id,
        "agent_id": None,
        "title": surface.get("title") or "Delivered surface",
        "summary": surface.get("summary"),
        "status": "done",
        "created_at": created_at,
        "updated_at": updated_at,
        "actions_json": "[]",
        "metadata_json": json.dumps(metadata, separators=(",", ":")),
    }


def upsert_item(conn: Any, item: dict[str, Any]) -> None:
    conn.execute(
        """
        DELETE FROM feed_items
        WHERE principal = ? AND workspace = ? AND id = ?
        """,
        [item["principal"], item["workspace"], item["id"]],
    )
    conn.execute(
        """
        INSERT INTO feed_items (
            principal, workspace, id, item_type, task_id, ui_thread_id, agent_id,
            title, summary, status, created_at, updated_at, actions_json, metadata_json
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?::JSON, ?::JSON)
        """,
        [
            item["principal"],
            item["workspace"],
            item["id"],
            item["item_type"],
            item["task_id"],
            item["ui_thread_id"],
            item["agent_id"],
            item["title"],
            item["summary"],
            item["status"],
            item["created_at"],
            item["updated_at"],
            item["actions_json"],
            item["metadata_json"],
        ],
    )


def main() -> int:
    args = parse_args()
    try:
        import duckdb
    except ImportError:
        print(
            "error: Python package 'duckdb' is required. Install it in the environment "
            "used for maintenance scripts, then rerun.",
            file=sys.stderr,
        )
        return 2

    mode = "apply" if args.apply else "dry-run"
    total_active = 0
    total_stale = 0
    total_written = 0
    total_removed = 0
    for scope_dir in iter_scope_dirs(args.root, args.principal, args.workspace):
        principal = scope_dir.parent.name
        workspace = scope_dir.name
        feed_db = scope_dir / "ui" / "feed" / "feed.duckdb"
        surfaces_dir = scope_dir / "ui" / "published_surfaces"
        if not feed_db.exists() or not surfaces_dir.exists():
            continue
        active_items: list[dict[str, Any]] = []
        stale_ids: list[str] = []
        for path in sorted(surfaces_dir.glob("*.json")):
            surface = read_surface(path)
            if not surface or not surface.get("surface_id"):
                continue
            if surface.get("status") == "active":
                active_items.append(delivery_item(surface))
            else:
                stale_ids.append(f"data_delivery:{surface['surface_id']}")
        total_active += len(active_items)
        total_stale += len(stale_ids)
        print(
            f"{mode}: {principal}/{workspace}: active_surfaces={len(active_items)} "
            f"stale_surface_rows={len(stale_ids)}"
        )
        if not args.apply:
            continue
        conn = duckdb.connect(str(feed_db), read_only=False)
        try:
            for item in active_items:
                upsert_item(conn, item)
                total_written += 1
            for stale_id in stale_ids:
                removed = conn.execute(
                    """
                    DELETE FROM feed_items
                    WHERE principal = ? AND workspace = ? AND id = ?
                    RETURNING id
                    """,
                    [principal, workspace, stale_id],
                ).fetchall()
                total_removed += len(removed)
            conn.execute("CHECKPOINT")
        finally:
            conn.close()

    if args.apply:
        print(
            f"Wrote {total_written} data_delivery row(s); "
            f"removed {total_removed} stale row(s)."
        )
    else:
        print(
            f"Dry run found {total_active} active surface(s) and "
            f"{total_stale} stale surface id(s). Rerun with --apply to write."
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
