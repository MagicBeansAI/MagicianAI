#!/usr/bin/env python3
"""Build, sign, notarize, staple, and verify Magican Desktop for macOS.

Secrets stay in the login Keychain and the operator's protected signing
directory. Only paths, public metadata, and the updater public key are passed
to the Tauri child process. The finished DMG is submitted separately after
Tauri notarizes and staples the application bundle inside it.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

import apple_signing_transfer as signing


ROOT = Path(__file__).resolve().parents[1]
TAURI_CONFIG_PATH = ROOT / "desktop/src-tauri/tauri.conf.json"
HOST_TARGET = "aarch64-apple-darwin"


def fail(message: str) -> None:
    print(f"ERROR: {message}", file=sys.stderr)
    raise SystemExit(1)


def run_capture(
    argv: list[str],
    *,
    env: dict[str, str] | None = None,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        argv,
        cwd=ROOT,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if check and result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        fail(f"command failed: {argv[0]}{f': {detail}' if detail else ''}")
    return result


def run_live(argv: list[str], *, env: dict[str, str] | None = None) -> None:
    result = subprocess.run(argv, cwd=ROOT, env=env, check=False)
    if result.returncode != 0:
        fail(f"command failed with exit code {result.returncode}: {' '.join(argv)}")


def target_dir() -> Path:
    if os.environ.get("CARGO_TARGET_DIR"):
        return Path(os.environ["CARGO_TARGET_DIR"]).expanduser().resolve()
    result = run_capture(
        ["make", "--no-print-directory", "-s", "print-cargo-target-dir"]
    )
    value = result.stdout.strip()
    if not value:
        fail("Makefile did not resolve CARGO_TARGET_DIR")
    return Path(value).expanduser().resolve()


def single_file(directory: Path, pattern: str, label: str) -> Path:
    matches = sorted(path for path in directory.glob(pattern) if path.is_file())
    if len(matches) != 1:
        fail(f"expected one {label} under {directory}; found {len(matches)}")
    return matches[0]


def single_directory(directory: Path, pattern: str, label: str) -> Path:
    matches = sorted(path for path in directory.glob(pattern) if path.is_dir())
    if len(matches) != 1:
        fail(f"expected one {label} under {directory}; found {len(matches)}")
    return matches[0]


def require_outside_repo(path: Path, label: str) -> None:
    try:
        path.resolve().relative_to(ROOT.resolve())
    except ValueError:
        return
    fail(f"{label} resolved inside the Git checkout: {path}")


def signing_environment(jobs: int) -> tuple[dict[str, str], dict[str, str]]:
    metadata = signing.read_json(signing.METADATA_PATH)
    identity = signing.require_text(metadata, "developer_id_identity")
    team_id = signing.require_text(metadata, "team_id")
    key_id = signing.require_text(metadata, "key_id")
    issuer_id = signing.require_text(metadata, "issuer_id")
    api_key = signing.ASC_ROOT / f"AuthKey_{key_id}.p8"
    updater_private = signing.TAURI_ROOT / "magician.key"
    updater_public = signing.TAURI_ROOT / "magician.key.pub"
    for path, label in (
        (api_key, "App Store Connect private key"),
        (updater_private, "Tauri updater private key"),
        (updater_public, "Tauri updater public key"),
    ):
        if not path.is_file():
            fail(f"{label} is missing: {path}")
        require_outside_repo(path, label)

    identities = signing.signing_identities()
    if identity not in identities:
        fail(f"Developer ID identity is unavailable in the login Keychain: {identity}")

    updater_password = signing.keychain_password(
        signing.KEYCHAIN_SERVICES["tauri_updater_password"]
    )
    updater_public_value = updater_public.read_text(encoding="utf-8").strip()
    if not updater_public_value:
        fail("Tauri updater public key is empty")
    tauri_config = signing.read_json(TAURI_CONFIG_PATH)
    configured_updater_key = (
        tauri_config.get("plugins", {}).get("updater", {}).get("pubkey", "")
    )
    if configured_updater_key.strip() != updater_public_value:
        fail(
            "desktop/src-tauri/tauri.conf.json does not contain the public key "
            "matching the protected Tauri updater keypair"
        )

    environment = os.environ.copy()
    environment.update(
        {
            "APPLE_SIGNING_IDENTITY": identity,
            "APPLE_API_ISSUER": issuer_id,
            "APPLE_API_KEY": key_id,
            "APPLE_API_KEY_PATH": str(api_key),
            "TAURI_SIGNING_PRIVATE_KEY": str(updater_private),
            "TAURI_SIGNING_PRIVATE_KEY_PASSWORD": updater_password,
            "TAURI_CONFIG": json.dumps(
                {"plugins": {"updater": {"pubkey": updater_public_value}}},
                separators=(",", ":"),
            ),
            "MAGICIAN_SIGN": "1",
            "MAGICIAN_SIGN_IDENTITY": identity,
            "MAGICIAN_PACKAGE_EXPECTED_SIGNING": "developer-id",
            "MAGICIAN_PACKAGE_TARGET": HOST_TARGET,
            "MAGICIAN_PACKAGE_EXPECTED_COMMIT": run_capture(
                ["git", "rev-parse", "HEAD"]
            ).stdout.strip(),
            "CARGO_BUILD_JOBS": str(jobs),
            "RUSTC_JOB_GATE_SLOTS": str(jobs),
        }
    )
    public = {
        "identity": identity,
        "team_id": team_id,
        "key_id": key_id,
        "issuer_id": issuer_id,
        "api_key": str(api_key),
    }
    return environment, public


def validate_app(app: Path, identity: str, team_id: str) -> None:
    run_capture(["codesign", "--verify", "--deep", "--strict", "--verbose=4", str(app)])
    details = run_capture(
        ["codesign", "--display", "--verbose=4", str(app)], check=False
    )
    rendered = details.stdout + details.stderr
    if f"Authority={identity}" not in rendered:
        fail("finished app is not signed by the configured Developer ID identity")
    if f"TeamIdentifier={team_id}" not in rendered:
        fail("finished app has the wrong Apple team identifier")
    if "Runtime Version=" not in rendered:
        fail("finished app is missing hardened-runtime signature metadata")
    run_capture(["xcrun", "stapler", "validate", str(app)])
    gatekeeper = run_capture(
        ["spctl", "--assess", "--type", "execute", "--verbose=4", str(app)],
        check=False,
    )
    if gatekeeper.returncode != 0:
        fail(
            "Gatekeeper rejected the application bundle: "
            + (gatekeeper.stderr.strip() or gatekeeper.stdout.strip())
        )


def notarize_dmg(dmg: Path, public: dict[str, str], receipt: Path) -> str:
    identity = public["identity"]
    run_capture(
        [
            "codesign",
            "--force",
            "--timestamp",
            "--sign",
            identity,
            str(dmg),
        ]
    )
    run_capture(["codesign", "--verify", "--verbose=4", str(dmg)])
    result = run_capture(
        [
            "xcrun",
            "notarytool",
            "submit",
            str(dmg),
            "--key",
            public["api_key"],
            "--key-id",
            public["key_id"],
            "--issuer",
            public["issuer_id"],
            "--wait",
            "--output-format",
            "json",
        ],
        check=False,
    )
    try:
        response = json.loads(result.stdout)
    except json.JSONDecodeError:
        response = {}
    receipt.parent.mkdir(parents=True, exist_ok=True)
    receipt.write_text(
        json.dumps(response, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    receipt.chmod(0o600)
    submission_id = str(response.get("id") or "")
    if result.returncode != 0 or response.get("status") != "Accepted":
        if submission_id:
            log_result = run_capture(
                [
                    "xcrun",
                    "notarytool",
                    "log",
                    submission_id,
                    "--key",
                    public["api_key"],
                    "--key-id",
                    public["key_id"],
                    "--issuer",
                    public["issuer_id"],
                ],
                check=False,
            )
            (receipt.parent / f"{submission_id}.log.json").write_text(
                log_result.stdout, encoding="utf-8"
            )
        detail = response.get("message") or result.stderr.strip() or "unknown rejection"
        fail(f"Apple did not accept the DMG notarization submission: {detail}")

    run_capture(["xcrun", "stapler", "staple", "-v", str(dmg)])
    run_capture(["xcrun", "stapler", "validate", str(dmg)])
    run_capture(["codesign", "--verify", "--verbose=4", str(dmg)])
    gatekeeper = run_capture(
        [
            "spctl",
            "--assess",
            "--type",
            "open",
            "--context",
            "context:primary-signature",
            "--verbose=4",
            str(dmg),
        ],
        check=False,
    )
    if gatekeeper.returncode != 0:
        fail(
            "Gatekeeper rejected the disk image: "
            + (gatekeeper.stderr.strip() or gatekeeper.stdout.strip())
        )
    return submission_id


def write_checksum(path: Path) -> Path:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    checksum = path.with_name(path.name + ".sha256")
    checksum.write_text(f"{digest.hexdigest()}  {path.name}\n", encoding="utf-8")
    return checksum


def ensure_no_sensitive_git_files() -> None:
    tracked = run_capture(["git", "ls-files"]).stdout.splitlines()
    forbidden = (
        ".p8",
        ".p12",
        ".pfx",
        ".mobileprovision",
        ".provisionprofile",
        ".magican-signing.enc",
    )
    leaked = [path for path in tracked if path.lower().endswith(forbidden)]
    if leaked:
        fail("sensitive signing files are tracked by Git: " + ", ".join(leaked))


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Build and qualify a signed/notarized Magican Desktop release."
    )
    parser.add_argument("--jobs", type=int, default=2)
    parser.add_argument(
        "--keep-bundle-output",
        action="store_true",
        help="do not remove previous macOS bundle outputs before building",
    )
    args = parser.parse_args()
    if args.jobs < 1 or args.jobs > 4:
        fail("--jobs must be between 1 and 4")
    if sys.platform != "darwin":
        fail("signed Apple Desktop releases must be built on macOS")

    ensure_no_sensitive_git_files()
    run_live([sys.executable, str(ROOT / "scripts/apple_signing_transfer.py"), "status", "--network"])
    environment, public = signing_environment(args.jobs)
    config = signing.read_json(TAURI_CONFIG_PATH)
    version = signing.require_text(config, "version")
    environment["RELEASE_VERSION"] = version
    build_dir = target_dir()
    environment["CARGO_TARGET_DIR"] = str(build_dir)
    environment["MAGICIAN_PACKAGE_RELEASE_DIR"] = str(build_dir / "release")

    bundle_root = build_dir / "release/bundle"
    if not args.keep_bundle_output:
        for generated in (bundle_root / "macos", bundle_root / "dmg"):
            if generated.exists():
                shutil.rmtree(generated)

    print(f"Building signed Magican Desktop {version} with {args.jobs} compile jobs.")
    print(f"Release artifacts will remain outside Git under: {bundle_root}")
    run_live(
        [
            "make",
            "--no-print-directory",
            "release-desktop-native",
            f"CARGO_BUILD_JOBS={args.jobs}",
            f"RUSTC_JOB_GATE_SLOTS={args.jobs}",
        ],
        env=environment,
    )

    app = single_directory(bundle_root / "macos", "*.app", "macOS application")
    dmg = single_file(bundle_root / "dmg", "*.dmg", "macOS disk image")
    updater = single_file(bundle_root / "macos", "*.app.tar.gz", "updater archive")
    updater_signature = Path(str(updater) + ".sig")
    if not updater_signature.is_file() or updater_signature.stat().st_size == 0:
        fail("Tauri updater signature is missing or empty")

    validate_app(app, public["identity"], public["team_id"])
    receipt = build_dir / "release/notarization" / f"{dmg.name}.json"
    submission_id = notarize_dmg(dmg, public, receipt)
    checksum = write_checksum(dmg)
    ensure_no_sensitive_git_files()

    print("Signed and notarized Desktop release is ready.")
    print(f"App: {app}")
    print(f"DMG: {dmg}")
    print(f"DMG checksum: {checksum}")
    print(f"Updater: {updater}")
    print(f"Updater signature: {updater_signature}")
    print(f"Apple notarization submission: {submission_id}")
    print(f"Notarization receipt: {receipt}")


if __name__ == "__main__":
    main()
