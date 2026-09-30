import UIKit
import XCTest
@testable import Magician

/// Drives the whole armed-window lifecycle against the five doubles, with no
/// microphone, no socket and no ActivityKit — none of which the simulator
/// offers, and the controller is the one piece of this feature that decides
/// when a microphone turns off.
@MainActor
final class AmbientControllerTests: XCTestCase {

    private var mic: FakeAmbientMicSource!
    private var spotter: FakeWakeSpotter!
    private var call: FakeAmbientCallSink!
    private var activity: FakeAmbientActivitySink!
    private var resumeKeepalive: FakeAmbientSessionKeepalive!
    private var controller: AmbientController!

    /// This suite writes to the REAL App Group, for the reason `AmbientArmTests`
    /// gives: that container is the contract between the app and the widget
    /// process. Ambient mode is a microphone feature so the suite gets run on
    /// hardware, where clobbering a developer's live record would leave the tap
    /// running with the disarm intent reading `nil`.
    private var preexisting: AmbientArm?

    /// The disarm channel lives in the same real container and is preserved for
    /// the same reason — see `AmbientSignalTests`, where the keys are spelled out
    /// as the cross-process contract they are.
    private let store = UserDefaults(suiteName: MagicianAccess.appGroup) ?? .standard
    private var preexistingSignals: [String: String] = [:]
    private static let signalKeys = [
        "ambient.pendingDisarm",
        "ambient.disarmAck",
        "ambient.pendingExtension"
    ]

    /// `VoiceCallAudioFocus` is a process-wide singleton, so asserting it is
    /// *held* is absolute but asserting it is *free* has to be relative to
    /// whatever the rest of the process was already holding.
    private var focusBaseline = false

    override func setUp() async throws {
        try await super.setUp()
        preexisting = AmbientArm.claim()
        AmbientArm.clear()
        for key in Self.signalKeys {
            if let value = store.string(forKey: key) { preexistingSignals[key] = value }
            store.removeObject(forKey: key)
        }
        focusBaseline = VoiceCallAudioFocus.shared.isActive
        mic = FakeAmbientMicSource()
        spotter = FakeWakeSpotter()
        call = FakeAmbientCallSink()
        activity = FakeAmbientActivitySink()
        resumeKeepalive = FakeAmbientSessionKeepalive()
        controller = AmbientController(
            mic: mic,
            spotter: spotter,
            call: call,
            activity: activity,
            resumeKeepalive: resumeKeepalive
        )
        // Pinned rather than inherited, and it has to be. `UIDevice.batteryLevel`
        // is whatever the simulator feels like reporting — `-1` and `.unknown` are
        // both normal there — and the power rails are real: a simulator that
        // answered `0.0` would refuse every window in this file and the failures
        // would look like ambient bugs. The rail's own cases set this explicitly, so
        // nothing is hidden by pinning a healthy phone here.
        controller.power.readings = { .healthy }
        // The two production pauses, zeroed suite-wide on the same lever as
        // `resumeCooldown`: the settle pause before the connect retry and the
        // spacing between re-arm attempts. A test that exercises a pause sets
        // its own; everything else must not slow down by 1.5 s per failed
        // connect. `resumeCooldown` itself stays real — `waitForResumeCooldown`
        // parks inside it on purpose.
        controller.connectRetryDelay = 0
        controller.micRetryDelay = 0
        // The phase-word pulse, DISABLED suite-wide by the driver's own safety
        // (a cadence that does not exceed the show refuses to schedule), so no
        // background timer can land a `.phaseWord` flip inside an unrelated
        // assertion. The pulse tests set real ratios explicitly.
        controller.phaseWordShowSeconds = 0
        controller.phaseWordCadenceSeconds = 0
    }

    override func tearDown() async throws {
        // Every test disarms, so the singleton audio focus cannot leak into the
        // next one and make its baseline meaningless.
        await controller.disarm(reason: nil)
        controller = nil
        if let preexisting {
            preexisting.save()
        } else {
            AmbientArm.clear()
        }
        preexisting = nil
        for key in Self.signalKeys {
            if let value = preexistingSignals[key] {
                store.set(value, forKey: key)
            } else {
                store.removeObject(forKey: key)
            }
        }
        preexistingSignals = [:]
        try await super.tearDown()
    }

    // MARK: - arming

