#!/usr/bin/env python3
"""Remove the regenerable runtime install tree under a Magician data root.

Removed whole (regenerable by install-scope + setup-env):
  - <data_root>/system/skills/
  - <data_root>/system/skill-bin/          (stored pinned executables; link-scopes)
  - <data_root>/scopes/*/*/skills/
  - <data_root>/scopes/*/*/capabilities/   (legacy umbrella)

Pruned, not removed, so per-scope state survives:
  - <data_root>/scopes/*/*/bots/
      kept:    bot_configs.yaml          (operator's enabled/disabled choices)
               <bot>/.env*               (secrets, tokens; see setup_bot_envs.py)
               <bot>/workdirs/           (runtime-managed bot state)
      removed: everything else (dist/ shims, legacy npm installs, stray files)

Deleting the whole bots/ tree used to take every bot's env file with it while
the target announced that secrets were preserved; the bots then failed to start
until someone regenerated their env files by hand.
"""

from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path

BOTS_KEEP_TOP = {"bot_configs.yaml"}
BOT_KEEP_DIRS = {"workdirs"}


def _bot_entry_kept(entry: Path) -> bool:
    return entry.name.startswith(".env") or (
        entry.name in BOT_KEEP_DIRS and entry.is_dir() and not entry.is_symlink()
    )


def _remove(path: Path) -> None:
    if path.is_dir() and not path.is_symlink():
        shutil.rmtree(path)
    else:
        path.unlink()


def bots_removals(bots_root: Path) -> list[Path]:
    """Paths under one scope's bots/ that clean-runtime deletes."""
    removals: list[Path] = []
    for entry in sorted(bots_root.iterdir()):
        if entry.name in BOTS_KEEP_TOP and not entry.is_dir():
            continue
        if entry.is_dir() and not entry.is_symlink():
            removals.extend(
                child for child in sorted(entry.iterdir()) if not _bot_entry_kept(child)
            )
        else:
            removals.append(entry)
    return removals


def whole_tree_targets(data_root: Path) -> list[Path]:
    targets = [data_root / "system" / "skills", data_root / "system" / "skill-bin"]
    targets += sorted(data_root.glob("scopes/*/*/skills"))
    targets += sorted(data_root.glob("scopes/*/*/capabilities"))
    return [target for target in targets if target.is_dir()]


def clean(data_root: Path, dry: bool, out=sys.stdout) -> None:
    verb = "would remove" if dry else "removed"
    for target in whole_tree_targets(data_root):
        if not dry:
            shutil.rmtree(target)
        print(f"  {'[dry] ' if dry else '✓ '}{verb} {target}", file=out)
    for bots_root in sorted(data_root.glob("scopes/*/*/bots")):
        if not bots_root.is_dir():
            continue
        removals = bots_removals(bots_root)
        for path in removals:
            if not dry:
                _remove(path)
        print(
            f"  {'[dry] ' if dry else '✓ '}{verb} {len(removals)} item(s) under {bots_root}"
            " (kept bot_configs.yaml, <bot>/.env*, <bot>/workdirs/)",
            file=out,
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--data-root", required=True)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    clean(Path(args.data_root).expanduser(), args.dry_run)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
