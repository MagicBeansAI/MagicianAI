package ai.magicbeans.magdroid.chat

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/**
 * State the shell owns rather than any one surface.
 *
 * Only the Attention badge so far. It belongs here and not in the chat model
 * because it has to stay current while Attention is not the visible tab — the
 * count is the one part of that surface you can see from everywhere else, and
 * a badge that only updates once you are already looking at it is pointless.
 */
class ShellViewModel(app: Application) : AndroidViewModel(app) {

    private val repository = ChatRepository(app)
    private val _badge = MutableStateFlow(0)
    val attentionBadge: StateFlow<Int> = _badge.asStateFlow()

    /**
     * Whether Magician is reachable, for the dot beside the session title.
     *
     * Held here rather than per-screen because two surfaces ask the same
     * question — the chat title and Settings — and two pollers would disagree
     * with each other about the answer for up to a refresh.
     *
     * Null means not yet checked, which the dot draws differently from "offline":
     * a phone that has just opened has not learned anything bad yet.
     */
    private val _magicianReachable = MutableStateFlow<Boolean?>(null)
    val magicianReachable: StateFlow<Boolean?> = _magicianReachable.asStateFlow()

    init {
        refreshBadge()
    }

    /**
     * Re-read what is waiting.
     *
     * Called at launch and on every tab change, which is what iOS does: cheap,
     * and it means the count is right at the moment someone might act on it
     * without holding a subscription open for a surface nobody is watching.
     */
    fun refreshBadge() {
        viewModelScope.launch {
            // Same cadence as the badge, and for the same reason: cheap, and
            // right at the moment somebody might act on it.
            _magicianReachable.value =
                runCatching { repository.healthStack().magician.reachable }.getOrDefault(false)
            val counts = repository.attentionCounts()
            _badge.value = resolveAttentionBadgeCount(
                // The live half of the number. Passing zero here left the
                // badge waiting on the next feed fetch to notice a request
                // that had already arrived.
                pendingHitl = PendingHitlTracker.count.value,
                needsAction = counts.needsAction,
                failed = counts.failed,
            )
        }
    }
}
