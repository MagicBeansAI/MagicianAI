import Foundation

/// Phase-2a next-word prediction seam.
///
/// A `NextWordPredictor` proposes the words a user is most likely to type **next**
/// (after a completed word / a space), given the recent context before the cursor.
/// It is intentionally tiny and framework-free so it can be:
///   - implemented on-device by `FoundationModelsPredictor` (Apple Foundation Models,
///     behind availability gating), and
///   - stubbed in tests (`StubNextWordPredictor`) — the real FM call is a device-only
///     system dependency and is not unit-testable.
///
/// The contract: `predict` is async, must NEVER throw or crash, and returns at most a
/// handful of candidate words (already cleaned — no empties/whitespace). Callers cap
/// to the strip width. When prediction is unavailable it returns `[]` (a clean no-op).
public protocol NextWordPredictor: Sendable {
    /// Predict likely next words given the text before the cursor. Returns `[]` when
    /// nothing sensible can be predicted (or prediction is unavailable). Never throws.
    func predict(context: String) async -> [String]
}

/// A fixed-list predictor for tests and previews. Returns its `words` regardless of
/// context (optionally after a small delay, to exercise debounce/cancellation), so
/// tests can assert the wiring/decision/limiting logic without a real model.
public struct StubNextWordPredictor: NextWordPredictor {
    public let words: [String]
    /// Optional artificial latency (nanoseconds) to simulate a slow model. `nil` = instant.
    public let delayNanos: UInt64?

    public init(words: [String], delayNanos: UInt64? = nil) {
        self.words = words
        self.delayNanos = delayNanos
    }

    public func predict(context: String) async -> [String] {
        if let delayNanos {
            try? await Task.sleep(nanoseconds: delayNanos)
            // Honour cancellation so debounce/cancel tests observe it.
            if Task.isCancelled { return [] }
        }
        return words
    }
}

/// Tries its predictors in order and returns the FIRST non-empty result. This lets
/// Foundation Models take precedence when it actually generates, while guaranteeing the
/// universal n-gram predictor fills in whenever FM yields nothing — not only when FM is
/// reported unavailable, but also the real-device cases where FM reports `available` yet
/// can't generate (Apple-Intelligence assets still downloading, model-not-ready, a
/// guardrail refusal, or any error → `[]`). Without this, such a device would show no
/// predictions at all instead of falling back to the n-gram that works.
public struct CompositeNextWordPredictor: NextWordPredictor {
    private let predictors: [NextWordPredictor]
    public init(_ predictors: [NextWordPredictor]) { self.predictors = predictors }
    public func predict(context: String) async -> [String] {
        for predictor in predictors {
            let result = await predictor.predict(context: context)
            if !result.isEmpty { return result }
        }
        return []
    }
}

/// Which slot of the shared suggestion strip should be populated for the current
/// cursor position — the pure, testable decision that keeps Phase-1 correction and
/// Phase-2 prediction from fighting over the strip.
public enum StripSlot: Equatable {
    /// There is an in-progress word under/left-of the cursor → keep Phase-1
    /// corrections/completions (unchanged behaviour).
    case corrections
    /// The cursor is right after a word boundary (a space or start-of-field with no
    /// in-progress word) → fill the strip with next-word predictions.
    case predictions
}

/// Pure strip-slot policy. Kept free of `UIKit`/FM so it is fully unit-testable.
public enum StripSlotDecider {
    /// Decide the strip slot from the *in-progress word* and the *text before the
    /// cursor*.
    ///
    /// - Predictions when there is **no** in-progress word AND the cursor sits right
    ///   after a space (mid-sentence included) or at the very start of an empty field.
    /// - Corrections otherwise (mid-typing a word) — Phase 1 owns the strip.
    ///
    /// `contextBeforeCursor` is the document text before the caret (as from
    /// `UITextDocumentProxy.documentContextBeforeInput`). `inProgressWord` is the
    /// trailing run of word characters (as from `KeyboardModel.currentWord`).
    public static func slot(inProgressWord: String, contextBeforeCursor: String) -> StripSlot {
        // Mid-word → corrections. Phase 1 is untouched.
        if !inProgressWord.isEmpty { return .corrections }

        // No in-progress word. Predict only when the caret is right after a space
        // (there is real, committed content to predict from) or the field is empty
        // (start of typing). If the char before the caret is a non-space, non-word
        // char (e.g. we just deleted into "foo" and the tail isn't a word char — rare),
        // we still fall back to corrections to avoid surprising the user.
        guard let last = contextBeforeCursor.last else {
            // Empty field: start-of-typing prediction is allowed.
            return .predictions
        }
        return last == " " ? .predictions : .corrections
    }
}

/// Shared cleanup used by every predictor + the wiring: cap to `limit`, drop empties/
/// whitespace-only, trim, and de-duplicate case-insensitively (first occurrence wins).
/// Kept here (framework-free) so the FM predictor, the stub, and tests all agree.
public enum PredictionCleanup {
    public static func clean(_ raw: [String], limit: Int = 3) -> [String] {
        var seen = Set<String>()
        var out: [String] = []
        for candidate in raw {
            let trimmed = candidate.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmed.isEmpty else { continue }
            let key = trimmed.lowercased()
            guard !seen.contains(key) else { continue }
            seen.insert(key)
            out.append(trimmed)
            if out.count == limit { break }
        }
        return out
    }
}
