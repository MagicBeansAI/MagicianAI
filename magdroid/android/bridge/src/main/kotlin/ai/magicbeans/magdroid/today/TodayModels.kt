package ai.magicbeans.magdroid.today

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.longOrNull
import java.net.URLDecoder
import java.nio.charset.StandardCharsets
import java.time.DayOfWeek
import java.time.ZonedDateTime
import java.time.temporal.ChronoUnit
import java.util.UUID

internal fun JsonElement?.text(): String? = (this as? JsonPrimitive)?.contentOrNull
internal fun JsonElement?.obj(): JsonObject? = this as? JsonObject
internal fun JsonElement?.array(): JsonArray? = this as? JsonArray
internal val TODAY_ACTION_ENDPOINT = Regex("^/api/magician/v2/today/items/[^/?#]+/actions/[^/?#]+$")

enum class TodaySection(val wire: String, val title: String, val summary: String) {
    NeedsYou("needs_you", "Needs You", "Decisions, approvals, failures, and blocked work."),
    FollowUps("followups", "Follow-ups", "Due, blocked, or stale work that needs a next action."),
    WorthALook("worth_a_look", "Worth a look", "Memory, tasks, and messages that may be useful again."),
    ActiveWork("active_work", "Active Work", "Current running work, shown as status instead of logs."),
    Delivered("delivered", "Delivered", "Finished work with something useful to open."),
    Changed("changed", "Changed", "Durable memory and state changes worth knowing."),
    ;

    companion object {
        fun fromWire(value: String): TodaySection = entries.firstOrNull { it.wire == value } ?: Changed
    }
}

@Serializable
data class TodayAction(
    val id: String = "",
    val label: String = "Action",
    @SerialName("action_type") val actionType: String? = null,
    val payload: JsonElement = JsonNull,
) {
    val executionEndpoint: String?
        get() {
            val record = payload.obj() ?: return null
            if ((record["method"].text() ?: "POST").uppercase() != "POST") return null
            return record["endpoint"].text()?.trim()?.takeIf { endpoint ->
                TODAY_ACTION_ENDPOINT.matches(endpoint)
            }
        }

    val isExecutableTodayAction: Boolean
        get() = actionType == "today_source_action" && executionEndpoint != null

}

data class TodayLearnedItem(
    val id: String,
    val title: String,
    val summary: String? = null,
    val updatedAt: Long? = null,
)

@Serializable
data class TodayItem(
    val id: String = "",
    val section: String = TodaySection.Changed.wire,
    val priority: Int = 0,
    val title: String = "Today item",
    val summary: String? = null,
    val reason: String = "",
    @SerialName("source_kind") val sourceKind: String = "unknown",
    @SerialName("source_id") val sourceId: String = "",
    @SerialName("source_url") val sourceUrl: String? = null,
    @SerialName("space_ids") val spaceIds: List<String> = emptyList(),
    @SerialName("thread_id") val threadId: String? = null,
    @SerialName("task_id") val taskId: String? = null,
    @SerialName("agent_id") val agentId: String? = null,
    val status: String = "info",
    val actions: List<TodayAction> = emptyList(),
    @SerialName("evidence_refs") val evidenceRefs: List<JsonElement> = emptyList(),
    val metadata: JsonElement = JsonNull,
    @SerialName("created_at") val createdAt: Long = 0,
    @SerialName("updated_at") val updatedAt: Long = 0,
    @SerialName("expires_at") val expiresAt: Long? = null,
    @SerialName("seen_at") val seenAt: Long? = null,
    @SerialName("dismissed_at") val dismissedAt: Long? = null,
    @SerialName("snoozed_until") val snoozedUntil: Long? = null,
) {
    val executableActions: List<TodayAction> get() = actions.filter(TodayAction::isExecutableTodayAction)

    val learnedItems: List<TodayLearnedItem>
        get() = metadata.obj()?.get("learned_items").array().orEmpty().mapIndexedNotNull { index, value ->
            val record = value.obj() ?: return@mapIndexedNotNull null
            val title = record["title"].text()?.trim().orEmpty()
            val summary = record["summary"].text()?.trim().orEmpty()
            if (title.isEmpty() && summary.isEmpty()) return@mapIndexedNotNull null
            TodayLearnedItem(
                id = record["id"].text() ?: "$id:$index",
                title = title.ifEmpty { summary },
                summary = summary.takeIf { it.isNotEmpty() && it != title },
                updatedAt = (record["updated_at"] as? JsonPrimitive)?.longOrNull,
            )
        }.take(4)

    val attentionItemId: String?
        get() {
            val queryValue = sourceUrl?.substringAfter('?', "")?.split('&')?.firstNotNullOfOrNull { pair ->
                val key = pair.substringBefore('=')
                pair.substringAfter('=', "").takeIf {
                    key in setOf("selected", "selected_item", "item_id") && it.isNotEmpty()
                }
            }
            if (!queryValue.isNullOrBlank()) return runCatching {
                URLDecoder.decode(queryValue, StandardCharsets.UTF_8.name())
            }.getOrDefault(queryValue)
            val record = metadata.obj()
            val keys = listOf("pause_state_id", "approval_id", "correlation_id", "request_id", "feed_item_id", "dedupe_key", "dedupe_id", "dedupe")
            keys.firstNotNullOfOrNull { record?.get(it).text()?.takeIf(String::isNotBlank) }?.let { return it }
            val identifiers = record?.get("hitl_request").obj()?.get("identifiers").obj()
            keys.firstNotNullOfOrNull { identifiers?.get(it).text()?.takeIf(String::isNotBlank) }?.let { return it }
            val prefix = "today:$section:"
            if (id.startsWith(prefix)) return id.removePrefix(prefix)
            if (listOf("v3:attention:", "skill_evolution_approval:", "skill_evolution_rollback:", "skill_evolution_post_promotion:").any(sourceId::startsWith)) return sourceId
            return null
        }

    val isMeetingAction: Boolean
        get() = sourceKind == "meeting_action" || metadata.obj()?.get("followup_kind").text() == "meeting_action_item"

    val detailMarkdown: String?
        get() {
            val record = metadata.obj()
            record?.get("detail_markdown").text()?.trim()?.takeIf(String::isNotEmpty)?.let { return it }
            return listOf(
                record?.get("meeting_summary").text(),
                record?.get("description").text(),
                record?.get("action_item").text(),
                summary,
            ).mapNotNull { it?.trim()?.takeIf(String::isNotEmpty) }.distinct().joinToString("\n\n").ifBlank { null }
        }

    val monitorTarget: TodayMonitorTarget?
        get() {
            val record = metadata.obj()
            val isMonitor = sourceKind == "monitor_update" || record?.get("update_kind").text() == "monitor_update"
            if (isMonitor) {
                record?.get("monitor_task_id").text()?.takeIf(String::isNotBlank)?.let { monitorId ->
                    return TodayMonitorTarget(monitorId, record?.get("update_id").text())
                }
                taskId?.takeIf(String::isNotBlank)?.let { return TodayMonitorTarget(it, sourceId) }
            }
            return parseMonitorTasksRoute(sourceUrl)
        }
}

