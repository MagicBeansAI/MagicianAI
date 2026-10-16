"""Voice delegation — the conformance rig's voice lane.

Invoked by eval-harness-conformance-live.py --lane voice. Drives a realtime
voice session the way the meeting responder does, without a human speaking:
`POST /media/sessions` → the control WebSocket `/media/voice/{id}/control` →
`session.start {realtime_profile}` → `session.ready` → one user turn →
frames until `delegate_to_chat.done` (or the timeout) → `session.end`.

Two drivers. `speech`, the default, says the prompt aloud (`say` → ffmpeg →
24 kHz mono PCM16) and streams it as binary frames — push-to-talk on a
`turn_detection: none` profile, server VAD otherwise. `text` sends the prompt
as `user.text {request_response: true}`, which reaches GPT Realtime as a
system item the model may decline to act on, GPT Live 1 only as commentary,
and Gemini Live not at all; it is selectable on Realtime only.

Each run is told as a story — what was asked, what the mouth did, whether
the person got it — and counted by outcome, not by method: a turn that ends
in the task the person asked for is `reached` whether the mouth delegated to
Magician, loaded the hand and did it itself, or anything in between; how it
got there is the `path` (delegated, self_served, declined, errored), reported
beside the outcome. Underneath, the proof gates stay as evidence: the
`delegate_to_chat.done` frame, the `voice.realtime` call row that names the
chat turn, the hands that ran on that turn, and the task. The title is three
spoken words so speech-to-text can carry it.
"""
from __future__ import annotations

import html
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import subprocess
import tempfile
import time
from urllib.parse import quote

PROFILES = ("voice_realtime_openai_backend", "voice_realtime_gpt_live_1",
            "voice_realtime_gemini_38_live", "voice_realtime_gemini_38_live_thinking")
# Which lane of the provider a typed prompt reaches: a user turn, or only a
# commentary item the model may speak but does not act on. Gemini Live keeps
# no runtime text injection at all.
TEXT_REACHES_USER_LANE = {"voice_realtime_openai_backend": True, "voice_realtime_gpt_live_1": False,
                          "voice_realtime_gemini_38_live": False, "voice_realtime_gemini_38_live_thinking": False}
DRIVERS = ("text", "speech")
DELEGATE_TOOL = "delegate_to_chat"
EFFECT_TOOL = "create_task"
REALTIME_SAMPLE_RATE = 24_000
REALTIME_FRAME_BYTES = 9_600  # 200 ms of 24 kHz mono s16le, the responder's frame
SPEECH_RATE_WPM = 160
# Words a speech synthesiser says and a transcriber hears the same way: two
# syllables, everyday, one spelling. ("cedar" came back as "seed", "garden"
# as "guard in", "orchid" as "or kid", "falcon" as "file in".)
SPOKEN_WORDS = ("banana", "pencil", "window", "purple", "silver", "monkey", "rocket", "yellow", "button", "candle",
                "dragon", "hammer", "jungle", "kitten", "lemon", "mirror", "pillow", "rabbit", "tiger", "turtle")
TERMINAL_FRAMES = {"delegate_to_chat.done", "session.error", "session.ended"}


def login(args, client):
    """Same rule as the other lanes: a bearer from the environment, else a
    session login from MAGICIAN_EVAL_USERNAME / MAGICIAN_EVAL_PASSWORD."""
    if os.environ.get("MAGICIAN_BEARER_TOKEN", "").strip():
        return
    username = os.environ.get("MAGICIAN_EVAL_USERNAME", "").strip()
    password = os.environ.get("MAGICIAN_EVAL_PASSWORD", "")
    if not username:
        raise RuntimeError("set MAGICIAN_BEARER_TOKEN or MAGICIAN_EVAL_USERNAME/MAGICIAN_EVAL_PASSWORD")
    status, raw, _ = client.request("POST", "/api/magician/v2/auth/login",
                                    body={"username": username, "password": password})
    if status not in (200, 201):
        raise RuntimeError(f"login refused with HTTP {status}")
    os.environ["MAGICIAN_BEARER_TOKEN"] = json.loads(raw)["token"]


# ---------------------------------------------------------------------------
# The probe: a spoken title, a prompt, and the drivers' audio
# ---------------------------------------------------------------------------


