package ai.magicbeans.magdroid.tasks

import ai.magicbeans.magdroid.net.Failure
import ai.magicbeans.magdroid.net.Failures
import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.Job
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray
import kotlinx.serialization.json.putJsonObject
import java.net.ConnectException
import java.net.NoRouteToHostException
import java.net.SocketException
import java.net.SocketTimeoutException
import java.net.UnknownHostException
import java.time.LocalDate
import java.time.ZoneId
import java.util.Collections
import java.util.IdentityHashMap

data class TaskUserError(
    val title: String,
    val message: String,
) {
    companion object {
        /** The shared classifier's wording, in the shape this screen holds. */
        fun of(failure: Failure) = TaskUserError(failure.headline, failure.detail)
    }

    val inlineMessage: String
        get() = if (message.isBlank() || message.trimEnd('.') == title.trimEnd('.')) title
        else "${title.trimEnd('.')}. $message"
}

data class DetailState(
    val taskId: String? = null,
    val loading: Boolean = false,
    val bundle: TaskDetailBundle? = null,
    val error: String? = null,
    val selectedExecutionId: String? = null,
    /** Explicit entry point (e.g. the card's "Result"); overrides the verdict's default act. */
    val preferredAct: TaskDetailAct? = null,
    val preferredSection: TaskDetailSection? = null,
    val historyLoading: Boolean = false,
    val historyError: String? = null,
    val historyExpanded: Boolean = false,
)

/** A section inside an act that a detail entry can ask to be scrolled to. */
enum class TaskDetailSection { Result }

data class MonitorDetailState(
    val taskId: String? = null,
    val loading: Boolean = false,
    val bundle: MonitorDetailBundle? = null,
    val error: String? = null,
    val highlightUpdateId: String? = null,
    val feedbackInFlight: Set<String> = emptySet(),
)

data class ExecutionControlUiState(
    val loading: Boolean = false,
    val state: ExecutionControlState? = null,
    val busy: ExecutionControlAction? = null,
    val error: String? = null,
)

data class TasksUiState(
    val completedReferences: List<TaskV3> = emptyList(),
    val referencesLoading: Boolean = false,
    val referencesError: String? = null,
    val referencesHasMore: Boolean = false,
    val threadContext: String? = null,
    val tasks: List<TaskV3> = emptyList(),
    val internalTasks: List<TaskV3> = emptyList(),
    val monitors: List<MonitorListItem> = emptyList(),
    val agents: List<AgentOption> = emptyList(),
    val agentsLoading: Boolean = false,
    val agentsError: String? = null,
    val lane: TaskLane = TaskLane.Tasks,
    val filter: TaskFilter = TaskFilter.All,
    val selectedTag: String? = null,
    val searchQuery: String = "",
    val sortField: TaskSortField = TaskSortField.Updated,
    val sortAscending: Boolean = false,
    val internalStatusFilter: String? = null,
    val internalAgentFilter: String? = null,
    val selectingInternal: Boolean = false,
    val selectedInternalIDs: Set<String> = emptySet(),
    val cleanupProgress: String? = null,
    val cleanupFailures: Map<String, String> = emptyMap(),
    val internalPageSize: Int = 50,
    val seenInternalAgents: Set<String> = emptySet(),
    val internalCrewChoices: List<AgentOption> = emptyList(),
    val monitorStateFilter: String? = null,
    val monitorPageSize: Int = 50,
    val taskLoadState: TaskLoadState = TaskLoadState.Idle,
    val internalLoadState: TaskLoadState = TaskLoadState.Idle,
    val monitorLoadState: TaskLoadState = TaskLoadState.Idle,
    val taskPageSize: Int = 50,
    val taskTotal: Int = 0,
    val taskHasMore: Boolean = false,
    val internalTotal: Int = 0,
    val internalHasMore: Boolean = false,
    val monitorTotal: Int? = null,
    val monitorNextCursor: String? = null,
    val monitorLoadingMore: Boolean = false,
    val laneCounts: Map<String, Int>? = null,
    val loadedView: TaskFilter? = null,
    val gracedRows: List<TaskV3> = emptyList(),
    val mutatingTaskId: String? = null,
    val loadErrors: Map<TaskLane, TaskUserError> = emptyMap(),
    val actionError: TaskUserError? = null,
    val actionNotice: String? = null,
    val retryingSynthesisId: String? = null,
    val publishingTaskId: String? = null,
    val detail: DetailState = DetailState(),
    val monitorDetail: MonitorDetailState = MonitorDetailState(),
    val executionControls: Map<String, ExecutionControlUiState> = emptyMap(),
) {
    val activeLoadState: TaskLoadState
        get() = when (lane) {
            TaskLane.Tasks -> taskLoadState
            TaskLane.Internal -> internalLoadState
            TaskLane.Monitors -> monitorLoadState
        }

    val activeLoadError: TaskUserError?
        get() = loadErrors[lane]

    val activeTasks: List<TaskV3>
        get() = when (lane) {
            TaskLane.Tasks -> tasks
            TaskLane.Internal -> internalTasks
            TaskLane.Monitors -> emptyList()
        }

    val availableTags: List<String>
        get() = activeTasks.flatMap { it.tags.map(TaskTag::name) }.filter(String::isNotBlank).distinct().sorted()

    val availableInternalAgents: List<String>
        get() = (seenInternalAgents + internalCrewChoices.map { it.id } + internalTasks.map(TaskV3::agentId) + listOfNotNull(internalAgentFilter)).filter(String::isNotBlank).distinct().sorted()

    fun laneCount(filter: TaskFilter): Int? =
        if (lane == TaskLane.Tasks && threadContext != null) tasks.count { it.uiThreadId.trim().lowercase() == threadContext && filter.matches(it, todayIso()) } +
            if (selectedTag == null && this.filter == filter && filter != TaskFilter.Completed) gracedRows.count { it.uiThreadId.trim().lowercase() == threadContext } else 0
        else if (lane == TaskLane.Tasks) laneCounts?.get(filter.wire)?.plus(
            if (selectedTag == null && this.filter == filter && filter != TaskFilter.Completed) gracedRows.size else 0,
        ) else null

    fun statusCount(vararg statuses: String): Int = activeTasks.count { it.status.lowercase() in statuses }

    fun visibleTasks(today: String = todayIso()): List<TaskV3> {
        var rows = when {
            // Internal Tasks mirrors the Web workspace's default "Any" status.
            // Regular task presets and tags belong to /tasks and must not hide
            // valid, commonly terminal rows returned by /tasks/internal.
            lane == TaskLane.Internal -> internalTasks
            selectedTag != null -> activeTasks.filter { task -> task.tags.any { it.name == selectedTag } }
            lane == TaskLane.Tasks -> {
                val serverRows = if (loadedView == filter) tasks else tasks.filter { filter.matches(it, today) }
                if (filter == TaskFilter.Completed) serverRows else (serverRows + gracedRows).distinctBy(TaskV3::id)
            }
            else -> emptyList()
        }
        if (threadContext != null && lane == TaskLane.Tasks) rows = rows.filter { it.uiThreadId.trim().lowercase() == threadContext }
        if (lane == TaskLane.Internal) {
            internalStatusFilter?.takeIf(String::isNotBlank)?.let { value -> rows = rows.filter { it.status == value } }
            internalAgentFilter?.takeIf(String::isNotBlank)?.let { value -> rows = rows.filter { it.agentId == value } }
        }
        val query = searchQuery.trim().lowercase()
        if (query.isNotEmpty()) rows = rows.filter { task ->
            task.title.lowercase().contains(query) || task.tags.any { it.name.lowercase().contains(query) }
        }
        val comparator = Comparator<TaskV3> { a, b ->
            when (sortField) {
                TaskSortField.Updated -> a.updatedAt.compareTo(b.updatedAt)
                TaskSortField.Created -> a.createdAt.compareTo(b.createdAt)
                TaskSortField.Title -> a.title.compareTo(b.title, ignoreCase = true)
                TaskSortField.Agent -> a.agentId.compareTo(b.agentId, ignoreCase = true)
                TaskSortField.Status -> a.status.compareTo(b.status)
            }
        }
        return rows.sortedWith(if (sortAscending) comparator else comparator.reversed())
    }
}

