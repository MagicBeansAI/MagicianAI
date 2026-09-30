package ai.magicbeans.magdroid.keyboard

import kotlin.math.abs
import kotlin.math.max
import kotlin.math.min

/**
 * The **SymSpell symmetric-delete** spelling algorithm (Wolf Garbe's approach),
 * ported from `magios/Shared/Correction/SymSpellCore.swift` so both keyboards
 * correct identically from the same dictionaries.
 *
 * For every dictionary word the set of strings reachable by deleting up to
 * [maxEditDistance] characters is precomputed and indexed deletion → words. At
 * lookup time the same deletes are generated for the *input*; any dictionary
 * word sharing a deletion is a candidate, then verified with the true
 * Damerau-Levenshtein distance. Candidate generation is therefore orders of
 * magnitude faster than scanning the vocabulary, at the cost of a one-time
 * index build — done off-thread on the pre-warm path, never on a keypress.
 */
class SymSpellCore(dictionary: CorrectionDictionary, maxEditDistance: Int = 2) {

    data class Suggestion(val term: String, val distance: Int, val frequency: Int)

    val maxEditDistance: Int = max(0, maxEditDistance)

    /** deletion string → indices of the dictionary words that generate it. */
    private val deletes: Map<String, IntArray>
    private val words: Array<String>
    private val frequencies: IntArray
    private val wordIndex: Map<String, Int>

    init {
        val entries = dictionary.entries.entries.toList()
        val wordList = ArrayList<String>(entries.size)
        val freqList = ArrayList<Int>(entries.size)
        val index = HashMap<String, Int>(entries.size)
        val deleteLists = HashMap<String, MutableList<Int>>(entries.size * 4)

        for ((word, freq) in entries) {
            val idx = wordList.size
            wordList.add(word)
            freqList.add(freq)
            index[word] = idx

            // Cap the delete neighborhood for long words. A word longer than
            // `LONG_TOKEN_THRESHOLD + editDistance` can never be a full-budget
            // match for any input — lookup caps long tokens to distance 1 and
            // the length pre-filter rejects a bigger gap — so its full O(L²)
            // delete set is pure memory waste. Provably lossless.
            val perWordDistance = if (word.length > LONG_TOKEN_THRESHOLD + this.maxEditDistance) {
                min(this.maxEditDistance, 1)
            } else {
                this.maxEditDistance
            }
            for (deleted in editsPrefix(word, perWordDistance)) {
                deleteLists.getOrPut(deleted) { mutableListOf() }.add(idx)
            }
            // The exact word always maps to itself, even when edits() excluded
            // it for very short words.
            val self = deleteLists.getOrPut(word) { mutableListOf() }
            if (!self.contains(idx)) self.add(idx)
        }

        words = wordList.toTypedArray()
        frequencies = freqList.toIntArray()
        wordIndex = index
        deletes = deleteLists.mapValues { it.value.toIntArray() }
    }

    fun contains(word: String): Boolean = wordIndex.containsKey(word.lowercase())

    fun frequency(of: String): Int = wordIndex[of.lowercase()]?.let { frequencies[it] } ?: 0

    /**
     * Correction candidates for [word], ranked by (distance ascending,
     * frequency descending). The per-call distance may be lowered but never
     * raised above the index's build distance; long tokens cap to distance 1 —
     * long words rarely carry two independent typos, and their neighborhood
     * explodes.
     */
    fun lookup(word: String, maxEditDistance: Int? = null): List<Suggestion> {
        val input = word.lowercase()
        if (input.isEmpty()) return emptyList()

        var maxDistance = min(maxEditDistance ?: this.maxEditDistance, this.maxEditDistance)
        if (input.length > LONG_TOKEN_THRESHOLD) maxDistance = min(maxDistance, 1)
        maxDistance = max(0, maxDistance)

        val candidateIndices = HashSet<Int>()
        // 1. Deletes of the INPUT, matched against the index.
        for (deleted in editsPrefix(input, maxDistance)) {
            deletes[deleted]?.forEach { candidateIndices.add(it) }
        }
        // 2. The input itself may be a delete of a longer dictionary word.
        deletes[input]?.forEach { candidateIndices.add(it) }

        val suggestions = ArrayList<Suggestion>(candidateIndices.size)
        for (idx in candidateIndices) {
            val term = words[idx]
            // Length pre-filter: true distance is at least the length delta.
            if (abs(term.length - input.length) > maxDistance) continue
            val distance = damerauLevenshtein(input, term, maxDistance)
            if (distance > maxDistance) continue
            suggestions.add(Suggestion(term, distance, frequencies[idx]))
        }

        suggestions.sortWith(
            compareBy<Suggestion> { it.distance }.thenByDescending { it.frequency },
        )
        return suggestions
    }

