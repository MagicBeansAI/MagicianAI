#!/usr/bin/env python3
"""Stage relocatable skill sources, exact native binaries and Python locks.

The release is the pin: no source builds or floating dependency resolution on
the installation machine. Missing or stale pinned artifacts fail packaging.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import re


ROOT = Path(__file__).resolve().parents[1]


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def native_target(path: Path) -> str:
    with path.open("rb") as handle:
        header = handle.read(64)
    if header[:4] == b"\xcf\xfa\xed\xfe":
        cpu = struct.unpack_from("<I", header, 4)[0]
        arch = {0x100000c: "aarch64", 0x1000007: "x86_64"}.get(cpu)
        return f"{arch}-apple-darwin"
    if header[:4] == b"\x7fELF":
        arch = {183: "aarch64", 62: "x86_64"}.get(struct.unpack_from("<H", header, 18)[0])
        return f"{arch}-unknown-linux-gnu"
    raise ValueError(f"not a supported native tool artifact: {path}")


def verify(root: Path):
    manifest = json.loads((root / "release-tools.json").read_text())
    if manifest["schema_version"] != 1 or not manifest["files"]:
        raise ValueError("unsupported or empty release tool manifest")
    for entry in manifest["files"]:
        path = Path(entry["path"])
        if path.is_absolute() or ".." in path.parts:
            raise ValueError("invalid release tool path")
        candidate = root / path
        if not candidate.is_file() or digest(candidate) != entry["sha256"]:
            raise ValueError(f"release tool checksum mismatch: {path}")
        if entry.get("executable") and not os.access(candidate, os.X_OK):
            raise ValueError(f"release tool is not executable: {path}")
    print(f"✓ {len(manifest['files'])} pinned release runtime files verified")


def seal_signed(root: Path):
    """Update only native artifact hashes after the packager signs them.

    Non-executable pins must still match before sealing, so signing cannot
    hide dependency-lock drift.
    """
    manifest = json.loads((root / "release-tools.json").read_text())
    for entry in manifest["files"]:
        candidate = root / entry["path"]
        if not entry.get("executable") and digest(candidate) != entry["sha256"]:
            raise ValueError(f"non-executable release input changed during signing: {entry['path']}")
        if entry.get("executable") and native_target(candidate) != manifest["target"]:
            raise ValueError("signed native artifact target changed")
    for entry in manifest["files"]:
        entry["sha256"] = digest(root / entry["path"])
    (root / "release-tools.json").write_text(json.dumps(manifest, indent=2) + "\n")
    verify(root)


def stage(source: Path, dest: Path, target: str, document_binary: Path):
    sources = subprocess.check_output(["git", "-C", str(source.parent), "ls-files", "-z", "--", "skillshub/"], text=True)
    paths = set(sources.split("\0")) - {""}
    # Newly added runtime infrastructure may be untracked while a developer
    # validates a candidate release. Enumerate it explicitly, never collect
    # arbitrary untracked files (which can include credentials).
    paths.update("skillshub/" + path for path in (
        "scripts/verify_runtime.py", "scripts/link_runtime.py", "scripts/migrate_metabase_mcp.py",
        "scripts/pinned_bin_store.py", "scripts/stage_release.py", "scripts/shared_skill_bins.py"))
    for relative in sorted(paths):
        if not relative:
            continue
        path = Path(relative).relative_to("skillshub")
        # Source lists can contain tracked examples, never live operator data.
        if path.name in ("operator-config.yaml", "client_secret.json", "credentials.json"):
            continue
        if path.name.startswith(".env") and not path.name.endswith((".example", ".template")):
            continue
        origin = source / path
        if not origin.is_file():
            raise ValueError(f"tracked runtime source missing: {origin}")
        output = dest / path
        output.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(origin, output)

    files = []

    def copy(origin: Path, relative: str, *, native: bool = False):
        if not origin.is_file():
            raise ValueError(f"release tool missing: {origin}; build release tools before packaging")
        if native and native_target(origin) != target:
            raise ValueError(f"{origin}: artifact target {native_target(origin)} differs from release {target}")
        output = dest / relative
        output.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(origin, output)
        files.append({"path": relative, "sha256": digest(output), "executable": native})

    copy(document_binary, "document-to-markdown/bin/document-to-markdown", native=True)
    copy(source / "browser/bin/agent-browser", "browser/bin/agent-browser", native=True)
    # The existing installer uses this reviewed fork instead of compiling it.
    browser_version = re.search(r"^AGENT_BROWSER_VERSION \?= (\S+)$", (source / "Makefile").read_text(), re.M).group(1)
    vendor_path = source / f"browser/_vendor/v{browser_version}/bin/agent-browser-darwin-arm64"
    copy(vendor_path, str(vendor_path.relative_to(source)), native=True)
    for bot in ("sdk", "agentmail", "gmail", "kapso", "telegram", "telegram-self", "whatsapp"):
        copy(source / "bots" / bot / "dist/index.js", f"bots/{bot}/dist/index.js")
    # Freeze the environments that were actually built and verified for this
    # release. End-user uv installs consume these exact transitive versions.
    for venv, relative in ((".venv", ".release/python-requirements.lock"),
                           ("marimo/.venv", ".release/marimo-requirements.lock")):
        frozen = subprocess.check_output(["uv", "--no-cache", "pip", "freeze", "--python", str(source / venv / "bin/python")], text=True)
        if not frozen.strip() or any(line.startswith("-e ") or "file://" in line for line in frozen.splitlines()):
            raise ValueError(f"nonportable release Python environment: {venv}")
        output = dest / relative
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(frozen)
        files.append({"path": relative, "sha256": digest(output), "executable": False})
    # The npm lock is also a release pin, consumed with npm ci.
    files.append({"path": "package-lock.json", "sha256": digest(dest / "package-lock.json"), "executable": False})
    manifest = {"schema_version": 1, "target": target, "files": files}
    (dest / "release-tools.json").write_text(json.dumps(manifest, indent=2) + "\n")
    verify(dest)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=ROOT)
    parser.add_argument("--dest", type=Path)
    parser.add_argument("--target")
    parser.add_argument("--document-binary", type=Path)
    parser.add_argument("--verify", type=Path)
    parser.add_argument("--seal-signed", type=Path)
    args = parser.parse_args()
    try:
        if args.seal_signed:
            seal_signed(args.seal_signed)
        elif args.verify:
            verify(args.verify)
        elif args.dest and args.target and args.document_binary:
            stage(args.source, args.dest, args.target, args.document_binary)
        else:
            parser.error("provide --verify or --dest, --target and --document-binary")
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        parser.exit(1, f"✗ {error}\n")


if __name__ == "__main__":
    main()
