package ai.magicbeans.magdroid.vibedev

import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.access.MagicianConnectionSnapshot
import ai.magicbeans.magdroid.attention.AttentionLane
import ai.magicbeans.magdroid.attention.AttentionRepository
import ai.magicbeans.magdroid.attention.isActionable
import ai.magicbeans.magdroid.attention.request
import ai.magicbeans.magdroid.chat.HitlResponseValue
import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.header
import io.ktor.client.request.request
import io.ktor.client.request.setBody
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.HttpMethod
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import kotlinx.serialization.KSerializer
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import java.net.URLEncoder
import java.nio.charset.StandardCharsets

/** A refused or failed VibeDev call; [message] is fit to show. */
class VibeDevError(val status: Int, message: String) : Exception(message)

/**
 * Everything the VibeDev screens read and do.
 *
 * An interface so the view models are tested against a fake; the app uses
 * [VibeDevRepository].
 */
interface VibeDevSource {
    suspend fun runHistory(limit: Int, cursor: String?): VibeDevRunHistory
    suspend fun liveRuns(): VibeDevLiveRuns
    suspend fun projects(): VibeDevProjects
    suspend fun projectRuns(projectId: String, limit: Int, offset: Int, query: String?): VibeDevRunsPage
    suspend fun crews(projectId: String): List<VibeDevCrew>
    suspend fun runTurns(runId: String): VibeDevRunTurns
    suspend fun timeline(turnTaskId: String, limit: Int = 200): VibeDevTimeline
    suspend fun changes(runId: String, turn: String? = null, path: String? = null): VibeDevRunChanges
    suspend fun repoSuggestions(): List<VibeDevRepoSuggestion>
    suspend fun codingProfiles(): List<VibeDevCodingProfile>

    suspend fun startRun(request: VibeDevRunRequest): VibeDevStartedRun
    suspend fun createProject(name: String, repoPath: String): VibeDevProject
    suspend fun updateProject(projectId: String, trusted: Boolean?, baseBranch: String?, location: String? = null): VibeDevProject
    /** `git init` + a first commit in the project folder; the project's health after. */
    suspend fun gitInit(projectId: String): VibeDevProjectHealth =
        throw VibeDevError(404, "This Magician cannot set up a git repository yet.")
    suspend fun control(liveTaskId: String, action: String, message: String? = null): VibeDevControlOutcome

    suspend fun createCrew(request: VibeDevCrewRequest): VibeDevCrew
    /** `start | stop | resolve | integrate | pull-request`. */
    suspend fun crewAction(projectId: String, crewId: String, action: String): VibeDevCrew
    suspend fun chooseCrew(projectId: String, crewId: String, laneIndex: Int?): VibeDevCrew
    /** Answer a crew's question (a user request) with one of its option ids. */
    suspend fun answerQuestion(requestId: String, optionId: String)

    suspend fun merge(rootTaskId: String)
    suspend fun openPullRequest(rootTaskId: String, title: String, body: String)
    suspend fun discard(rootTaskId: String)
    /**
     * A sandboxed run's diff approval, answered as the Attention lane does:
     * directly by [approvalId] when the live list named it, else found in the
     * Attention feed by the run's task ids.
     */
    suspend fun answerDiffApproval(taskIds: Collection<String>, approve: Boolean, approvalId: String? = null)

    /**
     * `GET /work/live`: runs, crews and Work tasks ranked once. Null from a
     * Magician that predates it (404); the coding runs alone stand in.
     */
    suspend fun workLive(): WorkLive? = null
    /** Follow an item; the followed ids after. 409 `follow_limit` past three. */
    suspend fun follow(itemId: String): List<String> = throw VibeDevError(404, "This Magician cannot follow work yet.")
    suspend fun unfollow(itemId: String): List<String> = throw VibeDevError(404, "This Magician cannot follow work yet.")
}

