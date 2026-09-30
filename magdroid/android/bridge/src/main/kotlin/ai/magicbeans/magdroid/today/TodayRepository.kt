package ai.magicbeans.magdroid.today

import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.net.CarriesFailure
import ai.magicbeans.magdroid.net.Failure
import ai.magicbeans.magdroid.net.FailureKind
import ai.magicbeans.magdroid.net.Failures
import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.plugins.websocket.WebSockets
import io.ktor.client.plugins.websocket.webSocket
import io.ktor.client.request.HttpRequestBuilder
import io.ktor.client.request.delete
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.parameter
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import io.ktor.websocket.Frame
import io.ktor.websocket.readText
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.isActive
import kotlinx.serialization.KSerializer
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put
import java.net.URLEncoder
import java.nio.charset.StandardCharsets
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import kotlin.math.min
import kotlin.random.Random
import java.util.concurrent.ConcurrentHashMap

val todayJson: Json = Json {
    ignoreUnknownKeys = true
    explicitNulls = false
    coerceInputValues = true
    encodeDefaults = true
}

/**
 * Why a Today call failed.
 *
 * Keeps [status] because callers branch on it, and adds a classified [Failure]
 * so the screen can say what went wrong rather than restate its own name.
 */
class TodayApiError(
    message: String,
    val status: Int = 0,
    override val failure: Failure = if (status > 0) {
        Failures.ofStatus(status, "your day")
    } else {
        Failure(FailureKind.Unknown, message, "", retryable = true)
    },
) : Exception(message), CarriesFailure

/** Injectable boundary for the complete Today surface. */
interface TodayDataSource {
    suspend fun today(section: TodaySection? = null, cursor: String? = null, digestOffset: Int = 0): TodayResponse
    suspend fun hidden(): List<HiddenTodayItem>
    suspend fun resurfacing(limit: Int = 8, cursor: ResurfacingCursor? = null, offset: Int? = null): ResurfacingPage
    suspend fun followUps(limit: Int = 5, cursor: String? = null): ChannelFollowUpPage
    /** Raw `/v2/feed` rows; the view model derives Activity and the wire from one read. */
    suspend fun feed(limit: Int = 80): List<TodayActivityItem>
    suspend fun agentUpdates(): List<TodayAgentUpdate>
    /** Events recorded in the last 24 hours, for the wire's count chip. */
    suspend fun eventCount24h(nowMs: Long = System.currentTimeMillis()): Long
    suspend fun agents(): TodayAgentCounts
    /** State of the Crew: per-agent model use and tasks over the last 24 hours. */
    suspend fun crew(nowMs: Long = System.currentTimeMillis()): TodayCrew
    suspend fun briefings(limit: Int = 8): List<TodayBriefing>
    suspend fun briefingRender(surfaceId: String): TodayBriefingRender
    suspend fun pulse(previous: TodayPulse? = null): TodayPulse

    suspend fun setVisibility(item: TodayItem?, itemId: String, action: String, snoozeMinutes: Int? = null)
    suspend fun executeTodayAction(endpoint: String): String
    suspend fun resolveFollowUp(item: ChannelFollowUp, action: String, hint: String? = null, reason: String? = null): AttentionFeedbackReceipt?
    suspend fun followUpMessage(annotationId: String): ChannelMessageView
    suspend fun composeFollowUp(item: ChannelFollowUp, descriptor: ChannelActionDescriptor, hint: String? = null): ChannelActionDraft
    suspend fun commitFollowUp(item: ChannelFollowUp, descriptor: ChannelActionDescriptor, body: String? = null, composeId: String? = null): AttentionFeedbackReceipt?
    suspend fun writingPreferences(annotationId: String): List<ChannelWritingPreference>
    suspend fun learnWritingPreference(annotationId: String, scope: String, statement: String, promote: Boolean)
    suspend fun updateWritingPreference(id: String, action: String)

    suspend fun resolveResurfacing(card: ResurfacingCard, action: ResurfacingFeedbackAction, reason: String? = null): AttentionFeedbackReceipt?
    suspend fun resurfacingDetail(cardId: String, original: Boolean = false): ResurfacingDetail
    suspend fun performResurfacingAction(card: ResurfacingCard, kind: ResurfacingActionKind, input: JsonObject = JsonObject(emptyMap())): ResurfacingActionResult
    suspend fun recordRecommendation(card: ResurfacingCard, kind: ResurfacingActionKind, event: String)

