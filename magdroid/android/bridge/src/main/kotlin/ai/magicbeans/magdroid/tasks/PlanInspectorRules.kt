package ai.magicbeans.magdroid.tasks

import kotlinx.coroutines.CancellationException
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import java.time.Instant

/*
 * The Plan Inspector's rules and state machine — Android port of iOS
 * `PlanInspectorModel.swift` (itself the port of the web
 * `ExecutionPlanInspector.svelte` state: stale responses dropped by request
 * generation; edit, save, history, restore, clarifications, manual resume,
 * approval, live run marks). No Compose, no ViewModel, no scope of its own:
 * the screen's ViewModel owns one [PlanInspectorState], calls its suspend
 * functions from its scope and polls [PlanInspectorState.refreshLiveMarks]
 * every [PlanInspectorRules.LIVE_POLL_SECONDS] while [PlanInspectorState.runActive]
 * and the screen is visible.
 */

/** Opens the Plan Inspector for a task. `tab` picks the first view. */
data class PlanInspectorRoute(
    val taskId: String,
    val title: String? = null,
    val tab: PlanInspectorTab = PlanInspectorTab.Graph,
    /** The plan's markdown document, when the caller has it (a More view). */
    val planDocument: String? = null,
) {
    val id: String get() = taskId
}

enum class PlanInspectorTab(val title: String) {
    Graph("Graph"), Steps("Steps"), History("History"),
}

/** The More menu's panes. */
enum class PlanInspectorPane(val title: String) {
    Analysis("Analysis"), Slots("Slots"), Attempts("Attempts"), Clarifications("Clarifications"),
}

/** A lazily loaded pane's state. */
sealed class PlanLoadable<out T> {
    data object Idle : PlanLoadable<Nothing>()
    data object Loading : PlanLoadable<Nothing>()
    data class Loaded<T>(val data: T) : PlanLoadable<T>()
    data class Failed(val message: String) : PlanLoadable<Nothing>()

    val value: T? get() = (this as? Loaded<T>)?.data
}

/** Draft plan decisions on the approval bar. */
enum class PlanDecision { Reject, Approve, ApproveAndRun }

object PlanInspectorRules {
    /** Seconds between live polls while a run is active. */
    const val LIVE_POLL_SECONDS = 4

    fun canEdit(status: PlanStatus?, hasGraph: Boolean): Boolean = hasGraph && (status?.canEdit ?: false)

    /** The unsaved-changes guard: editing, with a draft that differs from where editing began. */
    fun isDirty(editing: Boolean, draft: PlanGraph?, baseline: PlanGraph?): Boolean =
        editing && draft != null && draft != baseline

    fun showsApprovalBar(status: PlanStatus?, planId: String?, editing: Boolean): Boolean =
        status == PlanStatus.Draft && !planId.isNullOrEmpty() && !editing

    /** The manual resume form is for a planner still waiting on inputs. */
    fun canResume(status: PlanStatus?): Boolean = status == PlanStatus.Planning || status == PlanStatus.Eliciting

    /** Replan restarts planning for a plan that is not approved or running. */
    fun canReplan(status: PlanStatus?): Boolean =
        status == PlanStatus.Draft || status == PlanStatus.Rejected || status == PlanStatus.Failed

    /** Map run rows onto plan steps: by step id, else by position. */
    fun marks(rows: List<PlanStepRow>, graph: PlanGraph): Map<String, PlanStepMark> {
        val marks = LinkedHashMap<String, PlanStepMark>()
        val ids = graph.steps.map { it.id }.toSet()
        for (row in rows) {
            val stepId = row.stepId
            if (stepId != null && stepId in ids) {
                marks[stepId] = row.mark
            } else {
                val id = graph.steps.getOrNull(row.number - 1)?.id ?: continue
                if (id !in marks) marks[id] = row.mark
            }
        }
        return marks
    }

    private val terminalRunStatuses = setOf(
        "completed", "complete", "done", "succeeded", "failed", "error",
        "cancelled", "canceled", "timed_out", "timeout",
    )

    /** A run has ended when it has an end time or a terminal status (iOS `TaskDetailSnapshot.runHasEnded`). */
    fun runHasEnded(status: String?, endedAt: Long?): Boolean =
        endedAt != null || (status ?: "").lowercase() in terminalRunStatuses

