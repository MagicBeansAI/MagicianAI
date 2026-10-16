package ai.magicbeans.magdroid.vibedev

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * The VibeDev wire shapes the phone reads.
 *
 * Pinned by the canonical fixtures in `magician/tests/fixtures/vibedev/`, the
 * same files the Rust tests, the web tests and `MagiosTests` load
 * (`VibeDevFixtureTest`). Optional fields default so an older server decodes;
 * required ones are the fields every screen keys on.
 */
internal val vibeDevJson: Json = Json { ignoreUnknownKeys = true; isLenient = true; explicitNulls = false }

/** The at-a-glance state of a run, in the server's words (`LiveRunState`). */
enum class VibeDevRunState(val wire: String, val label: String) {
    NeedsYou("needs_you", "Needs you"),
    Working("working", "Working"),
    Paused("paused", "Paused"),
    Failed("failed", "Failed"),
    Stopped("stopped", "Stopped"),
    Done("done", "Done");

    val isLive: Boolean get() = this == Working || this == Paused || this == NeedsYou
    val isEnded: Boolean get() = this == Failed || this == Stopped || this == Done

    companion object {
        fun from(wire: String?): VibeDevRunState = entries.firstOrNull { it.wire == wire } ?: Working

        /**
         * A task status (`running`, `completed`, `failed`, `cancelled`, …)
         * folded to the same states, for rows that come from task lists.
         */
        fun fromTaskStatus(status: String?): VibeDevRunState = when (status?.lowercase()) {
            null, "" -> Working
            "completed", "done", "succeeded", "success" -> Done
            "failed", "error" -> Failed
            "cancelled", "canceled", "stopped", "aborted" -> Stopped
            "paused" -> Paused
            "needs_action", "waiting", "awaiting_approval", "needs_you", "blocked" -> NeedsYou
            else -> Working
        }
    }
}

// ── GET /vibedev/runs/live ──────────────────────────────────────────────────

@Serializable
data class VibeDevLiveRuns(
    val runs: List<VibeDevLiveRun> = emptyList(),
    val counts: Map<String, Int> = emptyMap(),
)

@Serializable
data class VibeDevLiveRun(
    @SerialName("run_id") val runId: String,
    /** The task a steer or stop names: the newest turn still going. */
    @SerialName("live_task_id") val liveTaskId: String,
    @SerialName("project_id") val projectId: String,
    @SerialName("project_name") val projectName: String = "",
    val title: String = "",
    @SerialName("latest_turn_title") val latestTurnTitle: String? = null,
    @SerialName("state") val stateWire: String = "working",
    val status: String = "",
    val step: String? = null,
    val summary: String? = null,
    val turns: Int = 1,
    /** When the run's root was created; absent from older servers. */
    @SerialName("started_at") val startedAt: String? = null,
    /** Spend so far, present only when the run is metered. */
    @SerialName("cost_usd") val costUsd: Double? = null,
    /** A run waiting on a diff approval: the approval (proposal) id to answer. */
    @SerialName("approval_id") val approvalId: String? = null,
    @SerialName("crew_id") val crewId: String? = null,
    val lane: String? = null,
    @SerialName("updated_at") val updatedAt: String = "",
    @SerialName("last_progress_at") val lastProgressAt: String? = null,
) {
    val state: VibeDevRunState get() = VibeDevRunState.from(stateWire)

    /**
     * Waits on a diff approval this phone can answer. Other asks (a question,
     * a plan, a sandbox request) have no approval id and are answered in
     * Attention; Approve/Reject on them only dead-ended.
     */
    val hasApproval: Boolean get() = !approvalId.isNullOrBlank()
}

// ── GET /vibedev/projects ───────────────────────────────────────────────────

@Serializable
data class VibeDevProjects(
    val projects: List<VibeDevProject> = emptyList(),
    @SerialName("active_project_id") val activeProjectId: String? = null,
)

