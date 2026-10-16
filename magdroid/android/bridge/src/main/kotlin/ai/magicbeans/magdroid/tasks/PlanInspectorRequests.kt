package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/*
 * The Plan Inspector's API: the task plan endpoints the web inspector uses
 * (`ExecutionPlanInspector.svelte`, `taskStore.ts` approve/reject/replan).
 * Android port of iOS `PlanInspectorClient.swift`. Requests are built here as
 * plain values and sent through a `PlanInspectorTransport` — the app's
 * authorised HTTP client in production, a stub in tests.
 */

// Wire records

enum class PlanStatus(val wire: String) {
    Planning("planning"), Draft("draft"), Eliciting("eliciting"),
    Approved("approved"), Rejected("rejected"), Failed("failed");

    /**
     * Editing is open while the plan is not running or settled. Approved
     * plans stay editable; saving writes a new version.
     */
    val canEdit: Boolean get() = this == Draft || this == Eliciting || this == Approved

    val label: String get() = wire.replaceFirstChar { it.uppercase() }

    companion object {
        fun fromWire(value: String?): PlanStatus? = value?.lowercase()?.let { v -> entries.firstOrNull { it.wire == v } }
    }
}

data class PlanQuestionOption(val value: String, val label: String, val description: String?) {
    val id: String get() = value
}

data class PlanQuestion(
    val id: String,
    val text: String,
    val options: List<PlanQuestionOption>,
    val sourceSlotId: String?,
    val urgency: Double?,
)

/** The `/plan` envelope (`TaskPlanRecord`). */
data class PlanRecord(
    val planId: String?,
    val status: PlanStatus?,
    val rawStatus: String?,
    val graph: PlanGraph?,
    val pendingQuestions: List<PlanQuestion>,
    val error: String?,
) {
    companion object {
        fun parse(json: JsonElement?): PlanRecord {
            val obj = json.jObj() ?: JsonObject(emptyMap())
            val rawStatus = obj["status"].jStr()
            return PlanRecord(
                planId = obj["plan_id"].jStr()?.takeIf { it.isNotEmpty() },
                status = PlanStatus.fromWire(rawStatus),
                rawStatus = rawStatus,
                graph = PlanGraph.decode(obj["plan_graph"]),
                pendingQuestions = obj["pending_questions"].jArr().orEmpty().mapNotNull { value ->
                    val q = value.jObj() ?: return@mapNotNull null
                    val id = q["id"].jStr() ?: return@mapNotNull null
                    PlanQuestion(
                        id = id,
                        text = q["question_text"].jStr() ?: "",
                        options = q["options"].jArr().orEmpty().mapNotNull { option ->
                            val o = option.jObj() ?: return@mapNotNull null
                            val optionValue = o["value"].jStr() ?: return@mapNotNull null
                            PlanQuestionOption(optionValue, o["label"].jStr() ?: optionValue, o["description"].jStr())
                        },
                        sourceSlotId = q["source_slot_id"].jStr(),
                        urgency = q["urgency"].jNum(),
                    )
                },
                error = obj["error"].jStr(),
            )
        }

        /**
         * A mutation's response, with the previous record (or the saved draft)
         * filling in whatever the response left out — web `applyCurrentPlanRecord`.
         */
        fun merging(response: PlanRecord, fallback: PlanRecord?, graph: PlanGraph?): PlanRecord = PlanRecord(
            planId = response.planId ?: fallback?.planId,
            status = response.status ?: fallback?.status,
            rawStatus = response.rawStatus ?: fallback?.rawStatus,
            graph = response.graph ?: graph ?: fallback?.graph,
            pendingQuestions = if (response.planId == null) fallback?.pendingQuestions.orEmpty() else response.pendingQuestions,
            error = response.error,
        )
    }
}

data class PlanVersionSummary(
    val epochMs: Long,
    val label: String?,
    val stepCount: Int,
    val confidence: Double?,
) {
    val id: Long get() = epochMs

    val title: String get() = label?.trim()?.takeIf { it.isNotEmpty() } ?: "Saved version"
}

