package ai.magicbeans.magdroid.attention

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

/*
 * Which open asks each surface shows. Attention lists every one; the composer
 * ask dock shows the ones that belong to the conversation on screen; a task's
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
     */
    fun forThread(
        feed: AttentionFeedResponse,
        threadIds: Set<String>,
        taskIds: Set<String>,
        settled: Set<String> = emptySet(),
    ): List<AttentionItem> {
        val threads = threadIds.mapNotNull { it.trim().takeIf(String::isNotEmpty) }.toSet()
        val tasks = taskIds.mapNotNull { it.trim().takeIf(String::isNotEmpty) }.toSet()
        if (threads.isEmpty() && tasks.isEmpty()) return emptyList()
        return open(feed, settled).filter { item ->
            item.uiThreadId?.trim()?.let { it in threads } == true ||
                item.openTaskId?.let { it in tasks } == true
        }
    }

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
     * grant always; a multi-question form only where space is short (the
     * composer dock). Everything else — choices, confirm / deny, text, a
     * review decision — is answered where it is shown.
     */
    fun answersInSheet(request: AttentionRequest, compact: Boolean): Boolean =
        collectsSecret(request) || request.grantKind != null || (compact && request.inputType == "form")

    /** The button that opens the sheet, worded for what it collects. */
    fun sheetLabel(request: AttentionRequest): String = when {
        collectsSecret(request) -> "Enter it securely"
        request.grantKind != null -> "Review and decide"
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