    suspend fun deleteActivity(id: String)
    suspend fun canonicalDeliveries(surface: String, reference: CanonicalAttentionProjectionReference, pageSize: Int): List<AttentionDeliveryBinding>
    suspend fun recordImpression(binding: AttentionDeliveryBinding, visibleMs: Int, eventId: String): AttentionImpressionReceipt
    /** Scope-matched realtime frames; [TodayRealtimeFrame.refresh] marks the Today-relevant ones. */
    fun events(): Flow<TodayRealtimeFrame>
}

/** One scope-matched websocket frame. */
data class TodayRealtimeFrame(val raw: String, val refresh: Boolean)

class TodayRepository(private val context: Context) : TodayDataSource {
    private val impressionReceipts = ConcurrentHashMap<String, AttentionImpressionReceipt>()
    private val client = HttpClient(CIO) {
        install(HttpTimeout) {
            requestTimeoutMillis = 300_000
            socketTimeoutMillis = 300_000
            connectTimeoutMillis = 20_000
        }
        install(WebSockets)
    }

    private fun host(): String = MagicianAccess.baseUrl(context).trimEnd('/').ifBlank {
        throw TodayApiError("No Magician host configured yet.")
    }
    private fun v2() = "${host()}/api/magician/v2"
    private fun v3() = "${host()}/api/magician/v3"

    private fun HttpRequestBuilder.authorize() {
        MagicianAccess.headers(context).forEach { (name, value) -> header(name, value) }
    }

    override suspend fun today(section: TodaySection?, cursor: String?, digestOffset: Int): TodayResponse {
        val response = client.get("${v2()}/today") {
            authorize()
            parameter("today", LocalDate.now(ZoneId.systemDefault()).toString())
            parameter("per_section", 8)
            parameter("digest_limit", TODAY_DIGEST_PAGE)
            if (digestOffset > 0) parameter("digest_offset", digestOffset)
            section?.let { parameter("section", it.wire); parameter("limit", 8) }
            cursor?.let { parameter("cursor", it) }
        }
        return response.decode(TodayResponse.serializer(), "load Today")
    }

    override suspend fun hidden(): List<HiddenTodayItem> = client.get("${v2()}/today/visibility") {
        authorize()
    }.decode(HiddenTodayResponse.serializer(), "load hidden Today items").items

    override suspend fun resurfacing(limit: Int, cursor: ResurfacingCursor?, offset: Int?): ResurfacingPage =
        client.get("${v2()}/channel-assist/resurfacing/today") {
            authorize(); parameter("limit", limit.coerceIn(1, 50))
            cursor?.let {
                parameter("cursor_surfaced_at", it.surfacedAt)
                parameter("cursor_score", it.score)
                parameter("cursor_candidate_id", it.candidateId)
            }
            // Keyset cursor for the deck's load-more; offset for the
            // broadsheet's numbered pages (web ServerPager does the same).
            if (cursor == null) offset?.let { parameter("offset", it.coerceAtLeast(0)) }
        }.decode(ResurfacingPage.serializer(), "load Worth a look")

    override suspend fun followUps(limit: Int, cursor: String?): ChannelFollowUpPage =
        client.get("${v2()}/channel-assist/follow-ups") {
            authorize(); parameter("limit", limit.coerceIn(1, 50)); cursor?.let { parameter("cursor", it) }
        }.decode(ChannelFollowUpPage.serializer(), "load message follow-ups")

    override suspend fun feed(limit: Int): List<TodayActivityItem> = client.get("${v2()}/feed") {
        authorize(); parameter("limit", limit.coerceIn(1, 200))
    }.decode(TodayActivityResponse.serializer(), "load activity").items

    override suspend fun agentUpdates(): List<TodayAgentUpdate> =
        parseAgentUpdates(client.get("${v2()}/agents/updates") { authorize() }.json("load agent updates"))

