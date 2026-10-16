import Combine
import XCTest
@testable import Magician

/// The real `AmbientCallSink`, exercised for everything that does not need a
/// socket — which is deliberately most of it, because the properties that fail
/// *silently* are all reachable here: the turn projection, and whether an
/// ordinary disarm can crash or deactivate the armed window's audio session.
///
/// What is NOT here, and why: `startCall` itself registers a media session over
/// the network and then awaits `session.ready`, so driving it would be a live
/// backend test with a ~46 s ceiling. Everything it composes is asserted
/// separately — `VoiceCallViewModel`'s pre-ready gate, and
/// `awaitReadySessionID`'s own deadline (`VoiceCallViewModelTests`).
@MainActor
final class RealtimeAmbientCallSinkTests: XCTestCase {

    // MARK: - endCall

    /// The protocol pins this and `AmbientController.disarm` depends on it:
    /// `endCall` is issued on EVERY disarm, including from `.armed` where no call
    /// was ever started, so "nothing to end" is the common path. An implementation
    /// shaped like `activeCall!.end()` crashes here.
    func testEndCallIsSafeWithNoCallToEnd() {
        let sink = RealtimeAmbientCallSink()
        sink.endCall()
    }

    /// And safe twice, which is the race rather than the tidy case: a disarm inside
    /// `startCall`'s await ends a call that has not connected yet, so the handoff
    /// issues a second `endCall` once the connect lands.
    func testEndCallIsSafeTwice() {
        let sink = RealtimeAmbientCallSink()
        sink.endCall()
        sink.endCall()
    }

    /// The headline invariant of this whole file: ending an ambient conversation —
    /// including the ordinary "nothing to end" disarm — must leave the shared audio
    /// session active, because `AmbientMicEngine` activates it in the
    /// foreground and `AmbientController.resumeSpotting` reopens the tap from the
    /// background, where a reactivation is refused.
    func testEndCallNeverDeactivatesTheSharedSession() {
        let sink = RealtimeAmbientCallSink()
        sink.endCall()
        sink.endCall()
        XCTAssertEqual(sink.call.mode, .ambient)
        XCTAssertEqual(sink.call.engine.lastSessionDisposition, .keepActive)
        XCTAssertEqual(sink.call.engine.sessionReleaseCount, 0)
        // And the same question asked of the OUTBOUND leg, which is where the
        // device-reproduced failure actually was: a `.keepActive` call must not
        // re-configure or re-activate the session either. `endCall` alone cannot
        // reach a configuration, so this is the standing zero the sink's mode
        // guarantees — see `VoiceCallViewModelTests` for the mapping and the
        // positive control.
        XCTAssertEqual(sink.call.engine.sessionConfigureCount, 0)
        XCTAssertFalse(
            sink.call.mode
                .sessionDisposition(ambientWindowIsLive: { true })
                .configuresSharedSession
        )
    }

    // MARK: - turnPublisher

    /// `AmbientCallSink` pins the publisher as NON-replaying, because
    /// `AmbientController` subscribes before `startCall` for exactly that reason. A
    /// `@Published` projection would replay its current value and make a
    /// late-subscribe bug indistinguishable from correct behaviour.
    func testTurnPublisherDoesNotReplayToALateSubscriber() {
        let sink = RealtimeAmbientCallSink()
        var early: [AmbientTurn] = []
        let earlySubscription = sink.turnPublisher.sink { early.append($0) }

        // The closure the real downstream-audio path invokes, called directly:
        // incoming assistant audio is what makes a turn `.speaking`.
        sink.call.onAssistantAudio?(Self.replyFrame())
        XCTAssertEqual(early, [.speaking])

        var late: [AmbientTurn] = []
        let lateSubscription = sink.turnPublisher.sink { late.append($0) }
        XCTAssertEqual(late, [], "a late subscriber must receive nothing already emitted")

        earlySubscription.cancel()
        lateSubscription.cancel()
    }

    /// A frame arrives roughly every 20 ms and a partial transcript per word, so
    /// an undeduplicated projection would push hundreds of identical turns through
    /// the controller and into ActivityKit.
    func testRepeatedAudioReportsSpeakingOnce() {
        let sink = RealtimeAmbientCallSink()
        var received: [AmbientTurn] = []
        let subscription = sink.turnPublisher.sink { received.append($0) }
        for _ in 0..<20 { sink.call.onAssistantAudio?(Self.replyFrame()) }
        XCTAssertEqual(received, [.speaking])
        subscription.cancel()
    }

    // MARK: - lifecyclePublisher

    /// Same non-replaying contract as `turnPublisher`, and it binds harder here:
    /// the follow-up window is armed INSIDE `startCall`, so the first `.quiet` of
    /// every conversation is emitted before `startCall` returns. A replaying
    /// subject would hand it to a late subscriber and make a controller that
    /// subscribed in the wrong order look correct.
    func testLifecyclePublisherDoesNotReplayToALateSubscriber() {
        let sink = RealtimeAmbientCallSink()
        var received: [AmbientCallLifecycle] = []
        let subscription = sink.lifecyclePublisher.sink { received.append($0) }
        XCTAssertEqual(received, [], "nothing to replay, and nothing invented on subscribe")
        subscription.cancel()
    }

    /// **The rule that keeps a disarm from restoring a microphone.** `endCall` is
    /// the controller's own hangup; reporting it back would close a loop whose far
    /// end puts the spotting tap back for a window the user just stopped. The
    /// hangup drives the transport to a terminal phase, which is exactly the input
    /// the `.ended` projection is built on — so this is a real ordering property,
    /// not a tautology.
    func testEndCallReportsNothingOnTheLifecyclePublisher() {
        let sink = RealtimeAmbientCallSink()
        var received: [AmbientCallLifecycle] = []
        let subscription = sink.lifecyclePublisher.sink { received.append($0) }

        sink.endCall()
        sink.endCall()

        XCTAssertEqual(received, [], "a hangup the controller asked for is not news to the controller")
        XCTAssertNil(sink.followUpExpiresAt)
        subscription.cancel()
    }

    // MARK: - The follow-up window (pure)

    /// `.listening` REFRESHES rather than cancels, and the asymmetry is the whole
    /// decision. Cancelling on any turn at all leaves an exchange with no way to
    /// end: the user speaks, the server judges the utterance unaddressed and
    /// *removes* the caption, and nothing further ever arrives.
    func testFollowUpDisposition() {
        XCTAssertEqual(RealtimeAmbientCallSink.followUpDisposition(for: .listening), .refresh)
        XCTAssertEqual(RealtimeAmbientCallSink.followUpDisposition(for: .thinking), .cancel)
        XCTAssertEqual(RealtimeAmbientCallSink.followUpDisposition(for: .speaking), .cancel)
        XCTAssertEqual(
            RealtimeAmbientCallSink.followUpDisposition(for: nil),
            .leave,
            "an assistant transcript projects no turn, and a reply whose text arrives and whose audio never does must still time out"
        )
    }

    func testEndedCausePerTransportPhase() {
        XCTAssertEqual(RealtimeAmbientCallSink.endedCause(for: .ended), .remote)
        XCTAssertEqual(RealtimeAmbientCallSink.endedCause(for: .failed), .dropped)
        for phase: RealtimeVoiceClient.Phase in [.idle, .connecting, .reconnecting, .ready, .rotating] {
            XCTAssertNil(
                RealtimeAmbientCallSink.endedCause(for: phase),
                "\(phase) is not an ending — reporting one would hang up a live conversation"
            )
        }
    }

