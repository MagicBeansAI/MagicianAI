"""Self-provisioning Python environment for the codegraph MCP server.

Every MCP client launches `python3 docs/codegraph/mcp_server.py` with
whatever `python3` is on its PATH, and that interpreter may lack the `mcp`
package (or carry an incompatible one) — the server then dies on import and
clients only report "Connection closed". `ensure()` runs before that import
and moves the process into a managed venv built from requirements.txt:

1. Already inside the managed venv: use it; if `mcp` went missing there,
   reinstall into it once.
2. The managed venv exists, its interpreter still runs, and its stamp
   matches requirements.txt: re-exec into it. The stamp deliberately
   ignores which python3 launched us — clients disagree on that, and
   keying on it would make them rebuild the shared venv in turn.
3. Otherwise build/refresh it under a lock from the newest Python >= 3.10
   available (macOS /usr/bin/python3 is 3.9, too old for mcp), then
   re-exec into it.
4. If building fails (offline, no venv module), fall back to the current
   interpreter when it can import a supported `mcp`; else exit with the
   command that fixes it.

The venv is shared by every clone and worktree:
`$CODEGRAPH_VENV`, else `${XDG_CACHE_HOME:-~/.cache}/magician/codegraph-mcp-venv`.

stdout is the MCP stdio channel, so everything here (including pip output)
goes to stderr. Standard library only.
"""

from __future__ import annotations

import contextlib
import hashlib
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REQUIREMENTS = HERE / "requirements.txt"
STAMP_NAME = ".codegraph-stamp.json"
REEXEC_ENV = "_CODEGRAPH_MCP_REEXEC"


def log(msg: str) -> None:
    print(f"[codegraph-mcp] {msg}", file=sys.stderr, flush=True)


def venv_dir() -> Path:
    override = os.environ.get("CODEGRAPH_VENV")
    if override:
        return Path(override).expanduser()
    cache = os.environ.get("XDG_CACHE_HOME") or str(Path.home() / ".cache")
    return Path(cache) / "magician" / "codegraph-mcp-venv"


def venv_python(venv: Path) -> Path:
    return venv / "bin" / "python3"


MIN_PYTHON = (3, 10)


def python_version(python: str) -> tuple[int, int] | None:
    try:
        out = subprocess.run(
            [python, "-c", "import sys; print(sys.version_info[0], sys.version_info[1])"],
            capture_output=True, text=True, timeout=10, stdin=subprocess.DEVNULL,
        ).stdout.split()
        return (int(out[0]), int(out[1]))
    except (OSError, ValueError, IndexError, subprocess.SubprocessError):
        return None


def base_interpreter() -> str | None:
    """Newest usable interpreter to build the venv from."""
    current = getattr(sys, "_base_executable", None) or sys.executable
    if sys.version_info[:2] >= MIN_PYTHON:
        return current
    for minor in range(20, MIN_PYTHON[1] - 1, -1):
        found = shutil.which(f"python3.{minor}")
        if found and (python_version(found) or (0, 0)) >= MIN_PYTHON:
            return found
    return None


def wanted_stamp() -> dict:
    return {"requirements_sha256": hashlib.sha256(REQUIREMENTS.read_bytes()).hexdigest()}


def read_stamp(venv: Path) -> dict | None:
    try:
        return json.loads((venv / STAMP_NAME).read_text())
    except (OSError, ValueError):
        return None


def venv_ready(venv: Path) -> bool:
    # exists() follows the bin/python3 symlink, so a venv whose base
    # interpreter was uninstalled counts as not ready and gets rebuilt.
    return venv_python(venv).exists() and read_stamp(venv) == wanted_stamp()


def in_venv(venv: Path) -> bool:
    try:
        return Path(sys.prefix).resolve() == venv.resolve()
    except OSError:
        return False


def mcp_importable() -> bool:
    try:
        import mcp.server  # noqa: F401
    except ImportError:
        return False
    return True


@contextlib.contextmanager
def build_lock(venv: Path):
    """Serialise builds across concurrently starting clients."""
    import fcntl

    venv.parent.mkdir(parents=True, exist_ok=True)
    with open(venv.parent / (venv.name + ".lock"), "w") as handle:
        fcntl.flock(handle, fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(handle, fcntl.LOCK_UN)


def run_to_stderr(cmd: list[str]) -> None:
    subprocess.run(cmd, check=True, stdout=sys.stderr, stderr=sys.stderr, stdin=subprocess.DEVNULL)


def pip_install(python: Path | str) -> None:
    run_to_stderr([str(python), "-m", "pip", "install", "--disable-pip-version-check",
                   "--quiet", "-r", str(REQUIREMENTS)])


def build_venv(venv: Path) -> None:
    with build_lock(venv):
        if venv_ready(venv):
            return  # another client finished the build while we waited
        base = base_interpreter()
        if not base:
            raise OSError(f"no Python >= {MIN_PYTHON[0]}.{MIN_PYTHON[1]} found to build the venv")
        log(f"provisioning {venv} with {base} (first run or requirements changed)")
        run_to_stderr([base, "-m", "venv", "--clear", str(venv)])
        pip_install(venv_python(venv))
        (venv / STAMP_NAME).write_text(json.dumps(wanted_stamp(), indent=2))
        log("venv ready")


def reexec(python: Path) -> None:
    os.environ[REEXEC_ENV] = str(int(os.environ.get(REEXEC_ENV, "0")) + 1)
    os.execv(str(python), [str(python), *sys.argv])


def fail(reason: str) -> None:
    log(reason)
    log("fix: make setup-codegraph-mcp   (or: python3 -m pip install 'mcp>=1.10,<3')")
    sys.exit(1)


def ensure() -> None:
    """Return only when `import mcp` will succeed in this process."""
    if os.environ.get("CODEGRAPH_MCP_NO_VENV") == "1":
        if not mcp_importable():
            fail("CODEGRAPH_MCP_NO_VENV=1 but this python3 has no mcp package")
        return

    venv = venv_dir()
    hops = int(os.environ.get(REEXEC_ENV, "0"))

    if in_venv(venv):
        if mcp_importable():
            return
        if hops > 1:
            fail(f"mcp still missing in {venv} after reinstall")
        log(f"mcp missing in {venv}; reinstalling")
        try:
            pip_install(sys.executable)
        except (OSError, subprocess.CalledProcessError) as exc:
            fail(f"reinstall into {venv} failed: {exc}")
        reexec(Path(sys.executable))

    if hops > 2:
        fail("re-exec loop while selecting the codegraph venv")

    if not venv_ready(venv):
        try:
            build_venv(venv)
        except (OSError, subprocess.CalledProcessError) as exc:
            if mcp_importable():
                log(f"could not build {venv} ({exc}); using {sys.executable}")
                return
            fail(f"could not build {venv}: {exc}")
    reexec(venv_python(venv))


def report() -> None:
    """`--ensure-env`: provision (idempotently), report, and do not serve."""
    ensure()  # re-execs into the venv; returns once mcp imports here
    import importlib.metadata as md

    where = venv_dir() if in_venv(venv_dir()) else Path(sys.executable)
    log(f"ready: {where} (mcp {md.version('mcp')})")
