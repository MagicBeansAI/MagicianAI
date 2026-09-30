import os
import XCTest
@testable import Magician

/// Microphone behavior is independent of the selected voice provider family.
///
/// `@MainActor` because `VoiceCallViewModel` and `VoiceAudioEngine` are: the
/// audio-session tests below construct the real types (they hold no socket until
/// a call starts) rather than a double, since the property under test is what the
/// real teardown does to the shared session.
@MainActor
final class VoiceCallViewModelTests: XCTestCase {

    func testSurfaceEngineOverrideWinsWithoutChangingTheChatDefault() {
        XCTAssertEqual(
            VoiceCallViewModel.resolvedVoiceEngine(
                defaultEngine: .realtime,
                surfaceOverride: .handsFree
            ),
            .handsFree
        )
        XCTAssertEqual(
            VoiceCallViewModel.resolvedVoiceEngine(
                defaultEngine: .handsFree,
                surfaceOverride: nil
            ),
            .handsFree
        )
    }

    func testTutorBlackboardHandoffEndsCallAndPresentsCanonicalQuestion() {
        let call = VoiceCallViewModel(mode: .inApp)
        call.guidedFlowScreenIsLocked = { false }
        var presented: String?
        call.tutorBlackboardPresenter = { presented = $0 }

        call.handleTutorBlackboardHandoff(
            text: "@tutor #quick blackboard explain recursion",
            quick: true
        )

        XCTAssertEqual(presented, "#quick blackboard explain recursion")
        XCTAssertEqual(call.client.phase, .ended)
    }

    func testTutorHandoffLockRaceSpeaksAndNeverPresents() {
        let call = VoiceCallViewModel(mode: .inApp)
        call.guidedFlowScreenIsLocked = { true }
        var spoken: String?
        var presentationCount = 0
        call.guidedFlowSpeaker = { spoken = $0 }
        call.tutorBlackboardPresenter = { _ in presentationCount += 1 }

        call.handleTutorBlackboardHandoff(
            text: "@tutor blackboard explain recursion",
            quick: false
        )

        XCTAssertEqual(spoken, "Please unlock your screen to use Tutor.")
        XCTAssertEqual(presentationCount, 0)
        XCTAssertEqual(call.client.phase, .ended)
    }

    func testBackendAnnouncedGuidedRejectionDoesNotDoubleSpeakLocally() {
        let call = VoiceCallViewModel(mode: .inApp)
        var spoken: [String] = []
        call.guidedFlowSpeaker = { spoken.append($0) }

        call.handleGuidedFlowRejection(
            message: "Please unlock your screen to use Tutor.",
            backendAnnounced: true
        )

        XCTAssertTrue(spoken.isEmpty)
        XCTAssertEqual(call.client.phase, .idle)
    }

    func testLocallyOwnedGuidedRejectionStopsCallBeforeSpeaking() {
        let call = VoiceCallViewModel(mode: .inApp)
        var phaseWhenSpoken: RealtimeVoiceClient.Phase?
        call.guidedFlowSpeaker = { _ in phaseWhenSpoken = call.client.phase }

        call.handleGuidedFlowRejection(
            message: "Screen tutoring isn't available on this device yet.",
            backendAnnounced: false
        )

        XCTAssertEqual(phaseWhenSpoken, .ended)
        XCTAssertEqual(call.client.phase, .ended)
    }

    func testBackendGuidedRejectionIsTransientAndKeepsLiveCallReady() {
        let call = VoiceCallViewModel(mode: .inApp)
        call.client.handleControl(text: RealtimeVoiceProtocol.envelopeText(
            kind: "session.ready",
            payload: [:]
        ))
        XCTAssertEqual(call.client.phase, .ready)

        call.client.handleControl(text: RealtimeVoiceProtocol.envelopeText(
            kind: "tutor.takeover.failed",
            payload: [
                "message": "Please unlock your screen to use Tutor.",
                "backend_announced": true
            ]
        ))

        XCTAssertEqual(call.client.phase, .ready)
        XCTAssertNil(call.errorMessage)
        XCTAssertEqual(
            call.guidedFlowNoticeMessage,
            "Please unlock your screen to use Tutor."
        )

        call.client.handleControl(text: RealtimeVoiceProtocol.envelopeText(
            kind: "transcript.user",
            payload: ["text": "Tutor blackboard explain recursion"]
        ))

        XCTAssertEqual(call.client.phase, .ready)
        XCTAssertNil(call.guidedFlowNoticeMessage)
    }

