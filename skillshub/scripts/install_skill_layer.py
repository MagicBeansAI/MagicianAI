#!/usr/bin/env python3
"""Install skills into the magician runtime tree as symlinks back to source.

Replaces the legacy `rsync -a --delete` materialization that hard-copied
every file. Symlinks let edits in `skillshub/<name>/` be live at the
runtime path without re-installing — and eliminate the drift bugs we
hit when source and runtime copies diverged.

Per-scope state below `config/` stays in a stable sibling state directory so
secrets and provider-owned sessions are never part of a package swap. The
installed skill's `config` entry points to that state; source-owned config
templates inside it remain file symlinks.

Usage:
    install_skill_layer.py --layer system  --dest /abs/data/system/skills
    install_skill_layer.py --layer scope   --dest /abs/data/scopes/p/w/skills

    # Subset install — pass a comma-separated list of skill names. The
    # `--layer` filter still applies; names that don't belong to the
    # selected layer are skipped with a warning rather than failing the
    # whole run. Useful when adding one or two new skills without
    # re-touching all 60+:
    install_skill_layer.py --layer all --dest <…>/skills \
        --names image-generation-via-minimax,video-generation-via-minimax
"""

from __future__ import annotations

import argparse
import ctypes
import errno
import fcntl
import os
import shutil
import subprocess
import sys
import threading
from contextlib import contextmanager
from pathlib import Path

# Directories at any depth that should be skipped entirely (source-only,
# typically large vendor blobs that the runtime doesn't read directly).
EXCLUDE_DIRS: set[str] = {"_vendor"}
EXCLUDE_FILES: set[str] = {".DS_Store"}
_THREAD_LOCKS_GUARD = threading.Lock()
_THREAD_LOCKS: dict[str, threading.Lock] = {}


def _layer_names(skill_layer_script: Path, layer: str, source_root: Path) -> list[str]:
    """Run the classifier with this interpreter and retain its real failure."""
    result = subprocess.run(
        [sys.executable, str(skill_layer_script), "--layer", layer, "--names-only"],
        check=False,
        capture_output=True,
        text=True,
        cwd=source_root,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "no child output"
        raise RuntimeError(
            f"skill layer classification failed for {layer!r} "
            f"with exit status {result.returncode}:\n{detail}"
        )
    return [name.strip() for name in result.stdout.splitlines() if name.strip()]


def _ensure_owned_directory(path: Path) -> None:
    if path.is_symlink() or (path.exists() and not path.is_dir()):
        raise ValueError(f"installer-owned path must be a real directory: {path}")
    path.mkdir(exist_ok=True)


@contextmanager
def skill_install_lock(dst: Path):
    key = os.path.abspath(os.fspath(dst))
    with _THREAD_LOCKS_GUARD:
        thread_lock = _THREAD_LOCKS.setdefault(key, threading.Lock())
    lock_dir = dst.parent / ".skill-install-locks"
    dst.parent.mkdir(parents=True, exist_ok=True)
    _ensure_owned_directory(lock_dir)
    lock_path = lock_dir / f"{dst.name}.lock"
    # `flock` serializes separate installer processes. The explicit process-
    # local mutex is also required because BSD flock ownership is process-wide
    # on supported macOS hosts, so sibling threads may otherwise re-enter it.
    with thread_lock:
        with lock_path.open("a+b") as handle:
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def _copy_runtime_config_into_state(source: Path, state: Path) -> None:
    if not source.is_dir() or source.is_symlink():
        return
    for root, dirs, files in os.walk(source, followlinks=False):
        dirs[:] = [name for name in dirs if not (Path(root) / name).is_symlink()]
        relative_root = Path(root).relative_to(source)
        (state / relative_root).mkdir(parents=True, exist_ok=True)
        for name in files:
            candidate = Path(root) / name
            if candidate.is_symlink() or not candidate.is_file():
                continue
            target = state / relative_root / name
            if not target.exists() and not target.is_symlink():
                shutil.copy2(candidate, target, follow_symlinks=False)


def _sync_source_config_links(source: Path, state: Path) -> None:
    if not source.is_dir():
        return
    for root, dirs, files in os.walk(source):
        relative_root = Path(root).relative_to(source)
        dirs[:] = [name for name in dirs if name not in EXCLUDE_DIRS]
        (state / relative_root).mkdir(parents=True, exist_ok=True)
        for name in files:
            if name in EXCLUDE_FILES or (*relative_root.parts, name) == (".env",):
                continue
            target = state / relative_root / name
            source_file = Path(root) / name
            # Runtime-owned regular files always win. Source-owned symlinks are
            # refreshed atomically to the latest package location.
            if target.exists() and not target.is_symlink():
                continue
            temporary = target.with_name(f".{target.name}.new")
            if temporary.exists() or temporary.is_symlink():
                temporary.unlink()
            temporary.symlink_to(source_file)
            os.replace(temporary, target)


def _atomic_exchange(left: Path, right: Path) -> None:
    """Exchange two existing paths without an absent-destination window."""
    libc = ctypes.CDLL(None, use_errno=True)
    left_bytes = os.fsencode(left)
    right_bytes = os.fsencode(right)
    if sys.platform == "darwin":
        renamex_np = libc.renamex_np
        renamex_np.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
        renamex_np.restype = ctypes.c_int
        result = renamex_np(left_bytes, right_bytes, 0x00000002)  # RENAME_SWAP
    elif sys.platform.startswith("linux") and hasattr(libc, "renameat2"):
        renameat2 = libc.renameat2
        renameat2.argtypes = [
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_uint,
        ]
        renameat2.restype = ctypes.c_int
        result = renameat2(-100, left_bytes, -100, right_bytes, 0x2)  # RENAME_EXCHANGE
    else:
        raise OSError(errno.ENOTSUP, "atomic directory exchange is unsupported")
    if result != 0:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error), f"{left} <-> {right}")


