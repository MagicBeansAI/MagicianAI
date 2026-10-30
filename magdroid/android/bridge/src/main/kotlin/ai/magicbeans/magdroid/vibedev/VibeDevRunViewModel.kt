package ai.magicbeans.magdroid.vibedev

import ai.magicbeans.magdroid.tasks.TaskCompletion
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/** One turn's timeline as the run screen holds it. */
data class VibeDevTimelineState(
    val loading: Boolean = false,
    val timeline: VibeDevTimeline? = null,
    val error: String? = null,
)

data class VibeDevRunData(
    val turns: VibeDevRunTurns? = null,
    /** This run's row on the live list, when it is there. */
    val live: VibeDevLiveRun? = null,
    val changes: VibeDevRunChanges? = null,
    val changesError: String? = null,
    /** Show the changes of one turn only. */
    val changesTurn: String? = null,
    val expanded: Set<String> = emptySet(),
    val timelines: Map<String, VibeDevTimelineState> = emptyMap(),
    /** Stop or Discard is waiting out its undo window. */
    val pendingStop: Boolean = false,
    val pendingDiscard: Boolean = false,
) {
    /** The live list knows best (needs you); otherwise the newest turn says. */
    val state: VibeDevRunState
        get() = live?.state ?: turns?.turns?.lastOrNull()?.state ?: VibeDevRunState.fromTaskStatus(turns?.status)

    val isLive: Boolean get() = !pendingStop && (state == VibeDevRunState.Working || state == VibeDevRunState.Paused)
    /** Done, but the newest turn left requirements open. */
    val isPartlyDone: Boolean
        get() = state == VibeDevRunState.Done &&
            (turns?.turns?.lastOrNull()?.isPartlyDone == true || live?.isPartlyDone == true)
    val statusLabel: String get() = if (isPartlyDone) TaskCompletion.PARTLY_DONE else state.label
    val openItemsLine: String?
        get() = if (!isPartlyDone) null
            else turns?.turns?.lastOrNull()?.openItems?.takeIf { it.isNotEmpty() }?.let(TaskCompletion::openLine)
                ?: live?.openItemsLine
    /** Waiting on a diff approval the run screen can answer itself. */
    val canAnswerApproval: Boolean get() = state == VibeDevRunState.NeedsYou && live?.hasApproval == true
    val liveTaskId: String? get() = live?.liveTaskId ?: turns?.liveTaskId
    val taskIds: Set<String> get() = buildSet {
        turns?.let { add(it.runId); it.turns.forEach { turn -> add(turn.taskId) } }
        live?.let { add(it.runId); add(it.liveTaskId) }
    }
}

