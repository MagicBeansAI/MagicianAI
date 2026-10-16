package ai.magicbeans.magdroid.attention

import ai.magicbeans.magdroid.chat.EscalationOption
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/** The scoped UserRequestService ledger, independent of feed projection timing. */
@Serializable
internal data class PendingUserRequests(val requests: List<PendingUserRequest> = emptyList())

@Serializable
internal data class PendingUserRequest(
    val id: String,
    val question: String,
    val options: List<EscalationOption> = emptyList(),
    val context: PendingUserRequestContext = PendingUserRequestContext(),
    @SerialName("created_at") val createdAt: Long = 0,
    @SerialName("request_type") val requestType: String? = null,
    @SerialName("task_id") val taskId: String? = null,
) {
    fun attentionItem(): AttentionItem {
        val task = taskId.clean() ?: context.taskId.clean()
        // The request type rides on the schema, where the feed projection
        // carries it, so a Relentless review reads the same from either path.
        val schema = when {
            requestType.clean() == null -> context.inputSchema
            context.inputSchema?.requestType != null -> context.inputSchema
            else -> (context.inputSchema ?: AttentionInputSchema()).copy(requestType = requestType.clean())
        }
        return AttentionItem(
            id = "user-request:$id",
            itemType = "request",
            taskId = task,
            uiThreadId = context.uiThreadId.clean(),
            title = question,
            status = "needs_action",
            createdAt = createdAt,
            updatedAt = createdAt,
            metadata = AttentionMetadata(
                source = "user_request",
                attentionKind = "user_request.pending",
                inputType = context.inputType ?: if (options.isEmpty()) "text" else "choice",
                inputSchema = schema,
                question = question,
                options = options,
                pauseStateId = id,
                reviewHref = context.reviewHref.clean(),
                reviewLabel = context.reviewLabel.clean(),
                taskId = task,
            ),
        )
    }
}

private fun String?.clean(): String? = this?.trim()?.takeIf { it.isNotEmpty() }

@Serializable
internal data class PendingUserRequestContext(
    @SerialName("input_type") val inputType: String? = null,
    @SerialName("input_schema") val inputSchema: AttentionInputSchema? = null,
    /** A Relentless review's Work Quality link and its wording. */
    @SerialName("review_href") val reviewHref: String? = null,
    @SerialName("review_label") val reviewLabel: String? = null,
    @SerialName("task_id") val taskId: String? = null,
    @SerialName("ui_thread_id") val uiThreadId: String? = null,
)

/** Reuse existing Attention forms and dispatch; correlate across different feed item ids. */
internal fun AttentionFeedResponse.withPendingUserRequests(
    pending: List<PendingUserRequest>,
): AttentionFeedResponse {
    val present = (requests + approvals + escalations).map { it.request(it.metadata) }
        .filter { it.source == "user_request" }.map { it.correlationId }.toSet()
    val added = pending.distinctBy { it.id }.filterNot { it.id in present }.map { it.attentionItem() }
    if (added.isEmpty()) return this
    val extra = added.size.toLong()
    return copy(
        requests = (requests + added).sortedByDescending { it.updatedAt },
        counts = counts.copy(requests = counts.requests + extra, needsAction = counts.needsAction + extra),
        totals = totals.copy(requests = maxOf(totals.requests, counts.requests) + extra),
        pages = pages.copy(requests = pages.requests.copy(total = pages.requests.total + added.size)),
    )
}