@Serializable
data class VibeDevProject(
    @SerialName("project_id") val projectId: String,
    val name: String = "",
    @SerialName("repo_path") val repoPath: String? = null,
    /** `trusted`; absent means sandboxed. */
    val trust: String? = null,
    @SerialName("base_branch") val baseBranch: String? = null,
    @SerialName("run_task_ids") val runTaskIds: List<String> = emptyList(),
    @SerialName("run_links") val runLinks: Map<String, VibeDevRunLink> = emptyMap(),
    val crews: List<VibeDevCrew> = emptyList(),
    val archived: Boolean = false,
    @SerialName("repo_display_path") val repoDisplayPath: String = "",
    @SerialName("repo_absolute_path") val repoAbsolutePath: String = "",
    @SerialName("chat_session_status") val chatSessionStatus: String = "",
    @SerialName("created_at_ms") val createdAtMs: Long = 0,
    @SerialName("updated_at_ms") val updatedAtMs: Long = 0,
) {
    val isTrusted: Boolean get() = trust == "trusted"
    /** What the cards print for the repository. */
    val repoLabel: String get() = repoDisplayPath.ifBlank { repoPath.orEmpty() }
}

/** `POST /vibedev/projects` and `PATCH /vibedev/projects/{id}`. */
@Serializable
data class VibeDevProjectMutation(val project: VibeDevProject)

@Serializable
data class VibeDevRunLink(
    @SerialName("parent_task_id") val parentTaskId: String? = null,
    @SerialName("crew_id") val crewId: String? = null,
    val lane: String? = null,
)

// ── GET /vibedev/projects/{id}/runs ─────────────────────────────────────────

@Serializable
data class VibeDevRunsPage(
    val tasks: List<VibeDevTaskItem> = emptyList(),
    val links: Map<String, VibeDevRunLink> = emptyMap(),
    val pagination: VibeDevRunsPagination = VibeDevRunsPagination(),
) {
    /**
     * Each root with the turns that follow it. The page lists a root and then
     * its threaded turns (tag `vibedev-threaded`, or a typed parent link);
     * a threaded turn whose root is not on the page stands as its own row.
     */
    fun runRows(): List<VibeDevRunRow> {
        val rows = mutableListOf<VibeDevRunRow>()
        val byId = mutableMapOf<String, Int>()
        tasks.forEach { task ->
            val parent = links[task.id]?.parentTaskId ?: task.parentFromDescription
            val threaded = task.isThreaded || parent != null
            val rootIndex = when {
                !threaded -> null
                parent != null && byId[parent] != null -> byId[parent]
                rows.isNotEmpty() -> rows.lastIndex
                else -> null
            }
            if (rootIndex == null) {
                val link = links[task.id]
                rows += VibeDevRunRow(task, listOf(task), link?.crewId, link?.lane)
                byId[task.id] = rows.lastIndex
            } else {
                val row = rows[rootIndex]
                rows[rootIndex] = row.copy(turns = row.turns + task)
                byId[task.id] = rootIndex
            }
        }
        return rows
    }
}

@Serializable
data class VibeDevRunsPagination(
    val runs: Int = 0,
    @SerialName("total_runs") val totalRuns: Int = 0,
    val offset: Int = 0,
    @SerialName("has_more") val hasMore: Boolean = false,
    @SerialName("next_cursor") val nextCursor: String? = null,
)

/** The `TaskListItemV3` fields a run row needs. */
@Serializable
data class VibeDevTaskItem(
    val id: String,
    val title: String = "",
    val description: String = "",
    val status: String = "",
    @SerialName("updated_at") val updatedAt: String = "",
    @SerialName("created_at") val createdAt: String = "",
    val tags: List<VibeDevTag> = emptyList(),
    @SerialName("completion_summary") val completionSummary: String? = null,
    @SerialName("current_step_title") val currentStepTitle: String? = null,
) {
    val isThreaded: Boolean get() = tags.any { it.name == THREADED_TAG }

    /** The `Parent task:` line the server folds on when there is no typed link. */
    val parentFromDescription: String?
        get() = description.lineSequence()
            .firstOrNull { it.startsWith("Parent task:") }
            ?.removePrefix("Parent task:")?.trim()?.takeIf { it.isNotEmpty() }

    companion object {
        const val THREADED_TAG = "vibedev-threaded"
    }
}

