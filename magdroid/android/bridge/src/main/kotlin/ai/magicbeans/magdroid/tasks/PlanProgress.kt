package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.JsonObject
import java.time.Instant

/*
 * Planning phases and plan-step marks, read from the `/execution-panel`
 * payload. Pure — no Compose, no networking.
 *
 * Android port of iOS `PlanProgressModel.swift`. Sources: web
 * `planProgressFrom` (`taskPanelModel.ts`), backend
 * `progress_channel_seam/normalize.rs::planning_milestone` (stage wording),
 * `execution_panel/v3_adapter.rs` (`plan_step_statuses_from`). The stage →
 * phase table is design §4 of
 * `docs/archive/plans/2026-10-01-mobile-lively-chat-and-task-detail-design.md`.
 */

/** The four-segment track a planning card shows. */
enum class PlanningPhase(val title: String) {
    Understand("Understand"),
    Outline("Outline"),
    Steps("Steps"),
    Check("Check"),
}

/** One planning milestone or planning model call, oldest first. */
data class PlanningEntry(
    val id: String,
    val atMillis: Long,
    val message: String,
    /** Warning or error severity: shown bold (a retry is not a failure). */
    val warning: Boolean,
    /** `query_analysis` … `plan_ready`, `plan_retry`, `failed`, `llm`, when readable. */
    val stage: String?,
    /** A planning LLM call: a compact secondary row, never moves the track. */
    val isModelCall: Boolean,
    /** A model call's tokens and price, when the row carries them. */
    val tokens: Int? = null,
    val costUsd: Double? = null,
    /**
     * How many model calls the row stands for: a backfilled planning-usage
     * summary row carries `llm_calls`; an ordinary model-call row is one.
     */
    val llmCalls: Int? = null,
)

/** Where the planning track stands. */
data class PlanningTrack(
    /** The latest phase reached; null before any phase-moving milestone. */
    val phase: PlanningPhase?,
    /** The latest move was a retry (back to Steps). */
    val retrying: Boolean,
    /** Planning hit a problem after the latest move; the phase is kept. */
    val failed: Boolean,
)

object PlanProgress {
    /**
     * `debug.planning_timeline` when the server sends it (even empty), else
     * `debug.timeline` rows titled "Progress" (an older server); either way
     * only `planexec_` rows, oldest first. Web `planProgressFrom`.
     */
    fun entries(panel: JsonObject): List<PlanningEntry> {
        val debug = panel["debug"].jObj()
        val dedicated = debug?.get("planning_timeline").jObjects()
        val source = dedicated ?: debug?.get("timeline").jObjects().orEmpty()
        return source
            .filter { row ->
                (dedicated != null || row["title"].jStr() == "Progress") &&
                    (row["execution_id"].jStr() ?: "").startsWith("planexec_")
            }
            .mapNotNull { row ->
                val id = row["id"].jStr() ?: return@mapNotNull null
                val at = instantMillis(row) ?: return@mapNotNull null
                val severity = row["severity"].jStr()?.lowercase()
                val stage = stage(row)
                PlanningEntry(
                    id = id,
                    atMillis = at,
                    message = row["message"].jStr() ?: "",
                    warning = severity == "warning" || severity == "error",
                    stage = stage,
                    isModelCall = stage == "llm",
                    tokens = row["tokens"].jNum()?.takeIf { it.isFinite() }?.toInt(),
                    costUsd = row["cost_usd"].jNum()?.takeIf { it.isFinite() && it >= 0 },
                    llmCalls = row["llm_calls"].jNum()?.takeIf { it.isFinite() && it >= 0 }?.toInt(),
                )
            }
            // A stable sort: equal times keep their payload order.
            .sortedBy { it.atMillis }
    }

    /**
     * The entry's planning stage: the `planning_stage` field; else the stage
     * segment of a `planning:{exec}:{stage}:{ts}` id; else, last, the
     * backend's fixed milestone wording. Model calls are recognised by the
     * ` · ` separator only they use.
     */
    fun stage(entry: JsonObject): String? {
        entry["planning_stage"].jStr()?.trim()?.takeIf { it.isNotEmpty() }?.let { return it }
        entry["id"].jStr()?.let { id ->
            val parts = id.split(":")
            if (parts.size >= 4 && parts[0] == "planning" && parts[parts.size - 2].isNotEmpty()) {
                return parts[parts.size - 2]
            }
        }
        return stageFromMessage(entry["message"].jStr() ?: "")
    }

    private val milestoneWording = listOf(
        "Understood the request" to "query_analysis",
        "Picking out the details the task needs" to "slot_extraction",
        "Restated the task" to "clarified_task",
        "Outlining the plan" to "outline",
        "Outlined " to "outline_done",
        "Working out the steps for" to "expansion",
        "Checking the drafted plan" to "validation",
        "Plan drafted and checked" to "plan_ready",
        "The draft did not pass its checks" to "plan_retry",
        "Planning hit a problem" to "failed",
    )

    private fun stageFromMessage(message: String): String? {
        val text = message.trim()
        if (text.contains(" · ")) return "llm"
        return milestoneWording.firstOrNull { text.startsWith(it.first) }?.second
    }