data class PlanVersionsPage(val versions: List<PlanVersionSummary>, val hasMore: Boolean)

data class PlanSlotRecord(
    val id: String,
    val slotType: String,
    val value: JsonElement,
    val confidence: Double,
    val sources: List<String>,
) {
    /** The id after the last `::` (slot ids are namespaced). */
    val shortId: String get() = id.split("::").last()
}

data class PlanSlotGroup(val type: String, val slots: List<PlanSlotRecord>) {
    val id: String get() = type
    val averageConfidence: Double
        get() = if (slots.isEmpty()) 0.0 else slots.sumOf { it.confidence } / slots.size
}

data class PlanSlotSnapshot(
    val executionId: String?,
    val slots: List<PlanSlotRecord>,
    val overallConfidence: Double?,
    val unresolvedSlots: List<String>,
    val clarifiedTask: JsonElement?,
    val triggerMappings: JsonElement?,
    val boostResults: JsonElement?,
    val enrichmentSummary: JsonElement?,
) {
    /** Slots grouped by type, the largest group first (web `computeSlotGroups`). */
    val groups: List<PlanSlotGroup>
        get() {
            val byType = LinkedHashMap<String, MutableList<PlanSlotRecord>>()
            for (slot in slots) byType.getOrPut(slot.slotType) { mutableListOf() }.add(slot)
            // Stable sort: equal sizes keep first-seen order.
            return byType.map { PlanSlotGroup(it.key, it.value) }.sortedByDescending { it.slots.size }
        }

    companion object {
        fun parse(json: JsonElement): PlanSlotSnapshot {
            val obj = json.jObj() ?: JsonObject(emptyMap())
            val summary = obj["confidence_summary"].jObj()
            return PlanSlotSnapshot(
                executionId = obj["execution_id"].jStr(),
                slots = obj["slots"].jArr().orEmpty().mapNotNull { value ->
                    val s = value.jObj() ?: return@mapNotNull null
                    val id = s["id"].jStr() ?: return@mapNotNull null
                    PlanSlotRecord(
                        id = id,
                        slotType = slotType(s["slot_type"]),
                        value = s["value"] ?: JsonNull,
                        confidence = s["confidence"].jNum() ?: 0.0,
                        sources = s["provenance"].jArr().orEmpty().mapNotNull { it.jObj()?.get("source").jStr() },
                    )
                },
                overallConfidence = summary?.get("overall").jNum(),
                unresolvedSlots = summary?.get("unresolved_slots").jArr().orEmpty().mapNotNull { it.jStr() },
                clarifiedTask = nonEmpty(obj["clarified_task"]),
                triggerMappings = nonEmpty(obj["slot_trigger_mappings"]),
                boostResults = nonEmpty(obj["confidence_boost_results"]),
                enrichmentSummary = nonEmpty(obj["enrichment_summary"]),
            )
        }

        /** A slot type arrives as a string or an externally tagged enum. */
        private fun slotType(value: JsonElement?): String {
            value.jStr()?.let { return it.lowercase() }
            value.jObj()?.keys?.sorted()?.firstOrNull()?.let { return it.lowercase() }
            return "other"
        }

        private fun nonEmpty(value: JsonElement?): JsonElement? = when {
            value == null || value is JsonNull -> null
            value is JsonArray && value.isEmpty() -> null
            value is JsonObject && value.isEmpty() -> null
            else -> value
        }
    }
}

data class PlanStrategyAttempt(
    val index: Int,
    val strategyType: String?,
    val attemptNumber: Int?,
    val failureReason: String?,
    val attemptedAtMillis: Long?,
    val explorationResult: JsonElement?,
    val resourcesUsed: JsonElement?,
) {
    val id: Int get() = index
    val title: String get() = "${strategyType ?: "Strategy attempt"} #${attemptNumber ?: (index + 1)}"
    val confidence: Double? get() = explorationResult.jObj()?.get("confidence").jNum()
}

