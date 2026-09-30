import Foundation
import os

/// One realtime-voice connect attempt's stage timings, and the single line they
/// print as.
///
/// ## Why this exists
///
/// On a real device the ambient wake handoff takes 8–10 s to reach a reply, and
/// the two costs anyone can name — a 2.5 s post-conversation resume cooldown and
/// ~1.2 s of finals-mode wake detection — do not account for it. The candidates
/// that remain have completely different fixes: a parked provider session helps
/// only if provider-session creation is where the time goes, and does nothing at
/// all for a cold DNS, a retry loop or an auth round trip. So the first job is to
/// measure rather than to guess, and the stage this measurement exists to isolate
/// cleanly is `ws_up>ready` — the wait on the backend, which is what
/// `session.ready` gates on.
///
/// It instruments the **in-app** Live call on the same path, which is nearly free
/// because `RealtimeVoiceClient` is shared, and which answers a question that
/// reframes the problem: if tapping Live also takes 8–10 s to reach ready, this is
/// not an ambient defect at all but the voice stack's cold-start cost — ambient
/// merely pays it on every wake instead of once per tap. Hence `mode=` on the
/// line, first field after the tag.
///
/// ## Why ONE line
///
/// The reading happens off a device console and gets pasted back by hand. Five
/// interleaved lines in a busy log are worse than one wide one: they have to be
/// found, ordered and correlated first, and a connect that fails halfway leaves a
/// stage's line simply missing rather than an outcome stated.
///
/// ## PERMANENT, not scaffolding — the choice, stated
///
/// This is meant to stay. What it costs a connect is a monotonic timestamp per
/// stage (one `mach_absolute_time` read), a nine-element array, and one `.notice`
/// log per attempt — set against a path that already performs an HTTP
/// registration and a WebSocket upgrade. Nothing here awaits, blocks, allocates on
/// the audio render thread, or changes an ordering, so leaving it in is free at
/// the only place where "free" matters. Removing it would also mean re-deriving
/// all of this the next time the question comes back, which it will.
struct VoiceConnectTrace: Sendable, Equatable {

    /// Which call this was, so an in-app Live call and an ambient wake handoff are
    /// never read as each other in a shared log.
    enum Mode: String, Sendable {
        case ambient
        case inApp = "in-app"
    }

    /// The instants worth separating, in the order they occur.
    ///
    /// `CaseIterable` **and ordered**, because the printed deltas are the gaps
    /// between *consecutive present* stages. That makes the segments a partition of
    /// the elapsed time rather than a sample of it — they always add up — and a
    /// stage that never happened merges into its neighbour instead of printing a
    /// zero that reads like "instant".
    enum Stage: String, Sendable, CaseIterable {
        /// The on-device wake spotter fired. Ambient only.
        case wake
        /// `AmbientController.handoff` began, i.e. the `Task` the wake enqueued got
        /// to run. Separated from `wake` precisely to price that hop.
        case handoff
        /// `VoiceCallViewModel.startCall` entered. The gap before it is the mic
        /// stop.
        case start
        /// The `POST …/v2/media/sessions` registration was issued. The gap before
        /// it is local: the audio graph starting, and the settings snapshot.
        case post
        /// …and returned.
        case postOK = "post_ok"
        /// The control WebSocket task was resumed.
        case ws
        /// The `session.start` frame left the socket — which is the socket being
        /// **up**: `URLSessionWebSocketTask` queues a send until the HTTP upgrade
        /// completes, so the send completing without error is the handshake having
        /// succeeded. Measured from the send's own completion handler rather than
        /// from a delegate, so nothing about the transport's wiring changes.
        case wsUp = "ws_up"
        /// `session.ready` applied. **The prime suspect**: this is what waits on
        /// backend provider-session creation, prompt/tool/context setup and any
        /// resume-context compaction.
        case ready
        /// `.ready` handling completed in the view model. Since the
        /// no-pre-ready-audio decision (2026-07-30) nothing flushes — the
        /// pre-ready gate opens and its drop count is recorded — but the stage
        /// and its raw value survive so long-lived trace tooling keeps its keys.
        case flush
        /// Where an attempt that never reached ready actually stopped. **Not a
        /// stage of the connect** — it is excluded from `reached` — but it is what
        /// keeps `total` honest, so a connect that dies at 30 s reports 30 s
        /// instead of the few hundred milliseconds it took to get to its last real
        /// stage.
        case end
    }

