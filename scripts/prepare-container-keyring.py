#!/usr/bin/env python3
"""Provision private Linux keyring mounts, without changing a running container.

Shared by the installer and the packaged desktop (which embeds this script).
Only paths and launch arguments are returned. Unlock bytes never enter argv,
environment, logs, JSON output, or the application runtime directory.
"""

import argparse
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import stat
import subprocess
import sys
import tempfile


class ProvisionError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise ProvisionError(message)


def private_path(path, directory=False):
    info = path.lstat()
    require(not stat.S_ISLNK(info.st_mode), "Keyring custody paths must not be symlinks")
    require((stat.S_ISDIR if directory else stat.S_ISREG)(info.st_mode),
            "Invalid keyring custody file type")
    require(info.st_uid == os.getuid() and not info.st_mode & 0o077,
            "Keyring custody must belong to the current user and have private permissions")
    if not directory:
        require(info.st_nlink == 1, "Keyring custody files must not have additional hard links")


def private_bytes(path, limit=16384):
    private_path(path)
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as source:
        info = os.fstat(source.fileno())
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid()
                and not info.st_mode & 0o077 and info.st_nlink == 1,
                "Keyring custody changed during validation")
        value = source.read(limit + 1)
    require(len(value) <= limit, "Keyring custody file exceeds its size limit")
    return value


def write_private(path, value):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "wb") as target:
        target.write(value)
        target.flush()
        os.fsync(target.fileno())


def mount_path(path):
    value = str(path)
    require(not any(char in value for char in (":", ",", "\n", "\r", "\0")),
            "Container mount paths cannot contain colons, commas or line breaks")
    return value


def provision(data_dir, custody_home, runtime="apple-container", migration_source=None):
    require(sys.platform != "win32", "Managed keyring provisioning requires a Unix desktop host")
    require(os.getuid() != 0, "Run managed container setup as the non-root desktop user")
    data = Path(data_dir).expanduser().resolve(strict=True)
    require(data.is_dir(), "Runtime root must be an existing directory")
    custody = Path(custody_home).expanduser().absolute()
    require(not custody.is_symlink(), "Keyring custody home must not be a symlink")
    require(not custody.resolve().is_relative_to(data), "Keep keyring custody outside the runtime root")
    require(custody.parent.is_dir(), "Keyring custody parent must already exist")
    custody.mkdir(mode=0o700, exist_ok=True)
    private_path(custody, directory=True)
    custody = custody.resolve()
    bundle = custody / hashlib.sha256(os.fsencode(data)).hexdigest()
    state = bundle / "state"
    secret = bundle / "unlock.secret"
    manifest = bundle / "manifest.json"
    # Serialize creation for one root. An interrupted bundle is retained and
    # rejected; a retry must never rotate a secret behind an existing keyring.
    lock = custody / (bundle.name + ".lock")
    descriptor = os.open(lock, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "rb") as handle:
        private_path(lock)
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if not bundle.exists() and not bundle.is_symlink():
            # Sealed runtime owners must be migrated explicitly with their
            # existing platform keys. Never generate replacement keys for them.
            system = data / "system"
            require(not system.is_symlink(), "Runtime system state must not be a symlink")
            existing_state = system.exists() and (not system.is_dir() or any(system.iterdir()))
            require(not existing_state or migration_source == "macos-keychain",
                    "Runtime already has system state; restore or explicitly migrate its existing keyring")
            bundle.mkdir(mode=0o700)
            state.mkdir(mode=0o700)
            password = secrets.token_hex(32).encode("ascii")
            write_private(secret, password)
            record = {"version": 1, "runtime_root": str(data), "runtime": runtime,
                      "secret_sha256": hashlib.sha256(password).hexdigest()}
            if existing_state:
                record["migration"] = {"source": migration_source, "status": "pending"}
            del password
            write_private(manifest, (json.dumps(record) + "\n").encode())
        private_path(bundle, directory=True)
        private_path(state, directory=True)
        record = json.loads(private_bytes(manifest))
        require(record.get("version") == 1 and record.get("runtime_root") == str(data),
                "Keyring custody belongs to another runtime or an unsupported version")
        require(record.get("runtime") == runtime, "Changing container engines requires explicit custody migration")
        migration = record.get("migration")
        require(migration is None or (isinstance(migration, dict)
                and migration.get("source") == "macos-keychain"
                and migration.get("status") in ("pending", "complete")),
                "Keyring custody has invalid migration state")
        migration_required = bool(migration and migration.get("status") == "pending")
        require(not migration_required or migration_source == migration.get("source"),
                "Runtime keyring migration is incomplete; retry it before starting the service")
        password = private_bytes(secret, 4096)
        require(32 <= len(password) <= 4096 and b"\0" not in password,
                "Invalid keyring unlock secret")
        require(secrets.compare_digest(hashlib.sha256(password).hexdigest(),
                                       str(record.get("secret_sha256", ""))),
                "Keyring unlock secret changed; restore the original instead of rotating it")
        del password
    launch_args = ["-v", f"{mount_path(data)}:/data",
                   "-v", f"{mount_path(state)}:/keyring",
                   "-v", f"{mount_path(secret)}:/run/secrets/magician-keyring-password:ro",
                   "-e", "MAGICIAN_KEYRING_STATE_DIR=/keyring",
                   "-e", "MAGICIAN_KEYRING_PASSWORD_FILE=/run/secrets/magician-keyring-password"]
    if runtime == "docker":
        # Linux bind mounts retain the host UID, unlike Apple's virtual IDs.
        # Keep host custody readable only by its owner without chowning it to
        # an unrelated numeric account on the host.
        launch_args += ["--user", f"{os.getuid()}:{os.getgid()}",
                        "-v", f"{mount_path(bundle / 'passwd')}:/etc/passwd:ro",
                        "-v", f"{mount_path(bundle / 'group')}:/etc/group:ro"]
    return {"version": 1, "runtime_root": str(data), "state_dir": str(state),
            "password_file": str(secret), "launch_args": launch_args}


