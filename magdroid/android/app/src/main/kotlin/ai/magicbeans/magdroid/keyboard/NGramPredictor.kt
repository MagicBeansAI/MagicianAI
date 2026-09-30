package ai.magicbeans.magdroid.keyboard

import kotlin.math.max

/**
 * Next-word predictor backed by the compact curated English **bigram seed**
 * plus the owner's own **learned bigrams**, ported from
 * `magios/Shared/Prediction/NGramPredictor.swift`.
 *
 * On iOS this is the universal fallback under Apple's Foundation Models; that
 * framework has no Android counterpart, so here the n-gram *is* the predictor
 * — which is exactly the role iOS designed it for on devices without FM.
 *
 * Ranking per [predict]:
 * - take the last one/two word tokens of the context;
 * - baseline is a bigram match on the last word — the seed's ranked next
 *   words merged with a ×[learnedBoost] lift for the owner's learned
 *   `(prev → next)` pairs;
 * - with two words of history, next words learned after the *earlier* word
 *   get a light additional lift — trigram-ish context without a trigram corpus;
 * - the just-typed words are never suggested straight back; output is top-3,
 *   deduped, non-empty.
 */
class NGramPredictor(
    seedEntries: List<Triple<String, String, Int>>,
    /** The owner's learned `(prev → next)` pairs with counts. */
    private val learnedNextWords: (String) -> List<Pair<String, Int>>,
    learnedBoost: Int = 4,
) {

    /** prev → ranked (next, weight), immutable after construction. */
    private val seed: Map<String, List<Pair<String, Int>>> = buildSeed(seedEntries)
    private val learnedBoost: Int = max(1, learnedBoost)

    /** Up to three next-word predictions for the text before the cursor. */
    fun predict(context: String): List<String> {
        val tokens = tailTokens(context, max = 2)
        val last = tokens.lastOrNull()?.takeIf { it.isNotEmpty() } ?: return emptyList()
        val prevPrev = if (tokens.size >= 2) tokens[tokens.size - 2] else null

        val weights = HashMap<String, Int>()
        fun add(word: String, weight: Int) {
            val key = word.lowercase()
            if (key.isEmpty() || weight <= 0) return
            weights[key] = (weights[key] ?: 0) + weight
        }

        seed[last]?.forEach { (word, weight) -> add(word, weight) }
        learnedNextWords(last).forEach { (word, count) -> add(word, count * learnedBoost) }
        if (prevPrev != null) {
            learnedNextWords(prevPrev).forEach { (word, count) -> add(word, count) }
        }

        if (weights.isEmpty()) return emptyList()

        val banned = setOf(last.lowercase(), prevPrev?.lowercase() ?: "")
        return weights.entries.asSequence()
            .filter { it.key !in banned }
            .sortedWith(
                compareByDescending<Map.Entry<String, Int>> { it.value }.thenBy { it.key },
            )
            .map { it.key }
            .filter { it.isNotBlank() }
            .distinct()
            .take(3)
            .toList()
    }

    companion object {

        /**
         * `prev → ranked [(next, weight)]` from (prev, next, count) triples.
         * Lowercased; the higher count wins a duplicate; each row pre-ranked
         * (weight desc, ties alphabetical) so [predict] stays cheap.
         */
        fun buildSeed(
            entries: List<Triple<String, String, Int>>,
        ): Map<String, List<Pair<String, Int>>> {
            val table = HashMap<String, HashMap<String, Int>>()
            for ((rawPrev, rawNext, count) in entries) {
                val prev = rawPrev.lowercase().trim()
                val next = rawNext.lowercase().trim()
                if (prev.isEmpty() || next.isEmpty() || count <= 0) continue
                val row = table.getOrPut(prev) { HashMap() }
                val existing = row[next]
                if (existing == null || count > existing) row[next] = count
            }
            return table.mapValues { (_, row) ->
                row.entries
                    .sortedWith(
                        compareByDescending<Map.Entry<String, Int>> { it.value }.thenBy { it.key },
                    )
                    .map { it.key to it.value }
            }
        }

        /**
         * Parse `prev<TAB>next<TAB>count` seed text. Tolerant: blanks,
         * comments (`#`, `//`) and malformed rows are skipped. Whitespace
         * separation accepted.
         */
        fun parseSeed(text: String): List<Triple<String, String, Int>> {
            val out = ArrayList<Triple<String, String, Int>>()
            text.lineSequence().forEach { rawLine ->
                val line = rawLine.trim()
                if (line.isEmpty() || line.startsWith("#") || line.startsWith("//")) return@forEach
                val fields = if (line.contains('\t')) {
                    line.split('\t').filter { it.isNotEmpty() }
                } else {
                    line.split(' ', '\t').filter { it.isNotEmpty() }
                }
                if (fields.size < 3) return@forEach
                val count = fields[2].trim().toIntOrNull() ?: return@forEach
                if (count <= 0) return@forEach
                val prev = fields[0].trim()
                val next = fields[1].trim()
                if (prev.isEmpty() || next.isEmpty()) return@forEach
                out.add(Triple(prev, next, count))
            }
            return out
        }

        /**
         * The last [max] word tokens of [context] — letters plus intra-word
         * apostrophes — lowercased. Punctuation and other separators split
         * tokens.
         */
        fun tailTokens(context: String, max: Int): List<String> {
            val tokens = ArrayList<String>()
            val current = StringBuilder()
            for (ch in context) {
                if (ch.isLetter() || ch == '\'' || ch == '’') {
                    current.append(ch)
                } else if (current.isNotEmpty()) {
                    tokens.add(current.toString().lowercase())
                    current.setLength(0)
                }
            }
            if (current.isNotEmpty()) tokens.add(current.toString().lowercase())
            return if (tokens.size > max) tokens.takeLast(max) else tokens
        }
    }
}
