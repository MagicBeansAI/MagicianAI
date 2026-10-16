package ai.magicbeans.magdroid.observe

/**
 * What the observation says about itself while it runs — the notification's
 * second line and the mini-bar's label, as pure functions so the words are
 * testable without a service.
 *
 * The screen share changes what is true, so it must change what is said: a
 * notification that reads "recording this room" while the screen is also
 * leaving the device is a disclosure that has fallen behind the facts.
 */

/** The live notification's detail line. */
fun observeNotificationDetail(
    state: ObserveState,
    chunksSent: Long,
    sharingScreen: Boolean,
): String {
    if (state == ObserveState.Starting) return "Starting…"
    if (chunksSent <= 0) {
        return if (sharingScreen) {
            "Audio and screen are being sent to Magician."
        } else {
            "Audio is being sent to Magician."
        }
    }
    val counted = "$chunksSent ${if (chunksSent == 1L) "chunk" else "chunks"} sent"
    return if (sharingScreen) "$counted · sharing screen." else "$counted."
}

/** The mini-bar's label, shown on every tab while a session runs. */
fun observeLiveLabel(state: ObserveState, sharingScreen: Boolean): String = when {
    state == ObserveState.Starting -> "Starting…"
    sharingScreen -> "Recording · sharing screen"
    else -> "Recording this room"
}