@Serializable
data class VibeDevTag(val name: String = "")

/** One run on a project's list: its root and every turn on the page. */
data class VibeDevRunRow(
    val root: VibeDevTaskItem,
    val turns: List<VibeDevTaskItem>,
    val crewId: String?,
    val lane: String?,
) {
    /** The newest turn decides whether the run is still going. */
    val state: VibeDevRunState get() = VibeDevRunState.fromTaskStatus(turns.last().status)
    val updatedAt: String get() = turns.maxOf { it.updatedAt }
}

// ── Crews ───────────────────────────────────────────────────────────────────

@Serializable
data class VibeDevCrews(val crews: List<VibeDevCrew> = emptyList())

@Serializable
data class VibeDevCrewEnvelope(val crew: VibeDevCrew)

@Serializable
data class VibeDevCrew(
    @SerialName("crew_id") val crewId: String,
    val goal: String = "",
    val status: String = "proposed",
    val mode: String = "split",
    val lanes: List<VibeDevCrewLane> = emptyList(),
    val review: Boolean = false,
    val winner: Int? = null,
    @SerialName("budget_usd") val budgetUsd: Double? = null,
    @SerialName("spent_usd") val spentUsd: Double? = null,
    @SerialName("start_request_id") val startRequestId: String? = null,
    @SerialName("choose_request_id") val chooseRequestId: String? = null,
    @SerialName("resolve_request_id") val resolveRequestId: String? = null,
    @SerialName("pull_request_request_id") val pullRequestRequestId: String? = null,
    val branch: String? = null,
    val conflict: VibeDevCrewConflict? = null,
    @SerialName("pull_request_url") val pullRequestUrl: String? = null,
    val note: String? = null,
    @SerialName("created_at_ms") val createdAtMs: Long = 0,
    @SerialName("updated_at_ms") val updatedAtMs: Long = 0,
) {
    val isRace: Boolean get() = mode == "race"
    /** Lanes are going (or about to): the Now screen's Crews section. */
    val isRunning: Boolean get() = status == "running" || status == "ready"
    /** Nothing more happens; the crew drops off Now. */
    val isClosed: Boolean get() = status == "cancelled" || status == "failed" ||
        (status == "integrated" && pullRequestRequestId == null)
}

@Serializable
data class VibeDevCrewLane(
    val lane: String,
    val brief: String = "",
    val owns: List<String> = emptyList(),
    @SerialName("coding_profile_id") val codingProfileId: String? = null,
    @SerialName("run_task_id") val runTaskId: String? = null,
    val status: String = "pending",
    val role: String = "work",
)

@Serializable
data class VibeDevCrewConflict(val lane: String = "", val files: List<String> = emptyList())

// ── GET /vibedev/runs/{id}/turns ────────────────────────────────────────────

@Serializable
data class VibeDevRunTurns(
    @SerialName("run_id") val runId: String,
    @SerialName("project_id") val projectId: String = "",
    @SerialName("project_name") val projectName: String = "",
    val title: String = "",
    val status: String = "",
    val trusted: Boolean = false,
    val branch: String? = null,
    @SerialName("crew_id") val crewId: String? = null,
    val lane: String? = null,
    val turns: List<VibeDevRunTurn> = emptyList(),
) {
    val costUsd: Double? get() = turns.mapNotNull { it.costUsd }.takeIf { it.isNotEmpty() }?.sum()
    val engine: String? get() = turns.lastOrNull { !it.engine.isNullOrBlank() }?.engine
    /** The task a steer or stop names: the newest turn. */
    val liveTaskId: String get() = turns.lastOrNull()?.taskId ?: runId
}

