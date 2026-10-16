import Foundation

/// Per-word learn-your-words store. Counts how often the user types each novel
/// word; once a word crosses `learnThreshold` repeats it becomes a *learned*
/// word — the correction engine then treats it as valid (never autocorrects it)
/// and folds it into the effective dictionary with a boosted frequency so it
/// starts being *suggested*.
///
/// Persisted in the App Group so the keyboard extension and the host app share
/// the same learned vocabulary. `init(inMemory: true)` swaps the backing store
/// for an in-process dictionary so tests run without touching `UserDefaults`.
final class LearnedWordsStore {
    /// App Group suite shared by the keyboard extension and host app.
    static let appGroupSuite = "group.ai.magicbeans.magician.shared"
    /// The `UserDefaults` key under which the `[word: count]` map is stored.
    private static let defaultsKey = "keyboard.learnedWordCounts.v1"
    /// A word is "learned" once it has been recorded at least this many times.
    let learnThreshold: Int

    /// nil ⇒ in-memory mode (tests); otherwise the App Group defaults.
    private let defaults: UserDefaults?
    /// In-memory counts (authoritative in in-memory mode; a cache otherwise).
    private var counts: [String: Int]
    /// Serializes read-modify-write so concurrent `record` calls (off the
    /// keypress thread) don't lose increments.
    private let lock = NSLock()

    init(inMemory: Bool = false, learnThreshold: Int = 3) {
        self.learnThreshold = max(1, learnThreshold)
        if inMemory {
            self.defaults = nil
            self.counts = [:]
        } else {
            let store = UserDefaults(suiteName: Self.appGroupSuite)
            self.defaults = store
            self.counts = (store?.dictionary(forKey: Self.defaultsKey) as? [String: Int]) ?? [:]
        }
    }

    /// Record one occurrence of a word the user typed. Lowercased; short/empty
    /// tokens are ignored. Safe to call off the main thread.
    func record(_ word: String) {
        let key = Self.normalize(word)
        guard !key.isEmpty else { return }
        lock.lock()
        let next = (counts[key] ?? 0) + 1
        counts[key] = next
        let snapshot = counts
        lock.unlock()
        // Persist the whole map (small) under the App Group suite. Cheap and
        // atomic enough for this volume; runs on the caller's background queue.
        defaults?.set(snapshot, forKey: Self.defaultsKey)
    }

    /// Immediately trust a word without waiting for `learnThreshold` natural
    /// repeats. One explicit signal — the user reverting an autocorrection back
    /// to what they typed ("reject feedback") — is enough: bump the count to at
    /// least the threshold so `isLearned` becomes true and the correction engine
    /// stops autocorrecting it (and starts suggesting it). Never lowers a higher
    /// existing count. Cleared by `reset()`. Safe to call off the main thread.
    func trust(_ word: String) {
        let key = Self.normalize(word)
        guard !key.isEmpty else { return }
        lock.lock()
        counts[key] = max(counts[key] ?? 0, learnThreshold)
        let snapshot = counts
        lock.unlock()
        defaults?.set(snapshot, forKey: Self.defaultsKey)
    }

    /// True once the word has been recorded at least `learnThreshold` times.
    func isLearned(_ word: String) -> Bool {
        let key = Self.normalize(word)
        guard !key.isEmpty else { return false }
        lock.lock()
        let count = counts[key] ?? 0
        lock.unlock()
        return count >= learnThreshold
    }

    /// Every learned word with its raw record count, for folding into the
    /// effective dictionary. Only words at/above the threshold are returned.
    func learnedEntries() -> [(String, Int)] {
        lock.lock()
        let snapshot = counts
        lock.unlock()
        return snapshot.compactMap { key, count in
            count >= learnThreshold ? (key, count) : nil
        }
    }

    /// Clear all learned words (used by tests / a future "reset" affordance).
    func reset() {
        lock.lock()
        counts = [:]
        lock.unlock()
        defaults?.removeObject(forKey: Self.defaultsKey)
    }

    private static func normalize(_ word: String) -> String {
        // Keep letters + intra-word apostrophes; drop everything else so
        // punctuation-attached tokens don't create junk entries.
        let allowed = CharacterSet.letters.union(CharacterSet(charactersIn: "'’"))
        let filtered = word.unicodeScalars.filter { allowed.contains($0) }
        return String(String.UnicodeScalarView(filtered)).lowercased()
    }
}
