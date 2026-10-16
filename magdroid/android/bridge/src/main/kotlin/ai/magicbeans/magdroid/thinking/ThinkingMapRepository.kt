package ai.magicbeans.magdroid.thinking

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
import io.ktor.client.statement.bodyAsText
import io.ktor.client.request.delete
import io.ktor.client.request.patch
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.http.ContentType
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.add
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray

/** Named once, so every failure sentence about the map list reads alike. */
private const val MAPS = "your maps"

/**
 * Why a map operation failed.
 *
 * Reads carry a classified [Failure] so the screen can offer Retry and tell a
 * stopped server apart from a rejected credential. Writes — "could not save
 * that change" — stay sentences, because the recovery is the owner's, not a
 * button's.
 */
class ThinkingMapError(override val failure: Failure) :
    Exception(failure.headline), CarriesFailure {

    constructor(message: String) : this(
        Failure(FailureKind.Unknown, message, "", retryable = true),
    )
}

/**
 * The map moved on before this change landed.
 *
 * Its own type because the recovery is different: a conflict is refetch and
 * retry, where an ordinary failure is tell the owner and stop.
 */
class ThinkingMapConflict(message: String) : Exception(message)

/** Steering for the interpreter, as the server names it. */
enum class FrontierIntent(val id: String) {
    ContinueThinking("continue_thinking"),
    BreakOpen("break_open"),
}

/**
 * What the owner does with a restructure proposal.
 *
 * `confirm`, not `accept` — the server's word, and the difference is a 400 that
 * would read as a decision that simply would not stick.
 */
enum class ProposalDecision(val id: String) {
    Confirm("confirm"),
    Reject("reject"),
}

/** A map's lifecycle, as the server names it. */
enum class MapLifecycle(val id: String) {
    Active("active"),
    Paused("paused"),
    Archived("archived"),
    Deleted("deleted"),
}

/**
 * Reads thinking maps.
 *
 * Read-only on purpose for now. The server exposes fifteen routes here —
 * operations, interpret, consolidate, proposals, replay, restore, promote — and
 * every one of them mutates a graph the owner is thinking in. Listing and
 * reading is the half that is safe to be wrong about, and it is also the half
 * a phone is actually good for.
 */
class ThinkingMapRepository(context: Context) {

    private val app = context.applicationContext

    private val client = HttpClient(CIO) {
        install(HttpTimeout) {
            requestTimeoutMillis = 30_000
            connectTimeoutMillis = 20_000
        }
    }

    suspend fun list(): List<ThinkingMapSummary> {
        val response = client.get("${base()}/thinking-maps") { authorize() }
        if (!response.status.isSuccess()) {
            throw ThinkingMapError(Failures.ofStatus(response.status.value, MAPS))
        }
        val body = response.bodyAsText()
        // A bare array of summaries. `MapSummary` is what `list_maps` returns —
        // not whole maps, which is why opening one fetches again.
        return runCatching {
            thinkingJson.decodeFromString(
                kotlinx.serialization.builtins.ListSerializer(ThinkingMapSummary.serializer()), body,
            )
        }.getOrElse { throw ThinkingMapError(Failures.garbled(MAPS)) }
    }

    suspend fun map(mapId: String): ThinkingMap {
        val response = client.get("${base()}/thinking-maps/$mapId") { authorize() }
        if (!response.status.isSuccess()) throw ThinkingMapError("Could not open that map.")
        return runCatching {
            thinkingJson.decodeFromString(ThinkingMap.serializer(), response.bodyAsText())
        }.getOrElse { throw ThinkingMapError("Could not open that map.") }
    }

