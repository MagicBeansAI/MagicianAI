package ai.magicbeans.magdroid.attention

import ai.magicbeans.magdroid.access.MagicianAccess
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
class AttentionRepository(context: Context) {

    private val app = context.applicationContext

    private val client = HttpClient(CIO) {
        install(HttpTimeout) {
            requestTimeoutMillis = 30_000
            connectTimeoutMillis = 20_000
        }
    }

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
        val response = client.get("${base()}/feed/attention") {
            MagicianAccess.headers(app).forEach { (name, value) -> header(name, value) }
            parameter("limit", limit)
            requestsCursor?.let { parameter("requests_cursor", it) }
            approvalsCursor?.let { parameter("approvals_cursor", it) }
            escalationsCursor?.let { parameter("escalations_cursor", it) }
            failedCursor?.let { parameter("failed_cursor", it) }
        }
        val body = response.bodyAsText()
        if (!response.status.isSuccess()) {
            throw AttentionError(Failures.ofStatus(response.status.value, WHAT))
        }
        val feed = runCatching {
            attentionJson.decodeFromString(AttentionFeedResponse.serializer(), body)
        }.getOrElse {
            throw AttentionError(Failures.garbled(WHAT))
        }
        // A chat question can already be blocking its turn before a feed item
        // exists. Read the canonical pending ledger as well, including after
        // app restart; a missed socket frame must not hide the answer form.
        val pending = client.get("${base()}/user-requests") {
            MagicianAccess.headers(app).forEach { (name, value) -> header(name, value) }
        }
        if (!pending.status.isSuccess()) {
            throw AttentionError(Failures.ofStatus(pending.status.value, "pending questions"))
        }
        val requests = runCatching {
            attentionJson.decodeFromString(PendingUserRequests.serializer(), pending.bodyAsText())
        }.getOrElse { throw AttentionError(Failures.garbled("pending questions")) }
        return feed.withPendingUserRequests(requests.requests)
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
        val response = client.get("${base()}/events") {
            MagicianAccess.headers(app).forEach { (name, value) -> header(name, value) }
            parameter("category", "hitl")
            parameter("backfill_only", true)
            parameter("limit", limit)
        }
        if (!response.status.isSuccess()) return emptyList()
        return pairResolvedHitl(response.bodyAsText())
    }

    suspend fun setDismissed(itemId: String, dismissed: Boolean) {
        val path = if (dismissed) "dismiss" else "undismiss"
        val response = client.post("${base()}/feed/attention/$path") {
            MagicianAccess.headers(app).forEach { (name, value) -> header(name, value) }
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
    ) {
        val response = client.post("${base()}/hitl/${request.correlationId}/respond") {
            MagicianAccess.headers(app).forEach { (name, value2) -> header(name, value2) }
            contentType(ContentType.Application.Json)
            setBody(
                buildJsonObject {
                    put("source", request.source)
                    put("value", value.toJson())
                    put("input_type", request.inputType)
                    put("channel", "android")
                }.toString(),
            )
        }
        val body = response.bodyAsText()
        if (!response.status.isSuccess()) throw AttentionError("Magician did not accept the answer.")
        // `accepted:false` arrives with 200 and a reason — an item already
        // answered, or one belonging to another scope. Treating the status code
        // alone as success would mark it done when nothing happened.
        val accepted = runCatching {
            attentionJson.decodeFromString(
                ai.magicbeans.magdroid.chat.HitlRespondResponse.serializer(), body,
            )
        }.getOrNull()
        if (accepted != null && !accepted.accepted) throw AttentionError(accepted.refusal())
    }

    /** Messages nobody has replied to, newest first. */
    suspend fun followUps(cursor: String? = null, limit: Int = PAGE): ChannelFollowUpPage {
        val response = client.get("${base()}/channel-assist/follow-ups") {
            MagicianAccess.headers(app).forEach { (name, value) -> header(name, value) }
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
        val response = client.post(
            "${base()}/channel-assist/annotations/$annotationId/${action.id}",
        ) {
            MagicianAccess.headers(app).forEach { (name, value) -> header(name, value) }
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

    private fun base(): String {
        val host = MagicianAccess.baseUrl(app).trimEnd('/')
        if (host.isEmpty()) {
            throw AttentionError("No Magician host configured yet.", setupRequired = true)
        }
        return "$host/api/magician/v2"
    }

    fun close() = client.close()

    companion object {
        /** Matches what the other paged surfaces on this client ask for. */
        const val PAGE = 25

        /** The history window iOS asks for. A history is browsed, not paged forever. */
        const val HISTORY_LIMIT = 300

        /** Named once, so every failure sentence about this feed reads alike. */
        private const val WHAT = "what needs you"
        private const val MESSAGES = "waiting messages"
    }
}