    override suspend fun eventCount24h(nowMs: Long): Long {
        val response = client.post("${v2()}/analytics/query") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(buildJsonObject { put("sql", eventCount24hSql(nowMs)) }.toString())
        }
        return parseEventCount24h(response.json("count today's events"))
            ?: throw TodayApiError("The event count was not returned.")
    }

    override suspend fun agents(): TodayAgentCounts =
        parseAgentCounts(client.get("${v2()}/agents") { authorize() }.json("load agents"))

    override suspend fun crew(nowMs: Long): TodayCrew = coroutineScope {
        val since = nowMs - CREW_WINDOW_MS
        val agents = async { parseCrewAgents(client.get("${v2()}/agents") { authorize() }.json("load agents")) }
        val usage = async { query("/analytics/llm_calls/query", crewUsageSql(since)).let { parseCrewUsage(it.columns, it.rows) } }
        val tasks = async {
            val collected = mutableListOf<TodayCrewTask>()
            var cursor: String? = null
            for (page in 0 until CREW_TASK_MAX_PAGES) {
                val root = client.get("${v3()}/tasks") {
                    authorize()
                    parameter("limit", CREW_TASK_PAGE_LIMIT); parameter("sort", "updated_at"); parameter("order", "desc")
                    cursor?.let { parameter("cursor", it) }
                }.json("load crew tasks") as? JsonObject ?: break
                val rows = root["tasks"] as? JsonArray ?: break
                collected += crewTasks(rows, since)
                val next = (root["pagination"] as? JsonObject)?.get("next_cursor").text()
                if (!crewTaskPageContinues(rows, since, next)) break
                cursor = next
            }
            collected
        }
        assembleCrew(agents.await(), usage.await(), tasks.await())
    }

    override suspend fun briefings(limit: Int): List<TodayBriefing> =
        client.get("${v3()}/published-surfaces/projections") {
            authorize(); parameter("route", "/briefing"); parameter("limit", limit.coerceIn(1, 50))
        }.decode(TodayBriefingsResponse.serializer(), "load briefings").surfaces

    override suspend fun briefingRender(surfaceId: String): TodayBriefingRender =
        client.get("${v3()}/published-surfaces/${segment(surfaceId)}/render") { authorize() }
            .decode(TodayBriefingRenderEnvelope.serializer(), "load briefing").render

    override suspend fun setVisibility(item: TodayItem?, itemId: String, action: String, snoozeMinutes: Int?) {
        require(action in setOf("dismiss", "snooze", "mark_seen", "restore"))
        val body = buildJsonObject {
            put("action", action)
            snoozeMinutes?.let { put("snooze_minutes", it) }
            item?.let { value ->
                put("snapshot", todayJson.encodeToJsonElement(TodayVisibilitySnapshot.serializer(), TodayVisibilitySnapshot(
                    title = value.title, summary = value.summary, reason = value.reason, section = value.section,
                    sourceKind = value.sourceKind, sourceId = value.sourceId, sourceUrl = value.sourceUrl,
                    spaceIds = value.spaceIds, itemUpdatedAt = value.updatedAt,
                )))
            }
        }
        post("${v2()}/today/items/${segment(itemId)}/visibility", body, "update Today item")
    }

    override suspend fun executeTodayAction(endpoint: String): String {
        require(TODAY_ACTION_ENDPOINT.matches(endpoint))
        val result = postDecode(host() + endpoint, JsonObject(emptyMap()), TodayActionExecutionResult.serializer(), "run Today action")
        return result.resolvedTaskId?.takeIf(String::isNotBlank)
            ?: throw TodayApiError("The Today action did not return a task.")
    }

    override suspend fun resolveFollowUp(
        item: ChannelFollowUp, action: String, hint: String?, reason: String?,
    ): AttentionFeedbackReceipt? {
        require(action in setOf("approve", "dismiss", "acknowledge", "useful", "snooze"))
        val body = buildJsonObject {
            hint?.trim()?.takeIf(String::isNotEmpty)?.let { put("hint", it) }
            reason?.takeIf(String::isNotEmpty)?.let { put("reason", it) }
            if (action in setOf("approve", "dismiss", "acknowledge", "useful")) {
                put("event_id", newEventId())
                feedbackAttribution(item)?.let { put("attribution", todayJson.encodeToJsonElement(AttentionFeedbackAttribution.serializer(), it)) }
            }
        }
        return postFeedback("${v2()}/channel-assist/annotations/${segment(item.id)}/${segment(action)}", body, "resolve follow-up")
    }

    override suspend fun followUpMessage(annotationId: String): ChannelMessageView =
        client.get("${v2()}/channel-assist/annotations/${segment(annotationId)}/message") { authorize() }
            .decode(ChannelMessageView.serializer(), "load message")

    override suspend fun composeFollowUp(item: ChannelFollowUp, descriptor: ChannelActionDescriptor, hint: String?): ChannelActionDraft {
        val body = buildJsonObject { hint?.trim()?.takeIf(String::isNotEmpty)?.let { put("hint", it) } }
        return postDecode(
            "${v2()}/channel-assist/annotations/${segment(item.id)}/action/${segment(descriptor.id)}/compose",
            body, ChannelActionDraft.serializer(), "compose ${descriptor.label}",
        )
    }

    override suspend fun commitFollowUp(
        item: ChannelFollowUp, descriptor: ChannelActionDescriptor, body: String?, composeId: String?,
    ): AttentionFeedbackReceipt? {
        val payload = buildJsonObject {
            body?.trim()?.takeIf(String::isNotEmpty)?.let { put("body", it) }
            composeId?.takeIf(String::isNotEmpty)?.let { put("compose_id", it) }
            put("event_id", newEventId())
            feedbackAttribution(item)?.let { put("attribution", todayJson.encodeToJsonElement(AttentionFeedbackAttribution.serializer(), it)) }
        }
        return postFeedback(
            "${v2()}/channel-assist/annotations/${segment(item.id)}/action/${segment(descriptor.id)}/commit",
            payload, "commit ${descriptor.label}",
        )
    }

    override suspend fun writingPreferences(annotationId: String): List<ChannelWritingPreference> =
        client.get("${v2()}/channel-assist/annotations/${segment(annotationId)}/writing-preferences") { authorize() }
            .decode(ChannelWritingPreferencesResponse.serializer(), "load writing preferences").items

    override suspend fun learnWritingPreference(annotationId: String, scope: String, statement: String, promote: Boolean) {
        require(statement.isNotBlank())
        post("${v2()}/channel-assist/annotations/${segment(annotationId)}/writing-preferences", buildJsonObject {
            put("scope", scope); put("statement", statement.trim()); put("promote", promote)
        }, "learn writing preference")
    }

    override suspend fun updateWritingPreference(id: String, action: String) {
        require(action in setOf("promote", "dismiss"))
        post("${v2()}/channel-assist/writing-preferences/${segment(id)}/${segment(action)}", JsonObject(emptyMap()), "update writing preference")
    }

    override suspend fun resolveResurfacing(card: ResurfacingCard, action: ResurfacingFeedbackAction, reason: String?): AttentionFeedbackReceipt? {
        val body = buildJsonObject {
            put("action", action.wire); put("event_id", newEventId())
            // Only dismiss carries a reason, and only one resurfacing accepts.
            reason?.takeIf { action == ResurfacingFeedbackAction.Dismiss && it in ChannelDismissOption.resurfacingCodes }
                ?.let { put("reason", it) }
            feedbackAttribution(card)?.let { put("attribution", todayJson.encodeToJsonElement(AttentionFeedbackAttribution.serializer(), it)) }
        }
        return postFeedback("${v2()}/channel-assist/resurfacing/${segment(card.id)}/action", body, "resolve Worth a look")
    }

    override suspend fun resurfacingDetail(cardId: String, original: Boolean): ResurfacingDetail {
        val suffix = if (original) "original" else "detail"
        return client.get("${v2()}/channel-assist/resurfacing/${segment(cardId)}/$suffix") { authorize() }
            .decode(ResurfacingDetail.serializer(), "load resurfacing detail")
    }

    override suspend fun performResurfacingAction(card: ResurfacingCard, kind: ResurfacingActionKind, input: JsonObject): ResurfacingActionResult =
        postDecode("${v2()}/channel-assist/resurfacing/${segment(card.id)}/actions", buildJsonObject {
            put("kind", kind.wire); put("idempotency_key", newEventId())
            put("content_revision", card.contentRevision?.let(::JsonPrimitive) ?: JsonNull)
            put("input", input)
        }, ResurfacingActionResult.serializer(), "run ${kind.wire}")

    override suspend fun recordRecommendation(card: ResurfacingCard, kind: ResurfacingActionKind, event: String) {
        require(event in setOf("presented", "selected", "completed"))
        post("${v2()}/channel-assist/resurfacing/${segment(card.id)}/recommendation-event", buildJsonObject {
            put("kind", kind.wire)
            put("content_revision", card.contentRevision?.let(::JsonPrimitive) ?: JsonNull)
            put("event", event)
        }, "record recommendation")
    }

    override suspend fun deleteActivity(id: String) {
        val response = client.delete("${v2()}/feed/items/${segment(id)}") { authorize() }
        response.successText("remove activity")
    }

    override suspend fun canonicalDeliveries(
        surface: String, reference: CanonicalAttentionProjectionReference, pageSize: Int,
    ): List<AttentionDeliveryBinding> {
        require(surface in setOf("follow_up", "worth_a_look"))
        val page = client.get("${v2()}/channel-assist/attention-learning/canonical-deliveries/$surface") {
            authorize(); parameter("page_size", pageSize.coerceAtLeast(1))
        }.decode(AttentionDeliveryPageResponse.serializer(), "load attention delivery")
        return page.validatedBindings(
            reference,
            surface,
            MagicianAccess.principal(context),
            MagicianAccess.workspace(context),
        )
            ?: throw TodayApiError("Attention delivery failed validation.")
    }

    override suspend fun recordImpression(
        binding: AttentionDeliveryBinding, visibleMs: Int, eventId: String,
    ): AttentionImpressionReceipt {
        val bounded = visibleMs.coerceIn(binding.minVisibleMs, 86_400_000)
        val response = postDecode("${v2()}/channel-assist/attention-learning/impressions", buildJsonObject {
            put("event_id", eventId); put("decision_id", binding.decisionId); put("delivery_id", binding.deliveryId)
            put("page_index", binding.pageIndex); put("position", binding.position); put("exposure_token", binding.exposureToken)
            put("candidate_id", binding.candidateId)
            put("source_revision", binding.sourceRevision?.let(::JsonPrimitive) ?: JsonNull)
            put("surface", binding.surface); put("visible_ms", bounded)
            put("visibility_rule_version", binding.visibilityRuleVersion)
            put("client_type", "android"); put("client_version", "development"); put("viewport_class", "compact")
        }, AttentionImpressionReceipt.serializer(), "record impression")
        if (!response.verified || response.eventId != eventId || response.decisionId != binding.decisionId ||
            response.deliveryId != binding.deliveryId || response.pageIndex != binding.pageIndex ||
            response.position != binding.position || response.exposureToken != binding.exposureToken ||
            response.candidateId != binding.candidateId || response.sourceRevision != binding.sourceRevision ||
            response.surface != binding.surface || response.visibilityRuleVersion != binding.visibilityRuleVersion ||
            response.rootPolicyPropensity != binding.rootPolicyPropensity ||
            response.conditionalDeliveryPropensity != 1.0 || response.accumulatedVisibleMs < bounded
        ) throw TodayApiError("The impression receipt failed validation.")
        impressionReceipts[binding.identity] = response
        return response
    }

    override suspend fun pulse(previous: TodayPulse?): TodayPulse = coroutineScope {
        val zone = ZoneId.systemDefault()
        val today = LocalDate.now(zone).atStartOfDay(zone).toInstant().toEpochMilli()
        val yesterday = LocalDate.now(zone).minusDays(1).atStartOfDay(zone).toInstant().toEpochMilli()
        val tomorrow = LocalDate.now(zone).plusDays(1).atStartOfDay(zone).toInstant().toEpochMilli()
        val llm = async { captured { query("/analytics/llm_calls/query", llmSql(yesterday, today, tomorrow)) } }
        val coding = async { captured { query("/analytics/query", codingSql(today, tomorrow)) } }
        val memory = async { captured { query("/analytics/memory_events/query", memorySql(today, tomorrow)) } }
        val tasks = async { captured { client.get("${v3()}/tasks") { authorize() }.json("load completed tasks") } }
        val llmResult = llm.await(); val codingResult = coding.await(); val memoryResult = memory.await(); val taskResult = tasks.await()
        var result = previous ?: TodayPulse()
        var successful = 0
        llmResult.getOrNull()?.let { result = applyLlmPulse(result, it); successful++ }
        codingResult.getOrNull()?.let { result = result.copy(codingRunsToday = cell(it, 0, "n").toInt()); successful++ }
        memoryResult.getOrNull()?.let { result = applyMemoryPulse(result, it); successful++ }
        taskResult.getOrNull()?.let { root ->
            val rows = when (root) {
                is JsonArray -> root
                is JsonObject -> root["tasks"] as? JsonArray ?: JsonArray(emptyList())
                else -> JsonArray(emptyList())
            }
            val buckets = taskBuckets(rows)
            var completedToday = 0; var completedYesterday = 0
            rows.forEach { element ->
                val row = element as? JsonObject ?: return@forEach
                if (row["status"].text() !in setOf("completed", "done")) return@forEach
                val updated = row["updated_at"].text()?.let { runCatching { Instant.parse(it).toEpochMilli() }.getOrNull() } ?: return@forEach
                if (updated in today until tomorrow) completedToday++ else if (updated in yesterday until today) completedYesterday++
            }
            result = result.copy(
                tasksCompletedToday = completedToday, tasksCompletedYesterday = completedYesterday, taskBuckets = buckets,
                recentTasks = recentTaskLines(rows),
            )
        }
        if (successful == 0) throw TodayApiError("Today's pulse is temporarily unavailable.")
        result
    }

    override fun events(): Flow<TodayRealtimeFrame> = flow {
        var attempt = 0
        while (currentCoroutineContext().isActive) {
            try {
                client.webSocket(urlString = realtimeUrl(), request = {
                    MagicianAccess.headers(context).forEach { (name, value) -> header(name, value) }
                }) {
                    attempt = 0
                    for (frame in incoming) {
                        val raw = (frame as? Frame.Text)?.readText() ?: continue
                        val principal = MagicianAccess.principal(context)
                        val workspace = MagicianAccess.workspace(context)
                        if (isScopedTodayEvent(raw, principal, workspace)) {
                            emit(TodayRealtimeFrame(raw, isRelevantTodayEvent(raw, principal, workspace)))
                        }
                    }
                }
                delay(Random.nextLong(750, 1_251)); attempt = 1
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Throwable) {
                val cap = min(30_000L, 1_000L shl attempt.coerceAtMost(5))
                delay(Random.nextLong(750L, cap + 1)); attempt++
            }
        }
    }

    private fun feedbackAttribution(item: ChannelFollowUp): AttentionFeedbackAttribution? =
        deliveryAttribution(item.deliveryBinding, item.deliveryBinding?.let { impressionReceipts[it.identity] })
            ?: validatedLegacyAttribution(item.decisionItem, item.candidateId, item.sourceRevision, "follow_up")

    private fun feedbackAttribution(card: ResurfacingCard): AttentionFeedbackAttribution? =
        deliveryAttribution(card.deliveryBinding, card.deliveryBinding?.let { impressionReceipts[it.identity] })
            ?: validatedLegacyAttribution(card.decisionItem, card.candidateId, card.sourceRevision, "worth_a_look")

    private suspend fun postFeedback(url: String, body: JsonObject, action: String): AttentionFeedbackReceipt? =
        postDecode(url, body, AttentionFeedbackEnvelope.serializer(), action).feedbackReceipt

    private suspend fun post(url: String, body: JsonObject, action: String) {
        val response = client.post(url) { authorize(); contentType(ContentType.Application.Json); setBody(body.toString()) }
        response.successText(action)
    }

    private suspend fun <T> postDecode(url: String, body: JsonObject, serializer: KSerializer<T>, action: String): T {
        val response = client.post(url) { authorize(); contentType(ContentType.Application.Json); setBody(body.toString()) }
        return response.decode(serializer, action)
    }

    private suspend fun query(path: String, sql: String): PulseQueryResponse {
        val response = client.post(v2() + path) {
            authorize()
            contentType(ContentType.Application.Json); setBody(buildJsonObject { put("sql", sql) }.toString())
        }
        val root = response.json("load pulse") as? JsonObject ?: JsonObject(emptyMap())
        return PulseQueryResponse(
            columns = (root["columns"] as? JsonArray).orEmpty().mapNotNull { it.text() },
            rows = (root["rows"] as? JsonArray).orEmpty().mapNotNull { it as? JsonArray },
        )
    }

    private suspend fun <T> captured(block: suspend () -> T): Result<T> = try {
        Result.success(block())
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (error: Throwable) {
        Result.failure(error)
    }

    private suspend fun <T> HttpResponse.decode(serializer: KSerializer<T>, action: String): T =
        todayJson.decodeFromString(serializer, successText(action))

    private suspend fun HttpResponse.json(action: String): JsonElement = todayJson.parseToJsonElement(successText(action))

    private suspend fun HttpResponse.successText(action: String): String {
        val body = bodyAsText()
        if (status.isSuccess()) return body
        val message = runCatching {
            val root = todayJson.parseToJsonElement(body).jsonObject
            (root["error"] ?: root["message"] ?: root["detail"])?.jsonPrimitive?.contentOrNull
        }.getOrNull()?.takeIf(String::isNotBlank)
        throw TodayApiError(message ?: "Could not $action (HTTP ${status.value}).", status.value)
    }

    private fun segment(value: String): String = URLEncoder.encode(value, StandardCharsets.UTF_8.name()).replace("+", "%20")

    private fun realtimeUrl(): String {
        val base = host()
        val socket = when {
            base.startsWith("https://") -> "wss://${base.removePrefix("https://")}"
            base.startsWith("http://") -> "ws://${base.removePrefix("http://")}"
            else -> "wss://$base"
        }
        return "$socket/api/magician/v2/realtime/ws"
    }
}

