#!/usr/bin/env python3
"""End-to-end check of planning on the decision engine against a running Magician.

Runs the product task flow unattended:
  1. create a V3 task and start planning (POST /v3/tasks, POST /v3/tasks/{id}/plan);
  2. answer every planning clarification and approve the draft plan;
  3. execute the task and answer any run-time HITL (user requests, approvals)
     until the execution ends;
  4. print the planning turn's analysis (intent, task clarity, complexity), the
     plan's unresolved inputs (priority / ask / discovery timing), and the
     decision comparison records for `query_intent` and `plan_parameter_priority`
     (engine answers next to the planner LLM's labels).

Unattended answers: confirmations are approved, choices take the first option,
free text uses --answer (sensible defaults, drafts only, never send).
Auth: MAGICIAN_BEARER_TOKEN, else scripts/magician-token.sh.

  scripts/eval-planning-decisions-live.py
  scripts/eval-planning-decisions-live.py --prompt "..." --plan-timeout 900 --run-timeout 1800
"""
import argparse
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

OPERATIONS = ("query_intent", "plan_parameter_priority")
DEFAULT_PROMPT = (
    "Look up today's weather in Pune and the top three Hacker News headlines, "
    "then summarize both in a short note titled 'Morning brief'."
)
HERE = os.path.dirname(os.path.abspath(__file__))


def request(api, token, method, path, body=None, timeout=60):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(api + path, data=data, method=method)
    req.add_header("Authorization", f"Bearer {token}")
    if data is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            raw = resp.read()
            return resp.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as err:
        raw = err.read()
        try:
            return err.code, json.loads(raw)
        except ValueError:
            return err.code, raw.decode(errors="replace")


def token_for(api):
    token = os.environ.get("MAGICIAN_BEARER_TOKEN", "").strip()
    if token:
        return token
    out = subprocess.run([os.path.join(HERE, "magician-token.sh")], capture_output=True,
                         text=True, env=dict(os.environ, MAGICIAN_API=api))
    if out.returncode != 0:
        sys.exit(f"token: {out.stderr.strip()}")
    return out.stdout.strip()


def scope_log(root, principal, workspace):
    return os.path.join(root, "scopes", principal, workspace, "events.jsonl")


def find_records(node):
    """Decision comparison records anywhere inside an event envelope."""
    if isinstance(node, dict):
        if "comparison_id" in node and "operation" in node:
            yield node
            return
        for value in node.values():
            yield from find_records(value)
    elif isinstance(node, list):
        for value in node:
            yield from find_records(value)


def new_records(path, since_ms, window_bytes=80_000_000):
    """Comparison records emitted at or after `since_ms`. Matched by the event
    timestamp, not a byte offset: the scope log can be compacted mid-run."""
    if not os.path.exists(path):
        return []
    with open(path, "rb") as handle:
        handle.seek(max(0, os.path.getsize(path) - window_bytes))
        chunk = handle.read()
    records = []
    for line in chunk.splitlines():
        if b"DecisionShadowAgreement" not in line:
            continue
        try:
            event = json.loads(line)
        except ValueError:
            continue
        stamp = (event.get("data") or {}).get("timestamp") or event.get("timestamp") or 0
        if stamp >= since_ms:
            records.extend(find_records(event))
    return records


def answers_of(item):
    response = item.get("response") or {}
    out = {}
    for question, answer in (response.get("answers") or {}).items():
        if not isinstance(answer, dict):
            continue
        body = answer  # internally tagged: {"type": "choice", "choice": ..., "confidence": ...}
        if "choice" in body:
            out[question] = f"{body['choice']} ({body.get('confidence', 0):.2f})"
        elif "noul" in body:
            out[question] = f"p={body['noul']:.2f}"
        elif "score" in body:
            out[question] = f"score={body['score']:.2f}"
    return out


def summarize(record):
    reference = (record.get("reference") or {}).get("labels") or {}
    print(f"\n  [{record.get('operation')}] mode={record.get('mode')} "
          f"offered={record.get('offered')} engine={record.get('engine_latency_ms')}ms "
          f"wall={record.get('wall_latency_ms')}ms accounting_complete={record.get('accounting_complete')}")
    agree = total = 0
    for item in record.get("items") or []:
        item_id = item.get("item_id")
        engine = answers_of(item)
        eligible = sorted((item.get("eligible_answers") or {}).keys())
        llm = reference.get(item_id, {})
        print(f"    item {item_id}: status={item.get('status')} eligible={eligible}")
        for question in sorted(set(engine) | set(llm)):
            e = engine.get(question, "-")
            r = llm.get(question, "-")
            match = str(e).split(" ")[0] == str(r)
            if question in engine and question in llm:
                total += 1
                agree += match
            print(f"      {question:<18} engine={e:<28} llm={r} {'✓' if match else '✗'}")
    if total:
        print(f"    agreement {agree}/{total}")
    return agree, total


