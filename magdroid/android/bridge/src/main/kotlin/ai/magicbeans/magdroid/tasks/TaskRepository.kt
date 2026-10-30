package ai.magicbeans.magdroid.tasks

import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.realtime.HubDelivery
import ai.magicbeans.magdroid.realtime.HubFilter
import ai.magicbeans.magdroid.realtime.RealtimeHub
import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.plugins.timeout
import io.ktor.client.request.HttpRequestBuilder
import io.ktor.client.request.delete
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.parameter
import io.ktor.client.request.patch
import io.ktor.client.request.post
import io.ktor.client.request.put
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.HttpStatusCode
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.mapNotNull
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray
import kotlinx.serialization.json.putJsonObject
import java.net.URLEncoder
import java.nio.charset.StandardCharsets

val taskJson: Json = Json {
    ignoreUnknownKeys = true
    explicitNulls = false
    coerceInputValues = true
    encodeDefaults = true
}

class TaskApiError(message: String, val status: Int = 0) : Exception(message)

/** Injectable task/monitor boundary used by the ViewModel and pure JVM tests. */
interface TasksDataSource {
    suspend fun listTasks(
        lane: TaskLane,
        filter: TaskFilter?,
        today: String,
        offset: Int,
        limit: Int,
        query: String? = null,
    ): TaskListResponse

    suspend fun listInternalTasks(query: InternalTaskQuery): TaskListResponse = listTasks(TaskLane.Internal, null, "", query.offset, query.limit, query.search)
    suspend fun taskThread(id: String): TaskThreadChoice = throw TaskApiError("The destination thread could not be loaded.")
    suspend fun taskSchedule(taskId: String): JsonObject? = throw TaskApiError("The current schedule could not be loaded.")
    fun knownCrewOptions(): List<AgentOption> = emptyList()
    suspend fun agents(): List<AgentOption>
    suspend fun createTask(draft: TaskCreateDraft): TaskV3?
    suspend fun execute(taskId: String)
    suspend fun preplan(taskId: String)
    suspend fun setStatus(taskId: String, status: String)
    suspend fun updateTask(taskId: String, fields: JsonObject)
    suspend fun deleteTask(task: TaskV3, removeFiles: Boolean)
    suspend fun planAction(taskId: String, planId: String?, action: String)
    suspend fun retrySynthesis(taskId: String, executionId: String)
    suspend fun publishToNotes(taskId: String)
    suspend fun executionControlState(executionId: String): ExecutionControlState
    suspend fun executionControl(executionId: String, action: ExecutionControlAction, guidance: String? = null)
    suspend fun taskDetailsPage(taskId: String, cursor: String): JsonObject = error("History paging is unavailable")
    suspend fun taskDetail(taskId: String, executionId: String? = null): TaskDetailBundle

    suspend fun listMonitors(limit: Int, cursor: String?, state: String?): MonitorListPage
    suspend fun monitorIdentity(taskId: String): MonitorDetail
    suspend fun monitorDetail(taskId: String): MonitorDetailBundle
    suspend fun createMonitor(draft: MonitorDraft): String
    suspend fun convertToMonitor(taskId: String, draft: MonitorDraft)
    suspend fun updateMonitor(taskId: String, draft: MonitorDraft)
    suspend fun monitorAction(taskId: String, action: String)
    suspend fun deleteMonitor(taskId: String, removeFiles: Boolean)
    suspend fun monitorFeedback(taskId: String, updateId: String, verdict: String)

    fun taskEvents(): Flow<TaskRealtimeEvent>
}

/**
 * Production implementation. Every request uses the same scope and Cloudflare
 * identity as chat and the Android bridge; Tasks must not become a second login.
 */
class TaskRepository(private val context: Context) : TasksDataSource, AutoCloseable {
    private val client = HttpClient(CIO) {
        install(HttpTimeout) {
            requestTimeoutMillis = 300_000
            socketTimeoutMillis = 300_000
            connectTimeoutMillis = 20_000
        }
    }
    private val hub = RealtimeHub.shared(context)
    override fun close() = client.close()

    private var allCrewOptions = emptyList<AgentOption>()
    override fun knownCrewOptions(): List<AgentOption> = allCrewOptions
    private val connection = MagicianAccess.connectionSnapshot(context)
    private fun host(): String {
        val frozen = connection ?: throw TaskApiError("No Magician host configured yet.")
        if (frozen != MagicianAccess.connectionSnapshot(context)) throw TaskApiError("Connection changed. Reopen this Work view.")
        return frozen.baseUrl
    }

