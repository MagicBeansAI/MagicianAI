package ai.magicbeans.magdroid.keyboard

import android.content.Context
import android.view.textservice.SuggestionsInfo
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.builtins.MapSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.Json
import java.util.UUID

@Serializable
enum class KeyboardLane { Write, Ask, Act }

@Serializable
data class KeyboardSkill(
    val id: String = UUID.randomUUID().toString(),
    val label: String,
    val lane: KeyboardLane,
    val template: String,
) {
    fun resolved(text: String): String = template.replace("{text}", text)

    fun isValid(): Boolean = label.isNotBlank() && template.isNotBlank()
}

enum class KeyboardLanguage(val wire: String, val label: String, val languageTag: String) {
    India("en_IN", "English (India)", "en-IN"),
    UK("en_GB", "English (UK)", "en-GB"),
    US("en_US", "English (US)", "en-US"),
    ;

    companion object {
        fun from(wire: String?): KeyboardLanguage = entries.firstOrNull { it.wire == wire } ?: India
    }
}

/** Process-shared keyboard configuration used by Settings and the IME service. */
object MagicanKeyboardStore {
    private const val File = "magican.keyboard.v1"
    private const val Skills = "skills"
    private const val Language = "language"
    private const val AskSession = "ask_session"
    private const val LearnedWords = "learned_words"

    private val json = Json { ignoreUnknownKeys = true; encodeDefaults = true }

    val defaultSkills = listOf(
        KeyboardSkill(label = "Fix tone", lane = KeyboardLane.Write,
            template = "Rewrite this to be clear, polished, and professional, keeping the meaning and language:\n\n{text}"),
        KeyboardSkill(label = "Shorten", lane = KeyboardLane.Write,
            template = "Make this shorter and punchier without losing the point:\n\n{text}"),
        KeyboardSkill(label = "Translate → EN", lane = KeyboardLane.Write,
            template = "Translate this to natural English:\n\n{text}"),
        KeyboardSkill(label = "Reply", lane = KeyboardLane.Write,
            template = "Write a short and friendly reply to this:\n\n{text}"),
        KeyboardSkill(label = "Continue", lane = KeyboardLane.Write,
            template = "Continue this in the same voice:\n\n{text}"),
        KeyboardSkill(label = "Summarize", lane = KeyboardLane.Ask,
            template = "Summarize this concisely:\n\n{text}"),
        KeyboardSkill(label = "Add task", lane = KeyboardLane.Act,
            template = "Create a task from this and do it: {text}"),
    )

    private fun prefs(context: Context) = context.applicationContext
        .getSharedPreferences(File, Context.MODE_PRIVATE)

    /**
     * Everything below is read on the keystroke path, so it is read from
     * memory.
     *
     * These were all `getSharedPreferences` plus a `decodeFromString` per call,
     * and the keyboard called them per key press: the skill list rebuilt the
     * action row on every letter, and the learned lexicon — up to two thousand
     * entries — was parsed again for every candidate refresh. On the main
     * thread, between a finger landing and a glyph appearing.
     *
     * The cache is the truth once loaded; every writer updates it and then
     * persists, so nothing reads back what it just wrote.
     */
    private val lock = Any()

    @Volatile private var skillCache: List<KeyboardSkill>? = null

    @Volatile private var languageCache: KeyboardLanguage? = null

    @Volatile private var lexicon: MutableMap<String, Int>? = null

    /** Coalesces bursts of typing into one write instead of one per word. */
    private val writer by lazy {
        java.util.concurrent.Executors.newSingleThreadExecutor { runnable ->
            Thread(runnable, "magican-keyboard-store").apply { isDaemon = true }
        }
    }

    @Volatile private var pendingLexicon: Map<String, Int>? = null

    /** Learned `(prev\tnext)` bigram counts, same lifecycle as the lexicon. */
    @Volatile private var bigrams: MutableMap<String, Int>? = null

    @Volatile private var pendingBigrams: Map<String, Int>? = null

    private const val MaxLearnedWords = 2_000
    private const val MaxLearnedBigrams = 2_000
    private const val LearnedBigrams = "learned_bigrams"

