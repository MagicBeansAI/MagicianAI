@file:OptIn(androidx.compose.foundation.layout.ExperimentalLayoutApi::class)

package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tasks.ExecutionControlAction
import ai.magicbeans.magdroid.tasks.TaskCardAction
import ai.magicbeans.magdroid.tasks.TaskCreateDraft
import ai.magicbeans.magdroid.tasks.TaskFilter
import ai.magicbeans.magdroid.tasks.TaskLane
import ai.magicbeans.magdroid.tasks.TaskLoadState
import ai.magicbeans.magdroid.tasks.TaskSortField
import ai.magicbeans.magdroid.tasks.TaskSwipeAction
import ai.magicbeans.magdroid.tasks.TaskTag
import ai.magicbeans.magdroid.tasks.TaskV3
import ai.magicbeans.magdroid.tasks.TasksUiState
import ai.magicbeans.magdroid.tasks.TaskUserError
import ai.magicbeans.magdroid.tasks.TasksViewModel
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.Menu
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.Article
import androidx.compose.material.icons.outlined.ArrowDropDown
import androidx.compose.material.icons.outlined.Cancel
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material.icons.outlined.EditCalendar
import androidx.compose.material.icons.outlined.FilterList
import androidx.compose.material.icons.outlined.Person
import androidx.compose.material.icons.outlined.PlayArrow
import androidx.compose.material.icons.outlined.RestartAlt
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.StopCircle
import androidx.compose.material.icons.outlined.Tag
import androidx.compose.material.icons.outlined.Tune
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import java.time.LocalDate
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle

private val Success: Color get() = activePalette.success
private val Warning: Color get() = activePalette.warning
private val Info: Color get() = activePalette.info
private val Discovery: Color get() = activePalette.discovery

internal const val TASK_HEADER_HEIGHT_DP = 52
internal const val TASK_HEADER_TITLE_FONT_SP = 15
internal const val TASK_SEARCH_HEIGHT_DP = 42
internal const val TASK_LANE_VERTICAL_PADDING_DP = 3
internal const val TASK_LANE_ITEM_VERTICAL_PADDING_DP = 3
internal const val TASK_LANE_FONT_SP = 10
internal const val TASK_CHIP_HORIZONTAL_PADDING_DP = 7
internal const val TASK_CHIP_VERTICAL_PADDING_DP = 4
internal const val TASK_CHIP_FONT_SP = 11
internal const val TASK_CHIP_LINE_HEIGHT_SP = 13
internal const val TASK_CHIP_ICON_DP = 13
internal const val TASK_CHIP_COUNT_FONT_SP = 9
internal const val TASK_LEDGER_HORIZONTAL_PADDING_DP = 6
internal const val TASK_LEDGER_VERTICAL_PADDING_DP = 1
internal const val TASK_LEDGER_FONT_SP = 9
internal const val TASK_LEDGER_LINE_HEIGHT_SP = 10
internal const val TASK_EMPTY_PROGRESS_DP = 22
internal const val TASK_EMPTY_ICON_DP = 24
internal const val TASK_EMPTY_TITLE_FONT_SP = 14
internal const val TASK_EMPTY_BODY_FONT_SP = 11
internal const val TASK_EMPTY_ACTION_HEIGHT_DP = 36
internal const val TASK_CARD_PADDING_DP = 10
internal const val TASK_CARD_SPACING_DP = 3
internal const val TASK_CARD_TITLE_FONT_SP = 14
internal const val TASK_CARD_TITLE_LINE_HEIGHT_SP = 16
internal const val TASK_CARD_BODY_FONT_SP = 12
internal const val TASK_CARD_BODY_LINE_HEIGHT_SP = 14
internal const val TASK_ACTION_HORIZONTAL_PADDING_DP = 7
internal const val TASK_ACTION_VERTICAL_PADDING_DP = 4
internal const val TASK_ACTION_SPACING_DP = 3
internal const val TASK_ACTION_FONT_SP = 10
internal const val TASK_ACTION_LINE_HEIGHT_SP = 13
internal const val TASK_ACTION_ICON_DP = 13
internal const val TASK_ACTION_CORNER_DP = 5
internal const val TASK_STATUS_HORIZONTAL_PADDING_DP = 6
internal const val TASK_STATUS_VERTICAL_PADDING_DP = 1
internal const val TASK_STATUS_FONT_SP = 9
internal const val TASK_STATUS_LINE_HEIGHT_SP = 10
internal const val TASK_STATUS_CORNER_DP = 4

internal fun taskWorkspaceColor(palette: Palette): Color = palette.background
internal fun taskCardColor(palette: Palette): Color = palette.card
internal fun taskCardBorderColor(palette: Palette): Color = palette.cardBorder
internal fun taskControlColor(palette: Palette): Color = palette.control

/**
 * Small state holder shared by the app bar and the Tasks workspace.
 *
 * The shell owns the real app bar, while the lane owns which composer the add
 * action opens and how refresh is performed. Keeping that bridge explicit lets
 * the actions live in the header without eagerly constructing TasksViewModel
 * (and starting its network refresh) on every app launch.
 */
@Stable
class TasksScreenActions {
    var canCreate by mutableStateOf(true)
        internal set
    var isCreateVisible by mutableStateOf(false)
        private set
    var isDetailVisible by mutableStateOf(false)
        private set

    private var refreshAction: (() -> Unit)? = null

    fun showCreate() {
        if (canCreate) isCreateVisible = true
    }

    fun hideCreate() {
        isCreateVisible = false
    }

    fun refresh() {
        refreshAction?.invoke()
    }

    internal fun bindRefresh(action: (() -> Unit)?) {
        refreshAction = action
    }

    internal fun showDetail(visible: Boolean) {
        isDetailVisible = visible
    }
}

@Composable
fun rememberTasksScreenActions(): TasksScreenActions = remember { TasksScreenActions() }