    func testArmingStartsTheMicAndLandsArmed() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        XCTAssertEqual(controller.state, .armed)
        XCTAssertTrue(mic.isRunning)
        XCTAssertEqual(spotter.configuredPhrases, ["hey sam"])
        XCTAssertEqual(spotter.resetCount, 1, "detection state from a previous window must not carry over")
        XCTAssertNotNil(AmbientArm.claim())
        XCTAssertTrue(
            VoiceCallAudioFocus.shared.isActive,
            "chat auto-speak seizes the shared AVAudioSession; the armed window has to hold focus against it"
        )
    }

    /// The armed window is a microphone tap. Arming twice would open a second
    /// one and leak the first, so a non-`.off` controller refuses.
    func testArmingTwiceDoesNotOpenASecondTap() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        await controller.arm(phrases: ["hey other"], capSeconds: 60)

        XCTAssertEqual(mic.startCount, 1)
        XCTAssertEqual(spotter.configuredPhrases, ["hey sam"], "the second arm must not have reconfigured the live spotter")
    }

    func testArmingIsRefusedWhileAnObservationSessionOwnsTheMic() async {
        controller.observationIsActive = { true }

        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        if case .recoverableError = controller.state {} else {
            XCTFail("expected a refusal, got \(controller.state)")
        }
        XCTAssertFalse(mic.isRunning)
        XCTAssertEqual(mic.startCount, 0)
        XCTAssertNil(AmbientArm.claim())
        XCTAssertTrue(
            activity.lifecycleVerbs.isEmpty,
            "a window that never opened must not put an orb on screen saying it is armed"
        )
    }

    /// The orb is the disarm control, and the only one reachable without opening
    /// the app — `Activity.request` throws and the user can turn Live Activities
    /// off entirely. Swallowing that would leave a live microphone with no proof
    /// of life and no way to stop it. So: no visible indicator, no armed mic.
    func testArmingFailsWhenTheOrbCannotBeShown() async {
        activity.failToStart = true

        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        if case .recoverableError = controller.state {} else {
            XCTFail("expected a refusal, got \(controller.state)")
        }
        XCTAssertFalse(mic.isRunning, "the tap must not outlive the orb that was meant to stop it")
        XCTAssertEqual(mic.stopCount, 1)
        XCTAssertNil(AmbientArm.claim(), "the widget must not find a record for a window that is not running")
        XCTAssertEqual(VoiceCallAudioFocus.shared.isActive, focusBaseline)
        XCTAssertEqual(
            activity.lifecycleVerbs,
            [.start],
            "a refused request must not be followed by an end for an activity that never existed"
        )
    }

    func testAMicFailureSurfacesAsAnErrorRatherThanASilentlyDeadWindow() async {
        mic.startError = FakeAmbientMicSource.StartFailure.unavailable

        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        if case .recoverableError = controller.state {} else {
            XCTFail("expected an error, got \(controller.state)")
        }
        XCTAssertFalse(mic.isRunning)
        XCTAssertNil(AmbientArm.claim())
        XCTAssertEqual(VoiceCallAudioFocus.shared.isActive, focusBaseline, "focus must not be held for a window that never opened")
        XCTAssertTrue(activity.lifecycleCalls.isEmpty, "no tap, no orb")
    }

    // MARK: - the phrase set

    /// Vosk drops out-of-lexicon words from a grammar with a log line and
    /// nothing else, so a fully rejected set leaves a spotter that can never
    /// call `onHit`. Arming over it would put an orb saying "listening" above a
    /// microphone the wake word cannot reach.
    func testArmingIsRefusedWhenEveryPhraseIsRejected() async {
        spotter.phrasesToReject = ["hey magican"]

        await controller.arm(phrases: ["hey magican"], capSeconds: 7_200)

        guard case .recoverableError(let message) = controller.state else {
            return XCTFail("expected a refusal, got \(controller.state)")
        }
        XCTAssertTrue(message.contains("hey magican"), "the user never typed the phrase; a refusal that does not name it is unactionable")
        XCTAssertEqual(mic.startCount, 0, "nothing may open a tap for a spotter that can never fire")
        XCTAssertFalse(mic.isRunning)
        XCTAssertNil(AmbientArm.claim())
        XCTAssertEqual(VoiceCallAudioFocus.shared.isActive, focusBaseline)
        XCTAssertTrue(activity.lifecycleVerbs.isEmpty, "no wakeable spotter, no orb")
        XCTAssertNil(controller.listeningFor)
    }

    /// The ordinary first-run degradation: the primary-agent identity cache is
    /// empty, so there is no phrase to derive. Refusing is the whole point —
    /// arming on an invented name would gate a live microphone on a word the
    /// user never chose.
    func testArmingIsRefusedWhenThereIsNoPhraseAtAll() async {
        await controller.arm(phrases: [], capSeconds: 7_200)

        if case .recoverableError = controller.state {} else {
            XCTFail("expected a refusal, got \(controller.state)")
        }
        XCTAssertEqual(mic.startCount, 0)
        XCTAssertNil(AmbientArm.claim())
        XCTAssertTrue(activity.lifecycleVerbs.isEmpty)
    }

    /// Every phrase passed the lexicon check and the decoder still did not come
    /// up. `rejectedPhrases` is empty here, so a controller that inferred
    /// armability from that list alone would open a window over an inert
    /// spotter — which is why `isArmed` is the thing consulted.
    func testArmingIsRefusedWhenTheSpotterCannotBuildARecognizer() async {
        spotter.isInert = true

        await controller.arm(phrases: ["hey magician"], capSeconds: 7_200)

        if case .recoverableError = controller.state {} else {
            XCTFail("expected a refusal, got \(controller.state)")
        }
        XCTAssertEqual(mic.startCount, 0)
        XCTAssertNil(AmbientArm.claim())
    }

    /// A measured rate is REPORTED, not enforced. Refusing on it is a product
    /// decision nobody has taken — and there is nowhere to take it, since the
    /// measured rates form a continuum — so the window opens and the number
    /// reaches a surface rather than only a log.
    func testAMeasuredPhraseArmsAndItsNumberIsReportedRatherThanRefused() async {
        spotter.notesToReport = ["hey sam": "Measured: 53% of 15 deliberately similar-sounding phrases also woke it."]

        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        XCTAssertEqual(controller.state, .armed, "the worst measured prefixed phrase must still arm")
        XCTAssertTrue(mic.isRunning)
        XCTAssertEqual(controller.listeningFor?.phrases, ["hey sam"])
        XCTAssertEqual(
            controller.listeningFor?.notes,
            [PhraseNote(phrase: "hey sam", note: "Measured: 53% of 15 deliberately similar-sounding phrases also woke it.")]
        )
    }

    /// **The gap the note shape exists to close.** A phrase nobody has measured
    /// used to be reported exactly like the best row in the matrix — as silence —
    /// so the bar said "Listening for “Hey Alexandra”" with nothing to say that
    /// the number behind it was unknown rather than good.
    func testAnUnmeasuredPhraseArmsAndSaysSoRatherThanReportingNothing() async {
        await controller.arm(phrases: ["hey alexandra"], capSeconds: 7_200)

        XCTAssertEqual(controller.state, .armed)
        XCTAssertEqual(controller.listeningFor?.phrases, ["hey alexandra"])
        XCTAssertEqual(controller.listeningFor?.notes.map(\.phrase), ["hey alexandra"])
        XCTAssertEqual(controller.listeningFor?.notes.first?.note, FakeWakeSpotter.unmeasuredNote)
    }

    /// A partly rejected set arms with the survivors, and reports only those:
    /// a bar saying it is listening for a phrase that was dropped from the
    /// grammar tells the user to say something that can never work.
    func testAPartlyRejectedPhraseSetArmsWithTheSurvivorsOnly() async {
        spotter.phrasesToReject = ["hey magican"]

        await controller.arm(phrases: ["hey magican", "hey magician"], capSeconds: 7_200)

        XCTAssertEqual(controller.state, .armed)
        XCTAssertEqual(controller.listeningFor?.phrases, ["hey magician"])
        // And only the survivor carries a note: a rejected phrase never armed, so
        // there is no measurement about it to show beside a live microphone.
        XCTAssertEqual(controller.listeningFor?.notes.map(\.phrase), ["hey magician"])
    }

    /// The bar is driven off this, so a phrase set outliving its window would
    /// offer to stop a microphone that is already off.
    func testTheListeningPhraseSetIsClearedWhenTheWindowCloses() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        XCTAssertNotNil(controller.listeningFor)

        await controller.disarm(reason: nil)

        XCTAssertNil(controller.listeningFor)
    }

    /// The unwind path clears it too — the teardown that does not go through
    /// `disarm`, and therefore the one that grows holes.
    func testAWindowUnwoundByAFailedOrbClearsThePhraseSet() async {
        activity.failToStart = true

        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        XCTAssertNil(controller.listeningFor)
    }

    func testTheRefusalMessageNamesEveryRejectedPhrase() {
        let message = AmbientController.unusablePhrasesMessage(
            requested: ["hey magican", "hey zzz"],
            rejected: ["hey magican", "hey zzz"]
        )
        XCTAssertTrue(message.contains("hey magican"))
        XCTAssertTrue(message.contains("hey zzz"))
    }

    /// Three different causes the user can do three different things about, so
    /// they must not collapse into one sentence.
    func testTheRefusalMessageDistinguishesItsThreeCauses() {
        let noPhrase = AmbientController.unusablePhrasesMessage(requested: [], rejected: [])
        let rejected = AmbientController.unusablePhrasesMessage(requested: ["hey magican"], rejected: ["hey magican"])
        let inert = AmbientController.unusablePhrasesMessage(requested: ["hey magician"], rejected: [])

        XCTAssertNotEqual(noPhrase, rejected)
        XCTAssertNotEqual(rejected, inert)
        XCTAssertNotEqual(noPhrase, inert)
    }

    // MARK: - the resume cooldown

    /// Park the handoff after its failed connect, inside the cooldown. A failed
    /// connect is retried once before the fallback, so "the handoff ran" is two
    /// attempts, not one.
    private func waitForResumeCooldown() async {
        for _ in 0 ..< 1_000 where call.startCallCount < 2 || mic.startCount < 2 {
            await Task.yield()
        }
        XCTAssertEqual(call.startCallCount, 2, "precondition: the handoff ran, including its one automatic retry")
        XCTAssertTrue(mic.isRunning, "precondition: active I/O keeps the ambient session alive during the cooldown")
        XCTAssertEqual(spotter.resetCount, 1, "the wake decoder must still be gated until the cooldown ends")
    }

    /// The one lesson the desktop integration transferred (`8bd5c2f9e`): the
    /// conversation's tail re-fires a spotter that was re-armed too eagerly.
    /// `VoskWakeSpotter.fireCooldown` cannot cover it — `resumeSpotting` builds a
    /// fresh recognizer, which has no memory of the fire that just happened.
    func testTheWakeDecoderStaysGatedWhileAudioIOBridgesTheResumeCooldown() async {
        XCTAssertEqual(
            controller.resumeCooldown,
            AmbientController.wakeResumeCooldown,
            "the hook must default to the real cooldown; defaulted to 0 it would disable it in production"
        )
        controller.resumeCooldown = 600
        call.failToStart = true
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        spotter.simulateHit()
        await waitForResumeCooldown()

        mic.emit(Data([1, 2, 3, 4]))
        XCTAssertEqual(mic.startCount, 2, "local audio I/O must restart before the cooldown sleep")
        XCTAssertEqual(spotter.fedByteCount, 0, "the conversation tail must be dropped rather than decoded")
        XCTAssertNotNil(AmbientArm.claim(), "the window is still open, so the orb's disarm must still find it")
        XCTAssertTrue(AmbientSignal.isObservingDisarm, "and the disarm signal must still be heard during the cooldown")
    }

    /// **The critical one.** The cooldown is a suspension point in the middle of
    /// a teardown-sensitive path: `disarm` can run start to finish inside it,
    /// needing no user at all because the cap timer is still live. A resume that
    /// did not re-check would put the tap back with no orb, no arm record, no cap
    /// timer and no audio-focus token — a microphone nothing will ever stop.
    func testADisarmDuringTheResumeCooldownDoesNotBringTheTapBack() async {
        controller.resumeCooldown = 600
        call.failToStart = true
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await waitForResumeCooldown()

        await controller.disarm(reason: "Listening window ended.")
        await controller.settle()

        XCTAssertEqual(controller.state, .off)
        XCTAssertFalse(mic.isRunning, "the tap must not come back for a window that closed during the cooldown")
        XCTAssertEqual(mic.startCount, 2)
        XCTAssertNil(AmbientArm.claim())
        XCTAssertEqual(VoiceCallAudioFocus.shared.isActive, focusBaseline)
        XCTAssertFalse(AmbientSignal.isObservingDisarm)
    }

    /// The mirror of the disarm case: a LATER window taking over while an older
    /// resume is still parked. The stale resume must not reinstall the spotting
    /// tap under the new window's live call — "exactly one path owns the
    /// microphone at any instant" is the invariant this whole design rests on.
    func testAResumeParkedInTheCooldownDoesNotDisturbTheWindowThatReplacedIt() async {
        controller.resumeCooldown = 600
        call.failToStart = true
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await waitForResumeCooldown()
        await controller.disarm(reason: nil)

        // Window two, armed and conversing while window one is still parked.
        controller.resumeCooldown = 0
        call.failToStart = false
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()
        XCTAssertEqual(controller.state, .conversing(.listening), "precondition: window two is live")

        for _ in 0 ..< 200 { await Task.yield() }

        XCTAssertEqual(controller.state, .conversing(.listening), "window two's conversation must be untouched")
        XCTAssertFalse(mic.isRunning, "a stale resume must not put the spotting tap back under a live call")
    }

    /// **The orb must not still be claiming it heard the user, 2.5 s after the
    /// connect already failed.** `.connecting` reduces to the `heard` orb phase —
    /// "I heard you, I am connecting" — which is the most-claiming phase reachable
    /// here, and the wake decoder is gated for the whole cooldown. `.armed` is the least,
    /// and it is where this route is going anyway. Same rule `handleCallEnded`
    /// established for the conversation-ended route.
    func testAFailedConnectDropsTheOrbToArmedBeforeTheCooldownRatherThanAfter() async {
        controller.resumeCooldown = 600
        call.failToStart = true
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        spotter.simulateHit()
        await waitForResumeCooldown()

        XCTAssertEqual(controller.state, .armed)
        XCTAssertEqual(
            activity.lifecycleCalls.last,
            .update(phase: .armed, announcing: false),
            "the orb must not assert `heard` over a connect that has already failed"
        )
        XCTAssertEqual(mic.startCount, 2, "and active local I/O is bridging the cooldown")
    }

    /// And it does come back, which is the other half: a failed connect must not
    /// cost the user the wake word for the rest of the window.
    func testTheWakeDecoderOpensOnceTheResumeCooldownHasPassed() async {
        controller.resumeCooldown = 0.01
        call.failToStart = true
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        spotter.simulateHit()
        await controller.settle()

        XCTAssertEqual(controller.state, .armed)
        XCTAssertTrue(mic.isRunning)
        XCTAssertEqual(mic.startCount, 2)
        XCTAssertEqual(spotter.resetCount, 2, "the decoder must start the next stretch from nothing")
        mic.emit(Data([1, 2, 3, 4]))
        XCTAssertEqual(spotter.fedByteCount, 4, "frames must reach Vosk once the cooldown gate opens")
    }

    /// Device ownership transfer, not just state: silent output must already be
    /// running at the instant the provider graph stops, and may leave only after
    /// the wake input graph has successfully started. Otherwise a
    /// voice-processing stop can lapse the background recording session even
    /// though neither side calls `setActive(false)`.
    func testConversationEndBridgesActiveIOAcrossProviderStopAndWakeStart() async {
        await armAndConverse()
        // `armAndConverse` removes the cooldown so its shared precondition can
        // settle. Restore a long one only for the end transition under test.
        controller.resumeCooldown = 600
        var bridgeWasActiveAtProviderStop = false
        call.onEndCall = { [weak resumeKeepalive] in
            bridgeWasActiveAtProviderStop = resumeKeepalive?.isActive == true
        }

        call.emitLifecycle(.ended(.wentQuiet))
        for _ in 0 ..< 1_000 where mic.startCount < 2 { await Task.yield() }

        XCTAssertTrue(bridgeWasActiveAtProviderStop)
        XCTAssertEqual(resumeKeepalive.startCount, 1)
        XCTAssertEqual(resumeKeepalive.stopCount, 1)
        XCTAssertFalse(resumeKeepalive.isActive, "the wake input now owns active I/O")
        XCTAssertTrue(mic.isRunning)
        XCTAssertEqual(spotter.resetCount, 1, "wake decoding remains gated during the tail pause")
    }

    // MARK: - the privacy invariant

    /// While armed, audio reaches the local spotter and NOTHING else. This is
    /// the feature's whole guarantee, so it is asserted directly rather than
    /// inferred from the spotter having no networking collaborator.
    func testFramesWhileArmedReachTheSpotterAndNothingElse() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        mic.emit(Data([1, 2, 3, 4]))

        XCTAssertEqual(spotter.fedByteCount, 4)
        XCTAssertEqual(call.startCallCount, 0, "no call may open before the phrase matches — and no audio leaves regardless")
        XCTAssertEqual(call.endCount, 0)
    }

    // MARK: - wake handoff

    /// A system tap has already expressed intent, so it must enter the SAME
    /// handoff without demanding a wake phrase too. The order assertion is the
    /// same privacy invariant as the wake path: spotting stops before the call
    /// can take the microphone.
    func testExplicitTalkStartsTheFirstConversationWithoutAWakePhrase() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        var tapWasRunningInsideStartCall: Bool?
        call.onStartCall = { [weak mic] in tapWasRunningInsideStartCall = mic?.isRunning }

        let started = await controller.talkNow()
        await controller.settle()

        XCTAssertTrue(started)
        XCTAssertEqual(tapWasRunningInsideStartCall, false)
        XCTAssertEqual(call.startCallCount, 1)
        XCTAssertEqual(controller.state, .conversing(.listening))
        XCTAssertFalse(mic.isRunning)
        XCTAssertNotNil(AmbientArm.claim(), "the immediate turn still belongs to the durable ambient window")
        XCTAssertTrue(
            activity.lifecycleCalls.contains(.update(phase: .heard, announcing: false)),
            "a watched explicit tap must not fire the wake alert or its unavoidable sound"
        )
        XCTAssertFalse(activity.lifecycleCalls.contains(.update(phase: .heard, announcing: true)))
    }

    /// The state transition happens synchronously before the handoff task is
    /// enqueued, so a double tap cannot open two provider sessions.
    func testExplicitTalkIsIdempotentWhileAConversationStarts() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        let first = await controller.talkNow()
        let second = await controller.talkNow()
        await controller.settle()

        XCTAssertTrue(first)
        XCTAssertFalse(second)
        XCTAssertEqual(call.startCallCount, 1)
    }

    func testExplicitTalkRefusesWhenNoAmbientWindowExists() async {
        let started = await controller.talkNow()

        XCTAssertFalse(started)
        XCTAssertEqual(call.startCallCount, 0)
        XCTAssertFalse(mic.isRunning)
    }

    /// A tap can land during the bounded post-call resume cooldown, when the
    /// state already says armed but the spotting tap is still off. The explicit
    /// action waits for that transition rather than becoming a visible no-op.
    func testExplicitTalkWaitsForTheResumeCooldownInsteadOfBeingDropped() async {
        controller.resumeCooldown = 0.01
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        let firstStarted = await controller.talkNow()
        XCTAssertTrue(firstStarted)
        await controller.settle()

        call.emitLifecycle(.ended(.wentQuiet))
        let restarted = await controller.talkNow()
        await controller.settle()

        XCTAssertTrue(restarted)
        XCTAssertEqual(call.startCallCount, 2)
        XCTAssertEqual(controller.state, .conversing(.listening))
    }

    /// After the sink's eight-second continuous window ends, only its provider
    /// call closes. The ambient window itself returns to spotting, and a later
    /// wake phrase still works without another system-control tap.
    func testExplicitFirstTurnReturnsToWakeWordFollowUps() async {
        controller.resumeCooldown = 0
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        let firstStarted = await controller.talkNow()
        XCTAssertTrue(firstStarted)
        await controller.settle()

        call.emitLifecycle(.ended(.wentQuiet))
        await controller.settle()
        XCTAssertEqual(controller.state, .armed)
        XCTAssertTrue(mic.isRunning)

        spotter.simulateHit()
        await controller.settle()
        XCTAssertEqual(call.startCallCount, 2)
        XCTAssertEqual(controller.state, .conversing(.listening))
    }

    func testAWakeHitStopsSpottingBeforeTheCallStarts() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        mic.emit(Data([9, 9, 9, 9]))
        // Asserting after `settle()` cannot tell "before" from "after" — both
        // orderings end with the tap stopped. Only a reading taken from inside
        // `startCall` pins it, and two paths owning the mic at once is the one
        // thing this feature's design rules out.
        var tapWasRunningInsideStartCall: Bool?
        call.onStartCall = { [weak mic] in tapWasRunningInsideStartCall = mic?.isRunning }

        spotter.simulateHit()
        await controller.settle()

        XCTAssertEqual(tapWasRunningInsideStartCall, false, "the spotting tap must be released BEFORE the call takes the mic")
        XCTAssertFalse(mic.isRunning)
        XCTAssertEqual(mic.stopCount, 1)
        XCTAssertEqual(call.startCallCount, 1)
    }

    /// A hit that lands mid-conversation is the user talking to the assistant,
    /// not asking for a second one.
    func testAWakeHitDuringAConversationIsIgnored() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()

        spotter.simulateHit()
        await controller.settle()

        XCTAssertEqual(call.startCallCount, 1)
    }

    func testConversationTurnsDriveTheState() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()

        call.emit(.thinking)
        XCTAssertEqual(controller.state, .conversing(.thinking))

        call.emit(.speaking)
        XCTAssertEqual(controller.state, .conversing(.speaking))
    }

    /// `turnPublisher` does not replay, so a controller that subscribes after
    /// `startCall` loses whatever the call emitted while connecting — and then
    /// clobbers it with `listening`. To the user that reads as the assistant
    /// ignoring them. The bug is invisible against a replaying publisher, which
    /// is exactly why it is pinned here.
    func testATurnEmittedWhileConnectingIsNotDropped() async {
        call.onStartCall = { [weak call] in call?.emit(.thinking) }

        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()

        XCTAssertEqual(
            controller.state,
            .conversing(.thinking),
            "the turn that arrived during connect must survive, not be overwritten by the default listening turn"
        )
    }

    /// A failed socket must not end the armed window — the user should still be
    /// able to say the phrase again without touching the phone.
    func testAFailedCallFallsBackToArmedNotOff() async {
        call.failToStart = true
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        spotter.simulateHit()
        await controller.settle()

        XCTAssertEqual(controller.state, .armed)
        XCTAssertTrue(mic.isRunning, "spotting must resume so the wake word still works")
        XCTAssertEqual(mic.startCount, 2)
        XCTAssertNotNil(AmbientArm.claim(), "the window is still armed, so the widget's disarm control must still find it")
    }

    // MARK: - the connect retry and its notice

    /// A connect that fails is retried ONCE before anything falls back: the
    /// user committed these seconds at the wake and has already stood through
    /// one whole failed connect, so a second wait is cheap next to what giving
    /// up costs — a fallback the user has to notice before the phrase works
    /// again.
    func testAFailedConnectRetriesOnceAndConversesWhenTheRetrySucceeds() async {
        call.scriptedStartResults = [false, true]
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        spotter.simulateHit()
        await controller.settle()

        XCTAssertEqual(controller.state, .conversing(.listening), "the retry connected, so the wake survived")
        XCTAssertEqual(call.startCallCount, 2, "one failed attempt, one automatic retry")
        XCTAssertFalse(
            captionRecords.contains(.caption(AmbientController.connectFailedNotice, role: nil)),
            "a retry that succeeded owes the user no apology"
        )
    }

    /// The retry re-publishes `heard` so the island's give-up gauge restarts
    /// with the attempt it depicts (`ContentState.connectingSince` is stamped
    /// fresh on every publish into `heard` — `AmbientActivity.connectClock`
    /// pins the rule) — and WITHOUT a second alert: the wake spent its one
    /// earned island expansion already, and a retry is the machine moving,
    /// not the user speaking.
    func testTheRetryRepublishesHeardWithoutASecondAlert() async {
        call.scriptedStartResults = [false, true]
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        spotter.simulateHit()
        await controller.settle()

        XCTAssertEqual(
            activity.calls.filter { $0 == .update(phase: .heard, announcing: true) }.count,
            1,
            "exactly one alert per wake — the retry must not re-expand the island"
        )
        XCTAssertEqual(
            activity.calls.filter { $0 == .update(phase: .heard, announcing: false) }.count,
            1,
            "the retry's one re-publish, which restarts the give-up gauge for the fresh attempt"
        )
    }

    /// Only after BOTH attempts fail does the window fall back — and no longer
    /// silently: the notice goes up, because the user was not looking at their
    /// phone when the connect died and the sentence has to survive until they
    /// are.
    func testBothAttemptsFailingFallBackToArmedWithTheNoticeOnTheOrb() async {
        controller.resumeCooldown = 0
        call.failToStart = true
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        spotter.simulateHit()
        await controller.settle()

        XCTAssertEqual(controller.state, .armed)
        XCTAssertTrue(mic.isRunning, "spotting resumed — the notice's instruction has to be actionable")
        XCTAssertEqual(
            call.startCallCount,
            2,
            "one automatic retry and no more — a third attempt would spend patience nobody granted"
        )
        XCTAssertEqual(
            captionRecords.last,
            .caption(AmbientController.connectFailedNotice, role: nil),
            "the fallback that used to be silent now says so, with no speaker chip"
        )
    }

    /// The settle pause is a suspension point like the connect itself: a
    /// disarm can land inside it, and the retry must then never fire — a retry
    /// for a window that is gone would connect a socket nobody can stop. The
    /// long delay here is never actually waited out: `disarm` cancels the
    /// parked handoff, which is exactly the path under test.
    func testADisarmDuringTheSettlePauseCancelsTheRetry() async {
        controller.connectRetryDelay = 60
        call.failToStart = true
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        spotter.simulateHit()
        for _ in 0 ..< 500 where call.startCallCount == 0 { await Task.yield() }
        XCTAssertEqual(call.startCallCount, 1, "precondition: the first attempt failed and the handoff is parked in the pause")

        await controller.disarm(reason: nil)
        await controller.settle()

        XCTAssertEqual(call.startCallCount, 1, "the retry must not fire for a window that is gone")
        XCTAssertEqual(controller.state, .off)
        XCTAssertFalse(mic.isRunning)
    }

    /// The notice must not outlive the wake that answers it: the next wake IS
    /// the instruction being followed, and the caption belongs to the new
    /// attempt from the moment it is heard.
    func testTheConnectNoticeClearsOnTheNextWake() async {
        controller.resumeCooldown = 0
        call.scriptedStartResults = [false, false, true]
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()
        XCTAssertEqual(
            captionRecords.last,
            .caption(AmbientController.connectFailedNotice, role: nil),
            "precondition: the notice is standing"
        )

        spotter.simulateHit()
        await controller.settle()

        XCTAssertEqual(controller.state, .conversing(.listening), "the wake the notice asked for worked")
        XCTAssertEqual(
            captionRecords.last,
            .caption("", role: nil),
            "the notice came down with the wake, not with the window"
        )
    }

    // MARK: - orb verb selection

    /// The ActivityKit verb comes from BOTH reductions, and the two boundary
    /// crossings are what a counter-per-verb double cannot see. A draft that
    /// called `update` on the way in never requested the activity at all, and
    /// one that returned early on non-`nil` → `nil` never called `end`, so the
    /// disarm reason never reached the user.
    func testTheOrbIsStartedThenEndedAndTheEndCarriesTheReason() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        await controller.disarm(reason: "Listening window ended.")

        XCTAssertEqual(activity.lifecycleVerbs, [.start, .end], "never update on the way in, never a missing end")
        guard case .start(let phase, let armedAt, let expiresAt) = activity.lifecycleCalls.first else {
            return XCTFail("expected the first verb to be start, got \(activity.lifecycleCalls)")
        }
        XCTAssertEqual(phase, .armed)
        XCTAssertEqual(expiresAt.timeIntervalSince(armedAt), 7_200, accuracy: 1, "the orb shows the remaining leash")
        XCTAssertEqual(activity.lifecycleCalls.last, .end(reason: "Listening window ended."))
    }

    func testTheOrbFollowsTheConversationWithUpdatesBetweenStartAndEnd() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()
        call.emit(.speaking)
        await controller.disarm(reason: "Done for now.")

        XCTAssertEqual(activity.lifecycleVerbs, [.start, .update, .update, .update, .end])
        let calls = activity.lifecycleCalls
        guard calls.count == 5 else { return XCTFail("got \(calls)") }
        // `heard` and `connecting` share one reduction, so the handoff spends a
        // single update between them — and that update is the wake, the one
        // publish that announces (carries the island's expand alert).
        XCTAssertEqual(Array(calls[1 ... 3]), [
            .update(phase: .heard, announcing: true),
            .update(phase: .listening, announcing: false),
            .update(phase: .speaking, announcing: false)
        ])
        XCTAssertEqual(calls[4], .end(reason: "Done for now."))
    }

    /// **Each wake earns its own alert, and nothing between wakes earns any.**
    /// A conversation ends, the window returns to armed, and the next wake
    /// passes through armed→heard again — the alert rides the phase publish
    /// that transition was already spending, so a second wake announces
    /// exactly like the first and the publish cadence is untouched. Every
    /// transition in between — into the conversation, back out to armed —
    /// must stay silent, or the island expands for the machine moving rather
    /// than the user speaking.
    func testEveryWakeAnnouncesAndNothingBetweenWakesDoes() async {
        controller.resumeCooldown = 0
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()
        call.emitLifecycle(.ended(.wentQuiet))
        await controller.settle()
        XCTAssertEqual(controller.state, .armed, "precondition: the window survived its first conversation")

        spotter.simulateHit()
        await controller.settle()

        let announcements = activity.lifecycleCalls.filter {
            if case .update(_, let announcing) = $0 { return announcing }
            return false
        }
        XCTAssertEqual(
            announcements,
            [.update(phase: .heard, announcing: true), .update(phase: .heard, announcing: true)],
            "both wakes announce, and only the wakes"
        )
        XCTAssertTrue(
            activity.lifecycleCalls.contains(.update(phase: .armed, announcing: false)),
            "the return to armed between the wakes rode a silent update"
        )
    }

    // MARK: - the reply's own span

    /// **The measured deadline reaches the orb on its own field, and costs one
    /// update.** The phase change into `speaking` is published anyway and carries no
    /// span, because none is knowable yet; the sink then re-reports the unchanged
    /// turn once its audio queue has settled, and THAT is the one publish the bar
    /// costs. A controller that published on every turn would spend an update per
    /// turn to say nothing.
    func testTheSpeakingSpanIsPublishedOnceTheSinkHasOneAndNotBefore() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()

        // The reply starts. The queue is still growing, so the sink has no span.
        call.emit(.speaking)
        for _ in 0 ..< 50 { await Task.yield() }
        XCTAssertEqual(
            activity.speakingSpans,
            [nil],
            "the orb is offered nothing while the queue is still growing"
        )

        // The queue settles: the sink records a span and re-reports the same turn.
        let now = Date()
        let span = AmbientSpeakingSpan(from: now, until: now.addingTimeInterval(6))
        call.speakingSpan = span
        call.emit(.speaking)
        for _ in 0 ..< 50 { await Task.yield() }

        XCTAssertEqual(activity.speakingSpans, [nil, span])
        XCTAssertEqual(
            activity.lifecycleVerbs,
            [.start, .update, .update, .update],
            "a settled span must not spend a phase update — armed, heard, listening, speaking and no more"
        )
    }

    /// The span dies with the reply. The sink clears it and the phase update that
    /// ends the reply carries the clear, so a bar cannot go on elapsing over a
    /// microphone that has been handed back to the user.
    func testTheSpanIsGoneWhenTheReplyIs() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()
        let now = Date()
        call.speakingSpan = AmbientSpeakingSpan(from: now, until: now.addingTimeInterval(6))
        call.emit(.speaking)
        for _ in 0 ..< 50 { await Task.yield() }

        // The reply runs out: the real sink clears its span before reporting the turn.
        call.speakingSpan = nil
        call.emit(.listening)
        for _ in 0 ..< 50 { await Task.yield() }

        XCTAssertEqual(activity.speakingSpans.last, .some(nil), "nothing left to draw")
        XCTAssertEqual(activity.lifecycleCalls.last, .update(phase: .listening, announcing: false))
    }

    /// **The span is a separate field, and must not touch the caption line.** The
    /// Low Power Mode warning has to survive the whole window — it is the entire
    /// justification for arming in Low Power Mode at all — and `updateCaption`
    /// REPLACES that line, so a span routed through it would blank the warning. This
    /// is the regression the single composition point exists to prevent, arriving
    /// from a new direction.
    func testPublishingASpeakingSpanLeavesThePowerWarningStanding() async {
        controller.power.readings = { .init(lowPowerMode: true, batteryLevel: 0.9, isCharging: false) }
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()
        let now = Date()
        call.speakingSpan = AmbientSpeakingSpan(from: now, until: now.addingTimeInterval(6))
        call.emit(.speaking)
        for _ in 0 ..< 50 { await Task.yield() }

        let captions = activity.calls.compactMap { call -> String? in
            guard case .caption(let text, _) = call else { return nil }
            return text
        }
        XCTAssertEqual(
            captions,
            [AmbientPowerBlock.lowPowerMode.warning],
            "the span published nothing to the caption line, and blanked nothing on it"
        )
        XCTAssertEqual(controller.powerWarning, .lowPowerMode)
    }

    /// The power warning is a system sentence: it must never grow a speaker chip,
    /// and it must win over any transcript line.
    func testPowerWarningOutranksTranscriptAndCarriesNoRole() {
        let line = AmbientController.orbCaption(
            powerWarning: .lowPowerMode,
            notice: nil,
            transcript: AmbientCaptionLine(role: .agent, text: "On it.")
        )
        XCTAssertNil(line.role)
        XCTAssertEqual(line.text, AmbientPowerBlock.lowPowerMode.warning)
    }

    func testTranscriptLineCarriesItsSpeaker() {
        let line = AmbientController.orbCaption(powerWarning: nil,
                                                notice: nil,
                                                transcript: AmbientCaptionLine(role: .user, text: "hey"))
        XCTAssertEqual(line.role, .user)
        XCTAssertEqual(line.text, "hey")
    }

    /// The connect notice sits between the other two inputs: outranked by the
    /// power warning (the one line that must survive the whole window),
    /// outranking the transcript — defensively: in production the notice is set
    /// over a transcript already cleared, so this pins the pure function's
    /// answer, not a reachable collision. A system sentence, so no speaker.
    func testTheConnectNoticeOutranksTheTranscriptAndCarriesNoRole() {
        let line = AmbientController.orbCaption(
            powerWarning: nil,
            notice: AmbientController.connectFailedNotice,
            transcript: AmbientCaptionLine(role: .agent, text: "On it.")
        )
        XCTAssertNil(line.role, "a system sentence must not wear a speaker chip")
        XCTAssertEqual(line.text, AmbientController.connectFailedNotice)
    }

    func testThePowerWarningOutranksTheConnectNotice() {
        let line = AmbientController.orbCaption(
            powerWarning: .lowPowerMode,
            notice: AmbientController.connectFailedNotice,
            transcript: nil
        )
        XCTAssertEqual(line.text, AmbientPowerBlock.lowPowerMode.warning)
        XCTAssertNil(line.role)
    }

    // MARK: - the transcript pipeline

    /// The caption writes alone, in order — the transcript assertions'
    /// counterpart to `lifecycleCalls`, because a caption assertion drowned in
    /// phase updates cannot pin "one publish per line".
    private var captionRecords: [FakeAmbientActivitySink.Call] {
        activity.calls.filter { if case .caption = $0 { return true } else { return false } }
    }

    /// A finalised line reaches the orb with its speaker; partials never existed
    /// on this path by construction (the sink emits finals only).
    func testFinalCaptionReachesTheOrbWithItsRole() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()

        call.emitCaption(AmbientCaptionLine(role: .user, text: "hey"))

        XCTAssertEqual(
            captionRecords,
            [.caption("", role: nil), .caption("hey", role: .user)],
            "one blank when the window opened, then the line with its speaker — one publish per finalised line"
        )
    }

    /// After disarm, a straggler caption must not republish — same class of bug
    /// as a resurrected window.
    func testCaptionAfterDisarmIsDropped() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()
        await controller.disarm(reason: nil)
        let recordsAtDisarm = activity.calls

        call.emitCaption(AmbientCaptionLine(role: .agent, text: "still here"))
        // Drained so a caption wrongly parked on a queued hop would get its
        // chance to land — the happy path delivers inline and needs no wait.
        for _ in 0 ..< 50 { await Task.yield() }

        XCTAssertEqual(activity.calls, recordsAtDisarm, "a dead window's orb must hear nothing more")
    }

    /// The conversation-end blank: a window back at `.armed` must not keep the
    /// finished conversation's words on the lock screen, so the end PUBLISHES
    /// the recomposed caption — blank here, the power warning when the window
    /// carries one — rather than merely clearing state for the next line to
    /// overwrite.
    func testAConversationEndTakesItsWordsOffTheOrb() async {
        await armAndConverse()
        call.emitCaption(AmbientCaptionLine(role: .user, text: "hey"))

        call.emitLifecycle(.ended(.wentQuiet))
        await controller.settle()

        XCTAssertEqual(
            captionRecords,
            [.caption("", role: nil), .caption("hey", role: .user), .caption("", role: nil)],
            "the words went up while the conversation ran, and came down with it"
        )
    }

    // MARK: - disarm and the cap

    func testDisarmStopsEverythingAndClearsTheArm() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()

        await controller.disarm(reason: nil)

        XCTAssertEqual(controller.state, .off)
        XCTAssertFalse(mic.isRunning)
        XCTAssertEqual(call.endCount, 1)
        XCTAssertNil(AmbientArm.claim())
        XCTAssertEqual(VoiceCallAudioFocus.shared.isActive, focusBaseline, "the window released the shared audio session")
    }

    func testExtendAddsThirtyMinutesToEveryWindowAuthority() async throws {
        await controller.arm(phrases: ["hey sam"], capSeconds: 1_800)
        let original = try XCTUnwrap(controller.expiresAt)

        let extended = try XCTUnwrap(
            controller.extendWindow(now: original.addingTimeInterval(-600))
        )

        XCTAssertEqual(
            extended.timeIntervalSince(original),
            AmbientExtensionPolicy.incrementSeconds,
            accuracy: 0.001
        )
        XCTAssertEqual(controller.expiresAt, extended)
        XCTAssertEqual(AmbientArm.claim()?.expiresAt, extended)
        XCTAssertEqual(activity.lifecycleCalls.last, .expiry(extended))
    }

    func testExtendStopsAtTheLiveActivityEightHourCeiling() async throws {
        let nearlyMaximum = Int(AmbientExtensionPolicy.maximumWindowSeconds) - 600
        await controller.arm(phrases: ["hey sam"], capSeconds: nearlyMaximum)
        let original = try XCTUnwrap(controller.expiresAt)
        let armedAt = try XCTUnwrap(AmbientArm.claim()?.armedAt)

        let extended = try XCTUnwrap(controller.extendWindow())
        XCTAssertEqual(
            extended,
            armedAt.addingTimeInterval(AmbientExtensionPolicy.maximumWindowSeconds)
        )
        XCTAssertEqual(extended.timeIntervalSince(original), 600, accuracy: 0.001)

        let callCountAtCeiling = activity.calls.count
        XCTAssertNil(controller.extendWindow())
        XCTAssertEqual(
            activity.calls.count,
            callCountAtCeiling,
            "a capped button tap must not spend an ActivityKit update"
        )
    }

    func testExtendCannotReviveAnExpiredWindow() async throws {
        await controller.arm(phrases: ["hey sam"], capSeconds: 1_800)
        let original = try XCTUnwrap(controller.expiresAt)

        XCTAssertNil(controller.extendWindow(now: original))
        XCTAssertEqual(controller.expiresAt, original)
        XCTAssertEqual(AmbientArm.claim()?.expiresAt, original)
        XCTAssertFalse(activity.lifecycleCalls.contains { call in
            if case .expiry = call { return true }
            return false
        })
    }

    func testPendingExtensionIsConsumedOnceAndAppliedToTheLiveWindow() async throws {
        await controller.arm(phrases: ["hey sam"], capSeconds: 1_800)
        let original = try XCTUnwrap(controller.expiresAt)
        AmbientSignal.requestExtension()

        controller.consumePendingExtensionRequest()

        let extended = try XCTUnwrap(controller.expiresAt)
        XCTAssertEqual(extended.timeIntervalSince(original), 1_800, accuracy: 0.001)
        XCTAssertNil(AmbientSignal.consumePendingExtension())
    }

    func testExtensionButtonSignalMovesTheLiveWindowWithoutForegrounding() async throws {
        await controller.arm(phrases: ["hey sam"], capSeconds: 1_800)
        let original = try XCTUnwrap(controller.expiresAt)
        AmbientSignal.requestExtension()

        AmbientSignal.postExtension()

        for _ in 0 ..< 100 where controller.expiresAt == original {
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        let extended = try XCTUnwrap(controller.expiresAt)
        XCTAssertEqual(extended.timeIntervalSince(original), 1_800, accuracy: 0.001)
        XCTAssertEqual(activity.lifecycleCalls.last, .expiry(extended))
    }

    func testExtensionObserverLivesExactlyAsLongAsTheAmbientWindow() async {
        XCTAssertFalse(AmbientSignal.isObservingExtension)

        await controller.arm(phrases: ["hey sam"], capSeconds: 1_800)
        XCTAssertTrue(AmbientSignal.isObservingExtension)

        await controller.disarm(reason: nil)
        XCTAssertFalse(AmbientSignal.isObservingExtension)
    }

    /// The hard cap is the leash on a microphone the user is not watching. A
    /// timer that never fires is the worst failure this feature has. This is
    /// also the immediate case — cap landing on an idle armed window — which the
    /// deferral below must never slow down.
    func testTheCapDisarmsTheWindowOnItsOwn() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 0)

        for _ in 0 ..< 400 where controller.state != .off {
            try? await Task.sleep(nanoseconds: 5_000_000)
        }

        XCTAssertEqual(controller.state, .off, "the cap timer never fired")
        XCTAssertFalse(mic.isRunning)
        XCTAssertNil(AmbientArm.claim())
        XCTAssertEqual(
            activity.lifecycleCalls.last,
            .end(reason: "Listening window ended."),
            "the user has to be told why the orb went away"
        )
    }

    /// The leash yields to a wake in flight — and when the connect then FAILS,
    /// the latch collects: the window closes with the cap's own reason, and the
    /// connect notice is deliberately absent, because "say the wake word to try
    /// again" over a window the cap is closing would invite a wake nothing is
    /// listening for. Driven through `handleCapExpiry` directly so the boundary
    /// lands at an exact instant a real timer cannot promise.
    func testACapFiringMidConnectDefersAndAFailedConnectThenDisarmsForTheCap() async {
        call.suspendStartCall = true
        call.failToStart = true
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await waitForConnectToSuspend()

        await controller.handleCapExpiry()

        XCTAssertEqual(controller.state, .connecting, "the cap must defer, not tear down a wake in flight")
        XCTAssertNotNil(AmbientArm.claim(), "the window survives its own boundary while the connect runs")

        call.suspendStartCall = false
        call.finishStartCall()
        await controller.settle()

        XCTAssertEqual(controller.state, .off)
        XCTAssertFalse(mic.isRunning)
        XCTAssertNil(AmbientArm.claim())
        XCTAssertEqual(
            activity.lifecycleCalls.last,
            .end(reason: AmbientEndedReason.capReached),
            "the deferred cap keeps its own sentence"
        )
        XCTAssertFalse(
            captionRecords.contains(.caption(AmbientController.connectFailedNotice, role: nil)),
            "no 'try again' over a window the cap is closing"
        )
    }

    /// And when the connect SUCCEEDS, the conversation the wake bought runs to
    /// its end — then the latch collects there, instead of the window returning
    /// to armed. Bounded by construction: the connect cannot outlive its
    /// watchdog, and the conversation cannot outlive the sink's own timers.
    func testACapFiringMidConnectLetsTheConversationCompleteThenDisarmsForTheCap() async {
        call.suspendStartCall = true
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await waitForConnectToSuspend()

        await controller.handleCapExpiry()
        XCTAssertEqual(
            controller.state,
            .connecting,
            "deferred: a wake in flight is the opposite of the idle microphone the cap leashes"
        )

        call.finishStartCall()
        await controller.settle()
        XCTAssertEqual(controller.state, .conversing(.listening), "the conversation the wake bought still happens")

        call.emitLifecycle(.ended(.wentQuiet))
        await controller.settle()

        XCTAssertEqual(controller.state, .off, "the deferred cap collects at the conversation's end instead of returning the window")
        XCTAssertFalse(mic.isRunning)
        XCTAssertNil(AmbientArm.claim())
        XCTAssertEqual(activity.lifecycleCalls.last, .end(reason: AmbientEndedReason.capReached))
    }

    /// The latch arms in `.heard` too — before any socket exists — which is why
    /// it is named for the WAKE rather than the connect: the boundary can land
    /// in the gap between the hit and the handoff, and tearing down there is
    /// the same "Starting conversation" followed by silence. A `.heard` latch is honoured
    /// exactly like a `.connecting` one.
    func testACapFiringWhileHeardLatchesBeforeAnySocketExists() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()

        // The hit moved the state synchronously; the handoff is still queued,
        // so the boundary lands with no socket in existence yet.
        await controller.handleCapExpiry()
        XCTAssertEqual(
            controller.state,
            .heard(phrase: "hey sam"),
            "the cap must defer to a wake it only just heard"
        )

        await controller.settle()
        XCTAssertEqual(controller.state, .conversing(.listening), "the deferred wake still connects")

        call.emitLifecycle(.ended(.wentQuiet))
        await controller.settle()

        XCTAssertEqual(controller.state, .off)
        XCTAssertEqual(activity.lifecycleCalls.last, .end(reason: AmbientEndedReason.capReached))
    }

    /// A fresh window owes nothing for the last one's failed connect or
    /// deferred cap — the doctrine `arm` and the latch both state, pinned
    /// behaviourally: the notice must not lead the new window's caption, and
    /// the latch must not make a later window pay a cap no boundary of its own
    /// reached.
    func testAFreshWindowStartsCleanOfTheLastWindowsNoticeAndLatch() async {
        controller.resumeCooldown = 0
        // Window one earns the notice — both attempts fail — and is disarmed
        // with the sentence still standing.
        call.failToStart = true
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()
        XCTAssertEqual(
            captionRecords.last,
            .caption(AmbientController.connectFailedNotice, role: nil),
            "precondition: window one closed over a standing notice"
        )
        await controller.disarm(reason: nil)

        // Window two earns the latch — the cap lands in `.heard` — and is
        // disarmed with the latch still set.
        call.failToStart = false
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.handleCapExpiry()
        XCTAssertEqual(controller.state, .heard(phrase: "hey sam"), "precondition: window two's latch is set")
        await controller.disarm(reason: nil)
        await controller.settle()

        // Window three inherits neither: it opens on a blank caption, and a
        // conversation that ends returns it to armed.
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        XCTAssertEqual(
            captionRecords.last,
            .caption("", role: nil),
            "a fresh window opens blank, not on its predecessor's apology"
        )
        spotter.simulateHit()
        await controller.settle()
        call.emitLifecycle(.ended(.wentQuiet))
        await controller.settle()

        XCTAssertEqual(controller.state, .armed, "window three owes nothing for window two's deferred cap")
        XCTAssertTrue(mic.isRunning)
        XCTAssertNotNil(AmbientArm.claim())
    }

    // MARK: - relaunch

    /// Ambient is a local tap that dies with the process, so a surviving record
    /// is garbage to collect and never a session to resume. See `AmbientArmTests`.
    func testALaunchWithAStaleArmClearsItRatherThanAdoptingIt() {
        AmbientArm(armedAt: Date(), capSeconds: 7_200, ownerID: "a-process-that-is-gone").save()

        controller.reconcileOnLaunch()

        XCTAssertNil(AmbientArm.claim())
        XCTAssertEqual(controller.state, .off, "a stale record must never be adopted back into an armed window")
        XCTAssertFalse(mic.isRunning)
        XCTAssertEqual(mic.startCount, 0)
        XCTAssertTrue(activity.calls.contains(.endOrphans), "an activity can outlive its process; the orb has to be collected too")
    }

    /// An orphaned orb is not conditional on an arm record surviving alongside
    /// it: the activity lives in the system's hands and a termination between
    /// the two writes leaves one with no record at all. Left there it claims to
    /// be listening when nothing is, and offers a disarm control for a window
    /// that no longer exists.
    func testALaunchWithNoArmRecordStillCollectsAnOrphanedOrb() {
        XCTAssertNil(AmbientArm.claim(), "precondition: nothing for the sweep to key off")

        controller.reconcileOnLaunch()

        XCTAssertEqual(activity.lifecycleVerbs, [.endOrphans])
        XCTAssertEqual(controller.state, .off)
        XCTAssertFalse(mic.isRunning)
    }

    /// The sweep must not collect the caller's OWN live window. Nothing ambient
    /// can be live at launch, so this cannot happen on the real path — which is
    /// exactly why the guard needs a test rather than a comment.
    func testReconcilingWhileArmedDoesNotEndTheLiveOrb() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        controller.reconcileOnLaunch()

        XCTAssertEqual(activity.lifecycleVerbs, [.start], "the live orb is the disarm control; the sweep must leave it alone")
        XCTAssertEqual(controller.state, .armed)
        XCTAssertTrue(mic.isRunning)
        XCTAssertNotNil(AmbientArm.claim())
    }

    // MARK: - cross-process disarm

    /// The orb's Disarm button, arriving from the widget process. It has to stop
    /// the tap for real — not set a flag someone else acts on later — because the
    /// acknowledgement it writes afterwards is what licenses the intent to take
    /// the orb down.
    func testTheDisarmSignalStopsTheWindowAndThenAcknowledges() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        let request = AmbientSignal.requestDisarm()
        // "And then" is the whole semantics, and an assertion taken after the call
        // returns cannot see it: an implementation that acknowledges FIRST passes
        // every assertion below identically. So the reading is taken from inside
        // the stop, the same move `testAWakeHitStopsSpottingBeforeTheCallStarts`
        // makes from inside `startCall`.
        var acknowledgedAtTheMomentTheMicStopped: Bool?
        mic.onStop = { [store] in
            acknowledgedAtTheMomentTheMicStopped = store.string(forKey: "ambient.disarmAck") != nil
        }

        await controller.handleDisarmSignal()

        XCTAssertEqual(
            acknowledgedAtTheMomentTheMicStopped,
            false,
            "the acknowledgement is read as proof the microphone is off; written before the stop it is only a promise"
        )

        XCTAssertEqual(controller.state, .off)
        XCTAssertFalse(mic.isRunning, "the acknowledgement claims the microphone is off, so it has to be")
        XCTAssertNil(AmbientArm.claim())
        XCTAssertEqual(VoiceCallAudioFocus.shared.isActive, focusBaseline)
        XCTAssertEqual(activity.lifecycleCalls.last, .end(reason: nil), "the user's own disarm needs no explanation")
        XCTAssertTrue(
            AmbientSignal.consumeAcknowledgement(of: request),
            "without this the intent cannot tell a stopped microphone from a dead app"
        )
        XCTAssertNil(AmbientSignal.pendingDisarm(), "the request was answered; it must not disarm the next window too")
    }

    /// The delivered notification IS the command. The record travels by a slower
    /// route and may not be readable yet, and gating the microphone on it would
    /// mean a tap that visibly did nothing until the user next opened the app.
    /// Only the acknowledgement is lost, which the intent already handles.
    func testTheDisarmSignalStopsTheWindowEvenWithNoRecordToRead() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        XCTAssertNil(AmbientSignal.pendingDisarm(), "precondition: the record has not arrived")

        await controller.handleDisarmSignal()

        XCTAssertEqual(controller.state, .off)
        XCTAssertFalse(mic.isRunning)
        XCTAssertNil(AmbientArm.claim())
    }

    /// A tap on an orb whose window is already gone. Nothing is armed in this
    /// process, so nothing is armed anywhere — but the orb the user tapped is
    /// still on screen, and leaving it there is the same lie inverted.
    func testARequestWithNothingArmedCollectsTheOrphanedOrbAndOpensNoWindow() async {
        let request = AmbientSignal.requestDisarm()

        await controller.consumePendingDisarmRequest()

        XCTAssertEqual(controller.state, .off)
        XCTAssertEqual(mic.startCount, 0, "a disarm must never be the thing that opens a window")
        XCTAssertFalse(mic.isRunning)
        XCTAssertEqual(activity.lifecycleVerbs, [.endOrphans])
        XCTAssertTrue(AmbientSignal.consumeAcknowledgement(of: request), "nothing is listening, and the orb is gone")
    }

    /// The resume backstop runs on every foreground, so the *absence* of a request
    /// is the common case by far. Acting on it would disarm the user's live window
    /// every time they glanced at the app.
    func testAResumeWithNoRequestLeavesTheArmedWindowAlone() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        await controller.consumePendingDisarmRequest()

        XCTAssertEqual(controller.state, .armed)
        XCTAssertTrue(mic.isRunning)
        XCTAssertNotNil(AmbientArm.claim())
        XCTAssertEqual(activity.lifecycleVerbs, [.start], "the live orb is the disarm control; nothing may end it here")
    }

    /// The resume route, where the record is the only evidence a tap happened.
    func testAPendingRequestFoundOnResumeDisarmsTheWindow() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        let request = AmbientSignal.requestDisarm()

        await controller.consumePendingDisarmRequest()

        XCTAssertEqual(controller.state, .off)
        XCTAssertFalse(mic.isRunning)
        XCTAssertTrue(AmbientSignal.consumeAcknowledgement(of: request))
    }

    /// Registered for exactly as long as there is a window to disarm.
    func testTheDisarmObserverIsRegisteredWhileArmedAndReleasedOnDisarm() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        XCTAssertTrue(AmbientSignal.isObservingDisarm, "an armed microphone with no way to hear the orb is the whole failure")

        await controller.disarm(reason: nil)

        XCTAssertFalse(AmbientSignal.isObservingDisarm)
    }

    /// The unwind path leaves no listener behind either. It is the one teardown
    /// that does not go through `disarm`, which is exactly how it grows holes.
    func testAWindowUnwoundByAFailedOrbReleasesTheDisarmObserver() async {
        activity.failToStart = true

        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        XCTAssertFalse(AmbientSignal.isObservingDisarm)
    }

    /// A request left by a tap on an orb whose process was already dead. Nobody is
    /// waiting for it, and it must not survive to disarm the window the user arms
    /// next — a stale request outliving its question is the same class of garbage
    /// as a stale arm record.
    func testALaunchClearsARequestLeftByADeadProcess() async {
        AmbientSignal.requestDisarm()

        controller.reconcileOnLaunch()

        XCTAssertNil(AmbientSignal.pendingDisarm())
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        XCTAssertEqual(controller.state, .armed, "the new window must not inherit the old window's disarm")
        XCTAssertTrue(mic.isRunning)
    }

    // MARK: - interleaving

    /// Parks the handoff inside `startCall` so main-actor work can run in the
    /// window the sequential tests cannot reach.
    private func waitForConnectToSuspend() async {
        for _ in 0 ..< 500 where call.startCallCount == 0 { await Task.yield() }
        XCTAssertEqual(call.startCallCount, 1, "precondition: the handoff is parked inside startCall")
    }

    /// `handleWake` enqueues the handoff rather than inlining it, so main-actor
    /// work already queued — the orb's disarm intent — runs first.
    func testDisarmBetweenTheWakeHitAndTheHandoffLeavesTheWindowClosed() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        spotter.simulateHit()
        await controller.disarm(reason: nil)
        await controller.settle()

        XCTAssertEqual(controller.state, .off)
        XCTAssertEqual(call.startCallCount, 0, "no call may start for a window that is already closed")
        XCTAssertFalse(mic.isRunning)
    }

    /// The Critical. `disarm` can run start to finish inside `startCall`'s await
    /// — and can need no user at all: the battery rails disarm on their own.
    /// (The cap timer no longer does from here — it defers to a wake in flight,
    /// which its own tests pin.) A failed connect afterwards would restart the
    /// tap with no orb, no arm record, no cap timer and no audio-focus token: a
    /// microphone nothing will ever stop, which chat auto-speak later kills
    /// silently.
    func testAFailedConnectAfterDisarmDoesNotRestartTheTap() async {
        call.suspendStartCall = true
        call.failToStart = true
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await waitForConnectToSuspend()

        await controller.disarm(reason: "Listening window ended.")
        call.finishStartCall()
        await controller.settle()

        XCTAssertEqual(controller.state, .off)
        XCTAssertFalse(mic.isRunning, "the tap must not come back for a window that was disarmed mid-connect")
        XCTAssertNil(AmbientArm.claim())
        XCTAssertEqual(VoiceCallAudioFocus.shared.isActive, focusBaseline)
        XCTAssertEqual(activity.lifecycleCalls.last, .end(reason: "Listening window ended."))
    }

    /// The mirror: a connect that SUCCEEDS after the window closed leaves a live
    /// realtime call behind. `disarm` issued its `endCall` before the socket came
    /// up and may have found nothing to end, so the handoff owes the second one.
    func testASuccessfulConnectAfterDisarmEndsTheCallItInherited() async {
        call.suspendStartCall = true
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await waitForConnectToSuspend()

        await controller.disarm(reason: nil)
        let endsAtDisarm = call.endCount
        call.finishStartCall()
        await controller.settle()

        XCTAssertEqual(controller.state, .off, "a connect landing after the window closed must not reopen it")
        XCTAssertGreaterThan(call.endCount, endsAtDisarm, "the call that came up inside the await is ours to end")
        XCTAssertFalse(mic.isRunning)
        XCTAssertNil(AmbientArm.claim())
    }

    /// A turn arriving during connect legitimately moves the state on, so the
    /// still-ours check must treat `.conversing` as live. Reading it as "not
    /// mine any more" would end a call that had only just come up.
    func testATurnDuringConnectDoesNotEndTheCall() async {
        call.onStartCall = { [weak call] in call?.emit(.thinking) }

        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()

        XCTAssertEqual(controller.state, .conversing(.thinking))
        XCTAssertEqual(call.endCount, 0, "the call is live; only a closed window ends it")
    }

    // MARK: - recovery

    /// Every failure path lands in `.recoverableError` and the captions tell the
    /// user to try again, so trying again has to work.
    func testArmingAgainAfterAFailureIsAllowed() async {
        mic.startError = FakeAmbientMicSource.StartFailure.unavailable
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        guard case .recoverableError = controller.state else {
            return XCTFail("precondition: expected an error, got \(controller.state)")
        }

        mic.startError = nil
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        XCTAssertEqual(controller.state, .armed, "the caption says 'try again', so a retry must actually arm")
        XCTAssertTrue(mic.isRunning)
        XCTAssertNotNil(AmbientArm.claim())
    }

    /// A tap that started can still die — an incoming call, a route change, a
    /// permission revoked mid-window. Left unhandled the state stays `.armed`
    /// and the orb keeps claiming the user is heard while nothing reaches the
    /// spotter: the same lie as an armed mic with no orb, from the other side.
    func testAMicrophoneThatDiesMidWindowDisarmsInsteadOfLyingAboutListening() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        XCTAssertTrue(mic.isRunning)

        mic.simulateFailure()
        for _ in 0 ..< 400 where controller.state != .off {
            try? await Task.sleep(nanoseconds: 5_000_000)
        }

        XCTAssertEqual(controller.state, .off, "a dead tap must not read as a live window")
        XCTAssertNil(AmbientArm.claim())
        XCTAssertEqual(VoiceCallAudioFocus.shared.isActive, focusBaseline)
        XCTAssertEqual(activity.lifecycleCalls.last, .end(reason: "Lost the microphone."))
    }

    /// The other route into `.recoverableError`. Distinct from the mic route
    /// because `.recoverableError → .arming` is a `(.some, nil)` transition in
    /// `publishIfNeeded` — safe only because `orbIsLive` is false, having never
    /// been set by the refused request.
    func testArmingAgainAfterAnOrbFailureIsAllowed() async {
        activity.failToStart = true
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        guard case .recoverableError = controller.state else {
            return XCTFail("precondition: expected an orb refusal, got \(controller.state)")
        }

        activity.failToStart = false
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        XCTAssertEqual(controller.state, .armed)
        XCTAssertTrue(mic.isRunning)
        XCTAssertEqual(
            activity.lifecycleVerbs,
            [.start, .start],
            "the refused request must not have produced an end for an activity that never existed"
        )
    }

    /// `windowIsLive` asked "is A window live", which a LATER window satisfies.
    /// Reachable with ordinary use: the cap disarms, the user re-arms and
    /// speaks, and window one's connect is still parked on a slow socket. The
    /// stale handoff must not touch the window that replaced it — its failure
    /// path would drop window two's turn subscription and put the spotting tap
    /// back while a call already owns the microphone, breaking "exactly one path
    /// owns the microphone at any instant".
    func testAStaleHandoffDoesNotDisturbTheWindowThatReplacedIt() async {
        call.suspendStartCall = true
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await waitForConnectToSuspend()
        await controller.disarm(reason: "Listening window ended.")

        // Window two, armed and conversing while window one is still parked.
        call.suspendStartCall = false
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()
        XCTAssertEqual(controller.state, .conversing(.listening), "precondition: window two is live")
        let endsBeforeStaleReturn = call.endCount

        // Window one's socket finally answers — and fails, which is the path
        // that would have restarted the tap under window two's live call.
        call.failToStart = true
        call.finishStartCall()
        for _ in 0 ..< 200 { await Task.yield() }

        XCTAssertEqual(controller.state, .conversing(.listening), "window two's conversation must be untouched")
        XCTAssertFalse(mic.isRunning, "a stale handoff must not put the spotting tap back under a live call")
        XCTAssertEqual(call.endCount, endsBeforeStaleReturn, "the stale handoff must not hang up the call that replaced it")
        XCTAssertNotNil(AmbientArm.claim())
    }

    /// The mirror of the test above, and the case that pins the `endCall`
    /// narrowing rather than the `resumeSpotting` one: window one's connect
    /// SUCCEEDS after being replaced. Ending "the call we opened" is right when
    /// the window simply closed, but here the sink has since been taken over, so
    /// the call that answered is window two's — hanging it up would cut off a
    /// live conversation.
    func testAStaleHandoffThatConnectsDoesNotHangUpTheReplacingWindow() async {
        call.suspendStartCall = true
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await waitForConnectToSuspend()
        await controller.disarm(reason: "Listening window ended.")

        call.suspendStartCall = false
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()
        call.emit(.speaking)
        XCTAssertEqual(controller.state, .conversing(.speaking), "precondition: window two is mid-conversation")
        let endsBeforeStaleReturn = call.endCount

        // Window one's socket answers, successfully, long after it was replaced.
        call.finishStartCall()
        for _ in 0 ..< 200 { await Task.yield() }

        XCTAssertEqual(
            call.endCount,
            endsBeforeStaleReturn,
            "the sink now belongs to window two; ending 'our' call would hang up a live conversation"
        )
        XCTAssertEqual(controller.state, .conversing(.speaking), "window two's turn must survive its predecessor landing")
        XCTAssertFalse(mic.isRunning)
    }

    // MARK: - the end of a conversation
    //
    // The route that makes an armed window worth arming. Without it the window
    // holds exactly one conversation and then sits in `.conversing` until the cap
    // expires — strictly worse than the tap-to-talk it replaced, because the whole
    // justification for a wake word is talking repeatedly without touching
    // anything.

    /// Park a controller mid-conversation, which is the precondition for
    /// everything in this section.
    private func armAndConverse() async {
        controller.resumeCooldown = 0
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()
        XCTAssertEqual(controller.state, .conversing(.listening), "precondition: mid-conversation")
        XCTAssertFalse(mic.isRunning, "precondition: the call owns the microphone, not the spotter")
    }

    /// **The headline.** A conversation that goes quiet hands the window back to
    /// the wake word, with the microphone the spotter's again.
    func testAConversationThatGoesQuietReturnsTheWindowToArmed() async {
        await armAndConverse()

        call.emitLifecycle(.ended(.wentQuiet))
        await controller.settle()

        XCTAssertEqual(controller.state, .armed)
        XCTAssertTrue(mic.isRunning, "the spotting tap must come back, or the window is spent")
        XCTAssertEqual(mic.startCount, 2)
        XCTAssertEqual(call.endCount, 1, "the CONTROLLER hangs up on an end the sink only reported")
        XCTAssertNotNil(AmbientArm.claim(), "the window is still the user's")
        XCTAssertEqual(activity.lifecycleVerbs.last, .update, "the orb goes back to armed, it does not end")
    }

    /// A FOREGROUNDED re-arm runs session activation, so its first failure can
    /// be a real transient that settles out by itself — one failed `mic.start`
    /// must not spend a window the user armed on purpose.
    func testATransientForegroundedRearmFailureIsRetriedAndTheWindowSurvives() async {
        await armAndConverse()
        controller.isAppForegrounded = { true }
        mic.scriptedStartErrors = [FakeAmbientMicSource.StartFailure.unavailable]

        call.emitLifecycle(.ended(.wentQuiet))
        await controller.settle()

        XCTAssertEqual(controller.state, .armed, "a transient must not cost the window")
        XCTAssertTrue(mic.isRunning, "the retry put the tap back")
        XCTAssertEqual(mic.startCount, 3, "the arm, the failed re-arm, and the retry that landed")
        XCTAssertNotNil(AmbientArm.claim(), "the window is still the user's")
    }

    /// The retries are bounded and the final disarm is kept: a microphone that
    /// stays gone is still a dead window, and the orb may claim `armed` over no
    /// tap only for the two short foregrounded retries — never indefinitely.
    func testAPersistentForegroundedRearmFailureStillDisarmsWithTheHonestReason() async {
        await armAndConverse()
        controller.isAppForegrounded = { true }
        mic.startError = FakeAmbientMicSource.StartFailure.unavailable

        call.emitLifecycle(.ended(.wentQuiet))
        await controller.settle()

        XCTAssertEqual(controller.state, .off)
        XCTAssertEqual(mic.startCount, 4, "the arm, then three re-arm attempts — two retries and no more")
        XCTAssertEqual(activity.lifecycleCalls.last, .end(reason: "Lost the microphone."))
    }

    /// Backgrounded, a retry cannot differ from the attempt it repeats: session
    /// activation is skipped by design there, so a failed start is StartIO
    /// refusing an inactive session — and the 0.1.158 device trace showed the
    /// futile rounds stalling the main actor for visible seconds. One attempt,
    /// then the honest disarm, exactly the pre-retry behaviour.
    func testABackgroundedRearmFailureDisarmsOnTheFirstThrow() async {
        await armAndConverse()
        controller.isAppForegrounded = { false }
        mic.startError = FakeAmbientMicSource.StartFailure.unavailable

        call.emitLifecycle(.ended(.wentQuiet))
        await controller.settle()

        XCTAssertEqual(controller.state, .off)
        XCTAssertEqual(mic.startCount, 2, "the arm and the one refused re-arm — a backgrounded retry is futile")
        XCTAssertEqual(activity.lifecycleCalls.last, .end(reason: "Lost the microphone."))
    }

    /// A revoked permission is the user's answer, not a transient: retrying it
    /// would nag a microphone the user just turned off, so the first throw
    /// disarms with the reason that says what to do about it — even where the
    /// foreground would otherwise license retries.
    func testAPermissionFailureOnRearmDisarmsImmediatelyWithoutRetries() async {
        await armAndConverse()
        controller.isAppForegrounded = { true }
        mic.startError = AmbientMicFailure.recordPermissionMissing

        call.emitLifecycle(.ended(.wentQuiet))
        await controller.settle()

        XCTAssertEqual(controller.state, .off)
        XCTAssertEqual(mic.startCount, 2, "the arm and the one refused re-arm — no retry may follow")
        XCTAssertEqual(activity.lifecycleCalls.last, .end(reason: AmbientController.permissionRevokedMessage))
    }

    /// The follow-up window is the user's licence to continue WITHOUT the
    /// activation phrase, and the controller renders the sink's deadline rather
    /// than running a competing timer.
    func testTheFollowUpWindowShowsAsCooldownWithTheSinksOwnDeadline() async {
        await armAndConverse()
        let deadline = Date().addingTimeInterval(8)

        call.emitLifecycle(.quiet(until: deadline))

        XCTAssertEqual(controller.state, .cooldown(until: deadline))
        XCTAssertEqual(
            controller.state.orbPhase,
            .listening,
            "the microphone is still open for the user, which is not the same as resting"
        )
        XCTAssertEqual(mic.startCount, 1, "the spotting tap must NOT come back while the call still has the mic")
    }

    /// A pushed-out deadline is carried through, because the sink owns the one
    /// timer: a controller that kept the first deadline would count down to a
    /// moment that had already moved.
    func testAPushedOutFollowUpDeadlineIsAdopted() async {
        await armAndConverse()
        let first = Date().addingTimeInterval(8)
        let later = first.addingTimeInterval(8)

        call.emitLifecycle(.quiet(until: first))
        call.emitLifecycle(.quiet(until: later))

        XCTAssertEqual(controller.state, .cooldown(until: later))
    }

    /// The other half of the follow-up window: speaking again inside it is a
    /// conversation turn, not a stale hop from a window that closed. This is what
    /// `callPhaseIsActive` including `.cooldown` buys.
    func testAFollowUpTurnInsideTheCooldownReturnsToConversing() async {
        await armAndConverse()
        call.emitLifecycle(.quiet(until: Date().addingTimeInterval(8)))

        call.emit(.thinking)

        XCTAssertEqual(controller.state, .conversing(.thinking))
    }

    /// A server-ended session ends the CONVERSATION, not the window. An ambient
    /// window is a local microphone lease with no server-side existence, and the
    /// wake spotter is entirely on-device, so nothing the backend does is evidence
    /// about whether the user still wants to be heard.
    func testAServerEndedConversationReturnsToArmedRatherThanDisarming() async {
        await armAndConverse()

        call.emitLifecycle(.ended(.remote))
        await controller.settle()

        XCTAssertEqual(controller.state, .armed)
        XCTAssertTrue(mic.isRunning)
        XCTAssertNotNil(AmbientArm.claim())
    }

    /// Design §9 decided this one explicitly — *"on exhaustion fall back to
    /// `.armed`, not `.off`, so the wake word survives"* — and it is the same
    /// disposition the handoff already gives a connect that never came up.
    func testADroppedConversationReturnsToArmedSoTheWakeWordSurvives() async {
        await armAndConverse()

        call.emitLifecycle(.ended(.dropped))
        await controller.settle()

        XCTAssertEqual(controller.state, .armed)
        XCTAssertTrue(mic.isRunning)
        XCTAssertNotNil(AmbientArm.claim())
    }

    /// **The whole leg this task exists for**: arm, wake, converse, end, resume,
    /// wake AGAIN. One window, two conversations, no phone touched in between.
    func testOneWindowHoldsASecondConversation() async {
        await armAndConverse()
        call.emitLifecycle(.ended(.wentQuiet))
        await controller.settle()
        XCTAssertEqual(controller.state, .armed, "precondition: the first conversation returned the window")

        spotter.simulateHit()
        await controller.settle()

        XCTAssertEqual(controller.state, .conversing(.listening), "the wake word must work a second time")
        XCTAssertEqual(call.startCallCount, 2, "a second conversation")
        XCTAssertFalse(mic.isRunning, "and the call owns the microphone again")
        XCTAssertEqual(
            spotter.resetCount,
            2,
            "one reset per stretch of spotting — the arm and the resume — so the second stretch starts from a fresh decoder"
        )
    }

    /// `.ended` is terminal and at most once. A `.dropped` chasing a
    /// `.wentQuiet` down the same teardown must not start a SECOND resume: two
    /// overlapping resumes are two attempts to reinstall one tap.
    func testASecondEndForTheSameConversationDoesNotStartASecondResume() async {
        await armAndConverse()

        call.emitLifecycle(.ended(.wentQuiet))
        call.emitLifecycle(.ended(.dropped))
        await controller.settle()

        XCTAssertEqual(controller.state, .armed)
        XCTAssertEqual(mic.startCount, 2, "one resume, not two")
        XCTAssertEqual(call.endCount, 1)
    }

    /// **The loop the seam contract forbids, tested rather than trusted.** The
    /// sink must never report a hangup the controller asked for — because the
    /// controller answers an end by putting the spotting tap back, so a disarm
    /// that reported its own `endCall` would restore a microphone the user had
    /// just stopped, with no orb, no arm record and no cap timer left to stop it
    /// again.
    func testAnEndReportedForTheControllersOwnHangUpNeverBringsTheTapBack() async {
        call.emitEndedAfterHangUp = true
        await armAndConverse()

        await controller.disarm(reason: nil)
        await controller.settle()
        for _ in 0 ..< 200 { await Task.yield() }

        XCTAssertEqual(controller.state, .off)
        XCTAssertFalse(mic.isRunning, "a microphone the user just stopped must stay stopped")
        XCTAssertEqual(mic.startCount, 1)
        XCTAssertNil(AmbientArm.claim())
        XCTAssertEqual(VoiceCallAudioFocus.shared.isActive, focusBaseline)
    }

    /// The lifecycle publisher does not replay and the follow-up window is armed
    /// INSIDE `startCall`, so the first `.quiet` of every conversation is emitted
    /// before `startCall` returns. Applied before the handoff's default turn it
    /// would be overwritten; this pins the ordering that keeps it.
    func testAQuietEmittedWhileConnectingSurvivesTheDefaultTurn() async {
        let deadline = Date().addingTimeInterval(8)
        call.onStartCall = { [weak call] in call?.emitLifecycle(.quiet(until: deadline)) }

        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()

        XCTAssertEqual(
            controller.state,
            .cooldown(until: deadline),
            "the follow-up window that opened during the connect must not be clobbered by conversing(.listening)"
        )
    }

    /// And the same for an end. A conversation the server refuses the instant it
    /// opens must not be described as live for the rest of the window.
    func testAnEndEmittedWhileConnectingStillReturnsTheWindow() async {
        controller.resumeCooldown = 0
        call.onStartCall = { [weak call] in call?.emitLifecycle(.ended(.remote)) }

        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()
        for _ in 0 ..< 200 { await Task.yield() }

        XCTAssertEqual(controller.state, .armed)
        XCTAssertTrue(mic.isRunning, "the wake word must survive a conversation that ended on arrival")
        XCTAssertNotNil(AmbientArm.claim())
    }

    /// The interleaving case for the new lifecycle pair: an end delivered while
    /// the resume it started is still parked in the cooldown, with a disarm in
    /// between. The tap must not come back for a window that closed.
    func testADisarmDuringTheResumeAfterAConversationDoesNotBringTheTapBack() async {
        controller.resumeCooldown = 600
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()
        XCTAssertEqual(controller.state, .conversing(.listening), "precondition: mid-conversation")

        call.emitLifecycle(.ended(.wentQuiet))
        for _ in 0 ..< 1_000 where mic.startCount < 2 { await Task.yield() }
        XCTAssertTrue(mic.isRunning, "precondition: active I/O bridges the resume cooldown")
        XCTAssertEqual(spotter.resetCount, 1, "precondition: wake decoding remains gated")

        await controller.disarm(reason: "Listening window ended.")
        await controller.settle()

        XCTAssertEqual(controller.state, .off)
        XCTAssertFalse(mic.isRunning)
        XCTAssertEqual(mic.startCount, 2)
        XCTAssertNil(AmbientArm.claim())
        XCTAssertEqual(VoiceCallAudioFocus.shared.isActive, focusBaseline)
    }

    /// And the mirror: a LATER window taking over while the post-conversation
    /// resume is still parked. "Exactly one path owns the microphone at any
    /// instant" is the invariant the whole design rests on.
    func testAResumeAfterAConversationDoesNotDisturbTheWindowThatReplacedIt() async {
        controller.resumeCooldown = 600
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()
        call.emitLifecycle(.ended(.wentQuiet))
        for _ in 0 ..< 200 { await Task.yield() }
        await controller.disarm(reason: nil)

        controller.resumeCooldown = 0
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()
        XCTAssertEqual(controller.state, .conversing(.listening), "precondition: window two is live")

        for _ in 0 ..< 300 { await Task.yield() }

        XCTAssertEqual(controller.state, .conversing(.listening), "window two's conversation must be untouched")
        XCTAssertFalse(mic.isRunning, "a stale resume must not put the spotting tap back under a live call")
    }

    /// **The one hole the `.armed`-during-the-resume-cooldown choice opened.**
    /// `handleWake` REPLACES `pending`, so a hit landing in that 2.5 s would
    /// discard the resume that was about to reinstall the tap and leave the window
    /// `.armed` with nothing listening until the cap expired — the orb asserting
    /// something untrue.
    ///
    /// Unreachable in production, because the running tap's frames are gated away
    /// from `ingest`'s spotter feed until the cooldown ends. The test drives the
    /// double's hit callback directly past that guarantee on purpose: the point is
    /// that the state machine rejects it too.
    func testAWakeHitDuringThePostConversationResumeIsIgnoredAndTheResumeStillLands() async {
        controller.resumeCooldown = 0.01
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await controller.settle()
        call.emitLifecycle(.ended(.wentQuiet))
        XCTAssertEqual(controller.state, .armed, "precondition: armed, but the tap is not back yet")
        for _ in 0 ..< 1_000 where mic.startCount < 2 { await Task.yield() }
        XCTAssertTrue(mic.isRunning, "precondition: audio I/O remains active during the resume cooldown")
        mic.emit(Data([1, 2, 3, 4]))
        XCTAssertEqual(spotter.fedByteCount, 0, "precondition: the decoder is still gated")

        spotter.simulateHit()
        await controller.settle()
        for _ in 0 ..< 300 { await Task.yield() }

        XCTAssertEqual(call.startCallCount, 1, "a hit while the wake decoder is gated must not open a conversation")
        XCTAssertEqual(controller.state, .armed)
        XCTAssertTrue(mic.isRunning, "and the resume it would have discarded must still land")
    }

    /// A wake hit inside the follow-up window is redundant — the user does not
    /// need the phrase there — and the microphone belongs to the call, so it must
    /// not start a second handoff.
    func testAWakeHitInsideTheCooldownIsIgnored() async {
        await armAndConverse()
        call.emitLifecycle(.quiet(until: Date().addingTimeInterval(8)))

        spotter.simulateHit()
        await controller.settle()

        XCTAssertEqual(call.startCallCount, 1)
    }

    // MARK: - the audio-graph prime
    //
    // The evidence-backed first-wake cure (0.1.160): the once-per-process VP
    // build AND the first VP stop must be spent at arm, foregrounded, before
    // the spotter owns the microphone — never inside a backgrounded wake. The
    // fake's hook is what makes the ordering visible.

    /// The prime runs once, at arm, and strictly BEFORE the spotter's start —
    /// the whole point is that its engine cycle finishes before anything is
    /// listening, so its first-VP-stop activation lapse lands where the very
    /// next start (the spotter's, foregrounded) repairs it.
    func testArmPrimesTheAudioGraphOnceBeforeTheSpotterStarts() async {
        controller.isAppForegrounded = { true }
        // The prime's guard reads the REAL focus singleton, so a token leaked
        // by any earlier test would (correctly) defer the prime and fail the
        // counts below for the wrong-looking reason. Named here so it fails
        // loudly as a leak, not quietly as a prime bug.
        XCTAssertFalse(VoiceCallAudioFocus.shared.isActive, "precondition: no call may hold audio focus")
        var micWasRunningAtPrime: Bool?
        call.onPrimeAudioGraph = { [weak mic] in micWasRunningAtPrime = mic?.isRunning }

        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        XCTAssertEqual(call.primeAudioGraphCount, 1)
        XCTAssertEqual(micWasRunningAtPrime, false, "the prime must finish before the spotter owns the microphone")

        await controller.disarm(reason: nil)
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        XCTAssertEqual(call.primeAudioGraphCount, 1, "once per process — the first-run build it spends only exists once")
    }

    /// `arm` is foregrounded by contract at both doors; the guard re-checks
    /// because contracts drift — and a backgrounded refusal must not burn the
    /// one prime, or the process would face its first wake unprimed after all.
    func testABackgroundedArmDoesNotSpendThePrimeLatch() async {
        XCTAssertFalse(VoiceCallAudioFocus.shared.isActive, "precondition: no call may hold audio focus")
        controller.isAppForegrounded = { false }
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        XCTAssertEqual(call.primeAudioGraphCount, 0, "a backgrounded prime would lapse a session nothing can repair")

        await controller.disarm(reason: nil)
        controller.isAppForegrounded = { true }
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        XCTAssertEqual(call.primeAudioGraphCount, 1, "the refusal must not have spent the latch")
    }

    /// The intent door reaches `arm` while an in-app Live call runs, and a
    /// prime there would build a second VP-armed engine under running IO —
    /// the exact churn class being cured — then have its `.release` stop's
    /// deactivate refused busy. The call holds `VoiceCallAudioFocus` for its
    /// whole life, so held focus defers the prime WITHOUT spending the latch;
    /// the next call-free arm pays it normally.
    func testAnArmDuringALiveVoiceCallDefersThePrimeWithoutSpendingTheLatch() async {
        controller.isAppForegrounded = { true }
        // The real singleton, deliberately — it is the exact fact the guard
        // reads in production. Released explicitly below; the deferred release
        // is the belt (releasing an absent owner is documented as a no-op).
        let liveCall = VoiceCallAudioFocus.shared.acquire()
        defer { VoiceCallAudioFocus.shared.release(liveCall) }

        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        XCTAssertEqual(call.primeAudioGraphCount, 0, "a prime under a live call would churn a session its IO is riding")

        await controller.disarm(reason: nil)
        VoiceCallAudioFocus.shared.release(liveCall)
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        XCTAssertEqual(call.primeAudioGraphCount, 1, "the deferral must not have spent the latch")
    }

    /// A prime whose engine cycle THREW may have built nothing, so it must not
    /// spend the latch either — the trade is a foregrounded, fast-failing
    /// retry at each arm, which self-heals, where a latched failure is
    /// first-wake exposure until relaunch.
    func testAFailedPrimeRetriesAtTheNextArmAndSuccessSpendsTheLatch() async {
        controller.isAppForegrounded = { true }
        call.primeAudioGraphSucceeds = false
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        XCTAssertEqual(call.primeAudioGraphCount, 1, "the failing prime ran")

        await controller.disarm(reason: nil)
        call.primeAudioGraphSucceeds = true
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        XCTAssertEqual(call.primeAudioGraphCount, 2, "a failure must retry at the next arm rather than latch shut")

        await controller.disarm(reason: nil)
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        XCTAssertEqual(call.primeAudioGraphCount, 2, "the success spent the latch; the cycle only exists once")
    }

    /// The backgrounded start-failure class earns its own sentence: iOS
    /// declining to hand a backgrounded process the session back is not "the
    /// microphone broke", and its fix — open the app — is an instruction the
    /// generic reason never gave. Both shapes the investigation identified map
    /// to it; everything else keeps the reasons it had.
    func testTheBackgroundedStartFailureClassGetsItsOwnHonestReason() {
        XCTAssertEqual(
            AmbientController.micFailureReason(AmbientMicFailure.inputFormatInvalid),
            AmbientController.backgroundedMicLossMessage,
            "the degenerate format after a skipped activation is the backgrounded class"
        )
        let startIORefusal = NSError(domain: NSOSStatusErrorDomain, code: 2_003_329_396)
        XCTAssertEqual(
            AmbientController.micFailureReason(startIORefusal),
            AmbientController.backgroundedMicLossMessage,
            "AURemoteIO's StartIO 'what' against an inactive session is the same class"
        )
        XCTAssertEqual(
            AmbientController.micFailureReason(FakeAmbientMicSource.StartFailure.unavailable),
            "Lost the microphone.",
            "an unclassified failure keeps the generic reason"
        )
        XCTAssertEqual(
            AmbientController.micFailureReason(AmbientMicFailure.recordPermissionMissing),
            AmbientController.permissionRevokedMessage,
            "permission wording is unchanged"
        )
    }

    // MARK: - the phase-word pulse
    //
    // App-driven because nothing out-of-process can schedule: every show and
    // hide of the compact word is a publish. The show rides the transition
    // publish (pinned as the pure rule in `AmbientStateTests`); what these pin
    // is the cadence the controller speaks through `setPhaseWordVisible` — and
    // above all that no flip can land against a window that disarmed or a
    // phase that moved on. setUp disables the pulse suite-wide; each test here
    // sets its own real ratio.

    /// The hide is the pulse's first scheduled publish: it lands while the
    /// phase persists, and nothing re-shows before the cadence.
    func testThePhaseWordHidesAfterItsShowWindowWhileThePhasePersists() async {
        controller.phaseWordShowSeconds = 0.01
        controller.phaseWordCadenceSeconds = 60
        await armAndConverse()

        for _ in 0 ..< 200 where !activity.calls.contains(.phaseWord(visible: false)) {
            try? await Task.sleep(nanoseconds: 5_000_000)
        }

        XCTAssertTrue(activity.calls.contains(.phaseWord(visible: false)), "the hide publish must land — a word that never hides is not a pulse")
        XCTAssertFalse(activity.calls.contains(.phaseWord(visible: true)), "no re-show before the cadence comes around")
        XCTAssertEqual(controller.state, .conversing(.listening), "a flip is a publish, never a state change")
    }

    /// While the phase persists, the cadence brings the word back: hide, then
    /// re-show, on the injected clocks.
    func testThePhaseWordReshowsOnTheCadenceWhileThePhasePersists() async {
        controller.phaseWordShowSeconds = 0.01
        controller.phaseWordCadenceSeconds = 0.05
        await armAndConverse()

        for _ in 0 ..< 200 where !activity.calls.contains(.phaseWord(visible: true)) {
            try? await Task.sleep(nanoseconds: 5_000_000)
        }

        let flips = activity.calls.filter { if case .phaseWord = $0 { return true } else { return false } }
        XCTAssertEqual(flips.first, .phaseWord(visible: false), "the cycle is hide-then-reshow — the first show rode the transition publish")
        XCTAssertTrue(flips.contains(.phaseWord(visible: true)), "the cadence must bring the word back")
    }

    /// A phase change mid-window supersedes the running pulse: the old task is
    /// cancelled (and guarded besides — a stale flip re-checks the phase it
    /// was scheduled for), so the new phase's cycle produces exactly one hide,
    /// never a doubled or stale one.
    func testAPhaseChangeMidPulseSupersedesCleanly() async {
        controller.phaseWordShowSeconds = 0.05
        controller.phaseWordCadenceSeconds = 60
        await armAndConverse()

        call.emit(.thinking)
        XCTAssertEqual(controller.state, .conversing(.thinking), "precondition: the phase moved before the first hide")
        try? await Task.sleep(nanoseconds: 150_000_000)

        let hides = activity.calls.filter { $0 == .phaseWord(visible: false) }
        XCTAssertEqual(hides.count, 1, "one cycle, the new phase's — a second hide would be the superseded task speaking")
        XCTAssertEqual(controller.state, .conversing(.thinking))
    }

    /// Disarm cancels the pulse outright: no flip may publish against a window
    /// that is gone — same idiom as the connect and the resume.
    func testDisarmCancelsThePhaseWordPulseOutright() async {
        controller.phaseWordShowSeconds = 0.2
        controller.phaseWordCadenceSeconds = 60
        await armAndConverse()

        await controller.disarm(reason: nil)
        try? await Task.sleep(nanoseconds: 300_000_000)

        XCTAssertFalse(
            activity.calls.contains { if case .phaseWord = $0 { return true } else { return false } },
            "a dead window's orb must hear nothing more — the pending hide dies with the pulse"
        )
    }

    // MARK: - windowIsLive
    //
    // The one predicate every neighbouring audio path checks before it deactivates
    // the shared session (`AmbientRail`). It has to answer about the TAP, not
    // about the orb: `.recoverableError` reduces to the `armed` orb phase and is
    // reached by a window that never opened.

    func testWindowIsLiveTracksTheTapRatherThanTheOrb() async {
        XCTAssertFalse(controller.windowIsLive, "nothing armed")

        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        XCTAssertTrue(controller.windowIsLive)

        spotter.simulateHit()
        await controller.settle()
        XCTAssertTrue(controller.windowIsLive, "a conversation depends on the session exactly as much as spotting does")

        await controller.disarm(reason: nil)
        XCTAssertFalse(controller.windowIsLive)
    }

    /// The case that makes reading `armedAt` rather than the state load-bearing: a
    /// refused arm lands in `.recoverableError`, which the orb renders as `armed`.
    /// A `windowIsLive` derived from the state would tell every neighbour to leave
    /// the audio session alone for a window that never opened.
    func testWindowIsNotLiveAfterARefusedArm() async {
        spotter.isInert = true
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        XCTAssertFalse(mic.isRunning, "precondition: nothing was opened")
        XCTAssertEqual(controller.state.orbPhase, .armed, "precondition: the orb reduction still says armed")
        XCTAssertFalse(controller.windowIsLive)
    }

    // MARK: - windowIsOpen (the in-app controls' predicate)

    /// **The predicate the ambient control in Settings and `AmbientMiniBar` share.**
    /// It exists as one value precisely so a second copy cannot drift: it must NOT
    /// be `orbPhase != nil`, because that reduction maps `.recoverableError` onto
    /// `.armed` and every refused arm lands there — so a control keyed off the
    /// reduction offers to STOP a window that never opened.
    func testWindowIsOpenExcludesTheStatesWithNoWindow() {
        for state: AmbientState in [
            .armed, .heard(phrase: "hey sam"), .connecting,
            .conversing(.listening), .conversing(.thinking), .conversing(.speaking),
            .cooldown(until: Date())
        ] {
            XCTAssertTrue(state.windowIsOpen, "\(state) has a window")
        }
        for state: AmbientState in [
            .off, .arming, .disarming(reason: nil), .recoverableError(message: "nope")
        ] {
            XCTAssertFalse(state.windowIsOpen, "\(state) has no window")
        }
    }

    /// The specific trap, stated as its own case because it is the one an author
    /// re-introduces by reaching for the reduction.
    func testARefusedArmIsNotAWindowEvenThoughTheOrbReductionSaysArmed() async {
        spotter.isInert = true
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        XCTAssertEqual(controller.state.orbPhase, .armed)
        XCTAssertFalse(
            controller.state.windowIsOpen,
            "a control offering to stop this would be offering to stop nothing"
        )
    }

    /// It tracks the window across every phase of a real conversation, including the
    /// follow-up window — where the microphone is the call's and a Stop control must
    /// still work.
    func testWindowIsOpenTracksAWholeConversation() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        XCTAssertTrue(controller.state.windowIsOpen)

        spotter.simulateHit()
        await controller.settle()
        XCTAssertTrue(controller.state.windowIsOpen)

        call.emitLifecycle(.quiet(until: Date().addingTimeInterval(8)))
        XCTAssertTrue(controller.state.windowIsOpen)

        await controller.disarm(reason: nil)
        XCTAssertFalse(controller.state.windowIsOpen)
    }

    // MARK: - the Control Center nudge

    /// Once, ever, and only after a window actually opened. A refused arm must not
    /// spend the one showing — the user has not seen the feature work yet, so a nudge
    /// towards a faster way to do the thing that just failed is noise.
    func testTheControlCenterHintIsShownOnceAndOnlyAfterASuccessfulArm() {
        XCTAssertTrue(
            AmbientControlCenterHint.shouldShow(alreadyShown: false, armSucceeded: true)
        )
        XCTAssertFalse(
            AmbientControlCenterHint.shouldShow(alreadyShown: true, armSucceeded: true),
            "shown once, ever — re-raising it is the nagging this is written to avoid"
        )
        XCTAssertFalse(
            AmbientControlCenterHint.shouldShow(alreadyShown: false, armSucceeded: false),
            "a refused arm must not spend the one showing"
        )
        XCTAssertFalse(
            AmbientControlCenterHint.shouldShow(alreadyShown: true, armSucceeded: false)
        )
    }

    /// The copy has to name the control the user is being sent to look for, or the
    /// prompt is a chore with no findable target. Pinned against the widget's own
    /// label so renaming one breaks this rather than silently breaking the user.
    func testTheControlCenterHintNamesTheControl() {
        XCTAssertTrue(
            AmbientControlCenterHint.message.contains("Talk to Magican"),
            "the message must name the control exactly as MagiosAmbientControl labels it"
        )
        XCTAssertFalse(AmbientControlCenterHint.title.isEmpty)
        XCTAssertFalse(AmbientControlCenterHint.shownKey.isEmpty)
    }

    // MARK: - the session rails

    /// Dictation and observation both yield the window rather than being refused:
    /// a user holding their phone and pressing hold-to-talk wants the microphone,
    /// and design §9 already ranks observation above ambient. The orb's last frame
    /// says which of the user's own actions closed it.
    func testAForegroundCaptureYieldsTheWindowWithItsReasonOnTheOrb() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        controller.yieldForForegroundCapture(reason: AmbientYieldReason.dictationStarted)

        XCTAssertEqual(controller.state, .off)
        XCTAssertFalse(mic.isRunning)
        XCTAssertNil(AmbientArm.claim())
        XCTAssertEqual(VoiceCallAudioFocus.shared.isActive, focusBaseline)
        XCTAssertEqual(
            activity.lifecycleCalls.last,
            .end(reason: AmbientYieldReason.dictationStarted),
            "an orb that just vanished would leave the user with no idea what stopped listening"
        )
    }

    /// The overwhelmingly common call: every dictation and every observation start
    /// asks, and almost none of them finds a window.
    func testYieldingWithNothingArmedDoesNothing() async {
        controller.yieldForForegroundCapture(reason: AmbientYieldReason.observationStarted)

        XCTAssertEqual(controller.state, .off)
        XCTAssertTrue(activity.calls.isEmpty, "no orb may be ended for a window that never existed")
    }

    /// The interleaving case for the rail: it fires from a synchronous UI handler,
    /// so it can land inside a connect. `disarmNow` has no `await` in it, which is
    /// what makes "the microphone is off now" true rather than promised.
    func testAYieldDuringTheConnectLeavesNoCallRunning() async {
        call.suspendStartCall = true
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        spotter.simulateHit()
        await waitForConnectToSuspend()

        controller.yieldForForegroundCapture(reason: AmbientYieldReason.dictationStarted)
        XCTAssertFalse(mic.isRunning, "the microphone must already be off when the rail returns")

        call.finishStartCall()
        await controller.settle()

        XCTAssertEqual(controller.state, .off)
        XCTAssertEqual(mic.startCount, 1, "the tap must not come back for a window the user yielded")
        XCTAssertGreaterThanOrEqual(call.endCount, 1, "the call that answered after the yield must be hung up")
        XCTAssertNil(AmbientArm.claim())
    }

    // MARK: - the battery rails

    /// Pure, and every case is a wrong answer that would ship silently.
    ///
    /// The battery floor REFUSES and Low Power Mode only WARNS, which is the
    /// owner's decision: refusing would make ambient mode unusable for anyone who
    /// lives in Low Power Mode, and the battery cost is theirs to accept.
    func testTheAdmissionVerdict() {
        XCTAssertEqual(
            AmbientPowerMonitor.admission(.healthy),
            .allowed
        )
        XCTAssertEqual(
            AmbientPowerMonitor.admission(
                .init(lowPowerMode: true, batteryLevel: 0.9, isCharging: false)
            ),
            .allowedWithWarning(.lowPowerMode),
            "Low Power Mode arms and says so; it does not refuse"
        )
        XCTAssertEqual(
            AmbientPowerMonitor.admission(
                .init(lowPowerMode: false, batteryLevel: 0.1, isCharging: false)
            ),
            .refused(.batteryLow)
        )
        XCTAssertEqual(
            AmbientPowerMonitor.admission(
                .init(lowPowerMode: true, batteryLevel: 0.1, isCharging: false)
            ),
            .refused(.batteryLow),
            "refusing is the stronger answer, so a phone that is both is refused rather than warned"
        )
        XCTAssertEqual(
            AmbientPowerMonitor.admission(
                .init(lowPowerMode: false, batteryLevel: 0.1, isCharging: true)
            ),
            .allowed,
            "a battery that is filling is not the drain this rail exists to stop"
        )
        XCTAssertEqual(
            AmbientPowerMonitor.admission(
                .init(lowPowerMode: false, batteryLevel: -1, isCharging: false)
            ),
            .allowed,
            "UNKNOWN is not empty — it is every read in the simulator and the first read on a device, and treating it as empty refuses every window"
        )
        XCTAssertEqual(
            AmbientPowerMonitor.admission(
                .init(lowPowerMode: false, batteryLevel: AmbientPowerMonitor.batteryFloor, isCharging: false)
            ),
            .allowed,
            "the floor itself is allowed"
        )
    }

    /// **Low Power Mode is a TRANSITION for a live window and a LEVEL at arm time**,
    /// and that asymmetry is what makes the warning honest. A window that opened
    /// knowing Low Power Mode was on must not then be ended by it the moment any
    /// unrelated notification fires.
    func testTheLiveVerdict() {
        let inLowPower = AmbientPowerMonitor.Readings(lowPowerMode: true, batteryLevel: 0.9, isCharging: false)
        XCTAssertEqual(
            AmbientPowerMonitor.liveBlock(inLowPower, lowPowerModeWasOn: false),
            .lowPowerMode,
            "turning it ON mid-window is a fresh instruction"
        )
        XCTAssertNil(
            AmbientPowerMonitor.liveBlock(inLowPower, lowPowerModeWasOn: true),
            "a standing condition the window was armed under must not end it"
        )
        XCTAssertEqual(
            AmbientPowerMonitor.liveBlock(
                .init(lowPowerMode: true, batteryLevel: 0.1, isCharging: false),
                lowPowerModeWasOn: true
            ),
            .batteryLow,
            "the floor stays a level: there is no transition to wait for, and a battery only falls on its own"
        )
        XCTAssertNil(AmbientPowerMonitor.liveBlock(.healthy, lowPowerModeWasOn: true))
    }

    /// Low Power Mode ARMS, with the reason carried for the whole window on both
    /// surfaces that can show it.
    func testArmingInLowPowerModeOpensTheWindowWithAWarning() async {
        controller.power.readings = { .init(lowPowerMode: true, batteryLevel: 0.9, isCharging: false) }

        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        XCTAssertEqual(controller.state, .armed, "the battery cost is the user's to accept")
        XCTAssertTrue(mic.isRunning)
        XCTAssertEqual(controller.powerWarning, .lowPowerMode, "the in-app bar reads this")
        XCTAssertTrue(
            activity.calls.contains(.caption(AmbientPowerBlock.lowPowerMode.warning, role: nil)),
            "and the orb carries it as a caption, because the warning is the entire justification for arming anyway"
        )
    }

    /// The awkward case the owner accepted, pinned so nobody "fixes" it by
    /// re-introducing the refusal: a window armed while Low Power Mode is ALREADY on
    /// runs to its leash, because both notifications report a change. An informed
    /// window running its leash is not the same failure as a silent one — which is
    /// why the warning has to still be there.
    func testAWindowArmedInLowPowerModeIsNotThenEndedByIt() async {
        controller.power.readings = { .init(lowPowerMode: true, batteryLevel: 0.9, isCharging: false) }
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        NotificationCenter.default.post(name: .NSProcessInfoPowerStateDidChange, object: nil)
        NotificationCenter.default.post(name: UIDevice.batteryLevelDidChangeNotification, object: nil)
        for _ in 0 ..< 300 { await Task.yield() }

        XCTAssertEqual(controller.state, .armed)
        XCTAssertTrue(mic.isRunning)
        XCTAssertEqual(controller.powerWarning, .lowPowerMode, "still warned, for the whole window")
        XCTAssertNotNil(AmbientArm.claim())
    }

    /// And turning it on WHILE a window runs still ends it. That is the behaviour
    /// the owner kept, and it is a different thing from the standing condition above.
    func testLowPowerModeTurnedOnMidWindowEndsIt() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        XCTAssertNil(controller.powerWarning, "precondition: armed on a healthy phone")

        controller.power.readings = { .init(lowPowerMode: true, batteryLevel: 0.9, isCharging: false) }
        NotificationCenter.default.post(name: .NSProcessInfoPowerStateDidChange, object: nil)
        for _ in 0 ..< 500 where controller.state != .off { await Task.yield() }

        XCTAssertEqual(controller.state, .off)
        XCTAssertFalse(mic.isRunning)
        XCTAssertEqual(activity.lifecycleCalls.last, .end(reason: AmbientPowerBlock.lowPowerMode.endedReason))
    }

    /// The battery floor still refuses outright, unchanged: a window opened at 8% is
    /// a microphone that will outlive the phone, and there is nothing informative to
    /// say about it that the user can act on while it runs.
    func testArmingIsRefusedBelowTheBatteryFloor() async {
        controller.power.readings = { .init(lowPowerMode: false, batteryLevel: 0.08, isCharging: false) }

        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        XCTAssertEqual(controller.state, .recoverableError(message: AmbientPowerBlock.batteryLow.refusal))
        XCTAssertFalse(mic.isRunning, "no microphone for a window that was refused")
        XCTAssertFalse(controller.windowIsLive)
        XCTAssertNil(controller.powerWarning, "a refused window warns about nothing; it never opened")
        XCTAssertNil(AmbientArm.claim())
        XCTAssertTrue(activity.calls.isEmpty, "and no orb")
    }

    /// Three sentences from one case, for three different tenses. Pinned because the
    /// wording difference is the whole value: "stopped listening" on a window that is
    /// still running would be a lie, and so would "plug in to listen" on one that has
    /// already stopped.
    func testThePowerCopyDistinguishesItsThreeTenses() {
        for block in [AmbientPowerBlock.lowPowerMode, .batteryLow] {
            XCTAssertFalse(block.refusal.isEmpty)
            XCTAssertFalse(block.endedReason.isEmpty)
            XCTAssertFalse(block.warning.isEmpty)
            XCTAssertNotEqual(block.refusal, block.endedReason)
            XCTAssertNotEqual(block.refusal, block.warning)
            XCTAssertNotEqual(block.endedReason, block.warning)
        }
        XCTAssertNotEqual(AmbientPowerBlock.lowPowerMode.refusal, AmbientPowerBlock.batteryLow.refusal)
    }

    /// The caption is composed in ONE place, so the transcript caption cannot
    /// blank the warning that justifies the window being open —
    /// `testPowerWarningOutranksTranscriptAndCarriesNoRole` pins the collision
    /// itself; this pins the quiet cases around it.
    func testTheOrbCaptionCarriesThePowerWarningWithNoTranscript() {
        XCTAssertEqual(AmbientController.orbCaption(powerWarning: nil, notice: nil, transcript: nil).text, "")
        XCTAssertEqual(
            AmbientController.orbCaption(powerWarning: .lowPowerMode, notice: nil, transcript: nil).text,
            AmbientPowerBlock.lowPowerMode.warning
        )
    }

    /// A live window ends when the battery crosses the floor, and the orb says why
    /// — it is the only surface the user will see, because this happens off screen.
    func testALowBatteryEndsTheWindowWithAReasonOnTheOrb() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        XCTAssertTrue(mic.isRunning, "precondition: armed on a healthy battery")

        controller.power.readings = { .init(lowPowerMode: false, batteryLevel: 0.05, isCharging: false) }
        NotificationCenter.default.post(name: UIDevice.batteryLevelDidChangeNotification, object: nil)
        for _ in 0 ..< 500 where controller.state != .off { await Task.yield() }

        XCTAssertEqual(controller.state, .off)
        XCTAssertFalse(mic.isRunning)
        XCTAssertNil(AmbientArm.claim())
        XCTAssertEqual(activity.lifecycleCalls.last, .end(reason: AmbientPowerBlock.batteryLow.endedReason))
    }

    /// The verdict is re-derived at the moment the rail acts rather than carried
    /// from the notification, so a power-state change that turned Low Power Mode
    /// OFF is a no-op instead of a disarm.
    func testAPowerNotificationWithNothingWrongLeavesTheWindowAlone() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        NotificationCenter.default.post(name: .NSProcessInfoPowerStateDidChange, object: nil)
        for _ in 0 ..< 300 { await Task.yield() }

        XCTAssertEqual(controller.state, .armed)
        XCTAssertTrue(mic.isRunning)
        XCTAssertNotNil(AmbientArm.claim())
    }

    /// The rail is released with the window and re-registered on the next one.
    ///
    /// Asserted through what it *does* rather than through
    /// `UIDevice.isBatteryMonitoringEnabled`, which the simulator reports as
    /// `false` however often it is set — so a test written against that property
    /// fails everywhere the suite actually runs while proving nothing about the
    /// rail. Re-arming is the half a once-only registration would break.
    func testThePowerRailIsReleasedWithTheWindowAndComesBackWithTheNext() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        await controller.disarm(reason: nil)
        let orbCallsAfterDisarm = activity.calls.count

        controller.power.readings = { .init(lowPowerMode: false, batteryLevel: 0.05, isCharging: false) }
        NotificationCenter.default.post(name: UIDevice.batteryLevelDidChangeNotification, object: nil)
        for _ in 0 ..< 300 { await Task.yield() }

        XCTAssertEqual(controller.state, .off)
        XCTAssertEqual(
            activity.calls.count,
            orbCallsAfterDisarm,
            "a released rail must not act on a window that is already over"
        )

        // And the rail is still there for the next window — which is refused
        // outright now, because the battery has not recovered.
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)
        XCTAssertEqual(controller.state, .recoverableError(message: AmbientPowerBlock.batteryLow.refusal))
    }


    // MARK: - backgrounding

    /// **Backgrounding must be a no-op, and it is the normal case.** An armed
    /// window spends nearly all of its life off screen; anything that reacted to
    /// this notification would break the feature for every user on the first use
    /// rather than in an edge case. Pinned because "we did not write that code" is
    /// not a guarantee that nobody will.
    func testEnteringTheBackgroundIsANoOpWhileArmed() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        NotificationCenter.default.post(name: UIApplication.didEnterBackgroundNotification, object: nil)
        for _ in 0 ..< 300 { await Task.yield() }

        XCTAssertEqual(controller.state, .armed)
        XCTAssertTrue(mic.isRunning)
        XCTAssertEqual(mic.stopCount, 0)
        XCTAssertNotNil(AmbientArm.claim())
        XCTAssertEqual(activity.lifecycleVerbs, [.start], "no update, and above all no end")
    }

    /// And mid-conversation, which is where an armed window most often is when the
    /// screen locks.
    func testEnteringTheBackgroundIsANoOpMidConversation() async {
        await armAndConverse()
        let endsBefore = call.endCount

        NotificationCenter.default.post(name: UIApplication.didEnterBackgroundNotification, object: nil)
        for _ in 0 ..< 300 { await Task.yield() }

        XCTAssertEqual(controller.state, .conversing(.listening))
        XCTAssertEqual(call.endCount, endsBefore, "a backgrounded conversation is the normal case, not a hangup")
        XCTAssertNotNil(AmbientArm.claim())
    }

    // MARK: - microphone permission

    /// One failure gets its own caption, and only one. Every other mic failure is
    /// answered by trying again; this one never will be, because the switch is off
    /// in Settings — so "Lost the microphone." would send the user round a loop
    /// that cannot succeed.
    func testARevokedPermissionEndsTheWindowPointingAtSettings() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        mic.simulateFailure(AmbientMicFailure.recordPermissionMissing)
        for _ in 0 ..< 500 where controller.state != .off { await Task.yield() }

        XCTAssertEqual(controller.state, .off)
        XCTAssertEqual(
            activity.lifecycleCalls.last,
            .end(reason: AmbientController.permissionRevokedMessage)
        )
    }

    /// And every other failure keeps the generic one, which is the decision
    /// `AmbientMicFailure` records: a route-change reason code is worth nothing to
    /// someone who is not looking at their phone.
    func testAnyOtherMicFailureKeepsTheGenericCaption() async {
        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        mic.simulateFailure(AmbientMicFailure.inputRouteLost)
        for _ in 0 ..< 500 where controller.state != .off { await Task.yield() }

        XCTAssertEqual(activity.lifecycleCalls.last, .end(reason: "Lost the microphone."))
    }

    /// The first-run and the revoked-in-Settings case at ARM time. `arm`'s catch
    /// said "Couldn't open the microphone." for every cause, which reads as "try
    /// again" for the one cause that never can.
    func testArmingWithNoMicrophonePermissionSaysWhereToTurnItOn() async {
        mic.startError = AmbientMicFailure.recordPermissionMissing

        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        XCTAssertEqual(
            controller.state,
            .recoverableError(message: AmbientController.permissionRevokedMessage)
        )
        XCTAssertNil(AmbientArm.claim())
    }

    func testArmingWithSomeOtherMicFailureKeepsTheGenericMessage() async {
        mic.startError = FakeAmbientMicSource.StartFailure.unavailable

        await controller.arm(phrases: ["hey sam"], capSeconds: 7_200)

        XCTAssertEqual(controller.state, .recoverableError(message: "Couldn't open the microphone."))
    }
}
