#!/usr/bin/env python3
"""Resumable, sequential Rust build experiments in a frozen SSD source copy."""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import html
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import statistics
import subprocess
import time

REPO = Path(__file__).resolve().parents[1]
COMPILERS = {"rustc", "clang", "clang++", "cc1", "cc1plus", "ld", "ld64.lld", "lld"}
MARKER = "\n// Isolated compilation benchmark; never copied into the working checkout.\n"


def processes():
    result = subprocess.run(
        ["/bin/ps", "-axo", "pid=,ppid=,pgid=,rss=,comm="],
        capture_output=True, text=True, check=True,
    )
    rows = []
    for line in result.stdout.splitlines():
        fields = line.strip().split(None, 4)
        if len(fields) == 5:
            pid, ppid, group, rss = map(int, fields[:4])
            rows.append((pid, ppid, group, rss, fields[4]))
    return rows


def compilers(rows):
    return [row for row in rows if Path(row[4]).name in COMPILERS]


def quiet():
    started = time.monotonic()
    last = 0.0
    while True:
        active = compilers(processes())
        if not active:
            return
        now = time.monotonic()
        if now - last >= 45:
            print(f"Waiting for {len(active)} existing compiler(s); "
                  f"RSS {sum(row[3] for row in active) / 1024**2:.1f} GiB", flush=True)
            last = now
        if now - started > 3600:
            raise RuntimeError("Other compilers have remained active for an hour; resume later")
        time.sleep(5)


def free_space(root):
    if shutil.disk_usage(root).free < 60 * 1024**3:
        raise RuntimeError("SSD1 has less than 60 GiB free; no build started")


def frontend_time(unit):
    if unit.get("rmeta_time") is not None:
        return unit["rmeta_time"]
    for name, section in unit.get("sections") or []:
        if name == "frontend":
            return section["end"] - section["start"]
    return None


def edit_workload(name):
    return "API body" if name.endswith("-api-only") or name == "llvm-api-256" else "Core body"


