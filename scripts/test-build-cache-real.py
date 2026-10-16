#!/usr/bin/env python3
"""Validate real mbx/C++ cache behavior in isolated build-storage fixtures."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import time


def main():
    repo = Path(__file__).resolve().parents[1]
    mbx = os.environ.get("MBX_BIN")
    if not mbx or not Path(mbx).is_file():
        raise SystemExit("Run make setup-build-cache before the real cache validation")
    base = Path(os.environ.get("MBX_CACHE_DIR") or Path.home() / ".cache/mbx") / "validation"
    root = base / (time.strftime("%Y%m%d-%H%M%S") + f"-{os.getpid()}")
    root.mkdir(parents=True)
    fixture = root / "native"
    probes = sorted((base.parent.parent / "build-benchmarks").glob("*/native-probe"))
    previous = Path(os.environ["BUILD_CACHE_DUCKDB_PROBE"]) if os.environ.get("BUILD_CACHE_DUCKDB_PROBE") else (probes[-1] if probes else None)
    actual_duckdb = previous is not None and previous.is_dir() and os.uname().sysname == "Darwin"
    if actual_duckdb:
        subprocess.run(["/bin/cp", "-cpR", str(previous), str(fixture)], check=True)
        builder = fixture / "build.rs"
        builder.write_text(builder.read_text().replace(str(previous), str(fixture)))
    else:
        (fixture / "src").mkdir(parents=True)
        (fixture / "Cargo.toml").write_text(
            '[package]\nname="magician-build-cache-validation"\nversion="0.0.0"\nedition="2021"\n'
            '[build-dependencies]\ncc="=1.2.60"\n')
        (fixture / "src/lib.rs").write_text("pub fn fixture() -> i32 { 7 }\n")
        (fixture / "sample.cpp").write_text("extern \"C\" int cache_fixture() { return 7; }\n")
        (fixture / "build.rs").write_text(
            'fn main() { println!("cargo:rerun-if-env-changed=BENCH_NATIVE_EPOCH"); '
            'cc::Build::new().cpp(true).opt_level(3).debug(false).define("NDEBUG", None)'
            '.file("sample.cpp").compile("cache_fixture"); }\n')
    env = os.environ.copy()
    for key in ("CC", "CXX", "HOST_CC", "HOST_CXX", "CC_KNOWN_WRAPPER_CUSTOM", "CARGO_INCREMENTAL"):
        env.pop(key, None)
    # Use the project Makefile while describing the isolated fixture as the
    # workspace being managed. No Magician runtime binaries are built or staged.
    target = fixture / "target"
    env.update(MAGICIAN_CARGO_MANAGED_VIEW=str(target), MAGICIAN_CARGO_WORKSPACE=str(fixture),
               RUSTC_JOB_GATE_NEXT="")
    evidence = {"source": str(fixture), "actual_duckdb_translation_units": actual_duckdb, "runs": []}
    for epoch in ("1", "2"):
        env["BENCH_NATIVE_EPOCH"] = epoch
        env["MBX_STATS_REPORT"] = str(root / f"stats-{epoch}.json")
        command = ["make", "--no-print-directory", "benchmark-rust-native-unit",
                   f"BENCH_RUST_NATIVE_MANIFEST={fixture / 'Cargo.toml'}",
                   f"CARGO_TARGET_DIR={target}", "CARGO_BUILD_JOBS=1"]
        print(f"Cache fixture pass {epoch}: {'two DuckDB C++ files' if actual_duckdb else 'C++ fixture'}", flush=True)
        started = time.monotonic()
        result = subprocess.run(command, cwd=repo, env=env, capture_output=True, text=True, timeout=300)
        (root / f"pass-{epoch}.log").write_text(result.stdout + result.stderr)
        evidence["runs"].append({"epoch": epoch, "returncode": result.returncode,
                                 "seconds": time.monotonic() - started})
        (root / "results.json").write_text(json.dumps(evidence, indent=2) + "\n")
        if result.returncode:
            print(result.stderr[-5000:], flush=True)
            raise SystemExit(result.returncode)
        if not target.is_symlink():
            raise SystemExit("mbx did not create a managed target link")
        managed_root = Path(os.environ.get("MBX_TARGET_ROOT") or base.parent / "targets").resolve()
        if not target.resolve().is_relative_to(managed_root):
            raise SystemExit("Fixture outputs escaped the configured managed target root")
        print(f"Pass {epoch} passed in {evidence['runs'][-1]['seconds']:.2f}s; target={target.resolve()}", flush=True)
    stats = json.loads((root / "stats-2.json").read_text())
    if stats["hits"] < (2 if actual_duckdb else 1):
        raise SystemExit("No confirmed repeat C++ cache hits")
    evidence["repeat_cache_hits"] = stats["hits"]
    (root / "results.json").write_text(json.dumps(evidence, indent=2) + "\n")
    print(f"EVIDENCE {root}", flush=True)


if __name__ == "__main__":
    main()
