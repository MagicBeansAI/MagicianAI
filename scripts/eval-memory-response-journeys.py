#!/usr/bin/env python3
"""Exercise real-model deliveries across process exits, using only fixture copies."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    if args.output_dir.exists():
        parser.error("Use a fresh output directory to preserve previous evidence")
    args.output_dir.mkdir(parents=True)
    baseline = json.loads(args.report.read_text())
    selected = {}
    for row in baseline.get("cases", []):
        record = row.get("record") or {}
        if record.get("state") == "published" and row.get("automated_passed"):
            selected.setdefault(record["connection"]["surface"], row)
    rows = []
    for surface in ("hitl", "worth_a_look", "for_you"):
        if surface not in selected:
            rows.append({"surface": surface, "passed": False,
                         "inconclusive": "No matching real-model delivery for this surface"})
            continue
        row = selected[surface]
        for action in (("acknowledge", "remember", "dismiss", "stale") if surface == "hitl"
                       else ("dismiss", "stale")):
            root = args.output_dir/f"{surface}-{action}"
            shutil.copytree(Path(row["fixture_root"]), root)
            results = []
            for phase in ("answer", "reconcile"):
                command = [str(args.binary.resolve()), str(root.resolve()),
                           f"{row['id']}-{row['repeat']}", row["id"], action, phase]
                with (root/f"{phase}.log").open("w") as log:
                    result = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT)
                evidence = root/f"journey-{phase}.json"
                results.append({"phase": phase, "exit_code": result.returncode,
                                "report": json.loads(evidence.read_text()) if evidence.exists() else None,
                                "log": str(root/f"{phase}.log")})
                if result.returncode:
                    break
            rows.append({"surface": surface, "action": action, "original_case": row["id"],
                         "fixture_root": str(root), "phases": results,
                         "passed": len(results) == 2 and all(r["exit_code"] == 0 for r in results)})
    report = {"kind": "real_model_deliveries_with_process_restart", "source_report": str(args.report),
              "model_calls": 0, "journeys": rows,
              "passed": len(rows) == 8 and all(r["passed"] for r in rows),
              "ui_input": "service API; actual browser/device input remains a separate gate"}
    path = args.output_dir/"report.json"
    path.write_text(json.dumps(report, indent=2)+"\n")
    print(json.dumps({"report": str(path), "passed": report["passed"]}))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
