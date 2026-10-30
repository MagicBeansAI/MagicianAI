package ai.magicbeans.magdroid.chat

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.doubleOrNull

/**
 * The event→row projection behind the chat turn's Steps section — Kotlin port
 * of `magios/Shared/ChatTurnActivity.swift`, which is itself the port of the
 * web's `RequestActivityCard` projection. One turn's granular events (llm /
 * reasoning / tool / step / artifact / pause, plus delegated-task, tutor and
 * coding lifecycles) accumulate into ordered rows, coalesced one row per
 * logical operation.
 *
 * Labels, coalescing keys, status transitions and complete-result ownership
 * match iOS verbatim — two phones reading the same turn must say the same
 * things about it.
 *
 * Events come from `GET /chat/sessions/{id}/turns/{turn}/events` — the
 * canonical projection the web and iOS read. They do NOT ride
 * `ExecutionPanelDelta`: this client once read `rows`/`activity_rows` off
 * that frame, fields the wire has never carried, so the section never drew a
 * single row. The parity register records the finding.
 */

internal enum class ActivityRowStatus(val wire: String) {
    Running("running"), Done("done"), Failed("failed"), Waiting("waiting")
}

internal enum class ActivityRowTone { Info, Tool, Error, Reasoning, Pause }

/** A host file a step names: its absolute path and the label to show. */
@kotlinx.serialization.Serializable
data class ActivityFileRef(val absolutePath: String, val label: String)

/** The full-fidelity row the accumulator maintains; projected to [ActivityRow] on snapshot. */
internal data class ActivityTurnRow(
    val key: String,
    var kind: String,
    var label: String,
    var detail: String?,
    var status: ActivityRowStatus,
    var tone: ActivityRowTone,
    var durationMs: Double?,
    var agentId: String? = null,
    var taskId: String? = null,
    var executionId: String? = null,
    var files: List<ActivityFileRef> = emptyList(),
    var resultRef: String? = null,
    var resultHash: String? = null,
    var resultSizeBytes: Int? = null,
    var resultOwner: ActivityResultOwner? = null,
    var callUsage: ActivityCallUsage? = null,
    var spawnedTaskIds: List<String> = emptyList(),
    var toolName: String? = null,
    /** When the call began (epoch ms), for a running row's live elapsed clock. */
    var startedAtMs: Double? = null,
)

internal class ChatTurnActivityAccumulator {

    private val rowsByKey = LinkedHashMap<String, ActivityTurnRow>()
    private val orderedKeys = mutableListOf<String>()
    private val terminalTutorRunIds = mutableSetOf<String>()
    /**
     * A tool row's display name, kept from the event that named it best (an
     * agentic `detail`, or a `__call_tool` start's args) so its terminal
     * event, which may carry neither, keeps saying the same thing. The flag
     * marks a human summary, which is shown as-is rather than with a verb.
     */
    private val toolDisplayByKey = mutableMapOf<String, Pair<String, Boolean>>()
    /** The model `LLMRouted` named for a call, for a response that names none. */
    private val routedModelByKey = mutableMapOf<String, String>()
    /**
     * The row each `llm_call_id` lives on. A chat call reaches the bubble
     * twice — chat-side `llm.*` keyed by trace, typed `LLM*` keyed by call id
     * — and this folds both copies into one row, in either order.
     */
    private val llmKeyByCallId = mutableMapOf<String, String>()
    private var ingestAgentId: String? = null
    private var ingestStartedAtMs: Double? = null
    private var ingestTaskId: String? = null
    private var ingestExecutionId: String? = null
    private var ingestFiles: List<ActivityFileRef> = emptyList()

    var pauseActive = false
        private set

    fun reset() {
        rowsByKey.clear(); orderedKeys.clear()
        terminalTutorRunIds.clear(); pauseActive = false
        toolDisplayByKey.clear()
        routedModelByKey.clear()
        llmKeyByCallId.clear()
    }

    /** The rows as the view draws them. */
    fun snapshot(): List<ActivityRow> = snapshotRows().map {
        ActivityRow(
            label = it.label,
            detail = it.detail,
            status = it.status.wire,
            resultRef = it.resultRef,
            resultHash = it.resultHash,
            resultSizeBytes = it.resultSizeBytes,
            resultOwner = it.resultOwner,
            taskId = it.taskId,
            executionId = it.executionId,
            kind = it.kind,
            durationMs = it.durationMs,
            callUsage = it.callUsage,
            spawnedTaskIds = it.spawnedTaskIds,
            files = it.files,
            tone = it.tone.name.lowercase(),
            toolName = it.toolName,
            key = it.key,
            startedAtMs = it.startedAtMs,
        )
    }

    internal fun snapshotRows(): List<ActivityTurnRow> {
        val rows = orderedKeys.mapNotNull { rowsByKey[it]?.copy() }
        val taskRows = rows.filter { it.key.startsWith("task::") }
        if (taskRows.isEmpty() || taskRows.any { it.status == ActivityRowStatus.Running }) {
            return rows
        }
        // Durable replay can miss a child tool/LLM terminal event even though
        // the task itself is authoritative and terminal. Settle those stale
        // running leaves so the card cannot spin or offer Stop forever.
        val terminal = if (taskRows.any { it.status == ActivityRowStatus.Failed }) {
            ActivityRowStatus.Failed
        } else {
            ActivityRowStatus.Done
        }
        rows.forEach { row ->
            if (!row.key.startsWith("task::") && row.status == ActivityRowStatus.Running) {
                row.status = terminal
                if (terminal == ActivityRowStatus.Failed) row.tone = ActivityRowTone.Error
                if (row.detail == null) {
                    row.detail = if (terminal == ActivityRowStatus.Failed) {
                        "Stopped when task failed"
                    } else {
                        "Settled when task finished"
                    }
                }
            }
        }
        return rows
    }

    private fun upsert(incoming: ActivityTurnRow) {
        val row = incoming
        row.agentId = row.agentId ?: ingestAgentId
        row.taskId = row.taskId ?: ingestTaskId
        row.executionId = row.executionId ?: ingestExecutionId
        if (row.files.isEmpty()) row.files = ingestFiles
        if (rowsByKey[row.key] == null) orderedKeys.add(row.key)
        rowsByKey[row.key] = row
    }

    private fun drop(key: String) {
        if (rowsByKey.remove(key) != null) orderedKeys.remove(key)
    }

    private fun patch(key: String, mutate: (ActivityTurnRow) -> Unit) {
        val row = rowsByKey[key] ?: return
        val previousStatus = row.status
        mutate(row)
        row.agentId = row.agentId ?: ingestAgentId
        row.taskId = row.taskId ?: ingestTaskId
        row.executionId = row.executionId ?: ingestExecutionId
        if (ingestFiles.isNotEmpty()) row.files = ingestFiles

        // Web moves a row to the bounded tail when its terminal event lands,
        // so a long-running tool cannot finish off-screen behind newer work.
        if (row.status != previousStatus &&
            (row.status == ActivityRowStatus.Done || row.status == ActivityRowStatus.Failed)
        ) {
            val index = orderedKeys.indexOf(key)
            if (index >= 0 && index != orderedKeys.lastIndex) {
                orderedKeys.removeAt(index)
                orderedKeys.add(key)
            }
        }
    }

    /**
     * Settle the pause rows an answer resolves: the row keyed on its
     * correlation id, plus the id-less `pause:pause` row a payload without ids
     * files under. An answer that names no id settles every waiting pause row.
     * The turn stops reading "Waiting on you" once no pause row is left
     * waiting. Mirrors iOS / web `ChatTurnProgress.settlePauseRows`.
     */
    private fun settlePauseRows(correlationId: String?) {
        for (key in orderedKeys) {
            val row = rowsByKey[key] ?: continue
            if (row.kind != "pause" || row.status != ActivityRowStatus.Waiting) continue
            val matches = correlationId == null || key == "pause:$correlationId" || key == "pause:pause"
            if (!matches) continue
            row.label = "Answered"
            row.status = ActivityRowStatus.Done
            row.tone = ActivityRowTone.Info
        }
        pauseActive = rowsByKey.values.any { it.kind == "pause" && it.status == ActivityRowStatus.Waiting }
    }

