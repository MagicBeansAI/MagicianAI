#!/usr/bin/env python3
"""Queue leftover (residue) workspaces for the boot purge.

A workspace deleted before workspace tombstones existed (magician < 0.7.97), or
one whose directory a stale wake re-created, has no deletion evidence: boot
skips it ("unregistered workspace directories left on disk; not admitted") but
never removes it, and its rows in the host databases stay. This script finds,
per principal, every workspace id that is NOT registered in that principal's
`scopes/<principal>/workspaces.json` yet still has a directory under `scopes/`
or rows in the host databases, and (with --apply) appends those ids to the
registry's `pending_purge`. The next start then does the real work through the
product path: `drain_pending_purges` removes the directory and tombstones the id,
and boot retirement removes the id's host files, HITL rows, bearers and its
attention learning / resurfacing / funnel / list-index rows.

Never queued: a principal's `default` workspace, registered workspaces, the
reserved sinks (`system/system`, `_quarantine/_quarantine`), invalid names, and
principals with no registry file (nothing can vouch for or against their
workspaces; they are reported only).

Default is a read-only report. --apply refuses while magician.bin runs (the
registry is rewritten under `workspaces.json.lock`, atomically, with a backup
beside it), unless --force.

Usage:
  python3 scripts/queue_residue_workspaces.py                  # report
  python3 scripts/queue_residue_workspaces.py --apply          # queue, then restart magician
  python3 scripts/queue_residue_workspaces.py --root /path/to/MagicianNotes --principal anonymous
"""

from __future__ import annotations

import argparse
import fcntl
import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
from collections import defaultdict
from pathlib import Path

NAME = re.compile(r"[a-z0-9_-]{1,32}")
RESERVED_PRINCIPALS = {"system", "_quarantine"}
DEFAULT_WORKSPACE = "default"
REGISTRY_FILE = "workspaces.json"
# Host databases keyed by (principal, workspace); every table carrying both
# columns is scanned. Missing files are skipped.
HOST_DATABASES = (
    "attention_learning.db",
    "resurfacing.db",
    "attention_funnel.db",
    "list_index.db",
    "pending_hitl_lifecycle.v3.sqlite3",
)


def default_root() -> Path:
    for env in ("MAGICIAN_ROOT_DIR", "MAGICIAN_STORAGE_PATH"):
        val = os.environ.get(env)
        if val:
            return Path(val).expanduser()
    return Path(os.environ.get("HOME", "."), "MagicianNotes")


def database_scopes(root: Path) -> tuple[dict[tuple[str, str], int], list[str]]:
    """Row counts per (principal, workspace) across the host databases (read-only)."""
    counts: dict[tuple[str, str], int] = defaultdict(int)
    notes: list[str] = []
    for name in HOST_DATABASES:
        path = root / name
        if not path.is_file():
            continue
        try:
            con = sqlite3.connect(f"file:{path}?mode=ro", uri=True, timeout=10)
        except sqlite3.Error as error:
            notes.append(f"{name}: cannot open read-only ({error})")
            continue
        try:
            tables = [row[0] for row in con.execute("SELECT name FROM sqlite_master WHERE type='table'")]
            for table in tables:
                columns = {row[1] for row in con.execute(f'PRAGMA table_info("{table}")')}
                if {"principal", "workspace"} <= columns:
                    query = f'SELECT principal, workspace, COUNT(*) FROM "{table}" GROUP BY 1, 2'
                    for principal, workspace, n in con.execute(query):
                        counts[(str(principal), str(workspace))] += int(n)
        except sqlite3.Error as error:
            notes.append(f"{name}: scan failed ({error})")
        finally:
            con.close()
    return counts, notes


def read_registry(path: Path) -> dict | None:
    try:
        raw = path.read_bytes()
    except FileNotFoundError:
        return None
    if not raw.strip():
        return None
    return json.loads(raw)


def magician_running() -> bool:
    try:
        out = subprocess.run(["pgrep", "-f", "magician.bin"], capture_output=True, text=True)
    except FileNotFoundError:
        return False
    return bool(out.stdout.strip())