    private fun v3(): String = "${host()}/api/magician/v3"
    private fun v2(): String = "${host()}/api/magician/v2"

    private fun HttpRequestBuilder.authorize() {
        host()
        connection!!.headers.forEach { (name, value) -> header(name, value) }
    }

    override suspend fun listTasks(
        lane: TaskLane,
        filter: TaskFilter?,
        today: String,
        offset: Int,
        limit: Int,
        query: String?,
    ): TaskListResponse {
        require(lane != TaskLane.Monitors)
        val path = if (lane == TaskLane.Internal) "/tasks/internal" else "/tasks"
        val response = client.get(v3() + path) {
            authorize()
            parameter("limit", limit.coerceIn(1, 500))
            parameter("offset", offset.coerceAtLeast(0))
            parameter("sort", "updated_at")
            parameter("order", "desc")
            if (lane == TaskLane.Tasks) {
                parameter("today", today)
                filter?.let { parameter("view", it.wire) }
            }
            query?.takeIf { it.isNotBlank() }?.let { parameter("query", it.trim()) }
        }
        return response.decode(TaskListResponse.serializer(), "load tasks")
    }

    override suspend fun listInternalTasks(query: InternalTaskQuery): TaskListResponse {
        val response = client.get(v3() + query.requestPath()) { authorize() }
        return response.decode(TaskListResponse.serializer(), "load internal work")
    }

    override suspend fun taskThread(id: String): TaskThreadChoice {
        val response = client.get(v2() + "/ui-threads/${segment(id)}") { authorize() }
        val root = response.json("check destination thread").jsonObject
        val row = root["thread"] as? JsonObject ?: root
        val actual = (row["id"] as? JsonPrimitive)?.contentOrNull
        check(actual == id) { "The destination thread could not be loaded." }
        host()
        return TaskThreadChoice(id, (row["name"] as? JsonPrimitive)?.contentOrNull ?: id, (row["archived"] as? JsonPrimitive)?.booleanOrNull == true)
    }
    override suspend fun taskSchedule(taskId: String): JsonObject? {
        val response = client.get(v3() + "/tasks/${segment(taskId)}") { authorize() }
        val root = response.json("load current schedule").jsonObject
        val manifest = (root["task"] as? JsonObject)?.get("manifest") as? JsonObject
        check((manifest?.get("task_id") as? JsonPrimitive)?.content == taskId) { "The current task could not be loaded." }
        host()
        return manifest?.get("schedule") as? JsonObject
    }
    override suspend fun taskDetailsPage(taskId: String, cursor: String): JsonObject {
        val response = client.get(v3() + "/tasks/${segment(taskId)}/details") { authorize(); parameter("cursor", cursor) }
        return response.json("load older runs").jsonObject
    }

    override suspend fun agents(): List<AgentOption> {
        val response = client.get("${v2()}/agents") { authorize() }
        val root = response.json("load agents")
        host()
        allCrewOptions = decodeTaskAgentOptions(root, personalOnly = false)
        return decodeTaskAgentOptions(root)
    }

    override suspend fun createTask(draft: TaskCreateDraft): TaskV3? {
        val body = buildJsonObject {
            put("title", draft.title.trim())
            put("description", draft.description)
            put("agent_id", draft.agentId)
            put("ui_thread_id", draft.threadId.ifBlank { "general" })
            put("created_by", "user")
            put("output_mode", draft.outputMode)
            draft.priority?.let { put("priority", it) }
            draft.dueDate?.let { put("due_date", it) }
            if (draft.tagNames.isNotEmpty()) putJsonArray("tags") {
                draft.tagNames.distinct().forEach { name ->
                    add(buildJsonObject { put("id", name); put("name", name) })
                }
            }
            if (draft.dependsOn.isNotEmpty()) putJsonArray("depends_on") {
                draft.dependsOn.distinct().forEach { add(JsonPrimitive(it)) }
            }
            draft.schedule?.let { put("schedule", it) }
        }
        val response = client.post("${v3()}/tasks") {
            authorize(); contentType(ContentType.Application.Json); setBody(body.toString())
        }
        val text = response.successText("create task")
        return runCatching { taskJson.decodeFromString(TaskV3.serializer(), text) }.getOrNull()
            ?: runCatching {
                val objectRoot = taskJson.parseToJsonElement(text).jsonObject
                taskJson.decodeFromJsonElement(TaskV3.serializer(), objectRoot["task"]!!)
            }.getOrNull()
    }

