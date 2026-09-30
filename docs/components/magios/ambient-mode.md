# Magios Ambient Mode

Ambient mode arms a listening window on iOS. While armed, an on-device wake
spotter listens locally for the activation phrase; on a hit, the app hands off
to the native conversation stack using the device-local **Ambient Listening →
Conversation mode** choice: Hands-free (cascaded FluidAudio), Live (the
selected realtime provider), or Dictation. The reply is spoken aloud. An orb
in the Dynamic Island is proof of life and the disarm control.

Arming is reachable from Control Center, the Lock Screen, the Action Button,
Shortcuts (`ArmAmbientIntent` + `MagiosAmbientControl`), and Settings.

The conversation call and the ambient window have different lifetimes. The
active call stays open for the negotiated eight-second quiet window so
follow-ups do not need the wake phrase. When that quiet window expires, only
the provider call ends: the controller reinstalls the on-device wake spotter
and the same bounded ambient window remains available. The window closes only
on Stop, its leash, a microphone failure, or another documented owner.

Wake-accuracy figures from the test suites are synthesised PCM fed straight
into the decoder — never through a microphone, echo canceller or AGC — while
the shipping spotter runs under `.voiceChat` with AEC/AGC in the live path.
Treat them as relative, not as real-room rates; see
[device item 8](#device-verification).

Design and task plan (archived):
design ·
plan. Awake half:
[ambient-call-sink.md](./ambient-call-sink.md).

### Conversation-mode routing

`AmbientController.shared` owns one `AmbientConversationCallSink`. The facade
snapshots the device-local mode at each wake and routes the entire conversation
through exactly one implementation:

- `RealtimeAmbientCallSink` handles Hands-free and Live with an explicit
  per-call `VoiceEngine`; it does not rewrite Chat's saved engine.
- `DictationAmbientCallSink` handles Dictation as a sequential loop:
  - One `AmbientMicEngine` input graph serves the whole conversation and is never
    stopped or replaced between turns. Its audio-thread resampler feeds 16 kHz
    mono PCM16 into a thread-safe per-turn gate; at an utterance boundary the
    gate closes, emits an immutable finalised WAV for the configured
    recording-STT path, and discards all input during STT, agent work and TTS.
    No background recorder, queue or replacement engine is allocated. A silent
    output graph exists only for the short initial handover.
  - After the 600 ms playback settle, follow-up admission pauses and restarts
    that same engine to re-prime StartIO and rebuilds the tap against the current
    hardware format before reopening the gate. A turn is not "listening" until
    the tap delivers fresh PCM; a running-but-deaf engine fails admission after
    1.5 s into the bounded three-attempt follow-up budget. It never activates or
    reconfigures the audio session.
  - One canonical chat session spans follow-ups; a 180-second deadline bounds
    each capture → STT → agent → TTS turn.

Ambient Dictation applies `TutorInvoke` before opening canonical chat: a leading
Tutor/Tutor Quick blackboard command ends the conversation and opens the native
overlay; locked requests speak the unlock instruction; screen Tutor/App Copilot
get the iPhone capability response. `TutorOverlayRouter` is the shared final rail
for all three modes: an admitted auto-start presentation yields the entire
ambient window before creating its presentation admission (otherwise ambient's
`VoiceCallAudioFocus` would suppress Tutor narration). A locked rejection does
not yield and is never queued for later.

Both implementations keep the `AmbientCallSink` lifecycle contract: requested
teardown is silent, ordinary no-speech ends as `wentQuiet`, failures end as
`dropped`, and generation guards discard callbacks from superseded captures,
network turns and speech. File transcription retains and cancels its
Speech/URLSession tasks and hands off temp-file ownership through a claim-once
stage reducer. The explicit STT route preserves the `On-device only` privacy
boundary when Apple recognition is unavailable. TTS does not own the
conversation lifetime: completed, failed, skipped and cancelled playback all
reopen the next bounded capture, scheduled as a new task, never recursively. The
ambient window remains the sole owner of the active `.voiceChat` session.

The backend media preference seeds the mode only when the iPhone has no saved
value; afterward it stays local and never changes Chat's voice controls.

## Why It Is Armed Rather Than Always On

Two hard iOS limits, with no workaround:

- **No overlay window.** Third-party apps cannot draw over other apps; the
  Dynamic Island / Live Activity is the only cross-app surface. Live Activity
  views are archived SwiftUI rendered out-of-process — no app render loop and no
  `repeatForever`. Continuous motion is available only from system-driven
  mechanisms: see [Motion](#motion).
- **Nothing wakes a terminated process on sound.** `UIBackgroundModes: audio`
  keeps the microphone open once running, but dies with the process on
  swipe-kill, reboot or jetsam.

The orb can be *armed*, never *summoned*. The arming contract is the product.

## Why Arming Opens The App Once

**Capture cannot be started from the background.** A background-launched App
Intent may not activate a recording session (CallKit, LiveCommunicationKit and
PushToTalk are exempt only because a separate system process activates them).

Apple DTS (thread 826462): have the `audio` background mode and **only activate
the audio session in the foreground**. Activate once while visible, never
deactivate, and the engine can be started and stopped from the background
indefinitely.

So the cost is **one visible app launch per armed window, not per
interaction.** Arming is an `OpenIntent`; after it, one-tap Talk and every wake
hit run from the background for the life of the window. That is why
`setActive(false, …)` teardown is a correctness bug here — see
[Collaborator seams](#collaborator-seams) and [Microphone tap](#microphone-tap).

## Privacy Invariant

While waiting for a wake phrase, **no audio leaves the device**. The wake spotter
has no networking collaborator, so the guarantee is structural. Audio reaches
the network only after the phrase matches locally or the user taps **Talk to
Magican**.

Server-side name-gating exists independently via `RealtimeVoiceProtocol.Addressing`
(`required`, `activationPhrases`, `followUpWindowMs`), negotiated in
`session.ready`, with `transcriptIgnored` for unaddressed speech.

## Components

### No pre-ready capture

Speech during wake-up/connect is not captured, buffered or forwarded; the
assistant hears from `session.ready` onward, and the activation-neutral wording
(**Starting conversation…**) never claims the provider is listening.
`AmbientCallSink.startCall` carries no audio; the call's pre-ready gate drops and
counts early frames (see [ambient-call-sink.md](./ambient-call-sink.md)).
`magios/Shared/WakePreRoll.swift` remains as an unwired pure type (2 s of 16 kHz
PCM16; even capacity and appends are `precondition`s; the owner serializes
`append` against `drain`/`reset`).

### State machine and orb reduction

Two vocabularies, kept apart:

- `magios/Magios/AmbientState.swift` — `AmbientState`, nine lifecycle states
  (`off`, `arming`, `armed`, `heard`, `connecting`, `conversing`, `cooldown`,
  `disarming`, `recoverableError`), plus `AmbientTurn`. App-only.
- `magios/Shared/AmbientActivityAttributes.swift` — `AmbientOrbPhase` (`armed`,
  `heard`, `listening`, `thinking`, `speaking`) and the `ActivityAttributes`,
  shared with the widget. Fixed attributes identify the window (`agentName`,
  `armedAt`, `expiresAt`); `ContentState` carries only what changes.

`AmbientState.orbPhase` reduces nine states to five, and
`orbPhaseChanged(from:to:)` reports whether the reduction moved — ActivityKit
update budgets are finite, so an invisible transition is not published. Captions
are gated separately. The ActivityKit verb comes from the two reductions: `nil` →
non-`nil` is `request`, non-`nil` → `nil` is `end`, otherwise `update`.

- **`recoverableError` reads as `armed`** (terminal failures disarm instead, so
  the spotter is still listening).
- **`cooldown` reads as `listening`** (the microphone is admitted without the
  phrase).
- `off`, `arming`, `disarming` reduce to `nil`. `disarming` carries
  `reason: String?`, published as `ContentState.endedReason`.

**`ContentState` decodes defensively** — the system hands persisted state to
whichever widget binary is installed later. `init(from:)` reads every field with
`decodeIfPresent` and falls back to memberwise defaults; unknown
`AmbientOrbPhase` values degrade to `armed`. Do not return to synthesised
decoding: a new non-optional field must be a compile error until old payloads
have a meaning, and a property-level default compiles but silently never decodes.
Tests: `MagiosTests/AmbientStateTests`.

### Arm record

`magios/Shared/AmbientArm.swift` — the persisted armed window (`armedAt`,
`capSeconds`, `ownerID`) as JSON in the App Group `UserDefaults` under
`ambient.activeArm`, so the widget-process disarm intent and the app agree on
what is armed. Same verbs as `ObservationArm` (`save()` / `claim()` / `clear()`).

- **Relaunch semantics are inverted from observation.** An ambient window has no
  server-side existence, so a record whose `ownerID` (regenerated per launch) is
  not the running process is garbage, never a session to resume
  (`isStale(currentOwnerID:)`).
- `claim()` returns whatever is stored without judging ownership (the widget
  would find every record stale). `isExpired(now:)` takes an injectable clock.
- **Undecodable bytes are dropped** by `claim()`; corrupt is not stale.
- `AmbientArmTests` writes the real App Group and preserves any existing record
  (clearing a live one would leave the tap running while disarm reads `nil`). Do
  not copy that onto `ObservationArm`.

### Collaborator seams

Everything non-value the controller touches reaches it through app-only
protocols in `magios/Magios/`:

- `WakeSpotter.swift` — on-device phrase detection and `WakeHit`.
- `AmbientMicSource.swift` — the armed-window tap (16 kHz mono PCM16, no sink).
- `AmbientCallSink.swift` — the awake conversation seam (Dictation or realtime).
- `AmbientActivitySink.swift` — the orb.
- `AmbientDictationKeepingAlive` (in `RealtimeAmbientCallSink.swift`) — silent
  local output used as the provider-stop/wake-start session bridge; no input or
  network destination.

`AmbientController.shared` injects `AmbientMicEngine`, `VoskWakeSpotter`,
`AmbientConversationCallSink` and `AmbientActivity`; the silent
`NativeAmbientDictationKeepalive` bridge is used on the end path. They are
protocols because the controller must be provable without a simulator
microphone, a Dynamic Island or a backend. They live in `Magios/`, not `Shared/`,
so a wake engine cannot link a speech model into the widget extension.

`AmbientActivitySink.updateCaption` is separate from `update(phase:)`.
`AmbientCallSink` has two non-replaying publishers, both subscribed before
`startCall`: `turnPublisher` (who is making a sound) and `lifecyclePublisher`
(whether the microphone is admitted without the phrase, then that the
conversation is over — see [The end of a conversation](#the-end-of-a-conversation)).
`AmbientMicSource` and `AmbientCallSink` are `@MainActor`; `onFrame` is a plain
closure called from the audio thread.

Contracts:

- **`WakeSpotter.onHit` is `@MainActor`**; `feed` is nonisolated and synchronous,
  so calling `onHit` from the audio callback does not compile.
- **`startCall` returns a bare `Bool`.** A failed connect retries once, then falls
  back to armed with "Couldn't connect — say the wake word to try again." (or
  closes with the cap's reason if the cap latched mid-wake).
- **`AmbientActivitySink.start` reports failure, and arming fails if the orb
  cannot be shown** — the orb is the disarm control. (`ObservationActivity`
  swallows failures; do not copy it.)
- **`endCall()` is safe with no call and safe twice**; `disarm` issues it every
  time. A re-entered `startCall`'s stale result is discarded by the controller.
- **`AmbientMicSource.start` takes an `@MainActor` `onFailure` channel**;
  throwing only reports a tap that never started. `stop()`'s no-callback
  guarantee covers both callbacks. Every acted-on session interruption is logged
  (type, reason, options, app state) here and in the call engine.

Doubles: `magios/MagiosTests/FakeAmbientCollaborators.swift`.
`FakeAmbientActivitySink` logs every call (`verbs`, `lifecycleVerbs`, `calls`).
Four doubles model refusal; `FakeWakeSpotter` does not — a spotter does not fail,
it simply does not hit.

### Wake spotter

`magios/Magios/VoskWakeSpotter.swift` — the concrete `WakeSpotter`.

Engine: Vosk (Apache 2.0, offline, runtime plain-string phrases). Porcupine needs
a compiled model per phrase and cannot accept server-supplied
`Addressing.activationPhrases`. Accepted costs: a 68 MB model resident for every
armed window, and an [open iOS leak](https://github.com/alphacep/vosk-api/issues/1970).
Recognizer construction is sub-millisecond, so every state clear rebuilds.
While armed the type touches only the bundled model directory and the PCM
passed to `feed`.

#### Grammar-constrained, not open-vocabulary

Unlike the desktop/web integrations (open-vocabulary transcription then
`contains(phrase)`), the decoder is restricted to the phrases plus `[unk]`, and
matching is a contiguous **whole-token** run. Carried over from desktop: do not
re-arm the instant a call ends, or the conversation's tail re-fires the spotter.

- **`[unk]` is mandatory**; without it the decoder force-maps every utterance
  onto the nearest phrase. `grammarJSON(for:)` always appends it.
- **`vosk_recognizer_set_grm` is absent from the iOS binary**; changing phrases
  rebuilds the recognizer. `magios/Vendor/vosk/include/vosk_api.h` is hand-written
  from the archive's `nm` symbols, so `set_grm` does not compile.
- **Out-of-lexicon words are silently dropped from a grammar.**
  `vosk_model_find_word` returns -1 for `"magican"` (`magician`, `presto`,
  `jarvis`, `computer` resolve). Every word of every phrase is validated and an
  unusable phrase is refused whole — "hey magican" would otherwise arm as "hey".

#### One refusal, and three things that can be said about a phrase that arms

`configure` records a `PhraseAssessment` per phrase:

| `PhraseAssessment` | Armed? | Meaning | Exposed as |
| --- | --- | --- | --- |
| `notInLexicon(unknownWords:)` | **No** | A grammar built from it would silently shorten. | `rejectedPhrases` |
| `measured(nearMissFalseAcceptPercent:syntheticTrueAcceptPercent:note:)` | **Yes** | This phrase went through the measurement matrix; carries both numbers, no verdict. | `phraseNotes` |
| `bareWord(note:)` | **Yes** | A single word with no measurement. | `phraseNotes` |
| `unmeasured(note:)` | **Yes** | In lexicon, never measured. **The ordinary production case.** | `phraseNotes` |

There is **no threshold** on either axis; surfaces render the note's sentence,
and the absence of a bad number is not an endorsement. `isArmed`,
`rejectedPhrases` and `phraseNotes` are all on the `WakeSpotter` protocol.

- **`isArmed == false` refuses the arm** before `mic.start`;
  `AmbientController.unusablePhrasesMessage(requested:rejected:)` distinguishes no
  phrase, lexicon rejection, and a decoder that did not come up
  (`vosk_recognizer_new_grm` returning NULL leaves an inert spotter with no
  rejected phrases — hence `isArmed`, not `rejectedPhrases.isEmpty`).
- **`phraseNotes` does not refuse**; it rides `AmbientController.listeningFor`
  (`AmbientPhraseSet`). Settings shows it before arming via the pure
  `assessment(of:)`; the one-line in-app bar does not.

#### What the measurements established

`MagiosTests/VoskWakeSpotterTests` and `VoskWakeAccuracyMeasurementTests`
(synthetic `AVSpeechSynthesizer` voices; a lower bound, not a real-world rate)
support these design decisions:

- **Finals only.** The hit path takes finalised results; partials exist only in
  the measurement suite. On partials, unrelated speech and adversarial openers
  ("listen, I'll call you back") false-fire; on finals they essentially do not.
  Finals add ~1.2 s of perceived latency, which costs no request audio now that
  nothing is captured before ready. Both suites use 1.5 s trailing silence.
- **No confidence signal to filter on** — near-miss accepts return confidence 1.0.
  `testNearMissFalseAcceptRate` pins a ceiling (a revert to partial matching fails
  loudly).
- **Accuracy is a property of the model's lexicon entry, not the phrase string**:
  spellings of the same sound resolving to different entries measure very
  differently. Name strength dominates prefix choice; a crowded name is not
  rescued by a prefix; `hey` helps modestly and `listen` does not beat it.
- A name absent from the lexicon (e.g. `tobo`) cannot be armed at all
  (`testToboIsRefusedWholeRatherThanMeasured`).
- Treat the *ordering* of phrases as the finding, not the absolute percentages.

#### Clearing decoder state rebuilds the recognizer

`reset()` does **not** call `vosk_recognizer_reset`: resetting mid-phrase then
feeding silence finalises the pending partial and fires. `reset()` and the
post-fire clear both rebuild from the stored grammar
(`testResetDiscardsPriorAudio` feeds silence after reset).

#### Cooldown

Two cooldowns; neither can do the other's job.

- `fireCooldown` (**4 s**, on the spotter): one spoken phrase produces a run of
  growing partials, and only the first may hit.
- `AmbientController.wakeResumeCooldown` (**2.5 s**, on the controller, as
  desktop's `WAKE_RESUME_COOLDOWN_MS`): only the controller knows a call just
  ended, and `reset()` forgets the fire. Applied on every route back to spotting.

`resumeSpotting` captures the window identity (`armedAt`) before its sleep and
compares after; a cancelled sleep enables nothing. A stopping voice-processing
AudioUnit can lapse the recording session even with `.keepActive`, so the end
path starts a silent local-output bridge **before** `call.endCall()`, keeps it
through the wake input graph's successful start, then releases it.
`wakeDecoderEnabled` stays closed during the cooldown (frames dropped in memory);
then Vosk resets and the gate opens. The duration is injected
(`AmbientController.resumeCooldown`).

#### Packaging

Both binaries are gitignored and fetched by `make setup-magios-vosk`, which
SHA-256-checks every slice and fails hard on mismatch; the header and module map
are committed. `make test-ios` / `test-ios-ui` run that setup; `make check-all`
only verifies (`verify-magios-vosk`).

- Scoped to the **app target only** (framework and `Accelerate` on `Magios`;
  `import CVosk` via module map, not a bridging header).
- The model is **bundled**, not an On-Demand Resource: ODR may be purged, and the
  spotter must work offline on first launch. Cost: 68 MB model (~39 MB
  compressed) plus Vosk/Kaldi.
- `PrivacyInfo.xcprivacy` declares `NSPrivacyAccessedAPICategoryFileTimestamp`
  (`C617.1`): libvosk `stat(2)`s the model directory.

Test fixtures are synthesised at test time via `AVSpeechSynthesizer`'s offline
write path. It delivers buffers on the main queue, so blocking on a semaphore
from a `@MainActor` test deadlocks and yields empty clips (reading as a perfect
0 false accepts); `testSynthesiserProducesAudio` and per-row non-empty audio
censuses guard that. Renderings are not cached (memory).

### Controller

`magios/Magios/AmbientController.swift` — the armed window: spotting tap,
handoff, hard cap, orb. Tested in `MagiosTests/AmbientControllerTests` with no
microphone, socket or ActivityKit.

- **Exactly one path owns the microphone — spotting XOR call.** The transfer
  bridge is output-only. The handoff calls `mic.stop()` before `startCall`; the
  failed-connect path restarts the tap. Arming is refused while
  `ListenController` holds an observation session (`observationIsActive`).
- **Nothing buffers while armed.** `ingest` (nonisolated, off the main actor)
  feeds the spotter and drops the frame.
- **Only `publishIfNeeded` publishes**, from the `state` `didSet`, switching on
  `(old.orbPhase, new.orbPhase)`; `arm`/`disarm` never call the sink. The
  `armedAt` gate prevents an orb on a window that never opened; `orbIsLive`
  prevents ending an unrequested activity; a refused request unwinds the window.
- **The turn subscription attaches before `startCall`**; turns are recorded in a
  lock-guarded box and applied on the main actor, so an early `thinking` is not
  overwritten by the default `listening`.
- **`handoff()` is the only non-atomic step** (`arm`/`disarm` have no `await`):
  `handleWake` enqueues it (disarm can run between `.heard` and its start), and
  `await call.startCall(…)` spans the connect and retry. A cap firing in
  `.heard`/`.connecting` **latches** (`capExpiredDuringWake`) and collects when
  the wake resolves. The post-await re-check compares the window (`armedAt`) and
  accepts `.connecting`, `.conversing` or `.cooldown`; `applyLatestTurn` and
  `applyLatestLifecycle` share that predicate.
- **A failed arm is recoverable** (`arm` admits `.recoverableError`). A
  microphone that dies mid-window disarms via `onFailure`. `WakeSpotter` needs no
  lock because `configure`/`reset` always sit outside a running tap.
- **A failed connect retries once after a ~1.5 s settle
  (`wakeConnectSettleDelay`), then falls back to `armed`, never `off`**, with a
  fresh `connectingSince` on retry and a standing connect notice afterwards
  (outranked only by the power warning; outranks the transcript).

### The end of a conversation

The seam is `AmbientCallSink.lifecyclePublisher` (`AmbientCallLifecycle`):

| Event | Meaning | Controller's answer |
|---|---|---|
| `.quiet(until:)` | Follow-up window runs until `until`; user may continue **without** the activation phrase. | `state = .cooldown(until:)`, which reduces to the `listening` orb. |
| `.ended(.wentQuiet)` | The window ran out. Ordinary end of an exchange. | `endCall()`, then `resumeSpotting()` → `.armed`. |
| `.ended(.remote)` | The server ended the session (`session.end`). | Same. |
| `.ended(.dropped)` | The transport gave up after its own reconnect backoff. | Same. |

- **One timer, owned by the sink**; `.cooldown(until:)` renders its deadline, and
  `.quiet` is re-emitted with later deadlines.
- **The window arms at session-ready** (the false-wake rail); the first `.quiet`
  is emitted before `startCall` returns, and `applyLatestLifecycle` runs after the
  handoff's default turn.
- **`.listening` refreshes the window; `.thinking`/`.speaking` cancel; `nil`
  leaves it** (`RealtimeAmbientCallSink.followUpDisposition(for:)`), applied
  outside the deduplicating `report`.
- **Cooldown is about admission, not audio.**
- **The sink never reports a hangup the controller asked for** (it would restore
  a tap after a disarm); `callIsLive` clears before `hangUp()`, and
  `FakeAmbientCallSink.emitEndedAfterHangUp` makes the forbidden behaviour a test.

**All three causes return to `armed`**, except a cap that expired during the
wake. `resumeSpotting`'s `mic.start` failures: a permission failure disarms
immediately; a **backgrounded** failure disarms on the first throw ("iOS released
the microphone while Magican was in the background — open Magican to listen
again."), since activation is skipped there by design; a **foregrounded** failure
retries up to two more times under a second apart (foreground state re-read each
time) before disarming with "Lost the microphone."

The orb reads `armed` during the 2.5 s resume cooldown; `wakeDecoderIsSpotting`
stays false until the gate opens, so a hit cannot replace the in-flight resume.
The failed-connect route sets `.armed` before its `await`.

### Safety rails

An armed window depends on one property: **the shared `AVAudioSession` is
activated in the foreground and never deactivated** (DTS 826462). Break it and
the window fails at the *next* wake word, off screen, with the orb still claiming
to listen.

`AmbientMicEngine` has no executable `setActive(false)`. Neighbours that
deactivate or re-categorise the session are gated by `magios/Magios/AmbientRail.swift`
(one injected struct, two verbs):

| Site | Disposition |
|---|---|
| `DictationController` (`startLive` yield; one guarded `releaseSession`) | **Yield** the window, with a reason. A user pressing hold-to-talk wants dictation. |
| `ListenController.startEngine` / `stopEngine` | **Yield**. Observation ranks above ambient and reconfigures the session without `.allowBluetoothHFP`. Yield is on `startEngine` so a failed server start never costs the window. |
| `BackgroundEngine.playSilence` / `stop` | **Defer**: leave the session exactly as found. An armed window already holds the keepalive `playSilence` exists to fabricate. |
| `SpeechSynthesizer.finish` | **Defer**: deactivate only if `configureSession()` actually ran (`holdsSession`). `AmbientController.arm` calls `abandonSessionClaim()`. The deactivation is not hopped to the main actor. |
| `SpeechSynthesizer.configureSession` | **Refuse**: leave the shared session alone. `.playback` has no input, so swapping to it takes the microphone out from under a live tap. The reply still plays on ambient's `.playAndRecord`. |
| `VoiceCallViewModel` in-app Live call | **Share**: `sessionDisposition` re-read on every teardown. An in-app call is the same purpose as the window. `.ambient` does not consult the rail: `disarm` clears `armedAt` *before* it ends the call. The rail is monotone: it can only turn a `.release` into a `.keepActive`. |

Every backstop is counted (`sessionReleaseCount` idiom) with a positive control.
`AmbientRail.live` reads `AmbientController.shared`; tests swap the struct.
`windowIsLive` reads `armedAt`, not the orb phase (`.recoverableError` reduces
to `armed` for a window that never opened).

#### Battery and Low Power Mode

`magios/Magios/AmbientPowerMonitor.swift`. The battery floor (`batteryFloor`
0.20) **refuses** a window; Low Power Mode **warns** and arms anyway. A negative
`UIDevice.batteryLevel` means unknown and is allowed; charging is exempt from the
floor but not from Low Power Mode. Low Power Mode is a **level** at arm time and a
**transition** for a live window:

| | `admission(_:)` | `liveBlock(_:lowPowerModeWasOn:)` |
|---|---|---|
| Below the floor, not charging | `refused(.batteryLow)` | `.batteryLow` |
| Low Power Mode on | `allowedWithWarning(.lowPowerMode)` | `.lowPowerMode` **only on a transition into it** |
| Both | `refused(.batteryLow)` | `.batteryLow` |

Copy: refusal "Plug Magican in to listen hands-free"; warning "Low Power Mode —
higher battery use"; ended "Low Power Mode turned on, so Magican stopped
listening". Caption priority (`publishOrbCaption`): power warning > connect
notice > finalised transcript. `power.start` is the last statement in `arm`; the
callback calls `disarmNow` directly, re-deriving the verdict when it acts.

#### `disarm` is synchronous, and that is now load-bearing

`disarm(reason:)` is a thin `async` face over `disarmNow(reason:)`, which contains
no `await`. Rails call it from synchronous UI handlers and notification hops; the
microphone must be off **now**.

#### Microphone permission

`AmbientMicEngine.handleDidBecomeActive` is the only moment a Settings
revocation can be noticed (`foregroundOutcome` → `onFailure`).
`micFailureReason` / `micUnavailableMessage` split out `recordPermissionMissing`.
With no tap during a conversation, a revocation then surfaces only when
`resumeSpotting`'s `mic.start` throws.

#### Backgrounding is a no-op, and it is the normal case

Nothing in the ambient path observes `didEnterBackgroundNotification`; the mic
engine observes `didBecomeActive` only. **A surviving `AmbientArm` is collected
on launch, never adopted**: `reconcileOnLaunch` clears stale/expired records and
ends orphaned activities unconditionally (guarded against mid-window calls). The
hard cap is a cancellable `Task` disarming with a user-facing reason. The Vosk
model loads lazily on the first `configure`.

### Entry point and app-side lifecycle

Files: `magios/MagiosIntents/ArmAmbientIntent.swift`, the Talk widget and
`MagiosAmbientControl` in `magios/MagiosWidgets/MagiosWidgets.swift`,
`magios/Magios/AmbientEntryPoint.swift`, the Settings control, `AmbientMiniBar` in
`AppShell.swift`, and the activation hook in `App.swift`.

Every Talk surface (widgets, Control Center/Action Button control, Shortcuts,
Settings) says **Talk to Magican** and goes through `AmbientEntryPoint.talk()`,
which opens the window if needed and calls `AmbientController.talkNow()` — the
same spotting-to-call handoff a wake uses. A duplicate tap is rejected
synchronously; a tap during the resume cooldown waits for spotting to return.
The Control Center control uses the custom `MagicanTalkControl` SF Symbol, and
the app reloads the control's kind on launch so upgrades adopt it. Shortcuts
donates `TalkToMagicanIntent` beside Start Listening.

**+30 min:** while the window can grow, the Lock Screen and expanded island show
`ExtendAmbientIntent`, which runs without opening the app, records one
coalescing App Group command and posts a Darwin notification. The app applies it
only to a still-live window, atomically moving the cap timer, `AmbientArm`,
`ContentState.expiresAt` and `staleDate`. Extensions clamp at eight hours from
the original `armedAt` (the button disappears at the ceiling); a simultaneous
Stop is consumed first.

`ArmAmbientIntent` is deliberately an `OpenIntent` (arming is the one visible
moment), listed in both `Magios` and `MagiosWidgets` sources. `perform()` only
persists `PendingAction.ambientArm` before activation; the action is latched in
`AppActions.consumePendingIntentAction()`. `StartVoiceIntent`, legacy pending
values and bare `magican://voice` migrate to `ambientArm`; explicit
`?mode=dictation|hands_free|live` links stay one-shot routes.

#### The activation steps, and why they are ordered

`App.handleActivation()` → `AmbientEntryPoint.handleActivation()` (skipped under
`--ui-test`):

1. **`reconcileOnLaunch()`, once per launch** (clears the pending disarm record).
2. **`consumePendingDisarmRequest()`, every foreground** — the intent leaves its
   request standing when unacknowledged.
3. **`consumePendingExtensionRequest()`** — after Stop, so simultaneous controls
   never keep alive a window the user asked to stop.
4. **Arm / talk, if requested** — strictly after step 2.

#### Where the wake phrases come from at arm time

`Addressing.activationPhrases` arrives only during a call, but the spotter needs
phrases first. `AmbientActivationPhrases.forArming(identity:)` reproduces the
server's construction (`magician-media/src/media_rails/voice_addressing.rs`) from
the App-Group-cached `PrimaryAgentSiriIdentity`: aliases plus canonical name,
strip a leading "hey", return `"Hey <name>"`, tokenising like the server's
`tokens()`. **There is no fallback name**: an empty cache yields no phrases and
`arm` refuses. `PrimaryAgentSiriAdvertiser` refreshes the cache every foreground.

#### The leash

`AmbientLeash` (in `AudioSettings.swift`): **30 min / 2 hours / Until I stop**,
App Group key `audio_ambient_leash`, default 2 hours. Every option is finite:
`untilStopped` is **8 hours** because ActivityKit ends a Live Activity after
eight hours. Settings shows each phrase's assessment before arming.

#### Settings and the one-time nudge

The Settings stop affordance keys off `AmbientState.windowIsOpen`, never
`orbPhase`; refusals render in Settings; Talk is disabled with an empty phrase
set. **`AmbientControlCenterHint` fires once, ever**, only after a window opened
from Settings and its first conversation returned to quiet; the versioned flag
(App Group, beside `SiriPhrasePresentation.customPhraseKey`) is set when the
prompt is raised.

#### `AmbientMiniBar` — the orb is unreachable while the user is in the app

The Dynamic Island does not show the owning app's Live Activity while that app
is foreground. `AmbientMiniBar` docks above the tab bar on every tab: one line —
**Available** plus the wake phrase, remaining leash, and a stop control;
**Starting conversation…** during connect; the Low Power warning replaces the
phrase. Visibility is `AmbientState.windowIsOpen`, not `orbPhase != nil`
(`.recoverableError` maps to `.armed`).

### Orb

Five files; the directory is load-bearing:

- `magios/Magios/AmbientActivity.swift` — concrete `AmbientActivitySink`. App-only.
- `magios/MagiosWidgets/AmbientLiveActivity.swift` — the `Widget`, registered in
  `MagiosWidgetBundle` (an unregistered `Widget` compiles and never renders).
- `magios/Shared/AmbientOrbAppearance.swift` — phase → appearance,
  `AuroraPalette`, `AmbientReceipt`.
- `magios/Shared/AuroraOrbView.swift` — `AuroraOrbView`, `AmbientLeashRing`,
  `auroraPhaseEase`; also used by the task Live Activity and in-app beacon.
- `magios/Shared/AuroraBlobShape.swift` — organic silhouette (three-frequency
  sine mix), used at 22 pt and up; smaller stays a circle.

### Motion

Live Activity views render out-of-process; widget `repeatForever` does not run.
The rate limit applies to pushes, not to views deriving progress from a date
range.

| Mechanism | Who renders | ActivityKit cost | Used for |
| --- | --- | --- | --- |
| Cross-dissolve of inserted/removed views on a content-state change | System | none beyond the update | Every phase change. Same-identity property easing is ignored out-of-process; the orb swaps view identity with its palette (`.id` on the gradient stops). |
| Time-driven views (`Text(timerInterval:)`, `ProgressView(timerInterval:)`) | System, continuously | **zero** | Leash countdown ring and digits; reply progress bar; compact conversing ring; connect give-up gauge |
| SF Symbol effects | System | zero | `thinking` / `heard` pulses. The halo `.pulse` is inert out-of-process; the static glow carries it. |

Microphone-reactive motion belongs to in-process `VoiceCallPanel`. Decorative
system motion is allowed where it cannot misstate the phase; invented durations
are forbidden.

| Phase | Fill (flat fallback) | Scale | Status | Motion |
| --- | --- | --- | --- | --- |
| `armed` | white at 35% | 0.85 | "Available — say Hey {agent}" | still |
| `heard` | purple | 1.15 | "Starting conversation…" | indeterminate |
| `listening` | purple | 1.0 | "Listening" | still |
| `thinking` | orange | 0.95 | "Thinking" | indeterminate |
| `speaking` | green | 1.1 | "Speaking" | elapsing |

Tests pin invariants over `AmbientOrbPhase.allCases`, not colour values: armed is
dimmer than every conversing phase; ended has no halo; no two phases share a body
gradient or blob seed; no phase differs from `armed` by scale alone; every live
phase glows.

#### `heard` is the connect wait

Both `.heard(phrase:)` and `.connecting` reduce to `heard`; the visible wait is
`.connecting`, dominated by backend provider-session creation (typically 11–13 s).
A sixth phase would spend a publish on a milliseconds-long state.
**Starting conversation…** names the pause as work and never claims capture
before readiness; hearing starts when "Listening" takes over.

#### Every animation corresponds to something real

`AmbientOrbMotion` (exhaustive `switch` over all phases):

- **`.elapsing` — only `speaking`**: `AssistantPlaybackClock` computes the end.
- **`.indeterminate` — `thinking` and `heard`**: no response deadline exists and
  the connect's end is the backend's; a bar over an invented range is forbidden.
- **`.still` — resting phases and any ended window** (also the default).

#### The reply's span, and how it reaches the orb

`ContentState` carries `speakingFrom` / `speakingUntil` (both `decodeIfPresent`),
read only via `speakingSpan`, a failable `AmbientSpeakingSpan` — not a
`ClosedRange<Date>`, whose `Decodable` traps when `lower > upper`. The span is a
pulled property (`AmbientCallSink.speakingSpan`); the only push is a re-emission
of the unchanged `.speaking` turn. (Rejected: an associated value on
`AmbientTurn`, which breaks dedup at frame rate; a lifecycle case, which clobbers
`.quiet` in the single-slot box; a third publisher.)

`queueSettleSeconds` (0.25 s) debounces frame arrival so the bar waits until the
provider stops sending; the span ends at the audio's end. Cost: one extra
ActivityKit update per reply. A long stutter revises the span (the bar can move
back); a process killed mid-reply leaves a stale `speaking` orb until `staleDate`.

#### The indeterminate pulse is the one thing asserted about the platform — so it is asserted nowhere

The treatment reads correctly whether or not the symbol effect animates
(coloured, scaled, captioned); degradation is a static tinted `ellipsis`, not a
false bar. See [What the motion pass owes on device](#what-the-motion-pass-owes-on-device).

#### Eased rather than cut

Phase change is an **identity swap**: `AmbientOrbGlyph` carries
`.id(appearance.palette.coreStops)`, shared by the lock-screen radial wash, never
on the leash ring. `Animation.auroraPhaseEase` (0.28 s `easeInOut`) is a ≤ 2 s
timing hint. Status words use `.contentTransition(.opacity)`.

Compact trailing indicator: a 16 pt mini leash ring (`AmbientConversingRing`)
plus a phase glyph (mic / ellipsis / speaker); no glyph while armed, through the
connect, or once ended.

- **Connect:** leading orb + **"Starting…"**; trailing gauge filling over
  `connectingSince` + `AmbientConnectAttemptWindow.attemptSeconds` (the constant
  the transport watchdog derives from). A retry republishes `connectingSince`; no
  clock, no gauge.
- **Conversing:** the leading word pulses (`AmbientPhaseWordPulse`: ~3 s shown
  every ~10 s) via controller-driven publishes (`resetPhaseWordPulse`).

A wake's phase publish carries an `AlertConfiguration` so the expanded island
auto-presents (Stop and Open in reach); an explicit Talk tap does not alert
(`.default` sound — the API has no silent case). Reduce Motion drops scale,
downgrades symbol effects to `.nonRepeating`, drops ring insertion scale and the
timing hint, but keeps the crossfade, progress bar and conversing ring.

#### Captions: finals only

The call seam emits each caption once, when it first turns final, with
`ContentState.captionRole`; empty finals are skipped. Conversation end
republishes the recomposed caption. Partials stay declined: the audience is not
looking, they compete with the Low Power warning, and they are unbounded. The
speaking span is a separate field so it cannot blank the warning. Widget
controls use `AuroraPalette.controlAccent`; conversation fill is
`ambientOrbAccent` (the widget process has no accent asset).

**An orb that has ended must not keep claiming to be heard.** The ended reason
replaces the status word; leash timer, caption and disarm control disappear.
Two paths: `endedReason` from `end(reason:)`, and `context.isStale` via
`staleDate: expiresAt`. `AmbientEndedReason.capReached` (asserts a cause) and
`.orphanCollected` (asserts nothing) are pinned apart; the cap/stale pair is
pinned together (`forWindow` and `windowIsOver` take `isStale` as a plain `Bool`).
An end with a reason lingers; the user's own disarm ends immediately. Surviving
activities are ended, never adopted (`endOrphans()`; `start` sweeps first). The
agent name is read at `start` from the cached identity; blank degrades to
"Listening".

**Stop performs a real disarm** (`Button(intent: DisarmAmbientIntent())`,
`.tint(.red)`, VoiceOver "Stop listening"). **Open** uses `magican://ambient`,
which `App.handleURL` claims with no voice action (bare `magican://voice` would
start a Talk turn). `AmbientOrbAppearance` is pure and tested without WidgetKit
(`AmbientOrbAppearanceTests`).

### Stop listening (internal disarm)

Both files are in `Shared/` because the button runs in the **widget** process:
`magios/Shared/AmbientSignal.swift` (Darwin name, `postDisarm()`, observer, App
Group request/acknowledgement records) and `magios/Shared/DisarmAmbientIntent.swift`
(`openAppWhenRun = false`).

An ambient window has no server side, so `StopObservationIntent`'s approach does
not transfer. **The orb disappearing must never be optimistic** — an orb that
stays up beats an orb that lies.

1. The intent publishes a request (`ambient.pendingDisarm`) with a fresh identity
   and posts Darwin `ai.magicbeans.magician.ambient.disarm`.
2. A running app's observer disarms for real, ends the orb via
   `publishIfNeeded`, and writes an acknowledgement carrying that identity.
3. The intent waits. An acknowledgement means the microphone is off (`mic.stop()`
   is synchronous; ActivityKit `end` follows — the orb outlives the microphone,
   never the reverse).
4. No acknowledgement: if the request is still outstanding, nothing is listening
   and the intent ends the orphan orb; if consumed, a live app is disarming — do
   not race it.

`awaitAcknowledgement` returns `acknowledged` / `silent` / `cancelled`; on
cancellation the intent does nothing. The request record stays even when the
intent ends the orb (resume-path evidence). `consumeAcknowledgement(of:)` matches
identity so a stale ack cannot answer a fresh request. **Budget: 1 s, polled
every 20 ms.** Residual risk: an alive, armed app whose main actor is blocked past
the budget can lose its orb while the mic runs; it self-heals when the actor
unblocks (Darwin is queued) and via the surviving request on resume.

A request finding nothing armed still sweeps orphans and acknowledges. `arm`
registers the observer right after saving the arm record; `disarm` and
`unwindLiveWindow` unregister. Tests (`AmbientSignalTests`,
`AmbientControllerTests`) preserve and restore the two App Group records.

### Microphone tap

`magios/Magios/AmbientMicEngine.swift` — the concrete `AmbientMicSource`: hardware
input resampled to 16 kHz mono PCM16 and handed to one callback. A port of
`ListenController`'s capture with deliberate differences; the most important is
an absence.

#### The audio session is activated only in the FOREGROUND, and NEVER deactivated

`AmbientMicEngine` has exactly **one** executable `setActive(true)` and **zero**
deactivations (the literal `setActive(false)` appears only in comments — strip
them before counting). `stop()` stops the engine, removes the tap, and leaves the
session active. Copying `ListenController.stopEngine()` here compiles, passes
tests, and makes the wake word work only while the app is on screen.

- **Where activation lives:** on every `start` while not backgrounded. A
  backgrounded `start` (`resumeSpotting`) sets the category, skips activation, and
  starts the engine. The gate is **app state, not an "activated once" latch** — a
  latch diverges from reality after interruptions, media-services resets or a
  neighbour's deactivation. Foregrounding does not re-activate a session that
  lapsed while away (that would be a second activation site).
- **`.mixWithOthers` is deliberately absent** (DTS guidance conflicts on whether
  a mixable session can activate from the background).

#### The window configures the session ONCE, with the mode the CONVERSATION needs

The mode is **`.voiceChat`**, because the armed window is the only party allowed
to configure the shared session and the conversation needs hardware echo
cancellation. Two halves, each insufficient alone: this engine configures
`.voiceChat`, so the handoff needs no mode change; and `VoiceAudioEngine.start`
configures nothing for a `.keepActive` caller. (A background mode change,
preferred-rate restatement, activation and VP rebuild returns a degenerate input
node and loses the microphone.) The two triples are asserted **equal**
(`AmbientMicEngineTests.testTheArmedWindowConfiguresTheSessionTheConversationNeeds`).

**Accepted cost:** `.voiceChat` puts system AEC/AGC on the spotting tap too. No
preferred rate is stated; the tap uses whatever `outputFormat(forBus:)` reports
and the resampler rebuilds on format change. Wake accuracy under AEC/AGC is
unmeasured ([device item 8](#device-verification)).

#### It follows the user's headset, which is the opposite of its neighbour

`.playAndRecord`, mode `.voiceChat`, options `[.defaultToSpeaker,
.allowBluetoothHFP]`: the reply is audible face-up, and capture follows what the
user wears (`ListenController` omits HFP so observation hears the room). The
category is re-applied on every `start` — setting a category is not activation,
so it is safe from the background, and on the return leg it restates the
identical triple.

#### 16 kHz mono PCM16, and why the byte count is a contract

`AmbientMicResampler` converts to 16 kHz mono float32 and encodes via
`VoicePCM.floatToInt16LE`. The encoded byte count is always even and never
sliced or padded. There is no fixed input format: route changes go through
`rebuildTap` (new tap, format, resampler). **One resampler per tap, owned by the
tap closure** — do not hoist it to a property.

#### No pump, no accumulator, no upload

While armed, captured audio has exactly one destination, on-device:
`WakeSpotter.feed` via `AmbientController.ingest`.

#### How `stop()`'s no-callback guarantee is actually kept

`AmbientMicSource.stop()` promises no `onFrame`/`onFailure` in flight or delivered
after it returns; `removeTap`/`engine.stop()` do not provide that.

- `onFrame` is delivered only inside a lock (`deliver`); `stop()` nils it inside
  the same lock.
- `onFailure` is main-actor with `stop()`; a generation counter (read before each
  handler's hop, bumped by `start` and `stop`) drops stale reports. The foreground
  permission re-check is exempt — it asks about whatever tap runs now.
- Lock order is one-way (the tap takes the frame gate; `ingest` takes nothing).
  `stop()` is safe with nothing running and safe twice.

#### The failure channel, and what each notification means

Each source reads the generation and parses into a `Sendable` outcome before
hopping:

| Source | Outcome |
|---|---|
| `AVAudioSession.interruptionNotification`, `.began` | fail |
| `AVAudioSession.interruptionNotification`, `.ended` | **ignore** — no `paused` phase, and the background cannot reactivate a session the system deactivated. An interruption ends the window. |
| `AVAudioSession.routeChangeNotification` | fail **only** if no input route remains. Reason is not a parameter: unplugging headphones is routine. |
| `AVAudioSession.mediaServicesWereResetNotification` | fail |
| `.AVAudioEngineConfigurationChange` (scoped to our engine) | rebuild the tap; fail only if the rebuild throws. Touches the *engine* only, no category and no activation. |
| `UIApplication.didBecomeActiveNotification` | fail if the record permission is gone *and* a tap is running |

A degenerate input format (zero sample rate or channels) is refused before
`installTap`, which would raise an uncatchable `NSException`. Reporting closes
the tap first, then calls the handler once (`stop()` nils `onFailure`).

`AVAudioEngine` capture has no simulator input, so `start`, `stop` and the tap
callback are untested; `AmbientMicEngineTests` covers the resampler, the session
triple (`nonisolated static let`, equal to `VoiceAudioEngine`'s, no
`.mixWithOthers`), and the notification → outcome mapping.

### Device verification

The simulator cannot exercise `AVAudioEngine` capture or out-of-process Live
Activity rendering; hardware checks live with the archived plans
(Task 13,
design §21, aurora plan);
call-path items are in [ambient-call-sink.md](./ambient-call-sink.md).

8. **Wake accuracy under `.voiceChat` is unmeasured.** The spotting tap runs with
   system AEC/AGC; the phrase ordering needs re-measuring at conversational
   volume, across a room, face-down, with music, and with the assistant audible.

Never captured as numbers: handoff latency percentiles, battery and thermals over
an armed hour, Live Activity update budget over a long window, and Vosk resident
memory against its open leak.

### What the motion pass owes on device

Out-of-process motion (symbol-effect cycling on `heard`, identity-swap fades,
compact ring and glyph fit, phase-word pulse, connect gauge and retry restart,
blob silhouettes, wake alert sound) can only be checked on hardware; the list is
in the archived aurora plan. Time-driven views are the motion that actually runs;
the halo `.pulse` is inert out-of-process.