/** Android counterpart of iOS TasksViewModel, including the Monitors lane. */
class TasksViewModel private constructor(
    app: Application,
    private val source: TasksDataSource,
) : AndroidViewModel(app) {
    constructor(app: Application) : this(app, TaskRepository(app))
    override fun onCleared() { (source as? TaskRepository)?.close(); super.onCleared() }

    private val _state = MutableStateFlow(TasksUiState())
    val state: StateFlow<TasksUiState> = _state.asStateFlow()


    private var taskSpan = PAGE_SIZE
    private var internalSpan = PAGE_SIZE
    private var taskRequest: Job? = null
    private var agentsRequest: Job? = null
    private var referenceRequest: Job? = null
    private var referenceOffset = 0
    private var internalRequest: Job? = null
    private var monitorRequest: Job? = null
    private var detailRequest: Job? = null
    private var historyRequest: Job? = null
    private var monitorDetailRequest: Job? = null
    private var navigationRequest: Job? = null
    private var realtimeRefresh: Job? = null
    private val executionControlLoads = mutableMapOf<String, Job>()
    private val graceJobs = mutableMapOf<String, Job>()

    init {
        refresh()
        viewModelScope.launch {
            source.taskEvents()
                .catch { /* the repository reconnects; a socket outage never blanks the list */ }
                .collect { event ->
                    applyRealtimeDetail(event)
                    if (event.progress) return@collect
                    realtimeRefresh?.cancel()
                    realtimeRefresh = viewModelScope.launch { delay(500); refresh() }
                }
        }
    }

    fun setThreadContext(thread: String?) {
        val normalized = thread?.trim()?.lowercase()?.takeIf(String::isNotBlank)
        if (normalized == _state.value.threadContext) return
        taskRequest?.cancel(); graceJobs.values.forEach { it.cancel() }; graceJobs.clear()
        _state.value = _state.value.copy(threadContext = normalized, tasks = emptyList(), gracedRows = emptyList(), laneCounts = null, loadedView = null, selectedTag = null, taskHasMore = false)
        taskSpan = PAGE_SIZE; loadTaskLane(TaskLane.Tasks, false, PAGE_SIZE)
    }

    fun clearError() { _state.value = _state.value.copy(actionError = null) }
    fun clearNotice() { _state.value = _state.value.copy(actionNotice = null) }

    fun selectLane(lane: TaskLane) {
        _state.value = _state.value.copy(lane = lane)
        if (lane == TaskLane.Monitors && _state.value.monitorLoadState == TaskLoadState.Idle) loadMonitors(false)
    }

    fun setFilter(filter: TaskFilter) {
        if (_state.value.filter == filter && _state.value.selectedTag == null) return
        _state.value = _state.value.copy(filter = filter, selectedTag = null)
        taskSpan = PAGE_SIZE
        loadTaskLane(TaskLane.Tasks, append = false, limit = taskSpan)
    }

    fun toggleTag(tag: String) {
        _state.value = _state.value.copy(selectedTag = if (_state.value.selectedTag == tag) null else tag)
        taskSpan = PAGE_SIZE
        loadTaskLane(TaskLane.Tasks, append = false, limit = taskSpan)
    }

    fun setSearch(query: String) { if (_state.value.searchQuery == query) return; _state.value = _state.value.copy(searchQuery = query); if (_state.value.lane == TaskLane.Internal) resetInternalQuery() else if (_state.value.lane == TaskLane.Tasks) { taskSpan = _state.value.taskPageSize; loadTaskLane(TaskLane.Tasks, false, taskSpan) } }
    fun setTaskPageSize(value: Int) { _state.value = _state.value.copy(taskPageSize = value.coerceIn(1, 500)); taskSpan = _state.value.taskPageSize; loadTaskLane(TaskLane.Tasks, false, taskSpan) }

    fun setSort(field: TaskSortField) {
        val current = _state.value
        _state.value = if (current.sortField == field) current.copy(sortAscending = !current.sortAscending)
        else current.copy(sortField = field, sortAscending = false)
        if (current.lane == TaskLane.Internal) resetInternalQuery() else if (current.lane == TaskLane.Tasks) { taskSpan = current.taskPageSize; loadTaskLane(TaskLane.Tasks, false, taskSpan) }
    }

    fun setInternalStatus(value: String?) { _state.value = _state.value.copy(internalStatusFilter = value); resetInternalQuery() }
    fun setInternalAgent(value: String?) { _state.value = _state.value.copy(internalAgentFilter = value); resetInternalQuery() }
    fun toggleInternalSelectionMode() { if (_state.value.mutatingTaskId == null) _state.value = _state.value.copy(selectingInternal = !_state.value.selectingInternal, selectedInternalIDs = emptySet()) }
    fun toggleInternalSelection(task: TaskV3) { if (!task.canCleanUpInternal || _state.value.mutatingTaskId != null) return; val ids = _state.value.selectedInternalIDs; _state.value = _state.value.copy(selectedInternalIDs = if (task.id in ids) ids - task.id else ids + task.id) }
    fun selectLoadedInternal() { if (_state.value.mutatingTaskId == null) _state.value = _state.value.copy(selectedInternalIDs = _state.value.visibleTasks().filter { it.canCleanUpInternal }.map { it.id }.toSet()) }
    fun deleteSelectedInternal() {
        if (_state.value.mutatingTaskId != null) return
        val targets = _state.value.internalTasks.filter { it.id in _state.value.selectedInternalIDs && it.canCleanUpInternal }
        if (targets.isEmpty()) return
        _state.value = _state.value.copy(mutatingTaskId = "internal-cleanup", cleanupFailures = emptyMap(), actionError = null)
        viewModelScope.launch {
            try {
                var deleted = 0
                targets.forEachIndexed { index, task ->
                    _state.value = _state.value.copy(cleanupProgress = "Deleting ${index + 1} of ${targets.size}…")
                    runSuspendCatching { source.deleteTask(task, false) }.onSuccess {
                        deleted++; _state.value = _state.value.copy(selectedInternalIDs = _state.value.selectedInternalIDs - task.id, internalTasks = _state.value.internalTasks.filter { it.id != task.id })
                    }.onFailure { _state.value = _state.value.copy(cleanupFailures = _state.value.cleanupFailures + (task.id to (it.message ?: "Delete failed"))) }
                }
                _state.value = _state.value.copy(cleanupProgress = "Deleted $deleted of ${targets.size}. ${_state.value.cleanupFailures.size} failed.")
                refresh()
            } finally { _state.value = _state.value.copy(mutatingTaskId = null) }
        }
    }
    fun setInternalPageSize(value: Int) { _state.value = _state.value.copy(internalPageSize = value.coerceIn(1, 500)); resetInternalQuery() }
    private fun resetInternalQuery() { if (_state.value.mutatingTaskId == "internal-cleanup") return; internalSpan = _state.value.internalPageSize; _state.value = _state.value.copy(internalTasks = emptyList(), internalHasMore = false, selectedInternalIDs = emptySet(), cleanupFailures = emptyMap(), cleanupProgress = null); loadTaskLane(TaskLane.Internal, false, internalSpan) }

    fun setMonitorPageSize(value: Int) { _state.value = _state.value.copy(monitorPageSize = value.coerceIn(1, MAX_MONITOR_PAGE)); loadMonitors(false) }
    fun setMonitorState(value: String?) {
        _state.value = _state.value.copy(monitorStateFilter = value)
        loadMonitors(false)
    }

    fun loadReferences(append: Boolean = false) {
        if (append && (_state.value.referencesLoading || !_state.value.referencesHasMore)) return
        referenceRequest?.cancel()
        val offset = if (append) referenceOffset else 0
        _state.value = _state.value.copy(referencesLoading = true, referencesError = null)
        referenceRequest = viewModelScope.launch {
            runSuspendCatching { source.listTasks(TaskLane.Tasks, TaskFilter.Completed, todayIso(), offset, PAGE_SIZE) }
                .onSuccess { page ->
                    referenceOffset = offset + page.tasks.size
                    _state.value = _state.value.copy(completedReferences = ((if (append) _state.value.completedReferences else emptyList()) + page.tasks.filter { it.status == "completed" }).distinctBy { it.id }, referencesLoading = false, referencesHasMore = page.pagination?.hasMore == true && page.tasks.isNotEmpty())
                }.onFailure { _state.value = _state.value.copy(referencesLoading = false, referencesError = "Completed tasks could not be loaded. Retry.") }
        }
    }

    fun refreshAgents() {
        agentsRequest?.cancel()
        _state.value = _state.value.copy(agentsLoading = true, agentsError = null)
        agentsRequest = viewModelScope.launch {
            runSuspendCatching { source.agents() }
                .onSuccess { _state.value = _state.value.copy(agents = it, internalCrewChoices = source.knownCrewOptions(), agentsLoading = false, agentsError = null) }
                .onFailure { _state.value = _state.value.copy(agents = emptyList(), agentsLoading = false, agentsError = "Crew could not be loaded. Retry to choose a task owner.") }
        }
    }
    fun refresh() {
        refreshAgents()
        loadTaskLane(TaskLane.Tasks, append = false, limit = taskSpan)
        loadTaskLane(TaskLane.Internal, append = false, limit = internalSpan)
        if (_state.value.lane == TaskLane.Monitors || _state.value.monitorLoadState != TaskLoadState.Idle) {
            loadMonitors(false, preserveSpan = true)
        }
    }

    fun loadMore() {
        when (_state.value.lane) {
            TaskLane.Tasks -> if (_state.value.taskHasMore) loadTaskLane(TaskLane.Tasks, true, _state.value.taskPageSize)
            TaskLane.Internal -> if (_state.value.internalHasMore) loadTaskLane(TaskLane.Internal, true, _state.value.internalPageSize)
            TaskLane.Monitors -> if (_state.value.monitorNextCursor != null) loadMonitors(true)
        }
    }

    private fun loadTaskLane(lane: TaskLane, append: Boolean, limit: Int) {
        val currentJob = if (lane == TaskLane.Tasks) taskRequest else internalRequest
        if (!append) currentJob?.cancel()
        val job = viewModelScope.launch {
            setLoadState(lane, TaskLoadState.Loading)
            clearLoadError(lane)
            val current = _state.value
            val offset = if (append) {
                if (lane == TaskLane.Tasks) current.tasks.size else current.internalTasks.size
            } else 0
            val requestedFilter = if (lane == TaskLane.Tasks && current.selectedTag == null) current.filter else null
            runSuspendCatching {
                if (lane == TaskLane.Tasks && (current.threadContext != null || current.searchQuery.isNotBlank() || current.selectedTag != null || current.sortField != TaskSortField.Updated || current.sortAscending)) {
                    val rows = mutableListOf<TaskV3>(); var position = 0
                    while (true) {
                        val page = source.listTasks(lane, if (current.threadContext == null) requestedFilter else null, todayIso(), position, 500)
                        rows.addAll(page.tasks.filter { current.threadContext == null || it.uiThreadId.trim().lowercase() == current.threadContext })
                        if (page.tasks.isEmpty() || page.pagination?.hasMore != true) break
                        position += page.tasks.size
                        kotlinx.coroutines.currentCoroutineContext().ensureActive()
                    }
                    val scoped = rows.distinctBy(TaskV3::id)
                    TaskListResponse(scoped, TaskListPagination(scoped.size, scoped.size, 0, false))
                } else if (lane == TaskLane.Internal) source.listInternalTasks(InternalTaskQuery(offset, limit, current.sortField, current.sortAscending, current.internalAgentFilter, current.internalStatusFilter, current.searchQuery))
                else source.listTasks(lane, requestedFilter, todayIso(), offset, limit)
            }.onSuccess { response ->
                kotlinx.coroutines.currentCoroutineContext().ensureActive()
                val state = _state.value
                val existing = if (lane == TaskLane.Tasks) state.tasks else state.internalTasks
                val rows = if (append) (existing + response.tasks).distinctBy(TaskV3::id) else response.tasks
                if (lane == TaskLane.Tasks) {
                    taskSpan = rows.size.coerceAtLeast(state.taskPageSize)
                    _state.value = state.copy(
                        tasks = rows,
                        taskLoadState = TaskLoadState.Loaded,
                        taskTotal = response.pagination?.total ?: rows.size,
                        taskHasMore = response.pagination?.hasMore == true,
                        laneCounts = response.counts,
                        loadedView = if (response.counts == null) null else requestedFilter,
                        loadErrors = state.loadErrors - lane,
                    )
                } else {
                    internalSpan = rows.size.coerceAtLeast(state.internalPageSize)
                    _state.value = state.copy(
                        internalTasks = rows,
                        seenInternalAgents = state.seenInternalAgents + response.tasks.map { it.agentId },
                        internalLoadState = TaskLoadState.Loaded,
                        internalTotal = response.pagination?.total ?: rows.size,
                        internalHasMore = response.pagination?.hasMore == true,
                        loadErrors = state.loadErrors - lane,
                    )
                }
            }.onFailure { error ->
                setLoadFailure(
                    lane,
                    error.taskUserError(
                        fallbackTitle = if (lane == TaskLane.Internal) "Internal tasks unavailable" else "Tasks unavailable",
                        fallbackMessage = if (lane == TaskLane.Internal) {
                            "Internal tasks could not be loaded. Try again."
                        } else {
                            "Tasks could not be loaded. Try again."
                        },
                    ),
                )
            }
        }
        if (lane == TaskLane.Tasks) taskRequest = job else internalRequest = job
    }

    private fun loadMonitors(append: Boolean, preserveSpan: Boolean = false) {
        if (!append) monitorRequest?.cancel()
        monitorRequest = viewModelScope.launch {
            if (append) _state.value = _state.value.copy(
                monitorLoadingMore = true,
                loadErrors = _state.value.loadErrors - TaskLane.Monitors,
            ) else _state.value = _state.value.copy(
                monitorLoadState = TaskLoadState.Loading,
                loadErrors = _state.value.loadErrors - TaskLane.Monitors,
            )
            val state = _state.value
            val requested = if (preserveSpan) state.monitors.size.coerceAtLeast(state.monitorPageSize) else state.monitorPageSize
            runSuspendCatching {
                var page = source.listMonitors(limit = if (append) state.monitorPageSize else requested.coerceAtMost(MAX_MONITOR_PAGE), cursor = if (append) state.monitorNextCursor else null, state = state.monitorStateFilter)
                var rows = page.items; val cursors = mutableSetOf<String>()
                while (!append && requested > MAX_MONITOR_PAGE && rows.size < requested && page.nextCursor != null && page.items.isNotEmpty()) {
                    kotlinx.coroutines.currentCoroutineContext().ensureActive()
                    val cursor = page.nextCursor!!; check(cursors.add(cursor)) { "The server repeated its monitor cursor." }
                    page = source.listMonitors(limit = (requested - rows.size).coerceAtMost(MAX_MONITOR_PAGE), cursor = cursor, state = state.monitorStateFilter)
                    rows = (rows + page.items).distinctBy(MonitorListItem::taskId)
                }
                page.copy(items = rows)
            }.onSuccess { page ->
                val before = _state.value.monitors
                val items = if (append) (before + page.items).distinctBy(MonitorListItem::taskId) else page.items
                _state.value = _state.value.copy(
                    monitors = items,
                    monitorNextCursor = page.nextCursor,
                    monitorTotal = page.total,
                    monitorLoadState = TaskLoadState.Loaded,
                    monitorLoadingMore = false,
                    loadErrors = _state.value.loadErrors - TaskLane.Monitors,
                )
            }.onFailure { error ->
                _state.value = _state.value.copy(
                    monitorLoadState = TaskLoadState.Failed,
                    monitorLoadingMore = false,
                    loadErrors = _state.value.loadErrors + (
                        TaskLane.Monitors to error.taskUserError(
                            fallbackTitle = "Monitors unavailable",
                            fallbackMessage = "Monitors could not be loaded. Try again.",
                        )
                    ),
                )
            }
        }
    }

    fun createTask(draft: TaskCreateDraft, done: (Boolean) -> Unit = {}) = mutate(draft.title, "Task created.", done) {
        require(draft.title.trim().isNotEmpty()) { "Give the task a title." }
        source.createTask(draft)
    }

    fun execute(task: TaskV3) = mutate(task.id) { source.execute(task.id) }
    fun preplan(task: TaskV3) = mutate(task.id) { source.preplan(task.id) }

    fun setStatus(task: TaskV3, status: String) {
        if (status == "completed" && !task.canMarkCompleteManually) return
        if (status == "completed") grace(task.copy(status = "completed"))
        val notice = when (status) {
            "ready" -> "Task reset to ready."
            "completed" -> "Task marked complete."
            "cancelled" -> "Task cancelled."
            else -> "Task updated."
        }
        mutate(task.id, notice) { source.setStatus(task.id, status) }
    }

    fun deleteTask(task: TaskV3, removeFiles: Boolean, done: (Boolean) -> Unit = {}) =
        mutate(task.id, "Task deleted.", done) {
        check(!task.isAppManaged) { "This run is managed by its App. Open Apps to manage its lifecycle." }
        source.deleteTask(task, removeFiles)
    }

    fun updateTask(task: TaskV3, fields: JsonObject) = mutate(task.id, "Task updated.") {
        source.updateTask(task.id, fields)
    }

    fun updateTitle(task: TaskV3, title: String) = mutate(task.id, "Task title saved.") {
        require(title.isNotBlank()) { "Task title is required." }
        source.updateTask(task.id, buildJsonObject { put("title", title.trim()) })
    }
    fun moveTask(task: TaskV3, threadId: String, done: (Boolean) -> Unit = {}) = mutate(task.id, "Task moved.", done) {
        val target = source.taskThread(threadId)
        require(target.id == threadId && !target.archived) { "The destination thread is unavailable or archived. Refresh threads before moving this task." }
        source.updateTask(task.id, buildJsonObject { put("ui_thread_id", target.id) })
    }
    fun updateDescription(task: TaskV3, description: String) =
        updateTask(task, buildJsonObject { put("description", description) })

    fun updatePriority(task: TaskV3, priority: String?) = updateTask(task, buildJsonObject {
        put("priority", priority?.let(::JsonPrimitive) ?: JsonNull)
    })

    fun updateDueDate(task: TaskV3, dueDate: String?) = updateTask(task, buildJsonObject {
        put("due_date", dueDate?.let(::JsonPrimitive) ?: JsonNull)
    })

    fun addTag(task: TaskV3, name: String) {
        val clean = name.trim()
        if (clean.isEmpty() || task.tags.any { it.name == clean }) return
        updateTags(task, task.tags + TaskTag(clean, clean))
    }

    fun removeTag(task: TaskV3, name: String) = updateTags(task, task.tags.filterNot { it.name == name })

    private fun updateTags(task: TaskV3, tags: List<TaskTag>) = updateTask(task, buildJsonObject {
        putJsonArray("tags") { tags.forEach { tag ->
            add(buildJsonObject { put("id", tag.id); put("name", tag.name); tag.color?.let { put("color", it) } })
        } }
    })

    fun setSchedulePaused(task: TaskV3, paused: Boolean) = mutate(task.id, if (paused) "Schedule paused." else "Schedule resumed.") {
        val schedule = source.taskSchedule(task.id)
        require((schedule?.get("kind") as? JsonObject)?.get("Cron") is JsonObject) { "The task no longer has a recurring schedule. Refresh before trying again." }
        source.updateTask(task.id, buildJsonObject { put("schedule", JsonObject(schedule.orEmpty() + ("paused" to JsonPrimitive(paused)))) })
    }

    fun updateSchedule(task: TaskV3, cron: String, timezone: String, maxRecords: String, maxDays: String) {
        val expression = cron.trim()
        if (expression.isEmpty()) {
            updateTask(task, buildJsonObject { put("schedule", JsonNull) })
            return
        }
        if (expression.split(Regex("\\s+")).size != 5) {
            _state.value = _state.value.copy(actionError = TaskUserError(
                "Check the schedule",
                "Use a five-field cron expression, for example 0 9 * * *.",
            ))
            return
        }
        val records = maxRecords.trim().takeIf(String::isNotEmpty)?.toIntOrNull()
        val days = maxDays.trim().takeIf(String::isNotEmpty)?.toIntOrNull()
        if ((maxRecords.isNotBlank() && (records == null || records <= 0)) ||
            (maxDays.isNotBlank() && (days == null || days <= 0))) {
            _state.value = _state.value.copy(actionError = TaskUserError(
                "Check the retention",
                "Schedule retention values must be positive numbers.",
            ))
            return
        }
        val zone = timezone.trim().ifEmpty { ZoneId.systemDefault().id }
        val schedule = buildJsonObject {
            putJsonObject("kind") { putJsonObject("Cron") { put("expression", expression); put("timezone", zone) } }
            put("timezone", zone)
            if (records != null || days != null) putJsonObject("execution_history_retention") {
                records?.let { put("max_records", it) }; days?.let { put("max_age_days", it) }
            }
        }
        mutate(task.id, "Schedule saved.") {
            val previous = source.taskSchedule(task.id).orEmpty() - "execution_history_retention"
            source.updateTask(task.id, buildJsonObject { put("schedule", JsonObject(previous + schedule)) })
        }
    }

    fun approvePlan(task: TaskV3) = mutate(task.id, "Plan approved.") {
        source.planAction(task.id, task.latestPlanId, "approve")
    }
    fun rejectPlan(task: TaskV3) = mutate(task.id, "Plan rejected.") {
        source.planAction(task.id, task.latestPlanId, "reject")
    }
    fun replan(task: TaskV3) = mutate(task.id, "Replanning started.") {
        source.planAction(task.id, task.latestPlanId, "replan")
    }

    /**
     * A draft plan's Reject · Approve · Approve & Run from the task detail
     * (`/plan/approve|reject?plan_id=`, then `/execute`), with the shared
     * outcome wording ([PlanApprovalActions]). Approve & Run keeps the
     * approval when the start fails and says the run did not start.
     */
    fun decidePlan(task: TaskV3, choice: PlanApprovalChoice, done: (PlanApprovalOutcome) -> Unit = {}) {
        if (choice == PlanApprovalChoice.Review) return
        if (_state.value.mutatingTaskId != null) return
        viewModelScope.launch {
            _state.value = _state.value.copy(mutatingTaskId = task.id, actionError = null, actionNotice = null)
            val approve: suspend () -> PlanApprovalPost = {
                runSuspendCatching { source.planAction(task.id, task.latestPlanId, "approve") }
                    .fold({ PlanApprovalPost.Ok }, { PlanApprovalPost.Failed(it.message ?: "The plan was not approved.") })
            }
            val outcome = when (choice) {
                PlanApprovalChoice.ApproveAndRun ->
                    PlanApprovalActions.approveAndRun(task.id, approve) { id -> source.execute(id) }
                PlanApprovalChoice.Approve -> when (val posted = approve()) {
                    PlanApprovalPost.Ok -> PlanApprovalOutcome.Approved
                    is PlanApprovalPost.Failed -> PlanApprovalOutcome.Failed(posted.reason)
                }
                else -> runSuspendCatching { source.planAction(task.id, task.latestPlanId, "reject") }
                    .fold({ PlanApprovalOutcome.Rejected }, { PlanApprovalOutcome.Failed(it.message ?: "The plan was not rejected.") })
            }
            _state.value = _state.value.copy(mutatingTaskId = null)
            if (outcome is PlanApprovalOutcome.Failed) {
                _state.value = _state.value.copy(actionError = TaskUserError("Plan decision failed", outcome.reason))
            } else {
                showNotice(outcome.message)
            }
            done(outcome)
            refresh()
            refreshOpenTaskDetail(task.id)
        }
    }

    /** The open detail's live poll: refetch while the screen is visible and the task is live. */
    fun refreshTaskDetail() {
        refreshOpenTaskDetail(_state.value.detail.taskId)
    }

    /** Open a task by id from outside the Tasks list (chat, Attention, a notification). */
    fun openTaskById(
        taskId: String,
        executionId: String? = null,
        preferredAct: TaskDetailAct? = null,
    ) {
        val id = taskId.trim()
        if (id.isEmpty()) return
        val seed = (_state.value.internalTasks + _state.value.tasks).firstOrNull { it.id == id }
            ?: TaskV3(id = id, title = "Task")
        openTask(seed, executionId ?: seed.latestRootExecutionId, preferredAct = preferredAct)
    }

    fun retrySynthesis(task: TaskV3) {
        if (task.isAppManaged || _state.value.mutatingTaskId != null) return
        val execution = task.synthesisFailedExecutionId?.trim()?.takeIf(String::isNotEmpty) ?: return
        if (_state.value.retryingSynthesisId != null) return
        viewModelScope.launch {
            _state.value = _state.value.copy(retryingSynthesisId = task.id, actionError = null)
            runSuspendCatching { source.retrySynthesis(task.id, execution) }
                .onFailure { _state.value = _state.value.copy(actionError = it.taskUserError(
                    "Couldn’t retry synthesis",
                    "Synthesis could not be retried. Try again.",
                )) }
            _state.value = _state.value.copy(retryingSynthesisId = null)
            refresh()
        }
    }

    fun publishToNotes(task: TaskV3) {
        if (!task.canPublishToNotes || _state.value.publishingTaskId != null) return
        viewModelScope.launch {
            _state.value = _state.value.copy(publishingTaskId = task.id, actionError = null)
            runSuspendCatching { source.publishToNotes(task.id) }
                .onSuccess { showNotice("Published to Notes.") }
                .onFailure { _state.value = _state.value.copy(actionError = it.taskUserError(
                    "Couldn’t publish to Notes",
                    "The task was not published. Try again.",
                )) }
            _state.value = _state.value.copy(publishingTaskId = null)
        }
    }

    fun loadExecutionControls(executionId: String, keepError: Boolean = false) {
        val existing = _state.value.executionControls[executionId]
        if (existing?.loading == true || existing?.busy != null) return
        setControl(executionId, (existing ?: ExecutionControlUiState()).let {
            it.copy(loading = true, error = if (keepError) it.error else null)
        })
        val load = viewModelScope.launch {
            runSuspendCatching { source.executionControlState(executionId) }
                .onSuccess { setControl(executionId, (_state.value.executionControls[executionId] ?: ExecutionControlUiState()).copy(loading = false, state = it)) }
                .onFailure { setControl(executionId, (_state.value.executionControls[executionId] ?: ExecutionControlUiState()).copy(loading = false, error = it.userMessage(
                    "Execution controls unavailable",
                    "Controls could not be loaded. Try again.",
                ))) }
        }
        executionControlLoads[executionId] = load
        // Runs at once if the load already finished, so no entry outlives its job.
        load.invokeOnCompletion { executionControlLoads.remove(executionId, load) }
    }

    fun executionControl(executionId: String, action: ExecutionControlAction, guidance: String? = null) {
        val trimmed = guidance?.trim().orEmpty()
        if (action == ExecutionControlAction.Steer && (trimmed.isEmpty() || trimmed.toByteArray().size > 4_096)) {
            setControl(executionId, (_state.value.executionControls[executionId] ?: ExecutionControlUiState()).copy(
                error = if (trimmed.isEmpty()) "Steer guidance cannot be empty." else "Steer guidance must be 4,096 bytes or less.",
            ))
            return
        }
        val current = _state.value.executionControls[executionId] ?: ExecutionControlUiState()
        if (current.busy != null) return
        // Reserve before launching so two taps in the same frame cannot submit twice.
        setControl(executionId, current.copy(busy = action, error = null))
        viewModelScope.launch {
            runSuspendCatching { source.executionControl(executionId, action, trimmed.ifEmpty { null }) }
                .onFailure {
                    setControl(executionId, (_state.value.executionControls[executionId] ?: current).copy(busy = null, error = it.userMessage(
                        "Execution action failed",
                        "The execution was not changed. Try again.",
                    )))
                    // A refused action usually means the run moved on; show what it allows now.
                    loadExecutionControls(executionId, keepError = true)
                }
                .onSuccess {
                    // Loading refuses a busy control; release the completed mutation first.
                    executionControlLoads.remove(executionId)?.cancel()
                    setControl(executionId, (_state.value.executionControls[executionId] ?: current).copy(loading = false, busy = null, error = null))
                    loadExecutionControls(executionId)
                    refresh()
                }
        }
    }

    private fun setControl(id: String, value: ExecutionControlUiState) {
        _state.value = _state.value.copy(executionControls = _state.value.executionControls + (id to value))
    }

    fun loadOlderDetailExecutions() {
        val current = _state.value.detail
        val taskId = current.taskId ?: return
        val cursor = (current.bundle?.details?.get("next_execution_cursor") as? JsonPrimitive)?.content?.takeIf(String::isNotBlank) ?: return
        if (current.historyLoading) return
        _state.value = _state.value.copy(detail = current.copy(historyLoading = true, historyError = null))
        historyRequest = viewModelScope.launch {
            runSuspendCatching { source.taskDetailsPage(taskId, cursor) }.onSuccess { page ->
                val detail = _state.value.detail
                if (detail.taskId != taskId || detail.selectedExecutionId != current.selectedExecutionId) return@onSuccess
                var merged = mergeTaskExecutionHistory(detail.bundle?.details, page)
                val repeated = (page["next_execution_cursor"] as? JsonPrimitive)?.content == cursor
                if (repeated) merged = JsonObject(merged - "next_execution_cursor")
                _state.value = _state.value.copy(detail = detail.copy(bundle = detail.bundle?.copy(details = merged), historyLoading = false, historyExpanded = true, historyError = if (repeated) "The server repeated its history cursor. Refresh to retry." else null))
            }.onFailure { if (_state.value.detail.taskId == taskId) _state.value = _state.value.copy(detail = _state.value.detail.copy(historyLoading = false, historyError = "Older runs could not be loaded. Retry.")) }
        }
    }
    fun openTask(
        task: TaskV3,
        executionId: String? = task.latestRootExecutionId,
        preferredAct: TaskDetailAct? = null,
        preferredSection: TaskDetailSection? = null,
    ) {
        historyRequest?.cancel()
        detailRequest?.cancel()
        val previous = _state.value.detail.takeIf { it.taskId == task.id && it.historyExpanded }
        _state.value = _state.value.copy(
            detail = DetailState(
                task.id,
                loading = true,
                selectedExecutionId = executionId,
                preferredAct = preferredAct,
                preferredSection = preferredSection,
            ),
        )
        detailRequest = viewModelScope.launch {
            runSuspendCatching { source.taskDetail(task.id, executionId) }
                .onSuccess {
                    if (_state.value.detail.taskId == task.id &&
                        _state.value.detail.selectedExecutionId == executionId) {
                        val resolvedExecution = executionId ?: it.selectedExecutionId()
                        _state.value = _state.value.copy(
                            detail = DetailState(
                                task.id,
                                bundle = (if (previous != null && it.details != null) it.copy(details = JsonObject(mergeTaskExecutionHistory(previous.bundle?.details, it.details).filterKeys { key -> key != "next_execution_cursor" } + (previous.bundle?.details?.get("next_execution_cursor")?.let { cursor -> mapOf("next_execution_cursor" to cursor) } ?: emptyMap()))) else it).withTaskProgress(previous?.bundle),
                                historyExpanded = previous != null,
                                selectedExecutionId = resolvedExecution,
                                preferredAct = preferredAct,
                                preferredSection = preferredSection,
                            ),
                        )
                    }
                }
                .onFailure {
                    if (_state.value.detail.taskId == task.id &&
                        _state.value.detail.selectedExecutionId == executionId) {
                        _state.value = _state.value.copy(
                            detail = DetailState(
                                task.id,
                                error = it.userMessage(
                                    "Task details unavailable",
                                    "Task details could not be loaded. Try again.",
                                ),
                                selectedExecutionId = executionId,
                                preferredAct = preferredAct,
                                preferredSection = preferredSection,
                            ),
                        )
                    }
                }
        }
    }

    /** The card's "Result": task detail opened on the Output act, at the Result card. */
    fun openTaskResult(task: TaskV3) =
        openTask(task, preferredAct = TaskDetailAct.Output, preferredSection = TaskDetailSection.Result)

    // Keep the act the reader is on (e.g. Output after Result) when switching runs.
    fun selectDetailExecution(task: TaskV3, executionId: String) =
        openTask(task, executionId, preferredAct = _state.value.detail.preferredAct)
    fun closeTaskDetail() {
        historyRequest?.cancel()
        detailRequest?.cancel()
        detailRequest = null
        _state.value = _state.value.copy(detail = DetailState())
    }

    /**
     * `magican://task/{id}` follows iOS's fallback contract: a monitor probe wins;
     * otherwise the shared task detail opens even when that row was outside the
     * currently loaded pages. The pending link is consumed by the UI exactly once.
     */
    fun openTaskLink(taskId: String, preferredAct: TaskDetailAct? = null, executionId: String? = null) {
        val id = taskId.trim()
        if (id.isEmpty()) return
        navigationRequest?.cancel()
        navigationRequest = viewModelScope.launch {
            val monitor = runSuspendCatching { source.monitorIdentity(id) }.getOrNull()
            if (monitor != null) {
                _state.value = _state.value.copy(
                    lane = TaskLane.Monitors,
                    monitorDetail = MonitorDetailState(id, loading = true),
                )
                openMonitor(id)
                return@launch
            }
            val seed = (_state.value.internalTasks + _state.value.tasks)
                .firstOrNull { it.id == id } ?: TaskV3(id = id, title = "Task")
            openTask(seed, executionId = executionId ?: seed.latestRootExecutionId, preferredAct = preferredAct)
        }
    }

    fun openMonitor(taskId: String, highlightUpdateId: String? = null) {
        monitorDetailRequest?.cancel()
        val highlight = highlightUpdateId ?: _state.value.monitorDetail
            .takeIf { it.taskId == taskId }?.highlightUpdateId
        _state.value = _state.value.copy(monitorDetail = MonitorDetailState(taskId, loading = true, highlightUpdateId = highlight))
        monitorDetailRequest = viewModelScope.launch {
            runSuspendCatching { source.monitorDetail(taskId) }
                .onSuccess {
                    if (_state.value.monitorDetail.taskId == taskId) {
                        _state.value = _state.value.copy(
                            monitorDetail = MonitorDetailState(taskId, bundle = it, highlightUpdateId = highlight),
                        )
                    }
                }
                .onFailure {
                    if (_state.value.monitorDetail.taskId == taskId) {
                        _state.value = _state.value.copy(
                            monitorDetail = MonitorDetailState(
                                taskId,
                                error = it.userMessage(
                                    "Monitor details unavailable",
                                    "Monitor details could not be loaded. Try again.",
                                ),
                                highlightUpdateId = highlight,
                            ),
                        )
                    }
                }
        }
    }

    fun closeMonitorDetail() {
        monitorDetailRequest?.cancel()
        monitorDetailRequest = null
        _state.value = _state.value.copy(monitorDetail = MonitorDetailState())
    }

    fun createMonitor(draft: MonitorDraft, done: (Boolean) -> Unit = {}) = mutate("new-monitor", "Monitor created.", done) {
        source.createMonitor(draft)
    }

    fun convertToMonitor(task: TaskV3, draft: MonitorDraft, done: (Boolean) -> Unit = {}) =
        mutate(task.id, "Task converted to a monitor.", done) { source.convertToMonitor(task.id, draft) }

    fun updateMonitor(taskId: String, draft: MonitorDraft, done: (Boolean) -> Unit = {}) =
        mutate(taskId, "Monitor updated.", done) { source.updateMonitor(taskId, draft) }

    fun monitorAction(taskId: String, action: String) = mutate(
        taskId,
        when (action) {
            "run" -> "Monitor run started."
            "pause" -> "Monitor paused."
            "resume" -> "Monitor resumed."
            else -> null
        },
    ) {
        source.monitorAction(taskId, action)
    }

    fun deleteMonitor(taskId: String, removeFiles: Boolean, done: (Boolean) -> Unit = {}) =
        mutate(taskId, "Monitor deleted.", done) {
            source.deleteMonitor(taskId, removeFiles)
            closeMonitorDetail()
        }

    fun monitorFeedback(taskId: String, updateId: String, verdict: String) {
        val detail = _state.value.monitorDetail
        if (detail.taskId != taskId || updateId in detail.feedbackInFlight) return
        val previous = detail.bundle?.feedbackByUpdate?.get(updateId)
        val optimisticBundle = detail.bundle?.copy(
            feedbackByUpdate = detail.bundle.feedbackByUpdate + (updateId to verdict),
        )
        _state.value = _state.value.copy(monitorDetail = detail.copy(
            bundle = optimisticBundle,
            feedbackInFlight = detail.feedbackInFlight + updateId,
        ))
        viewModelScope.launch {
            runSuspendCatching { source.monitorFeedback(taskId, updateId, verdict) }
                .onSuccess {
                    val current = _state.value.monitorDetail
                    _state.value = _state.value.copy(monitorDetail = current.copy(
                        feedbackInFlight = current.feedbackInFlight - updateId,
                    ))
                }
                .onFailure { error ->
                    val current = _state.value.monitorDetail
                    val feedback = current.bundle?.feedbackByUpdate.orEmpty().toMutableMap()
                    if (previous == null) feedback.remove(updateId) else feedback[updateId] = previous
                    _state.value = _state.value.copy(
                        monitorDetail = current.copy(
                            bundle = current.bundle?.copy(feedbackByUpdate = feedback),
                            feedbackInFlight = current.feedbackInFlight - updateId,
                        ),
                        actionError = error.taskUserError(
                            "Couldn’t save feedback",
                            "Monitor feedback was not saved. Try again.",
                        ),
                    )
                }
        }
    }

    private fun mutate(
        key: String,
        notice: String? = null,
        done: (Boolean) -> Unit = {},
        block: suspend () -> Any?,
    ): Job {
        if (_state.value.mutatingTaskId != null) {
            _state.value = _state.value.copy(actionError = TaskUserError(
                "Task action in progress",
                "Another task action is still finishing. Try again in a moment.",
            ))
            done(false)
            return viewModelScope.launch { }
        }
        return viewModelScope.launch {
            _state.value = _state.value.copy(mutatingTaskId = key, actionError = null, actionNotice = null)
            runSuspendCatching { block() }
                .onSuccess {
                    _state.value = _state.value.copy(mutatingTaskId = null)
                    notice?.let(::showNotice)
                    refresh()
                    refreshOpenTaskDetail(key)
                    refreshOpenMonitorDetail(key)
                    done(true)
                    delay(1_600)
                    refresh()
                    refreshOpenTaskDetail(key)
                    refreshOpenMonitorDetail(key)
                }
                .onFailure {
                    _state.value = _state.value.copy(
                        mutatingTaskId = null,
                        actionError = it.taskUserError(
                            "Task action failed",
                            "The task was not changed. Try again.",
                        ),
                    )
                    refresh()
                    done(false)
                }
        }
    }

    private fun showNotice(message: String) {
        _state.value = _state.value.copy(actionNotice = message)
        viewModelScope.launch {
            delay(2_600)
            if (_state.value.actionNotice == message) _state.value = _state.value.copy(actionNotice = null)
        }
    }

    private fun applyRealtimeDetail(event: TaskRealtimeEvent) {
        val detail = _state.value.detail
        if (event.resync) {
            // Events may have been missed; whatever is open re-reads. The list
            // refresh that follows every event covers the lanes.
            refreshOpenTaskDetail(detail.taskId)
            refreshOpenMonitorDetail(_state.value.monitorDetail.taskId)
            return
        }
        if (event.progress) {
            val current = detail.bundle ?: return
            val bundle = if (current.progressExecutionId != null) current else current.withTaskProgress()
            if (bundle.progressExecutionId != null && event.executionId == bundle.progressExecutionId &&
                event.eventTimestamp - (bundle.observedProgressAt ?: 0) >= 1000) {
                _state.value = _state.value.copy(detail = detail.copy(bundle = bundle.copy(
                    observedProgressAt = TaskRunEvidence.latest(bundle.observedProgressAt, event.eventTimestamp))))
            }
            return
        }
        if (event.eventType == "ExecutionPanelDelta" && event.panel != null &&
            detail.selectedExecutionId != null && event.executionId == detail.selectedExecutionId) {
            val bundle = detail.bundle ?: return
            _state.value = _state.value.copy(detail = detail.copy(
                bundle = bundle.copy(
                    panel = event.panel,
                    unavailableSections = bundle.unavailableSections - "live run",
                ).withTaskProgress(bundle),
            ))
            return
        }
        if (event.taskId == detail.taskId && event.eventType != "ExecutionPanelDelta") {
            refreshOpenTaskDetail(event.taskId)
        }
        val monitor = _state.value.monitorDetail
        if (event.eventType.contains("Monitor") && event.taskId == monitor.taskId) {
            refreshOpenMonitorDetail(event.taskId)
        }
    }

    private fun refreshOpenTaskDetail(taskId: String?) {
        val id = taskId ?: return
        val current = _state.value.detail
        if (current.taskId != id || current.loading || detailRequest?.isActive == true) return
        val selected = current.selectedExecutionId
        detailRequest = viewModelScope.launch {
            runSuspendCatching { source.taskDetail(id, selected) }.onSuccess { bundle ->
                val latest = _state.value.detail
                if (latest.taskId == id && latest.selectedExecutionId == selected) {
                    _state.value = _state.value.copy(detail = latest.copy(bundle = (if (latest.historyExpanded && bundle.details != null) bundle.copy(details = JsonObject(mergeTaskExecutionHistory(latest.bundle?.details, bundle.details).filterKeys { it != "next_execution_cursor" } + (latest.bundle?.details?.get("next_execution_cursor")?.let { mapOf("next_execution_cursor" to it) } ?: emptyMap()))) else bundle).withTaskProgress(latest.bundle), error = null))
                }
            }
        }
    }

    private fun refreshOpenMonitorDetail(taskId: String?) {
        val id = taskId ?: return
        val current = _state.value.monitorDetail
        if (current.taskId != id || current.loading || monitorDetailRequest?.isActive == true) return
        monitorDetailRequest = viewModelScope.launch {
            runSuspendCatching { source.monitorDetail(id) }.onSuccess { bundle ->
                val latest = _state.value.monitorDetail
                if (latest.taskId == id) {
                    _state.value = _state.value.copy(
                        monitorDetail = latest.copy(bundle = bundle, error = null),
                    )
                }
            }
        }
    }

    private fun grace(task: TaskV3) {
        val rows = (_state.value.gracedRows + task).distinctBy(TaskV3::id)
        _state.value = _state.value.copy(gracedRows = rows)
        graceJobs.remove(task.id)?.cancel()
        graceJobs[task.id] = viewModelScope.launch {
            delay(5_000)
            _state.value = _state.value.copy(gracedRows = _state.value.gracedRows.filterNot { it.id == task.id })
            graceJobs.remove(task.id)
        }
    }

    private fun setLoadState(lane: TaskLane, value: TaskLoadState) {
        _state.value = when (lane) {
            TaskLane.Tasks -> _state.value.copy(taskLoadState = value)
            TaskLane.Internal -> _state.value.copy(internalLoadState = value)
            TaskLane.Monitors -> _state.value.copy(monitorLoadState = value)
        }
    }

    private fun clearLoadError(lane: TaskLane) {
        _state.value = _state.value.copy(loadErrors = _state.value.loadErrors - lane)
    }

    private fun setLoadFailure(lane: TaskLane, error: TaskUserError) {
        val state = _state.value
        _state.value = when (lane) {
            TaskLane.Tasks -> state.copy(
                taskLoadState = TaskLoadState.Failed,
                loadErrors = state.loadErrors + (lane to error),
            )
            TaskLane.Internal -> state.copy(
                internalLoadState = TaskLoadState.Failed,
                loadErrors = state.loadErrors + (lane to error),
            )
            TaskLane.Monitors -> state.copy(
                monitorLoadState = TaskLoadState.Failed,
                loadErrors = state.loadErrors + (lane to error),
            )
        }
    }

    companion object {
        /** Injectable task boundary for deterministic UI qualification. */
        fun usingDataSource(app: Application, source: TasksDataSource) = TasksViewModel(app, source)
        const val PAGE_SIZE = 50
        const val MAX_MONITOR_PAGE = 200
    }
}

