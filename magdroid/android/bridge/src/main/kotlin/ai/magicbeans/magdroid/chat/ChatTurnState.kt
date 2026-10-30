package ai.magicbeans.magdroid.chat

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.doubleOrNull

/**
 * The server's word on a chat turn (`chat::turn_state` in magician).
 *
 * A turn sent with `continue_on_disconnect` keeps running when the phone's
 * stream drops or the app is killed. The SSE stream cannot say what happened
 * after it closed, so the session detail, `/chat/active` and `/queue` carry
 * the session's latest turn as `turn_state`, and every change goes out as the
 * `chat.turn.state_changed` agent event. A reopened chat reads it to show a
 * running turn live, a stopped turn as stopped and a failed one as failed,
 * instead of guessing from the transcript.
 */
enum class ChatTurnStatus(val wire: String) {
    Running("running"),
    Completed("completed"),
    Stopped("stopped"),
    Failed("failed"),
    ;

    companion object {
        fun fromWire(value: String?): ChatTurnStatus? = entries.firstOrNull { it.wire == value?.trim() }
    }
}

data class ChatTurnState(
    val chatTurnId: String,
    val status: ChatTurnStatus,
    /** Epoch ms. */
    val startedAtMs: Long = 0,
    /** Epoch ms; set once the turn is no longer running. */
    val endedAtMs: Long? = null,
    /**
     * Why it stopped or failed: [USER_STOP_REASON], a watchdog reason
     * (`turn_stalled`, `turn_wall_clock_exceeded`), `error` or
     * [INTERRUPTED_REASON]. Null for running and completed turns.
     */
    val reason: String? = null,
    /** The session it belongs to, when the payload named it. */
    val sessionId: String? = null,
) {
    val running: Boolean get() = status == ChatTurnStatus.Running

    /** The owner pressed Stop. A stop without a reason is read the same way, as the server does. */
    val stoppedByUser: Boolean
        get() = status == ChatTurnStatus.Stopped && (reason ?: USER_STOP_REASON) == USER_STOP_REASON

    /** The liveness watchdog ended it; its own notice is persisted in the transcript. */
    val stoppedByWatchdog: Boolean get() = status == ChatTurnStatus.Stopped && !stoppedByUser

    companion object {
        const val EVENT = "chat.turn.state_changed"
        const val USER_STOP_REASON = "user_stop"
        const val INTERRUPTED_REASON = "interrupted"
    }
}

/**
 * What a snapshot said about the latest turn.
 *
 * [supported] separates "no turn known" (`turn_state: null`: no turn yet, or
 * the server restarted and lost the running one) from a server too old to
 * send the field at all. Only the first may conclude anything from absence.
 */
data class ChatTurnStateReport(val supported: Boolean, val state: ChatTurnState?) {
    companion object {
        val UNSUPPORTED = ChatTurnStateReport(supported = false, state = null)

        /** Read `turn_state` off a session detail or queue response body. */
        fun from(body: JsonObject, sessionId: String?): ChatTurnStateReport {
            if (!body.containsKey("turn_state")) return UNSUPPORTED
            return ChatTurnStateReport(supported = true, state = parseChatTurnState(body["turn_state"], sessionId))
        }
    }
}

/** `GET /chat/sessions/{id}`: the session, its transcript and its latest turn. */
data class ChatConversation(
    val summary: SessionSummary,
    val messages: List<ChatMessage>,
    val turnState: ChatTurnStateReport = ChatTurnStateReport.UNSUPPORTED,
    /** Older messages exist before [oldestMessageId]. */
    val hasMore: Boolean = false,
    /** The oldest raw wire message read, the `before` cursor for the next older page. */
    val oldestMessageId: String? = null,
)

/** Parse one wire `turn_state`; [sessionId] fills a payload that does not name its session. */
internal fun parseChatTurnState(raw: JsonElement?, sessionId: String? = null): ChatTurnState? {
    val value = raw as? JsonObject ?: return null
    fun text(key: String): String? =
        (value[key] as? JsonPrimitive)?.takeIf { it !is JsonNull && it.isString }
            ?.contentOrNull?.trim()?.takeIf { it.isNotEmpty() }
    fun millis(key: String): Long? {
        val primitive = value[key] as? JsonPrimitive ?: return null
        if (primitive is JsonNull) return null
        val number = if (primitive.isString) primitive.content.trim().toDoubleOrNull() else primitive.doubleOrNull
        return number?.takeIf { it.isFinite() }?.toLong()
    }
    val turnId = text("chat_turn_id") ?: return null
    val status = ChatTurnStatus.fromWire(text("status")) ?: return null
    return ChatTurnState(
        chatTurnId = turnId,
        status = status,
        startedAtMs = millis("started_at") ?: 0,
        endedAtMs = millis("ended_at"),
        reason = text("reason"),
        sessionId = text("chat_session_id") ?: sessionId?.trim()?.takeIf { it.isNotEmpty() },
    )
}

