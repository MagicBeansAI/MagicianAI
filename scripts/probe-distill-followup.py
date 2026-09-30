#!/usr/bin/env python3
"""Follow-up probes:
  1. gemma4:12b distill — dump the FULL raw ollama response + all completion
     fields + the exact json.loads error, to see why the baseline is 'invalid'.
  2. gemma-4-31B Q3 distill with think=false and a longer timeout — is the
     earlier timeout a thinking-mode runaway?
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


def raw(model, system, user, *, think=None, num_predict=4096, timeout=900):
    body = {"model": model, "prompt": f"{system}\n\n{user}", "stream": False, "format": "json",
            "options": {"num_ctx": 32768, "num_predict": num_predict, "temperature": 0.1}}
    if think is not None:
        body["think"] = think
    req = urllib.request.Request(f"{BASE}/api/generate", data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    try:
        return json.loads(urllib.request.urlopen(req, timeout=timeout).read())
    except Exception as e:
        return {"_transport_error": str(e)}


def main():
    sys_d, fd = gec.latest_prompt("channel_ingest_distill_system_v*.json")
    case = gec.DISTILL_CASES[0]  # bank_multichange

    print("=" * 70)
    print("1) gemma4:12b distill — full raw response")
    print("=" * 70)
    d = raw("gemma4:12b", sys_d, case["user"])
    if "_transport_error" in d:
        print("transport error:", d["_transport_error"])
    else:
        txt = d.get("response", "")
        print(f"done={d.get('done')} done_reason={d.get('done_reason')} "
              f"eval_count={d.get('eval_count')} prompt_eval_count={d.get('prompt_eval_count')}")
        print(f"response length: {len(txt)} chars")
        # exact parse error
        try:
            o = json.loads(txt)
            print(f"json.loads OK -> type={type(o).__name__}, keys={list(o)[:12] if isinstance(o, dict) else 'N/A'}")
        except Exception as e:
            print(f"json.loads ERROR: {e}")
        print("\n--- FULL response text ---")
        print(txt)
        print("--- end (last 300 chars) ---")
        print(repr(txt[-300:]))
    gec.unload(BASE, "gemma4:12b")

    print("\n" + "=" * 70)
    print("2) gemma-4-31B Q3 distill — think=false, timeout=900s")
    print("=" * 70)
    d = raw("hf.co/unsloth/gemma-4-31B-it-GGUF:Q3_K_M", sys_d, case["user"], think=False, timeout=900)
    if "_transport_error" in d:
        print("transport error:", d["_transport_error"])
    else:
        txt = d.get("response", "")
        g = gec.grade_distill(txt)
        rec = gec.distill_recall(g["obj"], case["facts"]) if g["valid"] else 0.0
        print(f"done_reason={d.get('done_reason')} eval_count={d.get('eval_count')} "
              f"resp={len(txt)}ch thinking={len(d.get('thinking') or '')}ch")
        print(f"valid={g['valid']} schema={g.get('schema')} recall={rec:.0%}")
        print(f"response[:500]: {txt[:500]!r}")
    gec.unload(BASE, "hf.co/unsloth/gemma-4-31B-it-GGUF:Q3_K_M")
    print("\n[done]")


if __name__ == "__main__":
    main()
