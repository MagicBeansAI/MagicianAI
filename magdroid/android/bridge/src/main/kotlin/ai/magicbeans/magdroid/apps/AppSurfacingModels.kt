package ai.magicbeans.magdroid.apps

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import java.time.Instant

/** Client-side mirrors of the closed app-widget V1 wire contract. */
object AppSurfacingLimits {
    const val SchemaVersion = 1
    const val MaximumSlotsPerPage = 12
    const val MaximumRowsPerWidget = 32
    const val MaximumFieldsPerRow = 32
    const val MaximumIndicators = 32
    const val MaximumActionsPerWidget = 8
    const val MaximumResponseBytes = 1024 * 1024
    const val MaximumSlotResponseBytes = 64 * 1024
    const val MaximumSlotBatchResponseBytes = 128 * 1024
    const val MaximumIndicatorResponseBytes = 128 * 1024
    // The public launch may inline a host-bounded synchronous result in
    // addition to its receipt envelope. Match the iOS transport ceiling.
    const val MaximumActionResponseBytes = 256 * 1024
    const val MaximumTitleBytes = 256
    const val MaximumIndicatorTextBytes = 256
    const val MaximumJsonDepth = 16
    const val MaximumJsonNodes = 2_048
    // Mini-frame declaration ceilings (gate S4), widget-sized rather than
    // page-sized. They are the web client's `APP_WIDGET_MINI_FRAME_MAX_HEIGHT_PX`
    // and entry-point bounds exactly: a declaration this handset accepted while
    // the other clients refused it would leave the same widget in two states.
    const val MaximumMiniFrameHeightPx = 480
    const val MaximumMiniFrameEntryPointBytes = 512
    const val MaximumMiniFrameEntryPointSegments = 16
    const val MaximumMiniFrameEntryPointSegmentBytes = 128
    // Slot settings/picker ceilings. These mirror the host's exactly: a
    // handset that accepted a larger page than the host can produce would be
    // trusting the transport rather than the contract.
    const val MaximumSlotAssignments = 128
    const val MaximumPickerCandidates = 100
    const val MaximumAccumulatedPickerCandidates = 512
    const val MaximumSuggestedSlotsPerWidget = 16
    const val MaximumSlotCursorBytes = 640
    const val MaximumSlotSettingsResponseBytes = 2 * 1024 * 1024
    const val DefaultSlotAssignmentLimit = 32
    const val DefaultSlotPickerLimit = 32
}

@Serializable
enum class AppWidgetClientCapability {
    @SerialName("detail_v1") DetailV1,
    @SerialName("list_v1") ListV1,
    @SerialName("table_v1") TableV1,
    @SerialName("timeline_v1") TimelineV1,
    @SerialName("tree_v1") TreeV1,
    @SerialName("graph_v1") GraphV1,
    @SerialName("governed_actions_v1") GovernedActionsV1,
}

internal val AndroidWidgetCapabilities = AppWidgetClientCapability.entries

@Serializable
data class AppWidgetRenderTarget(
    @SerialName("installation_id") val installationId: String,
    @SerialName("widget_id") val widgetId: String,
)

@Serializable
data class AppWidgetRenderBatchRequest(
    @SerialName("schema_version") val schemaVersion: Int = AppSurfacingLimits.SchemaVersion,
    @SerialName("client_capabilities") val clientCapabilities: List<AppWidgetClientCapability> = AndroidWidgetCapabilities,
    val widgets: List<AppWidgetRenderTarget>,
)

@Serializable
data class AppActionRunHandle(
    @SerialName("protocol_version") val protocolVersion: String,
    @SerialName("run_ref") val runRef: String,
    @SerialName("installation_id") val installationId: String,
    @SerialName("action_id") val actionId: String,
)

@Serializable
data class AppActionLaunchReceipt(
    @SerialName("run_handle") val runHandle: AppActionRunHandle,
    @SerialName("execution_id") val executionId: String? = null,
    val result: JsonElement? = null,
)

