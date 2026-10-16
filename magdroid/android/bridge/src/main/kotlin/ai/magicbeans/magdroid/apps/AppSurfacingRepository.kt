package ai.magicbeans.magdroid.apps

import ai.magicbeans.magdroid.access.MagicianAccess
import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.HttpRequestBuilder
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.parameter
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsChannel
import io.ktor.http.ContentType
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import java.io.Closeable
import java.io.ByteArrayOutputStream
import java.net.URLEncoder
import java.nio.charset.StandardCharsets
import java.security.MessageDigest
import java.time.Instant
import io.ktor.utils.io.ByteReadChannel
import io.ktor.utils.io.readAvailable

data class AppSlotRegionSpec(val page: String, val region: String) {
    val slotId: AppSlotId = AppSlotId.forPageRegion(page, region)
}

data class AppWidgetPageSnapshot(
    val assignments: Map<AppSlotId, AppResolvedSlotAssignment>,
    val widgetsBySlot: Map<AppSlotId, AppWidgetRenderItem>,
    val targetFingerprint: String,
    val etag: String?,
    val refreshAfter: Instant?,
    /**
     * When the widget bodies were last confirmed by the host (a 200 or a
     * 304). A transient failure keeps them on screen only within the
     * staleness bound measured from here.
     */
    val confirmedAt: Instant? = null,
)

sealed interface AppWidgetPageRefresh {
    data class Modified(val snapshot: AppWidgetPageSnapshot) : AppWidgetPageRefresh
    data class NotModified(
        val assignments: Map<AppSlotId, AppResolvedSlotAssignment>,
        val targetFingerprint: String,
        val etag: String,
        val refreshAfter: Instant,
    ) : AppWidgetPageRefresh
}

sealed interface AppIndicatorRefresh {
    data class Modified(val response: AppIndicatorListResponse, val etag: String) : AppIndicatorRefresh
    data class NotModified(val etag: String) : AppIndicatorRefresh
}

class AppSurfacingApiException(message: String, val status: Int = 0) : Exception(message)

/**
 * Authenticated, foreground-owned transport for Android app surfacing.
 *
 * Requests cannot provide reads, predicates, action inputs, or package
 * identity. Slot resolution chooses exact server-owned widget bindings; one
 * page is rendered in one bounded batch; governed action launch always sends
 * the exact empty object.
 */