data class PlanClarificationEntry(
    val questionId: String,
    val questionText: String,
    val sourceSlotId: String?,
    val stage: String,
    val answeredAtMillis: Long?,
    val slotUpdates: List<JsonElement>,
) {
    val id: String get() = questionId

    /** `waiting_user` → `Waiting User` (web `stageLabel`). */
    val stageLabel: String
        get() {
            val words = stage.split('_', ' ').filter { it.isNotEmpty() }
            if (words.isEmpty()) return "Unknown"
            return words.joinToString(" ") { it.take(1).uppercase() + it.drop(1).lowercase() }
        }
}

/** Why a manual resume form is not sent. */
sealed class PlanManualResumeInvalid(message: String) : Exception(message) {
    data object Confidence : PlanManualResumeInvalid("Updated confidence must be a number between 0 and 1.")
    data object SlotsNotArray : PlanManualResumeInvalid("Manual resume slots must be a JSON array.")
    data class SlotMissingFields(val index: Int) :
        PlanManualResumeInvalid("Manual resume slot $index must include slot_id and slot_type.")
    data object SlotsNotJson : PlanManualResumeInvalid("The slots are not valid JSON.")
}

/** The Advanced → manual resume form, validated (web `handleManualResume`). */
data class PlanManualResume(
    val handledQuestion: String = "",
    val updatedConfidence: String = "",
    val slotsJson: String = "",
) {
    /** The POST body; throws the first [PlanManualResumeInvalid]. */
    fun payload(): JsonObject {
        val body = LinkedHashMap<String, JsonElement>()
        val question = handledQuestion.trim()
        if (question.isNotEmpty()) body["handled_question"] = JsonPrimitive(question)
        val confidenceText = updatedConfidence.trim()
        if (confidenceText.isNotEmpty()) {
            val value = confidenceText.toDoubleOrNull()
            if (value == null || !value.isFinite() || value !in 0.0..1.0) throw PlanManualResumeInvalid.Confidence
            body["updated_confidence"] = JsonPrimitive(value)
        }
        val slotsText = slotsJson.trim()
        if (slotsText.isNotEmpty()) {
            val parsed = PlanJson.parse(slotsText) ?: throw PlanManualResumeInvalid.SlotsNotJson
            val items = parsed as? JsonArray ?: throw PlanManualResumeInvalid.SlotsNotArray
            val slots = items.mapIndexed { index, item ->
                val o = item.jObj()
                val slotId = o?.get("slot_id").jStr()?.trim() ?: ""
                val slotType = o?.get("slot_type").jStr()?.trim() ?: ""
                if (slotId.isEmpty() || slotType.isEmpty()) throw PlanManualResumeInvalid.SlotMissingFields(index + 1)
                val slot = linkedMapOf<String, JsonElement>(
                    "slot_id" to JsonPrimitive(slotId),
                    "slot_type" to JsonPrimitive(slotType),
                    "value" to (o?.get("value") ?: JsonNull),
                )
                o?.get("confidence").jNum()?.takeIf { it.isFinite() }?.let { slot["confidence"] = JsonPrimitive(it) }
                JsonObject(slot)
            }
            if (slots.isNotEmpty()) body["slots"] = JsonArray(slots)
        }
        return JsonObject(body)
    }
}

// Transport

/** One request: method, path from `/api/…`, query pairs, optional JSON body. */
data class PlanInspectorRequest(
    val method: String,
    val path: String,
    val query: List<Pair<String, String>> = emptyList(),
    val body: JsonElement? = null,
)

data class PlanInspectorResponse(val status: Int, val body: JsonElement?)

/** Sends a request: the app's authorised HTTP client, or a stub in tests. */
interface PlanInspectorTransport {
    suspend fun send(request: PlanInspectorRequest): PlanInspectorResponse
}

/** A non-2xx answer, carrying the server's message. */
class PlanInspectorException(val status: Int, message: String) : Exception(message)

// Client