data class TodayMonitorTarget(val taskId: String, val updateId: String? = null)

fun parseMonitorTasksRoute(raw: String?): TodayMonitorTarget? {
    val route = raw?.let { runCatching { java.net.URI(it) }.getOrNull() } ?: return null
    if (route.path?.trimEnd('/')?.endsWith("/tasks") != true) return null
    val query = runCatching {
        route.rawQuery.orEmpty().split('&').filter(String::isNotBlank).associate { pair ->
            val parts = pair.split('=', limit = 2)
            URLDecoder.decode(parts[0], StandardCharsets.UTF_8.name()) to
                URLDecoder.decode(parts.getOrElse(1) { "" }, StandardCharsets.UTF_8.name())
        }
    }.getOrNull() ?: return null
    if (query["type"] != "monitors") return null
    return query["selected"]?.takeIf(String::isNotBlank)?.let {
        TodayMonitorTarget(it, query["update"]?.takeIf(String::isNotBlank))
    }
}

@Serializable
data class TodayCounts(
    @SerialName("needs_you") val needsYou: Int = 0,
    val delivered: Int = 0,
    val changed: Int = 0,
    @SerialName("active_work") val activeWork: Int = 0,
    val followups: Int = 0,
    val total: Int = 0,
) {
    fun count(section: TodaySection, resurfacing: Int = 0): Int = when (section) {
        TodaySection.NeedsYou -> needsYou
        TodaySection.FollowUps -> followups
        TodaySection.WorthALook -> resurfacing
        TodaySection.ActiveWork -> activeWork
        TodaySection.Delivered -> delivered
        TodaySection.Changed -> changed
    }
}

@Serializable
data class TodaySectionsPayload(
    @SerialName("needs_you") val needsYou: List<TodayItem> = emptyList(),
    val delivered: List<TodayItem> = emptyList(),
    val changed: List<TodayItem> = emptyList(),
    @SerialName("active_work") val activeWork: List<TodayItem> = emptyList(),
    val followups: List<TodayItem> = emptyList(),
) {
    fun items(section: TodaySection): List<TodayItem> = when (section) {
        TodaySection.NeedsYou -> needsYou
        TodaySection.FollowUps -> followups
        TodaySection.ActiveWork -> activeWork
        TodaySection.Delivered -> delivered
        TodaySection.Changed -> changed
        TodaySection.WorthALook -> emptyList()
    }

    fun replacing(section: TodaySection, rows: List<TodayItem>): TodaySectionsPayload = when (section) {
        TodaySection.NeedsYou -> copy(needsYou = rows)
        TodaySection.FollowUps -> copy(followups = rows)
        TodaySection.ActiveWork -> copy(activeWork = rows)
        TodaySection.Delivered -> copy(delivered = rows)
        TodaySection.Changed -> copy(changed = rows)
        TodaySection.WorthALook -> this
    }
}

@Serializable
data class TodayDigestBullet(
    val id: String = "",
    val text: String = "",
    @SerialName("source_kind") val sourceKind: String = "unknown",
    @SerialName("source_id") val sourceId: String = "",
    @SerialName("source_url") val sourceUrl: String? = null,
    @SerialName("space_ids") val spaceIds: List<String> = emptyList(),
    @SerialName("updated_at") val updatedAt: Long = 0,
)