    enum Outcome: String, Sendable {
        /// Reached `session.ready`; the pre-ready gate opened and hearing began.
        case ready
        /// The transport failed: a registration error, a socket error, or the 45 s
        /// connect watchdog.
        case failed
        /// Torn down before ready with no failure — a disarm, the orb's button, a
        /// server `session.end`. Reported because from outside, a 30 s failure and
        /// a 30 s abandonment look identical, and they are not the same bug.
        case ended
        /// A newer attempt began before this one finished. Printed rather than
        /// dropped, so an abandoned attempt is visible as abandoned.
        case superseded
    }

    struct Mark: Sendable, Equatable {
        let stage: Stage
        let at: TimeInterval
    }

    let mode: Mode
    private(set) var marks: [Mark]

    init(mode: Mode, first: Stage, at: TimeInterval) {
        self.mode = mode
        self.marks = [Mark(stage: first, at: at)]
    }

    /// **First write wins.** `session.ready`, `openControl` and the `session.start`
    /// send are all reachable more than once per call — a reconnect re-runs the
    /// handshake and a rotation returns to `.ready` — and this trace is about the
    /// FIRST connect. A later stamp overwriting an earlier one would silently
    /// re-time the attempt against a socket that came up second.
    mutating func mark(_ stage: Stage, at: TimeInterval) {
        guard time(of: stage) == nil else { return }
        marks.append(Mark(stage: stage, at: at))
    }

    func time(of stage: Stage) -> TimeInterval? {
        marks.first { $0.stage == stage }?.at
    }

    /// The stages present, in canonical order rather than arrival order.
    ///
    /// Ordered by the enum instead of by `marks` so a stamp that lands out of order
    /// — which a completion handler on another queue can do — produces a delta that
    /// is merely odd rather than a timeline that runs backwards.
    var ordered: [Mark] {
        Stage.allCases.compactMap { stage in
            time(of: stage).map { Mark(stage: stage, at: $0) }
        }
    }

    /// The furthest real stage reached — what a failure is named by. Excludes
    /// `.end`, which is where it stopped, not what it got through.
    var reached: Stage? {
        Stage.allCases.last { $0 != .end && time(of: $0) != nil }
    }

    /// The one line.
    ///
    /// `total` is deliberately **wake → ready** (or, for an attempt that never got
    /// there, wake → wherever it stopped), so it is the number the complaint is
    /// about and the deltas up to `ready` sum to it exactly. The `ready>flush`
    /// delta sits after the total for that reason: it is post-ready handling —
    /// the pre-ready gate opening and its drop count being recorded, nothing
    /// flushes anymore — happening once the assistant is already reachable.
    func summary(outcome: Outcome, error: String? = nil, detail: String? = nil) -> String {
        let present = ordered
        var fields = ["voice.connect", "mode=\(mode.rawValue)", "outcome=\(outcome.rawValue)"]
        if outcome != .ready, let stage = reached {
            fields.append("at=\(stage.rawValue)")
        }
        if let first = present.first {
            let end = time(of: .ready) ?? present.last?.at ?? first.at
            fields.append("total=\(Self.millis(from: first.at, to: end))ms")
        }
        if present.count > 1 {
            fields.append("deltas_ms:")
            for (from, to) in zip(present, present.dropFirst()) {
                fields.append("\(from.stage.rawValue)>\(to.stage.rawValue)=\(Self.millis(from: from.at, to: to.at))")
            }
        }
        if let detail, !detail.isEmpty { fields.append(detail) }
        if let error, !error.isEmpty { fields.append("err=\"\(Self.oneLine(error))\"") }
        return fields.joined(separator: " ")
    }

