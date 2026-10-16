package ai.magicbeans.magdroid.keyboard

import android.content.Context
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Process-wide holder for the lazily-built correction engine and next-word
 * predictor — the Android counterpart of iOS's `CorrectionStore` +
 * `PredictorStore`.
 *
 * The engine build is the heavy step: parsing ~83k dictionary rows and
 * computing the symmetric-delete neighborhood. It runs exactly once, on a
 * background thread, kicked off from the keyboard's create path — never on a
 * keypress. Until it publishes, [engine] and [predictor] read null and the
 * keyboard falls back to what it had before this stack existed: the system
 * spell checker and learned words. The first keystrokes always work.
 *
 * iOS caps the merged dictionary at the top 30k words to stay far under the
 * keyboard extension's jetsam budget. Android has no extension jetsam, but the
 * same cap is kept anyway: the delete-neighborhood index grows superlinearly
 * with vocabulary, the top-30k covers the vast majority of real typing, and
 * two keyboards correcting from the same effective dictionary is the point.
 * Curated Indian-English/Hinglish words and learned words survive the cap
 * regardless of rank.
 *
 * There is no Foundation-Models tier here — that framework is Apple-only, and
 * the n-gram predictor is exactly the universal fallback iOS designed for
 * devices without it.
 */
object KeyboardIntelligence {

    @Volatile
    var engine: CorrectionEngine? = null
        private set

    @Volatile
    var predictor: NGramPredictor? = null
        private set

    private val building = AtomicBoolean(false)

    /** One-time background build. Cheap to call repeatedly; wire it at create. */
    fun warmUp(context: Context) {
        if (engine != null && predictor != null) return
        if (!building.compareAndSet(false, true)) return
        val app = context.applicationContext
        Executors.newSingleThreadExecutor { runnable ->
            Thread(runnable, "magican-keyboard-intelligence").apply { isDaemon = true }
        }.execute {
            runCatching { build(app) }
            // Deliberately not reset on failure: a build that threw (corrupt
            // asset, OOM) would throw identically on retry, and a keyboard
            // that rebuilds a broken index on every keypress is worse than one
            // that quietly stays on the fallback path.
        }
    }

    private fun build(app: Context) {
        fun read(name: String): String? = runCatching {
            app.assets.open("keyboard/$name.txt").bufferedReader().use { it.readText() }
        }.getOrNull()

        // Any of the three being absent is tolerated — a partial dictionary
        // yields fewer corrections, never a crash.
        val english = read("frequency_dictionary_en_82_765")
        val indian = read("indian_english_seed")
        val hinglish = read("hinglish_seed")

        val base = CorrectionDictionary.loadTabSeparated(listOfNotNull(english, indian, hinglish))
        val curated = CorrectionDictionary.loadTabSeparated(listOfNotNull(indian, hinglish))
        val learnedEntries =
            MagicanKeyboardStore.learnedEntries(app, CorrectionEngine.LEARN_THRESHOLD)

        val keep = curated.entries.keys + learnedEntries.map { it.first.lowercase() }
        val capped = base.cappedToTop(DICTIONARY_CAP, keeping = keep)

        engine = CorrectionEngine(
            dictionary = capped,
            learnedEntries = learnedEntries,
            // Live, so a word learned after this build is valid immediately —
            // validity must never wait for a process restart.
            isLearnedNow = { word ->
                MagicanKeyboardStore.isLearned(app, word, CorrectionEngine.LEARN_THRESHOLD)
            },
        )

        val seed = read("english_bigrams_seed")
            ?.let { NGramPredictor.parseSeed(it) }
            .orEmpty()
        predictor = NGramPredictor(
            seedEntries = seed,
            learnedNextWords = { prev -> MagicanKeyboardStore.nextWordsAfter(app, prev) },
        )
    }

    private const val DICTIONARY_CAP = 30_000
}