@Serializable
data class VibeDevRunTurn(
    @SerialName("task_id") val taskId: String,
    val prompt: String = "",
    val mode: String = "build",
    val status: String = "",
    @SerialName("created_at") val createdAt: String = "",
    @SerialName("updated_at") val updatedAt: String = "",
    val summary: String? = null,
    val engine: String? = null,
    @SerialName("cost_usd") val costUsd: Double? = null,
    val commit: String? = null,
    @SerialName("files_changed") val filesChanged: Int? = null,
) {
    val state: VibeDevRunState get() = VibeDevRunState.fromTaskStatus(status)
}

// ── GET /vibedev/runs/{turn}/timeline ───────────────────────────────────────

@Serializable
data class VibeDevTimeline(
    @SerialName("task_id") val taskId: String,
    val cards: List<VibeDevTimelineCard> = emptyList(),
    val truncated: Boolean = false,
)

/** `message | reasoning | action | error | completed | notice`. */
@Serializable
data class VibeDevTimelineCard(
    val index: Int = 0,
    val kind: String = "notice",
    val status: String? = null,
    val title: String = "",
    val text: String? = null,
    @SerialName("at_ms") val atMs: Long = 0,
)

// ── GET /vibedev/runs/{id}/changes ──────────────────────────────────────────

@Serializable
data class VibeDevRunChanges(
    @SerialName("run_id") val runId: String,
    val turn: String? = null,
    /** `branch` (trusted) or `proposals` (sandboxed). */
    val source: String = "branch",
    val files: List<VibeDevChangedFile> = emptyList(),
    val patch: VibeDevFilePatch? = null,
)

@Serializable
data class VibeDevChangedFile(
    val path: String,
    val status: String = "M",
    val additions: Int = 0,
    val deletions: Int = 0,
)

@Serializable
data class VibeDevFilePatch(
    val path: String,
    @SerialName("unified_diff") val unifiedDiff: String = "",
    val truncated: Boolean = false,
)

// ── Composer support ────────────────────────────────────────────────────────

@Serializable
data class VibeDevRepoSuggestions(val repos: List<VibeDevRepoSuggestion> = emptyList())

@Serializable
data class VibeDevRepoSuggestion(val path: String, val name: String = "", val project: String? = null)

@Serializable
data class VibeDevCodingProfiles(val profiles: List<VibeDevCodingProfile> = emptyList())

@Serializable
data class VibeDevCodingProfile(
    val id: String,
    val label: String = "",
    val engine: String? = null,
    val selectable: Boolean = true,
    val readiness: String? = null,
    @SerialName("is_default") val isDefault: Boolean = false,
)

@Serializable
data class VibeDevStartedRun(
    @SerialName("task_id") val taskId: String,
    @SerialName("execution_id") val executionId: String? = null,
    @SerialName("is_follow_up") val isFollowUp: Boolean = false,
    val scheduled: Boolean = false,
    val replayed: Boolean = false,
)

/** What the New run sheet sends (`POST /vibedev/runs`). */
data class VibeDevRunRequest(
    val prompt: String,
    val projectId: String,
    val mode: String = "build",
    /** One per submission; a retry reuses it, so the server starts one run. */
    val clientRunId: String,
    val parentTaskId: String? = null,
    val codingProfileId: String? = null,
    val costBudgetUsd: Double? = null,
)

/** What the New run sheet sends with its crew switch on. */
data class VibeDevCrewRequest(
    val projectId: String,
    val goal: String,
    val mode: String = "split",
    val racers: Int? = null,
    val review: Boolean = true,
    val budgetUsd: Double? = null,
)

/** A steer or stop's result: delivered, or the run is between turns (409). */
enum class VibeDevControlOutcome { Delivered, BetweenTurns }