class AppSurfacingRepository(
    context: Context,
    private val client: HttpClient = HttpClient(CIO) {
        followRedirects = false
        install(HttpTimeout) {
            requestTimeoutMillis = 30_000
            socketTimeoutMillis = 30_000
            connectTimeoutMillis = 20_000
        }
    },
) : Closeable {
    private val applicationContext = context.applicationContext

    private fun host(): String = MagicianAccess.baseUrl(applicationContext).trimEnd('/').ifBlank {
        throw AppSurfacingApiException("No Magician host configured yet.")
    }

    private fun appsBase(): String = "${host()}/api/magician/v2/apps"

    /**
     * Non-secret identity for invalidating conditional bodies across pairing changes.
     *
     * Principal/workspace/device labels are not enough: a replacement bearer or
     * Access credential can be installed without changing those labels. Hash the
     * complete outbound authority snapshot so an old card can never launch with
     * newly installed credentials while the foreground owner still believes it
     * is operating under the previous authority. Only the digest is retained.
     */
    internal fun authorityKey(): String {
        val credentialBytes = MessageDigest.getInstance("SHA-256").digest(
            MagicianAccess.headers(applicationContext)
                .toSortedMap()
                .entries
                .joinToString("\u0000") { (name, value) -> "$name\u0000$value" }
                .encodeToByteArray(),
        )
        val credentialDigest = buildString(credentialBytes.size * 2) {
            credentialBytes.forEach { byte ->
                val value = byte.toInt() and 0xff
                append(HexDigits[value ushr 4])
                append(HexDigits[value and 0x0f])
            }
        }
        return listOf(
            MagicianAccess.baseUrl(applicationContext),
            MagicianAccess.principal(applicationContext),
            MagicianAccess.workspace(applicationContext),
            MagicianAccess.deviceId(applicationContext),
            credentialDigest,
        ).joinToString("\u0000")
    }

    private fun HttpRequestBuilder.authorize() {
        MagicianAccess.headers(applicationContext).forEach { (name, value) -> header(name, value) }
    }

    suspend fun refreshPage(
        regions: List<AppSlotRegionSpec>,
        previousEtag: String?,
        previousTargetFingerprint: String?,
        expectedAuthorityKey: String,
    ): AppWidgetPageRefresh {
        requireAuthority(expectedAuthorityKey)
        require(regions.isNotEmpty() && regions.size <= AppSurfacingLimits.MaximumSlotsPerPage)
        require(regions.map(AppSlotRegionSpec::slotId).toSet().size == regions.size)
        require(regions.map(AppSlotRegionSpec::page).toSet().size == 1)

        val assignments = resolveSlots(regions.map(AppSlotRegionSpec::slotId), expectedAuthorityKey)
            .associateBy(AppResolvedSlotAssignment::slotId)
        val targets = assignments.values.mapNotNull(AppResolvedSlotAssignment::target).distinct()
        val fingerprint = regions.joinToString("\u0000") { region ->
            val assignment = assignments[region.slotId]
            val target = assignment?.target
            listOf(
                region.slotId.value,
                target?.installationId.orEmpty(),
                target?.widgetId.orEmpty(),
                assignment?.packageRevisionRef.orEmpty(),
                assignment?.packageContentDigest.orEmpty(),
                assignment?.installationGeneration?.toString().orEmpty(),
            ).joinToString("\u0001")
        }
        if (targets.isEmpty()) {
            return AppWidgetPageRefresh.Modified(
                AppWidgetPageSnapshot(assignments, emptyMap(), fingerprint, null, null),
            )
        }

        val reusableEtag = previousEtag?.takeIf { previousTargetFingerprint == fingerprint }
        val request = AppWidgetRenderBatchRequest(widgets = targets)
        val response = client.post("${appsBase()}/widgets/render-batch") {
            authorize()
            requireAuthority(expectedAuthorityKey)
            contentType(ContentType.Application.Json)
            reusableEtag?.let { header(HttpHeaders.IfNoneMatch, "\"$it\"") }
            setBody(appSurfacingJson.encodeToString(request))
        }
        requireAuthority(expectedAuthorityKey)
        val responseEtag = response.headers[HttpHeaders.ETag]?.let(::normalizedAppEtag)
        if (response.status == HttpStatusCode.NotModified) {
            val retained = responseEtag?.takeIf { it == reusableEtag }
                ?: throw AppSurfacingApiException("Widget refresh returned an unbound 304 ETag.", 304)
            // A 304 means the bodies are unchanged. A deadline at or before
            // now (the batch deadline is the minimum over a shared cache and
            // can land milliseconds late) or a missing one is not a failure:
            // keep the bodies and ask again after a short floor.
            val refreshAfter = acceptedWidgetRefreshDeadline(
                Instant.now(),
                response.headers[WidgetRefreshAfterHeader]?.let { runCatching { Instant.parse(it) }.getOrNull() },
            )
            return AppWidgetPageRefresh.NotModified(assignments, fingerprint, retained, refreshAfter)
        }
        val decoded = appSurfacingJson.decodeFromString(
            AppWidgetRenderBatchResponse.serializer(),
            response.successText(AppSurfacingLimits.MaximumResponseBytes, "load app widgets"),
        ).checked(targets)
        val byTarget = decoded.widgets.associateBy { it.installationId to it.widgetId }
        val bySlot = assignments.mapNotNull { (slotId, assignment) ->
            val target = assignment.target ?: return@mapNotNull null
            val item = byTarget[target.installationId to target.widgetId] ?: return@mapNotNull null
            if (item.state != "unavailable" && item.installationGeneration != assignment.installationGeneration) {
                return@mapNotNull null
            }
            slotId to item
        }.toMap()
        val etag = responseEtag?.takeIf { it == decoded.etag }
            ?: throw AppSurfacingApiException("Widget response ETag did not bind its body.", response.status.value)
        val bodyRefreshAfter = Instant.parse(decoded.refreshAfter)
        // The body carries the bound deadline; the header only mirrors it.
        val headerRefreshAfter = response.headers[WidgetRefreshAfterHeader]?.let(Instant::parse)
        if (headerRefreshAfter != null && headerRefreshAfter != bodyRefreshAfter) {
            throw AppSurfacingApiException("Widget response refresh deadlines disagree.", response.status.value)
        }
        val now = Instant.now()
        val modified = AppWidgetPageRefresh.Modified(
            AppWidgetPageSnapshot(
                assignments,
                bySlot,
                fingerprint,
                etag,
                acceptedWidgetRefreshDeadline(now, bodyRefreshAfter),
                confirmedAt = now,
            ),
        )
        requireAuthority(expectedAuthorityKey)
        return modified
    }

    suspend fun refreshIndicators(
        previousEtag: String?,
        expectedAuthorityKey: String,
    ): AppIndicatorRefresh {
        requireAuthority(expectedAuthorityKey)
        val response = client.get("${appsBase()}/indicators") {
            authorize()
            requireAuthority(expectedAuthorityKey)
            previousEtag?.let { header(HttpHeaders.IfNoneMatch, "\"$it\"") }
        }
        requireAuthority(expectedAuthorityKey)
        val responseEtag = response.headers[HttpHeaders.ETag]?.let(::normalizedAppEtag)
        if (response.status == HttpStatusCode.NotModified) {
            return AppIndicatorRefresh.NotModified(
                responseEtag?.takeIf { it == previousEtag }
                    ?: throw AppSurfacingApiException("Indicator refresh returned an unbound 304 ETag.", 304),
            )
        }
        val decoded = appSurfacingJson.decodeFromString(
            AppIndicatorListResponse.serializer(),
            response.successText(AppSurfacingLimits.MaximumIndicatorResponseBytes, "load app indicators"),
        ).checked()
        val live = decoded.copy(indicators = decoded.indicators.filter { it.isLiveAt(Instant.now()) })
        val etag = responseEtag?.takeIf { it == decoded.etag }
            ?: throw AppSurfacingApiException("Indicator response ETag did not bind its body.", response.status.value)
        val modified = AppIndicatorRefresh.Modified(live, etag)
        requireAuthority(expectedAuthorityKey)
        return modified
    }

    /** Native widget actions accept no client-supplied input in V1. */
    internal suspend fun launchEmptyAction(
        installationId: String,
        actionId: String,
        idempotencyKey: String,
        expectedInstallationGeneration: Long,
        expectedPackageRevisionRef: String,
        expectedAuthorityKey: String,
    ): AppActionLaunchReceipt {
        requireAuthority(expectedAuthorityKey)
        val response = client.post(
            "${appsBase()}/installations/${segment(installationId)}/actions/${segment(actionId)}/runs",
        ) {
            authorize()
            requireAuthority(expectedAuthorityKey)
            contentType(ContentType.Application.Json)
            setBody(emptyActionRequestJson(
                idempotencyKey,
                expectedInstallationGeneration,
                expectedPackageRevisionRef,
            ))
        }
        requireAuthority(expectedAuthorityKey)
        if (response.status != HttpStatusCode.Accepted) {
            response.successText(AppSurfacingLimits.MaximumActionResponseBytes, "launch app action")
            throw AppSurfacingApiException("App action launch returned an unexpected status.", response.status.value)
        }
        val receipt = appSurfacingJson.decodeFromString(
            AppActionLaunchReceipt.serializer(),
            response.successText(AppSurfacingLimits.MaximumActionResponseBytes, "launch app action"),
        ).checked(installationId, actionId)
        requireAuthority(expectedAuthorityKey)
        return receipt
    }

    /**
     * Bounded slot settings read. This is also how the client acquires the
     * write fence a mutation must present, so it is never cached or
     * conditionally revalidated: a stale head is a stale write.
     */
    internal suspend fun fetchSlotSettings(
        query: AppSlotSettingsQuery,
        expectedAuthorityKey: String,
    ): AppSlotSettingsPage {
        requireAuthority(expectedAuthorityKey)
        val checkedQuery = query.checked()
        val response = client.get("${appsBase()}/slot-assignments") {
            authorize()
            requireAuthority(expectedAuthorityKey)
            parameter("assignment_limit", checkedQuery.assignmentLimit)
            parameter("picker_limit", checkedQuery.pickerLimit)
            checkedQuery.assignmentCursor?.let { parameter("assignment_cursor", it) }
            checkedQuery.pickerCursor?.let { parameter("picker_cursor", it) }
        }
        requireAuthority(expectedAuthorityKey)
        val page = appSurfacingJson.decodeFromString(
            AppSlotSettingsPageWire.serializer(),
            response.successText(
                AppSurfacingLimits.MaximumSlotSettingsResponseBytes,
                "load the app widget picker",
            ),
        ).checked()
        // A page larger than the one requested is a host that did not honour
        // the bound, not a bonus. Refuse it rather than paginate from it.
        require(page.assignments.size <= checkedQuery.assignmentLimit)
        require(page.picker.size <= checkedQuery.pickerLimit)
        requireAuthority(expectedAuthorityKey)
        return page
    }

    /**
     * Applies one slot change under the exact revision and fence its settings
     * read returned. The receipt is bound to this request before it is
     * believed; an unbound receipt is a failure, not a success.
     */
    internal suspend fun mutateSlotAssignment(
        request: AppSlotAssignmentWriteRequest,
        expectedAuthorityKey: String,
    ): AppSlotAssignmentMutationReceipt {
        requireAuthority(expectedAuthorityKey)
        val body = slotMutationRequestJson(request)
        val response = client.post("${appsBase()}/slot-assignments") {
            authorize()
            requireAuthority(expectedAuthorityKey)
            contentType(ContentType.Application.Json)
            setBody(body)
        }
        requireAuthority(expectedAuthorityKey)
        val receipt = appSurfacingJson.decodeFromString(
            AppSlotAssignmentMutationReceiptWire.serializer(),
            response.successText(
                AppSurfacingLimits.MaximumSlotResponseBytes,
                "apply the app widget change",
            ),
        ).checked(request)
        requireAuthority(expectedAuthorityKey)
        return receipt
    }

    private suspend fun resolveSlots(
        slotIds: List<AppSlotId>,
        expectedAuthorityKey: String,
    ): List<AppResolvedSlotAssignment> {
        requireAuthority(expectedAuthorityKey)
        require(slotIds.isNotEmpty() && slotIds.size <= AppSurfacingLimits.MaximumSlotsPerPage)
        require(slotIds.toSet().size == slotIds.size)
        val response = client.post("${appsBase()}/slots/resolve-batch") {
            authorize()
            requireAuthority(expectedAuthorityKey)
            contentType(ContentType.Application.Json)
            setBody(appSurfacingJson.encodeToString(
                AppSlotResolutionBatchRequest(slotIds.map(AppSlotId::value)),
            ))
        }
        requireAuthority(expectedAuthorityKey)
        val decoded = appSurfacingJson.decodeFromString(
            AppSlotResolutionBatchResponse.serializer(),
            response.successText(
                AppSurfacingLimits.MaximumSlotBatchResponseBytes,
                "resolve app widget slots",
            ),
        )
        val checked = decoded.checked(slotIds)
        requireAuthority(expectedAuthorityKey)
        return checked
    }

    private fun requireAuthority(expectedAuthorityKey: String) {
        if (authorityKey() != expectedAuthorityKey) {
            throw AppSurfacingApiException("The paired Apps authority changed during the request.")
        }
    }

    private suspend fun HttpResponse.successText(maximumBytes: Int, action: String): String {
        val declared = headers[HttpHeaders.ContentLength]?.toLongOrNull()
        if (declared != null && declared > maximumBytes) {
            throw AppSurfacingApiException("The $action response exceeded its byte ceiling.", status.value)
        }
        // `bodyAsText` allocates the complete transfer before a post-read size
        // check. Read at most ceiling + 1 bytes from the channel instead, so a
        // chunked or dishonest response cannot make the handset buffer an
        // unbounded body.
        val body = bodyAsChannel().readBoundedUtf8(maximumBytes, action, status.value)
        if (status.isSuccess()) return body
        throw AppSurfacingApiException("Could not $action (HTTP ${status.value}).", status.value)
    }

    private fun segment(value: String): String =
        URLEncoder.encode(value, StandardCharsets.UTF_8.name()).replace("+", "%20")

    override fun close() {
        client.close()
    }

    companion object {
        const val WidgetRefreshAfterHeader = "X-App-Widget-Refresh-After"
        private const val HexDigits = "0123456789abcdef"
    }
}