/** Keep the 52dp toolbar below the status inset, with every control centered. */
@Composable
internal fun TasksTopBar(onMenu: () -> Unit, onCreate: () -> Unit, onRefresh: () -> Unit, canCreate: Boolean = true) {
    Row(
        Modifier.fillMaxWidth().background(Ground).statusBarsPadding().height(TASK_HEADER_HEIGHT_DP.dp)
            .padding(horizontal = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        IconButton(onClick = onMenu) { Icon(Icons.Outlined.Menu, "Open navigation", tint = Ink) }
        Text("Work", color = Ink, fontSize = TASK_HEADER_TITLE_FONT_SP.sp,
            fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
        IconButton(onClick = onCreate, enabled = canCreate) { Icon(Icons.Outlined.Add, "New work", tint = if (canCreate) Coral else Muted) }
        IconButton(onClick = onRefresh) { Icon(Icons.Outlined.Refresh, "Refresh tasks", tint = Coral) }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun TasksScreen(
    viewModel: TasksViewModel = viewModel(),
    actions: TasksScreenActions = rememberTasksScreenActions(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    var actionTask by remember { mutableStateOf<TaskV3?>(null) }
    var destructive by remember { mutableStateOf<Pair<TaskV3, TaskSwipeAction>?>(null) }
    var convertTask by remember { mutableStateOf<TaskV3?>(null) }
    val deepLink by TaskDeepLinks.target.collectAsStateWithLifecycle()
    val taskListState = rememberLazyListState()

    DisposableEffect(viewModel, actions) {
        actions.bindRefresh(viewModel::refresh)
        onDispose { actions.bindRefresh(null); actions.showDetail(false) }
    }
    SideEffect { actions.showDetail(state.detail.taskId != null || state.monitorDetail.taskId != null) }

    LaunchedEffect(deepLink) {
        when (val target = deepLink) {
            // Task links are pushed over the current screen by the shell.
            is TaskDeepLinkTarget.Task, null -> Unit
            is TaskDeepLinkTarget.Monitor -> {
                viewModel.openMonitor(target.taskId, target.updateId)
                TaskDeepLinks.consume(target)
            }
        }
    }
    BackHandler(enabled = state.detail.taskId != null || state.monitorDetail.taskId != null) {
        if (state.detail.taskId != null) viewModel.closeTaskDetail() else viewModel.closeMonitorDetail()
    }

    when {
        state.detail.taskId != null -> TaskDetailScreen(state, viewModel)
        state.monitorDetail.taskId != null -> MonitorDetailScreen(state, viewModel)
        else -> Column(Modifier.fillMaxSize().background(taskWorkspaceColor(activePalette))) {
            state.actionNotice?.let { notice ->
                Row(
                    Modifier.fillMaxWidth().background(Success.copy(alpha = .12f)).padding(10.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(Icons.Outlined.CheckCircle, null, tint = Success, modifier = Modifier.size(17.dp))
                    Text(notice, color = Success, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
                }
            }
            TasksToolbar(
                state = state,
                onSearch = viewModel::setSearch,
            )
            LanePicker(state.lane, viewModel::selectLane)
            if (state.lane == TaskLane.Monitors) {
                MonitorLane(state, viewModel, onCreate = actions::showCreate)
            } else {
                FilterToolbar(state, viewModel)
                InternalCleanupControls(state, viewModel)
                StatusLedger(state)
                HorizontalDivider(color = BorderSoft)
                TaskLaneContent(
                    state = state,
                    listState = taskListState,
                    onOpen = viewModel::openTask,
                    onAction = { task, action -> taskAction(viewModel, task, action) },
                    onMore = { if (it.isAppManaged) viewModel.openTask(it) else actionTask = it },
                    onSelectCleanup = viewModel::toggleInternalSelection,
                    onRetrySynthesis = viewModel::retrySynthesis,
                    onLoadExecutionControls = viewModel::loadExecutionControls,
                    onExecutionControl = { executionId, action ->
                        viewModel.executionControl(executionId, action)
                    },
                    onDestructive = { task, action ->
                        when (action) {
                            TaskSwipeAction.MarkComplete -> viewModel.setStatus(task, "completed")
                            TaskSwipeAction.MarkNotDone, TaskSwipeAction.Reset -> viewModel.setStatus(task, "ready")
                            TaskSwipeAction.Cancel, TaskSwipeAction.Delete -> destructive = task to action
                        }
                    },
                    onLoadMore = viewModel::loadMore,
                    onRetry = viewModel::refresh,
                )
            }
        }
    }

    state.actionError?.let { error ->
        AlertDialog(
            onDismissRequest = viewModel::clearError,
            confirmButton = { TextButton(onClick = viewModel::clearError) { Text("OK", color = Coral) } },
            title = { Text(error.title, color = Ink) },
            text = { Text(error.message, color = Muted) },
            containerColor = Ground,
        )
    }

    if (actions.isCreateVisible) {
        if (state.lane == TaskLane.Monitors) {
            MonitorComposer(
                title = "New Monitor",
                initial = null,
                onDismiss = actions::hideCreate,
                onSave = { draft -> viewModel.createMonitor(draft) { if (it) actions.hideCreate() } },
                busy = state.mutatingTaskId != null,
            )
        } else {
            TaskCreateSheet(
                agents = state.agents,
                agentsLoading = state.agentsLoading,
                agentsError = state.agentsError,
                onRefreshAgents = viewModel::refreshAgents,
                referenceState = state,
                onLoadReferences = viewModel::loadReferences,
                lockedThread = state.threadContext,
                busy = state.mutatingTaskId != null,
                onDismiss = actions::hideCreate,
                onCreate = { draft -> viewModel.createTask(draft) { if (it) actions.hideCreate() } },
            )
        }
    }

    actionTask?.let { task ->
        TaskActionsSheet(
            task = task,
            state = state,
            viewModel = viewModel,
            onDismiss = { actionTask = null },
            onOpen = { actionTask = null; viewModel.openTask(task) },
            onConvert = { actionTask = null; convertTask = task },
            onCancel = { actionTask = null; destructive = task to TaskSwipeAction.Cancel },
            onDelete = { actionTask = null; destructive = task to TaskSwipeAction.Delete },
        )
    }

    convertTask?.let { task ->
        MonitorComposer(
            title = "Convert to Monitor",
            initial = ai.magicbeans.magdroid.tasks.MonitorDraft(
                title = task.title,
                objective = task.description.ifBlank { task.title },
                cron = null,
            ),
            onDismiss = { convertTask = null },
            onSave = { draft -> viewModel.convertToMonitor(task, draft) { if (it) convertTask = null } },
            busy = state.mutatingTaskId != null,
            keptSchedule = task.keptScheduleSummary,
        )
    }

    destructive?.let { (task, action) ->
        DestructiveTaskDialog(
            task = task,
            action = action,
            onDismiss = { destructive = null },
            onConfirm = { removeFiles ->
                destructive = null
                if (action == TaskSwipeAction.Cancel) viewModel.setStatus(task, "cancelled")
                else viewModel.deleteTask(task, removeFiles)
            },
        )
    }
}

@Composable
private fun TasksToolbar(
    state: TasksUiState,
    onSearch: (String) -> Unit,
) {
    Surface(
        modifier = Modifier
            .fillMaxWidth()
            .background(taskWorkspaceColor(activePalette))
            .padding(horizontal = 12.dp, vertical = 6.dp)
            .height(TASK_SEARCH_HEIGHT_DP.dp),
        color = taskControlColor(activePalette),
        shape = RoundedCornerShape(12.dp),
        border = BorderStroke(1.dp, ControlBorder),
    ) {
        BasicTextField(
            value = state.searchQuery,
            onValueChange = onSearch,
            modifier = Modifier
                .fillMaxSize()
                .semantics { contentDescription = "Search tasks" },
            singleLine = true,
            textStyle = MaterialTheme.typography.bodyMedium.copy(color = Ink, fontSize = 13.sp),
            cursorBrush = SolidColor(Coral),
            decorationBox = { field ->
                Row(
                    modifier = Modifier.fillMaxSize().padding(horizontal = 12.dp),
                    horizontalArrangement = Arrangement.spacedBy(9.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(Icons.Outlined.Search, null, tint = Muted, modifier = Modifier.size(18.dp))
                    Box(Modifier.weight(1f), contentAlignment = Alignment.CenterStart) {
                        if (state.searchQuery.isEmpty()) {
                            Text("Search tasks", color = Muted, fontSize = 13.sp)
                        }
                        field()
                    }
                }
            },
        )
    }
}

@Composable
private fun LanePicker(selected: TaskLane, onSelect: (TaskLane) -> Unit) {
    Row(
        Modifier.fillMaxWidth().background(taskWorkspaceColor(activePalette))
            .padding(horizontal = 12.dp, vertical = TASK_LANE_VERTICAL_PADDING_DP.dp),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        TaskLane.entries.forEach { lane ->
            val active = lane == selected
            Surface(
                color = if (active) AccentFill else Control,
                contentColor = if (active) OnAccent else Ink,
                shape = Lively.chipShape,
                modifier = Modifier.weight(1f).clickable(role = Role.Tab) { onSelect(lane) },
            ) {
                Text(
                    lane.title,
                    textAlign = androidx.compose.ui.text.style.TextAlign.Center,
                    fontSize = TASK_LANE_FONT_SP.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.padding(vertical = TASK_LANE_ITEM_VERTICAL_PADDING_DP.dp),
                )
            }
        }
    }
}

@Composable
private fun InternalCleanupControls(state: TasksUiState, viewModel: TasksViewModel) {
    if (state.lane != TaskLane.Internal) return
    var confirm by remember { mutableStateOf(false) }
    Column(Modifier.fillMaxWidth().padding(horizontal = 12.dp)) {
        Row(Modifier.horizontalScroll(rememberScrollState())) {
            TextButton(onClick = viewModel::toggleInternalSelectionMode, enabled = state.mutatingTaskId == null) { Text(if (state.selectingInternal) "Finish selection" else "Select cleanup") }
            if (state.selectingInternal) {
                TextButton(onClick = viewModel::selectLoadedInternal, enabled = state.mutatingTaskId == null) { Text("Select loaded") }
                TextButton(onClick = { confirm = true }, enabled = state.selectedInternalIDs.isNotEmpty() && state.mutatingTaskId == null) { Text("Delete selected (${state.selectedInternalIDs.size})", color = Danger) }
            }
        }
        state.cleanupProgress?.let { Text(it, color = Muted) }
    }
    if (confirm) AlertDialog(onDismissRequest = { confirm = false }, title = { Text("Delete ${state.selectedInternalIDs.size} selected internal records?") }, text = { Text("App-managed runs are excluded. Failed deletions remain selected for retry.") }, confirmButton = { TextButton(onClick = { confirm = false; viewModel.deleteSelectedInternal() }) { Text("Delete selected records", color = Danger) } }, dismissButton = { TextButton(onClick = { confirm = false }) { Text("Cancel") } })
}

@Composable
private fun FilterToolbar(state: TasksUiState, viewModel: TasksViewModel) {
    Column(
        Modifier.fillMaxWidth().background(taskWorkspaceColor(activePalette)),
        verticalArrangement = Arrangement.spacedBy(5.dp),
    ) {
        if (state.lane == TaskLane.Tasks) {
            LazyRow(
                contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 12.dp),
                horizontalArrangement = Arrangement.spacedBy(5.dp),
            ) {
                items(TaskFilter.entries) { filter ->
                    val count = state.laneCount(filter)
                    MagicianFilterChip(
                        text = filter.title,
                        count = count,
                        selected = state.selectedTag == null && state.filter == filter,
                        onClick = { viewModel.setFilter(filter) },
                    )
                }
            }
        }
        Row(
            Modifier.horizontalScroll(rememberScrollState()).padding(horizontal = 12.dp),
            horizontalArrangement = Arrangement.spacedBy(5.dp),
        ) {
            SortMenu(state, viewModel)
            if (state.lane == TaskLane.Tasks) StringMenu(label = "${state.taskPageSize} per page", icon = Icons.Outlined.FilterList, selected = false, values = listOf("25", "50", "100", "200"), render = { "$it per page" }, onSelect = { it?.toIntOrNull()?.let(viewModel::setTaskPageSize) })
            if (state.lane == TaskLane.Internal) {
                StringMenu(label = "${state.internalPageSize} per page", icon = Icons.Outlined.FilterList, selected = false, values = listOf("25", "50", "100", "200"), render = { "$it per page" }, onSelect = { it?.toIntOrNull()?.let(viewModel::setInternalPageSize) })
                StringMenu(
                    label = state.internalStatusFilter?.replaceFirstChar(Char::uppercase) ?: "Status",
                    icon = Icons.Outlined.FilterList,
                    selected = state.internalStatusFilter != null,
                    values = listOf(null, "pending", "queued", "planning", "ready", "running", "completed", "failed", "paused", "deferred", "cancelled"),
                    render = { it?.replaceFirstChar(Char::uppercase) ?: "Any status" },
                    onSelect = viewModel::setInternalStatus,
                )
                if (state.availableInternalAgents.isNotEmpty()) {
                    StringMenu(
                        label = state.internalAgentFilter ?: "Agent",
                        icon = Icons.Outlined.Person,
                        selected = state.internalAgentFilter != null,
                        values = listOf(null) + state.availableInternalAgents,
                        render = { it ?: "Any agent" },
                        onSelect = viewModel::setInternalAgent,
                    )
                }
            }
        }
        if (state.lane == TaskLane.Tasks && state.availableTags.isNotEmpty()) {
            LazyRow(
                contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 12.dp),
                horizontalArrangement = Arrangement.spacedBy(5.dp),
            ) {
                items(state.availableTags) { tag ->
                    MagicianFilterChip(
                        text = "#$tag",
                        selected = state.selectedTag == tag,
                        onClick = { viewModel.toggleTag(tag) },
                    )
                }
            }
        }
        Spacer(Modifier.height(1.dp))
    }
}

@Composable
private fun SortMenu(state: TasksUiState, viewModel: TasksViewModel) {
    var expanded by remember { mutableStateOf(false) }
    Box {
        MagicianFilterChip(
            text = "${state.sortField.title} ${if (state.sortAscending) "↑" else "↓"}",
            selected = false,
            onClick = { expanded = true },
            leadingIcon = Icons.Outlined.Tune,
        )
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            TaskSortField.entries.forEach { field ->
                DropdownMenuItem(
                    text = { Text(field.title + if (field == state.sortField) " ${if (state.sortAscending) "↑" else "↓"}" else "") },
                    onClick = { viewModel.setSort(field); expanded = false },
                )
            }
        }
    }
}

@Composable
internal fun <T> StringMenu(
    label: String,
    icon: ImageVector,
    selected: Boolean,
    values: List<T>,
    render: (T) -> String,
    onSelect: (T) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    Box {
        MagicianFilterChip(
            text = label,
            selected = selected,
            onClick = { expanded = true },
            leadingIcon = icon,
        )
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            values.forEach { value -> DropdownMenuItem(
                text = { Text(render(value)) },
                onClick = { onSelect(value); expanded = false },
            ) }
        }
    }
}

@Composable
private fun StatusLedger(state: TasksUiState) {
    val groups = listOf(
        "Paused" to arrayOf("paused"), "Pending" to arrayOf("pending"),
        "Planning" to arrayOf("planning"), "Ready" to arrayOf("ready"),
        "Running" to arrayOf("running"), "Failed" to arrayOf("failed", "cancelled", "canceled"),
    ).mapNotNull { (label, statuses) -> state.statusCount(*statuses).takeIf { it > 0 }?.let { label to it } }
    if (groups.isEmpty()) return
    LazyRow(
        modifier = Modifier.fillMaxWidth().background(taskWorkspaceColor(activePalette)).padding(bottom = 4.dp),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        items(groups) { (label, count) ->
            Surface(color = taskControlColor(activePalette), shape = Lively.chipShape) {
                Text(
                    "$label  $count",
                    color = Muted,
                    fontSize = TASK_LEDGER_FONT_SP.sp,
                    lineHeight = TASK_LEDGER_LINE_HEIGHT_SP.sp,
                    maxLines = 1,
                    modifier = Modifier.padding(
                        horizontal = TASK_LEDGER_HORIZONTAL_PADDING_DP.dp,
                        vertical = TASK_LEDGER_VERTICAL_PADDING_DP.dp,
                    ),
                )
            }
        }
    }
}

@Composable
private fun TaskLaneContent(
    state: TasksUiState,
    listState: LazyListState,
    onOpen: (TaskV3) -> Unit,
    onAction: (TaskV3, TaskCardAction) -> Unit,
    onMore: (TaskV3) -> Unit,
    onRetrySynthesis: (TaskV3) -> Unit,
    onSelectCleanup: (TaskV3) -> Unit,
    onLoadExecutionControls: (String) -> Unit,
    onExecutionControl: (String, ExecutionControlAction) -> Unit,
    onDestructive: (TaskV3, TaskSwipeAction) -> Unit,
    onLoadMore: () -> Unit,
    onRetry: () -> Unit,
) {
    val rows = state.visibleTasks()
    if (rows.isEmpty()) {
        EmptyTaskState(state.activeLoadState, state.lane, state.activeLoadError, onRetry)
        return
    }
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        state = listState,
        contentPadding = androidx.compose.foundation.layout.PaddingValues(10.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        state.activeLoadError?.let { error ->
            item(key = "load-error-${state.lane.name}") {
                TaskLoadErrorBanner(error, onRetry)
            }
        }
        items(rows, key = TaskV3::id, contentType = { "task-card" }) { task ->
            if (state.lane == TaskLane.Internal && state.selectingInternal) {
                if (task.canCleanUpInternal) Row(Modifier.fillMaxWidth().heightIn(min = 48.dp), verticalAlignment = Alignment.CenterVertically) { Checkbox(checked = task.id in state.selectedInternalIDs, onCheckedChange = { onSelectCleanup(task) }, enabled = state.mutatingTaskId == null, colors = magicanCheckboxColors()); Text("Select for cleanup", color = Ink) }
                else Text("App-managed · manage in Apps", color = Muted)
                state.cleanupFailures[task.id]?.let { Text(it, color = Danger) }
            }
            TaskCard(
                task = task,
                state = state,
                onOpen = onOpen,
                onAction = onAction,
                onMore = onMore,
                onRetrySynthesis = onRetrySynthesis,
                onLoadExecutionControls = onLoadExecutionControls,
                onExecutionControl = onExecutionControl,
                onDestructive = onDestructive,
            )
        }
        val hasMore = if (state.lane == TaskLane.Internal) state.internalHasMore else state.taskHasMore
        if (hasMore) item {
            OutlinedButton(shape = MagicanButtonShape, onClick = onLoadMore, modifier = Modifier.fillMaxWidth()) {
                if (state.activeLoadState == TaskLoadState.Loading) {
                    CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
                    Spacer(Modifier.width(7.dp))
                }
                val loaded = if (state.lane == TaskLane.Internal) state.internalTasks.size else state.tasks.size
                val total = if (state.lane == TaskLane.Internal) state.internalTotal else state.taskTotal
                Text("Load more · $loaded of $total")
            }
        }
    }
}

@Composable
private fun EmptyTaskState(
    loadState: TaskLoadState,
    lane: TaskLane,
    error: TaskUserError?,
    retry: () -> Unit,
) {
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 28.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            when (loadState) {
                TaskLoadState.Idle, TaskLoadState.Loading -> {
                    CircularProgressIndicator(
                        modifier = Modifier.size(TASK_EMPTY_PROGRESS_DP.dp),
                        color = Coral,
                        strokeWidth = 2.dp,
                    )
                    Text(
                        if (lane == TaskLane.Internal) "Loading internal tasks…" else "Loading tasks…",
                        color = Muted,
                        fontSize = TASK_EMPTY_BODY_FONT_SP.sp,
                    )
                }
                TaskLoadState.Failed -> {
                    Text(
                        error?.title ?: if (lane == TaskLane.Internal) "Internal tasks unavailable" else "Tasks unavailable",
                        color = Ink,
                        fontSize = TASK_EMPTY_TITLE_FONT_SP.sp,
                        fontWeight = FontWeight.SemiBold,
                        textAlign = TextAlign.Center,
                    )
                    Text(
                        error?.message ?: "Check the connection and try again.",
                        color = Muted,
                        fontSize = TASK_EMPTY_BODY_FONT_SP.sp,
                        lineHeight = 15.sp,
                        textAlign = TextAlign.Center,
                        modifier = Modifier.widthIn(max = 320.dp),
                    )
                    OutlinedButton(shape = MagicanButtonShape,
                        onClick = retry,
                        modifier = Modifier.height(TASK_EMPTY_ACTION_HEIGHT_DP.dp),
                        contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 14.dp, vertical = 2.dp),
                    ) { Text("Try again", fontSize = TASK_EMPTY_BODY_FONT_SP.sp) }
                }
                TaskLoadState.Loaded -> {
                    Icon(Icons.Outlined.CheckCircle, null, tint = Muted, modifier = Modifier.size(TASK_EMPTY_ICON_DP.dp))
                    Text(
                        "No tasks in this view",
                        color = Ink,
                        fontSize = TASK_EMPTY_TITLE_FONT_SP.sp,
                        fontWeight = FontWeight.SemiBold,
                    )
                    Text("Try another filter, tag, or search.", color = Muted, fontSize = TASK_EMPTY_BODY_FONT_SP.sp)
                }
            }
        }
    }
}

@Composable
internal fun MagicianFilterChip(
    text: String,
    selected: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    count: Int? = null,
    leadingIcon: ImageVector? = null,
) {
    val foreground = if (selected) OnAccent else Ink
    Surface(
        color = if (selected) AccentFill else Control,
        contentColor = foreground,
        shape = Lively.chipShape,
        border = if (selected) null else BorderStroke(1.dp, ControlBorder),
        modifier = modifier
            .selectable(selected = selected, role = Role.Button, onClick = onClick)
            .semantics {
                contentDescription = buildString {
                    append(text)
                    count?.let { append(", $it") }
                }
            },
    ) {
        Row(
            modifier = Modifier.padding(
                horizontal = TASK_CHIP_HORIZONTAL_PADDING_DP.dp,
                vertical = TASK_CHIP_VERTICAL_PADDING_DP.dp,
            ),
            horizontalArrangement = Arrangement.spacedBy(5.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            leadingIcon?.let {
                Icon(it, null, tint = foreground, modifier = Modifier.size(TASK_CHIP_ICON_DP.dp))
            }
            Text(
                text,
                color = foreground,
                fontSize = TASK_CHIP_FONT_SP.sp,
                lineHeight = TASK_CHIP_LINE_HEIGHT_SP.sp,
                fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Medium,
                maxLines = 1,
            )
            count?.let {
                Text(
                    it.toString(),
                    color = foreground.copy(alpha = .78f),
                    fontSize = TASK_CHIP_COUNT_FONT_SP.sp,
                    lineHeight = TASK_CHIP_LINE_HEIGHT_SP.sp,
                    maxLines = 1,
                )
            }
        }
    }
}

@Composable
internal fun TaskLoadErrorBanner(
    error: TaskUserError,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Surface(
        modifier = modifier.fillMaxWidth(),
        color = Warning.copy(alpha = .10f),
        shape = RoundedCornerShape(12.dp),
        border = BorderStroke(1.dp, Warning.copy(alpha = .28f)),
    ) {
        Row(
            Modifier.padding(horizontal = 12.dp, vertical = 10.dp),
            horizontalArrangement = Arrangement.spacedBy(9.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(error.title, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                Text(error.message, color = Muted, fontSize = 11.sp)
            }
            TextButton(onClick = onRetry) { Text("Retry", color = Coral) }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun TaskCard(
    task: TaskV3,
    state: TasksUiState,
    onOpen: (TaskV3) -> Unit,
    onAction: (TaskV3, TaskCardAction) -> Unit,
    onMore: (TaskV3) -> Unit,
    onRetrySynthesis: (TaskV3) -> Unit,
    onLoadExecutionControls: (String) -> Unit,
    onExecutionControl: (String, ExecutionControlAction) -> Unit,
    onDestructive: (TaskV3, TaskSwipeAction) -> Unit,
) {
    val activityLine = remember(task) { task.activityLine }
    val recurring = remember(task) { task.isRecurring }
    val recurringDescription = remember(task) { task.recurringDescription }
    val displayTags = remember(task) { task.displayTags }
    val isInternalRow = state.lane == TaskLane.Internal
    val updatedLabel = remember(task.updatedAt) { relativeUpdated(task) }
    val currentOnDestructive = rememberUpdatedState(onDestructive)
    val swipePalette = activePalette
    val leading = remember(task, swipePalette) {
        task.leadingSwipeActions.map { action ->
            taskSwipeAction(task, action, swipePalette) { target, swipeAction ->
                currentOnDestructive.value(target, swipeAction)
            }
        }
    }
    val trailing = remember(task, swipePalette) {
        task.trailingSwipeActions.map { action ->
            taskSwipeAction(task, action, swipePalette) { target, swipeAction ->
                currentOnDestructive.value(target, swipeAction)
            }
        }
    }
    TodaySwipeActionCard(
        itemId = task.id,
        leadingActions = leading,
        trailingActions = trailing,
        enabled = state.mutatingTaskId == null,
    ) {
    Card(
        modifier = Modifier.fillMaxWidth().clickable { onOpen(task) }
            .semantics { role = Role.Button; contentDescription = "${task.title}, ${task.statusLabel}" },
        colors = CardDefaults.cardColors(containerColor = taskCardColor(activePalette)),
        border = BorderStroke(1.dp, taskCardBorderColor(activePalette)),
        shape = RoundedCornerShape(14.dp),
    ) {
        Column(
            Modifier.padding(TASK_CARD_PADDING_DP.dp),
            verticalArrangement = Arrangement.spacedBy(TASK_CARD_SPACING_DP.dp),
        ) {
            Row(verticalAlignment = Alignment.Top, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                if (ai.magicbeans.magdroid.tasks.TaskPlanMarker.hasPlan(task)) {
                    TaskPlanGlyph(Modifier.padding(top = 4.dp))
                }
                Text(
                    task.title.ifBlank { "Untitled task" },
                    color = Ink, fontSize = TASK_CARD_TITLE_FONT_SP.sp, fontWeight = FontWeight.SemiBold,
                    lineHeight = TASK_CARD_TITLE_LINE_HEIGHT_SP.sp,
                    maxLines = 2, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = !(isInternalRow && recurring)),
                )
                if (isInternalRow && recurring) {
                    // Web internal rows: a bare ↻ glyph right after the title.
                    Text(
                        "↻",
                        color = Info,
                        fontSize = TASK_CARD_TITLE_FONT_SP.sp,
                        lineHeight = TASK_CARD_TITLE_LINE_HEIGHT_SP.sp,
                        fontWeight = FontWeight.Bold,
                        modifier = Modifier.weight(1f).semantics {
                            contentDescription = recurringDescription?.let { "Recurring task, $it" } ?: "Recurring task"
                        },
                    )
                }
                if (task.needsAnswer) StatusPill("Answer", Warning)
                StatusPill(task.statusLabel, taskStatusTint(task.status))
            }
            activityLine?.takeIf(String::isNotBlank)?.let {
                Text(
                    it,
                    color = Muted,
                    fontSize = TASK_CARD_BODY_FONT_SP.sp,
                    lineHeight = TASK_CARD_BODY_LINE_HEIGHT_SP.sp,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Row(
                Modifier.horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                task.visibleActions.forEachIndexed { index, action ->
                    val publishing = action == TaskCardAction.PublishToNotes
                    TaskActionButton(
                        action = action,
                        primary = index == 0,
                        busy = if (publishing) state.publishingTaskId == task.id else state.mutatingTaskId == task.id,
                        enabled = state.mutatingTaskId == null && (!publishing || state.publishingTaskId == null),
                    ) { onAction(task, action) }
                }
                CompactTaskButton(
                    label = "Actions",
                    icon = null,
                    tint = activePalette.secondaryText,
                    filled = false,
                    enabled = state.mutatingTaskId == null,
                    onClick = { onMore(task) },
                )
            }
            Row(
                Modifier.horizontalScroll(rememberScrollState()),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                if (recurring && !isInternalRow) RecurringChip(recurringDescription)
                displayTags.forEach { TagPill(it) }
                task.priorityLabel?.let { MetaPill(it, Danger) }
                task.dueDate?.takeIf(String::isNotBlank)?.let { MetaPill(it, Coral) }
                if (task.isBlocked) MetaPill("Blocked", Warning)
                if (task.agentId.isNotBlank()) {
                    Icon(Icons.Outlined.Person, null, tint = Muted, modifier = Modifier.size(12.dp))
                    Text(agentName(state, task.agentId), color = Muted, fontSize = 10.sp, maxLines = 1)
                }
                Text("#${task.uiThreadId}", color = Muted, fontSize = 10.sp, maxLines = 1)
                updatedLabel?.let { Text("· $it", color = Muted, fontSize = 10.sp) }
                if (state.lane == TaskLane.Internal) StatusPill(task.lifecycleLabel, lifecycleTint(task.lifecycleLabel))
                when {
                    task.synthesisPending -> StatusPill("synthesizing…", Coral)
                    task.synthesisFailed && !task.isAppManaged -> CompactTaskButton(
                        label = "synth failed · retry",
                        icon = Icons.Outlined.RestartAlt,
                        tint = Danger,
                        filled = false,
                        enabled = state.retryingSynthesisId != task.id,
                        onClick = { onRetrySynthesis(task) },
                    )
                }
            }
            task.activeExecutionIdForControls?.let { executionId ->
                ExecutionControls(
                    task = task,
                    executionId = executionId,
                    controls = state.executionControls[executionId],
                    onLoad = onLoadExecutionControls,
                    onControl = onExecutionControl,
                    onSteer = { onOpen(task) },
                )
            }
        }
    }
    }
}

private fun taskSwipeAction(
    task: TaskV3,
    action: TaskSwipeAction,
    palette: Palette,
    invoke: (TaskV3, TaskSwipeAction) -> Unit,
): TodaySwipeAction = TodaySwipeAction(
    id = action.name,
    title = taskSwipeTitle(action),
    icon = when (action) {
        TaskSwipeAction.MarkComplete -> Icons.Outlined.CheckCircle
        TaskSwipeAction.MarkNotDone, TaskSwipeAction.Reset -> Icons.Outlined.RestartAlt
        TaskSwipeAction.Cancel -> Icons.Outlined.Cancel
        TaskSwipeAction.Delete -> Icons.Outlined.Delete
    },
    color = taskSwipeTint(action, palette),
    run = { invoke(task, action) },
)

internal fun taskSwipeTitle(action: TaskSwipeAction): String = when (action) {
    TaskSwipeAction.MarkComplete -> "Complete"
    TaskSwipeAction.MarkNotDone -> "Not done"
    TaskSwipeAction.Reset -> "Reset"
    TaskSwipeAction.Cancel -> "Cancel"
    TaskSwipeAction.Delete -> "Delete"
}

internal fun taskSwipeTint(action: TaskSwipeAction, palette: Palette = activePalette): Color = when (action) {
    TaskSwipeAction.MarkComplete -> palette.success
    TaskSwipeAction.MarkNotDone -> palette.info
    TaskSwipeAction.Reset -> palette.discovery
    TaskSwipeAction.Cancel -> palette.warning
    TaskSwipeAction.Delete -> palette.danger
}

@Composable
private fun ExecutionControls(
    task: TaskV3,
    executionId: String,
    controls: ai.magicbeans.magdroid.tasks.ExecutionControlUiState?,
    onLoad: (String) -> Unit,
    onControl: (String, ExecutionControlAction) -> Unit,
    onSteer: (TaskV3) -> Unit,
) {
    LaunchedEffect(executionId) { onLoad(executionId) }
    if (controls?.loading == true) LinearProgressIndicator(Modifier.fillMaxWidth(), color = Coral)
    controls?.error?.let { Text(it, color = Danger, fontSize = 11.sp) }
    controls?.state?.let { control ->
        Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(5.dp)) {
            if (control.canPause) SmallControl("Pause", Icons.Outlined.Schedule, controls.busy == ExecutionControlAction.Pause) {
                onControl(executionId, ExecutionControlAction.Pause)
            }
            if (control.canResume) SmallControl("Resume", Icons.Outlined.PlayArrow, controls.busy == ExecutionControlAction.Resume) {
                onControl(executionId, ExecutionControlAction.Resume)
            }
            if (control.canSteer) SmallControl("Steer", Icons.Outlined.Tune, controls.busy == ExecutionControlAction.Steer) {
                // Full guidance entry is in task detail; card keeps the compact safe controls.
                onSteer(task)
            }
            if (control.canCancel) SmallControl("Stop", Icons.Outlined.StopCircle, controls.busy == ExecutionControlAction.Cancel, Danger) {
                onControl(executionId, ExecutionControlAction.Cancel)
            }
        }
    }
}

@Composable
private fun SmallControl(label: String, icon: ImageVector, busy: Boolean, tint: Color = Info, action: () -> Unit) {
    CompactTaskButton(
        label = label,
        icon = icon,
        tint = tint,
        filled = false,
        busy = busy,
        enabled = !busy,
        onClick = action,
    )
}

@Composable
private fun TaskActionButton(
    action: TaskCardAction,
    primary: Boolean,
    busy: Boolean,
    enabled: Boolean,
    onClick: () -> Unit,
) {
    val (label, icon) = when (action) {
        TaskCardAction.ViewPlan -> "View Plan" to Icons.Outlined.EditCalendar
        TaskCardAction.AnswerQuestion -> "Answer Question" to Icons.Outlined.EditCalendar
        TaskCardAction.ReviewPlan -> "Review Plan" to Icons.Outlined.EditCalendar
        TaskCardAction.RunPlan -> "Run Plan" to Icons.Outlined.PlayArrow
        TaskCardAction.Preplan -> "PrePlan" to Icons.Outlined.EditCalendar
        TaskCardAction.RunNow -> "Run Now" to Icons.Outlined.PlayArrow
        TaskCardAction.ViewExecution -> "View Execution" to Icons.Outlined.Search
        TaskCardAction.ViewQuestion -> "View Question" to Icons.Outlined.Search
        TaskCardAction.Reset -> "Reset to Ready" to Icons.Outlined.RestartAlt
        TaskCardAction.Result -> taskCardActionLabel(action) to Icons.Outlined.CheckCircle
        TaskCardAction.PublishToNotes -> taskCardActionLabel(action) to Icons.AutoMirrored.Outlined.Article
    }
    CompactTaskButton(
        label = label,
        icon = icon,
        tint = Coral,
        // The first action is the card's primary; the rest are secondary.
        filled = primary,
        busy = busy,
        enabled = enabled,
        onClick = onClick,
    )
}

internal fun taskCardActionLabel(action: TaskCardAction): String = when (action) {
    TaskCardAction.ViewPlan -> "View Plan"
    TaskCardAction.AnswerQuestion -> "Answer Question"
    TaskCardAction.ReviewPlan -> "Review Plan"
    TaskCardAction.RunPlan -> "Run Plan"
    TaskCardAction.Preplan -> "PrePlan"
    TaskCardAction.RunNow -> "Run Now"
    TaskCardAction.ViewExecution -> "View Execution"
    TaskCardAction.ViewQuestion -> "View Question"
    TaskCardAction.Reset -> "Reset to Ready"
    TaskCardAction.Result -> "Result"
    TaskCardAction.PublishToNotes -> "Publish to Notes"
}

/** "↻ Recurring" with the human schedule beside it; replaces the raw cron pill. */
@Composable
private fun RecurringChip(description: String?) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        modifier = Modifier.semantics(mergeDescendants = true) {
            contentDescription = description?.let { "Recurring, $it" } ?: "Recurring task"
        },
    ) {
        MetaPill(TASK_RECURRING_LABEL, Info)
        description?.let { Text(it, color = Muted, fontSize = 10.sp, maxLines = 1) }
    }
}

internal const val TASK_RECURRING_LABEL = "↻ Recurring"

@Composable
private fun CompactTaskButton(
    label: String,
    icon: ImageVector?,
    tint: Color,
    filled: Boolean,
    busy: Boolean = false,
    enabled: Boolean = true,
    onClick: () -> Unit,
) {
    val opacity = if (enabled || busy) 1f else .48f
    val foreground = if (filled) activePalette.foregroundFor(tint) else tint
    // A filled accent action sits on the deeper fill so on-accent text reads.
    val background = if (filled) activePalette.fillFor(tint) else tint.copy(alpha = .10f)
    Surface(
        modifier = Modifier.clickable(enabled = enabled && !busy, role = Role.Button, onClick = onClick),
        color = background.copy(alpha = background.alpha * opacity),
        contentColor = foreground.copy(alpha = foreground.alpha * opacity),
        shape = RoundedCornerShape(TASK_ACTION_CORNER_DP.dp),
        border = if (filled) null else BorderStroke(1.dp, tint.copy(alpha = .25f * opacity)),
    ) {
        Row(
            modifier = Modifier.padding(
                horizontal = TASK_ACTION_HORIZONTAL_PADDING_DP.dp,
                vertical = TASK_ACTION_VERTICAL_PADDING_DP.dp,
            ),
            horizontalArrangement = Arrangement.spacedBy(TASK_ACTION_SPACING_DP.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (busy) {
                CircularProgressIndicator(
                    Modifier.size(TASK_ACTION_ICON_DP.dp),
                    color = foreground,
                    strokeWidth = 1.5.dp,
                )
            } else if (icon != null) {
                Icon(icon, null, modifier = Modifier.size(TASK_ACTION_ICON_DP.dp))
            }
            Text(
                if (busy) "Working…" else label,
                fontSize = TASK_ACTION_FONT_SP.sp,
                lineHeight = TASK_ACTION_LINE_HEIGHT_SP.sp,
                fontWeight = FontWeight.Bold,
                maxLines = 1,
            )
        }
    }
}

private fun taskAction(viewModel: TasksViewModel, task: TaskV3, action: TaskCardAction) {
    when (action) {
        TaskCardAction.RunPlan, TaskCardAction.RunNow -> viewModel.execute(task)
        TaskCardAction.Preplan -> viewModel.preplan(task)
        TaskCardAction.Reset -> viewModel.setStatus(task, "ready")
        TaskCardAction.Result -> viewModel.openTaskResult(task)
        TaskCardAction.PublishToNotes -> viewModel.publishToNotes(task)
        else -> viewModel.openTask(task)
    }
}

@Composable
private fun StatusPill(label: String, tint: Color, size: Int = TASK_STATUS_FONT_SP) {
    Text(
        label,
        color = tint,
        fontSize = size.sp,
        lineHeight = TASK_STATUS_LINE_HEIGHT_SP.sp,
        fontWeight = FontWeight.Bold,
        maxLines = 1,
        modifier = Modifier
            .clip(RoundedCornerShape(TASK_STATUS_CORNER_DP.dp))
            .background(tint.copy(alpha = .14f))
            .padding(
                horizontal = TASK_STATUS_HORIZONTAL_PADDING_DP.dp,
                vertical = TASK_STATUS_VERTICAL_PADDING_DP.dp,
            ),
    )
}

@Composable private fun TagPill(tag: TaskTag) = MetaPill(tag.name, parseColor(tag.color) ?: Coral)

@Composable
private fun MetaPill(label: String, tint: Color) {
    Text(
        label,
        color = tint,
        fontSize = 10.sp,
        modifier = Modifier
            .clip(RoundedCornerShape(6.dp))
            .background(tint.copy(alpha = .12f))
            .padding(horizontal = 6.dp, vertical = 2.dp),
    )
}

internal fun taskStatusTint(status: String, palette: Palette = activePalette): Color = when (status.lowercase()) {
    "running", "planning", "executing" -> palette.info
    "completed" -> palette.success
    "failed", "cancelled", "canceled" -> palette.danger
    "paused", "deferred" -> palette.warning
    else -> palette.secondaryText
}

private fun lifecycleTint(label: String): Color = when (label) {
    "internal" -> Coral; "chat" -> Info; "debug" -> Warning; else -> Muted
}

private fun parseColor(hex: String?): Color? = runCatching {
    val clean = hex?.removePrefix("#") ?: return@runCatching null
    val value = clean.toLong(16)
    Color(if (clean.length == 6) 0xFF000000 or value else value)
}.getOrNull()

private fun agentName(state: TasksUiState, id: String): String = state.agents.firstOrNull { it.id == id }?.name ?: id

private fun relativeUpdated(task: TaskV3): String? {
    val then = task.updatedInstant()?.toEpochMilli() ?: return null
    val seconds = ((System.currentTimeMillis() - then) / 1000).coerceAtLeast(0)
    return when {
        seconds < 60 -> "now"
        seconds < 3_600 -> "${seconds / 60}m ago"
        seconds < 86_400 -> "${seconds / 3_600}h ago"
        else -> "${seconds / 86_400}d ago"
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun TaskCreateSheet(
    agents: List<ai.magicbeans.magdroid.tasks.AgentOption>,
    busy: Boolean,
    onDismiss: () -> Unit,
    onCreate: (TaskCreateDraft) -> Unit,
    lockedThread: String? = null,
    agentsLoading: Boolean = false,
    agentsError: String? = null,
    onRefreshAgents: () -> Unit = {},
    referenceState: TasksUiState = TasksUiState(),
    onLoadReferences: (Boolean) -> Unit = {},
) {
    LaunchedEffect(Unit) { onRefreshAgents() }
    var title by remember { mutableStateOf("") }
    var description by remember { mutableStateOf("") }
    var agent by remember { mutableStateOf("") }
    LaunchedEffect(agents, agentsLoading) { if (!agentsLoading && agents.none { it.id == agent }) agent = agents.firstOrNull { it.isPrimary }?.id ?: agents.firstOrNull()?.id.orEmpty() }
    var thread by remember(lockedThread) { mutableStateOf(lockedThread ?: "general") }
    var priority by remember { mutableStateOf("") }
    var dueDate by remember { mutableStateOf("") }
    var tags by remember { mutableStateOf("") }
    var outputMode by remember { mutableStateOf("accumulate") }
    var dependsOn by remember { mutableStateOf("") }
    var referencesExpanded by remember { mutableStateOf(false) }
    var referenceIDs by remember { mutableStateOf(emptyList<String>()) }
    var referenceQuery by remember { mutableStateOf("") }
    var repeat by remember { mutableStateOf("none") }
    var customCron by remember { mutableStateOf("") }
    var scheduleTimezone by remember { mutableStateOf(ZoneId.systemDefault().id) }
    val invalidSchedule = repeat != "none" && (runCatching { ZoneId.of(scheduleTimezone.trim()) }.isFailure || (repeat == "custom" && customCron.trim().split(Regex("\\s+")).size != 5))
    var maxRecords by remember { mutableStateOf("") }
    var maxDays by remember { mutableStateOf("") }
    val canCreate = title.isNotBlank() && !busy && !invalidSchedule && !agentsLoading && agents.any { it.id == agent }
    val submit = { onCreate(TaskCreateDraft(title = title.trim(), description = ai.magicbeans.magdroid.tasks.taskReferenceDescription(description, referenceIDs), agentId = agent, threadId = lockedThread ?: thread.ifBlank { "general" }, priority = priority.ifBlank { null }, dueDate = dueDate.trim().ifBlank { null }, tagNames = csv(tags), outputMode = outputMode, dependsOn = ai.magicbeans.magdroid.tasks.taskReferenceDependencies(csv(dependsOn), referenceIDs), schedule = taskSchedule(repeat, maxRecords, maxDays, customCron, scheduleTimezone))) }
    val sheet = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheet, containerColor = Ground) {
        LazyColumn(
            Modifier.fillMaxWidth().padding(horizontal = 18.dp),
            verticalArrangement = Arrangement.spacedBy(11.dp),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(bottom = 28.dp),
        ) {
            item { SheetTitle("New Task", onDismiss, dismissLabel = "Cancel") { TextButton(onClick = submit, enabled = canCreate) { Text("Create") } } }
            item { SectionLabel("Task") }
            item { MagicianTextField(title, { title = it }, Modifier.fillMaxWidth(), label = { Text("Title") }, singleLine = true) }
            item { MagicianTextField(description, { description = it }, Modifier.fillMaxWidth(), label = { Text("Description") }, minLines = 2, maxLines = 5) }
            item { WorkLink("Reference completed work (${referenceIDs.size})") { referencesExpanded = !referencesExpanded; if (referencesExpanded) onLoadReferences(false) } }
            if (referencesExpanded) {
                items(referenceIDs, key = { "selected-reference-$it" }) { id -> WorkLink("Remove reference: ${referenceState.completedReferences.firstOrNull { it.id == id }?.title ?: id}") { referenceIDs = referenceIDs - id } }
                item { Text("Selected tasks supply their context and outputs. Other dependencies can still be added below.", color = Muted) }
                item { MagicianTextField(referenceQuery, { referenceQuery = it }, Modifier.fillMaxWidth(), label = { Text("Search loaded completed tasks") }) }
                items(referenceState.completedReferences.filter { referenceQuery.isBlank() || (it.title + " " + it.id).contains(referenceQuery, ignoreCase = true) }, key = { "reference-${it.id}" }) { task ->
                    Row(Modifier.fillMaxWidth().clickable { referenceIDs = if (task.id in referenceIDs) referenceIDs - task.id else referenceIDs + task.id }.heightIn(min = 48.dp), verticalAlignment = Alignment.CenterVertically) {
                        Checkbox(checked = task.id in referenceIDs, onCheckedChange = { checked -> referenceIDs = if (checked) (referenceIDs + task.id).distinct() else referenceIDs - task.id }, colors = magicanCheckboxColors())
                        Column(Modifier.weight(1f)) { Text(task.title, color = Ink); Text(task.id, color = Muted, style = MaterialTheme.typography.bodySmall) }
                    }
                }
                if (referenceState.referencesLoading) item { Text("Loading completed work", color = Muted) }
                else if (referenceState.referencesError != null) item { Text(referenceState.referencesError.orEmpty(), color = Danger); WorkLink("Retry references") { onLoadReferences(false) } }
                else if (referenceState.completedReferences.isEmpty()) item { Text("No completed tasks available.", color = Muted) }
                if (referenceState.referencesHasMore) item { TextButton(onClick = { onLoadReferences(true) }, enabled = !referenceState.referencesLoading) { Text("Load more completed tasks") } }
            }
            item { SectionLabel("Assignment") }
            item { ChoiceField("Agent", agent, agents.map { it.id }.distinct(), { id -> agents.firstOrNull { it.id == id }?.name ?: "Choose personal crew" }) { agent = it } }
            if (agentsLoading) item { Text("Loading crew", color = Muted) }
            else if (agentsError != null) item { Text(agentsError, color = Danger); WorkLink("Retry crew", onRefreshAgents) }
            else if (agents.isEmpty()) item { Text("Add or enable a Personal member in Work’s Crew view.", color = Muted) }
            item { MagicianTextField(thread, { thread = it }, Modifier.fillMaxWidth(), label = { Text("Thread") }, singleLine = true, enabled = lockedThread == null) }
            item { SectionLabel("Details") }
            item { ChoiceField("Priority", priority, listOf("", "p1", "p2", "p3", "p4"), { if (it.isEmpty()) "None" else it.uppercase() }) { priority = it } }
            item { MagicianTextField(dueDate, { dueDate = it }, Modifier.fillMaxWidth(), label = { Text("Due date · YYYY-MM-DD") }, singleLine = true) }
            item { MagicianTextField(tags, { tags = it }, Modifier.fillMaxWidth(), label = { Text("Tags · comma-separated") }) }
            item { ChoiceField("Output mode", outputMode, listOf("accumulate", "overwrite"), { it.replaceFirstChar(Char::uppercase) }) { outputMode = it } }
            item { MagicianTextField(dependsOn, { dependsOn = it }, Modifier.fillMaxWidth(), label = { Text("Depends on · task ids") }) }
            item { SectionLabel("Schedule") }
            item { ChoiceField("Repeat", repeat, listOf("none", "daily", "weekdays", "weekly", "custom"), { it.replaceFirstChar(Char::uppercase) }) { repeat = it } }
            if (repeat == "custom") item { MagicianTextField(customCron, { customCron = it }, Modifier.fillMaxWidth(), label = { Text("Cron · five fields") }) }
            if (repeat != "none") {
                item { MagicianTextField(scheduleTimezone, { scheduleTimezone = it }, Modifier.fillMaxWidth(), label = { Text("Timezone") }) }
                if (invalidSchedule) item { Text("Use a five-field cron and a valid timezone such as Asia/Kolkata.", color = Danger) }
                item { MagicianTextField(maxRecords, { maxRecords = it }, Modifier.fillMaxWidth(), label = { Text("Keep last N runs · optional") }, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number)) }
                item { MagicianTextField(maxDays, { maxDays = it }, Modifier.fillMaxWidth(), label = { Text("Keep runs for N days · optional") }, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number)) }
            }
            item {
                Button(shape = MagicanButtonShape,
                    onClick = submit,
                    enabled = canCreate,
                    modifier = Modifier.fillMaxWidth(),
                    colors = magicanButtonColors(),
                ) { if (busy) CircularProgressIndicator(Modifier.size(17.dp), color = Color.White, strokeWidth = 2.dp) else Text("Create Task") }
            }
        }
    }
}

@Composable
private fun ChoiceField(
    label: String,
    value: String,
    options: List<String>,
    display: (String) -> String = { it },
    onChoose: (String) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    Box {
        MagicianTextField(
            value = display(value), onValueChange = {}, readOnly = true,
            modifier = Modifier.fillMaxWidth().clickable { expanded = true },
            label = { Text(label) },
            trailingIcon = { Icon(Icons.Outlined.ArrowDropDown, null) },
        )
        DropdownMenu(expanded, { expanded = false }) {
            options.forEach { option -> DropdownMenuItem(
                text = { Text(display(option)) },
                onClick = { onChoose(option); expanded = false },
            ) }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun TaskActionsSheet(
    task: TaskV3,
    state: TasksUiState,
    viewModel: TasksViewModel,
    onDismiss: () -> Unit,
    onOpen: () -> Unit,
    onConvert: () -> Unit,
    onCancel: () -> Unit,
    onDelete: () -> Unit,
) {
    val currentTask = (state.tasks + state.internalTasks).firstOrNull { it.id == task.id } ?: task
    var title by remember(currentTask.id) { mutableStateOf(currentTask.title) }
    var description by remember(currentTask.id) { mutableStateOf(currentTask.description) }
    var priority by remember(currentTask.id) { mutableStateOf(currentTask.priority.orEmpty()) }
    var dueDate by remember(currentTask.id) { mutableStateOf(currentTask.dueDate.orEmpty()) }
    var tag by remember(currentTask.id) { mutableStateOf("") }
    var cron by remember(currentTask.id) { mutableStateOf(currentTask.scheduleCron.orEmpty()) }
    var timezone by remember(currentTask.id) { mutableStateOf(currentTask.scheduleTimezone ?: ZoneId.systemDefault().id) }
    var maxRecords by remember(currentTask.id) { mutableStateOf(currentTask.scheduleRetentionMaxRecords?.toString().orEmpty()) }
    var maxDays by remember(currentTask.id) { mutableStateOf(currentTask.scheduleRetentionMaxDays?.toString().orEmpty()) }
    val busy = state.mutatingTaskId != null || state.publishingTaskId != null
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = Ground, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        LazyColumn(
            Modifier.fillMaxWidth().padding(horizontal = 18.dp).testTag("task-actions-scroll"),
            verticalArrangement = Arrangement.spacedBy(10.dp),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(bottom = 30.dp),
        ) {
            item { SheetTitle("Task actions", onDismiss) }
            item {
                Text(currentTask.title.ifBlank { "Untitled task" }, color = Ink, fontSize = 17.sp, fontWeight = FontWeight.Bold)
                StatusPill(currentTask.statusLabel, taskStatusTint(currentTask.status))
            }
            item { OutlinedButton(shape = MagicanButtonShape, onClick = onOpen, modifier = Modifier.fillMaxWidth()) { Text("Open full task") } }
            item { QuickActions(currentTask, viewModel, busy) }
            if (currentTask.canPublishToNotes) item {
                OutlinedButton(shape = MagicanButtonShape, onClick = { viewModel.publishToNotes(currentTask) }, enabled = !busy, modifier = Modifier.fillMaxWidth()) {
                    Text(if (state.publishingTaskId == currentTask.id) "Publishing…" else "Publish to Notes")
                }
            }
            if (currentTask.canConvertToMonitor) item {
                OutlinedButton(shape = MagicanButtonShape, onClick = onConvert, enabled = !busy, modifier = Modifier.fillMaxWidth()) { Text("Convert to monitor") }
            }
            if (currentTask.hasPlan) {
                item { SectionLabel("Plan") }
                if (currentTask.planAwaitsReview) item {
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        Button(shape = MagicanButtonShape, colors = magicanButtonColors(), onClick = { viewModel.approvePlan(currentTask) }, enabled = !busy, modifier = Modifier.weight(1f)) { Text("Approve") }
                        OutlinedButton(shape = MagicanButtonShape, onClick = { viewModel.rejectPlan(currentTask) }, enabled = !busy, modifier = Modifier.weight(1f)) { Text("Reject", color = Danger) }
                    }
                }
                item { OutlinedButton(shape = MagicanButtonShape, onClick = { viewModel.replan(currentTask) }, enabled = !busy) { Text("Replan") } }
            }
            item { SectionLabel("Title") }
            item { MagicianTextField(title, { title = it }, Modifier.fillMaxWidth(), label = { Text("Task title") }) }
            item { OutlinedButton(shape = MagicanButtonShape, onClick = { viewModel.updateTitle(currentTask, title) }, enabled = !busy && title.isNotBlank() && title.trim() != currentTask.title) { Text("Save title") } }
            item { TaskThreadMove(currentTask, viewModel, busy, onDismiss) }
            item { SectionLabel("Description") }
            item { MagicianTextField(description, { description = it }, Modifier.fillMaxWidth(), minLines = 3) }
            item { OutlinedButton(shape = MagicanButtonShape, onClick = { viewModel.updateDescription(currentTask, description) }, enabled = !busy && description != currentTask.description) { Text("Save description") } }
            item { SectionLabel("Priority") }
            item { ChoiceField("Priority", priority, listOf("", "p1", "p2", "p3", "p4"), { if (it.isEmpty()) "None" else it.uppercase() }) { priority = it; viewModel.updatePriority(currentTask, it.ifBlank { null }) } }
            item { SectionLabel("Due date") }
            item {
                Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(7.dp)) {
                    listOf("Today" to 0L, "Tomorrow" to 1L, "Next week" to 7L).forEach { (name, days) ->
                        OutlinedButton(shape = MagicanButtonShape, onClick = { viewModel.updateDueDate(currentTask, LocalDate.now().plusDays(days).toString()) }) { Text(name, fontSize = 11.sp) }
                    }
                }
            }
            item { MagicianTextField(dueDate, { dueDate = it }, Modifier.fillMaxWidth(), label = { Text("YYYY-MM-DD") }) }
            item {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(shape = MagicanButtonShape, onClick = { viewModel.updateDueDate(currentTask, dueDate.ifBlank { null }) }) { Text("Set date") }
                    if (currentTask.dueDate != null) TextButton(onClick = { viewModel.updateDueDate(currentTask, null) }) { Text("Remove", color = Danger) }
                }
            }
            item { SectionLabel("Tags") }
            items(currentTask.tags, key = TaskTag::name) { existing ->
                Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                    Icon(Icons.Outlined.Tag, null, tint = Muted, modifier = Modifier.size(16.dp)); Spacer(Modifier.width(7.dp))
                    Text(existing.name, modifier = Modifier.weight(1f))
                    IconButton(onClick = { viewModel.removeTag(currentTask, existing.name) }) { Icon(Icons.Outlined.Cancel, "Remove ${existing.name}", tint = Danger) }
                }
            }
            item {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    MagicianTextField(tag, { tag = it }, Modifier.weight(1f), label = { Text("Add a tag") })
                    Button(shape = MagicanButtonShape, colors = magicanButtonColors(), onClick = { viewModel.addTag(currentTask, tag); tag = "" }, enabled = tag.isNotBlank()) { Text("Add") }
                }
            }
            item { SectionLabel("Schedule") }
            if (currentTask.scheduleCron != null) item {
                Text(if (currentTask.schedulePaused) "Schedule paused. Execution controls are independent." else "Schedule active. Execution controls are independent.", color = Muted)
                OutlinedButton(shape = MagicanButtonShape, onClick = { viewModel.setSchedulePaused(currentTask, !currentTask.schedulePaused) }, enabled = !busy) { Text(if (currentTask.schedulePaused) "Resume schedule" else "Pause schedule") }
            }
            item { MagicianTextField(cron, { cron = it }, Modifier.fillMaxWidth(), label = { Text("Cron · 0 9 * * *") }) }
            item { MagicianTextField(timezone, { timezone = it }, Modifier.fillMaxWidth(), label = { Text("Timezone") }) }
            item { MagicianTextField(maxRecords, { maxRecords = it }, Modifier.fillMaxWidth(), label = { Text("Maximum saved runs") }, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number)) }
            item { MagicianTextField(maxDays, { maxDays = it }, Modifier.fillMaxWidth(), label = { Text("Maximum history days") }, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number)) }
            item { OutlinedButton(shape = MagicanButtonShape, onClick = { viewModel.updateSchedule(currentTask, cron, timezone, maxRecords, maxDays) }, enabled = !busy) { Text(if (currentTask.scheduleCron != null && cron.isBlank()) "Remove schedule" else "Save schedule") } }
            item { SectionLabel("Task management") }
            if (currentTask.status.lowercase() !in setOf("completed", "cancelled", "canceled")) item {
                TextButton(onClick = onCancel) { Icon(Icons.Outlined.Cancel, null, tint = Warning); Spacer(Modifier.width(6.dp)); Text("Cancel task", color = Warning) }
            }
            if (!currentTask.isAppManaged) item { TextButton(onClick = onDelete) { Icon(Icons.Outlined.Delete, null, tint = Danger); Spacer(Modifier.width(6.dp)); Text("Delete…", color = Danger) } }
        }
    }
}

