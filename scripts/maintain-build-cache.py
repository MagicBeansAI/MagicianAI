#!/usr/bin/env python3
"""Bound old Rust incremental caches while holding Cargo's profile build lock.

Never traverses target symlinks, deps, source snapshots, models, or runtime data.
Active Cargo builds cause a skip; a new build waits on the same profile lock.
"""
import argparse
import fcntl
import os
from pathlib import Path
import shutil
import stat
import time


def maintain(target, budget_bytes, min_age_seconds, dry_run=False):
    target = Path(target).resolve(strict=True)
    removed = []
    for profile in ('debug', 'release'):
        base = target / profile
        cache = base / 'incremental'
        if base.is_symlink() or cache.is_symlink() or not cache.is_dir():
            continue
        flags = os.O_RDWR | os.O_CREAT | getattr(os, 'O_NOFOLLOW', 0)
        try:
            fd = os.open(base / '.cargo-lock', flags, 0o600)
        except OSError:
            print(f'build-cache: skipped {profile}: cannot safely acquire Cargo lock')
            continue
        with os.fdopen(fd, 'a') as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                print(f'build-cache: skipped {profile}: build is active')
                continue
            candidates = []
            for path in cache.iterdir():
                if path.is_symlink() or not path.is_dir():
                    continue
                # The top directory can retain an old mtime while its sessions
                # are being replaced. The newest descendant controls retention.
                newest = path.stat().st_mtime
                size = 0
                for root, dirs, files in os.walk(path, followlinks=False):
                    for name in dirs + files:
                        meta = (Path(root) / name).lstat()
                        newest = max(newest, meta.st_mtime)
                        if stat.S_ISREG(meta.st_mode):
                            size += meta.st_size
                candidates.append((newest, path, size))
            total = sum(row[2] for row in candidates)
            for newest, path, size in sorted(candidates):
                if total <= budget_bytes:
                    break
                if time.time() - newest < min_age_seconds:
                    continue
                if not dry_run:
                    shutil.rmtree(path)
                total -= size
                removed.append(str(path))
                print(f'build-cache: {"would prune" if dry_run else "pruned"} {path.name} ({size // 1048576} MiB)')
            print(f'build-cache: {profile} incremental {total // 1048576} MiB; budget {budget_bytes // 1048576} MiB')
    return removed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target-dir', default=os.environ.get('CARGO_TARGET_DIR'))
    parser.add_argument('--budget-gb', type=float, default=float(os.environ.get('INCREMENTAL_BUDGET_GB', '40')))
    parser.add_argument('--min-age-days', type=float, default=7)
    parser.add_argument('--dry-run', action='store_true')
    args = parser.parse_args()
    if not args.target_dir or args.budget_gb < 0 or args.min_age_days < 1:
        parser.error('target directory, nonnegative budget, and minimum age >= 1 day are required')
    if not Path(args.target_dir).exists():
        return
    maintain(args.target_dir, int(args.budget_gb * 1024**3), args.min_age_days * 86400, args.dry_run)
    free = shutil.disk_usage(args.target_dir).free
    if free < 3 * 1024**3:
        raise SystemExit('build-cache: less than 3 GiB free; build stopped before artifacts or service replacement can fail')


if __name__ == '__main__':
    main()
