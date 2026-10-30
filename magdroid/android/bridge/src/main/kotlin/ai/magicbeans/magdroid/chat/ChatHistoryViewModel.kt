package ai.magicbeans.magdroid.chat

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

data class ChatHistoryState(
    val workingSessionIds: Set<String> = emptySet(),
    val sessions: List<SessionSummary> = emptyList(),
    val threads: List<UiThreadRecord> = emptyList(),
    val searchResults: List<HistorySearchItem> = emptyList(),
    val activeTab: ChatHistoryTab = ChatHistoryTab.Sessions,
    val historyLane: ChatHistoryLane = ChatHistoryLane.Personal,
    val searchText: String = "",
    val appliedSearch: String = "",
    val total: Int = 0,
    val offset: Int = 0,
    val loading: Boolean = false,
    val error: String? = null,
    val mutationInFlight: String? = null,
    /** What [mutationInFlight] is doing, so its row can say "Deleting…". */
    val mutationKind: HistoryMutation? = null,
    val openingThreadId: String? = null,
    val activeSessionId: String? = null,
    val activeThreadId: String = "general",
) {
    val searchActive: Boolean get() = appliedSearch.isNotBlank()
    val currentEmpty: Boolean
        get() = when {
            searchActive -> searchResults.isEmpty()
            activeTab == ChatHistoryTab.Sessions -> sessions.isEmpty()
            else -> threads.isEmpty()
        }
    val pageStart: Int get() = if (total == 0) 0 else offset + 1
    val pageEnd: Int get() = minOf(offset + ChatHistoryViewModel.PAGE_SIZE, total)
    val canLoadPrevious: Boolean get() = offset > 0 && !loading
    val canLoadNext: Boolean get() = offset + ChatHistoryViewModel.PAGE_SIZE < total && !loading
    val canCreate: Boolean get() = historyLane == ChatHistoryLane.Personal && !searchActive

    /** The change being saved for this session's row, if any. */
    fun sessionMutation(sessionId: String): HistoryMutation? =
        mutationKind.takeIf { mutationInFlight == sessionMutationKey(sessionId) }

    /** The change being saved for this thread's row, if any. */
    fun threadMutation(threadId: String): HistoryMutation? =
        mutationKind.takeIf { mutationInFlight == threadMutationKey(threadId) }
}

internal fun sessionMutationKey(sessionId: String) = "session:$sessionId"
internal fun threadMutationKey(threadId: String) = "thread:$threadId"

/**
 * Hold the history controls for one change until the server answers.
 *
 * Null when another change is already in flight: one at a time, so a second
 * delete cannot race the first.
 */
internal fun ChatHistoryState.beginMutation(key: String, kind: HistoryMutation? = null): ChatHistoryState? =
    if (mutationInFlight != null) null else copy(mutationInFlight = key, mutationKind = kind, error = null)

/** Release the controls; a failure keeps the row and says why. */
internal fun ChatHistoryState.settleMutation(error: String? = null): ChatHistoryState =
    copy(mutationInFlight = null, mutationKind = null, error = error ?: this.error)

/** What a row says while its change is being saved. */
fun HistoryMutation.progressLabel(): String = when (this) {
    HistoryMutation.Archive -> "Archiving…"
    HistoryMutation.Restore -> "Restoring…"
    HistoryMutation.Delete -> "Deleting…"
}

/** The visible rows polled once, and which of them were working. */
internal data class HistoryActivityPoll(val polledIds: Set<String>, val working: Set<String>)

/**
 * Whether a poll saw chat activity worth re-reading the page for: the same
 * rows as last time, with a session's working state flipped. A different set
 * of rows means the page itself was just re-read.
 */
internal fun historyActivityChanged(previous: HistoryActivityPoll?, next: HistoryActivityPoll): Boolean =
    previous != null && previous.polledIds == next.polledIds && previous.working != next.working

/** How long a quiet re-read waits so at most one runs per poll interval. */
internal fun quietRefreshDelay(lastAtMs: Long?, nowMs: Long, intervalMs: Long = HISTORY_QUIET_REFRESH_INTERVAL_MS): Long =
    if (lastAtMs == null) 0 else (lastAtMs + intervalMs - nowMs).coerceIn(0, intervalMs)

