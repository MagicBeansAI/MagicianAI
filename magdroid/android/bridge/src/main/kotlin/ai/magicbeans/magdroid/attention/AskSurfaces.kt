package ai.magicbeans.magdroid.attention

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

/*
 * Which open asks each surface shows. Attention lists every one; the composer
 * ask dock lists every one too, the conversation on screen's first; a task's
 * deep panel shows the ones raised for that task. All three answer the same
 * item through the same form, so they must agree on what an ask is and on when
 * two rows are the same ask. Pure: no Compose, no networking.
 */

/** The id an answer is posted against. */
val AttentionItem.askKey: String get() = request(metadata).correlationId

/**
 * Every identifier this ask is known by.
 *
 * A transcript card, a feed row and an execution-panel row for one pause can
 * each lead with a different id (correlation, pause state, request, item), so
 * matching on one of them would show the same question twice.
 */
val AttentionItem.askIds: Set<String>
    get() {
        val hitl = metadata?.hitlRequest
        val ids = hitl?.identifiers
        return listOfNotNull(
            askKey,
            id.takeIf { it.isNotBlank() },
            hitl?.id,
            hitl?.correlationId,
            ids?.correlationId,
            ids?.pauseStateId,
            ids?.requestId,
            ids?.approvalId,
            metadata?.pauseStateId,
        ).map { it.trim() }.filter { it.isNotEmpty() }.toSet()
    }

/**
 * The chat session that raised this ask: the row's own stamp, its
 * `input_schema.context`, or its scope. Null for an ask no chat raised.
 */
val AttentionItem.originChatSessionId: String?
    get() {
        val hitl = metadata?.hitlRequest
        val schema = metadata?.inputSchema ?: hitl?.inputSchema
        return listOf(chatSessionId, schema?.contextString("chat_session_id"), hitl?.scope?.chatSessionId)
            .firstNotNullOfOrNull { it?.trim()?.takeIf(String::isNotEmpty) }
    }

/**
 * A notice whose only answer is Dismiss (a service-health notice, an expired
 * notice): nothing to decide, so the composers (chat, VibeDev) leave it to
 * Attention, as on web and iOS.
 */
val AttentionItem.isDismissOnly: Boolean
    get() = request(metadata).options.singleOrNull()?.let { it.id == "dismiss" && it.requiresInput != true } == true

/**
 * The composer dock's feed asks, this conversation's first; [here] names those
 * by [askKey]. An ask from elsewhere is answered only with the dock's controls.
 */
data class ComposerFeedAsks(val items: List<AttentionItem>, val here: Set<String>)

object AskSurfaces {

    /**
     * Everything in the feed still waiting on an answer, oldest first, one row
     * per ask. Failed, running and stalled-wait rows are not asks.
     */
    fun open(feed: AttentionFeedResponse, settled: Set<String> = emptySet()): List<AttentionItem> =
        dedupe(
            (feed.requests + feed.approvals + feed.escalations)
                .filter { it.isActionable && it.askIds.none(settled::contains) }
                .sortedBy { it.updatedAt.takeIf { at -> at > 0 } ?: it.createdAt },
        )

    /**
     * The asks that belong to one conversation: the row names this thread
     * (`ui_thread_id`), or its task is one the conversation started. Relentless
     * and quality-controller asks carry a task and no thread, so the task match
     * is what brings them into the chat that launched the work.
     *
     * A chat's question belongs to that chat only: when the ask names the
     * session that raised it ([originChatSessionId]) and [chatSessionIds]
     * names this one, it is here exactly when they match, whatever thread or
     * task they share. A named thread can hold several chats.
     */
    fun forThread(
        feed: AttentionFeedResponse,
        threadIds: Set<String>,
        taskIds: Set<String>,
        settled: Set<String> = emptySet(),
        chatSessionIds: Set<String> = emptySet(),
    ): List<AttentionItem> = matching(feed, threadIds, taskIds, settled, chatSessionIds, godModeHere = true)

