#!/usr/bin/env python3
"""Live semantic eval for the Thinking Map interpreter (plan Phase 3, item 8).

Runs fixture utterances against the RUNNING magician server's real
`/thinking-maps/{id}/interpret` (live LLM), scoring the Phase-3 measures:

  - operation-envelope validity (interpret returns applied/no_operations);
  - expected node precision & recall (keyword match over produced labels);
  - correction target accuracy (a correction must REFERENCE the existing
    target node — update/state/edge/supersede — not spawn an orphan);
  - duplicate-node / avoidable-churn rate (near-dup labels vs the board);
  - unsupported assertion rate (nodes sharing no content words with the
    utterance or board = hallucinated content);
  - authority violation rate (interpreter output must stay model_inferred +
    provisional; owner-only effects like title changes are violations);
  - clarification usefulness/restraint (none on plain statements; allowed on
    the deliberately ambiguous case);
  - latency per call (context tokens are not exposed on the wire; the
    interpreter's own bounded `build_context` is covered by Rust unit tests);
  - N-repeat stability on the release-critical subset (default 5).

SHADOW-ONLY by design: every case runs on a fresh scratch map (uuid id,
`eval-tm-` title prefix) which is soft-deleted (lifecycle=deleted) afterwards —
nothing is persisted into user maps.

Exit gates (non-zero exit on failure):
  - correction target accuracy >= 0.95 (release gate from the plan);
  - authority violations == 0;
  - envelope validity >= 0.95.

Reports: coverage/evals/thinking-map/latest.{html,json} (+ timestamped copy).

Cost bounds: one interpret call per case-run. Default = 8 cases + 3
release-critical cases x (TM_EVAL_RUNS-1) extra repeats = 8 + 3*4 = 20 calls
of gpt-5.6-terra-class traffic per invocation. Override TM_EVAL_RUNS=1 for a
minimal 8-call smoke.

Env:
  MAGICIAN_URL   (default http://127.0.0.1:3002)
  TM_EVAL_RUNS   repeats for the release-critical subset (default 5)
  TM_EVAL_REPORT_DIR (default coverage/evals/thinking-map)
"""

import json
import os
import re
import sys
import time
import urllib.request
import uuid
from datetime import datetime, timezone

BASE = os.environ.get("MAGICIAN_URL", "http://127.0.0.1:3002").rstrip("/")
API = f"{BASE}/api/magician/v2/thinking-maps"
BEARER_TOKEN = os.environ.get("MAGICIAN_BEARER_TOKEN", "").strip()
SCOPE = {"Authorization": f"Bearer {BEARER_TOKEN}"} if BEARER_TOKEN else {}
RUNS = int(os.environ.get("TM_EVAL_RUNS") or "5")
REPORT_DIR = os.environ.get("TM_EVAL_REPORT_DIR") or "coverage/evals/thinking-map"

STOPWORDS = set(
    "a an the of to for in on with and or we our i you they it its is are was be will would "
    "could should let lets let's that this those these there here about over under my your "
    "than then so if not no yes do does did done can cannot at by as from into out up down "
    "s t re ve ll d m don didn actually really said discussed keep going okay ok uh um yeah "
    "like need needs go month months plan plans".split()
)


def words(text):
    return {w for w in re.findall(r"[a-z0-9']+", text.lower()) if w not in STOPWORDS and len(w) > 2}


