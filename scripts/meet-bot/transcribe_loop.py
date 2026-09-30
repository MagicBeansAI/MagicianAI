#!/usr/bin/env python3
"""
Spike 1 — meeting "ears": capture meeting audio (BlackHole) → chunked STT →
rolling transcript file → periodic local-Gemma summary. See the meet-bot design
doc §9 (Spike 1).

This is the throwaway "prove the hearing pipeline" cut. It reuses the configured
OpenAI key for STT (gpt-transcribe) and the local Ollama/Gemma summarizer —
no new Rust, no model download. The PRODUCTION path is a native Core-Audio
bridge → StreamingSttProvider → MeetingSession (the Rust scaffold's TODO hooks).

Capture is continuous (ffmpeg segment muxer, no gaps); each finished chunk is
transcribed as the next one records.

Prereqs:
  • macOS default OUTPUT = "BlackHole 2ch" so the meeting audio flows there:
        SwitchAudioSource -s "BlackHole 2ch" -t output
    (use a Multi-Output Device — Speakers + BlackHole 2ch — if you also want to
    hear the meeting while it's captured.)
  • OPENAI_API_KEY in the environment.
  • Ollama running with the summary model (run scripts/setup-meet-bot.sh first).

Usage:
  python3 scripts/meet-bot/transcribe_loop.py
Then join your Meet and talk. Ctrl-C to stop (prints a final summary).

Tunables (env):
  MEET_BOT_AUDIO_INDEX     ffmpeg avfoundation index of BlackHole 2ch (default 2;
                           re-check with: ffmpeg -f avfoundation -list_devices true -i "")
  MEET_BOT_CHUNK_SEC       seconds per chunk (default 15)
  MEET_BOT_STT_MODEL       OpenAI STT model (default gpt-transcribe)
  MEET_BOT_SUMMARIZE_EVERY summarize every N chunks (default 4)
  MEET_BOT_TRANSCRIPT      transcript file (default /tmp/meet_transcript.txt)
  MEET_BOT_OLLAMA_URL / MEET_BOT_SUMMARY_MODEL   explicit summarizer overrides

Respond-on-wake (Spike 2 — opt in with MEET_BOT_RESPOND=1):
  When enabled, each chunk is scanned for the "Hey Magican" wake phrase; on a
  hit the bot asks local Gemma for a short spoken reply (grounded in
  the rolling summary + recent transcript) and injects it into the meeting's mic
  via `say` -> `sox -t coreaudio "BlackHole 16ch"`. The reply goes ONLY to
  BlackHole 16ch, so it never disturbs the 2ch capture (no echo / no default
  flip). Throwaway responder; production uses the OpenAI-Realtime path + a
  sherpa-onnx wake word wired into MeetingSession. Needs `sox` + `say` on PATH.
  MEET_BOT_RESPOND          1 to enable the responder (default 0 = ears only)
  MEET_BOT_WAKE_PHRASES     comma-separated wake phrases (default "hey magican")
  MEET_BOT_INJECT_DEVICE    CoreAudio output = Meet's mic (default "BlackHole 16ch")
  MEET_BOT_RESPONDER_MODEL  Ollama model for replies (default = summary model)
  MEET_BOT_VOICE            macOS `say` voice (default = system voice)
"""
import glob
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request
from datetime import datetime
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "scripts"))
from ollama_config import resolve_operation  # noqa: E402

AUDIO_INDEX = os.environ.get("MEET_BOT_AUDIO_INDEX", "2")
CHUNK_SEC = int(os.environ.get("MEET_BOT_CHUNK_SEC", "15"))
STT_MODEL = os.environ.get("MEET_BOT_STT_MODEL", "gpt-transcribe")
SUMMARIZE_EVERY = int(os.environ.get("MEET_BOT_SUMMARIZE_EVERY", "4"))
TRANSCRIPT = os.environ.get("MEET_BOT_TRANSCRIPT", "/tmp/meet_transcript.txt")
SUMMARY_CONFIG_MODEL, SUMMARY_CONTEXT, SUMMARY_ENDPOINT = resolve_operation(
    REPO_ROOT, "meeting_summary"
)
RESPONSE_CONFIG_MODEL, RESPONSE_CONTEXT, RESPONSE_ENDPOINT = resolve_operation(
    REPO_ROOT, "meeting_response"
)
SUMMARY_OLLAMA_URL = os.environ.get(
    "MEET_BOT_OLLAMA_URL", SUMMARY_ENDPOINT.removesuffix("/api/generate")
)
RESPONSE_OLLAMA_URL = os.environ.get(
    "MEET_BOT_OLLAMA_URL", RESPONSE_ENDPOINT.removesuffix("/api/generate")
)
SUMMARY_MODEL = os.environ.get("MEET_BOT_SUMMARY_MODEL", SUMMARY_CONFIG_MODEL)
SUMMARY_CONTEXT = int(os.environ.get("MEET_BOT_SUMMARY_CONTEXT_TOKENS", SUMMARY_CONTEXT))
OPENAI_KEY = os.environ.get("OPENAI_API_KEY", "")