    private fun requireTaskLifecycleWrite(taskId: String) { check(!taskId.startsWith("agent-cycle:") && !Regex("^task_app_[0-9a-f]{64}$").matches(taskId)) { "This record is managed by its runtime or App. Open its owner to manage its lifecycle." } }
    override suspend fun execute(taskId: String) { requireTaskLifecycleWrite(taskId); postV3("/tasks/${segment(taskId)}/execute") }
    override suspend fun preplan(taskId: String) { requireTaskLifecycleWrite(taskId); postV3("/tasks/${segment(taskId)}/plan") }

    override suspend fun setStatus(taskId: String, status: String) {
        requireTaskLifecycleWrite(taskId)
        putV3("/tasks/${segment(taskId)}/status", buildJsonObject { put("status", status) })
    }

    override suspend fun updateTask(taskId: String, fields: JsonObject) {
        requireTaskLifecycleWrite(taskId)
        putV3("/tasks/${segment(taskId)}", fields)
    }

    override suspend fun deleteTask(task: TaskV3, removeFiles: Boolean) {
        check(!task.isAppManaged) { "This run is managed by its App." }
        val path = if (task.isInternal) "/tasks/internal/${segment(task.id)}" else "/tasks/${segment(task.id)}"
        val response = client.delete(v3() + path) {
            authorize()
            if (!task.isInternal) parameter("remove_files", removeFiles)
        }
        response.successText("delete task")
    }

    override suspend fun planAction(taskId: String, planId: String?, action: String) {
        requireTaskLifecycleWrite(taskId)
        val suffix = when (action) {
            "approve", "reject" -> "/plan/$action"
            "replan" -> "/plan/replan"
            else -> throw IllegalArgumentException("Unknown plan action: $action")
        }
        val response = client.post("${v3()}/tasks/${segment(taskId)}$suffix") {
            authorize(); contentType(ContentType.Application.Json); setBody("{}")
            if (action in setOf("approve", "reject")) parameter("plan_id", planId.orEmpty())
        }
        response.successText("$action plan")
    }

    override suspend fun retrySynthesis(taskId: String, executionId: String) {
        requireTaskLifecycleWrite(taskId)
        postV3("/tasks/${segment(taskId)}/executions/${segment(executionId)}/retry-synthesis")
    }

    override suspend fun publishToNotes(taskId: String) {
        val response = client.post("${v2()}/notes/publish/task/${segment(taskId)}") {
            authorize(); contentType(ContentType.Application.Json); setBody("{}")
        }
        response.successText("publish task to Notes")
    }

    override suspend fun executionControlState(executionId: String): ExecutionControlState {
        val response = client.get("${v2()}/executions/${segment(executionId)}/control-state") { authorize() }
        return response.decode(ExecutionControlState.serializer(), "load execution controls")
    }

    override suspend fun executionControl(
        executionId: String,
        action: ExecutionControlAction,
        guidance: String?,
    ) {
        val response = client.post("${v2()}/executions/${segment(executionId)}/${action.wire}") {
            authorize()
            if (action == ExecutionControlAction.Steer) {
                contentType(ContentType.Application.Json)
                setBody(buildJsonObject { put("message", guidance.orEmpty()) }.toString())
            }
        }
        response.successText(action.wire + " execution")
    }

    override suspend fun taskDetail(taskId: String, executionId: String?): TaskDetailBundle = coroutineScope {
        suspend fun fetch(path: String): Result<JsonObject> = runCatching {
            client.get(v3() + path) {
                authorize()
                timeout {
                    requestTimeoutMillis = 15_000
                    socketTimeoutMillis = 15_000
                }
            }.json("load task detail").jsonObject
        }
        val requests = taskDetailRequestPaths(taskId, executionId).mapValues { (_, path) -> async { fetch(path) } }
        val results = requests.mapValues { (_, response) -> response.await() }
        assembleTaskDetail(results)
    }

    override suspend fun listMonitors(limit: Int, cursor: String?, state: String?): MonitorListPage {
        val response = client.get("${v3()}/monitors") {
            authorize(); parameter("limit", limit.coerceIn(1, 200))
            cursor?.takeIf(String::isNotBlank)?.let { parameter("cursor", it) }
            state?.takeIf(String::isNotBlank)?.let { parameter("state", it) }
        }
        return response.decode(MonitorListPage.serializer(), "load monitors")
    }

