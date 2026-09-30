---
name: "screen-observation"
version: 0.1.2
description: "Procedure skill — capture the user's Mac screen on explicit request and answer questions about it. Pull this when the user asks 'what's on my screen', 'what does this error say', 'look at this window / this app', 'summarize what I'm reading', or any ask that requires SEEING their display right now — plus SUSTAINED observation asks: 'watch my screen', 'tell me when the build finishes', 'keep an eye on this dashboard', 'take notes on what I do' (these start the observation rail via /screen/observe, not one-shot captures). Capture via `screencapture` (full display) or cua-driver (one app's window), then understand via the backend's `/screen/describe` endpoint (config-routed vision profile) — or the `ocr` skill when the ask is pure text extraction. This skill OBSERVES only — for clicking/typing/driving apps, that's macos-ui-automation (Bolt's domain, reach via delegation). Never capture without an explicit ask in the current conversation."
metadata:
  magician:
    skill_type: procedure
    requires:
      bins: []
      host_gateway: true
    install_hint:
      docs: "full-display capture relays through Magican Desktop's host gateway (Screen Recording TCC on that app); the desktop app must be running. Window-scoped capture additionally needs `cua-driver` (install the pinned release with `make setup-cua-driver`). Understanding goes through the backend (`/screen/describe`) — no extra CLIs."
---

# Screen Observation — capture and answer

You can SEE the user's screen, but only when they ask. The flow is always
capture → understand → answer → clean up.

## Hard rules (read first)

1. **Explicit ask only.** Capture exclusively when the CURRENT user message
   asks about their screen. Never capture proactively. Sustained watching is
   allowed ONLY through the observation rail below, and ONLY when explicitly
   asked ("watch my screen…") — never re-capture to "check something" the
   user didn’t ask about.
2. **Tell them you captured.** Your answer should make the capture obvious
   ("Looking at your screen: …") — observation must never be silent.
3. **Clean up.** Delete the temp file after answering (`rm -f` in the same
   turn). The capture's job ends with the answer.
4. This skill observes; it never clicks, types, or drives apps. If the user
   wants the screen ACTED on, delegate to Bolt (mac-operator).

## Step 1 — capture

**Full display** (default — "what's on my screen", "what am I looking at"):

```bash
F=/tmp/screen-ask-$(date +%s).png && /usr/sbin/screencapture -x "$F" && echo "$F"
```

**One app's window** (the user names an app — "what does this Xcode error
say"): use cua-driver, which captures windows without foregrounding them.

```bash
cua-driver call list_windows            # find the app's window_id (+ pid)
cua-driver call get_window_state '{"pid": <PID>, "window_id": <ID>, "include_accessibility_tree": false}' --screenshot-out-file /tmp/screen-ask.png
```

If cua-driver errors about permissions or the daemon, fall back to the
full-display `screencapture` — a bigger picture beats no picture.

**Text/structure without pixels**: when the question is purely about UI text
in a NAMED app ("what's in my Slack sidebar"), `cua-driver call
get_window_state '{"pid": <PID>, "window_id": <ID>, "include_screenshot": false}'`
returns JSON with the tree as `tree_markdown` plus structured `elements` — often
enough to answer with no image at all, and cheaper than a vision call. Add
`"query":"<text>"` or `"max_elements":300` to keep a large window's tree small.

## Step 2 — understand

**The one engine: `POST /screen/describe`** — the backend captures (or
takes your frame) and answers via the `screen_understanding` operation's
vision profile in magician-config (full-tier model, same config-managed
routing as every other LLM call). No vision CLIs.

Simplest form — capture + answer in ONE call (skip Step 1 entirely for
full-display asks):

```bash
curl -s -X POST -H "Authorization: Bearer $MAGICIAN_BEARER_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"question":"<the user's question, verbatim>"}' \
  http://localhost:3002/api/magician/v2/screen/describe
```

With a window-scoped frame you captured via cua-driver in Step 1:

