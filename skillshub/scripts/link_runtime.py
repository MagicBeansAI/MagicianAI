#!/usr/bin/env python3
"""Add missing scoped skill files and link the verified shared software root.

Keep existing regular files, config, credentials and bot state. This repair
does not prune old scoped npm installs or replace entire skill directories.

Pinned native executables (see `pinned_bin_store`) are copied into
`<data root>/system/skill-bin/` and linked relatively, so a scope keeps its
CLIs when the checkout's generated `bin/` files are cleaned or the checkout
moves. Every other file is an absolute link to this skillshub; a link left
pointing at a checkout that no longer exists is re-pointed here.

`--all-scopes` repairs every tenant scope on disk as well as `--scope`, which
is what setup, `make -C skillshub link-scopes` and `setup-agent-browser` run so
that a workspace created after the first setup is never left behind.
"""
from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(Path(__file__).resolve().parent))
import pinned_bin_store  # noqa: E402
import scope_env  # noqa: E402
import shared_skill_bins  # noqa: E402

SKIP_DIRS = {"node_modules", ".venv", ".node", ".runtime", ".pp-build", ".cache", "_vendor", "__pycache__", ".git", "config"}
# Reserved sinks never host a tenant's skills (mirrors Rust's
# `scope_hosts_user_subsystems`).
RESERVED_SCOPES = {"system/system"}


def link(source: Path, dest: Path) -> bool:
    if dest.exists() or dest.is_symlink():
        if not dest.is_symlink() or dest.resolve() == source.resolve():
            return False
    dest.parent.mkdir(parents=True, exist_ok=True)
    temporary = dest.with_name(f".{dest.name}.{os.getpid()}.new")
    temporary.symlink_to(source.absolute())
    os.replace(temporary, dest)
    return True


def user_owned(scoped_skill: Path) -> bool:
    """A scope's own copy of a skill (real SKILL.md) is never overlaid.

    A SKILL.md the retired renderer wrote (`.skill-render.json` beside it) is
    setup-managed, not scope-owned: it is relinked to the shared manifest."""
    manifest = scoped_skill / "SKILL.md"
    return (
        manifest.exists()
        and not manifest.is_symlink()
        and not scope_env.is_legacy_rendered(scoped_skill)
    )


def scope_dirs(data_root: Path, scope: str, all_scopes: bool, existing_only: bool) -> list[Path]:
    scopes_root = data_root / "scopes"
    chosen = [] if existing_only and not (scopes_root / scope).is_dir() else [scopes_root / scope]
    if all_scopes:
        for path in sorted(scopes_root.glob("*/*")):
            name = path.relative_to(scopes_root).as_posix()
            if path.is_dir() and name not in RESERVED_SCOPES and path not in chosen:
                chosen.append(path)
    return chosen


def link_scope(scope: Path, names: set[str] | None, store_root: Path) -> tuple[int, int]:
    """Returns (links changed, skills skipped as scope-owned)."""
    changed = skipped = 0
    resolver = shared_skill_bins.Resolver(ROOT)
    for manifest in sorted(ROOT.glob("*/SKILL.md")):
        skill = manifest.parent
        if names is not None and skill.name not in names:
            continue
        scoped_skill = scope / "skills" / skill.name
        if user_owned(scoped_skill) or shared_skill_bins.scope_removed(scoped_skill):
            skipped += 1
            continue
        changed += resolver.wire(skill)
        stored, store_changes = pinned_bin_store.store_skill_bins(ROOT, skill, scoped_skill, store_root)
        changed += store_changes
        # A manifest an older setup rendered into this scope becomes the
        # shared link again; its `${NAME}` values resolve from config/.env.
        retired = scope_env.retire_rendered_manifest(skill, scoped_skill)
        changed += int(retired == "relinked")
        if retired == "edited":
            print(f"  ! {skill.name}: rendered SKILL.md was edited after setup wrote it; left as it is")
        for directory, dirs, files in os.walk(skill):
            dirs[:] = [d for d in dirs if d not in SKIP_DIRS]
            for name in files:
                if name in (".DS_Store", ".env"):
                    continue
                source = Path(directory) / name
                relative = source.relative_to(skill)
                if relative.parent == Path("bin") and name in stored:
                    continue
                if source.is_file():
                    changed += int(link(source, scoped_skill / relative))
    return changed, skipped


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-root", required=True, type=Path)
    parser.add_argument("--scope", default="anonymous/default")
    parser.add_argument("--all-scopes", action="store_true",
                        help="also repair every other tenant scope under <data-root>/scopes")
    parser.add_argument("--existing-only", action="store_true",
                        help="do not create --scope when it does not exist yet")
    parser.add_argument("--names", default="",
                        help="comma-separated skill names to link (default: every skill)")
    parser.add_argument("--prune-bin-store", action="store_true",
                        help="with --all-scopes, delete stored executables no scope links to")
    args = parser.parse_args()
    scope_parts = Path(args.scope).parts
    if len(scope_parts) != 2 or any(p in (".", "..") for p in scope_parts) or Path(args.scope).is_absolute():
        parser.error("scope must be principal/workspace")
    if args.prune_bin_store and not args.all_scopes:
        parser.error("--prune-bin-store needs --all-scopes: a store entry is only unused if no scope links to it")
    names = {n.strip() for n in args.names.split(",") if n.strip()} or None
    link(ROOT, args.data_root / "system/skillshub")
    store_root = args.data_root / "system" / pinned_bin_store.STORE_DIRNAME
    for scope in scope_dirs(args.data_root, args.scope, args.all_scopes, args.existing_only):
        changed, skipped = link_scope(scope, names, store_root)
        note = f", {skipped} scope-owned or removed skill(s) left alone" if skipped else ""
        print(f"✓ scoped runtime files linked: {scope} ({changed} link(s) changed{note})")
    if args.prune_bin_store:
        for removed in pinned_bin_store.prune_store(args.data_root / "scopes", store_root):
            print(f"  · pruned unused stored executable {removed.name}")


if __name__ == "__main__":
    main()
