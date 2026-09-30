# Magios Ambient Call Sink

The awake half of [ambient mode](./ambient-mode.md): when the on-device wake spotter fires, this
turns the wake hit into a hands-free conversation on the existing realtime voice stack and reports the
conversation's turns and lifecycle back to the orb.

- `magios/Magios/AmbientCallSink.swift` — the protocol: the socket-free seam the controller is
  proved against.
- `magios/Magios/RealtimeAmbientCallSink.swift` — the concrete implementation, wrapping
  `VoiceCallViewModel`.
- `magios/Magios/VoiceAudioEngine.swift` — `VoiceSessionDisposition`.
- `magios/Magios/VoiceCallViewModel.swift` — `VoiceCallMode` and the pre-ready gate.

Design: design §13.3 and §15.

---

## The audio-session disposition, and why it is a required parameter

**A wrong value here is a microphone the user cannot reopen, and it is invisible everywhere it could be
caught cheaply.**

Apple DTS (thread 826462): with the `audio` background mode, *only activate the audio session in the
foreground*. Activate while visible and never deactivate, and the **engine** can be started and stopped
from the background indefinitely. **A deactivated session cannot be reactivated from the background.**
A mode change (`setCategory`) plus re-activation plus a voice-processing rebuild from the background is
just as fatal: the input node comes back degenerate, the spotting tap's format guard refuses it, and
the window disarms with "Lost the microphone."

`AmbientMicEngine` contains zero deactivations. The wake handoff gives the microphone to
`VoiceAudioEngine`, whose `start()`, `start()` failure path and `stop()` all touch the shared session.
Each takes a **required, named** `VoiceSessionDisposition`, with **no default value**:

```swift
enum VoiceSessionDisposition {
    case release, keepActive
    var configuresSharedSession: Bool   // .release only
}

func start(session: VoiceSessionDisposition, onFrameOut: @escaping (Data) -> Void) throws
func stop(session: VoiceSessionDisposition)
func configureSharedSessionIfOwned(_ disposition: VoiceSessionDisposition) throws
```

|  | `start` configures + activates | `stop` deactivates |
|---|---|---|
| `.release` | yes | yes |
| `.keepActive` | **no** | **no** |

- **One value governs both ends.** A call that must not hand the session back on the way out does not
  own it on the way in; two independent flags invite setting one and forgetting the other.
- **The skip is an ownership decision, not an idempotence check.** No `AVAudioSession` API reports
  whether a session is active, and `category`/`mode` read back what was *requested*. An ambient call is
  not entitled to do the work at all.
- **`setVoiceProcessingEnabled(true)` on input and output nodes still runs on both paths.** It is
  per-`AVAudioEngine`, not per-session, and load-bearing: without it the capture tap hears the
  assistant's own voice, server VAD scores it as a user turn, and the model answers itself in a loop. It
  fails soft — falling back to half-duplex capture gating and publishing `echoCancellationActive` — so
  its failure mode is lost barge-in, not silence.
- **Required, not a defaulted `Bool`**, so every call site states which it means. With no ambient window
  armed, every in-app site resolves to `.release` and behaves exactly as a plain call — that equivalence
  is the acceptance criterion.

### Where the value comes from

`VoiceCallViewModel` carries a required `let mode: VoiceCallMode` (`.inApp` / `.ambient`), and every
teardown reads the view model's own `sessionDisposition` — because not all teardowns are reachable from
the caller: `teardownLocal()` runs on every terminal phase (server `session.end`, dropped socket,
transport error), all of which must leave an armed window's session alone.

Mode alone is insufficient: an in-app Live call started *inside* an armed window would deactivate the
window's session on hangup, and the window would die at the next wake word. So the disposition consults
**`AmbientRail`** — the same injected seam `DictationController`, `ListenController`, `BackgroundEngine`
and `SpeechSynthesizer` use:

```swift
func sessionDisposition(ambientWindowIsLive: () -> Bool) -> VoiceSessionDisposition {
    switch self {
    case .ambient: return .keepActive
    case .inApp: return ambientWindowIsLive() ? .keepActive : .release
    }
}
```

- **It does not yield.** Dictation and observation yield (design §17) because they take the microphone
  for a different purpose. An in-app Live call is the same purpose inside a window the user armed, so the
  window survives the call.
- **Nothing changes for anyone not armed.** `.release` exists so the session is handed back and the
  user's music resumes; while a window is armed the session stays active regardless.
