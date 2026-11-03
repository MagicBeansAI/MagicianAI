package ai.magicbeans.magdroid.chat

import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.tasks.ExecutionControlAction
import ai.magicbeans.magdroid.tasks.ExecutionControlState
import ai.magicbeans.magdroid.tasks.PlanApprovalActions
import ai.magicbeans.magdroid.tasks.PlanApprovalClient
import ai.magicbeans.magdroid.tasks.PlanApprovalPost
import ai.magicbeans.magdroid.tasks.PlanApprovalRequest
import ai.magicbeans.magdroid.tasks.PlanProgress
import ai.magicbeans.magdroid.tasks.PlanStepRow
import ai.magicbeans.magdroid.tasks.PlanSteps
import ai.magicbeans.magdroid.tasks.PlanningEntry
import ai.magicbeans.magdroid.tasks.TaskPlanMarker
import ai.magicbeans.magdroid.tasks.TaskUsageLedger
import ai.magicbeans.magdroid.tasks.TaskUsageTotal
import ai.magicbeans.magdroid.tasks.jArr
import ai.magicbeans.magdroid.tasks.jObj
import ai.magicbeans.magdroid.tasks.jObjects
import ai.magicbeans.magdroid.tasks.jText
import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.post
import io.ktor.client.request.put
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsBytes
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.JsonObject
import java.net.URLEncoder
import java.nio.charset.StandardCharsets

/*
 * What feeds the chat's live task card beyond its status message: the task's
 * stored status (`GET /v3/tasks/{id}`, store wins), the planning track, step
 * bar and recent activity (`/execution-panel`, polled every 4 s only while
 * [LiveTaskCardRules.shouldPoll] says so), and the plan, once, when the card
 * wants it. Android port of iOS `LiveTaskCardLoader` / `LiveTaskCardClient`
 * (`LiveTaskCardView.swift`) and `TaskUsageStore`. No Compose; the fetch is
 * injected so the gating is tested without a socket.
 */

/** A v3 GET (`/tasks/{id}…`), parsed; null on any failure. */
typealias LiveTaskJsonFetch = suspend (v3Path: String) -> JsonObject?

/** The recent activity lines a task's panel states (iOS `updateExecutionPanel` steps). */
object LiveTaskActivity {
    /**
     * `run.activity_log` when the panel carries it, else `run.recent_activity`;
     * each item's title, else its content. Oldest first.
     */
    fun lines(panel: JsonObject): List<String> {
        val run = panel["run"].jObj() ?: return emptyList()
        val items = if (run["activity_log"].jArr() != null) run["activity_log"].jObjects().orEmpty()
        else run["recent_activity"].jObjects().orEmpty()
        return items.mapNotNull { it["title"].jText() ?: it["content"].jText() }
    }

    /** The card's expanded activity: the newest four. */
    fun recent(lines: List<String>, limit: Int = 4): List<String> = lines.takeLast(limit)
}

/**
 * The URL a chat output block is read from (iOS `ArtifactRef.direct` /
 * `.taskOutput` / `.sessionOutput`): its own `url` (absolute, or relative to
 * the host), else its `relative_path` under the owner [LiveTaskCardRules.outputHome]
 * names. Null when the block has neither.
 */
object LiveTaskOutputLinks {
    fun url(baseUrl: String, block: ContentBlockRecord, home: LiveTaskOutputHome?): String? {
        val base = baseUrl.trim().trimEnd('/')
        block.url?.trim()?.takeIf { it.isNotEmpty() }?.let { raw ->
            if (raw.startsWith("http://") || raw.startsWith("https://")) return raw
            if (base.isEmpty()) return null
            return base + (if (raw.startsWith("/")) "" else "/") + raw
        }
        val relative = block.relativePath?.trim()?.takeIf { it.isNotEmpty() } ?: return null
        if (base.isEmpty()) return null
        val path = relative.trimStart('/').removePrefix("outputs/")
            .split('/').filter { it.isNotBlank() && it != "." && it != ".." }
            .joinToString("/") { LiveTaskCardClient.segment(it) }
            .takeIf { it.isNotEmpty() } ?: return null
        return when (home) {
            is LiveTaskOutputHome.Task -> "$base/api/magician/v3/tasks/${LiveTaskCardClient.segment(home.taskId)}/outputs/$path"
            is LiveTaskOutputHome.Session -> "$base/api/magician/v2/chat/sessions/${LiveTaskCardClient.segment(home.sessionId)}/outputs/$path"
            null -> null
        }
    }
}