/** Shortest wait before re-asking after a deadline that was already due. */
const val AppWidgetDeadlineFloorMillis = 2_000L

/**
 * The deadline a client schedules against for a widget batch.
 *
 * A deadline at or before now, or none at all, is accepted and pushed to
 * `now + floor` rather than treated as an error — the floor is never below
 * [AppSurfacingViewModel.MinimumRefreshMillis], so it cannot become a tight
 * loop. A later deadline is kept exactly.
 */
fun acceptedWidgetRefreshDeadline(now: Instant, deadline: Instant?): Instant {
    val floor = now.plusMillis(maxOf(AppWidgetDeadlineFloorMillis, AppSurfacingViewModel.MinimumRefreshMillis))
    return if (deadline == null || deadline.isBefore(floor)) floor else deadline
}

internal fun normalizedAppEtag(raw: String): String? {
    var value = raw.trim()
    if (',' in value) return null
    if (value.startsWith("W/")) value = value.removePrefix("W/")
    if (value.length >= 2 && value.first() == '"' && value.last() == '"') {
        value = value.substring(1, value.length - 1)
    }
    return value.takeIf { it.matches(Regex("blake3:[0-9a-f]{64}")) }
}

internal suspend fun ByteReadChannel.readBoundedUtf8(
    maximumBytes: Int,
    action: String,
    status: Int,
): String {
    require(maximumBytes > 0)
    val buffer = ByteArray(minOf(8 * 1024, maximumBytes + 1))
    val output = ByteArrayOutputStream(minOf(8 * 1024, maximumBytes))
    var total = 0
    while (true) {
        // Reading one byte beyond the ceiling detects an oversized stream
        // without ever allocating the remainder of the response.
        val count = readAvailable(
            buffer,
            0,
            minOf(buffer.size, maximumBytes - total + 1),
        )
        if (count == -1) break
        if (count == 0) continue
        total += count
        if (total > maximumBytes) {
            throw AppSurfacingApiException(
                "The $action response exceeded its byte ceiling.",
                status,
            )
        }
        output.write(buffer, 0, count)
    }
    return output.toByteArray().decodeToString(throwOnInvalidSequence = true)
}

