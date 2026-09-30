#!/usr/bin/env python3
"""Uninstall the package view created by install_skill_layer.py.

Walks each skill subdir at `--dest` and removes only symlinks (which are the
install artifacts). Per-scope provider state lives outside the swapped package
view at `<skills>/.skill-state/<name>/config` and is intentionally retained.
Unexpected regular files inside the package view are also retained.

Inverse of `install_skill_layer.py`. Safe to re-run; idempotent.

Usage:
    uninstall_skill_layer.py --dest /abs/data/scopes/p/w/skills
    uninstall_skill_layer.py --dest .../skills --only-skill comic-strip
"""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path


def uninstall_skill_dir(skill_dir: Path) -> tuple[int, int]:
    """Remove every symlink under `skill_dir`, then prune empty dirs
    bottom-up. Returns `(symlinks_removed, real_files_kept)`."""
    if skill_dir.is_symlink():
        # Never walk through an unexpected top-level link. Removing the link
        # itself is the complete inverse operation and cannot touch its target.
        skill_dir.unlink()
        return (1, 0)
    if not skill_dir.exists():
        return (0, 0)
    removed = 0
    kept = 0
    for root, _dirs, files in os.walk(skill_dir, topdown=False):
        root_path = Path(root)
        for f in files:
            p = root_path / f
            if p.is_symlink():
                p.unlink()
                removed += 1
            else:
                kept += 1
        # os.walk does not descend into a directory symlink with its default
        # followlinks=False, and reports that entry through `dirs`, not `files`.
        # Remove the installed stable-config link without touching its target.
        for name in _dirs:
            path = root_path / name
            if path.is_symlink():
                path.unlink()
                removed += 1
        # Try to prune this dir now that its contents are processed.
        # Will succeed only if the dir is now empty (no real files,
        # no surviving subdirs).
        try:
            root_path.rmdir()
        except OSError:
            pass
    return (removed, kept)


def installed_skill_targets(skills_root: Path) -> list[Path]:
    """List package views without traversing installer-owned hidden state."""
    return sorted(
        path
        for path in skills_root.iterdir()
        if path.is_dir() and not path.is_symlink() and not path.name.startswith(".")
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--dest",
        required=True,
        type=Path,
        help="Skills root to uninstall from (e.g. <scope>/skills)",
    )
    parser.add_argument(
        "--only-skill",
        default=None,
        help="If set, uninstall only the named skill subdir; otherwise uninstall all",
    )
    args = parser.parse_args()

    if not args.dest.is_dir():
        print(f">>> nothing to uninstall (dest missing: {args.dest})")
        return 0

    if args.only_skill:
        targets = [args.dest / args.only_skill]
    else:
        # Dot-prefixed directories are installer-owned infrastructure, not
        # installed package views. In particular, walking `.skill-state`
        # would unlink template symlinks inside durable provider state.
        targets = installed_skill_targets(args.dest)

    total_removed = 0
    for skill_dir in targets:
        if not skill_dir.is_dir():
            print(f"  ! {skill_dir.name}: not a directory, skipping")
            continue
        removed, kept = uninstall_skill_dir(skill_dir)
        if removed == 0 and kept == 0 and not skill_dir.exists():
            # Was empty / fully cleaned in a previous run.
            continue
        if kept > 0:
            print(f"  ✓ {skill_dir.name}: -{removed} symlink(s), kept {kept} real file(s)")
        elif removed > 0:
            print(f"  ✓ {skill_dir.name}: -{removed} symlink(s), fully removed")
        total_removed += removed

    print(f">>> removed {total_removed} symlink(s) under {args.dest}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