    /**
     * [forThread]'s rule. A God-mode approval is workspace-wide, so the thread
     * view lists it everywhere ([godModeHere]); the composer keeps it with its
     * origin unless this conversation raised it, as on web and iOS.
     */
    private fun matching(
        feed: AttentionFeedResponse,
        threadIds: Set<String>,
        taskIds: Set<String>,
        settled: Set<String>,
        chatSessionIds: Set<String>,
        godModeHere: Boolean,
    ): List<AttentionItem> {
        val threads = threadIds.mapNotNull { it.trim().takeIf(String::isNotEmpty) }.toSet()
        val tasks = taskIds.mapNotNull { it.trim().takeIf(String::isNotEmpty) }.toMutableSet()
        val sessions = chatSessionIds.mapNotNull { it.trim().takeIf(String::isNotEmpty) }.toSet()
        val asks = open(feed, settled)
        // A scoped child ask establishes that task's ownership, bringing its
        // sibling asks along even if they carry only a task ID.
        asks.filter { sessionVerdict(it, sessions) ?: belongsToThread(it, threads) }.mapNotNullTo(tasks) { it.openTaskId }
        return asks.filter { item ->
            // Workspace-wide approval; the displayed origin keeps session grants unambiguous.
            if (godModeHere && item.request(item.metadata).isGodMode) return@filter true
            sessionVerdict(item, sessions) ?: (belongsToThread(item, threads) ||
                item.openTaskId?.let { it in tasks } == true)
        }
    }

    /**
     * The composer dock's asks: every open one, so the reader answers wherever
     * they are, with this conversation's ([forThread]) first. The dock labels
     * the others with where they came from. A plan approval the planner dock
     * already offers here ([planApprovalTaskIds]) is left to it. Another
     * conversation's God-mode approval is listed with its origin, not as
     * this one's ([godModeHere] keeps VibeDev's God approvals first).
     */
    fun forComposer(
        feed: AttentionFeedResponse,
        threadIds: Set<String>,
        taskIds: Set<String>,
        settled: Set<String> = emptySet(),
        planApprovalTaskIds: Set<String> = emptySet(),
        chatSessionIds: Set<String> = emptySet(),
        godModeHere: Boolean = false,
    ): ComposerFeedAsks {
        val belongs = matching(feed, threadIds, taskIds, settled, chatSessionIds, godModeHere)
            .filterNot { it.isDismissOnly }
        val belongKeys = belongs.mapTo(mutableSetOf()) { it.askKey }
        val here = belongs.filterNot { item ->
            item.request(item.metadata).source == "plan_approval" && item.openTaskId?.let { it in planApprovalTaskIds } == true
        }
        val elsewhere = open(feed, settled).filterNot { it.askKey in belongKeys || it.isDismissOnly }
        return ComposerFeedAsks(here + elsewhere, here.mapTo(mutableSetOf()) { it.askKey })
    }

    /** Whether the raising session decides it: null when either side is unknown. */
    private fun sessionVerdict(item: AttentionItem, sessions: Set<String>): Boolean? {
        if (sessions.isEmpty()) return null
        return item.originChatSessionId?.let { it in sessions }
    }

    private fun belongsToThread(item: AttentionItem, threads: Set<String>): Boolean {
        val scope = item.metadata?.hitlRequest?.scope
        val schema = item.metadata?.inputSchema ?: item.metadata?.hitlRequest?.inputSchema
        return listOfNotNull(item.chatSessionId, schema?.contextString("chat_session_id"),
            scope?.chatSessionId, scope?.sessionId, scope?.threadId, scope?.uiThreadId, item.uiThreadId)
            .any { it.trim() in threads }
    }

    /** VibeDev's dock lists every open ask as chat's does, its run's turns,
     * project chat and God approvals first. Answering retains the originating scope. */
    fun forVibeDev(
        feed: AttentionFeedResponse,
        taskIds: Set<String>,
        chatSessionId: String? = null,
        settled: Set<String> = emptySet(),
    ): ComposerFeedAsks = forComposer(feed, setOfNotNull(chatSessionId), taskIds, settled, godModeHere = true)

