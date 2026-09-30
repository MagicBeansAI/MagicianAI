package ai.magicbeans.magdroid.chat

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

internal data class ActivityFileRef(val absolutePath: String, val label: String)

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
)

internal class ChatTurnActivityAccumulator {

    private val rowsByKey = LinkedHashMap<String, ActivityTurnRow>()
    private val orderedKeys = mutableListOf<String>()
    private val terminalTutorRunIds = mutableSetOf<String>()
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
        // AgentEvent-enveloped tool lifecycle. Normalize the completed action
        // into the same tool branch the web uses.
        if (outerType == "AgenticActionExecuted") {
            eventType = if (payload.bool("success") == false) "tool.call.failed" else "tool.call.finished"
            payload["tool_name"] = JsonPrimitive(
                payload.nonEmpty("target") ?: payload.nonEmpty("action_type") ?: "tool",
            )
            payload["call_id"] = JsonPrimitive(
                listOf(
                    payload.scalar("step_id") ?: "step",
                    payload.scalar("iteration") ?: "0",
                    payload.scalar("timestamp") ?: "0",
                ).joinToString("-"),
            )
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

        // Typed flat-loop LLM completions carry no stable call id that can
        // pair request/response, so each response is one terminal row keyed
        // by its timestamp, exactly as on web.
        if (eventType == "LLMResponseReceived") {
            val capability = payload.string("capability") ?: "model"
            val succeeded = payload.bool("success") != false
            val duration = payload.double("latency_ms")
            val summary = payload.string("decision_summary") ?: payload.string("error")
            val timestampKey = payload.scalar("timestamp")
                ?: ingestStartedAtMs?.toString() ?: orderedKeys.size.toString()
            upsert(
                ActivityTurnRow(
                    key = "$normalizedAgentId::llm::$timestampKey",
                    kind = "llm",
                    label = "${agentPrefix}Thinking with $capability",
                    detail = duration?.let(::formatLatency) ?: summary?.let { trimTo(it, 180) },
                    status = if (succeeded) ActivityRowStatus.Done else ActivityRowStatus.Failed,
                    tone = if (succeeded) ActivityRowTone.Info else ActivityRowTone.Error,
                    durationMs = duration,
                ),
            )
            return
        }
        if (eventType == "LLMRequestSent") return

        // ─── LLM lifecycle (coalesced by trace_id or iteration) ───
        if (eventType.startsWith("llm.")) {
            val trace = payload.string("trace_id") ?: payload.int("iteration")?.toString() ?: "llm"
            val key = "$normalizedAgentId::llm::$trace"
            when (eventType) {
                "llm.requested" -> {
                    if (rowsByKey[key] != null) return
                    val model = payload.string("model") ?: payload.string("capability") ?: "model"
                    upsert(
                        ActivityTurnRow(
                            key = key, kind = "llm", label = "${agentPrefix}Thinking with $model",
                            detail = null, status = ActivityRowStatus.Running,
                            tone = ActivityRowTone.Info, durationMs = null,
                        ),
                    )
                }
                "llm.first_token" -> {
                    payload.double("duration_ms")?.let { ttft ->
                        patch(key) { it.detail = formatLatencyPair(ttft, null) }
                    }
                }
                "llm.succeeded" -> {
                    val ms = payload.double("duration_ms")
                    val ttft = payload.double("ttft_ms")
                    if (rowsByKey[key] != null) {
                        patch(key) {
                            it.status = ActivityRowStatus.Done; it.tone = ActivityRowTone.Info
                            it.detail = formatLatencyPair(ttft, ms); it.durationMs = ms
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
                    var label = "${agentPrefix}Calling $tool"
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
                            durationMs = null,
                        ),
                    )
                }
                eventType.endsWith(".succeeded") || eventType.endsWith(".finished") -> {
                    val ms = payload.double("duration_ms")
                    val preview = payload.string("content_preview")?.let { trimTo(it, 200) }
                    if (rowsByKey[key] != null) {
                        patch(key) {
                            it.label = "$agentPrefix$tool returned"
                            it.status = ActivityRowStatus.Done; it.tone = ActivityRowTone.Tool
                            it.detail = preview; it.durationMs = ms
                        }
                    } else {
                        upsert(
                            ActivityTurnRow(
                                key = key, kind = "tool", label = "$agentPrefix$tool returned",
                                detail = preview, status = ActivityRowStatus.Done,
                                tone = ActivityRowTone.Tool, durationMs = ms,
                            ),
                        )
                    }
                }
                eventType.endsWith(".failed") -> {
                    val err = payload.string("error") ?: "tool failed"
                    if (rowsByKey[key] != null) {
                        patch(key) {
                            it.label = "$agentPrefix$tool failed"
                            it.status = ActivityRowStatus.Failed; it.tone = ActivityRowTone.Error
                            it.detail = trimTo(err, 200)
                        }
                    } else {
                        upsert(
                            ActivityTurnRow(
                                key = key, kind = "tool", label = "$agentPrefix$tool failed",
                                detail = trimTo(err, 200), status = ActivityRowStatus.Failed,
                                tone = ActivityRowTone.Error, durationMs = null,
                            ),
                        )
                    }
                }
            }
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
    val discriminator = (
        data?.scalarMember("iteration")
            ?: data?.scalarMember("step_index")
            ?: data?.scalarMember("call_id")
        ).orEmpty()
    return "$agent|$type|$execution|$discriminator|${activityTimestampMs(event) ?: 0.0}"
}

// ─── Pure helpers, ported verbatim ───

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