    /** Ingest one raw event frame, exactly as iOS classifies it. */
    fun ingest(parsed: JsonObject) {
        val outerType = parsed.string("event_type").orEmpty()
        if (outerType.isEmpty() || outerType.startsWith("__events_")) return

        // Unwrap the AgentEvent envelope so classification sees the inner type.
        var eventType = outerType
        val payload = mutableMapOf<String, JsonElement>()
        var envelopeAgentId: String? = null
        val data = parsed.obj("data")
        val inner = data?.obj("event")
        val innerType = inner?.string("event_type")
        if (outerType == "AgentEvent" && inner != null && innerType != null) {
            eventType = innerType
            inner.obj("payload")?.let { payload.putAll(it) }
            envelopeAgentId = inner.string("agent_id")
        } else if (data != null) {
            payload.putAll(data)
            envelopeAgentId = data.string("agent_id")
        }

        // Flat-loop executions use typed transport events rather than the
        // AgentEvent-enveloped tool lifecycle. Normalize the action's start
        // (a running row the moment it begins) and its completion into the
        // same tool branch the web uses. Both carry the server's `call_id`, so
        // the completion settles the row its start opened; an older server
        // sends only the completion, keyed by the legacy composite.
        if (outerType == "AgenticActionStarted" || outerType == "AgenticActionExecuted") {
            eventType = when {
                outerType == "AgenticActionStarted" -> "tool.call.started"
                payload.bool("success") == false -> "tool.call.failed"
                else -> "tool.call.finished"
            }
            payload["tool_name"] = JsonPrimitive(
                payload.nonEmpty("target") ?: payload.nonEmpty("action_type") ?: "tool",
            )
            payload["call_id"] = JsonPrimitive(
                payload.scalar("call_id") ?: listOf(
                    payload.scalar("step_id") ?: "step",
                    payload.scalar("iteration") ?: "0",
                    payload.scalar("timestamp") ?: "0",
                ).joinToString("-"),
            )
            payload.nonEmpty("detail")?.let { payload[ACTIVITY_LABEL_KEY] = JsonPrimitive(it) }
            payload["latency_ms"]?.let { payload["duration_ms"] = it }
        }

        // Agent prefix for delegated sub-agent events (prefer origin_agent_id).
        val effectiveAgentId = payload.string("origin_agent_id") ?: envelopeAgentId
        val skipPrefix = effectiveAgentId == null ||
            effectiveAgentId == "__system__" ||
            effectiveAgentId.startsWith("task_") ||
            effectiveAgentId.startsWith("exec_") ||
            effectiveAgentId.startsWith("cycle_")
        val agentPrefix = if (skipPrefix) "" else "[$effectiveAgentId] "

        val normalizedAgentId = when {
            effectiveAgentId == null || effectiveAgentId == "__system__" -> "system"
            effectiveAgentId.startsWith("task_") || effectiveAgentId.startsWith("exec_") ||
                effectiveAgentId.startsWith("cycle_") -> "execution"
            else -> effectiveAgentId
        }
        ingestAgentId = normalizedAgentId
        ingestStartedAtMs = activityTimestampMs(parsed)
        ingestTaskId = payload.nonEmpty("task_id")
        ingestExecutionId = payload.nonEmpty("execution_id") ?: payload.nonEmpty("root_execution_id")
        ingestFiles = extractActivityFiles(payload)
        try {
            classify(outerType, eventType, payload, agentPrefix, normalizedAgentId)
        } finally {
            ingestAgentId = null
            ingestStartedAtMs = null
            ingestTaskId = null
            ingestExecutionId = null
            ingestFiles = emptyList()
        }
    }

