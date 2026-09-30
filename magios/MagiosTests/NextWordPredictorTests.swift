import XCTest
@testable import Magician

/// Phase-2a next-word prediction unit tests.
///
/// Module boundary (same as `SuggestionEngineTests`): the FM-backed
/// `FoundationModelsPredictor` is a device-only system dependency and is NOT
/// unit-testable here. These tests cover the pure, app-host-testable seams that live
/// in `Shared/Prediction/**` — the strip-slot decision, output cleanup
/// (limit/dedupe/empty-drop), and the debounce/cancel wrapper — using
/// `StubNextWordPredictor` in place of the real model.
final class NextWordPredictorTests: XCTestCase {

    // MARK: - Strip-slot decision (predictions vs corrections)

    func testEmptyWordAfterSpaceChoosesPredictions() {
        // Cursor right after a space, no in-progress word → predictions.
        XCTAssertEqual(
            StripSlotDecider.slot(inProgressWord: "", contextBeforeCursor: "hello "),
            .predictions
        )
    }

    func testEmptyFieldChoosesPredictions() {
        // Start of typing (empty field) → predictions (start-of-sentence guesses).
        XCTAssertEqual(
            StripSlotDecider.slot(inProgressWord: "", contextBeforeCursor: ""),
            .predictions
        )
    }

    func testPredictionsFireMidSentenceAfterEverySpace() {
        // Design: predict after EVERY space, not only sentence start.
        XCTAssertEqual(
            StripSlotDecider.slot(inProgressWord: "", contextBeforeCursor: "I am going to "),
            .predictions
        )
    }

    func testNonEmptyWordChoosesCorrections() {
        // Mid-typing a word → Phase-1 corrections own the strip.
        XCTAssertEqual(
            StripSlotDecider.slot(inProgressWord: "hel", contextBeforeCursor: "hel"),
            .corrections
        )
    }

    func testNonEmptyWordMidSentenceChoosesCorrections() {
        XCTAssertEqual(
            StripSlotDecider.slot(inProgressWord: "goin", contextBeforeCursor: "I am goin"),
            .corrections
        )
    }

    func testCursorAfterPunctuationNonSpaceChoosesCorrections() {
        // No in-progress word but the char before the caret isn't a space → don't
        // surprise the user with predictions; fall back to corrections (empty strip).
        XCTAssertEqual(
            StripSlotDecider.slot(inProgressWord: "", contextBeforeCursor: "hello."),
            .corrections
        )
    }

    // MARK: - Output cleanup (limit / dedupe / empty-drop)

    func testCleanupCapsToLimit() {
        let out = PredictionCleanup.clean(["a", "b", "c", "d", "e"], limit: 3)
        XCTAssertEqual(out, ["a", "b", "c"])
    }

    func testCleanupDropsEmptiesAndWhitespace() {
        let out = PredictionCleanup.clean(["", "   ", "the", "\n", "you"], limit: 3)
        XCTAssertEqual(out, ["the", "you"])
    }

    func testCleanupDedupesCaseInsensitivelyKeepingFirst() {
        let out = PredictionCleanup.clean(["The", "the", "THE", "you"], limit: 3)
        XCTAssertEqual(out, ["The", "you"])
    }

    func testCleanupTrimsWhitespace() {
        let out = PredictionCleanup.clean(["  hi ", "there  "], limit: 3)
        XCTAssertEqual(out, ["hi", "there"])
    }

    // MARK: - StubNextWordPredictor

    func testStubReturnsFixedList() async {
        let stub = StubNextWordPredictor(words: ["one", "two", "three"])
        let out = await stub.predict(context: "anything")
        XCTAssertEqual(out, ["one", "two", "three"])
    }

    // MARK: - DebouncedPredictor: coalescing + cancellation

    func testDebouncedPredictorDeliversForSurvivingRequest() async {
        let stub = StubNextWordPredictor(words: ["hello", "there", "world"])
        let debounced = DebouncedPredictor(predictor: stub, debounceMillis: 20, limit: 3)

        let received = Expectation()
        await debounced.request(context: "hi ") { words in
            received.fulfil(words)
        }
        let words = await received.value(timeout: 2.0)
        XCTAssertEqual(words, ["hello", "there", "world"])
    }