/** Everything the card's loader has learnt about its task. */
data class LiveTaskCardFeed(
    val planningEntries: List<PlanningEntry> = emptyList(),
    val stepRows: List<PlanStepRow> = emptyList(),
    val plan: LiveTaskPlan? = null,
    /** `task.state.status` from the store, when fetched. */
    val storeStatus: String? = null,
    /** The store's active root run, for Cancel and the run controls. */
    val activeRootExecutionId: String? = null,
    /** Recent activity lines from the polled panel, oldest first. */
    val activityLines: List<String> = emptyList(),
    /** The store record shows a plan (`TaskPlanMarker.hasPlan`). */
    val storeHasPlan: Boolean = false,
    /** The store record's created and updated times (epoch ms), when fetched ([TaskStoreRecord]). */
    val storeCreatedAtMs: Long? = null,
    val storeUpdatedAtMs: Long? = null,
)

/**
 * Whether the chat card shows the plan glyph by its title: the store record
 * shows a plan, the card loaded plan steps or step rows, or the message or
 * store status is a planning one (planning, draft, eliciting).
 */
object LiveTaskPlanGlyph {
    private val PLANNING = setOf("planning", "draft", "eliciting")

    fun shows(
        messageStatus: String?,
        storeStatus: String?,
        storeHasPlan: Boolean,
        planStepCount: Int,
        stepRowCount: Int,
    ): Boolean =
        storeHasPlan || planStepCount > 0 || stepRowCount > 0 ||
            listOf(messageStatus, storeStatus).any { it?.trim()?.lowercase() in PLANNING }

    fun shows(task: TaskStatusCard, feed: LiveTaskCardFeed): Boolean = shows(
        messageStatus = task.status,
        storeStatus = feed.storeStatus ?: task.storeStatus,
        storeHasPlan = feed.storeHasPlan,
        planStepCount = feed.plan?.steps?.size ?: 0,
        stepRowCount = feed.stepRows.size,
    )
}

/**
 * App-wide per-task usage (iOS `TaskUsageStore.shared`): live calls and
 * stored panel snapshots in one [TaskUsageLedger], republished as a revision
 * so the card's usage line and the turn chip recompose when it changes.
 */
data class TaskUsageScope(val key: String, val principal: String, val workspace: String)

class TaskUsageStore(private val ledger: TaskUsageLedger = TaskUsageLedger()) {
    private val _revision = MutableStateFlow(0L)
    val revision: StateFlow<Long> = _revision.asStateFlow()
    private val loaded = mutableSetOf<String>()
    private val inFlight = mutableSetOf<String>()
    private var currentScope = "local"
    private val reads = Semaphore(2)
    @Volatile var scopeProvider: (() -> TaskUsageScope)? = null
    @Volatile var fetch: LiveTaskJsonFetch? = null
    @Volatile var historyFetch: (suspend (String) -> JsonObject?)? = null

    private fun ensureScope(): String = synchronized(ledger) {
        val next = scopeProvider?.invoke()?.key ?: "local"
        if (next != currentScope) {
            currentScope = next; ledger.clear(); loaded.clear(); inFlight.clear(); _revision.value += 1
        }
        next
    }
    fun total(taskId: String): TaskUsageTotal? { ensureScope(); return synchronized(ledger) { ledger.total(taskId) } }

