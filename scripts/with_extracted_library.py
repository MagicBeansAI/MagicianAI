#!/usr/bin/env python3
"""Run an explicit library lane at the same Git revision Magician consumes.

Preparation may fetch source, but never runs Cargo, hooks, checks, or tests.
Verification is requested only by an explicit lane (not by `prepare`). Existing
checkouts are never reset or overwritten. Build output stays below the caller's
Makefile-exported CARGO_TARGET_DIR, isolated per upstream library.
"""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
LIBRARIES = {
    "magicrun": ("tool-runtime-core", "https://github.com/MagicBeansAI/MagicRun.git"),
    "magicvault": ("magicvault-core", "https://github.com/MagicBeansAI/MagicVault.git"),
}
LANES = {
    "magicrun": {"prepare", "check", "test", "test-lifecycle", "inventory", "classification", "replay"},
    "magicvault": {"prepare", "check", "test", "test-compatibility", "test-foundation"},
}


def git(directory: Path, *args: str) -> str:
    return subprocess.check_output(
        ["git", "-c", "core.hooksPath=/dev/null", "-C", str(directory), *args],
        text=True,
    ).strip()


def revision_for(library: str) -> tuple[str, str]:
    dependency, expected_url = LIBRARIES[library]
    # The root pins are intentionally literal single-line inline tables. Fail
    # closed on a format change instead of accidentally falling back to a branch.
    manifest = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    section = manifest.split("[workspace.dependencies]", 1)[1].split("\n[", 1)[0]
    rows = re.findall(r"(?m)^" + re.escape(dependency) + r"\s*=\s*\{([^\n]+)\}\s*$", section)
    if len(rows) != 1:
        raise ValueError(f"expected one explicit workspace pin for {dependency}")
    urls = re.findall(r'\bgit\s*=\s*"([^"]+)"', rows[0])
    revisions = re.findall(r'\brev\s*=\s*"([0-9a-f]{40})"', rows[0])
    if urls != [expected_url] or len(revisions) != 1:
        raise ValueError(f"{dependency} must use its reviewed repository and a full commit ID")
    return expected_url, revisions[0]


def checkout_location(library: str) -> tuple[str, str, Path]:
    url, revision = revision_for(library)
    checkout = ROOT / ".extracted" / "qualification" / library / revision
    return url, revision, checkout


def prepared_checkout(library: str) -> Path:
    """Validate cached source without fetching, modifying it, or running a lane."""
    url, revision, checkout = checkout_location(library)
    if not checkout.is_dir():
        raise ValueError(f"missing pinned {library} source; run make setup-extracted-libraries")
    if git(checkout, "remote", "get-url", "origin") != url:
        raise ValueError(f"unexpected repository at {checkout}")
    if git(checkout, "rev-parse", "HEAD") != revision:
        raise ValueError(f"unexpected revision at {checkout}; preserve it and prepare a clean checkout")
    if git(checkout, "status", "--porcelain", "--untracked-files=all"):
        raise ValueError(f"edited qualification checkout at {checkout}; refusing to overwrite it")
    return checkout


def prepare(library: str) -> Path:
    url, revision, checkout = checkout_location(library)
    if not checkout.exists():
        checkout.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(["git", "-c", "core.hooksPath=/dev/null", "clone", "--no-checkout", url, str(checkout)], check=True)
        git(checkout, "checkout", "--detach", revision)
    # An interrupted or edited checkout is an error, not permission to erase it.
    return prepared_checkout(library)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("library", choices=LIBRARIES)
    parser.add_argument("lane")
    parser.add_argument("arguments", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.lane not in LANES[args.library]:
        parser.error("unsupported library lane")
    is_tool = args.library == "magicrun" and args.lane in {"inventory", "classification", "replay"}
    if args.arguments and not is_tool:
        parser.error("only inventory/classification/replay accept extra arguments")
    target_dir = os.environ.get("CARGO_TARGET_DIR")
    if args.lane != "prepare" and not target_dir:
        parser.error("run this lane through the root Makefile (CARGO_TARGET_DIR is required)")
    checkout = prepare(args.library)
    if args.lane == "prepare":
        print(checkout)
        return 0
    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(Path(target_dir).resolve() / "extracted" / args.library)
    if is_tool:
        arguments = args.arguments[1:] if args.arguments[:1] == ["--"] else args.arguments
        # Keep the product working directory for relative inventory/report paths.
        return subprocess.call([
            "cargo", "run", "--manifest-path", str(checkout / "Cargo.toml"),
            "-p", "tool-runtime-core", "--bin", "tool-runtime-" + args.lane,
            "--", *arguments,
        ], cwd=ROOT, env=env)
    return subprocess.call(["make", "-C", str(checkout), args.lane], env=env)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, IndexError, subprocess.CalledProcessError) as error:
        print(f"extracted library lane: {error}", file=sys.stderr)
        sys.exit(1)