    /// Rounded to the nearest millisecond.
    ///
    /// A negative result is **not** clamped. It is impossible from a monotonic
    /// clock and would therefore mean a stamping bug; printing `-4` says so, while
    /// clamping to `0` would present the bug as a fast stage.
    static func millis(from: TimeInterval, to: TimeInterval) -> Int {
        Int(((to - from) * 1000).rounded())
    }

    /// Keep a server-supplied error inside the one line it is a field of.
    ///
    /// Newlines would split the line the whole design depends on being one, and a
    /// double quote would close the field early — both from text that arrives over
    /// the network, i.e. from outside this app's control. Truncated because a
    /// backend can return a whole HTML error page as a "detail".
    static func oneLine(_ text: String, limit: Int = 180) -> String {
        let flattened = text
            .replacingOccurrences(of: "\"", with: "'")
            .split(whereSeparator: \.isNewline)
            .joined(separator: " ")
            .trimmingCharacters(in: .whitespaces)
        guard flattened.count > limit else { return flattened }
        return String(flattened.prefix(limit)) + "…"
    }
}

/// The one in-flight `VoiceConnectTrace`, stamped from every layer of the connect
/// path and printed once when the attempt ends.
///
/// **A process-wide singleton rather than a value threaded through the call stack,
/// and that is what keeps this behaviour-free.** The stages span three types that
/// deliberately do not know about each other — `AmbientController`,
/// `VoiceCallViewModel` and `RealtimeVoiceClient` — so carrying a token between
/// them would mean changing `AmbientCallSink.startCall`,
/// `VoiceCallViewModel.startCall` and `RealtimeVoiceClient.startCall`: real API
/// changes, in seams that exist to be independently testable, to pass a value only
/// a log line reads.
///
/// **One slot rather than a table**, because every reachable shape of this app has
/// at most one connect in flight: the ambient sink owns exactly one
/// `VoiceCallViewModel`, and the panel owns one more. A second attempt starting
/// while one is in flight prints the first as `superseded` rather than dropping
/// it, so the ambiguity is visible instead of silent.
///
/// Lock-guarded and non-isolated because one stamp happens on `URLSession`'s
/// delivery queue rather than the main actor. **Every mark takes its timestamp at
/// the call site and passes it in**, so a hop taken to record a stamp can never
/// inflate the thing being measured.
final class VoiceConnectTracer: Sendable {

    static let shared = VoiceConnectTracer()

    /// The clock every stamp is taken from: `mach_absolute_time`-backed, so it
    /// cannot step the way `Date()` can — and a wall-clock step mid-connect would
    /// land as a fabricated multi-second stage, which is exactly the reading this
    /// whole file exists to make trustworthy.
    ///
    /// That it does not advance across device sleep is correct here rather than a
    /// limitation: a connect that spanned a sleep is not an 8-second connect.
    static var now: TimeInterval { ProcessInfo.processInfo.systemUptime }

    private let inFlight = OSAllocatedUnfairLock<VoiceConnectTrace?>(initialState: nil)
    /// The categories already in place for these two surfaces, so the line lands
    /// where someone reading about that surface is already looking.
    private let ambientLog = Logger(subsystem: "ai.magicbeans.magios", category: "ambient.call")
    private let inAppLog = Logger(subsystem: "ai.magicbeans.magios", category: "voice.call")

    /// The ambient wake fired. Begins a trace.
    func wake(at: TimeInterval = VoiceConnectTracer.now) {
        supersede(with: VoiceConnectTrace(mode: .ambient, first: .wake, at: at))
    }