    /**
     * The `/execution-panel` selected run: whether it is active and the plan
     * steps' live marks against [graph].
     */
    fun liveMarks(panel: JsonObject, graph: PlanGraph?): Pair<Boolean, Map<String, PlanStepMark>> {
        val selected = panel["debug"].jObj()?.get("selected_execution").jObj() ?: JsonObject(emptyMap())
        val hasRun = selected.isNotEmpty()
        val endedValue: JsonElement? = selected["ended_at"]
        val endedAt = endedValue.jStr()?.let { runCatching { Instant.parse(it).toEpochMilli() }.getOrNull() }
            ?: endedValue.jNum()?.toLong()
        val ended = runHasEnded(selected["status"].jStr(), endedAt)
        val rows = PlanSteps.rows(panel, runEnded = ended)
        return (hasRun && !ended) to (graph?.let { marks(rows, it) } ?: emptyMap())
    }
}

/**
 * The Plan Inspector's state and actions. Not thread-safe: call it from one
 * coroutine context (the screen ViewModel's main scope).
 */
class PlanInspectorState(val route: PlanInspectorRoute, val client: PlanInspectorClient) {
    var tab: PlanInspectorTab = route.tab
    var record: PlanRecord? = null; private set
    var hasLoaded = false; private set
    var isLoading = false; private set
    var errorMessage: String? = null; private set
    /** One transient line (saved, restored, approved…). */
    var notice: String? = null

    // Editing
    var isEditing = false; private set
    var draft: PlanGraph? = null; private set
    var draftBaseline: PlanGraph? = null; private set
    var isSaving = false; private set

    // History
    var versions: List<PlanVersionSummary> = emptyList(); private set
    var hasMoreVersions = false; private set
    var versionsLoading = false; private set
    var versionsError: String? = null; private set
    var preview: PlanVersionSummary? = null; private set
    var previewGraph: PlanLoadable<PlanGraph> = PlanLoadable.Idle; private set
    var isRestoring = false; private set

    // More panes
    var analysis: PlanLoadable<JsonElement?> = PlanLoadable.Idle; private set
    var slots: PlanLoadable<PlanSlotSnapshot?> = PlanLoadable.Idle; private set
    var attempts: PlanLoadable<List<PlanStrategyAttempt>> = PlanLoadable.Idle; private set
    var clarifications: PlanLoadable<List<PlanClarificationEntry>> = PlanLoadable.Idle; private set

    // Clarifications
    val answers: MutableMap<String, String> = mutableMapOf()
    val answering: MutableSet<String> = mutableSetOf()
    val answerErrors: MutableMap<String, String> = mutableMapOf()
    var resumeForm = PlanManualResume()
    var isResuming = false; private set
    var resumeError: String? = null; private set

    // Approval
    var deciding: PlanDecision? = null; private set

    // Live run marks (from `/execution-panel`)
    var runMarks: Map<String, PlanStepMark> = emptyMap(); private set
    var runActive = false; private set

    private var planGeneration = 0L
    private var versionsGeneration = 0L
    private var previewGeneration = 0L

    // Derived state

    val status: PlanStatus? get() = record?.status
    val graph: PlanGraph? get() = record?.graph
    /** What the Graph and Steps views draw: the draft while editing. */
    val shownGraph: PlanGraph? get() = if (isEditing) draft else graph
    val canEdit: Boolean get() = PlanInspectorRules.canEdit(status, graph != null)
    val isDirty: Boolean get() = PlanInspectorRules.isDirty(isEditing, draft, draftBaseline)
    val showsApprovalBar: Boolean get() = PlanInspectorRules.showsApprovalBar(status, record?.planId, isEditing)
    val pendingQuestions: List<PlanQuestion> get() = record?.pendingQuestions.orEmpty()
    val canResume: Boolean get() = PlanInspectorRules.canResume(status)
    val canReplan: Boolean get() = PlanInspectorRules.canReplan(status)

    private fun describe(error: Throwable): String = error.message ?: error.toString()

    private inline fun <T> attempt(block: () -> T): Result<T> = try {
        Result.success(block())
    } catch (error: CancellationException) {
        throw error
    } catch (error: Exception) {
        Result.failure(error)
    }

    // Loading

    suspend fun load() {
        val generation = ++planGeneration
        isLoading = true
        errorMessage = null
        attempt { client.loadPlan() }
            .onSuccess {
                if (generation != planGeneration) return
                record = it
                hasLoaded = true
            }
            .onFailure {
                if (generation != planGeneration) return
                errorMessage = describe(it)
                hasLoaded = true
            }
        isLoading = false
    }

