package ai.magicbeans.magdroid.voice

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonPrimitive

/**
 * Deciding whether a wake phrase was said.
 *
 * Kept apart from the audio and the decoder so the rule can be tested without
 * either. The iOS spotter's judgement lives tangled in its Vosk plumbing; the
 * decision is the part that gets subtly wrong, so here it is on its own.
 */
object WakeSpotter {

    /** Vosk's own token for "speech that is not one of the phrases". */
    const val UNKNOWN = "[unk]"

    /**
     * The grammar the decoder is restricted to.
     *
     * The phrases plus `[unk]`. Without the unknown alternative the decoder has
     * nowhere to put ordinary speech and must spend it on the phrases, which
     * makes every conversation in the room a wake word. It is not optional.
     */
    fun grammar(phrases: Collection<String>): String {
        val quoted = (phrases.map(::normalise).filter { it.isNotEmpty() }.distinct().sorted() + UNKNOWN)
            .joinToString(",") { "\"${it.replace("\"", "")}\"" }
        return "[$quoted]"
    }

    /**
     * Normalise a phrase to what the decoder can actually emit.
     *
     * Lower case, letters and single spaces. Punctuation and casing are not
     * things a speech decoder produces, so a phrase carrying them could never
     * match what comes back.
     */
    fun normalise(phrase: String): String = phrase
        .lowercase()
        .map { if (it.isLetter() || it.isWhitespace()) it else ' ' }
        .joinToString("")
        .split(" ")
        .filter { it.isNotBlank() }
        .joinToString(" ")

    /**
     * Whether a decoded utterance is one of the phrases.
     *
     * Whole-phrase, on word boundaries. A substring test fires "hey" inside
     * "heyday" and a prefix test fires "sam" inside "Samantha" — both are the
     * classic ways a wake word becomes unusable in a room where people talk.
     */
    fun matches(decoded: String, phrases: Collection<String>): String? {
        val words = normalise(decoded).split(" ").filter { it.isNotBlank() }
        if (words.isEmpty()) return null
        return phrases.firstOrNull { phrase ->
            val target = normalise(phrase).split(" ").filter { it.isNotBlank() }
            target.isNotEmpty() && words.containsInOrderContiguous(target)
        }
    }

    /** `text` for a final, `partial` while still speaking. */
    fun decoded(resultJson: String?): String? {
        if (resultJson.isNullOrBlank()) return null
        // Parsed with the serialization library rather than `org.json`: the
        // platform's is a stub under unit tests, so the decision this whole
        // object exists to make could not have been tested at all.
        val obj = runCatching {
            Json.parseToJsonElement(resultJson) as? JsonObject
        }.getOrNull() ?: return null
        fun field(name: String): String? =
            runCatching { obj[name]?.jsonPrimitive?.content }.getOrNull()?.takeIf { it.isNotBlank() }
        return field("text") ?: field("partial")
    }

    private fun List<String>.containsInOrderContiguous(target: List<String>): Boolean {
        if (target.size > size) return false
        for (start in 0..(size - target.size)) {
            if (subList(start, start + target.size) == target) return true
        }
        return false
    }
}