def word_nonce():
    """Three distinct words from the spoken list; the title they make is
    matched loosely, so it survives punctuation and case from a transcriber."""
    pool = list(SPOKEN_WORDS)
    words = []
    for _ in range(3):
        words.append(pool.pop(secrets.randbelow(len(pool))))
    return tuple(words)


def probe_title(words):
    return "Voice probe " + " ".join(words)


def transcript_heard(text, words):
    """Every nonce word, in any case or punctuation — the transcriber's
    accuracy on the words that matter."""
    heard = set(re.findall(r"[a-z]+", (text or "").lower()))
    return all(word in heard for word in words)


def title_tokens(title):
    """A title's words: case, punctuation and camel-casing are the writer's
    ("VoiceProbe-AmberCedarPlum" is five words)."""
    spaced = re.sub(r"([a-z])([A-Z])", r"\1 \2", title or "")
    return re.findall(r"[a-z]+", spaced.lower().replace("voiceprobe", "voice probe"))


def title_matches(title, words):
    """The task that was asked for: a title that *is* the probe title, not a
    delegation brief or a note that quotes it. The word order is the request's."""
    tokens = title_tokens(title)
    return is_probe_prefix(tokens[:2]) and tokens[2:2 + len(words)] == list(words)


def is_probe_prefix(tokens):
    """"voice probe", or "voice pro…" as a transcriber that drops the end of
    the word renders it."""
    return len(tokens) == 2 and tokens[0] == "voice" and tokens[1].startswith("pro")


def voice_prompt(words, profile):
    """The request itself, with no name in it for a transcriber to mangle
    ("Ask Magician" came back as "ask my jacket", and the delegated turn went
    looking for a specialist called Jacket). The realtime catalog has no
    task-creating hand, so creating a task is itself the reason to delegate
    — or to load the hand and do it, which the lane counts as self-served;
    GPT Live 1 delegates on its own and relays these words as the intent."""
    del profile  # one request on every profile; what differs is who acts on it
    # One clause: a pause at a full stop is a turn boundary to a server VAD,
    # and a Live delegation built on the second half lost the request.
    return (f"Create a task titled {probe_title(words)} with the description voice conformance probe "
            "and tell me when it is created")


# Where the spoken request goes on after the title.
TITLE_STOP_WORDS = ("with", "and", "then", "description")


def heard_words(transcript, count):
    """The title as the transcriber heard it: the tokens after "voice probe"
    up to the request's next phrase, however many the transcriber made of
    the words (a split word is two tokens, two merged words one), bounded a
    little above the word count; None when it heard no title."""
    tokens = re.findall(r"[a-z]+", (transcript or "").lower())
    for index in range(len(tokens) - 1):
        if not is_probe_prefix(tokens[index:index + 2]):
            continue
        heard = []
        for token in tokens[index + 2:index + 2 + count + 2]:
            if token in TITLE_STOP_WORDS:
                break
            heard.append(token)
        return heard or None
    return None


def title_as_heard(title, transcript, count):
    """Whether a task title is exactly the probe title as the transcriber
    heard it — the title Magician was actually asked for."""
    heard = heard_words(transcript, count)
    tokens = title_tokens(title)
    return bool(heard) and is_probe_prefix(tokens[:2]) and tokens[2:] == heard


def delegated_turn_ids(folded, voice_rows):
    """The chat turns a delegation may have run under: GPT Realtime runs it
    under the voice turn its call row names, GPT Live 1 under the delegation
    id, which is the delegate call id. Nearest-known first, no duplicates."""
    ids = []
    for row in voice_rows or []:
        turn = row.get("chat_turn_id")
        if turn and turn not in ids:
            ids.append(turn)
    for call_id in folded.get("delegate_call_ids") or []:
        if call_id not in ids:
            ids.append(call_id)
    return ids


def driver_for(profile, requested):
    # A typed prompt reaches GPT Realtime as a system item, which the model
    # may decline to act on, and GPT Live 1 only as commentary; a spoken
    # prompt is a user turn on both.
    if requested == "auto":
        return "speech"
    if requested == "text" and not TEXT_REACHES_USER_LANE.get(profile, True):
        raise ValueError(f"{profile} cannot be typed at: user.text is not a user turn there")
    if requested not in DRIVERS:
        raise ValueError(f"voice drivers are {DRIVERS}")
    return requested


