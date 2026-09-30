package ai.magicbeans.magdroid.keyboard

/**
 * QWERTY physical-neighbor model for fat-finger re-ranking, ported from
 * `magios/Shared/Correction/KeyboardAdjacency.swift`.
 *
 * SymSpell ranks purely by edit distance + frequency and treats every
 * substitution equally, so "amd"→"and" and "amd"→"aid" look equally likely.
 * But on a QWERTY layout `m` and `n` are adjacent while `m` and `i` are not,
 * so "and" is the far more plausible fat-finger fix. [adjacencyScore] rewards
 * edits whose changed letters sit on physically neighboring keys.
 */
class KeyboardAdjacency private constructor(
    private val neighbors: Map<Char, Set<Char>>,
) {

    fun areNeighbors(a: Char, b: Char): Boolean =
        a == b || neighbors[a]?.contains(b) == true

    /**
     * A 0..1 score for how well [candidate] explains [typed] as a fat-finger
     * slip:
     * - equal length → the fraction of differing positions whose letters are
     *   physical neighbors;
     * - a single adjacent transposition (`teh`→`the`) → 1.0 when the swapped
     *   keys are neighbors, 0.6 otherwise (a transposition is a typing slip
     *   whichever keys it lands on);
     * - length differs (insert/delete) → neutral 0.5, because adjacency has
     *   nothing to say about a missing or extra character — frequency and
     *   edit distance decide those.
     */
    fun adjacencyScore(typed: String, candidate: String): Double {
        val t = typed.lowercase().toCharArray()
        val c = candidate.lowercase().toCharArray()
        if (t.isEmpty() || c.isEmpty()) return 0.0

        if (t.size == c.size) {
            singleTransposition(t, c)?.let { (i, j) ->
                return if (areNeighbors(t[i], t[j])) 1.0 else 0.6
            }
            var diffs = 0
            var adjacent = 0
            for (k in t.indices) {
                if (t[k] != c[k]) {
                    diffs += 1
                    if (areNeighbors(t[k], c[k])) adjacent += 1
                }
            }
            if (diffs == 0) return 1.0
            return adjacent.toDouble() / diffs.toDouble()
        }

        return 0.5
    }

    /**
     * If the arrays differ by swapping exactly two positions, those indices;
     * else null. Assumes equal length.
     */
    private fun singleTransposition(a: CharArray, b: CharArray): Pair<Int, Int>? {
        val mismatches = ArrayList<Int>(3)
        for (k in a.indices) {
            if (a[k] != b[k]) {
                mismatches.add(k)
                if (mismatches.size > 2) return null
            }
        }
        if (mismatches.size != 2) return null
        val (i, j) = mismatches
        return if (a[i] == b[j] && a[j] == b[i]) i to j else null
    }

    companion object {
        /** The shared default QWERTY map. */
        val QWERTY: KeyboardAdjacency = build()

        private fun build(): KeyboardAdjacency {
            // Standard offset QWERTY letter rows — the same three rows this
            // keyboard's renderKeys() draws.
            val rows = listOf(
                "qwertyuiop".toCharArray(),
                "asdfghjkl".toCharArray(),
                "zxcvbnm".toCharArray(),
            )
            val map = HashMap<Char, MutableSet<Char>>()
            for ((r, row) in rows.withIndex()) {
                for ((c, key) in row.withIndex()) {
                    val set = map.getOrPut(key) { mutableSetOf() }
                    if (c > 0) set.add(row[c - 1])
                    if (c + 1 < row.size) set.add(row[c + 1])
                    // The rows are offset, so a key overlaps roughly the keys
                    // straddling its column on the rows above and below —
                    // columns c-1..c+1 capture the diagonals that cause real
                    // fat-finger substitutions.
                    for (adj in intArrayOf(r - 1, r + 1)) {
                        if (adj < 0 || adj >= rows.size) continue
                        val other = rows[adj]
                        for (oc in (c - 1)..(c + 1)) {
                            if (oc in other.indices) set.add(other[oc])
                        }
                    }
                }
            }
            return KeyboardAdjacency(map)
        }
    }
}