internal data class PulseQueryResponse(val columns: List<String>, val rows: List<JsonArray>)

/** Frames for another principal or workspace never reach this device's Today. */
internal fun isScopedTodayEvent(raw: String, principal: String, workspace: String): Boolean {
    val root = runCatching { todayJson.parseToJsonElement(raw).jsonObject }.getOrNull() ?: return false
    val data = root["data"] as? JsonObject
    if (data?.get("principal").text()?.let { it != principal } == true) return false
    if (data?.get("workspace").text()?.let { it != workspace } == true) return false
    return true
}

internal fun isRelevantTodayEvent(raw: String, principal: String, workspace: String): Boolean {
    if (!isScopedTodayEvent(raw, principal, workspace)) return false
    val root = todayJson.parseToJsonElement(raw).jsonObject
    val type = (root["event_type"].text() ?: root["type"].text()).orEmpty().lowercase()
    return listOf("today", "feed", "task", "planning", "execution", "learning", "published", "surface",
        "attention", "approval", "channel", "follow_up", "resurfacing").any(type::contains)
}

internal fun applyAttentionDelivery(
    bindings: List<AttentionDeliveryBinding>, followUps: List<ChannelFollowUp>,
): List<ChannelFollowUp>? {
    val byId = followUps.associateBy(ChannelFollowUp::id)
    if (byId.size != followUps.size || bindings.any {
        it.surface != "follow_up" || it.originKind != "follow_up" ||
            it.rawItemId !in byId || byId.getValue(it.rawItemId).sourceRevision != it.sourceRevision
    }) return null
    val bound = bindings.map(AttentionDeliveryBinding::rawItemId).toSet()
    return bindings.sortedBy(AttentionDeliveryBinding::position).map { binding ->
        byId.getValue(binding.rawItemId).copy(deliveryBinding = binding)
    } + followUps.filterNot { it.id in bound }
}