    /// Gemini 3.8 Live Extended Thinking says "let me check…", runs a tool
    /// without blocking, then answers; the gap is silent. The status line must
    /// say "Working…" through it, drop back once the interaction is idle, and
    /// never survive an interrupt or a fresh session.
    func testInteractionStatusShowsWorkingUntilIdleInterruptOrNewSession() {
        let call = VoiceCallViewModel(mode: .inApp)
        call.client.handleControl(text: RealtimeVoiceProtocol.envelopeText(
            kind: "session.ready",
            payload: [:]
        ))
        XCTAssertEqual(call.client.phase, .ready)
        XCTAssertFalse(call.client.assistantWorking)
        let idleStatus = call.statusText

        call.client.handleControl(text: RealtimeVoiceProtocol.envelopeText(
            kind: "interaction.status",
            payload: ["status": "in_progress"]
        ))
        XCTAssertTrue(call.client.assistantWorking)
        XCTAssertEqual(call.statusText, "Working…")

        call.client.handleControl(text: RealtimeVoiceProtocol.envelopeText(
            kind: "interaction.status",
            payload: ["status": "idle"]
        ))
        XCTAssertFalse(call.client.assistantWorking)
        XCTAssertEqual(call.statusText, idleStatus)

        // An interrupt abandons the work the model was doing.
        call.client.handleControl(text: RealtimeVoiceProtocol.envelopeText(
            kind: "interaction.status",
            payload: ["status": "in_progress"]
        ))
        call.client.handleControl(text: RealtimeVoiceProtocol.envelopeText(
            kind: "response.interrupted",
            payload: ["response_id": "r1"]
        ))
        XCTAssertFalse(call.client.assistantWorking)

        // A rotated upstream starts idle; the old flag must not outlive it.
        call.client.handleControl(text: RealtimeVoiceProtocol.envelopeText(
            kind: "interaction.status",
            payload: ["status": "in_progress"]
        ))
        call.client.handleControl(text: RealtimeVoiceProtocol.envelopeText(
            kind: "session.ready",
            payload: [:]
        ))
        XCTAssertFalse(call.client.assistantWorking)
        XCTAssertEqual(call.statusText, idleStatus)
    }

    func testNativeStartupDeadlineCoversColdResumeContextPreparation() {
        XCTAssertGreaterThanOrEqual(RealtimeVoiceClient.startupTimeoutSeconds, 45)
        XCTAssertGreaterThan(
            RealtimeVoiceClient.readyWaitTimeoutSeconds,
            RealtimeVoiceClient.startupTimeoutSeconds
        )
    }

    func testMicIsOpenWheneverPttIsOffRegardlessOfEngine() {
        for engine in VoiceEngine.allCases {
            XCTAssertFalse(VoiceCallViewModel.micMuted(pttOn: false, engine: engine),
                           "PTT Off must leave the mic open on \(engine)")
        }
    }

    func testHoldToTalkGatesMicRegardlessOfEngine() {
        for engine in VoiceEngine.allCases {
            XCTAssertTrue(VoiceCallViewModel.micMuted(pttOn: true, engine: engine))
            XCTAssertTrue(VoiceCallViewModel.effectivePttOn(
                requested: true,
                engine: engine,
                profile: nil,
                mode: .inApp
            ))
        }
    }

    /// An ambient conversation has no button to hold, so a stored hold-to-talk
    /// preference must not reach it: honouring it would mute local capture AND ask
    /// the provider for a `push_to_talk` boundary that nothing ever commits, while
    /// the orb went on reporting a live conversation.
    func testAmbientCallsIgnoreTheStoredHoldToTalkPreference() {
        for engine in VoiceEngine.allCases {
            XCTAssertFalse(VoiceCallViewModel.effectivePttOn(
                requested: true,
                engine: engine,
                profile: nil,
                mode: .ambient
            ), "An ambient call must keep the mic open on \(engine)")
        }
        XCTAssertFalse(VoiceCallMode.ambient.appliesStoredHoldToTalk)
        XCTAssertTrue(VoiceCallMode.inApp.appliesStoredHoldToTalk)
    }

    // MARK: - Audio-session disposition
    //
    // The tests below are the regression guard on the flag that decides
    // whether a microphone can ever be reopened. Without them the flag is a
    // footgun: `VoiceAudioEngine.stop(session:)`'s deactivation is unreachable in
    // the simulator (the engine cannot start without a microphone), so a wrong
    // disposition compiles, passes, and only shows up on a device as an armed
    // ambient window that dies at the first wake word.

    /// Nothing armed. **This is the acceptance criterion for every change to this
    /// file and it still is:** with no ambient window, the in-app call behaves
    /// exactly as it did before any of these flags existed — it borrowed the shared
    /// session and gives it back, so the user's music resumes.
    func testInAppTeardownReleasesTheSharedSessionWhenNothingIsArmed() {
        let call = VoiceCallViewModel(mode: .inApp)
        call.ambientRail = Self.rail(windowIsLive: false)
        XCTAssertEqual(call.sessionDisposition, .release)
        call.hangUp()
        XCTAssertEqual(call.engine.lastSessionDisposition, .release)
    }