    /** The toolbar's Refresh: the plan, history, any pane already opened, the live marks. */
    suspend fun refresh() {
        load()
        loadVersions()
        for (pane in PlanInspectorPane.entries) if (paneState(pane) != PlanLoadable.Idle) loadPane(pane)
        refreshLiveMarks()
    }

    suspend fun loadVersions() {
        val generation = ++versionsGeneration
        versionsLoading = true
        versionsError = null
        attempt { client.versions(20) }
            .onSuccess {
                if (generation != versionsGeneration) return
                versions = it.versions
                hasMoreVersions = it.hasMore
            }
            .onFailure {
                if (generation != versionsGeneration) return
                versionsError = describe(it)
            }
        versionsLoading = false
    }

    // Editing

    fun beginEditing() {
        val graph = graph ?: return
        if (!canEdit) return
        val sorted = graph.copy()
        sorted.sortByWaterfall()
        draft = sorted
        draftBaseline = sorted.copy()
        isEditing = true
    }

    /** Leave edit mode, dropping the draft. */
    fun discardEdits() {
        isEditing = false
        draft = null
        draftBaseline = null
    }

    /** Change the draft (a copy, so the baseline never moves). */
    fun editDraft(change: (PlanGraph) -> Unit) {
        if (!isEditing) return
        val graph = draft?.copy() ?: return
        change(graph)
        draft = graph
    }

    /** Change a dependency, then re-sort into waterfall order as the web does. */
    fun setDependency(stepId: String, on: String, enabled: Boolean): Boolean {
        var changed = false
        editDraft { graph ->
            changed = graph.setDependency(stepId, on, enabled)
            if (changed) graph.sortByWaterfall()
        }
        return changed
    }

    fun makeStart(stepId: String) = editDraft { graph ->
        graph.makeStart(stepId)
        graph.sortByWaterfall()
    }

    fun addStep(nowMillis: Long = System.currentTimeMillis()): String? {
        var id: String? = null
        editDraft { id = it.addStep(nowMillis) }
        return id
    }

    /** `PUT /plan` with the draft; on success leave edit mode and reload. */
    suspend fun save(label: String?): Boolean {
        val draft = draft
        if (!isEditing || draft == null || isSaving) return false
        isSaving = true
        errorMessage = null
        try {
            val saved = attempt { client.savePlan(draft, label) }.getOrElse {
                errorMessage = describe(it)
                return false
            }
            record = PlanRecord.merging(saved, record, draft)
            discardEdits()
            notice = "Plan saved."
            load()
            loadVersions()
            return true
        } finally {
            isSaving = false
        }
    }

    // History

    suspend fun previewVersion(version: PlanVersionSummary) {
        val generation = ++previewGeneration
        preview = version
        previewGraph = PlanLoadable.Loading
        attempt { client.version(version.epochMs) }
            .onSuccess {
                if (generation != previewGeneration) return
                previewGraph = it?.let { graph -> PlanLoadable.Loaded(graph) }
                    ?: PlanLoadable.Failed("This version has no plan graph.")
            }
            .onFailure {
                if (generation != previewGeneration) return
                previewGraph = PlanLoadable.Failed(describe(it))
            }
    }

    fun clearPreview() {
        previewGeneration++
        preview = null
        previewGraph = PlanLoadable.Idle
    }

    /** Restore the previewed version (the screen confirms first), refetch, and show Steps. */
    suspend fun restorePreview(): Boolean {
        val preview = preview
        if (preview == null || isRestoring) return false
        isRestoring = true
        try {
            val restored = attempt { client.restore(preview.epochMs) }.getOrElse {
                previewGraph = PlanLoadable.Failed(describe(it))
                return false
            }
            record = PlanRecord.merging(restored, record, null)
            discardEdits()
            load()
            loadVersions()
            clearPreview()
            tab = PlanInspectorTab.Steps
            notice = "Plan version restored."
            return true
        } finally {
            isRestoring = false
        }
    }

    // More panes