internal fun applyWorthAttentionDelivery(
    bindings: List<AttentionDeliveryBinding>, cards: List<ResurfacingCard>,
): List<ResurfacingCard>? {
    val byId = cards.associateBy(ResurfacingCard::id)
    if (byId.size != cards.size || bindings.any {
        it.surface != "worth_a_look" || it.originKind != "worth_a_look" ||
            it.rawItemId !in byId || byId.getValue(it.rawItemId).sourceRevision != it.sourceRevision
    }) return null
    val bound = bindings.map(AttentionDeliveryBinding::rawItemId).toSet()
    return bindings.sortedBy(AttentionDeliveryBinding::position).map { binding ->
        byId.getValue(binding.rawItemId).copy(deliveryBinding = binding)
    } + cards.filterNot { it.id in bound }
}

private fun cell(response: PulseQueryResponse, row: Int, column: String): Double {
    val index = response.columns.indexOf(column)
    if (row !in response.rows.indices || index !in response.rows[row].indices) return 0.0
    val value = response.rows[row][index]
    return (value as? JsonPrimitive)?.doubleOrNull ?: value.text()?.toDoubleOrNull() ?: 0.0
}

private fun cellText(response: PulseQueryResponse, row: Int, column: String): String {
    val index = response.columns.indexOf(column)
    return response.rows.getOrNull(row)?.getOrNull(index)?.text().orEmpty()
}