    /// The ambient path must not deactivate the shared session by ANY route.
    /// `AmbientMicEngine` activates it in the foreground and never undoes it,
    /// and `AmbientController.resumeSpotting` reopens the spotting tap from the
    /// background, where a reactivation is refused.
    func testAmbientTeardownNeverDeactivatesTheSharedSession() {
        let call = VoiceCallViewModel(mode: .ambient)
        call.ambientRail = Self.rail(windowIsLive: true)
        XCTAssertEqual(call.sessionDisposition, .keepActive)
        call.hangUp()
        XCTAssertEqual(call.engine.lastSessionDisposition, .keepActive)
        XCTAssertEqual(call.engine.sessionReleaseCount, 0)
        // Twice, because `AmbientController` issues `endCall` twice whenever a
        // disarm lands inside a connect.
        call.hangUp()
        XCTAssertEqual(call.engine.sessionReleaseCount, 0)
    }

    /// The mapping itself, driven directly — the only way to observe that
    /// `.release` really does deactivate, since `stop()`'s `isRunning` guard is
    /// unreachable here.
    func testOnlyReleaseDeactivatesTheSharedSession() {
        let engine = VoiceAudioEngine()
        engine.releaseSessionIfRequested(.keepActive)
        XCTAssertEqual(engine.sessionReleaseCount, 0, "keepActive must never deactivate")
        engine.releaseSessionIfRequested(.release)
        XCTAssertEqual(engine.sessionReleaseCount, 1, "release must deactivate")
    }

    // MARK: - The outbound leg: a `.keepActive` start configures nothing
    //
    // The three tests below are the regression guard on this feature's headline
    // device failure. It was reproduced on hardware: arming worked, the wake phrase
    // fired with the app backgrounded / killed / the screen locked — which proves
    // the session was active and the spotting tap ran correctly off screen — and
    // then the microphone was lost. The handoff was the cause. `start` changed the
    // session's MODE, re-stated a preferred sample rate and called `setActive(true)`
    // from the background, then rebuilt the audio unit with voice processing on top.
    // Apple DTS 826462: have the `audio` background mode and only activate in the
    // foreground; that sequence is not "leaving an active session alone".
    //
    // Neither the failure nor the fix is observable in the simulator, which is why
    // these are driven through `configureSharedSessionIfOwned` and a counter rather
    // than through `start` — the same reason, and the same shape, as the disposition
    // tests above.

    /// The ambient path must not configure or activate the shared session by ANY
    /// route: the armed window did that once, in the foreground, and owns it.
    func testAnAmbientStartDoesNotConfigureTheSharedSession() {
        XCTAssertFalse(
            VoiceCallMode.ambient
                .sessionDisposition(ambientWindowIsLive: { true })
                .configuresSharedSession,
            "the mode is the seam the call site reads; a wrong answer here reaches every start"
        )
        let engine = VoiceAudioEngine()
        XCTAssertNoThrow(try engine.configureSharedSessionIfOwned(.keepActive))
        XCTAssertEqual(engine.sessionConfigureCount, 0)
        // Twice, because a failed connect is retried within one armed window.
        XCTAssertNoThrow(try engine.configureSharedSessionIfOwned(.keepActive))
        XCTAssertEqual(engine.sessionConfigureCount, 0)
    }

    /// The positive control, without which the test above passes over a method that
    /// configures nothing for anybody. The in-app call must behave exactly as it did
    /// before the skip existed — that equivalence is the acceptance criterion, the
    /// same as when the disposition flag was introduced.
    ///
    /// `try?`, not `XCTAssertNoThrow`: activating a `.playAndRecord` session inside a
    /// test host may legitimately fail, and the assertion is about whether the
    /// configuration was *attempted*, which the counter records before the call.
    func testTheInAppStartStillConfiguresTheSharedSession() {
        XCTAssertTrue(
            VoiceCallMode.inApp
                .sessionDisposition(ambientWindowIsLive: { false })
                .configuresSharedSession
        )
        let engine = VoiceAudioEngine()
        _ = try? engine.configureSharedSessionIfOwned(.release)
        XCTAssertEqual(engine.sessionConfigureCount, 1, "release owns the session and must configure it")
    }

    /// The mapping itself, in both directions, driven directly — the enum is now read
    /// on the way in as well as on the way out, so a future case added to it has two
    /// answers to give rather than one.
    func testOnlyAReleaseCallerOwnsTheSharedSession() {
        XCTAssertTrue(VoiceSessionDisposition.release.configuresSharedSession)
        XCTAssertFalse(VoiceSessionDisposition.keepActive.configuresSharedSession)
    }

    // MARK: - The fourteenth session site: the IN-APP call under an armed window
    //
    // The disposition used to be read from `mode` alone, and `.inApp` meant
    // `.release` unconditionally. So the front door was open: arm a two-hour window,
    // tap Live, hang up — and the hangup deactivated the window's session from a
    // place nothing could reactivate it, killing the window at the next wake word
    // with no cause the user could connect it to. Design §15's fourth correction
    // exactly: the `VoiceAudioEngine.stop()` row was marked RESOLVED, and it was
    // resolved for the caller that found it.
    //
    // The disposition is now derived from the SAME rail the other four session sites
    // consult, so the question is "is a window live?" rather than "which caller am
    // I?". All four combinations are pinned below, because three of them passing is
    // how the one that matters gets broken.

