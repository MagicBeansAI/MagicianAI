#!/usr/bin/env python3
"""Run a headless service with the standard Linux Secret Service backend.

The encrypted keyring lives on a separate persistent volume. Unlock material
comes from a mounted private secret or an owner's stdin pipe, never argv/env.
No Magician key, pairing format, or rollback check is replaced here.
"""

import argparse
import os
from pathlib import Path
import re
import selectors
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time


class KeyringError(Exception):
    pass


def private_directory(path):
    if not path.is_absolute() or path.is_symlink():
        raise KeyringError("keyring state must be an absolute, non-symlink directory")
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = path.stat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise KeyringError("keyring state must be owned by the runtime user with mode 0700")
    return path.resolve()


def read_password(path, stream):
    if path:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, "rb") as source:
            info = os.fstat(source.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_mode & 0o077:
                raise KeyringError("keyring password must be a private regular file (0400 or 0600)")
            password = source.read(4097)
    else:
        password = stream.read(4097)
    if not 32 <= len(password) <= 4096 or b"\0" in password:
        raise KeyringError("keyring unlock secret must contain 32–4096 bytes and no NUL")
    return password


def wait_for_mount_access(state, password_file, runtime_root):
    """Apple VirtioFS can briefly cache guest-root ownership during mount setup.

    Poll access without chmod/chown or relaxing the checks below. Other private
    input failures still fail closed after this bounded observation window.
    """
    deadline = time.monotonic() + 5
    while True:
        state_ready = not state.exists() or (
            state.stat().st_uid == os.getuid() and os.access(state, os.R_OK | os.W_OK | os.X_OK))
        secret_ready = not password_file or os.access(password_file, os.R_OK)
        root_ready = not runtime_root or not Path(runtime_root).exists() or os.access(
            runtime_root, os.R_OK | os.W_OK | os.X_OK)
        if state_ready and secret_ready and root_ready:
            return
        if time.monotonic() >= deadline:
            raise KeyringError("private container mounts are not accessible to the runtime user")
        time.sleep(0.1)


def stop(process):
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def run(args):
    if sys.platform != "linux" or os.getuid() == 0:
        raise KeyringError("run the keyring and application as the non-root Linux runtime user")
    programs = {}
    for name in ("dbus-daemon", "dbus-send", "gnome-keyring-daemon"):
        programs[name] = shutil.which(name)
        if not programs[name]:
            raise KeyringError(f"missing {name}; install dbus and gnome-keyring")
    runtime_root = os.environ.get("MAGICIAN_ROOT_DIR")
    wait_for_mount_access(Path(args.state_dir), args.password_file, runtime_root)
    state = private_directory(Path(args.state_dir))
    if runtime_root and state.is_relative_to(Path(runtime_root).resolve()):
        raise KeyringError("persist the OS keyring outside the application runtime/data volume")
    data = private_directory(state / "data")
    private_directory(data / "keyrings")
    password = read_password(args.password_file, sys.stdin.buffer)
    temporary = Path(tempfile.mkdtemp(prefix="magician-secret-service-"))
    control = private_directory(temporary / "keyring")
    env = {**os.environ, "XDG_DATA_HOME": str(data), "XDG_RUNTIME_DIR": str(temporary),
           "GNOME_KEYRING_CONTROL": str(control)}
    for key in ("DBUS_SESSION_BUS_ADDRESS", "DBUS_STARTER_ADDRESS", "DBUS_STARTER_BUS_TYPE"):
        env.pop(key, None)
    bus = keyring = child = None

    def forward(signum, _frame):
        if child is not None and child.poll() is None:
            child.send_signal(signum)
        else:
            raise KeyringError("keyring startup interrupted")

    previous = {sig: signal.signal(sig, forward) for sig in (signal.SIGTERM, signal.SIGINT)}
    try:
        bus = subprocess.Popen([programs["dbus-daemon"], "--session", "--nofork",
                                "--nopidfile", "--print-address=1"],
                               env=env, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        with selectors.DefaultSelector() as selector:
            selector.register(bus.stdout, selectors.EVENT_READ)
            if not selector.select(timeout=10):
                raise KeyringError("private D-Bus startup timed out")
            address = bus.stdout.readline(4096).decode().strip()
        if not address.startswith("unix:") or bus.poll() is not None:
            raise KeyringError("private D-Bus did not publish a Unix address")
        env["DBUS_SESSION_BUS_ADDRESS"] = address
        keyring = subprocess.Popen(
            [programs["gnome-keyring-daemon"], "--foreground", "--unlock",
             "--components=secrets", f"--control-directory={control}"], env=env,
            stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        keyring.stdin.write(password)
        keyring.stdin.close()
        del password
        deadline = time.monotonic() + 10
        while not (control / "control").exists():
            if keyring.poll() is not None or time.monotonic() >= deadline:
                raise KeyringError("Linux keyring startup failed")
            time.sleep(0.05)
        subprocess.run([programs["gnome-keyring-daemon"], "--start", "--components=secrets"],
                       env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                       check=True, timeout=10)
        # --unlock may exit successfully with the wrong password. Query the
        # actual collection before starting a runtime that could otherwise hang
        # trying to open an unavailable graphical password prompt.
        result = subprocess.run([
            programs["dbus-send"], "--session", "--print-reply", "--reply-timeout=5000",
            "--dest=org.freedesktop.secrets", "/org/freedesktop/secrets/collection/login",
            "org.freedesktop.DBus.Properties.Get", "string:org.freedesktop.Secret.Collection",
            "string:Locked"], env=env, capture_output=True, text=True, timeout=7)
        if result.returncode or not re.search(r"variant\s+boolean false\s*$", result.stdout):
            raise KeyringError("Linux keyring is locked; verify the mounted unlock secret")
        app_env = {**os.environ, "DBUS_SESSION_BUS_ADDRESS": address}
        # Only the private bus address reaches the application. Service-owned
        # XDG paths and the unlock material do not become application settings.
        print("Linux Secret Service ready (persistent keyring)", flush=True)
        child = subprocess.Popen(args.command, env=app_env, umask=0o077,
                                 stdin=subprocess.DEVNULL if args.password_stdin else None)
        while child.poll() is None:
            if bus.poll() is not None or keyring.poll() is not None:
                raise KeyringError("Linux Secret Service stopped while the runtime was active")
            try:
                child.wait(timeout=0.5)
            except subprocess.TimeoutExpired:
                pass
        return child.returncode if child.returncode >= 0 else 128 - child.returncode
    finally:
        stop(child)
        stop(keyring)
        stop(bus)
        for sig, handler in previous.items():
            signal.signal(sig, handler)
        shutil.rmtree(temporary)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state-dir", required=True, help="separate persistent keyring volume, mode 0700")
    parser.add_argument("--check-inputs", action="store_true", help="check mounted private inputs before runtime seeding")
    secret = parser.add_mutually_exclusive_group(required=True)
    secret.add_argument("--password-file", help="mounted private unlock secret; contents are used verbatim")
    secret.add_argument("--password-stdin", action="store_true", help="read unlock secret from the owner's pipe")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.command[:1] == ["--"]:
        args.command.pop(0)
    if not args.command and not args.check_inputs:
        parser.error("a command to run is required after --")
    try:
        if args.check_inputs:
            if not args.password_file or args.command:
                parser.error("--check-inputs requires a password file and no command")
            state = Path(args.state_dir)
            wait_for_mount_access(state, args.password_file, os.environ.get("MAGICIAN_ROOT_DIR"))
            private_directory(state)
            read_password(args.password_file, None)
            return 0
        return run(args)
    except (KeyringError, OSError, subprocess.SubprocessError) as error:
        # Never include command arguments, secret input, or daemon output.
        print(f"Linux keyring unavailable: {error if isinstance(error, KeyringError) else type(error).__name__}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