    override suspend fun monitorIdentity(taskId: String): MonitorDetail =
        client.get("${v3()}/monitors/${segment(taskId)}") { authorize() }
            .decode(MonitorDetail.serializer(), "load monitor")

    override suspend fun monitorDetail(taskId: String): MonitorDetailBundle = coroutineScope {
        val id = segment(taskId)
        // The identity document is the only required section. Do not fan out
        // optional reads for a missing or unauthorized monitor.
        val loadedDetail = monitorIdentity(taskId)
        val updates = async { runCatching {
            client.get("${v3()}/monitors/$id/updates") { authorize(); parameter("limit", 50) }
                .decode(ItemsPage.serializer(MonitorUpdate.serializer()), "load monitor updates").items
        }.getOrDefault(emptyList()) }
        val runs = async { runCatching {
            client.get("${v3()}/monitors/$id/runs") { authorize(); parameter("limit", 50) }
                .decode(ItemsPage.serializer(MonitorRun.serializer()), "load monitor runs").items
        }.getOrDefault(emptyList()) }
        val feedback = async { runCatching {
            client.get("${v3()}/monitors/$id/feedback") { authorize(); parameter("limit", 50) }
                .decode(ItemsPage.serializer(MonitorFeedbackRecord.serializer()), "load monitor feedback").items
        }.getOrDefault(emptyList()) }
        val records = feedback.await().sortedWith(compareByDescending<MonitorFeedbackRecord> {
            runCatching { java.time.Instant.parse(it.recordedAt) }.getOrNull()
        })
        MonitorDetailBundle(
            detail = loadedDetail, updates = updates.await(), runs = runs.await(),
            feedbackByUpdate = records.distinctBy(MonitorFeedbackRecord::updateId)
                .associate { it.updateId to it.verdict },
        )
    }

    override suspend fun createMonitor(draft: MonitorDraft): String {
        draft.validationError()?.let { throw TaskApiError(it) }
        val body = monitorBody(draft, includeSchedule = true)
        val response = client.post("${v3()}/monitors") {
            authorize(); contentType(ContentType.Application.Json); setBody(body.toString())
        }
        val root = response.json("create monitor").jsonObject
        return root["task_id"]?.stringValue() ?: throw TaskApiError("Monitor was created without an id.")
    }

    override suspend fun convertToMonitor(taskId: String, draft: MonitorDraft) {
        draft.validationError()?.let { throw TaskApiError(it) }
        val body = buildJsonObject {
            put("spec", taskJson.encodeToJsonElement(MonitorSpec.serializer(), draft.spec()))
            draft.title.trim().takeIf(String::isNotEmpty)?.let { put("title", it) }
        }
        val response = client.post("${v3()}/monitors/${segment(taskId)}/convert") {
            authorize(); contentType(ContentType.Application.Json); setBody(body.toString())
        }
        response.successText("convert task to monitor")
    }

    override suspend fun updateMonitor(taskId: String, draft: MonitorDraft) {
        draft.validationError()?.let { throw TaskApiError(it) }
        val response = client.patch("${v3()}/monitors/${segment(taskId)}") {
            authorize(); contentType(ContentType.Application.Json); setBody(monitorBody(draft, true).toString())
        }
        response.successText("update monitor")
    }

    override suspend fun monitorAction(taskId: String, action: String) {
        require(action in setOf("pause", "resume", "run"))
        postV3("/monitors/${segment(taskId)}/$action")
    }

    override suspend fun deleteMonitor(taskId: String, removeFiles: Boolean) {
        val response = client.delete("${v3()}/monitors/${segment(taskId)}") {
            authorize(); parameter("remove_files", removeFiles)
        }
        response.successText("delete monitor")
    }

    override suspend fun monitorFeedback(taskId: String, updateId: String, verdict: String) {
        val response = client.post(
            "${v3()}/monitors/${segment(taskId)}/updates/${segment(updateId)}/feedback",
        ) {
            authorize(); contentType(ContentType.Application.Json)
            setBody(buildJsonObject { put("verdict", verdict) }.toString())
        }
        response.successText("record monitor feedback")
    }