def save(root, report):
    stats_file = root / "native-sccache-stats.json"
    if stats_file.exists():
        stats = json.loads(stats_file.read_text())
        report["metadata"]["native_cache"] = {
            "compile_requests": stats["stats"]["compile_requests"],
            "cache_hits": sum(stats["stats"]["cache_hits"]["counts"].values()),
            "cache_misses": sum(stats["stats"]["cache_misses"]["counts"].values()),
            "evidence": stats_file.name}
    # Backfill crate timings for campaign runs recorded by earlier runner versions.
    for recorded in report["runs"]:
        if recorded.get("timings") and "slowest_units" not in recorded:
            timing_file = root / recorded["timings"]
            if timing_file.exists():
                match = re.search(r"const UNIT_DATA = (\[.*?\]);", timing_file.read_text(), re.S)
                if match:
                    recorded["slowest_units"] = sorted(
                        json.loads(match.group(1)), key=lambda unit: unit.get("duration", 0), reverse=True)[:12]
    report_path = root / "results.json"
    temporary = root / "results.json.tmp"
    temporary.write_text(json.dumps(report, indent=2) + "\n")
    temporary.replace(report_path)
    rows = []
    for run in report["runs"]:
        status = "Passed" if run["returncode"] == 0 else f"Failed ({run['returncode']})"
        rows.append("<tr>" + "".join(f"<td>{html.escape(str(value))}</td>" for value in (
            run["variant"], run["phase"], status,
            f'{run["seconds"]:.2f}', f'{run["peak_compiler_rss_gib"]:.2f}',
            run.get("foreign_compilers_seen", False),
        )) + f'<td><a href="{html.escape(run["log"])}">log</a>'
                    + (f' · <a href="{html.escape(run["timings"])}">crate timings</a>'
                       if run.get("timings") else '') + '</td></tr>')
    medians = []
    for name in dict.fromkeys(run["variant"] for run in report["runs"]):
        edits = [run for run in report["runs"] if run["variant"] == name
                 and run["phase"].startswith("edit-") and run["returncode"] == 0
                 and not run["foreign_compilers_seen"]]
        if not edits:
            continue
        elapsed = [x["seconds"] for x in edits]
        values = [html.escape(name), edit_workload(name), str(len(edits)),
                  f'{statistics.median(elapsed):.2f}', f'{min(elapsed):.2f}–{max(elapsed):.2f}']
        for crate in ("magician", "magician-api", "magician-bin"):
            measured = [unit for run in edits for unit in run.get("slowest_units", []) if unit["name"] == crate]
            times = [unit["duration"] for unit in measured]
            values.append(f'{statistics.median(times):.2f}' if times else '—')
            if crate != "magician-bin":
                frontend = [frontend_time(unit) for unit in measured if frontend_time(unit) is not None]
                values.append(f'{statistics.median(frontend):.2f}' if frontend else '—')
        medians.append('<tr>' + ''.join(f'<td>{value}</td>' for value in values) + '</tr>')
    setup = []
    for recorded in report["runs"]:
        if recorded["phase"] != "warm-up" or recorded["returncode"] or recorded["foreign_compilers_seen"]:
            continue
        values = [html.escape(recorded["variant"]),
                  "API setup" if edit_workload(recorded["variant"]) == "API body" else "Core setup"]
        for crate in ("magician", "magician-api"):
            units = [u for u in recorded.get("slowest_units", []) if u["name"] == crate]
            if units:
                value = frontend_time(units[0])
                values.extend([f'{units[0]["duration"]:.2f}', f'{value:.2f}' if value is not None else '—'])
            else:
                values.extend(['—', '—'])
        if values[2] != '—' or values[4] != '—':
            setup.append('<tr>' + ''.join(f'<td>{value}</td>' for value in values) + '</tr>')
    (root / "report.html").write_text(
        "<!doctype html><meta charset='utf-8'><title>Magician build experiments</title>"
        "<style>body{font:16px system-ui;margin:40px;max-width:1200px}"
        "table{border-collapse:collapse}td,th{padding:10px;border:1px solid #ccc}"
        "th{background:#eee}pre{white-space:pre-wrap}</style>"
        "<h1>Magician build experiments</h1>"
        "<p>Frozen working-tree source; sequential builds; shared dependencies. "
        "Warm-up prepares each configuration's cache and is excluded from edit medians. "
        "Warm-ups rebuild different amounts of work: their elapsed totals cannot rank compilers or linkers. "
        "RSS is sampled compiler-tree RSS, "
        "not a memory limit. The main codegen sweep overrides magician and magician-bin; "
        "llvm-api-256 varies only magician-api, keeping the core and binary at 16 units. "
        "Frontend flags and Cranelift apply to magician, magician-api and magician-bin. "
        "Compare frontend variants on the same nightly compiler. "
        "A failed build is a compatibility result, not a timing win. "
        "Crate durations overlap and should not be added together.</p>"
        "<h2>Warm edit medians (seconds; uncontended successful runs)</h2>"
        "<p>Compare rows with the same workload. Core-body edits may recompile dependent crates; "
        "API-body samples leave the core source unchanged. Small differences from two samples "
        "are not strong evidence of a speedup. Stable and nightly also use different compiler versions "
        "and Cargo job counts; their rows compare the tested configurations.</p>"
        "<table><tr><th>Variant</th><th>Workload</th><th>Samples</th><th>End to end</th><th>Range</th>"
        "<th>magician total</th><th>magician frontend</th>"
        "<th>API total</th><th>API frontend</th><th>Binary including link</th></tr>"
        + ''.join(medians) + "</table><p><a href='native-sccache-stats.json'>Native cache counters</a>"
        " · <a href='llvm-lines-top.txt'>LLVM IR profile</a></p>"
        "<h2>Setup pass: individual crate timings (seconds)</h2>"
        "<p>These reuse existing dependency artifacts and may reuse incremental crate state. "
        "They are not complete clean builds. The first nightly pass also rebuilds dependencies "
        "for that compiler; the second nightly pass reuses them. "
        "Exclude setup from edit medians; compare each compiler/settings pair together. "
        "Contended setup passes are omitted here and retained in the full evidence below.</p>"
        "<table><tr><th>Variant</th><th>Setup workload</th><th>magician total</th><th>magician frontend</th>"
        "<th>API total</th><th>API frontend</th></tr>" + ''.join(setup) + "</table><h2>All runs</h2>"
        + "<p>Failed compatibility probes and links are retained as evidence; "
        "their elapsed times do not describe a successful build.</p>"
        + "<table><tr><th>Variant</th><th>Phase</th><th>Status (exit)</th><th>Seconds</th>"
        "<th>Peak compiler GiB</th><th>Contended</th><th>Evidence</th></tr>"
        + "".join(rows) + "</table><h2>Metadata</h2><pre>"
        + html.escape(json.dumps(report["metadata"], indent=2)) + "</pre>"
    )


