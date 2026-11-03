package ai.magicbeans.magdroid.ui

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.util.concurrent.atomic.AtomicLong

/** Where a chat link (`/chat?session=…`, `/t/<thread>[/chat]…`, `/history`, `/settings`) lands. */
sealed interface ChatDeepLinkTarget {
    /** [request] distinguishes two taps on the same link. */
    val request: Long

    /** That conversation, or (no [sessionId]) the thread's latest, or (neither) the chat tab. */
    data class Chat(val sessionId: String?, val threadId: String?, override val request: Long) : ChatDeepLinkTarget
    data class History(override val request: Long) : ChatDeepLinkTarget
    data class Settings(override val request: Long) : ChatDeepLinkTarget
    data class Notes(override val request: Long) : ChatDeepLinkTarget
}

/**
 * One in-process handoff from a chat link to the shell, which owns the chat
 * and History view models, the settings sheet and Notes. Modelled on
 * [TaskDeepLinks]: the shell consumes a target once it has acted on it.
 */
object ChatDeepLinks {
    private val requests = AtomicLong(0)
    private val _target = MutableStateFlow<ChatDeepLinkTarget?>(null)
    val target: StateFlow<ChatDeepLinkTarget?> = _target.asStateFlow()

    fun openChat(sessionId: String?, threadId: String?) {
        _target.value = ChatDeepLinkTarget.Chat(sessionId, threadId, requests.incrementAndGet())
    }

    fun openHistory() { _target.value = ChatDeepLinkTarget.History(requests.incrementAndGet()) }
    fun openSettings() { _target.value = ChatDeepLinkTarget.Settings(requests.incrementAndGet()) }
    fun openNotes() { _target.value = ChatDeepLinkTarget.Notes(requests.incrementAndGet()) }

    fun consume(target: ChatDeepLinkTarget) { _target.compareAndSet(target, null) }
}
