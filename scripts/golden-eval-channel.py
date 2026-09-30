#!/usr/bin/env python3
"""Blind golden eval for the channel-assist LOCAL models, on REAL magician data.

Decides whether a candidate local ollama model can replace the one pinned to the
channel-assist operations (`op-channel-classify-local` / `op-channel-distill-local`
in magician-config.yaml). Motivated by a regression where a heavily-quantized model
(`gemma4:26b-a4b-it-qat`) degenerated into repetition loops and returned invalid
JSON, while `gemma4:12b` worked.

Two use cases:

  classify  — REAL data. Reads the local `mail_assist.duckdb`, samples real threads
              balanced across the four golden labels, reconstructs the EXACT
              production classify prompt (metadata + local summary; body-blind),
              runs each model, and grades JSON validity + agreement with the stored
              golden label (reported separately for human-touched annotations, which
              carry a stronger signal than model-only `classified` rows).

  distill   — SYNTHETIC data. The distiller's raw input (message body) is discarded
              after distillation by design (privacy), so it cannot be replayed from
              storage. Instead a fixed set of representative messages of varying
              complexity is used, graded on JSON validity, schema conformance, and
              fact recall (did the summary/brief preserve the key amounts/dates/ids?).

Privacy: real summaries are sent only to LOCAL ollama (localhost) and are never
printed or written to the report — the report stores case ids, verdicts, and
aggregate metrics only. Run it against the live scope; nothing personal is committed.

Usage:
    python3 scripts/golden-eval-channel.py --models gemma4:12b,gemma4:26b-a4b-it-qat
    python3 scripts/golden-eval-channel.py --models gemma4:12b,qwen3:8b \\
        --classify-samples 100 --distill-samples 25 --iterations 1
    python3 scripts/golden-eval-channel.py --models gemma4:12b --only classify --db /path/to/mail_assist.duckdb

Requires a running Ollama with each model pulled, and duckdb (`pip install duckdb`).
"""

from __future__ import annotations

import argparse
import glob
import hashlib
import json
import os
import subprocess
import sys
import threading
import time
import urllib.request

from channel_eval_preflight import preflight

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


class ResSampler:
    """Samples aggregate CPU% and RSS of all `ollama` processes on a background
    thread while a model runs. No sudo (so GPU/ANE util, which needs powermetrics,
    is omitted); RSS is the process view, and the per-model unified-memory
    footprint comes from /api/ps (see footprint()). Used as a context manager."""

    def __init__(self, interval: float = 1.0, proc: str = "ollama"):
        self.interval = interval
        self.proc = proc
        self._stop = threading.Event()
        self.cpu: list = []
        self.rss: list = []
        self._t = None

    def _run(self):
        while not self._stop.is_set():
            try:
                out = subprocess.run(["ps", "-Axo", "rss=,%cpu=,comm="],
                                     capture_output=True, text=True, timeout=5).stdout
                cpu = rss = 0.0
                for ln in out.splitlines():
                    p = ln.split(None, 2)
                    if len(p) == 3 and self.proc in p[2].lower():
                        rss += float(p[0])
                        cpu += float(p[1])
                self.cpu.append(cpu)
                if rss:
                    self.rss.append(rss / 1024.0 / 1024.0)  # KB -> GB
            except Exception:
                pass
            self._stop.wait(self.interval)

    def __enter__(self):
        self._stop.clear()
        self.cpu, self.rss = [], []
        self._t = threading.Thread(target=self._run, daemon=True)
        self._t.start()
        return self

    def __exit__(self, *a):
        self._stop.set()
        if self._t:
            self._t.join(timeout=3)

    def summary(self):
        """(cpu_avg%, cpu_peak%, rss_peak_gb)."""
        cpu_avg = sum(self.cpu) / len(self.cpu) if self.cpu else 0.0
        cpu_peak = max(self.cpu) if self.cpu else 0.0
        rss_peak = max(self.rss) if self.rss else 0.0
        return cpu_avg, cpu_peak, rss_peak