@Composable
private fun QuickActions(task: TaskV3, viewModel: TasksViewModel, busy: Boolean) {
    Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(5.dp)) {
        when (task.status.lowercase()) {
            "pending" -> {
                CompactTaskButton("PrePlan", Icons.Outlined.EditCalendar, Coral, false, enabled = !busy) {
                    viewModel.preplan(task)
                }
                CompactTaskButton("Run Now", Icons.Outlined.PlayArrow, Coral, false, enabled = !busy) {
                    viewModel.execute(task)
                }
            }
            "ready" -> CompactTaskButton(
                if (task.planStatus == "approved") "Run Plan" else "Run Now",
                Icons.Outlined.PlayArrow,
                Coral,
                true,
                enabled = !busy,
            ) { viewModel.execute(task) }
            "paused", "failed", "cancelled", "canceled" -> if (task.canResetToReady) CompactTaskButton(
                "Reset to Ready",
                Icons.Outlined.RestartAlt,
                Coral,
                task.status != "paused",
                enabled = !busy,
            ) { viewModel.setStatus(task, "ready") }
            "running", "planning", "executing", "queued" -> task.activeExecutionIdForControls?.let { id ->
                CompactTaskButton("Stop execution", Icons.Outlined.StopCircle, Danger, false, enabled = !busy) {
                    viewModel.executionControl(id, ExecutionControlAction.Cancel)
                }
            }
            "completed" -> CompactTaskButton("Mark not done", Icons.Outlined.RestartAlt, Info, false, enabled = !busy) {
                viewModel.setStatus(task, "ready")
            }
        }
        if (task.canMarkCompleteManually && task.status != "completed") {
            CompactTaskButton("Mark complete", Icons.Outlined.CheckCircle, Success, false, enabled = !busy) {
                viewModel.setStatus(task, "completed")
            }
        }
        if (task.synthesisFailed && !task.isAppManaged) {
            CompactTaskButton("Retry synthesis", Icons.Outlined.RestartAlt, Danger, false, enabled = !busy) {
                viewModel.retrySynthesis(task)
            }
        }
    }
}

