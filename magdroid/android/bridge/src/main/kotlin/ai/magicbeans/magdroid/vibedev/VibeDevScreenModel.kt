package ai.magicbeans.magdroid.vibedev

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** A signal that what a VibeDev screen shows may have changed. */
enum class VibeDevSignal { Event, Resync }

/** A destructive action waiting out its undo window. */
data class VibeDevPendingUndo(val id: Long, val message: String)

/**
 * Stop, Discard and Keep none wait [windowMs] before they are sent, and
 * Undo inside the window sends nothing. A second destructive action while one
 * is pending sends the first at once rather than dropping it.
 */
class VibeDevUndo(private val scope: CoroutineScope, private val windowMs: Long = WINDOW_MS) {
    private val mutable = MutableStateFlow<VibeDevPendingUndo?>(null)
    val pending: StateFlow<VibeDevPendingUndo?> = mutable.asStateFlow()

    private var job: Job? = null
    private var commit: (suspend () -> Unit)? = null
    private var revert: (() -> Unit)? = null
    private var next = 0L

    fun schedule(message: String, onUndo: () -> Unit = {}, commit: suspend () -> Unit) {
        flush()
        val id = ++next
        this.commit = commit
        this.revert = onUndo
        mutable.value = VibeDevPendingUndo(id, message)
        job = scope.launch {
            delay(windowMs)
            if (mutable.value?.id == id) take()?.invoke()
        }
    }

    /** Take the pending action back; nothing is sent. */
    fun undo() {
        if (mutable.value == null) return
        job?.cancel()
        job = null
        commit = null
        mutable.value = null
        revert?.invoke()
        revert = null
    }

    /** Send the pending action now, on [into] when the owner's scope is ending. */
    fun flush(into: CoroutineScope = scope) {
        if (mutable.value == null) return
        job?.cancel()
        val action = take() ?: return
        into.launch { action() }
    }

    /** Close the window and hand back its action, exactly once. */
    private fun take(): (suspend () -> Unit)? {
        val action = commit ?: return null
        commit = null
        revert = null
        job = null
        mutable.value = null
        return action
    }

    companion object {
        const val WINDOW_MS = 2_000L
    }
}