def footprint(base_url: str, model: str) -> float:
    """The model's resident unified-memory footprint in GB, from /api/ps."""
    try:
        d = json.loads(urllib.request.urlopen(f"{base_url.rstrip('/')}/api/ps", timeout=10).read())
        for m in d.get("models", []):
            if model in (m.get("name"), m.get("model")):
                return m.get("size", 0) / 1e9
        sizes = [m.get("size", 0) for m in d.get("models", [])]
        return max(sizes) / 1e9 if sizes else 0.0
    except Exception:
        return 0.0


def tok_per_s(count: int, duration_ns: int) -> float:
    return count / (duration_ns / 1e9) if duration_ns else 0.0
PROMPT_DIR = os.path.join(REPO, "data", "magician_v2", "prompts")
CLASSIFY_LABELS = ["needs_reply", "follow_up", "fyi", "no_action"]
# Annotation states that reflect a human touching the item (stronger golden signal
# than a bare model-produced `classified` row).
HUMAN_TOUCHED = {"needs_approval", "dismissed", "acknowledged", "completed", "superseded"}


def default_db() -> str:
    root = os.environ.get("MAGICIAN_ROOT_DIR", os.path.expanduser("~/MagicianNotes"))
    return os.path.join(root, "scopes", "anonymous", "default", "mail_assist", "mail_assist.duckdb")


def latest_prompt(pattern: str):
    matches = sorted(glob.glob(os.path.join(PROMPT_DIR, pattern)))
    if not matches:
        sys.exit(f"no prompt file matching {pattern} under {PROMPT_DIR}")
    return "\n".join(json.load(open(matches[-1]))["content"]), os.path.basename(matches[-1])


# ---------------------------------------------------------------------------
# Ollama
# ---------------------------------------------------------------------------