    private static func rail(
        windowIsLive: Bool,
        onConsult: @escaping () -> Void = {}
    ) -> AmbientRail {
        AmbientRail(
            windowIsLive: {
                onConsult()
                return windowIsLive
            },
            yield: { _ in
                XCTFail("an in-app call must not yield the window — it is the same purpose")
            }
        )
    }

    /// **The fix.** A window is live, so the in-app call is a guest: it does not hand
    /// the session back, because the window needs it to stay active and nothing else
    /// does.
    func testAnInAppCallLeavesAnArmedWindowsSessionAlone() {
        let call = VoiceCallViewModel(mode: .inApp)
        call.ambientRail = Self.rail(windowIsLive: true)
        XCTAssertEqual(call.sessionDisposition, .keepActive)
        call.hangUp()
        XCTAssertEqual(call.engine.lastSessionDisposition, .keepActive)
        XCTAssertEqual(call.engine.sessionReleaseCount, 0)
        // Twice, because the panel's hangup and a terminal server phase both reach
        // `teardownLocal`.
        call.hangUp()
        XCTAssertEqual(call.engine.sessionReleaseCount, 0)
    }

    /// All four combinations of armed × mode, in one place, because the interesting
    /// property is the shape of the table rather than any single cell.
    ///
    /// Note what the `.ambient` row is *not*: it is not "`.ambient` implies a live
    /// window". `AmbientController.disarm` clears `armedAt` before it ends the call,
    /// so an ambient teardown routinely runs with the rail already reading false —
    /// and it must still keep the session, because `AmbientMicEngine` never
    /// deactivates and a later `resumeSpotting` may still be in flight. Deriving the
    /// ambient answer from the rail would have reintroduced the original bug through
    /// a new door.
    func testTheDispositionTableOverArmedAndMode() {
        let cases: [(VoiceCallMode, Bool, VoiceSessionDisposition)] = [
            (.inApp, false, .release),
            (.inApp, true, .keepActive),
            (.ambient, false, .keepActive),
            (.ambient, true, .keepActive),
        ]
        for (mode, armed, expected) in cases {
            XCTAssertEqual(
                mode.sessionDisposition(ambientWindowIsLive: { armed }),
                expected,
                "\(mode) with windowIsLive=\(armed)"
            )
        }
    }

    /// **The rail can only ever make a call KEEP the session, never release one it
    /// would have kept.** That monotonicity is the safety property of adding a rail
    /// here at all: whatever the rail answers, and however wrong it is, no call is
    /// made *more* likely to deactivate a session than it was before.
    func testTheRailCanOnlyEverAddAKeepNeverARelease() {
        for mode in [VoiceCallMode.inApp, .ambient] {
            XCTAssertEqual(
                mode.sessionDisposition(ambientWindowIsLive: { true }),
                .keepActive,
                "a live window must keep the session for \(mode)"
            )
        }
    }

    /// The ambient answer does not depend on the rail, so it must not consult it.
    ///
    /// Not a micro-optimisation: the production `windowIsLive` reads
    /// `AmbientController.shared`, which constructs a Vosk spotter, a microphone
    /// engine and a call stack. An ambient call already has a controller, but the
    /// property worth pinning is the independence — the ambient disposition is
    /// unconditional, and a future edit that makes it ask the rail would be reading a
    /// value it must not act on.
    func testAnAmbientCallDoesNotEvenConsultTheRail() {
        var consulted = 0
        let disposition = VoiceCallMode.ambient.sessionDisposition(
            ambientWindowIsLive: {
                consulted += 1
                return false
            }
        )
        XCTAssertEqual(disposition, .keepActive)
        XCTAssertEqual(consulted, 0, "the ambient answer is unconditional")

        // The positive control: the in-app path DOES consult it, so the zero above
        // is a property of `.ambient` rather than of a closure nobody calls.
        var inAppConsulted = 0
        _ = VoiceCallMode.inApp.sessionDisposition(
            ambientWindowIsLive: {
                inAppConsulted += 1
                return false
            }
        )
        XCTAssertEqual(inAppConsulted, 1)
    }

    /// **Read at the moment it matters, not latched at `startCall`.** A window can
    /// open *during* an in-app call — Settings has an arm control and the call panel
    /// does not block it — and design §19's second lesson is that a snapshot latch is
    /// not a fix if the deferred path can change the answer under it. Both directions
    /// are asserted on one call object, which is what proves it is re-read.
    func testTheDispositionIsReReadRatherThanSnapshot() {
        let call = VoiceCallViewModel(mode: .inApp)
        call.ambientRail = Self.rail(windowIsLive: false)
        XCTAssertEqual(call.sessionDisposition, .release)

        // The user arms a window while the call is up.
        call.ambientRail = Self.rail(windowIsLive: true)
        XCTAssertEqual(call.sessionDisposition, .keepActive, "a window opened mid-call must be protected")

        // And the reverse: the leash expires mid-call, so nothing needs the session
        // and the hangup is polite again.
        call.ambientRail = Self.rail(windowIsLive: false)
        XCTAssertEqual(call.sessionDisposition, .release)
    }

