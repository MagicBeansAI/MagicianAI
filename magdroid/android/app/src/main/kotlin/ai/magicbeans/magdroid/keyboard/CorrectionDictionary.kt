package ai.magicbeans.magdroid.keyboard

/**
 * The merged **word → frequency** vocabulary that backs the correction engine.
 *
 * A port of `magios/Shared/Correction/CorrectionDictionary.swift`, rule for
 * rule, because both keyboards load the *same* bundled word lists (English
 * base + curated Indian-English + curated Hinglish) and a word valid on one
 * phone must be valid on the other. Words are stored lowercased; on a
 * duplicate the **higher** frequency wins, so a curated seed can raise, never
 * lower, an English-base weight. Every key here is a *valid word* — nothing in
 * this table is ever autocorrected.
 */
class CorrectionDictionary private constructor(
    /** lowercased word → frequency (occurrence count / weight). */
    val entries: Map<String, Int>,
) {

    val count: Int get() = entries.size

    fun contains(word: String): Boolean = entries.containsKey(word.lowercase())

    fun frequency(of: String): Int = entries[of.lowercase()] ?: 0

    /**
     * A copy with extra `(word, frequency)` pairs merged in (max-on-duplicate).
     * Used to fold learned words into the effective dictionary without
     * mutating the shared base.
     */
    fun merging(pairs: List<Pair<String, Int>>): CorrectionDictionary {
        val table = entries.toMutableMap()
        pairs.forEach { (word, freq) -> insert(word, freq, table) }
        return CorrectionDictionary(table)
    }

    /**
     * Keep only the top-[limit] words by frequency, dropping the low-frequency
     * tail to fit a phone keyboard's memory budget. [keeping] forces entries to
     * survive the cap regardless of rank — every curated Indian/Hinglish word,
     * and every learned one, stays even when an English tail word would not.
     */
    fun cappedToTop(limit: Int, keeping: Set<String> = emptySet()): CorrectionDictionary {
        if (limit <= 0 || entries.size <= limit) return this
        val ranked = entries.entries.sortedWith(
            compareByDescending<Map.Entry<String, Int>> { it.value }.thenBy { it.key },
        )
        val table = HashMap<String, Int>(limit + keeping.size)
        ranked.take(limit).forEach { table[it.key] = it.value }
        keeping.forEach { word ->
            val key = word.lowercase()
            if (!table.containsKey(key)) entries[key]?.let { table[key] = it }
        }
        return CorrectionDictionary(table)
    }

    companion object {

        fun of(pairs: List<Pair<String, Int>>): CorrectionDictionary {
            val table = HashMap<String, Int>(pairs.size)
            pairs.forEach { (word, freq) -> insert(word, freq, table) }
            return CorrectionDictionary(table)
        }

        fun fromTable(table: Map<String, Int>): CorrectionDictionary =
            CorrectionDictionary(table)

        /**
         * Parse `word<TAB>count` text (SymSpell's format) and merge into one
         * dictionary. Tolerant on purpose: blank lines, comment lines (`#`,
         * `//`), rows without a numeric count and rows with too few fields are
         * skipped rather than fatal — a partial dictionary yields fewer
         * corrections, never a crash. Whitespace-separated is accepted too;
         * SymSpell's own English list is space-delimited.
         */
        fun loadTabSeparated(texts: List<String>): CorrectionDictionary {
            val table = HashMap<String, Int>(90_000)
            texts.forEach { text ->
                text.lineSequence().forEach { line -> parseLine(line, table) }
            }
            return CorrectionDictionary(table)
        }

        /** One `word<sep>count` line. Internal so the tolerant parser is testable. */
        internal fun parseLine(rawLine: String, table: MutableMap<String, Int>) {
            val line = rawLine.trim()
            if (line.isEmpty() || line.startsWith("#") || line.startsWith("//")) return
            val fields = if (line.contains('\t')) {
                line.split('\t').filter { it.isNotEmpty() }
            } else {
                line.split(' ', '\t').filter { it.isNotEmpty() }
            }
            if (fields.size < 2) return
            val freq = fields[1].trim().toIntOrNull() ?: return
            if (freq <= 0) return
            insert(fields[0], freq, table)
        }

        private fun insert(word: String, freq: Int, table: MutableMap<String, Int>) {
            val key = word.lowercase().trim()
            if (key.isEmpty() || freq <= 0) return
            val existing = table[key]
            if (existing == null || freq > existing) table[key] = freq
        }
    }
}
