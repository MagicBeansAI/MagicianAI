import Foundation

/// QWERTY physical-neighbor model for fat-finger re-ranking.
///
/// SymSpell ranks purely by edit distance + frequency and treats every
/// substitution equally, so "amd"→"and" and "amd"→"aid" look equally likely.
/// But on a QWERTY layout `m` and `n` are adjacent while `m` and `i` are not,
/// so "and" is the far more plausible fat-finger fix. `adjacencyScore` rewards
/// edits whose changed letters sit on physically neighboring keys.
///
/// Neighbors are derived from the standard 3-row lowercase layout the keyboard
/// already models (see `KeyboardLayoutModel`): each key's left/right on its row
/// plus the (roughly) overlapping keys on the rows above and below.
struct KeyboardAdjacency {
    /// a–z → the set of physically adjacent letter keys.
    private let neighbors: [Character: Set<Character>]

    /// The shared default QWERTY map.
    static let qwerty = KeyboardAdjacency()

    init() {
        // Standard offset QWERTY letter rows.
        let rows: [[Character]] = [
            Array("qwertyuiop"),
            Array("asdfghjkl"),
            Array("zxcvbnm"),
        ]
        var map: [Character: Set<Character>] = [:]

        for (r, row) in rows.enumerated() {
            for (c, key) in row.enumerated() {
                var set = map[key] ?? []
                // Same-row left / right.
                if c > 0 { set.insert(row[c - 1]) }
                if c + 1 < row.count { set.insert(row[c + 1]) }
                // Row above / below: the keyboard is offset, so a key overlaps
                // (approximately) the two keys straddling its column. Using
                // columns c-1, c, c+1 on the adjacent row captures the diagonal
                // neighbors that cause real fat-finger substitutions.
                for adj in [r - 1, r + 1] where adj >= 0 && adj < rows.count {
                    let other = rows[adj]
                    for oc in (c - 1)...(c + 1) where oc >= 0 && oc < other.count {
                        set.insert(other[oc])
                    }
                }
                map[key] = set
            }
        }
        self.neighbors = map
    }

    /// True when `a` and `b` are the same key or physically adjacent keys.
    func areNeighbors(_ a: Character, _ b: Character) -> Bool {
        if a == b { return true }
        return neighbors[a]?.contains(b) ?? false
    }

    /// A 0…1 score for how well `candidate` explains `typed` as a fat-finger
    /// slip. 1.0 means every differing position is a swap between physically
    /// adjacent keys (a very plausible correction); 0.0 means the differences
    /// land on distant keys.
    ///
    /// - Equal length: score = fraction of differing positions whose typed vs.
    ///   candidate letters are neighbors (a pure fat-finger substitution set).
    /// - Adjacent transposition (`teh`→`the`): treated as fully adjacent.
    /// - Length differs by an insert/delete: neutral 0.5 (adjacency doesn't
    ///   speak to inserts/deletes; frequency + edit distance decide those).
    func adjacencyScore(typed: String, candidate: String) -> Double {
        let t = Array(typed.lowercased())
        let c = Array(candidate.lowercased())
        guard !t.isEmpty, !c.isEmpty else { return 0 }

        if t.count == c.count {
            // Detect a single adjacent transposition first (swap of two chars).
            if let (i, j) = singleTransposition(t, c) {
                return areNeighbors(t[i], t[j]) ? 1.0 : 0.6
            }
            var diffs = 0
            var adjacent = 0
            for k in 0..<t.count where t[k] != c[k] {
                diffs += 1
                if areNeighbors(t[k], c[k]) { adjacent += 1 }
            }
            guard diffs > 0 else { return 1.0 } // identical → maximal
            return Double(adjacent) / Double(diffs)
        }

        // Insert/delete: adjacency is uninformative → neutral.
        return 0.5
    }

    /// If `a` and `b` differ only by swapping exactly two positions, return
    /// those indices; otherwise `nil`. Both strings are assumed equal length.
    private func singleTransposition(_ a: [Character], _ b: [Character]) -> (Int, Int)? {
        var mismatches: [Int] = []
        for k in 0..<a.count where a[k] != b[k] {
            mismatches.append(k)
            if mismatches.count > 2 { return nil }
        }
        guard mismatches.count == 2 else { return nil }
        let (i, j) = (mismatches[0], mismatches[1])
        // A transposition means a[i]==b[j] and a[j]==b[i].
        return (a[i] == b[j] && a[j] == b[i]) ? (i, j) : nil
    }
}