    func testDebouncedPredictorCancelsSupersededRequests() async {
        // A slow stub so the first request is still in-flight when the second arrives.
        let slow = StubNextWordPredictor(words: ["STALE"], delayNanos: 200_000_000) // 200ms
        let debounced = DebouncedPredictor(predictor: slow, debounceMillis: 10, limit: 3)

        let staleFired = LockedFlag()
        await debounced.request(context: "first ") { _ in staleFired.set() }
        // Immediately supersede with a fresh, fast request.
        let fresh = Expectation()
        let fast = StubNextWordPredictor(words: ["FRESH"])
        // Swap by cancelling the old wrapper isn't possible (different predictor), so
        // model the real flow: same wrapper, second request supersedes the first.
        await debounced.request(context: "second ") { _ in
            // (still the slow predictor) — assert only the LATEST request delivers.
            fresh.fulfil(["slow-latest"])
        }
        _ = fast // silence unused in this scenario

        let latest = await fresh.value(timeout: 2.0)
        XCTAssertEqual(latest, ["slow-latest"])
        // The first (superseded) request must NOT have delivered.
        XCTAssertFalse(staleFired.isSet, "Superseded request should not deliver its completion")
    }

    func testDebouncedPredictorExplicitCancelSuppressesDelivery() async {
        let slow = StubNextWordPredictor(words: ["x"], delayNanos: 150_000_000)
        let debounced = DebouncedPredictor(predictor: slow, debounceMillis: 10, limit: 3)

        let fired = LockedFlag()
        await debounced.request(context: "typing ") { _ in fired.set() }
        await debounced.cancel()

        // Wait past when the (cancelled) request would have completed.
        try? await Task.sleep(nanoseconds: 300_000_000)
        XCTAssertFalse(fired.isSet, "Cancelled request should not deliver its completion")
    }

    // MARK: - CompositeNextWordPredictor (FM → n-gram fallback on empty)

    func testCompositeFallsThroughToFirstNonEmpty() async {
        // FM present but generating nothing (e.g. Apple-Intelligence assets not ready)
        // → the n-gram fallback still supplies predictions.
        let composite = CompositeNextWordPredictor([
            StubNextWordPredictor(words: []),
            StubNextWordPredictor(words: ["and", "the"]),
        ])
        let result = await composite.predict(context: "I am going to ")
        XCTAssertEqual(result, ["and", "the"])
    }

    func testCompositePrefersEarlierNonEmpty() async {
        let composite = CompositeNextWordPredictor([
            StubNextWordPredictor(words: ["fm"]),
            StubNextWordPredictor(words: ["ngram"]),
        ])
        let result = await composite.predict(context: "hi ")
        XCTAssertEqual(result, ["fm"])
    }

    func testCompositeEmptyWhenAllEmpty() async {
        let composite = CompositeNextWordPredictor([
            StubNextWordPredictor(words: []),
            StubNextWordPredictor(words: []),
        ])
        let result = await composite.predict(context: "x ")
        XCTAssertTrue(result.isEmpty)
    }
}

// MARK: - Small async test helpers

/// A one-shot async value holder for verifying a completion fired with a payload.
private final class Expectation: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: [String]?
    private var continuation: CheckedContinuation<[String], Never>?

    func fulfil(_ value: [String]) {
        lock.lock()
        if let c = continuation { continuation = nil; lock.unlock(); c.resume(returning: value); return }
        stored = value
        lock.unlock()
    }

    func value(timeout: TimeInterval) async -> [String] {
        await withTaskGroup(of: [String]?.self) { group in
            group.addTask { await self.await_() }
            group.addTask {
                try? await Task.sleep(nanoseconds: UInt64(timeout * 1_000_000_000))
                return nil
            }
            let first = await group.next() ?? nil
            group.cancelAll()
            return first ?? []
        }
    }

    private func await_() async -> [String] {
        await withCheckedContinuation { (c: CheckedContinuation<[String], Never>) in
            lock.lock()
            if let s = stored { stored = nil; lock.unlock(); c.resume(returning: s); return }
            continuation = c
            lock.unlock()
        }
    }
}

/// A thread-safe boolean flag for "did this fire?" assertions.
private final class LockedFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var value = false
    func set() { lock.lock(); value = true; lock.unlock() }
    var isSet: Bool { lock.lock(); defer { lock.unlock() }; return value }
}
