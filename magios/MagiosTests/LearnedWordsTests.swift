import XCTest
@testable import Magician

/// Tests for the learn-your-words store (Task 5). Uses the in-memory backing so
/// nothing touches the App Group `UserDefaults`.
final class LearnedWordsTests: XCTestCase {

    func testWordBecomesLearnedAfterThreshold() {
        let store = LearnedWordsStore(inMemory: true, learnThreshold: 3)
        XCTAssertFalse(store.isLearned("chaiwala"))
        store.record("chaiwala")
        XCTAssertFalse(store.isLearned("chaiwala"))  // 1
        store.record("chaiwala")
        XCTAssertFalse(store.isLearned("chaiwala"))  // 2
        store.record("chaiwala")
        XCTAssertTrue(store.isLearned("chaiwala"))   // 3 -> learned
    }

    func testLearnedEntriesReturnsOnlyThresholdWords() {
        let store = LearnedWordsStore(inMemory: true, learnThreshold: 3)
        for _ in 0..<3 { store.record("bhelpuri") }
        store.record("onceoff")
        let entries = Dictionary(uniqueKeysWithValues: store.learnedEntries())
        XCTAssertNotNil(entries["bhelpuri"])
        XCTAssertNil(entries["onceoff"])             // below threshold
        XCTAssertEqual(entries["bhelpuri"], 3)
    }

    func testCaseInsensitiveAndPunctuationTolerant() {
        let store = LearnedWordsStore(inMemory: true, learnThreshold: 2)
        store.record("Chaiwala")
        store.record("chaiwala!")   // punctuation stripped, same key
        XCTAssertTrue(store.isLearned("chaiwala"))
        XCTAssertTrue(store.isLearned("CHAIWALA"))
    }

    func testIgnoresEmptyTokens() {
        let store = LearnedWordsStore(inMemory: true, learnThreshold: 1)
        store.record("")
        store.record("   ")
        store.record("!!!")
        XCTAssertTrue(store.learnedEntries().isEmpty)
    }

    func testResetClearsCounts() {
        let store = LearnedWordsStore(inMemory: true, learnThreshold: 1)
        store.record("temp")
        XCTAssertTrue(store.isLearned("temp"))
        store.reset()
        XCTAssertFalse(store.isLearned("temp"))
        XCTAssertTrue(store.learnedEntries().isEmpty)
    }

    func testTrustLearnsImmediatelyInOneSignal() {
        // Reject feedback: one revert should trust the word without 3 repeats.
        let store = LearnedWordsStore(inMemory: true, learnThreshold: 3)
        XCTAssertFalse(store.isLearned("hii"))
        store.trust("hii")
        XCTAssertTrue(store.isLearned("hii"))
        XCTAssertEqual(Dictionary(uniqueKeysWithValues: store.learnedEntries())["hii"], 3)
    }

    func testTrustNeverLowersAHigherCount() {
        let store = LearnedWordsStore(inMemory: true, learnThreshold: 3)
        for _ in 0..<10 { store.record("frequent") }
        store.trust("frequent")   // must not drop 10 down to the threshold
        XCTAssertEqual(Dictionary(uniqueKeysWithValues: store.learnedEntries())["frequent"], 10)
    }

    func testTrustNormalizesAndIgnoresEmpty() {
        let store = LearnedWordsStore(inMemory: true, learnThreshold: 3)
        store.trust("Yaar!")             // punctuation stripped, lowercased
        XCTAssertTrue(store.isLearned("yaar"))
        store.trust("!!!")               // nothing to trust
        XCTAssertEqual(store.learnedEntries().count, 1)
    }

    func testResetClearsTrustedWordsToo() {
        let store = LearnedWordsStore(inMemory: true, learnThreshold: 3)
        store.trust("chaiwala")
        XCTAssertTrue(store.isLearned("chaiwala"))
        store.reset()
        XCTAssertFalse(store.isLearned("chaiwala"))
    }
}