def http(method, url, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    for k, v in SCOPE.items():
        req.add_header(k, v)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(req, timeout=90) as resp:
        return json.loads(resp.read().decode() or "{}")


def now_iso():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def owner_node(label, kind="fact"):
    nid = str(uuid.uuid4())
    ts = now_iso()
    return nid, {
        "op": "add_node",
        "node": {
            "node_id": nid,
            "kind": kind,
            "label": label,
            "epistemic_state": "asserted",
            "assertion_origin": "owner_spoken",
            "confidence": 1.0,
            "created_at": ts,
            "updated_at": ts,
        },
    }


# ── Fixture cases ─────────────────────────────────────────────────────────────
# board: list of (label, kind) owner nodes seeded before the utterance.
# expect_keywords: at least one produced node label must hit each keyword GROUP
#   (a group is satisfied by any of its alternatives) → recall; precision counts
#   produced nodes that hit ANY group.
# expect_noop: interpret should return no_operations (or add nothing).
# correction_target: index into `board` — some produced op must reference that
#   node's id (update/state/supersede/edge endpoint/parent).
# allow_clarification: clarifications are acceptable (ambiguity case).
CASES = [
    {
        "name": "capture_new_idea",
        "critical": True,
        "board": [],
        "utterance": "We could offer a free student tier to grow adoption on campuses",
        "expect_keywords": [["free", "student", "tier", "campus", "adoption"]],
    },
    {
        "name": "correction_targets_existing",
        "critical": True,
        "board": [("Launch price is $20 per month", "fact")],
        "utterance": "Correction on that price point - the launch price will actually be $25 per month, not $20",
        "expect_keywords": [["25", "price"]],
        "correction_target": 0,
    },
    {
        "name": "chitchat_noop",
        "critical": True,
        "board": [("Ship the beta in March", "decision")],
        "utterance": "uh okay yeah that sounds good, let's keep going",
        "expect_noop": True,
    },
    {
        "name": "question_capture",
        "board": [],
        "utterance": "How do we handle refunds for annual subscriptions?",
        "expect_keywords": [["refund", "annual", "subscription"]],
        "expect_kind": "question",
    },
    {
        "name": "risk_capture",
        "board": [("Migrate the database to Postgres", "decision")],
        "utterance": "I'm worried the migration could corrupt older records if we rush it",
        "expect_keywords": [["corrupt", "migration", "older", "records", "rush"]],
        "expect_kind": "risk",
    },
    {
        "name": "decision_capture",
        "board": [("Postgres or SQLite for storage?", "question")],
        "utterance": "Decision made: we're going with Postgres for the storage layer",
        "expect_keywords": [["postgres"]],
    },
    {
        "name": "duplicate_restraint",
        "board": [("Add dark mode to the app", "idea")],
        "utterance": "yeah dark mode, like I already said, dark mode for the app",
        "max_new_nodes": 0,
        "allow_reference": True,
    },
    {
        "name": "ambiguity_clarification",
        "board": [("Improve onboarding flow", "idea"), ("Speed up the export pipeline", "action")],
        "utterance": "the thing we discussed earlier definitely needs to be faster",
        "allow_clarification": True,
        "max_new_nodes": 2,
    },
]


def run_case(case):
    """Create a scratch map, seed the board, interpret, score, soft-delete."""
    map_id = str(uuid.uuid4())
    result = {
        "name": case["name"],
        "valid": False,
        "latency_ms": None,
        "outcome": None,
        "new_nodes": [],
        "precision_hits": 0,
        "recall_groups_hit": 0,
        "recall_groups": len(case.get("expect_keywords", [])),
        "correction_ok": None,
        "duplicates": 0,
        "unsupported": 0,
        "authority_violations": 0,
        "clarifications": 0,
        "clarification_violation": False,
        "noop_ok": None,
        "error": None,
    }
    try:
        http("POST", API, {"title": f"eval-tm-{case['name']}", "map_id": map_id})
        board_ids = []
        board_labels = []
        setup_ops = []
        for label, kind in case.get("board", []):
            nid, op = owner_node(label, kind)
            board_ids.append(nid)
            board_labels.append(label)
            setup_ops.append(op)
        if setup_ops:
            http(
                "POST",
                f"{API}/{map_id}/operations",
                {"operations": setup_ops, "idempotency_key": f"seed-{map_id}", "base_revision": 0},
            )

        before = http("GET", f"{API}/{map_id}")
        before_nodes = set(before["nodes"].keys())
        before_title = before["title"]

        started = time.time()
        outcome = http(
            "POST",
            f"{API}/{map_id}/interpret",
            {"text": case["utterance"], "intent": "continue_thinking"},
        )
        result["latency_ms"] = round((time.time() - started) * 1000)
        result["outcome"] = outcome.get("outcome")
        result["valid"] = outcome.get("outcome") in ("applied", "no_operations")

        after = http("GET", f"{API}/{map_id}")
        new_node_ids = [n for n in after["nodes"] if n not in before_nodes]
        new_nodes = [after["nodes"][n] for n in new_node_ids]
        result["new_nodes"] = [f"{n['kind']}:{n['label']}" for n in new_nodes]
        result["clarifications"] = len(after.get("clarifications", {}))

        # Authority: interpreter output must be model_inferred (+ provisional),
        # and owner-only effects (title change) must not occur.
        for n in new_nodes:
            if n["assertion_origin"] != "model_inferred" or n["epistemic_state"] not in (
                "provisional",
            ):
                result["authority_violations"] += 1
        if after["title"] != before_title:
            result["authority_violations"] += 1

        # No-op expectation (churn restraint).
        if case.get("expect_noop"):
            result["noop_ok"] = len(new_nodes) == 0
        if "max_new_nodes" in case and len(new_nodes) > case["max_new_nodes"]:
            if not case.get("allow_reference") or any(
                len(words(n["label"]) & words(board_labels[0])) >= 2 for n in new_nodes
            ):
                result["duplicates"] += sum(
                    1
                    for n in new_nodes
                    for bl in board_labels
                    if len(words(n["label"]) & words(bl)) >= 2
                )

        # Precision / recall over expected keyword groups.
        groups = case.get("expect_keywords", [])
        hit_groups = set()
        for n in new_nodes:
            lw = words(n["label"]) | words(n.get("detail_markdown") or "")
            matched = False
            for gi, group in enumerate(groups):
                if any(k in " ".join(lw) or k in lw for k in group):
                    hit_groups.add(gi)
                    matched = True
            if matched:
                result["precision_hits"] += 1
            # Unsupported assertion: shares no content words with utterance/board.
            support = words(case["utterance"]) | set().union(
                *(words(b) for b in board_labels or [""])
            )
            if not (lw & support):
                result["unsupported"] += 1
        result["recall_groups_hit"] = len(hit_groups)

        # Correction targeting: any event op this apply references the target id.
        if "correction_target" in case:
            target = board_ids[case["correction_target"]]
            refs = False
            events = http("GET", f"{API}/{map_id}/events?after_seq=0")
            for ev in events if isinstance(events, list) else events.get("events", []):
                blob = json.dumps(ev)
                if target in blob and '"interp:' in blob or (
                    target in blob and '"actor":{"actor":"model"' in blob.replace(" ", "")
                ):
                    refs = True
            # Simpler + robust: the target id appearing in any model-authored
            # envelope, or a new edge touching it, or its node mutated.
            tgt_after = after["nodes"].get(target, {})
            tgt_before = before["nodes"].get(target, {})
            if tgt_after != tgt_before:
                refs = True
            for e in after.get("edges", {}).values():
                if e.get("from_node") == target or e.get("to_node") == target:
                    refs = True
            for n in new_nodes:
                if n.get("parent_id") == target:
                    refs = True
            result["correction_ok"] = refs

        # Clarification restraint: only the ambiguity case may ask.
        if result["clarifications"] > 0 and not case.get("allow_clarification"):
            result["clarification_violation"] = True
    except Exception as exc:  # noqa: BLE001 — report, don't crash the suite
        result["error"] = str(exc)[:200]
    finally:
        try:
            http("PATCH", f"{API}/{map_id}", {"lifecycle": "deleted"})
        except Exception:
            pass
    return result


def main():
    runs = []
    for case in CASES:
        repeats = RUNS if case.get("critical") else 1
        for i in range(repeats):
            r = run_case(case)
            r["repeat"] = i
            runs.append(r)
            status = "ok" if (r["valid"] and not r["error"]) else "FAIL"
            print(f"[{status}] {r['name']}#{i} outcome={r['outcome']} "
                  f"nodes={len(r['new_nodes'])} {r['latency_ms']}ms", flush=True)

    total = len(runs)
    valid = sum(1 for r in runs if r["valid"])
    corr = [r for r in runs if r["correction_ok"] is not None]
    corr_ok = sum(1 for r in corr if r["correction_ok"])
    noop = [r for r in runs if r["noop_ok"] is not None]
    noop_ok = sum(1 for r in noop if r["noop_ok"])
    prec_nodes = sum(len(r["new_nodes"]) for r in runs if r["recall_groups"])
    prec_hits = sum(r["precision_hits"] for r in runs)
    recall_groups = sum(r["recall_groups"] for r in runs)
    recall_hits = sum(r["recall_groups_hit"] for r in runs)
    dup = sum(r["duplicates"] for r in runs)
    unsupported = sum(r["unsupported"] for r in runs)
    authority = sum(r["authority_violations"] for r in runs)
    clar_viol = sum(1 for r in runs if r["clarification_violation"])
    latencies = sorted(r["latency_ms"] for r in runs if r["latency_ms"])
    p50 = latencies[len(latencies) // 2] if latencies else None
    p95 = latencies[int(len(latencies) * 0.95) - 1] if latencies else None

    # Stability: release-critical cases must produce a consistent verdict
    # across repeats (valid + same noop/correction verdict).
    stability = {}
    for case in CASES:
        if not case.get("critical"):
            continue
        rs = [r for r in runs if r["name"] == case["name"]]
        verdicts = {
            (r["valid"], r["noop_ok"], r["correction_ok"], bool(r["error"])) for r in rs
        }
        stability[case["name"]] = len(verdicts) == 1

    summary = {
        "generated_at": now_iso(),
        "server": BASE,
        "runs": total,
        "envelope_validity": round(valid / total, 3) if total else 0,
        "correction_target_accuracy": round(corr_ok / len(corr), 3) if corr else None,
        "noop_restraint": round(noop_ok / len(noop), 3) if noop else None,
        "node_precision": round(prec_hits / prec_nodes, 3) if prec_nodes else None,
        "keyword_recall": round(recall_hits / recall_groups, 3) if recall_groups else None,
        "duplicate_churn_events": dup,
        "unsupported_assertions": unsupported,
        "authority_violations": authority,
        "clarification_violations": clar_viol,
        "latency_ms_p50": p50,
        "latency_ms_p95": p95,
        "stability": stability,
        "cases": runs,
    }

    os.makedirs(REPORT_DIR, exist_ok=True)
    stamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    with open(os.path.join(REPORT_DIR, "latest.json"), "w") as f:
        json.dump(summary, f, indent=2)
    rows = "".join(
        f"<tr><td>{r['name']}#{r['repeat']}</td><td>{r['outcome']}</td>"
        f"<td>{'; '.join(r['new_nodes']) or '—'}</td><td>{r['latency_ms']}</td>"
        f"<td>{r['error'] or ''}</td></tr>"
        for r in runs
    )
    metrics = "".join(
        f"<tr><td>{k}</td><td>{json.dumps(v)}</td></tr>"
        for k, v in summary.items()
        if k != "cases"
    )
    html = (
        "<html><head><title>Thinking Map live eval</title><style>"
        "body{font-family:sans-serif;margin:2rem}table{border-collapse:collapse;margin:1rem 0}"
        "td,th{border:1px solid #ccc;padding:4px 10px;font-size:13px;text-align:left}"
        "</style></head><body><h1>Thinking Map — live semantic eval</h1>"
        f"<table>{metrics}</table><h2>Case runs</h2>"
        f"<table><tr><th>case</th><th>outcome</th><th>new nodes</th><th>ms</th><th>error</th></tr>"
        f"{rows}</table></body></html>"
    )
    with open(os.path.join(REPORT_DIR, "latest.html"), "w") as f:
        f.write(html)
    with open(os.path.join(REPORT_DIR, f"run-{stamp}.json"), "w") as f:
        json.dump(summary, f, indent=2)

    print(json.dumps({k: v for k, v in summary.items() if k != "cases"}, indent=2))

    # Exit gates (plan Phase 3).
    failures = []
    if summary["envelope_validity"] < 0.95:
        failures.append(f"envelope validity {summary['envelope_validity']} < 0.95")
    if corr and summary["correction_target_accuracy"] < 0.95:
        failures.append(
            f"correction target accuracy {summary['correction_target_accuracy']} < 0.95"
        )
    if authority > 0:
        failures.append(f"{authority} authority violations")
    if failures:
        print("GATE FAILURES: " + "; ".join(failures), file=sys.stderr)
        sys.exit(1)
    print("ALL GATES PASSED")


if __name__ == "__main__":
    main()
