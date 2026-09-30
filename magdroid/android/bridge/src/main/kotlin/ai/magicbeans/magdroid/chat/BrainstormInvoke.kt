package ai.magicbeans.magdroid.chat

/**
 * `@brainstorm` — a native lane, not a chat turn.
 *
 * The invocation never reaches the backend. It opens a thinking map seeded with
 * whatever followed it, because the map owns its own facilitator session and
 * posting the text as an ordinary turn would start a second conversation about
 * the same thought.
 *
 * This client left the lane out once, on the grounds that no server-side lane
 * answers to that name. True and beside the point: it is a client lane on iOS,
 * and a composer that will not offer it is missing a feature rather than
 * declining a nonexistent one.
 *
 * Mirrors `BrainstormInvoke` in `magios/Shared/MentionCatalog.swift`, typo and
 * all — `@brainstrom` is accepted there because people type it, and refusing it
 * here would make the same slip behave differently on two phones.
 */
object BrainstormInvoke {

    /**
     * Leading only, and followed by end, space, colon or comma.
     *
     * Anchored so a sentence that merely mentions brainstorming stays an
     * ordinary message; the lookahead so `@brainstorming` — a different word —
     * is not swallowed as an invocation.
     */
    private val INVOKE = Regex("""^\s*@brain(?:storm|strom)(?=$|[\s:,])""", RegexOption.IGNORE_CASE)

    private val INVOKE_AND_SEPARATORS =
        Regex("""^\s*@brain(?:storm|strom)(?=$|[\s:,])[\s,:]*""", RegexOption.IGNORE_CASE)

    fun isInvoke(text: String): Boolean = INVOKE.containsMatchIn(text)

    /**
     * What is left after the invocation, which becomes the first thought.
     *
     * An empty result is meaningful rather than a failure: a bare
     * `@brainstorm` opens capture for a fresh map, which is the
     * zero-ceremony path.
     */
    fun strip(text: String): String =
        INVOKE_AND_SEPARATORS.replaceFirst(text, "").trim()
}
