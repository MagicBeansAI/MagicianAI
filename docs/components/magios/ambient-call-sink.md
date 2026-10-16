# Magios Ambient Call Sink

The awake half of [ambient mode](./ambient-mode.md): when the on-device wake spotter fires, this turns
the wake into a hands-free conversation on the existing realtime voice stack and reports its turns and
lifecycle back to the orb.

- `magios/Magios/AmbientCallSink.swift`: the protocol, the socket-free seam the controller is tested
  against.
- `magios/Magios/RealtimeAmbientCallSink.swift`: the implementation, wrapping `VoiceCallViewModel`.
- `magios/Magios/VoiceAudioEngine.swift`: `VoiceSessionDisposition`.
- `magios/Magios/VoiceCallViewModel.swift`: `VoiceCallMode` and the pre-ready gate.

Design: design §13.3 and §15.

---

## The audio-session disposition, and why it is a required parameter

**A wrong value here is a microphone the user cannot reopen, and no cheap check catches it.**

Apple DTS (thread 826462): with the `audio` background mode, activate the audio session only in the
foreground. Activate while visible and never deactivate, and the **engine** can start and stop from the
background indefinitely. **A deactivated session cannot be reactivated from the background.** A
`setCategory` plus re-activation plus a voice-processing rebuild from the background is just as fatal: the
input node comes back degenerate, the spotting tap's format guard refuses it, and the window disarms with
"Lost the microphone."

`AmbientMicEngine` never deactivates. The wake handoff gives the microphone to `VoiceAudioEngine`, whose
`start()`, its failure path and `stop()` all touch the shared session. Each takes a **required, named**
`VoiceSessionDisposition` with **no default**:

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

- **One value governs both ends**, so a caller cannot set one flag and forget the other.
- **The skip is an ownership decision, not an idempotence check.** No `AVAudioSession` API reports
  whether a session is active, and `category`/`mode` read back what was requested.
- **`setVoiceProcessingEnabled(true)` on input and output nodes runs on both paths.** It is
  per-`AVAudioEngine`, and without it the assistant hears itself and answers in a loop. It fails soft to
  half-duplex capture gating and publishes `echoCancellationActive`, so failure costs barge-in, not
  sound.
- With no ambient window armed, every in-app site resolves to `.release` and behaves exactly as a plain
  call; that is the acceptance criterion.

### Where the value comes from

`VoiceCallViewModel` carries a required `let mode: VoiceCallMode` (`.inApp` / `.ambient`), and every
teardown reads the view model's own `sessionDisposition`, because `teardownLocal()` runs on terminal
phases the caller never sees (server `session.end`, dropped socket, transport error).

Mode alone is not enough: an in-app Live call started inside an armed window would deactivate the
window's session on hangup. So the disposition also asks **`AmbientRail`**, the seam
`DictationController`, `ListenController`, `BackgroundEngine` and `SpeechSynthesizer` use:

```swift
func sessionDisposition(ambientWindowIsLive: () -> Bool) -> VoiceSessionDisposition {
    switch self {
    case .ambient: return .keepActive
    case .inApp: return ambientWindowIsLive() ? .keepActive : .release
    }
}
```

- **It does not yield.** Dictation and observation yield (design §17) because they use the microphone for
  something else; an in-app Live call is the same purpose, so the window survives it.
- **`.ambient` never consults the rail.** `AmbientController.disarm` clears `armedAt` before ending the
  call, so an ambient teardown often sees the rail read false.
- **Re-read when it matters, not latched at `startCall`**: a window can open during an in-app call, and a
  leash expiring mid-call makes the hangup release again.

`VoiceCallMode` decides three things as one type, because they are one fact (is a human looking at a call
panel?). As separate flags, a caller could set two of three and leave a live microphone nothing can speak
into.

| `VoiceCallMode` | `sessionDisposition` (no window) | `sessionDisposition` (window live) | `appliesStoredHoldToTalk` | `hushesChatNarration` |
|---|---|---|---|---|
| `.inApp` | `.release` | **`.keepActive`** | yes | yes |
| `.ambient` | `.keepActive` | `.keepActive` | **no** | **no** |