def uses_push_to_talk(descriptor):
    return str((descriptor or {}).get("turn_detection_mode") or "").lower() == "none"


def speech_pcm(text):
    """The prompt as 24 kHz mono PCM16, said by the host's speech synthesiser."""
    for tool in ("say", "ffmpeg"):
        if shutil.which(tool) is None:
            raise RuntimeError(f"the speech driver needs `{tool}` on PATH")
    with tempfile.TemporaryDirectory(prefix="hc-voice-") as directory:
        aiff = os.path.join(directory, "prompt.aiff")
        pcm = os.path.join(directory, "prompt.pcm")
        # Unhurried: the words are the whole point, and a transcriber hears
        # a fast synthetic voice worse than a person.
        subprocess.run(["say", "-r", str(SPEECH_RATE_WPM), "-o", aiff, text], check=True, capture_output=True)
        subprocess.run(["ffmpeg", "-loglevel", "error", "-y", "-i", aiff, "-ac", "1", "-ar", str(REALTIME_SAMPLE_RATE),
                        "-f", "s16le", pcm], check=True, capture_output=True)
        return Path(pcm).read_bytes()


def pcm_frames(pcm, silence_secs=2.0):
    """The audio in realtime-sized frames, followed by silence long enough
    for a server VAD to close the turn."""
    silence = b"\x00" * (int(REALTIME_SAMPLE_RATE * silence_secs) * 2)
    data = pcm + silence
    return [data[i:i + REALTIME_FRAME_BYTES] for i in range(0, len(data), REALTIME_FRAME_BYTES)]


# ---------------------------------------------------------------------------
# The control socket
# ---------------------------------------------------------------------------


def fold_frames(frames):
    """What the control frames say about the turn: the session, the
    delegation the model made (its `tool.result` names the tool), the done
    frame, the words spoken on each side, and any error."""
    folded = {"voice_session_id": None, "tools": [], "descriptor": {}, "delegate_call_ids": [], "done": None,
              "assistant_text": None, "user_transcript": None, "user_transcripts": [], "chunks": [], "error": None,
              "own_calls": [], "kinds": []}
    for item in frames:
        kind = item.get("kind")
        payload = item.get("payload") or {}
        folded["kinds"].append(kind)
        if kind == "session.ready":
            folded["voice_session_id"] = payload.get("voice_session_id")
            folded["tools"] = [tool.get("name") for tool in payload.get("tools") or [] if isinstance(tool, dict)]
            folded["descriptor"] = payload.get("descriptor") or {}
        elif kind == "tool.result":
            try:
                output = json.loads(payload.get("output") or "{}")
            except (TypeError, ValueError):
                output = {}
            name = payload.get("tool_name") or output.get("tool_name")
            if name == DELEGATE_TOOL and payload.get("call_id"):
                folded["delegate_call_ids"].append(payload["call_id"])
            elif name:
                # The mouth's own hand, in order, with the runtime's refusal if any.
                message = (output.get("result") or {}).get("message") if isinstance(output.get("result"), dict) else None
                folded["own_calls"].append({"tool": name, "status": payload.get("status") or output.get("status"),
                                            "message": message})
        elif kind == "delegate_to_chat.chunk":
            folded["chunks"].append(payload.get("text") or "")
        elif kind == "delegate_to_chat.done":
            folded["done"] = payload
            if payload.get("call_id") and payload["call_id"] not in folded["delegate_call_ids"]:
                folded["delegate_call_ids"].append(payload["call_id"])
        elif kind == "transcript.assistant":
            folded["assistant_text"] = payload.get("text")
        elif kind == "transcript.user":
            # Every user turn of the call: a server VAD may split one spoken
            # request into two, and the title is in whichever half had it.
            folded["user_transcript"] = payload.get("text")
            folded["user_transcripts"].append(payload.get("text") or "")
        elif kind == "session.error":
            folded["error"] = payload.get("message") or "session.error"
    return folded


