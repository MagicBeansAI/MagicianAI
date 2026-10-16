package ai.magicbeans.magdroid.keyboard

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The correction/prediction stack, ported from iOS's Shared/Correction and
 * Shared/Prediction. These pin the *algorithms* — the same dictionaries ship
 * on both platforms, and a word corrected one way on one phone must correct
 * the same way on the other.
 */
class SymSpellCoreTest {

    private fun core(vararg entries: Pair<String, Int>): SymSpellCore =
        SymSpellCore(CorrectionDictionary.of(entries.toList()), maxEditDistance = 2)

    @Test
    fun `a transposition is one edit, and the classic typo corrects`() {
        assertEquals(1, SymSpellCore.damerauLevenshtein("teh", "the"))
        assertEquals(1, SymSpellCore.damerauLevenshtein("amd", "and"))
        assertEquals(3, SymSpellCore.damerauLevenshtein("kitten", "sitting"))
        assertEquals(0, SymSpellCore.damerauLevenshtein("same", "same"))
    }

    @Test
    fun `distance verification early-exits beyond the budget`() {
        // Distance is 3; with a budget of 1 the answer is "more than 1", spelled
        // budget + 1 — the caller only ever compares against the budget.
        assertEquals(2, SymSpellCore.damerauLevenshtein("kitten", "sitting", maxDistance = 1))
    }

    @Test
    fun `the delete neighborhood never contains the empty string`() {
        val edits = SymSpellCore.editsPrefix("ab", maxEditDistance = 2)
        assertEquals(setOf("a", "b"), edits)
        // Deleting the last char would yield "" and match every word.
        assertFalse("" in SymSpellCore.editsPrefix("a", maxEditDistance = 2))
    }

    @Test
    fun `lookup ranks by distance first, then frequency`() {
        val core = core("the" to 100, "then" to 500, "tea" to 50)
        val results = core.lookup("teh")
        // "the" is distance 1 (transposition); "then"/"tea" are distance 2 —
        // the higher-frequency "then" must not outrank a closer fix.
        assertEquals("the", results.first().term)
    }

    @Test
    fun `long tokens cap to one edit`() {
        val core = core("understanding" to 100)
        // Nine+ chars: two independent typos are no longer credible.
        assertTrue(core.lookup("understandxy").isEmpty())
        assertEquals("understanding", core.lookup("understandin").firstOrNull()?.term)
    }

    @Test
    fun `completions rank by frequency and exclude shorter-or-equal words`() {
        val core = core("the" to 10, "then" to 500, "them" to 300, "theory" to 50)
        val completions = core.completions(startingWith = "the")
        assertEquals(listOf("then", "them", "theory"), completions.map { it.term })
    }
}

class CorrectionDictionaryTest {

    @Test
    fun `parser tolerates comments, blanks and rubbish, and takes both separators`() {
        val table = mutableMapOf<String, Int>()
        listOf(
            "# a comment",
            "// another",
            "",
            "word-without-count",
            "the\t100",
            "and 90",
            "bad count",
            "The\t40", // duplicate, lower — must not lower the stored 100
        ).forEach { CorrectionDictionary.parseLine(it, table) }
        assertEquals(mapOf("the" to 100, "and" to 90), table)
    }

    @Test
    fun `capping keeps forced words past the cut`() {
        val dictionary = CorrectionDictionary.of(
            listOf("common" to 1000, "middling" to 500, "rare" to 1, "chai" to 2),
        )
        val capped = dictionary.cappedToTop(2, keeping = setOf("chai"))
        assertTrue(capped.contains("common"))
        assertTrue(capped.contains("middling"))
        assertTrue("curated words survive the cap regardless of rank", capped.contains("chai"))
        assertFalse(capped.contains("rare"))
    }

    @Test
    fun `merging is max-on-duplicate`() {
        val base = CorrectionDictionary.of(listOf("word" to 100))
        assertEquals(100, base.merging(listOf("word" to 5)).frequency("word"))
        assertEquals(900, base.merging(listOf("word" to 900)).frequency("word"))
    }
}

class KeyboardAdjacencyTest {

    private val qwerty = KeyboardAdjacency.QWERTY

    @Test
    fun `physical neighbors are neighbors and distant keys are not`() {
        assertTrue(qwerty.areNeighbors('m', 'n'))
        assertTrue(qwerty.areNeighbors('q', 'a'))
        assertFalse(qwerty.areNeighbors('m', 'i'))
        assertFalse(qwerty.areNeighbors('q', 'p'))
    }

    @Test
    fun `a fat-finger substitution scores full and a distant one scores zero`() {
        assertEquals(1.0, qwerty.adjacencyScore("amd", "and"), 0.0)
        assertEquals(0.0, qwerty.adjacencyScore("amd", "aid"), 0.0)
    }

    @Test
    fun `a transposition scores by whether the swapped keys neighbor`() {
        // e/h are not physical neighbors, but a swap is still a typing slip.
        assertEquals(0.6, qwerty.adjacencyScore("teh", "the"), 0.0)
    }

    @Test
    fun `inserts and deletes are neutral`() {
        assertEquals(0.5, qwerty.adjacencyScore("helo", "hello"), 0.0)
    }
}

class CorrectionEngineTest {

    private fun engine(
        entries: List<Pair<String, Int>>,
        learned: List<Pair<String, Int>> = emptyList(),
        isLearnedNow: (String) -> Boolean = { false },
    ) = CorrectionEngine(
        dictionary = CorrectionDictionary.of(entries),
        learnedEntries = learned,
        isLearnedNow = isLearnedNow,
    )