@Serializable
data class TodayDigest(
    @SerialName("generated_at") val generatedAt: Long = 0,
    val since: Long? = null,
    val total: Int = 0,
    val limit: Int = 0,
    val offset: Int = 0,
    val bullets: List<TodayDigestBullet> = emptyList(),
)

@Serializable
data class TodaySectionPage(
    val section: String = "",
    val total: Int = 0,
    val limit: Int = 0,
    val cursor: String? = null,
    @SerialName("next_cursor") val nextCursor: String? = null,
    @SerialName("has_more") val hasMore: Boolean = false,
)

@Serializable
data class TodayResponse(
    @SerialName("generated_at") val generatedAt: Long = 0,
    val headline: String = "",
    val digest: TodayDigest = TodayDigest(),
    val sections: TodaySectionsPayload = TodaySectionsPayload(),
    val counts: TodayCounts = TodayCounts(),
    @SerialName("section_page") val sectionPage: TodaySectionPage? = null,
)

@Serializable
data class TodayVisibilitySnapshot(
    val title: String = "",
    val summary: String? = null,
    val reason: String = "",
    val section: String = TodaySection.Changed.wire,
    @SerialName("source_kind") val sourceKind: String = "unknown",
    @SerialName("source_id") val sourceId: String = "",
    @SerialName("source_url") val sourceUrl: String? = null,
    @SerialName("space_ids") val spaceIds: List<String> = emptyList(),
    @SerialName("item_updated_at") val itemUpdatedAt: Long = 0,
)

@Serializable
data class HiddenTodayRecord(
    @SerialName("seen_at") val seenAt: Long? = null,
    @SerialName("dismissed_at") val dismissedAt: Long? = null,
    @SerialName("snoozed_until") val snoozedUntil: Long? = null,
    val snapshot: TodayVisibilitySnapshot? = null,
)

@Serializable
data class HiddenTodayItem(
    @SerialName("item_id") val itemId: String = "",
    @SerialName("hidden_kind") val hiddenKind: String = "dismissed",
    val record: HiddenTodayRecord = HiddenTodayRecord(),
) { val id: String get() = itemId }

@Serializable internal data class HiddenTodayResponse(val items: List<HiddenTodayItem> = emptyList())

@Serializable
data class AttentionDecisionBinding(
    @SerialName("decision_id") val decisionId: String = "",
    @SerialName("candidate_id") val candidateId: String = "",
    @SerialName("source_revision") val sourceRevision: String? = null,
    @SerialName("served_route") val servedRoute: String = "",
    val selected: Boolean = false,
)

@Serializable
data class AttentionFeedbackAttribution(
    @SerialName("decision_id") val decisionId: String,
    @SerialName("candidate_id") val candidateId: String,
    @SerialName("source_revision") val sourceRevision: String? = null,
    @SerialName("impression_id") val impressionId: String? = null,
    @SerialName("delivery_id") val deliveryId: String? = null,
)

@Serializable
data class CanonicalAttentionProjectionReference(
    @SerialName("projection_id") val projectionId: String,
    @SerialName("universe_digest") val universeDigest: String,
    val status: String,
)

@Serializable internal data class AttentionDeliveryRootDecision(
    @SerialName("decision_id") val decisionId: String,
    val lane: String,
    @SerialName("projection_id") val projectionId: String,
    @SerialName("universe_digest") val universeDigest: String,
    @SerialName("expires_at") val expiresAt: Long,
)

@Serializable internal data class AttentionDeliveryPageIdentity(
    @SerialName("delivery_id") val deliveryId: String,
    @SerialName("page_index") val pageIndex: Int,
    @SerialName("page_start") val pageStart: Int,
    @SerialName("page_size") val pageSize: Int,
    @SerialName("next_cursor") val nextCursor: String? = null,
    @SerialName("expires_at") val expiresAt: Long,
)

@Serializable internal data class AttentionImpressionPolicy(
    @SerialName("min_visible_ms") val minVisibleMs: Int,
    @SerialName("visibility_rule_version") val visibilityRuleVersion: String,
)

@Serializable internal data class AttentionCanonicalOrigin(
    val kind: String,
    @SerialName("annotation_id") val annotationId: String? = null,
    @SerialName("candidate_id") val candidateId: String? = null,
)

@Serializable internal data class AttentionDeliveredCanonicalItem(
    @SerialName("canonical_id") val canonicalId: String,
    @SerialName("source_revision") val sourceRevision: String? = null,
    @SerialName("served_lane") val servedLane: String,
    val origin: AttentionCanonicalOrigin,
)

@Serializable internal data class AttentionDeliveredItem(
    val position: Int,
    @SerialName("candidate_id") val candidateId: String,
    @SerialName("source_revision") val sourceRevision: String? = null,
    @SerialName("root_policy_propensity") val rootPolicyPropensity: Double,
    @SerialName("conditional_delivery_propensity") val conditionalDeliveryPropensity: Double,
    @SerialName("exposure_token") val exposureToken: String,
    val item: AttentionDeliveredCanonicalItem,
)