def run(root, report, variant, phase, command, cwd, env, timeout=2400):
    free_space(root)
    quiet()
    log_path = root / "logs" / f"{variant}-{phase}.log"
    log_path.parent.mkdir(exist_ok=True)
    print(f"START {variant} / {phase}", flush=True)
    started = time.monotonic()
    peak = 0
    foreign = False
    last = started
    stopped = None
    with log_path.open("w") as log:
        log.write("COMMAND " + repr(command) + "\n")
        log.flush()
        process = subprocess.Popen(command, cwd=cwd, env=env, stdout=log,
                                   stderr=subprocess.STDOUT, start_new_session=True)
        try:
            while process.poll() is None:
                rows = processes()
                owned_pids = {process.pid}
                if env.get("BENCH_NATIVE_SERVER_PID"):
                    owned_pids.add(int(env["BENCH_NATIVE_SERVER_PID"]))
                # sccache's detached daemon launches C++ compiler children in
                # another process group; count those as this benchmark's work.
                while True:
                    descendants = {row[0] for row in rows if row[1] in owned_pids}
                    if descendants <= owned_pids:
                        break
                    owned_pids.update(descendants)
                own = [row for row in compilers(rows) if row[2] == process.pid or row[0] in owned_pids]
                foreign = foreign or any(row not in own for row in compilers(rows))
                rss = sum(row[3] for row in own)
                peak = max(peak, rss)
                now = time.monotonic()
                if now - last >= 45:
                    print(f"RUNNING {variant} / {phase}: {now - started:.0f}s, "
                          f"compiler RSS {rss / 1024**2:.1f} GiB", flush=True)
                    last = now
                if rss > 24 * 1024**2 or now - started > timeout:
                    stopped = "compiler RSS >24 GiB" if rss > 24 * 1024**2 else "timeout"
                    os.killpg(process.pid, signal.SIGTERM)
                    try:
                        process.wait(timeout=15)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGKILL)
                    break
                try:
                    process.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    pass
            code = process.wait()
        except BaseException:
            os.killpg(process.pid, signal.SIGTERM)
            process.wait()
            raise
    entry = {"variant": variant, "phase": phase, "returncode": code,
             "seconds": time.monotonic() - started,
             "peak_compiler_rss_gib": peak / 1024**2,
             "foreign_compilers_seen": foreign,
             "log": str(log_path.relative_to(root)), "command": command,
             "toolchain": env.get("RUSTUP_TOOLCHAIN"),
             "cargo_jobs": env.get("CARGO_BUILD_JOBS"),
             "rustflags": env.get("RUSTFLAGS", "")}
    if stopped:
        entry["stopped"] = stopped
    timings = Path(env["CARGO_TARGET_DIR"]) / "cargo-timings" / "cargo-timing.html"
    if timings.exists():
        destination = root / "logs" / f"{variant}-{phase}-timings.html"
        shutil.copy2(timings, destination)
        entry["timings"] = str(destination.relative_to(root))
        match = re.search(r"const UNIT_DATA = (\[.*?\]);", destination.read_text(), re.S)
        if match:
            data = json.loads(match.group(1))
            entry["slowest_units"] = sorted(data, key=lambda unit: unit.get("duration", 0), reverse=True)[:12]
    report["runs"].append(entry)
    save(root, report)
    print(f"END {variant} / {phase}: exit {code}, {entry['seconds']:.2f}s, "
          f"peak {entry['peak_compiler_rss_gib']:.2f} GiB", flush=True)
    if code:
        print("\n".join(log_path.read_text(errors="replace").splitlines()[-18:]), flush=True)
    return code


def snapshot(root, report):
    source = root / "source"
    if source.exists():
        return source
    source.mkdir()
    names = subprocess.check_output(
        ["/usr/bin/git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"], cwd=REPO,
    ).split(b"\0")
    digest = hashlib.sha256()
    count = 0
    for raw in sorted(set(names)):
        if not raw:
            continue
        relative = Path(os.fsdecode(raw))
        if relative.parts[0] in {".git", ".aws", ".codex", ".agents", "output"}:
            continue
        if relative.name in {".env", ".env.development"}:
            continue
        original = REPO / relative
        destination = source / relative
        if not original.is_file():
            continue
        destination.parent.mkdir(parents=True, exist_ok=True)
        # Independent bytes: edits never reach the checkout, even for source symlinks.
        shutil.copy2(original, destination)
        digest.update(raw + b"\0" + hashlib.sha256(destination.read_bytes()).digest())
        count += 1
    report["metadata"]["snapshot_sha256"] = digest.hexdigest()
    report["metadata"]["snapshot_files"] = count
    report["metadata"]["git_head"] = subprocess.check_output(
        ["/usr/bin/git", "rev-parse", "HEAD"], cwd=REPO, text=True).strip()
    report["metadata"]["source"] = str(source)
    lib = source / "magician/src/lib.rs"
    lib.write_text(lib.read_text() + MARKER +
                   '#[doc(hidden)]\n#[inline(never)]\npub fn magician_build_benchmark_marker(value: u64) -> u64 {\n'
                   '    value.wrapping_add(0)\n}\n')
    main = source / "magician-bin/src/main.rs"
    content = main.read_text()
    content, matches = re.subn(
        r"((?:async\s+)?fn main\([^)]*\)[^{]*\{)",
        r"\1\n    std::hint::black_box(magician::magician_build_benchmark_marker(std::hint::black_box(0)));",
        content, count=1,
    )
    if matches != 1:
        raise RuntimeError("Cannot locate binary main for the isolated source-edit workload")
    main.write_text(content)
    save(root, report)
    return source


