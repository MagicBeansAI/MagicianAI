#!/usr/bin/env python3
"""Repair governed LLM fact rows that carry a chat session id as `task_id`.

Before magicllm 0.2.35 the dispatcher merged a chat turn's session-keyed
`TaskRef` into the trace context: every chat call and provider-attempt row was
written with `task_id = chat_session_id` and `scope_resolution = 'inherited'`,
while the turn's own tool-lineage rows (built from the un-merged context) carry
no task and `'explicit'`. The governed read refuses a whole day partition for
that ownership drift. This rewrites the affected rows in place:

  task_id           -> NULL      where task_id = chat_session_id
  scope_resolution  -> explicit  on those same rows

for the raw batch files AND the compacted file of each affected partition,
then refreshes the compaction manifest's byte length and checksum so the
compacted file stays usable. Dry-run by default; nothing is written without
`--apply`.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import tempfile
from pathlib import Path

import duckdb

DATASETS = ("llm_calls", "llm_provider_attempts", "llm_tool_calls")
PREDICATE = "task_id IS NOT NULL AND task_id = chat_session_id"
# Row-conditional: only a session-keyed row is rewritten; every other row in
# the same file is carried through byte-for-byte in value. (An earlier draft
# applied the REPLACE to every row of a selected file and nulled the task id
# of unrelated agentic rows; the backup-and-restore that followed is why this
# is conditional and why `--apply` still insists on a backup directory.)
FIX = (
    "SELECT * REPLACE ("
    f"CASE WHEN {PREDICATE} THEN NULL ELSE task_id END AS task_id, "
    f"CASE WHEN {PREDICATE} AND scope_resolution = 'inherited' THEN 'explicit' ELSE scope_resolution END AS scope_resolution)"
)


def blake3_hex(path: Path) -> str:
    try:
        import blake3  # type: ignore

        h = blake3.blake3()
    except ImportError:
        sys.exit("python blake3 module required for manifest refresh: pip install blake3")
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def affected_rows(con: duckdb.DuckDBPyConnection, path: Path) -> int:
    cols = {r[0] for r in con.execute("DESCRIBE SELECT * FROM read_parquet(?)", [str(path)]).fetchall()}
    if "task_id" not in cols or "chat_session_id" not in cols:
        return 0
    return con.execute(f"SELECT count(*) FROM read_parquet(?) WHERE {PREDICATE}", [str(path)]).fetchone()[0]


def rewrite(con: duckdb.DuckDBPyConnection, path: Path) -> None:
    tmp = path.with_name(path.name + ".repair.tmp")
    escaped_tmp = str(tmp).replace("'", "''")
    con.execute(
        f"COPY ({FIX} FROM read_parquet(?)) TO '{escaped_tmp}' (FORMAT PARQUET, COMPRESSION ZSTD)",
        [str(path)],
    )
    os.replace(tmp, path)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--root", default=os.environ.get("MAGICIAN_ROOT_DIR", str(Path.home() / "MagicianNotes")))
    ap.add_argument("--date", action="append", dest="dates", help="partition date(s) to repair, e.g. 2026-09-10")
    ap.add_argument("--apply", action="store_true", help="write changes (default: dry run)")
    ap.add_argument("--backup-dir", help="required with --apply: every file is copied here before it is rewritten")
    args = ap.parse_args()
    root = Path(args.root)
    con = duckdb.connect()
    con.execute("SET memory_limit='2GB'")
    con.execute("SET threads=2")
    total_rows = 0
    plan: list[tuple[Path, int, Path | None]] = []
    for scope_dir in sorted(root.glob("scopes/*/*/analytics")):
        for dataset in DATASETS:
            for part in sorted((scope_dir / dataset).glob("dt=*")):
                if args.dates and part.name[len("dt=") :] not in args.dates:
                    continue
                manifest = part / "_compact" / "canonical.compaction-manifest.json"
                for f in sorted(part.glob("*.parquet")) + sorted((part / "_compact").glob("*.parquet")):
                    n = affected_rows(con, f)
                    if n:
                        plan.append((f, n, manifest if f.parent.name == "_compact" else None))
                        total_rows += n
    if not plan:
        print("nothing to repair")
        return 0
    by_part: dict[Path, tuple[int, int]] = {}
    for f, n, _ in plan:
        part = f.parent.parent if f.parent.name == "_compact" else f.parent
        files, rows = by_part.get(part, (0, 0))
        by_part[part] = (files + 1, rows + n)
    for part, (files, rows) in by_part.items():
        print(f"{'REPAIR' if args.apply else 'would repair'} {part.relative_to(root)}: {files} file(s), {rows} row(s)")
    print(f"total affected rows: {total_rows}")
    if not args.apply:
        print("dry run; re-run with --apply to write")
        return 0
    if not args.backup_dir:
        sys.exit("--apply requires --backup-dir")
    import shutil

    backup_root = Path(args.backup_dir)
    for f, _, manifest in plan:
        for src in (f, manifest):
            if src is None or not src.exists():
                continue
            dest = backup_root / src.relative_to(root)
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(src, dest)
    print(f"backed up {len(plan)} file(s) under {backup_root}")
    for f, n, manifest in plan:
        rewrite(con, f)
        print(f"  rewrote {f.relative_to(root)} ({n} rows)")
        if manifest is not None and manifest.exists():
            data = json.loads(manifest.read_text())
            data["compacted_byte_len"] = f.stat().st_size
            data["compacted_checksum_blake3"] = blake3_hex(f)
            tmp = manifest.with_name(manifest.name + ".tmp")
            tmp.write_text(json.dumps(data, indent=2))
            os.replace(tmp, manifest)
            print(f"  refreshed manifest {manifest.relative_to(root)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
