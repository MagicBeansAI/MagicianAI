import Foundation
import os

/// Identity of one disarm request, so an acknowledgement can be matched to the
/// tap that asked for it rather than to any tap at all.
///
/// A bare "the app acknowledged" flag would be satisfied by an answer to an
/// EARLIER question — an acknowledgement written just after a previous request
/// gave up waiting is still sitting in the container when the next tap arrives.
/// The intent would then read that as "the microphone is off", when nothing has
/// heard the new request at all.
public struct AmbientDisarmRequest: Equatable, Sendable {
    public let id: String

    public init(id: String) { self.id = id }
}

/// Identity of one request to add `AmbientExtensionPolicy.incrementSeconds` to
/// the live window. The amount is policy, not payload: both processes compile
/// the same Shared source, so a stale or malformed record cannot ask the app to
/// create an unbounded microphone lease.
public struct AmbientExtensionRequest: Equatable, Sendable {
    public let id: String

    public init(id: String) { self.id = id }
}

/// The cross-process disarm channel between the orb's Disarm button (which runs
/// in the WIDGET process) and the app that owns the microphone.
///
/// `StopObservationIntent`'s approach does not transfer. That intent runs with
/// `openAppWhenRun = false` and works because it stops a *server-side* session;
/// the in-app capture discovers this on its next chunk upload, via a `410`.
/// Ambient has no server session while armed — it is a purely local microphone
/// tap — so there is nothing for the app to trip over. The intent has to signal
/// the app process directly, and then find out whether it was heard.
///
/// **The orb disappearing must never be optimistic.** The naive shape — set a
/// flag, post a notification, end the activity — produces this feature's
/// signature failure: the orb vanishes, the app never got the signal, and the
/// microphone keeps running. The user now believes they are not being heard, and
/// they are. An orb that stays up is far better than an orb that lies.
///
/// So the channel is an acknowledgement, and it runs over two routes that fail
/// independently:
///
/// - **A Darwin notification** (`CFNotificationCenterGetDarwinNotifyCenter`),
///   which is cross-process and immediate, and is what makes the tap feel
///   instant. It carries no payload and reaches only a process that is running.
/// - **A record in the App Group**, which persists. It carries the request's
///   identity, and it is the only evidence available to an app that was not
///   listening when the tap happened.
///
/// The app answers by writing an acknowledgement carrying the same identity —
/// *after* it has actually stopped. See `DisarmAmbientIntent` for what the
/// absence of one is allowed to mean.
public enum AmbientSignal {

    /// The Darwin notification name. It is a process-global string rather than a
    /// typed constant on either side, so it is spelled once here and read by both
    /// binaries from this one declaration — `Shared/` is compiled into the app
    /// and the widget extension alike, which is why this file lives there.
    public static let disarmNotification = "ai.magicbeans.magician.ambient.disarm"
    public static let extensionNotification = "ai.magicbeans.magician.ambient.extend"

    /// Spelled out rather than derived, for the reason `AmbientArmTests` gives
    /// about `ambient.activeArm`: these keys are a cross-process contract, and a
    /// rename that changes one side would produce no compile error anywhere.
    private static let pendingKey = "ambient.pendingDisarm"
    private static let acknowledgementKey = "ambient.disarmAck"
    private static let pendingExtensionKey = "ambient.pendingExtension"

    /// Same storage shape as `AmbientArm`: the App Group suite, computed per
    /// access. Values are plain strings rather than encoded records, because the
    /// only field either record needs is the identity — which also means this
    /// channel has no schema to drift and no undecodable-bytes case to collect.
    private static var store: UserDefaults {
        UserDefaults(suiteName: MagicianAccess.appGroup) ?? .standard
    }

    // MARK: - The request

    /// Publish a disarm request and return its identity.
    ///
    /// Any acknowledgement left over from an earlier request is dropped first, so
    /// the container does not accumulate answers to questions nobody is asking.
    /// That is hygiene, not the safety property: the identity match in
    /// `consumeAcknowledgement(of:)` is what actually stops a stale answer from
    /// satisfying a fresh request, and it holds even if this clear never ran.
    @discardableResult
    public static func requestDisarm() -> AmbientDisarmRequest {
        let request = AmbientDisarmRequest(id: UUID().uuidString)
        store.removeObject(forKey: acknowledgementKey)
        store.set(request.id, forKey: pendingKey)
        return request
    }

    /// Read the outstanding request WITHOUT consuming it.
    ///
    /// This is the intent's evidence check, and it must not consume: the app may
    /// still be about to read the same record.
    public static func pendingDisarm() -> AmbientDisarmRequest? {
        store.string(forKey: pendingKey).map(AmbientDisarmRequest.init(id:))
    }

