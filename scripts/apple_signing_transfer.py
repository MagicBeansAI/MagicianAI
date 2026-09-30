#!/usr/bin/env python3
"""Transfer Magican Apple release signing material without using Git.

The encrypted bundle contains only the credentials that must remain identical
across release Macs: Developer ID and Apple Distribution PKCS#12 archives, the
App Store Connect API private key, and the Tauri updater signing keypair. Xcode
managed development certificates and provisioning profiles are deliberately
not copied; the destination Mac creates/downloads those after the operator
signs in to Xcode.
"""

from __future__ import annotations

import argparse
import base64
import datetime as dt
import getpass
import hashlib
import hmac
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import sys
import tarfile
import tempfile
from typing import NoReturn


REPO_ROOT = Path(__file__).resolve().parents[1]
APPLE_ROOT = (
    Path.home() / "Library/Application Support/Magican/release-signing/apple"
)
ASC_ROOT = APPLE_ROOT / "app-store-connect"
CERT_ROOT = APPLE_ROOT / "certificates"
TAURI_ROOT = Path.home() / ".tauri"
METADATA_PATH = ASC_ROOT / "metadata.json"
SIGNING_XCCONFIG = REPO_ROOT / "magios/Signing.local.xcconfig"
ARCHIVE_VERSION = 1
BUNDLE_MAGIC = b"MAGICSIG1\x00"
KDF_ITERATIONS = 300_000

KEYCHAIN_ACCOUNT = "Magican Release"
KEYCHAIN_SERVICES = {
    "developer_id_p12_password": (
        "ai.magicbeans.magician.apple-developer-id-p12-password"
    ),
    "distribution_p12_password": (
        "ai.magicbeans.magician.apple-distribution-p12-password"
    ),
    "tauri_updater_password": "ai.magicbeans.magician.tauri-updater-key-password",
}


def fail(message: str) -> NoReturn:
    print(f"ERROR: {message}", file=sys.stderr)
    raise SystemExit(1)


