#!/usr/bin/env python3
"""Reject new workspace Clippy warnings without refactoring legacy APIs for CI."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
BASELINE = ROOT / "scripts/clippy_baseline.json"
REPORT = ROOT / ".cache/ci/clippy-diagnostics.jsonl"
FIELDS = ("code", "file", "message", "source_sha256")


def workspace_span(span):
    while True:
        file = span["file_name"].replace("\\", "/")
        if not file.startswith("<"):
            try:
                return span, (ROOT / file).resolve().relative_to(ROOT).as_posix()
            except ValueError:
                pass
        # Standard-library/procedural macro diagnostics can point into the
        # toolchain. Attribute them to the workspace expansion call site.
        expansion = span.get("expansion")
        if not expansion:
            raise ValueError("Cannot locate a Clippy warning in the workspace")
        span = expansion["span"]


def finding(message):
    spans = [span for span in message["spans"] if span["is_primary"]]
    if not spans or not message.get("code"):
        raise ValueError("Cannot identify a Clippy warning; refusing to baseline it")
    located = [workspace_span(span) for span in spans]
    file = located[0][1]
    source = [line["text"].strip() for span, _ in located for line in span["text"]]
    return {
        "code": message["code"]["code"],
        "file": file,
        "message": message["message"],
        "source_sha256": hashlib.sha256("\n".join(source).encode()).hexdigest(),
    }


def counts(items):
    result = Counter()
    for item in items:
        if set(item) != {*FIELDS, "count"} or not isinstance(item["count"], int) or item["count"] < 1:
            raise ValueError("Invalid Clippy baseline entry")
        key = tuple(item[field] for field in FIELDS)
        if key in result:
            raise ValueError("Duplicate Clippy baseline entry")
        result[key] = item["count"]
    return result


def new_findings(actual, baseline):
    return actual - baseline


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--refresh", action="store_true", help="Explicitly replace the reviewed warning baseline")
    args = parser.parse_args()
    REPORT.parent.mkdir(parents=True, exist_ok=True)
    command = ["make", "--silent", "--no-print-directory", "cargo",
               "ARGS=clippy --workspace --all-targets --locked --message-format=json"]
    actual = Counter()
    invalid = []
    with REPORT.open("w") as report:
        process = subprocess.Popen(command, cwd=ROOT, stdout=subprocess.PIPE, text=True)
        for line in process.stdout:
            report.write(line)
            try:
                event = json.loads(line)
            except ValueError:
                # Tool/router progress can share stdout; it is not a diagnostic.
                print(line, end="")
                continue
            if event.get("reason") != "compiler-message":
                continue
            message = event["message"]
            if message["level"] == "warning":
                try:
                    identified = finding(message)
                    actual[tuple(identified[field] for field in FIELDS)] += 1
                except ValueError as error:
                    invalid.append(str(error))
            elif message["level"] == "error":
                print(message.get("rendered", message["message"]), file=sys.stderr)
        code = process.wait()
    if code or invalid:
        print(f"Clippy failed (exit {code}); baseline was not updated. {invalid}", file=sys.stderr)
        return 1
    if args.refresh:
        entries = [dict(zip(FIELDS, key), count=count) for key, count in sorted(actual.items())]
        BASELINE.write_text('{\n  "rust": "1.95.0",\n  "findings": [\n    '
                            + ',\n    '.join(json.dumps(entry) for entry in entries) + '\n  ]\n}\n')
        print(f"Recorded {sum(actual.values())} warnings for review in {BASELINE.relative_to(ROOT)}")
        return 0
    policy = json.loads(BASELINE.read_text())
    if policy["rust"] != "1.95.0":
        raise ValueError("Review the Clippy baseline when changing the CI Rust toolchain")
    new = new_findings(actual, counts(policy["findings"]))
    # Keep existing debt visible, grouped by source file and lint.
    grouped = Counter()
    for key, count in actual.items():
        grouped[(key[1], key[0])] += count
    for (file, lint), count in sorted(grouped.items()):
        print(f"Clippy: {file}: {lint} ({count})")
    for key, count in sorted(new.items()):
        print("NEW WARNING: " + json.dumps(dict(zip(FIELDS, key), count=count)), file=sys.stderr)
    print(f"Workspace Clippy: {sum(actual.values())} existing warnings; {sum(new.values())} new warnings. "
          f"Full diagnostics: {REPORT.relative_to(ROOT)}")
    return bool(new)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"Clippy baseline check failed: {error}", file=sys.stderr)
        sys.exit(2)
