package ai.magicbeans.magdroid.attention

import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.access.MagicianConnectionSnapshot
import ai.magicbeans.magdroid.net.CarriesFailure
import ai.magicbeans.magdroid.net.Failure
import ai.magicbeans.magdroid.net.FailureKind
import ai.magicbeans.magdroid.net.Failures
import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.parameter
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.http.ContentType
import io.ktor.http.contentType
import io.ktor.client.statement.bodyAsText
import io.ktor.http.isSuccess
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * The ask was settled on another surface before this answer landed.
 *
 * Not an error the form should show: the owner's decision is no longer needed,
 * so the sheet closes with a notice instead of offering a retry that would be
 * refused again.
 */
class AttentionAlreadyAnswered : Exception("Answered elsewhere.")

/**
 * Why a read failed, in words the screen can show.
 *
 * Carries a classified [Failure] rather than only a sentence, so the screen can
 * decide whether to offer Retry and whether Settings is where the fix lives.
 */
class AttentionError(override val failure: Failure) : Exception(failure.headline), CarriesFailure {

    /**
     * A refusal rather than a transport failure — the server was reached and
     * declined. There is nothing to classify and nothing to retry, so the
     * sentence is the whole of it.
     */
    constructor(message: String, setupRequired: Boolean = false) : this(
        Failure(
            kind = FailureKind.Unknown,
            headline = message,
            detail = "",
            retryable = !setupRequired,
            setupRequired = setupRequired,
        ),
    )

    val setupRequired: Boolean get() = failure.setupRequired
}

/**
 * Reads the attention feed.
 *
 * Separate from `ChatRepository`, which already reaches this endpoint for the
 * badge with `limit=1`. That call wants a number and nothing else; this one
 * wants the lanes. Sharing it would mean one of them always fetching more than
 * it needs.
 */
