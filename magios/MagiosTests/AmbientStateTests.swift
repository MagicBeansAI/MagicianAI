import ActivityKit
import XCTest
@testable import Magician

/// The controller's state space is richer than the orb's. This pins the
/// reduction, because Live Activity updates are budgeted and a mis-mapped state
/// either burns the budget or shows the user the wrong thing.
final class AmbientStateTests: XCTestCase {

    func testTransientLifecycleStatesHaveNoOrb() {
        XCTAssertNil(AmbientState.off.orbPhase)
        XCTAssertNil(AmbientState.arming.orbPhase)
        XCTAssertNil(AmbientState.disarming(reason: nil).orbPhase)
        // The reason rides along for the final content; it is not a phase to draw.
        XCTAssertNil(AmbientState.disarming(reason: "cap reached").orbPhase)
    }

    func testArmedRests() {
        XCTAssertEqual(AmbientState.armed.orbPhase, .armed)
    }

    /// A recoverable error drops back to armed rather than tearing the window
    /// down, so the wake word keeps working — the orb must say so.
    func testRecoverableErrorStillReadsAsArmed() {
        XCTAssertEqual(AmbientState.recoverableError(message: "socket dropped").orbPhase, .armed)
    }

    func testHeardAndConnectingBothReadAsHeard() {
        XCTAssertEqual(AmbientState.heard(phrase: "hey sam").orbPhase, .heard)
        XCTAssertEqual(AmbientState.connecting.orbPhase, .heard)
    }

    func testConversingMapsTurnDirectly() {
        XCTAssertEqual(AmbientState.conversing(.listening).orbPhase, .listening)
        XCTAssertEqual(AmbientState.conversing(.thinking).orbPhase, .thinking)
        XCTAssertEqual(AmbientState.conversing(.speaking).orbPhase, .speaking)
    }

    /// During the follow-up window the microphone is still open FOR THE USER —
    /// that is materially different from resting, so it renders as listening.
    func testCooldownReadsAsListeningNotArmed() {
        XCTAssertEqual(AmbientState.cooldown(until: Date()).orbPhase, .listening)
    }

    /// Only transitions that CHANGE the orb may push an update.
    func testUpdateIsSuppressedWhenTheReductionIsUnchanged() {
        XCTAssertFalse(AmbientState.orbPhaseChanged(from: .armed, to: .recoverableError(message: "x")))
        XCTAssertFalse(AmbientState.orbPhaseChanged(from: .heard(phrase: "a"), to: .connecting))
        XCTAssertTrue(AmbientState.orbPhaseChanged(from: .armed, to: .heard(phrase: "a")))
        XCTAssertTrue(AmbientState.orbPhaseChanged(from: .conversing(.thinking), to: .conversing(.speaking)))
    }

    /// The reduction is what selects the ActivityKit verb, so the two boundary
    /// crossings have to be distinguishable from the pair alone.
    func testEnteringAndLeavingTheActivityAreBothChanges() {
        XCTAssertTrue(AmbientState.orbPhaseChanged(from: .arming, to: .armed))
        XCTAssertTrue(AmbientState.orbPhaseChanged(from: .conversing(.speaking), to: .disarming(reason: "cap reached")))
        XCTAssertFalse(AmbientState.orbPhaseChanged(from: .disarming(reason: "cap reached"), to: .off))
    }