    fun loadSkills(context: Context): List<KeyboardSkill> {
        skillCache?.let { return it }
        val raw = prefs(context).getString(Skills, null)
        val loaded = raw?.let {
            runCatching {
                json.decodeFromString(ListSerializer(KeyboardSkill.serializer()), it)
                    .filter(KeyboardSkill::isValid)
            }.getOrNull()?.takeIf(List<KeyboardSkill>::isNotEmpty)
        } ?: defaultSkills
        skillCache = loaded
        return loaded
    }

    fun saveSkills(context: Context, skills: List<KeyboardSkill>) {
        val valid = skills.filter(KeyboardSkill::isValid)
        require(valid.isNotEmpty()) { "At least one valid keyboard skill is required." }
        skillCache = valid
        prefs(context).edit().putString(
            Skills,
            json.encodeToString(ListSerializer(KeyboardSkill.serializer()), valid),
        ).apply()
    }

    fun resetSkills(context: Context) {
        skillCache = defaultSkills
        prefs(context).edit().remove(Skills).apply()
    }

    fun language(context: Context): KeyboardLanguage {
        languageCache?.let { return it }
        val loaded = KeyboardLanguage.from(prefs(context).getString(Language, null))
        languageCache = loaded
        return loaded
    }

    fun setLanguage(context: Context, language: KeyboardLanguage) {
        languageCache = language
        prefs(context).edit().putString(Language, language.wire).apply()
    }

    fun askSession(context: Context): String? = prefs(context).getString(AskSession, null)
    fun setAskSession(context: Context, id: String) {
        prefs(context).edit().putString(AskSession, id).apply()
    }
    fun clearAskSession(context: Context) { prefs(context).edit().remove(AskSession).apply() }

    fun learnWord(context: Context, raw: String) {
        val word = raw.trim().lowercase().takeIf { it.length in 2..40 && it.all(Char::isLetter) } ?: return
        val snapshot: Map<String, Int>
        synchronized(lock) {
            val counts = mutableLexicon(context)
            counts[word] = (counts[word] ?: 0).coerceAtMost(9_999) + 1
            // Bound both storage and suggestion work. Least useful/oldest ties
            // are discarded; this is a convenience lexicon, never conversation
            // memory. Pruned only on the words that cross the bound rather than
            // sorting the whole lexicon for every word typed.
            if (counts.size > MaxLearnedWords) {
                val kept = counts.entries
                    .sortedByDescending(Map.Entry<String, Int>::value)
                    .take(MaxLearnedWords)
                    .associate(Map.Entry<String, Int>::toPair)
                counts.clear()
                counts.putAll(kept)
            }
            snapshot = counts.toMap()
        }
        persist(context, snapshot)
    }

    /**
     * Write off the typing thread, keeping only the newest pending state.
     *
     * Serializing two thousand entries took as long as it took, and it used to
     * happen inline at every space bar. The lexicon in memory is already
     * correct by the time this is queued, so a burst of typing costs one write
     * rather than one per word.
     */
    private fun persist(context: Context, snapshot: Map<String, Int>) {
        val app = context.applicationContext
        val alreadyQueued = synchronized(lock) {
            val queued = pendingLexicon != null
            pendingLexicon = snapshot
            queued
        }
        if (alreadyQueued) return
        writer.execute {
            val toWrite = synchronized(lock) { pendingLexicon.also { pendingLexicon = null } } ?: return@execute
            runCatching {
                prefs(app).edit().putString(
                    LearnedWords,
                    json.encodeToString(MapSerializer(String.serializer(), Int.serializer()), toWrite),
                ).apply()
            }
        }
    }

    fun suggestions(context: Context, prefix: String, limit: Int = 3): List<String> {
        val clean = prefix.trim().lowercase()
        if (clean.isEmpty()) return emptyList()
        return learned(context).entries.asSequence()
            .filter { (word, _) -> word.startsWith(clean) && word != clean }
            .sortedWith(compareByDescending<Map.Entry<String, Int>> { it.value }.thenBy { it.key })
            .map(Map.Entry<String, Int>::key)
            .take(limit.coerceIn(0, 8))
            .toList()
    }

    fun learnedWordCount(context: Context): Int = learned(context).size

    fun resetLearnedWords(context: Context) {
        synchronized(lock) {
            lexicon = mutableMapOf()
            pendingLexicon = null
            bigrams = mutableMapOf()
            pendingBigrams = null
        }
        prefs(context).edit().remove(LearnedWords).remove(LearnedBigrams).apply()
    }