class AttentionRepository internal constructor(
    private val currentConnection: () -> MagicianConnectionSnapshot?,
    private val client: HttpClient,
) {
    constructor(context: Context) : this(connectionSource(context.applicationContext), runtimeClient())

    /**
     * One page of every lane.
     *
     * Cursors are per-lane and optional. Passing only the one being extended
     * keeps the other lanes on their first page, which is what "load more" in
     * a single tab should do — advancing all five would silently drop rows the
     * owner has not seen from the four they are not looking at.
     */
    suspend fun feed(
        limit: Int = PAGE,
        requestsCursor: String? = null,
        approvalsCursor: String? = null,
        escalationsCursor: String? = null,
        failedCursor: String? = null,
    ): AttentionFeedResponse {
        val connection = connected()
        val feed = readFeed(connection, limit, requestsCursor, approvalsCursor, escalationsCursor, failedCursor)
        return addPendingRequests(feed, connection)
    }

    /** Composer docks need every open ask, independently of Attention's viewed page. */
    suspend fun pendingFeed(): AttentionFeedResponse {
        val connection = connected()
        var feed = readFeed(connection, 200)
        var requests = feed.pages.requests.nextCursor?.takeIf(String::isNotBlank)
        var approvals = feed.pages.approvals.nextCursor?.takeIf(String::isNotBlank)
        var escalations = feed.pages.escalations.nextCursor?.takeIf(String::isNotBlank)
        val seen = mutableSetOf<Pair<String, String>>()
        while (listOf(requests, approvals, escalations).any { !it.isNullOrBlank() }) {
            for ((lane, cursor) in listOf("requests" to requests, "approvals" to approvals, "escalations" to escalations)) {
                if (!cursor.isNullOrBlank() && !seen.add(lane to cursor))
                    throw AttentionError("The server repeated a pending-question cursor. Refresh to retry.")
            }
            val page = readFeed(connection, 200, requests, approvals, escalations)
            feed = feed.copy(
                requests = (feed.requests + if (requests != null) page.requests else emptyList()).distinctBy { it.id },
                approvals = (feed.approvals + if (approvals != null) page.approvals else emptyList()).distinctBy { it.id },
                escalations = (feed.escalations + if (escalations != null) page.escalations else emptyList()).distinctBy { it.id },
            )
            requests = if (requests != null) page.pages.requests.nextCursor?.takeIf(String::isNotBlank) else null
            approvals = if (approvals != null) page.pages.approvals.nextCursor?.takeIf(String::isNotBlank) else null
            escalations = if (escalations != null) page.pages.escalations.nextCursor?.takeIf(String::isNotBlank) else null
        }
        return addPendingRequests(feed, connection)
    }

    /** Open one request without depending on which inbox page is loaded. */
    suspend fun pendingItem(id: String): AttentionItem? {
        val connection = connected()
        val encoded = java.net.URLEncoder.encode(id, "UTF-8").replace("+", "%20")
        val response = client.get("${base(connection)}/feed/attention/$encoded") {
            connection.headers.forEach { (name, value) -> header(name, value) }
        }
        requireCurrent(connection)
        if (response.status.value == 404) {
            return addPendingRequests(AttentionFeedResponse(), connection).items(AttentionLane.All)
                .firstOrNull { id in it.askIds }
        }
        if (!response.status.isSuccess()) throw AttentionError("Could not load this request. Please try again.")
        val body = response.bodyAsText()
        requireCurrent(connection)
        return attentionJson.decodeFromString(AttentionItem.serializer(), body)
            .copy(originConnection = connection)
    }

    private suspend fun readFeed(
        connection: MagicianConnectionSnapshot, limit: Int,
        requestsCursor: String? = null, approvalsCursor: String? = null,
        escalationsCursor: String? = null, failedCursor: String? = null,
    ): AttentionFeedResponse {
        requireCurrent(connection)
        val response = client.get("${base(connection)}/feed/attention") {
            connection.headers.forEach { (name, value) -> header(name, value) }
            parameter("limit", limit)
            requestsCursor?.let { parameter("requests_cursor", it) }
            approvalsCursor?.let { parameter("approvals_cursor", it) }
            escalationsCursor?.let { parameter("escalations_cursor", it) }
            failedCursor?.let { parameter("failed_cursor", it) }
        }
        val body = response.bodyAsText()
        requireCurrent(connection)
        if (!response.status.isSuccess()) throw AttentionError(Failures.ofStatus(response.status.value, WHAT))
        return runCatching { attentionJson.decodeFromString(AttentionFeedResponse.serializer(), body) }
            .getOrElse { throw AttentionError(Failures.garbled(WHAT)) }
    }

    private suspend fun addPendingRequests(feed: AttentionFeedResponse, connection: MagicianConnectionSnapshot): AttentionFeedResponse {
        requireCurrent(connection)
        val pending = client.get("${base(connection)}/user-requests") {
            connection.headers.forEach { (name, value) -> header(name, value) }
        }
        requireCurrent(connection)
        if (!pending.status.isSuccess()) throw AttentionError(Failures.ofStatus(pending.status.value, "pending questions"))
        val body = pending.bodyAsText()
        requireCurrent(connection)
        val requests = runCatching {
            attentionJson.decodeFromString(PendingUserRequests.serializer(), body)
        }.getOrElse { throw AttentionError(Failures.garbled("pending questions")) }
        val merged = feed.withPendingUserRequests(requests.requests)
        fun bind(rows: List<AttentionItem>) = rows.map { it.copy(originConnection = connection) }
        return merged.copy(requests = bind(merged.requests), approvals = bind(merged.approvals),
            escalations = bind(merged.escalations), failed = bind(merged.failed), running = bind(merged.running))
    }

    /**
     * Take a failed card off the list, or put it back.
     *
     * Two routes rather than one with a flag, because that is what the server
     * exposes; `dismissed` defaults true on the dismiss route, so the body only
     * ever needs the id.
     */
    /**
     * Requests already answered, newest first.
     *
     * There is no resolved-requests resource: the history is the HITL event
     * backfill, paired here. `backfill_only` asks for what has already happened
     * rather than opening a live stream.
     */
    suspend fun resolved(limit: Int = HISTORY_LIMIT): List<ResolvedHitl> {
        val connection = connected()
        val response = client.get("${base(connection)}/events") {
            connection.headers.forEach { (name, value) -> header(name, value) }
            parameter("category", "hitl")
            parameter("backfill_only", true)
            parameter("limit", limit)
        }
        if (!response.status.isSuccess()) return emptyList()
        return pairResolvedHitl(response.bodyAsText())
    }

    suspend fun setDismissed(itemId: String, dismissed: Boolean) {
        val connection = connected()
        val path = if (dismissed) "dismiss" else "undismiss"
        val response = client.post("${base(connection)}/feed/attention/$path") {
            connection.headers.forEach { (name, value) -> header(name, value) }
            contentType(ContentType.Application.Json)
            setBody(buildJsonObject { put("item_id", itemId) }.toString())
        }
        if (!response.status.isSuccess()) {
            throw AttentionError(
                if (dismissed) "Could not dismiss this card." else "Could not restore this card.",
            )
        }
    }

    /**
     * Answer a waiting item.
     *
     * The source is derived per item rather than fixed, because an approval,
     * a clarification and an agentic pause resume through different paths on
     * the server and posting the wrong one leaves the execution where it was.
     */
    suspend fun respond(
        request: AttentionRequest,
        value: ai.magicbeans.magdroid.chat.HitlResponseValue,
        /**
         * The task a planning ask belongs to. A `plan_approval` ask's
         * correlation id is the plan id, so the respond arm needs the task
         * named to resume it (web `resolveHitlCall` sends it the same way).
         */
        taskId: String? = null,
    ) {
        // The server's respond handle names the call; the derivation is the
        // fallback for servers that predate it ([respondCall]).
        val connection = connected(request.originConnection)
        val call = request.respondCall(value.toJson(), taskId = taskId)
        val response = client.post("${connection.baseUrl.trimEnd('/')}${call.path}") {
            connection.headers.forEach { (name, value2) -> header(name, value2) }
            contentType(ContentType.Application.Json)
            setBody(call.body.toString())
        }
        val body = response.bodyAsText()
        requireCurrent(connection)
        val payload = runCatching { attentionJson.parseToJsonElement(body) as? kotlinx.serialization.json.JsonObject }.getOrNull()
        fun text(key: String) = (payload?.get(key) as? kotlinx.serialization.json.JsonPrimitive)
            ?.takeUnless { it == kotlinx.serialization.json.JsonNull }?.content?.takeIf(String::isNotBlank)
        if (!response.status.isSuccess()) {
            // Gone already (the call's soft-success statuses: a drained pause's
            // 404/410, a 409 `already_resolved`): the decision is no longer
            // needed, so close with a notice rather than a retryable error.
            val status = response.status.value
            if (status in call.softSuccessStatuses && (status != 409 || text("reason") == "already_resolved")) {
                throw AttentionAlreadyAnswered()
            }
            throw AttentionError("Magician did not accept the answer.")
        }
        // `accepted:false` arrives with 200 and a reason — an item already
        // answered, or one belonging to another scope. Treating the status code
        // alone as success would mark it done when nothing happened.
        if (text("status") == "reask_required") {
            throw AttentionError(text("message") ?: text("question") ?: "Please revise your answer.")
        }
        if (text("accepted") == "false" || text("resumed") == "false" ||
            (text("accepted") != "true" && text("resumed") != "true")) {
            if (text("reason") == "already_resolved") throw AttentionAlreadyAnswered()
            throw AttentionError(text("message") ?: text("reason") ?: "Magician did not accept the answer.")
        }
    }

    /**
     * Start a task's run: `POST /api/magician/v3/tasks/{id}/execute`.
     *
     * The second half of Approve & Run, as web `startRunAfterPlanApproval`
     * does it. Approving a plan does not start a run, so this is its own call;
     * it throws with the server's reason when the run did not start, and the
     * approval already posted stands either way.
     */
    suspend fun startRun(taskId: String, originConnection: MagicianConnectionSnapshot? = null) {
        val connection = connected(originConnection)
        val id = java.net.URLEncoder.encode(taskId, "UTF-8").replace("+", "%20")
        val response = client.post("${base(connection).removeSuffix("/v2")}/v3/tasks/$id/execute") {
            connection.headers.forEach { (name, value) -> header(name, value) }
            contentType(ContentType.Application.Json)
            setBody("{}")
        }
        requireCurrent(connection)
        if (response.status.isSuccess()) return
        val body = response.bodyAsText()
        val reason = runCatching {
            val json = attentionJson.parseToJsonElement(body) as? kotlinx.serialization.json.JsonObject
            listOf("message", "error", "reason").firstNotNullOfOrNull { key ->
                (json?.get(key) as? kotlinx.serialization.json.JsonPrimitive)?.content?.takeIf { it.isNotBlank() }
            }
        }.getOrNull()
        throw AttentionError(reason ?: "The run did not start (HTTP ${response.status.value}).")
    }

    /**
     * Cancel a run that is waiting on the owner with no question delivered (C3).
     *
     * The stalled-wait card's only way out: there is nothing to answer.
     */
    suspend fun cancelExecution(executionId: String) {
        val connection = connected()
        val id = java.net.URLEncoder.encode(executionId, "UTF-8").replace("+", "%20")
        val response = client.post("${base(connection)}/executions/$id/cancel") {
            connection.headers.forEach { (name, value) -> header(name, value) }
        }
        if (!response.status.isSuccess()) throw AttentionError("Could not cancel that run.")
    }

    /** Messages nobody has replied to, newest first. */
    suspend fun followUps(cursor: String? = null, limit: Int = PAGE): ChannelFollowUpPage {
        val connection = connected()
        val response = client.get("${base(connection)}/channel-assist/follow-ups") {
            connection.headers.forEach { (name, value) -> header(name, value) }
            parameter("limit", limit)
            cursor?.let { parameter("cursor", it) }
        }
        if (!response.status.isSuccess()) {
            throw AttentionError(Failures.ofStatus(response.status.value, MESSAGES))
        }
        return runCatching {
            attentionJson.decodeFromString(ChannelFollowUpPage.serializer(), response.bodyAsText())
        }.getOrElse { throw AttentionError(Failures.garbled(MESSAGES)) }
    }

    /**
     * Resolve one follow-up.
     *
     * The action is a path segment, which is why [FollowUpAction] is an enum
     * rather than a string — a typo would be a 404 that reads as a card which
     * simply will not resolve.
     */
    suspend fun resolveFollowUp(
        annotationId: String,
        action: FollowUpAction,
        reason: String? = null,
    ) {
        val connection = connected()
        val response = client.post(
            "${base(connection)}/channel-assist/annotations/$annotationId/${action.id}",
        ) {
            connection.headers.forEach { (name, value) -> header(name, value) }
            contentType(ContentType.Application.Json)
            setBody(
                buildJsonObject {
                    reason?.trim()?.takeIf { it.isNotEmpty() }?.let { put("reason", it) }
                }.toString(),
            )
        }
        if (!response.status.isSuccess()) {
            throw AttentionError("Could not ${action.label.lowercase()} that message.")
        }
    }

    /**
     * The owner-private read of one alignment hold: why it was held, and the
     * choices that change what Magician remembers. Android is an owner-private
     * surface (2026-10-11). A replaced or already-answered ask reads as
     * [AlignmentDetailOutcome.Gone]; anything else unreadable falls back to
     * the act's own choices.
     */
    suspend fun alignmentDetail(correlationId: String): AlignmentDetailOutcome {
        val connection = runCatching { connected() }.getOrElse { return AlignmentDetailOutcome.Unavailable }
        val response = runCatching {
            client.get("${base(connection)}/hitl/$correlationId/alignment") {
                connection.headers.forEach { (name, value) -> header(name, value) }
                parameter("surface", "android")
            }
        }.getOrElse { return AlignmentDetailOutcome.Unavailable }
        return when {
            response.status.value == 404 || response.status.value == 409 -> AlignmentDetailOutcome.Gone
            !response.status.isSuccess() -> AlignmentDetailOutcome.Unavailable
            else -> parseAlignmentDetail(response.bodyAsText())
                ?.takeIf { it.isPrivate }
                ?.let { AlignmentDetailOutcome.Ready(it) }
                ?: AlignmentDetailOutcome.Unavailable
        }
    }

    private fun base(connection: MagicianConnectionSnapshot): String = "${connection.baseUrl.trimEnd('/')}/api/magician/v2"

    private fun connected(expected: MagicianConnectionSnapshot? = null): MagicianConnectionSnapshot {
        val connection = currentConnection() ?: throw AttentionError("No Magician host configured yet.", setupRequired = true)
        if (expected != null && connection != expected) throw AttentionError("Connection changed. Reopen this request before answering.")
        return connection
    }

    private fun requireCurrent(connection: MagicianConnectionSnapshot) {
        if (connection != currentConnection()) throw AttentionError("Connection changed. Reopen this request before answering.")
    }

    fun close() = client.close()

    companion object {
        private fun connectionSource(app: Context): () -> MagicianConnectionSnapshot? = { MagicianAccess.connectionSnapshot(app) }
        private fun runtimeClient() = HttpClient(CIO) {
            install(HttpTimeout) { requestTimeoutMillis = 30_000; connectTimeoutMillis = 20_000 }
        }

        /** Matches what the other paged surfaces on this client ask for. */
        const val PAGE = 25

        /** The history window iOS asks for. A history is browsed, not paged forever. */
        const val HISTORY_LIMIT = 300

        /** Named once, so every failure sentence about this feed reads alike. */
        private const val WHAT = "what needs you"
        private const val MESSAGES = "waiting messages"
    }
}
