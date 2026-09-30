#!/usr/bin/env python3
"""Run the focused production worker and retain inspectable acceptance evidence."""
import argparse
import datetime as dt
import hashlib
import html
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def file_sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def summarize(report):
    cases = report.get("cases", [])
    groups = {}
    for row in cases:
        key = (row.get("partition", "unknown"), row.get("repeat", 0))
        group = groups.setdefault(key, {"positive": 0, "positive_matched": 0,
                                      "negative": 0, "negative_matched": 0,
                                      "critical_failures": 0, "unavailable": 0})
        checks = row.get("checks", {})
        negative = row.get("expected", {}).get("kind") == "silent"
        group["negative" if negative else "positive"] += 1
        if row.get("automated_passed") is True:
            group["negative_matched" if negative else "positive_matched"] += 1
        if checks.get("provider_ok") is not True:
            group["unavailable"] += 1
        if any(checks.get(k) is False for k in
               ("no_unrequested_memory_write", "cross_scope_clear", "delivery_ok")):
            group["critical_failures"] += 1
    rows = []
    for (partition, repeat), g in sorted(groups.items()):
        positive_rate = g["positive_matched"] / g["positive"] if g["positive"] else None
        negative_rate = g["negative_matched"] / g["negative"] if g["negative"] else None
        rows.append(dict(partition=partition, repeat=repeat, **g,
                         positive_match_rate=positive_rate, suppression_rate=negative_rate,
                         automated_gate_passed=(positive_rate is not None and positive_rate >= .8
                                                and negative_rate is not None and negative_rate >= .9
                                                and g["critical_failures"] == 0 and g["unavailable"] == 0)))
    complete = len(cases) == report.get("expected_cases") and not report.get("stop_reason")
    observations = [o for row in cases for o in row.get("observations", [])]
    latency = sorted(o["latency_ms"] for o in observations if o.get("latency_ms") is not None)
    costs = [o["router_estimated_cost_usd"] for o in observations if o.get("router_estimated_cost_usd") is not None]
    return {"complete": complete, "by_partition_and_repeat": rows,
            "observed_calls": len(observations), "costed_calls": len(costs),
            "router_estimated_cost_usd": sum(costs) if costs else None,
            "model_latency_median_ms": latency[len(latency)//2] if latency else None,
            "model_latency_p95_ms": latency[min(len(latency)-1,int(len(latency)*.95))] if latency else None,
            "automated_gates_passed": bool(rows) and complete and all(r["automated_gate_passed"] for r in rows),
            "usefulness_gate": "pending_independent_review", "owner_review": "pending",
            "note": "Scenario matching is not a human usefulness rating. These are finite-sample acceptance gates, not population guarantees."}


def render(report):
    esc = lambda x: html.escape(str(x))
    pretty = lambda x: esc(json.dumps(x, indent=2, ensure_ascii=False))
    parts = ["<!doctype html><meta charset='utf-8'><meta name='viewport' content='width=device-width'>",
             "<title>Memory connections evaluation</title><style>body{font:16px system-ui;max-width:1100px;margin:2rem auto;padding:0 1rem;color:#17202a}pre{white-space:pre-wrap;overflow-wrap:anywhere;background:#f4f6f8;padding:1rem}details{border:1px solid #ccd3db;border-radius:8px;margin:1rem 0;padding:1rem}summary{cursor:pointer;font-weight:650}.pass{color:#14643b}.fail{color:#a82922}small{color:#526170}h1{font-size:1.8rem}</style>",
             "<h1>Memory connections: real-model evaluation</h1>",
             "<p>Production recall, configured model, validation and delivery. Synthetic fixtures. Owner usefulness review is pending.</p>",
             "<h2>Acceptance summary</h2><pre>"+pretty(report.get("summary"))+"</pre>",
             "<p>Reported tokens: "+esc(report.get("reported_tokens", "unavailable"))+". Cost is a router estimate when available; unavailable usage is never treated as zero.</p>"]
    if report.get("stop_reason"):
        parts.append("<p class='fail'>Stopped: "+esc(report["stop_reason"])+"</p>")
    for row in report.get("cases", []):
        status = "MATCH" if row.get("automated_passed") else "FAIL / INCONCLUSIVE"
        parts.append("<details><summary class='"+("pass" if row.get("automated_passed") else "fail")+"'>"+
                     esc(f"{row['id']} · repeat {row['repeat']} · {status}")+"</summary>")
        for label, value in [("Expected",row.get("expected")),("Activity",row.get("activity")),
                             ("Checks",row.get("checks")),("Observed model call",row.get("observations")),
                             ("Published result",row.get("delivery")),("Error",row.get("error"))]:
            if value is not None: parts.append("<h3>"+label+"</h3><pre>"+pretty(value)+"</pre>")
        parts.append("<p><strong>Review:</strong> Is this useful now? Is it supported by the evidence? Is the chosen surface appropriate? Would silence be better?</p></details>")
    parts.append("<h2>Run provenance</h2><pre>"+pretty(report.get("provenance"))+"</pre>")
    return "\n".join(parts)


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--binary", required=True, type=Path)
    p.add_argument("--config", default="")
    p.add_argument("--suite", type=Path, default=ROOT/"scripts/fixtures/memory_connections/scenarios.json")
    p.add_argument("--source-root", type=Path, default=ROOT)
    p.add_argument("--output-dir", type=Path, required=True)
    p.add_argument("--partition", choices=["smoke","development","validation","heldout","all"], default="smoke")
    p.add_argument("--repeats", type=int, default=1)
    p.add_argument("--max-calls", type=int, default=12)
    p.add_argument("--max-reported-tokens", type=int, default=100000)
    args = p.parse_args()
    config = Path(args.config or os.environ.get("MAGICIAN_CONFIG_PATH") or
                  Path(os.environ.get("MAGICIAN_ROOT_DIR", str(Path.home()/"MagicianNotes")))/"magician-config.yaml")
    if not args.binary.is_file() or not config.is_file():
        p.error("The built evaluator and runtime config must both exist")
    run_id = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
    output = args.output_dir/run_id
    args.output_dir.mkdir(parents=True, exist_ok=True)
    command = [str(args.binary.resolve()), "--config", str(config.resolve()), "--suite", str(args.suite.resolve()),
               "--output-dir", str(output.resolve()), "--partition", args.partition, "--repeats", str(args.repeats),
               "--max-calls", str(args.max_calls), "--max-reported-tokens", str(args.max_reported_tokens)]
    binary_sha256 = file_sha256(args.binary)
    with (args.output_dir/(run_id+".log")).open("w") as log:
        result = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT)
    path = output/"report.json"
    if path.exists():
        report = json.loads(path.read_text())
    else:
        output.mkdir(exist_ok=True)
        report = {"cases":[],"expected_cases":None,"stop_reason":"Evaluator did not produce case evidence; inspect run log"}
    source_files = ["magician-comms/examples/memory_connections_live_eval.rs",
                    "magician-comms/src/channel_assist/resurfacing/memory_connections.rs",
                    "magician/src/magician_v2/attention/resurfacing/memory_connections.rs",
                    "magician/src/magician_v2/agents/memory_prompt_blocks.rs"]
    report["provenance"] = {"run_id":run_id,"binary":str(args.binary.resolve()),"binary_sha256":binary_sha256,"process_exit_code":result.returncode,
                            "source_root":str(args.source_root.resolve()),
                            "source_sha256":{name:hashlib.sha256((args.source_root/name).read_bytes()).hexdigest() for name in source_files},
                            "suite_sha256":hashlib.sha256(args.suite.read_bytes()).hexdigest(),
                            "run_log":str(args.output_dir/(run_id+".log"))}
    report["summary"] = summarize(report)
    path.write_text(json.dumps(report, indent=2)+"\n")
    (output/"report.html").write_text(render(report))
    latest = args.output_dir/"latest"
    latest.mkdir(exist_ok=True)
    for name in ["report.json","report.html"]:
        shutil.copyfile(output/name, latest/name)
    print(json.dumps({"report":str(output/"report.html"),"summary":report["summary"]},indent=2))
    return 0 if report["summary"]["automated_gates_passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