    /**
     * Task-family events off the app's one [RealtimeHub] connection.
     *
     * A hub resync arrives as a [TaskRealtimeEvent] with `resync = true`:
     * events may have been missed, and every consumer re-reads by REST.
     */
    override fun taskEvents(): Flow<TaskRealtimeEvent> = hub.subscribe(HubFilter.ALL).mapNotNull { delivery ->
        if (connection == null || connection != MagicianAccess.connectionSnapshot(context)) return@mapNotNull null
        when (delivery) {
            is HubDelivery.Resync -> TaskRealtimeEvent(eventType = TaskRealtimeEvent.RESYNC, resync = true)
            is HubDelivery.Event -> parseTaskRealtimeEvent(
                delivery.event.root,
                MagicianAccess.principal(context),
                MagicianAccess.workspace(context),
            )
        }
    }

    private suspend fun postV3(path: String) {
        val response = client.post(v3() + path) {
            authorize(); contentType(ContentType.Application.Json); setBody("{}")
        }
        response.successText(path.substringAfterLast('/'))
    }

    private suspend fun putV3(path: String, body: JsonObject) {
        val response = client.put(v3() + path) {
            authorize(); contentType(ContentType.Application.Json); setBody(body.toString())
        }
        response.successText("update task")
    }

    private fun monitorBody(draft: MonitorDraft, includeSchedule: Boolean): JsonObject = buildJsonObject {
        draft.title.trim().takeIf(String::isNotEmpty)?.let { put("title", it) }
        put("spec", taskJson.encodeToJsonElement(MonitorSpec.serializer(), draft.spec()))
        if (includeSchedule) draft.cron?.trim()?.takeIf(String::isNotEmpty)?.let { cron ->
            putJsonObject("schedule") {
                putJsonObject("kind") {
                    putJsonObject("Cron") {
                        put("expression", cron)
                        put("timezone", draft.timezone)
                    }
                }
                put("timezone", draft.timezone)
            }
        }
    }

    private suspend fun <T> HttpResponse.decode(
        serializer: kotlinx.serialization.KSerializer<T>,
        action: String,
    ): T = taskJson.decodeFromString(serializer, successText(action))

    private suspend fun HttpResponse.json(action: String): JsonElement =
        taskJson.parseToJsonElement(successText(action))

    private suspend fun HttpResponse.successText(action: String): String {
        val body = bodyAsText()
        if (status.isSuccess()) return body
        val message = runCatching {
            val root = taskJson.parseToJsonElement(body).jsonObject
            val raw = (root["error"] ?: root["message"] ?: root["detail"])?.jsonPrimitive?.content
            // 409 `task_active`: the run is still finishing (e.g. writing outputs).
            if ((root["code"] as? JsonPrimitive)?.contentOrNull == "task_active" || raw?.startsWith("task_active:") == true) {
                "This task is still finishing. Try again in a few seconds."
            } else {
                raw
            }
        }.getOrNull()?.takeIf(String::isNotBlank)
        throw TaskApiError(message ?: "Could not $action (HTTP ${status.value}).", status.value)
    }

    private fun segment(value: String): String = URLEncoder.encode(
        value,
        StandardCharsets.UTF_8.name(),
    ).replace("+", "%20")

    private fun JsonElement.stringValue(): String? =
        if (this is JsonNull) null else runCatching { jsonPrimitive.contentOrNull }.getOrNull()
}

internal fun decodeTaskAgentOptions(root: JsonElement, personalOnly: Boolean = true): List<AgentOption> {
    val rows = when (root) {
        is JsonArray -> root
        is JsonObject -> root["agents"] as? JsonArray ?: throw TaskApiError("Crew response could not be read.")
        else -> throw TaskApiError("Crew response could not be read.")
    }
    return rows.mapNotNull { row ->
        val record = row as? JsonObject ?: return@mapNotNull null
        val item = record["definition"] as? JsonObject ?: record
        fun text(key: String) = (item[key] as? JsonPrimitive)?.contentOrNull
        val id = (text("agent_id") ?: text("id"))?.takeIf(String::isNotBlank) ?: return@mapNotNull null
        if (personalOnly && ((item["disabled"] as? JsonPrimitive)?.booleanOrNull == true || (text("kind") != null && !text("kind").equals("Personal", true)))) return@mapNotNull null
        AgentOption(id, text("name")?.takeIf(String::isNotBlank) ?: id, (item["is_primary"] as? JsonPrimitive)?.booleanOrNull == true)
    }
}