    /// An ambient call must not hush chat narration, because
    /// `SpeechSynthesizer.stop()` deactivates the shared session unconditionally —
    /// the same fatal edit as a `.release` teardown, reached through a line that
    /// looks like housekeeping.
    func testAmbientCallsDoNotHushChatNarration() {
        XCTAssertFalse(VoiceCallMode.ambient.hushesChatNarration)
        XCTAssertTrue(VoiceCallMode.inApp.hushesChatNarration)
    }

    /// **And gating the ambient call's hush was not enough**, which is the other
    /// half of the same bug and was missed by the fix above. Chat calls
    /// `SpeechSynthesizer.stop()` routinely as a hush — on send, on leaving a
    /// thread, on barge-in — and `stop()` reached the deactivation *with nothing
    /// speaking*, against a session it had never taken. So a user who armed an
    /// ambient window and then sent one chat message lost the window, invisibly,
    /// at their next wake word: an armed session cannot be reactivated from the
    /// background (Apple DTS 826462).
    ///
    /// `sessionReleaseCount` exists because "it did not deactivate" is not
    /// observable through any `AVAudioSession` API.
    func testAChatHushDoesNotHandBackASessionItNeverTook() {
        let synthesizer = SpeechSynthesizer.shared
        let railBefore = synthesizer.ambientRail
        defer { synthesizer.ambientRail = railBefore }
        synthesizer.ambientRail = AmbientRail(windowIsLive: { false }, yield: { _ in })
        let releasesBefore = synthesizer.sessionReleaseCount

        synthesizer.stop()
        synthesizer.stop()

        XCTAssertEqual(
            synthesizer.sessionReleaseCount,
            releasesBefore,
            "a hush with nothing speaking must not deactivate the shared session"
        )
    }

    /// **The thirteenth session site, and the one that failed the standard §15
    /// states.** `configureSession()` swaps the shared session to `.playback`, which
    /// has no *input* — so it takes the microphone out from under a live ambient tap,
    /// exactly as `BackgroundEngine.playSilence` did.
    ///
    /// `speak()`'s audio-focus guard does not cover it, because the Magician TTS path
    /// defers: `speak` → `speakViaMagician` → a URLSession round trip →
    /// `DispatchQueue.main.async` → `playAudio` → `configureSession`. `arm()` can run
    /// start to finish inside that round trip — trivially now that Settings has an
    /// in-app arm button — so the focus check passed against a world that no longer
    /// exists.
    ///
    /// Refusing is what keeps `holdsSession` false, and therefore what keeps
    /// `finish()` from deactivating a session this object never took.
    /// `abandonSessionClaim()` alone could not: it does not bump `playbackGeneration`
    /// or stop the synthesizer, so the callback re-armed the latch one line later.
    func testTtsDoesNotRecategoriseTheSessionUnderAnArmedWindow() {
        let synthesizer = SpeechSynthesizer.shared
        let railBefore = synthesizer.ambientRail
        defer { synthesizer.ambientRail = railBefore }
        synthesizer.ambientRail = AmbientRail(windowIsLive: { true }, yield: { _ in })
        let configuresBefore = synthesizer.sessionConfigureCount
        let releasesBefore = synthesizer.sessionReleaseCount

        // The deferred callback, arriving after a window armed.
        synthesizer.configureSession()

        XCTAssertEqual(
            synthesizer.sessionConfigureCount,
            configuresBefore,
            "`.playback` has no input — swapping to it takes the microphone out from under the armed tap"
        )

        // And the latch it must not have re-armed: a hush now has nothing to hand
        // back, which is the half `abandonSessionClaim()` could not protect.
        synthesizer.stop()
        XCTAssertEqual(
            synthesizer.sessionReleaseCount,
            releasesBefore,
            "holdsSession must still be false, or finish() deactivates a session it never took"
        )
    }

    /// The positive control, without which both assertions above pass vacuously
    /// against counters that never increment. It restores the session on the way out
    /// through `stop()`'s own deactivation.
    func testTtsTakesAndReturnsTheSessionWithNoWindowArmed() {
        let synthesizer = SpeechSynthesizer.shared
        let railBefore = synthesizer.ambientRail
        defer { synthesizer.ambientRail = railBefore }
        synthesizer.ambientRail = AmbientRail(windowIsLive: { false }, yield: { _ in })
        let configuresBefore = synthesizer.sessionConfigureCount
        let releasesBefore = synthesizer.sessionReleaseCount

        synthesizer.configureSession()
        XCTAssertEqual(synthesizer.sessionConfigureCount, configuresBefore + 1)

        synthesizer.stop()
        XCTAssertEqual(
            synthesizer.sessionReleaseCount,
            releasesBefore + 1,
            "having taken the session, it hands it back"
        )
    }