def environment(root, target, toolchain="stable", rustflags=""):
    env = os.environ.copy()
    for key in ("FAST", "FAST_CRANELIFT", "MAGICIAN_FAST_APPLIED", "RUSTFLAGS",
                "CARGO_ENCODED_RUSTFLAGS", "CARGO_PROFILE_DEV_CODEGEN_UNITS",
                "CARGO_PROFILE_DEV_PANIC", "RUSTC_WORKSPACE_WRAPPER"):
        env.pop(key, None)
    env.update(CARGO_TARGET_DIR=str(target), TMPDIR=str(root / "tmp"),
               RUSTUP_TOOLCHAIN=toolchain, CARGO_BUILD_JOBS="2" if toolchain == "nightly" else "4", CARGO_INCREMENTAL="1",
               RUSTC_JOB_GATE_DIR=f"/Volumes/build/magician/tmp/rustc-job-gate-{os.getuid()}",
               RUSTC_JOB_GATE_NEXT="", CARGO_TERM_COLOR="never")
    if rustflags:
        env["RUSTFLAGS"] = rustflags
    return env


def command(source, units, args=""):
    config = f'--config profile.dev.package.magician.codegen-units={units} '
    config += f'--config profile.dev.package.magician-bin.codegen-units={units}'
    extra = ["BENCH_RUST_LLD=1"] if args == "--lld" else []
    cargo_args = "" if extra else args
    return ["/usr/bin/make", "--no-print-directory", "benchmark-rust-build-unit",
            f"BENCH_RUST_CARGO_ARGS={config} {cargo_args}", *extra]


def edit(source, number):
    lib = source / "magician/src/lib.rs"
    text = lib.read_text().split(MARKER)[0]
    lib.write_text(text + MARKER +
                   '#[doc(hidden)]\n#[inline(never)]\npub fn magician_build_benchmark_marker(value: u64) -> u64 {\n'
                   f'    value.wrapping_add({number})\n}}\n')


def variant(root, report, source, name, units, iterations, toolchain="stable", rustflags="", args=""):
    if any(run["variant"] == name and (run["phase"] == f"edit-{iterations}" or run["returncode"])
           for run in report["runs"]):
        print(f"Already recorded {name}; skipping", flush=True)
        return
    target = clone_seed(root, name, units)
    env = environment(root, target, toolchain, rustflags)
    edit(source, 0)
    cmd = command(source, units, args)
    if run(root, report, name, "warm-up", cmd, source, env):
        return
    for number in range(1, iterations + 1):
        edit(source, number)
        if run(root, report, name, f"edit-{number}", cmd, source, env):
            break


def best_units(report):
    candidates = {}
    for run in report["runs"]:
        if re.fullmatch(r"llvm-(16|64|128|256)", run["variant"]) and run["phase"].startswith("edit-") \
                and not run["returncode"] and not run["foreign_compilers_seen"]:
            candidates.setdefault(int(run["variant"].split("-")[1]), []).append(run["seconds"])
    if not candidates or 16 not in candidates:
        return 16
    baseline = candidates[16]
    winners = {units: values for units, values in candidates.items()
               if units != 16 and statistics.median(values) <= statistics.median(baseline) * 0.95
               and max(values) < min(baseline)}
    return min(winners, key=lambda units: statistics.median(winners[units])) if winners else 16


def api_edits(root, report, source, iterations):
    """Measure API-only edits using the already validated compiler configurations."""
    api = source / "magician-api/src/lib.rs"
    original = api.read_text().split(MARKER)[0]
    for name in ("llvm-16", "nightly-1-thread", "nightly-2-threads", "cranelift"):
        completed = [run for run in report["runs"] if run["variant"] == name
                     and run["phase"] == f"edit-{iterations}" and not run["returncode"]]
        if not completed:
            continue
        label = name + "-api-only"
        if any(run["variant"] == label for run in report["runs"]):
            continue
        previous = completed[-1]
        target = root / "targets" / name
        toolchain = previous.get("toolchain", "nightly" if name.startswith("nightly-") or name == "cranelift" else "stable")
        env = environment(root, target, toolchain)
        for number in range(iterations + 1):
            api.write_text(original + MARKER +
                           '#[doc(hidden)]\n#[inline(never)]\npub fn magician_api_build_benchmark_marker(value: u64) -> u64 {\n'
                           f'    value.wrapping_add({number})\n}}\n')
            phase = "warm-up" if number == 0 else f"edit-{number}"
            if run(root, report, label, phase, previous["command"], source, env):
                break
    report["metadata"]["api_only_workload"] = "body edit in magician-api; core source unchanged during samples"