    private fun classify(
        outerType: String,
        eventType: String,
        payload: MutableMap<String, JsonElement>,
        agentPrefix: String,
        normalizedAgentId: String,
    ) {
        // ─── HITL answer: settle the waiting pause rows it resolves ───
        if (eventType == "context.retrieval.started" || eventType == "context.retrieval.completed") {
            val phase = payload.nonEmpty("phase_id") ?: return
            val key = "context:$phase"
            val running = eventType.endsWith(".started")
            if (running && rowsByKey[key] != null) return
            val label = when {
                running -> "Preparing context"
                payload.string("status") == "cancelled" -> "Context preparation cancelled"
                payload.string("status") == "partial" -> "Context prepared (partial)"
                else -> "Context prepared"
            }
            upsert(ActivityTurnRow(key = key, kind = "step", label = "$agentPrefix$label",
                detail = payload.string("summary"), status = if (running) ActivityRowStatus.Running else ActivityRowStatus.Done,
                tone = ActivityRowTone.Info, durationMs = payload.double("duration_ms")?.takeIf { it.isFinite() && it >= 0 },
                startedAtMs = rowsByKey[key]?.startedAtMs ?: ingestStartedAtMs))
            return
        }

        if (isPauseResolvedEvent(outerType, eventType)) {
            settlePauseRows(
                payload.string("correlation_id")
                    ?: payload.string("pause_state_id")
                    ?: payload.string("request_id"),
            )
            return
        }

        // ─── HITL pause family ───
        if (isPauseEvent(outerType, eventType)) {
            pauseActive = true
            val cid = payload.string("correlation_id")
                ?: payload.string("pause_state_id")
                ?: payload.string("request_id") ?: "pause"
            val prompt = payload.string("prompt") ?: payload.string("question")
            upsert(
                ActivityTurnRow(
                    key = "pause:$cid", kind = "pause", label = "Waiting on your input",
                    detail = prompt?.let { trimTo(it, 140) },
                    status = ActivityRowStatus.Waiting, tone = ActivityRowTone.Pause, durationMs = null,
                ),
            )
            return
        }

        // Typed flat-loop LLM calls. A server that stamps the call's
        // `correlation.llm_call_id` on its request, route and response gets
        // one row per call: running from `LLMRequestSent` ("Thinking…"),
        // retitled by `LLMRouted` once the model is chosen, settled in place
        // by `LLMResponseReceived`. An older server's request carries no id
        // and draws nothing, and its response is one terminal row keyed by
        // its timestamp, exactly as on web.
        if (eventType == "LLMRequestSent" || eventType == "LLMRouted" || eventType == "LLMResponseReceived") {
            classifyTypedLlm(eventType, payload, agentPrefix, normalizedAgentId)
            return
        }

        // ─── LLM lifecycle (coalesced by trace_id or iteration) ───
        if (eventType.startsWith("llm.")) {
            val trace = payload.string("trace_id") ?: payload.int("iteration")?.toString() ?: "llm"
            val key = chatLlmKey(eventType, payload, "$normalizedAgentId::llm::$trace", normalizedAgentId)
            when (eventType) {
                "llm.requested" -> {
                    if (rowsByKey[key] != null) return
                    val model = payload.string("model") ?: payload.string("capability") ?: "model"
                    upsert(
                        ActivityTurnRow(
                            key = key, kind = "llm", label = "${agentPrefix}Thinking with $model",
                            detail = null, status = ActivityRowStatus.Running,
                            tone = ActivityRowTone.Info, durationMs = null,
                            startedAtMs = callStartedAtMs(payload),
                        ),
                    )
                }
                "llm.first_token" -> {
                    payload.double("duration_ms")?.let { ttft ->
                        patch(key) { it.detail = formatLatencyPair(ttft, null) }
                    }
                }
                "llm.succeeded" -> {
                    // Task-run calls report `latency_ms`; chat calls `duration_ms`.
                    val ms = payload.double("duration_ms") ?: payload.double("latency_ms")
                    val ttft = payload.double("ttft_ms")
                    val usage = readActivityCallUsage(payload)
                    // The model that actually answered: an external harness can
                    // take a turn the native profile requested, so the
                    // completion's `model` wins over the request-time label.
                    val answeredModel = payload.nonEmpty("model")
                    if (rowsByKey[key] != null) {
                        patch(key) {
                            if (answeredModel != null) it.label = "${agentPrefix}Thinking with $answeredModel"
                            it.status = ActivityRowStatus.Done; it.tone = ActivityRowTone.Info
                            it.detail = formatLatencyPair(ttft, ms); it.durationMs = ms
                            it.callUsage = usage
                        }
                    } else {
                        val model = payload.string("model") ?: payload.string("capability") ?: "model"
                        upsert(
                            ActivityTurnRow(
                                key = key, kind = "llm", label = "${agentPrefix}Thinking with $model",
                                detail = formatLatencyPair(ttft, ms), status = ActivityRowStatus.Done,
                                tone = ActivityRowTone.Info, durationMs = ms,
                            ),
                        )
                        patch(key) { it.callUsage = usage }
                    }
                }
                "llm.failed" -> {
                    val err = payload.string("error") ?: "unknown error"
                    val ms = payload.double("duration_ms")
                    val model = payload.string("model") ?: payload.string("capability") ?: "model"
                    if (rowsByKey[key] != null) {
                        patch(key) {
                            it.label = "${agentPrefix}Thinking with $model"
                            it.status = ActivityRowStatus.Failed; it.tone = ActivityRowTone.Error
                            it.detail = trimTo(err, 200); it.durationMs = ms
                        }
                    } else {
                        upsert(
                            ActivityTurnRow(
                                key = key, kind = "llm", label = "${agentPrefix}Thinking with $model",
                                detail = trimTo(err, 200), status = ActivityRowStatus.Failed,
                                tone = ActivityRowTone.Error, durationMs = ms,
                            ),
                        )
                    }
                }
            }
            return
        }

        // ─── Reasoning (coalesced by trace_id) ───
        if (eventType.startsWith("reasoning.")) {
            val key = "$normalizedAgentId::reasoning::${payload.string("trace_id") ?: "reasoning"}"
            when (eventType) {
                "reasoning.start" -> {
                    if (rowsByKey[key] == null) {
                        upsert(
                            ActivityTurnRow(
                                key = key, kind = "reasoning", label = "Reasoning", detail = null,
                                status = ActivityRowStatus.Running, tone = ActivityRowTone.Reasoning,
                                durationMs = null,
                            ),
                        )
                    }
                }
                "reasoning.content" -> {
                    val delta = payload.string("delta") ?: payload.string("content") ?: ""
                    val existing = rowsByKey[key]
                    if (existing != null) {
                        val merged = existing.detail?.let { "$it $delta".trim() } ?: delta
                        patch(key) { it.detail = trimTo(merged, 200) }
                    } else {
                        upsert(
                            ActivityTurnRow(
                                key = key, kind = "reasoning", label = "Reasoning",
                                detail = trimTo(delta, 200), status = ActivityRowStatus.Running,
                                tone = ActivityRowTone.Reasoning, durationMs = null,
                            ),
                        )
                    }
                }
                "reasoning.end" -> patch(key) {
                    it.status = ActivityRowStatus.Done
                    it.durationMs = payload.double("duration_ms")
                }
            }
            return
        }

        // ─── Tool lifecycle (coalesced by call_id) ───
        if (eventType.startsWith("tool.")) {
            val callId = payload.string("origin_call_id")
                ?: payload.string("call_id")
                ?: payload.string("tool_call_id") ?: "anon"
            val tool = payload.string("tool_name") ?: payload.string("tool")
                ?: payload.string("name") ?: "tool"
            val key = "$normalizedAgentId::tool::$callId"
            // What the row calls this tool: an agentic `detail` as-is, else a
            // `__call_tool` start's pack and remote tool, else the tool name.
            val summary = payload.nonEmpty(ACTIVITY_LABEL_KEY)?.let { trimTo(it, ACTIVITY_TOOL_LABEL_MAX) }
            // A terminal event usually carries no args: it keeps what the start derived.
            val derived = toolCallArgs(payload)?.let { callToolDisplayName(tool, it) }
            val display = when {
                summary != null -> summary to true
                derived != null -> derived to false
                else -> toolDisplayByKey[key] ?: ((callToolDisplayName(tool, null) ?: tool) to false)
            }
            toolDisplayByKey[key] = display
            val (shown, verbless) = display
            fun calling() = if (verbless) "$agentPrefix$shown" else "${agentPrefix}Calling $shown"
            fun returned() = if (verbless) "$agentPrefix$shown" else "$agentPrefix$shown returned"
            fun failed() = if (verbless) "$agentPrefix$shown" else "$agentPrefix$shown failed"
            when {
                eventType == "tool.result.projected" && payload.nonEmpty("result_ref") != null -> {
                    val resultRef = payload.nonEmpty("result_ref")!!
                    val resultHash = payload.nonEmpty("content_hash")
                    val resultSizeBytes = payload.int("size_bytes")
                    val resultOwner = parseActivityResultOwner(payload["result_owner"])
                    val resultTaskId = payload.nonEmpty("task_id")
                    val resultExecutionId = payload.nonEmpty("execution_id")
                    if (rowsByKey[key] != null) {
                        patch(key) {
                            it.resultRef = resultRef
                            it.resultHash = resultHash
                            it.resultSizeBytes = resultSizeBytes
                            it.resultOwner = resultOwner
                            if (resultTaskId != null) it.taskId = resultTaskId
                            if (resultExecutionId != null) it.executionId = resultExecutionId
                        }
                    } else {
                        upsert(
                            ActivityTurnRow(
                                key = key, kind = "tool",
                                label = "$agentPrefix$tool result available", detail = null,
                                status = ActivityRowStatus.Done, tone = ActivityRowTone.Tool,
                                durationMs = null, taskId = resultTaskId,
                                executionId = resultExecutionId, resultRef = resultRef,
                                resultHash = resultHash, resultSizeBytes = resultSizeBytes,
                                resultOwner = resultOwner,
                            ),
                        )
                    }
                }
                eventType.endsWith(".started") || eventType.endsWith(".args") -> {
                    if (rowsByKey[key] != null) return
                    var label = calling()
                    var detail: String? = null
                    if (tool == "delegate_to_agent" && eventType.endsWith(".started")) {
                        val targets = (payload["args"] as? JsonObject)
                            ?.let { it["delegation_targets"] as? JsonArray }
                            ?.mapNotNull { (it as? JsonObject)?.string("target_agent_id") }
                            .orEmpty()
                        if (targets.size >= 2) {
                            label = "${agentPrefix}Decomposing into ${targets.size} parallel agents"
                            detail = targets.joinToString(", ")
                        } else if (targets.size == 1) {
                            label = "${agentPrefix}Delegating to ${targets[0]}"
                        }
                    }
                    upsert(
                        ActivityTurnRow(
                            key = key, kind = "tool", label = label, detail = detail,
                            status = ActivityRowStatus.Running, tone = ActivityRowTone.Tool,
                            durationMs = null, startedAtMs = callStartedAtMs(payload),
                        ),
                    )
                }
                eventType.endsWith(".succeeded") || eventType.endsWith(".finished") -> {
                    val ms = payload.double("duration_ms")
                    val preview = payload.string("content_preview")?.let { trimTo(it, 200) }
                    val spawned = if (eventType == "tool.call.finished") {
                        activitySpawnedTaskIds(payload, tool)
                    } else {
                        emptyList()
                    }
                    if (rowsByKey[key] != null) {
                        patch(key) {
                            it.label = returned()
                            it.status = ActivityRowStatus.Done; it.tone = ActivityRowTone.Tool
                            it.detail = preview; it.durationMs = ms
                            if (spawned.isNotEmpty()) it.spawnedTaskIds = spawned
                        }
                    } else {
                        upsert(
                            ActivityTurnRow(
                                key = key, kind = "tool", label = returned(),
                                detail = preview, status = ActivityRowStatus.Done,
                                tone = ActivityRowTone.Tool, durationMs = ms,
                            ),
                        )
                        if (spawned.isNotEmpty()) patch(key) { it.spawnedTaskIds = spawned }
                    }
                }
                eventType.endsWith(".failed") -> {
                    val err = payload.string("error") ?: "tool failed"
                    if (rowsByKey[key] != null) {
                        patch(key) {
                            it.label = failed()
                            it.status = ActivityRowStatus.Failed; it.tone = ActivityRowTone.Error
                            it.detail = trimTo(err, 200)
                        }
                    } else {
                        upsert(
                            ActivityTurnRow(
                                key = key, kind = "tool", label = failed(),
                                detail = trimTo(err, 200), status = ActivityRowStatus.Failed,
                                tone = ActivityRowTone.Error, durationMs = null,
                            ),
                        )
                    }
                }
            }
            if (tool != "tool" && rowsByKey[key] != null) patch(key) { it.toolName = tool }
            return
        }

        // ─── Delegated task lifecycle ───
        if (eventType == "task.status_changed" || eventType == "chat.delegate.status_changed") {
            val taskId = payload.nonEmpty("task_id")
                ?: "task-${ingestStartedAtMs?.toString() ?: orderedKeys.size.toString()}"
            val targetAgent = payload.nonEmpty("chat_inline_delegate_agent_id")
                ?: payload.nonEmpty("target_agent_id")
                ?: payload.nonEmpty("origin_agent_id")
                ?: normalizedAgentId
            val displayLabel = payload.nonEmpty("display_label") ?: targetAgent
            val status = (payload.nonEmpty("status") ?: "running").lowercase()
            val synthesisPending = payload.bool("synthesis_pending") == true
            val rowStatus = when {
                synthesisPending -> ActivityRowStatus.Running
                status in setOf("completed", "succeeded") -> ActivityRowStatus.Done
                status in setOf("failed", "cancelled", "canceled") -> ActivityRowStatus.Failed
                else -> ActivityRowStatus.Running
            }
            val label = if (synthesisPending) {
                "$displayLabel preparing final result"
            } else {
                "$displayLabel $status"
            }
            val summary = payload.nonEmpty("summary")
                ?: if (synthesisPending) "Task completed. Preparing final result..." else null
            val key = "task::$taskId"
            if (rowsByKey[key] != null) {
                patch(key) {
                    it.label = label
                    it.detail = summary?.let { s -> trimTo(s, 240) } ?: it.detail
                    it.status = rowStatus
                    it.tone = if (rowStatus == ActivityRowStatus.Failed) ActivityRowTone.Error else ActivityRowTone.Tool
                    it.agentId = targetAgent
                    it.taskId = taskId
                }
            } else {
                upsert(
                    ActivityTurnRow(
                        key = key, kind = "step", label = label,
                        detail = summary?.let { trimTo(it, 240) }, status = rowStatus,
                        tone = if (rowStatus == ActivityRowStatus.Failed) ActivityRowTone.Error else ActivityRowTone.Tool,
                        durationMs = null, agentId = targetAgent, taskId = taskId,
                    ),
                )
            }
            return
        }

        if (eventType == "chat.delegate.output_ready") {
            val taskId = payload.nonEmpty("task_id") ?: return
            val key = "task::$taskId"
            val terminalStatus = payload.nonEmpty("terminal_task_status")?.lowercase()
            val failed = terminalStatus in setOf("failed", "cancelled", "canceled") ||
                rowsByKey[key]?.status == ActivityRowStatus.Failed
            val summary = payload.nonEmpty("summary_preview")
            if (rowsByKey[key] != null) {
                patch(key) {
                    it.label = if (failed) {
                        "${it.agentId ?: "Task"} output ready after failure"
                    } else {
                        "${it.agentId ?: "Task"} output ready"
                    }
                    it.detail = summary?.let { s -> trimTo(s, 240) } ?: it.detail
                    it.status = if (failed) ActivityRowStatus.Failed else ActivityRowStatus.Done
                    it.tone = if (failed) ActivityRowTone.Error else ActivityRowTone.Tool
                    it.taskId = taskId
                }
            } else {
                upsert(
                    ActivityTurnRow(
                        key = key, kind = "step",
                        label = if (failed) "Task output ready after failure" else "Task output ready",
                        detail = summary?.let { trimTo(it, 240) },
                        status = if (failed) ActivityRowStatus.Failed else ActivityRowStatus.Done,
                        tone = if (failed) ActivityRowTone.Error else ActivityRowTone.Tool,
                        durationMs = null, taskId = taskId,
                    ),
                )
            }
            return
        }

        if (eventType == "chat.delegate.output_failed") {
            val taskId = payload.nonEmpty("task_id") ?: return
            val key = "task::$taskId"
            val stage = payload.nonEmpty("stage") ?: "synthesis"
            val error = payload.nonEmpty("last_error") ?: "unknown error"
            val detail = "Output synthesis failed ($stage): $error"
            if (rowsByKey[key] != null) {
                patch(key) {
                    it.label = "${it.agentId ?: "Task"} output failed"
                    it.detail = trimTo(detail, 240)
                    it.status = ActivityRowStatus.Failed
                    it.tone = ActivityRowTone.Error
                    it.taskId = taskId
                }
            } else {
                upsert(
                    ActivityTurnRow(
                        key = key, kind = "step", label = "Task output failed",
                        detail = trimTo(detail, 240), status = ActivityRowStatus.Failed,
                        tone = ActivityRowTone.Error, durationMs = null, taskId = taskId,
                    ),
                )
            }
            return
        }

        // ─── Step lifecycle (coalesced by step_id) ───
        if (eventType.startsWith("step.")) {
            val stepId = payload.string("step_id") ?: payload.string("label") ?: "step"
            val key = "step:$stepId"
            when (eventType) {
                "step.started" -> upsert(
                    ActivityTurnRow(
                        key = key, kind = "step", label = "Step: $stepId", detail = null,
                        status = ActivityRowStatus.Running, tone = ActivityRowTone.Info,
                        durationMs = null,
                    ),
                )
                "step.completed" -> patch(key) {
                    it.status = ActivityRowStatus.Done; it.label = "Step complete: $stepId"
                }
                "step.failed" -> patch(key) {
                    it.status = ActivityRowStatus.Failed; it.label = "Step failed: $stepId"
                    it.tone = ActivityRowTone.Error; it.detail = payload.string("error")
                }
            }
            return
        }

        // ─── Agentic iteration markers ───
        if (eventType == "agentic.iteration_started") {
            val iter = payload.int("iteration")
            upsert(
                ActivityTurnRow(
                    key = "iter:${iter?.toString() ?: "x"}", kind = "step",
                    label = iter?.let { "Iteration $it" } ?: "Iteration", detail = null,
                    status = ActivityRowStatus.Running, tone = ActivityRowTone.Info, durationMs = null,
                ),
            )
            return
        }

        // ─── Artifact / output ───
        if (eventType == "artifact.created" || eventType == "output.created") {
            if (ingestFiles.isNotEmpty()) {
                ingestFiles.forEach { file ->
                    upsert(
                        ActivityTurnRow(
                            key = "$normalizedAgentId::file::${file.absolutePath}", kind = "artifact",
                            label = "${agentPrefix}Produced ${file.label}", detail = null,
                            status = ActivityRowStatus.Done, tone = ActivityRowTone.Tool,
                            durationMs = null, files = listOf(file),
                        ),
                    )
                }
            } else {
                val name = payload.string("display_name") ?: payload.string("name")
                    ?: payload.string("title") ?: "file"
                upsert(
                    ActivityTurnRow(
                        key = "artifact:$name:${orderedKeys.size}", kind = "artifact",
                        label = "${agentPrefix}Produced $name", detail = null,
                        status = ActivityRowStatus.Done, tone = ActivityRowTone.Tool, durationMs = null,
                    ),
                )
            }
            return
        }

        // ─── Tutor run lifecycle (lightweight) ───
        if (eventType.startsWith("tutor.")) {
            val runId = payload.string("run_id") ?: "active"
            val runKey = "tutor:$runId"
            when (eventType) {
                "tutor.run.started" -> {
                    terminalTutorRunIds.remove(runId)
                    upsert(
                        ActivityTurnRow(
                            key = runKey, kind = "step", label = "${agentPrefix}Tutor started",
                            detail = payload.string("goal")?.let { trimTo(it, 180) },
                            status = ActivityRowStatus.Running, tone = ActivityRowTone.Info,
                            durationMs = null,
                        ),
                    )
                }
                "tutor.run.completed" -> {
                    terminalTutorRunIds.add(runId)
                    if (rowsByKey[runKey] != null) {
                        patch(runKey) {
                            it.label = "${agentPrefix}Tutor completed"
                            it.status = ActivityRowStatus.Done; it.tone = ActivityRowTone.Info
                        }
                    } else {
                        upsert(
                            ActivityTurnRow(
                                key = runKey, kind = "step", label = "${agentPrefix}Tutor completed",
                                detail = null, status = ActivityRowStatus.Done,
                                tone = ActivityRowTone.Info, durationMs = null,
                            ),
                        )
                    }
                }
                "tutor.run.failed" -> {
                    terminalTutorRunIds.add(runId)
                    val note = payload.string("note")
                    if (rowsByKey[runKey] != null) {
                        patch(runKey) {
                            it.label = "${agentPrefix}Tutor failed"
                            it.status = ActivityRowStatus.Failed; it.tone = ActivityRowTone.Error
                            it.detail = note?.let { n -> trimTo(n, 180) }
                        }
                    } else {
                        upsert(
                            ActivityTurnRow(
                                key = runKey, kind = "step", label = "${agentPrefix}Tutor failed",
                                detail = note?.let { trimTo(it, 180) }, status = ActivityRowStatus.Failed,
                                tone = ActivityRowTone.Error, durationMs = null,
                            ),
                        )
                    }
                }
                else -> {
                    val step = tutorStepLabel(eventType) ?: return
                    val stepKey = "$runKey:${payload.int("step_count") ?: orderedKeys.size}"
                    val detail = payload.string("step_label") ?: payload.string("target")
                    upsert(
                        ActivityTurnRow(
                            key = stepKey, kind = "step", label = "$agentPrefix${step.first}",
                            detail = detail, status = step.second, tone = step.third, durationMs = null,
                        ),
                    )
                }
            }
            return
        }

        // ─── Coding-engine lifecycle (lightweight) ───
        if (eventType.startsWith("coding.")) {
            val shadow = payload.string("shadow_workspace_id") ?: "coding"
            val key = "coding:$shadow"
            when (eventType) {
                "coding.started" -> {
                    val profile = payload["coding_profile"] as? JsonObject
                    val label = profile?.string("label") ?: profile?.string("id") ?: "Coding agent"
                    upsert(
                        ActivityTurnRow(
                            key = key, kind = "step", label = "${agentPrefix}Coding with $label",
                            detail = null, status = ActivityRowStatus.Running,
                            tone = ActivityRowTone.Info, durationMs = null,
                        ),
                    )
                }
                "coding.completed" -> patch(key) {
                    it.label = "${agentPrefix}Coding finished"
                    it.status = ActivityRowStatus.Done; it.tone = ActivityRowTone.Info
                }
                "coding.failed" -> {
                    val err = payload.string("error") ?: "coding failed"
                    if (rowsByKey[key] != null) {
                        patch(key) {
                            it.label = "${agentPrefix}Coding failed"
                            it.status = ActivityRowStatus.Failed; it.tone = ActivityRowTone.Error
                            it.detail = trimTo(err, 180)
                        }
                    } else {
                        upsert(
                            ActivityTurnRow(
                                key = key, kind = "step", label = "${agentPrefix}Coding failed",
                                detail = trimTo(err, 180), status = ActivityRowStatus.Failed,
                                tone = ActivityRowTone.Error, durationMs = null,
                            ),
                        )
                    }
                }
            }
            return
        }
        // Anything else drops silently, same as the web.
    }