/** One run: header, turns with their timelines, changes, and its actions. */
class VibeDevRunViewModel(
    val runId: String,
    source: () -> VibeDevSource,
    live: Flow<VibeDevSignal>,
    clock: () -> Long = System::currentTimeMillis,
    /** The offline copy for the active machine, or null for none. */
    private val cache: () -> VibeDevCache? = { null },
    /** The active machine and scope; a read that outlives it is dropped. */
    private val identity: () -> Any? = { null },
) : VibeDevScreenModel(source, live, clock) {
    private val mutable = MutableStateFlow(VibeDevRunData())
    val data: StateFlow<VibeDevRunData> = mutable.asStateFlow()
    private var autoExpanded = false

    init {
        cache()?.let { stored ->
            restore({ stored.loadRun(runId) }) { turns -> mutable.value = mutable.value.copy(turns = turns) }
        }
    }

    override suspend fun load() = coroutineScope {
        // Machine, connection and cache resolved once, before the reads, so a
        // switch mid-read cannot file one machine's run under another.
        val owner = identity()
        val stored = cache()
        val api = source()
        val liveRuns = async { runCatching { api.liveRuns() }.getOrNull() }
        val changes = async { runCatching { api.changes(runId, mutable.value.changesTurn) } }
        val turns = api.runTurns(runId)
        val row = liveRuns.await()?.runs?.firstOrNull { it.runId == turns.runId || it.runId == runId }
        val changed = changes.await()
        // Switched while this read was out: it is the old machine's.
        if (identity() != owner) return@coroutineScope
        mutable.value = mutable.value.copy(
            turns = turns,
            live = row,
            changes = changed.getOrNull() ?: mutable.value.changes,
            changesError = changed.exceptionOrNull()?.message,
        )
        stored?.let { persist { it.saveRun(turns, clock(), runId) } }
        // A live run opens on its newest turn, as the web's rail does.
        if (!autoExpanded) {
            autoExpanded = true
            val last = turns.turns.lastOrNull()
            if (last != null && mutable.value.isLive) expand(last.taskId)
        }
        // Expanded turns that are still going follow along.
        mutable.value.expanded.forEach { id ->
            val turn = turns.turns.firstOrNull { it.taskId == id }
            if (turn != null && turn.state.isLive) loadTimeline(id)
        }
    }

    fun toggleTurn(taskId: String) {
        if (taskId in mutable.value.expanded) {
            mutable.value = mutable.value.copy(expanded = mutable.value.expanded - taskId)
        } else {
            expand(taskId)
        }
    }

    private fun expand(taskId: String) {
        mutable.value = mutable.value.copy(expanded = mutable.value.expanded + taskId)
        if (mutable.value.timelines[taskId]?.timeline == null) loadTimeline(taskId)
    }

    fun loadTimeline(taskId: String) {
        val before = mutable.value.timelines[taskId] ?: VibeDevTimelineState()
        if (before.loading) return
        setTimeline(taskId, before.copy(loading = true, error = null))
        viewModelScope.launch {
            try {
                val timeline = source().timeline(taskId)
                setTimeline(taskId, VibeDevTimelineState(timeline = timeline))
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                setTimeline(taskId, before.copy(loading = false, error = error.message ?: "The timeline could not be loaded."))
            }
        }
    }

    private fun setTimeline(taskId: String, value: VibeDevTimelineState) {
        mutable.value = mutable.value.copy(timelines = mutable.value.timelines + (taskId to value))
    }

    /** Filter Changes to one turn, or (null) the whole run. */
    fun showChanges(turn: String?) {
        if (turn == mutable.value.changesTurn) return
        mutable.value = mutable.value.copy(changesTurn = turn, changes = null)
        reload(quiet = true)
    }

    fun steer(message: String) {
        val text = message.trim()
        val target = mutable.value.liveTaskId ?: return
        if (text.isEmpty()) return
        act("steer") {
            val outcome = control(target, "steer", text)
            notify(if (outcome == VibeDevControlOutcome.Delivered) "Sent" else BETWEEN_TURNS)
        }
    }

    fun stop() {
        val target = mutable.value.liveTaskId ?: return
        mutable.value = mutable.value.copy(pendingStop = true)
        undo.schedule("Stopping this run", onUndo = { mutable.value = mutable.value.copy(pendingStop = false) }) {
            write("stop", "Stopped") { control(target, "stop") }
            mutable.value = mutable.value.copy(pendingStop = false)
        }
    }

    private val rootId: String get() = mutable.value.turns?.runId ?: runId

    fun merge() = act("land", "Merged into the base branch") { merge(rootId) }

    fun openPullRequest() {
        val turns = mutable.value.turns
        val title = turns?.title?.removePrefix("VibeDev · ")?.ifBlank { null } ?: "VibeDev run"
        val body = turns?.turns?.mapNotNull { it.summary?.takeIf(String::isNotBlank) }?.joinToString("\n\n").orEmpty()
        act("land", "Pull request opened") { openPullRequest(rootId, title, body) }
    }

    fun discard() {
        mutable.value = mutable.value.copy(pendingDiscard = true)
        undo.schedule("Discarding this run's branch", onUndo = { mutable.value = mutable.value.copy(pendingDiscard = false) }) {
            write("land", "Discarded") { discard(rootId) }
            mutable.value = mutable.value.copy(pendingDiscard = false)
        }
    }

    fun approve(approve: Boolean) {
        val ids = mutable.value.taskIds.ifEmpty { setOf(runId) }
        act("approval", if (approve) "Changes applied" else "Changes rejected") {
            answerDiffApproval(ids, approve, mutable.value.live?.approvalId)
        }
    }
}
