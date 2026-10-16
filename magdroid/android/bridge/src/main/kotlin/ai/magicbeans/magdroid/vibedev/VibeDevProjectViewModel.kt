package ai.magicbeans.magdroid.vibedev

import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

data class VibeDevProjectData(
    val project: VibeDevProject? = null,
    val crews: List<VibeDevCrew> = emptyList(),
    /** Every task on the pages read so far, in server order. */
    val tasks: List<VibeDevTaskItem> = emptyList(),
    val links: Map<String, VibeDevRunLink> = emptyMap(),
    val totalRuns: Int = 0,
    val hasMore: Boolean = false,
    /** The next page's offset (runs, not tasks). */
    val nextOffset: Int = 0,
    val query: String = "",
    val loadingMore: Boolean = false,
    val moreError: String? = null,
) {
    val runs: List<VibeDevRunRow> get() = VibeDevRunsPage(tasks, links).runRows()
    val openCrews: List<VibeDevCrew> get() = crews.filterNot { it.isClosed }
    val finishedCrews: Int get() = crews.count { it.isClosed }
}

/** One project: header, its crews, and its runs a page at a time. */
class VibeDevProjectViewModel(
    val projectId: String,
    source: () -> VibeDevSource,
    live: Flow<VibeDevSignal>,
    clock: () -> Long = System::currentTimeMillis,
) : VibeDevScreenModel(source, live, clock) {
    private val mutable = MutableStateFlow(VibeDevProjectData())
    val data: StateFlow<VibeDevProjectData> = mutable.asStateFlow()
    private var searchJob: Job? = null

    override suspend fun load() = coroutineScope {
        val current = mutable.value
        // A re-read covers what is already on screen, up to the server's cap.
        val runsShown = current.runs.size
        val limit = runsShown.coerceIn(PAGE, 50)
        val projects = async { source().projects() }
        val crews = async { runCatching { source().crews(projectId) }.getOrNull() }
        val page = source().projectRuns(projectId, limit, 0, current.query)
        val project = projects.await().projects.firstOrNull { it.projectId == projectId }
            ?: throw VibeDevError(404, "This project is gone.")
        mutable.value = mutable.value.copy(
            project = project,
            crews = crews.await() ?: project.crews,
            tasks = page.tasks,
            links = page.links,
            totalRuns = page.pagination.totalRuns,
            hasMore = page.pagination.hasMore,
            nextOffset = page.pagination.offset + page.pagination.runs,
            moreError = null,
        )
    }

    /** Server search over run titles; typing settles before it reads. */
    fun search(query: String) {
        if (query == mutable.value.query) return
        mutable.value = mutable.value.copy(query = query)
        searchJob?.cancel()
        searchJob = viewModelScope.launch {
            delay(SEARCH_DEBOUNCE_MS)
            mutable.value = mutable.value.copy(tasks = emptyList(), links = emptyMap())
            reload()
        }
    }

    fun loadMore() {
        val current = mutable.value
        if (!current.hasMore || current.loadingMore) return
        mutable.value = current.copy(loadingMore = true, moreError = null)
        viewModelScope.launch {
            try {
                val page = source().projectRuns(projectId, PAGE, current.nextOffset, current.query)
                val known = mutable.value.tasks.map { it.id }.toSet()
                mutable.value = mutable.value.copy(
                    tasks = mutable.value.tasks + page.tasks.filterNot { it.id in known },
                    links = mutable.value.links + page.links,
                    totalRuns = page.pagination.totalRuns,
                    hasMore = page.pagination.hasMore,
                    nextOffset = page.pagination.offset + page.pagination.runs,
                    loadingMore = false,
                )
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                mutable.value = mutable.value.copy(loadingMore = false, moreError = error.message ?: "More runs could not be loaded.")
            }
        }
    }

    companion object {
        const val PAGE = 20
        const val SEARCH_DEBOUNCE_MS = 400L
    }
}