class VoiceSession:
    """One control-socket session, driven the way the meeting responder drives
    it. Text frames are JSON envelopes `{kind, payload}`; binary frames are
    PCM in both directions (the assistant's audio is counted, not kept)."""

    def __init__(self, base_url, media_session_id, token, http_timeout):
        import websocket  # websocket-client, which two other evals already use
        ws_base = base_url.replace("https://", "wss://", 1).replace("http://", "ws://", 1)
        self.socket = websocket.create_connection(
            f"{ws_base}/api/magician/v2/media/voice/{quote(media_session_id)}/control",
            header=[f"Authorization: Bearer {token}"], timeout=http_timeout)
        self.timeout_error = websocket.WebSocketTimeoutException
        self.frames = []
        self.audio_frames_received = 0

    def send(self, kind, **payload):
        self.socket.send(json.dumps({"kind": kind, "payload": payload}))

    def send_audio(self, frame):
        self.socket.send_binary(frame)

    def next(self, timeout):
        """The next text frame within `timeout` seconds, or None."""
        self.socket.settimeout(timeout)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                raw = self.socket.recv()
            except self.timeout_error:
                return None
            if isinstance(raw, bytes):
                self.audio_frames_received += 1
                continue
            try:
                item = json.loads(raw)
            except ValueError:
                continue
            self.frames.append(item)
            return item
        return None

    def wait_for(self, kinds, timeout):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            item = self.next(min(5.0, max(0.1, deadline - time.monotonic())))
            if item is not None and item.get("kind") in kinds:
                return item
        return None

    def drain(self):
        while self.next(0.05) is not None:
            pass

    def close(self):
        try:
            self.send("session.end")
            self.wait_for({"session.ended"}, 5.0)
        except Exception:
            pass
        try:
            self.socket.close()
        except Exception:
            pass


def speak(session, pcm, push_to_talk):
    frames = pcm_frames(pcm)
    if push_to_talk:
        session.send("ptt.engage")
    for frame in frames:
        session.send_audio(frame)
        # Real time, slightly quick: a provider VAD paces on arrival.
        time.sleep(REALTIME_FRAME_BYTES / (REALTIME_SAMPLE_RATE * 2) * 0.9)
        session.drain()
    if push_to_talk:
        session.send("ptt.release")


# ---------------------------------------------------------------------------
# Evidence from Magician's side
# ---------------------------------------------------------------------------


def voice_call_rows(client, voice_session_id, started_ms):
    safe = "".join(ch for ch in voice_session_id if ch.isalnum() or ch in "_-")
    sql = ("SELECT operation, capability, provider, model, chat_session_id, chat_turn_id, success, "
           "input_tokens, output_tokens, cost_usd, cost_source, timestamp_ms "
           f"FROM llm_calls WHERE execution_id = 'voice:{safe}'")
    payload, _ = client.json("POST", "/api/magician/v2/analytics/llm/facts/query",
                             body={"sql": sql, "from_ms": started_ms - 5000, "to_ms": int(time.time() * 1000) + 60000,
                                   "limit": 200}, expected=(200,), timeout=90.0)
    rows = (payload.get("data") or {}).get("rows") if isinstance(payload, dict) else None
    return [row for row in rows or [] if isinstance(row, dict)]


def voice_chat_session(client, thread):
    """The voice channel's chat session on the probe's UI thread."""
    payload, _ = client.json("GET", "/api/magician/v2/chat/sessions", query={"ui_thread_id": thread})
    sessions = payload.get("sessions") if isinstance(payload, dict) else payload
    for session in sessions or []:
        if isinstance(session, dict) and session.get("id"):
            return session["id"]
    return None


def turn_events(client, chat_session_id, chat_turn_id):
    payload, _ = client.json("GET", f"/api/magician/v2/chat/sessions/{quote(chat_session_id)}/turns/{quote(chat_turn_id)}/events")
    events = payload if isinstance(payload, list) else (payload or {}).get("events") or []
    return [event for event in events if isinstance(event, dict)]


def inner_event(event):
    data = event.get("data")
    inner = data.get("event") if isinstance(data, dict) and isinstance(data.get("event"), dict) else event.get("event")
    source = inner if isinstance(inner, dict) else event
    kind = source.get("event_type") if isinstance(inner, dict) else (event.get("event_type") or event.get("type"))
    payload = source.get("payload")
    return str(kind or ""), payload if isinstance(payload, dict) else {}