def ollama_generate(base_url: str, model: str, system: str, user: str, num_predict: int) -> dict:
    body = {
        "model": model, "prompt": f"{system}\n\n{user}", "stream": False, "format": "json",
        # Disable "thinking" for hybrid reasoning models (e.g. Qwen3.x): with
        # thinking ON + format:json, ollama routes the structured answer into a
        # separate `thinking` field and leaves `response` EMPTY — which reads as
        # invalid here even though the model answered correctly. It also wastes
        # huge time emitting reasoning tokens. Non-thinking models (gemma) ignore
        # this flag. This is a channel-assist classify/distill task, not a
        # reasoning task, so thinking is undesirable regardless.
        "think": False,
        "options": {"num_ctx": 32768, "num_predict": num_predict, "temperature": 0.1},
    }
    req = urllib.request.Request(f"{base_url.rstrip('/')}/api/generate",
                                 data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    try:
        d = json.loads(urllib.request.urlopen(req, timeout=300).read())
        return {"text": d.get("response", ""), "done_reason": d.get("done_reason"),
                "eval_count": d.get("eval_count") or 0, "eval_duration": d.get("eval_duration") or 0}
    except Exception as e:  # network / model-not-pulled / timeout
        return {"text": "", "done_reason": f"error:{e}", "eval_count": 0, "eval_duration": 0}


def parse_obj(text: str):
    try:
        o = json.loads(text)
        return o if isinstance(o, dict) else None
    except Exception:
        return None


def unload(base_url: str, model: str) -> None:
    """Free a model from memory (keep_alive=0) so the next candidate can load —
    two big models (e.g. 27B + 31B) won't both fit on a 32GB machine."""
    try:
        body = json.dumps({"model": model, "keep_alive": 0}).encode()
        req = urllib.request.Request(f"{base_url.rstrip('/')}/api/generate", data=body,
                                     headers={"Content-Type": "application/json"})
        urllib.request.urlopen(req, timeout=30)
    except Exception:
        pass


# ---------------------------------------------------------------------------
# Apple on-device model (apfel)
# ---------------------------------------------------------------------------

APPLE_PREFIX = "apple:"
CLASSIFY_SCHEMA = os.path.join(REPO, "scripts", "fixtures", "channel_classify.schema.json")


def is_apple(model: str) -> bool:
    return model.startswith(APPLE_PREFIX)


def apfel_generate(base_url: str, system: str, user: str, num_predict: int,
                   schema_path: str = None) -> dict:
    """Apple's ~3B on-device model via apfel's OpenAI-compatible server.

    Server mode, not the CLI: a per-case `apfel` spawn would pay session setup 100
    times over and make latency incomparable to ollama's warm daemon. Note the
    server defaults to ollama's own port, so the eval expects it on another one.

    When a schema is given the request uses guided generation (constrained
    decoding), which is the deployment-realistic setup and the closest analogue to
    the `format:json` the ollama side gets — stronger, in fact, since the label
    enum is enforced during sampling rather than checked afterwards.
    """
    body = {
        "model": "apple-foundationmodel",
        "messages": [{"role": "system", "content": system},
                     {"role": "user", "content": user}],
        "stream": False, "temperature": 0.1, "max_tokens": num_predict,
    }
    if schema_path and os.path.exists(schema_path):
        body["response_format"] = {"type": "json_schema", "json_schema": {
            "name": "verdict", "strict": True, "schema": json.load(open(schema_path))}}
    else:
        body["response_format"] = {"type": "json_object"}
    req = urllib.request.Request(f"{base_url.rstrip('/')}/v1/chat/completions",
                                 data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    t0 = time.monotonic_ns()
    try:
        d = json.loads(urllib.request.urlopen(req, timeout=300).read())
        ns = time.monotonic_ns() - t0
        choice = (d.get("choices") or [{}])[0]
        text = (choice.get("message") or {}).get("content") or ""
        finish = choice.get("finish_reason")
        out_tok = (d.get("usage") or {}).get("completion_tokens") or max(1, len(text) // 4)
        return {"text": text, "done_reason": finish, "eval_count": out_tok,
                "eval_duration": ns}
    except Exception as e:  # server down / model unavailable / guardrail refusal
        return {"text": "", "done_reason": f"error:{e}", "eval_count": 0, "eval_duration": 0}


def generate(args, model: str, system: str, user: str, num_predict: int,
             schema_path: str = None) -> dict:
    if is_apple(model):
        return apfel_generate(args.apple_base_url, system, user, num_predict, schema_path)
    return ollama_generate(args.base_url, model, system, user, num_predict)


# ---------------------------------------------------------------------------
# classify — REAL golden cases from mail_assist.duckdb
# ---------------------------------------------------------------------------

def load_classify_cases(db_path: str, per_label: int, seed: int):
    import duckdb
    if not os.path.exists(db_path):
        sys.exit(f"mail_assist.duckdb not found at {db_path} (pass --db)")
    c = duckdb.connect(db_path, read_only=True)
    # Latest annotation per thread = the current golden label.
    rows = c.execute(
        """
        with latest as (
            select *, row_number() over (partition by thread_id order by updated_at desc) rn
            from mail_annotations
        )
        select a.thread_id, a.label, a.state, a.confidence,
               t.provider, t.lane, t.subject, t.latest_from_name, t.latest_from_address,
               t.recipient_domains_json, t.label_ids_json, t.message_count, t.last_message_at,
               t.latest_summary,
               m.message_id, m.direction, m.intent, m.needs_reply_hint, m.follow_up_hint_json,
               m.summary as msg_summary, m.internal_date
        from latest a
        join mail_threads t on a.thread_id=t.thread_id and a.principal=t.principal
        left join mail_messages m
             on m.thread_id=t.thread_id and m.principal=t.principal
             and m.internal_date = (select max(m2.internal_date) from mail_messages m2
                                    where m2.thread_id=t.thread_id and m2.principal=t.principal)
        where a.rn=1 and a.label in ('needs_reply','follow_up','fyi','no_action')
          and t.latest_summary is not null and length(t.latest_summary) > 20
        """
    ).fetchdf()
    # Balanced sample per label, deterministic by seed.
    cases = []
    for label in CLASSIFY_LABELS:
        sub = rows[rows["label"] == label]
        if len(sub):
            sub = sub.sample(min(per_label, len(sub)), random_state=seed)
            cases.extend(sub.to_dict("records"))
    return cases


def age_words(ts_ms, now_ms):
    if not ts_ms:
        return "unknown"
    days = max(0, int(now_ms) - int(ts_ms)) // 86_400_000
    return "today" if days == 0 else ("1 day ago" if days == 1 else f"{days} days ago")


def render_classify_user(tmpl: str, row: dict) -> str:
    def js(v):
        try:
            return ", ".join(json.loads(v)) if v else "(none)"
        except Exception:
            return "(none)"
    now = row.get("last_message_at") or 0
    sender = f"{row.get('latest_from_name') or '(unknown)'} <{row.get('latest_from_address') or ''}>"
    channel = "email" if (row.get("provider") or "").startswith(("gmail", "agentmail")) else "chat"
    fields = {
        "channel": channel,
        "lane": "the agent's (Presto's)" if row.get("lane") == "envoy" else "the owner's",
        "subject": row.get("subject") or "(none)",
        "sender": sender,
        "recipient_domains": js(row.get("recipient_domains_json")),
        "label_ids": js(row.get("label_ids_json")),
        "message_count": row.get("message_count") or 1,
        "age": age_words(row.get("last_message_at"), now),
        "latest_message_id": row.get("message_id") or "unknown",
        "latest_message_age": age_words(row.get("internal_date"), now),
        "latest_direction": row.get("direction") or "unknown",
        "latest_intent": row.get("intent") or "unknown",
        "needs_reply_hint": str(bool(row.get("needs_reply_hint"))).lower(),
        "follow_up_hint": row.get("follow_up_hint_json") or "none",
        "recent_handled_followups": "none",
        "summary": row.get("latest_summary") or row.get("msg_summary") or "",
    }
    out = tmpl
    for k, v in fields.items():
        out = out.replace("{" + k + "}", str(v))
    return out


def grade_classify(text: str, golden: str):
    o = parse_obj(text)
    if o is None:
        return {"valid": False, "agree": False, "pred": None, "conf": None, "why": "invalid JSON"}
    pred = o.get("label")
    if pred not in CLASSIFY_LABELS or "confidence" not in o or "reason" not in o:
        return {"valid": False, "agree": False, "pred": pred, "conf": o.get("confidence"),
                "why": "bad/missing fields"}
    return {"valid": True, "agree": pred == golden, "pred": pred,
            "conf": o.get("confidence"), "why": "ok"}


# ---------------------------------------------------------------------------
# distill — SYNTHETIC cases (raw inputs are not retained)
# ---------------------------------------------------------------------------

DISTILL_CASES = [
    {"id": "bank_multichange", "facts": ["40,000", "3.5%", "5,000", "4412", "Jul 28", "18,432"],
     "user": "Channel: chat. Newest-first.\n[1] From: Acme Bank | Subject: Statement + 3 policy changes\n"
             "Body: Effective Aug 1: ATM cap 25,000->40,000 INR/day; intl fee 2.5%->3.5%; min balance "
             "10,000->5,000. Autopay card ending 4412 on Jul 28, amount 18,432.10 INR."},
    {"id": "meeting_request", "facts": ["Thursday", "3pm", "Q3"],
     "user": "Channel: chat. Newest-first.\n[1] From: Sam | Subject: Q3 review\n"
             "Body: Can we meet Thursday 3pm to review the Q3 numbers? Confirm or propose another time."},
    {"id": "receipt", "facts": ["A123", "Tuesday"],
     "user": "Channel: chat. Newest-first.\n[1] From: ShopMart | Subject: Shipped\n"
             "Body: Order A123 shipped, arriving Tuesday."},
    {"id": "deadline", "facts": ["Aug 15", "renew", "passport"],
     "user": "Channel: chat. Newest-first.\n[1] From: Gov Portal | Subject: Action required\n"
             "Body: Your passport expires soon; renew before Aug 15 to avoid a lapse."},
    {"id": "invoice", "facts": ["INV-7781", "45,200", "Net 30"],
     "user": "Channel: chat. Newest-first.\n[1] From: Vendor | Subject: Invoice INV-7781\n"
             "Body: Invoice INV-7781 for 45,200 INR is due Net 30 from receipt."},
]


def grade_distill(text: str):
    o = parse_obj(text)
    if o is None:
        return {"valid": False, "schema": False, "recall": 0.0, "why": "invalid JSON"}
    schema_ok = "summary" in o and "intent" in o
    return {"valid": True, "schema": schema_ok, "obj": o, "why": "ok"}


def distill_recall(o: dict, facts: list) -> float:
    blob = json.dumps(o, ensure_ascii=False).lower()
    hit = sum(1 for f in facts if f.lower() in blob)
    return hit / max(1, len(facts))


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------

def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--models", default="gemma4:12b",
                    help="comma list of candidates (default: gemma4:12b baseline). An "
                         "`apple:foundationmodel` entry routes to apfel instead of ollama.")
    ap.add_argument("--candidate", help="convenience: compare the gemma4:12b baseline against this one")
    ap.add_argument("--stop-stack", action="store_true",
                    help="if the magician stack is running, stop it (make stop-supervisor) before the eval")
    ap.add_argument("--db", default=default_db())
    ap.add_argument("--base-url", default=os.environ.get("OLLAMA_BASE_URL", "http://localhost:11434"))
    ap.add_argument("--apple-base-url",
                    default=os.environ.get("APFEL_BASE_URL", "http://127.0.0.1:11500"),
                    help="apfel --serve base URL for `apple:*` candidates. Not 11434: that "
                         "is ollama's port and apfel defaults to it, so a side-by-side run "
                         "needs apfel started with `--port 11500`.")
    ap.add_argument("--classify-samples", type=int, default=100, help="total real classify cases (balanced 4 labels)")
    ap.add_argument("--distill-samples", type=int, default=25, help="synthetic distill runs (cases cycled)")
    ap.add_argument("--iterations", type=int, default=1, help="repeats per classify case")
    ap.add_argument("--only", choices=["classify", "distill"], help="run just one use case")
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--report", help="write a JSONL verdict report here (no personal content)")
    args = ap.parse_args()

    if args.candidate:
        models = ["gemma4:12b"] + ([args.candidate] if args.candidate != "gemma4:12b" else [])
    else:
        models = [m.strip() for m in args.models.split(",") if m.strip()]

    # The eval must own ollama exclusively — require the stack down, bring ollama up.
    stopped_stack = preflight(args.base_url, args.stop_stack,
                              require_ollama=not all(is_apple(m) for m in models))

    run_classify = args.only in (None, "classify")
    run_distill = args.only in (None, "distill")
    verdicts = []

    if run_classify:
        sys_c, fc = latest_prompt("channel_classify_system_v*.json")
        usr_c, _ = latest_prompt("channel_classify_user_v*.json")
        cases = load_classify_cases(args.db, max(1, args.classify_samples // 4), args.seed)
        print(f"\n### classify — {len(cases)} REAL cases (prompt {fc}); golden = stored label\n")
        header = f"{'model':<26} valid%   agree%   agree%(human)   err%   by-label agree"
        print(header); print("-" * len(header))
        for model in models:
            v = a = n = err = 0
            hv = ha = 0
            gen_tok = gen_ns = 0
            per = {L: [0, 0] for L in CLASSIFY_LABELS}
            with ResSampler(proc="apfel" if is_apple(model) else "ollama") as rs:
                for row in cases:
                    cid = hashlib.sha1((str(row["thread_id"])).encode()).hexdigest()[:8]
                    for _ in range(args.iterations):
                        user = render_classify_user(usr_c, row)
                        out = generate(args, model, sys_c, user, 2048, CLASSIFY_SCHEMA)
                        err += str(out["done_reason"]).startswith("error:")
                        gen_tok += out["eval_count"]; gen_ns += out["eval_duration"]
                        g = grade_classify(out["text"], row["label"])
                        n += 1; v += g["valid"]; a += g["agree"]
                        per[row["label"]][1] += 1; per[row["label"]][0] += g["agree"]
                        if row["state"] in HUMAN_TOUCHED:
                            hv += 1; ha += g["agree"]
                        verdicts.append({"use": "classify", "model": model, "case": cid,
                                         "golden": row["label"], "state": row["state"], **{k: g[k] for k in ("valid", "agree", "pred", "conf")}})
                fp = 0.0 if is_apple(model) else footprint(args.base_url, model)
            ca, cp, rp = rs.summary()
            by = " ".join(f"{L[:4]}:{(per[L][0]/per[L][1]*100 if per[L][1] else 0):.0f}" for L in CLASSIFY_LABELS)
            print(f"{model:<26} {v/n*100:5.1f}   {a/n*100:5.1f}   {(ha/hv*100 if hv else 0):9.1f}   {err/n*100:4.0f}   {by}")
            print(f"{'':<26} ↳ {tok_per_s(gen_tok, gen_ns):.1f} tok/s | RAM {fp:.1f}GB | "
                  f"ollama CPU {ca:.0f}%/{cp:.0f}% avg/peak | RSS peak {rp:.1f}GB")
            if not is_apple(model):
                unload(args.base_url, model)  # free RAM before the next candidate

    if run_distill:
        sys_d, fd = latest_prompt("channel_ingest_distill_system_v*.json")
        print(f"\n### distill — SYNTHETIC (raw inputs discarded by design); prompt {fd}\n")
        header = f"{'model':<26} valid%   schema%   fact-recall%   err%   deg(hit num_predict)"
        print(header); print("-" * len(header))
        seq = [DISTILL_CASES[i % len(DISTILL_CASES)] for i in range(args.distill_samples)]
        for model in models:
            v = s = n = deg = err = 0
            rec = 0.0
            gen_tok = gen_ns = 0
            with ResSampler(proc="apfel" if is_apple(model) else "ollama") as rs:
                for case in seq:
                    out = generate(args, model, sys_d, case["user"], 4096)
                    err += str(out["done_reason"]).startswith("error:")
                    gen_tok += out["eval_count"]; gen_ns += out["eval_duration"]
                    g = grade_distill(out["text"])
                    n += 1; v += g["valid"]; s += g.get("schema", False)
                    deg += out["done_reason"] == "length"
                    r = distill_recall(g["obj"], case["facts"]) if g["valid"] else 0.0
                    rec += r
                    verdicts.append({"use": "distill", "model": model, "case": case["id"],
                                     "valid": g["valid"], "schema": g.get("schema", False), "recall": round(r, 2)})
                fp = 0.0 if is_apple(model) else footprint(args.base_url, model)
            ca, cp, rp = rs.summary()
            print(f"{model:<26} {v/n*100:5.1f}   {s/n*100:6.1f}   {rec/n*100:9.1f}   {err/n*100:4.0f}   {deg}/{n}")
            print(f"{'':<26} ↳ {tok_per_s(gen_tok, gen_ns):.1f} tok/s | RAM {fp:.1f}GB | "
                  f"ollama CPU {ca:.0f}%/{cp:.0f}% avg/peak | RSS peak {rp:.1f}GB")
            if not is_apple(model):
                unload(args.base_url, model)  # free RAM before the next candidate

    if args.report:
        with open(args.report, "w") as f:
            for row in verdicts:
                f.write(json.dumps(row) + "\n")
        print(f"\nwrote {len(verdicts)} verdicts -> {args.report}")
    print("\nHigher valid% and agree% = safer replacement. deg>0 = repetition/truncation risk.")
    print("↳ line = resource cost: tok/s (generation throughput), RAM (model's unified-memory "
          "footprint from /api/ps), and ollama CPU%/RSS sampled during the run "
          "(GPU/ANE util needs sudo powermetrics, so it is not sampled).")
    print("err% = runner unreachable or model not pulled (NOT a model-quality signal) — "
          "start ollama and `ollama pull <model>`, or start apfel, then re-run.")
    print("CAVEAT (apple:*): tok/s is WALL-CLOCK per request (prompt processing + decode), "
          "while ollama's is decode-only from eval_duration. The apple number is therefore "
          "pessimistic and the two are not directly comparable; compare agree% freely, "
          "tok/s only as an order-of-magnitude. RAM shows 0.0 because the on-device model "
          "is not resident in the process (/api/ps has no apfel analogue).")
    if stopped_stack:
        print("\nNOTE: this eval stopped the magician stack — restart it with `make run-supervisor`.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