@Serializable internal data class AttentionDeliveryPageResponse(
    @SerialName("schema_version") val schemaVersion: Int,
    @SerialName("root_decision") val rootDecision: AttentionDeliveryRootDecision,
    val page: AttentionDeliveryPageIdentity,
    val items: List<AttentionDeliveredItem>,
    @SerialName("impression_policy") val impressionPolicy: AttentionImpressionPolicy,
)

data class AttentionDeliveryBinding(
    val principal: String,
    val workspace: String,
    val rawItemId: String,
    val originKind: String,
    val decisionId: String,
    val deliveryId: String,
    val pageIndex: Int,
    val position: Int,
    val exposureToken: String,
    val candidateId: String,
    val sourceRevision: String?,
    val surface: String,
    val minVisibleMs: Int,
    val visibilityRuleVersion: String,
    val rootPolicyPropensity: Double,
    val expiresAt: Long,
) {
    val identity: String get() = listOf(
        principal, workspace, decisionId, deliveryId, pageIndex.toString(), position.toString(),
        candidateId, sourceRevision.orEmpty(), exposureToken, visibilityRuleVersion,
    ).joinToString("\u0000")
}

internal fun AttentionDeliveryPageResponse.validatedBindings(
    reference: CanonicalAttentionProjectionReference,
    surface: String,
    principal: String,
    workspace: String,
    nowMs: Long = System.currentTimeMillis(),
): List<AttentionDeliveryBinding>? {
    if (schemaVersion != 1 || rootDecision.lane != surface ||
        rootDecision.projectionId != reference.projectionId ||
        rootDecision.universeDigest != reference.universeDigest ||
        rootDecision.expiresAt != page.expiresAt || rootDecision.expiresAt <= nowMs ||
        rootDecision.decisionId.isBlank() || page.deliveryId.isBlank() || page.pageIndex < 0 ||
        page.pageStart < 0 || page.pageSize < 1 || items.size > page.pageSize ||
        impressionPolicy.minVisibleMs !in 1..60_000 || impressionPolicy.visibilityRuleVersion.isBlank()
    ) return null
    val rawIds = mutableSetOf<String>()
    val candidateIds = mutableSetOf<String>()
    val exposureTokens = mutableSetOf<String>()
    return items.mapIndexed { offset, delivered ->
        val rawId = when (delivered.item.origin.kind) {
            "follow_up" -> delivered.item.origin.annotationId
            "worth_a_look" -> delivered.item.origin.candidateId
            else -> null
        }
        if (delivered.position != page.pageStart + offset + 1 ||
            delivered.candidateId != delivered.item.canonicalId ||
            delivered.sourceRevision != delivered.item.sourceRevision ||
            delivered.item.servedLane != surface || delivered.rootPolicyPropensity !in 0.0..1.0 ||
            delivered.conditionalDeliveryPropensity != 1.0 || delivered.exposureToken.isBlank() ||
            rawId.isNullOrBlank() || !rawIds.add(rawId) || !candidateIds.add(delivered.candidateId) ||
            !exposureTokens.add(delivered.exposureToken)
        ) return null
        AttentionDeliveryBinding(
            principal, workspace, rawId, delivered.item.origin.kind, rootDecision.decisionId,
            page.deliveryId, page.pageIndex, delivered.position, delivered.exposureToken,
            delivered.candidateId, delivered.sourceRevision, surface, impressionPolicy.minVisibleMs,
            impressionPolicy.visibilityRuleVersion, delivered.rootPolicyPropensity, rootDecision.expiresAt,
        )
    }
}

@Serializable
data class AttentionImpressionReceipt(
    @SerialName("impression_id") val impressionId: String,
    @SerialName("event_id") val eventId: String,
    @SerialName("decision_id") val decisionId: String,
    @SerialName("delivery_id") val deliveryId: String,
    @SerialName("page_index") val pageIndex: Int,
    val position: Int,
    @SerialName("exposure_token") val exposureToken: String,
    @SerialName("candidate_id") val candidateId: String,
    @SerialName("source_revision") val sourceRevision: String? = null,
    val surface: String,
    @SerialName("accumulated_visible_ms") val accumulatedVisibleMs: Int,
    @SerialName("min_visible_ms") val minVisibleMs: Int,
    @SerialName("visibility_rule_version") val visibilityRuleVersion: String,
    @SerialName("root_policy_propensity") val rootPolicyPropensity: Double,
    @SerialName("conditional_delivery_propensity") val conditionalDeliveryPropensity: Double,
    val verified: Boolean,
    val deduplicated: Boolean = false,
)

@Serializable
data class ResurfacingChangeFact(
    val aspect: String = "",
    val before: String? = null,
    val after: String? = null,
    @SerialName("effective_text") val effectiveText: String? = null,
)

@Serializable
data class ResurfacingTemporalFact(
    val kind: String = "",
    val text: String = "",
    @SerialName("at_ms") val atMs: Long? = null,
    val timezone: String? = null,
)

@Serializable
data class ResurfacingBrief(
    @SerialName("schema_version") val schemaVersion: Int = 0,
    @SerialName("key_facts") val keyFacts: List<String> = emptyList(),
    val changes: List<ResurfacingChangeFact> = emptyList(),
    @SerialName("temporal_facts") val temporalFacts: List<ResurfacingTemporalFact> = emptyList(),
    @SerialName("detail_status") val detailStatus: String = "",
    @SerialName("missing_details") val missingDetails: List<String> = emptyList(),
)