    /**
     * The row key for a chat-side `llm.*` event: the trace/iteration key, or,
     * when the event names an `llm_call_id`, the row that call already has.
     * The server's `llm.requested` names no id and `llm.succeeded` does, so a
     * typed copy that opened its own row first is folded into the trace row
     * while that row still runs (web `chatLlmKey`).
     */
    private fun chatLlmKey(
        eventType: String,
        payload: Map<String, JsonElement>,
        traceKey: String,
        normalizedAgentId: String,
    ): String {
        val callId = activityLlmCallId(payload) ?: return traceKey
        val aliased = llmKeyByCallId[callId]
        val key = when {
            eventType == "llm.requested" -> aliased ?: "$normalizedAgentId::llm::call::$callId"
            aliased != null && aliased != traceKey && rowsByKey[traceKey]?.status == ActivityRowStatus.Running -> {
                drop(aliased)
                traceKey
            }
            else -> aliased ?: traceKey
        }
        // Only a key that holds (or is about to hold) this call's row: a
        // first token that patches nothing must not move the typed copy.
        if (eventType != "llm.first_token" || rowsByKey[key] != null) llmKeyByCallId[callId] = key
        return key
    }

    /** `LLMRequestSent` / `LLMRouted` / `LLMResponseReceived`, one row per call when ids allow. */
    private fun classifyTypedLlm(
        eventType: String,
        payload: Map<String, JsonElement>,
        agentPrefix: String,
        normalizedAgentId: String,
    ) {
        val callKey = activityLlmCallId(payload)?.let { id ->
            llmKeyByCallId.getOrPut(id) { "$normalizedAgentId::llm::call::$id" }
        }
        when (eventType) {
            "LLMRequestSent" -> {
                val key = callKey ?: return
                if (rowsByKey[key] != null) {
                    // A late request never regresses a routed or settled row.
                    patch(key) { if (it.startedAtMs == null) it.startedAtMs = callStartedAtMs(payload) }
                    return
                }
                upsert(
                    ActivityTurnRow(
                        key = key, kind = "llm", label = "${agentPrefix}Thinking…", detail = null,
                        status = ActivityRowStatus.Running, tone = ActivityRowTone.Info,
                        durationMs = null, startedAtMs = callStartedAtMs(payload),
                    ),
                )
            }
            "LLMRouted" -> {
                val key = callKey ?: return
                val model = payload.nonEmpty("model") ?: return
                // Sent again on a fallback: the latest route names the model.
                routedModelByKey[key] = model
                val existing = rowsByKey[key]
                if (existing == null) {
                    upsert(
                        ActivityTurnRow(
                            key = key, kind = "llm", label = "${agentPrefix}Thinking with $model", detail = null,
                            status = ActivityRowStatus.Running, tone = ActivityRowTone.Info,
                            durationMs = null, startedAtMs = callStartedAtMs(payload),
                        ),
                    )
                } else if (existing.status == ActivityRowStatus.Running) {
                    patch(key) { it.label = "${agentPrefix}Thinking with $model" }
                }
            }
            else -> {
                val succeeded = payload.bool("success") != false
                val duration = payload.double("latency_ms")
                val summary = payload.string("decision_summary") ?: payload.string("error")
                val timestampKey = payload.scalar("timestamp")
                    ?: ingestStartedAtMs?.toString() ?: orderedKeys.size.toString()
                val key = callKey ?: "$normalizedAgentId::llm::$timestampKey"
                // A row the chat-side settle already closed keeps what it
                // wrote (time to first token, usage).
                if (callKey != null && rowsByKey[key]?.let { it.status != ActivityRowStatus.Running } == true) return
                // The model that answered, else the routed one, else the capability.
                val model = payload.nonEmpty("model") ?: payload.nonEmpty("harness_model")
                    ?: routedModelByKey[key] ?: payload.string("capability") ?: "model"
                val usage = readActivityCallUsage(payload)
                val settle: (ActivityTurnRow) -> Unit = {
                    it.label = "${agentPrefix}Thinking with $model"
                    it.detail = duration?.let(::formatLatency) ?: summary?.let { s -> trimTo(s, 180) }
                    it.status = if (succeeded) ActivityRowStatus.Done else ActivityRowStatus.Failed
                    it.tone = if (succeeded) ActivityRowTone.Info else ActivityRowTone.Error
                    it.durationMs = duration
                    it.callUsage = usage
                }
                if (rowsByKey[key] == null) {
                    upsert(
                        ActivityTurnRow(
                            key = key, kind = "llm", label = "", detail = null,
                            status = ActivityRowStatus.Done, tone = ActivityRowTone.Info, durationMs = null,
                        ),
                    )
                }
                patch(key, settle)
            }
        }
    }

