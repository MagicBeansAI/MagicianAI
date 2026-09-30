#!/usr/bin/env python3
"""Distill diagnostic — figure out WHY a candidate model fails the channel
distill task before committing to a 100-case run.

Runs 1-2 synthetic distill cases (NCASES env, default 1) through each model and
prints the RAW output so failure modes are visible:
  - degeneration : done_reason == "length"  (repetition to the token cap)
  - thinking-diversion : response empty, JSON went to the `thinking` field
  - bad schema  : valid JSON but missing summary/intent
  - poor recall : summary dropped the key amounts/dates/ids

Models: gemma4:12b (baseline), gemma-26 (a4b), gemma-31; and BOTH qwen quants
(Q4_K_M, Q3_K_M) in thinking-ON and thinking-OFF. One model resident at a time
(unloaded between) so two big models never contend for RAM.
"""
import importlib.util
import json
import os
import urllib.request

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
spec = importlib.util.spec_from_file_location("gec", os.path.join(REPO, "scripts", "golden-eval-channel.py"))
gec = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gec)

BASE = os.environ.get("OLLAMA_BASE_URL", "http://localhost:11434")


def gen(model, system, user, *, think=None, num_predict=4096):
    body = {"model": model, "prompt": f"{system}\n\n{user}", "stream": False, "format": "json",
            "options": {"num_ctx": 32768, "num_predict": num_predict, "temperature": 0.1}}
    if think is not None:
        body["think"] = think
    req = urllib.request.Request(f"{BASE}/api/generate", data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    try:
        d = json.loads(urllib.request.urlopen(req, timeout=600).read())
        return {"text": d.get("response", ""), "thinking": d.get("thinking") or "",
                "done_reason": d.get("done_reason"), "eval_count": d.get("eval_count") or 0}
    except Exception as e:
        return {"text": "", "thinking": "", "done_reason": f"error:{e}", "eval_count": 0}


def show(tag, r, facts):
    g = gec.grade_distill(r["text"])
    recall = gec.distill_recall(g["obj"], facts) if g["valid"] else 0.0
    if g["valid"] and g.get("schema"):
        verdict = "OK"
    elif g["valid"]:
        verdict = "BAD(valid JSON but missing summary/intent)"
    else:
        verdict = f"BAD({g['why']})"
    deg = "DEGEN:hit-cap" if r["done_reason"] == "length" else str(r["done_reason"])
    print(tag)
    print(f"  valid={g['valid']} schema={g.get('schema')} recall={recall:.0%} | "
          f"done={deg} eval_count={r['eval_count']} | resp={len(r['text'])}ch "
          f"thinking={len(r['thinking'])}ch  ->  {verdict}")
    if r["thinking"]:
        print(f"  [thinking][:200]: {r['thinking'][:200]!r}")
    print(f"  [response][:500]: {r['text'][:500]!r}")


def main():
    sys_d, fd = gec.latest_prompt("channel_ingest_distill_system_v*.json")
    ncases = int(os.environ.get("NCASES", "1"))
    cases = gec.DISTILL_CASES[:ncases]
    print(f"distill prompt: {os.path.basename(fd)}")
    print(f"cases: {[c['id'] for c in cases]}  (num_predict=4096, same as the real eval)\n")

    runs = [
        ("gemma4:12b", [None]),                                   # baseline
        ("gemma4:26b-a4b-it-q4_K_M", [None]),                     # gemma-26 (a4b)
        ("hf.co/unsloth/gemma-4-31B-it-GGUF:Q3_K_M", [None]),     # gemma-31
        ("qwen3.6:27b-q4_K_M", [True, False]),                    # qwen Q4: think ON + OFF
        ("hf.co/unsloth/Qwen3.6-27B-GGUF:Q3_K_M", [True, False]),  # qwen Q3: think ON + OFF
    ]
    for model, thinks in runs:
        for case in cases:
            for think in thinks:
                mode = "default" if think is None else ("think=ON" if think else "think=OFF")
                r = gen(model, sys_d, case["user"], think=think)
                show(f"\n--- {model}  [{mode}]  case={case['id']} ---", r, case["facts"])
        gec.unload(BASE, model)
    print("\n[done]")


if __name__ == "__main__":
    main()
