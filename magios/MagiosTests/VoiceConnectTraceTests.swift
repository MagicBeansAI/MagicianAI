import XCTest
@testable import Magician

/// The connect-latency line, asserted as the pure formatting it is.
///
/// **Deliberately no test that a clock advances.** The stamping is a monotonic
/// read at a call site, and a test that slept and then asserted a positive delta
/// would prove `systemUptime` works while saying nothing about the only thing that
/// can actually be wrong here: whether the line a person reads off a device
/// console attributes the time to the right stage, adds up, and still says
/// something useful when the connect died. That is all pure, so all of it is here.
final class VoiceConnectTraceTests: XCTestCase {

    /// Marks at whole-millisecond offsets from an arbitrary uptime origin, so the
    /// expected line can be written by hand.
    private func trace(
        mode: VoiceConnectTrace.Mode,
        _ stages: [(VoiceConnectTrace.Stage, Int)]
    ) -> VoiceConnectTrace {
        let origin: TimeInterval = 12_345.678
        var trace = VoiceConnectTrace(
            mode: mode,
            first: stages[0].0,
            at: origin + TimeInterval(stages[0].1) / 1000
        )
        for (stage, ms) in stages.dropFirst() {
            trace.mark(stage, at: origin + TimeInterval(ms) / 1000)
        }
        return trace
    }

    /// The whole shape of the line the owner is being asked to read back, in one
    /// assertion, because every field of it is load-bearing: the tag they filter
    /// on, which surface it was, whether it got there, the total the complaint is
    /// about, and the per-stage attribution that decides which fix is the right
    /// one.
    func testASuccessfulAmbientConnectPrintsEveryStage() {
        let line = trace(mode: .ambient, [
            (.wake, 0),
            (.handoff, 3),
            (.start, 44),
            (.post, 224),
            (.postOK, 1_404),
            (.ws, 1_405),
            (.wsUp, 1_619),
            (.ready, 8_509),
            (.flush, 8_521),
        ]).summary(outcome: .ready, detail: "dropped_b=96000")

        XCTAssertEqual(
            line,
            """
            voice.connect mode=ambient outcome=ready total=8509ms deltas_ms: \
            wake>handoff=3 handoff>start=41 start>post=180 post>post_ok=1180 \
            post_ok>ws=1 ws>ws_up=214 ws_up>ready=6890 ready>flush=12 dropped_b=96000
            """
        )
    }

    /// **The property the whole format is built around.** The deltas are gaps
    /// between consecutive present stages, so everything from the first stamp to
    /// `ready` is accounted for by exactly one of them — nothing hides in a gap
    /// between two segments, which is precisely how a "we measured it" answer would
    /// otherwise still be wrong.
    func testTheDeltasUpToReadySumToTheTotal() {
        let line = trace(mode: .ambient, [
            (.wake, 0), (.handoff, 3), (.start, 44), (.post, 224),
            (.postOK, 1_404), (.ws, 1_405), (.wsUp, 1_619), (.ready, 8_509),
            (.flush, 8_521),
        ]).summary(outcome: .ready)

        let deltas = line
            .split(separator: " ")
            .filter { $0.contains(">") }
            .compactMap { Int($0.split(separator: "=")[1]) }
        // Every segment except the post-ready flush.
        XCTAssertEqual(deltas.dropLast().reduce(0, +), 8_509)
        XCTAssertEqual(deltas.last, 12)
    }

    /// An in-app call has no wake and no handoff, and the line must not print zeros
    /// for them — a `wake>handoff=0` on a call nobody woke would read as "the wake
    /// hop is free", which is a claim about the very hop this exists to price.
    /// Absent stages merge into their neighbour instead.
    func testAnInAppConnectOmitsTheStagesItNeverHad() {
        let line = trace(mode: .inApp, [
            (.start, 0), (.post, 190), (.postOK, 900), (.ws, 901),
            (.wsUp, 1_100), (.ready, 7_600), (.flush, 7_601),
        ]).summary(outcome: .ready, detail: "dropped_b=0")

        XCTAssertTrue(line.hasPrefix("voice.connect mode=in-app outcome=ready total=7600ms"))
        XCTAssertFalse(line.contains("wake"))
        XCTAssertFalse(line.contains("handoff"))
        XCTAssertTrue(line.contains("start>post=190"))
        XCTAssertTrue(line.contains("ws_up>ready=6500"))
    }