    /// Clamped because the value arrives from the network, and a zero would hang up
    /// the instant a conversation went quiet — one sentence per wake word.
    func testFollowUpWindowIsClampedToTheProtocolDefault() {
        XCTAssertEqual(RealtimeAmbientCallSink.followUpWindow(millis: 6_000), 6, accuracy: 0.001)
        let fallback = TimeInterval(RealtimeVoiceProtocol.Addressing.disabled.followUpWindowMs) / 1000
        XCTAssertEqual(fallback, 8, accuracy: 0.001)
        XCTAssertEqual(
            DictationAmbientCallSink.followUpSeconds,
            fallback,
            accuracy: 0.001,
            "streaming and Dictation must offer the same continuous window before returning to wake spotting"
        )
        XCTAssertEqual(RealtimeAmbientCallSink.followUpWindow(millis: 0), fallback, accuracy: 0.001)
        XCTAssertEqual(RealtimeAmbientCallSink.followUpWindow(millis: -1), fallback, accuracy: 0.001)
    }

    // MARK: - Turn projection (pure)

    func testPartialUserTranscriptIsListening() {
        let caption = RealtimeVoiceCaption(role: .user, text: "hey sam what", isFinal: false)
        XCTAssertEqual(RealtimeAmbientCallSink.turn(forLatestCaption: caption), .listening)
    }

    /// A final user transcript is by construction an ADDRESSED one — the backend
    /// removes an unaddressed utterance via `transcript.user.ignored` rather than
    /// finalising it — so the projection needs no addressing check of its own.
    func testFinalUserTranscriptIsThinking() {
        let caption = RealtimeVoiceCaption(role: .user, text: "what's on my calendar", isFinal: true)
        XCTAssertEqual(RealtimeAmbientCallSink.turn(forLatestCaption: caption), .thinking)
    }

    /// An unaddressed utterance never reaches a final user caption at all: it is
    /// removed. Pinned through the reducer so the claim above is not just a comment.
    func testIgnoredUtteranceLeavesNoUserCaptionToProjectFrom() {
        var state = RealtimeVoiceCaptionState()
        state.apply(.transcriptPartial(role: .user, text: "someone else talking",
                                      itemID: "item-1", turnGeneration: 1))
        XCTAssertEqual(
            RealtimeAmbientCallSink.turn(forLatestCaption: state.captions.last),
            .listening
        )
        state.apply(.transcriptIgnored(itemID: "item-1", turnGeneration: 1, reason: "unaddressed"))
        XCTAssertNil(
            RealtimeAmbientCallSink.turn(forLatestCaption: state.captions.last),
            "an ignored utterance must not project a turn"
        )
    }

    /// The assistant's transcript can arrive before, with, or after its audio, so
    /// it says nothing about whether a voice is coming out of the speaker — and
    /// must not demote a `.speaking` the audio already established.
    func testAssistantTranscriptProjectsNoTurn() {
        let caption = RealtimeVoiceCaption(role: .assistant, text: "You have two meetings.")
        XCTAssertNil(RealtimeAmbientCallSink.turn(forLatestCaption: caption))
    }

    func testNoCaptionsProjectNoTurn() {
        XCTAssertNil(RealtimeAmbientCallSink.turn(forLatestCaption: nil))
    }

    /// The projection walks a real conversation through the reducer the client
    /// actually uses, rather than hand-built captions: partial → final → assistant.
    func testProjectionAcrossOneRealTurn() {
        var state = RealtimeVoiceCaptionState()
        state.apply(.transcriptPartial(role: .user, text: "hey sam",
                                      itemID: "item-1", turnGeneration: 1))
        XCTAssertEqual(
            RealtimeAmbientCallSink.turn(forLatestCaption: state.captions.last), .listening
        )
        state.apply(.transcript(role: .user, text: "hey sam what's on my calendar",
                               itemID: "item-1", turnGeneration: 1))
        XCTAssertEqual(
            RealtimeAmbientCallSink.turn(forLatestCaption: state.captions.last), .thinking
        )
        state.apply(.transcript(role: .assistant, text: "Two meetings.",
                               itemID: "item-2", turnGeneration: 1))
        XCTAssertNil(
            RealtimeAmbientCallSink.turn(forLatestCaption: state.captions.last),
            "the reply's transcript must leave the turn to the audio"
        )
    }

    // MARK: - The finals walk (pure)

    /// A partial is not a final: the walk emits nothing for it, and does not
    /// remember it either — an id enters the emitted set only when its line
    /// actually went out.
    func testAPartialIsNotEmittedAndNotRemembered() {
        let partial = RealtimeVoiceCaption(role: .user, text: "hey sam what", isFinal: false)
        let walk = RealtimeAmbientCallSink.newFinals(in: [partial], alreadyEmitted: [])
        XCTAssertTrue(walk.lines.isEmpty)
        XCTAssertTrue(walk.emitted.isEmpty, "a line that never went out is not emitted")
    }

    /// The shape the emitted-set exists for, walked through the reducer the
    /// client actually uses: a user partial finalises IN PLACE under its own id
    /// after a caption has been appended behind it, and the walk emits it
    /// exactly once — at the snapshot where its id first carries final.
    func testAPartialThatFinalisesInPlaceBehindTheTailIsEmittedExactlyOnce() {
        var state = RealtimeVoiceCaptionState()
        state.apply(.transcriptPartial(role: .user, text: "hey sam what's on",
                                      itemID: "item-1", turnGeneration: 1))
        state.apply(.transcript(role: .assistant, text: "Two meetings.",
                               itemID: "item-2", turnGeneration: 1))
        let first = RealtimeAmbientCallSink.newFinals(in: state.captions, alreadyEmitted: [])
        XCTAssertEqual(first.lines.map(\.text), ["Two meetings."],
                       "the partial is not final yet; only the assistant line goes out")

        // The final lands on the SAME id, behind the tail — the in-place flip
        // the reducer performs for a user transcript matched by server item id.
        state.apply(.transcript(role: .user, text: "hey sam what's on my calendar",
                               itemID: "item-1", turnGeneration: 1))
        let second = RealtimeAmbientCallSink.newFinals(in: state.captions,
                                                       alreadyEmitted: first.emitted)
        XCTAssertEqual(second.lines.map(\.text), ["hey sam what's on my calendar"])
        XCTAssertEqual(second.lines.map(\.role), [.user])

        // And never again: the next snapshot finds nothing newly final.
        let third = RealtimeAmbientCallSink.newFinals(in: state.captions,
                                                      alreadyEmitted: second.emitted)
        XCTAssertTrue(third.lines.isEmpty)
    }

    /// A final already emitted is not a new one on the next snapshot.
    func testAlreadyEmittedFinalsAreNotReEmitted() {
        let final = RealtimeVoiceCaption(role: .assistant, text: "You have two meetings.")
        let first = RealtimeAmbientCallSink.newFinals(in: [final], alreadyEmitted: [])
        XCTAssertEqual(first.lines.map(\.text), ["You have two meetings."])
        let second = RealtimeAmbientCallSink.newFinals(in: [final], alreadyEmitted: first.emitted)
        XCTAssertTrue(second.lines.isEmpty)
        XCTAssertEqual(second.emitted, first.emitted)
    }

    /// The reducer never stores empty text, but the skip is contract rather
    /// than trust: a blank line would blank the orb's caption while claiming a
    /// speaker said it.
    func testEmptyTextFinalsAreSkipped() {
        let blank = RealtimeVoiceCaption(role: .assistant, text: "")
        let walk = RealtimeAmbientCallSink.newFinals(in: [blank], alreadyEmitted: [])
        XCTAssertTrue(walk.lines.isEmpty)
        XCTAssertTrue(walk.emitted.isEmpty, "skipped means not emitted, in the set too")
    }

    /// The orb knows two speakers: the user is the user, and anything else the
    /// conversation says is the agent's side of it.
    func testTheWalkMapsUserToUserAndAssistantToAgent() {
        let walk = RealtimeAmbientCallSink.newFinals(
            in: [
                RealtimeVoiceCaption(role: .user, text: "what's on my calendar"),
                RealtimeVoiceCaption(role: .assistant, text: "Two meetings."),
            ],
            alreadyEmitted: []
        )
        XCTAssertEqual(walk.lines.map(\.role), [.user, .agent])
    }