/** The drawer's activity poll interval; quiet re-reads share it. */
internal const val HISTORY_QUIET_REFRESH_INTERVAL_MS = 3_000L

/**
 * Owns the history drawer independently from the streaming conversation.
 *
 * Search keystrokes and page changes must not rebuild the transcript dozens of
 * times. Fetch generations additionally prevent a slow Personal response from
 * replacing a newer Automated selection after it arrives.
 */
class ChatHistoryViewModel(app: Application) : AndroidViewModel(app) {

    companion object { const val PAGE_SIZE = 15 }

    private val repository = ChatRepository(app)
    private val _state = MutableStateFlow(ChatHistoryState())
    val state: StateFlow<ChatHistoryState> = _state.asStateFlow()

    private var fetchGeneration = 0
    private var threadOpenGeneration = 0
    private var fetchJob: Job? = null
    private var searchJob: Job? = null
    private var quietRefreshJob: Job? = null
    private var lastQuietRefreshAtMs: Long? = null
    private var lastActivityPoll: HistoryActivityPoll? = null

    fun refresh() = fetchCurrentPage()

    fun clearActivity() {
        lastActivityPoll = null
        _state.value = _state.value.copy(workingSessionIds = emptySet())
    }

    /**
     * Re-read the current page without blanking it: rows stay on screen, no
     * spinner, filters/search/tab untouched. Coalesced to at most one per
     * poll interval; skipped while a visible read or a change is in flight,
     * since both end in a full read anyway.
     */
    fun refreshQuietly() {
        if (quietRefreshJob?.isActive == true) return
        val wait = quietRefreshDelay(lastQuietRefreshAtMs, nowMs())
        quietRefreshJob = viewModelScope.launch {
            if (wait > 0) delay(wait)
            val current = _state.value
            if (current.loading || current.mutationInFlight != null) return@launch
            lastQuietRefreshAtMs = nowMs()
            fetchCurrentPage(quiet = true)
        }
    }

    private fun nowMs(): Long = System.nanoTime() / 1_000_000

    /** Called only while the history drawer is visible and the app is started. */
    suspend fun refreshActivity() {
        val generation = fetchGeneration
        val current = _state.value
        val ids = when {
            current.loading || current.error != null -> emptySet()
            current.searchActive -> current.searchResults.mapNotNull { it.session?.identifier() }.toSet()
            current.activeTab == ChatHistoryTab.Sessions -> current.sessions.map { it.identifier() }.toSet()
            else -> emptySet()
        }
        val connection = ai.magicbeans.magdroid.access.MagicianAccess.connectionSnapshot(getApplication())
        val working = try {
            repository.workingSessionIds(ids)
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            null
        }
        if (generation == fetchGeneration && connection == ai.magicbeans.magdroid.access.MagicianAccess.connectionSnapshot(getApplication())) {
            _state.value = _state.value.copy(workingSessionIds = working.orEmpty())
            // A failed poll is no evidence of change; it keeps the baseline.
            if (working != null) {
                val poll = HistoryActivityPoll(ids, working)
                val changed = historyActivityChanged(lastActivityPoll, poll)
                lastActivityPoll = poll
                if (changed) refreshQuietly()
            }
        }
    }

    fun syncActive(sessionId: String?, threadId: String?) {
        val current = _state.value
        val matching = current.sessions.firstOrNull { it.identifier() == sessionId }
            ?: current.searchResults.asSequence().mapNotNull { it.session }
                .firstOrNull { it.identifier() == sessionId }
        _state.value = current.copy(
            activeSessionId = sessionId,
            activeThreadId = threadId?.takeIf { it.isNotBlank() }
                ?: matching?.uiThreadId?.takeIf { it.isNotBlank() }
                ?: current.activeThreadId,
        )
    }

    fun selectTab(tab: ChatHistoryTab) {
        if (_state.value.activeTab == tab) return
        resetAndFetch(_state.value.copy(activeTab = tab))
    }

    fun selectLane(lane: ChatHistoryLane) {
        if (_state.value.historyLane == lane) return
        resetAndFetch(_state.value.copy(historyLane = lane))
    }

