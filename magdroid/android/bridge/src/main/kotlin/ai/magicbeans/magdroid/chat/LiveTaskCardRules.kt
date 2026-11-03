package ai.magicbeans.magdroid.chat

import ai.magicbeans.magdroid.tasks.PlanStepMark
import ai.magicbeans.magdroid.tasks.PlanStepRow
import ai.magicbeans.magdroid.tasks.PlanSteps
import ai.magicbeans.magdroid.tasks.PlanningEntry
import ai.magicbeans.magdroid.tasks.PlanningPhase
import ai.magicbeans.magdroid.tasks.PlanningTrack
import ai.magicbeans.magdroid.tasks.TaskOutputClassification
import ai.magicbeans.magdroid.tasks.TaskRunEvidence
import ai.magicbeans.magdroid.tasks.jObj
import ai.magicbeans.magdroid.tasks.jObjects
import ai.magicbeans.magdroid.tasks.jText
import kotlinx.serialization.json.JsonObject

/*
 * Pure rules the chat's live task card view reads on top of
 * [LiveTaskCardState]: which phase the card is in, its one line, the planning
 * track's segments, the running step bar, the plan preview payload, when to
 * poll, what a file-name tap copies, where an output is served from and the
 * card's action cluster. No Compose, no networking, no clock.
 *
 * Android port of iOS `LiveTaskCardSupport.swift`; design §2 of
 * `docs/archive/plans/2026-10-01-mobile-lively-chat-and-task-detail-design.md`.
 */

/** The card's body, one at a time, in the order a task lives them. */
enum class LiveTaskCardPhase {
    /** Working out a plan: the four-segment phase track. */
    Planning,

    /** A drafted plan waits for review: Review · Approve & Run. */
    PlanReady,

    /** No run yet (`ready`, `pending`, `rejected`): the quiet tag, Run when approved. */
    NotStarted,

    /** A run in progress (or paused / waiting on the reader). */
    Running,

    /** Completed, partial, failed or cancelled. */
    Finished,
}

/** The status dot's tone (iOS `StatusDot.Tone`); the view maps it to theme colours. */
enum class LiveTaskDotTone { NotStarted, Running, Waiting, Done, Partial, Failed, Cancelled }

/** Where a chat output file is served from. */
sealed interface LiveTaskOutputHome {
    data class Task(val taskId: String) : LiveTaskOutputHome
    data class Session(val sessionId: String) : LiveTaskOutputHome
}

/**
 * One task action on the card. Every action sits in one cluster beside the
 * title (or, when that row is too narrow, wrapped directly under it) — never
 * split across the card.
 */
enum class LiveTaskCardAction {
    /** The task detail entry. Always offered, always first. */
    Inspect,

    /** Start a task that has not started and is startable. */
    Run,

    /** Approve a drafted plan and start the run (the plan-ready primary). */
    ApproveAndRun,

    /** Open a drafted plan for review. */
    Review,

    /** Open the plan graph of a task whose plan the card already knows. */
    PlanGraph,

    /** Stop a live task. Always last, beside the expand chevron. */
    Cancel,
}

object LiveTaskCardRules {
    /** The effective raw status: the message's, unless the store knows better. */
    fun effectiveStatus(input: LiveTaskCardInput): String? =
        LiveTaskCardState.effectiveStatus(input.status, input.storeStatus)

    fun phase(state: LiveTaskCardState, effectiveStatus: String?): LiveTaskCardPhase = when {
        state.terminal -> LiveTaskCardPhase.Finished
        state.notStarted -> LiveTaskCardPhase.NotStarted
        effectiveStatus == "draft" -> LiveTaskCardPhase.PlanReady
        effectiveStatus == "planning" || effectiveStatus == "created" -> LiveTaskCardPhase.Planning
        else -> LiveTaskCardPhase.Running
    }