    /// The bound: an id that has left the array can never finalise again, so
    /// the walk forgets it — without losing exactly-once for the ids still
    /// present.
    func testTheEmittedSetIsBoundedByTheLiveArrayWithoutLosingExactlyOnce() {
        let departed = RealtimeVoiceCaption(role: .assistant, text: "Hello.")
        let kept = RealtimeVoiceCaption(role: .assistant, text: "Two meetings.")
        let first = RealtimeAmbientCallSink.newFinals(in: [departed, kept], alreadyEmitted: [])
        XCTAssertEqual(first.lines.count, 2)

        let second = RealtimeAmbientCallSink.newFinals(in: [kept], alreadyEmitted: first.emitted)
        XCTAssertTrue(second.lines.isEmpty, "the survivor was already emitted")
        XCTAssertEqual(second.emitted, [kept.id], "the departed id is forgotten, the survivor kept")
    }

    // MARK: - The end of a reply

    /// Nothing else in the stack reports that the assistant stopped talking, so
    /// without this the orb asserts `speaking` over silence until the user's next
    /// word or the hard cap.
    func testSpeakingReturnsToListeningWhenTheReplyRunsOut() async {
        let sink = RealtimeAmbientCallSink()
        var received: [AmbientTurn] = []
        let subscription = sink.turnPublisher.sink { received.append($0) }
        // 20 ms of reply, so the watch fires at ~0.37 s.
        sink.call.onAssistantAudio?(Self.replyFrame(seconds: 0.02))
        XCTAssertEqual(received, [.speaking])

        let arrived = XCTestExpectation(description: "listening after the reply")
        let watcher = sink.turnPublisher.sink { if $0 == .listening { arrived.fulfill() } }
        await fulfillment(of: [arrived], timeout: 3)
        XCTAssertEqual(received, [.speaking, .listening])
        XCTAssertNil(sink.silenceWatchFiresAtUptime)
        subscription.cancel()
        watcher.cancel()
    }

    /// **The difference between this and a debounce.** A realtime provider streams
    /// a reply faster than realtime, so frames queued back to back must push the
    /// deadline out by their own durations — not reset a fixed timer. Two 0.5 s
    /// frames mean the speaker is busy for a second, not for one tail.
    func testQueuedFramesPushTheDeadlineOutByTheirOwnDuration() {
        let sink = RealtimeAmbientCallSink()
        let before = ProcessInfo.processInfo.systemUptime
        sink.call.onAssistantAudio?(Self.replyFrame(seconds: 0.5))
        let afterOne = sink.silenceWatchFiresAtUptime
        sink.call.onAssistantAudio?(Self.replyFrame(seconds: 0.5))
        let afterTwo = sink.silenceWatchFiresAtUptime

        XCTAssertNotNil(afterOne)
        XCTAssertNotNil(afterTwo)
        XCTAssertEqual(
            afterOne! - before,
            0.5 + RealtimeAmbientCallSink.speakerTailSeconds,
            accuracy: 0.1
        )
        XCTAssertEqual(
            afterTwo! - before,
            1.0 + RealtimeAmbientCallSink.speakerTailSeconds,
            accuracy: 0.1,
            "a debounce would leave both deadlines one tail from now"
        )
        sink.endCall()
    }

    /// An ordinary disarm during a reply must not leave a watch that reports a turn
    /// for a call that is over.
    func testEndCallDisarmsThePendingSilenceWatch() {
        let sink = RealtimeAmbientCallSink()
        sink.call.onAssistantAudio?(Self.replyFrame(seconds: 0.5))
        XCTAssertNotNil(sink.silenceWatchFiresAtUptime)
        sink.endCall()
        XCTAssertNil(sink.silenceWatchFiresAtUptime)
    }

    // MARK: - AssistantPlaybackClock (pure)

    func testClockAccumulatesFramesQueuedAheadOfPlayback() {
        var clock = AssistantPlaybackClock()
        let now: TimeInterval = 1_000
        // Two 0.5 s frames queued in the same instant: the speaker is busy for 1 s.
        XCTAssertEqual(
            clock.queue(frameBytes: 24_000, sampleRate: 24_000, now: now), now + 0.5, accuracy: 0.0001
        )
        XCTAssertEqual(
            clock.queue(frameBytes: 24_000, sampleRate: 24_000, now: now), now + 1.0, accuracy: 0.0001
        )
    }

    /// A frame arriving after the speaker has already run dry starts from the
    /// present, rather than inheriting a deadline from the previous reply.
    func testClockRestartsFromNowWhenTheSpeakerHasRunDry() {
        var clock = AssistantPlaybackClock()
        _ = clock.queue(frameBytes: 24_000, sampleRate: 24_000, now: 1_000)   // idle at 1000.5
        XCTAssertEqual(
            clock.queue(frameBytes: 24_000, sampleRate: 24_000, now: 2_000),
            2_000.5,
            accuracy: 0.0001
        )
    }

    func testClockResetClearsTheDeadline() {
        var clock = AssistantPlaybackClock()
        _ = clock.queue(frameBytes: 24_000, sampleRate: 24_000, now: 1_000)
        clock.reset()
        XCTAssertEqual(clock.idleAtUptime, 0)
        XCTAssertNil(clock.wallClockSpan(now: 1_000), "no audio, no span")
    }

    // MARK: - AssistantPlaybackClock → the orb's span (pure)

    /// The span the orb draws covers the WHOLE audible stretch, not the part of it
    /// still to come.
    ///
    /// A bar anchored at the moment the span became knowable would read 0% when the
    /// first half-second of the reply had already been heard — understating by
    /// exactly the settle delay. Anchoring at the first frame's own arrival is what
    /// makes the bar's position mean "this much of the reply has been said".
    func testTheSpanCoversTheWholeAudibleStretchRatherThanItsRemainder() {
        var clock = AssistantPlaybackClock()
        let begin: TimeInterval = 1_000
        // A reply streamed faster than realtime: 10 s of audio delivered in 0.1 s.
        _ = clock.queue(frameBytes: 24_000, sampleRate: 24_000, now: begin)
        _ = clock.queue(frameBytes: 24_000 * 19, sampleRate: 24_000, now: begin + 0.1)
        let reference = Date(timeIntervalSince1970: 500_000)

        // Read half a second in, which is where the settle debounce puts it.
        let span = clock.wallClockSpan(now: begin + 0.5, reference: reference)

        XCTAssertEqual(span?.from.timeIntervalSince(reference) ?? .nan, -0.5, accuracy: 0.001)
        XCTAssertEqual(span?.until.timeIntervalSince(reference) ?? .nan, 9.5, accuracy: 0.001)
        XCTAssertEqual(
            span?.until.timeIntervalSince(span?.from ?? reference) ?? .nan,
            10,
            accuracy: 0.001,
            "the span's length is the reply's length, whenever it happens to be read"
        )
    }

    /// **The span must not outlive the audio it measures.** Motion continuing after
    /// the thing it depicts has stopped is the same lie as an orb still claiming to
    /// listen, so past the deadline there is nothing to draw rather than a bar
    /// sitting complete.
    func testThereIsNoSpanOnceTheSpeakerHasRunDry() {
        var clock = AssistantPlaybackClock()
        XCTAssertNil(clock.wallClockSpan(now: 1_000), "nothing queued yet")
        _ = clock.queue(frameBytes: 24_000, sampleRate: 24_000, now: 1_000)   // dry at 1000.5
        XCTAssertNotNil(clock.wallClockSpan(now: 1_000.2))
        XCTAssertNil(clock.wallClockSpan(now: 1_000.5), "exactly dry is dry")
        XCTAssertNil(clock.wallClockSpan(now: 1_001))
    }