    /**
     * Apply operations to a map.
     *
     * `base_revision` is optimistic concurrency: the server refuses the
     * envelope if the map has moved on, which is what stops two clients
     * silently overwriting each other's thinking. A conflict is reported as
     * itself so the caller can refetch and retry rather than assume it landed.
     *
     * `idempotency_key` is the caller's, and the server keeps a ledger of
     * applied envelopes — so a retry after a timeout replays instead of
     * duplicating the node.
     */
    suspend fun applyOperations(
        mapId: String,
        operations: List<JsonObject>,
        baseRevision: Long,
        idempotencyKey: String,
    ): ThinkingMap {
        val response = client.post("${base()}/thinking-maps/$mapId/operations") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(
                buildJsonObject {
                    putJsonArray("operations") { operations.forEach { add(it) } }
                    put("idempotency_key", idempotencyKey)
                    put("base_revision", baseRevision)
                }.toString(),
            )
        }
        val body = response.bodyAsText()
        if (response.status.value == 409) {
            throw ThinkingMapConflict("This map changed while you were editing it.")
        }
        if (!response.status.isSuccess()) throw ThinkingMapError("Could not save that change.")
        return runCatching {
            thinkingJson.decodeFromString(ThinkingMap.serializer(), body)
        }.getOrElse {
            // Applied, but the answer was unreadable. Refetching is the honest
            // recovery — reporting failure would invite a duplicate retry.
            map(mapId)
        }
    }

    /** Create a map, optionally seeded by the caller's own id. */
    suspend fun create(title: String, mapId: String? = null): ThinkingMap {
        val response = client.post("${base()}/thinking-maps") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(
                buildJsonObject {
                    put("title", title)
                    mapId?.let { put("map_id", it) }
                }.toString(),
            )
        }
        if (!response.status.isSuccess()) throw ThinkingMapError("Could not start that map.")
        return runCatching {
            thinkingJson.decodeFromString(ThinkingMap.serializer(), response.bodyAsText())
        }.getOrElse { throw ThinkingMapError("Could not start that map.") }
    }

    /**
     * Ask the interpreter to think alongside the owner.
     *
     * `focus_node_id` is sent explicitly rather than relying on a preceding
     * shared-view mutation, so interpretation never depends on winning a race
     * against it.
     */
    suspend fun interpret(
        mapId: String,
        text: String,
        intent: FrontierIntent = FrontierIntent.ContinueThinking,
        focusNodeId: String? = null,
        // Client-minted so progress events on the realtime bus can be matched
        // to this run. Absent, the server mints one the client never learns —
        // the interpretation still works, its narration just can't be claimed.
        utteranceId: String? = null,
    ) {
        val response = client.post("${base()}/thinking-maps/$mapId/interpret") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(
                buildJsonObject {
                    put("text", text)
                    put("intent", intent.id)
                    focusNodeId?.let { put("focus_node_id", it) }
                    utteranceId?.let { put("utterance_id", it) }
                }.toString(),
            )
        }
        if (!response.status.isSuccess()) throw ThinkingMapError("Magician could not pick that up.")
    }

    /** Accept or reject a restructure proposal. */
    suspend fun decideProposal(mapId: String, proposalId: String, decision: ProposalDecision) {
        val response = client.post(
            "${base()}/thinking-maps/$mapId/proposals/$proposalId/decision",
        ) {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(buildJsonObject { put("decision", decision.id) }.toString())
        }
        if (!response.status.isSuccess()) throw ThinkingMapError("Could not record that decision.")
    }

    /**
     * Rename a map, or move it through its lifecycle.
     *
     * At least one of the two must be set — the server rejects an empty patch —
     * so a caller that changes nothing never reaches the network.
     */
    suspend fun patch(mapId: String, title: String? = null, lifecycle: MapLifecycle? = null) {
        val newTitle = title?.trim()?.takeIf { it.isNotEmpty() }
        if (newTitle == null && lifecycle == null) return
        val response = client.patch("${base()}/thinking-maps/$mapId") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(
                buildJsonObject {
                    newTitle?.let { put("title", it) }
                    lifecycle?.let { put("lifecycle", it.id) }
                }.toString(),
            )
        }
        if (!response.status.isSuccess()) throw ThinkingMapError("Could not update that map.")
    }

    /**
     * Delete a map for good.
     *
     * Distinct from the `deleted` lifecycle that [patch] sets, which is
     * reversible and what the UI should reach for first. This is the server's
     * permanent removal, and the endpoint went uncalled by this client — a map
     * could be archived here but never actually got rid of.
     */
    suspend fun delete(mapId: String) {
        val response = client.delete("${base()}/thinking-maps/$mapId") { authorize() }
        if (!response.status.isSuccess()) throw ThinkingMapError("Could not delete that map.")
    }

    /** Ask the map to tidy itself — merge duplicates, settle what is settled. */
    suspend fun consolidate(mapId: String): ThinkingMap {
        val response = client.post("${base()}/thinking-maps/$mapId/consolidate") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody("{}")
        }
        if (!response.status.isSuccess()) throw ThinkingMapError("Could not consolidate that map.")
        return runCatching {
            thinkingJson.decodeFromString(ThinkingMap.serializer(), response.bodyAsText())
        }.getOrElse { map(mapId) }
    }

    /**
     * The map as it stood at a point in its history.
     *
     * Read-only: replay answers with a projection and changes nothing, which is
     * what makes it safe to browse before deciding whether to restore.
     */
    suspend fun replay(mapId: String, atSequence: Long): ThinkingMap {
        val response = client.get("${base()}/thinking-maps/$mapId/replay") {
            authorize()
            parameter("at_seq", atSequence)
        }
        if (!response.status.isSuccess()) throw ThinkingMapError("Could not replay that map.")
        return runCatching {
            thinkingJson.decodeFromString(ThinkingMap.serializer(), response.bodyAsText())
        }.getOrElse { throw ThinkingMapError("Could not replay that map.") }
    }

    /**
     * Fork a point in history into a new map.
     *
     * A new map rather than a rewind, which is the server's design and the
     * kinder one: restoring in place would throw away everything thought since,
     * and this leaves both to compare.
     */
    suspend fun restore(
        mapId: String,
        atSequence: Long,
        newMapId: String,
        newTitle: String,
    ): ThinkingMap {
        val response = client.post("${base()}/thinking-maps/$mapId/restore") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(
                buildJsonObject {
                    put("at_sequence", atSequence)
                    put("new_map_id", newMapId)
                    put("new_title", newTitle)
                }.toString(),
            )
        }
        if (!response.status.isSuccess()) throw ThinkingMapError("Could not restore that map.")
        return runCatching {
            thinkingJson.decodeFromString(ThinkingMap.serializer(), response.bodyAsText())
        }.getOrElse { map(newMapId) }
    }

    /**
     * The map as markdown.
     *
     * Provisional and superseded nodes are excluded by default: an export is
     * something somebody sends to another person, and unsettled thoughts read
     * as claims once they leave the map.
     */
    suspend fun exportMarkdown(
        mapId: String,
        includeProvisional: Boolean = false,
        includeSuperseded: Boolean = false,
    ): String {
        val response = client.get("${base()}/thinking-maps/$mapId/export/markdown") {
            authorize()
            parameter("include_provisional", includeProvisional.toString())
            parameter("include_superseded", includeSuperseded.toString())
        }
        if (!response.status.isSuccess()) throw ThinkingMapError("Could not export that map.")
        return response.bodyAsText()
    }

    /**
     * Turn a node into something that outlives the map — a task, a note.
     *
     * `confirm` is the second step of a two-phase flow: the first call answers
     * with what would happen, and only a confirmed one commits.
     */
    suspend fun promoteNode(
        mapId: String,
        nodeId: String,
        target: String,
        confirm: Boolean,
    ): String {
        val response = client.post(
            "${base()}/thinking-maps/$mapId/nodes/$nodeId/promote",
        ) {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(
                buildJsonObject {
                    put("target", target)
                    put("confirm", confirm)
                }.toString(),
            )
        }
        val body = response.bodyAsText()
        if (!response.status.isSuccess()) throw ThinkingMapError("Could not promote that node.")
        return body
    }

    /**
     * Attach a live session so its utterances map themselves.
     *
     * This is what makes a recorded conversation become a map without anybody
     * typing: the coordinator interprets each finalized utterance and applies
     * the operations.
     */
    suspend fun attachSession(mapId: String, sourceSessionId: String) {
        val response = client.post("${base()}/thinking-maps/$mapId/sessions") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(
                buildJsonObject { put("source_session_id", sourceSessionId) }.toString(),
            )
        }
        if (!response.status.isSuccess()) throw ThinkingMapError("Could not attach that session.")
    }

    suspend fun detachSession(mapId: String, sourceSessionId: String) {
        val response = client.delete("${base()}/thinking-maps/$mapId/sessions/$sourceSessionId") {
            authorize()
        }
        if (!response.status.isSuccess()) throw ThinkingMapError("Could not detach that session.")
    }

    private fun io.ktor.client.request.HttpRequestBuilder.authorize() {
        MagicianAccess.headers(app).forEach { (name, value) -> header(name, value) }
    }

    private fun base(): String {
        val host = MagicianAccess.baseUrl(app).trimEnd('/')
        if (host.isEmpty()) throw ThinkingMapError("No Magician host configured yet.")
        return "$host/api/magician/v2"
    }

    fun close() = client.close()
}
