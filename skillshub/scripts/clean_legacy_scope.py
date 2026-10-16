#!/usr/bin/env python3
"""One-shot cleanup + migration of the pre-skills `capabilities/`
umbrella under each scope.

The runtime layout used to be:
  <scope>/capabilities/{bots,auth,workdirs,bot_configs.yaml,
                         node_modules,package.json,package-lock.json,
                         tools/, config/, packs/, ...}

Now:
  <scope>/{skills,bots,auth,workdirs,bot_configs.yaml}

This script does two passes per scope (idempotent):

PASS 1 — Migrate live state from `<scope>/capabilities/` to scope root:
  bots/, auth/, workdirs/, bot_configs.yaml are MOVED up one level.
  If a target already exists at scope root, the legacy copy is left
  in place and a warning printed (manual resolution required).

PASS 2 — Drop dead pre-bundling artifacts (whether at scope root OR
under the legacy capabilities/ umbrella, since some scopes might be
mid-migration):
  - node_modules/, package.json, package-lock.json (root-level npm)
  - tools/{node_modules,package-lock.json}
  - config/{.env, .env.development, .env.example, gws/, README.md}
  - per-bot bot_dir/{src,test,node_modules,package.json,
                       package-lock.json,tsconfig.json,README.md,
                       CHANGELOG.md}
  - per-bot dist/*.d.ts, dist/*.js.map (only dist/index.js is kept)
  Finally: remove `<scope>/capabilities/` itself if empty.

Preserves: per-bot dist/index.js, .env.*, bot_configs.yaml, the new
scope-root tree (skills/, bots/, auth/, workdirs/), and anything not
explicitly listed.

Usage:
  python3 skillshub/scripts/clean_legacy_scope.py
  python3 skillshub/scripts/clean_legacy_scope.py --scope alice/staging
  python3 skillshub/scripts/clean_legacy_scope.py --all-scopes
  python3 skillshub/scripts/clean_legacy_scope.py --dry-run
"""
from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
DEFAULT_SCOPE = "anonymous/default"

# Items to lift from `<scope>/capabilities/` up to `<scope>/` if they
# still live under the old umbrella. Skills/ is intentionally NOT in
# this list — workspace-layer skill overrides are already at scope
# root by convention; capabilities never held them.
LIFT_FROM_CAPS = (
    "bots",
    "auth",
    "workdirs",
    "bot_configs.yaml",
)

# Items to delete unconditionally (no migration value).
SCOPE_ROOT_REMOVE = (
    "node_modules",
    "package.json",
    "package-lock.json",
)
# Per-skill `.env` now lives at <data_root>/system/skills/<skill>/
# config/.env (populated by setup-env). The legacy shared scope-level
# files are dead. `gws/` was a master + symlinks for the OAuth client;
# now each `auth/gws-<account>/` holds its own real copy.
CONFIG_REMOVE = (
    ".env",
    ".env.development",
    ".env.example",
    "gws",
)
TOOLS_REMOVE = (
    "node_modules",
    "package-lock.json",
)
BOT_DIR_REMOVE = (
    "src",
    "test",
    "node_modules",
    "package.json",
    "package-lock.json",
    "tsconfig.json",
    "README.md",
    "CHANGELOG.md",
)
BOT_DIST_KEEP = ("index.js",)


def total_size(path: Path) -> int:
    if path.is_file() or path.is_symlink():
        try:
            return path.stat().st_size
        except OSError:
            return 0
    total = 0
    if path.is_dir():
        for child in path.rglob("*"):
            if child.is_file() and not child.is_symlink():
                try:
                    total += child.stat().st_size
                except OSError:
                    pass
    return total


def fmt_size(n: int) -> str:
    for unit in ("B", "K", "M", "G"):
        if n < 1024:
            return f"{n}{unit}"
        n //= 1024
    return f"{n}T"


def remove(path: Path, dry: bool) -> int:
    if not path.exists() and not path.is_symlink():
        return 0
    size = total_size(path)
    if dry:
        print(f"  [dry] would remove {path}  ({fmt_size(size)})")
        return size
    if path.is_symlink() or path.is_file():
        path.unlink()
    else:
        shutil.rmtree(path)
    print(f"  ✓ removed {path}  ({fmt_size(size)})")
    return size


def migrate_to_scope_root(scope_root: Path, dry: bool) -> int:
    """Lift live state out of the legacy `capabilities/` umbrella.
    Returns the number of items migrated."""
    caps_root = scope_root / "capabilities"
    if not caps_root.is_dir():
        return 0
    moved = 0
    for name in LIFT_FROM_CAPS:
        src = caps_root / name
        dst = scope_root / name
        if not src.exists():
            continue
        if dst.exists():
            print(f"  ⚠  {dst} already exists; leaving legacy {src} in place "
                  f"(manual resolution needed)")
            continue
        if dry:
            print(f"  [dry] would move {src} → {dst}")
            moved += 1
            continue
        shutil.move(str(src), str(dst))
        print(f"  ✓ moved {src.name}/  ({src} → {dst})")
        moved += 1
    return moved