    /**
     * Prefix completions: dictionary words starting with [prefix], frequency
     * descending, up to [limit]. Linear over the capped vocabulary — cheap for
     * a top-N dictionary and only run on the background suggestion path.
     */
    fun completions(startingWith: String, limit: Int = 10): List<Suggestion> {
        val p = startingWith.lowercase()
        if (p.isEmpty()) return emptyList()
        val out = ArrayList<Suggestion>()
        for (idx in words.indices) {
            val term = words[idx]
            if (term.length > p.length && term.startsWith(p)) {
                out.add(Suggestion(term, 0, frequencies[idx]))
            }
        }
        out.sortByDescending { it.frequency }
        return if (out.size > limit) out.subList(0, limit).toList() else out
    }

    companion object {
        /** Tokens longer than this cap their edit distance to 1. */
        const val LONG_TOKEN_THRESHOLD = 8

        /**
         * The recursive delete neighborhood of [word] up to [maxEditDistance]
         * deletions. Deleting the last remaining character would yield "" —
         * deliberately never indexed, since an empty deletion matches every word.
         */
        fun editsPrefix(word: String, maxEditDistance: Int): Set<String> {
            val result = HashSet<String>()
            if (maxEditDistance <= 0 || word.isEmpty()) return result
            edits(StringBuilder(word), maxEditDistance, result)
            return result
        }

        private fun edits(chars: StringBuilder, remaining: Int, result: MutableSet<String>) {
            if (remaining <= 0 || chars.length <= 1) return
            for (i in 0 until chars.length) {
                val removed = chars[i]
                chars.deleteCharAt(i)
                val s = chars.toString()
                // Re-recursing an already-seen deletion is redundant — its
                // children were generated the first time.
                if (result.add(s)) edits(chars, remaining - 1, result)
                chars.insert(i, removed)
            }
        }

        /**
         * Damerau-Levenshtein, optimal-string-alignment variant: adjacent
         * transpositions count as one edit. Early-exits once a whole row
         * exceeds [maxDistance] (returns `maxDistance + 1`).
         */
        fun damerauLevenshtein(s1: String, s2: String, maxDistance: Int = Int.MAX_VALUE): Int {
            val a = s1.toCharArray()
            val b = s2.toCharArray()
            val n = a.size
            val m = b.size
            if (n == 0) return m
            if (m == 0) return n
            if (abs(n - m) > maxDistance) return maxDistance + 1

            var prevPrev = IntArray(m + 1)
            var prev = IntArray(m + 1) { it }
            var curr = IntArray(m + 1)

            for (i in 1..n) {
                curr[0] = i
                var rowMin = curr[0]
                for (j in 1..m) {
                    val cost = if (a[i - 1] == b[j - 1]) 0 else 1
                    var value = min(
                        min(prev[j] + 1, curr[j - 1] + 1),
                        prev[j - 1] + cost,
                    )
                    if (i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1]) {
                        value = min(value, prevPrev[j - 2] + 1)
                    }
                    curr[j] = value
                    rowMin = min(rowMin, value)
                }
                if (rowMin > maxDistance) return maxDistance + 1
                val temp = prevPrev
                prevPrev = prev
                prev = curr
                curr = temp
            }
            return prev[m]
        }
    }
}