private fun applyLlmPulse(current: TodayPulse, response: PulseQueryResponse): TodayPulse {
    val hourly = MutableList(24) { 0.0 }
    val hourlyCalls = MutableList(24) { 0 }
    var spendToday = 0.0; var spendYesterday = 0.0; var callsToday = 0; var callsYesterday = 0
    val providers = mutableListOf<Triple<String, String, Double>>()
    response.rows.indices.forEach { row ->
        when (cellText(response, row, "section")) {
            "today_hour" -> cellText(response, row, "k").toIntOrNull()?.takeIf { it in 0..23 }?.let {
                hourly[it] += cell(response, row, "v1"); hourlyCalls[it] += cell(response, row, "v2").toInt()
            }
            "today_total" -> { spendToday = cell(response, row, "v1"); callsToday = cell(response, row, "v2").toInt() }
            "yesterday_total" -> { spendYesterday = cell(response, row, "v1"); callsYesterday = cell(response, row, "v2").toInt() }
            "today_provider" -> providers += Triple(cellText(response, row, "k"), cellText(response, row, "model"), cell(response, row, "v2"))
        }
    }
    val providerTotal = providers.sumOf(Triple<String, String, Double>::third)
    val top = providers.maxByOrNull(Triple<String, String, Double>::third)?.takeIf { providerTotal > 0 }
    return current.copy(
        spendToday = spendToday, spendYesterday = spendYesterday, callsToday = callsToday, callsYesterday = callsYesterday,
        hourlySpend = hourly, hourlyCalls = hourlyCalls, topModel = top?.let { TodayPulseTopModel(it.first, it.second, it.third / providerTotal) },
    )
}