# Respond-on-wake (Spike 2) — opt in; ears-only behaviour is unchanged when off.
RESPOND = os.environ.get("MEET_BOT_RESPOND", "0").lower() not in ("", "0", "false", "no")
WAKE_PHRASES = [
    p.strip().lower()
    for p in os.environ.get("MEET_BOT_WAKE_PHRASES", "hey magican").split(",")
    if p.strip()
]
INJECT_DEVICE = os.environ.get("MEET_BOT_INJECT_DEVICE", "BlackHole 16ch")
RESPONDER_MODEL = os.environ.get("MEET_BOT_RESPONDER_MODEL", RESPONSE_CONFIG_MODEL)
RESPONDER_CONTEXT = int(os.environ.get("MEET_BOT_RESPONDER_CONTEXT_TOKENS", RESPONSE_CONTEXT))
VOICE = os.environ.get("MEET_BOT_VOICE", "")

# Whisper/gpt-transcribe routinely hallucinate these on silent chunks — drop them.
SILENCE_NOISE = {"", "you", "thank you.", "thanks for watching!", "okay.", "."}

SUMMARY_PROMPT = (
    "You are a meeting assistant. Summarize the meeting transcript (it may mix "
    "English and Hindi / Hinglish). Write the summary in ENGLISH but preserve "
    "names and key terms verbatim. Be faithful — no omissions, no invention. "
    "Output:\n## Summary (3-4 sentences)\n## Decisions\n## Action items (with owner if stated)"
)

RESPONDER_PROMPT = (
    "You are Presto, an AI assistant sitting in on a live meeting. A participant "
    "addressed you directly by name. Using the meeting context, give a helpful, "
    "concise reply in 2-4 sentences of spoken English — no markdown, no lists, no "
    "stage directions; it will be read aloud into the meeting. If there is no clear "
    "question, briefly offer the most useful recap or next step."
)


def need(bin_name: str) -> None:
    if shutil.which(bin_name) is None:
        sys.exit(f"error: `{bin_name}` not found on PATH.")


def transcribe(wav_path: str) -> str:
    """One-shot STT via OpenAI (chunked). Swappable for Deepgram/local-whisper."""
    proc = subprocess.run(
        [
            "curl", "-sS", "https://api.openai.com/v1/audio/transcriptions",
            "-H", f"Authorization: Bearer {OPENAI_KEY}",
            "-F", f"file=@{wav_path}",
            "-F", f"model={STT_MODEL}",
            "-F", "response_format=json",
        ],
        capture_output=True, text=True,
    )
    try:
        return (json.loads(proc.stdout).get("text") or "").strip()
    except Exception:
        sys.stderr.write(f"[stt] unparseable response: {proc.stdout[:200]}\n")
        return ""


def summarize(transcript: str) -> str:
    payload = json.dumps({
        "model": SUMMARY_MODEL,
        "messages": [
            {"role": "system", "content": SUMMARY_PROMPT},
            {"role": "user", "content": transcript},
        ],
        "stream": False,
        "options": {"num_ctx": SUMMARY_CONTEXT, "temperature": 0.2},
    }).encode()
    req = urllib.request.Request(
        f"{SUMMARY_OLLAMA_URL}/api/chat",
        data=payload,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=300) as resp:
        return json.loads(resp.read()).get("message", {}).get("content", "")


def is_addressed(text: str) -> bool:
    """True if a wake phrase appears in the chunk (punctuation/case-insensitive)."""
    norm = re.sub(r"\s+", " ", re.sub(r"[^a-z0-9 ]+", " ", text.lower())).strip()
    return any(phrase in norm for phrase in WAKE_PHRASES)


def answer(question: str, context: str) -> str:
    """Short spoken reply from local Gemma, grounded in the meeting context."""
    payload = json.dumps({
        "model": RESPONDER_MODEL,
        "messages": [
            {"role": "system", "content": RESPONDER_PROMPT},
            {"role": "user", "content": (
                f"Meeting context so far:\n{context}\n\n"
                f"A participant just addressed you and said:\n\"{question}\"\n\n"
                "Your spoken reply:"
            )},
        ],
        "stream": False,
        "options": {"num_ctx": RESPONDER_CONTEXT, "temperature": 0.4},
    }).encode()
    req = urllib.request.Request(
        f"{RESPONSE_OLLAMA_URL}/api/chat",
        data=payload,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=300) as resp:
        return json.loads(resp.read()).get("message", {}).get("content", "").strip()


