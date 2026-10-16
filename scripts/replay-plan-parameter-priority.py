#!/usr/bin/env python3
"""Replay planner parameter-priority cases against the running decision engine.

Sends one shadow `/v1/decide` batch for `plan_parameter_priority` (whatever
pack version the engine has bound) over the engine's Unix socket, using the
same item projection as `strategy/parameter_priority_decisions.rs`, and prints
engine priority + confidence next to each case's reference label, plus how
many answers clear the configured 0.8 threshold.

  scripts/replay-plan-parameter-priority.py
  scripts/replay-plan-parameter-priority.py --cases data/magician_v2/evals/plan_parameter_priority/replay-2026-09-30.json
"""
import argparse
import http.client
import json
import os
import socket
import uuid

CONTRACT_VERSION = 5
PROJECTION = "plan_parameter_priority_v2"
KINDS = {
    "tool_selection": "which connected tool, account or service carries out the step",
    "slot": "a value in the user's world the step needs",
    "consent": "a permission the user must grant",
}


class UnixConnection(http.client.HTTPConnection):
    def __init__(self, path, timeout=120):
        super().__init__("engine", timeout=timeout)
        self.path = path

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self.path)


def call(sock, method, path, body=None):
    conn = UnixConnection(sock)
    conn.request(method, path, body=json.dumps(body) if body is not None else None,
                 headers={"Content-Type": "application/json"})
    resp = conn.getresponse()
    return resp.status, json.loads(resp.read() or b"null")


def kind_of(parameter):
    prefix = parameter.split(":", 1)[0] if ":" in parameter else None
    return (prefix, KINDS[prefix]) if prefix in KINDS else ("value", "a value the step needs")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--socket", default=os.path.expanduser("~/MagicianNotes/run/decision-engine.sock"))
    parser.add_argument("--cases", default="data/magician_v2/evals/plan_parameter_priority/replay-2026-09-30.json")
    parser.add_argument("--threshold", type=float, default=0.8)
    parser.add_argument("--locality", default="cloud", choices=["cloud", "local"])
    args = parser.parse_args()

    fixture = json.load(open(args.cases))
    status, ops = call(args.socket, "GET", "/v1/operations")
    if status != 200:
        raise SystemExit(f"/v1/operations: HTTP {status}")
    op = next((o for o in (ops.get("operations") or []) if isinstance(o, dict)
               and o.get("operation") == "plan_parameter_priority"), None)
    if isinstance(ops.get("operations"), dict):
        op = ops["operations"].get("plan_parameter_priority")
    print(f"engine {ops.get('engine_instance')} policy {ops.get('policy_revision')}; "
          f"pack {(op or {}).get('pack_version') or (op or {}).get('pack')}")

    items = []
    for index, case in enumerate(fixture["cases"]):
        kind, meaning = kind_of(case["parameter"])
        items.append({"item_id": str(index), "state": {
            "parameter": case["parameter"], "kind": kind, "kind_meaning": meaning,
            "step_tool": case["step_tool"], "step_description": case["step_description"]}})
    request = {
        "contract_version": CONTRACT_VERSION, "operation": "plan_parameter_priority",
        "state": "", "choice_candidates": {}, "locality": args.locality,
        "mode": "shadow", "reference_version": "replay", "request_id": str(uuid.uuid4()),
        "expected_policy_revision": ops.get("policy_revision", ""),
        "projection_version": PROJECTION, "execution_budget_ms": 60000,
        "context": {"user_request": fixture["user_request"]}, "items": items,
    }
    status, reply = call(args.socket, "POST", "/v1/decide", request)
    if status != 200:
        raise SystemExit(f"/v1/decide: HTTP {status} {json.dumps(reply)[:600]}")
    results = {i["item_id"]: i for i in reply.get("items", [])}
    agree = confident = confident_agree = 0
    for index, case in enumerate(fixture["cases"]):
        item = results.get(str(index), {})
        answer = ((item.get("response") or {}).get("answers") or {}).get("priority") or {}
        choice, conf = answer.get("choice"), answer.get("confidence") or 0.0
        match = choice == case["reference"]
        agree += match
        if conf >= args.threshold:
            confident += 1
            confident_agree += match
        print(f"  {case['id']:<26} engine={str(choice):<14} ({conf:.2f})  ref={case['reference']:<14} "
              f"{'✓' if match else '✗'}{'  [gates]' if conf >= args.threshold else ''}")
    n = len(fixture["cases"])
    print(f"\nagreement {agree}/{n}; at ≥{args.threshold}: {confident} answers, {confident_agree} agree")
    print(f"engine latency {reply.get('latency_ms')} ms")


if __name__ == "__main__":
    main()