/**
 * The VibeDev REST client.
 *
 * Built like `WorkRepository`: Ktor CIO, a frozen connection snapshot (a
 * machine switch cannot send this bearer to the new host), the connection's
 * own headers (which carry the scope), and a path guard that admits only the
 * v2 API.
 */
class VibeDevRepository(
    connection: MagicianConnectionSnapshot,
    private val client: HttpClient = HttpClient(CIO) {
        followRedirects = false
        install(HttpTimeout) { requestTimeoutMillis = 30_000; connectTimeoutMillis = 10_000 }
    },
    /** Diff approvals go through the Attention lane's own call. */
    private val attention: (() -> AttentionRepository)? = null,
) : VibeDevSource, AutoCloseable {
    private val connection = connection.copy(headers = connection.headers.toMap())

    fun isCurrentConnection(context: Context): Boolean = connection == MagicianAccess.connectionSnapshot(context)
    override fun close() = client.close()

    // ── Reads ──────────────────────────────────────────────────────────────

    override suspend fun runHistory(limit: Int, cursor: String?): VibeDevRunHistory {
        val params = mutableListOf("limit" to limit.coerceIn(1, 50).toString())
        if (cursor != null) params += "cursor" to cursor
        return get("/vibedev/runs${queryString(params)}", VibeDevRunHistory.serializer())
    }

    override suspend fun liveRuns(): VibeDevLiveRuns =
        get("/vibedev/runs/live", VibeDevLiveRuns.serializer())

    override suspend fun projects(): VibeDevProjects =
        get("/vibedev/projects", VibeDevProjects.serializer())

    override suspend fun projectRuns(projectId: String, limit: Int, offset: Int, query: String?): VibeDevRunsPage {
        val params = buildList {
            add("limit" to limit.coerceIn(1, 50).toString())
            if (offset > 0) add("offset" to offset.toString())
            query?.trim()?.takeIf { it.isNotEmpty() }?.let { add("query" to it) }
        }
        return get("/vibedev/projects/${segment(projectId)}/runs${queryString(params)}", VibeDevRunsPage.serializer())
    }

    override suspend fun crews(projectId: String): List<VibeDevCrew> =
        get("/vibedev/projects/${segment(projectId)}/crews", VibeDevCrews.serializer()).crews

    override suspend fun runTurns(runId: String): VibeDevRunTurns =
        get("/vibedev/runs/${segment(runId)}/turns", VibeDevRunTurns.serializer())

    override suspend fun timeline(turnTaskId: String, limit: Int): VibeDevTimeline =
        get("/vibedev/runs/${segment(turnTaskId)}/timeline?limit=${limit.coerceIn(1, 400)}", VibeDevTimeline.serializer())

    override suspend fun changes(runId: String, turn: String?, path: String?): VibeDevRunChanges {
        val params = buildList {
            turn?.takeIf { it.isNotBlank() }?.let { add("turn" to it) }
            path?.takeIf { it.isNotBlank() }?.let { add("path" to it) }
        }
        return get("/vibedev/runs/${segment(runId)}/changes${queryString(params)}", VibeDevRunChanges.serializer())
    }

    override suspend fun repoSuggestions(): List<VibeDevRepoSuggestion> =
        get("/vibedev/repos/suggest", VibeDevRepoSuggestions.serializer()).repos

    override suspend fun codingProfiles(): List<VibeDevCodingProfile> =
        get("/coding/models", VibeDevModelCatalog.serializer()).rows()

    override suspend fun workLive(): WorkLive? = try {
        get("/work/live", WorkLive.serializer())
    } catch (error: VibeDevError) {
        if (error.status == 404) null else throw error
    }

    // ── Writes ─────────────────────────────────────────────────────────────

    override suspend fun startRun(request: VibeDevRunRequest): VibeDevStartedRun {
        val body = buildJsonObject {
            put("prompt", request.prompt)
            put("project_id", request.projectId)
            put("mode", request.mode)
            put("client_run_id", request.clientRunId)
            request.parentTaskId?.let { put("parent_task_id", it); put("threaded", true) }
            request.codingChoice?.let { put("coding_choice", vibeDevJson.encodeToJsonElement(VibeDevCodingSelection.serializer(), it)) }
            request.costBudgetUsd?.let { put("cost_budget_usd", it) }
            put("auto_apply", false)
            if (request.mode == "discuss") put("agent_id", "cto")
            if (request.attachments.isNotEmpty()) {
                put("attachments", kotlinx.serialization.json.buildJsonArray {
                    request.attachments.forEach { attachment -> attachment.remoteId?.let { id ->
                        add(buildJsonObject { put("attachment_id", id); put("filename", attachment.name)
                            put("mime_type", attachment.mimeType ?: "application/octet-stream") })
                    } }
                })
                request.attachmentSessionId?.let { put("attachment_session_id", it) }
            }
            if (request.nightly && request.mode == "autopilot" && request.parentTaskId == null) {
                put("schedule", buildJsonObject {
                    put("kind", buildJsonObject { put("Cron", buildJsonObject { put("expression", "0 2 * * *"); put("timezone", "UTC") }) })
                    put("timezone", "UTC"); put("missed_fire_policy", "skip"); put("concurrent_execution_policy", "skip")
                })
            }
        }
        return decode(send("/vibedev/runs", HttpMethod.Post, body).second, VibeDevStartedRun.serializer())
    }

    override suspend fun createProject(name: String, repoPath: String): VibeDevProject {
        val body = buildJsonObject { put("name", name); put("repo_path", repoPath) }
        return decode(send("/vibedev/projects", HttpMethod.Post, body).second, VibeDevProjectMutation.serializer()).project
    }

    override suspend fun updateProject(projectId: String, trusted: Boolean?, baseBranch: String?, location: String?): VibeDevProject {
        val body = buildJsonObject {
            if (location != null) put("trust", location) else trusted?.let { put("trust", if (it) "trusted" else "sandboxed") }
            baseBranch?.let { put("base_branch", it) }
        }
        return decode(
            send("/vibedev/projects/${segment(projectId)}", HttpMethod.Patch, body).second,
            VibeDevProjectMutation.serializer(),
        ).project
    }

    override suspend fun gitInit(projectId: String): VibeDevProjectHealth {
        val (_, value) = send("/vibedev/projects/${segment(projectId)}/git-init", HttpMethod.Post, null)
        return VibeDevProjectHealth.decode((value as? JsonObject)?.get("health"))
    }

    override suspend fun control(liveTaskId: String, action: String, message: String?): VibeDevControlOutcome {
        val body = buildJsonObject {
            put("action", action)
            message?.trim()?.takeIf { it.isNotEmpty() }?.let { put("message", it) }
        }
        val (status, value) = send("/vibedev/runs/${segment(liveTaskId)}/control", HttpMethod.Post, body, allowConflict = true)
        if (status == 409) return VibeDevControlOutcome.BetweenTurns
        val delivered = (value as? JsonObject)?.get("delivered")?.jsonPrimitive?.contentOrNull
        return if (delivered == "false") VibeDevControlOutcome.BetweenTurns else VibeDevControlOutcome.Delivered
    }

    override suspend fun createCrew(request: VibeDevCrewRequest): VibeDevCrew {
        val body = buildJsonObject {
            put("goal", request.goal)
            put("mode", request.mode)
            request.racers?.let { put("racers", it) }
            put("review", request.review)
            request.budgetUsd?.let { put("budget_usd", it) }
            // The owner starts it from Now, where the plan is visible first.
            put("start", false)
        }
        return decode(
            send("/vibedev/projects/${segment(request.projectId)}/crews", HttpMethod.Post, body).second,
            VibeDevCrewEnvelope.serializer(),
        ).crew
    }

    override suspend fun crewAction(projectId: String, crewId: String, action: String): VibeDevCrew {
        require(action in CREW_ACTIONS) { "unknown crew action $action" }
        return decode(
            send("/vibedev/projects/${segment(projectId)}/crews/${segment(crewId)}/$action", HttpMethod.Post, buildJsonObject {}).second,
            VibeDevCrewEnvelope.serializer(),
        ).crew
    }

    override suspend fun chooseCrew(projectId: String, crewId: String, laneIndex: Int?): VibeDevCrew {
        val body = JsonObject(mapOf("lane_index" to (laneIndex?.let(::JsonPrimitive) ?: JsonNull)))
        return decode(
            send("/vibedev/projects/${segment(projectId)}/crews/${segment(crewId)}/choose", HttpMethod.Post, body).second,
            VibeDevCrewEnvelope.serializer(),
        ).crew
    }

    override suspend fun answerQuestion(requestId: String, optionId: String) {
        val body = buildJsonObject {
            put("source", "user_request")
            put("value", HitlResponseValue.Choice(selectedId = optionId).toJson())
            put("input_type", "choice")
            put("channel", "android")
        }
        val (_, value) = send("/hitl/${segment(requestId)}/respond", HttpMethod.Post, body)
        val accepted = (value as? JsonObject)?.get("accepted")?.jsonPrimitive?.contentOrNull
        if (accepted == "false") throw VibeDevError(409, "That question was already answered.")
    }

    override suspend fun merge(rootTaskId: String) {
        send("/vibedev/runs/${segment(rootTaskId)}/merge", HttpMethod.Post, buildJsonObject {})
    }

    override suspend fun openPullRequest(rootTaskId: String, title: String, body: String) {
        send(
            "/vibedev/runs/${segment(rootTaskId)}/pull-request", HttpMethod.Post,
            buildJsonObject { put("title", title); put("body", body) },
        )
    }

    override suspend fun discard(rootTaskId: String) {
        send("/vibedev/runs/${segment(rootTaskId)}/discard", HttpMethod.Post, buildJsonObject {})
    }

    override suspend fun answerDiffApproval(taskIds: Collection<String>, approve: Boolean, approvalId: String?) {
        approvalId?.trim()?.takeIf { it.isNotEmpty() }?.let { id ->
            val body = buildJsonObject {
                put("source", "diff_approval")
                put("value", HitlResponseValue.Choice(selectedId = if (approve) "apply" else "reject").toJson())
                put("input_type", "diff_approval")
                put("channel", "android")
            }
            val (_, value) = send("/hitl/${segment(id)}/respond", HttpMethod.Post, body)
            val result = value as? JsonObject
            if (result?.get("accepted")?.jsonPrimitive?.contentOrNull == "false") {
                val reason = (result["reason"] as? JsonPrimitive)?.contentOrNull
                throw VibeDevError(409, if (reason == "already_resolved") "That change was already answered." else "Magician did not accept the answer.")
            }
            return
        }
        val repository = attention?.invoke()
            ?: throw VibeDevError(0, "Open Attention to review this change.")
        try {
            val item = repository.feed().items(AttentionLane.All).firstOrNull { item ->
                item.isActionable && item.taskId in taskIds &&
                    item.request(item.metadata).inputType == "diff_approval"
            } ?: throw VibeDevError(404, "No change is waiting for approval on this run.")
            repository.respond(
                item.request(item.metadata),
                HitlResponseValue.Choice(selectedId = if (approve) "apply" else "reject"),
                taskId = item.taskId,
            )
        } finally {
            repository.close()
        }
    }

    override suspend fun follow(itemId: String): List<String> {
        val (status, value) = send("/work/follow", HttpMethod.Post, buildJsonObject { put("item_id", itemId) }, allowConflict = true)
        if (status == 409) {
            val body = value as? JsonObject
            val error = (body?.get("error") as? JsonPrimitive)?.contentOrNull
            throw VibeDevError(409, if (error == "follow_limit") WorkFollows.FOLLOW_LIMIT_MESSAGE else failureMessage(409, body))
        }
        return followedIds(value)
    }

    override suspend fun unfollow(itemId: String): List<String> =
        followedIds(send("/work/follow/${segment(itemId)}", HttpMethod.Delete, null).second)

    private fun followedIds(value: JsonElement): List<String> =
        ((value as? JsonObject)?.get("followed") as? JsonArray).orEmpty()
            .mapNotNull { (it as? JsonPrimitive)?.contentOrNull?.trim()?.takeIf(String::isNotEmpty) }

    // ── Transport ──────────────────────────────────────────────────────────

    private suspend fun <T> get(path: String, serializer: KSerializer<T>): T =
        decode(send(path, HttpMethod.Get, null).second, serializer)

    private fun <T> decode(value: JsonElement, serializer: KSerializer<T>): T =
        runCatching { vibeDevJson.decodeFromJsonElement(serializer, value) }
            .getOrElse { throw VibeDevError(0, "This VibeDev response could not be read.") }

    private suspend fun send(
        relative: String,
        method: HttpMethod,
        body: JsonObject?,
        allowConflict: Boolean = false,
    ): Pair<Int, JsonElement> {
        val path = API_PREFIX + relative
        require(path.startsWith(API_PREFIX) && !relative.contains("..")) { "VibeDev path outside the v2 API" }
        val response = client.request(connection.baseUrl.trimEnd('/') + path) {
            this.method = method
            connection.headers.forEach { (name, value) -> header(name, value) }
            if (body != null) { contentType(ContentType.Application.Json); setBody(body.toString()) }
        }
        val text = response.bodyAsText()
        val value = runCatching { vibeDevJson.parseToJsonElement(text) }.getOrNull()
        val status = response.status.value
        if (!response.status.isSuccess() && !(allowConflict && status == 409)) {
            throw VibeDevError(status, failureMessage(status, value as? JsonObject))
        }
        return status to (value ?: JsonObject(emptyMap()))
    }

    companion object {
        const val API_PREFIX = "/api/magician/v2"
        private val CREW_ACTIONS = setOf("start", "stop", "resolve", "integrate", "pull-request")

        fun connected(context: Context): VibeDevRepository = VibeDevRepository(
            MagicianAccess.connectionSnapshot(context)
                ?: throw VibeDevError(0, "Connect to your Magician in Settings."),
            attention = { AttentionRepository(context.applicationContext) },
        )

        fun segment(value: String): String =
            URLEncoder.encode(value, StandardCharsets.UTF_8.name()).replace("+", "%20")

        internal fun queryString(params: List<Pair<String, String>>): String =
            if (params.isEmpty()) "" else params.joinToString("&", prefix = "?") { (key, value) -> "$key=${segment(value)}" }

        /**
         * The server's own sentence where it sent one (a landing action's 403
         * says why), else a plain one for the status.
         */
        internal fun failureMessage(status: Int, body: JsonObject?): String {
            fun text(key: String) = (body?.get(key) as? JsonPrimitive)?.contentOrNull?.takeIf { it.isNotBlank() }
            val nested = (body?.get("error") as? JsonObject)?.get("message")?.let { (it as? JsonPrimitive)?.contentOrNull }
            return text("message") ?: text("detail") ?: nested ?: when (status) {
                401 -> "Your connection cannot reach VibeDev. Check Settings."
                403 -> "Only the owner can do that, from the owner's terminal."
                404 -> "That VibeDev item is gone."
                409 -> "That changed elsewhere. Refresh and try again."
                else -> text("error") ?: "VibeDev request failed ($status)."
            }
        }
    }
}

/**
 * One repository per process, rebuilt when the active machine changes, so
 * every VibeDev screen shares one client and none talks to the old host.
 */
object VibeDevConnection {
    @Volatile private var current: VibeDevRepository? = null

    fun source(context: Context): VibeDevSource = synchronized(this) {
        val app = context.applicationContext
        current?.takeIf { it.isCurrentConnection(app) } ?: run {
            current?.close()
            VibeDevRepository.connected(app).also { current = it }
        }
    }
}