    // ── Correction-engine accessors ──────────────────────────────────────────
    //
    // The engine trusts a word only once it has been typed [threshold]+ times
    // (the same rule iOS's LearnedWordsStore keeps): a store that trusted on
    // first sight would make every typo immune to correction the moment it was
    // typed, which defeats the corrector on exactly the words it exists for.

    /** Words at/above [threshold], as (word, count) — the fold-in set. */
    fun learnedEntries(context: Context, threshold: Int): List<Pair<String, Int>> =
        learned(context).entries
            .filter { it.value >= threshold }
            .map { it.key to it.value }

    /** True when [word] has crossed [threshold] natural repeats (or was trusted). */
    fun isLearned(context: Context, word: String, threshold: Int): Boolean =
        (learned(context)[word.trim().lowercase()] ?: 0) >= threshold

    /**
     * Trust a word immediately, without waiting for natural repeats. One
     * explicit signal — the owner reverting an autocorrection back to what they
     * typed — is enough: the corrector must never fight the same word twice.
     * Never lowers an existing higher count.
     */
    fun trustWord(context: Context, word: String, threshold: Int) {
        val key = word.trim().lowercase().takeIf { it.length in 2..40 && it.all(Char::isLetter) } ?: return
        val snapshot: Map<String, Int>
        synchronized(lock) {
            val counts = mutableLexicon(context)
            counts[key] = maxOf(counts[key] ?: 0, threshold)
            snapshot = counts.toMap()
        }
        persist(context, snapshot)
    }

    // ── Learned bigrams (next-word prediction) ───────────────────────────────

    /**
     * Record one `(prev → next)` pair the owner actually typed. Bounded like
     * the lexicon and persisted off the typing thread the same way.
     */
    fun learnBigram(context: Context, prev: String, next: String) {
        val p = prev.trim().lowercase().takeIf { it.length in 1..40 && it.all(Char::isLetter) } ?: return
        val n = next.trim().lowercase().takeIf { it.length in 1..40 && it.all(Char::isLetter) } ?: return
        val key = "$p\t$n"
        val snapshot: Map<String, Int>
        synchronized(lock) {
            val counts = mutableBigrams(context)
            counts[key] = (counts[key] ?: 0).coerceAtMost(9_999) + 1
            if (counts.size > MaxLearnedBigrams) {
                val kept = counts.entries
                    .sortedByDescending(Map.Entry<String, Int>::value)
                    .take(MaxLearnedBigrams)
                    .associate(Map.Entry<String, Int>::toPair)
                counts.clear()
                counts.putAll(kept)
            }
            snapshot = counts.toMap()
        }
        persistBigrams(context, snapshot)
    }

    /** The owner's learned next words after [prev], as (word, count). */
    fun nextWordsAfter(context: Context, prev: String): List<Pair<String, Int>> {
        val p = prev.trim().lowercase()
        if (p.isEmpty()) return emptyList()
        val prefix = "$p\t"
        return synchronized(lock) {
            mutableBigrams(context).entries
                .filter { it.key.startsWith(prefix) }
                .map { it.key.substringAfter('\t') to it.value }
        }
    }

    private fun persistBigrams(context: Context, snapshot: Map<String, Int>) {
        val app = context.applicationContext
        val alreadyQueued = synchronized(lock) {
            val queued = pendingBigrams != null
            pendingBigrams = snapshot
            queued
        }
        if (alreadyQueued) return
        writer.execute {
            val toWrite = synchronized(lock) { pendingBigrams.also { pendingBigrams = null } } ?: return@execute
            runCatching {
                prefs(app).edit().putString(
                    LearnedBigrams,
                    json.encodeToString(MapSerializer(String.serializer(), Int.serializer()), toWrite),
                ).apply()
            }
        }
    }

    /** Parsed once per process; the caller must hold [lock]. */
    private fun mutableBigrams(context: Context): MutableMap<String, Int> {
        bigrams?.let { return it }
        val raw = prefs(context).getString(LearnedBigrams, null)
        val parsed = raw?.let {
            runCatching {
                json.decodeFromString(MapSerializer(String.serializer(), Int.serializer()), it)
            }.getOrNull()
        }.orEmpty()
        return parsed.toMutableMap().also { bigrams = it }
    }

    private fun learned(context: Context): Map<String, Int> =
        synchronized(lock) { mutableLexicon(context) }

