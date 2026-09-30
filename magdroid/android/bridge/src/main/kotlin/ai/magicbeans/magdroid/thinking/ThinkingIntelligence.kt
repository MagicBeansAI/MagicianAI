package ai.magicbeans.magdroid.thinking

import java.io.IOException

/**
 * What the facilitator is doing, and what to say when it cannot.
 *
 * The stages mirror `InterpretStage` on the server, which binds each one to a
 * real step of `interpret()` — the server narrates them as
 * `ThinkingMapInterpretProgress` events and this client displays them. That
 * binding is the whole discipline: iOS declared seven states, five of which
 * were assigned nowhere because nothing emitted stages on any surface, and a
 * label wired to nothing outlives everyone's memory of why. `openingThread`
 * and `grounding` are deliberately absent here — no server step backs them.
 *
 * [wire] is the server's spelling. An event whose stage matches nothing here
 * is a stage this build has never heard of: the client keeps its current line
 * rather than guessing, which is what lets the vocabulary grow server-first.
 */
enum class ThinkingProgress(val wire: String, val label: String, private val detailText: String) {
    Idle("idle", "Ready", "Ready for another direction."),
    Preparing("preparing", "Reading map", "Reading the board and the active branch…"),
    LoadingContext(
        "loading_context",
        "Loading context",
        "Giving the facilitator the graph slice and the move budget…",
    ),
    Facilitating(
        "facilitating",
        "Exploring",
        "Finding the few directions that would change the next minute of thinking…",
    ),
    Parsing("parsing", "Reading back", "Reading the facilitator's answer back…"),
    Shaping("shaping", "Shaping moves", "Removing repeats and shaping the strongest cards…"),
    ;

    /**
     * The sentence under the label. `Preparing` says how many thoughts the
     * facilitator is actually reading when the event carried the count — the
     * number is the part that makes a several-second wait feel like motion.
     */
    fun detail(nodeCount: Int? = null): String = when {
        this == Preparing && nodeCount != null && nodeCount > 0 ->
            "Reading $nodeCount ${if (nodeCount == 1) "thought" else "thoughts"} and the active branch…"
        else -> detailText
    }

    companion object {
        /** The server's spelling → a stage, or null for one this build predates. */
        fun fromWire(stage: String): ThinkingProgress? = entries.firstOrNull { it.wire == stage }
    }
}

/**
 * Whether a narrated stage belongs on this screen's strip.
 *
 * Pure so the rule is testable without a socket. Four gates, each refusing a
 * real hazard:
 * - **another map's run** — the bus is shared; two maps thinking at once must
 *   not drive each other's strip;
 * - **a run this screen did not start** — ambient auto-maps and other devices
 *   narrate against the same map id; only the utterance id minted for *this*
 *   request is ours;
 * - **a stage this build has never heard of** — parsed as null; the honest
 *   response is to keep the current line, not to guess;
 * - **the terminal idle** — settling is the HTTP response's job (it also owns
 *   failure classification), and a realtime idle can outrun the response
 *   body. Clearing the strip early would show "Ready" over a run still being
 *   applied.
 */
fun interpretProgressApplies(
    event: ThinkingInterpretProgress,
    openMapId: String?,
    inFlightUtteranceId: String?,
): Boolean =
    event.stage != null &&
        event.stage != ThinkingProgress.Idle &&
        openMapId != null && event.mapId == openMapId &&
        inFlightUtteranceId != null && event.utteranceId == inFlightUtteranceId

/**
 * Why the facilitator produced nothing.
 *
 * Retryable is the distinction that earns its keep: a timeout is worth trying
 * again and a rejected request is not, and an owner who cannot tell will either
 * hammer it or give up on something that would have worked.
 */
data class ThinkingFallback(
    val message: String,
    val canRetry: Boolean,
) {
    /** Shown where iOS shows "AI UNAVAILABLE". */
    val statusLabel: String get() = "FACILITATOR UNAVAILABLE"

    val provenance: String
        get() = "Fallback — facilitator intelligence was unavailable"
}

/**
 * Turn a failure into something worth reading.
 *
 * Mirrors iOS's `classify`, which maps transport failures onto sentences a
 * person can act on rather than surfacing an exception. The wording is kept
 * close deliberately: two clients describing the same outage differently makes
 * it look like two different outages.
 */
fun classifyThinkingFailure(error: Throwable): ThinkingFallback = when {
    error is ThinkingMapConflict -> ThinkingFallback(
        // Not a facilitator problem at all — somebody else moved the map.
        message = error.message ?: "This map changed while you were editing it.",
        canRetry = true,
    )

    error is java.net.SocketTimeoutException -> ThinkingFallback(
        "The facilitator took too long to read this map. Try again.",
        canRetry = true,
    )

    error is java.net.UnknownHostException || error is java.net.ConnectException -> ThinkingFallback(
        "The backend could not be reached. Check the connection and try again.",
        canRetry = true,
    )

    error is kotlinx.coroutines.CancellationException -> ThinkingFallback(
        "The request was cancelled before the facilitator finished reading the map.",
        canRetry = true,
    )

    // Any other IO problem is the network being the network: worth one more go.
    error is IOException -> ThinkingFallback(
        error.message ?: "The backend could not be reached. Check the connection and try again.",
        canRetry = true,
    )

    else -> ThinkingFallback(
        error.message ?: "The facilitator could not read this map.",
        // Not retryable: something rejected the request rather than failing to
        // deliver it, and sending it again would be rejected the same way.
        canRetry = false,
    )
}
