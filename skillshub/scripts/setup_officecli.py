#!/usr/bin/env python3
"""Install the pinned OfficeCLI release used by Magician tool skills."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path


SKILLSHUB_ROOT = Path(__file__).resolve().parents[1]
RUNTIME_ROOT = SKILLSHUB_ROOT / "officecli-runtime"
RELEASE_FILE = RUNTIME_ROOT / "release.json"
OFFICE_SKILLS = ("office-word", "office-excel", "office-powerpoint")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def is_musl_linux() -> bool:
    if Path("/etc/alpine-release").exists():
        return True
    ldd = shutil.which("ldd")
    if not ldd:
        return False
    result = subprocess.run(
        [ldd, "--version"],
        check=False,
        capture_output=True,
        text=True,
    )
    return "musl" in f"{result.stdout}\n{result.stderr}".lower()


def platform_key() -> str:
    system = platform.system().lower()
    machine = platform.machine().lower()
    if machine in {"aarch64", "arm64"}:
        machine = "arm64"
    elif machine in {"amd64", "x86_64"}:
        machine = "x86_64"
    if system == "linux" and is_musl_linux():
        system = "linux-musl"
    return f"{system}-{machine}"


def load_release() -> dict[str, object]:
    with RELEASE_FILE.open(encoding="utf-8") as handle:
        return json.load(handle)


def download(url: str, destination: Path) -> None:
    curl = shutil.which("curl")
    if not curl:
        raise RuntimeError("curl is required to download OfficeCLI")
    result = subprocess.run(
        [
            curl,
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--max-time",
            "300",
            "--user-agent",
            "Magician-OfficeCLI-Setup/1",
            "--output",
            str(destination),
            url,
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or f"curl exited with {result.returncode}"
        raise RuntimeError(f"downloading OfficeCLI: {detail}")


def link_skill_bins(binary: Path) -> None:
    for skill_name in OFFICE_SKILLS:
        bin_dir = SKILLSHUB_ROOT / skill_name / "bin"
        bin_dir.mkdir(parents=True, exist_ok=True)
        link = bin_dir / "officecli"
        relative_target = Path("..") / ".." / "officecli-runtime" / "bin" / "officecli"
        if link.is_symlink() and os.readlink(link) == str(relative_target):
            continue
        if link.exists() or link.is_symlink():
            link.unlink()
        link.symlink_to(relative_target)
    if not binary.is_file():
        raise RuntimeError(f"OfficeCLI binary missing after setup: {binary}")


def verify_binary(binary: Path, expected_sha256: str, version: str) -> None:
    actual = sha256(binary)
    if actual != expected_sha256:
        raise RuntimeError(
            f"OfficeCLI checksum mismatch: expected {expected_sha256}, got {actual}"
        )
    env = os.environ.copy()
    env["OFFICECLI_SKIP_UPDATE"] = "1"
    env["OFFICECLI_NO_AUTO_RESIDENT"] = "1"
    result = subprocess.run(
        [str(binary), "--version"],
        check=False,
        capture_output=True,
        text=True,
        env=env,
        timeout=30,
    )
    version_output = f"{result.stdout}\n{result.stderr}".strip()
    if result.returncode != 0 or version not in version_output:
        raise RuntimeError(
            f"OfficeCLI version check failed for {version}: {version_output or 'no output'}"
        )


def install(force: bool) -> Path:
    release = load_release()
    version = str(release["version"])
    assets = release["assets"]
    assert isinstance(assets, dict)
    key = platform_key()
    asset = assets.get(key)
    if not isinstance(asset, dict):
        supported = ", ".join(sorted(assets))
        raise RuntimeError(f"Unsupported OfficeCLI platform {key}; supported: {supported}")

    asset_name = str(asset["name"])
    expected_sha256 = str(asset["sha256"])
    repository = str(release["repository"])
    binary = RUNTIME_ROOT / "bin" / "officecli"
    binary.parent.mkdir(parents=True, exist_ok=True)

    if not force and binary.is_file() and sha256(binary) == expected_sha256:
        verify_binary(binary, expected_sha256, version)
        link_skill_bins(binary)
        print(f"OfficeCLI v{version} already verified at {binary}")
        return binary

    url = f"https://github.com/{repository}/releases/download/v{version}/{asset_name}"
    print(f"Downloading OfficeCLI v{version} ({asset_name})...")
    with tempfile.NamedTemporaryFile(
        prefix="officecli-", dir=binary.parent, delete=False
    ) as handle:
        temporary = Path(handle.name)
    try:
        download(url, temporary)
        actual = sha256(temporary)
        if actual != expected_sha256:
            raise RuntimeError(
                f"OfficeCLI checksum mismatch: expected {expected_sha256}, got {actual}"
            )
        temporary.chmod(
            temporary.stat().st_mode
            | stat.S_IXUSR
            | stat.S_IXGRP
            | stat.S_IXOTH
        )
        if platform.system() == "Darwin" and shutil.which("xattr"):
            subprocess.run(
                ["xattr", "-d", "com.apple.quarantine", str(temporary)],
                check=False,
                capture_output=True,
            )
        verify_binary(temporary, expected_sha256, version)
        os.replace(temporary, binary)
    finally:
        temporary.unlink(missing_ok=True)

    verify_binary(binary, expected_sha256, version)
    link_skill_bins(binary)
    print(f"Installed and verified OfficeCLI v{version} at {binary}")
    return binary


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--force", action="store_true", help="redownload the pinned binary")
    parser.add_argument(
        "--verify-only",
        action="store_true",
        help="verify an existing binary without downloading",
    )
    args = parser.parse_args()
    try:
        release = load_release()
        asset = release["assets"][platform_key()]
        binary = RUNTIME_ROOT / "bin" / "officecli"
        if args.verify_only:
            if not binary.is_file():
                raise RuntimeError(f"OfficeCLI is not installed at {binary}")
            verify_binary(binary, str(asset["sha256"]), str(release["version"]))
            link_skill_bins(binary)
            print(f"Verified OfficeCLI v{release['version']} at {binary}")
        else:
            install(args.force)
    except (KeyError, OSError, RuntimeError) as error:
        print(f"OfficeCLI setup failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