    /// A second reply is its own span. Inheriting the first one's start would draw a
    /// bar that had been running since a reply the user finished hearing.
    func testEachReplyGetsItsOwnSpan() {
        var clock = AssistantPlaybackClock()
        _ = clock.queue(frameBytes: 24_000, sampleRate: 24_000, now: 1_000)
        _ = clock.queue(frameBytes: 24_000, sampleRate: 24_000, now: 2_000)
        let reference = Date(timeIntervalSince1970: 500_000)

        let span = clock.wallClockSpan(now: 2_000.1, reference: reference)

        XCTAssertEqual(span?.from.timeIntervalSince(reference) ?? .nan, -0.1, accuracy: 0.001)
        XCTAssertEqual(span?.until.timeIntervalSince(reference) ?? .nan, 0.4, accuracy: 0.001)
    }

    /// The type refuses a span that is not a forward stretch of time, which is what
    /// keeps a `ClosedRange` — and its trapping initialiser — out of the widget.
    func testASpanCannotRunBackwardsOrHaveNoLength() {
        let instant = Date(timeIntervalSince1970: 500_000)
        XCTAssertNil(AmbientSpeakingSpan(from: instant, until: instant))
        XCTAssertNil(AmbientSpeakingSpan(from: instant, until: instant.addingTimeInterval(-1)))
        XCTAssertNotNil(AmbientSpeakingSpan(from: instant, until: instant.addingTimeInterval(1)))
    }

    // MARK: - the span reaching the orb

    /// **The bar waits for the queue to stop growing, and that wait is the whole
    /// reason it is honest.** A realtime provider streams a reply faster than
    /// realtime, so the deadline known at the first frame is a few tens of
    /// milliseconds out — a bar drawn to it would fill and complete while the
    /// assistant talked for seconds more, which is exactly the failure the orb
    /// exists to refuse.
    ///
    /// Also pins the vehicle: the sink re-reports the UNCHANGED `.speaking` turn,
    /// because the controller learns spans by re-reading `speakingSpan` and there is
    /// no span at the instant `speaking` begins.
    func testTheSpanIsPublishedOnlyOnceTheQueueHasStoppedGrowing() async {
        let sink = RealtimeAmbientCallSink()
        var received: [AmbientTurn] = []
        let subscription = sink.turnPublisher.sink { received.append($0) }
        sink.call.onAssistantAudio?(Self.replyFrame(seconds: 3))

        XCTAssertEqual(received, [.speaking], "the orb goes green on the first frame, not on the settle")
        XCTAssertNil(
            sink.speakingSpan,
            "the queue is still growing; a bar drawn from here would complete mid-reply"
        )

        let settled = XCTestExpectation(description: "the queue settles and the span is announced")
        let watcher = sink.turnPublisher.sink { if $0 == .speaking { settled.fulfill() } }
        await fulfillment(of: [settled], timeout: 3)

        XCTAssertEqual(
            received,
            [.speaking, .speaking],
            "the turn has not changed; the re-report exists so the span is re-read"
        )
        guard let span = sink.speakingSpan else { return XCTFail("expected a settled span") }
        XCTAssertEqual(
            span.until.timeIntervalSince(span.from),
            3,
            accuracy: 0.5,
            "the span is the queued audio's own length"
        )

        sink.endCall()
        XCTAssertNil(sink.speakingSpan, "a hangup mid-reply leaves no bar elapsing over silence")
        subscription.cancel()
        watcher.cancel()
    }

    func testClockIgnoresADegenerateSampleRate() {
        var clock = AssistantPlaybackClock()
        XCTAssertEqual(clock.queue(frameBytes: 24_000, sampleRate: 0, now: 1_000), 0)
    }

    // MARK: - Helpers

    /// `seconds` of transport-rate PCM16 silence — the shape of a downstream frame,
    /// which is all the playback clock reads.
    private static func replyFrame(seconds: TimeInterval = 0.02) -> Data {
        Data(count: Int(TimeInterval(transportRate) * seconds) * 2)
    }

    private static let transportRate = RealtimeAmbientCallSink.transportSampleRate
}

final class AmbientDictationSilenceGateTests: XCTestCase {
    func testNoSpeechExpiresAtTheFollowUpDeadline() {
        var gate = AmbientDictationSilenceGate(startedAt: 100)
        XCTAssertEqual(gate.observe(powerDB: -80, at: 107.99), .none)
        XCTAssertEqual(gate.observe(powerDB: -80, at: 108), .noSpeech)
    }

    func testSpeechFinishesOnlyAfterTrailingSilence() {
        var gate = AmbientDictationSilenceGate(startedAt: 100)
        XCTAssertEqual(gate.observe(powerDB: -20, at: 100.2), .speechBegan)
        XCTAssertEqual(gate.observe(powerDB: -80, at: 101.2), .none)
        XCTAssertEqual(gate.observe(powerDB: -80, at: 101.31), .finishUtterance)
    }

    func testQuietNoiseDoesNotPretendAnUtteranceExists() {
        var gate = AmbientDictationSilenceGate(startedAt: 10)
        XCTAssertEqual(
            gate.observe(
                powerDB: AmbientDictationSilenceGate.speechThresholdDB - 0.1,
                at: 10.5
            ),
            .none
        )
        XCTAssertFalse(gate.heardSpeech)
    }

    func testMaximumUtteranceBoundsContinuousSpeech() {
        var gate = AmbientDictationSilenceGate(startedAt: 10)
        XCTAssertEqual(gate.observe(powerDB: -20, at: 10.1), .speechBegan)
        XCTAssertEqual(gate.observe(powerDB: -20, at: 54.9), .none)
        XCTAssertEqual(gate.observe(powerDB: -20, at: 55), .finishUtterance)
    }
}

@MainActor
final class DictationAmbientCallSinkTests: XCTestCase {
    func testStartOpensBoundedListeningCapture() async {
        let capture = FakeAmbientDictationCapture()
        let keepalive = FakeAmbientDictationKeepalive()
        let deadline = FakeAmbientDictationDeadlineScheduler()
        let sink = makeSink(
            capture: capture,
            keepalive: keepalive,
            deadlineScheduler: deadline
        )
        var turns: [AmbientTurn] = []
        var lifecycle: [AmbientCallLifecycle] = []
        let turnSubscription = sink.turnPublisher.sink { turns.append($0) }
        let lifecycleSubscription = sink.lifecyclePublisher.sink { lifecycle.append($0) }

        let started = await sink.startCall()
        XCTAssertTrue(started)

        XCTAssertEqual(capture.startCount, 1)
        XCTAssertEqual(keepalive.startCount, 1)
        XCTAssertFalse(
            keepalive.isActive,
            "the continuous input graph, not the silent output graph, owns I/O while listening"
        )
        XCTAssertEqual(keepalive.stopCount, 1)
        XCTAssertEqual(deadline.scheduledSeconds, [DictationAmbientCallSink.maximumTurnSeconds])
        XCTAssertTrue(deadline.isArmed)
        XCTAssertEqual(turns, [.listening])
        guard case .quiet(let deadline)? = lifecycle.last else {
            return XCTFail("a live Dictation capture must publish its no-speech deadline")
        }
        XCTAssertGreaterThan(deadline, Date())
        XCTAssertNil(sink.speakingSpan)
        turnSubscription.cancel()
        lifecycleSubscription.cancel()
    }

    func testTurnCompletionDoesNotReplaceTheContinuousInputGraphWithKeepalive() async {
        let capture = FakeAmbientDictationCapture()
        let keepalive = FakeAmbientDictationKeepalive()
        let responder = FakeAmbientDictationResponder(responses: [.success("Done")])
        let speaker = FakeAmbientDictationSpeaker()
        let sink = makeSink(
            capture: capture,
            responder: responder,
            speaker: speaker,
            keepalive: keepalive
        )
        let replyReachedSpeaker = expectation(description: "turn reached TTS")
        speaker.onSpeak = { _ in replyReachedSpeaker.fulfill() }

        let started = await sink.startCall()
        XCTAssertTrue(started)
        XCTAssertFalse(keepalive.isActive)

        capture.finish(.transcript("What changed?"))
        await fulfillment(of: [replyReachedSpeaker], timeout: 1)

        XCTAssertFalse(
            keepalive.isActive,
            "the retained input graph, not a second output graph, owns STT and TTS gaps"
        )
        sink.endCall()
    }