@Serializable
data class AppWidgetRenderHints(
    @SerialName("display_field") val displayField: String? = null,
    @SerialName("partition_field") val partitionField: String? = null,
    @SerialName("parent_field") val parentField: String? = null,
    @SerialName("order_field") val orderField: String? = null,
    @SerialName("status_field") val statusField: String? = null,
    @SerialName("timestamp_field") val timestampField: String? = null,
    @SerialName("action_field") val actionField: String? = null,
    @SerialName("actor_field") val actorField: String? = null,
    @SerialName("type_field") val typeField: String? = null,
    @SerialName("target_field") val targetField: String? = null,
)

@Serializable
data class AppWidgetGovernedAction(
    @SerialName("action_id") val actionId: String,
    val label: String,
)

@Serializable
data class AppWidgetRenderRow(
    val entity: String,
    @SerialName("record_id") val recordId: String,
    @SerialName("record_revision") val recordRevision: Long,
    val fields: Map<String, JsonElement>,
)

/**
 * The server's model is internally tagged by `model`. Keeping a strict flat
 * wire type avoids accepting an open polymorphic subtype on the handset; the
 * validator below closes both the tag and the fields each tag may use.
 */
@Serializable
data class AppWidgetNativeModel(
    val model: String,
    val row: AppWidgetRenderRow? = null,
    val rows: List<AppWidgetRenderRow>? = null,
    val columns: List<String>? = null,
    val hints: AppWidgetRenderHints,
    val actions: List<AppWidgetGovernedAction>,
)

@Serializable
data class AppWidgetFallback(
    val kind: String,
    val title: String? = null,
    val body: String? = null,
)

/**
 * A widget's declared escalation to a sandboxed mini frame (gate S4).
 *
 * It rides BESIDE a complete native model, never instead of one, so a client
 * that hosts no frame still draws the widget's real content — and this one
 * hosts none: [CustomSurfaceSupport] is closed and Android ships no WebView.
 *
 * The member is still decoded rather than left unknown because the render
 * batch is decoded as a whole: one unknown key fails the whole response, so a
 * single escalating widget would blank every pinned widget on the page the day
 * the host starts emitting one. Validating it to the same bounds web and iOS
 * use also keeps this client from being the lax one if a later gate admits a
 * host here.
 */
@Serializable
data class AppWidgetMiniFrameDeclaration(
    @SerialName("entry_point") val entryPoint: String,
    @SerialName("max_height_px") val maxHeightPx: Int,
)

@Serializable
data class AppWidgetRenderItem(
    @SerialName("installation_id") val installationId: String,
    @SerialName("widget_id") val widgetId: String,
    val title: String? = null,
    @SerialName("installation_generation") val installationGeneration: Long? = null,
    val revision: String,
    @SerialName("rendered_at") val renderedAt: String,
    @SerialName("refresh_after") val refreshAfter: String,
    val state: String,
    val model: AppWidgetNativeModel? = null,
    val fallback: AppWidgetFallback? = null,
    @SerialName("mini_frame") val miniFrame: AppWidgetMiniFrameDeclaration? = null,
    /**
     * Optional manifest staleness bound. Decoded (the batch is decoded
     * strictly, so an unknown key would blank every widget) and, when
     * present, bounds how long a body survives transient refresh failures.
     */
    @SerialName("max_staleness_seconds") val maxStalenessSeconds: Long? = null,
)

@Serializable
data class AppWidgetRenderBatchResponse(
    @SerialName("schema_version") val schemaVersion: Int,
    val revision: String,
    val etag: String,
    @SerialName("rendered_at") val renderedAt: String,
    @SerialName("refresh_after") val refreshAfter: String,
    val widgets: List<AppWidgetRenderItem>,
)

@Serializable
data class AppIndicatorModel(
    val kind: String,
    val text: String? = null,
    val count: Long? = null,
    val label: String? = null,
)