    /// A Live Activity started by a previous build can still be on screen after an
    /// app update. If an unknown phase threw, the whole `ContentState` would fail
    /// to decode and the orb — the only disarm control — would stop rendering,
    /// stranding the user with an armed microphone and no way to stop it.
    func testUnknownOrbPhaseDecodesAsArmedRatherThanThrowing() throws {
        let json = Data(#"{"phase":"telepathy","caption":"still here"}"#.utf8)
        let state = try JSONDecoder().decode(AmbientActivityAttributes.ContentState.self, from: json)
        XCTAssertEqual(state.phase, .armed)
        XCTAssertEqual(state.caption, "still here")
        XCTAssertNil(state.endedReason)
    }

    /// The identical hazard one key over: an old payload predates a field the
    /// current binary declares. A missing key must degrade to the memberwise
    /// default, because one `keyNotFound` destroys the whole `ContentState` and
    /// with it the orb the user disarms from.
    func testContentStateDecodesAroundMissingFields() throws {
        let onlyPhase = try JSONDecoder().decode(
            AmbientActivityAttributes.ContentState.self,
            from: Data(#"{"phase":"speaking"}"#.utf8)
        )
        XCTAssertEqual(onlyPhase.phase, .speaking)
        XCTAssertEqual(onlyPhase.caption, "")
        XCTAssertNil(onlyPhase.endedReason)

        // No key is individually fatal — including the phase itself.
        let empty = try JSONDecoder().decode(
            AmbientActivityAttributes.ContentState.self,
            from: Data("{}".utf8)
        )
        XCTAssertEqual(empty.phase, .armed)
        XCTAssertEqual(empty.caption, "")
        XCTAssertNil(empty.endedReason)
    }

    /// The speaking span is the newest pair of fields, so it is the one an activity
    /// started by the PREVIOUS build is guaranteed not to carry — precisely the case
    /// the hand-written decoder exists for. Absent means "no reply is playing",
    /// which is also the memberwise default.
    func testContentStateDecodesAroundAnAbsentSpeakingSpan() throws {
        let old = try JSONDecoder().decode(
            AmbientActivityAttributes.ContentState.self,
            from: Data(#"{"phase":"speaking","caption":""}"#.utf8)
        )
        XCTAssertEqual(old.phase, .speaking)
        XCTAssertNil(old.speakingSpan, "a phase without a span shows no bar rather than an invented one")

        // Half a pair is no pair. A payload with only one end of the span cannot
        // describe a stretch of time, so it must not produce one.
        let halfWritten = try JSONDecoder().decode(
            AmbientActivityAttributes.ContentState.self,
            from: Data(#"{"phase":"speaking","speakingUntil":760000000}"#.utf8)
        )
        XCTAssertNil(halfWritten.speakingSpan)
    }

    /// **A payload must not be able to crash the widget while decoding**, which is
    /// why the span is two plain `Date`s on the wire and a failable construction on
    /// the way out. `ClosedRange` was the obvious shape and would have trapped here:
    /// its own `Decodable` preconditions `lower <= upper`, so a reordered or stale
    /// pair would take down the orb — the only disarm control — rather than
    /// degrading to no bar.
    func testAnInvertedSpeakingSpanDecodesToNoBarRatherThanTrapping() throws {
        let inverted = try JSONDecoder().decode(
            AmbientActivityAttributes.ContentState.self,
            from: Data(#"{"phase":"speaking","speakingFrom":760000010,"speakingUntil":760000000}"#.utf8)
        )
        XCTAssertEqual(inverted.phase, .speaking, "the rest of the state still decodes")
        XCTAssertNil(inverted.speakingSpan)
    }

    /// A span that IS honest survives the round trip, so the defence above cannot be
    /// mistaken for "spans never render".
    func testAnHonestSpeakingSpanRoundTrips() throws {
        let from = Date(timeIntervalSince1970: 760_000_000)
        guard let span = AmbientSpeakingSpan(from: from, until: from.addingTimeInterval(4)) else {
            return XCTFail("expected a forward span")
        }
        let encoded = try JSONEncoder().encode(
            AmbientActivityAttributes.ContentState(phase: .speaking, speakingSpan: span)
        )
        let decoded = try JSONDecoder().decode(AmbientActivityAttributes.ContentState.self, from: encoded)

        XCTAssertEqual(decoded.speakingSpan, span)
    }

    /// The connect clock is the newest field, so it is the one an activity
    /// started by the PREVIOUS build is guaranteed not to carry. Absent must
    /// mean "draw no gauge" — the widget must never guess when an attempt
    /// began — and a carried clock must survive the round trip, or the gauge
    /// never renders at all.
    func testConnectingSinceDecodesAroundAbsenceAndRoundTrips() throws {
        let old = try JSONDecoder().decode(
            AmbientActivityAttributes.ContentState.self,
            from: Data(#"{"phase":"heard","caption":""}"#.utf8)
        )
        XCTAssertNil(old.connectingSince, "an old build's payload degrades the slot to empty, never to a guessed gauge")

        let since = Date(timeIntervalSince1970: 760_000_000)
        let encoded = try JSONEncoder().encode(
            AmbientActivityAttributes.ContentState(phase: .heard, connectingSince: since)
        )
        let decoded = try JSONDecoder().decode(AmbientActivityAttributes.ContentState.self, from: encoded)
        XCTAssertEqual(decoded.connectingSince, since)
    }

    /// The gauge's existence rule, pinned where it is decided: the clock exists
    /// exactly while a connect attempt does. `heard` — the phase every publish
    /// into the connect wears, the wake's and the retry's alike — stamps the
    /// instant it was published (which is how the retry's re-publish restarts
    /// the gauge honestly); every other phase carries nil, so a conversing or
    /// resting orb can never drag a stale give-up clock behind it.
    func testTheConnectClockExistsExactlyWhileAnAttemptDoes() {
        let now = Date(timeIntervalSince1970: 760_000_000)
        for phase in AmbientOrbPhase.allCases {
            let clock = AmbientActivity.connectClock(for: phase, now: now)
            if phase == .heard {
                XCTAssertEqual(clock, now, "the attempt begins at the publish that announces it")
            } else {
                XCTAssertNil(clock, "\(phase) has no attempt for a gauge to time")
            }
        }
    }

    /// The pulse flag is the newest field, so it is the one an old build's
    /// payload is guaranteed not to carry — absent must mean no pulse (the
    /// resting reading, never a stuck word), and a carried flag must survive
    /// the round trip or the pulse never renders.
    func testShowsPhaseWordDecodesAroundAbsenceAndRoundTrips() throws {
        let old = try JSONDecoder().decode(
            AmbientActivityAttributes.ContentState.self,
            from: Data(#"{"phase":"listening","caption":""}"#.utf8)
        )
        XCTAssertFalse(old.showsPhaseWord, "an old build's payload pulses nothing")

        let encoded = try JSONEncoder().encode(
            AmbientActivityAttributes.ContentState(phase: .listening, showsPhaseWord: true)
        )
        let decoded = try JSONDecoder().decode(AmbientActivityAttributes.ContentState.self, from: encoded)
        XCTAssertTrue(decoded.showsPhaseWord)
    }

    /// The pulse's first show is free — it rides the phase publish the
    /// transition was already spending — and this is the rule that decides
    /// which publishes carry it: the three conversing phases show their word,
    /// `armed` has no conversation to caption, and `heard`'s word is the
    /// continuous connect regime rather than the pulse. Exhaustive so a sixth
    /// phase must pick a side here.
    func testThePhaseWordRidesExactlyTheConversingPublishes() {
        for phase in AmbientOrbPhase.allCases {
            let shows = AmbientActivity.phaseWordOnPublish(for: phase)
            switch phase {
            case .listening, .thinking, .speaking:
                XCTAssertTrue(shows, "\(phase)'s transition publish is the pulse's free first show")
            case .armed, .heard:
                XCTAssertFalse(shows, "\(phase) has no pulsed word to show")
            }
        }
    }

    /// The fallback must not swallow a phase that is genuinely still known.
    func testKnownOrbPhasesStillRoundTrip() throws {
        for phase in [AmbientOrbPhase.armed, .heard, .listening, .thinking, .speaking] {
            let encoded = try JSONEncoder().encode(AmbientActivityAttributes.ContentState(phase: phase))
            let decoded = try JSONDecoder().decode(AmbientActivityAttributes.ContentState.self, from: encoded)
            XCTAssertEqual(decoded.phase, phase)
        }
    }

    /// A payload written by today's build has none of the three new keys. It must
    /// decode — the orb is the only disarm control — and mean what it meant.
    func testLegacyPayloadDecodesWithNewFieldsDefaulted() throws {
        let legacy = Data(#"{"phase":"listening","caption":"hi"}"#.utf8)
        let state = try JSONDecoder().decode(AmbientActivityAttributes.ContentState.self, from: legacy)
        XCTAssertNil(state.captionRole)
        XCTAssertEqual(state.exchangeCount, 0)
        XCTAssertNil(state.endedAt)
        XCTAssertNil(state.expiresAt)
        XCTAssertEqual(
            state.effectiveExpiresAt(fallback: Date(timeIntervalSince1970: 9_999)),
            Date(timeIntervalSince1970: 9_999),
            "an activity from before Extend must retain its immutable attribute deadline"
        )
    }

    /// A role string this binary does not know degrades to nil (caption renders
    /// unattributed) rather than throwing the whole state away.
    func testUnknownCaptionRoleDegradesToNil() throws {
        let payload = Data(#"{"phase":"listening","caption":"hi","captionRole":"narrator"}"#.utf8)
        let state = try JSONDecoder().decode(AmbientActivityAttributes.ContentState.self, from: payload)
        XCTAssertNil(state.captionRole)
        XCTAssertEqual(state.phase, .listening, "the rest of the state still decodes")
    }

    /// The round trip moves encoder and decoder together, so only a pinned
    /// literal catches a case rename — which would strand every payload the
    /// previous build persisted as an unattributed caption.
    func testCaptionRoleWireStringsArePinned() throws {
        let agent = try JSONDecoder().decode(
            AmbientActivityAttributes.ContentState.self,
            from: Data(#"{"phase":"listening","caption":"hi","captionRole":"agent"}"#.utf8)
        )
        XCTAssertEqual(agent.captionRole, .agent)

        let user = try JSONDecoder().decode(
            AmbientActivityAttributes.ContentState.self,
            from: Data(#"{"phase":"listening","caption":"hi","captionRole":"user"}"#.utf8)
        )
        XCTAssertEqual(user.captionRole, .user)
    }

    /// The tolerance above must not be mistaken for "the new fields never carry":
    /// the wire format is what ActivityKit persists, so they have to survive it.
    func testNewFieldsRoundTrip() throws {
        let state = AmbientActivityAttributes.ContentState(
            phase: .speaking, caption: "On it.", captionRole: .agent, exchangeCount: 3,
            endedAt: Date(timeIntervalSince1970: 1000),
            expiresAt: Date(timeIntervalSince1970: 2000))
        let decoded = try JSONDecoder().decode(AmbientActivityAttributes.ContentState.self,
                                               from: JSONEncoder().encode(state))
        XCTAssertEqual(decoded.captionRole, .agent)
        XCTAssertEqual(decoded.exchangeCount, 3)
        XCTAssertEqual(decoded.endedAt, Date(timeIntervalSince1970: 1000))
        XCTAssertEqual(decoded.expiresAt, Date(timeIntervalSince1970: 2000))
        XCTAssertEqual(
            decoded.effectiveExpiresAt(fallback: Date(timeIntervalSince1970: 9_999)),
            Date(timeIntervalSince1970: 2000)
        )
    }
}
