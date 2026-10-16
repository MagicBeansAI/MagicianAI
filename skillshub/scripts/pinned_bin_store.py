#!/usr/bin/env python3
"""Content-addressed store for the native executables skills pin.

A scope's skills layer is a mirror of `skillshub/<skill>/` made of absolute
symlinks into the checkout (or the installed package). That keeps scripts and
SKILL.md edits live, but it ties every scope to files the checkout regenerates:
`skillshub/browser/bin/agent-browser` is an ignored build output, and when it
disappeared on 2026-09-30 every scope's browser link dangled at once.

Native executables that skillshub's own setup builds or fetches (agent-browser,
OfficeCLI, the Metabase CLI, document-to-markdown) are therefore copied once
into `<data root>/system/skill-bin/<sha256>/<name>`, and each scope's
`skills/<skill>/bin/<name>` is a RELATIVE symlink to that copy. The scope keeps
working when the checkout is cleaned, re-cloned or moved, and a data root that
is copied or moved keeps its links because they never leave the data root.

Which entries qualify (`store_candidate`): a top-level entry of
`<skill>/bin/` that resolves to a regular, executable Mach-O or ELF file inside
the skillshub tree, outside `node_modules/` and `.venv/`. npm and Python own the
layout of those trees (dugite's git, for one, finds its helpers relative to its
own location), and host tools such as Homebrew's pdftotext are not pinned, so
those stay plain symlinks.

The Rust runtime applies the same rule when it reconciles a scope
(`magician::magician_v2::skills::scope_layer`). Keep the two in step.
"""
from __future__ import annotations

import hashlib
import os
import shutil
from pathlib import Path

STORE_DIRNAME = "skill-bin"
NATIVE_MAGIC = (
    b"\xcf\xfa\xed\xfe",  # Mach-O 64-bit, little endian
    b"\xce\xfa\xed\xfe",  # Mach-O 32-bit, little endian
    b"\xfe\xed\xfa\xcf",  # Mach-O 64-bit, big endian
    b"\xfe\xed\xfa\xce",  # Mach-O 32-bit, big endian
    b"\xca\xfe\xba\xbe",  # Mach-O universal
    b"\xca\xfe\xba\xbf",  # Mach-O universal, 64-bit offsets
    b"\x7fELF",
)
UNOWNED_TREES = {"node_modules", ".venv"}


def store_root_for(skills_root: Path) -> Path | None:
    """`<root>/system/skill-bin` for a `<root>/scopes/<p>/<w>/skills` tree.

    Any other destination (a container's private layer, a test fixture) gets no
    store, and its executables stay plain source symlinks.
    """
    skills_root = Path(os.path.abspath(skills_root))
    parents = skills_root.parents
    if skills_root.name != "skills" or len(parents) < 4 or parents[2].name != "scopes":
        return None
    return parents[3] / "system" / STORE_DIRNAME


def is_native_executable(path: Path) -> bool:
    try:
        if not path.is_file() or not os.access(path, os.X_OK):
            return False
        with path.open("rb") as handle:
            return handle.read(4) in NATIVE_MAGIC
    except OSError:
        return False


def store_candidate(skillshub_root: Path, entry: Path) -> Path | None:
    """Return the real file to store for a `<skill>/bin/<name>` entry, or None."""
    try:
        resolved = entry.resolve(strict=True)
        relative = resolved.relative_to(skillshub_root.resolve())
    except (OSError, ValueError, RuntimeError):
        return None
    if UNOWNED_TREES.intersection(relative.parts):
        return None
    return resolved if is_native_executable(resolved) else None


def native_bin_entries(skillshub_root: Path, skill_src: Path) -> dict[str, Path]:
    """`{name: resolved file}` for every storable entry of `<skill>/bin/`."""
    bin_dir = skill_src / "bin"
    if not bin_dir.is_dir():
        return {}
    entries = {}
    for entry in sorted(bin_dir.iterdir()):
        candidate = store_candidate(skillshub_root, entry)
        if candidate is not None:
            entries[entry.name] = candidate
    return entries


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def materialize(store_root: Path, name: str, source: Path) -> Path:
    """Copy `source` into the store once; return the stored file."""
    digest = sha256_file(source)
    stored = store_root / digest / name
    if stored.is_file() and stored.stat().st_size == source.stat().st_size:
        return stored
    stored.parent.mkdir(parents=True, exist_ok=True)
    temporary = stored.with_name(f".{name}.{os.getpid()}.tmp")
    shutil.copyfile(source, temporary)
    os.chmod(temporary, 0o755)
    os.replace(temporary, stored)
    return stored


def link_to_store(stored: Path, scoped: Path) -> bool:
    """Point `scoped` at `stored` with a relative link. True when it changed.

    A regular file at `scoped` belongs to the scope and is left alone.
    """
    desired = os.path.relpath(stored, scoped.parent)
    if scoped.is_symlink():
        if os.readlink(scoped) == desired:
            return False
    elif scoped.exists():
        return False
    scoped.parent.mkdir(parents=True, exist_ok=True)
    temporary = scoped.with_name(f".{scoped.name}.{os.getpid()}.new")
    if temporary.is_symlink() or temporary.exists():
        temporary.unlink()
    temporary.symlink_to(desired)
    os.replace(temporary, scoped)
    return True


def store_skill_bins(skillshub_root: Path, skill_src: Path, scoped_skill: Path,
                     store_root: Path) -> tuple[set[str], int]:
    """Store every pinned native executable of one skill and link it.

    Returns the handled names (callers must not overwrite them with source
    symlinks) and how many scoped links changed.
    """
    handled: set[str] = set()
    changed = 0
    for name, source in native_bin_entries(skillshub_root, skill_src).items():
        stored = materialize(store_root, name, source)
        changed += int(link_to_store(stored, scoped_skill / "bin" / name))
        handled.add(name)
    return handled, changed


def referenced_store_dirs(scopes_root: Path, store_root: Path) -> set[Path]:
    """Store digests any `<scope>/skills/*/bin/*` link still points at."""
    referenced: set[Path] = set()
    store_root = Path(os.path.abspath(store_root))
    for link in scopes_root.glob("*/*/skills/*/bin/*"):
        if not link.is_symlink():
            continue
        target = Path(os.path.abspath(link.parent / os.readlink(link)))
        try:
            relative = target.relative_to(store_root)
        except ValueError:
            continue
        if relative.parts:
            referenced.add(store_root / relative.parts[0])
    return referenced


def prune_store(scopes_root: Path, store_root: Path) -> list[Path]:
    """Remove store entries no scope references. Only safe over ALL scopes."""
    if not store_root.is_dir():
        return []
    keep = referenced_store_dirs(scopes_root, store_root)
    removed = []
    for entry in sorted(store_root.iterdir()):
        if entry.is_dir() and not entry.is_symlink() and Path(os.path.abspath(entry)) not in keep:
            shutil.rmtree(entry)
            removed.append(entry)
    return removed