```bash
B64=$(base64 -i /tmp/screen-ask.png) && curl -s -X POST \
  -H "Authorization: Bearer $MAGICIAN_BEARER_TOKEN" \
  -H 'Content-Type: application/json' \
  -d "{\"question\":\"<the question>\",\"image_b64\":\"$B64\"}" \
  http://localhost:3002/api/magician/v2/screen/describe
```

- For PURE text extraction at scale ("read every row of this table") the
  `ocr` skill (`--engine agy`) remains a fine alternative.
- Answer in your own voice from the `answer` field — the user asked YOU.
  Quote on-screen text exactly when the ask is about exact text.

## Step 3 — clean up

```bash
rm -f /tmp/screen-ask*.png
```

## Relationship to the ⇧⌥S / ⇧⌥A / ⇧⌥R shortcuts

The user also has desktop shortcuts: ⇧⌥S (screenshot + ask), ⇧⌥A (pick a
region or window + ask), and ⇧⌥R (clip + ask). They capture via the
backend, stage into the `#screens` thread's daily session, and record
provenance in the `screen_observations` memory tier.
This skill is the LANGUAGE twin for asks that arrive in chat. If the user
wants a capture kept on record ("save this to my screens thread"), use the
backend endpoint instead of a temp file:

```bash
curl -s -X POST -H "Authorization: Bearer $MAGICIAN_BEARER_TOKEN" \
  -H 'Content-Type: application/json' -d '{"mode":"screenshot"}' \
  http://localhost:3002/api/magician/v2/screen/capture
```

That stages the capture as an attachment on today's `Screens — <date>`
session and appends the memory entry — then tell the user it's filed there.

## Continuous observation ("watch my screen…")

For SUSTAINED asks — not "what's on my screen now" but "keep watching" —
use the observation rail instead of one-shot captures. It runs
autonomously (you are OUT of the loop once started): a frame every few
seconds, only CHANGED frames analyzed, narration streamed into a
`Watching: <purpose>` session under the `screen-watch` thread, final
summary + memory entry at stop.

Map the user's phrasing:

- "tell me when / alert me if X" → `mode: "watch"` with X as `watch_for`
  (the session stops itself once X matches and posts an ⚠ ALERT line):

```bash
curl -s -X POST -H "Authorization: Bearer $MAGICIAN_BEARER_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"purpose":"<their words>","mode":"watch","watch_for":"<the condition>"}' \
  http://localhost:3002/api/magician/v2/screen/observe/start
```

- "take notes / journal what I do / watch my work" → `mode: "notes"`
  (never interrupts; same call without `watch_for`).
- For stable screens where details matter ("watch this dashboard deeply",
  "keep reading this build page", "understand this screen even if it does not
  change"), add `"deep_observation": true` to the same `start` call. This is
  NOT a different mode: it works with notes or watch. The rail waits for the
  same screen to stay stable for about 20 seconds, runs one high-detail deep
  read, then repeats at most every 5 minutes while that same screen remains
  stable; any screen change resets the timers.
- **A condition while an observation is ALREADY running** ("actually,
  alert me when the export finishes", "switch to just taking notes",
  "change what you're watching for") → RETARGET the running session, do
  NOT start a new one (start returns 409 while one runs):

```bash
curl -s -X POST -H "Authorization: Bearer $MAGICIAN_BEARER_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"watch_for":"<the new condition>"}' \
  http://localhost:3002/api/magician/v2/screen/observe/retarget
```

  (`watch_for` alone implies watch mode; `{"mode":"notes"}` drops back to
  silent narration. `{"deep_observation":true}` or
  `{"deep_observation":false}` toggles the stable-screen deep read without
  restarting. The session announces the change with a 🎯 line.)
  This is exactly what happens when the user presses ⇧⌥W — a notes-mode
  session starts instantly and the HUD opens on it — and then types what
  they want watched: you receive that message IN the observation session;
  retarget, confirm in one line, done.
- "stop watching" → `POST /screen/observe/stop` (same headers). Tell the
  user where the summary landed.
- "are you watching?" → `GET /screen/observe/status`.

One observation runs at a time (a second start returns 409 — retarget it
or offer to stop it). Reply with what you started ("👁 watching — I'll
flag it when the build fails") and then STOP — the rail does the
watching, not you.