def complete_migration(plan, source="macos-keychain"):
    bundle = Path(plan["state_dir"]).parent
    manifest = bundle / "manifest.json"
    private_path(bundle, directory=True)
    record = json.loads(private_bytes(manifest))
    migration = record.get("migration")
    require(isinstance(migration, dict) and migration.get("source") == source
            and migration.get("status") == "pending",
            "Keyring custody is not awaiting this migration")
    record["migration"] = {"source": source, "status": "complete"}
    temporary = bundle / (".manifest-" + secrets.token_hex(8))
    try:
        write_private(temporary, (json.dumps(record) + "\n").encode())
        os.replace(temporary, manifest)
        descriptor = os.open(bundle, os.O_RDONLY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    finally:
        with contextlib.suppress(FileNotFoundError):
            temporary.unlink()


def read_macos_password(security, service, account):
    result = subprocess.run(
        [security, "find-generic-password", "-w", "-s", service, "-a", account],
        capture_output=True, timeout=60)
    require(result.returncode == 0,
            f"Required macOS keychain entry is unavailable: {service}/{account}")
    value = result.stdout
    if value.endswith(b"\r\n"):
        value = value[:-2]
    elif value.endswith(b"\n"):
        value = value[:-1]
    require(0 < len(value) <= 65536 and b"\0" not in value and b"\n" not in value,
            f"Required macOS keychain entry is invalid: {service}/{account}")
    try:
        return value.decode("utf-8")
    except UnicodeDecodeError as error:
        raise ProvisionError(
            f"Required macOS keychain entry is not text: {service}/{account}") from error


def validate_master_key(value):
    return (isinstance(value, str) and len(value) == 64
            and all(char in "0123456789abcdefABCDEF" for char in value))


def validate_app_data_keyring(value):
    if validate_master_key(value):
        return True
    try:
        document = json.loads(value)
    except (json.JSONDecodeError, TypeError):
        return False
    rows = document.get("generations") if isinstance(document, dict) else None
    return (document.get("format_version") == 1 and isinstance(rows, list)
            and 0 < len(rows) <= 32
            and all(isinstance(row, dict) and row.get("revision") == index
                    and validate_master_key(row.get("key", ""))
                    for index, row in enumerate(rows, 1)))


def pairing_anchor_from_dump(security, expected):
    result = subprocess.run([security, "dump-keychain"], capture_output=True, timeout=60)
    require(result.returncode == 0 and len(result.stdout) <= 8 * 1024 * 1024,
            "Cannot inspect macOS keychain metadata for the paired-device anchor")
    text = result.stdout.decode("utf-8", errors="replace")
    accounts = []
    for block in re.split(r"(?m)(?=^class: )", text):
        service = re.search(r'^\s+"svce"<blob>="([^"]+)"$', block, re.MULTILINE)
        account = re.search(r'^\s+"acct"<blob>="([^"]+)"$', block, re.MULTILINE)
        if service and account and service.group(1) == "com.magicbeans.magician.paired-devices":
            accounts.append(account.group(1))
    matches = []
    for account in sorted(set(accounts)):
        value = read_macos_password(
            security, "com.magicbeans.magician.paired-devices", account)
        if secrets.compare_digest(value, expected):
            matches.append((account, value))
    require(len(matches) == 1,
            "Cannot identify the macOS paired-device anchor for this runtime root")
    return matches[0]


def load_macos_keychain_entries(data_dir, pairing_anchor_account=None):
    require(sys.platform == "darwin", "macOS keychain migration requires a macOS host")
    security = shutil.which("security")
    require(security is not None, "macOS keychain migration requires the security tool")
    master = read_macos_password(security, "com.magician.secret-store", "master-key")
    require(validate_master_key(master), "The Magician master key in macOS Keychain is invalid")
    app_data = read_macos_password(security, "com.magician.app-data", "root-key-v1")
    require(validate_app_data_keyring(app_data),
            "The Magician app-data keyring in macOS Keychain is invalid")
    entries = [
        {"service": "com.magician.secret-store", "account": "master-key", "value": master},
        {"service": "com.magician.app-data", "account": "root-key-v1", "value": app_data},
    ]
    roster = Path(data_dir) / "system" / "paired-devices.json"
    if roster.exists():
        require(not roster.is_symlink() and roster.is_file()
                and roster.stat().st_size <= 2 * 1024 * 1024,
                "Paired-device roster is not a safe regular file")
        document = json.loads(roster.read_text())
        generation = document.get("generation") if isinstance(document, dict) else None
        seal_hex = document.get("seal_hex") if isinstance(document, dict) else None
        require(isinstance(generation, int) and generation > 0
                and isinstance(seal_hex, str) and len(seal_hex) == 64
                and all(char in "0123456789abcdefABCDEF" for char in seal_hex),
                "Paired-device roster cannot be matched to its keychain anchor")
        expected = f"{generation}:{seal_hex}"
        if pairing_anchor_account:
            require(re.fullmatch(r"generation-[0-9a-f]{64}", pairing_anchor_account) is not None,
                    "Paired-device keychain account is invalid")
            anchor = read_macos_password(
                security, "com.magicbeans.magician.paired-devices", pairing_anchor_account)
            require(secrets.compare_digest(anchor, expected),
                    "Paired-device keychain anchor does not match this runtime root")
            account = pairing_anchor_account
        else:
            account, anchor = pairing_anchor_from_dump(security, expected)
        entries.append({"service": "com.magicbeans.magician.paired-devices",
                        "account": account, "value": anchor})
    return entries


KEYRING_IMPORTER = r'''import json, shutil, subprocess, sys
tool = shutil.which("secret-tool")
if not tool:
    raise SystemExit(2)
document = json.load(sys.stdin)
entries = document.get("entries") if isinstance(document, dict) else None
if document.get("version") != 1 or not isinstance(entries, list) or not 2 <= len(entries) <= 3:
    raise SystemExit(3)
for entry in entries:
    service, account, value = entry.get("service"), entry.get("account"), entry.get("value")
    if not all(isinstance(item, str) and item for item in (service, account, value)):
        raise SystemExit(4)
    attributes = ["service", service, "username", account,
                  "target", "default", "application", "rust-keyring"]
    stored = subprocess.run([tool, "store", "--label", "Magician migrated credential", *attributes],
                            input=value.encode(), stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL, timeout=20)
    if stored.returncode:
        raise SystemExit(5)
    checked = subprocess.run([tool, "lookup", "service", service, "username", account],
                             capture_output=True, timeout=20)
    actual = checked.stdout[:-1] if checked.stdout.endswith(b"\n") else checked.stdout
    if checked.returncode or actual != value.encode():
        raise SystemExit(6)
print("MAGICIAN_KEYRING_MIGRATION_OK")
'''


def migrate_macos_keychain(plan, engine, image, pairing_anchor_account=None):
    entries = load_macos_keychain_entries(plan["runtime_root"], pairing_anchor_account)
    payload = json.dumps({"version": 1, "entries": entries}).encode()
    name = "magician-keyring-migrate-" + secrets.token_hex(8)
    command = [engine, "run", "--rm", "--name", name, "--cpus", "1", "--memory", "512m",
               "-i", *plan["launch_args"], "--entrypoint", "python3", image,
               "/app/scripts/run-linux-keyring.py", "--state-dir", "/keyring",
               "--password-file", "/run/secrets/magician-keyring-password", "--",
               "python3", "-c", KEYRING_IMPORTER]
    try:
        result = subprocess.run(command, input=payload, capture_output=True, timeout=120)
        require(result.returncode == 0 and b"MAGICIAN_KEYRING_MIGRATION_OK" in result.stdout,
                "Could not import and verify macOS credentials in the Linux keyring")
    except subprocess.TimeoutExpired:
        with contextlib.suppress(OSError, subprocess.SubprocessError):
            subprocess.run([engine, "rm", "-f", name], capture_output=True, timeout=15)
        raise ProvisionError("macOS keychain migration timed out; the runtime was not started")
    finally:
        del payload
        for entry in entries:
            entry["value"] = ""


def docker_accounts(passwd, groups, uid, gid):
    """Retain image accounts and add the bind-mount owner for D-Bus/NSS lookup."""
    require(uid > 0 and gid >= 0, "Container owner must be a non-root Unix account")
    user_rows = [line.split(":") for line in passwd.splitlines() if line]
    group_rows = [line.split(":") for line in groups.splitlines() if line]
    require(all(len(row) == 7 and row[2].isdigit() for row in user_rows)
            and all(len(row) == 4 and row[2].isdigit() for row in group_rows),
            "Image has an unsupported account database")
    require(all(row[1] in ("x", "*", "!", "!!", "") for row in user_rows + group_rows),
            "Image account database must not contain password hashes")
    username = f"magician-host-{uid}"
    groupname = f"magician-host-{gid}"
    if not any(int(row[2]) == uid for row in user_rows):
        require(not any(row[0] == username for row in user_rows), "Image account name conflicts with host mapping")
        user_rows.append([username, "x", str(uid), str(gid), "Magician runtime", f"/tmp/{username}", "/bin/sh"])
    if not any(int(row[2]) == gid for row in group_rows):
        require(not any(row[0] == groupname for row in group_rows), "Image group name conflicts with host mapping")
        group_rows.append([groupname, "x", str(gid), ""])
    return ("\n".join(":".join(row) for row in user_rows) + "\n",
            "\n".join(":".join(row) for row in group_rows) + "\n")


def prepare_docker_accounts(plan, engine, image):
    # Read the candidate IMAGE's accounts, never the host's passwd database.
    query = "import json,pathlib; print(json.dumps({n:pathlib.Path('/etc/'+n).read_text() for n in ('passwd','group')}))"
    value = json.loads(guest_check(engine, image, [], query))
    passwd, groups = docker_accounts(value["passwd"], value["group"], os.getuid(), os.getgid())
    bundle = Path(plan["state_dir"]).parent
    for name, contents in (("passwd", passwd), ("group", groups)):
        fd, temporary = tempfile.mkstemp(prefix=f".{name}-", dir=bundle)
        try:
            with os.fdopen(fd, "w") as output:
                output.write(contents)
                output.flush()
                os.fsync(output.fileno())
            # Standard world-readable image account metadata, inside a private
            # host directory and mounted read-only. No passwords are copied.
            os.chmod(temporary, 0o644)
            os.replace(temporary, bundle / name)
        finally:
            with contextlib.suppress(FileNotFoundError):
                os.unlink(temporary)


def run_cli(command):
    result = subprocess.run(command, capture_output=True, text=True, timeout=30)
    require(result.returncode == 0, "Cannot inspect container; existing service was not changed")
    return result.stdout


def validate_existing(plan, runtime, engine, name):
    """Refuse replacement of containers whose actual custody differs from config."""
    if runtime == "apple-container":
        items = json.loads(run_cli([engine, "list", "--all", "--format", "json"]))
        require(isinstance(items, list) and all(isinstance(item, dict)
                and isinstance(item.get("configuration"), dict)
                and isinstance(item["configuration"].get("id"), str) for item in items),
                "Unrecognized container inventory; replacement was refused")
        matches = [item for item in items if item.get("configuration", {}).get("id") == name]
        require(len(matches) <= 1, "Container identity is ambiguous")
        if not matches:
            return
        items = json.loads(run_cli([engine, "inspect", name]))
        require(len(items) == 1, "Container inspection is ambiguous")
        config = items[0]["configuration"]
        env = dict(entry.split("=", 1) for entry in config["initProcess"]["environment"] if "=" in entry)
        mounts = {item["destination"]: (item["source"], "ro" in item.get("options", []))
                  for item in config["mounts"]}
        mount_count = len(config["mounts"])
    else:
        names = run_cli([engine, "ps", "-a", "--format", "{{.Names}}"]).splitlines()
        require(all(re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_.-]*", name) for name in names),
                "Unrecognized container inventory; replacement was refused")
        if name not in names:
            return
        items = json.loads(run_cli([engine, "inspect", name]))
        require(len(items) == 1, "Container inspection is ambiguous")
        config = items[0]
        env = dict(entry.split("=", 1) for entry in config["Config"]["Env"] if "=" in entry)
        require(all(item["Type"] == "bind" for item in config["Mounts"]),
                "Existing container has unmanaged volumes; explicit migration is required before replacement")
        mounts = {item["Destination"]: (item["Source"], not item["RW"])
                  for item in config["Mounts"]}
        mount_count = len(config["Mounts"])
    require(len(mounts) == mount_count,
            "Existing container has ambiguous mount destinations; replacement was refused")
    expected = {"/data": (plan["runtime_root"], False),
                "/keyring": (plan["state_dir"], False),
                "/run/secrets/magician-keyring-password": (plan["password_file"], True)}
    if runtime == "docker":
        bundle = Path(plan["state_dir"]).parent
        expected.update({"/etc/passwd": (str(bundle / "passwd"), True),
                         "/etc/group": (str(bundle / "group"), True)})
        require(config["Config"].get("User") == f"{os.getuid()}:{os.getgid()}",
                "Existing container user differs from managed custody; replacement was refused")
    require(mounts == expected,
            "Existing container mounts differ from managed custody; explicit migration is required before replacement")
    require(env.get("MAGICIAN_ROOT_DIR", "/data") == "/data"
            and env.get("MAGICIAN_KEYRING_STATE_DIR") == "/keyring"
            and env.get("MAGICIAN_KEYRING_PASSWORD_FILE") == "/run/secrets/magician-keyring-password",
            "Existing container keyring configuration differs; replacement was refused")


def verify_image(plan, engine, image):
    # Read-only access checks in a bounded disposable guest. Do not start a
    # second Secret Service against a keyring the existing runtime may be using.
    check = """import os, pathlib, shutil, stat, time, pwd
assert os.getuid() != 0, 'runtime image must use a non-root user'
pwd.getpwuid(os.getuid())
assert all(shutil.which(p) for p in ('dbus-daemon','dbus-send','gnome-keyring-daemon','secret-tool'))
assert pathlib.Path('/app/scripts/run-linux-keyring.py').is_file()
assert '--check-inputs' in pathlib.Path('/app/scripts/container-entrypoint.sh').read_text()
deadline=time.monotonic()+5
while pathlib.Path('/keyring').stat().st_uid != os.getuid() and time.monotonic()<deadline:
 time.sleep(0.1)
s=pathlib.Path('/keyring').stat()
assert s.st_uid == os.getuid() and not s.st_mode & 0o077
assert os.access('/keyring', os.R_OK | os.W_OK | os.X_OK)
p=pathlib.Path('/run/secrets/magician-keyring-password')
s=p.stat(); assert stat.S_ISREG(s.st_mode) and not s.st_mode & 0o077
b=p.read_bytes(); assert 32 <= len(b) <= 4096 and b'\\0' not in b
assert not os.access(p, os.W_OK), 'unlock secret must be mounted read-only'
assert os.access('/data', os.R_OK | os.W_OK | os.X_OK)
"""
    guest_check(engine, image, plan["launch_args"], check)


def guest_check(engine, image, launch_args, check):
    name = "magician-keyring-check-" + secrets.token_hex(8)
    command = [engine, "run", "--rm", "--name", name, "--cpus", "1", "--memory", "512m",
               *launch_args,
               "--entrypoint", "python3", image, "-c", check]
    try:
        result = subprocess.run(command, capture_output=True, timeout=60)
        require(result.returncode == 0,
                "Image cannot access private keyring mounts as its runtime user; check image packages and mount ownership")
        return result.stdout.decode()
    except subprocess.TimeoutExpired:
        with contextlib.suppress(OSError, subprocess.SubprocessError):
            subprocess.run([engine, "rm", "-f", name], capture_output=True, timeout=15)
        raise ProvisionError("Keyring image access check timed out; existing service was not changed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", required=True)
    parser.add_argument("--custody-home", default=os.environ.get(
        "MAGICIAN_CONTAINER_KEYRING_HOME", str(Path.home() / ".magician-container-keyrings")))
    parser.add_argument("--runtime", choices=["apple-container", "docker"], required=True)
    parser.add_argument("--engine", required=True, help="resolved container CLI path")
    parser.add_argument("--container", required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--pairing-anchor-account")
    parser.add_argument("--format", choices=["json", "args"], default="json")
    args = parser.parse_args()
    try:
        migration_source = "macos-keychain" if sys.platform == "darwin" else None
        plan = provision(args.data_dir, args.custody_home, args.runtime, migration_source)
        manifest = json.loads(private_bytes(Path(plan["state_dir"]).parent / "manifest.json"))
        migration = manifest.get("migration")
        if isinstance(migration, dict) and migration.get("status") == "pending":
            migrate_macos_keychain(
                plan, args.engine, args.image, args.pairing_anchor_account)
            complete_migration(plan)
            plan = provision(args.data_dir, args.custody_home, args.runtime)
        if args.runtime == "docker":
            prepare_docker_accounts(plan, args.engine, args.image)
        validate_existing(plan, args.runtime, args.engine, args.container)
        verify_image(plan, args.engine, args.image)
        print(json.dumps(plan) if args.format == "json" else "\n".join(plan["launch_args"]))
        return 0
    except (ProvisionError, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        # Never print subprocess output or secret-bearing file contents.
        reason = str(error) if isinstance(error, ProvisionError) else type(error).__name__
        print(f"Container keyring provisioning failed: {reason}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