def api_codegen(root, report, source, iterations):
    name = "llvm-api-256"
    if any(run["variant"] == name and (run["phase"] == f"edit-{iterations}" or run["returncode"])
           for run in report["runs"]):
        return
    target = clone_seed(root, name)
    env = environment(root, target)
    cmd = command(source, 16, "--config profile.dev.package.magician-api.codegen-units=256")
    api = source / "magician-api/src/lib.rs"
    original = api.read_text().split(MARKER)[0]
    for number in range(iterations + 1):
        api.write_text(original + MARKER +
                       '#[doc(hidden)]\n#[inline(never)]\npub fn magician_api_build_benchmark_marker(value: u64) -> u64 {\n'
                       f'    value.wrapping_add({number})\n}}\n')
        phase = "warm-up" if number == 0 else f"edit-{number}"
        if run(root, report, name, phase, cmd, source, env):
            break
    report["metadata"]["api_codegen_scope"] = {"magician-api": 256, "magician": 16, "magician-bin": 16}


def cranelift_preflight(root, report, source):
    """Reject an unsupported panic strategy before recompiling the large crates."""
    previous = [run for run in report["runs"] if run["variant"] == "cranelift"
                and run["phase"].startswith("preflight")]
    if previous and (previous[-1]["returncode"] or previous[-1]["phase"] == "preflight-smoke"):
        return previous[-1]["phase"] == "preflight-smoke" and previous[-1]["returncode"] == 0
    probe = root / "cranelift-probe"
    (probe / "src").mkdir(parents=True, exist_ok=True)
    (probe / "Cargo.toml").write_text(
        'cargo-features=["codegen-backend"]\n[package]\nname="magician-cranelift-unwind-probe"\n'
        'version="0.0.0"\nedition="2021"\n[profile.dev]\npanic="unwind"\ncodegen-backend="cranelift"\n')
    (probe / "src/main.rs").write_text(
        'fn main() { assert!(std::panic::catch_unwind(|| panic!("probe")).is_err()); }\n')
    env = environment(root, root / "targets/cranelift-probe", "nightly")
    code = run(root, report, "cranelift", "preflight",
               ["/usr/bin/make", "--no-print-directory", "benchmark-rust-native-unit",
                f"BENCH_RUST_NATIVE_MANIFEST={probe / 'Cargo.toml'}"], source, env, timeout=60)
    report["metadata"]["cranelift_preflight"] = "same panic=unwind strategy as Magician; minimal compile probe"
    if not code:
        code = run(root, report, "cranelift", "preflight-smoke",
                   [str(root / "targets/cranelift-probe/debug/magician-cranelift-unwind-probe")],
                   source, env, timeout=15)
    return code == 0


def clone_seed(root, name, units=None):
    target = root / "targets" / name
    if not target.exists():
        target.parent.mkdir(exist_ok=True)
        seed = root / "targets" / "llvm-16"
        if units and (root / "targets" / f"llvm-{units}").exists() and not name.startswith("llvm-"):
            seed = root / "targets" / f"llvm-{units}"
        if name in {"nightly-2-threads", "cranelift"} and (root / "targets/nightly-1-thread").exists():
            seed = root / "targets/nightly-1-thread"
        if not seed.exists() or name == "llvm-16":
            seed = Path("/Volumes/build/magician/builds")
        quiet()
        free_space(root)
        print(f"Clone-on-write debug seed {seed} -> {target}", flush=True)
        target.mkdir()
        subprocess.run(["/bin/cp", "-cpR", str(seed / "debug"), str(target / "debug")], check=True)
        if (seed / ".rustc_info.json").exists():
            shutil.copy2(seed / ".rustc_info.json", target / ".rustc_info.json")
    return target


def llvm_lines(root, report, source):
    if any(x["variant"] == "llvm-lines" and x["phase"] == "profile" for x in report["runs"]):
        return
    tools_dir = root / "tools"
    env = environment(root, root / "tools-target")
    executable = tools_dir / "bin/cargo-llvm-lines"
    if not executable.exists():
        if run(root, report, "llvm-lines", "install",
               ["/usr/bin/make", "--no-print-directory", "benchmark-rust-tools",
                f"BENCH_RUST_TOOLS_DIR={tools_dir}"], source, env):
            return
    target = clone_seed(root, "llvm-lines")
    env = environment(root, target)
    env["PATH"] = str(tools_dir / "bin") + os.pathsep + env["PATH"]
    edit(source, 0)
    code = run(root, report, "llvm-lines", "profile",
               ["/usr/bin/make", "--no-print-directory", "benchmark-rust-llvm-lines"], source, env)
    if not code:
        log = (root / "logs/llvm-lines-profile.log").read_text()
        lines = llvm_profile_rows(log)
        (root / "llvm-lines-top.txt").write_text("\n".join(lines[:35]) + "\n")