@Composable private fun SectionLabel(label: String) = Text(label.uppercase(), color = Muted, fontSize = 11.sp, fontWeight = FontWeight.Bold)

@Composable
private fun SheetTitle(title: String, onDismiss: () -> Unit, dismissLabel: String = "Done", trailing: (@Composable () -> Unit)? = null) {
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Text(title, color = Ink, fontSize = 19.sp, fontWeight = FontWeight.Bold, modifier = Modifier.weight(1f))
        trailing?.invoke()
        TextButton(onClick = onDismiss) { Text(dismissLabel, color = Coral) }
    }
}

@Composable
private fun DestructiveTaskDialog(
    task: TaskV3,
    action: TaskSwipeAction,
    onDismiss: () -> Unit,
    onConfirm: (Boolean) -> Unit,
) {
    var removeFiles by remember { mutableStateOf(false) }
    val deleting = action == TaskSwipeAction.Delete
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(if (deleting) "Delete this task?" else "Cancel this task?") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(if (deleting) "\"${task.title}\" and its saved task state will be removed." else "\"${task.title}\" will stop and move to Cancelled.")
                if (deleting && !task.isInternal) Row(verticalAlignment = Alignment.CenterVertically) {
                    Checkbox(removeFiles, { removeFiles = it }, colors = magicanCheckboxColors()); Text("Also delete the task folder and files")
                }
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Keep task") } },
        confirmButton = { Button(shape = MagicanButtonShape, onClick = { onConfirm(removeFiles) }, colors = ButtonDefaults.buttonColors(containerColor = Danger, contentColor = chatContrastingTextColor(Danger))) { Text(if (deleting) "Delete task" else "Cancel task") } },
    )
}

private fun csv(value: String): List<String> = value.split(',').map(String::trim).filter(String::isNotEmpty)

internal fun taskSchedule(repeat: String, maxRecords: String, maxDays: String, customCron: String = "", timezone: String = ZoneId.systemDefault().id) = when (repeat) {
    "daily" -> "0 9 * * *"; "weekdays" -> "0 9 * * 1-5"; "weekly" -> "0 9 * * 1"; "custom" -> customCron.trim(); else -> null
}?.let { cron ->
    val zone = timezone.trim()
    buildJsonObject {
        put("timezone", zone)
        put("kind", buildJsonObject { put("Cron", buildJsonObject { put("expression", cron); put("timezone", zone) }) })
        val records = maxRecords.toIntOrNull()?.takeIf { it > 0 }
        val days = maxDays.toIntOrNull()?.takeIf { it > 0 }
        if (records != null || days != null) put("execution_history_retention", buildJsonObject {
            records?.let { put("max_records", it) }; days?.let { put("max_age_days", it) }
        })
    }
}