    /**
     * `/execution-panel` is polled every 4 s only while the card is on
     * screen, the app is in the foreground, and the task is planning or
     * running.
     */
    const val POLL_INTERVAL_MILLIS: Long = 4_000

    fun shouldPoll(phase: LiveTaskCardPhase, hasTaskId: Boolean, visible: Boolean, appActive: Boolean): Boolean {
        if (!hasTaskId || !visible || !appActive) return false
        return phase == LiveTaskCardPhase.Planning || phase == LiveTaskCardPhase.Running
    }

    /** The usage snapshot's phase bucket (web `usagePhase`). */
    fun usagePhase(state: LiveTaskCardState): String = when {
        state.terminal -> "done"
        state.notStarted || state.status == LiveTaskStatus.Waiting -> "planned"
        else -> "running"
    }

    /**
     * The one line under the header. Planning: the latest planning milestone.
     * Running a planned task: `Step 3 of 6 · <step>`. Otherwise the reason,
     * else the latest activity line (never the bare run label).
     */
    fun line(
        phase: LiveTaskCardPhase,
        state: LiveTaskCardState,
        planningEntries: List<PlanningEntry>,
        stepRows: List<PlanStepRow>,
        latestActivity: String?,
    ): String? {
        when (phase) {
            LiveTaskCardPhase.Planning ->
                planningEntries.lastOrNull { !it.isModelCall && it.message.isNotEmpty() }?.let { return it.message }
            LiveTaskCardPhase.Running -> if (stepRows.isNotEmpty()) return PlanSteps.summary(stepRows)
            LiveTaskCardPhase.PlanReady, LiveTaskCardPhase.NotStarted, LiveTaskCardPhase.Finished -> Unit
        }
        return firstLine(state.reason) ?: if (state.terminal) null else firstLine(latestActivity)
    }

    /** The planning line reads bold on a retry (a retry is not a failure) or a failure. */
    fun lineIsEmphasised(phase: LiveTaskCardPhase, track: PlanningTrack): Boolean =
        phase == LiveTaskCardPhase.Planning && (track.retrying || track.failed)

    /** Each of the four planning segments: finished, current or ahead. */
    enum class SegmentState { Finished, Current, Ahead }

    fun planningSegments(track: PlanningTrack): List<SegmentState> = PlanningPhase.entries.map { phase ->
        val current = track.phase
        when {
            // Nothing reached yet: the first segment is what is happening.
            current == null -> if (phase == PlanningPhase.Understand) SegmentState.Current else SegmentState.Ahead
            phase.ordinal < current.ordinal -> SegmentState.Finished
            phase == current -> SegmentState.Current
            else -> SegmentState.Ahead
        }
    }

    /** What a tap on a deliverable's name copies: its host path, else the path the task wrote it at. */
    fun copyPath(absolutePath: String?, relativePath: String?): String? {
        TaskOutputClassification.absolutePath(absolutePath)?.let { return it }
        return relativePath?.trim()?.takeIf { it.isNotEmpty() }
    }

    /**
     * Where a chat output file is served from. The chat copies a task's
     * deliverables into its session (`source.type` `session_output` /
     * `chat_output`), so only a block that says `task_output` is read from
     * the task; a session copy asked of the task endpoint is "Task output not
     * found". Without a session, a sourceless block falls back to the task.
     */
    fun outputHome(
        sourceType: String?,
        sourceTaskId: String?,
        cardTaskId: String?,
        sessionId: String?,
    ): LiveTaskOutputHome? {
        if (sourceType == "task_output") (sourceTaskId ?: cardTaskId)?.let { return LiveTaskOutputHome.Task(it) }
        if (!sessionId.isNullOrEmpty()) return LiveTaskOutputHome.Session(sessionId)
        if (sourceType == null && cardTaskId != null) return LiveTaskOutputHome.Task(cardTaskId)
        return null
    }