    func testTranscriptRunsAgentSpeaksReplyAndReopensFollowUpCapture() async {
        let capture = FakeAmbientDictationCapture()
        let responder = FakeAmbientDictationResponder(
            responses: [.success("Here is the answer.")]
        )
        let speaker = FakeAmbientDictationSpeaker()
        let keepalive = FakeAmbientDictationKeepalive()
        let sink = makeSink(
            capture: capture,
            responder: responder,
            speaker: speaker,
            keepalive: keepalive
        )
        var turns: [AmbientTurn] = []
        var captions: [AmbientCaptionLine] = []
        let turnSubscription = sink.turnPublisher.sink { turns.append($0) }
        let captionSubscription = sink.captionPublisher.sink { captions.append($0) }
        let replyReachedSpeaker = expectation(description: "agent reply reached Dictation TTS")
        speaker.onSpeak = { _ in
            XCTAssertFalse(
                keepalive.isActive,
                "the continuous input graph must remain the sole I/O owner during agent/TTS"
            )
            replyReachedSpeaker.fulfill()
        }

        let started = await sink.startCall()
        XCTAssertTrue(started)
        capture.emitSpeechBegan()
        capture.finish(.transcript("What changed?"))
        await fulfillment(of: [replyReachedSpeaker], timeout: 1)

        XCTAssertEqual(responder.transcripts, ["What changed?"])
        XCTAssertEqual(
            captions,
            [
                AmbientCaptionLine(role: .user, text: "What changed?"),
                AmbientCaptionLine(role: .agent, text: "Here is the answer."),
            ]
        )
        speaker.emitStarted()
        XCTAssertTrue(turns.contains(.thinking))
        XCTAssertTrue(turns.contains(.speaking))

        let followUpStarted = expectation(description: "follow-up Dictation capture opened")
        capture.onStart = { count in
            if count == 2 {
                XCTAssertFalse(
                    keepalive.isActive,
                    "the keepalive must remain released when the follow-up PCM gate opens"
                )
                followUpStarted.fulfill()
            }
        }
        speaker.finish(.completed)
        await fulfillment(of: [followUpStarted], timeout: 1)
        XCTAssertEqual(capture.startCount, 2)

        turnSubscription.cancel()
        captionSubscription.cancel()
        sink.endCall()
    }

    func testCompletedReplyStaysLiveWhilePlaybackRouteSettles() async {
        let capture = FakeAmbientDictationCapture()
        let responder = FakeAmbientDictationResponder(
            responses: [.success("Here is the answer.")]
        )
        let speaker = FakeAmbientDictationSpeaker()
        let keepalive = FakeAmbientDictationKeepalive()
        let sink = makeSink(
            capture: capture,
            responder: responder,
            speaker: speaker,
            keepalive: keepalive,
            followUpPlaybackSettleSeconds: 600
        )
        var lifecycle: [AmbientCallLifecycle] = []
        let subscription = sink.lifecyclePublisher.sink { lifecycle.append($0) }
        let replyReachedSpeaker = expectation(description: "reply reached Dictation TTS")
        speaker.onSpeak = { _ in replyReachedSpeaker.fulfill() }

        let started = await sink.startCall()
        XCTAssertTrue(started)
        capture.finish(.transcript("What changed?"))
        await fulfillment(of: [replyReachedSpeaker], timeout: 1)
        speaker.finish(.completed)
        for _ in 0 ..< 100 { await Task.yield() }

        XCTAssertEqual(capture.startCount, 1, "the input gate waits for the output route to settle")
        XCTAssertFalse(keepalive.isActive, "the retained input graph keeps the ambient window admitted during the wait")
        XCTAssertFalse(
            lifecycle.contains { if case .ended = $0 { return true }; return false },
            "a completed reply is not itself a terminal lifecycle event"
        )
        subscription.cancel()
        sink.endCall()
    }

    func testTransientFollowUpCaptureFailureRetriesWithoutEndingConversation() async {
        let capture = FakeAmbientDictationCapture(startResults: [true, false, true])
        let responder = FakeAmbientDictationResponder(
            responses: [.success("Here is the answer.")]
        )
        let speaker = FakeAmbientDictationSpeaker()
        let keepalive = FakeAmbientDictationKeepalive()
        let sink = makeSink(
            capture: capture,
            responder: responder,
            speaker: speaker,
            keepalive: keepalive
        )
        var lifecycle: [AmbientCallLifecycle] = []
        let subscription = sink.lifecyclePublisher.sink { lifecycle.append($0) }
        let replyReachedSpeaker = expectation(description: "reply reached Dictation TTS")
        let followUpStarted = expectation(description: "follow-up capture recovered")
        speaker.onSpeak = { _ in replyReachedSpeaker.fulfill() }
        capture.onStart = { count in if count == 3 { followUpStarted.fulfill() } }

        let started = await sink.startCall()
        XCTAssertTrue(started)
        capture.finish(.transcript("What changed?"))
        await fulfillment(of: [replyReachedSpeaker], timeout: 1)
        speaker.finish(.completed)
        await fulfillment(of: [followUpStarted], timeout: 1)

        XCTAssertEqual(capture.startCount, 3)
        XCTAssertFalse(
            keepalive.isActive,
            "the retained input graph is the sole I/O owner during its follow-up window"
        )
        XCTAssertFalse(
            lifecycle.contains { if case .ended = $0 { return true }; return false },
            "one transient capture refusal must not spend the ambient window"
        )
        guard case .quiet? = lifecycle.last else {
            return XCTFail("the recovered capture must open the eight-second follow-up window")
        }
        subscription.cancel()
        sink.endCall()
    }

    func testFailedFollowUpCaptureRestoresKeepaliveDuringRetryDelay() async {
        let capture = FakeAmbientDictationCapture(startResults: [true, false, true])
        let responder = FakeAmbientDictationResponder(
            responses: [.success("Here is the answer.")]
        )
        let speaker = FakeAmbientDictationSpeaker()
        let keepalive = FakeAmbientDictationKeepalive()
        let sink = makeSink(
            capture: capture,
            responder: responder,
            speaker: speaker,
            keepalive: keepalive,
            followUpCaptureRetrySeconds: 600
        )
        let replyReachedSpeaker = expectation(description: "reply reached Dictation TTS")
        let firstFollowUpFailed = expectation(description: "first follow-up capture refused")
        speaker.onSpeak = { _ in replyReachedSpeaker.fulfill() }
        capture.onStart = { count in
            if count == 2 { firstFollowUpFailed.fulfill() }
        }

        let started = await sink.startCall()
        XCTAssertTrue(started)
        capture.finish(.transcript("What changed?"))
        await fulfillment(of: [replyReachedSpeaker], timeout: 1)
        speaker.finish(.completed)
        await fulfillment(of: [firstFollowUpFailed], timeout: 1)
        for _ in 0 ..< 20 { await Task.yield() }

        XCTAssertTrue(
            keepalive.isActive,
            "a refused capture must not leave the background conversation with no active I/O"
        )
        XCTAssertEqual(capture.startCount, 2)
        sink.endCall()
    }