    fun apply(panel: JsonObject, taskId: String, history: Map<String, ai.magicbeans.magdroid.tasks.TaskCallUsage> = emptyMap()) {
        ensureScope()
        val snapshot = ai.magicbeans.magdroid.tasks.TaskUsageModel.usageSnapshot(panel)
        val calls = ai.magicbeans.magdroid.tasks.TaskUsageModel.mergeCalls(snapshot.calls, history)
        val changed = synchronized(ledger) { ledger.setSnapshot(snapshot.copy(calls = calls,
            run = ai.magicbeans.magdroid.tasks.TaskUsageModel.sumCalls(calls)), taskId) }
        if (changed) _revision.value += 1
    }
    fun record(taskId: String, callKey: String, tokens: Int?, costUsd: Double?, executionId: String? = null) {
        ensureScope()
        if (synchronized(ledger) { ledger.record(taskId, callKey, tokens, costUsd, executionId) }) _revision.value += 1
    }
    fun ingest(event: JsonObject) {
        ensureScope()
        val scope = scopeProvider?.invoke() ?: return
        val data = event["data"].jObj() ?: return
        val inner = if (event["event_type"].jText() == "AgentEvent") data["event"].jObj() else null
        val payload = inner?.get("payload").jObj() ?: data
        if ((payload["principal"] ?: inner?.get("principal") ?: data["principal"]).jText() != scope.principal ||
            (payload["workspace"] ?: inner?.get("workspace") ?: data["workspace"]).jText() != scope.workspace) return
        if (event["event_type"].jText() == "ExecutionPanelDelta") {
            val panel = data["state"].jObj() ?: return
            val task = data["task_id"].jText() ?: return
            apply(panel, task); return
        }
        val type = inner?.get("event_type").jText() ?: event["event_type"].jText()
        if (type !in setOf("llm.succeeded", "LLMResponseReceived")) return
        val correlation = payload["correlation"].jObj()
        val number = ai.magicbeans.magdroid.tasks.TaskUsageModel::number
        if (number(payload["provider_attempt_count"] ?: correlation?.get("provider_attempt_count")) == 0.0) return
        val call = (payload["llm_call_id"] ?: correlation?.get("llm_call_id")).jText() ?: return
        val execution = (payload["root_execution_id"] ?: correlation?.get("root_execution_id") ?: payload["execution_id"] ?: correlation?.get("execution_id")).jText()
        val tasks = synchronized(ledger) { ledger.snapshots.filterValues { execution != null && it.executionId == execution }.keys.toMutableSet() }
        (payload["task_id"] ?: correlation?.get("task_id")).jText()?.let(tasks::add)
        val available = (payload["usage_availability"] ?: correlation?.get("usage_availability")).jObj()
        fun denied(value: JsonElement?) = (value as? JsonPrimitive)?.booleanOrNull == false
        if (denied(payload["usage_reported"])) return
        val input = if (denied(available?.get("tokens"))) null else number(payload["input_tokens"])
        val output = if (denied(available?.get("tokens"))) null else number(payload["output_tokens"])
        val cost = if (denied(available?.get("cost"))) null else number(payload["cost_usd"] ?: payload["cost"])
        tasks.forEach { record(it, call, if (input == null && output == null) null else ((input ?: 0.0) + (output ?: 0.0)).toInt(), cost, execution) }
    }
    suspend fun loadSnapshot(taskId: String, phase: String, through: LiveTaskJsonFetch? = null): Boolean {
        val expected = ensureScope()
        val read = through ?: fetch ?: return false
        val key = "$taskId::$phase"
        if (!synchronized(ledger) { if (key in loaded || taskId in inFlight) false else { loaded.add(key); inFlight.add(taskId); true } }) return false
        try { return reads.withPermit {
            if (ensureScope() != expected) return@withPermit false
            val panel = read("/tasks/${LiveTaskCardClient.segment(taskId)}/execution-panel") ?: run {
                synchronized(ledger) { if (currentScope == expected) loaded.remove(key) }; return@withPermit false
            }
            if (ensureScope() != expected) return@withPermit false
            apply(panel, taskId)
            val execution = ai.magicbeans.magdroid.tasks.TaskRunEvidence.executionId(panel)
            val history = execution?.let { historyFetch?.invoke(it) }
            if (execution != null && history != null && ensureScope() == expected) {
                apply(panel, taskId, ai.magicbeans.magdroid.tasks.TaskRunEvidence.history(history, execution))
            }
            true
        } } catch (error: Exception) {
            synchronized(ledger) { if (currentScope == expected) loaded.remove(key) }
            if (error is CancellationException) throw error
            return false
        } finally { synchronized(ledger) { if (currentScope == expected) inFlight.remove(taskId) } }
    }
    companion object { val shared = TaskUsageStore() }
}

/**
 * One card's loader. [sync] is called whenever the card's visibility, the
 * app's foreground state or the card's phase changes; it starts or stops the
 * poll and makes the one-off reads.
 */