@Serializable
data class AppMaterializedIndicator(
    @SerialName("installation_id") val installationId: String,
    @SerialName("installation_generation") val installationGeneration: Long,
    @SerialName("indicator_id") val indicatorId: String,
    val title: String,
    val revision: String,
    @SerialName("evaluated_at") val evaluatedAt: String,
    @SerialName("expires_at") val expiresAt: String,
    val model: AppIndicatorModel,
)

@Serializable
data class AppIndicatorListResponse(
    @SerialName("schema_version") val schemaVersion: Int,
    val revision: String,
    val etag: String,
    @SerialName("generated_at") val generatedAt: String,
    val indicators: List<AppMaterializedIndicator>,
)

@Serializable
data class AppSlotPackageBinding(
    @SerialName("installation_id") val installationId: String,
    @SerialName("package_id") val packageId: String,
    @SerialName("package_revision_ref") val packageRevisionRef: String,
    @SerialName("package_content_digest") val packageContentDigest: String,
    @SerialName("installation_generation") val installationGeneration: Long,
)

/** Exact wire mirror for `AppSlotWidgetBinding.package`. */
@Serializable
data class AppSlotWidgetBindingWire(
    @SerialName("package") val packageIdentity: AppSlotPackageBinding,
    @SerialName("widget_id") val widgetId: String,
)

@Serializable
data class AppSlotEffectiveWidgetWire(
    val pinned: AppSlotWidgetBindingWire,
    val current: AppSlotWidgetBindingWire,
    @SerialName("restored_across_generation") val restoredAcrossGeneration: Boolean,
    @SerialName("assignment_compatibility") val assignmentCompatibility: String,
)

@Serializable
data class AppResolvedSlotAssignmentWire(
    @SerialName("slot_id") val slotId: String,
    val source: String? = null,
    @SerialName("pinned_system_default") val pinnedSystemDefault: Boolean,
    @SerialName("opted_out") val optedOut: Boolean,
    val widget: AppSlotEffectiveWidgetWire? = null,
    @SerialName("hidden_reason") val hiddenReason: String? = null,
)

@Serializable
data class AppSlotResolutionBatchRequest(
    @SerialName("slot_ids") val slotIds: List<String>,
)

@Serializable
data class AppSlotResolutionBatchResponse(
    val assignments: List<AppResolvedSlotAssignmentWire>,
)

internal fun AppSlotResolutionBatchResponse.checked(
    expected: List<AppSlotId>,
): List<AppResolvedSlotAssignment> {
    require(expected.isNotEmpty() && expected.size <= AppSurfacingLimits.MaximumSlotsPerPage)
    require(expected.toSet().size == expected.size && assignments.size == expected.size)
    return assignments.mapIndexed { index, assignment -> assignment.checked(expected[index]) }
}

data class AppResolvedSlotAssignment(
    val slotId: AppSlotId,
    val target: AppWidgetRenderTarget?,
    val packageRevisionRef: String?,
    val packageContentDigest: String?,
    val installationGeneration: Long?,
    val optedOut: Boolean,
    val hiddenReason: String?,
    /**
     * The exact validated wire shape this projection came from.
     *
     * Slot editing compares a settings snapshot against the resolution the
     * visible card was drawn from, and that comparison must cover every field
     * the host may have changed — source, pinned default, both bindings, and
     * the restore flag — not just the ones this client renders. Structural
     * equality of the wire record *is* that comparison.
     */
    val wire: AppResolvedSlotAssignmentWire,
) {
    /** `null` until someone assigns, a workspace default pins, or one hides. */
    val source: String? get() = wire.source

    /** True while any authority — assignment, default, or hide — owns the slot. */
    val occupied: Boolean
        get() = wire.widget != null || wire.source != null || wire.hiddenReason != null

    /**
     * A workspace default the host hid for a reason the owner cannot act on
     * (package gone, identity changed, …). Nothing the owner chose is
     * missing, so the slot reads as empty rather than as a broken widget.
     * An owner's own assignment, and a default that is merely disabled or
     * waiting on an update, keep the unavailable card.
     */
    val quietlyHiddenDefault: Boolean
        get() = source == "workspace_default" && hiddenReason != null &&
            hiddenReason !in LoudHiddenReasons

    private companion object {
        val LoudHiddenReasons = setOf("disabled", "update_pending")
    }
}