    func testFollowUpCaptureFailureIsBoundedBeforeReportingDropped() async {
        let capture = FakeAmbientDictationCapture(
            startResults: [true, false, false, false]
        )
        let responder = FakeAmbientDictationResponder(
            responses: [.success("Here is the answer.")]
        )
        let speaker = FakeAmbientDictationSpeaker()
        let sink = makeSink(capture: capture, responder: responder, speaker: speaker)
        let replyReachedSpeaker = expectation(description: "reply reached Dictation TTS")
        let ended = expectation(description: "bounded follow-up attempts exhausted")
        speaker.onSpeak = { _ in replyReachedSpeaker.fulfill() }
        let subscription = sink.lifecyclePublisher.sink { event in
            if event == .ended(.dropped) { ended.fulfill() }
        }

        let started = await sink.startCall()
        XCTAssertTrue(started)
        capture.finish(.transcript("What changed?"))
        await fulfillment(of: [replyReachedSpeaker], timeout: 1)
        speaker.finish(.completed)
        await fulfillment(of: [ended], timeout: 1)

        XCTAssertEqual(
            capture.startCount,
            1 + DictationAmbientCallSink.followUpCaptureAttempts
        )
        subscription.cancel()
    }

    func testNoSpeechIsAnOrdinaryConversationEnd() async {
        let capture = FakeAmbientDictationCapture()
        let keepalive = FakeAmbientDictationKeepalive()
        let deadline = FakeAmbientDictationDeadlineScheduler()
        let sink = makeSink(
            capture: capture,
            keepalive: keepalive,
            deadlineScheduler: deadline
        )
        var lifecycle: [AmbientCallLifecycle] = []
        let ended = expectation(description: "false wake ended")
        let subscription = sink.lifecyclePublisher.sink { event in
            lifecycle.append(event)
            if event == .ended(.wentQuiet) { ended.fulfill() }
        }

        let started = await sink.startCall()
        XCTAssertTrue(started)
        capture.finish(.noSpeech)
        await fulfillment(of: [ended], timeout: 1)
        XCTAssertEqual(lifecycle.last, .ended(.wentQuiet))
        XCTAssertFalse(keepalive.isActive)
        XCTAssertFalse(deadline.isArmed)
        subscription.cancel()
    }

    func testTranscriptionFailureIsNotMisreportedAsUserSilence() async {
        let capture = FakeAmbientDictationCapture()
        let sink = makeSink(capture: capture)
        let ended = expectation(description: "failed transcription ended")
        var terminal: AmbientCallLifecycle?
        let subscription = sink.lifecyclePublisher.sink { event in
            if case .ended = event {
                terminal = event
                ended.fulfill()
            }
        }

        let started = await sink.startCall()
        XCTAssertTrue(started)
        capture.emitSpeechBegan()
        capture.finish(.failedToTranscribe)
        await fulfillment(of: [ended], timeout: 1)
        XCTAssertEqual(terminal, .ended(.dropped))
        subscription.cancel()
    }

    func testAgentFailureEndsDroppedAndDoesNotStartTTS() async {
        let capture = FakeAmbientDictationCapture()
        let responder = FakeAmbientDictationResponder(
            responses: [.failure(FakeAmbientDictationError())]
        )
        let speaker = FakeAmbientDictationSpeaker()
        let sink = makeSink(capture: capture, responder: responder, speaker: speaker)
        let ended = expectation(description: "agent failure ended")
        let subscription = sink.lifecyclePublisher.sink { event in
            if event == .ended(.dropped) { ended.fulfill() }
        }

        let started = await sink.startCall()
        XCTAssertTrue(started)
        capture.finish(.transcript("Try this"))
        await fulfillment(of: [ended], timeout: 1)
        XCTAssertTrue(speaker.spokenTexts.isEmpty)
        subscription.cancel()
    }

    func testEveryTerminalSpeechOutcomePreservesConversationAndReopensCapture() async {
        for playbackResult: SpeechPlaybackResult in [
            .completed,
            .cancelled,
            .failed,
            .skipped,
        ] {
            let capture = FakeAmbientDictationCapture()
            let speaker = FakeAmbientDictationSpeaker()
            let keepalive = FakeAmbientDictationKeepalive()
            let deadline = FakeAmbientDictationDeadlineScheduler()
            let sink = makeSink(
                capture: capture,
                speaker: speaker,
                keepalive: keepalive,
                deadlineScheduler: deadline
            )
            var lifecycle: [AmbientCallLifecycle] = []
            let subscription = sink.lifecyclePublisher.sink { lifecycle.append($0) }
            let replyReachedSpeaker = expectation(
                description: "reply reached TTS for \(playbackResult)"
            )
            let followUpStarted = expectation(
                description: "follow-up capture opened for \(playbackResult)"
            )
            speaker.onSpeak = { _ in replyReachedSpeaker.fulfill() }
            capture.onStart = { count in
                if count == 2 { followUpStarted.fulfill() }
            }

            let started = await sink.startCall()
            XCTAssertTrue(started)
            capture.finish(.transcript("Say the answer"))
            await fulfillment(of: [replyReachedSpeaker], timeout: 1)
            speaker.finish(playbackResult)
            await fulfillment(of: [followUpStarted], timeout: 1)

            XCTAssertEqual(capture.startCount, 2)
            XCTAssertFalse(
                keepalive.isActive,
                "a live follow-up input gate replaces the inter-turn keepalive"
            )
            XCTAssertTrue(deadline.isArmed)
            XCTAssertFalse(
                lifecycle.contains { if case .ended = $0 { return true }; return false },
                "TTS \(playbackResult) must not own the ambient conversation lifetime"
            )
            guard case .quiet? = lifecycle.last else {
                subscription.cancel()
                sink.endCall()
                return XCTFail(
                    "TTS \(playbackResult) must restore the bounded follow-up window"
                )
            }
            subscription.cancel()
            sink.endCall()
        }
    }

    func testTutorBlackboardTearsDownAudioBeforePresentingAndSkipsChat() async {
        let capture = FakeAmbientDictationCapture()
        let responder = FakeAmbientDictationResponder(responses: [.success("unused")])
        let keepalive = FakeAmbientDictationKeepalive()
        var presented: String?
        var keepaliveWasActiveAtPresentation: Bool?
        let sink = makeSink(
            capture: capture,
            responder: responder,
            keepalive: keepalive,
            guidedFlowScreenIsLocked: { false },
            tutorBlackboardPresenter: { concept in
                presented = concept
                keepaliveWasActiveAtPresentation = keepalive.isActive
            }
        )
        var lifecycle: [AmbientCallLifecycle] = []
        let subscription = sink.lifecyclePublisher.sink { lifecycle.append($0) }

        let started = await sink.startCall()
        XCTAssertTrue(started)
        capture.finish(.transcript("Tutor Quick blackboard explain recursion"))

        XCTAssertEqual(presented, "#quick blackboard explain recursion")
        XCTAssertEqual(keepaliveWasActiveAtPresentation, false)
        XCTAssertTrue(responder.transcripts.isEmpty)
        XCTAssertEqual(lifecycle.last, .ended(.wentQuiet))
        subscription.cancel()
    }

    func testLockedTutorSpeaksUnlockGuidanceAndNeverPresentsOrChats() async {
        let capture = FakeAmbientDictationCapture()
        let responder = FakeAmbientDictationResponder(responses: [.success("unused")])
        let speaker = FakeAmbientDictationSpeaker()
        var presented: [String] = []
        let sink = makeSink(
            capture: capture,
            responder: responder,
            speaker: speaker,
            guidedFlowScreenIsLocked: { true },
            tutorBlackboardPresenter: { presented.append($0) }
        )
        var lifecycle: [AmbientCallLifecycle] = []
        let subscription = sink.lifecyclePublisher.sink { lifecycle.append($0) }

        let started = await sink.startCall()
        XCTAssertTrue(started)
        capture.finish(.transcript("Tutor blackboard explain recursion"))
        XCTAssertEqual(speaker.spokenTexts, ["Please unlock your screen to use Tutor."])
        speaker.finish(.completed)

        XCTAssertTrue(presented.isEmpty)
        XCTAssertTrue(responder.transcripts.isEmpty)
        XCTAssertEqual(lifecycle.last, .ended(.wentQuiet))
        subscription.cancel()
    }