    /** When a call began: its own `started_at` stamp, else the event's timestamp. */
    private fun callStartedAtMs(payload: Map<String, JsonElement>): Double? {
        val stamped = payload.double("started_at_ms") ?: payload.double("started_at")
        val millis = stamped?.takeIf { it.isFinite() && it > 0 }?.let { if (it < 10_000_000_000.0) it * 1000 else it }
        return millis ?: ingestStartedAtMs
    }

    private fun tutorStepLabel(eventType: String): Triple<String, ActivityRowStatus, ActivityRowTone>? =
        when (eventType) {
            "tutor.step.observed" -> Triple("Observed screen", ActivityRowStatus.Done, ActivityRowTone.Info)
            "tutor.step.target_resolved" -> Triple("Resolved target", ActivityRowStatus.Done, ActivityRowTone.Info)
            "tutor.step.drawing" -> Triple("Drawing marker", ActivityRowStatus.Done, ActivityRowTone.Tool)
            "tutor.step.action_delegated" -> Triple("Action delegated", ActivityRowStatus.Running, ActivityRowTone.Tool)
            "tutor.step.verifying" -> Triple("Verifying action", ActivityRowStatus.Running, ActivityRowTone.Info)
            "tutor.step.verified" -> Triple("Verified action", ActivityRowStatus.Done, ActivityRowTone.Info)
            "tutor.step.failed" -> Triple("Tutor step failed", ActivityRowStatus.Failed, ActivityRowTone.Error)
            "tutor.step.recovering" -> Triple("Recovering tutor flow", ActivityRowStatus.Running, ActivityRowTone.Pause)
            "tutor.step.clearing" -> Triple("Clearing tutor marks", ActivityRowStatus.Done, ActivityRowTone.Tool)
            else -> null
        }
}