    /// Read and remove — the app's side. A request is answered once.
    public static func consumePendingDisarm() -> AmbientDisarmRequest? {
        guard let request = pendingDisarm() else { return nil }
        store.removeObject(forKey: pendingKey)
        return request
    }

    public static func clearPendingDisarm() {
        store.removeObject(forKey: pendingKey)
    }

    // MARK: - The acknowledgement

    /// Written by the app, and only ever AFTER the microphone is actually off.
    /// It is the single fact the intent is allowed to treat as "it really
    /// stopped".
    public static func acknowledgeDisarm(_ request: AmbientDisarmRequest) {
        store.set(request.id, forKey: acknowledgementKey)
    }

    /// True only for an acknowledgement of THIS request, and true only once.
    public static func consumeAcknowledgement(of request: AmbientDisarmRequest) -> Bool {
        guard store.string(forKey: acknowledgementKey) == request.id else { return false }
        store.removeObject(forKey: acknowledgementKey)
        return true
    }

    public static func clearAcknowledgement() {
        store.removeObject(forKey: acknowledgementKey)
    }

    // MARK: - The extension request

    /// Publish a single +30 minute request. Replacing a still-pending identity
    /// deliberately coalesces frantic repeated taps into one extension rather
    /// than letting a delayed widget process queue hours of listening at once.
    @discardableResult
    public static func requestExtension() -> AmbientExtensionRequest {
        let request = AmbientExtensionRequest(id: UUID().uuidString)
        store.set(request.id, forKey: pendingExtensionKey)
        return request
    }

    /// Read and remove — the live app applies a request at most once.
    public static func consumePendingExtension() -> AmbientExtensionRequest? {
        guard let id = store.string(forKey: pendingExtensionKey) else { return nil }
        store.removeObject(forKey: pendingExtensionKey)
        return AmbientExtensionRequest(id: id)
    }

    public static func clearPendingExtension() {
        store.removeObject(forKey: pendingExtensionKey)
    }

    /// How a wait for an acknowledgement ended.
    ///
    /// Three cases rather than a `Bool`, because "no acknowledgement" is two
    /// completely different facts and only one of them is evidence:
    ///
    /// - `silent` means nobody answered **in a full budget**. That is what makes
    ///   an outstanding request meaningful — the app was given its chance and did
    ///   not take it.
    /// - `cancelled` means the wait was cut short, which can happen after a single
    ///   poll interval. Nothing has been learned about the app at all: it has
    ///   simply not had time. Collapsing this into `silent` is how a healthy,
    ///   still-listening app gets its orb collected ~20 ms after the tap — this
    ///   feature's signature failure, reached without a stall, a dead app or any
    ///   unusual state.
    ///
    /// The distinction is a type rather than a comment because it has to survive
    /// at the call site, which is where the destructive branch lives.
    public enum DisarmWaitOutcome: Equatable, Sendable {
        case acknowledged
        case silent
        case cancelled
    }

    /// Wait briefly for the app to confirm it stopped.
    ///
    /// Polling rather than observing: cross-process `UserDefaults` change
    /// notification is not something to build a microphone's off-switch on, and
    /// the identity has to be re-read from the container either way. Fifty reads
    /// of one string is not a cost worth a mechanism. The first read happens
    /// before the first sleep, so an app that answers immediately costs the wait
    /// nothing.
    public static func awaitAcknowledgement(
        of request: AmbientDisarmRequest,
        within budget: TimeInterval,
        pollInterval: TimeInterval = 0.02
    ) async -> DisarmWaitOutcome {
        let deadline = Date().addingTimeInterval(budget)
        while true {
            if consumeAcknowledgement(of: request) { return .acknowledged }
            guard Date() < deadline else { return .silent }
            do {
                try await Task.sleep(nanoseconds: UInt64(max(0, pollInterval) * 1_000_000_000))
            } catch {
                // `Task.sleep` throws only on cancellation. Check once more —
                // an answer may already be sitting there — and otherwise report
                // the cancellation as itself.
                return consumeAcknowledgement(of: request) ? .acknowledged : .cancelled
            }
        }
    }

    // MARK: - The Darwin notification

    /// Post the disarm signal. Reaches every running process immediately; a
    /// process that is not running does not hear it, which is exactly the case
    /// the App Group record covers.
    public static func postDisarm() {
        CFNotificationCenterPostNotification(
            CFNotificationCenterGetDarwinNotifyCenter(),
            CFNotificationName(disarmNotification as CFString),
            nil,
            nil,
            true
        )
    }

