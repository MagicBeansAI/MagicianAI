package ai.magicbeans.magdroid.chat

/**
 * Which canvas a lesson draws on.
 *
 * `ScreenOverlay` annotates a visual source that is already there; `Blackboard`
 * is source-free — a darkened canvas with the concept drawn from scratch.
 * Mirrors `TutorCanvasMode` in `tutor.rs` and in `TutorInvoke.swift`.
 *
 * This is *what* the lesson draws on, not *where* it is drawn. Where is
 * [ai.magicbeans.magdroid.tutor.TutorSurface], which answers a different
 * question — whether the overlay grant exists on this handset.
 */
enum class TutorCanvasMode { ScreenOverlay, Blackboard }

/**
 * `@tutor` in the composer, and the spoken grammar behind it.
 *
 * A port of `TutorInvoke.swift`, which is itself a port of the web rule in
 * `ChatPanel.svelte`. All three must agree: the same sentence typed on a phone,
 * spoken to a phone, or pasted into the web composer has to start the same
 * lesson, and the backend is told which canvas will draw before it sends the
 * first shape.
 *
 * What was here before was `text.contains("@tutor")`, which is a different rule
 * in three ways that matter. Unanchored, so "ask the @tutor about this later"
 * opened a lesson mid-sentence. No typo tolerance, so `@tutur` — accepted on
 * iOS and the web precisely because people type it — was posted as ordinary
 * chat. And no spoken form, so "hey tutor" was never an invocation at all.
 */
object TutorInvoke {

    /** Which feature a spoken command asked for. */
    enum class VoiceFeature { Tutor, AppCopilot }

    /**
     * A recognised spoken command, and the capability facts it implies.
     *
     * [normalizedText] is the command rewritten in canonical composer form, so
     * a spoken turn and a typed one reach the backend identically.
     */
    data class VoiceInvocation(
        val feature: VoiceFeature,
        val canvasMode: TutorCanvasMode,
        val quick: Boolean,
        val normalizedText: String,
    ) {
        val requiresScreenCapture: Boolean get() = canvasMode == TutorCanvasMode.ScreenOverlay
    }

    private data class CommandToken(val value: String, val end: Int)

    /**
     * Leading only, and followed by end, space, colon or comma.
     *
     * Anchored so a sentence that merely mentions the tutor stays an ordinary
     * message; the lookahead so `@tutorial` — a different word — is not
     * swallowed as an invocation.
     */
    private val INVOKE = Regex(
        """^\s*(@tut(?:or|ur)|hey[\s,]+tut(?:or|ur))(?=$|[\s:,])""",
        RegexOption.IGNORE_CASE,
    )

    private val INVOKE_AND_SEPARATORS = Regex(
        """^\s*(@tut(?:or|ur)|hey[\s,]+tut(?:or|ur))(?=$|[\s:,])[\s,:]*""",
        RegexOption.IGNORE_CASE,
    )

    private val TOKEN = Regex("""[\p{L}\p{N}@#_-]+""")
    private val LEADING_SEPARATORS = Regex("""^[\s,:;-]+""")

    private val STARTERS = setOf("hey", "start", "open", "launch", "use")
    private val QUICK_TOKENS = setOf("quick", "#quick")
    private val SCREEN_NOUNS = setOf("screen", "screenshot", "screencap", "screen-capture", "display")
    private val SCREEN_ACTIONS = setOf("take", "capture", "use", "share", "show")

    /**
     * True when the composer text opens a lesson.
     *
     * `@copilot` is deliberately not a tutor invoke, matching iOS. It names a
     * different thing on each platform — desktop UI mutation there, the Pilot
     * agent driving this handset here — and neither of them is a canvas the
     * tutor draws on. Treating it as one opened a blank blackboard over a turn
     * that was never going to draw a shape.
     */
    fun isTutorInvoke(text: String): Boolean = INVOKE.containsMatchIn(text)