// ─── Turn-events normalization (port of iOS's normalizedActivityEvents) ───

/**
 * Chat fan-out can persist a logical event twice and file order is not
 * guaranteed chronological; lifecycle completion must apply after its start.
 * Dedupe on the strongest identity available, then sort by timestamp with
 * arrival order as the tiebreak.
 */
internal fun normalizedChatTurnEvents(events: List<JsonObject>): List<JsonObject> {
    val seen = mutableSetOf<String>()
    data class Entry(val index: Int, val timestamp: Double, val event: JsonObject)
    val unique = mutableListOf<Entry>()
    events.forEachIndexed { index, event ->
        val key = activityEventDedupeKey(event)
        if (key != null && !seen.add(key)) return@forEachIndexed
        unique.add(Entry(index, activityTimestampMs(event) ?: 0.0, event))
    }
    return unique
        .sortedWith(compareBy({ it.timestamp }, { it.index }))
        .map { it.event }
}

/** Normalize, accumulate, snapshot — the whole projection in one call. */
internal fun chatTurnActivityRows(events: List<JsonObject>): List<ActivityRow> {
    val accumulator = ChatTurnActivityAccumulator()
    normalizedChatTurnEvents(events).forEach(accumulator::ingest)
    return accumulator.snapshot()
}

private fun activityEventDedupeKey(event: JsonObject): String? {
    val data = event.obj("data")
    val inner = data?.obj("event")
    val payload = inner?.obj("payload")
    payload?.string("event_id")?.takeIf { it.isNotEmpty() }?.let { return it }
    data?.obj("message")?.string("id")?.takeIf { it.isNotEmpty() }?.let { return it }
    data?.string("event_id")?.takeIf { it.isNotEmpty() }?.let { return it }

    val type = inner?.string("event_type") ?: event.string("event_type") ?: ""
    if (type.isEmpty()) return null
    val agent = inner?.string("agent_id") ?: data?.string("agent_id") ?: ""
    val execution = data?.scalarMember("execution_id").orEmpty()
    // Several actions can share an iteration (and a millisecond); their
    // `call_id` keeps them apart, as `llm_call_id` keeps model calls apart.
    val discriminator = (
        data?.scalarMember("iteration")
            ?: data?.scalarMember("step_index")
            ?: data?.scalarMember("call_id")
        ).orEmpty() + (data?.scalarMember("call_id")?.let { "|$it" }.orEmpty()) +
        (data?.let(::activityLlmCallId)?.let { "|llm:$it" }.orEmpty())
    return "$agent|$type|$execution|$discriminator|${activityTimestampMs(event) ?: 0.0}"
}

// ─── Pure helpers, ported verbatim ───

/** An answer to a pause: the realtime events that resolve a HITL ask. */
private val terminalFailureStatuses = setOf("failed", "error", "cancelled", "canceled")

/**
 * Safety net for a delegated task whose step strip never settles: the
 * transcript's latest `task_status_update` is authoritative, so a terminal
 * card settles that task's still-running `task::<id>` row — and, once no task
 * row is left running, its stale leaves, exactly as a terminal replay does.
 * [terminalTaskStatuses] maps task id to the card's lowercased status.
 */
internal fun settleActivityRows(
    rows: List<ActivityRow>,
    terminalTaskStatuses: Map<String, String>,
): List<ActivityRow> {
    if (terminalTaskStatuses.isEmpty() || rows.isEmpty()) return rows
    var changed = false
    val settled = rows.map { row ->
        val key = row.key
        if (key == null || !key.startsWith("task::") ||
            (row.status != ActivityRowStatus.Running.wire && row.status != ActivityRowStatus.Waiting.wire)
        ) return@map row
        val status = terminalTaskStatuses[key.removePrefix("task::")]
            ?: row.taskId?.let(terminalTaskStatuses::get)
            ?: return@map row
        val failed = status in terminalFailureStatuses
        changed = true
        row.copy(
            label = "${activityTaskDisplayName(row.label) ?: "Task"} $status",
            status = if (failed) ActivityRowStatus.Failed.wire else ActivityRowStatus.Done.wire,
            tone = if (failed) "error" else "tool",
        )
    }
    if (!changed) return rows
    val taskRows = settled.filter { it.key?.startsWith("task::") == true }
    if (taskRows.any { it.status == ActivityRowStatus.Running.wire || it.status == ActivityRowStatus.Waiting.wire }) {
        return settled
    }
    val failed = taskRows.any { it.status == ActivityRowStatus.Failed.wire }
    return settled.map { row ->
        if (row.key?.startsWith("task::") == true || row.status != ActivityRowStatus.Running.wire) return@map row
        row.copy(
            status = if (failed) ActivityRowStatus.Failed.wire else ActivityRowStatus.Done.wire,
            tone = if (failed) "error" else row.tone,
            detail = row.detail ?: if (failed) "Stopped when task failed" else "Settled when task finished",
        )
    }
}

/** A `task::` row's label is "<display> <status>" or "<display> preparing final result". */
private fun activityTaskDisplayName(label: String): String? {
    val pending = " preparing final result"
    if (label.endsWith(pending)) return label.removeSuffix(pending)
    val space = label.lastIndexOf(' ')
    if (space <= 0) return null
    return label.substring(0, space).trim().ifEmpty { null }
}

internal fun isPauseResolvedEvent(outer: String, event: String): Boolean {
    if (outer == "HitlResolved") return true
    return event == "hitl.resolved" || event == "input.received" || event == "input.resolved"
}

/**
 * The turn reads "Waiting on you" while any pause row still waits; an
 * answered ask (settled to "Answered") clears it.
 */
fun activityAwaitsUser(rows: List<ActivityRow>): Boolean =
    rows.any { it.kind == "pause" && it.status == ActivityRowStatus.Waiting.wire }

/**
 * The newest [limit] rows the inline Steps panel shows; the header keeps the
 * whole count and "All" opens the rest (iOS `activityPreviewRows`).
 */
fun activityPreviewRows(rows: List<ActivityRow>, limit: Int = 5): List<ActivityRow> =
    if (limit <= 0) emptyList() else rows.takeLast(limit)

/** The Steps header's dot strip: the latest settled rows and how many earlier ones fold into "+N". */
data class ActivityStageDots(val shown: List<ActivityRow>, val hidden: Int)

/**
 * How many settled-step dots the Steps header draws before folding the rest
 * into "+N". Three keeps the one-line header (count, dots, usage, Inspect,
 * Stop) from crowding on a phone.
 */
const val ACTIVITY_STAGE_DOT_LIMIT = 3