class LiveTaskCardLoader(
    private val scope: CoroutineScope,
    private val fetch: LiveTaskJsonFetch,
    private val usage: TaskUsageStore = TaskUsageStore.shared,
    private val nowMillis: () -> Long = System::currentTimeMillis,
) {
    private val _feed = MutableStateFlow(LiveTaskCardFeed())
    val feed: StateFlow<LiveTaskCardFeed> = _feed.asStateFlow()

    private var pollJob: Job? = null
    private var pollingTaskId: String? = null
    private var planRequestedFor: String? = null
    private var storeFetchedAt: Long? = null
    private var storeInFlight = false

    val isPolling: Boolean get() = pollJob?.isActive == true

    fun sync(
        taskId: String?,
        phase: LiveTaskCardPhase,
        visible: Boolean,
        appActive: Boolean,
        wantsPlan: Boolean,
        terminal: Boolean,
        usagePhase: String,
    ) {
        if (taskId == null) {
            stop()
            return
        }
        val poll = LiveTaskCardRules.shouldPoll(phase, hasTaskId = true, visible = visible, appActive = appActive)
        if (poll) startPolling(taskId) else stop()
        if (visible && wantsPlan) loadPlanOnce(taskId)
        // While the card reads non-terminal, check the stored status on
        // appear (the poll re-checks it each tick).
        if (visible && !terminal) refreshStoreStatus(taskId)
        // The poll keeps usage current while planning or running; otherwise
        // the stored figure is read once per phase.
        if (visible && !poll) scope.launch { usage.loadSnapshot(taskId, usagePhase, fetch) }
    }

    fun startPolling(taskId: String) {
        if (pollingTaskId == taskId && pollJob?.isActive == true) return
        stop()
        pollingTaskId = taskId
        pollJob = scope.launch {
            while (isActive) {
                fetch("/tasks/${LiveTaskCardClient.segment(taskId)}/execution-panel")?.let { apply(it, taskId) }
                refreshStoreStatus(taskId)
                delay(LiveTaskCardRules.POLL_INTERVAL_MILLIS)
            }
        }
    }

    fun stop() {
        pollJob?.cancel()
        pollJob = null
        pollingTaskId = null
    }

    /** Fetch the task's stored status, at most once per poll interval. */
    fun refreshStoreStatus(taskId: String) {
        if (storeInFlight) return
        val now = nowMillis()
        storeFetchedAt?.let { if (now - it < LiveTaskCardRules.POLL_INTERVAL_MILLIS - 500) return }
        storeInFlight = true
        storeFetchedAt = now
        scope.launch {
            try {
                val payload = fetch("/tasks/${LiveTaskCardClient.segment(taskId)}")
                val record = payload?.let(TaskStoreRecord::parse)
                if (record != null) {
                    val task = payload["task"].jObj()
                    _feed.value = _feed.value.copy(
                        storeStatus = record.status,
                        activeRootExecutionId = record.activeRootExecutionId,
                        storeHasPlan = TaskPlanMarker.hasPlan(task) ||
                            TaskPlanMarker.hasPlan(task?.get("state").jObj()) ||
                            TaskPlanMarker.hasPlan(task?.get("manifest").jObj()),
                        storeCreatedAtMs = record.createdAtMs ?: _feed.value.storeCreatedAtMs,
                        storeUpdatedAtMs = record.updatedAtMs ?: _feed.value.storeUpdatedAtMs,
                    )
                }
            } finally {
                storeInFlight = false
            }
        }
    }

    fun loadPlanOnce(taskId: String) {
        if (planRequestedFor == taskId) return
        planRequestedFor = taskId
        scope.launch { fetchPlan(taskId) }
    }

    suspend fun fetchPlan(taskId: String): LiveTaskPlan? {
        val parsed = fetch("/tasks/${LiveTaskCardClient.segment(taskId)}/plan")?.let(LiveTaskPlan::parse) ?: return null
        _feed.value = _feed.value.copy(plan = parsed)
        return parsed
    }

    /** One polled panel: the planning entries, the step rows, the activity and the usage. */
    fun apply(panel: JsonObject, taskId: String) {
        val current = _feed.value
        val next = current.copy(
            planningEntries = PlanProgress.entries(panel),
            stepRows = PlanSteps.rows(panel, runEnded = false),
            activityLines = LiveTaskActivity.lines(panel).ifEmpty { current.activityLines },
        )
        if (next != current) _feed.value = next
        usage.apply(panel, taskId)
    }
}

/**
 * The card's few authenticated calls, on the app's host and identity
 * ([MagicianAccess]). Also the card's [PlanApprovalClient] for Approve & Run.
 */