/** Shared loading, staleness and live re-read for the VibeDev view models. */
abstract class VibeDevScreenModel(
    protected val source: () -> VibeDevSource,
    private val live: Flow<VibeDevSignal>,
    protected val clock: () -> Long = System::currentTimeMillis,
) : ViewModel() {
    protected val status = MutableStateFlow(VibeDevStatus())
    /** Loading, the last good read's time, and the last failure. */
    val loadStatus: StateFlow<VibeDevStatus> = status.asStateFlow()

    val undo = VibeDevUndo(viewModelScope)

    /** Crews taken off the screen while their undo window runs. */
    protected val hiddenCrews = MutableStateFlow<Set<String>>(emptySet())
    val hiddenCrewIds: StateFlow<Set<String>> = hiddenCrews.asStateFlow()

    private var liveJob: Job? = null
    private var loadJob: Job? = null

    /** Read what the screen shows. Called on attach, on events and on refresh. */
    protected abstract suspend fun load()

    /**
     * The screen is visible: read now and follow live events. Events are
     * coalesced to at most one re-read per [LIVE_DEBOUNCE_MS] — not a plain
     * debounce, which a running turn's steady `coding.*` stream would starve.
     */
    fun attach() {
        refresh(quiet = status.value.updatedAtMs != null)
        if (liveJob?.isActive == true) return
        liveJob = viewModelScope.launch {
            var scheduled: Job? = null
            live.collect {
                if (scheduled?.isActive != true) {
                    scheduled = launch {
                        delay(LIVE_DEBOUNCE_MS)
                        refresh(quiet = true)
                    }
                }
            }
        }
    }

    /** Off screen or in the background: stop following events. */
    fun detach() {
        liveJob?.cancel()
        liveJob = null
    }

    /**
     * Re-read. A [quiet] read keeps the screen as it is while loading, and a
     * failure keeps the last good data with its time.
     */
    fun refresh(quiet: Boolean = false) {
        // A change that lands mid-read is read once more afterwards.
        if (loadJob?.isActive == true) { rerun = true; return }
        if (!quiet) status.value = status.value.copy(loading = true)
        loadJob = viewModelScope.launch {
            try {
                load()
                status.value = status.value.copy(loading = false, error = null, updatedAtMs = clock())
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                status.value = status.value.copy(loading = false, error = error.message ?: "VibeDev could not be reached.")
            }
            if (rerun) {
                rerun = false
                loadJob = null
                refresh(quiet = true)
            }
        }
    }
    private var rerun = false

    /** Drop a read in flight (its inputs changed) and read again. */
    fun reload(quiet: Boolean = false) {
        rerun = false
        loadJob?.cancel()
        loadJob = null
        refresh(quiet)
    }

    /**
     * Show a cached copy until the first live read lands: [read] runs off the
     * main thread, and [show] applies it only if nothing newer arrived.
     */
    protected fun <T> restore(read: () -> VibeDevCached<T>?, show: (T) -> Unit) {
        viewModelScope.launch {
            val cached = runCatching { withContext(Dispatchers.IO) { read() } }.getOrNull() ?: return@launch
            if (status.value.updatedAtMs != null) return@launch
            show(cached.value)
            status.value = status.value.copy(updatedAtMs = cached.savedAtMs)
        }
    }

    /** Keep a copy of a good read for the next cold start. */
    protected suspend fun persist(write: () -> Unit) {
        runCatching { withContext(Dispatchers.IO) { write() } }
    }

    /** Clear a shown notice. */
    fun consumeNotice() { status.value = status.value.copy(notice = null) }

    protected fun notify(message: String) { status.value = status.value.copy(notice = message) }

    /** Run a write, marking [key] busy, and re-read after it. */
    protected fun act(key: String, success: String? = null, block: suspend VibeDevSource.() -> Unit) {
        if (key in status.value.busy) return
        viewModelScope.launch { write(key, success, block) }
    }

    /** [act]'s body, for callers already in a coroutine (an undo commit). */
    protected suspend fun write(key: String, success: String? = null, block: suspend VibeDevSource.() -> Unit) {
        status.value = status.value.copy(busy = status.value.busy + key)
        try {
            source().block()
            success?.let(::notify)
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Exception) {
            notify(error.message ?: "That did not go through.")
        } finally {
            status.value = status.value.copy(busy = status.value.busy - key)
            refresh(quiet = true)
        }
    }

    /** A crew decision, through the undo window when it cancels work. */
    fun decide(projectId: String, crew: VibeDevCrew, decision: VibeDevCrewDecision) {
        val key = "crew:${crew.crewId}"
        if (decision.isDestructive) {
            hiddenCrews.value = hiddenCrews.value + crew.crewId
            undo.schedule(
                if (decision == VibeDevCrewDecision.KeepNone) "Keeping none" else "Crew not started",
                onUndo = { hiddenCrews.value = hiddenCrews.value - crew.crewId },
            ) {
                write(key) { applyCrewDecision(this, projectId, crew, decision) }
                hiddenCrews.value = hiddenCrews.value - crew.crewId
            }
            return
        }
        act(key, decisionNotice(decision)) { applyCrewDecision(this, projectId, crew, decision) }
    }

    override fun onCleared() {
        // The view model's scope is already closing; a pending Stop still goes.
        undo.flush(CoroutineScope(SupervisorJob() + Dispatchers.IO))
        super.onCleared()
    }

    companion object {
        const val LIVE_DEBOUNCE_MS = 1_000L
        const val BETWEEN_TURNS = "Between turns. Open it to send a follow-up."

        internal suspend fun applyCrewDecision(
            source: VibeDevSource,
            projectId: String,
            crew: VibeDevCrew,
            decision: VibeDevCrewDecision,
        ) {
            when (decision) {
                VibeDevCrewDecision.Start -> source.crewAction(projectId, crew.crewId, "start")
                VibeDevCrewDecision.DontStart -> crew.startRequestId
                    ?.let { source.answerQuestion(it, "cancel") }
                    ?: source.crewAction(projectId, crew.crewId, "stop")
                is VibeDevCrewDecision.Keep -> source.chooseCrew(projectId, crew.crewId, decision.laneIndex)
                VibeDevCrewDecision.KeepNone -> source.chooseCrew(projectId, crew.crewId, null)
                VibeDevCrewDecision.Resolve -> source.crewAction(projectId, crew.crewId, "resolve")
                VibeDevCrewDecision.ResolveMyself -> crew.resolveRequestId?.let { source.answerQuestion(it, "manual") }
                VibeDevCrewDecision.MergeAgain -> source.crewAction(projectId, crew.crewId, "integrate")
                VibeDevCrewDecision.OpenPullRequest -> source.crewAction(projectId, crew.crewId, "pull-request")
                VibeDevCrewDecision.NotNow -> crew.pullRequestRequestId?.let { source.answerQuestion(it, "later") }
            }
        }

        internal fun decisionNotice(decision: VibeDevCrewDecision): String = when (decision) {
            VibeDevCrewDecision.Start -> "Crew started"
            is VibeDevCrewDecision.Keep -> "Keeping ${decision.lane}"
            VibeDevCrewDecision.Resolve -> "Integrator started"
            VibeDevCrewDecision.ResolveMyself -> "Left for you on the lane branches"
            VibeDevCrewDecision.MergeAgain -> "Merging the lanes again"
            VibeDevCrewDecision.OpenPullRequest -> "Pull request opened"
            VibeDevCrewDecision.NotNow -> "Not now"
            VibeDevCrewDecision.DontStart -> "Crew not started"
            VibeDevCrewDecision.KeepNone -> "Kept none"
        }
    }
}

data class VibeDevStatus(
    val loading: Boolean = false,
    val error: String? = null,
    /** When the shown data was last read; null before the first good read. */
    val updatedAtMs: Long? = null,
    val busy: Set<String> = emptySet(),
    val notice: String? = null,
)