    fun paneState(pane: PlanInspectorPane): PlanLoadable<Boolean> {
        fun <T> shape(state: PlanLoadable<T>): PlanLoadable<Boolean> = when (state) {
            PlanLoadable.Idle -> PlanLoadable.Idle
            PlanLoadable.Loading -> PlanLoadable.Loading
            is PlanLoadable.Loaded -> PlanLoadable.Loaded(true)
            is PlanLoadable.Failed -> state
        }
        return when (pane) {
            PlanInspectorPane.Analysis -> shape(analysis)
            PlanInspectorPane.Slots -> shape(slots)
            PlanInspectorPane.Attempts -> shape(attempts)
            PlanInspectorPane.Clarifications -> shape(clarifications)
        }
    }

    private suspend fun <T> loadInto(set: (PlanLoadable<T>) -> Unit, fetch: suspend () -> T) {
        set(PlanLoadable.Loading)
        attempt { fetch() }
            .onSuccess { set(PlanLoadable.Loaded(it)) }
            .onFailure { set(PlanLoadable.Failed(describe(it))) }
    }

    suspend fun loadPane(pane: PlanInspectorPane) = when (pane) {
        PlanInspectorPane.Analysis -> loadInto({ analysis = it }) { client.analysis() }
        PlanInspectorPane.Slots -> loadInto({ slots = it }) { client.slots() }
        PlanInspectorPane.Attempts -> loadInto({ attempts = it }) { client.attempts() }
        PlanInspectorPane.Clarifications -> loadInto({ clarifications = it }) { client.clarifications() }
    }

    // Clarifications

    /**
     * Answer a pending question (an option's value or the typed text). One
     * request per question at a time.
     */
    suspend fun answer(question: PlanQuestion, value: String? = null) {
        val text = (value ?: answers[question.id] ?: "").trim()
        if (text.isEmpty() || question.id in answering) return
        answering.add(question.id)
        answerErrors.remove(question.id)
        try {
            attempt { client.answer(question.id, text) }
                .onSuccess {
                    answers.remove(question.id)
                    notice = "Response sent."
                }
                .onFailure {
                    answerErrors[question.id] = describe(it)
                    return
                }
            load()
            loadPane(PlanInspectorPane.Clarifications)
        } finally {
            answering.remove(question.id)
        }
    }

    suspend fun submitResume() {
        if (isResuming) return
        resumeError = null
        isResuming = true
        try {
            val resumed = attempt { client.resume(resumeForm) }.getOrElse {
                resumeError = describe(it)
                return
            }
            resumeForm = PlanManualResume()
            notice = if (resumed) "Planning resumed."
            else "Manual resume recorded. Planning is still waiting on remaining inputs."
            load()
            for (pane in PlanInspectorPane.entries) loadPane(pane)
        } finally {
            isResuming = false
        }
    }

    // Approval

    /**
     * Reject, Approve or Approve & Run the draft. After a started run the
     * live marks are fetched once; the screen keeps polling while
     * [runActive].
     */
    suspend fun decide(decision: PlanDecision) {
        val planId = record?.planId
        if (deciding != null || planId == null) return
        deciding = decision
        try {
            val outcome: PlanApprovalOutcome = when (decision) {
                PlanDecision.Reject -> attempt { client.reject(planId) }.fold(
                    { PlanApprovalOutcome.Rejected }, { PlanApprovalOutcome.Failed(describe(it)) })
                PlanDecision.Approve -> attempt { client.approve(planId) }.fold(
                    { PlanApprovalOutcome.Approved }, { PlanApprovalOutcome.Failed(describe(it)) })
                PlanDecision.ApproveAndRun -> PlanApprovalActions.approveAndRun(
                    taskId = route.taskId,
                    approve = {
                        attempt { client.approve(planId) }.fold(
                            { PlanApprovalPost.Ok }, { PlanApprovalPost.Failed(describe(it)) })
                    },
                    startRun = { client.startRun() },
                )
            }
            notice = outcome.message
            load()
            if (outcome == PlanApprovalOutcome.ApprovedAndStarted) refreshLiveMarks()
        } finally {
            deciding = null
        }
    }

    suspend fun replan() {
        attempt { client.replan() }
            .onSuccess {
                discardEdits()
                notice = "Planning restarted."
                load()
            }
            .onFailure { errorMessage = describe(it) }
    }

    // Live run marks

    /** Fetch the run's step marks once (a failed fetch changes nothing). */
    suspend fun refreshLiveMarks() {
        val panel = attempt { client.executionPanel() }.getOrNull() ?: return
        apply(panel)
    }

    fun apply(panel: JsonObject) {
        val (active, marks) = PlanInspectorRules.liveMarks(panel, graph)
        runActive = active
        runMarks = marks
    }
}
