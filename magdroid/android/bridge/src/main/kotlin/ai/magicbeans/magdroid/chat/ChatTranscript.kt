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
    settleEscalation(messages, incoming)?.let { return it }
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
        uiThreadId = incomingTask.uiThreadId ?: previousTask?.uiThreadId,
        storeStatus = incomingTask.storeStatus ?: previousTask?.storeStatus,
        // One card for the task's whole life: its clock starts at the first
        // update the chat saw, never at the latest one.
        startedAtMs = listOfNotNull(incomingTask.startedAtMs, previousTask?.startedAtMs).minOrNull(),
        updatedAtMs = listOfNotNull(incomingTask.updatedAtMs, previousTask?.updatedAtMs).maxOrNull(),
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

/**
 * An `escalation_resolved` row settles the card it answers.
 *
 * The open card flips to resolved in place (its buttons spent), and the
 * resolution row itself is not drawn — appending it drew a second "answered"
 * card under a first one that still offered buttons. Null when no card
 * matches, so the row is appended as it stands (resolved card or system note).
 */
private fun settleEscalation(messages: List<ChatMessage>, incoming: ChatMessage): List<ChatMessage>? {
    val ref = incoming.resolvesEscalation ?: return null
    val index = messages.indexOfLast { message ->
        message.kind == MessageKind.Escalation && message.escalation?.let(ref::matches) == true
    }
    if (index < 0) return null
    return messages.toMutableList().also {
        val card = it[index].escalation ?: return null
        it[index] = it[index].copy(
            escalation = card.copy(resolved = true, answering = null, error = null),
        )
    }
}

/** Collapse an append-only history into the cards the transcript should draw. */
internal fun coalesceTaskStatusMessages(messages: List<ChatMessage>): List<ChatMessage> =
    placeTaskCardsAfterTurnReply(messages.fold(emptyList(), ::appendTranscriptMessage))

private fun ChatMessage.isTurnReply(): Boolean =
    !fromUser && kind == MessageKind.Text && !chatTurnId.isNullOrBlank()

/**
 * A task card is stamped mid-turn, its turn's assistant reply at turn end, so
 * server order puts the card above the reply that started it. Move a card
 * whose `chatTurnId` names an assistant reply below it to directly after that
 * turn's last reply. Cards without a turn id, or already below their reply,
 * keep their position; a live streaming placeholder counts as the reply.
 */
internal fun placeTaskCardsAfterTurnReply(messages: List<ChatMessage>): List<ChatMessage> {
    val lastReply = HashMap<String, Int>()
    messages.forEachIndexed { index, message ->
        if (message.isTurnReply()) lastReply[message.chatTurnId!!.trim()] = index
    }
    if (lastReply.isEmpty()) return messages
    val deferred = HashMap<String, MutableList<ChatMessage>>()
    val ordered = ArrayList<ChatMessage>(messages.size)
    messages.forEachIndexed { index, message ->
        val turn = message.chatTurnId?.trim()?.takeIf { it.isNotEmpty() }
        if (message.kind == MessageKind.TaskStatus && turn != null && (lastReply[turn] ?: -1) > index) {
            deferred.getOrPut(turn) { mutableListOf() } += message
            return@forEachIndexed
        }
        ordered += message
        if (turn != null && message.isTurnReply() && lastReply[turn] == index) {
            deferred.remove(turn)?.let { ordered += it }
        }
    }
    return ordered
}

/** Realtime and SSE describe the same persisted messages with different timing. */
internal fun mergeRealtimeMessage(messages: List<ChatMessage>, incoming: ChatMessage): List<ChatMessage> {
    if (messages.any { it.id == incoming.id }) return messages
    if (incoming.kind == MessageKind.TaskStatus || incoming.resolvesEscalation != null) {
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
                // Any local bubble of the turn: a clean stream drop leaves it
                // neither streaming nor failed, and the reply still replaces it.
                !it.fromUser && it.kind == MessageKind.Text &&
                    (it.streaming || it.failed || it.optimistic) && it.chatTurnId == incoming.chatTurnId
            }
        }
        else -> -1
    }
    return if (local < 0) {
        // A reply landing after its own turn's task card (no local preview
        // held its slot, e.g. a resync) goes above that card.
        if (incoming.isTurnReply()) placeTaskCardsAfterTurnReply(messages + incoming) else messages + incoming
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

/** Task id → lowercased status, for each task whose latest transcript card is terminal. */
internal fun terminalTaskStatuses(messages: List<ChatMessage>): Map<String, String> {
    val latest = LinkedHashMap<String, TaskStatusCard>()
    messages.forEach { message ->
        val task = message.task ?: return@forEach
        if (message.kind == MessageKind.TaskStatus) latest[task.taskId] = task
    }
    return latest.filterValues { it.terminal && !it.synthesisPending }
        .mapValues { it.value.status.lowercase() }
}

/** Settle every response's step strip against the transcript's terminal task cards. */
internal fun settleTranscriptActivity(messages: List<ChatMessage>): List<ChatMessage> {
    val terminal = terminalTaskStatuses(messages)
    if (terminal.isEmpty()) return messages
    var changed = false
    val next = messages.map { message ->
        if (message.activityRows.isEmpty()) return@map message
        val rows = settleActivityRows(message.activityRows, terminal)
        if (rows === message.activityRows) message else message.copy(activityRows = rows).also { changed = true }
    }
    return if (changed) next else messages
}