def run(
    argv: list[str],
    *,
    input_bytes: bytes | None = None,
    check: bool = True,
) -> subprocess.CompletedProcess[bytes]:
    result = subprocess.run(
        argv,
        input=input_bytes,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if check and result.returncode != 0:
        detail = result.stderr.decode("utf-8", errors="replace").strip()
        fail(f"command failed: {argv[0]}{f': {detail}' if detail else ''}")
    return result


def require_macos() -> None:
    if sys.platform != "darwin":
        fail("Apple signing transfer is supported only on macOS")
    for command in ("openssl", "security", "xcrun"):
        if shutil.which(command) is None:
            fail(f"required command is unavailable: {command}")


def resolved(path: Path) -> Path:
    return path.expanduser().resolve(strict=False)


def ensure_outside_repo(path: Path, label: str) -> None:
    candidate = resolved(path)
    root = resolved(REPO_ROOT)
    try:
        candidate.relative_to(root)
    except ValueError:
        return
    fail(f"{label} must be outside the Git checkout: {candidate}")


def read_json(path: Path) -> dict[str, object]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read {path}: {error}")
    if not isinstance(value, dict):
        fail(f"expected a JSON object in {path}")
    return value


def require_text(data: dict[str, object], key: str) -> str:
    value = data.get(key)
    if not isinstance(value, str) or not value.strip():
        fail(f"metadata is missing {key}")
    return value.strip()


def keychain_password(service: str) -> str:
    result = run(
        [
            "security",
            "find-generic-password",
            "-w",
            "-a",
            KEYCHAIN_ACCOUNT,
            "-s",
            service,
        ]
    )
    value = result.stdout.decode("utf-8").rstrip("\r\n")
    if not value:
        fail(f"Keychain item is empty: {service}")
    return value


def load_passphrase(path: Path | None, *, confirm: bool) -> str:
    if path is not None:
        ensure_outside_repo(path, "passphrase file")
        try:
            mode = path.stat().st_mode & 0o777
            if mode & 0o077:
                fail(f"passphrase file must be owner-only (chmod 600): {path}")
            value = path.read_text(encoding="utf-8").rstrip("\r\n")
        except OSError as error:
            fail(f"cannot read passphrase file {path}: {error}")
        if not value:
            fail("passphrase file is empty")
        return value

    first = getpass.getpass("Encrypted signing bundle passphrase: ")
    if len(first) < 16:
        fail("use a passphrase of at least 16 characters")
    if confirm:
        second = getpass.getpass("Confirm signing bundle passphrase: ")
        if first != second:
            fail("passphrases do not match")
    return first


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def copy_private(source: Path, destination: Path) -> None:
    if not source.is_file():
        fail(f"required signing file is missing: {source}")
    destination.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    shutil.copyfile(source, destination)
    destination.chmod(0o600)


def write_json_private(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    path.chmod(0o600)


def archive_sources(metadata: dict[str, object]) -> dict[str, Path]:
    key_id = require_text(metadata, "key_id")
    return {
        "apple/certificates/magican-developer-id.p12": (
            CERT_ROOT / "magican-developer-id.p12"
        ),
        "apple/certificates/magican-apple-distribution.p12": (
            CERT_ROOT / "magican-apple-distribution.p12"
        ),
        f"apple/app-store-connect/AuthKey_{key_id}.p8": (
            ASC_ROOT / f"AuthKey_{key_id}.p8"
        ),
        "apple/app-store-connect/metadata.json": METADATA_PATH,
        "tauri/magician.key": TAURI_ROOT / "magician.key",
        "tauri/magician.key.pub": TAURI_ROOT / "magician.key.pub",
    }


def build_manifest(root: Path) -> dict[str, object]:
    files: dict[str, str] = {}
    for path in sorted(root.rglob("*")):
        if path.is_file() and path.name != "manifest.json":
            files[path.relative_to(root).as_posix()] = sha256(path)
    return {
        "format": "magican-apple-signing-transfer",
        "version": ARCHIVE_VERSION,
        "created_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "files": files,
    }


def encrypt(source: Path, destination: Path, passphrase: str) -> None:
    with tempfile.TemporaryDirectory(
        prefix="magican-signing-ciphertext-", dir=destination.parent
    ) as raw_work:
        ciphertext_path = Path(raw_work) / "ciphertext"
        run(
            [
                "openssl",
                "enc",
                "-aes-256-cbc",
                "-salt",
                "-pbkdf2",
                "-iter",
                str(KDF_ITERATIONS),
                "-md",
                "sha256",
                "-in",
                str(source),
                "-out",
                str(ciphertext_path),
                "-pass",
                "stdin",
            ],
            input_bytes=(passphrase + "\n").encode("utf-8"),
        )
        ciphertext = ciphertext_path.read_bytes()

    # `openssl enc` deliberately does not support AEAD modes. Authenticate its
    # complete salted ciphertext with an independently salted PBKDF2/HMAC key
    # so a corrupted or modified bundle is rejected before decryption.
    mac_salt = os.urandom(16)
    mac_key = hashlib.pbkdf2_hmac(
        "sha256",
        passphrase.encode("utf-8"),
        mac_salt,
        KDF_ITERATIONS,
        dklen=32,
    )
    authenticated = BUNDLE_MAGIC + mac_salt + ciphertext
    tag = hmac.new(mac_key, authenticated, hashlib.sha256).digest()
    destination.write_bytes(authenticated + tag)
    destination.chmod(0o600)


def decrypt(source: Path, destination: Path, passphrase: str) -> None:
    payload = source.read_bytes()
    minimum = len(BUNDLE_MAGIC) + 16 + 32 + 1
    if len(payload) < minimum or not payload.startswith(BUNDLE_MAGIC):
        fail("the signing bundle has an invalid or unsupported envelope")
    mac_salt_start = len(BUNDLE_MAGIC)
    ciphertext_start = mac_salt_start + 16
    mac_salt = payload[mac_salt_start:ciphertext_start]
    ciphertext = payload[ciphertext_start:-32]
    supplied_tag = payload[-32:]
    mac_key = hashlib.pbkdf2_hmac(
        "sha256",
        passphrase.encode("utf-8"),
        mac_salt,
        KDF_ITERATIONS,
        dklen=32,
    )
    expected_tag = hmac.new(mac_key, payload[:-32], hashlib.sha256).digest()
    if not hmac.compare_digest(supplied_tag, expected_tag):
        fail("the signing bundle failed authentication; check its passphrase and integrity")

    ciphertext_path = destination.with_name(destination.name + ".ciphertext")
    ciphertext_path.write_bytes(ciphertext)
    ciphertext_path.chmod(0o600)
    result = run(
        [
            "openssl",
            "enc",
            "-d",
            "-aes-256-cbc",
            "-pbkdf2",
            "-iter",
            str(KDF_ITERATIONS),
            "-md",
            "sha256",
            "-in",
            str(ciphertext_path),
            "-out",
            str(destination),
            "-pass",
            "stdin",
        ],
        input_bytes=(passphrase + "\n").encode("utf-8"),
        check=False,
    )
    ciphertext_path.unlink(missing_ok=True)
    if result.returncode != 0:
        fail("the signing bundle could not be decrypted; check its passphrase and integrity")
    destination.chmod(0o600)


def safe_extract(archive: Path, destination: Path) -> None:
    with tarfile.open(archive, "r") as handle:
        members = handle.getmembers()
        for member in members:
            path = PurePosixPath(member.name)
            if path.is_absolute() or ".." in path.parts:
                fail("signing bundle contains an unsafe path")
            if not (member.isfile() or member.isdir()):
                fail("signing bundle contains an unsupported link or special file")
        handle.extractall(destination)


def verify_extracted(root: Path) -> dict[str, object]:
    manifest_path = root / "manifest.json"
    manifest = read_json(manifest_path)
    if manifest.get("format") != "magican-apple-signing-transfer":
        fail("unrecognized signing bundle format")
    if manifest.get("version") != ARCHIVE_VERSION:
        fail(f"unsupported signing bundle version: {manifest.get('version')}")
    expected = manifest.get("files")
    if not isinstance(expected, dict) or not expected:
        fail("signing bundle manifest has no files")

    actual = {
        path.relative_to(root).as_posix()
        for path in root.rglob("*")
        if path.is_file() and path.name != "manifest.json"
    }
    expected_names = {str(name) for name in expected}
    if actual != expected_names:
        fail("signing bundle file list does not match its manifest")
    for name, expected_hash in expected.items():
        path = root / str(name)
        if sha256(path) != expected_hash:
            fail(f"signing bundle checksum failed: {name}")
    return manifest


def unpack_bundle(bundle: Path, passphrase: str, work: Path) -> Path:
    archive = work / "bundle.tar"
    extracted = work / "extracted"
    extracted.mkdir(mode=0o700)
    decrypt(bundle, archive, passphrase)
    try:
        safe_extract(archive, extracted)
    except tarfile.TarError as error:
        fail(f"invalid signing bundle archive: {error}")
    verify_extracted(extracted)
    return extracted


def export_bundle(args: argparse.Namespace) -> None:
    require_macos()
    output = resolved(args.output)
    ensure_outside_repo(output, "output bundle")
    if output.exists() and not args.force:
        fail(f"output already exists (pass --force to replace it): {output}")
    output.parent.mkdir(parents=True, exist_ok=True, mode=0o700)

    metadata = read_json(METADATA_PATH)
    sources = archive_sources(metadata)
    for source in sources.values():
        if not source.is_file():
            fail(f"required signing file is missing: {source}")

    secrets = {
        name: keychain_password(service)
        for name, service in KEYCHAIN_SERVICES.items()
    }
    passphrase = load_passphrase(args.passphrase_file, confirm=True)

    with tempfile.TemporaryDirectory(prefix="magican-signing-export-") as raw_work:
        work = Path(raw_work)
        stage = work / "stage"
        stage.mkdir(mode=0o700)
        for relative, source in sources.items():
            copy_private(source, stage / relative)
        write_json_private(stage / "keychain-secrets.json", secrets)
        (stage / "README.txt").write_text(
            "This encrypted archive is for scripts/apple_signing_transfer.py.\n"
            "It deliberately excludes Xcode-managed development certificates,\n"
            "provisioning profiles, and raw CSR private-key working files.\n",
            encoding="utf-8",
        )
        (stage / "README.txt").chmod(0o600)
        write_json_private(stage / "manifest.json", build_manifest(stage))

        archive = work / "bundle.tar"
        with tarfile.open(archive, "w") as handle:
            handle.add(stage, arcname=".", recursive=True)
        encrypt(archive, output, passphrase)

        verification = work / "verification"
        verification.mkdir(mode=0o700)
        unpack_bundle(output, passphrase, verification)

    print(f"Encrypted signing bundle created and verified: {output}")
    print("Transfer it directly (for example, AirDrop or an encrypted drive).")
    print("Send the bundle passphrase through a separate channel.")


def read_bundle_metadata(root: Path) -> tuple[dict[str, object], Path]:
    metadata_path = root / "apple/app-store-connect/metadata.json"
    metadata = read_json(metadata_path)
    key_id = require_text(metadata, "key_id")
    private_key = root / f"apple/app-store-connect/AuthKey_{key_id}.p8"
    if not private_key.is_file():
        fail(f"bundle is missing App Store Connect key {key_id}")
    return metadata, private_key


def read_bundle_secrets(root: Path) -> dict[str, str]:
    data = read_json(root / "keychain-secrets.json")
    result: dict[str, str] = {}
    for name in KEYCHAIN_SERVICES:
        value = data.get(name)
        if not isinstance(value, str) or not value:
            fail(f"bundle is missing protected Keychain value: {name}")
        result[name] = value
    return result


def destination_files(root: Path, metadata: dict[str, object]) -> dict[Path, Path]:
    key_id = require_text(metadata, "key_id")
    return {
        CERT_ROOT / "magican-developer-id.p12": (
            root / "apple/certificates/magican-developer-id.p12"
        ),
        CERT_ROOT / "magican-apple-distribution.p12": (
            root / "apple/certificates/magican-apple-distribution.p12"
        ),
        ASC_ROOT / f"AuthKey_{key_id}.p8": (
            root / f"apple/app-store-connect/AuthKey_{key_id}.p8"
        ),
        ASC_ROOT / "metadata.json": root / "apple/app-store-connect/metadata.json",
        TAURI_ROOT / "magician.key": root / "tauri/magician.key",
        TAURI_ROOT / "magician.key.pub": root / "tauri/magician.key.pub",
    }


def signing_identities() -> str:
    result = run(
        ["security", "find-identity", "-v", "-p", "codesigning"], check=False
    )
    return (result.stdout + result.stderr).decode("utf-8", errors="replace")


def import_p12(path: Path, password: str, identity: str) -> None:
    if identity in signing_identities():
        print(f"OK  Keychain identity already present: {identity}")
        return
    result = run(
        [
            "security",
            "import",
            str(path),
            "-k",
            str(Path.home() / "Library/Keychains/login.keychain-db"),
            "-P",
            password,
            "-T",
            "/usr/bin/codesign",
            "-T",
            "/usr/bin/security",
        ],
        check=False,
    )
    if result.returncode != 0 or identity not in signing_identities():
        detail = result.stderr.decode("utf-8", errors="replace").strip()
        fail(f"could not import {identity}{f': {detail}' if detail else ''}")
    print(f"OK  Imported Keychain identity: {identity}")


def store_keychain_password(service: str, password: str) -> None:
    run(
        [
            "security",
            "add-generic-password",
            "-U",
            "-a",
            KEYCHAIN_ACCOUNT,
            "-s",
            service,
            "-w",
            password,
        ]
    )


def write_local_signing_config(team_id: str, *, replace: bool) -> None:
    content = (
        "// Local Apple signing identity. This file is intentionally gitignored.\n"
        f"DEVELOPMENT_TEAM = {team_id}\n"
    )
    if SIGNING_XCCONFIG.exists():
        existing = SIGNING_XCCONFIG.read_text(encoding="utf-8")
        if existing == content or f"DEVELOPMENT_TEAM = {team_id}" in existing:
            return
        if not replace:
            fail(
                f"{SIGNING_XCCONFIG} already selects another team; rerun with --replace"
            )
    SIGNING_XCCONFIG.write_text(content, encoding="utf-8")
    SIGNING_XCCONFIG.chmod(0o600)


def import_bundle(args: argparse.Namespace) -> None:
    require_macos()
    bundle = resolved(args.bundle)
    ensure_outside_repo(bundle, "input bundle")
    if not bundle.is_file():
        fail(f"signing bundle does not exist: {bundle}")
    passphrase = load_passphrase(args.passphrase_file, confirm=False)

    with tempfile.TemporaryDirectory(prefix="magican-signing-import-") as raw_work:
        root = unpack_bundle(bundle, passphrase, Path(raw_work))
        metadata, _ = read_bundle_metadata(root)
        secrets = read_bundle_secrets(root)
        destinations = destination_files(root, metadata)
        conflicts = [
            destination
            for destination, source in destinations.items()
            if destination.exists() and destination.read_bytes() != source.read_bytes()
        ]
        if conflicts and not args.replace:
            joined = "\n  ".join(str(path) for path in conflicts)
            fail(f"destination has different signing files; rerun with --replace:\n  {joined}")

        team_id = require_text(metadata, "team_id")
        developer_identity = require_text(metadata, "developer_id_identity")
        distribution_identity = require_text(metadata, "distribution_identity")
        if args.dry_run:
            print("Signing bundle decrypted and verified.")
            print(f"Would install {len(destinations)} files for Apple team {team_id}.")
            print("Would import the Developer ID and Apple Distribution identities.")
            print("Would write the gitignored magios/Signing.local.xcconfig.")
            return

        backup_root: Path | None = None
        if conflicts:
            stamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
            backup_root = APPLE_ROOT / "backups" / stamp
            for destination in conflicts:
                if destination.is_relative_to(APPLE_ROOT):
                    relative = destination.relative_to(APPLE_ROOT)
                elif destination.is_relative_to(TAURI_ROOT):
                    relative = Path("tauri") / destination.relative_to(TAURI_ROOT)
                else:
                    fail(f"refusing to back up an unexpected path: {destination}")
                copy_private(destination, backup_root / relative)

        for destination, source in destinations.items():
            copy_private(source, destination)

        installed_metadata = read_json(METADATA_PATH)
        installed_metadata.update(
            {
                "developer_id_p12_path": str(
                    CERT_ROOT / "magican-developer-id.p12"
                ),
                "distribution_p12_path": str(
                    CERT_ROOT / "magican-apple-distribution.p12"
                ),
                "private_key_path": str(
                    ASC_ROOT / f"AuthKey_{require_text(metadata, 'key_id')}.p8"
                ),
                "tauri_private_key_path": str(TAURI_ROOT / "magician.key"),
                "tauri_public_key_path": str(TAURI_ROOT / "magician.key.pub"),
            }
        )
        write_json_private(METADATA_PATH, installed_metadata)

        import_p12(
            CERT_ROOT / "magican-developer-id.p12",
            secrets["developer_id_p12_password"],
            developer_identity,
        )
        import_p12(
            CERT_ROOT / "magican-apple-distribution.p12",
            secrets["distribution_p12_password"],
            distribution_identity,
        )
        for name, service in KEYCHAIN_SERVICES.items():
            store_keychain_password(service, secrets[name])
        write_local_signing_config(team_id, replace=args.replace)

        print(f"Apple release signing setup imported for team {team_id}.")
        if backup_root is not None:
            print(f"Previous local files were backed up under: {backup_root}")
        print("Next: sign in to the same Apple team in Xcode Settings > Accounts.")
        print("Let Xcode create this Mac's Apple Development certificate and profiles.")


def verify_pkcs12(path: Path, password: str) -> bool:
    return (
        run(
            [
                "openssl",
                "pkcs12",
                "-in",
                str(path),
                "-noout",
                # macOS Keychain exports PKCS#12 with legacy RC2 protection.
                # OpenSSL 3 requires its legacy provider to read that format.
                "-legacy",
                "-passin",
                "stdin",
            ],
            input_bytes=(password + "\n").encode("utf-8"),
            check=False,
        ).returncode
        == 0
    )


def status(args: argparse.Namespace) -> None:
    require_macos()
    failures: list[str] = []

    def check(condition: bool, label: str) -> None:
        print(f"{'OK  ' if condition else 'FAIL'} {label}")
        if not condition:
            failures.append(label)

    check(METADATA_PATH.is_file(), "App Store Connect metadata is present")
    if not METADATA_PATH.is_file():
        raise SystemExit(1)
    metadata = read_json(METADATA_PATH)
    team_id = require_text(metadata, "team_id")
    key_id = require_text(metadata, "key_id")
    issuer_id = require_text(metadata, "issuer_id")
    developer_identity = require_text(metadata, "developer_id_identity")
    distribution_identity = require_text(metadata, "distribution_identity")
    asc_key = ASC_ROOT / f"AuthKey_{key_id}.p8"
    dev_p12 = CERT_ROOT / "magican-developer-id.p12"
    dist_p12 = CERT_ROOT / "magican-apple-distribution.p12"

    identities = signing_identities()
    check(developer_identity in identities, "Developer ID identity is in the login Keychain")
    check(distribution_identity in identities, "Apple Distribution identity is in the login Keychain")
    check(asc_key.is_file(), "App Store Connect private key is present outside Git")
    if asc_key.is_file():
        key_check = run(
            ["openssl", "pkey", "-in", str(asc_key), "-noout", "-check"],
            check=False,
        )
        check(key_check.returncode == 0, "App Store Connect private key parses")

    passwords: dict[str, str] = {}
    for name, service in KEYCHAIN_SERVICES.items():
        try:
            passwords[name] = keychain_password(service)
            check(True, f"Keychain password exists: {service}")
        except SystemExit:
            check(False, f"Keychain password exists: {service}")
    if dev_p12.is_file() and "developer_id_p12_password" in passwords:
        check(
            verify_pkcs12(dev_p12, passwords["developer_id_p12_password"]),
            "Developer ID PKCS#12 password verifies",
        )
    else:
        check(False, "Developer ID PKCS#12 archive is present")
    if dist_p12.is_file() and "distribution_p12_password" in passwords:
        check(
            verify_pkcs12(dist_p12, passwords["distribution_p12_password"]),
            "Apple Distribution PKCS#12 password verifies",
        )
    else:
        check(False, "Apple Distribution PKCS#12 archive is present")
    check((TAURI_ROOT / "magician.key").is_file(), "Tauri updater private key is present outside Git")
    check((TAURI_ROOT / "magician.key.pub").is_file(), "Tauri updater public key is present outside Git")
    check(
        SIGNING_XCCONFIG.is_file()
        and f"DEVELOPMENT_TEAM = {team_id}" in SIGNING_XCCONFIG.read_text(encoding="utf-8"),
        f"gitignored Xcode config selects team {team_id}",
    )

    if args.network and asc_key.is_file():
        result = run(
            [
                "xcrun",
                "notarytool",
                "history",
                "--key",
                str(asc_key),
                "--key-id",
                key_id,
                "--issuer",
                issuer_id,
                "--output-format",
                "json",
            ],
            check=False,
        )
        check(result.returncode == 0, "App Store Connect notarization credentials authenticate")

    if failures:
        raise SystemExit(1)


def verify_bundle(args: argparse.Namespace) -> None:
    require_macos()
    bundle = resolved(args.bundle)
    ensure_outside_repo(bundle, "input bundle")
    if not bundle.is_file():
        fail(f"signing bundle does not exist: {bundle}")
    passphrase = load_passphrase(args.passphrase_file, confirm=False)
    with tempfile.TemporaryDirectory(prefix="magican-signing-verify-") as raw_work:
        root = unpack_bundle(bundle, passphrase, Path(raw_work))
        metadata, _ = read_bundle_metadata(root)
        read_bundle_secrets(root)
        print(
            "Signing bundle is valid for Apple team "
            f"{require_text(metadata, 'team_id')}."
        )


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(
        description="Securely transfer Magican Apple release signing setup between Macs."
    )
    commands = result.add_subparsers(dest="command", required=True)

    export = commands.add_parser("export", help="create an encrypted transfer bundle")
    export.add_argument("--output", required=True, type=Path)
    export.add_argument("--passphrase-file", type=Path)
    export.add_argument("--force", action="store_true")
    export.set_defaults(handler=export_bundle)

    verify = commands.add_parser("verify", help="decrypt and verify a transfer bundle")
    verify.add_argument("--bundle", required=True, type=Path)
    verify.add_argument("--passphrase-file", type=Path)
    verify.set_defaults(handler=verify_bundle)

    import_command = commands.add_parser(
        "import", help="install a transfer bundle on another Mac"
    )
    import_command.add_argument("--bundle", required=True, type=Path)
    import_command.add_argument("--passphrase-file", type=Path)
    import_command.add_argument("--replace", action="store_true")
    import_command.add_argument("--dry-run", action="store_true")
    import_command.set_defaults(handler=import_bundle)

    status_command = commands.add_parser("status", help="verify local signing setup")
    status_command.add_argument("--network", action="store_true")
    status_command.set_defaults(handler=status)
    return result


def main() -> None:
    args = parser().parse_args()
    args.handler(args)


if __name__ == "__main__":
    main()