private fun applyMemoryPulse(current: TodayPulse, response: PulseQueryResponse): TodayPulse {
    var memories = 0; var evals = 0; var passes = 0
    response.rows.indices.forEach { row -> when (cellText(response, row, "section")) {
        "memories_today" -> memories = cell(response, row, "n").toInt()
        "evals_today" -> { evals = cell(response, row, "n").toInt(); passes = cell(response, row, "passes").toInt() }
    } }
    return current.copy(memoriesToday = memories, evalCasesToday = evals, evalPassesToday = passes)
}

private fun llmSql(yesterday: Long, today: Long, tomorrow: Long): String =
    "SELECT 'today_hour' AS section, CAST(CAST(FLOOR((timestamp_ms - $today) / 3600000.0) AS INTEGER) AS VARCHAR) AS k, NULL::VARCHAR AS model, COALESCE(SUM(cost_usd), 0) AS v1, COUNT(*) AS v2 FROM llm_calls WHERE timestamp_ms >= $today AND timestamp_ms < $tomorrow GROUP BY 2 UNION ALL SELECT 'today_total', 'all', NULL::VARCHAR, COALESCE(SUM(cost_usd), 0), COUNT(*) FROM llm_calls WHERE timestamp_ms >= $today AND timestamp_ms < $tomorrow UNION ALL SELECT 'yesterday_total', 'all', NULL::VARCHAR, COALESCE(SUM(cost_usd), 0), COUNT(*) FROM llm_calls WHERE timestamp_ms >= $yesterday AND timestamp_ms < $today UNION ALL SELECT 'today_provider', COALESCE(NULLIF(provider, ''), 'unknown'), COALESCE(NULLIF(model, ''), 'unknown'), COALESCE(SUM(cost_usd), 0), COUNT(*) FROM llm_calls WHERE timestamp_ms >= $today AND timestamp_ms < $tomorrow GROUP BY 2, 3"

private fun codingSql(today: Long, tomorrow: Long): String =
    "SELECT 'coding_runs_today' AS section, COUNT(*) AS n FROM events WHERE event_type = 'coding.started' AND source = 'coding_engine' AND epoch_ms(timestamp) >= $today AND epoch_ms(timestamp) < $tomorrow"

private fun memorySql(today: Long, tomorrow: Long): String =
    "SELECT 'memories_today' AS section, CAST(COUNT(*) AS DOUBLE) AS n, CAST(0 AS DOUBLE) AS passes FROM memory_events WHERE timestamp_ms >= $today AND timestamp_ms < $tomorrow AND event_kind IN ('learning_memory_candidate_promoted', 'learning_memory_candidate_review_promoted') UNION ALL SELECT 'evals_today', CAST(COUNT(*) AS DOUBLE), CAST(COALESCE(SUM(CASE WHEN eval_pass THEN 1 ELSE 0 END), 0) AS DOUBLE) FROM memory_events WHERE timestamp_ms >= $today AND timestamp_ms < $tomorrow AND event_kind = 'eval_case'"