    func testAmbientGuidedFlowAdmissionDoesNotInferFromIncidentalTutorWords() {
        XCTAssertEqual(
            DictationAmbientCallSink.guidedFlowDecision(
                for: "Could a tutor explain this?",
                screenIsLocked: false
            ),
            .ordinaryChat
        )
        XCTAssertEqual(
            DictationAmbientCallSink.guidedFlowDecision(
                for: "App Copilot show me the next step",
                screenIsLocked: false
            ),
            .reject("App Copilot isn't available on this device yet.")
        )
        XCTAssertEqual(
            DictationAmbientCallSink.guidedFlowDecision(
                for: "Tutor screen explain this graph",
                screenIsLocked: false
            ),
            .reject(
                "Screen tutoring isn't available on this device yet. Say Tutor blackboard instead."
            )
        )
    }

    func testRequestedEndIsSilentAndLateCaptureCannotReviveIt() async {
        let capture = FakeAmbientDictationCapture()
        let keepalive = FakeAmbientDictationKeepalive()
        let deadline = FakeAmbientDictationDeadlineScheduler()
        let sink = makeSink(
            capture: capture,
            keepalive: keepalive,
            deadlineScheduler: deadline
        )
        var lifecycle: [AmbientCallLifecycle] = []
        let subscription = sink.lifecyclePublisher.sink { lifecycle.append($0) }

        let started = await sink.startCall()
        XCTAssertTrue(started)
        lifecycle.removeAll()
        sink.endCall()
        capture.finish(.transcript("too late"))

        XCTAssertTrue(lifecycle.isEmpty)
        XCTAssertGreaterThanOrEqual(capture.cancelCount, 2)
        XCTAssertFalse(keepalive.isActive)
        XCTAssertFalse(deadline.isArmed)
        subscription.cancel()
    }

    func testCaptureStartFailureReturnsFalseWithoutInventingLifecycle() async {
        let capture = FakeAmbientDictationCapture(startSucceeds: false)
        let keepalive = FakeAmbientDictationKeepalive()
        let sink = makeSink(capture: capture, keepalive: keepalive)
        var lifecycle: [AmbientCallLifecycle] = []
        let subscription = sink.lifecyclePublisher.sink { lifecycle.append($0) }

        let started = await sink.startCall()
        XCTAssertFalse(started)
        XCTAssertTrue(lifecycle.isEmpty)
        XCTAssertFalse(keepalive.isActive)
        XCTAssertEqual(
            keepalive.stopCount,
            2,
            "failed capture restores the bridge before start-call teardown stops it"
        )
        subscription.cancel()
    }

    func testKeepaliveFailurePreventsABackgroundUnsafeCapture() async {
        let capture = FakeAmbientDictationCapture()
        let keepalive = FakeAmbientDictationKeepalive(startSucceeds: false)
        let sink = makeSink(capture: capture, keepalive: keepalive)
        var lifecycle: [AmbientCallLifecycle] = []
        let subscription = sink.lifecyclePublisher.sink { lifecycle.append($0) }

        let started = await sink.startCall()

        XCTAssertFalse(started)
        XCTAssertEqual(keepalive.startCount, 1)
        XCTAssertEqual(capture.startCount, 0)
        XCTAssertTrue(lifecycle.isEmpty)
        subscription.cancel()
    }

    func testTurnDeadlineStopsEveryOwnedRailAndReportsDropped() async {
        let capture = FakeAmbientDictationCapture()
        let keepalive = FakeAmbientDictationKeepalive()
        let deadline = FakeAmbientDictationDeadlineScheduler()
        let sink = makeSink(
            capture: capture,
            keepalive: keepalive,
            deadlineScheduler: deadline
        )
        var lifecycle: [AmbientCallLifecycle] = []
        let subscription = sink.lifecyclePublisher.sink { lifecycle.append($0) }

        let started = await sink.startCall()
        XCTAssertTrue(started)
        deadline.fire()

        XCTAssertEqual(lifecycle.last, .ended(.dropped))
        XCTAssertFalse(keepalive.isActive)
        XCTAssertFalse(deadline.isArmed)
        XCTAssertGreaterThanOrEqual(capture.cancelCount, 2)
        subscription.cancel()
    }

    func testAmbientDictationMaySpeakThroughItsOwnWindowFocus() {
        XCTAssertFalse(
            SpeechSynthesizer.permitsSpeech(
                voiceCallFocusActive: true,
                policy: .respectVoiceCall
            )
        )
        XCTAssertTrue(
            SpeechSynthesizer.permitsSpeech(
                voiceCallFocusActive: true,
                policy: .ambientDictationOwner
            )
        )
    }

    func testDictationDoesNotClaimItPrimedAStreamingAudioGraph() {
        let sink = makeSink(capture: FakeAmbientDictationCapture())
        XCTAssertFalse(sink.primeAudioGraph())
    }

    func testAmbientConversationRouterMapsAllThreeLocalModes() {
        XCTAssertEqual(
            AmbientConversationCallSink.route(for: .dictation),
            .dictation
        )
        XCTAssertEqual(
            AmbientConversationCallSink.route(for: .handsFree),
            .streaming(.handsFree)
        )
        XCTAssertEqual(
            AmbientConversationCallSink.route(for: .realtime),
            .streaming(.realtime)
        )
    }

    func testAmbientCaptionRemovesSpeechProtocolWrappers() {
        XCTAssertEqual(
            DictationAmbientCallSink.displayText(
                for: "Visible <speech voice=\"warm\">spoken answer</speech>"
            ),
            "Visible spoken answer"
        )
    }

    func testAmbientDictationChatBodyPreservesVoiceOriginAndTurnIdentity() {
        let body = NetworkAmbientDictationResponder.messageBody(
            transcript: "hello",
            chatTurnID: "turn-1"
        )
        XCTAssertEqual(body["text"] as? String, "hello")
        XCTAssertEqual(body["chat_turn_id"] as? String, "turn-1")
        XCTAssertEqual(body["source_surface"] as? String, "ios")
        XCTAssertEqual(body["continue_on_disconnect"] as? Bool, true)
        XCTAssertEqual(body["voice_origin"] as? Bool, true)
    }

    func testAmbientDictationSSEParserAcceptsOnlyNonEmptyTextDataFrames() {
        XCTAssertEqual(
            NetworkAmbientDictationResponder.responseToken(
                fromSSELine: #"data: {"text":"hello "}"#
            ),
            "hello "
        )
        XCTAssertNil(
            NetworkAmbientDictationResponder.responseToken(
                fromSSELine: #"event: token"#
            )
        )
        XCTAssertNil(
            NetworkAmbientDictationResponder.responseToken(
                fromSSELine: #"data: {"text":""}"#
            )
        )
    }

    func testAmbientDictationDoneParserReadsCanonicalCompleteAssistantText() {
        let line = #"data: {"assistant_message":{"content":{"type":"text","text":"complete answer"}}}"#
        XCTAssertEqual(
            NetworkAmbientDictationResponder.doneResponseText(fromSSELine: line),
            "complete answer"
        )
        XCTAssertNil(
            NetworkAmbientDictationResponder.doneResponseText(
                fromSSELine: #"data: {"assistant_message":{"content":{"type":"tool_call_executed"}}}"#
            )
        )
    }

    func testAmbientDictationErrorParserKeepsServerFailureReason() {
        XCTAssertEqual(
            NetworkAmbientDictationResponder.responseError(
                fromSSELine: #"data: {"error":"provider unavailable"}"#
            ),
            "provider unavailable"
        )
    }

