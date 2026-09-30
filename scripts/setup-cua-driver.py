#!/usr/bin/env python3
"""Install/verify CuaDriver on the desktop that owns the graphical session.

Windows: py -3 scripts/setup-cua-driver.py [--start | --check]
Linux/macOS: make setup-cua-driver ARGS=--start
Headless backend: make check-cua-driver ARGS=--relay
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import runpy
import shutil
import ssl
import subprocess
import sys
import tempfile
import urllib.request

CONTROLLER = Path(__file__).resolve().parents[1] / "skillshub/macos-ui-automation/bin/macos-ui-controller"

# The one CuaDriver release this checkout is written and tested against. The
# same pin (version, URLs, hashes) is the `cua-driver` entry of
# magician-components/src/setup_catalog.yaml, which Magican Desktop installs
# from; scripts/test-cua-setup.py fails when the two differ. The
# skill's action enum (`scripts/sync_desktop_action_enum.py`) and the
# controller's parsing of driver output follow this version; bump them together.
# 0.28.3 is skipped: its macOS CuaDriver.app shipped unsigned and the official
# installer refuses it. Bump only to a release whose app passes codesign.
CUA_DRIVER_VERSION = "0.28.2"
_TAG = f"cua-driver-rs-v{CUA_DRIVER_VERSION}"
_RELEASE = f"https://github.com/trycua/cua/releases/download/{_TAG}/"
_SOURCE = f"https://raw.githubusercontent.com/trycua/cua/{_TAG}/libs/cua-driver/scripts/"
# Every script the official installer executes, fetched from the pinned tag and
# checked by hash. The helpers sit beside the entry script, so the installer
# uses them from disk instead of fetching the moving copies on cua.ai.
INSTALLER_FILES = {
    "unix": (
        ("install.sh", _RELEASE, "317ba3a49fdba10f2a7f1b9f392c1bc1b7657f3aae85e1e2e43684cf17a1bf3b"),
        ("_install-rust.sh", _RELEASE, "c3b4423dd4290f65f03579013f2c350e5a08b03458c9ae34ac4904865ec3f833"),
        ("_install-common.sh", _SOURCE, "5bc3aa010eb8667a099b582a9ada9a8f93001745b842cc7cf3cc6c472520cf29"),
    ),
    "win32": (
        ("install.ps1", _RELEASE, "3e770fa8c351b80db99ae6b080f696a22f844534498bf44d45816cbd05eb0c3f"),
        ("_install-common.psm1", _SOURCE, "324bca98ad19f0487d4afd36a9e2d06478fcfb8e1e20225cdd8ec8ef5150e720"),
    ),
}
# A script block has no $PSScriptRoot, so install.ps1 would fetch its module
# from cua.ai; point it at the verified copy instead.
_PS_MODULE_ANCHOR = "-LocalDir $PSScriptRoot"


def installer_command(platform: str, path: Path) -> list[str]:
    if platform == "win32":
        shell = shutil.which("powershell.exe") or shutil.which("pwsh.exe")
        if not shell:
            raise RuntimeError("PowerShell is required for the official Windows installer")
        # Match the official script-block install without changing the machine's
        # execution policy. The temporary path is data, escaped as a PS literal.
        literal = str(path).replace("'", "''")
        return [shell, "-NoProfile", "-Command", f"& ([scriptblock]::Create([IO.File]::ReadAllText('{literal}')))"]
    if platform in ("darwin", "linux"):
        shell = shutil.which("bash")
        if not shell:
            raise RuntimeError("bash is required for the official Unix installer")
        return [shell, str(path)]
    raise RuntimeError(f"Unsupported CuaDriver platform: {platform}")


def tls_context() -> ssl.SSLContext:
    context = ssl.create_default_context()
    # A python.org macOS Python ships an empty CA store until its "Install
    # Certificates" step runs; trust the OS bundle rather than fail every fetch.
    if not context.get_ca_certs() and os.path.isfile("/etc/ssl/cert.pem"):
        context.load_verify_locations("/etc/ssl/cert.pem")
    return context


def fetch_verified(url: str, sha256: str) -> bytes:
    # Download completely before executing; never execute a partial stream.
    with urllib.request.urlopen(url, timeout=30, context=tls_context()) as response:
        content = response.read(1024 * 1024 + 1)
    if len(content) > 1024 * 1024 or not content:
        raise RuntimeError(f"Unexpected official CuaDriver installer size: {url}")
    if hashlib.sha256(content).hexdigest() != sha256:
        raise RuntimeError(f"CuaDriver installer does not match the pinned {CUA_DRIVER_VERSION} hash: {url}")
    return content


def install(platform: str) -> None:
    files = INSTALLER_FILES["win32" if platform == "win32" else "unix"]
    with tempfile.TemporaryDirectory(prefix="magician-cua-setup-") as folder:
        for name, base, sha256 in files:
            (Path(folder) / name).write_bytes(fetch_verified(base + name, sha256))
        entry = Path(folder) / files[0][0]
        if platform == "win32":
            script = entry.read_text(encoding="utf-8-sig")
            if script.count(_PS_MODULE_ANCHOR) != 1:
                raise RuntimeError("Pinned install.ps1 no longer loads its module from $PSScriptRoot")
            literal = folder.replace("'", "''")
            entry.write_text(script.replace(_PS_MODULE_ANCHOR, f"-LocalDir '{literal}'"), encoding="utf-8")
        # The installer's own exact pin: without it, it resolves the latest release.
        env = {**os.environ, "CUA_DRIVER_RS_VERSION": CUA_DRIVER_VERSION}
        env.pop("CUA_DRIVER_VERSION", None)
        subprocess.run(installer_command(platform, entry), check=True, env=env)


def installed_version(binary: str) -> str | None:
    try:
        result = subprocess.run([binary, "--version"], capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.SubprocessError):
        return None
    match = re.search(r"\b\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?\b", result.stdout)
    return match.group(0) if result.returncode == 0 and match else None


def check_local(controller: dict) -> int:
    binary = controller["driver_binary"]()
    if not binary:
        print("CUA unavailable: driver is not installed on this desktop.", file=sys.stderr)
        return 1
    version = installed_version(binary)
    if version != CUA_DRIVER_VERSION:
        print(f"CuaDriver {version or 'of unknown version'} is installed; this checkout pins {CUA_DRIVER_VERSION}. Run `make setup-cua-driver ARGS=--start`.", file=sys.stderr)
        return 1
    # Doctor output includes platform-specific prerequisites. Warnings can
    # exit zero, so it is diagnostic evidence, never our readiness verdict.
    doctor = subprocess.run([binary, "doctor"], timeout=30, check=False)
    status = controller["daemon_status"]()
    if status.returncode:
        print("CUA is installed but not ready. Start it in the graphical desktop session; review doctor output.", file=sys.stderr)
        return 1
    if doctor.returncode:
        # Older macOS releases run doctor in the CLI process, so its TCC
        # attribution can differ from the app-owned daemon we actually use.
        print("Doctor reported issues; checking the running daemon's window observation separately.", file=sys.stderr)
    result = subprocess.run([binary, "call", "list_windows", "{}"], capture_output=True, text=True, timeout=30)
    try:
        body = json.loads(result.stdout)
        if isinstance(body, dict) and isinstance(body.get("structuredContent"), dict):
            body = body["structuredContent"]
        windows = body if isinstance(body, list) else body.get("windows")
    except (ValueError, AttributeError):
        windows = None
    # An empty service-session result must not masquerade as a verified desktop.
    if result.returncode or not isinstance(windows, list) or not windows:
        print("CUA daemon is reachable but no desktop windows were verified. Open a window in the target session and check accessibility access.", file=sys.stderr)
        return 1
    print(f"CUA desktop observation verified (CuaDriver {version}; {sys.platform}; {len(windows)} windows). No input actions performed.")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="Read-only: no installation or daemon startup")
    parser.add_argument("--start", action="store_true", help="Start the daemon in this graphical session, then verify")
    parser.add_argument("--upgrade", action="store_true", help=f"Reinstall the pinned {CUA_DRIVER_VERSION} release even when it is already installed")
    parser.add_argument("--relay", action="store_true", help="Verify the configured desktop relay instead of installing locally")
    args = parser.parse_args(argv)
    if args.check and (args.start or args.upgrade):
        parser.error("--check cannot start or upgrade a driver")
    if args.relay and (args.start or args.upgrade):
        parser.error("--relay cannot start or upgrade a local driver")
    controller = runpy.run_path(str(CONTROLLER))
    try:
        if args.relay:
            # Status is read-only; unlike list_apps, the gateway won't auto-start
            # a daemon. Check both wrapper and nested daemon result.
            base = controller["host_gateway_url"]().rstrip("/")
            request = urllib.request.Request(base + "/host/ax/status", data=b"{}", headers={"Content-Type": "application/json"})
            with urllib.request.urlopen(request, timeout=10) as response:
                body = json.loads(controller["read_bounded_response"](response))
            if not isinstance(body, dict):
                raise ValueError("Malformed desktop relay status response")
            status = json.loads(body.get("stdout") or "{}")
            if not isinstance(status, dict):
                raise ValueError("Malformed CUA daemon status response")
            ready = body.get("ok") is True and status.get("running") is True
            print("CUA desktop relay daemon " + ("reachable" if ready else "not ready"))
            return 0 if ready else 1
        if args.check:
            return check_local(controller)
        if sys.platform not in ("win32", "linux", "darwin"):
            raise RuntimeError(f"Unsupported CuaDriver platform: {sys.platform}")
        desktop_session = controller["has_desktop_session"]()
        if args.start and not desktop_session:
            raise RuntimeError("Start CuaDriver inside the signed-in Windows/Linux desktop. Headless backends use --check --relay. Omit --start to install the binary only.")
        binary = controller["driver_binary"]()
        # A different version in either direction is replaced: the pin is exact.
        if args.upgrade or not binary or installed_version(binary) != CUA_DRIVER_VERSION:
            install(sys.platform)
        binary = controller["driver_binary"]()
        if not binary:
            raise RuntimeError("Installer finished but cua-driver was not found; set MAGICIAN_CUA_DRIVER_BIN to its absolute path")
        version = installed_version(binary)
        if version != CUA_DRIVER_VERSION:
            raise RuntimeError(f"{binary} reports {version or 'no version'} after install; expected {CUA_DRIVER_VERSION}")
        print(f"CuaDriver {version} installed: {binary}")
        if not desktop_session:
            print("No graphical desktop session: binary installed only, CUA is not ready. Start it after desktop login, or use --check --relay for a remote desktop.")
            return 0
        if args.start:
            if not controller["ensure_daemon"]():
                return 1
            return check_local(controller)
        print("Use --start to start and verify, or --check for read-only verification.")
        if sys.platform == "darwin":
            # LaunchServices launch: macOS attributes the grants to CuaDriver.app, not this terminal.
            print("Run `cua-driver permissions grant` to grant CuaDriver.app Accessibility, Screen Recording and direct capture, and verify a live capture.")
        elif sys.platform == "linux":
            print("Run in the target X11/Wayland session with its display and AT-SPI accessibility bus.")
        else:
            print("Run as the signed-in desktop user; Windows service/SSH Session 0 cannot control that desktop.")
        return 0
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        print(f"CUA setup: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