TERMINAL_STATES = {"Completed", "Failed", "Cancelled", "Canceled", "Aborted"}
DEFAULT_ANSWER = (
    "Use sensible defaults and your best judgment; don't ask again. Only draft or "
    "prepare anything outward-facing (emails, messages, posts) - never send or publish it."
)


def quote(value):
    return urllib.parse.quote(str(value), safe="")


def hitl_value(item, answer_text):
    """Unattended answer: confirm confirmations, first option for choices, else text."""
    input_type = str(item.get("input_type") or item.get("type") or "").lower()
    schema = item.get("input_schema") if isinstance(item.get("input_schema"), dict) else {}
    options = item.get("options") or schema.get("options") or []
    options = [o for o in options if isinstance(o, dict)]
    if input_type in ("confirmation", "approval", "plan_approval"):
        return {"type": "confirmation", "confirmed": True}
    if input_type == "external_action":
        # Unattended: acknowledge the step as done (what the "Done" button sends).
        return {"type": "external_action_completed"}
    if options:
        option_id = str(options[0].get("id") or options[0].get("value") or "")
        return {"type": "choice", "selected_id": option_id}
    return {"type": "text", "value": answer_text}


class AutoResponder:
    """Answers every HITL item that belongs to one execution, once each."""

    def __init__(self, api, token, execution_id, answer_text):
        self.api, self.token, self.execution_id = api, token, execution_id
        self.answer_text = answer_text
        self.answered = []
        self.seen = set()

    def respond(self, correlation_id, source, value, prompt, task_id=None):
        if correlation_id in self.seen:
            return
        self.seen.add(correlation_id)
        body = {"source": source, "value": value, "execution_id": self.execution_id,
                "input_type": {"external_action_completed": "external_action"}.get(value.get("type"), value.get("type")),
                "channel": "planning_decisions_live_eval"}
        if task_id:
            body["task_id"] = task_id
        status, reply = request(self.api, self.token, "POST",
                                f"/api/magician/v2/hitl/{quote(correlation_id)}/respond", body)
        accepted = status == 200 and isinstance(reply, dict) and reply.get("accepted") is not False
        shown = (value.get("value") or value.get("selected_id") or value.get("confirmed")
                 or [a.get("value") or a.get("selected_ids") for a in value.get("answers", [])])
        print(f"  HITL [{source}] {prompt[:100]!r} -> {shown!r}: HTTP {status} "
              f"{'accepted' if accepted else reply}", flush=True)
        self.answered.append({"source": source, "prompt": prompt, "answer": shown, "status": status})

    def form_answer(self, question):
        """Plausible test value for one form question; never real credentials."""
        key = f"{question.get('id', '')} {question.get('prompt', '')}".lower()
        if "password" in key or "secret" in key or "token" in key:
            return {"id": question.get("id"), "skipped": True}
        if "landlord" in key and "email" in key:
            value = "landlord@example.com"
        elif "timezone" in key or "time zone" in key:
            value = "Asia/Kolkata. " + self.answer_text
        elif "email" in key:
            value = "owner@example.com (keep everything as unsent drafts)"
        else:
            value = self.answer_text
        options = [o for o in question.get("options") or [] if isinstance(o, dict)]
        if options:
            return {"id": question.get("id"), "selected_ids": [str(options[0].get("id") or options[0].get("value"))]}
        return {"id": question.get("id"), "value": value}

    def poll_agentic(self, execution_id):
        """Agentic pause on the run or any active delegated child."""
        ids = [execution_id]
        status, payload = request(self.api, self.token, "GET",
                                  f"/api/magician/v2/executions/{quote(execution_id)}/responsibility")
        if status == 200 and isinstance(payload, dict):
            ids += [c.get("execution_id") for c in payload.get("active_children") or [] if c.get("execution_id")]
        for target in ids:
            status, payload = request(self.api, self.token, "GET",
                                      f"/api/magician/v2/executions/{quote(target)}/pause-state")
            pause = payload.get("active_pause_state") if status == 200 and isinstance(payload, dict) else None
            if not isinstance(pause, dict) or not pause.get("pause_state_id"):
                continue
            input_type = pause.get("input_type")
            base_id = pause["pause_state_id"]
            if any(seen == base_id or seen.startswith(base_id + "~") for seen in self.seen):
                continue  # already answered through the feed (its id adds a ~revision)
            if isinstance(input_type, dict) and ("done_label" in input_type or "instructions" in input_type) \
                    and not input_type.get("questions"):
                value, kind = {"type": "external_action_completed"}, "external_action"
            elif isinstance(input_type, dict) and input_type.get("questions"):
                value = {"type": "form", "answers": [self.form_answer(q) for q in input_type["questions"]]}
                kind = "form"
            elif pause.get("confirmation_action_type") or str(input_type).lower().startswith("confirm"):
                value, kind = {"type": "confirmation", "confirmed": True}, "confirmation"
            elif pause.get("options"):
                first = pause["options"][0]
                value = {"type": "choice", "selected_id": str(first.get("id") or first.get("value"))}
                kind = "choice"
            else:
                value, kind = {"type": "text", "value": self.answer_text}, "text"
            if pause["pause_state_id"] not in self.seen:
                print(f"  PAUSE-STATE fallback: execution {target} had an ask the feed did not show", flush=True)
            saved, self.execution_id = self.execution_id, target
            self.respond(pause["pause_state_id"], "agentic", dict(value, **{}), str(pause.get("question") or ""))
            self.execution_id = saved

    def poll_feed(self, task_id):
        """Answer asks the way the clients do: from the attention feed. Rows for
        this task (including delegated children) and stalled-wait escalations
        are logged with where they came from."""
        status, payload = request(self.api, self.token, "GET", "/api/magician/v2/feed/attention?limit=200")
        if status != 200 or not isinstance(payload, dict):
            return
        for lane in ("requests", "escalations"):
            for row in payload.get(lane) or []:
                meta = row.get("metadata") or {}
                if task_id and row.get("task_id") != task_id:
                    continue
                if meta.get("attention_reason") == "waiting_without_question":
                    key = f"stalled:{meta.get('execution_id')}"
                    if key not in self.seen:
                        self.seen.add(key)
                        print(f"  FEED [stalled wait] execution {meta.get('execution_id')}", flush=True)
                        self.answered.append({"source": "stalled_wait", "prompt": row.get("title"), "answer": None})
                    continue
                hitl = meta.get("hitl_request")
                if not isinstance(hitl, dict):
                    continue
                cid = hitl.get("id") or (hitl.get("identifiers") or {}).get("correlation_id")
                if not cid or cid in self.seen:
                    continue
                target = (hitl.get("scope") or {}).get("execution_id") or meta.get("execution_id")
                source = str(hitl.get("source") or meta.get("source") or "agentic")
                schema = hitl.get("input_schema") or meta.get("input_schema") or {}
                item = dict(hitl, input_type=hitl.get("input_type") or meta.get("input_type"), input_schema=schema)
                if isinstance(schema, dict) and schema.get("questions"):
                    value = {"type": "form", "answers": [self.form_answer(q) for q in schema["questions"]]}
                else:
                    value = hitl_value(item, self.answer_text)
                label = "delegated child" if meta.get("delegation_child") else "task"
                print(f"  FEED [{label} ask via /feed/attention] source={source} execution={target}", flush=True)
                saved, self.execution_id = self.execution_id, target or self.execution_id
                self.respond(cid, source, value, str(hitl.get("prompt") or row.get("title") or ""), task_id)
                self.execution_id = saved

    def poll(self, task_id):
        if task_id:
            status, payload = request(self.api, self.token, "GET", f"/api/magician/v3/tasks/{quote(task_id)}/plan")
            plan = payload.get("plan") if status == 200 and isinstance(payload, dict) else None
            if isinstance(plan, dict):
                for question in plan.get("pending_questions") or []:
                    qid = question.get("id") or question.get("question_id")
                    if qid:
                        self.respond(qid, "clarification", hitl_value(question, self.answer_text),
                                     str(question.get("question_text") or question.get("question") or ""), task_id)
                if plan.get("status") == "draft" and plan.get("plan_id"):
                    self.respond(plan["plan_id"], "plan_approval", {"type": "confirmation", "confirmed": True},
                                 "Task plan is ready for review", task_id)
                hitl = plan.get("hitl_request")
                if isinstance(hitl, dict):
                    cid = hitl.get("correlation_id") or hitl.get("id")
                    source = str(hitl.get("source") or "user_request")
                    if cid:
                        self.respond(cid, source, hitl_value(hitl, self.answer_text), str(hitl.get("prompt") or ""), task_id)
        status, payload = request(self.api, self.token, "GET", "/api/magician/v2/user-requests")
        for item in (payload or {}).get("requests", []) if isinstance(payload, dict) else []:
            if str(item.get("execution_id") or "") == self.execution_id and item.get("id"):
                self.respond(item["id"], "user_request", hitl_value(item, self.answer_text),
                             str(item.get("question") or item.get("prompt") or ""), task_id)
        status, payload = request(self.api, self.token, "GET", "/api/magician/v2/approvals")
        for item in (payload or {}).get("approvals", []) if isinstance(payload, dict) else []:
            if str(item.get("execution_id") or "") == self.execution_id and item.get("approval_id"):
                actions = item.get("pending_actions") or [{}]
                self.respond(item["approval_id"], "approval", {"type": "confirmation", "confirmed": True},
                             str(actions[0].get("action_description") or "approval"), task_id)