    /** Design §4. `llm` and `failed` never move the track (null). */
    fun phase(stage: String): PlanningPhase? = when (stage) {
        "query_analysis", "slot_extraction", "clarified_task" -> PlanningPhase.Understand
        "outline", "outline_done" -> PlanningPhase.Outline
        "expansion", "plan_retry" -> PlanningPhase.Steps
        "validation", "plan_ready" -> PlanningPhase.Check
        else -> null
    }

    /**
     * The latest phase reached. A `plan_retry` steps back to Steps and marks
     * the retry; `failed` keeps the phase and marks it; a later move clears
     * both (a durable retry resumed planning).
     */
    fun currentPhase(entries: List<PlanningEntry>): PlanningTrack {
        var phase: PlanningPhase? = null
        var retrying = false
        var failed = false
        for (entry in entries) {
            val stage = entry.stage ?: continue
            if (entry.isModelCall) continue
            if (stage == "failed") {
                failed = true
                continue
            }
            val moved = phase(stage) ?: continue
            phase = moved
            retrying = stage == "plan_retry"
            failed = false
        }
        return PlanningTrack(phase, retrying, failed)
    }

    /** The folded line after planning: `N events · duration` (first to last). */
    fun activitySummary(entries: List<PlanningEntry>): String {
        val first = entries.firstOrNull() ?: return ""
        val last = entries.last()
        val count = "${entries.size} ${if (entries.size == 1) "event" else "events"}"
        if (entries.size <= 1) return count
        val span = TaskVerdict.durationIfKnown((last.atMillis - first.atMillis) / 1_000.0) ?: return count
        return "$count · $span"
    }

    /** Epoch milliseconds (the wire's `i64`), or ISO-8601. */
    private fun instantMillis(row: JsonObject): Long? {
        val value = row["timestamp"]
        value.jStr()?.let { text ->
            if (text.isEmpty()) return null
            return runCatching { Instant.parse(text).toEpochMilli() }.getOrNull()
        }
        val number = value.jNum()?.takeIf { it.isFinite() } ?: return null
        return number.toLong()
    }
}

/** A plan step's mark in the run checklist and the step bar. */
enum class PlanStepMark {
    Done, Failed, InProgress, Pending, Skipped,

    /** Still pending when the run ended. */
    NotReached,
}

data class PlanStepRow(
    val number: Int,
    val name: String,
    val mark: PlanStepMark,
    /** The agent's reason (a skipped, failed or done-another-way step), on any mark. */
    val note: String?,
    val stepId: String? = null,
    /** Activity rows recorded while the run was on this step (`activity_count`); null when not attributed. */
    val activityCount: Int? = null,
) {
    val id: String get() = stepId ?: "step-$number"
}

object PlanSteps {
    /**
     * `debug.selected_execution.step_statuses` as rows in plan order (the
     * order the payload lists them; never re-sorted by completion).
     */
    fun rows(panel: JsonObject, runEnded: Boolean): List<PlanStepRow> {
        val selected = panel["debug"].jObj()?.get("selected_execution").jObj()
        return rows(selected?.get("step_statuses").jObjects().orEmpty(), runEnded)
    }

    fun rows(statuses: List<JsonObject>, runEnded: Boolean): List<PlanStepRow> =
        statuses.mapIndexed { offset, item ->
            val number = item["number"].jInt() ?: (offset + 1)
            PlanStepRow(
                number = number,
                name = item["name"].jText() ?: "Step $number",
                mark = mark(item["status"].jStr(), runEnded),
                note = item["note"].jText(),
                stepId = item["step_id"].jText(),
                activityCount = item["activity_count"].jInt(),
            )
        }

    /**
     * `completed | failed | in_progress | pending | skipped`. An unknown or
     * cancelled status reads as pending; pending reads `NotReached` once the
     * run ended.
     */
    fun mark(status: String?, runEnded: Boolean): PlanStepMark = when (status?.trim()?.lowercase()) {
        "completed", "done" -> PlanStepMark.Done
        "failed" -> PlanStepMark.Failed
        "in_progress", "running" -> PlanStepMark.InProgress
        "skipped" -> PlanStepMark.Skipped
        else -> if (runEnded) PlanStepMark.NotReached else PlanStepMark.Pending
    }

    /**
     * `Step 3 of 6 · <name>` while a step runs; `Step 2 of 3 failed · <name>`
     * after a failure; else `4 of 6 done` (`· 1 skipped` when any were).
     */
    fun summary(rows: List<PlanStepRow>): String {
        if (rows.isEmpty()) return ""
        val total = rows.size
        rows.indexOfFirst { it.mark == PlanStepMark.InProgress }.takeIf { it >= 0 }?.let {
            return "Step ${it + 1} of $total · ${rows[it].name}"
        }
        rows.indexOfFirst { it.mark == PlanStepMark.Failed }.takeIf { it >= 0 }?.let {
            return "Step ${it + 1} of $total failed · ${rows[it].name}"
        }
        val done = rows.count { it.mark == PlanStepMark.Done }
        val skipped = rows.count { it.mark == PlanStepMark.Skipped }
        return if (skipped > 0) "$done of $total done · $skipped skipped" else "$done of $total done"
    }
}