def find_task(client, matcher, since_ms):
    payload, _ = client.json("GET", "/api/magician/v3/tasks", query={"limit": 100})
    for task in payload.get("tasks") or payload.get("items") or []:
        manifest = task.get("manifest") or task
        if not matcher(manifest.get("title")):
            continue
        created = manifest.get("created_at") or task.get("created_at") or ""
        return {"task_id": manifest.get("task_id") or task.get("task_id") or task.get("id"),
                "title": manifest.get("title"), "created_at": created}
    return None


def wait_for_task(client, matcher, since_ms, timeout):
    """The first task the matcher accepts, polled until the deadline: the
    effect lands after the done frame."""
    deadline = time.monotonic() + timeout
    while True:
        task = find_task(client, matcher, since_ms)
        if task or time.monotonic() >= deadline:
            return task
        time.sleep(2)


# ---------------------------------------------------------------------------
# Grading
# ---------------------------------------------------------------------------


def voice_call_is_accounted(row):
    """A realtime call the ledger can stand behind: the voice capability, a
    turn to attribute it to, and a success the provider actually reported."""
    return (row.get("capability") == "voice.realtime"
            and bool(row.get("chat_turn_id"))
            and bool(row.get("success")))


def grade_run(folded, driver, words, voice_rows, turn_events, task):
    done = folded.get("done") or {}
    delegated = bool(folded.get("delegate_call_ids"))
    # The task that was asked for — as asked, or as the transcriber heard
    # it in any user turn of the call: Magician can only act on the words
    # that reached it, and the transcriber's slip is reported beside the
    # verdict as `heard`.
    title = (task or {}).get("title")
    asked_for = title_matches(title, words) or any(
        title_as_heard(title, transcript, len(words)) for transcript in folded.get("user_transcripts") or [])
    hands = {}
    for event in turn_events or []:
        kind, payload = inner_event(event)
        if payload.get("tool_name") == EFFECT_TOOL and kind in ("tool.call.started", "tool.result.projected"):
            hands[kind] = True
    gates = {
        "session_ready": folded.get("voice_session_id") is not None,
        "delegation_observed": delegated,
        "delegation_succeeded": delegated and done.get("success") is True,
        "voice_call_row": any(voice_call_is_accounted(row) for row in voice_rows or []),
        # A duration-billed mouth (GPT-Live) is priced by the clock, so a
        # call with no price at all is an unmetered session, not a free one.
        "voice_call_priced": any(voice_call_is_accounted(row) and row.get("cost_usd") is not None
                                 for row in voice_rows or []),
        "hands_on_turn": hands.get("tool.call.started", False) and hands.get("tool.result.projected", False),
        "effect": bool(task) and asked_for,
    }
    return gates


def brief(text, limit=160):
    text = " ".join(str(text or "").split())
    return text if len(text) <= limit else text[:limit - 1] + "…"