class PlanInspectorClient(
    val taskId: String,
    private val transport: PlanInspectorTransport,
    val channel: String = "android",
) {
    val planPath: String get() = "/api/magician/v3/tasks/${encodeSegment(taskId)}/plan"

    // Request building (pure)

    fun loadPlanRequest() = PlanInspectorRequest("GET", planPath)

    fun savePlanRequest(graph: PlanGraph, label: String?): PlanInspectorRequest {
        val trimmed = label?.trim() ?: ""
        return PlanInspectorRequest("PUT", planPath, body = JsonObject(linkedMapOf(
            "plan" to graph.savePayload(),
            "label" to if (trimmed.isEmpty()) JsonNull else JsonPrimitive(trimmed),
        )))
    }

    fun versionsRequest(limit: Int = 20) =
        PlanInspectorRequest("GET", "$planPath/versions", query = listOf("limit" to limit.toString()))

    fun versionRequest(epochMs: Long) = PlanInspectorRequest("GET", "$planPath/versions/$epochMs")

    fun restoreRequest(epochMs: Long) = PlanInspectorRequest("POST", "$planPath/versions/$epochMs/restore")

    fun artifactRequest(name: String) = PlanInspectorRequest("GET", "$planPath/$name")

    /** Throws [PlanManualResumeInvalid] when the form does not validate. */
    fun resumeRequest(form: PlanManualResume) =
        PlanInspectorRequest("POST", "$planPath/clarifications/resume", body = form.payload())

    fun answerRequest(questionId: String, text: String) = PlanInspectorRequest(
        "POST", "/api/magician/v2/hitl/${encodeSegment(questionId)}/respond",
        body = JsonObject(linkedMapOf(
            "source" to JsonPrimitive("clarification"),
            "value" to JsonObject(linkedMapOf("type" to JsonPrimitive("text"), "value" to JsonPrimitive(text))),
            "channel" to JsonPrimitive(channel),
            "task_id" to JsonPrimitive(taskId),
        )),
    )

    fun decisionRequest(action: String, planId: String) =
        PlanInspectorRequest("POST", "$planPath/$action", query = listOf("plan_id" to planId))

    fun replanRequest() = PlanInspectorRequest("POST", "$planPath/replan")

    fun executeRequest() = PlanInspectorRequest(
        "POST", "/api/magician/v3/tasks/${encodeSegment(taskId)}/execute", body = JsonObject(emptyMap()),
    )

    fun executionPanelRequest() =
        PlanInspectorRequest("GET", "/api/magician/v3/tasks/${encodeSegment(taskId)}/execution-panel")

    // Calls

    /** The task's plan; null when the task has none (404). */
    suspend fun loadPlan(): PlanRecord? {
        val response = transport.send(loadPlanRequest())
        if (response.status == 404) return null
        return PlanRecord.parse(checked(response)["plan"])
    }

    suspend fun savePlan(graph: PlanGraph, label: String?): PlanRecord =
        PlanRecord.parse(checked(transport.send(savePlanRequest(graph, label)))["plan"])

    suspend fun versions(limit: Int = 20): PlanVersionsPage {
        val body = checked(transport.send(versionsRequest(limit)))
        val versions = body["versions"].jArr().orEmpty().mapNotNull { value ->
            val v = value.jObj() ?: return@mapNotNull null
            val epoch = v["epoch_ms"].jNum() ?: return@mapNotNull null
            PlanVersionSummary(epoch.toLong(), v["label"].jStr(), (v["step_count"].jNum() ?: 0.0).toInt(), v["confidence"].jNum())
        }
        return PlanVersionsPage(versions, body["has_more"].jBool() == true)
    }

    suspend fun version(epochMs: Long): PlanGraph? =
        PlanGraph.decode(checked(transport.send(versionRequest(epochMs)))["version"].jObj()?.get("plan_graph"))

    suspend fun restore(epochMs: Long): PlanRecord =
        PlanRecord.parse(checked(transport.send(restoreRequest(epochMs)))["plan"])

    suspend fun analysis(): JsonElement? {
        val value = checked(transport.send(artifactRequest("analysis")))["query_analysis"]
        return if (value == null || value is JsonNull) null else value
    }

    suspend fun slots(): PlanSlotSnapshot? {
        val value = checked(transport.send(artifactRequest("slots")))["slot_graph_snapshot"]
        if (value.jObj() == null) return null
        return PlanSlotSnapshot.parse(value!!)
    }

    suspend fun attempts(): List<PlanStrategyAttempt> {
        val items = checked(transport.send(artifactRequest("attempts")))["strategy_attempts"].jArr().orEmpty()
        return items.mapIndexed { index, value ->
            val a = value.jObj() ?: JsonObject(emptyMap())
            PlanStrategyAttempt(
                index = index,
                strategyType = a["strategy_type"].jStr(),
                attemptNumber = a["attempt_number"].jNum()?.toInt(),
                failureReason = a["failure_reason"].jStr(),
                attemptedAtMillis = a["attempted_at"].jNum()?.toLong(),
                explorationResult = a["exploration_result"]?.takeIf { it !is JsonNull },
                resourcesUsed = a["resources_used"]?.takeIf { it !is JsonNull },
            )
        }
    }

    suspend fun clarifications(): List<PlanClarificationEntry> {
        val items = checked(transport.send(artifactRequest("clarifications")))["clarification_history"].jArr().orEmpty()
        return items.mapNotNull { value ->
            val c = value.jObj() ?: return@mapNotNull null
            val id = c["question_id"].jStr() ?: return@mapNotNull null
            PlanClarificationEntry(
                questionId = id,
                questionText = c["question_text"].jStr() ?: "",
                sourceSlotId = c["source_slot_id"].jStr(),
                stage = c["stage"].jStr() ?: "",
                answeredAtMillis = c["answered_at"].jNum()?.toLong(),
                slotUpdates = c["slot_updates"].jArr().orEmpty().toList(),
            )
        }
    }

    /** Returns whether the planning workflow resumed. */
    suspend fun resume(form: PlanManualResume): Boolean =
        checked(transport.send(resumeRequest(form)))["workflow_resumed"].jBool() == true

    /** Answer a pending planning question. 409 means already answered, which counts as done. */
    suspend fun answer(questionId: String, text: String) {
        val response = transport.send(answerRequest(questionId, text))
        if (response.status == 409) return
        checked(response)
    }

    suspend fun approve(planId: String) { checked(transport.send(decisionRequest("approve", planId))) }
    suspend fun reject(planId: String) { checked(transport.send(decisionRequest("reject", planId))) }
    suspend fun replan() { checked(transport.send(replanRequest())) }
    suspend fun startRun() { checked(transport.send(executeRequest())) }

    /** The `/execution-panel` payload, for [PlanSteps]. */
    suspend fun executionPanel(): JsonObject = checked(transport.send(executionPanelRequest()))

    companion object {
        /** The body of a 2xx (an empty object when there is none), else a [PlanInspectorException]. */
        fun checked(response: PlanInspectorResponse): JsonObject {
            if (response.status !in 200..299) {
                val body = response.body.jObj()
                val message = body?.get("message").jStr() ?: body?.get("error").jStr()
                    ?: "The request failed (HTTP ${response.status})."
                throw PlanInspectorException(response.status, message)
            }
            return response.body.jObj() ?: JsonObject(emptyMap())
        }

        /** Percent-encode a path segment: unreserved characters (`A–Z a–z 0–9 - . _ ~`) stay. */
        fun encodeSegment(value: String): String {
            val out = StringBuilder()
            for (byte in value.toByteArray(Charsets.UTF_8)) {
                val c = byte.toInt() and 0xff
                val ch = c.toChar()
                if (c < 0x80 && (ch.isLetterOrDigit() || ch in "-._~")) out.append(ch)
                else out.append('%').append("%02X".format(c))
            }
            return out.toString()
        }
    }
}
