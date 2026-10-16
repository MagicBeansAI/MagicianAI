#!/usr/bin/env python3
"""Build an acceptance index without changing the frozen scenario scores."""
import argparse
import html
import json
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--run-root", type=Path, required=True)
    args = parser.parse_args()
    base = args.run_root
    partitions = []
    reviews = []
    for name in ("development", "validation", "heldout"):
        path = base/f"results-{name}/latest/report.json"
        report = json.loads(path.read_text())
        partitions.append({"partition": name, "report": str(path), **report["summary"]})
        for row in report["cases"]:
            reviews.append({"id": row["id"], "repeat": row["repeat"], "partition": name,
                            "automated_passed": row["automated_passed"],
                            "useful_now": None, "supported": None, "surface_appropriate": None,
                            "prefer_silence": None, "owner_notes": ""})
    model_cost = 0.0
    tokens = 0
    calls = 0
    costed_calls = 0
    for folder in ("results", "results-development", "results-validation", "results-heldout"):
        for path in (base/folder).glob("*/report.json"):
            if path.parent.name == "latest":
                continue
            report = json.loads(path.read_text())
            for row in report.get("cases", []):
                for observation in row.get("observations", []):
                    if observation.get("raw_response") is None:
                        continue
                    calls += 1
                    tokens += (observation.get("usage") or {}).get("total_tokens", 0)
                    cost = observation.get("router_estimated_cost_usd")
                    if cost is not None:
                        costed_calls += 1
                        model_cost += cost
    captures = []
    for name in ("capture-before-handoff", "capture-after-handoff"):
        report = json.loads((base/name/"report.json").read_text())
        captures.append({"stage": name, "checks": report["checks"], "report": str(base/name/"report.json")})
        for usage in report.get("capture_usage", []):
            calls += 1
            tokens += usage.get("input_tokens", 0)+usage.get("output_tokens", 0)
            if usage.get("router_estimated_cost_usd") is not None:
                costed_calls += 1
                model_cost += usage["router_estimated_cost_usd"]
        for observation in report.get("connection_observations", []):
            if observation.get("response") is None:
                continue
            calls += 1
            tokens += (observation.get("usage") or {}).get("total_tokens", 0)
            if observation.get("cost_usd") is not None:
                costed_calls += 1
                model_cost += observation["cost_usd"]
    summary = {"verdict": "not_fully_accepted", "partitions": partitions, "capture": captures,
               "response_journeys": json.loads((base/"response-journeys/report.json").read_text()),
               "runtime_tests": json.loads((base/"runtime-test-status.json").read_text()),
               "ui_component_tests": json.loads((base/"ui-status.json").read_text()),
               "responded_model_calls": calls, "costed_calls": costed_calls, "reported_tokens": tokens,
               "router_estimated_cost_usd": model_cost,
               "pending": ["native iOS/Android and live browser/Tauri verification", "background capture scheduling journey", "multi-day owner usefulness review"],
               "dataset_caveat": "career-quiet retains a Portfolio deadline title beside an unrelated seminar body. Its frozen score is retained; this ambiguous fixture needs a versioned replacement before the next study.",
               "next_study": "Do not tune and claim the same held-out set is unseen. Keep it as regression evidence and freeze a fresh held-out set."}
    (base/"acceptance-summary.json").write_text(json.dumps(summary, indent=2)+"\n")
    template = base/"owner-review-template.json"
    if not template.exists():
        template.write_text(json.dumps({"reviewer": None, "cases": reviews}, indent=2)+"\n")
    esc = html.escape
    parts = ["<!doctype html><meta charset='utf-8'><meta name='viewport' content='width=device-width'>",
             "<title>Memory connections acceptance</title><style>body{font:16px system-ui;max-width:1000px;margin:3rem auto;padding:0 1rem;color:#19242d}table{border-collapse:collapse;width:100%}th,td{padding:.7rem;text-align:left;border-bottom:1px solid #ddd}a{color:#185bb5}strong{color:#942c25}pre{white-space:pre-wrap;overflow-wrap:anywhere;background:#f3f5f7;padding:1rem}</style>",
             "<h1>Memory connections: acceptance evidence</h1><p><strong>Not fully accepted.</strong> Inspect the per-repeat gates below. Owner and device review remain open.</p>",
             "<p>60 frozen scenarios × 3 repeats; actual configured Luna calls, production recall, validation and delivery.</p>",
             "<table><tr><th>Partition / repeat</th><th>Positive matches</th><th>Correct silence</th><th>Gate</th></tr>"]
    for partition in partitions:
        for row in partition["by_partition_and_repeat"]:
            parts.append(f"<tr><td>{esc(partition['partition'])} / {row['repeat']+1}</td><td>{row['positive_matched']}/{row['positive']}</td><td>{row['negative_matched']}/{row['negative']}</td><td>{'PASS' if row['automated_gate_passed'] else 'FAIL'}</td></tr>")
    parts.append("</table><p>Gates apply per repeat: ≥80% positive matches, ≥90% correct silence, zero observed critical failures.</p><ul>")
    for name in ("development", "validation", "heldout"):
        parts.append(f"<li><a href='results-{name}/latest/report.html'>{name.title()}: inspect every response and source</a></li>")
    parts += ["<li><a href='response-journeys/report.json'>Restart and response journey results</a></li>",
              "<li><a href='capture-before-handoff/report.json'>Capture baseline</a> · <a href='capture-after-handoff/report.json'>Capture after the fix</a></li>",
              "<li><a href='runtime-tests.log'>Focused runtime tests</a> · <a href='ui-component-tests.log'>Focused component tests (HTTP mocked)</a></li></ul>",
              f"<p>Including baseline and capture: {calls} responded calls, {tokens:,} reported tokens, ${model_cost:.5f} router-estimated cost. This is not an invoice.</p>",
              "<h2>Limits and next review</h2><p>"+esc(summary["dataset_caveat"])+"</p><p>"+esc(summary["next_study"])+"</p>",
              "<p>No main Magician service was running at local port 3002. Component success does not establish device layout or a live UI round trip. Capture was service-driven explicitly; scheduling and multi-day owner usefulness still need observations.</p>",
              "<p><a href='owner-review-template.json'>Owner review template</a> · <a href='acceptance-summary.json'>Complete machine-readable summary</a></p>"]
    (base/"acceptance-summary.html").write_text("\n".join(parts))
    print(base/"acceptance-summary.html")


if __name__ == "__main__":
    main()
