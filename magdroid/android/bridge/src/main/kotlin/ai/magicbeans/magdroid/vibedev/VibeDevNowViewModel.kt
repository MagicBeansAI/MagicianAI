package ai.magicbeans.magdroid.vibedev

import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** What the Now and Projects tabs show. */
data class VibeDevNowData(
    val runs: List<VibeDevLiveRun> = emptyList(),
    val projects: List<VibeDevProject> = emptyList(),
    /** Runs taken off the list while their Stop waits out its undo window. */
    val stopping: Set<String> = emptySet(),
    val hiddenCrews: Set<String> = emptySet(),
) {
    private val visibleProjects: List<VibeDevProject>
        get() = projects.map { project ->
            if (hiddenCrews.isEmpty()) project else project.copy(crews = project.crews.filterNot { it.crewId in hiddenCrews })
        }

    val needsYou: List<VibeDevNeedsYou> get() = VibeDevNeedsYou.order(visibleProjects, runs)

    /** Crews whose lanes are going. */
    val runningCrews: List<Pair<VibeDevProject, VibeDevCrew>>
        get() = visibleProjects.filterNot { it.archived }.flatMap { project ->
            project.crews.filter { it.isRunning }.map { project to it }
        }.sortedByDescending { it.second.updatedAtMs }

    val working: List<VibeDevLiveRun>
        get() = runs.filter { (it.state == VibeDevRunState.Working || it.state == VibeDevRunState.Paused) && it.runId !in stopping }

    /** Ended in the server's recent window (30 minutes). */
    val justFinished: List<VibeDevLiveRun> get() = runs.filter { it.state.isEnded }

    val openProjects: List<VibeDevProject>
        get() = projects.filterNot { it.archived }.sortedByDescending { it.updatedAtMs }

    fun liveCount(projectId: String): Int =
        runs.count { it.projectId == projectId && it.state.isLive }
}

/** VibeDev home: the Now inbox and the project list, from one read. */
class VibeDevNowViewModel(
    source: () -> VibeDevSource,
    live: Flow<VibeDevSignal>,
    clock: () -> Long = System::currentTimeMillis,
    /** The offline copy for the active machine, or null for none. */
    private val cache: () -> VibeDevCache? = { null },
    /** Each good live-runs read, for the coding-runs notification. */
    private val onRuns: (List<VibeDevLiveRun>) -> Unit = {},
    /** The active machine and scope; a read that outlives it is dropped. */
    private val identity: () -> Any? = { null },
) : VibeDevScreenModel(source, live, clock) {
    private val mutable = MutableStateFlow(VibeDevNowData())
    val data: StateFlow<VibeDevNowData> = mutable.asStateFlow()

    init {
        cache()?.let { stored ->
            restore({ stored.loadNow() }) { snapshot ->
                mutable.value = mutable.value.copy(runs = snapshot.runs, projects = snapshot.projects)
            }
        }
    }

    override suspend fun load() = coroutineScope {
        // Machine, connection and cache resolved once, before the reads, so a
        // switch mid-read cannot file one machine's runs under another.
        val owner = identity()
        val stored = cache()
        val api = source()
        val runs = async { api.liveRuns() }
        val projects = async { api.projects() }
        val loaded = mutable.value.copy(runs = runs.await().runs, projects = projects.await().projects)
        // Switched while this read was out: it is the old machine's, and must
        // not reach the coding-runs card, the badge or the new cache.
        if (identity() != owner) return@coroutineScope
        mutable.value = loaded
        VibeDevBadge.publish(loaded.needsYou.size)
        runCatching { onRuns(loaded.runs) }
        stored?.let { persist { it.saveNow(VibeDevNowSnapshot(loaded.runs, loaded.projects), clock()) } }
        Unit
    }

    fun steer(run: VibeDevLiveRun, message: String) {
        val text = message.trim()
        if (text.isEmpty()) return
        act("run:${run.runId}") {
            val outcome = control(run.liveTaskId, "steer", text)
            notify(if (outcome == VibeDevControlOutcome.Delivered) "Told ${run.lane?.let { "the $it lane" } ?: shortTitle(run.title)}" else BETWEEN_TURNS)
        }
    }

    /** Stop after the undo window; the row leaves the list at once. */
    fun stop(run: VibeDevLiveRun) {
        mutable.value = mutable.value.copy(stopping = mutable.value.stopping + run.runId)
        undo.schedule(
            "Stopping ${shortTitle(run.title)}",
            onUndo = { mutable.value = mutable.value.copy(stopping = mutable.value.stopping - run.runId) },
        ) {
            write("run:${run.runId}", "Stopped") { control(run.liveTaskId, "stop") }
            mutable.value = mutable.value.copy(stopping = mutable.value.stopping - run.runId)
        }
    }

    /** Apply a sandboxed run's waiting change set. */
    fun approve(run: VibeDevLiveRun) {
        act("run:${run.runId}", "Changes applied") {
            answerDiffApproval(setOf(run.liveTaskId, run.runId), approve = true, approvalId = run.approvalId)
        }
    }
}

internal fun shortTitle(title: String): String =
    title.removePrefix("VibeDev · ").let { if (it.length > 28) it.take(26) + "…" else it }

/**
 * The drawer's "N need you" count: the last Now read, so opening the drawer
 * shows a number without its own request.
 */
object VibeDevBadge {
    private val mutable = MutableStateFlow(0)
    val needsYou: StateFlow<Int> = mutable.asStateFlow()
    fun publish(count: Int) { mutable.value = count }

    /** Re-read the count (the drawer opening); a failure keeps the last one. */
    suspend fun refresh(source: VibeDevSource) {
        runCatching {
            coroutineScope {
                val runs = async { source.liveRuns() }
                val projects = async { source.projects() }
                VibeDevNeedsYou.order(projects.await().projects, runs.await().runs).size
            }
        }.getOrNull()?.let(::publish)
    }
}