enum class ResurfacingActionKind(val wire: String) {
    ViewDetails("view_details"), OpenSource("open_source"), ShowOriginal("show_original"),
    AskPresto("ask_presto"), CreateTask("create_task"), CreateReminder("create_reminder"),
    Share("share"), SaveToMemory("save_to_memory"), SummarizeDeeper("summarize_deeper"),
    ;
    companion object { fun fromWire(value: String) = entries.firstOrNull { it.wire == value } }
}

enum class ResurfacingFeedbackAction(val wire: String) { Open("open"), Acknowledge("acknowledge"), Dismiss("dismiss") }

@Serializable
data class ResurfacingActionCapability(
    val kind: String,
    val label: String,
    @SerialName("requires_input") val requiresInput: Boolean = false,
    @SerialName("side_effect") val sideEffect: String = "none",
)

@Serializable
data class ResurfacingRecommendation(
    val kind: String,
    val label: String,
    val rationale: String = "",
    val confidence: Double = 0.0,
    @SerialName("content_revision") val contentRevision: String? = null,
    val source: String = "",
)

@Serializable
data class ResurfacingCard(
    @SerialName("candidate_id") val candidateId: String,
    @SerialName("source_revision") val sourceRevision: String? = null,
    val line: String = "",
    @SerialName("why_now") val whyNow: String = "",
    @SerialName("source_title") val sourceTitle: String = "",
    val summary: String = "",
    @SerialName("source_kind") val sourceKind: String = "unknown",
    @SerialName("source_ref") val sourceRef: String = "",
    @SerialName("detail_label") val detailLabel: String = "Details",
    @SerialName("temporal_anchor_at") val temporalAnchorAt: Long? = null,
    val brief: ResurfacingBrief? = null,
    @SerialName("brief_status") val briefStatus: String = "legacy",
    @SerialName("content_revision") val contentRevision: String? = null,
    @SerialName("source_updated") val sourceUpdated: Boolean = false,
    @SerialName("recommended_action") val recommendedAction: ResurfacingRecommendation? = null,
    val actions: List<ResurfacingActionCapability> = emptyList(),
    @SerialName("decision_item") val decisionItem: AttentionDecisionBinding? = null,
    @SerialName("source_route") val sourceRoute: String? = null,
    @SerialName("open_url") val openUrl: String? = null,
    @kotlinx.serialization.Transient val deliveryBinding: AttentionDeliveryBinding? = null,
) { val id: String get() = candidateId }

@Serializable data class ResurfacingCursor(
    @SerialName("surfaced_at") val surfacedAt: Long,
    val score: Double,
    @SerialName("candidate_id") val candidateId: String,
)

@Serializable data class ResurfacingPage(
    val cards: List<ResurfacingCard> = emptyList(),
    val total: Int = 0,
    val limit: Int? = null,
    val offset: Int? = null,
    @SerialName("has_more") val hasMore: Boolean? = null,
    @SerialName("next_cursor") val nextCursor: ResurfacingCursor? = null,
    @SerialName("canonical_attention_projection_ref") val canonicalProjectionReference: CanonicalAttentionProjectionReference? = null,
)

@Serializable data class ResurfacingDetail(
    @SerialName("candidate_id") val candidateId: String,
    @SerialName("source_kind") val sourceKind: String = "unknown",
    val status: String = "",
    val title: String? = null,
    val summary: String? = null,
    val brief: ResurfacingBrief? = null,
    @SerialName("content_revision") val contentRevision: String? = null,
    @SerialName("source_revision") val sourceRevision: String? = null,
    @SerialName("source_updated") val sourceUpdated: Boolean = false,
    @SerialName("has_newer") val hasNewer: Boolean = false,
    @SerialName("source_route") val sourceRoute: String? = null,
    @SerialName("open_url") val openUrl: String? = null,
    val source: JsonElement? = null,
    @SerialName("recommended_action") val recommendedAction: ResurfacingRecommendation? = null,
    val actions: List<ResurfacingActionCapability> = emptyList(),
    val original: JsonElement? = null,
    @SerialName("temporal_anchor_at") val temporalAnchorAt: Long? = null,
)

@Serializable data class ResurfacingActionResult(
    @SerialName("candidate_id") val candidateId: String? = null,
    val action: String? = null,
    @SerialName("result_ref") val resultRef: String? = null,
    val replayed: Boolean? = null,
    val result: JsonElement? = null,
)

@Serializable
data class ChannelActionDescriptor(
    val id: String,
    val label: String,
    @SerialName("needs_compose") val needsCompose: Boolean = false,
    val confirm: Boolean = false,
    val icon: String? = null,
)

@Serializable data class ChannelActionDraft(
    @SerialName("compose_id") val composeId: String,
    val text: String,
)