def describe_run(folded, words, task, turn_events, proof):
    """The run as a story: what was asked, what the mouth did — delegated,
    loaded the hand and did it, declined, or broke — and whether the person
    got what they asked for. The proof gates stay underneath as evidence."""
    expected = f'a task titled "{probe_title(words)}"'
    hands = []
    for event in turn_events or []:
        kind, payload = inner_event(event)
        name = payload.get("tool_name")
        if kind == "tool.call.started" and name and name not in hands:
            hands.append(name)
    own = folded.get("own_calls") or []
    refused = [call for call in own if call.get("status") == "error"]
    done = folded.get("done")
    steps = []
    if folded.get("error"):
        path = "errored"
        steps.append(f"the session failed: {brief(folded['error'])}")
    elif folded.get("delegate_call_ids"):
        path = "delegated"
        steps.append(f"delegated to Magician ({DELEGATE_TOOL})")
        if hands:
            steps.append("Magician's turn ran " + ", ".join(hands))
        if done is None:
            steps.append("the delegation never finished before the call ended")
        elif done.get("success") is False:
            steps.append(f"the delegation failed: {brief(done.get('error'))}")
        chunks = [chunk for chunk in folded.get("chunks") or [] if chunk.strip()]
        if chunks:
            steps.append("Magician said: " + brief(" ".join(chunks)))
    elif refused:
        path = "errored"
        steps.append("called " + ", ".join(call["tool"] for call in own) + " itself")
        steps.append(f"{refused[0]['tool']} was refused: {brief(refused[0].get('message'))}")
    elif own:
        path = "self_served"
        steps.append("called " + ", ".join(call["tool"] for call in own) + " itself")
    else:
        path = "declined"
    if folded.get("assistant_text"):
        steps.append("it said: " + brief(folded["assistant_text"]))
    if not steps:
        steps.append("nothing: no delegation, no hand, no words")
    reached = bool(proof.get("effect"))
    if reached:
        result = f'reached: task "{(task or {}).get("title")}" exists'
    elif task:
        result = f'not reached: a task exists but titled "{task.get("title")}"'
    else:
        result = "not reached: no task"
    return {"expected": expected, "did": "; ".join(steps), "result": result, "path": path,
            "outcome": "reached" if reached else "not_reached"}


def summarize(results):
    summary = {}
    for row in results:
        engine = row.get("engine")
        entry = summary.setdefault(engine, {"runs": 0, "reached": 0, "paths": {}, "heard": 0, "inconclusive": 0})
        if row.get("outcome") == "inconclusive":
            entry["inconclusive"] += 1
            continue
        entry["runs"] += 1
        entry["reached"] += row.get("outcome") == "reached"
        entry["heard"] += bool(row.get("heard"))
        path = entry["paths"].setdefault(row.get("path") or "unknown", {"runs": 0, "reached": 0})
        path["runs"] += 1
        path["reached"] += row.get("outcome") == "reached"
    for entry in summary.values():
        entry["reached_rate"] = entry["reached"] / entry["runs"] if entry["runs"] else None
    return summary


# ---------------------------------------------------------------------------
# One run
# ---------------------------------------------------------------------------