    /**
     * The card's one run (the chat holds one status message per task): its
     * status, ids, first and latest update times and output count.
     */
    fun runs(task: TaskStatusCard, messageId: String): List<LiveTaskRunInput> = listOf(
        LiveTaskRunInput(
            id = task.executionId ?: messageId,
            status = task.status,
            executionId = task.executionId,
            sessionId = task.uiThreadId,
            timestamps = listOfNotNull(task.startedAtMs, task.updatedAtMs).map { it.toDouble() },
            outputCount = task.outputs.size,
        ),
    )

    /** The card's input from the chat's status card. */
    fun input(task: TaskStatusCard, messageId: String): LiveTaskCardInput = LiveTaskCardInput.from(
        task = task,
        messageId = messageId,
        storeStatus = task.storeStatus,
        createdAt = task.startedAtMs?.toDouble(),
        completionKind = task.completionKind,
        runs = runs(task, messageId),
    )

    fun dotTone(status: LiveTaskStatus): LiveTaskDotTone = when (status) {
        LiveTaskStatus.Queued -> LiveTaskDotTone.NotStarted
        LiveTaskStatus.Running -> LiveTaskDotTone.Running
        LiveTaskStatus.Waiting -> LiveTaskDotTone.Waiting
        LiveTaskStatus.Completed -> LiveTaskDotTone.Done
        LiveTaskStatus.Partial -> LiveTaskDotTone.Partial
        LiveTaskStatus.Failed -> LiveTaskDotTone.Failed
        LiveTaskStatus.Cancelled -> LiveTaskDotTone.Cancelled
    }

    /** A long reason is clamped while collapsed; expanding shows it whole. */
    fun reasonOverflows(reason: String?): Boolean {
        reason ?: return false
        return reason.length > 140 || reason.contains("\n")
    }

    /**
     * The card's actions in display (and TalkBack) order: Inspect first, then
     * the state's primary action, then its secondary ones, Cancel last.
     *
     * - plan ready: Approve & Run, Review (Review already opens the plan, so
     *   Plan graph is not offered twice)
     * - not started and startable: Run
     * - a known plan outside plan review: Plan graph
     * - a live task: Cancel
     */
    fun actions(
        phase: LiveTaskCardPhase,
        hasTaskId: Boolean,
        startable: Boolean,
        canCancel: Boolean,
        showsPlan: Boolean,
    ): List<LiveTaskCardAction> {
        val actions = mutableListOf(LiveTaskCardAction.Inspect)
        if (!hasTaskId) return actions
        when {
            phase == LiveTaskCardPhase.PlanReady ->
                actions += listOf(LiveTaskCardAction.ApproveAndRun, LiveTaskCardAction.Review)
            phase == LiveTaskCardPhase.NotStarted && startable -> actions += LiveTaskCardAction.Run
        }
        if (showsPlan && phase != LiveTaskCardPhase.PlanReady) actions += LiveTaskCardAction.PlanGraph
        if (canCancel) actions += LiveTaskCardAction.Cancel
        return actions
    }

    /**
     * A chat task card brought in line with the task's stored status. The
     * chat posts no message for every transition (approving a plan posts
     * none), so a card's latest message can say `draft` long after the task
     * went `ready`.
     *
     * - A not-started store status behind a `planning` / `draft` message is
     *   recorded as `storeStatus` only: the card's own store-wins rule shows
     *   it and drops the message's summary.
     * - Any other store status that differs replaces the message's, and the
     *   message's summary (a state the task left) is dropped.
     */
    fun reconciled(task: TaskStatusCard, storeStatus: String): TaskStatusCard {
        val model = task.copy(storeStatus = storeStatus)
        val message = task.status.lowercase()
        val store = storeStatus.lowercase()
        val behind = message.isEmpty() || message == "planning" || message == "draft"
        if (behind && LiveTaskCardState.isNotStarted(store)) return model
        if (store != message) return model.copy(status = storeStatus, summary = null)
        return model
    }