def llvm_profile_rows(log):
    # Compiler diagnostics also start with line numbers; require both numeric columns.
    return [line for line in log.splitlines()
            if re.match(r"\s*\d+(?:\s+\([^)]*\))?\s+\d+\s", line)]


def native(root, report, source):
    if any(x["variant"].startswith("native-explicit-cache") and x["phase"] == "warm"
           and not x["returncode"] for x in report["runs"]):
        return
    attempts = {re.sub(r"^native-(?:gate-only|explicit-cache)", "", x["variant"])
                for x in report["runs"] if x["variant"].startswith(("native-gate-only", "native-explicit-cache"))}
    suffix = f"-retry-{len(attempts)}" if attempts else ""
    probe = root / "native-probe"
    (probe / "src").mkdir(parents=True, exist_ok=True)
    (probe / "Cargo.toml").write_text(
        '[package]\nname="magician-native-cache-probe"\nversion="0.0.0"\nedition="2021"\n'
        '[build-dependencies]\ncc="=1.2.60"\n')
    # Exercise actual bundled DuckDB unity translation units, not a toy C++ file.
    # This measures repeat compilation/cache wiring, not a complete DuckDB build.
    duckdb = probe / "duckdb"
    if not duckdb.exists():
        manifests = sorted((root / "targets/llvm-16/debug/build").glob(
            "libduckdb-sys-*/out/duckdb/manifest.json"))
        if not manifests:
            raise RuntimeError("No extracted bundled DuckDB source in the baseline target")
        subprocess.run(["/bin/cp", "-cpR", str(manifests[0].parent), str(duckdb)], check=True)
    manifest = json.loads((duckdb / "manifest.json").read_text())
    samples = ["ub_src_main.cpp", "ub_src_planner.cpp"]
    includes = manifest["base"]["include_dirs"]
    includes += manifest["extensions"]["core_functions"]["include_dirs"]
    builder = ('let mut build = cc::Build::new(); build.cpp(true).opt_level(3).debug(false)'
               '.warnings(false).flag("-std=c++11").flag("-w").define("NDEBUG", None)'
               '.define("DUCKDB_EXTENSION_CORE_FUNCTIONS_LINKED", "1")'
               '.define("DUCKDB_EXTENSION_AUTOINSTALL_DEFAULT", "1")'
               '.define("DUCKDB_EXTENSION_AUTOLOAD_DEFAULT", "1");')
    builder += f'build.include({json.dumps(str(duckdb))});'
    for directory in sorted(set(includes)):
        builder += f'build.include({json.dumps(str(duckdb / directory))});'
    for sample in samples:
        builder += f'build.file({json.dumps(str(duckdb / sample))});'
    builder += 'build.compile("duckdb_cache_sample");'
    (probe / "src/lib.rs").write_text('pub const DUCKDB_CACHE_SAMPLE: bool = true;\n')
    (probe / "build.rs").write_text(
        'fn main() { println!("cargo:rerun-if-env-changed=BENCH_NATIVE_EPOCH"); '
        + builder + '}\n')
    report["metadata"]["native_sample"] = {
        "source": str(duckdb), "translation_units": samples, "optimization": "-O3, NDEBUG",
        "scope": "two bundled DuckDB translation units; not a full native build"}
    target = root / "targets/native-probe"
    env = environment(root, target)
    # sccache 0.15 rejects CARGO_INCREMENTAL=1 before compiler-family detection,
    # even for C++. Keep the Rust experiment's incremental setting out of this probe.
    env.pop("CARGO_INCREMENTAL", None)
    env["CARGO_PROFILE_DEV_INCREMENTAL"] = "false"
    for key in ("CC", "CXX", "HOST_CC", "HOST_CXX", "CC_KNOWN_WRAPPER_CUSTOM"):
        env.pop(key, None)
    for key in list(env):
        if key.startswith("SCCACHE"):
            env.pop(key)
    cache_config = root / "native-sccache.toml"
    cache_config.write_text("# Isolated local benchmark cache; no inherited remote backend.\n")
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    env.update(SCCACHE_CONF=str(cache_config), SCCACHE_DIR=str(root / "native-cache"), SCCACHE_SERVER_PORT=str(port),
               SCCACHE_IDLE_TIMEOUT="0", SCCACHE_CACHE_SIZE="2G", CC_ENABLE_DEBUG_OUTPUT="1")
    env.pop("SCCACHE_SERVER_UDS", None)
    cmd = ["/usr/bin/make", "--no-print-directory", "benchmark-rust-native-unit",
           f"BENCH_RUST_NATIVE_MANIFEST={probe / 'Cargo.toml'}"]
    for phase, epoch in (("warm-up", "0"), ("repeat", "1")):
        env["BENCH_NATIVE_EPOCH"] = epoch
        if run(root, report, "native-gate-only" + suffix, phase, cmd, source, env):
            return
    sccache = shutil.which("sccache")
    if not sccache:
        raise RuntimeError("sccache is required for the explicit native cache comparison")
    env["HOST_CC"] = sccache + " /usr/bin/clang"
    env["HOST_CXX"] = sccache + " /usr/bin/clang++"
    try:
        subprocess.run([sccache, "--start-server"], env=env, check=True,
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        listeners = set(subprocess.check_output(
            ["/usr/sbin/lsof", "-nP", "-t", f"-iTCP:{port}", "-sTCP:LISTEN"], text=True).split())
        if len(listeners) != 1:
            raise RuntimeError("Cannot identify the isolated sccache daemon")
        env["BENCH_NATIVE_SERVER_PID"] = str(int(listeners.pop()))
        for phase, epoch in (("cold", "2"), ("warm", "3")):
            env["BENCH_NATIVE_EPOCH"] = epoch
            if run(root, report, "native-explicit-cache" + suffix, phase, cmd, source, env):
                break
        stats = subprocess.run([sccache, "--show-stats", "--stats-format=json"],
                               env=env, capture_output=True, text=True)
        (root / "native-sccache-stats.json").write_text(stats.stdout)
    finally:
        subprocess.run([sccache, "--stop-server"], env=env, capture_output=True)


def nightly_plan(root, source):
    args = "-Zprofile-rustflags -Zunstable-options --unit-graph "
    args += "--config 'profile.dev.package.magician.rustflags=[\"-Zthreads=2\"]' "
    args += "--config 'profile.dev.package.magician-api.rustflags=[\"-Zthreads=2\"]' "
    args += "--config 'profile.dev.package.magician-bin.rustflags=[\"-Zthreads=2\"]'"
    result = subprocess.run(command(source, 16, args), cwd=source,
                            env=environment(root, root / "targets/plan", "nightly"),
                            capture_output=True, text=True)
    (root / "nightly-plan.log").write_text(result.stderr)
    if result.returncode:
        print(result.stderr[-2500:])
        raise RuntimeError("Nightly profile-rustflags plan failed")
    plan = json.loads(result.stdout)
    (root / "nightly-plan.json").write_text(json.dumps(plan, indent=2))
    selected = [unit for unit in plan["units"] if any('/' + name + '#' in unit["pkg_id"] for name in ("magician", "magician-api", "magician-bin"))]
    print(json.dumps(selected[:2], indent=2)[:4500])


def validate_binaries(root, report):
    complete = {run["variant"] for run in report["runs"]
                if run["phase"] == f"edit-{report['metadata']['iterations']}" and not run["returncode"]}
    validations = []
    for name in sorted(complete):
        binary = root / "targets" / name / "debug/magician"
        if not binary.exists():
            continue
        result = subprocess.run([str(binary), "--help"], capture_output=True, text=True, timeout=30)
        log = root / "logs" / f"{name}-help.log"
        log.write_text(result.stdout + result.stderr)
        passed = result.returncode == 0 and "Usage: magician" in result.stdout
        validations.append({"variant": name, "returncode": result.returncode,
                            "help_passed": passed, "log": str(log.relative_to(root))})
        print(f"HELP {name}: {'passed' if passed else 'failed'}", flush=True)
    report["metadata"]["binary_validation"] = validations
    save(root, report)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    default_root = Path(os.environ.get("BENCH_RUST_ROOT", "/Volumes/build/magician/build-benchmarks/" + time.strftime("%Y-%m-%d", time.gmtime())))
    parser.add_argument("--root", type=Path, default=default_root)
    parser.add_argument("--stage", choices=["prepare", "plan", "report", "validate", "baseline", "cgu", "nightly", "cranelift", "lld", "api", "api-cgu", "llvm-lines", "native", "all"], default="all")
    parser.add_argument("--iterations", type=int, default=2)
    parser.add_argument("--dry-run", action="store_true")
    opts = parser.parse_args()
    if opts.iterations < 1:
        parser.error("--iterations must be positive")
    if not str(opts.root.absolute()).startswith("/Volumes/build/magician/"):
        parser.error("benchmark source, outputs and tools must stay on SSD1")
    if opts.dry_run:
        print(json.dumps({"root": str(opts.root), "stage": opts.stage, "iterations": opts.iterations,
                          "codegen_units": [16, 64, 256], "source": "frozen working tree",
                          "runtime_install": False}, indent=2))
        return
    root = opts.root.absolute()
    root.mkdir(parents=True, exist_ok=True)
    (root / "tmp").mkdir(exist_ok=True)
    free_space(root)
    report_file = root / "results.json"
    report = json.loads(report_file.read_text()) if report_file.exists() else {
        "metadata": {"created": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                     "iterations": opts.iterations, "platform": os.uname().machine,
                     "units_scope": ["magician", "magician-bin"]}, "runs": []}
    if opts.stage != "plan":
        lock = (root / "benchmark.lock").open("a+")
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            parser.error("a benchmark runner is already using this source snapshot")
    if opts.iterations != report["metadata"]["iterations"]:
        parser.error("iterations must match the existing report")
    for toolchain in ("stable", "nightly"):
        version = subprocess.check_output(
            [str(Path.home() / ".cargo/bin/rustc"), f"+{toolchain}", "-Vv"], text=True).strip()
        if toolchain in report["metadata"] and version != report["metadata"][toolchain]:
            parser.error("compiler version changed; use a new benchmark root")
        report["metadata"][toolchain] = version
    source = snapshot(root, report)
    stage = opts.stage
    if stage == "report":
        profile = root / "logs/llvm-lines-profile.log"
        if profile.exists():
            lines = llvm_profile_rows(profile.read_text())
            (root / "llvm-lines-top.txt").write_text("\n".join(lines[:35]) + "\n")
            if lines:
                total = re.match(r"\s*(\d+)\s+(\d+)", lines[0])
                if total and "(TOTAL)" in lines[0]:
                    report["metadata"]["llvm_lines"] = {
                        "ir_lines": int(total[1]), "function_copies": int(total[2]),
                        "evidence": "llvm-lines-top.txt"}
        save(root, report)
        print(f"REPORT {root / 'report.html'}", flush=True)
        return
    if stage == "validate":
        validate_binaries(root, report)
        return
    if stage == "plan":
        nightly_plan(root, source)
        return
    if stage == "prepare":
        print(f"Prepared {source}", flush=True)
        return
    if stage in {"baseline", "cgu", "all"}:
        variant(root, report, source, "llvm-16", 16, opts.iterations)
    if stage in {"cgu", "all"}:
        for units in (256, 64):
            variant(root, report, source, f"llvm-{units}", units, opts.iterations)
    units = best_units(report)
    if stage in {"nightly", "all"}:
        for threads in (1, 2):
            args = '-Zprofile-rustflags '
            args += f"--config 'profile.dev.package.magician.rustflags=[\"-Zthreads={threads}\"]' "
            args += f"--config 'profile.dev.package.magician-api.rustflags=[\"-Zthreads={threads}\"]' "
            args += f"--config 'profile.dev.package.magician-bin.rustflags=[\"-Zthreads={threads}\"]'"
            variant(root, report, source, f"nightly-{threads}-thread" + ("s" if threads != 1 else ""),
                    units, opts.iterations, "nightly", args=args)
    if stage in {"cranelift", "all"} and cranelift_preflight(root, report, source):
        variant(root, report, source, "cranelift", units, opts.iterations, "nightly", "",
                "-Zprofile-rustflags -Zcodegen-backend "
                "--config 'profile.dev.package.magician.rustflags=[\"-Zthreads=1\"]' "
                "--config 'profile.dev.package.magician-api.rustflags=[\"-Zthreads=1\"]' "
                "--config 'profile.dev.package.magician-bin.rustflags=[\"-Zthreads=1\"]' "
                "--config 'profile.dev.package.magician.codegen-backend=\"cranelift\"' "
                "--config 'profile.dev.package.magician-api.codegen-backend=\"cranelift\"' "
                "--config 'profile.dev.package.magician-bin.codegen-backend=\"cranelift\"'")
    if stage in {"lld", "all"}:
        # Pass only the final binary's link flag, keeping dependency compilations unchanged.
        variant(root, report, source, "macho-lld", units, opts.iterations, args="--lld")
    if stage in {"api", "all"}:
        api_edits(root, report, source, opts.iterations)
    if stage in {"api-cgu", "all"}:
        api_codegen(root, report, source, opts.iterations)
    if stage in {"native", "all"}:
        native(root, report, source)
    if stage in {"llvm-lines", "all"}:
        llvm_lines(root, report, source)
    if stage == "all":
        validate_binaries(root, report)
    save(root, report)
    print(f"REPORT {root / 'report.html'}", flush=True)


if __name__ == "__main__":
    main()
