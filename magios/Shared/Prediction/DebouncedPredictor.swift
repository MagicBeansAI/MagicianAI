import Foundation

/// Debounce + cancellation wrapper around any `NextWordPredictor`.
///
/// Typing fires a prediction request on (nearly) every keystroke after a space. We
/// must not run the model on each one: `DebouncedPredictor` coalesces bursts into a
/// single call and cancels any in-flight prediction when newer input arrives, so the
/// suggestion strip updates smoothly and the model work never piles up. All of this
/// runs off the keypress thread (the caller `await`s from a background `Task`).
///
/// Usage: call `request(context:)` on every after-space refresh; the completion runs
/// only for the *latest* request that survives the debounce window. Older requests are
/// dropped (their completions never fire). Guarantees:
///   - At most one prediction task is in flight at a time.
///   - A newer `request` cancels the pending debounce AND any running prediction.
///   - `completion` runs at most once per surviving request, with cleaned output.
public actor DebouncedPredictor {
    private let predictor: NextWordPredictor
    private let debounceNanos: UInt64
    private let limit: Int

    /// The current in-flight (debounce + predict) task, cancelled when superseded.
    private var current: Task<Void, Never>?
    /// Monotonic request id so a late completion from a superseded task is ignored.
    private var latestRequestID: UInt64 = 0

    /// - Parameters:
    ///   - predictor: the underlying model (FM in prod, a stub in tests).
    ///   - debounceMillis: coalescing window; ~200 ms per the design.
    ///   - limit: max candidates surfaced (strip width).
    public init(predictor: NextWordPredictor, debounceMillis: UInt64 = 200, limit: Int = 3) {
        self.predictor = predictor
        self.debounceNanos = debounceMillis * 1_000_000
        self.limit = limit
    }

    /// Request predictions for `context`, cancelling any pending/in-flight request.
    /// `completion` runs (with cleaned, capped output) only if this request survives
    /// the debounce window and isn't superseded before it finishes. It never runs for
    /// a cancelled/superseded request.
    public func request(context: String, completion: @escaping @Sendable ([String]) -> Void) {
        latestRequestID &+= 1
        let requestID = latestRequestID
        current?.cancel()

        // Detached so the debounce sleep + model call run OFF this actor (they must not
        // serialize `request`/`cancel`); we hop back onto the actor only to deliver.
        current = Task.detached(priority: .userInitiated) { [predictor, debounceNanos, limit] in
            // Debounce: wait out the coalescing window; a newer request cancels us here.
            try? await Task.sleep(nanoseconds: debounceNanos)
            if Task.isCancelled { return }

            let raw = await predictor.predict(context: context)
            if Task.isCancelled { return }

            let cleaned = PredictionCleanup.clean(raw, limit: limit)

            // Re-hop onto the actor and guard against a superseded task that raced past
            // the cancel check (a newer request bumped `latestRequestID`).
            await self.deliver(cleaned, forRequestID: requestID, completion: completion)
        }
    }

    /// Deliver a completed prediction only if its request is still the latest. Running
    /// on the actor makes the `latestRequestID` read + `completion` call atomic w.r.t.
    /// new requests, so a superseded completion can never fire.
    private func deliver(_ words: [String], forRequestID requestID: UInt64,
                         completion: @escaping @Sendable ([String]) -> Void) {
        guard requestID == latestRequestID else { return }
        completion(words)
    }

    /// Cancel any pending/in-flight prediction (e.g. we switched to the corrections
    /// slot or entered a secure field). Idempotent.
    public func cancel() {
        current?.cancel()
        current = nil
        latestRequestID &+= 1  // invalidate any completion still racing
    }
}
