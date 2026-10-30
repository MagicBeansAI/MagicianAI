#!/usr/bin/env python3
"""Audit both committed Rust lockfiles without regenerating or ignoring findings."""
import json
from pathlib import Path
import shlex
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
LOCKFILES = ("Cargo.lock", "desktop/src-tauri/Cargo.lock")


def audit_lockfile(lockfile):
    args = shlex.join(["audit", "--json", "--file", lockfile])
    result = subprocess.run(["make", "--silent", "--no-print-directory", "cargo", "ARGS=" + args],
                            cwd=ROOT, capture_output=True, text=True)
    sys.stderr.write(result.stderr)
    try:
        report = json.loads(result.stdout)
        findings = report["vulnerabilities"]["list"]
        if not isinstance(findings, list) or report["vulnerabilities"]["count"] != len(findings):
            raise ValueError("Invalid audit finding list")
        if result.returncode and not findings:
            raise ValueError("Audit failed without a vulnerability report")
    except (ValueError, KeyError, TypeError) as error:
        raise ValueError(f"Could not audit {lockfile}: {error}\n{result.stdout}") from error
    warnings = {key: len(items) for key, items in report.get("warnings", {}).items()}
    print(f"{lockfile}: {len(findings)} vulnerability findings; warnings: {warnings}")
    return findings


def main():
    findings = [finding for lockfile in LOCKFILES for finding in audit_lockfile(lockfile)]
    for finding in findings:
        print(f"BLOCKED: {finding['advisory']['id']} {finding['package']['name']} "
              f"{finding['package']['version']}: {finding['advisory']['title']}")
    print(f"Rust audit: {len(findings)} blocking findings")
    return bool(findings)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, OSError, KeyError, TypeError) as error:
        print(f"Rust audit failed: {error}", file=sys.stderr)
        sys.exit(2)