class LiveTaskCardClient(
    private val client: HttpClient,
    private val baseUrl: () -> String,
    private val headers: () -> Map<String, String>,
    private val identity: () -> Pair<String, String> = { "anonymous" to "default" },
) : PlanApprovalClient {

    /** `GET /api/magician/v3{path}`; null on any failure. */
    suspend fun json(v3Path: String): JsonObject? {
        val origin = runCatching { root() }.getOrNull() ?: return null
        val auth = headers().toMap(); val scope = identity()
        return try {
            val response = client.get("$origin/api/magician/v3$v3Path") { auth.forEach { (name, value) -> header(name, value) } }
            if (origin == root() && auth == headers() && scope == identity() && response.status.isSuccess()) parse(response.bodyAsText()) else null
        } catch (error: Exception) {
            if (error is CancellationException) throw error
            null
        }
    }

    suspend fun callHistory(executionId: String): JsonObject? {
        val origin = runCatching { root() }.getOrNull() ?: return null
        val auth = headers().toMap(); val scope = identity()
        val id = "'" + executionId.replace("'", "''") + "'"
        val sql = "SELECT llm_call_id, input_tokens, output_tokens, cost_usd FROM llm_calls WHERE (root_execution_id = $id OR execution_id = $id) AND COALESCE(provider_attempt_count, 1) <> 0"
        return try {
            val response = client.post("$origin/api/magician/v2/analytics/llm_calls/query?principal=${segment(scope.first)}&workspace=${segment(scope.second)}") {
                auth.forEach { (name, value) -> header(name, value) }
                contentType(ContentType.Application.Json)
                setBody(kotlinx.serialization.json.buildJsonObject { put("sql", JsonPrimitive(sql)) }.toString())
            }
            if (origin == root() && auth == headers() && scope == identity() && response.status.isSuccess()) parse(response.bodyAsText()) else null
        } catch (error: Exception) {
            if (error is CancellationException) throw error
            null
        }
    }

    /** `POST /v3/tasks/{id}/execute`. Null on success, else what went wrong. */
    suspend fun execute(taskId: String): String? =
        send("/api/magician/v3/tasks/${segment(taskId)}/execute", "{}", "The run did not start") { url, body ->
            client.post(url) { authorize(); contentType(ContentType.Application.Json); setBody(body) }
        }

    /**
     * Stop the active run (`POST /v2/executions/{id}/cancel`, as the run
     * controls do); with no run yet (planning), mark the task cancelled.
     */
    suspend fun cancel(taskId: String, activeExecutionId: String?): String? {
        val execution = activeExecutionId?.trim()?.takeIf { it.isNotEmpty() }
        if (execution != null) {
            return send("/api/magician/v2/executions/${segment(execution)}/cancel", "{}", "The task could not be cancelled") { url, body ->
                client.post(url) { authorize(); contentType(ContentType.Application.Json); setBody(body) }
            }
        }
        return send("/api/magician/v3/tasks/${segment(taskId)}/status", """{"status":"cancelled"}""", "The task could not be cancelled") { url, body ->
            client.put(url) { authorize(); contentType(ContentType.Application.Json); setBody(body) }
        }
    }

    /** The first 8 KB of a text file, for the card's preview. */
    suspend fun textSnippet(url: String): String? = try {
        val response = client.get(url) { authorize(); header("Range", "bytes=0-8191") }
        if (response.status.isSuccess()) response.bodyAsText().take(8192).trim() else null
    } catch (error: Exception) {
        if (error is CancellationException) throw error
        null
    }

    /** Raw bytes (an image preview), capped. */
    suspend fun bytes(url: String, maxBytes: Int = 8 * 1024 * 1024): ByteArray? = try {
        val response = client.get(url) { authorize() }
        if (!response.status.isSuccess()) null
        else {
            val data = response.bodyAsBytes()
            data.takeIf { it.size <= maxBytes }
        }
    } catch (error: Exception) {
        if (error is CancellationException) throw error
        null
    }

    /** `POST /v2/chat/sessions/{id}/outputs/open-{file|folder}`: open on the host. */
    suspend fun openOnHost(sessionId: String, absolutePath: String, folder: Boolean): String? {
        val body = kotlinx.serialization.json.buildJsonObject {
            put("absolute_path", kotlinx.serialization.json.JsonPrimitive(absolutePath))
        }.toString()
        val action = if (folder) "open-folder" else "open-file"
        return send("/api/magician/v2/chat/sessions/${segment(sessionId)}/outputs/$action", body,
            if (folder) "The folder could not be revealed." else "The file could not be opened.") { url, payload ->
            client.post(url) { authorize(); contentType(ContentType.Application.Json); setBody(payload) }
        }
    }

    /**
     * `POST /v3/tasks/{id}/plan/{approve|reject|replan}` (`plan_id` for
     * approve / reject). Null on success, else what went wrong.
     */
    suspend fun planAction(taskId: String, action: String, planId: String?): String? {
        val query = if (action == "approve" || action == "reject") "?plan_id=${segment(planId.orEmpty())}" else ""
        return send("/api/magician/v3/tasks/${segment(taskId)}/plan/$action$query", "{}", "The plan could not be updated") { url, body ->
            client.post(url) { authorize(); contentType(ContentType.Application.Json); setBody(body) }
        }
    }

    /** `GET /v2/executions/{id}/control-state`: what the run can do now. */
    suspend fun controlState(executionId: String): ExecutionControlState? = try {
        val response = client.get("${root()}/api/magician/v2/executions/${segment(executionId)}/control-state") { authorize() }
        if (!response.status.isSuccess()) null
        else lenient.decodeFromString(ExecutionControlState.serializer(), response.bodyAsText())
    } catch (error: Exception) {
        if (error is CancellationException) throw error
        null
    }

    /** `POST /v2/executions/{id}/{pause|resume}`. Null on success, else what went wrong. */
    suspend fun control(executionId: String, action: ExecutionControlAction): String? =
        send("/api/magician/v2/executions/${segment(executionId)}/${action.wire}", "{}", "The run did not ${action.wire}") { url, body ->
            client.post(url) { authorize(); contentType(ContentType.Application.Json); setBody(body) }
        }

    override suspend fun respond(request: PlanApprovalRequest, confirmed: Boolean): PlanApprovalPost = try {
        val response = client.post("${root()}/api/magician/v2/hitl/${segment(request.correlationId)}/respond") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(PlanApprovalActions.respondPayload(request, confirmed).toString())
        }
        PlanApprovalActions.respondOutcome(response.status.value, parse(response.bodyAsText()))
    } catch (error: Exception) {
        if (error is CancellationException) throw error
        PlanApprovalPost.Failed(error.message ?: "The answer could not be sent.")
    }

    override suspend fun startRun(taskId: String) {
        execute(taskId)?.let { throw IllegalStateException(it) }
    }

    private suspend fun send(
        path: String,
        body: String,
        fallback: String,
        call: suspend (String, String) -> HttpResponse,
    ): String? = try {
        val response = call("${root()}$path", body)
        if (response.status.isSuccess()) null
        else {
            val obj = parse(response.bodyAsText())
            obj?.get("error").jText() ?: obj?.get("message").jText() ?: obj?.get("detail").jText()
                ?: "$fallback (HTTP ${response.status.value})"
        }
    } catch (error: Exception) {
        if (error is CancellationException) throw error
        error.message ?: fallback
    }

    private fun root(): String {
        val trimmed = baseUrl().trim().trimEnd('/')
        check(trimmed.isNotEmpty()) { "No Magician host configured yet." }
        return trimmed
    }

    private fun io.ktor.client.request.HttpRequestBuilder.authorize() {
        headers().forEach { (name, value) -> header(name, value) }
    }

    companion object {
        fun segment(id: String): String =
            URLEncoder.encode(id, StandardCharsets.UTF_8.name()).replace("+", "%20")

        private val lenient = Json { ignoreUnknownKeys = true; isLenient = true }

        internal fun parse(text: String): JsonObject? =
            runCatching { Json.parseToJsonElement(text) as? JsonObject }.getOrNull()

        @Volatile private var instance: LiveTaskCardClient? = null

        /** The app's one client; also installs the usage store's fetch. */
        fun shared(context: Context): LiveTaskCardClient = instance ?: synchronized(this) {
            instance ?: run {
                val app = context.applicationContext
                LiveTaskCardClient(
                    client = HttpClient(CIO) {
                        install(HttpTimeout) {
                            requestTimeoutMillis = 30_000
                            socketTimeoutMillis = 30_000
                            connectTimeoutMillis = 15_000
                        }
                        expectSuccess = false
                    },
                    baseUrl = { MagicianAccess.baseUrl(app) },
                    headers = { MagicianAccess.headers(app) },
                    identity = { MagicianAccess.principal(app) to MagicianAccess.workspace(app) },
                ).also { created ->
                    instance = created
                    TaskUsageStore.shared.fetch = created::json
                    TaskUsageStore.shared.historyFetch = created::callHistory
                    TaskUsageStore.shared.scopeProvider = {
                        TaskUsageScope(MagicianAccess.baseUrl(app) + "|" + MagicianAccess.principal(app) + "|" + MagicianAccess.workspace(app) + "|" + MagicianAccess.headers(app).toSortedMap().toString(),
                            MagicianAccess.principal(app), MagicianAccess.workspace(app))
                    }
                }
            }
        }
    }
}
