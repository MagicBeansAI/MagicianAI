#!/usr/bin/env python3
"""
summarize_spike.py — validate the configured local Gemma summarizer via Ollama on a
code-switched English+Hindi ("Hinglish") meeting transcript. Also the bake-off
tool for comparing models (see the meet-bot design doc §5.D).

Usage:
  python3 scripts/meet-bot/summarize_spike.py [MODEL] [TRANSCRIPT_FILE]
    MODEL            default: configured meeting_summary profile
    TRANSCRIPT_FILE  default: built-in Hinglish sample

Requires Ollama running on localhost:11434 with the model pulled.
"""
import json
import os
from pathlib import Path
import sys
import time
import urllib.request

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "scripts"))
from ollama_config import resolve_operation  # noqa: E402

CONFIG_MODEL, CONFIG_CONTEXT, CONFIG_ENDPOINT = resolve_operation(REPO_ROOT, "meeting_summary")
MODEL = sys.argv[1] if len(sys.argv) > 1 else CONFIG_MODEL
CONTEXT_TOKENS = int(os.environ.get("MEET_BOT_SUMMARY_CONTEXT_TOKENS", CONFIG_CONTEXT))

SAMPLE_TRANSCRIPT = """\
[10:01] Riya: Good morning everyone, let's start the standup. Aman, aap shuru karo.
[10:01] Aman: Sure. Kal maine payment gateway ka integration complete kiya. Razorpay webhook ab live hai, but ek edge case hai — refund ke time signature mismatch aa raha hai.
[10:02] Riya: Okay, that's a blocker for the launch. Kab tak fix ho jayega?
[10:02] Aman: Aaj shaam tak. I'll pair with Neha on it.
[10:03] Neha: Haan main help kar dungi. Also, maine dashboard ka latency 800ms se 300ms tak optimize kar diya — caching layer add ki.
[10:03] Riya: Great. Decision: we postpone the marketing push to Friday until the refund bug is fixed. Aman is the owner for the refund fix, Neha for the dashboard rollout.
[10:04] Vikram: One more thing — client ne pricing page pe Hindi support maanga hai. Should we scope it this sprint?
[10:04] Riya: Let's not — next sprint. Action item: Vikram, create a ticket for the Hindi pricing page.
"""

SYSTEM = (
    "You are a meeting assistant. Summarize the meeting transcript below. "
    "The transcript mixes English and Hindi (Hinglish/code-switching). "
    "Write the summary in ENGLISH, but preserve names, product names, and key "
    "terms verbatim (including any in Hindi). Be faithful: do NOT omit any "
    "decision or action item, and do NOT invent anything not in the transcript.\n\n"
    "Output exactly these sections:\n"
    "## Summary  (3-4 sentences)\n"
    "## Decisions  (bullets)\n"
    "## Action items  (bullets, with owner + due if stated)"
)


def main() -> None:
    transcript = SAMPLE_TRANSCRIPT
    if len(sys.argv) > 2:
        with open(sys.argv[2], encoding="utf-8") as fh:
            transcript = fh.read()

    payload = {
        "model": MODEL,
        "messages": [
            {"role": "system", "content": SYSTEM},
            {"role": "user", "content": transcript},
        ],
        "stream": False,
        "options": {"num_ctx": CONTEXT_TOKENS, "temperature": 0.2},
    }
    req = urllib.request.Request(
        CONFIG_ENDPOINT.replace("/api/generate", "/api/chat"),
        data=json.dumps(payload).encode("utf-8"),
        headers={"Content-Type": "application/json"},
    )
    print(f"== model: {MODEL} ==  (summarizing {len(transcript)} chars)\n")
    t0 = time.time()
    with urllib.request.urlopen(req, timeout=300) as resp:
        body = json.loads(resp.read().decode("utf-8"))
    dt = time.time() - t0
    print(body.get("message", {}).get("content", "(no content)"))
    print(f"\n--- {dt:.1f}s · prompt_eval={body.get('prompt_eval_count')} "
          f"eval={body.get('eval_count')} tokens ---")


if __name__ == "__main__":
    main()
