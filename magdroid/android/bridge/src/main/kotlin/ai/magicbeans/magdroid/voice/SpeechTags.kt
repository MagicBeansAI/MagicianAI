package ai.magicbeans.magdroid.voice

/**
 * The `<speech>` protocol, shared with the web and iOS.
 *
 * The assistant wraps the parts of a reply meant to be heard in
 * `<speech …>…</speech>`. Everything else is shown but not read — a table is
 * worth displaying and painful to listen to.
 */
object SpeechTags {
    private val body = Regex("""<speech\b[^>]*>([\s\S]*?)</speech>""", RegexOption.IGNORE_CASE)
    private val openTag = Regex("""<speech\b[^>]*>""", RegexOption.IGNORE_CASE)
    private val closeTag = Regex("""</speech>""", RegexOption.IGNORE_CASE)
    private val whitespace = Regex("""\s+""")

    /**
     * What to read aloud: the tagged bodies joined, or the whole reply when
     * there are no tags.
     *
     * An untagged reply is read in full — a typed answer with nothing marked
     * should still be speakable.
     */
    fun spokenText(raw: String): String {
        val bodies = bodies(raw)
        return if (bodies.isEmpty()) raw.trim() else bodies.joined()
    }

    fun hasSpeech(raw: String): Boolean = bodies(raw).isNotEmpty()

    /**
     * The reply without the wrappers, keeping what was inside them.
     *
     * iOS and the web leave the markers in the bubble. They are protocol, not
     * prose, and nobody reading a reply wants to see `<speech>` in it — so this
     * client strips them. The inner words are kept, so nothing is lost, and the
     * spoken and shown text stay the same sentences.
     */
    fun stripped(raw: String): String =
        closeTag.replace(openTag.replace(raw, ""), "")

    private fun bodies(raw: String): List<String> =
        body.findAll(raw)
            .map { whitespace.replace(it.groupValues[1], " ").trim() }
            .filter { it.isNotEmpty() }
            .toList()

    private fun List<String>.joined(): String = joinToString(" ")
}