/**
 * Realtime voice turns render through the call surface, not a chat bubble
 * (the web's `liveTurn.ts` skips their rows for the same reason).
 */
internal fun isRealtimeVoiceTurn(chatTurnId: String): Boolean =
    chatTurnId.startsWith("voice-turn-") || chatTurnId.startsWith("voice-request-")

/** Id of the local status line a settled turn without a reply shows. */
internal fun turnNoticeId(chatTurnId: String): String = "$TURN_NOTICE_PREFIX$chatTurnId"

internal fun ChatMessage.isTurnNotice(): Boolean = id.startsWith(TURN_NOTICE_PREFIX)

private const val TURN_NOTICE_PREFIX = "turn-notice:"
internal const val TURN_STOPPED_NOTICE = "Stopped"
internal const val TURN_INTERRUPTED_NOTICE = "Interrupted before it finished"
internal const val TURN_FAILED_NOTICE = "This turn failed before it could answer"

/**
 * The status line for a settled turn that has no reply of its own, or null.
 *
 * A completed turn has its reply and a watchdog stop has its persisted
 * notice, so neither gets a line here.
 */
internal fun turnStateNotice(state: ChatTurnState): String? = when {
    state.stoppedByUser -> TURN_STOPPED_NOTICE
    state.status == ChatTurnStatus.Failed ->
        if (state.reason == ChatTurnState.INTERRUPTED_REASON) TURN_INTERRUPTED_NOTICE else TURN_FAILED_NOTICE
    else -> null
}

/** A local assistant bubble for [chatTurnId] still waiting for its persisted reply. */
private fun ChatMessage.isPreviewOf(chatTurnId: String): Boolean =
    !fromUser && kind == MessageKind.Text && optimistic && this.chatTurnId == chatTurnId

/** A persisted assistant answer (or the watchdog's notice) for [chatTurnId]. */
internal fun hasServerReply(messages: List<ChatMessage>, chatTurnId: String): Boolean =
    messages.any {
        !it.fromUser && it.kind == MessageKind.Text && !it.optimistic && !it.isTurnNotice() &&
            it.chatTurnId == chatTurnId
    }

/** Insert [message] after the last row of [chatTurnId] (its user message or reply), else at the end. */
private fun insertAfterTurn(messages: List<ChatMessage>, chatTurnId: String, message: ChatMessage): List<ChatMessage> {
    val anchor = messages.indexOfLast { it.chatTurnId == chatTurnId }
    return if (anchor < 0) messages + message
    else messages.toMutableList().also { it.add(anchor + 1, message) }
}

private fun noticeMessage(chatTurnId: String, text: String, stopped: Boolean, from: ChatMessage?, atMs: Long?) =
    ChatMessage(
        id = turnNoticeId(chatTurnId),
        fromUser = false,
        text = text,
        // "Stopped" is the owner's own action, said quietly; a failure is a
        // line in the conversation's failure colour.
        system = stopped,
        failed = !stopped,
        chatTurnId = chatTurnId,
        activityRows = from?.activityRows.orEmpty(),
        activity = from?.activity.orEmpty(),
        createdAtMs = from?.createdAtMs ?: atMs,
    )

/**
 * Show [state] in the transcript.
 *
 * - running: the turn has a live bubble (streaming, so its steps read as live
 *   and Stop is offered), mounted after its user message when none exists. A
 *   failed local bubble of a turn the server still runs was only a dropped
 *   stream and goes live again.
 * - completed: the reply is the answer; local previews of the turn go once it
 *   is in the transcript.
 * - stopped by the owner, failed, interrupted: the live bubble settles. An
 *   empty one becomes the status line (keeping its steps); one with partial
 *   text keeps it and the line follows. A failure the stream already worded
 *   keeps that wording.
 * - stopped by the watchdog: its persisted notice is the answer.
 *
 * Idempotent: replaying a state, or the snapshot after the event, changes nothing.
 */