    /**
     * The bare concept, with a leading `@tutor` / `hey tutor` removed.
     *
     * The canvas re-prepends `@tutor ` when it sends, so it is handed the
     * concept alone. Text with no leading invoke comes back trimmed and
     * otherwise untouched.
     */
    fun strip(text: String): String = INVOKE_AND_SEPARATORS.replaceFirst(text, "").trim()

    /** A staged image means annotate it; nothing staged means draw from scratch. */
    fun mode(hasImage: Boolean): TutorCanvasMode =
        if (hasImage) TutorCanvasMode.ScreenOverlay else TutorCanvasMode.Blackboard

    /**
     * The spoken grammar, shared with the web and the backend.
     *
     * Only a *leading* command takes over; a passing mention stays an ordinary
     * message. Returning the capability facts rather than a bare boolean is
     * what lets a caller refuse a screen capture it cannot perform instead of
     * silently posting the command as chat.
     */
    fun parseVoiceGuidedFlow(text: String): VoiceInvocation? {
        val tokens = commandTokens(text)
        var cursor = 0
        if (tokens.getOrNull(cursor)?.value.orEmpty() in STARTERS) cursor += 1

        var quick = false
        if (tokens.getOrNull(cursor)?.value.orEmpty() in QUICK_TOKENS) {
            quick = true
            cursor += 1
        }

        val feature = when (tokens.getOrNull(cursor)?.value) {
            "tutor", "tutur", "@tutor", "@tutur" -> {
                cursor += 1
                VoiceFeature.Tutor
            }

            "copilot", "app-copilot", "@copilot", "@appcopilot", "@app-copilot", "@app_copilot" -> {
                cursor += 1
                VoiceFeature.AppCopilot
            }

            "app" -> {
                if (tokens.getOrNull(cursor + 1)?.value != "copilot") return null
                cursor += 2
                VoiceFeature.AppCopilot
            }

            else -> return null
        }

        if (tokens.getOrNull(cursor)?.value.orEmpty() in QUICK_TOKENS) {
            quick = true
            cursor += 1
        }
        val hasCanonicalQuick = tokens.drop(cursor).any { it.value == "#quick" }
        quick = quick || hasCanonicalQuick

        // Source selection is command grammar, not topic inference. Only the
        // selector immediately after `Tutor [Quick]` may choose the canvas —
        // otherwise a lesson *about* screens would capture the screen.
        val explicitBlackboard = tokens.getOrNull(cursor)?.value == "blackboard"
        val screen = feature == VoiceFeature.AppCopilot ||
            (!explicitBlackboard && requestsScreen(tokens.drop(cursor)))

        val commandEnd = tokens.getOrNull(maxOf(0, cursor - 1))?.end ?: 0
        val remainder = LEADING_SEPARATORS.replaceFirst(text.substring(commandEnd), "").trim()
        var normalized = if (feature == VoiceFeature.AppCopilot) "@copilot" else "@tutor"
        if (quick && !hasCanonicalQuick) normalized += " #quick"
        if (remainder.isNotEmpty()) normalized += " $remainder"

        return VoiceInvocation(
            feature = feature,
            canvasMode = if (screen) TutorCanvasMode.ScreenOverlay else TutorCanvasMode.Blackboard,
            quick = quick,
            normalizedText = normalized,
        )
    }

    private fun commandTokens(text: String): List<CommandToken> =
        TOKEN.findAll(text)
            .map { CommandToken(it.value.lowercase(), it.range.last + 1) }
            .toList()

    private fun requestsScreen(tokens: List<CommandToken>): Boolean {
        val words = tokens.map { it.value }
        val head = words.firstOrNull() ?: return false
        if (head in SCREEN_NOUNS) return true
        if (head !in SCREEN_ACTIONS) return false
        // "take a screenshot" and "take screenshot" both count; the article is
        // the only filler allowed between the verb and the source.
        val source = words.getOrNull(if (words.getOrNull(1) == "a") 2 else 1) ?: return false
        return source == "screenshot" || source == "screen"
    }
}
