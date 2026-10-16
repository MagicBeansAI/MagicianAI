# Magios Ambient Mode

Ambient mode arms a bounded listening window on iOS. An on-device wake spotter
listens for the activation phrase; on a hit the app hands off to the native
conversation stack in the device-local **Ambient Listening → Conversation
mode**: Hands-free (cascaded FluidAudio), Live (the selected realtime
provider), or Dictation. A Dynamic Island orb is proof of life and the disarm
control. Arm from Control Center, the Lock Screen,
the Action Button, Shortcuts (`ArmAmbientIntent` + `MagiosAmbientControl`), or
Settings.

The call stays open for the negotiated eight-second quiet window (follow-ups
need no wake phrase); then only the provider call ends and the wake spotter is
reinstalled. The window closes only on Stop, its leash, a microphone failure,
or another documented owner. Test-suite wake-accuracy figures use synthesised
PCM fed straight to the decoder, not the shipping `.voiceChat` AEC/AGC path;
treat them as relative ([device item 8](#device-verification)).

Archived design ·
plan. Awake half:
[ambient-call-sink.md](./ambient-call-sink.md).

### Conversation-mode routing

`AmbientController.shared` owns one `AmbientConversationCallSink`, which
snapshots the mode at each wake:

- `RealtimeAmbientCallSink`: Hands-free and Live, with an explicit per-call
  `VoiceEngine`; Chat's saved engine is untouched.
- `DictationAmbientCallSink`: a sequential loop on one `AmbientMicEngine` input
  graph, never replaced between turns. A per-turn gate emits a finalised 16 kHz
  PCM16 WAV for recording STT and discards input through STT, agent and TTS (no
  background recorder, queue or replacement engine). After a 600 ms settle the
  same engine restarts with a rebuilt tap; a turn is not "listening" until
  fresh PCM arrives (a deaf engine fails after 1.5 s, three attempts, then
  `dropped`). It never activates or reconfigures the audio session. One chat
  session spans follow-ups; 180 s bounds each capture → STT → agent → TTS turn.
  Every TTS outcome (completed, failed, skipped, cancelled) reopens the next
  capture as a new task, never recursively. File transcription retains and
  cancels its Speech/URLSession tasks and hands off temp-file ownership once.

Dictation applies `TutorInvoke` first: a leading Tutor/Tutor Quick blackboard
command ends the conversation and opens the native overlay; a locked screen
speaks the unlock instruction; screen Tutor and App Copilot get the "not
available on this device" reply. `TutorOverlayRouter` is the final rail for all
three modes: an admitted Tutor auto-start yields the whole ambient window
before its presentation admission (else `VoiceCallAudioFocus` suppresses
narration); a locked rejection does not yield and is never queued.

Both sinks keep the `AmbientCallSink` contract: requested teardown is silent,
no-speech ends `wentQuiet`, failures end `dropped`, generation guards discard
superseded callbacks. STT keeps the `On-device only` boundary when Apple
recognition is unavailable. The ambient window is the sole owner of the
`.voiceChat` session. The backend media preference seeds the mode only when the
iPhone has no saved value; after that it stays local and never changes Chat's
voice controls.

## Why It Is Armed Rather Than Always On

The Live Activity is the only cross-app surface (out-of-process, no
`repeatForever`; see [Motion](#motion)), and nothing wakes a terminated process
on sound: `UIBackgroundModes: audio` holds the microphone only until
swipe-kill, reboot or jetsam. The orb can be *armed*, never *summoned*.

## Why Arming Opens The App Once

A background-launched App Intent may not activate a recording session
(CallKit, LiveCommunicationKit and PushToTalk are exempt only because a system
process activates them). Apple
DTS (thread 826462): with the `audio` background mode, **activate only in the
foreground** and never deactivate; the engine can then start and stop from the
background indefinitely. So arming (an `OpenIntent`) costs **one visible launch
per window**; Talk and every wake then run from the background, and
`setActive(false, …)` teardown is a bug ([Collaborator
seams](#collaborator-seams), [Microphone tap](#microphone-tap)).

## Privacy Invariant

While waiting for a wake phrase, **no audio leaves the device**: the wake
spotter has no networking collaborator. Audio reaches the network only after a
local phrase match or a tap on **Talk to Magican**. Speech during wake-up and
connect is never captured, buffered or forwarded (`AmbientCallSink.startCall`
carries no audio; the call's pre-ready gate drops and counts early frames; the
assistant hears from `session.ready` on, and **Starting conversation…** never
claims the provider is listening). `magios/Shared/WakePreRoll.swift` is an
unwired pure type. Server-side
name-gating is separate: `RealtimeVoiceProtocol.Addressing` (`required`,
`activationPhrases`, `followUpWindowMs`) in `session.ready`, with
`transcriptIgnored` for unaddressed speech.

## Components

### State machine and orb reduction

- `magios/Magios/AmbientState.swift`: app-only `AmbientState` (`off`,
  `arming`, `armed`, `heard`, `connecting`, `conversing`, `cooldown`,
  `disarming`, `recoverableError`) and `AmbientTurn`.
- `magios/Shared/AmbientActivityAttributes.swift`: `AmbientOrbPhase` (`armed`,
  `heard`, `listening`, `thinking`, `speaking`) and the widget-shared
  `ActivityAttributes` (fixed `agentName`, `armedAt`, `expiresAt`;
  `ContentState` carries what changes).

`AmbientState.orbPhase` maps `recoverableError` → `armed`, `cooldown` →
`listening`, `off`/`arming`/`disarming` → `nil` (`disarming`'s
`reason: String?` becomes `ContentState.endedReason`).
`orbPhaseChanged(from:to:)` skips invisible transitions; `nil` → non-`nil` is
`request`, the reverse `end`, else `update`. **`ContentState` decodes
defensively** (`decodeIfPresent`; unknown `AmbientOrbPhase` → `armed`), since
a later widget binary may read persisted state. Never return to synthesised
decoding: a property-level default compiles but never decodes. Tests:
`MagiosTests/AmbientStateTests`.

### Arm record

`magios/Shared/AmbientArm.swift` persists the window (`armedAt`, `capSeconds`,
`ownerID`) as JSON in App Group `UserDefaults` under `ambient.activeArm`.
Same verbs as `ObservationArm` (`save()` / `claim()` / `clear()`), but a record
whose per-launch `ownerID` is not the running process is garbage, never
resumed (`isStale(currentOwnerID:)`). `claim()` returns the record without
judging ownership (the widget would find every record stale) and drops
undecodable bytes; `isExpired(now:)` takes an injectable clock.
`AmbientArmTests` writes the real App Group and preserves any existing record.

### Collaborator seams

App-only protocols in `magios/Magios/` (not `Shared/`, so no speech model
links into the widget):

| Protocol | Owns | Concrete |
| --- | --- | --- |
| `WakeSpotter.swift` | phrase detection, `WakeHit` | `VoskWakeSpotter` |
| `AmbientMicSource.swift` | armed-window tap, 16 kHz mono PCM16, no sink | `AmbientMicEngine` |
| `AmbientCallSink.swift` | the awake conversation | `AmbientConversationCallSink` |
| `AmbientActivitySink.swift` | the orb (`AmbientActivitySink.updateCaption` separate from `update(phase:)`) | `AmbientActivity` |
| `AmbientDictationKeepingAlive` (in `RealtimeAmbientCallSink.swift`) | silent output bridging provider stop to wake start | `NativeAmbientDictationKeepalive` |

- **`WakeSpotter.onHit` is `@MainActor`**; `feed` is nonisolated and
  synchronous; `AmbientMicSource` and `AmbientCallSink` are `@MainActor`;
  `onFrame` is a plain closure on the audio thread.
- `turnPublisher` and `lifecyclePublisher` are non-replaying, subscribed before
  `startCall` ([The end of a conversation](#the-end-of-a-conversation)).
- **`startCall` returns a bare `Bool`**; after its one retry fails the
  controller shows "Couldn't connect — say the wake word to try again." (or
  closes with the cap's reason if the cap latched). A re-entered `startCall`'s
  stale result is discarded. **`endCall()` is safe with no call and safe
  twice**; `disarm` issues it every time.
- **Arming fails if the orb cannot be shown** (`AmbientActivitySink.start`
  reports failure, unlike `ObservationActivity`).
- **`AmbientMicSource.start` takes an `@MainActor` `onFailure` channel**;
  throwing only reports a tap that never started; after
  `AmbientMicSource.stop()` neither callback fires.

Doubles: `magios/MagiosTests/FakeAmbientCollaborators.swift`.

### Wake spotter

`magios/Magios/VoskWakeSpotter.swift`: Vosk (Apache 2.0, offline, runtime
phrases; Porcupine cannot take server-supplied
`Addressing.activationPhrases`). Costs: a ~68 MB model and an [open
iOS leak](https://github.com/alphacep/vosk-api/issues/1970). While armed it
touches only the bundled model directory and the PCM passed to `feed`.

#### Grammar-constrained, not open-vocabulary

- The decoder sees only the phrases plus `[unk]` (**mandatory**;
  `grammarJSON(for:)` appends it); a match is a contiguous **whole-token** run.
- **`vosk_recognizer_set_grm` is absent from the iOS binary**
  (`magios/Vendor/vosk/include/vosk_api.h` is hand-written from `nm` symbols),
  so new phrases, `reset()` and the post-fire clear rebuild the recognizer.
  `reset()` never calls `vosk_recognizer_reset` (mid-phrase reset then silence
  fires).
- **Out-of-lexicon words are silently dropped from a grammar**
  (`vosk_model_find_word` returns -1 for `"magican"`), so every word is
  validated and an unusable phrase is refused whole.

#### One refusal, and three things that can be said about a phrase that arms

| `PhraseAssessment` | Armed? | Meaning | Exposed as |
| --- | --- | --- | --- |
| `notInLexicon(unknownWords:)` | **No** | Grammar would silently shorten. | `rejectedPhrases` |
| `measured(nearMissFalseAcceptPercent:syntheticTrueAcceptPercent:note:)` | **Yes** | Both numbers, no verdict. | `phraseNotes` |
| `bareWord(note:)` | **Yes** | Single word, unmeasured. | `phraseNotes` |
| `unmeasured(note:)` | **Yes** | In lexicon, never measured; **the ordinary case**. | `phraseNotes` |

There is **no threshold**. **`isArmed == false` refuses the arm** before
`mic.start`; `AmbientController.unusablePhrasesMessage(requested:rejected:)`
distinguishes no phrase, lexicon rejection, and a decoder that did not come up
(`vosk_recognizer_new_grm` returning NULL leaves an inert spotter with no
rejected phrases, hence `isArmed`, not `rejectedPhrases.isEmpty`).
`phraseNotes` does not refuse; it rides `AmbientController.listeningFor`
(`AmbientPhraseSet`), and Settings shows it before arming via the pure
`assessment(of:)` (the in-app bar does not).
`MagiosTests/VoskWakeSpotterTests` and `VoskWakeAccuracyMeasurementTests`
(synthetic voices, a lower bound) established: **finals only** (partials
false-fire; finals cost ~1.2 s), **no confidence signal** (near-miss accepts
return 1.0; `testNearMissFalseAcceptRate` pins a ceiling so a revert to
partials fails), and accuracy follows the name's lexicon entry, not the phrase
string. A name absent from the lexicon (`tobo`) cannot arm
(`testToboIsRefusedWholeRatherThanMeasured`). The phrase *ordering* is the
finding, not the percentages.

#### Cooldown

- `fireCooldown` (**4 s**, spotter): only the first of a phrase's growing
  partials may hit.
- `AmbientController.wakeResumeCooldown` (**2.5 s**, as desktop's
  `WAKE_RESUME_COOLDOWN_MS`; injected as `AmbientController.resumeCooldown`):
  on every return to spotting, so a conversation's tail cannot re-fire;
  `wakeDecoderEnabled` stays closed meanwhile. `resumeSpotting` compares the
  window (`armedAt`) across its sleep; a cancelled sleep enables nothing. The
  orb reads `armed`, but `wakeDecoderIsSpotting` stays false until the gate
  opens, so a hit cannot replace the in-flight resume.

The end path starts a silent output bridge **before** `call.endCall()` (a
stopping voice-processing AudioUnit can lapse the session even with
`.keepActive`) and releases it once the wake graph starts.

#### Packaging

`make setup-magios-vosk` (`scripts/setup-magios-vosk.sh`) fetches the
gitignored libvosk slices, SHA-256-checks each and exits non-zero on mismatch,
then fetches the model zip (not checksummed); header and module map are
committed. `make test-ios`
/ `test-ios-ui` run setup; `make check-all` only verifies
(`verify-magios-vosk`). App target only (`import CVosk`, `Accelerate` on
`Magios`). The model is **bundled**, not an On-Demand Resource, so it works
offline on first launch (~68 MB unpacked). `PrivacyInfo.xcprivacy`
declares `NSPrivacyAccessedAPICategoryFileTimestamp` (`C617.1`) because
libvosk `stat(2)`s the model directory. The model loads lazily on the first
`configure`. Fixtures come from `AVSpeechSynthesizer`, which delivers on the
main queue: blocking on it from a `@MainActor` test deadlocks into empty clips
that read as 0 false accepts, so `testSynthesiserProducesAudio` and per-row
audio censuses guard them.

### Controller

`magios/Magios/AmbientController.swift`: spotting tap, handoff, hard cap, orb
(`MagiosTests/AmbientControllerTests`).

- **Exactly one path owns the microphone: spotting XOR call** (`mic.stop()`
  before `startCall`; the keepalive bridge is output-only; a failed connect
  restarts the tap). Arming is refused during an observation session
  (`observationIsActive`).
- **Nothing buffers while armed**: `ingest` (nonisolated) feeds the spotter
  and drops the frame.
- **Only `publishIfNeeded` publishes** (from the `state` `didSet`; `arm` /
  `disarm` never call the sink); the `armedAt` and `orbIsLive` gates stop orbs
  for unopened windows and ends for unrequested activities; a refused request
  unwinds the window.
- **The turn subscription attaches before `startCall`**; turns land in a
  lock-guarded box applied on the main actor, so an early `thinking` is not
  overwritten by the default `listening`.
- **`handoff()` is the only non-atomic step** (`arm` / `disarm` have no
  `await`). A cap firing in `.heard`/`.connecting` latches
  (`capExpiredDuringWake`) and collects when the wake resolves. The post-await
  re-check compares `armedAt` and accepts `.connecting`, `.conversing` or
  `.cooldown`.
- A failed arm is recoverable (`.recoverableError`); a mid-window microphone
  death disarms via `onFailure`. A failed connect retries once after ~1.5 s
  (`wakeConnectSettleDelay`, fresh `connectingSince`), then sets `.armed`
  (**never `off`**) with a standing connect notice.

### The end of a conversation

Seam: `AmbientCallSink.lifecyclePublisher` (`AmbientCallLifecycle`); the sink
side and follow-up timer are in [ambient-call-sink.md](./ambient-call-sink.md).

| Event | Meaning | Controller's answer |
|---|---|---|
| `.quiet(until:)` | Follow-up window; user may continue **without** the phrase. | `state = .cooldown(until:)` (`listening` orb). |
| `.ended(.wentQuiet)` | Window ran out; ordinary end. | `endCall()`, then `resumeSpotting()` → `.armed`. |
| `.ended(.remote)` | Server ended the session (`session.end`). | Same. |
| `.ended(.dropped)` | Transport gave up after its reconnect backoff. | Same. |

The sink never reports a hangup the controller asked for. Every end returns to
`armed` unless the cap expired during the wake. If `resumeSpotting`'s
`mic.start` fails: permission failure disarms at once; **backgrounded**, the
first throw disarms ("iOS released the microphone while Magican was in the
background — open Magican to listen again."); **foregrounded**, up to three
attempts 0.8 s apart (`micStartAttempts`, `micStartRetryDelay`; foreground
re-read each time), then "Lost the microphone."

### Safety rails

The shared `AVAudioSession` must stay active (DTS 826462), or the next wake
word fails off screen while the orb claims to listen. Neighbours that
deactivate or re-categorise it go through `magios/Magios/AmbientRail.swift`:

| Site | Disposition |
|---|---|
| `DictationController` (`startLive` yield; one guarded `releaseSession`) | **Yield** the window, with a reason. |
| `ListenController.startEngine` / `stopEngine` | **Yield** (observation outranks ambient and drops `.allowBluetoothHFP`), on `startEngine` so a failed server start never costs the window. |
| `BackgroundEngine.playSilence` / `stop` | **Defer**: leave the session as found. |
| `SpeechSynthesizer.finish` | **Defer**: deactivate only if `configureSession()` ran (`holdsSession`); `AmbientController.arm` calls `abandonSessionClaim()`. |
| `SpeechSynthesizer.configureSession` | **Refuse**: `.playback` has no input; the reply plays on ambient's `.playAndRecord`. |
| `VoiceCallViewModel` in-app Live call | **Share**: `sessionDisposition` re-read on every teardown; can only turn `.release` into `.keepActive`. `.ambient` does not consult the rail (`disarm` clears `armedAt` before ending the call). |

Every backstop is counted (`sessionReleaseCount` idiom) with a positive
control; `AmbientRail.live` reads `AmbientController.shared` (tests swap the
struct), and `windowIsLive` reads `armedAt`, not the orb phase
(`.recoverableError` reduces to `armed` for a window that never opened). `disarmNow(reason:)` (behind `disarm(reason:)`) has no `await`, so
rails in synchronous handlers turn the microphone off **now**.

#### Battery and Low Power Mode

`magios/Magios/AmbientPowerMonitor.swift`. The battery floor (`batteryFloor`
0.20) **refuses** a window; Low Power Mode **warns** and arms anyway. A
negative (unknown) `UIDevice.batteryLevel` is allowed; charging is exempt from
the floor only.

| | `admission(_:)` | `liveBlock(_:lowPowerModeWasOn:)` |
|---|---|---|
| Below the floor, not charging | `refused(.batteryLow)` | `.batteryLow` |
| Low Power Mode on | `allowedWithWarning(.lowPowerMode)` | `.lowPowerMode` **only on a transition into it** |
| Both | `refused(.batteryLow)` | `.batteryLow` |

Copy: refusal "Plug Magican in to listen hands-free"; warning "Low Power Mode —
higher battery use"; ended "Low Power Mode turned on, so Magican stopped
listening". Caption priority (`publishOrbCaption`): power warning > connect
notice > finalised transcript. `power.start` is the last statement in `arm`;
its callback calls `disarmNow` directly.

#### Microphone permission and backgrounding

`AmbientMicEngine.handleDidBecomeActive` is the only moment a Settings
revocation is noticed (`foregroundOutcome` → `onFailure`; `micFailureReason` /
`micUnavailableMessage` split out `recordPermissionMissing`). With no tap
during a conversation, a revocation surfaces only when `resumeSpotting`'s
`mic.start` throws. Backgrounding is the normal case and a no-op: nothing
observes `didEnterBackgroundNotification`. **A surviving `AmbientArm` is
collected on launch, never adopted**: `reconcileOnLaunch` clears stale/expired
records and ends orphaned activities (guarded against mid-window calls). The
hard cap is a cancellable `Task` disarming with a user-facing reason.

### Entry point and app-side lifecycle

Files: `magios/MagiosIntents/ArmAmbientIntent.swift`, the Talk widget and
`MagiosAmbientControl` in `magios/MagiosWidgets/MagiosWidgets.swift`,
`magios/Magios/AmbientEntryPoint.swift`, the Settings control, `AmbientMiniBar`
in `AppShell.swift`, and the activation hook in `App.swift`.

- **Talk:** every **Talk to Magican** surface calls
  `AmbientEntryPoint.talk()` → `AmbientController.talkNow()` (the wake
  handoff), opening the window if needed. A duplicate tap is rejected
  synchronously; a tap during the resume cooldown waits for spotting. Control
  Center uses the `MagicanTalkControl` SF Symbol (the app reloads the
  control's kind on launch); Shortcuts donates `TalkToMagicanIntent`.
- **+30 min:** shown on the Lock Screen and expanded island while the window
  can grow. `ExtendAmbientIntent` runs without opening the app (one coalescing
  App Group command + Darwin notification), applies only to a live window, and
  moves the cap timer, `AmbientArm`, `ContentState.expiresAt` and `staleDate`
  together, clamped at eight hours from `armedAt` (the button disappears at the
  ceiling); a simultaneous Stop wins.
- **Arm:** `ArmAmbientIntent` (an `OpenIntent`, in both `Magios` and
  `MagiosWidgets` sources) only persists `PendingAction.ambientArm`, latched in
  `AppActions.consumePendingIntentAction()`. `StartVoiceIntent`, legacy pending
  values and bare `magican://voice` migrate to `ambientArm`;
  `?mode=dictation|hands_free|live` links stay one-shot routes.

#### The activation steps, and why they are ordered

`App.handleActivation()` → `AmbientEntryPoint.handleActivation()` (skipped
under `--ui-test`):

1. **`reconcileOnLaunch()`, once per launch** (clears the pending disarm record).
2. **`consumePendingDisarmRequest()`, every foreground**: the intent leaves its
   request standing when unacknowledged.
3. **`consumePendingExtensionRequest()`**, after Stop, so a window the user
   asked to stop is never extended.
4. **Arm / talk, if requested**, strictly after step 2.

#### Wake phrases, leash and in-app surfaces

- **Phrases:** `AmbientActivationPhrases.forArming(identity:)` mirrors the
  server's `"Hey <name>"` construction
  (`magician-media/src/media_rails/voice_addressing.rs`: aliases plus
  canonical name, leading "hey" stripped, tokenised like the server's
  `tokens()`) from the App-Group-cached `PrimaryAgentSiriIdentity` (refreshed
  by `PrimaryAgentSiriAdvertiser` every foreground). **There is no fallback
  name**: an empty cache refuses `arm`.
- **Leash:** `AmbientLeash` (in `AudioSettings.swift`): **30 min / 2 hours /
  Until I stop**, App Group key `audio_ambient_leash`, default 2 hours.
  `untilStopped` is **8 hours**, ActivityKit's Live Activity limit.
- **Settings:** the stop control keys off `AmbientState.windowIsOpen`, never
  `orbPhase`; refusals render there; Talk is disabled with no phrase.
  **`AmbientControlCenterHint` fires once, ever**, after a Settings-opened
  window's first conversation returns to quiet (versioned App Group flag beside
  `SiriPhrasePresentation.customPhraseKey`, set when the prompt is raised).
- **`AmbientMiniBar`:** the island hides the app's own Live Activity in the
  foreground, so this bar docks above the tab bar on every tab while
  `AmbientState.windowIsOpen` (not `orbPhase != nil`): **Available** plus
  phrase, remaining leash and stop; **Starting conversation…** during connect;
  the Low Power warning replaces the phrase.

### Orb

- `magios/Magios/AmbientActivity.swift`: concrete `AmbientActivitySink`.
- `magios/MagiosWidgets/AmbientLiveActivity.swift`: the `Widget`, registered in
  `MagiosWidgetBundle` (an unregistered `Widget` never renders).
- `magios/Shared/AmbientOrbAppearance.swift` (phase → appearance,
  `AuroraPalette`, `AmbientReceipt`; `AmbientOrbAppearanceTests`),
  `magios/Shared/AuroraOrbView.swift` (`AuroraOrbView`, `AmbientLeashRing`,
  `auroraPhaseEase`; also the task Live Activity and in-app beacon) and
  `magios/Shared/AuroraBlobShape.swift` (organic silhouette at 22 pt and up;
  smaller stays a circle): shared views.

### Motion

`repeatForever` does not run out-of-process, and same-identity property easing
is ignored there; the rate limit applies to pushes, not to views that derive
progress from a date range.

| Mechanism | Who renders | ActivityKit cost | Used for |
| --- | --- | --- | --- |
| Cross-dissolve on a content-state change | System | none beyond the update | Every phase change (`.id` on the gradient stops). |
| Time-driven views (`Text(timerInterval:)`, `ProgressView(timerInterval:)`) | System, continuously | **zero** | Leash ring and digits; reply progress bar; compact conversing ring; connect gauge |
| SF Symbol effects | System | zero | `thinking` / `heard` pulses; the halo `.pulse` is inert out-of-process. |

Every animation reflects something real; no invented durations.
Microphone-reactive motion belongs to in-process `VoiceCallPanel`.

| Phase | Fill (flat fallback) | Scale | Status | Motion |
| --- | --- | --- | --- | --- |
| `armed` | white at 35% | 0.85 | "Available — say Hey {agent}" | still |
| `heard` | purple | 1.15 | "Starting conversation…" | indeterminate |
| `listening` | purple | 1.0 | "Listening" | still |
| `thinking` | orange | 0.95 | "Thinking" | indeterminate |
| `speaking` | green | 1.1 | "Speaking" | elapsing |

- `heard` (`.heard(phrase:)` and `.connecting`) is the connect wait, mostly
  provider-session creation (typically 11–13 s): **"Starting…"** and a gauge
  over `connectingSince` + `AmbientConnectAttemptWindow.attemptSeconds`.
- `.elapsing` (`AmbientOrbMotion`) runs only for `speaking`, over
  `ContentState`'s `speakingFrom` / `speakingUntil` (read via the failable
  `speakingSpan`, `AmbientSpeakingSpan`, never a `ClosedRange<Date>`, whose
  `Decodable` traps when `lower > upper`), pulled from
  `AmbientCallSink.speakingSpan` (`AssistantPlaybackClock`) after
  `queueSettleSeconds` (0.25 s); the only push is a re-emitted `.speaking`
  turn (one extra update per reply). A long stutter revises the span; a killed
  process leaves a stale `speaking` orb until `staleDate`. `.still` covers
  resting phases and any ended window.
- The indeterminate pulse degrades to a static tinted `ellipsis`, never a false
  bar ([What the motion pass owes on device](#what-the-motion-pass-owes-on-device)).
- Phase changes swap identity (`AmbientOrbGlyph`'s
  `.id(appearance.palette.coreStops)`, never on the leash ring) eased by
  `Animation.auroraPhaseEase` (0.28 s `easeInOut`); status words use
  `.contentTransition(.opacity)`. Compact trailing: 16 pt
  `AmbientConversingRing` plus a glyph (none while armed, connecting or
  ended); a retry republishes `connectingSince`. The conversing word pulses
  (`AmbientPhaseWordPulse`, ~3 s every ~10 s, `resetPhaseWordPulse`). A wake's
  publish carries an `AlertConfiguration` (`.default` sound; no silent case); a
  Talk tap does not alert. Reduce Motion makes symbol effects `.nonRepeating`
  and drops scale and the timing hint, keeping crossfade, progress bar and ring.

Tests pin invariants over `AmbientOrbPhase.allCases`, not colours: armed glows
dimmer than every conversing phase, every live phase glows, ended has no halo,
no two phases share a core gradient or blob seed, and none differs from
`armed` by scale alone.

#### Captions and the ended orb

Captions are finals only, each emitted once with `ContentState.captionRole`;
empty finals are skipped and conversation end republishes the recomposed
caption. Widget controls use `AuroraPalette.controlAccent`; conversation fill
is `ambientOrbAccent` (the widget process has no accent asset).

**An ended orb must not keep claiming to be heard**: the ended reason
(`endedReason` from `end(reason:)`, or `context.isStale` via
`staleDate: expiresAt`) replaces the status word, and timer, caption and
disarm control disappear. `AmbientEndedReason.capReached` and
`.orphanCollected` stay distinct; cap and stale render alike (`forWindow` /
`windowIsOver` take `isStale` as a `Bool`). An end with a reason lingers; the
user's own disarm ends immediately. Surviving activities are ended, never
adopted (`endOrphans()`; `start` sweeps first). A blank agent name degrades to
"Listening".
**Stop performs a real disarm** (`Button(intent: DisarmAmbientIntent())`,
`.tint(.red)`, VoiceOver "Stop listening"). **Open** uses `magican://ambient`,
which `App.handleURL` claims with no voice action (bare `magican://voice` would
start a Talk turn).

### Stop listening (internal disarm)

The button runs in the **widget** process: `magios/Shared/AmbientSignal.swift`
(Darwin name, `postDisarm()`, request/acknowledgement records) and
`magios/Shared/DisarmAmbientIntent.swift` (`openAppWhenRun = false`).
**The orb disappearing must never be optimistic.**

1. The intent publishes a request (`ambient.pendingDisarm`) with a fresh
   identity and posts Darwin `ai.magicbeans.magician.ambient.disarm`.
2. A running app disarms, ends the orb via `publishIfNeeded`, then
   acknowledges that identity: `mic.stop()` is synchronous, so the orb may
   outlive the microphone, never the reverse.
3. No acknowledgement within **1 s** (`awaitAcknowledgement` →
   `acknowledged` / `silent` / `cancelled`, polled every 20 ms): a
   still-outstanding request means nothing is listening, so the intent ends the
   orphan orb; a consumed one means a live app is disarming, so it does not
   race. On `cancelled` it does nothing.

The request record stays even when the intent ends the orb (resume-path
evidence); `consumeAcknowledgement(of:)` matches identity so a stale ack cannot
answer a fresh request. A request finding nothing armed still sweeps orphans
and acknowledges. `arm` registers the observer right after saving the arm
record; `disarm` and `unwindLiveWindow` unregister. Residual risk: an app whose
main actor is blocked past the budget can lose its orb while the mic runs; it
heals when the actor unblocks (Darwin is queued) and via the surviving request
on resume.

### Microphone tap

`magios/Magios/AmbientMicEngine.swift`, the concrete `AmbientMicSource`:
hardware input resampled to 16 kHz mono PCM16 for one callback.

- **Session activated only in the FOREGROUND, NEVER deactivated:** one
  executable `setActive(true)` and no executable `setActive(false)`; `stop()`
  leaves the session active (copying `ListenController.stopEngine()` makes the
  wake word work only on screen). A backgrounded `start` (`resumeSpotting`)
  sets the category and starts the engine without activating; the gate is
  **app state, not an "activated once" latch**, and foregrounding does not
  re-activate a session that lapsed while away. No `.mixWithOthers` (DTS is
  unsettled on whether a mixable session can activate from the background).
- **Configured once, for the conversation:** `.playAndRecord`, **`.voiceChat`**
  (hardware AEC), `[.defaultToSpeaker, .allowBluetoothHFP]` (follows the
  user's headset; `ListenController` omits HFP), re-applied on every `start`
  and asserted equal to `VoiceAudioEngine`'s triple
  (`AmbientMicEngineTests.testTheArmedWindowConfiguresTheSessionTheConversationNeeds`);
  `VoiceAudioEngine.start` configures nothing for a `.keepActive` caller (a
  background mode change returns a degenerate input node). No preferred rate:
  the tap uses `outputFormat(forBus:)`.
- **Byte contract:** `AmbientMicResampler` → `VoicePCM.floatToInt16LE`; the
  byte count is always even, never sliced or padded. Route changes go through
  `rebuildTap`. One resampler per tap, owned by the tap closure; never hoist it
  to a property.
- **No pump, no accumulator, no upload:** while armed, audio's one destination
  is `WakeSpotter.feed` via `AmbientController.ingest`.
- **`stop()`** is safe twice and guarantees no `onFrame`/`onFailure` afterwards:
  `onFrame` is delivered and nilled inside one `deliver` lock; a generation
  counter (bumped by `start` and `stop`) drops stale failure reports, except the
  foreground permission re-check. Reporting closes the tap, then calls the
  handler once.

#### The failure channel, and what each notification means

| Source | Outcome |
|---|---|
| `AVAudioSession.interruptionNotification`, `.began` | fail |
| `AVAudioSession.interruptionNotification`, `.ended` | **ignore**: the background cannot reactivate a session the system deactivated, so an interruption ends the window. |
| `AVAudioSession.routeChangeNotification` | fail **only** if no input route remains. |
| `AVAudioSession.mediaServicesWereResetNotification` | fail |
| `.AVAudioEngineConfigurationChange` (scoped to our engine) | rebuild the tap; fail only if the rebuild throws. |
| `UIApplication.didBecomeActiveNotification` | fail if the record permission is gone *and* a tap is running |

A degenerate input format (zero sample rate or channels) is refused before
`installTap`, which would raise an uncatchable `NSException`.
`AmbientMicEngineTests` covers the resampler, session triple and this mapping;
`start`, `stop` and the tap callback are untested (the simulator has no
capture input).

### Device verification

The simulator cannot exercise `AVAudioEngine` capture or out-of-process Live
Activity rendering; hardware checks live with the archived plans
(Task 13,
design §21, aurora plan);
call-path items are in [ambient-call-sink.md](./ambient-call-sink.md).

8. **Wake accuracy under `.voiceChat` is unmeasured.** The spotting tap runs
   with system AEC/AGC; phrase ordering needs re-measuring at conversational
   volume, across a room, face-down, with music, and with the assistant
   audible.

Never captured as numbers: handoff latency percentiles, battery and thermals
over an armed hour, Live Activity update budget over a long window, and Vosk
resident memory against its open leak.

### What the motion pass owes on device

Out-of-process motion (symbol-effect cycling on `heard`, identity-swap fades,
compact ring and glyph fit, phase-word pulse, connect gauge and retry restart,
blob silhouettes, wake alert sound) can only be checked on hardware; the list
is in the archived aurora plan.