    /// `abandonSessionClaim()` is a backstop rather than the whole fix, and this is
    /// the half it does cover: the window between `arm()` and the next deferred
    /// callback, where an utterance already in the air would otherwise hand back a
    /// session ambient had just taken.
    func testAbandoningTheClaimStopsAnInFlightReplyFromHandingTheSessionBack() {
        let synthesizer = SpeechSynthesizer.shared
        let railBefore = synthesizer.ambientRail
        defer { synthesizer.ambientRail = railBefore }
        synthesizer.ambientRail = AmbientRail(windowIsLive: { false }, yield: { _ in })
        synthesizer.configureSession()
        let releasesBefore = synthesizer.sessionReleaseCount

        // `AmbientController.arm` calls this the instant the ambient tap is live.
        synthesizer.abandonSessionClaim()
        synthesizer.stop()

        XCTAssertEqual(
            synthesizer.sessionReleaseCount,
            releasesBefore,
            "the claim was void the moment ambient reconfigured the session"
        )
    }

    /// An unreadable/absent stored value must fall back to the quality path
    /// rather than silently opening calls on the local engine.
    func testPersistedEngineFallsBackToRealtime() {
        XCTAssertEqual(VoiceEngine(rawValue: "nonsense") ?? .realtime, .realtime)
        XCTAssertEqual(VoiceEngine(rawValue: "") ?? .realtime, .realtime)
    }

    /// Both engines must round-trip through UserDefaults storage.
    func testEngineRawValuesRoundTrip() {
        for engine in VoiceEngine.allCases {
            XCTAssertEqual(VoiceEngine(rawValue: engine.rawValue), engine)
        }
    }

    /// The wire value is what the backend switches on — pin both spellings.
    func testVoiceModeWireValues() {
        XCTAssertEqual(VoiceEngine.realtime.voiceMode, "realtime")
        XCTAssertEqual(VoiceEngine.handsFree.voiceMode, "hands_free")
    }

    func testFirstAssistantPlaybackIsNotForwardedAsNextUserTurn() {
        var gate = VoiceCaptureGateState()

        XCTAssertFalse(gate.shouldSuppressCapture(
            playbackStarted: false,
            initialPlaybackActive: false,
            assistantSpeaking: false,
            echoCancellationActive: true
        ))
        XCTAssertTrue(gate.shouldSuppressCapture(
            playbackStarted: true,
            initialPlaybackActive: true,
            assistantSpeaking: true,
            echoCancellationActive: true
        ))
        XCTAssertFalse(gate.shouldSuppressCapture(
            playbackStarted: true,
            initialPlaybackActive: false,
            assistantSpeaking: false,
            echoCancellationActive: true
        ))
        XCTAssertFalse(gate.initialAssistantPlaybackPending)
        XCTAssertFalse(gate.shouldSuppressCapture(
            playbackStarted: true,
            initialPlaybackActive: false,
            assistantSpeaking: true,
            echoCancellationActive: true
        ), "AEC-backed barge-in must return after the first reply")
    }

    func testMissingAecKeepsSuppressingAssistantPlaybackAfterWarmup() {
        var gate = VoiceCaptureGateState()
        _ = gate.shouldSuppressCapture(
            playbackStarted: true,
            initialPlaybackActive: false,
            assistantSpeaking: false,
            echoCancellationActive: false
        )

        XCTAssertTrue(gate.shouldSuppressCapture(
            playbackStarted: true,
            initialPlaybackActive: false,
            assistantSpeaking: true,
            echoCancellationActive: false
        ))
    }

    // MARK: - Half-duplex playback-clock gate (pure)
    //
    // The gate's basis is the drain deadline `AssistantPlaybackClock` accumulates
    // from queued frame DURATIONS, never from frame arrivals: a realtime provider
    // streams a reply 5–10× faster than it plays, so an arrival debounce reopens
    // the gate while seconds of the reply are still coming out of the speaker —
    // which, with AEC unarmed, is the assistant hearing itself. All pure math,
    // assertable without AVFoundation or sleeping through a reply.

    func testQueuedFrameAdvancesTheDrainDeadlineByItsPCMDuration() {
        var clock = AssistantPlaybackClock()
        // 960 bytes of PCM16 at 24 kHz = 480 samples = 20 ms.
        XCTAssertEqual(
            clock.queue(frameBytes: 960, sampleRate: 24_000, now: 100),
            100.02,
            accuracy: 1e-6
        )
        // A frame queued while the first still plays stacks behind it rather
        // than restarting the deadline from its own arrival instant.
        XCTAssertEqual(
            clock.queue(frameBytes: 960, sampleRate: 24_000, now: 100.005),
            100.04,
            accuracy: 1e-6
        )
    }

