#!/usr/bin/env python3
"""Evaluate the channel-assist distill + classify prompts against an Ollama model.

Why this exists
---------------
`op-channel-distill-local` and `op-channel-classify-local` (magician-config.yaml)
run a LOCAL ollama model that must emit strict JSON. Some models — notably heavily
quantized ones like `gemma4:26b-a4b-it-qat` — degenerate into repetition loops on
the complex distill task and return invalid/truncated JSON (observed 2026-07-14:
"no JSON object in distill reply"). `gemma4:12b` handled it cleanly.

This script runs the REAL managed prompts (data/magician_v2/prompts/) against a set
of representative messages and reports JSON validity, schema conformance,
`done_reason`, and output size — so you can compare candidate models before pinning
one in config. It approximates the production user-prompt (metadata/summary for the
body-blind classifier; raw content for the distiller); it is a relative model
comparison, not a byte-identical replay of the pipeline.

Usage
-----
    python3 scripts/eval-channel-llm.py --model gemma4:12b
    python3 scripts/eval-channel-llm.py --model gemma4:26b-a4b-it-qat --iterations 5
    python3 scripts/eval-channel-llm.py --model qwen3:8b --ops distill --num-predict 2048

Requires a running Ollama (default http://localhost:11434) with the model pulled
(`ollama pull <model>`). Exit code is non-zero if any operation's pass rate < 100%.
"""

from __future__ import annotations

import argparse
import glob
import json
import os
import sys
import urllib.request

from channel_eval_preflight import preflight

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PROMPT_DIR = os.path.join(REPO, "data", "magician_v2", "prompts")

# Distiller sees raw content; these are representative inbound messages.
DISTILL_CASES = [
    (
        "bank_multichange",
        "Channel: chat. Newest-first batch.\n"
        "[1] From: Acme Bank <alerts@acme.com> | Subject: Statement ready + 3 policy changes\n"
        "Body: July e-statement available. Effective Aug 1: (a) ATM withdrawal cap 25,000 -> "
        "40,000 INR/day; (b) international txn fee 2.5% -> 3.5%; (c) minimum balance 10,000 -> "
        "5,000. Autopay for card ending 4412 scheduled Jul 28, amount 18,432.10 INR. Review before Aug 1.",
    ),
    (
        "simple_receipt",
        "Channel: chat. Newest-first batch.\n"
        "[1] From: ShopMart | Subject: Your order shipped\n"
        "Body: Order #A123 shipped, arriving Tuesday. Track at the app.",
    ),
    (
        "meeting_request",
        "Channel: chat. Newest-first batch.\n"
        "[1] From: Sam | Subject: Q3 review\n"
        "Body: Can we meet Thursday 3pm to review the Q3 numbers? Please confirm or propose another time.",
    ),
]

# Classifier is body-blind: it sees metadata + a locally-derived summary.
CLASSIFY_CASES = [
    (
        "needs_reply",
        "Thread metadata: inbound, direct message, thread age 1 day, unanswered.\n"
        "Local summary: Sam asks the owner to confirm a Thursday 3pm meeting or propose another time.",
    ),
    (
        "fyi_notice",
        "Thread metadata: inbound, automated sender, thread age 0 days.\n"
        "Local summary: Bank statement is ready; three account policy changes take effect Aug 1.",
    ),
    (
        "no_action_receipt",
        "Thread metadata: inbound, automated sender, thread age 0 days.\n"
        "Local summary: Shipping notification that order A123 shipped and arrives Tuesday.",
    ),
]

# Required keys per operation — a parse that omits these is a schema miss (the
# 26b-a4b model sometimes returned e.g. `follow_up_action` and dropped `brief`).
REQUIRED_KEYS = {
    "distill": ["summary", "intent"],
    "classify": ["label", "confidence", "reason"],
}
CLASSIFY_LABELS = {"needs_reply", "follow_up", "fyi", "no_action"}
SYSTEM_GLOB = {
    "distill": "channel_ingest_distill_system_v*.json",
    "classify": "channel_classify_system_v*.json",
}


