# Voice resume context — reference-only framing

On a realtime voice call, the server hands the client a **resume context** (a
compacted `summary` of older turns + the last few `recent_turns`) at session
start and after each ~28-min session rotation. `providers/openai.ts`
(`replayResume`) injects it into the OpenAI Realtime session as a system message
before live audio begins.

**Constraint:** the summary captures tasks the assistant created/started. Injected
as a bare "Conversation so far: …", the model reads those as **open to-dos and
re-fires them** (an old multi-step pipeline re-dispatched days later).

So the summary is injected with explicit **reference-only** framing: *"Earlier
conversation (reference only — do NOT re-run, re-create, or re-dispatch any task,
pipeline, or action mentioned here; those already happened. Act only on the
user's current request): …"*.

This is the frontend (DirectPeerToPeer) half. The cross-surface half lives in the
prompts the server controls:
- `voice_context_compaction_system` — records created/started tasks as **already
  handled** (with outcome), not as open to-dos.
- `voice_modality_addendum` (rule 8) — never re-run/re-create/re-dispatch
  anything that appears in the replayed summary/older turns; act only on the
  current request.

Together they stop the realtime agent treating its own history as a work queue.

## Client failure lifecycle

`realtimeVoiceClient.ts` tears down microphone tracks, provider transport, audio
nodes, and the Magician control socket on provider or connection failure while
preserving `state: error` and its message for the UI. Only an explicit clean
stop transitions to `idle`. This prevents a useful failure explanation from
being overwritten by cleanup, while retaining the same resource-release
guarantees as a normal end. Mounted lifecycle tests cover startup, session
readiness, duplicate starts, push-to-talk queuing, rotation/rebind, microphone
denial, and both clean and failed teardown.
