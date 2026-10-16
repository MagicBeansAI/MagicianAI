"""Summarize retained lifecycle evidence without changing its grading."""
from collections import Counter
import html
import json
import math
from pathlib import Path


def summarize(report, raw_root):
    cases = report.get("cases", [])
    events = [e for c in cases for e in c.get("events", [])]
    help_rows = [c["downstream_help"] for c in cases if c.get("downstream_help")]
    captures = [c["capture"] for c in cases if c.get("capture")]
    clarifications = [c["clarification"] for c in cases if c.get("clarification")]
    calls = []

    def observe(o):
        usage = o.get("usage") or {}
        calls.append(dict(model=o.get("model"), provider=o.get("provider"),
                          input=usage.get("input_tokens"), output=usage.get("output_tokens"),
                          cost=o.get("router_estimated_cost_usd"), reported=bool(usage)))

    for event in events:
        for o in event.get("observations", []): observe(o)
    for row in help_rows:
        for o in row.get("observations", []):
            t = o.get("telemetry") or {}
            calls.append(dict(model=t.get("model"), provider=t.get("provider"),
                              input=t.get("input_tokens"), output=t.get("output_tokens"),
                              cost=t.get("router_estimated_cost_usd"), reported=t.get("usage_reported") is True))
    for capture in captures:
        for stage in capture.get("stages", []):
            for o in stage.get("observations", []): observe(o)
        for u in capture.get("promotion_usage", []):
            calls.append(dict(model=u.get("model"), provider=u.get("provider"),
                              input=u.get("input_tokens"), output=u.get("output_tokens"),
                              cost=u.get("cost_usd"), reported=u.get("reported") is True))
    for row in cases:
        clarification = row.get("clarification")
        if not clarification: continue
        observations = clarification.get("observations", [])
        if not observations and clarification.get("error"):
            # An initial failure may precede construction of the full journey.
            # Resolve only a known child of this report, never its absolute root field.
            sidecar = Path(raw_root) / f"free-text-clarification-{row['repeat']}" / "failed-initial-review.json"
            if sidecar.is_file():
                observations = json.loads(sidecar.read_text()).get("observations", [])
        for o in observations: observe(o)

    def score(rows):
        return dict(passed=sum(r.get("passed") is True for r in rows), total=len(rows))

    def valid_number(v):
        return isinstance(v, (float, int)) and not isinstance(v, bool) and math.isfinite(v) and v >= 0

    costs = [c["cost"] for c in calls if c["reported"] and valid_number(c["cost"])]
    known_tokens = all(c["reported"] and valid_number(c["input"]) and valid_number(c["output"]) for c in calls)
    return dict(journeys=score(cases), steps=score(events), downstream=score(help_rows),
                capture=score(captures), clarification=score(clarifications),
                repeats={str(i+1): score([c for c in cases if c.get("repeat") == i])
                         for i in sorted({c["repeat"] for c in cases})},
                failures=[dict(id=c["id"], repeat=c["repeat"]+1) for c in cases if c.get("passed") is not True],
                models=dict(Counter(c["model"] or "unreported" for c in calls)),
                providers=dict(Counter(c["provider"] or "unreported" for c in calls)),
                calls=len(calls), usage_reported=sum(c["reported"] for c in calls),
                input_tokens=sum(c["input"] for c in calls) if calls and known_tokens else None,
                output_tokens=sum(c["output"] for c in calls) if calls and known_tokens else None,
                costed_calls=len(costs), router_estimated_cost_usd=sum(costs) if costs else None,
                cost_complete=bool(calls) and len(costs) == len(calls))


def render(report):
    esc = lambda value: html.escape(str(value))
    pretty = lambda value: esc(json.dumps(value, indent=2, ensure_ascii=False))
    parts = ["<!doctype html><html lang='en'><meta charset='utf-8'><meta name='viewport' content='width=device-width'>",
             "<title>Memory lifecycle evaluation</title><style>body{font:16px/1.5 system-ui;max-width:1050px;margin:2rem auto;padding:0 1rem;color:#18222d}table{border-collapse:collapse;width:100%}td,th{padding:.7rem;text-align:left;border-bottom:1px solid #ccd3dc}pre{white-space:pre-wrap;overflow-wrap:anywhere;background:#f3f5f7;padding:1rem}details{margin:1rem 0}summary{cursor:pointer}a{color:#164bad}</style>",
             "<h1>Memory lifecycle</h1><p>Status: <strong>"+esc(report["status"])+"</strong></p>",
             "<p>Frozen synthetic journeys through production memory and owner-answer services. Every failed journey remains a failure. Costs below are evaluator estimates; the Evals task ledger does not include this separate process.</p>"]
    if report.get("error"): parts.append("<pre>"+esc(report["error"])+"</pre>")
    parts.append("<p><a href='report.json'>Run data</a> · <a href='output.txt'>Run log</a></p>")
    for row in report.get("runs", []):
        s = row["summary"]
        score = s["journeys"]
        cost = s["router_estimated_cost_usd"]
        cost_text = "unavailable" if cost is None else ("" if s["cost_complete"] else "at least ")+f"${cost:.6f}"
        parts += ["<h2>"+esc(row["profile"])+"</h2>",
                  f"<p>{score['passed']}/{score['total']} journeys passed · estimated model cost {esc(cost_text)} · {esc(row['duration_seconds'])} seconds</p>",
                  "<p><a href='"+esc(row["evidence"])+"'>Raw journey evidence</a></p>",
                  "<details><summary>Scores, models, usage and failed cases</summary><pre>"+pretty(s)+"</pre></details>"]
    parts.append("<details><summary>Run configuration and provenance</summary><pre>"+pretty({k:v for k,v in report.items() if k != "runs"})+"</pre></details></html>")
    return "\n".join(parts)