/**
 * Whether a slot should draw the "App widget unavailable" card when it has no
 * rendered body. A quietly hidden workspace default draws nothing instead.
 */
fun AppResolvedSlotAssignment.showsUnavailablePlaceholder(item: AppWidgetRenderItem?): Boolean =
    item == null && occupied && !quietlyHiddenDefault

/** Canonical page-qualified slot identity, byte-for-byte compatible with Rust. */
@JvmInline
value class AppSlotId private constructor(val value: String) {
    companion object {
        fun forPageRegion(page: String, region: String): AppSlotId {
            require(page.isNotEmpty() && page.length <= 256 && page.startsWith('/'))
            require(page.all { it.code in 0x20..0x7e && it !in "\\?#%:" })
            if (page != "/") {
                val segments = page.drop(1).split('/')
                require(segments.size in 1..16)
                require(segments.all { segment ->
                    segment.isNotEmpty() && segment != "." && segment != ".." &&
                        segment.all { (it.isLetterOrDigit() && it.code < 128) || it in "_-." }
                })
            }
            require(region.matches(Regex("[A-Za-z0-9][A-Za-z0-9_-]{0,63}")))
            val pageHex = page.encodeToByteArray().joinToString("") { byte ->
                (byte.toInt() and 0xff).toString(16).padStart(2, '0')
            }
            return AppSlotId("page:$pageHex:$region")
        }
    }
}

/** The region every canonical app-surface entity route fits. */
const val AppSlotContextualRegion: String = "contextual"

/**
 * The canonical app-surface entity route for an installation.
 *
 * A slot assignment is data, not layout code, so the same fitting must resolve
 * to the same slot on every client. This derives the page byte-for-byte the
 * way web does; a handset that invented its own route would silently give the
 * owner a second, private layout for the same app. Returns null when the
 * identity cannot form a bounded static route.
 */
fun appSurfaceSlotPage(installationId: String, surfacePath: String? = null): String? {
    if (!installationId.isOpaqueId()) return null
    val page = "/apps/" + installationId + (surfacePath?.let { "/$it" } ?: "")
    return runCatching { AppSlotId.forPageRegion(page, AppSlotContextualRegion) }
        .map { page }
        .getOrNull()
}

/**
 * Re-derives a slot identity from its canonical wire form.
 *
 * A settings page names slots this client never asked for, so their ids cannot
 * be checked against an expectation. Decoding and re-encoding is the check:
 * anything that does not round-trip to the exact same string is refused rather
 * than trusted as an opaque key.
 */
internal fun canonicalAppSlotId(value: String): AppSlotId? {
    if (value.length > 600) return null
    val match = Regex("^page:([0-9a-f]*):([A-Za-z0-9][A-Za-z0-9_-]{0,63})$").matchEntire(value) ?: return null
    val hex = match.groupValues[1]
    if (hex.isEmpty() || hex.length % 2 != 0 || hex.length > 512) return null
    val decoded = ByteArray(hex.length / 2) { index ->
        ((hex[index * 2].digitToInt(16) shl 4) or hex[index * 2 + 1].digitToInt(16)).toByte()
    }
    val page = runCatching { decoded.decodeToString(throwOnInvalidSequence = true) }.getOrNull() ?: return null
    return runCatching { AppSlotId.forPageRegion(page, match.groupValues[2]) }
        .getOrNull()
        ?.takeIf { it.value == value }
}