@Serializable
data class ChannelFollowUp(
    @SerialName("candidate_id") val candidateId: String = "",
    @SerialName("source_revision") val sourceRevision: String? = null,
    @SerialName("annotation_id") val annotationId: String,
    val provider: String = "unknown",
    @SerialName("account_alias") val accountAlias: String = "",
    @SerialName("account_email") val accountEmail: String? = null,
    @SerialName("thread_id") val threadId: String = "",
    val lane: String = "user_assist",
    val label: String? = null,
    val reason: String? = null,
    val confidence: Double? = null,
    @SerialName("proposed_action") val proposedAction: JsonElement = JsonNull,
    val subject: String? = null,
    val sender: String? = null,
    val summary: String? = null,
    @SerialName("received_at") val receivedAt: Long? = null,
    @SerialName("created_at") val createdAt: Long = 0,
    @SerialName("evidence_message_id") val evidenceMessageId: String? = null,
    @SerialName("evidence_message_at") val evidenceMessageAt: Long? = null,
    @SerialName("open_url") val openUrl: String? = null,
    val state: String? = null,
    @SerialName("review_required") val reviewRequired: Boolean = false,
    @SerialName("source_family") val sourceFamily: String? = null,
    @SerialName("available_actions") val availableActions: List<ChannelActionDescriptor> = emptyList(),
    @SerialName("decision_item") val decisionItem: AttentionDecisionBinding? = null,
    @kotlinx.serialization.Transient val deliveryBinding: AttentionDeliveryBinding? = null,
) {
    val id: String get() = annotationId
    val canAcknowledge: Boolean get() = !reviewRequired
    val actionSummary: String?
        get() {
            val record = proposedAction.obj() ?: return null
            val parts = mutableListOf<String>()
            record["follow_up_kind"].text()?.takeIf(String::isNotBlank)?.let { parts += titleCase(it) }
            record["action_owner"].text()?.takeIf { it.isNotBlank() && it != "unknown" }?.let { parts += "Owner: ${titleCase(it)}" }
            record["due_text"].text()?.takeIf(String::isNotBlank)?.let { parts += "Due: $it" }
            record["urgency"].text()?.takeIf { it.isNotBlank() && it != "normal" }?.let { parts += titleCase(it) }
            record["key_details"].array()?.mapNotNull { it.text() }?.take(3)?.takeIf(List<String>::isNotEmpty)?.let {
                parts += "Details: ${it.joinToString(" · ")}"
            }
            return parts.joinToString(" · ").ifBlank { null }
        }
}

@Serializable data class ChannelFollowUpPage(
    val items: List<ChannelFollowUp> = emptyList(),
    val total: Int = 0,
    val limit: Int? = null,
    val cursor: String? = null,
    @SerialName("next_cursor") val nextCursor: String? = null,
    @SerialName("has_more") val hasMore: Boolean? = null,
    @SerialName("canonical_attention_projection_ref") val canonicalProjectionReference: CanonicalAttentionProjectionReference? = null,
)

@Serializable data class ChannelEvidenceMessage(
    @SerialName("message_id") val messageId: String? = null,
    val body: String? = null,
    val summary: String? = null,
    val subject: String? = null,
    @SerialName("received_at") val receivedAt: Long? = null,
)

@Serializable data class ChannelMessageView(
    val body: String? = null,
    val summary: String? = null,
    val subject: String? = null,
    @SerialName("has_newer") val hasNewer: Boolean = false,
    @SerialName("evidence_messages") val evidenceMessages: List<ChannelEvidenceMessage> = emptyList(),
)

@Serializable data class ChannelWritingPreference(
    val id: String,
    @SerialName("scope_kind") val scopeKind: String,
    @SerialName("scope_value") val scopeValue: String,
    val statement: String,
    val status: String,
    @SerialName("evidence_count") val evidenceCount: Int = 0,
)

@Serializable internal data class ChannelWritingPreferencesResponse(val items: List<ChannelWritingPreference> = emptyList())

data class ChannelDismissOption(val code: String?, val label: String) {
    companion object {
        val all = listOf(
            ChannelDismissOption(null, "No reason"), ChannelDismissOption("spam", "Spam / junk"),
            ChannelDismissOption("already_handled", "Already taken care of"),
            ChannelDismissOption("duplicate", "Duplicate request"),
            ChannelDismissOption("delegated", "Someone else handles this"),
            ChannelDismissOption("not_relevant", "Not relevant to me"),
            ChannelDismissOption("wrong_classification", "Shouldn't have been flagged"),
        )

        /** Resurfacing accepts only these reasons; it has no classifier to correct. */
        val resurfacing = listOf(
            ChannelDismissOption(null, "No reason"), ChannelDismissOption("spam", "Spam / junk"),
            ChannelDismissOption("already_handled", "Already taken care of"),
            ChannelDismissOption("duplicate", "Duplicate request"),
            ChannelDismissOption("delegated", "Someone else handles this"),
            ChannelDismissOption("not_relevant", "Not relevant to me"),
        )
        val resurfacingCodes: Set<String> = resurfacing.mapNotNull(ChannelDismissOption::code).toSet()
    }
}

