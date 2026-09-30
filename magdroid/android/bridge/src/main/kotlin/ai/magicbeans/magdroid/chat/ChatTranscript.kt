package ai.magicbeans.magdroid.chat

/**
 * Append one projected message to the visible transcript.
 *
 * Task progress is persisted as an append-only lifecycle (running, ready,
 * running, completed). Those records are one card changing state, not four
 * separate messages. Keep only the newest record for a task and carry forward
 * transient details that a later persisted record may omit.
 */
private fun appendTranscriptMessage(
    messages: List<ChatMessage>,
    incoming: ChatMessage,
): List<ChatMessage> {
    val incomingTask = incoming.task
    val taskId = incomingTask?.taskId?.trim()?.takeIf {
        it.isNotEmpty() && it != "unknown-task"
    } ?: return messages + incoming
    val previousIndex = messages.indexOfLast { message ->
        message.kind == MessageKind.TaskStatus && message.task?.taskId?.trim() == taskId
    }
    if (previousIndex < 0) return messages + incoming

    val previous = messages[previousIndex]
    // Keep a specifically linked historical answer visible even when a newer task row exists.
    if (previous.linkedAnswerTarget || incoming.linkedAnswerTarget) return messages + incoming
    val previousTask = previous.task
    val mergedTask = incomingTask.copy(
        title = incomingTask.title.takeUnless { it.isBlank() || it == taskId }
            ?: previousTask?.title?.takeUnless { it.isBlank() }
            ?: incomingTask.title,
        executionId = incomingTask.executionId ?: previousTask?.executionId,
        outputs = incomingTask.outputs.ifEmpty { previousTask?.outputs.orEmpty() },
        terminalLines = incomingTask.terminalLines.ifEmpty {
            previousTask?.terminalLines.orEmpty()
        },
    )
    val merged = incoming.copy(
        task = mergedTask,
        chatTurnId = incoming.chatTurnId?.takeIf { it.isNotBlank() } ?: previous.chatTurnId,
        activity = incoming.activity.ifEmpty { previous.activity },
        activityRows = incoming.activityRows.ifEmpty { previous.activityRows },
    )
    return messages.toMutableList().also {
        it.removeAt(previousIndex)
        it += merged
    }
}

/** Collapse an append-only history into the cards the transcript should draw. */
internal fun coalesceTaskStatusMessages(messages: List<ChatMessage>): List<ChatMessage> =
    messages.fold(emptyList(), ::appendTranscriptMessage)

/** Realtime and SSE describe the same persisted messages with different timing. */
internal fun mergeRealtimeMessage(messages: List<ChatMessage>, incoming: ChatMessage): List<ChatMessage> {
    if (messages.any { it.id == incoming.id }) return messages
    if (incoming.kind == MessageKind.TaskStatus) {
        return appendTranscriptMessage(messages, incoming)
    }
    val local = when {
        incoming.fromUser && incoming.kind == MessageKind.Text && incoming.chatTurnId != null -> {
            messages.indexOfFirst {
                it.optimistic && it.fromUser && it.kind == MessageKind.Text &&
                    it.chatTurnId == incoming.chatTurnId
            }
        }
        !incoming.fromUser && incoming.kind == MessageKind.Text && incoming.chatTurnId != null -> {
            messages.indexOfFirst {
                !it.fromUser && it.kind == MessageKind.Text &&
                    (it.streaming || it.failed) && it.chatTurnId == incoming.chatTurnId
            }
        }
        else -> -1
    }
    return if (local < 0) {
        messages + incoming
    } else {
        messages.toMutableList().also {
            val preview = it[local]
            it[local] = incoming.copy(
                activity = preview.activity.ifEmpty { incoming.activity },
                activityRows = preview.activityRows.ifEmpty { incoming.activityRows },
            )
        }
    }
}

internal fun settleChatMessages(
    messages: List<ChatMessage>, replyId: String, event: ChatStreamEvent.Done,
): List<ChatMessage> {
    val streamed = messages.firstOrNull { it.id == replyId } ?: return messages
    val settled = event.messages.filter { !it.fromUser }.distinctBy { it.id }
    val reconciled = event.messages.filter { it.fromUser }.fold(messages, ::mergeRealtimeMessage)
    if (event.cancelled || settled.isEmpty()) {
        return coalesceTaskStatusMessages(
            reconciled.map { if (it.id == replyId) it.copy(streaming = false) else it },
        )
    }
    val settledIds = settled.mapTo(mutableSetOf()) { it.id }
    return coalesceTaskStatusMessages(reconciled.flatMap { message ->
        when {
            message.id == replyId -> settled.map { row ->
                row.copy(
                    text = if (row.kind == MessageKind.Text && row.text.isBlank()) streamed.text else row.text,
                    activity = streamed.activity.ifEmpty { row.activity },
                    activityRows = streamed.activityRows.ifEmpty { row.activityRows },
                    chatTurnId = row.chatTurnId ?: streamed.chatTurnId,
                )
            }
            // A socket delivery can beat the SSE done frame. Replace its copy
            // at the placeholder's position instead of rendering the id twice.
            message.id in settledIds -> emptyList()
            else -> listOf(message)
        }
    })
}