internal fun applyChatTurnState(messages: List<ChatMessage>, state: ChatTurnState): List<ChatMessage> {
    val turn = state.chatTurnId
    val noticeId = turnNoticeId(turn)
    if (state.running) {
        if (hasServerReply(messages, turn)) return messages
        val withoutNotice = messages.filterNot { it.id == noticeId }
        val previews = withoutNotice.filter { it.isPreviewOf(turn) }
        if (previews.isNotEmpty()) {
            return withoutNotice.map { message ->
                if (!message.isPreviewOf(turn) || (message.streaming && !message.failed)) message
                else message.copy(
                    streaming = true,
                    failed = false,
                    // A failed bubble's text is the transport's error, which is
                    // no longer true; partial tokens of a dropped stream stay.
                    text = if (message.failed) "" else message.text,
                )
            }
        }
        val live = liveAssistantPlaceholder(replyId = "live-turn:$turn", chatTurnId = turn)
            .copy(createdAtMs = state.startedAtMs.takeIf { it > 0 } ?: System.currentTimeMillis())
        return insertAfterTurn(withoutNotice, turn, live)
    }

    val answered = hasServerReply(messages, turn)
    if (state.status == ChatTurnStatus.Completed || state.stoppedByWatchdog) {
        if (!answered) {
            // The reply (or notice) is on its way or needs a transcript read;
            // keep what the bubble shows, but it is no longer live.
            return messages.map { if (it.isPreviewOf(turn) && it.streaming) it.copy(streaming = false) else it }
        }
        return messages.filterNot { it.isPreviewOf(turn) || it.id == noticeId }
    }

    // Stopped by the owner, failed, or interrupted.
    if (answered) return messages.filterNot { it.isPreviewOf(turn) || it.id == noticeId }
    val notice = turnStateNotice(state) ?: return messages
    val previews = messages.filter { it.isPreviewOf(turn) }
    val worded = state.status == ChatTurnStatus.Failed && previews.any { it.failed && it.text.isNotBlank() }
    val emptyPreview = previews.lastOrNull { it.text.isBlank() && it.structured == null && !it.failed }
    val addLine = !worded && messages.none { it.id == noticeId }
    val line = noticeMessage(
        turn, notice, stopped = state.status == ChatTurnStatus.Stopped,
        from = emptyPreview, atMs = state.endedAtMs,
    )
    var placed = false
    val next = ArrayList<ChatMessage>(messages.size + 1)
    messages.forEach { message ->
        when {
            !message.isPreviewOf(turn) -> next += message
            // The line takes the empty bubble's place, so cards stay below it.
            message === emptyPreview -> if (addLine) { next += line; placed = true }
            message.streaming -> next += message.copy(streaming = false)
            else -> next += message
        }
    }
    return if (addLine && !placed) insertAfterTurn(next, turn, line) else next
}

/**
 * The server knows no turn (`turn_state: null`) — it restarted, so nothing of
 * this session is running. A latest user turn with no answer was interrupted;
 * any bubble still drawn as live is not.
 *
 * Only the newest user turn is judged, and only when nothing at all followed
 * it: an older server stamped no turn id on some replies, and an answer of
 * cards (a task, a question) is still an answer.
 */
internal fun markInterruptedTurn(messages: List<ChatMessage>): List<ChatMessage> {
    val settled = messages.mapNotNull { message ->
        when {
            message.fromUser || !message.optimistic || message.kind != MessageKind.Text -> message
            message.text.isBlank() && message.structured == null && !message.failed -> null
            message.streaming -> message.copy(streaming = false)
            else -> message
        }
    }
    val lastUser = settled.indexOfLast { it.fromUser && it.kind == MessageKind.Text }
    if (lastUser < 0) return settled
    val user = settled[lastUser]
    val turn = user.chatTurnId?.trim()?.takeIf { it.isNotEmpty() } ?: return settled
    if (user.optimistic || isRealtimeVoiceTurn(turn)) return settled
    // Everything after the last user message is the assistant's; a partial
    // local bubble is not an answer, a persisted row of any kind is.
    val answered = settled.subList(lastUser + 1, settled.size).any {
        !(it.optimistic && it.kind == MessageKind.Text) && !it.isTurnNotice()
    }
    if (answered || settled.any { it.id == turnNoticeId(turn) }) return settled
    return settled + noticeMessage(turn, TURN_INTERRUPTED_NOTICE, stopped = false, from = null, atMs = user.createdAtMs)
}