def latest_prompt(pattern: str) -> str:
    matches = sorted(glob.glob(os.path.join(PROMPT_DIR, pattern)))
    if not matches:
        sys.exit(f"no prompt file matching {pattern} under {PROMPT_DIR}")
    doc = json.load(open(matches[-1]))
    return "\n".join(doc["content"]), os.path.basename(matches[-1])


def run_once(base_url: str, model: str, system: str, user: str, num_predict: int) -> dict:
    body = {
        "model": model,
        "prompt": f"{system}\n\n{user}",
        "stream": False,
        "format": "json",
        "options": {"num_ctx": 32768, "num_predict": num_predict, "temperature": 0.1},
    }
    req = urllib.request.Request(
        f"{base_url.rstrip('/')}/api/generate",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    d = json.loads(urllib.request.urlopen(req, timeout=240).read())
    return {"response": d.get("response", ""), "done_reason": d.get("done_reason")}


def check(op: str, raw: str) -> tuple[bool, str]:
    try:
        obj = json.loads(raw)
    except Exception as e:
        return False, f"invalid JSON ({e})"
    if not isinstance(obj, dict):
        return False, "not a JSON object"
    missing = [k for k in REQUIRED_KEYS[op] if k not in obj]
    if missing:
        return False, f"missing keys {missing}"
    if op == "classify" and obj.get("label") not in CLASSIFY_LABELS:
        return False, f"bad label {obj.get('label')!r}"
    return True, "ok"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--model", required=True, help="ollama model tag, e.g. gemma4:12b")
    ap.add_argument("--base-url", default=os.environ.get("OLLAMA_BASE_URL", "http://localhost:11434"))
    ap.add_argument("--iterations", type=int, default=3, help="runs per case (catches flakiness)")
    ap.add_argument("--num-predict", type=int, default=2048)
    ap.add_argument("--ops", default="distill,classify", help="comma list: distill,classify")
    ap.add_argument("--stop-stack", action="store_true",
                    help="if the magician stack is running, stop it (make stop-supervisor) before the eval")
    args = ap.parse_args()

    # The eval must own ollama exclusively — require the stack down, bring ollama up.
    stopped_stack = preflight(args.base_url, args.stop_stack)

    ops = [o.strip() for o in args.ops.split(",") if o.strip()]
    cases = {"distill": DISTILL_CASES, "classify": CLASSIFY_CASES}
    overall_ok = True

    print(f"model={args.model}  iterations={args.iterations}  num_predict={args.num_predict}\n")
    for op in ops:
        system, fname = latest_prompt(SYSTEM_GLOB[op])
        passes = total = 0
        chars_sum = 0
        length_stops = 0
        print(f"== {op}  (prompt: {fname}) ==")
        for name, user in cases[op]:
            results = []
            for _ in range(args.iterations):
                out = run_once(args.base_url, args.model, system, user, args.num_predict)
                ok, why = check(op, out["response"])
                total += 1
                passes += ok
                chars_sum += len(out["response"])
                length_stops += out["done_reason"] == "length"
                results.append("ok" if ok else f"FAIL:{why}")
            tag = "PASS" if all(r == "ok" for r in results) else "FLAKY/FAIL"
            print(f"  {name:<18} {tag:<11} {results}")
        rate = passes / total if total else 0
        overall_ok = overall_ok and rate == 1.0
        print(f"  -> {passes}/{total} valid  |  avg {chars_sum // max(total,1)} chars  |  "
              f"{length_stops} hit num_predict (degeneration/truncation)\n")

    if stopped_stack:
        print("NOTE: this eval stopped the magician stack — restart it with `make run-supervisor`.")
    return 0 if overall_ok else 1


if __name__ == "__main__":
    sys.exit(main())
