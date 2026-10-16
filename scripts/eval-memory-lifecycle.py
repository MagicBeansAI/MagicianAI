#!/usr/bin/env python3
"""Run lifecycle fixtures with immutable reports for the existing Evals page."""
import argparse
import copy
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time
import uuid

import yaml
from magician_config_text import read_config_text
from memory_lifecycle_report import render, summarize
from write_eval_harness_report import run_command, build_payload, write_reports, utc_now

ROOT = Path(__file__).resolve().parents[1]
OPERATIONS = ("memory_lifecycle_review", "memory_connection_review", "memory_user_promotion")
FIXTURES = ("cases.json", "help_journeys.json", "capture_journey.json", "clarification_journey.json")
TOKEN = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}\Z")


def sha(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as handle:
        for block in iter(lambda: handle.read(1024*1024), b""): digest.update(block)
    return digest.hexdigest()


def private_write(path, content):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with path.open("x") as handle:
        os.chmod(path, 0o600)
        handle.write(content)


def snapshot_configs(config, profiles, private_root):
    """Preflight every profile before any call; snapshot Settings overrides too."""
    text = read_config_text(config)
    parsed = yaml.safe_load(text)
    router = parsed["llm"]["router"]
    for operation in OPERATIONS:
        if operation not in router["operation_mapping"]:
            raise ValueError(f"Required operation is unbound: {operation}")
    for profile in profiles:
        if profile not in router["profiles"]:
            raise ValueError(f"Unknown configured profile: {profile}")
    override_path = config.parent / "system/llm_routing_overrides.json"
    overrides = json.loads(override_path.read_text()) if override_path.exists() else {}
    if not isinstance(overrides, dict): raise ValueError("Invalid runtime routing overrides")
    snapshots = []
    for i, profile in enumerate(profiles or [None]):
        directory = private_root / str(i)
        chosen = copy.deepcopy(overrides)
        if profile:
            chosen.update({operation: profile for operation in OPERATIONS})
        destination = directory / "magician-config.yaml"
        private_write(destination, text)
        private_write(directory / "system/llm_routing_overrides.json", json.dumps(chosen))
        for name in (".env", ".env.development"):
            source = config.parent / name
            if source.is_file(): (directory / name).symlink_to(source.resolve())
        snapshots.append((profile or "configured routes", destination))
    return snapshots


def execute(args):
    run_id = args.run_id or (dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%S")+"-"+uuid.uuid4().hex[:12])
    if not TOKEN.fullmatch(run_id): raise ValueError("Invalid run id")
    output = args.output_dir.resolve() / "runs" / run_id
    output.mkdir(parents=True, exist_ok=False)
    started = utc_now()
    if args.mode == "harness":
        command = ["make", "--no-print-directory", "test-memory-lifecycle-unit"]
        code, log, duration = run_command(command)
        payload = build_payload(title="Memory lifecycle focused tests", command=command, exit_code=code,
                                duration_s=duration, output=log, started_at=started)
        write_reports(output, payload, log)
        print(f"Memory lifecycle report: {output / 'report.html'}")
        return code

    report = dict(run_id=run_id, status="running", started_at=started.isoformat(), runs=[],
                  repeats=args.repeats, partition=args.partition, profiles=args.profiles,
                  grading="All expected journeys must pass; percentage thresholds do not erase failures.")
    log = ""
    code = 1
    try:
        config = Path(args.config or os.environ.get("MAGICIAN_CONFIG_PATH") or
                      Path(os.environ.get("MAGICIAN_ROOT_DIR", str(Path.home()/"MagicianNotes"))) / "magician-config.yaml").resolve()
        private_root = Path(tempfile.mkdtemp(prefix="memory-lifecycle-config-"))
        snapshots = snapshot_configs(config, args.profiles, private_root)
        report["config_sha256"] = {label: {"config": sha(path), "overrides": sha(path.parent/"system/llm_routing_overrides.json")} for label, path in snapshots}
        report["fixture_sha256"] = {name: sha(args.suite.with_name(name)) for name in FIXTURES}
        if not args.skip_build:
            code, build_log, _ = run_command(["make", "--no-print-directory", "build-memory-lifecycle-eval"])
            log += build_log
            if code: raise RuntimeError(f"Evaluator build failed with exit code {code}")
        report["binary_sha256"] = sha(args.binary)
        for i, (profile, config_copy) in enumerate(snapshots):
            raw = output / f"profile-{i}"
            command = [str(args.binary.resolve()), "--config", str(config_copy), "--suite", str(args.suite.resolve()),
                       "--partition", args.partition, "--repeats", str(args.repeats), "--output-dir", str(raw)]
            model_start = time.monotonic()
            try:
                result = subprocess.run(command, capture_output=True, text=True, timeout=args.timeout)
            except subprocess.TimeoutExpired as error:
                text = lambda value: value.decode(errors="replace") if isinstance(value, bytes) else value or ""
                result = subprocess.CompletedProcess(command, 124, text(error.stdout), text(error.stderr)+"\nEvaluator timed out; partial evidence is inconclusive.")
            log += f"\nProfile: {profile}\n"+result.stdout+result.stderr
            (output / "output.txt").write_text(log)
            path = raw / "report.json"
            evidence = json.loads(path.read_text()) if path.is_file() else {"cases": []}
            summary = summarize(evidence, raw)
            expected_core = sum(args.partition == "all" or c["partition"] == args.partition for c in json.loads(args.suite.read_text()))
            expected = (expected_core + 2) * args.repeats
            complete = result.returncode in (0, 1) and summary["journeys"]["total"] == expected
            summary["cost_complete"] = summary["cost_complete"] and complete
            passed = result.returncode == 0 and summary["journeys"] == {"passed": expected, "total": expected}
            report["runs"].append(dict(profile=profile, exit_code=result.returncode, complete=complete, passed=passed,
                                       duration_seconds=round(time.monotonic()-model_start, 3), summary=summary,
                                       evidence=f"profile-{i}/report.json" if path.exists() else "output.txt"))
            publish(output, report, log)
        code = 0 if report["runs"] and all(r["passed"] for r in report["runs"]) else 1
        report["status"] = "passed" if code == 0 else "failed"
    except Exception as error:
        code = 1
        report["status"] = "failed"
        report["error"] = str(error)
        log += "\n"+str(error)+"\n"
    finally:
        report["finished_at"] = utc_now().isoformat()
        publish(output, report, log)
    print(json.dumps({"report": str(output / "report.html"), "status": report["status"],
                      "runs": [{"profile":r["profile"], "journeys":r["summary"]["journeys"]} for r in report["runs"]]}))
    return code


def publish(output, report, log):
    # Only this run writes these paths. Readers see either complete revision.
    for name, text in [("report.json", json.dumps(report, indent=2)+"\n"),
                       ("report.html", render(report)), ("output.txt", log)]:
        temporary = output / (name+".tmp")
        temporary.write_text(text)
        temporary.replace(output / name)


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--mode", choices=["harness", "live"], default="live")
    p.add_argument("--binary", type=Path, default=Path(os.environ.get("CARGO_TARGET_DIR", "target"))/"debug/examples/memory_lifecycle_eval")
    p.add_argument("--config", default="")
    p.add_argument("--suite", type=Path, default=ROOT/"scripts/fixtures/memory_lifecycle/cases.json")
    p.add_argument("--output-dir", type=Path, required=True)
    p.add_argument("--run-id", default="")
    p.add_argument("--profiles", default="", help="Comma-separated configured profiles; empty uses existing operation routes")
    p.add_argument("--repeats", type=int, choices=[1,2,3], default=3)
    p.add_argument("--partition", choices=["all", "development", "validation"], default="all")
    p.add_argument("--skip-build", action="store_true")
    p.add_argument("--timeout", type=int, default=1800)
    args = p.parse_args(argv)
    if args.timeout < 1: p.error("timeout must be positive")
    args.profiles = args.profiles.split(",") if args.profiles else []
    if len(args.profiles)>3 or len(set(args.profiles)) != len(args.profiles) or any(not TOKEN.fullmatch(n) for n in args.profiles):
        p.error("Use at most three distinct configured profile names")
    return execute(args)


if __name__ == "__main__":
    sys.exit(main())