def run_case(args, client, profile, driver, run_index, runtime_unavailable):
    words = word_nonce()
    thread = f"hc-voice-{secrets.token_hex(4)}"
    result = {"engine": profile, "case": f"voice_{driver}", "repeat": run_index, "driver": driver, "words": words,
              "title": probe_title(words), "thread_id": thread, "outcome": "not_reached", "path": None,
              "expected": f'a task titled "{probe_title(words)}"', "did": None, "result": None, "proof": {},
              "cleanup_errors": []}
    started_ms, started = int(time.time() * 1000), time.monotonic()
    session = None
    media_session_id = None
    task = None
    try:
        registration, _ = client.json("POST", "/api/magician/v2/media/sessions", body={
            "thread_id": thread, "surface_type": "web_desktop", "transport": "websocket",
            "display_label": "Harness conformance voice probe"}, expected=(200, 201))
        media_session_id = registration["session"]["session_id"]
        result["media_session_id"] = media_session_id
        session = VoiceSession(args.api_base_url, media_session_id, os.environ.get("MAGICIAN_BEARER_TOKEN", ""),
                               args.http_timeout_secs)
        session.send("session.start", ui_thread_id=thread, thread_id=thread, realtime_profile=profile)
        ready = session.wait_for({"session.ready", "session.error"}, 60.0)
        if ready is None or ready.get("kind") != "session.ready":
            raise RuntimeError(f"no session.ready: {json.dumps(ready)[:300] if ready else 'timed out'}")
        print(f"  {profile}/{driver} run {run_index}: session ready, {ready['payload'].get('descriptor', {}).get('model')}",
              flush=True)
        prompt = voice_prompt(words, profile)
        result["prompt"] = prompt
        if driver == "text":
            session.send("user.text", text=prompt, request_response=True)
        else:
            speak(session, speech_pcm(prompt), uses_push_to_talk(ready["payload"].get("descriptor")))
        session.wait_for(TERMINAL_FRAMES, args.turn_timeout_secs)
        folded = fold_frames(session.frames)
        result["folded"] = {key: value for key, value in folded.items() if key != "kinds"}
        result["frame_kinds"] = folded["kinds"]
        result["frames"] = [item for item in session.frames
                            if item.get("kind") not in ("transcript.assistant.delta", "transcript.user.partial")]
        result["audio_frames_received"] = session.audio_frames_received
        session.close()
        session = None
        # The effect and the analytics rows land after the done frame; give them the
        # remaining budget, bounded.
        remaining = max(10.0, args.turn_timeout_secs - (time.monotonic() - started))
        # Every run may have produced the task: a delegation, or the realtime
        # model loading the hand and doing it itself. Look for it as asked and
        # as heard in any user turn of the call.
        matcher = lambda title: title_matches(title, words) or any(
            title_as_heard(title, transcript, len(words)) for transcript in folded["user_transcripts"])
        task = wait_for_task(client, matcher, started_ms, min(remaining, 90.0)) if folded["delegate_call_ids"] else \
            find_task(client, matcher, started_ms)
        result["task"] = task
        rows = []
        deadline = time.monotonic() + min(remaining, 90.0)
        while True:
            rows = voice_call_rows(client, media_session_id, started_ms)
            if rows or time.monotonic() >= deadline:
                break
            time.sleep(3)
        result["voice_rows"] = rows
        events = []
        chat_session_id = voice_chat_session(client, thread)
        turn_ids = delegated_turn_ids(folded, rows)
        result["chat_session_id"], result["chat_turn_ids"] = chat_session_id, turn_ids
        if chat_session_id:
            for turn_id in turn_ids:
                events.extend(turn_events(client, chat_session_id, turn_id))
        result["turn_event_kinds"] = sorted({f"{kind}:{payload.get('tool_name') or ''}" for kind, payload in map(inner_event, events)})
        result["proof"] = grade_run(folded, driver, words, rows, events, task)
        result.update(describe_run(folded, words, task, events, result["proof"]))
        if driver == "speech":
            # What the transcriber heard, beside the outcome: a garbled word
            # the model still acted on correctly is the transcriber's fact.
            result["heard"] = any(transcript_heard(transcript, words) for transcript in folded["user_transcripts"])
    except runtime_unavailable as error:
        result["outcome"] = "inconclusive"
        result["error"] = f"Runtime unavailable during evaluation: {error}"
    except ImportError as error:
        result["outcome"] = "inconclusive"
        result["error"] = f"the voice lane needs the websocket-client package: {error}"
    except Exception as error:
        result["error"] = f"{type(error).__name__}: {error}"
        result["did"] = result.get("did") or f"the lane failed: {error}"
        result["result"] = result.get("result") or "not reached: the lane failed"
        if session is not None:
            result["frame_kinds"] = [item.get("kind") for item in session.frames]
    finally:
        if session is not None:
            session.close()
        result["latency_ms"] = round((time.monotonic() - started) * 1000)
        if not args.keep_artifacts:
            if task and task.get("task_id"):
                try:
                    client.json("DELETE", f"/api/magician/v3/tasks/{quote(task['task_id'])}", query={"remove_files": "true"},
                                expected=(200, 202, 204, 404))
                except Exception as error:
                    result["cleanup_errors"].append(f"task: {error}")
            try:
                client.json("DELETE", f"/api/magician/v2/ui-threads/{quote(thread)}", expected=(200, 204, 404))
            except Exception as error:
                result["cleanup_errors"].append(f"thread: {error}")
        retained = args.output_dir / "cases" / f"{profile}-{driver}-{run_index}-{thread}"
        retained.mkdir(parents=True, exist_ok=True)
        (retained / "evidence.json").write_text(json.dumps(result, indent=2, default=str))
    heard_note = "" if "heard" not in result else f" heard={result['heard']}"
    print(f"  {profile}/{driver} run {run_index}: {result['outcome']} ({result.get('path')}){heard_note}\n"
          f"      expected {result['expected']}\n      did      {result.get('did')}\n      result   {result.get('result')} "
          f"{result.get('error', '')}", flush=True)
    return result


# ---------------------------------------------------------------------------
# Runner
# ---------------------------------------------------------------------------