    @Test
    fun `a valid word is never autocorrected`() {
        val engine = engine(listOf("the" to 100, "then" to 50))
        assertNull(engine.autocorrection("the"))
    }

    @Test
    fun `the classic transposition corrects`() {
        val engine = engine(listOf("the" to 22_000_000, "tea" to 5_000))
        assertEquals("the", engine.autocorrection("teh"))
    }

    @Test
    fun `a word learned after the build is valid immediately`() {
        // "jugaad" is not in the dictionary, so it would be fair game — the
        // live check is what keeps a just-learned word out of the corrector
        // without waiting for a rebuild.
        val engine = engine(
            listOf("jugged" to 1_000),
            isLearnedNow = { it == "jugaad" },
        )
        assertNull(engine.autocorrection("jugaad"))
    }

    @Test
    fun `short tokens only accept one-edit fixes`() {
        // "cet" → "cat" is distance 1: fires. "crt" → "crate" is distance 2,
        // and a two-edit "fix" on a token of four chars or fewer is usually a
        // different word — the gate must stay shut.
        assertEquals("cat", engine(listOf("cat" to 1_000)).autocorrection("cet"))
        assertNull(engine(listOf("crate" to 1_000)).autocorrection("crt"))
    }

    @Test
    fun `an ambiguous fix stays in the strip rather than auto-applying`() {
        // "bxg" is one edit from both, equal frequency, and `x` neighbors
        // neither `i` nor `u` — no signal separates them, so the gate must
        // not pick one. (Two earlier drafts failed because the substituted
        // letters WERE offset-QWERTY neighbors — s under a, x under a — and
        // adjacency correctly broke the tie. The gate only holds when nothing
        // genuinely separates the rivals.)
        val engine = engine(listOf("big" to 1_000, "bug" to 1_000))
        assertNull(engine.autocorrection("bxg"))
    }

    @Test
    fun `adjacency breaks a frequency tie confidently`() {
        // "cst": s neighbors a but not u, so "cat" is the plausible
        // fat-finger and the gate is right to fire.
        val engine = engine(listOf("cat" to 1_000, "cut" to 1_000))
        assertEquals("cat", engine.autocorrection("cst"))
    }

    @Test
    fun `suggestions offer completions first and never the typed word`() {
        val engine = engine(listOf("hello" to 900, "help" to 800, "hell" to 100))
        val suggestions = engine.suggestions("hel")
        assertEquals(listOf("hello", "help", "hell"), suggestions)
        assertFalse("hel" in engine.suggestions("hel"))
    }

    @Test
    fun `learned words are folded in and suggested`() {
        val engine = engine(
            listOf("hello" to 900),
            learned = listOf("helical" to 3),
        )
        assertTrue("helical" in engine.suggestions("heli"))
        assertNull("folded learned words are valid", engine.autocorrection("helical"))
    }

    @Test
    fun `capitalization carries from the typed word onto the fix`() {
        assertEquals("The", CorrectionEngine.preserveCapitalization("Teh", "the"))
        assertEquals("the", CorrectionEngine.preserveCapitalization("teh", "the"))
    }
}

class NGramPredictorTest {

    private fun predictor(
        seed: List<Triple<String, String, Int>>,
        learned: Map<String, List<Pair<String, Int>>> = emptyMap(),
    ) = NGramPredictor(
        seedEntries = seed,
        learnedNextWords = { prev -> learned[prev].orEmpty() },
    )

    @Test
    fun `seed bigrams rank by weight`() {
        val p = predictor(
            listOf(
                Triple("i", "am", 95),
                Triple("i", "have", 92),
                Triple("i", "will", 88),
                Triple("i", "was", 80),
            ),
        )
        assertEquals(listOf("am", "have", "will"), p.predict("I"))
    }

    @Test
    fun `a learned pair outranks the seed once boosted`() {
        val p = predictor(
            seed = listOf(Triple("good", "morning", 50)),
            learned = mapOf("good" to listOf("vibes" to 20)),
        )
        // 20 × the default ×4 boost = 80 > 50.
        assertEquals("vibes", p.predict("good").first())
    }

    @Test
    fun `the just-typed words are never suggested back`() {
        val p = predictor(
            seed = listOf(Triple("the", "the", 100), Triple("the", "end", 10)),
        )
        assertEquals(listOf("end"), p.predict("the"))
    }

    @Test
    fun `two words of history lift what follows the earlier one`() {
        val p = predictor(
            seed = listOf(Triple("you", "are" , 10), Triple("you", "can", 10)),
            learned = mapOf("thank" to listOf("are" to 5)),
        )
        // "thank you" → the trigram-ish lift breaks the tie toward "are".
        assertEquals("are", p.predict("thank you").first())
    }

    @Test
    fun `tokenisation keeps apostrophes and splits on punctuation`() {
        assertEquals(listOf("don't", "worry"), NGramPredictor.tailTokens("Hey. Don't worry", max = 2))
        assertEquals(listOf("worry"), NGramPredictor.tailTokens("don't worry", max = 1))
        assertTrue(NGramPredictor.tailTokens("123 456", max = 2).isEmpty())
    }

    @Test
    fun `seed parsing tolerates comments and malformed rows`() {
        val parsed = NGramPredictor.parseSeed(
            """
            # comment
            i	am	95
            broken row
            i	will	notanumber
            you can 12
            """.trimIndent(),
        )
        assertEquals(
            listOf(Triple("i", "am", 95), Triple("you", "can", 12)),
            parsed,
        )
    }
}
