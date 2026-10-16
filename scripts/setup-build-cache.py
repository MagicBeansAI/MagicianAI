#!/usr/bin/env python3
"""Install a checksum-verified mbx release; optionally persist host overrides."""
import argparse
import fcntl
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile

from build_cache_defaults import VERSION, memory_budget, release_host, required_volume, storage_root


def config_path():
    return Path(os.environ.get("MAGICIAN_BUILD_CONFIG", str(
        Path(os.environ.get("XDG_CONFIG_HOME", str(Path.home() / ".config"))) /
        "magician" / "build-cache.mk"))).expanduser()


def download(url, maximum):
    # System curl uses the host trust store, including managed-machine roots.
    curl = shutil.which("curl")
    if not curl:
        raise RuntimeError("curl is required to download the verified mbx release")
    value = subprocess.check_output([curl, "--fail", "--location", "--silent", "--show-error",
                                     "--max-time", "60", "--max-filesize", str(maximum), url])
    if len(value) > maximum:
        raise RuntimeError("Release download exceeds the expected size")
    return value


def setting(value):
    value = str(value)
    if "\n" in value or "\r" in value:
        raise ValueError("Build paths cannot contain newlines")
    return value.replace("$", "$$").replace("#", "\\#")


def install(root, version):
    if not all(part.isdigit() for part in version.split(".")) or len(version.split(".")) != 3:
        raise RuntimeError("version must be a numeric release such as 1.23.0")
    host = release_host()
    if not host:
        raise RuntimeError("No prebuilt mbx release for this host; install mbx with Cargo and configure MBX_BIN")
    binary = root / "tools/mbx" / version / "bin/mbx"
    binary.parent.mkdir(parents=True, exist_ok=True)
    # Concurrent clones share the same tool; only one downloads/installs it.
    with (binary.parent / ".install.lock").open("a+") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        install_release(binary, version, host)
    return binary


def install_release(binary, version, host):
    if not binary.exists():
        asset = f"mbx-{host}.tar.gz"
        release = f"https://github.com/jdx/mr-boxington/releases/download/v{version}"
        print(f"Downloading verified mbx {version} for {host}", flush=True)
        checksums = download(release + "/SHA256SUMS", 1024 * 1024).decode()
        expected = [line.split()[0] for line in checksums.splitlines()
                    if len(line.split()) == 2 and line.split()[1].lstrip("*") == asset]
        archive = download(release + "/" + asset, 50 * 1024**2)
        if len(expected) != 1 or hashlib.sha256(archive).hexdigest() != expected[0]:
            raise RuntimeError("mbx release checksum verification failed")
        with tempfile.TemporaryDirectory(dir=binary.parent) as temporary:
            archive_path = Path(temporary) / asset
            archive_path.write_bytes(archive)
            with tarfile.open(archive_path) as bundle:
                entries = [entry for entry in bundle.getmembers() if entry.isfile() and Path(entry.name).name == "mbx"]
                if len(entries) != 1 or entries[0].size > 100 * 1024**2:
                    raise RuntimeError("Unexpected mbx release archive")
                executable = Path(temporary) / "mbx"
                executable.write_bytes(bundle.extractfile(entries[0]).read())
                executable.chmod(0o755)
                executable.replace(binary)


def prepare_tools(root):
    compiler_gate = root / "tools/rustc-gated"
    compiler_gate.parent.mkdir(parents=True, exist_ok=True)
    proxy = Path(__file__).resolve().parent / "build-tools/rustc-gated"
    if not compiler_gate.exists() or compiler_gate.read_bytes() != proxy.read_bytes():
        with tempfile.TemporaryDirectory(dir=compiler_gate.parent) as temporary:
            temporary_proxy = Path(temporary) / "rustc-gated"
            shutil.copyfile(proxy, temporary_proxy)
            temporary_proxy.chmod(0o755)
            temporary_proxy.replace(compiler_gate)
    native_cache = root / "sccache"
    native_cache.mkdir(parents=True, exist_ok=True)
    native_config = native_cache / "config.toml"
    if not native_config.exists():
        native_config.write_text("# Project-local fallback cache; no default remote backend.\n")
    return compiler_gate, native_config


def write_config(root, binary):
    compiler_gate, native_config = prepare_tools(root)
    native_cache = native_config.parent
    destination = config_path()
    destination.parent.mkdir(parents=True, exist_ok=True)
    required = required_volume(root)
    values = {
        "MAGICIAN_BUILD_CACHE": "mbx", "MBX_BIN": binary, "MAGICIAN_BUILD_STORAGE_ROOT": root,
        "MBX_CACHE_DIR": root / "mbx", "MBX_TARGET_ROOT": root / "mbx/targets",
        "MBX_GC_MAX_TOTAL_SIZE": os.environ.get("MBX_GC_MAX_TOTAL_SIZE") or None,
        "MBX_GC_MIN_FREE_SIZE": os.environ.get("MBX_GC_MIN_FREE_SIZE") or None,
        "MBX_GC_INTERVAL": "15m", "MBX_SCHEDULER_CPUS": str(min(4, os.cpu_count() or 1)),
        "MBX_SCHEDULER_MEMORY": memory_budget(), "MAGICIAN_BUILD_REQUIRED_VOLUME": required,
        "MAGICIAN_BUILD_TMPDIR": root / "tmp",
        "MAGICIAN_GATED_RUSTC": compiler_gate,
        "SCCACHE_CONF": native_config, "SCCACHE_DIR": native_cache / "objects",
        "SCCACHE_CACHE_SIZE": "2G", "SCCACHE_SERVER_UDS": native_cache / "server.sock",
    }
    content = "# Machine-local; generated by make setup-build-cache. Never commit this file.\n"
    content += "".join(f"{key} ?= {setting(value)}\n" for key, value in values.items() if value is not None)
    if destination.exists() and destination.read_text() != content:
        destination.with_suffix(".mk.previous").write_text(destination.read_text())
    temporary = destination.with_suffix(".mk.tmp")
    temporary.write_text(content)
    temporary.replace(destination)
    print(f"Build settings: {destination}\nStorage root: {root}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", default=VERSION)
    parser.add_argument("--storage-root", type=Path)
    parser.add_argument("--write-config", action="store_true", help="Persist optional machine-local overrides")
    opts = parser.parse_args()
    root = (opts.storage_root or storage_root()).expanduser().absolute()
    required = required_volume(root)
    if required and not os.access(required, os.W_OK):
        raise RuntimeError(f"Build volume {required} is not writable")
    binary = install(root, opts.version)
    subprocess.run([str(binary), "--version"], check=True)
    prepare_tools(root)
    if opts.write_config:
        write_config(root, binary)
    else:
        print(f"Storage root: {root}; using repository defaults (no host configuration required)", flush=True)


if __name__ == "__main__":
    main()