    func testGateStaysClosedWhileQueuedAudioRemainsAfterArrivalsStop() {
        var clock = AssistantPlaybackClock()
        // Two seconds of reply delivered in one burst at t=10 — the exact shape
        // that defeated the old arrival debounce.
        let drainsAt = clock.queue(frameBytes: 2 * 24_000 * 2, sampleRate: 24_000, now: 10)
        let tail = 0.35

        // 0.4 s after the last ARRIVAL the old gate had already reopened; 1.6 s
        // of speech is still audible here.
        XCTAssertTrue(
            VoiceCaptureGateState.playbackAudible(drainsAt: drainsAt, tail: tail, now: 10.4),
            "queued duration outlives arrivals — the gate must too"
        )
        // Closed right up to the drain and through the acoustic tail...
        XCTAssertTrue(
            VoiceCaptureGateState.playbackAudible(drainsAt: drainsAt, tail: tail, now: 12.3)
        )
        // ...open only once the speaker has actually run dry plus the tail.
        XCTAssertFalse(
            VoiceCaptureGateState.playbackAudible(drainsAt: drainsAt, tail: tail, now: 12.36)
        )
    }

    func testFlushResetsTheDrainDeadline() {
        var clock = AssistantPlaybackClock()
        _ = clock.queue(frameBytes: 24_000 * 2, sampleRate: 24_000, now: 50)   // 1 s queued

        clock.reset()

        // The inverse bug: a deadline surviving `player.stop()`'s flush would
        // gate a live microphone against audio that will never play.
        XCTAssertFalse(
            VoiceCaptureGateState.playbackAudible(
                drainsAt: clock.idleAtUptime, tail: 0.35, now: 50.1
            )
        )
        XCTAssertEqual(clock.idleAtUptime, 0, "zero doubles as 'playback never started'")
    }

    /// The composed fallback, past warmup: AEC unarmed + queued audio still
    /// audible must suppress, and the same gate with the queue drained must not.
    func testMissingAecSuppressesExactlyWhileTheQueueIsAudible() {
        var clock = AssistantPlaybackClock()
        var gate = VoiceCaptureGateState()
        // Clear the first-playback warmup so what's under test is the ongoing
        // fallback branch, not the initial gate.
        _ = gate.shouldSuppressCapture(
            playbackStarted: true,
            initialPlaybackActive: false,
            assistantSpeaking: false,
            echoCancellationActive: false
        )
        let drainsAt = clock.queue(frameBytes: 24_000 * 2, sampleRate: 24_000, now: 0)   // 1 s

        XCTAssertTrue(gate.shouldSuppressCapture(
            playbackStarted: drainsAt > 0,
            initialPlaybackActive: false,
            assistantSpeaking: VoiceCaptureGateState.playbackAudible(
                drainsAt: drainsAt, tail: 0.35, now: 0.5
            ),
            echoCancellationActive: false
        ))
        XCTAssertFalse(gate.shouldSuppressCapture(
            playbackStarted: drainsAt > 0,
            initialPlaybackActive: false,
            assistantSpeaking: VoiceCaptureGateState.playbackAudible(
                drainsAt: drainsAt, tail: 0.35, now: 2
            ),
            echoCancellationActive: false
        ), "drained + tail elapsed: the user's next words must go upstream")
    }

    func testCaptionViewportStartsAtTopUntilContentOverflows() {
        XCTAssertFalse(VoiceCaptionViewportPolicy.shouldFollowLatest(contentHeight: 40))
        XCTAssertFalse(VoiceCaptionViewportPolicy.shouldFollowLatest(
            contentHeight: VoiceCaptionViewportPolicy.height
        ))
        XCTAssertTrue(VoiceCaptionViewportPolicy.shouldFollowLatest(contentHeight: 100))
    }

    // MARK: - Audio wiring
    //
    // Both hooks below are one line each in `wireAudioPaths()`, and deleting either
    // leaves every other test in the suite green: the orb would simply never report
    // speaking, and the pre-ready gate would never open — a call that connects and
    // stays deaf. So they are pinned where they are attached, not only where they
    // are consumed.

    /// An EMPTY frame, deliberately: `engine.play` returns before it touches the
    /// audio graph for one, and a frame with samples in it would be scheduled on a
    /// player node that no `start()` ever attached — an uncatchable AVFAudio
    /// exception in the simulator, which would make this a test of the audio graph
    /// rather than of the wiring. What is under test is that the hook is attached
    /// and receives the frame.
    func testWireAudioPathsAttachesTheAssistantAudioHook() {
        let call = VoiceCallViewModel(mode: .ambient)
        var frames: [Int] = []
        call.onAssistantAudio = { frames.append($0.count) }
        call.wireAudioPaths()
        call.client.onIncomingAudio?(Data())
        XCTAssertEqual(frames, [0], "the downstream hook must reach onAssistantAudio")
    }

    func testWireAudioPathsAttachesTheReadyHookThatOpensTheGate() {
        let call = VoiceCallViewModel(mode: .ambient)
        call.wireAudioPaths()
        XCTAssertNotNil(
            call.client.onReady,
            "without this the pre-ready gate never opens and the call stays deaf"
        )
    }