@Serializable
private data class AppExpectedInstallationBinding(
    val generation: Long,
    @SerialName("package_revision_ref") val packageRevisionRef: String,
)

@Serializable
private data class AppDirectEmptyActionRequest(
    @SerialName("idempotency_key") val idempotencyKey: String,
    val input: Map<String, String> = emptyMap(),
    @SerialName("expected_installation_binding")
    val expectedInstallationBinding: AppExpectedInstallationBinding,
)

internal fun emptyActionRequestJson(
    idempotencyKey: String,
    expectedInstallationGeneration: Long,
    expectedPackageRevisionRef: String,
): String {
    require(idempotencyKey.startsWith("android-widget-") && idempotencyKey.length <= 128)
    require(expectedInstallationGeneration > 0)
    require(expectedPackageRevisionRef.length in 1..192 &&
        expectedPackageRevisionRef.matches(Regex("[A-Za-z0-9][A-Za-z0-9_.:/@#-]*")))
    return appSurfacingJson.encodeToString(AppDirectEmptyActionRequest(
        idempotencyKey = idempotencyKey,
        expectedInstallationBinding = AppExpectedInstallationBinding(
            generation = expectedInstallationGeneration,
            packageRevisionRef = expectedPackageRevisionRef,
        ),
    ))
}

internal val appSurfacingJson = Json {
    ignoreUnknownKeys = false
    isLenient = false
    coerceInputValues = false
    useAlternativeNames = false
    explicitNulls = false
    encodeDefaults = true
}
