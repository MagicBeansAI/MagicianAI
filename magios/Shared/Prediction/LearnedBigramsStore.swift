import Foundation

/// Learn-your-bigrams store: counts how often the user types each ordered word
/// **pair** `(prev → next)`. This is the strongest lever for universal next-word
/// prediction — it captures the user's own Indian-English + Hinglish phrasing
/// organically over time (same idea as `LearnedWordsStore`, but for pairs).
///
/// Persisted in the App Group so the keyboard extension and the host app share the
/// same learned bigrams. The backing map is `prev -> [next: count]` (lowercased
/// keys). `init(inMemory: true)` swaps the backing store for an in-process
/// dictionary so tests run without touching `UserDefaults`.
///
/// A **distinct key namespace** from `LearnedWordsStore` (`keyboard.learnedBigramCounts.v1`)
/// so the two never collide in the shared suite.
public final class LearnedBigramsStore {
    /// App Group suite shared by the keyboard extension and host app.
    public static let appGroupSuite = "group.ai.magicbeans.magician.shared"
    /// The `UserDefaults` key under which the `[prev: [next: count]]` map is stored.
    /// Distinct from `LearnedWordsStore`'s key.
    private static let defaultsKey = "keyboard.learnedBigramCounts.v1"

    /// nil ⇒ in-memory mode (tests); otherwise the App Group defaults.
    private let defaults: UserDefaults?
    /// In-memory counts: `prev -> (next -> count)` (authoritative in in-memory mode;
    /// a write-through cache otherwise).
    private var counts: [String: [String: Int]]
    /// Serializes read-modify-write so concurrent `record` calls (off the keypress
    /// thread) don't lose increments.
    private let lock = NSLock()

    public init(inMemory: Bool = false) {
        if inMemory {
            self.defaults = nil
            self.counts = [:]
        } else {
            let store = UserDefaults(suiteName: Self.appGroupSuite)
            self.defaults = store
            self.counts = Self.decode(store?.dictionary(forKey: Self.defaultsKey))
        }
    }

    /// Record one occurrence of the ordered pair `(prev → next)`. Both tokens are
    /// normalized (letters + intra-word apostrophes, lowercased); empty tokens are
    /// ignored. Safe to call off the main thread.
    public func record(prev: String, next: String) {
        let p = Self.normalize(prev)
        let n = Self.normalize(next)
        guard !p.isEmpty, !n.isEmpty else { return }
        lock.lock()
        var row = counts[p] ?? [:]
        row[n] = (row[n] ?? 0) + 1
        counts[p] = row
        let snapshot = counts
        lock.unlock()
        // Persist the whole map (small) under the App Group suite. Runs on the
        // caller's background queue. Encoded as `[prev: [next: count]]` — a plist-
        // safe nested dictionary.
        defaults?.set(snapshot, forKey: Self.defaultsKey)
    }

    /// The next words seen after `prev`, ranked by count descending (ties broken
    /// alphabetically for determinism). Empty when nothing has been learned for it.
    public func nextWords(after prev: String) -> [(String, Int)] {
        let p = Self.normalize(prev)
        guard !p.isEmpty else { return [] }
        lock.lock()
        let row = counts[p] ?? [:]
        lock.unlock()
        return row.sorted { lhs, rhs in
            lhs.value != rhs.value ? lhs.value > rhs.value : lhs.key < rhs.key
        }.map { ($0.key, $0.value) }
    }

    /// Clear all learned bigrams (tests / a future "reset" affordance).
    public func reset() {
        lock.lock()
        counts = [:]
        lock.unlock()
        defaults?.removeObject(forKey: Self.defaultsKey)
    }

    // MARK: - Helpers

    /// Decode the persisted `[prev: [next: count]]` map, tolerating a nil/garbage
    /// value (returns an empty map rather than crashing on a schema mismatch).
    private static func decode(_ raw: [String: Any]?) -> [String: [String: Int]] {
        guard let raw else { return [:] }
        var out: [String: [String: Int]] = [:]
        for (prev, value) in raw {
            guard let row = value as? [String: Int] else { continue }
            out[prev] = row
        }
        return out
    }

    private static func normalize(_ word: String) -> String {
        // Keep letters + intra-word apostrophes; drop everything else so
        // punctuation-attached tokens don't create junk entries. Mirrors
        // `LearnedWordsStore.normalize`.
        let allowed = CharacterSet.letters.union(CharacterSet(charactersIn: "'’"))
        let filtered = word.unicodeScalars.filter { allowed.contains($0) }
        return String(String.UnicodeScalarView(filtered)).lowercased()
    }
}
