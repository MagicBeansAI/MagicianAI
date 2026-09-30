package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.access.MagicanAppLinks
import android.net.Uri
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Where a launcher shortcut lands.
 *
 * The same four entry points iOS exposes to Siri. Android has no Siri, but it
 * has the long press on the app icon, which is the platform's own answer to the
 * same question — start somewhere specific without going through the app's
 * front door first.
 */
sealed interface AppShortcutTarget {
    /** A fresh session, not the one that was open. */
    data object NewChat : AppShortcutTarget

    /** The blackboard, ready for a concept. */
    data object AskTutor : AppShortcutTarget

    /** Begin observing the room. */
    data object StartListening : AppShortcutTarget

    /** Open chat and begin the configured conversational voice mode. */
    data object StartTalking : AppShortcutTarget

    /** Open the consolidated glance destination that needs the owner. */
    data object OpenAttention : AppShortcutTarget

    /** Open the full Today surface from an idle widget fallback. */
    data object OpenToday : AppShortcutTarget

    /** Open Observe, optionally on one Command Deck view (`?pane=now|sources|audio|notes`). */
    data class OpenObserve(val pane: ObservePane? = null) : AppShortcutTarget
}

/**
 * One durable in-process handoff from a launcher shortcut into the shell.
 *
 * Modelled on [TaskDeepLinks], including the consume step: a target that stayed
 * set would re-fire every time the shell recomposed, which for "new chat" means
 * silently discarding the session the owner had just started typing in.
 */
object AppShortcutLinks {

    private val _target = MutableStateFlow<AppShortcutTarget?>(null)
    val target: StateFlow<AppShortcutTarget?> = _target.asStateFlow()

    /** Returns whether the URI was one of ours, so the caller can stop looking. */
    fun accept(uri: Uri?): Boolean {
        val parsed = parse(uri?.toString()) ?: return false
        _target.value = parsed
        return true
    }

    /** In-app producers use the same one-shot route as the launcher does. */
    fun request(target: AppShortcutTarget) {
        _target.value = target
    }

    fun consume(target: AppShortcutTarget) {
        if (_target.value == target) _target.value = null
    }

    /**
     * `magican://new-chat`, `magican://tutor`, and `magican://listen`.
     *
     * Hosts rather than paths, matching the `magican://task/…` links already
     * recognised, and carrying no id because none of these open a *thing* —
     * they open a place.
     */
    internal fun parse(raw: String?): AppShortcutTarget? {
        val uri = raw?.let { runCatching { java.net.URI(it) }.getOrNull() } ?: return null
        if (!MagicanAppLinks.isScheme(uri.scheme)) return null
        return when (uri.host) {
            "new-chat" -> AppShortcutTarget.NewChat
            "tutor" -> AppShortcutTarget.AskTutor
            "listen" -> AppShortcutTarget.StartListening
            "talk" -> AppShortcutTarget.StartTalking
            "attention" -> AppShortcutTarget.OpenAttention
            "today" -> AppShortcutTarget.OpenToday
            "observe" -> AppShortcutTarget.OpenObserve(ObservePane.fromDeepLink(raw))
            else -> null
        }
    }
}