private suspend inline fun <T> runSuspendCatching(crossinline block: suspend () -> T): Result<T> =
    try {
        Result.success(block())
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (error: Throwable) {
        Result.failure(error)
    }

internal fun Throwable.taskUserError(
    fallbackTitle: String,
    fallbackMessage: String,
): TaskUserError {
    val causes = mutableListOf<Throwable>()
    val seen = Collections.newSetFromMap(IdentityHashMap<Throwable, Boolean>())
    var current: Throwable? = this
    while (current != null && seen.add(current)) {
        causes += current
        current = current.cause
    }
    val api = causes.filterIsInstance<TaskApiError>().firstOrNull()
    if (api?.message == "No Magician host configured yet.") {
        return TaskUserError(
            "Connect to Magician",
            "Set the Magician address in Settings, then try again.",
        )
    }
    if (causes.any(Throwable::isTimeoutFailure)) {
        return TaskUserError(
            "Magician took too long",
            "The request timed out. Check the connection and try again.",
        )
    }
    // Deliberately not `Failures.offline()`. These exceptions mean a connection
    // could not be established, which is either end — and this path has not
    // checked the radio, so it must not assert that the phone is the one at
    // fault. The copy names both possibilities on purpose.
    if (causes.any(Throwable::isOfflineFailure)) {
        return TaskUserError(
            "Magician is offline",
            "Start Magician or check this device’s connection, then try again.",
        )
    }
    when (api?.status) {
        401, 403 -> return TaskUserError.of(Failures.ofStatus(api.status, "your tasks"))
        408, 504 -> return TaskUserError(
            "Magician took too long",
            "The request timed out. Check the connection and try again.",
        )
        409 -> return TaskUserError(
            "This task changed",
            "Refresh the task before trying that action again.",
        )
        429 -> return TaskUserError(
            "Magician is busy",
            "Wait a moment, then try again.",
        )
    }
    // Deferred to the shared classifier so a stopped server reads the same
    // here as it does on Attention or in Chat. It used to say "Magician is
    // unavailable / try again shortly", which is true of a 500 and misleading
    // for a 502 — where the fix is to start the server, not to wait.
    if ((api?.status ?: 0) >= 500) {
        return TaskUserError.of(Failures.ofStatus(api!!.status, "your tasks"))
    }
    if (api != null && api.status in 400..499) {
        return TaskUserError(
            fallbackTitle,
            api.message?.takeIf(String::isNotBlank) ?: fallbackMessage,
        )
    }
    if (api != null && api.status == 0) {
        return TaskUserError(
            fallbackTitle,
            api.message?.takeIf(String::isNotBlank) ?: fallbackMessage,
        )
    }
    if (causes.any { it::class.simpleName == "SerializationException" }) {
        return TaskUserError(
            "Magician sent an unreadable response",
            "Refresh and try again. If this keeps happening, update Magdroid and Magician together.",
        )
    }
    return TaskUserError(fallbackTitle, fallbackMessage)
}

private fun Throwable.isOfflineFailure(): Boolean =
    this is ConnectException ||
        this is UnknownHostException ||
        this is NoRouteToHostException ||
        this is SocketException ||
        this::class.simpleName in setOf(
            "UnresolvedAddressException",
            "ClosedChannelException",
        )

private fun Throwable.isTimeoutFailure(): Boolean =
    this is SocketTimeoutException ||
        this::class.simpleName in setOf(
            "ConnectTimeoutException",
            "HttpRequestTimeoutException",
            "SocketTimeoutException",
        )

private fun Throwable.userMessage(fallbackTitle: String, fallbackMessage: String): String =
    taskUserError(fallbackTitle, fallbackMessage).inlineMessage

private fun TaskDetailBundle.selectedExecutionId(): String? {
    val overview = panel?.get("overview") as? JsonObject
    val debug = panel?.get("debug") as? JsonObject
    val selected = debug?.get("selected_execution") as? JsonObject
    return (selected?.get("execution_id") as? JsonPrimitive)?.content
        ?.takeIf(String::isNotBlank)
        ?: (overview?.get("execution_id") as? JsonPrimitive)?.content?.takeIf(String::isNotBlank)
}