def speak_into_meeting(text: str) -> None:
    """Render the reply with macOS `say`, then inject it to Meet's mic (BlackHole
    16ch) by name via sox — the default output (2ch capture) is left untouched."""
    aiff = "/tmp/meetbot_answer.aiff"
    say_cmd = ["say", "-o", aiff] + (["-v", VOICE] if VOICE else []) + [text]
    subprocess.run(say_cmd, check=False)
    subprocess.run(["sox", aiff, "-t", "coreaudio", INJECT_DEVICE], check=False)


def main() -> None:
    if not OPENAI_KEY:
        sys.exit("error: OPENAI_API_KEY is not set (needed for STT).")
    need("ffmpeg")
    need("curl")
    if RESPOND:
        need("say")
        need("sox")

    responder_line = (
        f"   responder: ON — wake {WAKE_PHRASES} -> {RESPONDER_MODEL} -> say|sox -> \"{INJECT_DEVICE}\"\n"
        if RESPOND else
        "   responder: OFF (ears only; set MEET_BOT_RESPOND=1 to answer on wake)\n"
    )
    print(f"== Meet bot — ears{' + respond-on-wake' if RESPOND else ''} ==\n"
          f"   capture: BlackHole 2ch (avfoundation :{AUDIO_INDEX}), {CHUNK_SEC}s chunks\n"
          f"   STT:     {STT_MODEL}   summary: {SUMMARY_MODEL} every {SUMMARIZE_EVERY} chunks\n"
          f"{responder_line}"
          f"   transcript -> {TRANSCRIPT}\n"
          f"   (ensure default OUTPUT is BlackHole 2ch; join your Meet; Ctrl-C to stop)\n")

    chunk_dir = tempfile.mkdtemp(prefix="meetbot_chunks_")
    pattern = os.path.join(chunk_dir, "chunk_%05d.wav")
    # Continuous, gapless capture into 15s segments.
    ff = subprocess.Popen(
        [
            "ffmpeg", "-hide_banner", "-loglevel", "error",
            "-f", "avfoundation", "-i", f":{AUDIO_INDEX}",
            "-ar", "16000", "-ac", "1",
            "-f", "segment", "-segment_time", str(CHUNK_SEC), "-reset_timestamps", "1",
            pattern,
        ]
    )
    open(TRANSCRIPT, "w").close()
    processed: set[str] = set()
    full: list[str] = []
    n = 0
    last_summary = ""

    def handle(path: str) -> None:
        nonlocal n, last_summary
        text = transcribe(path)
        if text.lower() in SILENCE_NOISE:
            return
        stamp = datetime.now().strftime("%H:%M:%S")
        line = f"[{stamp}] {text}"
        full.append(line)
        with open(TRANSCRIPT, "a") as fh:
            fh.write(line + "\n")
        print(line, flush=True)
        n += 1
        if n % SUMMARIZE_EVERY == 0:
            print("\n----- rolling summary -----")
            try:
                last_summary = summarize("\n".join(full))
                print(last_summary, flush=True)
            except Exception as exc:
                sys.stderr.write(f"[summary] failed: {exc}\n")
            print("---------------------------\n", flush=True)
        if RESPOND and is_addressed(text):
            print("\n----- responder (wake word) -----", flush=True)
            try:
                ctx = f"Summary so far:\n{last_summary}\n\n" if last_summary else ""
                ctx += "Recent transcript:\n" + "\n".join(full[-25:])
                reply = answer(text, ctx)
                print(f"[Presto] {reply}", flush=True)
                speak_into_meeting(reply)
            except Exception as exc:
                sys.stderr.write(f"[responder] failed: {exc}\n")
            print("---------------------------------\n", flush=True)

    try:
        while True:
            time.sleep(1)
            files = sorted(glob.glob(os.path.join(chunk_dir, "chunk_*.wav")))
            # All but the last are complete (the last is still being written).
            for f in files[:-1]:
                if f not in processed:
                    processed.add(f)
                    handle(f)
    except KeyboardInterrupt:
        print("\n== stopping; flushing final chunk + summary ==", flush=True)
        ff.terminate()
        try:
            ff.wait(timeout=5)
        except Exception:
            ff.kill()
        for f in sorted(glob.glob(os.path.join(chunk_dir, "chunk_*.wav"))):
            if f not in processed:
                processed.add(f)
                handle(f)
        if full:
            print("\n===== FINAL SUMMARY =====")
            try:
                print(summarize("\n".join(full)))
            except Exception as exc:
                sys.stderr.write(f"[summary] failed: {exc}\n")
        print(f"\nFull transcript: {TRANSCRIPT}")
    finally:
        if ff.poll() is None:
            ff.terminate()


if __name__ == "__main__":
    main()