- **Monotonicity:** the rail can only turn `.release` into `.keepActive`, never the reverse.
- An in-app call under an armed window **configures nothing**. That is safe only because the two engines'
  session triples are asserted equal; otherwise it would run without echo cancellation.
- **`appliesStoredHoldToTalk`:** hold-to-talk mutes capture and requests a `push_to_talk` boundary. An
  ambient call has no button, so a stored preference (or a profile's `defaultsToPushToTalk`) would leave
  the orb reporting a conversation into a muted microphone.
- **`hushesChatNarration`:** `SpeechSynthesizer.stop()` routes through `finish()`, which calls
  `setActive(false, .notifyOthersOnDeactivation)` **unconditionally**. Hushing on the ambient path would
  deactivate the window's session. Nothing needs hushing there: `AmbientController` holds
  `VoiceCallAudioFocus` for the whole window, which suppresses chat auto-speak.

### The invariant to check after any edit

1. `VoiceAudioEngine.swift` contains **exactly one executable** `setActive(false…)`, inside
   `releaseSessionIfRequested`, guarded by `disposition == .release`.
2. It contains **exactly one executable `setCategory` and one executable `setActive(true)` reachable from
   `start`**, both inside `configureSharedSessionIfOwned` behind `disposition.configuresSharedSession`.
   (The interruption handler's `try?` `setActive(true)` is deliberate; design §15.)
3. Every `stop(`/`start(` on a `VoiceAudioEngine` passes the view model's `sessionDisposition`, never a
   literal `.release`, never `mode` alone.
4. `AmbientMicEngine`'s session triple equals `VoiceAudioEngine`'s
   (`callSessionCategory`/`callSessionMode`/`callSessionOptions`), so the handoff needs no session change
   (`AmbientMicEngineTests.testTheArmedWindowConfiguresTheSessionTheConversationNeeds`).
5. `VoiceCallViewModelTests` and `RealtimeAmbientCallSinkTests` assert `sessionReleaseCount == 0`
   **and `sessionConfigureCount == 0`** for `.ambient`.

Deactivations can span lines, so grep for `setActive` alone, or read the four methods above.

### Testing what the simulator cannot run

`stop()` returns early unless `isRunning`, which only a real microphone start sets. So `VoiceAudioEngine`
exposes:

- `lastSessionDisposition`, recorded **before** the `isRunning` guard;
- `sessionReleaseCount`, incremented only on an actual deactivation (ambient must be zero);
- `sessionConfigureCount`, the same for the outbound leg, mirroring
  `SpeechSynthesizer.sessionConfigureCount`.

`releaseSessionIfRequested` / `configureSharedSessionIfOwned` are `internal` so tests drive each mapping
directly, each ambient zero beside a positive control. Injected-rail tests cover all four armed × mode
combinations (`testTheDispositionTableOverArmedAndMode`), monotonicity
(`testTheRailCanOnlyEverAddAKeepNeverARelease`), re-read-not-latched, and that `.ambient` never consults
the rail while `.inApp` consults it once. The injected rail's `yield` is an `XCTFail`.

### Voice processing is primed once, in the foreground

The first `setVoiceProcessingEnabled(true)` in a process builds the VP AudioUnit and rebuilds the
engine's I/O, and the first **stop** of a VP-armed engine lapses the session activation. Paid lazily, both
land in the first backgrounded wake, where no `setActive` may repair them (the re-arm fails with
AURemoteIO `StartIO` `'what'`).

`AmbientController.arm` therefore runs a once-per-process `primeAudioGraph`: a **real** engine start/stop
cycle (`.release` both halves) in the foreground, before the spotter re-activates the session. Setting the
VP flag without running the engine does not build the AudioUnit.

- The prime defers while a live call holds `VoiceCallAudioFocus` (arm is reachable mid-call via the intent
  door).
- Only a cycle that actually ran spends the latch; a failed start retries at the next arm.
- Backgrounded re-arm does not retry session activation (futile, and it stalls the main actor). Left: a
  ~1.5 s settle before the connect retry, a foreground-only re-arm, and a clear loss reason ("iOS released
  the microphone while Magican was in the background — open Magican to listen again.").
- Logs: the prime line at arm, engine-stop lines (disposition, running, AEC, app state), and interruption
  lines.

`setPreferredSampleRate(24_000)` is not set on an ambient call (it would bias the hardware under the
window's tap at the next activation). Both capture paths resample from whatever `outputFormat(forBus:)`
reports, with a new resampler per tap.

---

## No pre-ready capture

Speech during wake-up and connect is not captured, buffered or forwarded: the assistant hears from
`session.ready` onward, and the connect wording ("Heard you, waking up…") does not invite talking.

`PreReadyGate` in `VoiceCallViewModel` drops and counts every frame captured before `.ready`, on in-app and
ambient calls; one log line at ready ("Discarded N bytes of pre-ready audio") is the tripwire. All three
arm sites are covered: ambient and in-app connects arm inside `startCall`, and the provider live-swap arms
in `performLiveProviderSwap` before capture
(`testTheLiveProviderSwapArmsThePreReadyGateBeforeCapture`). The gate opens from the transport's own
`.ready` handling (`RealtimeVoiceClient.onReady`), not the `$phase` subscriber, so a `.ready` superseded
within one run-loop turn cannot leave the microphone deaf.

`WakePreRoll` and `VoicePCM.resamplePCM16LE` remain as unwired pure types from the removed pre-roll design
(see the design doc); a 16 kHz buffer sent unconverted plays 1.5× slow.

---

## Turn projection

`AmbientTurn` comes from signals the call stack already publishes, plus one hook.

| Signal | Turn |
|---|---|
| A non-final user caption | `.listening` |
| A final user caption | `.thinking` |
| An incoming assistant audio frame | `.speaking` |
| The queued reply audio running out | `.listening` |
| An assistant caption | **nothing** |

- **The end of a reply is computed**, since nothing reports it. `AssistantPlaybackClock` models the
  player's queue (each frame's duration added from when the speaker is next free) and fires when it runs
  dry plus a 0.35 s tail. It is **not** a "no frame for N ms" debounce: providers stream faster than
  realtime. Any other turn cancels the watch, which re-checks `speaking` before firing (barge-in race).
- **A final user transcript is already addressed**: the backend reports an unaddressed utterance as
  `transcript.user.ignored`, which `RealtimeVoiceCaptionState` removes rather than finalises.
- **An assistant caption projects nothing**: text can arrive before, with or after its audio.
- The caption→turn mapping is a pure `static` function
  (`RealtimeAmbientCallSink.turn(forLatestCaption:)`).
- **`.speaking` comes from `VoiceCallViewModel.onAssistantAudio`**, fired after each frame is queued
  (wrapping `client.onIncomingAudio` would be undone by the next `wireAudioPaths()`).
- **The captions subscription does not hop** (`MainActor.assumeIsolated`), so a caption cannot overtake
  assistant audio and report `.thinking` over `.speaking`.
- **Non-replaying `PassthroughSubject`**; `AmbientController` subscribes before `startCall`. Turns are
  deduplicated.

---

## `startCall` and `endCall`

**`startCall`** starts the call and awaits `awaitReadySessionID()`. It binds no chat thread
(`uiThreadId: ""`, omitted by `startPayload`, so the backend creates one).

- **The failure reason is logged here and nowhere else; a disarm is not a failure.** `endCall` during a
  connect lands on `.ended`, logged at `info`; anything else logs at `error` with phase and reason. The
  return is a bare `Bool` on purpose (the controller only falls back to `.armed`). **Do not widen it.**

**`endCall()` is safe with no call and safe twice**: `AmbientController.disarm` calls it on every disarm,
including from `.armed`. It is `VoiceCallViewModel.hangUp()`: `client.end()` sends nothing without a
socket, `engine.stop(session:)` guards on `isRunning`, the focus release is nil-guarded, and
`RealtimeVoiceClient.teardown` bumps the start generation so an in-flight connect aborts.

- **It does not touch `VoiceCallAudioFocus`**, which `AmbientController` holds for the whole window.
- **It reports nothing on `lifecyclePublisher`**: `callIsLive` is cleared before `hangUp()`.

---

## Connect latency — the one line per attempt

`magios/Magios/VoiceConnectTrace.swift` (`VoiceConnectTrace`, stamped through the process-wide
`VoiceConnectTracer`); formatting tested in `VoiceConnectTraceTests.swift`. Logging only (no awaits,
blocking, reordering or audio-session work) and permanent.

One persisted `.notice` line per attempt, subsystem `ai.magicbeans.magios`, category `ambient.call` or
`voice.call`; filter on `voice.connect`:

```
voice.connect mode=ambient outcome=failed at=ws_up total=30517ms deltas_ms: wake>handoff=4
handoff>start=38 start>post=176 post>post_ok=210 post_ok>ws=1 ws>ws_up=88 ws_up>end=30000
err="Voice call didn't start in time."
```

| Field | Means |
|---|---|
| `mode=` | `ambient` (wake handoff) or `in-app` (Live panel). If in-app is as slow, the cost is voice-stack cold start. |
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
| `ready>flush` | Post-ready handling (gate opening); key name kept for tooling. |
| `dropped_b=` | Bytes the gate discarded before ready. |

Deltas are gaps between consecutive present stages, so they sum to `total`; a stage a mode lacks is
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

Stamps are first-write-wins: reconnects re-run stages, and the trace covers the first connect.

---

## `lifecyclePublisher` — how a conversation ends

`AmbientCallLifecycle` reports when an exchange is over:

- **`.quiet(until:)`**: nothing addressed to the assistant; the follow-up window
  (`Addressing.followUpWindowMs`, negotiated at `session.ready`) runs until `until`, and the user may
  continue without the activation phrase.
- **`.ended(AmbientCallEnded)`**: `.wentQuiet`, `.remote` (server `session.end`) or `.dropped`
  (reconnect backoff exhausted).

Contract:

1. **Non-replaying.** The first `.quiet` is emitted inside `startCall`.
2. **`.ended` is terminal and at most once per `startCall`** (`callIsLive`).
3. **Never emitted for a requested `endCall()`**, since the controller answers `.ended` by restoring the
   spotting tap and would reopen a microphone the user just stopped.
4. **The sink does not hang up on `.ended`**; it stops its timers and the controller calls `endCall()`.
   The microphone owner alone decides when the socket closes.

### The follow-up window

The sink owns the timer; the controller only renders the deadline (`AmbientState.cooldown(until:)`).
`.quiet` is re-emitted with a later deadline on every push, and the watch carries the deadline it was
armed for.

`followUpWindow(millis:)` clamps network values, falling back to
`RealtimeVoiceProtocol.Addressing.disabled.followUpWindowMs`. **The window arms at `session.ready`**, not
only after a reply, so a false wake on unrelated speech still times out.

| Turn | `followUpDisposition(for:)` |
|---|---|
| `.listening` | `refresh` — start or push out the window |
| `.thinking`, `.speaking` | `cancel` |
| `nil` | `leave` |

`.listening` refreshes rather than cancels, because an unaddressed utterance's caption is removed and
nothing more arrives. `nil` (an assistant transcript) leaves the window, so a reply whose audio never
arrives still times out. The refresh runs outside `report`'s deduplication, since repeated partials must
refresh.

**No response watchdog.** A conversation stuck in `.thinking` is bounded only by the armed window's hard
cap (`AmbientController.expiresAt`), so long tool
calls are not cut off; the orb shows the state and the user can end it.

## Why the implementation is its own file

The protocol is the socket-free seam the controller is tested against (as with `AmbientMicEngine` vs
`AmbientMicSource.swift`, `VoskWakeSpotter` vs `WakeSpotter.swift`). The sink **owns** its
`VoiceCallViewModel`, constructed `.ambient`; an injection point would admit a `.inApp` view model that
releases the shared session on teardown.

## Tests and device-only gaps

`MagiosTests/RealtimeAmbientCallSinkTests` and `MagiosTests/VoiceCallViewModelTests` cover the pre-ready
gate, audio-path wiring, turn projection (over the real caption reducer), the playback clock, `endCall`
safety, session disposition both ways, lifecycle non-replay, follow-up disposition and clamping;
`AmbientControllerTests` covers the controller's response to each lifecycle event.

`startCall` is not driven end to end (it needs a live backend). Device-only questions: whether the
playback clock tracks the real speaker through long replies and barge-in, whether real partials keep the
follow-up window pushed out, and whether the negotiated `followUpWindowMs` is comfortable. Each would show
as a conversation cut short, not as an error.