    /// The provider swap is the one path that re-handshakes without re-entering
    /// `startCall`, and the one that shipped with the pre-ready gate left OPEN
    /// — frames captured while the new provider session's socket came up went
    /// straight upstream. Its guards are gated on the real client's phase,
    /// which no unit test can drive to `.ready` without a socket, so the
    /// extracted body is what carries the contract and what this pins: the
    /// gate is ARMED, and armed BEFORE the `startAudioCapture()` guard. The
    /// injected capture probe observes that exact boundary without initializing
    /// RemoteIO in the simulator (which can abort the entire XCTest process on
    /// an audio-service timeout). Asserting the boundary directly means moving
    /// or deleting the arm line fails rather than merely not crashing.
    func testTheLiveProviderSwapArmsThePreReadyGateBeforeCapture() {
        let call = VoiceCallViewModel(mode: .inApp)
        XCTAssertFalse(call.preReadyGate.withLock { $0.isGating },
                       "precondition: a fresh view model is not gating, so the arm below is the swap's doing")

        var gateWasArmedWhenCaptureStarted = false
        call.performLiveProviderSwap(uiThreadId: "thread") {
            gateWasArmedWhenCaptureStarted = call.preReadyGate.withLock { $0.isGating }
            return false
        }

        XCTAssertTrue(gateWasArmedWhenCaptureStarted,
                      "the gate must be armed before the capture starter is invoked")
        // `hangUp` is documented safe with nothing to end and clears the gate.
        call.hangUp()
    }

    /// The interrupt hook is one line in `wireAudioPaths()` too, and deleting
    /// it leaves every other test green while an interrupted reply keeps
    /// sounding for seconds — exactly the orphaned tail whose echo finalizes
    /// after the server's collapsed echo window and re-seeds the self-hearing
    /// loop. Driven through the client's hook (not `engine.flushPlayback()`
    /// directly), so what is pinned is the attachment.
    func testWireAudioPathsFlushesPlaybackOnServerInterruptOnly() {
        let call = VoiceCallViewModel(mode: .ambient)
        call.wireAudioPaths()
        let flushesBefore = call.engine.playbackFlushCount

        call.client.onAssistantAudioEnded?(false)
        XCTAssertEqual(call.engine.playbackFlushCount, flushesBefore,
                       "a natural end drains truthfully — no flush")

        call.client.onAssistantAudioEnded?(true)
        XCTAssertEqual(call.engine.playbackFlushCount, flushesBefore + 1,
                       "an interrupted reply must stop sounding, not drain")
    }

    /// Flushing with nothing playing is an ordinary event, not an edge case —
    /// an interrupt routinely lands after the queue has already drained, and
    /// in the simulator no `start` ever attaches the player node. Both must be
    /// safe no-ops, twice (a barge-in and its `audio.output.ended` can each
    /// arrive for the same reply).
    func testFlushPlaybackIsIdempotentWithNothingPlaying() {
        let engine = VoiceAudioEngine()
        engine.flushPlayback()
        engine.flushPlayback()
        XCTAssertEqual(engine.playbackFlushCount, 2)
    }

    // MARK: - PreReadyGate
    //
    // Pure state, so all of it is reachable without a socket, an engine or
    // hardware. What replaced the upstream hold's ordering tests is deliberately
    // smaller: the gate has no buffer to order, and that ABSENCE is the contract
    // (owner decision, 2026-07-30) — the type offers no way to get a pre-ready
    // byte back out, so "nothing stale goes upstream at ready" holds by
    // construction and what is left to pin is the count, the once-only release,
    // and the gating itself.

    private typealias Gate = VoiceCallViewModel.PreReadyGate

    /// The contract's first half: before ready, every frame is discarded — the
    /// caller must not send it — and the discard is counted, because the count
    /// is the tripwire for ever revisiting this decision.
    func testTheGateDropsAndCountsEverythingBeforeReady() {
        var gate = Gate.armed()
        XCTAssertTrue(gate.drop(Data([1, 2])), "pre-ready frames must not be sent")
        XCTAssertTrue(gate.drop(Data([3, 4, 5])))
        XCTAssertEqual(gate.droppedBytes, 5, "the tripwire must count what was thrown away")
    }

    /// The contract's second half: at ready the gate opens, reports its count
    /// exactly once, and everything after goes direct. The once-only matters
    /// because `.ready` is reachable more than once per call — a reconnect
    /// re-runs the handshake, a rotation returns via `audio.rebind` — and the
    /// tripwire log line must not print twice.
    func testReleaseReportsTheDropOnceAndThenFramesGoDirect() {
        var gate = Gate.armed()
        XCTAssertTrue(gate.drop(Data([1, 2, 3])))

        XCTAssertEqual(gate.release(), 3, "the first ready reads the count")
        XCTAssertNil(gate.release(), "a repeat ready reads nothing — one log line per call")
        XCTAssertFalse(gate.drop(Data([4, 5])), "hearing has started; frames go direct")
    }

    /// A gate nobody armed sends everything direct — the state a torn-down call
    /// resets to, so a stale gate can never mute a later call's microphone.
    func testAnUnarmedGateSendsEverythingDirect() {
        var gate = Gate()
        XCTAssertFalse(gate.drop(Data([1, 2])))
        XCTAssertNil(gate.release())
    }
}
