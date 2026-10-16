#!/usr/bin/env python3
"""Reviewed Task 16A compatibility shim for skillshub operator batches.

Closed Magician-launched skills must not inherit ``MAGICIAN_ROOT_DIR`` or
``MAGICIAN_STORAGE_PATH``. Operator setup scripts that still need the live
runtime root must call this module instead of resolving those variables
directly. The shim cannot expose arbitrary engine roots; it only returns the
same runtime-root translation Magician itself uses and emits a deprecation
metric so conversion progress is measured.

Usage:
    from runtime_root_shim import legacy_runtime_root
    root = legacy_runtime_root(owner="list_skills")

    python3 skillshub/scripts/runtime_root_shim.py --owner cloak-browser-smoke
"""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path


def legacy_runtime_root(*, owner: str) -> Path:
    """Translate the historical runtime-root env vars without leaking extra roots."""
    if not owner.strip():
        raise ValueError("compatibility shim requires an owner id")
    raw = (os.environ.get("MAGICIAN_ROOT_DIR") or "").strip()
    if not raw:
        raw = (os.environ.get("MAGICIAN_STORAGE_PATH") or "").strip()
    if raw:
        root = Path(raw).expanduser()
    else:
        home = os.environ.get("HOME")
        root = Path(home).expanduser() / "MagicianNotes" if home else Path("MagicianNotes")
    sys.stderr.write(
        f"magician.storage.subprocess.compat owner={owner} count=1\n"
    )
    return root


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Print the compatibility-shim runtime root (operator batches only)."
    )
    parser.add_argument("--owner", required=True, help="catalog or script owner id")
    args = parser.parse_args()
    print(legacy_runtime_root(owner=args.owner))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
