#!/usr/bin/env python3
"""Read-only host discovery for the repository's portable build-cache defaults."""
import hashlib
import os
from pathlib import Path
import platform
import subprocess
import sys

VERSION = "1.23.0"


def storage_root():
    ssd = Path("/Volumes/build")
    if ssd.is_mount() and os.access(ssd, os.W_OK):
        return ssd / "magician"
    if os.environ.get("XDG_CACHE_HOME"):
        return Path(os.environ["XDG_CACHE_HOME"]) / "magician-build"
    if platform.system() == "Darwin":
        return Path.home() / "Library/Caches/magician-build"
    return Path.home() / ".cache/magician-build"


def release_host():
    machine = {"arm64": "aarch64", "aarch64": "aarch64", "x86_64": "x86_64"}.get(platform.machine())
    system = platform.system()
    suffix = {"Darwin": "apple-darwin", "Linux": "unknown-linux-gnu"}.get(system)
    if system == "Linux" and platform.libc_ver()[0] != "glibc":
        return None
    if not machine or not suffix or (suffix == "apple-darwin" and machine != "aarch64"):
        return None
    return f"{machine}-{suffix}"


def required_volume(root):
    return next((str(parent) for parent in [root, *root.parents]
                 if parent != Path("/") and parent.is_mount()), "")


def memory_budget():
    try:
        if platform.system() == "Darwin":
            physical = int(subprocess.check_output(["/usr/sbin/sysctl", "-n", "hw.memsize"], text=True, stderr=subprocess.DEVNULL))
        else:
            physical = os.sysconf("SC_PHYS_PAGES") * os.sysconf("SC_PAGE_SIZE")
        return f"{max(1, min(24, int(physical * 0.75 / 1024**3)))}GiB"
    except (OSError, ValueError, subprocess.CalledProcessError):
        return ""  # mbx derives its own physical-memory budget.


if __name__ == "__main__":
    field = sys.argv[1]
    if field == "root":
        print(storage_root())
    elif field == "volume":
        print(required_volume(Path(sys.argv[2]).expanduser().absolute()))
    elif field == "cpus":
        print(min(4, os.cpu_count() or 1))
    elif field == "memory":
        print(memory_budget())
    elif field == "supported":
        print("1" if release_host() else "0")
    elif field == "version":
        print(VERSION)
    elif field == "workspace":
        print(hashlib.sha256(os.path.realpath(sys.argv[2]).encode()).hexdigest()[:16])
    else:
        raise SystemExit(f"Unknown build-cache default: {field}")