internal fun AppResolvedSlotAssignmentWire.checked(expected: AppSlotId): AppResolvedSlotAssignment {
    require(slotId == expected.value)
    require(source == null || source in setOf("user", "workspace_default"))
    require(hiddenReason == null || hiddenReason in setOf(
        "package_unavailable", "disabled", "quarantined", "update_pending",
        "package_identity_changed", "package_digest_changed", "generation_rollback",
        "widget_no_longer_declared",
    ))
    require(!(widget != null && hiddenReason != null))
    require(!optedOut || (source == null && widget == null && hiddenReason == null))
    require(widget == null || source != null)
    require(hiddenReason == null || source != null)
    val currentBinding = widget?.current
    val target = currentBinding?.let { current ->
        require(widget.assignmentCompatibility == "exact_digest_only")
        val pinned = widget.pinned
        pinned.packageIdentity.checked()
        require(widget.pinned.widgetId.isAppName())
        current.packageIdentity.checked()
        require(current.widgetId.isAppName())
        require(pinned.widgetId == current.widgetId)
        require(pinned.packageIdentity.installationId == current.packageIdentity.installationId)
        require(pinned.packageIdentity.packageId == current.packageIdentity.packageId)
        require(pinned.packageIdentity.packageContentDigest == current.packageIdentity.packageContentDigest)
        require(current.packageIdentity.installationGeneration >= pinned.packageIdentity.installationGeneration)
        require(widget.restoredAcrossGeneration ==
            (current.packageIdentity.installationGeneration > pinned.packageIdentity.installationGeneration))
        AppWidgetRenderTarget(current.packageIdentity.installationId, current.widgetId)
    }
    require(!(optedOut && target != null))
    return AppResolvedSlotAssignment(
        expected,
        target,
        currentBinding?.packageIdentity?.packageRevisionRef,
        currentBinding?.packageIdentity?.packageContentDigest,
        currentBinding?.packageIdentity?.installationGeneration,
        optedOut,
        hiddenReason,
        this,
    )
}

internal fun AppWidgetRenderBatchResponse.checked(expected: List<AppWidgetRenderTarget>): AppWidgetRenderBatchResponse {
    require(schemaVersion == AppSurfacingLimits.SchemaVersion)
    require(widgets.size <= AppSurfacingLimits.MaximumSlotsPerPage)
    require(revision.isDigest() && etag == revision)
    val rendered = Instant.parse(renderedAt)
    val refresh = Instant.parse(refreshAfter)
    val batchSeconds = java.time.Duration.between(rendered, refresh).seconds
    require(refresh.isAfter(rendered) && batchSeconds <= 86_400)
    require(widgets.map { it.installationId to it.widgetId }.toSet().size == widgets.size)
    val expectedSet = expected.map { it.installationId to it.widgetId }.toSet()
    require(widgets.map { it.installationId to it.widgetId }.toSet() == expectedSet)
    widgets.forEach(AppWidgetRenderItem::checked)
    require(widgets.minOf { Instant.parse(it.refreshAfter) } == refresh)
    return this
}

internal fun AppActionLaunchReceipt.checked(
    expectedInstallationId: String,
    expectedActionId: String,
): AppActionLaunchReceipt {
    require(runHandle.protocolVersion == "1")
    require(runHandle.runRef.isReference())
    require(runHandle.installationId == expectedInstallationId && expectedInstallationId.isOpaqueId())
    require(runHandle.actionId == expectedActionId && expectedActionId.isAppName())
    require(executionId == null || (
        executionId.isNotBlank() && executionId.toByteArray().size <= 192 &&
            executionId.none { Character.isISOControl(it) }
    ))
    require(result == null || result.boundedJson(maximumDepth = 32, maximumNodes = 4_096))
    return this
}

