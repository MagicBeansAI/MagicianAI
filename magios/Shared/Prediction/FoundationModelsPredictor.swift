import Foundation

#if canImport(FoundationModels)
import FoundationModels
#endif

/// On-device next-word predictor backed by Apple Foundation Models (the ~3B system
/// language model, run out-of-process by the OS — so it does NOT count against the
/// keyboard extension's memory budget).
///
/// ALL Foundation Models types are confined behind `#if canImport(FoundationModels)`
/// AND `@available(iOS 26.1, *)` (the FM API is declared `iOS 26.0`, but per the plan
/// we gate at 26.1 = Apple-Intelligence-capable OS; 26.1 satisfies the 26.0 API).
///
/// Fallback contract (matches `NextWordPredictor`):
///   - On a toolchain without the framework (`!canImport`), on iOS < 26.1, on an
///     ineligible device, or with Apple Intelligence off / model not ready →
///     `isAvailable == false` and `predict(context:)` returns `[]`. No crash, no strip
///     regression (the after-space slot is simply empty, exactly today's behaviour).
///
/// Verified against the iOS 26.5 SDK `.swiftinterface`:
///   - `SystemLanguageModel.default.availability` → `.available` / `.unavailable(reason)`.
///   - `LanguageModelSession(model:tools:instructions: String?)` + `prewarm(promptPrefix:)`.
///   - `respond(to:generating:includeSchemaInPrompt:options:)` → `Response<Content>` with `.content`.
///   - `@Generable` / `@Guide` macros + `GenerationGuide.count/.maximumCount` for arrays.
public final class FoundationModelsPredictor: NextWordPredictor, @unchecked Sendable {

    /// True only when the system model reports `.available` (eligible device, Apple
    /// Intelligence on, model downloaded) on a supporting OS + toolchain.
    public static var isAvailable: Bool {
        #if canImport(FoundationModels)
        if #available(iOS 26.1, *) {
            switch SystemLanguageModel.default.availability {
            case .available: return true
            case .unavailable: return false
            }
        } else {
            return false
        }
        #else
        return false
        #endif
    }

    public init() {}

    // The heavy state (one session) is created lazily on first use and prewarmed. It's
    // held only when the framework is importable AND the OS is new enough; otherwise
    // this predictor is a pure no-op.
    #if canImport(FoundationModels)
    /// One session per keyboard lifetime (built lazily, prewarmed). Serialized by the
    /// actor below so we never issue concurrent requests to a single session.
    @available(iOS 26.1, *)
    private var session: LanguageModelSession? {
        get { _session as? LanguageModelSession }
        set { _session = newValue }
    }
    private var _session: AnyObject?
    private let sessionLock = NSLock()

    @available(iOS 26.1, *)
    private func makeInstructions() -> String {
        """
        You predict the next word a person is about to type on a phone keyboard.
        The user writes Indian English and Hinglish (Hindi–English code-mixing), so be \
        aware of common Indian-English phrasing and romanized Hindi words.
        Given the text so far, return ONLY the 3 most likely next words the user will \
        type next, most likely first. Words only — no punctuation, no numbering, no \
        explanation, no repeats. If unsure, return your best single-word guesses.
        """
    }

    /// Lazily build + prewarm the session. Returns nil if the model isn't available.
    @available(iOS 26.1, *)
    private func ensureSession() -> LanguageModelSession? {
        sessionLock.lock(); defer { sessionLock.unlock() }
        if let existing = _session as? LanguageModelSession { return existing }
        guard case .available = SystemLanguageModel.default.availability else { return nil }
        let created = LanguageModelSession(instructions: makeInstructions())
        created.prewarm()
        _session = created
        return created
    }
    #endif

    /// Warm the model on a background path (call from the keyboard's existing
    /// `prewarm()`). No-op when FM is unavailable. Never blocks the caller meaningfully.
    public func prewarm() {
        #if canImport(FoundationModels)
        if #available(iOS 26.1, *) {
            _ = ensureSession()
        }
        #endif
    }

    public func predict(context: String) async -> [String] {
        #if canImport(FoundationModels)
        if #available(iOS 26.1, *) {
            return await predictFM(context: context)
        } else {
            return []
        }
        #else
        return []
        #endif
    }

    #if canImport(FoundationModels)
    /// Typed output for guided generation: exactly the top-3 next words.
    @available(iOS 26.1, *)
    @Generable
    struct NextWords {
        @Guide(description: "The 3 most likely next words, most likely first, words only.", .maximumCount(3))
        var words: [String]
    }

    @available(iOS 26.1, *)
    private func predictFM(context: String) async -> [String] {
        // Cheap guard: nothing to predict from.
        let trimmed = Self.trimToLastWords(context, max: 20)
        guard !trimmed.isEmpty else { return [] }
        guard let session = ensureSession() else { return [] }

        // A single session can't service concurrent requests; if one is already
        // running (a debounce race slipped through), skip rather than crash/queue.
        guard !session.isResponding else { return [] }

        let prompt = "Text so far: \"\(trimmed)\"\nNext words:"
        let options = GenerationOptions(temperature: 0.4, maximumResponseTokens: 24)
        do {
            let response = try await session.respond(
                to: prompt,
                generating: NextWords.self,
                options: options
            )
            return PredictionCleanup.clean(response.content.words, limit: 3)
        } catch {
            // Catch ALL errors (context-window, guardrail, decoding, rate-limited,
            // concurrent, refusal, cancellation, …) → clean no-op. Never crash.
            return []
        }
    }

    /// Trim context to roughly the last `max` whitespace-separated tokens (keeps the
    /// prompt short + latency low; the tail is what matters for next-word prediction).
    static func trimToLastWords(_ context: String, max: Int) -> String {
        let tokens = context.split(whereSeparator: { $0 == " " || $0 == "\n" || $0 == "\t" })
        guard tokens.count > max else {
            return context.trimmingCharacters(in: .whitespacesAndNewlines)
        }
        return tokens.suffix(max).joined(separator: " ")
    }
    #endif
}