def write_registry(path: Path, data: dict) -> Path:
    backup = path.with_name(f"{path.name}.bak-{time.strftime('%Y%m%d-%H%M%S')}")
    shutil.copy2(path, backup)
    fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}.")
    try:
        os.write(fd, (json.dumps(data, indent=2) + "\n").encode())
        os.fsync(fd)
    finally:
        os.close(fd)
    os.chmod(tmp, 0o600)
    os.replace(tmp, path)
    return backup


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", type=Path, default=default_root(),
                        help="Runtime data root (default: $MAGICIAN_ROOT_DIR / $MAGICIAN_STORAGE_PATH / ~/MagicianNotes).")
    parser.add_argument("--principal", action="append", default=[],
                        help="Limit to these principal directories (repeatable).")
    parser.add_argument("--apply", action="store_true", help="Append the residue ids to pending_purge.")
    parser.add_argument("--force", action="store_true", help="Apply even while magician.bin runs.")
    args = parser.parse_args()

    root = args.root.resolve()
    scopes = root / "scopes"
    if not scopes.is_dir():
        print(f"no scopes directory under {root}", file=sys.stderr)
        return 2
    if args.apply and not args.force and magician_running():
        print("magician.bin is running; stop it first (./supervisor-ctl stop-magician) or pass --force",
              file=sys.stderr)
        return 3

    db_counts, notes = database_scopes(root)
    principals = sorted({p.name for p in scopes.iterdir() if p.is_dir()} | {p for p, _ in db_counts})
    if args.principal:
        principals = [p for p in principals if p in set(args.principal)]

    total_queued = 0
    for principal in principals:
        if principal in RESERVED_PRINCIPALS or not NAME.fullmatch(principal):
            continue
        principal_dir = scopes / principal
        registry_path = principal_dir / REGISTRY_FILE
        dirs = {d.name for d in principal_dir.iterdir() if d.is_dir()} if principal_dir.is_dir() else set()
        rows = {w: n for (p, w), n in db_counts.items() if p == principal}
        registry = read_registry(registry_path)
        if registry is None:
            if dirs or rows:
                print(f"[{principal}] no registry file: reported only, never queued")
                for workspace in sorted(dirs | set(rows)):
                    print(f"    {workspace:<40} dir={'yes' if workspace in dirs else 'no '} rows={rows.get(workspace, 0)}")
            continue
        registered = {w.get("id") for w in registry.get("workspaces", [])}
        pending = list(registry.get("pending_purge", []))
        deleted = set(registry.get("deleted", []))
        if DEFAULT_WORKSPACE not in registered and registered:
            print(f"[{principal}] registry lists no `default` row: implausible, skipped")
            continue
        candidates = (dirs | set(rows)) - registered - {DEFAULT_WORKSPACE}
        queue, invalid = [], []
        for workspace in sorted(candidates):
            if not NAME.fullmatch(workspace):
                invalid.append(workspace)
            elif workspace in pending:
                continue
            else:
                queue.append(workspace)
        print(f"[{principal}] registered={sorted(registered)} already pending={len(pending)} "
              f"tombstoned={len(deleted)} residue to queue={len(queue)}")
        for workspace in queue:
            note = " (tombstoned; rows are cleaned anyway)" if workspace in deleted else ""
            print(f"    {workspace:<40} dir={'yes' if workspace in dirs else 'no '} rows={rows.get(workspace, 0)}{note}")
        for workspace in invalid:
            print(f"    {workspace!r:<40} invalid name: skipped")
        if not queue or not args.apply:
            continue
        lock_path = principal_dir / f"{REGISTRY_FILE}.lock"
        with open(lock_path, "a+") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            fresh = read_registry(registry_path) or registry
            fresh_registered = {w.get("id") for w in fresh.get("workspaces", [])}
            fresh_pending = list(fresh.get("pending_purge", []))
            add = [w for w in queue if w not in fresh_registered and w not in fresh_pending]
            fresh["pending_purge"] = fresh_pending + add
            backup = write_registry(registry_path, fresh)
        total_queued += len(add)
        print(f"[{principal}] queued {len(add)} for purge (backup: {backup.name})")

    for note in notes:
        print(f"note: {note}")
    if args.apply:
        print(f"queued {total_queued} workspace(s); restart magician to purge them "
              "(the boot drain removes directories and tombstones the ids; retirement removes their rows)")
    else:
        print("report only; re-run with --apply (magician stopped) to queue the residue for purge")
    return 0


if __name__ == "__main__":
    sys.exit(main())
