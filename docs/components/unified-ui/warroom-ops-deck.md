# `/warroom` — DECK

## The rule

**No motion without information.** Every element binds a real source. When a
source is down the deck renders dashes, dark LEDs and UPLINK OFFLINE — **it never
animates to fill silence.**

## Panels and their sources

| Panel | Source | Notes |
| --- | --- | --- |
| `SystemBar` | `/health` (15s poll) · `todayPulseStore` | LEDs are per-component; spend/calls/mem are live DuckDB SQL through the pulse queries |
| `NeedsYou` | `attentionStore` lanes → `hitlRequestFromFeedItem` | Confirmations resolve inline via canonical `/v2/hitl/{id}/respond`; richer input types deep-link to `/attention` |
| `CoreStage` | `chatStore` · pulse pacing · event-rate EMA · `voiceTranscriptStore` · `pushToTalk*` | Arcs are today vs yesterday; text send via `chatStore.sendMessage`; whisper line carries OPEN/END CHANNEL, hold-to-talk, PTT toggle |
| `VoiceOrbCanvas` | `voiceMicAnalyser` (`AnalyserNode`) · `voiceCallStore` | Real mic amplitude; mic closed → still ring + VOICE OFFLINE |
| `TextChat` | left rail; `chatStore` | Channel identity, composer, ≋ voice rows; dictation reuses `MicCaptureButton` |
| `InFlight` (TASKS) | `taskStore.executingTask` · `/v3/tasks` · `/v3/tasks/internal` | Live progress on top; below, a server-paginated task browser (USER·INTERNAL and status segments); ⌕ opens the run graph FRONT |
| `EventTape` | `/api/magician/v3/events` NDJSON | Ambient strip: each real frame blips once; stats, 60s sparkline and `/events` link persist |

`failed` and `running` attention lanes are **off** NeedsYou — they are history
and status, not "waiting on you".

## Deck modes

Derived in `deck.ts` from measurements only, fault outranking attention:

- **FAULT** (red) — the health probe failed, or ≥3 errors on the tape within a
  minute.
- **ATTENTION** (amber) — at least one human-input request pending.
- **NOMINAL** (phosphor) — neither.

An **unprobed** health check (`null`) does not fault the deck — "not yet known"
must not render as broken on every load.

## Derived motion

- **Breathing and tick-ring spin periods** come from `nextEventRate`, an EMA of
  real arrivals (20s half-life): idle 6s, saturated asymptotes to 1.8s, never
  strobes.
- **Pacing arcs** are `pacingRatio(today, yesterday)` clamped to [0, 2], marker
  at 1.0. No baseline → `null` → hollow arc; never a ratio from a zero
  denominator.
- **The flash** is keyed to a real frame, throttled to the flush window.

`prefers-reduced-motion` disables all of it; every animation duplicates
information the numbers and colours carry.

## The voice core

### The orb is the control — there are no buttons

Tap the core or press SPACE to open the selected channel; tap again to end; hold
to transmit in PTT mode. In Dictation, tap/SPACE admits the first bounded turn;
after a reply the orb is **WAKE ARMED** — the explicit `Hey <assistant>` phrase or
another tap/SPACE admits the next turn, unrelated speech cannot. During
capture/processing, tap or Escape ends the exchange; during playback, a wake
phrase or tap is barge-in into the next turn. Affordances are a whisper line of
text, not chrome.

- `startVoiceCall` requires a registered realtime media session;
  `CoreStage` calls `ensureMediaSessionStarted({ threadId })` first.
- Tap vs hold: pointerdown starts a 220ms timer; crossing it engages PTT, and the
  pointerup after a hold must not count as a tap (or every transmission ends the
  call). SPACE is ignored in typing targets.

### Modes and the PTT-OFF gate

The whisper line has the iOS DICTATION · HANDS-FREE · LIVE picker. The value is
a browser-local key; only a successful backend media-preference read may seed it
(`recording` → Dictation, `hands_free` → Hands-free, `realtime` → Realtime), and
once the key exists shared preference updates never overwrite it. Selecting any
option writes the key.

Hands-free (`voice_mode: 'hands_free'`) dials the backend cascaded pipeline and
hides PTT (turn boundaries belong to the pipeline). Realtime opens the configured
low-latency transport. **Dictation** opens neither; one browser loop owns:

1. `startMicRecording` and its `AnalyserNode`;
2. 1.1s trailing-silence boundary, 8s no-speech exit, 45s utterance cap;
3. streamed `transcribeAudioStreaming` (same STT path as Chat);
4. `chatStore.sendMessage` on the thread pinned at loop start, with
   `source_surface=web_ambient_dictation` and `voice_origin=true`;