    private fun firstLine(text: String?): String? =
        text?.lines()?.map { it.trim() }?.firstOrNull { it.isNotEmpty() }
}

/**
 * What `GET /api/magician/v3/tasks/{id}` says about a task. The record is
 * `{task: {manifest: {...}, state: {status, active_root_execution_id, ...},
 * refs: {...}}}`; an older flat `{task: {status, ...}}` still reads. The
 * status is `task.state.status`, never a top-level `task.status` alone.
 */
data class TaskStoreRecord(
    val status: String,
    val activeRootExecutionId: String?,
    val chatSessionId: String?,
    /** `manifest.created_at` (epoch ms), for the card's "Created" time. */
    val createdAtMs: Long? = null,
    /**
     * The task's own updated time (epoch ms): `state.last_progress_at` (the
     * run last advanced) when present, else `updated_at`. Never the later of
     * the two: `updated_at` moves on every heartbeat and metadata write.
     */
    val updatedAtMs: Long? = null,
) {
    companion object {
        fun parse(record: JsonObject): TaskStoreRecord? {
            val task = record["task"].jObj() ?: return null
            val state = task["state"].jObj()
            val manifest = task["manifest"].jObj()
            val status = task["status"].jText() ?: state?.get("status").jText() ?: return null
            return TaskStoreRecord(
                status = status,
                activeRootExecutionId = task["active_root_execution_id"].jText()
                    ?: state?.get("active_root_execution_id").jText(),
                chatSessionId = task["chat_session_id"].jText()
                    ?: manifest?.get("chat_session_id").jText()
                    ?: state?.get("chat_session_id").jText(),
                createdAtMs = TaskRunEvidence.instant(task["created_at"] ?: manifest?.get("created_at")),
                updatedAtMs = TaskRunEvidence.instant(task["last_progress_at"] ?: state?.get("last_progress_at"))
                    ?: TaskRunEvidence.instant(state?.get("updated_at") ?: task["updated_at"]),
            )
        }
    }
}

/** The running step bar. */
object LiveTaskStepBar {
    /** Beyond this many plan steps the bar is one continuous fill. */
    const val MAX_SEGMENTS = 12

    sealed interface Layout {
        /** One segment per plan step, in plan order. */
        data class Segments(val marks: List<PlanStepMark>) : Layout

        /** Share of steps settled (done or skipped), 0…1. */
        data class Continuous(val fraction: Double) : Layout
    }

    fun layout(rows: List<PlanStepRow>): Layout? {
        if (rows.isEmpty()) return null
        if (rows.size <= MAX_SEGMENTS) return Layout.Segments(rows.map { it.mark })
        val settled = rows.count { it.mark == PlanStepMark.Done || it.mark == PlanStepMark.Skipped }
        return Layout.Continuous(settled.toDouble() / rows.size)
    }
}

/**
 * What the card reads from `GET /v3/tasks/{id}/plan` (web
 * `applyTaskPlanPayload` + `extractPlanStepsFromPlanGraph`): the plan id (the
 * plan-approval ask's correlation id), its status and its steps' names.
 */
data class LiveTaskPlan(val planId: String?, val status: String?, val steps: List<String>) {
    companion object {
        /** `{plan: {...}}` or the plan object itself. */
        fun parse(payload: JsonObject): LiveTaskPlan? {
            val plan = payload["plan"].jObj() ?: payload
            val rawSteps = plan["plan_graph"].jObj()?.get("steps").jObjects().orEmpty()
            val steps = rawSteps.map { step ->
                step["task"].jText()
                    ?: step["metadata"].jObj()?.get("description").jText()
                    ?: "Step ${step["id"].jText() ?: ""}".trim()
            }
            val planId = plan["plan_id"].jText()
            val status = plan["status"].jText()
            if (planId == null && status == null && steps.isEmpty()) return null
            return LiveTaskPlan(planId, status, steps)
        }
    }
}
