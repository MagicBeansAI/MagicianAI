#!/usr/bin/env python3
"""Run a command and write report.json + report.html for a Makefile eval lane.

Used by cargo-test / bash eval targets that do not have a dedicated evaluator.
Reports are always written (including on failure) so `/evals` can link a
result; the process then exits with the wrapped command's status.
"""

from __future__ import annotations

import argparse
import html
import json
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


OUTPUT_TAIL_LINES = 200


def utc_now() -> datetime:
    return datetime.now(timezone.utc)


def command_display(command: list[str]) -> str:
    return " ".join(command)


def output_tail(text: str, limit: int = OUTPUT_TAIL_LINES) -> str:
    lines = text.splitlines()
    if len(lines) <= limit:
        return text.rstrip()
    omitted = len(lines) - limit
    return "\n".join([f"… {omitted} earlier lines omitted …", *lines[-limit:]]).rstrip()


def run_command(command: list[str]) -> tuple[int, str, float]:
    started = time.perf_counter()
    try:
        proc = subprocess.Popen(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )
    except OSError as error:
        return 127, f"failed to start {command_display(command)}: {error}\n", time.perf_counter() - started
    chunks: list[str] = []
    try:
        assert proc.stdout is not None
        for line in proc.stdout:
            sys.stdout.write(line)
            sys.stdout.flush()
            chunks.append(line)
        code = proc.wait()
    finally:
        if proc.stdout is not None:
            proc.stdout.close()
        if proc.poll() is None:
            proc.wait()
    return code if code is not None else 1, "".join(chunks), time.perf_counter() - started


def build_payload(
    *,
    title: str,
    command: list[str],
    exit_code: int,
    duration_s: float,
    output: str,
    started_at: datetime,
) -> dict[str, Any]:
    status = "passed" if exit_code == 0 else "failed"
    return {
        "title": title,
        "status": status,
        "exit_code": exit_code,
        "duration_ms": int(round(duration_s * 1000)),
        "command": command,
        "started_at": started_at.isoformat(),
        "finished_at": utc_now().isoformat(),
        "output_tail": output_tail(output),
    }


def render_html(payload: dict[str, Any]) -> str:
    status = str(payload.get("status") or "failed")
    color = "#0f7b3a" if status == "passed" else "#b42318"
    output = str(payload.get("output_tail") or "")
    command = payload.get("command") or []
    command_text = command_display(command) if isinstance(command, list) else str(command)
    return f"""<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>{html.escape(str(payload.get("title") or "Eval harness"))}</title>
  <style>
    body {{ font: 14px/1.45 ui-sans-serif, system-ui, sans-serif; margin: 2rem; color: #111; }}
    .status {{ color: {color}; font-weight: 700; text-transform: uppercase; }}
    pre {{ background: #f6f6f4; padding: 1rem; overflow: auto; white-space: pre-wrap; }}
    dt {{ font-weight: 600; }}
    dd {{ margin: 0 0 0.75rem 0; }}
  </style>
</head>
<body>
  <h1>{html.escape(str(payload.get("title") or "Eval harness"))}</h1>
  <p class="status">{html.escape(status)}</p>
  <dl>
    <dt>Exit code</dt><dd>{html.escape(str(payload.get("exit_code")))}</dd>
    <dt>Duration</dt><dd>{html.escape(str(payload.get("duration_ms")))} ms</dd>
    <dt>Command</dt><dd><code>{html.escape(command_text)}</code></dd>
    <dt>Started</dt><dd>{html.escape(str(payload.get("started_at") or ""))}</dd>
    <dt>Finished</dt><dd>{html.escape(str(payload.get("finished_at") or ""))}</dd>
  </dl>
  <h2>Output tail</h2>
  <pre>{html.escape(output)}</pre>
</body>
</html>
"""


def write_reports(output_dir: Path, payload: dict[str, Any], output: str) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    (output_dir / "output.txt").write_text(output, encoding="utf-8")
    (output_dir / "report.json").write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    (output_dir / "report.html").write_text(render_html(payload), encoding="utf-8")


def run_and_report(*, title: str, output_dir: Path, command: list[str]) -> int:
    if not command:
        raise ValueError("command is required")
    started_at = utc_now()
    exit_code, output, duration_s = run_command(command)
    payload = build_payload(
        title=title,
        command=command,
        exit_code=exit_code,
        duration_s=duration_s,
        output=output,
        started_at=started_at,
    )
    write_reports(output_dir, payload, output)
    print(f"Eval harness report: {output_dir / 'report.html'}")
    return exit_code


def self_test() -> int:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        ok_dir = root / "ok"
        fail_dir = root / "fail"
        ok_code = run_and_report(
            title="ok harness",
            output_dir=ok_dir,
            command=[sys.executable, "-c", "print('ok')"],
        )
        if ok_code != 0:
            raise SystemExit(f"self-test expected 0, got {ok_code}")
        for name in ("report.html", "report.json", "output.txt"):
            if not (ok_dir / name).is_file():
                raise SystemExit(f"self-test missing {ok_dir / name}")
        ok_payload = json.loads((ok_dir / "report.json").read_text(encoding="utf-8"))
        if ok_payload.get("status") != "passed":
            raise SystemExit(f"self-test expected passed, got {ok_payload}")
        fail_code = run_and_report(
            title="fail harness",
            output_dir=fail_dir,
            command=[sys.executable, "-c", "raise SystemExit(3)"],
        )
        if fail_code != 3:
            raise SystemExit(f"self-test expected 3, got {fail_code}")
        fail_payload = json.loads((fail_dir / "report.json").read_text(encoding="utf-8"))
        if fail_payload.get("status") != "failed" or fail_payload.get("exit_code") != 3:
            raise SystemExit(f"self-test expected failed/3, got {fail_payload}")
        if "fail harness" not in (fail_dir / "report.html").read_text(encoding="utf-8"):
            raise SystemExit("self-test HTML missing title")
    print("write_eval_harness_report self-test passed")
    return 0


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--title", default="Eval harness")
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if args.self_test:
        return self_test()
    command = list(args.command)
    if command and command[0] == "--":
        command = command[1:]
    if args.output_dir is None:
        print("write_eval_harness_report.py: --output-dir is required", file=sys.stderr)
        return 2
    if not command:
        print("write_eval_harness_report.py: command is required after --", file=sys.stderr)
        return 2
    return run_and_report(title=args.title, output_dir=args.output_dir, command=command)


if __name__ == "__main__":
    raise SystemExit(main())