def write_report(directory, results, mode):
    directory.mkdir(parents=True, exist_ok=True)
    summary = summarize(results)
    measured = [row for row in results if row.get("outcome") != "inconclusive"]
    payload = {"lane": "voice", "mode": mode, "results": results, "profiles": summary,
               "summary": {"ok": bool(measured) and all(row["outcome"] == "reached" for row in measured),
                           **{v: sum(row.get("outcome") == v for row in results)
                              for v in ("reached", "not_reached", "inconclusive")}},
               "selection": "one voice session per run on the named realtime profile; no global setting changes"}
    (directory / "report.json").write_text(json.dumps(payload, indent=2, default=str))
    rows = "".join("<tr>" + "".join(f"<td>{html.escape(str(row.get(key, '')))}</td>"
                   for key in ("engine", "repeat", "expected", "did", "result", "path", "heard", "error")) + "</tr>"
                   for row in results)
    rates = "".join(
        f"<li>{html.escape(str(profile))}: {entry['reached']}/{entry['runs']} reached — "
        + ", ".join(f"{name} {stats['reached']}/{stats['runs']}" for name, stats in entry["paths"].items())
        + f"; heard {entry['heard']}/{entry['runs']}</li>" for profile, entry in summary.items())
    (directory / "report.html").write_text(
        "<!doctype html><meta charset=utf-8><title>Voice conformance</title>"
        "<style>body{font:15px system-ui;margin:40px}td,th{text-align:left;padding:10px;border-bottom:1px solid #ddd;"
        "vertical-align:top}</style>"
        "<h1>Voice turns on the realtime profiles</h1><p>Each run: what was asked, what the mouth did, whether the "
        "person got it. Reached by any path counts; how it got there is reported, not judged.</p><ul>" + rates + "</ul>"
        "<table><tr><th>Profile</th><th>Run</th><th>Expected</th><th>Did</th><th>Result</th><th>Path</th><th>Heard</th>"
        "<th>Error</th></tr>" + rows + "</table>")
    return payload


def main(args, client_type, runtime_unavailable):
    if args.turn_timeout_secs <= 0:
        raise ValueError("voice timeout must be positive")
    if args.self_test:
        import unittest
        import test_eval_harness_voice
        suite = unittest.defaultTestLoader.loadTestsFromModule(test_eval_harness_voice)
        ok = unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful()
        write_report(args.output_dir, [{"engine": "provider-free", "driver": "-", "repeat": 1, "case": "voice_contracts",
                                        "outcome": "reached" if ok else "not_reached", "path": "contracts",
                                        "expected": "every contract passes", "did": f"ran {suite.countTestCases()} contracts",
                                        "result": "reached" if ok else "not reached"}], "self-test")
        print("Voice delegation contracts passed" if ok else "Voice delegation contracts FAILED")
        return 0 if ok else 1
    profiles = args.engines or list(PROFILES)
    unknown = set(profiles) - set(PROFILES)
    if unknown:
        raise ValueError(f"voice profiles are {PROFILES}")
    plan = [(profile, driver_for(profile, args.voice_driver), run) for profile in profiles for run in range(1, args.runs + 1)]
    client = client_type(args.api_base_url, args.http_timeout_secs)
    try:
        login(args, client)
        client.json("GET", "/api/magician/v2/auth/session")
    except runtime_unavailable as error:
        write_report(args.output_dir, [{"engine": profile, "driver": driver, "repeat": run, "outcome": "inconclusive",
                                        "path": None, "error": str(error)} for profile, driver, run in plan], "live")
        print(f"Runtime unavailable; nothing started. Report: {args.output_dir / 'report.html'}")
        return 2
    results = []
    for profile, driver, run in plan:
        results.append(run_case(args, client, profile, driver, run, runtime_unavailable))
        write_report(args.output_dir, results, "live")
    for profile, entry in summarize(results).items():
        paths = ", ".join(f"{name} {stats['reached']}/{stats['runs']}" for name, stats in entry["paths"].items())
        print(f"{profile}: {entry['reached']}/{entry['runs']} reached — {paths}; heard {entry['heard']}/{entry['runs']}")
    print(f"Voice report: {args.output_dir / 'report.html'}")
    measured = [row for row in results if row["outcome"] != "inconclusive"]
    return 0 if measured and all(row["outcome"] == "reached" for row in measured) else 1
