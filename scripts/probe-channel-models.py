#!/usr/bin/env python3
"""Manual format-probe for candidate channel-assist local models.

Before spending 30-50 min running the full golden eval on a model, this sends
ONE real classify case (exact production prompt + request shape) to each model
and prints the RAW response so we can eyeball whether it returns the expected
JSON schema at all. For thinking-capable models (Qwen3.6) it also probes the
think=on vs think=off variations, since default thinking mode emits reasoning
that breaks the strict-JSON contract.
"""
import importlib.util
import json
import os
import sys
import urllib.request

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
spec = importlib.util.spec_from_file_location("gec", os.path.join(REPO, "scripts", "golden-eval-channel.py"))
gec = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gec)

BASE = os.environ.get("OLLAMA_BASE_URL", "http://localhost:11434")


def gen(model, system, user, *, fmt_json=True, think=None, num_predict=512):
    """Raw /api/generate — mirrors gec.ollama_generate but lets us toggle
    format:json and add a top-level think flag (thinking models)."""
    body = {"model": model, "prompt": f"{system}\n\n{user}", "stream": False,
            "options": {"num_ctx": 32768, "num_predict": num_predict, "temperature": 0.1}}
    if fmt_json:
        body["format"] = "json"
    if think is not None:
        body["think"] = think
    req = urllib.request.Request(f"{BASE}/api/generate", data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    try:
        d = json.loads(urllib.request.urlopen(req, timeout=300).read())
        return {"text": d.get("response", ""), "thinking": d.get("thinking") or "",
                "done_reason": d.get("done_reason"), "eval_count": d.get("eval_count") or 0}
    except Exception as e:
        return {"text": "", "thinking": "", "done_reason": f"error:{e}", "eval_count": 0}


def show(tag, r):
    g = gec.grade_classify(r["text"], "needs_reply")  # golden irrelevant here; we care about valid
    verdict = "VALID-SCHEMA" if g["valid"] else f"BAD ({g['why']})"
    think_note = f"thinking-field={len(r['thinking'])}ch" if r["thinking"] else "thinking-field=empty"
    print(f"\n--- {tag} ---")
    print(f"  eval_count={r['eval_count']} done={r['done_reason']} | response={len(r['text'])}ch | {think_note} -> {verdict} pred={g.get('pred')}")
    if r["thinking"]:
        print(f"  [thinking][:160]: {r['thinking'][:160]!r}")
    print(f"  [response][:400]: {r['text'][:400]!r}")


def main():
    sys_txt, fc = gec.latest_prompt("channel_classify_system_v*.json")
    usr_tmpl, _ = gec.latest_prompt("channel_classify_user_v*.json")
    print(f"classify prompt: {os.path.basename(fc)}")

    cases = gec.load_classify_cases(gec.default_db(), per_label=1, seed=0)
    if not cases:
        sys.exit("no classify cases from DB")
    case = cases[0]
    user = gec.render_classify_user(usr_tmpl, case)
    print(f"probe case golden-label={case.get('label')}  (user prompt {len(user)} chars)")
    print("Each model x 3 variations: [default no-think-flag] / [think=false] / [think=true].")
    print("A model 'requires thinking off' iff default or think=true diverts its JSON into the")
    print("thinking-field (empty response -> BAD) while think=false yields a valid response.\n")

    # Reference baseline.
    show("gemma4:12b  [default]", gen("gemma4:12b", sys_txt, user))
    gec.unload(BASE, "gemma4:12b")

    # Both candidates through the SAME three variations, one model resident at a time.
    for m in ["hf.co/unsloth/gemma-4-31B-it-GGUF:Q3_K_M", "qwen3.6:27b-q4_K_M"]:
        show(f"{m}  [default: no think flag — what the ORIGINAL script sent]", gen(m, sys_txt, user))
        show(f"{m}  [think=false]", gen(m, sys_txt, user, think=False))
        show(f"{m}  [think=true]", gen(m, sys_txt, user, think=True))
        gec.unload(BASE, m)


if __name__ == "__main__":
    main()