    /**
     * A task's asks: its execution panel's `needs_attention` rows merged with
     * the feed rows for the same task. A feed row wins a duplicate, because it
     * is re-read after every answer while the panel is only polled.
     */
    fun forTask(
        feed: AttentionFeedResponse,
        taskId: String,
        panelRows: List<AttentionItem>,
        settled: Set<String> = emptySet(),
    ): List<AttentionItem> {
        val id = taskId.trim()
        if (id.isEmpty()) return emptyList()
        val fromFeed = open(feed, settled).filter { it.openTaskId == id }
        val fromPanel = panelRows.filter { it.isActionable && it.askIds.none(settled::contains) }
        return dedupe(fromFeed + fromPanel)
    }

    /**
     * Whether an ask is answered in the focused sheet rather than inline: a
     * secret or one-time code (masked, with its window) and a tool or sandbox
     * grant always, except a god-mode grant, which is answered where it is
     * shown; a multi-question form or a previewed approval (its diff and
     * evidence) only where space is short (the composer dock). Everything
     * else — choices, confirm / deny, text, a review decision — is answered
     * where it is shown.
     */
    fun answersInSheet(request: AttentionRequest, compact: Boolean): Boolean =
        collectsSecret(request) || (request.grantKind != null && !request.isGodMode) ||
            (compact && (request.inputType == "form" || request.approvalPreview != null))

    /** The button that opens the sheet, worded for what it collects. */
    fun sheetLabel(request: AttentionRequest): String = when {
        collectsSecret(request) -> "Enter it securely"
        request.grantKind != null -> "Review and decide"
        request.outwardReview != null && request.approvalPreview != null -> "Review the send"
        request.approvalPreview != null && request.inputType != "form" -> "Review the change"
        else -> "Answer the questions"
    }

    private fun collectsSecret(request: AttentionRequest): Boolean =
        request.isSensitive || request.renderKind == "password" || request.renderKind == "otp"

    /** One row per ask: a later row sharing any identifier with an earlier one is dropped. */
    fun dedupe(items: List<AttentionItem>): List<AttentionItem> {
        val seen = mutableSetOf<String>()
        return items.filter { item ->
            val ids = item.askIds
            if (ids.any(seen::contains)) false else { seen += ids; true }
        }
    }

    /**
     * An execution panel's `needs_attention` rows as feed items.
     *
     * The server flattens a feed item and puts the answer target beside it
     * (`ExecutionPanelAttentionItem { item, hitl_request }`), so a row decodes
     * as an [AttentionItem] once `hitl_request` is folded into its metadata —
     * after which it is answered exactly as the Attention sheet answers it.
     */
    fun panelItems(rows: List<JsonElement>): List<AttentionItem> = rows.mapNotNull { element ->
        val row = element as? JsonObject ?: return@mapNotNull null
        val item = runCatching { attentionJson.decodeFromJsonElement(AttentionItem.serializer(), row) }
            .getOrNull() ?: return@mapNotNull null
        val target = row["hitl_request"]?.let {
            runCatching { attentionJson.decodeFromJsonElement(HitlRequestPayload.serializer(), it) }.getOrNull()
        }
        val metadata = item.metadata ?: AttentionMetadata()
        val merged = if (metadata.hitlRequest == null && target != null) metadata.copy(hitlRequest = target) else metadata
        // A row with no answer target and no stamped contract is a status,
        // not an ask: answering it would post to a derived id nothing waits on.
        if (merged.hitlRequest == null && merged.pauseStateId.isNullOrBlank() && merged.inputType.isNullOrBlank() &&
            merged.inputSchema == null && !item.isStalledWait
        ) return@mapNotNull null
        item.copy(
            metadata = merged,
            status = item.status.ifBlank { "needs_action" },
        )
    }
}