    fun updateSearchText(value: String) {
        val bounded = boundedHistoryQuery(value)
        _state.value = _state.value.copy(searchText = bounded)
        searchJob?.cancel()
        searchJob = viewModelScope.launch {
            delay(300)
            applySearch()
        }
    }

    fun submitSearch() {
        searchJob?.cancel()
        applySearch()
    }

    fun clearSearch() {
        searchJob?.cancel()
        _state.value = _state.value.copy(searchText = "")
        applySearch()
    }

    private fun applySearch() {
        val query = _state.value.searchText.trim()
        if (_state.value.appliedSearch == query) return
        resetAndFetch(_state.value.copy(appliedSearch = query))
    }

    fun loadPreviousPage() {
        val current = _state.value
        if (!current.canLoadPrevious) return
        _state.value = current.copy(offset = maxOf(0, current.offset - PAGE_SIZE))
        fetchCurrentPage()
    }

    fun loadNextPage() {
        val current = _state.value
        if (!current.canLoadNext) return
        _state.value = current.copy(offset = current.offset + PAGE_SIZE)
        fetchCurrentPage()
    }

    fun dismissError() { _state.value = _state.value.copy(error = null) }

    private fun resetAndFetch(next: ChatHistoryState) {
        _state.value = next.copy(
            sessions = emptyList(),
            threads = emptyList(),
            searchResults = emptyList(),
            total = 0,
            offset = 0,
            error = null,
        )
        fetchCurrentPage()
    }