    /// `VoiceCallViewModel.startCall` entered — **and where an in-app trace
    /// begins**, because this is the first point on the shared path that knows
    /// which surface is connecting.
    ///
    /// An ambient trace begun at the wake is continued rather than replaced. A mode
    /// that disagrees with the trace in flight means the attempt in flight is not
    /// this one, so it is printed and a fresh one started.
    func callStart(mode: VoiceConnectTrace.Mode, at: TimeInterval = VoiceConnectTracer.now) {
        let superseded: VoiceConnectTrace? = inFlight.withLock { slot in
            if var trace = slot, trace.mode == mode {
                trace.mark(.start, at: at)
                slot = trace
                return nil
            }
            let previous = slot
            slot = VoiceConnectTrace(mode: mode, first: .start, at: at)
            return previous
        }
        if let superseded { emit(superseded, outcome: .superseded) }
    }

    /// Stamp a stage on the attempt in flight. A no-op when there is none, which is
    /// the ordinary case for a reconnect or a rotation after the line has printed.
    func mark(_ stage: VoiceConnectTrace.Stage, at: TimeInterval = VoiceConnectTracer.now) {
        inFlight.withLock { $0?.mark(stage, at: at) }
    }

    /// The assistant is reachable and hearing has begun. Prints, and clears the
    /// slot so nothing later in the call can print a second line. What it
    /// carries changed with the no-pre-ready-audio decision (2026-07-30): the
    /// byte count is what the gate DISCARDED before ready, not what a hold
    /// flushed — there is no hold left to flush.
    func finishReady(droppedPreReadyBytes: Int, at: TimeInterval = VoiceConnectTracer.now) {
        let done: VoiceConnectTrace? = inFlight.withLock { slot in
            guard var trace = slot else { return nil }
            trace.mark(.flush, at: at)
            slot = nil
            return trace
        }
        guard let done else { return }
        emit(done, outcome: .ready, detail: "dropped_b=\(droppedPreReadyBytes)")
    }

    /// The attempt ended without reaching ready. `failed` distinguishes a transport
    /// failure (including the 45 s watchdog) from an ordinary teardown; both print,
    /// because both are the user waiting and not being answered.
    func finishTerminal(failed: Bool, error: String?, at: TimeInterval = VoiceConnectTracer.now) {
        let done: VoiceConnectTrace? = inFlight.withLock { slot in
            guard var trace = slot else { return nil }
            trace.mark(.end, at: at)
            slot = nil
            return trace
        }
        guard let done else { return }
        emit(done, outcome: failed ? .failed : .ended, error: error)
    }

    private func supersede(with trace: VoiceConnectTrace) {
        let superseded: VoiceConnectTrace? = inFlight.withLock { slot in
            let previous = slot
            slot = trace
            return previous
        }
        if let superseded { emit(superseded, outcome: .superseded) }
    }

    /// `.notice`, deliberately: it is visible in Console.app with no extra levels
    /// enabled and it persists, so the line can be read after the fact rather than
    /// only while someone is watching. `.public` because every field is a stage
    /// name, a duration or an error the transport already surfaces — no transcript,
    /// no audio, no credential.
    private func emit(
        _ trace: VoiceConnectTrace,
        outcome: VoiceConnectTrace.Outcome,
        error: String? = nil,
        detail: String? = nil
    ) {
        let line = trace.summary(outcome: outcome, error: error, detail: detail)
        let log = trace.mode == .ambient ? ambientLog : inAppLog
        log.notice("\(line, privacy: .public)")
    }
}

extension VoiceCallMode {
    /// The trace's label for this surface.
    ///
    /// A separate type from `VoiceCallMode` on purpose: that enum answers three
    /// questions about audio sessions and turn boundaries, and must not acquire a
    /// fourth about logging vocabulary — nor should the trace acquire an opinion
    /// about audio sessions.
    var connectTraceMode: VoiceConnectTrace.Mode {
        switch self {
        case .ambient: return .ambient
        case .inApp: return .inApp
        }
    }
}
