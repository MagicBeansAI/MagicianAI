#!/usr/bin/env python3
"""Patch already-persisted chat task cards with the backfilled task summary.

A completed task's chat card (`content.type == "task_status_update"`,
`status == "completed"`) bakes its `content.summary` at completion time. Tasks
that completed BEFORE the HTML-aware summary fix shipped baked the generic
`"Execution completed."` (the old loader skipped the HTML deliverable). The
sidecar written by `scripts/backfill_task_summaries.py` (or by the live finalize
hook) is NOT read into an already-posted chat message — chat cards are immutable
snapshots.

This one-shot script rewrites those baked terminal-card summaries IN PLACE from
the `task_summary.json` sidecars, so existing chat cards show the real result.

It is conservative by default: it only rewrites a terminal `task_status_update`
card when (a) its `task_id` has a sidecar AND (b) its current summary is a
generic placeholder (`"Execution completed." / "Execution failed." / empty`).
Use `--force` to overwrite ANY summary. Writes a `.bak` of each changed file
(disable with `--no-backup`) and writes atomically.

Run AFTER `scripts/backfill_task_summaries.py` has populated the sidecars.

Examples:
  python3 scripts/patch_chat_task_summaries.py --dry-run
  python3 scripts/patch_chat_task_summaries.py
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

SIDECAR_NAME = "task_summary.json"
LLM_TAG = "\n\n_— LLM generated_"  # matches the Rust reader's appended note
GENERIC_SUMMARIES = {
    "",
    "Execution completed.",
    "Execution is complete.",
    "Execution failed.",
    "Execution cancelled.",
    "Execution was cancelled.",
}
TERMINAL_STATUSES = {"completed", "failed", "cancelled"}


def default_root() -> Path:
    for env in ("MAGICIAN_ROOT_DIR", "MAGICIAN_STORAGE_PATH"):
        val = os.environ.get(env)
        if val:
            return Path(val).expanduser()
    home = os.environ.get("HOME")
    return Path(home, "MagicianNotes") if home else Path("MagicianNotes")


def load_sidecars(root: Path) -> dict[str, str]:
    """task_id -> display summary text (with the LLM tag when model-generated)."""
    out: dict[str, str] = {}
    for sidecar in root.rglob(SIDECAR_NAME):
        if sidecar.parent.name != "outputs":
            continue
        task_id = sidecar.parent.parent.name  # .../<task_id>/outputs/task_summary.json
        try:
            d = json.loads(sidecar.read_text(encoding="utf-8"))
        except Exception as exc:  # noqa: BLE001
            print(f"  ! bad sidecar {sidecar}: {exc}", file=sys.stderr)
            continue
        text = (d.get("text") or "").strip()
        if not text:
            continue
        if d.get("generated_by") == "llm":
            text = text.rstrip() + LLM_TAG
        out[task_id] = text
    return out


def message_files(root: Path):
    # .../scopes/<principal>/<workspace>/ui/chat_sessions/<session>/messages/*.jsonl
    yield from root.glob("scopes/*/*/ui/chat_sessions/*/messages/*.jsonl")


def atomic_write(path: Path, lines: list[str]) -> None:
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text("\n".join(lines) + "\n", encoding="utf-8")
    os.replace(tmp, path)


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--root", type=Path, default=default_root())
    p.add_argument("--force", action="store_true", help="Overwrite ANY existing summary, not just generic placeholders.")
    p.add_argument("--no-backup", action="store_true", help="Do not write a .bak of each changed file.")
    p.add_argument("--dry-run", action="store_true")
    args = p.parse_args()

    root = args.root.expanduser()
    if not root.is_dir():
        print(f"error: root not found: {root}", file=sys.stderr)
        return 2

    sidecars = load_sidecars(root)
    print(f"root        : {root}")
    print(f"sidecars    : {len(sidecars)} task(s) with a task_summary.json")
    print(f"mode        : {'DRY RUN' if args.dry_run else 'WRITE'}{' (force)' if args.force else ''}\n")
    if not sidecars:
        print("nothing to do (no sidecars — run backfill_task_summaries.py first)")
        return 0

    files = patched = cards = 0
    for mf in message_files(root):
        try:
            raw_lines = mf.read_text(encoding="utf-8", errors="replace").splitlines()
        except Exception as exc:  # noqa: BLE001
            print(f"  ! read {mf}: {exc}")
            continue
        changed = False
        new_lines: list[str] = []
        for line in raw_lines:
            if not line.strip():
                new_lines.append(line)
                continue
            try:
                obj = json.loads(line)
            except Exception:  # noqa: BLE001 - leave non-JSON lines untouched
                new_lines.append(line)
                continue
            c = obj.get("content")
            if (
                isinstance(c, dict)
                and c.get("type") == "task_status_update"
                and c.get("status") in TERMINAL_STATUSES
                and c.get("task_id") in sidecars
            ):
                cur = (c.get("summary") or "").strip()
                if args.force or cur in GENERIC_SUMMARIES:
                    new = sidecars[c["task_id"]]
                    if new != c.get("summary"):
                        c["summary"] = new
                        changed = True
                        cards += 1
                        rel = mf.relative_to(root)
                        print(f"  + {rel} :: {c['task_id']} ({c.get('status')}): {cur!r} -> {new[:60]!r}…")
                        new_lines.append(json.dumps(obj, ensure_ascii=False))
                        continue
            new_lines.append(line)
        if changed:
            files += 1
            patched += 1
            if not args.dry_run:
                if not args.no_backup:
                    mf.with_suffix(mf.suffix + ".bak").write_text(
                        "\n".join(raw_lines) + "\n", encoding="utf-8"
                    )
                atomic_write(mf, new_lines)

    print(f"\ndone: files_patched={files} cards_updated={cards}"
          + ("  (dry run — nothing written)" if args.dry_run else ""))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