    /** Parsed once per process; the caller must hold [lock]. */
    private fun mutableLexicon(context: Context): MutableMap<String, Int> {
        lexicon?.let { return it }
        val raw = prefs(context).getString(LearnedWords, null)
        val parsed = raw?.let {
            runCatching {
                json.decodeFromString(MapSerializer(String.serializer(), Int.serializer()), it)
            }.getOrNull()
        }.orEmpty()
        return parsed.toMutableMap().also { lexicon = it }
    }
}

/**
 * Pure contract used by the IME before it is allowed to read field contents.
 *
 * A variation only means anything alongside its class. Android reuses the same
 * bit values across classes — `TYPE_TEXT_VARIATION_URI` and
 * `TYPE_NUMBER_VARIATION_PASSWORD` are both `0x10` — so testing the variation
 * on its own read every URL field as a password field. Magican's actions were
 * refused in address bars, and the strip that carries them disappeared there,
 * for a field that is ordinary text.
 *
 * Numeric fields stay wholesale secure, which is what makes the numeric
 * password variation redundant rather than missing.
 */
fun isSecureKeyboardField(inputType: Int): Boolean {
    val klass = inputType and android.text.InputType.TYPE_MASK_CLASS
    if (klass == android.text.InputType.TYPE_CLASS_NUMBER) return true
    if (klass != android.text.InputType.TYPE_CLASS_TEXT) return false
    return inputType and android.text.InputType.TYPE_MASK_VARIATION in setOf(
        android.text.InputType.TYPE_TEXT_VARIATION_PASSWORD,
        android.text.InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD,
        android.text.InputType.TYPE_TEXT_VARIATION_WEB_PASSWORD,
    )
}

/** Honor the editor's candidate policy and avoid correcting structured input. */
internal fun supportsKeyboardCandidates(inputType: Int): Boolean {
    if (isSecureKeyboardField(inputType)) return false
    if (inputType and android.text.InputType.TYPE_MASK_CLASS != android.text.InputType.TYPE_CLASS_TEXT) return false
    if (inputType and android.text.InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS != 0) return false
    return inputType and android.text.InputType.TYPE_MASK_VARIATION !in setOf(
        android.text.InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS,
        android.text.InputType.TYPE_TEXT_VARIATION_WEB_EMAIL_ADDRESS,
        android.text.InputType.TYPE_TEXT_VARIATION_URI,
        android.text.InputType.TYPE_TEXT_VARIATION_FILTER,
    )
}

/**
 * Combine Android's selected spell-checker result with this install's learned
 * completions. The platform result wins; exact and duplicate words are not
 * useful candidate buttons.
 */
/**
 * Merge the candidate lanes into the row's three cells.
 *
 * Order is the priority: the correction engine first — it ranks completions
 * and adjacency-verified fixes over the same dictionary iOS ships — then the
 * system spell checker as the fallback for words the bundled dictionary
 * cannot cover, then the learned-word completions. The exact typed word is
 * never offered back.
 */
internal fun keyboardCandidates(
    prefix: String,
    engine: List<String> = emptyList(),
    spelling: List<String> = emptyList(),
    learned: List<String> = emptyList(),
    limit: Int = 3,
): List<String> {
    val current = prefix.trim()
    if (current.isEmpty()) return emptyList()
    return (engine.asSequence() + spelling.asSequence() + learned.asSequence())
        .map(String::trim)
        .filter(String::isNotEmpty)
        .filterNot { it.equals(current, ignoreCase = true) }
        .distinctBy { it.lowercase() }
        .take(limit.coerceIn(0, 8))
        .toList()
}

internal fun spellingCandidates(info: SuggestionsInfo, limit: Int = 3): List<String> {
    if (info.suggestionsAttributes and SuggestionsInfo.RESULT_ATTR_DONT_SHOW_UI_FOR_SUGGESTIONS != 0) {
        return emptyList()
    }
    return buildList {
        repeat(info.suggestionsCount.coerceAtMost(limit.coerceIn(0, 8))) { index ->
            add(info.getSuggestionAt(index))
        }
    }
}

internal fun candidateResultIsCurrent(
    resultSequence: Int,
    activeSequence: Int,
    requestedPrefix: String,
    currentPrefix: String,
    fieldSupportsCandidates: Boolean,
): Boolean = fieldSupportsCandidates &&
    resultSequence == activeSequence &&
    requestedPrefix == currentPrefix