    private fun fetchCurrentPage(quiet: Boolean = false) {
        fetchGeneration++
        val generation = fetchGeneration
        fetchJob?.cancel()
        if (!quiet) {
            quietRefreshJob?.cancel()
            _state.value = _state.value.copy(loading = true, error = null, workingSessionIds = emptySet())
        }
        fetchJob = viewModelScope.launch {
            try {
                val request = _state.value
                when {
                    request.searchActive -> repository.searchHistory(
                        request.appliedSearch,
                        PAGE_SIZE,
                        request.offset,
                    ).let { page ->
                        if (generation != fetchGeneration) return@launch
                        _state.value = _state.value.copy(
                            searchResults = page.items,
                            sessions = emptyList(),
                            threads = emptyList(),
                            total = page.total,
                            offset = page.offset,
                        )
                    }
                    request.activeTab == ChatHistoryTab.Sessions -> repository.sessionsPage(
                        historyLane = request.historyLane,
                        limit = PAGE_SIZE,
                        offset = request.offset,
                    ).let { page ->
                        if (generation != fetchGeneration) return@launch
                        _state.value = _state.value.copy(
                            sessions = page.items,
                            threads = emptyList(),
                            searchResults = emptyList(),
                            total = page.total,
                            offset = page.offset,
                        )
                    }
                    else -> repository.threadsPage(
                        request.historyLane,
                        PAGE_SIZE,
                        request.offset,
                    ).let { page ->
                        if (generation != fetchGeneration) return@launch
                        _state.value = _state.value.copy(
                            threads = page.items,
                            sessions = emptyList(),
                            searchResults = emptyList(),
                            total = page.total,
                            offset = page.offset,
                        )
                    }
                }
                if (generation != fetchGeneration) return@launch
                val settled = _state.value
                if (settled.offset > 0 && settled.offset >= settled.total) {
                    _state.value = settled.copy(
                        offset = if (settled.total == 0) 0 else ((settled.total - 1) / PAGE_SIZE) * PAGE_SIZE,
                    )
                    fetchCurrentPage()
                } else {
                    _state.value = settled.copy(loading = false)
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (problem: Throwable) {
                // A quiet re-read keeps the rows it has; the next poll tries again.
                if (generation == fetchGeneration && !quiet) {
                    _state.value = _state.value.copy(
                        loading = false,
                        error = historyError(problem, "History could not be loaded."),
                    )
                }
            }
        }
    }

    fun openSession(session: SessionSummary, onReady: (String) -> Unit) {
        val id = session.identifier()
        _state.value = _state.value.copy(
            activeSessionId = id,
            activeThreadId = session.uiThreadId ?: _state.value.activeThreadId,
        )
        onReady(id)
    }

    /** Resolve a thread to an active session, an archived fallback, or a new session. */
    fun openThread(thread: UiThreadRecord, onReady: (String) -> Unit) {
        threadOpenGeneration++
        val generation = threadOpenGeneration
        _state.value = _state.value.copy(openingThreadId = thread.id, error = null)
        viewModelScope.launch {
            try {
                var offset = 0
                var active: SessionSummary? = null
                var fallback: SessionSummary? = null
                do {
                    val page = repository.sessionsPage(
                        uiThreadId = thread.id,
                        limit = PAGE_SIZE,
                        offset = offset,
                    )
                    if (generation != threadOpenGeneration) return@launch
                    val selected = selectThreadSession(active, fallback, page.items)
                    active = selected.first
                    fallback = selected.second
                    offset += page.items.size
                    if (active != null || page.items.isEmpty() || offset >= page.total) break
                } while (true)

                val sessionId = active?.identifier()
                    ?: fallback?.identifier()
                    ?: repository.createSession(
                        uiThreadId = thread.id,
                        historyLane = ChatHistoryLane.entries.firstOrNull { it.wire == thread.historyLane }
                            ?: ChatHistoryLane.Personal,
                    )
                if (generation != threadOpenGeneration) return@launch
                _state.value = _state.value.copy(
                    openingThreadId = null,
                    activeSessionId = sessionId,
                    activeThreadId = thread.id,
                )
                onReady(sessionId)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (problem: Throwable) {
                if (generation == threadOpenGeneration) {
                    _state.value = _state.value.copy(
                        openingThreadId = null,
                        error = historyError(problem, "That thread could not be opened."),
                    )
                }
            }
        }
    }

    fun createSession(onReady: (String) -> Unit) {
        mutate("create:session") {
            val id = repository.createSession()
            _state.value = _state.value.copy(activeSessionId = id, activeThreadId = "general")
            onReady(id)
        }
    }

    fun createThread(name: String, onCreated: () -> Unit) {
        val clean = name.trim()
        if (clean.isEmpty()) return
        mutate("create:thread") {
            repository.createThread(clean)
            onCreated()
        }
    }

    fun mutateSession(session: SessionSummary, mutation: HistoryMutation, onRemovedActive: () -> Unit) {
        if (session.isDefaultSession) return
        val id = session.identifier()
        mutate(sessionMutationKey(id), mutation) {
            when (mutation) {
                HistoryMutation.Archive -> repository.setSessionArchived(id, true)
                HistoryMutation.Restore -> repository.setSessionArchived(id, false)
                HistoryMutation.Delete -> repository.deleteSession(id)
            }
            if (id == _state.value.activeSessionId && mutation != HistoryMutation.Restore) {
                onRemovedActive()
            }
        }
    }

    fun mutateThread(thread: UiThreadRecord, mutation: HistoryMutation, onRemovedActive: () -> Unit) {
        if (thread.isGeneral) return
        mutate(threadMutationKey(thread.id), mutation) {
            when (mutation) {
                HistoryMutation.Archive -> repository.setThreadArchived(thread.id, true)
                HistoryMutation.Restore -> repository.setThreadArchived(thread.id, false)
                HistoryMutation.Delete -> repository.deleteThread(thread.id)
            }
            if (thread.id == _state.value.activeThreadId && mutation != HistoryMutation.Restore) {
                onRemovedActive()
            }
        }
    }

    /**
     * One change at a time, settled on the server before the list moves: the
     * row says what is happening ("Deleting…") until the server answers, and
     * only then does the list refresh or the open chat switch away.
     */
    private fun mutate(key: String, kind: HistoryMutation? = null, block: suspend () -> Unit) {
        _state.value = _state.value.beginMutation(key, kind) ?: return
        viewModelScope.launch {
            try {
                block()
                _state.value = _state.value.settleMutation()
                fetchCurrentPage()
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (problem: Throwable) {
                _state.value = _state.value.settleMutation(
                    historyError(problem, "That history change could not be saved."),
                )
            }
        }
    }

    override fun onCleared() {
        repository.close()
        super.onCleared()
    }
}

internal fun historyError(problem: Throwable, fallback: String): String = when (problem) {
    is ChatError -> problem.message?.takeIf { it.isNotBlank() } ?: fallback
    else -> serviceHealthMessage(problem).takeIf { it != "Could not reach Magician" } ?: fallback
}
