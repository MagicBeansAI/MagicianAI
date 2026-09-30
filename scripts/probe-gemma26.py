#!/usr/bin/env python3
"""gemma4:26b-a4b-it-q4_K_M distill — thinking OFF (and an explicit think=ON for
contrast), to see whether the earlier degeneration (default mode -> "s/he/it"
repetition to the token cap) changes when thinking is toggled."""
import importlib.util
import json
import os
import urllib.request

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
spec = importlib.util.spec_from_file_location("gec", os.path.join(REPO, "scripts", "golden-eval-channel.py"))
gec = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gec)
BASE = os.environ.get("OLLAMA_BASE_URL", "http://localhost:11434")
MODEL = "gemma4:26b-a4b-it-q4_K_M"


def gen(system, user, *, think=None, num_predict=4096):
    body = {"model": MODEL, "prompt": f"{system}\n\n{user}", "stream": False, "format": "json",
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


def show(mode, r, facts):
    g = gec.grade_distill(r["text"])
    rec = gec.distill_recall(g["obj"], facts) if g["valid"] else 0.0
    deg = "DEGEN:hit-cap" if r["done_reason"] == "length" else str(r["done_reason"])
    print(f"\n--- {MODEL} [{mode}] ---")
    print(f"  valid={g['valid']} schema={g.get('schema')} recall={rec:.0%} | "
          f"done={deg} eval_count={r['eval_count']} | resp={len(r['text'])}ch thinking={len(r['thinking'])}ch")
    print(f"  [response][:400]: {r['text'][:400]!r}")


def main():
    sys_d, _ = gec.latest_prompt("channel_ingest_distill_system_v*.json")
    case = gec.DISTILL_CASES[0]  # bank_multichange
    print(f"gemma-26 distill probe on case={case['id']} (num_predict=4096)")
    show("think=OFF", gen(sys_d, case["user"], think=False), case["facts"])
    show("think=ON", gen(sys_d, case["user"], think=True), case["facts"])
    gec.unload(BASE, MODEL)
    print("\n[done]")


if __name__ == "__main__":
    main()
