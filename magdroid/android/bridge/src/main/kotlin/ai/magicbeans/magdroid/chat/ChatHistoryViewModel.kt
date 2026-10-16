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
}

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

    fun refresh() = fetchCurrentPage()

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

    private fun fetchCurrentPage() {
        fetchGeneration++
        val generation = fetchGeneration
        fetchJob?.cancel()
        _state.value = _state.value.copy(loading = true, error = null)
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
                if (generation == fetchGeneration) {
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
        mutate("session:$id") {
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
        mutate("thread:${thread.id}") {
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

    private fun mutate(key: String, block: suspend () -> Unit) {
        if (_state.value.mutationInFlight != null) return
        _state.value = _state.value.copy(mutationInFlight = key, error = null)
        viewModelScope.launch {
            try {
                block()
                _state.value = _state.value.copy(mutationInFlight = null)
                fetchCurrentPage()
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (problem: Throwable) {
                _state.value = _state.value.copy(
                    mutationInFlight = null,
                    error = historyError(problem, "That history change could not be saved."),
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