/**
 * One dot per settled row, then one for the newest running row, the latest
 * [limit] only (web TurnStage, iOS `activityStageDots`). The running dot is
 * last, so it is the one the live header breathes; other running rows get no
 * dot until they settle.
 */
fun activityStageDots(rows: List<ActivityRow>, limit: Int = ACTIVITY_STAGE_DOT_LIMIT): ActivityStageDots {
    val settled = rows.filter { it.status != ActivityRowStatus.Running.wire }
    val dots = settled + listOfNotNull(rows.lastOrNull { it.status == ActivityRowStatus.Running.wire })
    val hidden = (dots.size - limit.coerceAtLeast(0)).coerceAtLeast(0)
    return ActivityStageDots(dots.drop(hidden), hidden)
}

/** Where the agentic normalization puts an action's human `detail` for the tool branch. */
private const val ACTIVITY_LABEL_KEY = "__activity_label"
/** A tool row's display name never runs past this. */
internal const val ACTIVITY_TOOL_LABEL_MAX = 100
/** A `__call_tool` argument summary (its sql / query / term) keeps this much. */
internal const val CALL_TOOL_SUMMARY_MAX = 80
/** A `__call_tool` with none of those fields shows its raw arguments only when this short. */
internal const val CALL_TOOL_RAW_ARGS_MAX = 100

private val CALL_TOOL_SUMMARY_FIELDS = listOf("sql", "query", "term", "q", "search", "question", "prompt")

/** A tool event's arguments object (`args`, else `arguments`), parsed when sent as a JSON string. */
private fun toolCallArgs(payload: Map<String, JsonElement>): JsonObject? =
    asJsonObject(payload["args"]) ?: asJsonObject(payload["arguments"])

private fun asJsonObject(element: JsonElement?): JsonObject? = when (element) {
    is JsonObject -> element
    is JsonPrimitive -> element.takeIf { it.isString }?.content?.let { text ->
        runCatching { Json.parseToJsonElement(text) as? JsonObject }.getOrNull()
    }
    else -> null
}

/**
 * The display name of an MCP pack's generic `<pack>__call_tool`: "<pack> ·
 * <remote tool>", then " — " and a short summary of its `arguments_json` (the
 * first sql / query / term field, [CALL_TOOL_SUMMARY_MAX] chars). Raw
 * arguments show only when no longer than [CALL_TOOL_RAW_ARGS_MAX]. Null for
 * any other tool.
 */
internal fun callToolDisplayName(tool: String, args: JsonObject?): String? {
    val suffix = listOf("__run", "__exec", "__execute").firstOrNull { tool.endsWith(it) }
    val argv = (args?.get("args") ?: args?.get("argv")) as? kotlinx.serialization.json.JsonArray
    val command = (argv?.firstOrNull() as? JsonPrimitive)?.takeIf { it.isString }?.content
    if (suffix != null && command != null && command.matches(Regex("^[A-Za-z][A-Za-z0-9_-]{0,63}$"))) {
        val verbs = setOf("list", "get", "show", "search", "create", "generate", "update", "delete", "remove", "send", "upload", "download", "status", "info", "help", "login", "logout", "run", "start", "stop", "wait", "select", "export", "import", "query", "render", "convert", "inspect")
        val verb = (argv?.getOrNull(1) as? JsonPrimitive)?.takeIf { it.isString }?.content?.takeIf { it in verbs }
        return trimTo("${tool.removeSuffix(suffix)} · $command${verb?.let { " $it" }.orEmpty()}", ACTIVITY_TOOL_LABEL_MAX)
    }
    if (!tool.endsWith("__call_tool")) return null
    val pack = tool.removeSuffix("__call_tool").trim().ifEmpty { return null }
    val remote = args?.nonEmptyMember("tool_name") ?: args?.nonEmptyMember("name")
    val head = if (remote != null) "$pack · $remote" else pack
    val summary = callToolArgumentSummary(args?.get("arguments_json") ?: args?.get("arguments"))
    return trimTo(if (summary != null) "$head — $summary" else head, ACTIVITY_TOOL_LABEL_MAX)
}

private fun callToolArgumentSummary(element: JsonElement?): String? {
    val raw = when (element) {
        null, is JsonNull -> return null
        is JsonPrimitive -> element.takeIf { it.isString }?.content ?: element.content
        else -> element.toString()
    }.trim()
    if (raw.isEmpty()) return null
    val parsed = asJsonObject(element)
    if (parsed != null) {
        CALL_TOOL_SUMMARY_FIELDS.firstNotNullOfOrNull { parsed.nonEmptyMember(it) }
            ?.let { return trimTo(it, CALL_TOOL_SUMMARY_MAX) }
        if (parsed.isEmpty()) return null
    }
    val compact = trimTo(raw, Int.MAX_VALUE)
    return compact.takeIf { it.length <= CALL_TOOL_RAW_ARGS_MAX }
}

/**
 * Stable tint slot 1..5 for [key] — the same FNV-1a over UTF-16 code units as
 * web `tintFor`, so a tool keeps its colour on every client.
 */
fun activityTintSlot(key: String): Int {
    var hash = 0x811c9dc5.toInt()
    for (ch in key) {
        hash = hash xor ch.code
        hash *= 0x01000193
    }
    return (hash.toUInt() % 5u).toInt() + 1
}

/** The key a row's dot tint hashes: its tool, else its label (web `tool ?? label`). */
fun activityTintKey(row: ActivityRow): String = row.toolName ?: row.label

/** The run a turn's Steps can inspect or stop: a durable task and its execution. */
data class ActivityInspectTarget(val taskId: String, val executionId: String)

/**
 * The most recent execution id and the most recent task id, captured
 * independently (transport events do not carry them together); both are
 * required, because inline model turns carry telemetry execution ids with no
 * durable run behind them (iOS `activityInspectTarget`).
 */
fun activityInspectTarget(rows: List<ActivityRow>): ActivityInspectTarget? {
    var taskId: String? = null
    var executionId: String? = null
    for (row in rows.asReversed()) {
        if (executionId == null) executionId = row.executionId?.trim()?.takeIf { it.isNotEmpty() }
        if (taskId == null) taskId = row.taskId?.trim()?.takeIf { it.isNotEmpty() }
        if (executionId != null && taskId != null) break
    }
    return if (taskId != null && executionId != null) ActivityInspectTarget(taskId, executionId) else null
}

/** A step's latency for its row: `920ms`, `1.8s` (iOS `formatLatency`). */
fun activityLatencyText(ms: Double): String = formatLatency(ms)

/**
 * The detail line a Steps row shows: its detail, with a finished model
 * call's usage appended (`· 9.1k tok · $0.0123`); null when there is neither.
 */
fun activityRowDetail(row: ActivityRow): String? {
    val suffix = if (row.kind == "llm" && row.status == ActivityRowStatus.Done.wire) {
        ai.magicbeans.magdroid.tasks.TaskUsageFormat.stepSuffix(row.callUsage?.stepTokens, row.callUsage?.costUsd)
    } else ""
    return ai.magicbeans.magdroid.tasks.TaskUsageFormat.withUsage(row.detail, suffix)
}

/** Tools whose result is a task the turn started. Delegation is not here. */
internal val activitySpawningTools = setOf("create_task", "run_task")

private val legacyTaskIdPattern = Regex("task_[0-9a-f]{32}")

/**
 * Ids of the tasks a `tool.call.finished` payload started: its
 * `spawned_task_ids`, else (turns recorded before that field) the task ids
 * in a spawning tool's result preview.
 */
internal fun activitySpawnedTaskIds(payload: Map<String, JsonElement>, toolName: String): List<String> {
    (payload["spawned_task_ids"] as? JsonArray)?.let { stated ->
        return stated.mapNotNull { element ->
            primitive(element)?.takeIf { it.isString }?.content?.trim()?.takeIf { it.isNotEmpty() }
        }
    }
    if (toolName !in activitySpawningTools) return emptyList()
    val preview = payload.string("content_preview") ?: return emptyList()
    return legacyTaskIdPattern.findAll(preview).map { it.value }.distinct().toList()
}

/** A finite JSON number, never a boolean or a string. */
private fun usageNumber(element: JsonElement?): Double? =
    primitive(element)?.takeIf { !it.isString && it.booleanOrNull == null }
        ?.doubleOrNull?.takeIf { it.isFinite() }