def nested(value, keys):
    """First non-empty string under any of `keys`, searching nested objects."""
    if isinstance(value, dict):
        for key in keys:
            if isinstance(value.get(key), str) and value[key].strip():
                return value[key].strip()
        for child in value.values():
            found = nested(child, keys)
            if found:
                return found
    elif isinstance(value, list):
        for child in value:
            found = nested(child, keys)
            if found:
                return found
    return None


def execution_record(api, token, execution_id):
    status, payload = request(api, token, "GET", f"/api/magician/v2/executions/{quote(execution_id)}")
    return (payload.get("execution") or payload) if status == 200 and isinstance(payload, dict) else {}


def planning_turn(api, token, execution_id):
    status, payload = request(api, token, "GET", f"/api/magician/v2/executions/{quote(execution_id)}/turns")
    turns = (payload.get("items") or []) if status == 200 and isinstance(payload, dict) else []
    analysed = [t for t in turns if t.get("query_analysis")]
    return analysed[-1] if analysed else None


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--api", default=os.environ.get("MAGICIAN_API", "http://127.0.0.1:3002"))
    parser.add_argument("--root", default=os.environ.get("MAGICIAN_ROOT_DIR", os.path.expanduser("~/MagicianNotes")))
    parser.add_argument("--prompt", default=DEFAULT_PROMPT)
    parser.add_argument("--agent", default="personal-assistant")
    parser.add_argument("--answer", default=DEFAULT_ANSWER, help="text used for free-text HITL questions")
    parser.add_argument("--plan-timeout", type=int, default=900, help="seconds for planning incl. clarifications")
    parser.add_argument("--run-timeout", type=int, default=1800, help="seconds for execution (0 = plan only)")
    parser.add_argument("--settle", type=int, default=30, help="seconds to wait for comparison records")
    parser.add_argument("--attach-run", help="skip create/plan: follow this run execution and answer its HITL")
    args = parser.parse_args()

    token = token_for(args.api)
    status, session = request(args.api, token, "GET", "/api/magician/v2/auth/session")
    if status != 200:
        sys.exit(f"session: HTTP {status} {session}")
    log = scope_log(args.root, session["principal"], session["workspace"])
    since_ms = int(time.time() * 1000)
    print(f"scope {session['principal']}/{session['workspace']}")

    started = time.time()
    if args.attach_run:
        responder = AutoResponder(args.api, token, args.attach_run, args.answer)
        final_state, last = None, None
        deadline = time.time() + args.run_timeout
        while time.time() < deadline:
            record = execution_record(args.api, token, args.attach_run)
            final_state = record.get("waiting_state")
            if final_state != last:
                print(f"  run state: {final_state} ({time.time() - started:.0f}s)", flush=True)
                last = final_state
            if final_state in TERMINAL_STATES:
                break
            responder.poll_feed(record.get("task_id"))
            responder.poll(record.get("task_id"))
            responder.poll_agentic(args.attach_run)
            time.sleep(5)
        print(f"HITL answered automatically: {len(responder.answered)}")
        return 0 if final_state in TERMINAL_STATES else 1
    status, created = request(args.api, token, "POST", "/api/magician/v3/tasks", {
        "title": "[live eval] planning decisions",
        "description": args.prompt,
        "agent_id": args.agent,
        "created_by": "planning-decisions-live-eval",
        "approved": True,
    })
    if status != 201:
        sys.exit(f"create task: HTTP {status} {created}")
    task_id = nested((created or {}).get("task") if isinstance(created, dict) else None, ("task_id", "id"))
    if not task_id:
        sys.exit(f"create task: no task id in {created}")
    status, planned = request(args.api, token, "POST", f"/api/magician/v3/tasks/{quote(task_id)}/plan")
    if status != 202:
        sys.exit(f"start planning: HTTP {status} {planned}")
    plan = planned.get("plan", {}) if isinstance(planned, dict) else {}
    planning_execution = plan.get("planning_execution_id")
    print(f"task {task_id}; planning execution {planning_execution}", flush=True)

    responder = AutoResponder(args.api, token, planning_execution or "", args.answer)
    plan_status = None
    while time.time() - started < args.plan_timeout:
        responder.poll(task_id)
        status, payload = request(args.api, token, "GET", f"/api/magician/v3/tasks/{quote(task_id)}/plan")
        plan = payload.get("plan", {}) if status == 200 and isinstance(payload, dict) else {}
        planning_execution = plan.get("planning_execution_id") or planning_execution
        responder.execution_id = planning_execution or responder.execution_id
        if plan.get("status") != plan_status:
            plan_status = plan.get("status")
            print(f"  plan status: {plan_status} ({time.time() - started:.0f}s)", flush=True)
        if plan_status in ("approved", "failed", "rejected"):
            break
        time.sleep(4)

    turn = planning_turn(args.api, token, planning_execution) if planning_execution else None
    if turn:
        analysis = turn["query_analysis"]
        print(f"\nanalysis: intent={analysis.get('intent')} task_clarity={analysis.get('task_clarity')} "
              f"complexity={(analysis.get('complexity') or {}).get('score')}")
    graph = plan.get("plan_graph") or {}
    inputs = graph.get("unresolved_inputs") or []
    print(f"plan: status={plan_status} steps={len(graph.get('steps') or [])} unresolved_inputs={len(inputs)}")
    for item in inputs:
        print(f"    - {item.get('parameter')}: priority={item.get('priority')} "
              f"ask={item.get('ask_timing')} discovery={item.get('discovery_timing')}")

    final_state = None
    if plan_status == "approved" and args.run_timeout:
        status, executed = request(args.api, token, "POST", f"/api/magician/v3/tasks/{quote(task_id)}/execute", {})
        run_execution = nested((executed or {}).get("execution") if isinstance(executed, dict) else None,
                               ("execution_id", "id")) or nested(executed, ("execution_id",))
        print(f"\nexecute: HTTP {status}; run execution {run_execution}", flush=True)
        if run_execution:
            responder.execution_id = run_execution
            deadline = time.time() + args.run_timeout
            last = None
            while time.time() < deadline:
                final_state = execution_record(args.api, token, run_execution).get("waiting_state")
                if final_state != last:
                    print(f"  run state: {final_state} ({time.time() - started:.0f}s)", flush=True)
                    last = final_state
                if final_state in TERMINAL_STATES:
                    break
                responder.poll_feed(task_id)
                responder.poll(task_id)
                responder.poll_agentic(run_execution)
                time.sleep(5)
    print(f"HITL answered automatically: {len(responder.answered)}")

    deadline = time.time() + args.settle
    records = []
    while time.time() < deadline:
        records = [r for r in new_records(log, since_ms) if r.get("operation") in OPERATIONS]
        if {r.get("operation") for r in records} >= set(OPERATIONS):
            break
        time.sleep(3)
    print(f"\ndecision comparison records: {len(records)}")
    agree = total = 0
    for record in records:
        a, t = summarize(record)
        agree, total = agree + a, total + t
    missing = [op for op in OPERATIONS if op not in {r.get("operation") for r in records}]
    if missing:
        print(f"\nno records for: {', '.join(missing)} (a plan without parameters has no priority batch)")
    if total:
        print(f"\noverall engine vs planner-LLM agreement: {agree}/{total}")
    ok = plan_status == "approved" and (not args.run_timeout or final_state in TERMINAL_STATES)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
