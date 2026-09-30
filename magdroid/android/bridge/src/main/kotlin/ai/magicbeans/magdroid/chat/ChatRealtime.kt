package ai.magicbeans.magdroid.chat

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject

/**
 * Chat events that arrive outside a send.
 *
 * The SSE stream a send opens carries that turn and closes with it, which is
 * everything Android could see: a message typed on the web, a turn finished by
 * a background execution, and every activity row produced while no send was in
 * flight were all invisible here. iOS has read these off `/realtime/ws` since
 * it had a chat screen.
 *
 * Only the events something on this client can draw are modelled — which is
 * why `ShellOutputChunk` joined late: it was deliberately absent until the
 * task card grew a terminal block to render it into.
 */
sealed class ChatRealtimeEvent {

    /** A message that appeared without this device asking for it. */
    data class MessageReceived(
        val sessionId: String?,
        val message: ChatMessageDto,
    ) : ChatRealtimeEvent()

    /** A turn finished — possibly one this device never started. */
    data class TurnCompleted(
        val sessionId: String?,
        val chatTurnId: String?,
    ) : ChatRealtimeEvent()

    /** Planning began for a task, which the transcript shows as a status. */
    data class PlanningStarted(
        val taskId: String?,
        val title: String?,
    ) : ChatRealtimeEvent()

    /**
     * A batch of shell output from a running step.
     *
     * Parsed from the wire's actual shape — a `data` string of newline-joined
     * output — not the `lines` array iOS's chat decoder still expects and
     * never receives. The split mirrors the backend's own
     * `parse_shell_lines`: interior blank lines are real output, the empty
     * artifact after a trailing newline is not.
     */
    data class ShellOutput(
        val executionId: String,
        val lines: List<String>,
        val isFinal: Boolean,
    ) : ChatRealtimeEvent()
}

private fun JsonObject.str(key: String): String? =
    (this[key] as? JsonPrimitive)?.contentOrNull?.takeIf { it.isNotBlank() }

/**
 * Read one socket frame, or `null` if it is not ours.
 *
 * Pure, and separate from the socket, for the same reason the task parser is:
 * the scope filtering and the shape-matching are where the mistakes live, and
 * neither needs a network to exercise.
 *
 * The bus is shared by every surface, so most frames belong to someone else.
 * Returning null for them is the normal case, not an error worth logging.
 */
internal fun parseChatRealtimeEvent(
    raw: String,
    principal: String,
    workspace: String,
): ChatRealtimeEvent? {
    val envelope = runCatching { chatJson.parseToJsonElement(raw).jsonObject }.getOrNull()
        ?: return null
    val type = envelope.str("event_type") ?: return null
    val data = envelope["data"] as? JsonObject ?: JsonObject(emptyMap())

    // Scope first, and only when the event states it. An event that names
    // another principal is not ours to act on; one that names nobody predates
    // the field and is allowed through rather than silently dropped.
    data.str("principal")?.let { if (it != principal) return null }
    data.str("workspace")?.let { if (it != workspace) return null }

    return when (type) {
        "ChatMessageReceived" -> {
            val message = data["message"] as? JsonObject ?: return null
            val dto = runCatching {
                chatJson.decodeFromJsonElement(ChatMessageDto.serializer(), message)
            }.getOrNull() ?: return null
            ChatRealtimeEvent.MessageReceived(
                sessionId = data.str("session_id") ?: dto.sessionId,
                message = dto,
            )
        }

        "MessageCompleted" -> ChatRealtimeEvent.TurnCompleted(
            sessionId = data.str("session_id"),
            chatTurnId = data.str("chat_turn_id") ?: data.str("turn_id"),
        )

        // ExecutionPanelDelta deliberately not handled: an earlier arm read
        // `rows`/`activity_rows` off it — fields that frame has never carried
        // — so it could not produce an event. Activity rows come from the
        // canonical `GET .../turns/{id}/events` projection instead, fetched
        // when a turn settles.
        "V3PlanningStarted" -> ChatRealtimeEvent.PlanningStarted(
            taskId = data.str("task_id"),
            title = data.str("task_title") ?: data.str("title"),
        )

        "ShellOutputChunk" -> {
            val executionId = data.str("execution_id") ?: return null
            val isFinal = (data["is_final"] as? JsonPrimitive)?.contentOrNull == "true"
            val lines = shellChunkLines(
                (data["data"] as? JsonPrimitive)?.contentOrNull.orEmpty(),
            )
            // A chunk with nothing to draw is not an event — except the final
            // one, which still says the process ended.
            if (lines.isEmpty() && !isFinal) null
            else ChatRealtimeEvent.ShellOutput(executionId, lines, isFinal)
        }

        else -> null
    }
}

/**
 * The backend's `parse_shell_lines`, mirrored: split on newline, keep interior
 * blanks (they are real output), drop only the empty artifact a trailing
 * newline leaves at the end.
 */
internal fun shellChunkLines(data: String): List<String> {
    if (data.isEmpty()) return emptyList()
    val raw = data.split('\n')
    return raw.filterIndexed { index, value -> value.isNotEmpty() || index != raw.lastIndex }
}