internal fun AppWidgetRenderItem.checked() {
    require(installationId.isOpaqueId() && widgetId.isAppName() && revision.isDigest())
    require(title == null || title.isBoundedText())
    require(installationGeneration == null || installationGeneration > 0)
    require(maxStalenessSeconds == null || maxStalenessSeconds in 1..86_400)
    requireRefreshWindow(Instant.parse(renderedAt), Instant.parse(refreshAfter))
    when (state) {
        "ready" -> {
            require(model != null && fallback == null && installationGeneration != null)
            model.checked()
            miniFrame?.checked()
        }
        // An item with no model has no content the host could produce, so a
        // frame claimed there would be app code standing in for it.
        "unsupported" -> {
            require(model == null && fallback != null && miniFrame == null)
            fallback.checked()
        }
        "unavailable" -> require(model == null && fallback == null && miniFrame == null)
        else -> error("unsupported widget state")
    }
}

internal fun AppWidgetNativeModel.checked() {
    require(model in setOf("detail", "list", "table", "timeline", "tree", "graph"))
    require(actions.size <= AppSurfacingLimits.MaximumActionsPerWidget)
    require(actions.distinctBy { it.actionId }.size == actions.size)
    actions.forEach { require(it.actionId.isAppName() && it.label.isBoundedActionLabel()) }
    listOfNotNull(
        hints.displayField, hints.partitionField, hints.parentField, hints.orderField,
        hints.statusField, hints.timestampField, hints.actionField, hints.actorField,
        hints.typeField, hints.targetField,
    ).forEach { require(it.isFieldPath()) }
    when (model) {
        "detail" -> require(rows == null && columns == null)
        "table" -> {
            require(row == null && rows != null && columns != null && columns.size <= AppSurfacingLimits.MaximumFieldsPerRow)
            require(columns.distinct().size == columns.size && columns.all(String::isFieldPath))
        }
        else -> require(row == null && rows != null && columns == null)
    }
    val projected = listOfNotNull(row) + rows.orEmpty()
    require(projected.size <= AppSurfacingLimits.MaximumRowsPerWidget)
    projected.forEach(AppWidgetRenderRow::checked)
}

internal fun AppWidgetRenderRow.checked() {
    require(entity.isAppName() && recordId.isOpaqueId() && recordRevision > 0)
    require(fields.size <= AppSurfacingLimits.MaximumFieldsPerRow)
    fields.forEach { (key, value) ->
        require(key.isFieldPath())
        require(value.boundedJson())
    }
}

internal fun AppWidgetMiniFrameDeclaration.checked() {
    require(maxHeightPx in 1..AppSurfacingLimits.MaximumMiniFrameHeightPx)
    require(entryPoint.toByteArray().size in 1..AppSurfacingLimits.MaximumMiniFrameEntryPointBytes)
    require(entryPoint.startsWith('/'))
    // `:` is refused alongside the escape, query and fragment characters, so a
    // parameterised route cannot be declared: V1 maps no widget input into a
    // frame, and a frame at a route nobody declared is what the gate prevents.
    require(entryPoint.none { it in "\\?#%:" })
    val segments = entryPoint.drop(1).split('/')
    require(segments.size in 1..AppSurfacingLimits.MaximumMiniFrameEntryPointSegments)
    require(segments.all { segment ->
        segment.isNotEmpty() &&
            segment.toByteArray().size <= AppSurfacingLimits.MaximumMiniFrameEntryPointSegmentBytes &&
            segment != "." && segment != ".." &&
            segment.all { (it.isLetterOrDigit() && it.code < 128) || it in "_-." }
    })
}

internal fun AppWidgetFallback.checked() {
    when (kind) {
        "hide" -> require(title == null && body == null)
        "message" -> require(title?.isBoundedText() == true && body?.isBoundedText() == true)
        else -> error("unsupported widget fallback")
    }
}

internal fun AppIndicatorListResponse.checked(): AppIndicatorListResponse {
    require(schemaVersion == AppSurfacingLimits.SchemaVersion)
    require(revision.isDigest() && etag == revision)
    Instant.parse(generatedAt)
    require(indicators.size <= AppSurfacingLimits.MaximumIndicators)
    require(indicators.map { it.installationId to it.indicatorId }.toSet().size == indicators.size)
    indicators.forEach { indicator ->
        require(indicator.installationId.isOpaqueId() && indicator.installationGeneration > 0)
        require(indicator.indicatorId.isAppName() && indicator.title.isBoundedText())
        require(indicator.revision.isDigest())
        val evaluated = Instant.parse(indicator.evaluatedAt)
        val expires = Instant.parse(indicator.expiresAt)
        require(expires.isAfter(evaluated))
        indicator.model.checked()
    }
    return this
}