def migrate_bot_configs_into_bots(scope_root: Path, dry: bool) -> int:
    """Move scope-root `bot_configs.yaml` into `<scope>/bots/` since
    it's a bot-launcher concern. Idempotent."""
    src = scope_root / "bot_configs.yaml"
    if not src.is_file():
        return 0
    dst_dir = scope_root / "bots"
    dst = dst_dir / "bot_configs.yaml"
    if dst.exists():
        if dry:
            print(f"  [dry] would remove duplicate {src} (bots/ copy already exists)")
            return 0
        src.unlink()
        print(f"  ✓ removed duplicate {src} (bots/ copy already in place)")
        return 1
    if dry:
        print(f"  [dry] would move {src} → {dst}")
        return 1
    dst_dir.mkdir(parents=True, exist_ok=True)
    shutil.move(str(src), str(dst))
    print(f"  ✓ moved bot_configs.yaml  ({src} → {dst})")
    return 1


def remove_ds_store_files(scope_root: Path, dry: bool) -> int:
    """Nuke macOS Finder droppings under the migration-relevant scope
    subtrees. Intentionally NOT a full rglob of `scope_root` — long-
    lived production scopes hold gigabytes of conversation history
    under tasks/, chat/, memory/, agent_runtime/, and walking those
    just to find Finder turds is wasteful. Restrict to dirs that
    operators are likely to inspect with Finder during setup."""
    if not scope_root.is_dir():
        return 0
    freed = 0
    for sub in ("bots", "auth", "workdirs", "skills", "config", "capabilities"):
        target = scope_root / sub
        if not target.is_dir():
            continue
        for ds in target.rglob(".DS_Store"):
            freed += remove(ds, dry)
    # Plus any .DS_Store directly at scope root.
    direct = scope_root / ".DS_Store"
    if direct.is_file() or direct.is_symlink():
        freed += remove(direct, dry)
    return freed


def _clean_under(root: Path, dry: bool) -> int:
    """Apply the dead-artifact cleanup rules to either `<scope>/` or
    `<scope>/capabilities/` (since both might be mid-migration).
    """
    freed = 0

    for name in SCOPE_ROOT_REMOVE:
        freed += remove(root / name, dry)

    config_dir = root / "config"
    if config_dir.is_dir():
        for name in CONFIG_REMOVE:
            freed += remove(config_dir / name, dry)
        freed += remove(config_dir / "README.md", dry)
        try:
            if not any(config_dir.iterdir()):
                freed += remove(config_dir, dry)
        except FileNotFoundError:
            pass

    tools_dir = root / "tools"
    if tools_dir.is_dir():
        for name in TOOLS_REMOVE:
            freed += remove(tools_dir / name, dry)

    bots_dir = root / "bots"
    if bots_dir.is_dir():
        for entry in sorted(bots_dir.iterdir()):
            if not entry.is_dir():
                continue
            for name in BOT_DIR_REMOVE:
                freed += remove(entry / name, dry)
            dist = entry / "dist"
            if dist.is_dir():
                # Bundled bots ship a single self-contained `index.js`.
                # Drop everything else (.d.ts, .js.map, stray pre-bundle
                # output files, etc.) — there's no other valid resident.
                for child in dist.iterdir():
                    if child.name in BOT_DIST_KEEP:
                        continue
                    freed += remove(child, dry)
    return freed


def clean_scope(scope_root: Path, dry: bool) -> int:
    if not scope_root.is_dir():
        print(f"  — skipping {scope_root}: not a directory")
        return 0
    print(f"== {scope_root} ==")
    freed = 0

    moved = migrate_to_scope_root(scope_root, dry)
    if moved:
        print(f"  migrated {moved} item(s) from capabilities/ to scope root")

    moved_bc = migrate_bot_configs_into_bots(scope_root, dry)
    if moved_bc:
        print(f"  bot_configs.yaml: now under bots/ where it belongs")

    freed += remove_ds_store_files(scope_root, dry)
    freed += _clean_under(scope_root, dry)

    caps_root = scope_root / "capabilities"
    if caps_root.is_dir():
        # Catch any dead artifacts left under the umbrella.
        freed += _clean_under(caps_root, dry)
        # Drop the umbrella itself if it's now empty.
        try:
            if not any(caps_root.iterdir()):
                freed += remove(caps_root, dry)
        except FileNotFoundError:
            pass

    return freed


def discover_scopes(data_root: Path) -> list[Path]:
    out: list[Path] = []
    scopes_root = data_root / "scopes"
    if not scopes_root.is_dir():
        return out
    for principal in sorted(scopes_root.iterdir()):
        if not principal.is_dir():
            continue
        for workspace in sorted(principal.iterdir()):
            if workspace.is_dir():
                out.append(workspace)
    return out


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--scope", default=None,
                        help="<principal>/<workspace> (default: %s)" % DEFAULT_SCOPE)
    parser.add_argument("--all-scopes", action="store_true",
                        help="clean every scope under data-root/scopes/")
    parser.add_argument("--data-root", default=str(REPO / "magician_data_v3"))
    parser.add_argument("--dry-run", action="store_true",
                        help="print what would be done without changing anything")
    args = parser.parse_args()

    data_root = Path(args.data_root)

    if args.all_scopes:
        targets = discover_scopes(data_root)
        if not targets:
            print(f"no scopes found under {data_root}/scopes/")
            return 0
    else:
        scope = args.scope or DEFAULT_SCOPE
        targets = [data_root / "scopes" / scope]

    total = 0
    for scope_root in targets:
        total += clean_scope(scope_root, args.dry_run)

    label = "would free" if args.dry_run else "freed"
    print(f"\n{label} {fmt_size(total)} total")
    return 0


if __name__ == "__main__":
    sys.exit(main())