internal fun taskDetailRequestPaths(taskId: String, executionId: String?): Map<String, String> {
    fun encode(value: String) = URLEncoder.encode(value, "UTF-8").replace("+", "%20")
    if (taskId.startsWith("agent-cycle:")) {
        require(!executionId.isNullOrBlank()) { "This cycle has no execution to inspect." }
        return mapOf("run" to "/executions/${encode(executionId)}/execution-panel")
    }
    val id = encode(taskId)
    val suffix = executionId?.takeIf(String::isNotBlank)?.let { "?execution_id=${encode(it)}" }.orEmpty()
    return linkedMapOf("task" to "/tasks/$id", "run" to "/tasks/$id/execution-panel$suffix", "output" to "/tasks/$id/outputs", "history" to "/tasks/$id/details", "plan" to "/tasks/$id/plan")
}

internal fun assembleTaskDetail(results: Map<String, Result<JsonObject>>): TaskDetailBundle {
    if (listOf("task", "run", "history").all { results[it]?.isFailure != false }) {
        throw TaskApiError("Task details could not be loaded.")
    }
    return TaskDetailBundle(
        task = results["task"]?.getOrNull(),
        panel = results["run"]?.getOrNull(),
        outputs = results["output"]?.getOrNull(),
        details = results["history"]?.getOrNull(),
        plan = results["plan"]?.getOrNull(),
        unavailableSections = buildSet {
            if (results["run"]?.isFailure != false) add("live run")
            if (results.containsKey("output") && results["output"]?.isFailure != false) add("outputs")
            if (results.containsKey("history") && results["history"]?.isFailure != false) add("execution history")
        },
    )
}

internal fun parseTaskRealtimeEvent(
    raw: String,
    principal: String,
    workspace: String,
): TaskRealtimeEvent? {
    val envelope = runCatching { taskJson.parseToJsonElement(raw).jsonObject }.getOrNull()
        ?: return null
    return parseTaskRealtimeEvent(envelope, principal, workspace)
}

/** The same, for a frame the realtime hub already parsed. */
internal fun parseTaskRealtimeEvent(
    envelope: JsonObject,
    principal: String,
    workspace: String,
): TaskRealtimeEvent? {
    val progressData = envelope["data"] as? JsonObject
    val inner = if (envelope["event_type"].textValue() == "AgentEvent") progressData?.get("event") as? JsonObject else null
    val payload = (inner?.get("payload") as? JsonObject) ?: progressData
    val correlation = payload?.get("correlation") as? JsonObject
    val workType = inner?.get("event_type").textValue() ?: envelope["event_type"].textValue().orEmpty()
    if (payload != null && TaskRunEvidence.isProgress(workType, payload)) {
        if ((payload["principal"] ?: inner?.get("principal") ?: progressData?.get("principal")).textValue() != principal ||
            (payload["workspace"] ?: inner?.get("workspace") ?: progressData?.get("workspace")).textValue() != workspace) return null
        val root = (payload["root_execution_id"] ?: correlation?.get("root_execution_id") ?: payload["execution_id"] ?: correlation?.get("execution_id")).textValue() ?: return null
        val at = TaskRunEvidence.instant(payload["timestamp"] ?: inner?.get("timestamp") ?: progressData?.get("timestamp") ?: envelope["timestamp"]) ?: return null
        return TaskRealtimeEvent(workType, executionId = root, eventTimestamp = at, progress = true)
    }
    val type = envelope["event_type"].textValue().orEmpty()
    if (!type.contains("Task") && !type.contains("Planning") &&
        !type.contains("Execution") && !type.contains("Monitor")) return null
    val data = envelope["data"] as? JsonObject ?: JsonObject(emptyMap())
    val eventPrincipal = data["principal"].textValue()
    val eventWorkspace = data["workspace"].textValue()
    if (eventPrincipal != null && eventPrincipal != principal) return null
    if (eventWorkspace != null && eventWorkspace != workspace) return null
    val state = data["state"] as? JsonObject
    val overview = state?.get("overview") as? JsonObject
    val selected = (state?.get("debug") as? JsonObject)
        ?.get("selected_execution") as? JsonObject
    return TaskRealtimeEvent(
        eventType = type,
        taskId = data["task_id"].textValue() ?: overview?.get("task_id").textValue(),
        executionId = data["execution_id"].textValue()
            ?: overview?.get("execution_id").textValue()
            ?: selected?.get("execution_id").textValue(),
        panel = state.takeIf { type == "ExecutionPanelDelta" },
        eventTimestamp = data["timestamp"].textValue()?.toLongOrNull() ?: 0,
    )
}

private fun JsonElement?.textValue(): String? = this?.let {
    if (it is JsonNull) null else runCatching { it.jsonPrimitive.contentOrNull }.getOrNull()
}
