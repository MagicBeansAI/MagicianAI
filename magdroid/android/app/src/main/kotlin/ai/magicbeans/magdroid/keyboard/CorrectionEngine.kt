package ai.magicbeans.magdroid.keyboard

import kotlin.math.ln
import kotlin.math.max

/**
 * The correction core, ported from
 * `magios/Shared/Correction/CorrectionEngine.swift`: the base dictionary
 * merged with learned words, the symmetric-delete index over it, and three
 * operations —
 *
 * - [suggestions] — completions/corrections for the candidate row;
 * - [bestCorrection] — the single best candidate after keyboard-adjacency
 *   re-ranking;
 * - [autocorrection] — the confidence-gated auto-replace: null unless the
 *   token is confidently a typo of a clearly better word.
 *
 * Construction builds the index and is the heavy step — always off the
 * keypress thread ([CorrectionStore] owns that). Queries are index lookups
 * plus a handful of Damerau-Levenshtein verifications.
 */
class CorrectionEngine(
    dictionary: CorrectionDictionary,
    /**
     * Words the owner has typed [LEARN_THRESHOLD]+ times, as (word, count).
     * Folded in as valid + frequency-boosted, so they are never corrected and
     * start being suggested.
     */
    learnedEntries: List<Pair<String, Int>> = emptyList(),
    /**
     * Live validity check for words learned *after* this engine was built —
     * the fold above only affects ranking; validity must not wait for a
     * rebuild. Mirrors iOS, where `isValid` consults the live store.
     */
    private val isLearnedNow: (String) -> Boolean = { false },
    maxEditDistance: Int = 2,
    private val adjacency: KeyboardAdjacency = KeyboardAdjacency.QWERTY,
) {

    val maxEditDistance: Int = max(1, maxEditDistance)

    private val dictionary: CorrectionDictionary
    private val core: SymSpellCore

    init {
        val boosted = learnedEntries.map { (word, count) -> word to count * LEARNED_BOOST }
        this.dictionary = if (boosted.isEmpty()) dictionary else dictionary.merging(boosted)
        this.core = SymSpellCore(this.dictionary, this.maxEditDistance)
    }

    /** True when the token is known-valid: dictionary or learned. Valid words are never autocorrected. */
    fun isValid(word: String): Boolean =
        dictionary.contains(word) || isLearnedNow(word)

    /**
     * Up to [limit] candidates for the row: frequency-ranked completions of the
     * in-progress word first — the highest-value thing a strip can offer — then
     * adjacency-re-ranked corrections to fill the remaining slots. The exact
     * typed word is never offered back.
     */
    fun suggestions(word: String, limit: Int = 3): List<String> {
        val input = word.lowercase()
        if (input.isEmpty()) return emptyList()

        val ordered = ArrayList<String>(limit * 2)
        val seen = HashSet<String>()
        fun add(term: String) {
            val key = term.lowercase()
            if (key == input || !seen.add(key)) return
            ordered.add(term)
        }

        core.completions(startingWith = input, limit = limit * 3).forEach { add(it.term) }
        rankedCandidates(input).forEach { add(it.suggestion.term) }

        return ordered.take(limit)
    }

    /**
     * The single best correction after adjacency re-ranking, or null when
     * nothing is within the edit budget. Does NOT apply the confidence gate —
     * that is [autocorrection]'s job.
     */
    fun bestCorrection(word: String): String? =
        rankedCandidates(word.lowercase()).firstOrNull()?.suggestion?.term

    /**
     * The confidence gate. A replacement ONLY when the token is confidently a
     * typo of a clearly better word:
     * - null for a valid or learned word;
     * - the edit distance must fit the budget — capped to 1 for short tokens
     *   (≤ 4 chars), where a two-edit "fix" is usually a different word;
     * - the frequency must clear the floor;
     * - the best must clearly beat any same-distance rival, or the strip gets
     *   it instead of the auto-replace.
     */
    fun autocorrection(word: String): String? {
        val input = word.lowercase()
        if (input.length < 2) return null
        if (isValid(input)) return null

        val candidates = rankedCandidates(input)
        val best = candidates.firstOrNull() ?: return null

        val distanceCap = if (input.length <= 4) 1 else maxEditDistance
        if (best.suggestion.distance > distanceCap) return null
        if (best.suggestion.frequency < MIN_AUTOCORRECT_FREQUENCY) return null

        val rival = candidates.drop(1)
            .firstOrNull { it.suggestion.distance == best.suggestion.distance }
        if (rival != null && best.score - rival.score < CONFIDENCE_MARGIN) return null

        return best.suggestion.term
    }

    // ── Ranking ──────────────────────────────────────────────────────────────

    private data class Scored(val suggestion: SymSpellCore.Suggestion, val score: Double)

    /**
     * Index candidates re-ranked within each edit-distance band by
     * `log(frequency)` plus keyboard adjacency. Distance stays the primary
     * key — a one-edit fix always beats a two-edit one; adjacency and
     * frequency only break ties within a band.
     */
    private fun rankedCandidates(word: String): List<Scored> {
        val input = word.lowercase()
        val raw = core.lookup(input, maxEditDistance)
        if (raw.isEmpty()) return emptyList()

        return raw.map { s ->
            val freqScore = FREQUENCY_WEIGHT * ln(max(s.frequency, 1).toDouble())
            val adjScore = ADJACENCY_WEIGHT * adjacency.adjacencyScore(input, s.term)
            Scored(s, freqScore + adjScore)
        }.sortedWith(
            compareBy<Scored> { it.suggestion.distance }.thenByDescending { it.score },
        )
    }

    companion object {
        /** Repeats before a typed word is trusted — same threshold as iOS. */
        const val LEARN_THRESHOLD = 3

        /**
         * The frequency multiplier for a learned word when folding it in —
         * enough to promote it above lookalikes without swamping genuinely
         * high-frequency English words.
         */
        private const val LEARNED_BOOST = 1000

        private const val FREQUENCY_WEIGHT = 1.0
        private const val ADJACENCY_WEIGHT = 2.0

        /** A candidate must clear this to be auto-applied. */
        private const val MIN_AUTOCORRECT_FREQUENCY = 1

        /** "Clearly beats" a same-distance rival = at least this score margin. */
        private const val CONFIDENCE_MARGIN = 0.20

        /** Copy the leading capitalization of [original] onto [correction]. */
        fun preserveCapitalization(original: String, correction: String): String {
            val first = original.firstOrNull() ?: return correction
            return if (first.isUpperCase()) {
                correction.replaceFirstChar { it.uppercase() }
            } else {
                correction
            }
        }
    }
}