@Serializable
data class TodayActivityItem(
    val id: String,
    @SerialName("item_type") val itemType: String = "unknown",
    @SerialName("task_id") val taskId: String? = null,
    @SerialName("ui_thread_id") val threadId: String? = null,
    @SerialName("agent_id") val agentId: String? = null,
    val title: String = "Activity",
    val summary: String? = null,
    val status: String = "info",
    @SerialName("updated_at") val updatedAt: Long = 0,
    @SerialName("created_at") val createdAt: Long = 0,
    val actions: List<TodayAction> = emptyList(),
    val metadata: JsonElement = JsonNull,
) {
    fun metadataString(key: String): String? = metadata.obj()?.get(key).text()?.trim()?.takeIf(String::isNotEmpty)
    val briefingSurfaceId: String? get() = metadataString("surface_id")
    val route: String? get() = metadataString("route")
}

@Serializable internal data class TodayActivityResponse(val items: List<TodayActivityItem> = emptyList())

@Serializable data class TodayBriefingSurface(
    @SerialName("surface_id") val surfaceId: String,
    val route: String,
    val title: String,
    val summary: String? = null,
    @SerialName("task_id") val taskId: String? = null,
    @SerialName("ui_thread_id") val threadId: String? = null,
    @SerialName("document_key") val documentKey: String? = null,
    val status: String? = null,
    @SerialName("published_at") val publishedAt: String,
    @SerialName("updated_at") val updatedAt: String? = null,
)

@Serializable data class TodayBriefing(
    val surface: TodayBriefingSurface,
    @SerialName("task_title") val taskTitle: String? = null,
    @SerialName("task_status") val taskStatus: String? = null,
    @SerialName("source_agent_id") val sourceAgentId: String? = null,
    @SerialName("source_output_summary") val sourceOutputSummary: String? = null,
    @SerialName("render_kind") val renderKind: String? = null,
    @SerialName("presentation_state") val presentationState: String? = null,
) { val id: String get() = surface.surfaceId }

@Serializable internal data class TodayBriefingsResponse(val surfaces: List<TodayBriefing> = emptyList())

@Serializable data class TodayBriefingRender(
    val surface: JsonElement? = null,
    @SerialName("source_agent_id") val sourceAgentId: String? = null,
    @SerialName("source_output_summary") val sourceOutputSummary: String? = null,
    @SerialName("text_content") val textContent: String? = null,
    @SerialName("json_content") val jsonContent: JsonElement? = null,
    @SerialName("muij_document") val muijDocument: JsonElement? = null,
    @SerialName("unavailable_reason") val unavailableReason: String? = null,
)

@Serializable internal data class TodayBriefingRenderEnvelope(val render: TodayBriefingRender)

data class TodayPulseTopModel(val provider: String, val model: String, val share: Double)
data class TodayPulse(
    val spendToday: Double = 0.0,
    val spendYesterday: Double = 0.0,
    val callsToday: Int = 0,
    val callsYesterday: Int = 0,
    val hourlySpend: List<Double> = List(24) { 0.0 },
    val hourlyCalls: List<Int> = List(24) { 0 },
    val topModel: TodayPulseTopModel? = null,
    val tasksCompletedToday: Int = 0,
    val tasksCompletedYesterday: Int = 0,
    val codingRunsToday: Int = 0,
    val memoriesToday: Int = 0,
    val evalCasesToday: Int = 0,
    val evalPassesToday: Int = 0,
    /** Whole-workspace `/v3/tasks` status buckets for the fleet pie. */
    val taskBuckets: TodayTaskBuckets = TodayTaskBuckets(),
    /** Most recently updated tasks, one line each on State of Operations. */
    val recentTasks: List<TodayTaskLine> = emptyList(),
) {
    val isEmpty: Boolean get() = spendToday == 0.0 && callsToday == 0 && tasksCompletedToday == 0 &&
        codingRunsToday == 0 && memoriesToday == 0 && evalCasesToday == 0
}

enum class TodayActivityFilter(val title: String) { All("All"), Learnings("Learnings"), Outcomes("Outcomes"), Failed("Failed"), Deliveries("Deliveries") }

fun TodayActivityFilter.matches(item: TodayActivityItem): Boolean = when (this) {
    TodayActivityFilter.All -> true
    TodayActivityFilter.Learnings -> item.itemType == "agent_learning"
    TodayActivityFilter.Outcomes -> item.itemType == "task" || item.itemType == "routine_result"
    TodayActivityFilter.Failed -> item.status == "failed"
    TodayActivityFilter.Deliveries -> item.itemType == "data_delivery"
}

data class TodaySpaceGroup(val id: String, val label: String, val items: List<TodayItem>)

fun todaySpaceGroups(items: List<TodayItem>): List<TodaySpaceGroup> = items
    .groupBy { item -> item.spaceIds.firstOrNull(String::isNotBlank) ?: "unfiled" }
    .map { (id, rows) -> TodaySpaceGroup(id, if (id == "unfiled") "Other" else titleCase(id), rows) }
    .sortedWith(compareBy<TodaySpaceGroup> { it.id == "unfiled" }.thenBy(String.CASE_INSENSITIVE_ORDER) { it.label })

fun titleCase(value: String): String = value.replace('_', ' ').replace('-', ' ').split(' ')
    .filter(String::isNotBlank).joinToString(" ") { word -> word.lowercase().replaceFirstChar(Char::uppercase) }