    func testAmbientDictationSessionDecoderRequiresCanonicalNestedID() {
        let valid = Data(#"{"session":{"id":"session-1"}}"#.utf8)
        let missing = Data(#"{"session":{}}"#.utf8)
        XCTAssertEqual(NetworkAmbientDictationResponder.sessionID(from: valid), "session-1")
        XCTAssertNil(NetworkAmbientDictationResponder.sessionID(from: missing))
    }

    func testEndingCallSuppressesAnAgentReplyThatIgnoresCancellation() async {
        let capture = FakeAmbientDictationCapture()
        let responder = SuspendedAmbientDictationResponder()
        let speaker = FakeAmbientDictationSpeaker()
        let sink = DictationAmbientCallSink(
            capture: capture,
            responder: responder,
            speaker: speaker,
            keepalive: FakeAmbientDictationKeepalive(),
            deadlineScheduler: FakeAmbientDictationDeadlineScheduler()
        )
        var captions: [AmbientCaptionLine] = []
        var lifecycle: [AmbientCallLifecycle] = []
        let captionSubscription = sink.captionPublisher.sink { captions.append($0) }
        let lifecycleSubscription = sink.lifecyclePublisher.sink { lifecycle.append($0) }
        let ignoredReplyReturned = expectation(
            description: "cancelled responder returned its deliberately late reply"
        )
        responder.onResponseReturned = { ignoredReplyReturned.fulfill() }

        let started = await sink.startCall()
        XCTAssertTrue(started)
        capture.finish(.transcript("keep this user line"))
        await responder.waitUntilRequested()

        sink.endCall()
        responder.finish(with: "too late")
        await fulfillment(of: [ignoredReplyReturned], timeout: 1)

        XCTAssertEqual(
            captions,
            [AmbientCaptionLine(role: .user, text: "keep this user line")]
        )
        XCTAssertTrue(speaker.spokenTexts.isEmpty)
        XCTAssertEqual(lifecycle.count, 1)
        guard let firstLifecycle = lifecycle.first,
              case .quiet = firstLifecycle else {
            return XCTFail("ending the call must not add a terminal lifecycle event")
        }
        captionSubscription.cancel()
        lifecycleSubscription.cancel()
    }

    private func makeSink(
        capture: FakeAmbientDictationCapture,
        responder: FakeAmbientDictationResponder? = nil,
        speaker: FakeAmbientDictationSpeaker? = nil,
        keepalive: FakeAmbientDictationKeepalive? = nil,
        deadlineScheduler: FakeAmbientDictationDeadlineScheduler? = nil,
        followUpPlaybackSettleSeconds: TimeInterval = 0,
        followUpCaptureRetrySeconds: TimeInterval = 0,
        guidedFlowScreenIsLocked: @escaping () -> Bool = { false },
        tutorBlackboardPresenter: @escaping (String) -> Void = { _ in }
    ) -> DictationAmbientCallSink {
        let resolvedResponder = responder ?? FakeAmbientDictationResponder(
            responses: [.success("Okay.")]
        )
        return DictationAmbientCallSink(
            capture: capture,
            responder: resolvedResponder,
            speaker: speaker ?? FakeAmbientDictationSpeaker(),
            keepalive: keepalive ?? FakeAmbientDictationKeepalive(),
            deadlineScheduler: deadlineScheduler ?? FakeAmbientDictationDeadlineScheduler(),
            followUpPlaybackSettleSeconds: followUpPlaybackSettleSeconds,
            followUpCaptureRetrySeconds: followUpCaptureRetrySeconds,
            guidedFlowScreenIsLocked: guidedFlowScreenIsLocked,
            tutorBlackboardPresenter: tutorBlackboardPresenter
        )
    }
}

@MainActor
private final class FakeAmbientDictationCapture: AmbientDictationCapturing {
    private let startResults: [Bool]
    private(set) var startCount = 0
    private(set) var cancelCount = 0
    var onStart: ((Int) -> Void)?
    private var speechBegan: (() -> Void)?
    private var result: ((AmbientDictationCaptureResult) -> Void)?

    init(startSucceeds: Bool = true) {
        startResults = [startSucceeds]
    }

    init(startResults: [Bool]) {
        self.startResults = startResults.isEmpty ? [false] : startResults
    }

    func startCapture(
        onSpeechBegan: @escaping () -> Void,
        onResult: @escaping (AmbientDictationCaptureResult) -> Void,
        onStarted: @escaping (Bool) -> Void
    ) {
        startCount += 1
        speechBegan = onSpeechBegan
        result = onResult
        onStart?(startCount)
        onStarted(startResults[min(startCount - 1, startResults.count - 1)])
    }

    func cancelCapture() {
        cancelCount += 1
        speechBegan = nil
        result = nil
    }

    func emitSpeechBegan() {
        speechBegan?()
    }

    func finish(_ value: AmbientDictationCaptureResult) {
        let callback = result
        speechBegan = nil
        result = nil
        callback?(value)
    }
}

@MainActor
private final class FakeAmbientDictationResponder: AmbientDictationResponding {
    var responses: [Result<String, Error>]
    private(set) var transcripts: [String] = []
    private(set) var resetCount = 0

    init(responses: [Result<String, Error>]) {
        self.responses = responses
    }

    func resetConversation() {
        resetCount += 1
    }

    func respond(to transcript: String) async throws -> String {
        transcripts.append(transcript)
        guard !responses.isEmpty else { throw FakeAmbientDictationError() }
        return try responses.removeFirst().get()
    }
}

@MainActor
private final class FakeAmbientDictationSpeaker: AmbientDictationSpeaking {
    private(set) var spokenTexts: [String] = []
    var onSpeak: ((String) -> Void)?
    private var started: (() -> Void)?
    private var completion: ((SpeechPlaybackResult) -> Void)?

    @discardableResult
    func speak(
        _ text: String,
        onStart: @escaping () -> Void,
        completion: @escaping (SpeechPlaybackResult) -> Void
    ) -> Bool {
        spokenTexts.append(text)
        started = onStart
        self.completion = completion
        onSpeak?(text)
        return true
    }

    func stop() {
        started = nil
        completion = nil
    }

    func emitStarted() {
        started?()
        started = nil
    }

    func finish(_ result: SpeechPlaybackResult) {
        let callback = completion
        completion = nil
        callback?(result)
    }
}

private struct FakeAmbientDictationError: Error {}

@MainActor
private final class FakeAmbientDictationKeepalive: AmbientDictationKeepingAlive {
    let startSucceeds: Bool
    private(set) var startCount = 0
    private(set) var stopCount = 0
    private(set) var isActive = false

    init(startSucceeds: Bool = true) {
        self.startSucceeds = startSucceeds
    }

    func start() -> Bool {
        startCount += 1
        isActive = startSucceeds
        return startSucceeds
    }

    func stop() {
        guard isActive else { return }
        stopCount += 1
        isActive = false
    }
}

@MainActor
private final class FakeAmbientDictationDeadlineScheduler:
    AmbientDictationDeadlineScheduling
{
    private(set) var scheduledSeconds: [TimeInterval] = []
    private(set) var isArmed = false
    private var action: (() -> Void)?

    func schedule(after seconds: TimeInterval, action: @escaping () -> Void) {
        scheduledSeconds.append(seconds)
        isArmed = true
        self.action = action
    }

    func cancel() {
        isArmed = false
        action = nil
    }

    func fire() {
        let callback = action
        action = nil
        isArmed = false
        callback?()
    }
}

@MainActor
private final class SuspendedAmbientDictationResponder: AmbientDictationResponding {
    private var requestContinuation: CheckedContinuation<Void, Never>?
    private var responseContinuation: CheckedContinuation<String, Error>?
    var onResponseReturned: (() -> Void)?

    func resetConversation() {}

    func respond(to transcript: String) async throws -> String {
        requestContinuation?.resume()
        requestContinuation = nil
        let response = try await withCheckedThrowingContinuation { continuation in
            responseContinuation = continuation
        }
        onResponseReturned?()
        return response
    }

    func waitUntilRequested() async {
        if responseContinuation != nil { return }
        await withCheckedContinuation { continuation in
            requestContinuation = continuation
        }
    }

    /// Deliberately resumes after cancellation to prove the sink's generation
    /// and task guards, rather than cooperation from the responder, own safety.
    func finish(with response: String) {
        responseContinuation?.resume(returning: response)
        responseContinuation = nil
    }
}