    public static func postExtension() {
        CFNotificationCenterPostNotification(
            CFNotificationCenterGetDarwinNotifyCenter(),
            CFNotificationName(extensionNotification as CFString),
            nil,
            nil,
            true
        )
    }

    /// Handler and registration flag under one lock, because they are one fact:
    /// a handler stored without a registration never fires, and a registration
    /// without a handler is a callback into nothing.
    private struct Observation: Sendable {
        var handler: (@Sendable () -> Void)?
        var isRegistered = false
    }

    private static let observation = OSAllocatedUnfairLock(initialState: Observation())
    private static let extensionObservation = OSAllocatedUnfairLock(initialState: Observation())

    /// `CFNotificationCenterAddObserver` identifies an observer by raw pointer,
    /// and the same pointer has to be handed back to remove it. A process-wide
    /// object's address is that pointer: it is stable for the life of the process
    /// and belongs to nothing else.
    private final class ObserverIdentity: @unchecked Sendable {}
    private static let observerIdentity = ObserverIdentity()
    private static let extensionObserverIdentity = ObserverIdentity()
    private static var observerPointer: UnsafeRawPointer {
        UnsafeRawPointer(Unmanaged.passUnretained(observerIdentity).toOpaque())
    }
    private static var extensionObserverPointer: UnsafeRawPointer {
        UnsafeRawPointer(Unmanaged.passUnretained(extensionObserverIdentity).toOpaque())
    }

    /// Whether a Darwin observer is currently registered. Exists so the
    /// registration LIFECYCLE is assertable in-process — the notification itself
    /// needs two processes and is verified on device.
    public static var isObservingDisarm: Bool {
        observation.withLock { $0.isRegistered }
    }

    public static var isObservingExtension: Bool {
        extensionObservation.withLock { $0.isRegistered }
    }

    /// Start listening for the disarm signal. Process-wide, and last writer wins:
    /// there is exactly one armed window per process, so a second registration
    /// replaces the handler rather than stacking a second observer that would
    /// disarm the same window twice.
    ///
    /// The callback is a C function pointer and can capture nothing, which is why
    /// the handler lives in the box above rather than in the closure.
    public static func startObservingDisarm(_ handler: @escaping @Sendable () -> Void) {
        let wasRegistered = observation.withLock { state -> Bool in
            let was = state.isRegistered
            state.handler = handler
            state.isRegistered = true
            return was
        }
        guard !wasRegistered else { return }
        CFNotificationCenterAddObserver(
            CFNotificationCenterGetDarwinNotifyCenter(),
            observerPointer,
            { _, _, _, _, _ in AmbientSignal.deliverDisarm() },
            disarmNotification as CFString,
            nil,
            .deliverImmediately
        )
    }

    public static func stopObservingDisarm() {
        let wasRegistered = observation.withLock { state -> Bool in
            let was = state.isRegistered
            state.handler = nil
            state.isRegistered = false
            return was
        }
        guard wasRegistered else { return }
        CFNotificationCenterRemoveObserver(
            CFNotificationCenterGetDarwinNotifyCenter(),
            observerPointer,
            CFNotificationName(disarmNotification as CFString),
            nil
        )
    }

    /// Start listening for the Live Activity's +30 minute button. Separate
    /// identity and registration state from Disarm ensure stopping one channel
    /// cannot silently unregister the other.
    public static func startObservingExtension(_ handler: @escaping @Sendable () -> Void) {
        let wasRegistered = extensionObservation.withLock { state -> Bool in
            let was = state.isRegistered
            state.handler = handler
            state.isRegistered = true
            return was
        }
        guard !wasRegistered else { return }
        CFNotificationCenterAddObserver(
            CFNotificationCenterGetDarwinNotifyCenter(),
            extensionObserverPointer,
            { _, _, _, _, _ in AmbientSignal.deliverExtension() },
            extensionNotification as CFString,
            nil,
            .deliverImmediately
        )
    }

    public static func stopObservingExtension() {
        let wasRegistered = extensionObservation.withLock { state -> Bool in
            let was = state.isRegistered
            state.handler = nil
            state.isRegistered = false
            return was
        }
        guard wasRegistered else { return }
        CFNotificationCenterRemoveObserver(
            CFNotificationCenterGetDarwinNotifyCenter(),
            extensionObserverPointer,
            CFNotificationName(extensionNotification as CFString),
            nil
        )
    }

    /// Read the handler under the lock, call it outside — the handler hops to the
    /// main actor and re-enters this type to stop observing, which would deadlock
    /// against a lock still held here.
    private static func deliverDisarm() {
        let handler = observation.withLock { $0.handler }
        handler?()
    }

    private static func deliverExtension() {
        let handler = extensionObservation.withLock { $0.handler }
        handler?()
    }
}