internal fun AppMaterializedIndicator.isLiveAt(now: Instant): Boolean =
    runCatching { Instant.parse(expiresAt).isAfter(now) }.getOrDefault(false)

internal fun AppIndicatorModel.checked() {
    when (kind) {
        "chip" -> require(text?.isIndicatorText() == true && count == null && label == null)
        "badge" -> require(count != null && count in 1L..9_999L && text == null && label == null)
        "state" -> require(label?.isIndicatorText() == true && text == null && count == null)
        else -> error("unsupported indicator kind")
    }
}

internal fun AppSlotPackageBinding.checked() {
    require(installationId.isOpaqueId() && packageId.isReference() && packageRevisionRef.isReference())
    require(packageContentDigest.isDigest() && installationGeneration > 0)
}

private fun requireRefreshWindow(renderedAt: Instant, refreshAfter: Instant) {
    val seconds = java.time.Duration.between(renderedAt, refreshAfter).seconds
    require(seconds in 5..86_400)
}

internal fun String.isAppName(): Boolean = matches(Regex("[A-Za-z0-9][A-Za-z0-9_-]{0,63}"))
internal fun String.isOpaqueId(): Boolean = length in 1..128 && matches(Regex("[A-Za-z0-9][A-Za-z0-9_.-]*"))
internal fun String.isReference(): Boolean = length in 1..192 && matches(Regex("[A-Za-z0-9][A-Za-z0-9_.:/@#-]*"))
internal fun String.isDigest(): Boolean = matches(Regex("blake3:[0-9a-f]{64}"))
private fun String.isFieldPath(): Boolean = length in 1..256 && split('.').let { parts ->
    parts.size <= 16 && parts.all(String::isAppName)
}
internal fun String.isBoundedText(): Boolean = isNotBlank() && toByteArray().size <= AppSurfacingLimits.MaximumTitleBytes && none { Character.isISOControl(it) }
private fun String.isBoundedActionLabel(): Boolean = isNotBlank() && toByteArray().size <= 64 && none { Character.isISOControl(it) }
private fun String.isIndicatorText(): Boolean = isNotEmpty() && toByteArray().size <= AppSurfacingLimits.MaximumIndicatorTextBytes && none { Character.isISOControl(it) }

private fun JsonElement.boundedJson(
    maximumDepth: Int = AppSurfacingLimits.MaximumJsonDepth,
    maximumNodes: Int = AppSurfacingLimits.MaximumJsonNodes,
): Boolean {
    var nodes = 0
    val pending = ArrayDeque<Pair<JsonElement, Int>>()
    pending.add(this to 0)
    while (pending.isNotEmpty()) {
        val (value, depth) = pending.removeLast()
        if (depth > maximumDepth || ++nodes > maximumNodes) return false
        when (value) {
            is JsonArray -> value.forEach { pending.add(it to depth + 1) }
            is JsonObject -> value.forEach { (key, child) ->
                if (key.length > 256) return false
                pending.add(child to depth + 1)
            }
            JsonNull -> Unit
            is JsonPrimitive -> if ((value.contentOrNull?.length ?: 0) > 16_384) return false
        }
    }
    return true
}

/** Compact, non-markup display for a bounded scalar or structured value. */
fun JsonElement?.appWidgetDisplay(): String = when (this) {
    null, JsonNull -> "—"
    is JsonPrimitive -> contentOrNull.orEmpty().take(256)
    is JsonArray -> "${size} items"
    is JsonObject -> entries.take(3).joinToString(" · ") { (key, value) -> "$key ${value.appWidgetDisplay()}" }.take(256)
}