def install_skill(src: Path, dst: Path) -> None:
    """Mirror `src` at `dst` with file-level symlinks.

    Preserves every pre-existing regular file below `config/` in a stable
    sibling state directory so secrets and provider-owned OAuth/session files
    survive re-installs and concurrent provider writes.
    Idempotent: re-running rewrites source symlinks without disturbing scoped
    state.

    Builds the new layout in a sibling staging directory and atomically
    exchanges it with the installed tree under a per-skill process lock.
    """
    with skill_install_lock(dst):
        _install_skill_locked(src, dst)


def _install_skill_locked(src: Path, dst: Path) -> None:
    src = src.resolve()
    if dst.is_symlink() or (dst.exists() and not dst.is_dir()):
        raise ValueError(f"installed skill destination must be a real directory: {dst}")
    state_root = dst.parent / ".skill-state"
    state_skill = state_root / dst.name
    state_config = state_skill / "config"
    for directory in (state_root, state_skill, state_config):
        _ensure_owned_directory(directory)
    _copy_runtime_config_into_state(dst / "config", state_config)
    _sync_source_config_links(src / "config", state_config)

    dst.parent.mkdir(parents=True, exist_ok=True)
    staging = dst.parent / f"{dst.name}.new"
    # Stale staging from a prior interrupted run is safe to clear while the
    # per-skill process lock is held.
    for stale in (staging,):
        if stale.is_symlink() or stale.is_file():
            stale.unlink()
        elif stale.exists():
            shutil.rmtree(stale)

    try:
        for root, dirs, files in os.walk(src):
            rel = Path(root).relative_to(src)
            # Prune excluded dirs in-place so os.walk skips them.
            dirs[:] = [d for d in dirs if d not in EXCLUDE_DIRS]
            if rel == Path("."):
                dirs[:] = [d for d in dirs if d != "config"]
            # Create the corresponding real directory under staging.
            (staging / rel).mkdir(parents=True, exist_ok=True)
            for f in files:
                if f in EXCLUDE_FILES:
                    continue
                link = staging / rel / f
                target = src / rel / f
                # Absolute symlink so the link stays valid even if dst
                # gets moved (e.g. data root relocated).
                link.symlink_to(target)

        # Keep the state link relative to the installed skills root so the
        # complete runtime tree remains relocatable (including container
        # mounts where its host absolute prefix differs).
        (staging / "config").symlink_to(
            os.path.relpath(state_config, staging), target_is_directory=True
        )

        # Exchange two complete trees in one filesystem operation. Readers
        # always resolve either the old tree or the new tree; there is no
        # intermediate missing destination.
        had_existing = dst.exists() or dst.is_symlink()
        if had_existing:
            _atomic_exchange(staging, dst)
        else:
            os.replace(staging, dst)
        if had_existing:
            # After exchange, staging owns the complete old tree.
            if staging.is_symlink() or staging.is_file():
                staging.unlink()
            else:
                shutil.rmtree(staging)
    except BaseException:
        # The prior destination was never removed. Only the incomplete staging
        # tree needs cleanup.
        if staging.is_symlink() or staging.is_file():
            staging.unlink(missing_ok=True)
        elif staging.exists():
            shutil.rmtree(staging, ignore_errors=True)
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--layer",
        choices=("system", "scope", "all"),
        required=True,
        help="Skill layer to install (system/scope drives skill_layer.py; all = every skill)",
    )
    parser.add_argument(
        "--dest",
        required=True,
        type=Path,
        help="Destination skills root (e.g. <data>/system/skills or <scope>/skills)",
    )
    parser.add_argument(
        "--source-root",
        default=Path(__file__).resolve().parent.parent,
        type=Path,
        help="Skillshub source root (default: parent of this script)",
    )
    parser.add_argument(
        "--names",
        default="",
        help=(
            "Optional comma-separated subset of skill names to (re-)install. "
            "Names outside the selected --layer are silently skipped. Useful "
            "after adding one or two new skills."
        ),
    )
    args = parser.parse_args()
    requested_names: set[str] | None = None
    if args.names.strip():
        requested_names = {n.strip() for n in args.names.split(",") if n.strip()}

    skill_layer_script = args.source_root / "scripts" / "skill_layer.py"
    try:
        if args.layer == "all":
            # Concatenate system + scope names — these two cover every skill
            # the validator approves.
            names = []
            for sub in ("system", "scope"):
                names.extend(_layer_names(skill_layer_script, sub, args.source_root))
        else:
            names = _layer_names(skill_layer_script, args.layer, args.source_root)
    except RuntimeError as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1
    args.dest.mkdir(parents=True, exist_ok=True)

    if requested_names is not None:
        layer_set = set(names)
        unknown = sorted(requested_names - layer_set)
        if unknown:
            print(
                f"  ! requested names not in {args.layer} layer (skipped): "
                f"{', '.join(unknown)}",
                file=sys.stderr,
            )
        names = [n for n in names if n in requested_names]
        if not names:
            print(
                f"  ! no matching skills to install — check --names against "
                f"`python3 scripts/skill_layer.py --layer {args.layer} --names-only`",
                file=sys.stderr,
            )
            return 1

    installed = 0
    for name in names:
        src = args.source_root / name
        if not src.is_dir():
            print(f"  ! skipping {name}: source dir missing at {src}", file=sys.stderr)
            continue
        dst = args.dest / name
        install_skill(src, dst)
        print(f"  ✓ {name}")
        installed += 1

    suffix = " (subset)" if requested_names is not None else ""
    print(
        f">>> installed {installed} {args.layer}-layer skill(s){suffix} "
        f"to {args.dest}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