- **`.ambient` never consults the rail.** `AmbientController.disarm` clears `armedAt` *before* ending the
  call, so an ambient teardown routinely sees the rail read false. The rail is a closure so this
  independence is real.
- **Re-read at the moment it matters, not latched at `startCall`**: a window can open during an in-app
  call (Settings' arm control is not blocked), and a leash expiring mid-call makes the hangup polite again.

`VoiceCallMode` decides three things as one type, because they are one fact — *is a human looking at a
call panel, or is this an ambient wake handoff?* Split into flags, a caller could set two of three and
produce a live microphone nothing can speak into.

| `VoiceCallMode` | `sessionDisposition` (no window) | `sessionDisposition` (window live) | `appliesStoredHoldToTalk` | `hushesChatNarration` |
|---|---|---|---|---|
| `.inApp` | `.release` | **`.keepActive`** | yes | yes |
| `.ambient` | `.keepActive` | `.keepActive` | **no** | **no** |

- **Monotonicity:** the rail can only turn a `.release` into a `.keepActive`, never the reverse, so a wrong
  rail answer never makes deactivation more likely.
- An in-app call under an armed window also **configures nothing**. That is safe only because the two
  engines' session triples are asserted equal; if they diverged, such a call would run without echo
  cancellation.
- **`appliesStoredHoldToTalk`:** hold-to-talk mutes capture and requests a `push_to_talk` boundary. An
  ambient conversation has no button, so a stored preference (or a profile's `defaultsToPushToTalk`) would
  leave the orb reporting a live conversation into a muted microphone.
- **`hushesChatNarration`:** `SpeechSynthesizer.stop()` routes through `finish()`, which calls
  `setActive(false, .notifyOthersOnDeactivation)` **unconditionally**, even when nothing was speaking.
  Hushing on the ambient path would deactivate the window's session on the way in. There is nothing to
  hush there anyway: `AmbientController` holds `VoiceCallAudioFocus` for the whole window, which
  suppresses chat auto-speak.

### The invariant to check after any edit

1. `VoiceAudioEngine.swift` contains **exactly one executable** `setActive(false…)` — inside
   `releaseSessionIfRequested`, guarded by `disposition == .release`.
2. It contains **exactly one executable `setCategory` and one executable `setActive(true)` reachable from
   `start`**, both inside `configureSharedSessionIfOwned` behind `disposition.configuresSharedSession`.
   (The interruption handler's `try?` `setActive(true)` is kept deliberately; design §15.)
3. Every `stop(`/`start(` on a `VoiceAudioEngine` states a disposition read from the view model's
   `sessionDisposition` — never a literal `.release`, never `mode` alone.
4. `AmbientMicEngine`'s session triple equals `VoiceAudioEngine`'s
   (`callSessionCategory`/`callSessionMode`/`callSessionOptions`), so the handoff needs no session
   change (`AmbientMicEngineTests.testTheArmedWindowConfiguresTheSessionTheConversationNeeds`).
5. `VoiceCallViewModelTests` and `RealtimeAmbientCallSinkTests` assert `sessionReleaseCount == 0`
   **and `sessionConfigureCount == 0`** for `.ambient`.

**A single-line grep is not sufficient** — deactivations have been multi-line calls. Grep for `setActive`
alone, or read the four methods above.

### Testing what the simulator cannot run

`stop()` returns early unless `isRunning`, which only a real microphone start sets, so a guard after that
gate would pass forever unexecuted. `VoiceAudioEngine` therefore exposes:

- `lastSessionDisposition` — recorded **before** the `isRunning` guard.
- `sessionReleaseCount` — incremented only on an actual deactivation; the ambient answer must be zero.
- `sessionConfigureCount` — the same for the outbound leg (a re-categorisation takes a microphone as
  surely as a deactivation), mirroring `SpeechSynthesizer.sessionConfigureCount`.

`releaseSessionIfRequested` / `configureSharedSessionIfOwned` are `internal` so each mapping is driven
directly. Every ambient zero has a **positive control** beside it. The rail is pinned by injection: all
four armed × mode combinations (`testTheDispositionTableOverArmedAndMode`), monotonicity
(`testTheRailCanOnlyEverAddAKeepNeverARelease`), re-read-not-latched, and that `.ambient` never consults
the rail while `.inApp` consults it once. The injected rail's `yield` is an `XCTFail`.

### Voice processing is primed once, in the foreground

The first `setVoiceProcessingEnabled(true)` in a process instantiates the VP AudioUnit and rebuilds the
engine's I/O, and the first **stop** of a VP-armed engine lapses the session activation. Paid lazily, both
land inside the first backgrounded wake, where no `setActive` may repair them (the re-arm then fails with
AURemoteIO `StartIO` `'what'`).

`AmbientController.arm` therefore runs a once-per-process `primeAudioGraph`: a **genuine** engine
start/stop cycle (`.release` both halves) that spends the first VP build and first VP stop in the
foreground, before the spotter's start re-activates the session. Setting the VP flag without running the
engine does not build the AudioUnit and is not a substitute. Constraints:

- The prime defers (latch unspent) while a live call holds `VoiceCallAudioFocus` — arm is reachable
  mid-call via the intent door, and the cycle must not churn a session another call is riding.
- Only a cycle that actually ran spends the latch; a failed start retries at the next arm.
- Backgrounded re-arm does not retry session activation (futile, and each blocking round stalls the main
  actor). What remains: a ~1.5 s settle before the connect retry, a foreground-only transient-tolerant
  re-arm, and an honest loss reason ("iOS released the microphone while Magican was in the background —
  open Magican to listen again.").
- Log receipts: the prime line at arm, engine-stop stamp lines (disposition, running, AEC, app state), and
  interruption lines.

`setPreferredSampleRate(24_000)` is not stated on an ambient call (a hint applied at the next activation,
which would bias the hardware under the window's tap). Both capture paths resample from whatever
`outputFormat(forBus:)` reports, with a new resampler per tap.

---

## No pre-ready capture

Speech during wake-up/connect is not captured, buffered or forwarded: the assistant hears from
`session.ready` onward, and the connect wording ("Heard you, waking up…") does not invite talking.
Replaying seconds-stale speech at connect served no one.

`PreReadyGate` in `VoiceCallViewModel` drops and counts every frame captured before `.ready`, on in-app and
ambient calls alike; one log line at ready ("Discarded N bytes of pre-ready audio") is the tripwire if
this is reconsidered. All three arm sites are covered: ambient and in-app connects arm inside `startCall`,
and the provider live-swap (the one handshake bypassing `startCall`) arms in `performLiveProviderSwap`
before capture (`testTheLiveProviderSwapArmsThePreReadyGateBeforeCapture`). The gate opens from the
transport's own `.ready` handling (`RealtimeVoiceClient.onReady`), not the `$phase` subscriber, so a
`.ready` superseded within one run-loop turn cannot leave the microphone deaf.

The removed design (2 s `WakePreRoll` at 16 kHz, resampled to the transport's 24 kHz and flushed at ready)
is in the design doc. `WakePreRoll` and `VoicePCM.resamplePCM16LE` remain as unwired pure types; a
16 kHz buffer sent unconverted plays 1.5× slow.

---

## Turn projection

`AmbientTurn` is derived from signals the call stack already publishes, plus one observation hook.

| Signal | Turn |
|---|---|
| A non-final user caption | `.listening` |
| A final user caption | `.thinking` |
| An incoming assistant audio frame | `.speaking` |
| The queued reply audio running out | `.listening` |
| An assistant caption | **nothing** |

- **The end of a reply is manufactured**, because nothing reports it; otherwise the orb asserts
  `speaking` over silence. `AssistantPlaybackClock` models the player's queue — each frame's duration
  accumulated from when the speaker is next free — and fires when it runs dry plus a 0.35 s tail
  (`VoiceAudioEngine`'s half-duplex hangover). **Not a "no frame for N ms" debounce**: providers stream
  faster than realtime, so a debounce would drop to `listening` while the assistant is still audible. Any
  other turn cancels the watch, and the watch re-checks `speaking` before firing (barge-in race).
- **A final user transcript is already addressed**: the backend owns admission and reports an unaddressed
  utterance as `transcript.user.ignored`, which `RealtimeVoiceCaptionState` removes rather than finalises.
- **An assistant caption projects nothing**: text can arrive before, with or after its audio, so it would
  light the orb early or demote an established `.speaking`.
- The caption→turn mapping is a pure `static` function
  (`RealtimeAmbientCallSink.turn(forLatestCaption:)`), testable without a socket.
- **`.speaking` comes from `VoiceCallViewModel.onAssistantAudio`**, fired after each downstream frame is
  queued. Wrapping `client.onIncomingAudio` from outside would be undone by the next `wireAudioPaths()`.
- **The captions subscription does not hop** (`MainActor.assumeIsolated`): assistant audio reaches the
  actor with no hop, so a `.receive(on: RunLoop.main)` caption could overtake it and report `.thinking`
  over a begun `.speaking`.
- **Non-replaying `PassthroughSubject`**; `AmbientController` subscribes before `startCall`. A
  `@Published` would hide late-subscribe bugs. Turns are deduplicated (partials arrive per word, audio every
  ~20 ms).

---

## `startCall` and `endCall`

**`startCall`** starts the call and awaits `awaitReadySessionID()`. It binds no chat thread
(`uiThreadId: ""`, omitted by `startPayload`, so the backend creates a fresh one).

- **The failure reason is logged here and nowhere else; a disarm is not a failure.** `endCall` during a
  connect drives the client to `.ended` and lands on the same branch, so `.ended` logs at `info` and
  everything else at `error` with phase and reason. The return is a bare `Bool` by decision: the
  controller's only response is falling back to `.armed`, and the user-facing caption is deliberately
  generic. **Do not widen it.**

**`endCall()` is safe with no call and safe twice** — `AmbientController.disarm` issues it on every
disarm (including from `.armed`), and a disarm during `startCall`'s await ends a call that the handoff
then ends again. It is `VoiceCallViewModel.hangUp()`: `client.end()` sends nothing without a socket,
`engine.stop(session:)` guards on `isRunning`, the focus release is nil-guarded, and
`RealtimeVoiceClient.teardown` bumps the start generation so a connect in flight is aborted.

- **It does not touch `VoiceCallAudioFocus`**, which `AmbientController` holds for the whole window.
- **It reports nothing on `lifecyclePublisher`**: `callIsLive` is cleared *before* `hangUp()`.

---

## Connect latency — the one line per attempt

`magios/Magios/VoiceConnectTrace.swift` (`VoiceConnectTrace`, stamped through the process-wide
`VoiceConnectTracer`); formatting tested in `VoiceConnectTraceTests.swift`. Logging only — no awaits,
blocking, render-thread allocation, reordering or audio-session work — and meant to be permanent.

One `.notice` line per attempt (visible in Console.app without extra levels, persisted), subsystem
`ai.magicbeans.magican`, category `ambient.call` or `voice.call`; filter on `voice.connect`:

```
voice.connect mode=ambient outcome=failed at=ws_up total=30517ms deltas_ms: wake>handoff=4
handoff>start=38 start>post=176 post>post_ok=210 post_ok>ws=1 ws>ws_up=88 ws_up>end=30000
err="Voice call didn't start in time."
```

| Field | Means |
|---|---|
| `mode=` | `ambient` (wake handoff) or `in-app` (Live panel). If in-app is equally slow, the cost is voice-stack cold start, not ambient. |
| `outcome=` | `ready` \| `failed` (transport error or the 45 s watchdog) \| `ended` (torn down before ready) \| `superseded` |
| `at=` | Furthest stage reached; non-`ready` outcomes only. |
| `total=` | Wake → `session.ready`, or wake → where it stopped. |
| `wake>handoff` | The `Task` hop `handleWake` enqueues. |
| `handoff>start` | The mic stop. |
| `start>post` | Audio graph start, settings snapshot, `client.startCall`. |
| `post>post_ok` | `POST …/v2/media/sessions`. |
| `post_ok>ws` | Same run-loop turn; ~0. |
| `ws>ws_up` | WebSocket upgrade, timed off the `session.start` send completing (`URLSessionWebSocketTask` queues sends until upgrade). |
| **`ws_up>ready`** | Backend provider-session creation, prompt/tool/context setup, resume-context compaction. |
| `ready>flush` | Post-ready handling (pre-ready gate opening); key name kept for tooling stability. |
| `dropped_b=` | Bytes the gate discarded before ready. |

Deltas are gaps between *consecutive present* stages, so they sum to `total`; a stage a mode never has is
omitted, not printed as `0`.

| Stage | Stamped in |
|---|---|
| `wake` | `AmbientController.handleWake`, before the `Task` is enqueued |
| `handoff` | `AmbientController.handoff`, after its `.heard` guard |
| `start` | `VoiceCallViewModel.startCall`, first line (where an in-app trace begins) |
| `post` / `post_ok` | `RealtimeVoiceClient.register()`, either side of the round trip |
| `ws` | `RealtimeVoiceClient.openControl`, before `resume()` |
| `ws_up` | the `session.start` send completion, on `URLSession`'s queue |
| `ready` | `handleControl`'s `.ready` case, first line |
| `flush` | `VoiceCallViewModel.flushHeldUpstream`, before its `flushed > 0` guard |
| failure | `RealtimeVoiceClient.teardown`, which every non-ready ending passes through |

A process-wide singleton rather than a threaded token, so the independently testable seams need no API
change for a log. Stamps are first-write-wins (reconnects and rotations re-run stages; the trace is about
the first connect).

---

## `lifecyclePublisher` — how a conversation ends

Without it an armed window holds one conversation: nothing else reports that an exchange is over.
`AmbientCallLifecycle`:

- **`.quiet(until:)`** — nothing addressed to the assistant; the follow-up window
  (`Addressing.followUpWindowMs`, negotiated at `session.ready`) runs until `until`, and the user may
  continue without the activation phrase.
- **`.ended(AmbientCallEnded)`** — `.wentQuiet`, `.remote` (server `session.end`) or `.dropped`
  (reconnect backoff exhausted).

Contract:

1. **Non-replaying.** The first `.quiet` is emitted inside `startCall`, so replay would mask late-subscribe
   bugs.
2. **`.ended` is terminal and at most once per `startCall`** (`callIsLive`).
3. **Never emitted for a requested `endCall()`.** The controller answers `.ended` by restoring the
   spotting tap, so reporting a disarm's hangup would reopen a microphone the user just stopped.
4. **The sink does not hang up on `.ended`**; it stops its timers and the controller calls `endCall()`.
   One object — the microphone owner — decides when the socket closes.

### The follow-up window

The sink owns the timer; the controller only renders the deadline (`AmbientState.cooldown(until:)`). Two
timers would disagree, and the controller's could cut off a user mid-sentence. `.quiet` is re-emitted with
a later deadline on every push, and the watch carries the deadline it was armed for.

`followUpWindow(millis:)` clamps network values (zero would allow one sentence per wake), falling back to
`RealtimeVoiceProtocol.Addressing.disabled.followUpWindowMs`. **The window arms at `session.ready`**, not
only after a reply — the false-wake rail: a wake on unrelated speech receives nothing, so a reply-only
window would never arm and the conversation would run to the cap.

| Turn | `followUpDisposition(for:)` |
|---|---|
| `.listening` | `refresh` — start or push out the window |
| `.thinking`, `.speaking` | `cancel` |
| `nil` | `leave` |

`.listening` refreshes rather than cancels: an unaddressed utterance's caption is removed and nothing
further arrives, so cancelling would leave the exchange unable to end. `nil` (an assistant transcript)
leaves the window, so a reply whose audio never arrives still times out. The refresh is applied outside
`report`, which deduplicates repeated partials — exactly the deliveries that must refresh.

**No response watchdog.** A conversation stuck in `.thinking` is bounded only by the hard cap: a client
response deadline would cut off legitimate long tool calls, and the orb describes the state honestly and
lets the user end it.

## Why the implementation is its own file

The protocol is the socket-free seam the controller is proved against (as with `AmbientMicEngine` vs
`AmbientMicSource.swift`, `VoskWakeSpotter` vs `WakeSpotter.swift`). The sink **owns** its
`VoiceCallViewModel`, constructed `.ambient`; an injection point would admit a `.inApp` view model that
releases the shared session on teardown.

## Tests and device-only gaps

`MagiosTests/RealtimeAmbientCallSinkTests` and `MagiosTests/VoiceCallViewModelTests` cover the pre-ready
gate, audio-path wiring, turn projection (over the real caption reducer), the playback clock, `endCall`
safety, session disposition in both directions, lifecycle non-replay, follow-up disposition and clamping;
`AmbientControllerTests` covers the controller's response to each lifecycle event.

`startCall` itself is not driven end to end (it needs a live backend and `session.ready`). Device-only
questions: whether the playback clock tracks the real speaker through long burst-streamed replies and
barge-in, whether real provider partials keep the follow-up window pushed out, and whether the negotiated
`followUpWindowMs` is comfortable. Each would surface as a conversation cut short, not as an error.