fun todayGreeting(hour: Int): String = when (hour) {
    in 5..11 -> "Good morning"
    in 12..17 -> "Good afternoon"
    // Late-night hours read as evening (web greeting.ts), never "Good morning".
    else -> "Good evening"
}

fun todayRelativeTime(epochMs: Long, nowMs: Long = System.currentTimeMillis()): String {
    if (epochMs <= 0) return "just now"
    val seconds = ((nowMs - epochMs).coerceAtLeast(0)) / 1_000
    return when {
        seconds < 60 -> "just now"
        seconds < 3_600 -> "${seconds / 60}m ago"
        seconds < 172_800 -> "${seconds / 3_600}h ago"
        else -> "${seconds / 86_400}d ago"
    }
}

enum class TodaySnoozeOption(val label: String) { Tonight("Until tonight"), TomorrowMorning("Tomorrow morning"), NextWeek("Next week") }

fun todaySnoozeMinutes(option: TodaySnoozeOption, now: ZonedDateTime = ZonedDateTime.now()): Int {
    val target = when (option) {
        TodaySnoozeOption.Tonight -> now.withHour(18).withMinute(0).withSecond(0).withNano(0).let {
            if (!it.isAfter(now)) now.plusHours(3) else it
        }
        TodaySnoozeOption.TomorrowMorning -> now.withHour(8).withMinute(0).withSecond(0).withNano(0).let {
            if (!it.isAfter(now)) it.plusDays(1) else it
        }
        TodaySnoozeOption.NextWeek -> {
            var date = now.toLocalDate().plusDays(1)
            while (date.dayOfWeek != DayOfWeek.MONDAY) date = date.plusDays(1)
            date.atTime(8, 0).atZone(now.zone)
        }
    }
    return ChronoUnit.MINUTES.between(now, target).coerceAtLeast(1).toInt()
}

fun isDurableTodayActivity(item: TodayActivityItem): Boolean {
    if (item.itemType in setOf("agent_learning", "data_delivery", "routine_result")) return true
    if (item.itemType != "task") return false
    if (item.status != "done") return item.status == "failed"
    if (!item.summary.isNullOrBlank() || item.metadataString("completion_outcome") != null) return true
    val artifacts = item.metadata.obj()?.get("completion_artifact_names")
    return when (artifacts) {
        is JsonPrimitive -> artifacts.contentOrNull?.isNotBlank() == true
        is JsonArray -> artifacts.isNotEmpty()
        else -> false
    }
}

fun validatedLegacyAttribution(
    decision: AttentionDecisionBinding?, candidateId: String, sourceRevision: String?, servedRoute: String,
): AttentionFeedbackAttribution? = decision?.takeIf {
    it.selected && it.servedRoute == servedRoute && it.candidateId == candidateId && it.sourceRevision == sourceRevision
}?.let { AttentionFeedbackAttribution(it.decisionId, it.candidateId, it.sourceRevision) }

fun deliveryAttribution(binding: AttentionDeliveryBinding?, receipt: AttentionImpressionReceipt?): AttentionFeedbackAttribution? =
    binding?.let { AttentionFeedbackAttribution(it.decisionId, it.candidateId, it.sourceRevision, receipt?.impressionId, it.deliveryId) }

internal fun newEventId(): String = UUID.randomUUID().toString()

@Serializable
data class AttentionPosteriorUpdateReceipt(
    val status: String,
    @SerialName("attribution_quality") val attributionQuality: String? = null,
    @SerialName("degradation_reason") val degradationReason: String? = null,
    @SerialName("posterior_version_before") val posteriorVersionBefore: Long? = null,
    @SerialName("posterior_version_after") val posteriorVersionAfter: Long? = null,
    @SerialName("rescore_scheduled") val rescoreScheduled: Boolean? = null,
)

@Serializable
data class AttentionFeedbackReceipt(
    @SerialName("outcome_id") val outcomeId: String,
    val outcome: String,
    val surface: String,
    @SerialName("feedback_recorded") val feedbackRecorded: Boolean,
    @SerialName("affected_candidates") val affectedCandidates: Int,
    @SerialName("rescore_status") val rescoreStatus: String,
    @SerialName("diagnostic_href") val diagnosticHref: String? = null,
    @SerialName("posterior_update") val posteriorUpdate: AttentionPosteriorUpdateReceipt? = null,
)

@Serializable internal data class AttentionFeedbackEnvelope(
    @SerialName("feedback_receipt") val feedbackReceipt: AttentionFeedbackReceipt? = null,
)

@Serializable internal data class TodayActionTaskManifest(@SerialName("task_id") val taskId: String? = null)
@Serializable internal data class TodayActionTaskRecord(val manifest: TodayActionTaskManifest? = null)
@Serializable internal data class TodayActionNavigation(@SerialName("task_id") val taskId: String? = null)

@Serializable internal data class TodayActionExecutionResult(
    @SerialName("task_id") val taskId: String? = null,
    val task: TodayActionTaskRecord? = null,
    @SerialName("navigate_to") val navigateTo: TodayActionNavigation? = null,
) {
    val resolvedTaskId: String?
        get() = navigateTo?.taskId ?: taskId ?: task?.manifest?.taskId
}