    /// A connect that dies is as informative as one that succeeds slowly, and from
    /// outside they look identical — so the line has to name the stage it stopped
    /// at, and the total has to be how long the user actually waited, not how long
    /// it took to reach the last stage it managed.
    func testAFailedConnectNamesTheStageItDiedAtAndCountsTheDeadTime() {
        let line = trace(mode: .ambient, [
            (.wake, 0), (.handoff, 4), (.start, 42), (.post, 218),
            (.postOK, 428), (.ws, 429), (.wsUp, 517), (.end, 30_517),
        ]).summary(outcome: .failed, error: "Voice call didn't start in time.")

        XCTAssertTrue(line.contains("outcome=failed"))
        // `ws_up` and not `end`: `end` is where it stopped, not what it got through.
        XCTAssertTrue(line.contains("at=ws_up"))
        XCTAssertTrue(line.contains("total=30517ms"))
        XCTAssertTrue(line.contains("ws_up>end=30000"))
        XCTAssertTrue(line.hasSuffix(#"err="Voice call didn't start in time.""#))
    }

    /// A teardown before ready is not a failure — a disarm, the orb's button, a
    /// server `session.end` all land here — but it is still the user waiting and
    /// not being answered, so it prints with its own outcome rather than as a
    /// failure or as nothing at all.
    func testAnAbandonedConnectIsReportedAsEndedRatherThanFailed() {
        let line = trace(mode: .ambient, [
            (.wake, 0), (.handoff, 3), (.start, 40), (.post, 210), (.end, 1_900),
        ]).summary(outcome: .ended, error: nil)

        XCTAssertTrue(line.contains("outcome=ended at=post total=1900ms"))
        XCTAssertFalse(line.contains("err="))
    }

    /// The error text arrives over the network — a backend can hand back a whole
    /// HTML page as a "detail" — and this format's single guarantee is that one
    /// connect is one line. A newline in that text would break it into several, and
    /// a double quote would close the field early.
    func testAServerErrorCannotBreakTheLineApart() {
        let messy = "Media session registration failed (502).\n<html>\"gateway\"</html>\n"
        let line = trace(mode: .ambient, [(.wake, 0), (.post, 100), (.end, 300)])
            .summary(outcome: .failed, error: messy)

        XCTAssertFalse(line.contains("\n"))
        XCTAssertEqual(line.filter { $0 == "\"" }.count, 2)
        XCTAssertTrue(line.contains(#"err="Media session registration failed (502). <html>'gateway'</html>""#))
    }

    func testALongErrorIsTruncatedRatherThanFloodingTheLine() {
        let flooded = String(repeating: "x", count: 400)
        let line = trace(mode: .inApp, [(.start, 0), (.end, 10)])
            .summary(outcome: .failed, error: flooded)

        XCTAssertTrue(line.contains(String(repeating: "x", count: 180) + "…"))
        XCTAssertLessThan(line.count, 300)
    }

    /// `session.ready`, `openControl` and the `session.start` send are each
    /// reachable more than once per call — a reconnect re-runs the handshake, a
    /// rotation returns to `.ready`. This trace is about the FIRST connect, so a
    /// later stamp must not silently re-time the attempt against a second socket.
    func testALaterStampDoesNotOverwriteTheFirstConnect() {
        var trace = VoiceConnectTrace(mode: .ambient, first: .wake, at: 100)
        trace.mark(.ready, at: 108)
        trace.mark(.ready, at: 140)

        XCTAssertEqual(trace.time(of: .ready), 108)
        XCTAssertTrue(trace.summary(outcome: .ready).contains("total=8000ms"))
    }

    /// Sub-millisecond stages are the common case for the local hops, and rounding
    /// is what keeps them readable — but rounding must not turn a real 0.6 ms hop
    /// into a `0` that reads as "nothing happened here".
    func testDurationsRoundToTheNearestMillisecond() {
        XCTAssertEqual(VoiceConnectTrace.millis(from: 0, to: 0.0006), 1)
        XCTAssertEqual(VoiceConnectTrace.millis(from: 0, to: 0.0004), 0)
        XCTAssertEqual(VoiceConnectTrace.millis(from: 1.5, to: 9.8217), 8_322)
    }

    /// Not clamped, on purpose: a monotonic clock cannot produce a negative delta,
    /// so one would mean a stamping bug — and clamping would present that bug as a
    /// fast stage, which is the single most expensive way this instrumentation
    /// could lie.
    func testABackwardsDeltaIsPrintedRatherThanClamped() {
        XCTAssertEqual(VoiceConnectTrace.millis(from: 5, to: 4), -1_000)
    }

    /// `.end` is bookkeeping, not a stage of the connect; `reached` is what a
    /// failure is named by and must skip it.
    func testReachedIgnoresTheTerminalMark() {
        var trace = VoiceConnectTrace(mode: .inApp, first: .start, at: 0)
        trace.mark(.post, at: 0.2)
        trace.mark(.end, at: 30)
        XCTAssertEqual(trace.reached, .post)
    }

    /// The ambient orb and the in-app panel share one transport, so the line has to
    /// say which one it came from — that answer is what decides whether this is an
    /// ambient defect or the voice stack's cold-start cost showing up on every
    /// wake.
    func testTheModeIsAlwaysStated() {
        for mode in [VoiceConnectTrace.Mode.ambient, .inApp] {
            let line = VoiceConnectTrace(mode: mode, first: .start, at: 0)
                .summary(outcome: .superseded)
            XCTAssertTrue(line.contains("mode=\(mode.rawValue)"))
        }
    }

    /// The mapping the call surfaces are labelled by, asserted because the two
    /// enums are deliberately separate types and nothing else would catch them
    /// being wired to each other backwards.
    func testCallModesMapToTheirTraceLabels() {
        XCTAssertEqual(VoiceCallMode.ambient.connectTraceMode, .ambient)
        XCTAssertEqual(VoiceCallMode.inApp.connectTraceMode, .inApp)
    }
}