/** A model call's stable id: `llm_call_id`, at the top level or under `correlation`. */
internal fun activityLlmCallId(payload: Map<String, JsonElement>): String? =
    payload.nonEmpty("llm_call_id") ?: (payload["correlation"] as? JsonObject)?.nonEmptyMember("llm_call_id")

/**
 * When a running row's live elapsed clock starts: model and tool rows only,
 * and only while they run; null otherwise (a settled row shows its duration).
 */
fun activityLiveSinceMs(row: ActivityRow): Double? =
    row.startedAtMs?.takeIf {
        row.status == ActivityRowStatus.Running.wire && (row.kind == "llm" || row.kind == "tool")
    }

/** One finished model call's usage, read off its payload (iOS `ActivityCallUsage.read`). */
internal fun readActivityCallUsage(payload: Map<String, JsonElement>): ActivityCallUsage {
    val correlation = payload["correlation"] as? JsonObject
    val availability = (payload["usage_availability"] ?: correlation?.get("usage_availability")) as? JsonObject
    fun flag(key: String): Boolean? = primitive(availability?.get(key))?.takeIf { !it.isString }?.booleanOrNull
    val tokensAvailable = flag("tokens") != false && primitive(payload["usage_reported"])?.booleanOrNull != false
    val rawInput = usageNumber(payload["input_tokens"])
    val rawOutput = usageNumber(payload["output_tokens"])
    val input = if (tokensAvailable) rawInput else null
    val output = if (tokensAvailable) rawOutput else null
    val cacheRead = if (flag("cache_read") == false) null else usageNumber(payload["cache_read_tokens"])
    val cacheWrite = if (flag("cache_write") == false) null else usageNumber(payload["cache_creation_tokens"])
    val callId = activityLlmCallId(payload)
    return ActivityCallUsage(
        callId = callId,
        inputTokens = input?.toInt(),
        outputTokens = output?.toInt(),
        reportedTokens = input != null || output != null || cacheRead != null || cacheWrite != null,
        stepTokens = if (input == null && output == null) null else ((input ?: 0.0) + (output ?: 0.0)).toInt(),
        costUsd = if (flag("cost") == false) null else usageNumber(payload["cost_usd"]) ?: usageNumber(payload["cost"]),
    )
}

internal fun isPauseEvent(outer: String, event: String): Boolean {
    if (outer == "HitlRequested") return true
    return event == "waiting_for_confirmation" ||
        event == "execution.waiting_for_user" ||
        event == "input.requested" ||
        event == "hitl.requested" ||
        event == "clarification.queued"
}

internal fun trimTo(value: String, max: Int): String {
    val trimmed = value.replace(Regex("\\s+"), " ").trim()
    if (trimmed.length <= max) return trimmed
    return trimmed.take(max - 1) + "…"
}

internal fun formatLatency(ms: Double): String {
    if (!ms.isFinite() || ms < 0) return "—"
    if (ms < 1000) return "${Math.round(ms)}ms"
    // Half-even, not Java's default half-up: Swift's String(format:) rounds
    // like C printf, so 1250ms is "1.2s" there and must be "1.2s" here.
    val seconds = java.math.BigDecimal(ms / 1000)
        .setScale(1, java.math.RoundingMode.HALF_EVEN)
    return "${seconds}s"
}

/** `TTFT (total)` — total renders as `…` until known. */
internal fun formatLatencyPair(ttft: Double?, total: Double?): String {
    val t = ttft?.let(::formatLatency) ?: "…"
    val all = total?.let(::formatLatency) ?: "…"
    return "$t ($all)"
}

private fun activityTimestampMs(parsed: JsonObject): Double? {
    val data = parsed.obj("data")
    val inner = data?.obj("event")
    val payload = inner?.obj("payload")
    val candidates = listOf(
        payload?.numberMember("timestamp_ms"), payload?.numberMember("timestamp"),
        inner?.numberMember("timestamp_ms"), inner?.numberMember("timestamp"),
        data?.numberMember("timestamp_ms"), data?.numberMember("timestamp"),
        parsed.numberMember("timestamp_ms"),
    )
    for (candidate in candidates) {
        if (candidate == null || !candidate.isFinite()) continue
        return if (candidate < 10_000_000_000.0) candidate * 1000 else candidate
    }
    return null
}

private fun extractActivityFiles(payload: Map<String, JsonElement>): List<ActivityFileRef> {
    val output = mutableListOf<ActivityFileRef>()
    val seen = mutableSetOf<String>()
    for (field in listOf("output_files", "files", "content_blocks")) {
        val entries = payload[field] as? JsonArray ?: continue
        for (entry in entries) {
            val obj = entry as? JsonObject ?: continue
            val path = obj.nonEmptyMember("absolute_path") ?: obj.nonEmptyMember("path") ?: continue
            if (!path.startsWith("/") || !seen.add(path)) continue
            val label = obj.nonEmptyMember("label")
                ?: obj.nonEmptyMember("display_name")
                ?: obj.nonEmptyMember("relative_path")
                ?: path.substringAfterLast('/')
            output.add(ActivityFileRef(path, label))
        }
    }
    if (output.isEmpty()) {
        val path = payload.nonEmpty("absolute_path")
        if (path != null && path.startsWith("/")) {
            val label = payload.nonEmpty("display_name")
                ?: payload.nonEmpty("name")
                ?: path.substringAfterLast('/')
            output.add(ActivityFileRef(path, label))
        }
    }
    return output
}

private fun parseActivityResultOwner(value: JsonElement?): ActivityResultOwner? {
    val wire = value as? JsonObject ?: return null
    return when (val kind = wire.nonEmptyMember("kind")) {
        "chat" -> wire.nonEmptyMember("session_id")?.let {
            ActivityResultOwner(kind = kind, sessionId = it)
        }
        "task" -> wire.nonEmptyMember("task_id")?.let {
            ActivityResultOwner(
                kind = kind,
                taskId = it,
                executionId = wire.nonEmptyMember("execution_id"),
            )
        }
        "ephemeral_voice" -> wire.nonEmptyMember("voice_session_id")?.let {
            ActivityResultOwner(kind = kind, voiceSessionId = it)
        }
        else -> null
    }
}

// ─── Strict JSON access, matching iOS's `as?` casts ───

private fun JsonObject.obj(key: String): JsonObject? = this[key] as? JsonObject

private fun primitive(element: JsonElement?): JsonPrimitive? =
    (element as? JsonPrimitive)?.takeIf { it !is JsonNull }

private fun JsonObject.string(key: String): String? =
    primitive(this[key])?.takeIf { it.isString }?.content

private fun JsonObject.numberMember(key: String): Double? =
    primitive(this[key])?.takeIf { !it.isString }?.doubleOrNull

private fun JsonObject.nonEmptyMember(key: String): String? =
    string(key)?.trim()?.takeIf { it.isNotEmpty() }

private fun JsonObject.scalarMember(key: String): String? =
    primitive(this[key])?.let { p ->
        if (p.isString) p.content.trim().takeIf { it.isNotEmpty() } else p.content
    }

private fun Map<String, JsonElement>.string(key: String): String? =
    primitive(this[key])?.takeIf { it.isString }?.content

private fun Map<String, JsonElement>.bool(key: String): Boolean? =
    primitive(this[key])?.takeIf { !it.isString }?.booleanOrNull

private fun Map<String, JsonElement>.int(key: String): Int? =
    primitive(this[key])?.takeIf { !it.isString }?.doubleOrNull
        ?.takeIf { it == Math.floor(it) && it.isFinite() }?.toInt()

private fun Map<String, JsonElement>.double(key: String): Double? =
    primitive(this[key])?.takeIf { !it.isString }?.doubleOrNull

private fun Map<String, JsonElement>.nonEmpty(key: String): String? =
    string(key)?.trim()?.takeIf { it.isNotEmpty() }

private fun Map<String, JsonElement>.scalar(key: String): String? =
    primitive(this[key])?.let { p ->
        if (p.isString) p.content.trim().takeIf { it.isNotEmpty() } else p.content
    }