5. backend-parsed speech segments through provider TTS, browser voice as
   fallback;
6. an explicit activation gate after playback before any new capture.

Provider multi-block replies are predecoded and scheduled on one `AudioContext`
clock (clip N+1 starts at clip N's exact end, no `onended` gap); no-WebAudio
falls back to sequential `HTMLAudioElement`.

Wake admission is stricter than substring detection: browser Vosk partials have
no authority, a bare name is insufficient, and the final utterance must begin
with the full `Hey <assistant>`. During TTS only that detector stays armed; a
valid phrase cancels playback and admits the next turn.

STT, agent processing and TTS each have a three-minute safety deadline. A
generation token fences every capture, transcription, reply and TTS completion so
tap/Escape/unmount cannot revive a stale session (even where speech-synthesis
cancellation goes unreported). The pinned thread keeps navigation from moving an
active exchange. Turns advance in a `while` loop, not recursion (constant stack).
Without WebAudio analysis but with MediaRecorder, capture is a finite 8s turn and
STT decides whether speech exists.

A `turn_boundary: push_to_talk` session is minted `turn_detection_mode: "none"`;
the live toggle in `providers/openai.ts` maps an open-mic request to
`server_vad`, not null. When the backend drops an unaddressed open-mic turn
(`transcript.user.ignored`) it removes it from the transcript; the deck detects
the vanished live turn and flashes `turn ignored — start with "Hey …"`, and shows
`open mic — start with "Hey …"` while `call.addressingRequired`. The turn tracker
is a plain untracked object — a reactive Map read and written by one `$:` block
self-invalidates forever.

Text-rail dictation reuses the chat page's `MicCaptureButton` (compact), piping
`transcribeDelta`/`transcribe` into the draft — one capture engine everywhere.

### Motion and transcription

While the channel is alive the ribbon runs `organicRadialWaveform`: layered
sinusoids with **integer** angular frequencies (fractional ones crack at the 2π
seam), gain and speed scaled by the measured envelope, blended with the real
time-domain term. Offline is a still dashed ring. Turn colours on orb, caption and
≋ rows: **green = you, accent = agent, dimmed accent = listening**.

The newest turn renders under the orb, speaker-coded, decaying ~2.5s after
completion; its permanent home is the left rail (≋). The stage never holds a
second transcript.

`voiceViz.ts` (framework-free, unit-tested):

- **Time-domain RMS** about 128 from `getByteTimeDomainData`, not a frequency-bin
  average (which reads high on hiss).
- **Asymmetric envelope** — fast attack, slow release, like a VU meter.
- **Perceptual gain curve** that never saturates, so shouting outranks talking.
- **Radial waveform** with bucket averaging (~168 points from 1024 samples) to
  avoid aliasing.
- **Pulse rings** whose positions are the recent amplitude history.
- **Stage precedence**: error first; during barge-in the assistant outranks the
  user.

### The render loop never touches Svelte state

Per-frame values live on a plain untracked object (`rt`); reactive writes at
display rate would re-run component effects. Theme colours resolve by assigning
the CSS variable to a hidden probe and reading `getComputedStyle().color`
(several tokens are `color-mix(...)`), ~1×/second plus on stage change — never
per frame, since it forces a style recalc.

## The run graph

Behind the stage a radial execution tree grows from real telemetry (`runGraph.ts`
model + `StageGraph.svelte`): task clusters branch outward, every uplink frame is
a node chained to its task's previous event; agent frames hang off shared agent
nodes; anonymous frames are short-lived sparks; live tasks get labelled clusters
before their first event. Clicking a task in IN FLIGHT spotlights its cluster.

**Tasks are their own roots** (no task→orb edge). A run is task → steps (DAG via
`depends_on`) → per-step ACTION nodes naming the tool and delegate agent.
**VIEW RUN GRAPH** (while spotlit) brings the run FRONT: scrim, re-centred, 2.3×,
all labels, ESC/CLOSE. FRONT lays the cluster out as a layered DAG
(longest-chain depth); the ambient background stays radial.

**Any task is inspectable.** Selecting one seeds its run: the task node wears its
outcome (green completed / red failed / amber cancelled). Seeding reads the
execution panel first (per-step statuses for panel-tracked runs;
`execution_id` omitted → latest run), then `GET /v3/tasks/{id}/plan` →
`plan_graph` (structure/tools/`depends_on`; unknown statuses render dim).
Single-shot agentic runs have neither, and their one outcome node is the truth.
Seeding is idempotent and advances the chain tip so live events extend the limb.

- **Deterministic layout** — home angle is an FNV-1a hash of the id; no
  `Math.random()`.
- **Born at the parent, eased outward.**
- **Bounded** — per-kind TTLs, hard 150-node cap pruned oldest-first, orphans
  re-rooted. History lives in `/events`.
- **Render law applies** — no Svelte writes in the loop, probe colours ~1×/s,
  reduced motion pins nodes to targets (the graph still grows).

## Scheduler safety — never call `tick()` from a reactive statement

`tick()` re-enters the flush scheduler from the microtask a reactive statement
queued, re-runs the statement, and never terminates; each iteration is a separate
flush, so `effect_update_depth_exceeded` never trips. Use
`requestAnimationFrame` for post-render DOM work and keep change-guards on plain
objects (`const scrollMark = { count: -1 }`). `deck.test.ts` fails on any deck
component calling `tick()` from a reactive statement.

## Why this route needs its own bootstrap

`/warroom` is **outside** the `(app)` group, whose layout is the only place
`installScopedApiFetch()` runs; without it no workspace bearer is attached and
every store returns empty. The warroom layout installs it explicitly — anything
else outside `(app)` needs the same.

- The health probe must hit **`/health`** (proxied at root), not
  `/api/magician/health` (404 → permanent FAULT).
- The chat session load must not be driven off `scopeIdentityStore`:
  `loadActiveSession` calls `scopeIdentityStore.observe(...)` itself, so a
  reactive retry would re-trigger its own load. It is a bounded 4-attempt retry.

## Uplink coalescing (performance)

The event stream replays history on connect: it is requested with `limit=80` and
replayed frames are split from live ones (backfill goes on the tape but must not
drive the rate gauge). The buffer drains with `shift()`, backoff resets only
after 10s stable, and the reassembly buffer is capped at 1MB. Frames land in
non-reactive buffers (`pendingRows`, `pendingBucket`, `pendingFlash`) and flush on
a fixed **4 Hz** timer — bounded work however hard the stream runs. Any new
uplink consumer must follow the same discipline.

## Theming

**The deck owns no palette**; geometry and typography carry the identity.

| Deck variable | Resolves from |
| --- | --- |
| `--deck-glow` | `--accent-primary` (mode overrides to `--color-warning` / `--color-error`) |
| `--deck-bg` / `--deck-panel` | `--bg-base` / `--bg-surface` |
| `--deck-text` / `--deck-dim` | `--text-primary` / `--text-secondary` |
| `--deck-line` | `--border-default`, warmed toward the accent |
| `--sev-*` | `--color-success` / `--color-warning` / `--color-error` |
| fonts | `--theme-font-display` / `--theme-font-mono` / `--theme-font-body`, deck stack fallback |

The `ThemeSwitcher` sits in a `position: fixed` wrapper that is a **sibling** of
`.deck` (like `/`'s `.landing-theme-corner`): the deck is fixed with
`overflow: hidden`, so a nested absolute menu is clipped and the control reads
dead; the deck's `room-shift` animation also touches `filter`, which traps fixed
descendants.

Do not reintroduce a forced palette: the unprobed LED, the scanline film and the
separation of the three core arcs must all derive from foreground/background
tokens (mix toward the background, not white; no `multiply` over black), or they
vanish or wash out on light themes.

## Layout

Fixed viewport grid, no document scroll (the `+layout.svelte` scroll-lock):

```
┌───────────────── sysbar ─────────────────┐
│  text   │                    │  needs    │
│  chat   │   voice  stage     ├───────────┤
│         │                    │  flight   │
├───────────────── tape ───────────────────┤
```

- The tape is a 40px ambient strip (uplink state, last age, total, 60s sparkline,
  peak/s, `EVENT TAPE ↗` to `/events`).
- NEEDS YOU collapses to one ALL CLEAR strip when empty (grid row `auto`); IN
  FLIGHT fills idle with RECENT settled tasks.
- Do not name a deck class `.stat` — daisyUI's global `.stat` grid collides.
- The left rail is the text channel; its header (`#thread · session name`) opens
  the app's `HistoryDrawer`, and selection switches the deck's session in place
  via `onSelectSession`. The centre is voice only; elements below the core are
  normal-flow siblings of `.core`, not overflow inside the ring box.
- Below 1180px: stacked rails over the stage, scroll allowed.

## Testing

`deck.ts` holds the pure logic: severity classification (PascalCase typed
variants **and** dot-namespaced GAUI emits), `AgentEvent` envelope unwrapping
(the inner `event_type` belongs on the tape), the literal-zero timestamp guard
(legacy `#[serde(default)] i64` serializes 0), mode precedence, rate decay, and
the no-fabricated-baseline rule. `ambientVoiceMode.test.ts` and
`ambientDictationLoop.test.ts` cover mode mapping/precedence, capture
boundaries, pinned-session multi-turn progression, cancellation, late/missing
replies and constant-stack depth.
