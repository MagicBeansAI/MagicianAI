package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.tasks.ExecutionControlAction
import ai.magicbeans.magdroid.tasks.TaskDetailBundle
import ai.magicbeans.magdroid.tasks.TaskAttention
import ai.magicbeans.magdroid.tasks.TaskAttentionSource
import ai.magicbeans.magdroid.tasks.TaskDelegationGroup
import ai.magicbeans.magdroid.tasks.TaskDetailAct
import ai.magicbeans.magdroid.tasks.TaskDetailSection
import ai.magicbeans.magdroid.tasks.TaskOutputGrouping
import ai.magicbeans.magdroid.tasks.TaskOutputScope
import ai.magicbeans.magdroid.tasks.TaskTimeline
import ai.magicbeans.magdroid.tasks.TaskTimelineMode
import ai.magicbeans.magdroid.tasks.TaskTimelineSegment
import ai.magicbeans.magdroid.tasks.TaskVerdict
import ai.magicbeans.magdroid.tasks.TaskVerdictInput
import ai.magicbeans.magdroid.tasks.TaskVerdictSeverity
import ai.magicbeans.magdroid.tasks.TaskVerdictValue
import ai.magicbeans.magdroid.tasks.TaskV3
import ai.magicbeans.magdroid.tasks.TasksUiState
import ai.magicbeans.magdroid.tasks.TasksViewModel
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.AccountTree
import androidx.compose.material.icons.automirrored.outlined.Article
import androidx.compose.material.icons.outlined.Cancel
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.Share
import androidx.compose.material.icons.outlined.Code
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.History
import androidx.compose.material.icons.outlined.Info
import androidx.compose.material.icons.outlined.ExpandLess
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.automirrored.outlined.OpenInNew
import androidx.compose.material.icons.outlined.Pause
import androidx.compose.material.icons.outlined.PlayArrow
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.RestartAlt
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material.icons.outlined.StopCircle
import androidx.compose.material.icons.outlined.Tune
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import kotlinx.coroutines.launch
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonPrimitive

private enum class DetailTab(val label: String) {
    Overview("Overview"), Plan("Plan"), Run("Run"), Output("Output"), History("History")
}

@Composable
internal fun TaskDetailScreen(state: TasksUiState, viewModel: TasksViewModel) {
    val detailState = state.detail
    val seed = (state.tasks + state.internalTasks).firstOrNull { it.id == detailState.taskId }
        ?: TaskV3(id = detailState.taskId.orEmpty(), title = "Task")
    var selectedTab by remember(detailState.taskId) { mutableStateOf<DetailTab?>(null) }
    var showSteer by remember { mutableStateOf<String?>(null) }
    var actionsExpanded by remember(detailState.taskId) { mutableStateOf(false) }
    var confirmDelete by remember(detailState.taskId) { mutableStateOf(false) }
    val detailError = detailState.error
    val loadedBundle = detailState.bundle
    val projection = remember(loadedBundle, seed) { loadedBundle?.let { TaskDetailProjection(seed, it) } }
    val actionTask = projection?.actionTask ?: seed
    val busy = state.mutatingTaskId != null || state.publishingTaskId != null

    Column(Modifier.fillMaxSize().background(Ground)) {
        Row(
            Modifier.fillMaxWidth().background(Panel).padding(horizontal = 5.dp, vertical = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton(onClick = viewModel::closeTaskDetail) { Icon(Icons.AutoMirrored.Outlined.ArrowBack, "Back to tasks", tint = Ink) }
            Text("Task detail", color = Ink, fontSize = 17.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
            if (actionTask.canPublishToNotes) {
                IconButton(
                    onClick = { viewModel.publishToNotes(actionTask) },
                    enabled = !busy,
                ) { Icon(Icons.AutoMirrored.Outlined.Article, "Publish to Notes", tint = Coral) }
            }
            IconButton(onClick = { viewModel.openTask(seed, detailState.selectedExecutionId, detailState.preferredAct, detailState.preferredSection) }) { Icon(Icons.Outlined.Refresh, "Refresh", tint = Coral) }
            Box {
                IconButton(onClick = { actionsExpanded = true }, enabled = !busy) {
                    Icon(Icons.Outlined.MoreVert, "Task actions", tint = Ink)
                }
                DropdownMenu(actionsExpanded, { actionsExpanded = false }) {
                    if (actionTask.status == "completed") {
                        DropdownMenuItem(
                            text = { Text("Mark not done") },
                            onClick = { actionsExpanded = false; viewModel.setStatus(actionTask, "ready") },
                        )
                    } else if (actionTask.canMarkCompleteManually) {
                        DropdownMenuItem(
                            text = { Text("Mark complete") },
                            onClick = { actionsExpanded = false; viewModel.setStatus(actionTask, "completed") },
                        )
                    }
                    if (actionTask.status == "pending") DropdownMenuItem(
                        text = { Text("Plan") },
                        onClick = { actionsExpanded = false; viewModel.preplan(actionTask) },
                    )
                    // One Reset only: the header's primary button owns it whenever it shows.
                    if (actionTask.canResetToReady && !detailHeaderShowsReset(actionTask)) {
                        DropdownMenuItem(
                            text = { Text("Reset to Ready") },
                            onClick = { actionsExpanded = false; viewModel.setStatus(actionTask, "ready") },
                        )
                    }
                    if (actionTask.hasPlan) {
                        DropdownMenuItem(
                            text = { Text("Open plan") },
                            onClick = { actionsExpanded = false; selectedTab = DetailTab.Plan },
                        )
                        DropdownMenuItem(
                            text = { Text("Replan") },
                            onClick = { actionsExpanded = false; viewModel.replan(actionTask) },
                        )
                    }
                    if (actionTask.status !in setOf("completed", "cancelled", "canceled")) {
                        DropdownMenuItem(
                            text = { Text("Cancel task") },
                            onClick = { actionsExpanded = false; viewModel.setStatus(actionTask, "cancelled") },
                        )
                    }
                    DropdownMenuItem(
                        text = { Text("Delete", color = DTDanger) },
                        onClick = { actionsExpanded = false; confirmDelete = true },
                    )
                }
            }
        }
        state.actionNotice?.let { DetailNoticeBar(it) }
        when {
            detailState.loading -> DetailPlaceholder("Loading task detail…", true)
            detailError != null -> DetailPlaceholder(detailError, false) { viewModel.openTask(seed, detailState.selectedExecutionId) }
            loadedBundle != null -> {
                val projection = requireNotNull(projection)
                val visibleTabs = projection.visibleTabs
                val preferredTab = detailState.preferredAct?.toDetailTab()?.takeIf { it in visibleTabs }
                val tab = selectedTab?.takeIf { it in visibleTabs } ?: preferredTab ?: projection.defaultTab
                // Every tab owns one scrolling page, including its task context.
                // A long title must not pin the result into a small nested pane.
                val header: @Composable () -> Unit = {
                    Column {
                        DetailHeader(projection, projection.actionTask, state, viewModel)
                        ExecutionSelector(projection, seed, detailState.selectedExecutionId, viewModel)
                        LazyRow(
                            Modifier.fillMaxWidth().background(Panel),
                            contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 10.dp, vertical = 6.dp),
                            horizontalArrangement = Arrangement.spacedBy(6.dp),
                        ) {
                            items(visibleTabs) { candidate ->
                                FilterChip(
                                    selected = tab == candidate,
                                    onClick = { selectedTab = candidate },
                                    label = { Text(candidate.label, fontSize = 12.sp) },
                                )
                            }
                        }
                        if (loadedBundle.unavailableSections.isNotEmpty()) {
                            PartialNotice(loadedBundle.unavailableSections)
                        }
                    }
                }
                when (tab) {
                    DetailTab.Overview -> OverviewTab(projection, projection.actionTask, viewModel, header)
                    DetailTab.Run -> RunTab(projection, state, viewModel, onSteer = { showSteer = it }, header = header)
                    DetailTab.Output -> OutputTab(
                        projection,
                        projection.actionTask,
                        viewModel,
                        header = header,
                        scrollToResult = detailState.preferredSection == TaskDetailSection.Result,
                    )
                    DetailTab.Plan -> PlanTab(projection, projection.actionTask, viewModel, header)
                    DetailTab.History -> HistoryTab(projection, projection.actionTask, viewModel, header)
                }
            }
        }
    }

    showSteer?.let { executionId ->
        var guidance by remember(executionId) { mutableStateOf("") }
        val bytes = guidance.toByteArray().size
        AlertDialog(
            onDismissRequest = { showSteer = null },
            title = { Text("Steer this run") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text("Give the active execution new guidance without restarting it.", color = Muted, fontSize = 12.sp)
                    MagicianTextField(guidance, { guidance = it }, Modifier.fillMaxWidth(), minLines = 3, maxLines = 7)
                    Text("$bytes / 4,096 bytes", color = if (bytes > 4_096) DTDanger else Muted, fontSize = 11.sp)
                }
            },
            dismissButton = { TextButton(onClick = { showSteer = null }) { Text("Cancel") } },
            confirmButton = { Button(shape = MagicanButtonShape,
                onClick = { viewModel.executionControl(executionId, ExecutionControlAction.Steer, guidance); showSteer = null },
                enabled = guidance.isNotBlank() && bytes <= 4_096,
            ) { Text("Send guidance") } },
        )
    }
    if (confirmDelete) AlertDialog(
        onDismissRequest = { confirmDelete = false },
        title = { Text("Delete this task?") },
        text = { Text("This removes the task record and cannot be undone.") },
        dismissButton = { TextButton(onClick = { confirmDelete = false }) { Text("Keep task") } },
        confirmButton = { Button(shape = MagicanButtonShape,
            onClick = {
                viewModel.deleteTask(actionTask, false) { deleted ->
                    if (deleted) {
                        confirmDelete = false
                        viewModel.closeTaskDetail()
                    }
                }
            },
            enabled = !busy,
            colors = ButtonDefaults.buttonColors(containerColor = DTDanger),
        ) { Text("Delete") } },
    )
}

@Composable
private fun DetailNoticeBar(message: String) {
    Row(
        Modifier.fillMaxWidth().background(DTSuccess.copy(alpha = .12f)).padding(9.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(Icons.Outlined.CheckCircle, null, tint = DTSuccess, modifier = Modifier.size(16.dp))
        Spacer(Modifier.width(7.dp))
        Text(message, color = DTSuccess, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
    }
}

@Composable
private fun DetailPlaceholder(message: String, loading: Boolean, retry: (() -> Unit)? = null) {
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(9.dp)) {
            if (loading) CircularProgressIndicator(color = Coral) else Icon(Icons.Outlined.ErrorOutline, null, tint = Muted, modifier = Modifier.size(34.dp))
            Text(message, color = Ink, fontWeight = FontWeight.SemiBold)
            retry?.let { OutlinedButton(shape = MagicanButtonShape, onClick = it) { Text("Try again") } }
        }
    }
}

@Composable
private fun DetailHeader(projection: TaskDetailProjection, task: TaskV3, state: TasksUiState, viewModel: TasksViewModel) {
    Column(Modifier.fillMaxWidth().background(Panel).padding(13.dp), verticalArrangement = Arrangement.spacedBy(7.dp)) {
        Row(verticalAlignment = Alignment.Top) {
            Column(Modifier.weight(1f)) {
                Text(projection.title, color = Ink, fontSize = 20.sp, fontWeight = FontWeight.Bold)
                if (projection.description.isNotBlank()) Text(projection.description, color = Muted, fontSize = 13.sp, maxLines = 3, overflow = TextOverflow.Ellipsis)
            }
            DetailPill(projection.status.replaceFirstChar(Char::uppercase), detailStatusTint(projection.status))
        }
        Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            projection.agentId.takeIf(String::isNotBlank)?.let { DetailPill(it, DTInfo) }
            projection.threadId.takeIf(String::isNotBlank)?.let { DetailPill("#$it", Coral) }
            projection.priority?.let { DetailPill(it.uppercase(), DTDanger) }
            projection.dueDate?.let { DetailPill(it, Coral) }
            projection.progress?.let { DetailPill("$it%", DTSuccess) }
        }
        if (task.isRecurring) {
            val description = task.recurringDescription
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.semantics(mergeDescendants = true) {
                    contentDescription = description?.let { "Recurring, $it" } ?: "Recurring task"
                },
            ) {
                DetailPill(TASK_RECURRING_LABEL, DTInfo)
                description?.let {
                    Spacer(Modifier.width(6.dp))
                    Text(it, color = Muted, fontSize = 12.sp)
                }
            }
        }
        TaskVerdictCard(projection.verdict)
        projection.progress?.let { progress ->
            Row(verticalAlignment = Alignment.CenterVertically) {
                LinearProgressIndicator(
                    progress = { progress.coerceIn(0, 100) / 100f },
                    modifier = Modifier.weight(1f),
                    color = Coral,
                )
                Spacer(Modifier.width(8.dp))
                Text("$progress%", color = Muted, fontSize = 11.sp, fontWeight = FontWeight.SemiBold)
            }
        }
        Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(7.dp)) {
            when {
                detailHeaderShowsPlanReview(task) -> {
                    Button(shape = MagicanButtonShape, onClick = { viewModel.approvePlan(task) }, enabled = state.mutatingTaskId == null) { Text("Approve plan") }
                    OutlinedButton(shape = MagicanButtonShape, onClick = { viewModel.rejectPlan(task) }, enabled = state.mutatingTaskId == null) { Text("Reject", color = DTDanger) }
                }
                task.status == "pending" -> {
                    OutlinedButton(shape = MagicanButtonShape, onClick = { viewModel.preplan(task) }) { Text("PrePlan") }
                    Button(shape = MagicanButtonShape, onClick = { viewModel.execute(task) }) { Text("Run Now") }
                }
                task.status == "ready" -> Button(shape = MagicanButtonShape, onClick = { viewModel.execute(task) }) { Text(if (task.planStatus == "approved") "Run Plan" else "Run Now") }
                detailHeaderShowsReset(task) -> OutlinedButton(shape = MagicanButtonShape, onClick = { viewModel.setStatus(task, "ready") }) {
                    Icon(Icons.Outlined.RestartAlt, null, modifier = Modifier.size(16.dp)); Spacer(Modifier.width(4.dp)); Text("Reset")
                }
            }
            if (task.canMarkCompleteManually && task.status != "completed") OutlinedButton(shape = MagicanButtonShape, onClick = { viewModel.setStatus(task, "completed") }) {
                Icon(Icons.Outlined.CheckCircle, null, tint = DTSuccess, modifier = Modifier.size(16.dp)); Spacer(Modifier.width(4.dp)); Text("Complete", color = DTSuccess)
            }
            if (task.synthesisFailed) OutlinedButton(shape = MagicanButtonShape, onClick = { viewModel.retrySynthesis(task) }) { Text("Retry synthesis", color = DTDanger) }
        }
    }
}

@Composable
private fun TaskVerdictCard(verdict: TaskVerdictValue) {
    val tint = when (verdict.severity) {
        TaskVerdictSeverity.Attention -> DTWarning
        TaskVerdictSeverity.Failure -> DTDanger
        TaskVerdictSeverity.Progress -> Coral
        TaskVerdictSeverity.Neutral -> Muted
        TaskVerdictSeverity.Success -> DTSuccess
    }
    Row(
        Modifier.fillMaxWidth().background(tint.copy(alpha = .10f), RoundedCornerShape(10.dp)).padding(10.dp),
        verticalAlignment = Alignment.Top,
    ) {
        Icon(Icons.Outlined.Info, null, tint = tint, modifier = Modifier.size(18.dp))
        Spacer(Modifier.width(8.dp))
        Column(Modifier.weight(1f)) {
            Text(verdict.headline, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.Bold)
            if (verdict.detail.isNotBlank()) {
                Text(
                    verdict.detail,
                    color = Muted,
                    fontSize = 11.sp,
                    lineHeight = 14.sp,
                    maxLines = TASK_VERDICT_DETAIL_MAX_LINES,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
    }
}

@Composable
private fun ExecutionSelector(
    projection: TaskDetailProjection,
    task: TaskV3,
    selectedId: String?,
    viewModel: TasksViewModel,
) {
    if (projection.executions.size <= 1) return
    var expanded by remember { mutableStateOf(false) }
    Box(Modifier.fillMaxWidth().background(Panel).padding(horizontal = 12.dp, vertical = 5.dp)) {
        OutlinedButton(shape = MagicanButtonShape, onClick = { expanded = true }, modifier = Modifier.fillMaxWidth()) {
            Icon(Icons.Outlined.History, null, modifier = Modifier.size(16.dp)); Spacer(Modifier.width(6.dp))
            Text("Run ${shortId(selectedId ?: projection.selectedExecutionId ?: projection.executions.first().id)}", maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        DropdownMenu(expanded, { expanded = false }) {
            projection.executions.forEach { execution -> DropdownMenuItem(
                text = { Text("${shortId(execution.id)} · ${execution.status}") },
                onClick = { viewModel.selectDetailExecution(task, execution.id); expanded = false },
            ) }
        }
    }
}

@Composable
private fun PartialNotice(sections: Set<String>) {
    Row(Modifier.fillMaxWidth().background(DTWarning.copy(alpha = .11f)).padding(9.dp), verticalAlignment = Alignment.CenterVertically) {
        Icon(Icons.Outlined.Info, null, tint = DTWarning, modifier = Modifier.size(16.dp)); Spacer(Modifier.width(6.dp))
        Text("Some sections are unavailable: ${sections.joinToString()}.", color = DTWarning, fontSize = 11.sp)
    }
}

@Composable
private fun OverviewTab(projection: TaskDetailProjection, task: TaskV3, viewModel: TasksViewModel, header: @Composable () -> Unit) {
    LazyColumn(
        Modifier.fillMaxSize(), contentPadding = androidx.compose.foundation.layout.PaddingValues(12.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        item(key = "task-header") { header() }
        if (projection.description.isNotBlank()) item {
            DetailCard("Description", Icons.Outlined.Description) {
                Text(projection.description, color = Ink, fontSize = 13.sp)
            }
        }
        projection.resultSummary?.let { item { DetailCard("Result", Icons.Outlined.CheckCircle) { Text(it, color = Ink, fontSize = 13.sp) } } }
        projection.runSummary?.let { item { DetailCard("Run summary", Icons.Outlined.Description) { Text(it, color = Ink, fontSize = 13.sp) } } }
        item {
            DetailCard("At a glance", Icons.Outlined.Info) {
                MetricGrid(listOf(
                    "Status" to projection.status,
                    "Progress" to projection.progress?.let { "$it%" }.orEmpty(),
                    "Current step" to projection.currentStep?.toString().orEmpty(),
                    "Outputs" to projection.outputGroups.deliverables.size.toString(),
                    "Runs" to projection.executions.size.toString(),
                    "Observations" to projection.observationCount.toString(),
                ).filter { it.second.isNotBlank() })
            }
        }
        if (projection.questions.isNotEmpty() || task.needsAnswer) item {
            DetailCard("Needs your attention", Icons.Outlined.ErrorOutline) {
                projection.questions.ifEmpty {
                    listOf(DetailQuestion(task.pendingQuestion?.display().orEmpty(), "pending", emptyList()))
                }.forEach { question ->
                    Text(question.text, color = Ink, fontWeight = FontWeight.SemiBold, fontSize = 13.sp)
                    if (question.options.isNotEmpty()) Text(question.options.joinToString(" · "), color = Muted, fontSize = 12.sp)
                    Text("Answer from the linked Chat escalation card.", color = DTWarning, fontSize = 11.sp)
                }
            }
        }
        projection.responsibility?.let { responsibility -> item {
            DetailCard("Responsibility", Icons.Outlined.AccountTree) {
                Metadata("Owner", responsibility.owner)
                Metadata("Waiting state", responsibility.waitingState)
                responsibility.stage?.let { Metadata("Stage", it) }
                responsibility.provider?.let { Metadata("Provider", it) }
                responsibility.children.forEach { child -> Metadata(if (child.blocking) "Blocking child" else "Child", "${child.title} · ${child.owner} · ${child.waitingState}") }
            }
        } }
        item {
            DetailCard("Task metadata", Icons.AutoMirrored.Outlined.Article) {
                Metadata("Task id", task.id, mono = true)
                projection.selectedExecutionId?.let { Metadata("Execution", it, mono = true) }
                Metadata("Agent", projection.agentId)
                Metadata("Thread", projection.threadId)
                projection.createdAt?.let { Metadata("Created", it) }
                projection.updatedAt?.let { Metadata("Updated", it) }
            }
        }
    }
}

@Composable
private fun RunTab(projection: TaskDetailProjection, state: TasksUiState, viewModel: TasksViewModel, onSteer: (String) -> Unit, header: @Composable () -> Unit) {
    LazyColumn(
        Modifier.fillMaxSize(), contentPadding = androidx.compose.foundation.layout.PaddingValues(12.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        item(key = "task-header") { header() }
        projection.activeExecutionId?.let { executionId -> item {
            DetailExecutionControls(executionId, state, viewModel, onSteer)
        } }
        if (projection.steps.isEmpty()) item { EmptyDetail("No step timeline is available for this run.") }
        else item { DetailCard("Steps", Icons.Outlined.AccountTree) { projection.steps.forEach { StepRow(it) } } }
        if (projection.activity.isEmpty()) item { EmptyDetail("No activity has been recorded yet.") }
        else item { ActivityTimelineCard(projection) }
        if (projection.shellEntries.isNotEmpty()) item {
            DetailCard("Shell", Icons.Outlined.Code) { projection.shellEntries.forEach { ShellRow(it) } }
        }
        projection.executionError?.let { item { InlineDetail(it, DTDanger, Icons.Outlined.ErrorOutline) } }
    }
}

@Composable
private fun DetailExecutionControls(executionId: String, state: TasksUiState, viewModel: TasksViewModel, onSteer: (String) -> Unit) {
    val ui = state.executionControls[executionId]
    LaunchedEffect(executionId) { viewModel.loadExecutionControls(executionId) }
    DetailCard("Execution controls", Icons.Outlined.Tune) {
        if (ui?.loading == true) LinearProgressIndicator(Modifier.fillMaxWidth(), color = Coral)
        ui?.error?.let { Text(it, color = DTDanger, fontSize = 12.sp) }
        ui?.state?.let { controls ->
            Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(7.dp)) {
                if (controls.canPause) OutlinedButton(shape = MagicanButtonShape, onClick = { viewModel.executionControl(executionId, ExecutionControlAction.Pause) }) {
                    Icon(Icons.Outlined.Pause, null, modifier = Modifier.size(15.dp)); Spacer(Modifier.width(4.dp)); Text("Pause")
                }
                if (controls.canResume) Button(shape = MagicanButtonShape, onClick = { viewModel.executionControl(executionId, ExecutionControlAction.Resume) }) {
                    Icon(Icons.Outlined.PlayArrow, null, modifier = Modifier.size(15.dp)); Spacer(Modifier.width(4.dp)); Text("Resume")
                }
                if (controls.canSteer) OutlinedButton(shape = MagicanButtonShape, onClick = { onSteer(executionId) }) { Icon(Icons.Outlined.Tune, null, modifier = Modifier.size(15.dp)); Spacer(Modifier.width(4.dp)); Text("Steer") }
                if (controls.canCancel) OutlinedButton(shape = MagicanButtonShape, onClick = { viewModel.executionControl(executionId, ExecutionControlAction.Cancel) }) {
                    Icon(Icons.Outlined.StopCircle, null, tint = DTDanger, modifier = Modifier.size(15.dp)); Spacer(Modifier.width(4.dp)); Text("Stop", color = DTDanger)
                }
            }
        }
    }
}

@Composable
private fun OutputTab(
    projection: TaskDetailProjection,
    task: TaskV3,
    viewModel: TasksViewModel,
    header: @Composable () -> Unit,
    scrollToResult: Boolean = false,
) {
    val listState = rememberLazyListState()
    // The "Result" entry lands on the Result card explicitly, not by accident of layout.
    LaunchedEffect(task.id, projection.selectedExecutionId, scrollToResult) {
        if (scrollToResult) listState.scrollToItem(1)
    }
    val groups = projection.outputGroups
    var intermediatesOpen by remember(task.id) { mutableStateOf(false) }
    LazyColumn(
        Modifier.fillMaxSize(), state = listState,
        contentPadding = androidx.compose.foundation.layout.PaddingValues(12.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        item(key = "task-header") { header() }
        item(key = "result") {
            DetailCard("Result", Icons.Outlined.CheckCircle) {
                projection.resultSummary?.let { MarkdownText(it, color = Ink, fontSize = 13.sp, lineHeight = 18.sp) }
                projection.resultOutcome?.takeIf { it != projection.resultSummary }?.let {
                    Metadata("Outcome", it)
                }
                if (projection.resultSummary == null && projection.resultOutcome == null) Text(
                    if (projection.synthesisPending) "The run finished and its output is still being synthesized."
                    else "No completion result has been recorded yet.",
                    color = Muted,
                    fontSize = 12.sp,
                )
                if (projection.synthesisPending) {
                    InlineDetail("Output synthesis is running.", Coral, Icons.Outlined.Schedule)
                } else if (projection.synthesisFailed) {
                    InlineDetail("Output synthesis needs attention.", DTDanger, Icons.Outlined.ErrorOutline)
                    OutlinedButton(shape = MagicanButtonShape, onClick = { viewModel.retrySynthesis(task) }) {
                        Text("Retry synthesis", color = DTDanger)
                    }
                }
            }
        }
        item(key = "deliverables-header") {
            OutputSectionHeader(
                "Deliverables",
                if (groups.deliverables.isEmpty()) "No deliverables are available yet." else null,
            )
        }
        items(groups.deliverables, key = { "task:" + it.path }) { ArtifactRow(it, task.id) }
        if (groups.intermediateCount > 0) {
            item(key = "intermediates") {
                val count = groups.intermediateCount
                Row(
                    Modifier.fillMaxWidth()
                        .background(Panel, RoundedCornerShape(10.dp))
                        .clickable { intermediatesOpen = !intermediatesOpen }
                        .padding(horizontal = 12.dp, vertical = 10.dp)
                        .semantics(mergeDescendants = true) {
                            contentDescription = intermediatesTitle(count).replace("&", "and") + ", " +
                                if (intermediatesOpen) "expanded" else "collapsed"
                        },
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(if (intermediatesOpen) Icons.Outlined.ExpandLess else Icons.Outlined.ExpandMore, null, tint = Muted, modifier = Modifier.size(18.dp))
                    Spacer(Modifier.width(6.dp))
                    Text(
                        intermediatesTitle(count),
                        color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold,
                    )
                }
            }
            if (intermediatesOpen) groups.intermediates.forEach { section ->
                item(key = "section:" + section.scope.wire) {
                    OutputSectionHeader(section.scope.title, section.scope.description)
                }
                items(section.files, key = { section.scope.wire + ":" + it.path }) { ArtifactRow(it, task.id) }
            }
        }
        if (projection.deliveries > 0) item { InlineDetail("${projection.deliveries} deliverable${if (projection.deliveries == 1) "" else "s"} projected.", DTSuccess, Icons.Outlined.CheckCircle) }
    }
}

internal fun intermediatesTitle(count: Int): String =
    "Intermediate artifacts & evidence ($count ${if (count == 1) "item" else "items"})"

@Composable
private fun OutputSectionHeader(title: String, description: String?) {
    Column(Modifier.fillMaxWidth().padding(top = 2.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Text(title, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.Bold)
        description?.let { Text(it, color = Muted, fontSize = 11.sp) }
    }
}

/** Activity with delegated children folded into bounded, status-accented envelopes. */
@Composable
private fun ActivityTimelineCard(projection: TaskDetailProjection) {
    val context = LocalContext.current
    var mode by remember { mutableStateOf(TaskTimelinePreference.read(context)) }
    val delegations = projection.delegations
    val delegatedIds = remember(delegations) { delegations.associate { it.executionId to it.agentId } }
    DetailCard("Activity", Icons.Outlined.History) {
        if (delegations.isNotEmpty()) {
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                TaskTimelineMode.entries.forEach { candidate ->
                    FilterChip(
                        selected = mode == candidate,
                        onClick = { mode = candidate; TaskTimelinePreference.write(context, candidate) },
                        label = { Text(candidate.title, fontSize = 11.sp) },
                    )
                }
            }
        }
        if (mode == TaskTimelineMode.Grouped && delegations.isNotEmpty()) {
            val segments = remember(projection.activity, delegations) {
                TaskTimeline.groupByDelegation(projection.activity, delegations, DetailActivity::executionId)
            }
            segments.forEach { segment ->
                when (segment) {
                    is TaskTimelineSegment.Row -> ActivityRow(segment.entry)
                    is TaskTimelineSegment.Delegation -> DelegationEnvelope(segment.group, segment.entries)
                }
            }
        } else {
            projection.activity.forEach { activity ->
                val agent = activity.executionId?.let(delegatedIds::get)
                ActivityRow(if (agent != null) activity.copy(title = "${activity.title} · $agent") else activity)
            }
        }
    }
}

@Composable
private fun DelegationEnvelope(group: TaskDelegationGroup, entries: List<DetailActivity>) {
    var expanded by remember(group.executionId) { mutableStateOf(true) }
    val accent = delegationAccent(group.status, activePalette)
    val span = remember(group, entries) { TaskTimeline.delegationSpan(entries.map(DetailActivity::atMillis), group) }
    val steps = "${entries.size} ${if (entries.size == 1) "step" else "steps"}"
    val meta = listOfNotNull(steps, group.status, span.summary).joinToString(" · ")
    Row(
        Modifier.fillMaxWidth()
            .height(androidx.compose.foundation.layout.IntrinsicSize.Min)
            .background(accent.copy(alpha = .06f), RoundedCornerShape(8.dp)),
    ) {
        Box(Modifier.width(3.dp).fillMaxHeight().background(accent, RoundedCornerShape(topStart = 8.dp, bottomStart = 8.dp)))
        Column(Modifier.weight(1f).padding(horizontal = 9.dp, vertical = 6.dp)) {
            Row(
                Modifier.fillMaxWidth().clickable { expanded = !expanded }.semantics(mergeDescendants = true) {
                    contentDescription = "Delegated to ${group.agentId}, $meta, " + if (expanded) "expanded" else "collapsed"
                },
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f)) {
                    Text("Delegated to ${group.agentId}", color = Ink, fontSize = 12.sp, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Text(meta, color = accent, fontSize = 10.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
                Icon(if (expanded) Icons.Outlined.ExpandLess else Icons.Outlined.ExpandMore, null, tint = Muted, modifier = Modifier.size(18.dp))
            }
            if (expanded) entries.forEach { ActivityRow(it) }
        }
    }
}

/** Envelope accent: running info, done success, failed danger, waiting warning. */
internal fun delegationAccent(status: String, palette: Palette): Color = when (status.lowercase()) {
    "running", "executing", "planning" -> palette.info
    "done", "completed", "succeeded" -> palette.success
    "failed", "cancelled", "canceled", "error" -> palette.danger
    "waiting", "paused", "pending", "queued" -> palette.warning
    else -> palette.secondaryText
}

/** Grouped/Chronological is a per-device reading preference. */
internal object TaskTimelinePreference {
    private const val PREFS = "magdroid.tasks"
    private const val KEY = "timeline_mode"

    fun read(context: android.content.Context): TaskTimelineMode = runCatching {
        TaskTimelineMode.fromWire(context.getSharedPreferences(PREFS, android.content.Context.MODE_PRIVATE).getString(KEY, null))
    }.getOrDefault(TaskTimelineMode.Grouped)

    fun write(context: android.content.Context, mode: TaskTimelineMode) {
        runCatching {
            context.getSharedPreferences(PREFS, android.content.Context.MODE_PRIVATE).edit().putString(KEY, mode.wire).apply()
        }
    }
}

/** The header's primary Reset; the overflow menu omits Reset whenever this is true. */
internal fun detailHeaderShowsReset(task: TaskV3): Boolean = !detailHeaderShowsPlanReview(task) && task.canResetToReady

/** Approve/Reject belongs to a task that can still run; a finished task's stale draft is not a decision. */
internal fun detailHeaderShowsPlanReview(task: TaskV3): Boolean =
    task.planAwaitsReview && task.status.lowercase() !in setOf("completed", "failed", "cancelled", "canceled")

internal const val TASK_VERDICT_DETAIL_MAX_LINES = 3

@Composable
private fun PlanTab(projection: TaskDetailProjection, task: TaskV3, viewModel: TasksViewModel, header: @Composable () -> Unit) {
    LazyColumn(
        Modifier.fillMaxSize(), contentPadding = androidx.compose.foundation.layout.PaddingValues(12.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        item(key = "task-header") { header() }
        item {
            DetailCard("Plan", Icons.Outlined.AccountTree) {
                Metadata("Status", projection.planStatus ?: task.planStatus ?: "No plan")
                projection.planId?.let { Metadata("Plan id", it, mono = true) }
                projection.planMarkdown?.let { Text(it, color = Ink, fontSize = 13.sp, fontFamily = LocalMagicanFontFamilies.current.mono) }
                    ?: Text("No rendered plan is available.", color = Muted, fontSize = 12.sp)
            }
        }
        if (projection.questions.isNotEmpty()) item {
            DetailCard("Plan questions", Icons.Outlined.ErrorOutline) {
                projection.questions.forEach { Text("• ${it.text}", color = Ink, fontSize = 13.sp) }
            }
        }
        item {
            DetailCard("Stepwise plan", Icons.Outlined.AccountTree) {
                if (projection.steps.isEmpty()) {
                    Text("No plan step snapshot is available yet.", color = Muted, fontSize = 12.sp)
                } else {
                    projection.steps.forEach { StepRow(it) }
                }
            }
        }
        item {
            Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(7.dp)) {
                if (task.planAwaitsReview) {
                    Button(shape = MagicanButtonShape, onClick = { viewModel.approvePlan(task) }) { Text("Approve plan") }
                    OutlinedButton(shape = MagicanButtonShape, onClick = { viewModel.rejectPlan(task) }) { Text("Reject plan", color = DTDanger) }
                }
                if (task.hasPlan) OutlinedButton(shape = MagicanButtonShape, onClick = { viewModel.replan(task) }) { Text("Replan") }
                if (task.planStatus == "approved" && task.status == "ready") Button(shape = MagicanButtonShape, onClick = { viewModel.execute(task) }) { Text("Run plan") }
            }
        }
    }
}

@Composable
private fun HistoryTab(projection: TaskDetailProjection, task: TaskV3, viewModel: TasksViewModel, header: @Composable () -> Unit) {
    LazyColumn(
        Modifier.fillMaxSize(), contentPadding = androidx.compose.foundation.layout.PaddingValues(12.dp),
        verticalArrangement = Arrangement.spacedBy(9.dp),
    ) {
        item(key = "task-header") { header() }
        if (projection.executions.isEmpty()) item { EmptyDetail("No execution history is available.") }
        items(projection.executions, key = DetailExecution::id) { execution ->
            Card(
                modifier = Modifier.fillMaxWidth().clickable { viewModel.selectDetailExecution(task, execution.id) },
                colors = CardDefaults.cardColors(containerColor = Panel), border = BorderStroke(1.dp, BorderSoft),
            ) {
                Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(5.dp)) {
                    Row {
                        Text(shortId(execution.id), color = Ink, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
                        DetailPill(execution.status, detailStatusTint(execution.status))
                    }
                    execution.summary?.let { Text(it, color = Muted, fontSize = 12.sp, maxLines = 3, overflow = TextOverflow.Ellipsis) }
                    execution.error?.let { Text(it, color = DTDanger, fontSize = 12.sp) }
                    Text(historySpanLine(execution.startedAt, execution.endedAt), color = Muted, fontSize = 10.sp)
                }
            }
        }
    }
}

@Composable
private fun DetailCard(title: String, icon: androidx.compose.ui.graphics.vector.ImageVector, body: @Composable () -> Unit) {
    Card(colors = CardDefaults.cardColors(containerColor = Panel), border = BorderStroke(1.dp, BorderSoft), shape = RoundedCornerShape(13.dp)) {
        Column(Modifier.fillMaxWidth().padding(13.dp), verticalArrangement = Arrangement.spacedBy(7.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(icon, null, tint = Coral, modifier = Modifier.size(17.dp)); Spacer(Modifier.width(7.dp))
                Text(title, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.Bold)
            }
            HorizontalDivider(color = BorderSoft)
            body()
        }
    }
}

@Composable
private fun MetricGrid(metrics: List<Pair<String, String>>) {
    metrics.chunked(2).forEach { row ->
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            row.forEach { (label, value) ->
                Surface(color = Ground, shape = RoundedCornerShape(9.dp), modifier = Modifier.weight(1f)) {
                    Column(Modifier.padding(9.dp)) {
                        Text(label.uppercase(), color = Muted, fontSize = 9.sp, fontWeight = FontWeight.Bold)
                        Text(value, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                    }
                }
            }
            if (row.size == 1) Spacer(Modifier.weight(1f))
        }
    }
}

@Composable
private fun Metadata(label: String, value: String, mono: Boolean = false) {
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(label, color = Muted, fontSize = 11.sp, modifier = Modifier.width(88.dp))
        Text(value, color = Ink, fontSize = 11.sp, fontFamily = if (mono) LocalMagicanFontFamilies.current.mono else LocalMagicanFontFamilies.current.body, modifier = Modifier.weight(1f))
    }
}

@Composable
private fun StepRow(step: DetailStep) {
    Row(Modifier.fillMaxWidth().padding(vertical = 4.dp), verticalAlignment = Alignment.Top) {
        Icon(if (step.status in setOf("completed", "succeeded")) Icons.Outlined.CheckCircle else Icons.Outlined.Schedule, null,
            tint = detailStatusTint(step.status), modifier = Modifier.size(17.dp))
        Spacer(Modifier.width(8.dp))
        Column(Modifier.weight(1f)) {
            Text(step.name, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
            listOfNotNull(step.progress, step.capability, step.agent).joinToString(" · ").takeIf(String::isNotBlank)?.let { Text(it, color = Muted, fontSize = 11.sp) }
        }
        DetailPill(step.status, detailStatusTint(step.status))
    }
}

@Composable
private fun ActivityRow(activity: DetailActivity) {
    Column(Modifier.fillMaxWidth().padding(vertical = 4.dp)) {
        Row {
            Text(activity.title, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
            (activity.atMillis?.let(::activityClock) ?: activity.timestamp)?.let { Text(it, color = Muted, fontSize = 10.sp) }
        }
        activity.body?.let { Text(it, color = Muted, fontSize = 12.sp, maxLines = 4, overflow = TextOverflow.Ellipsis) }
        listOfNotNull(activity.agent, activity.model, activity.latency?.let { "${it}ms" }, activity.cost?.let { "$${"%.4f".format(it)}" })
            .joinToString(" · ").takeIf(String::isNotBlank)?.let { Text(it, color = DTInfo, fontSize = 10.sp) }
    }
}

@Composable
private fun ShellRow(entry: DetailShellEntry) {
    Surface(color = Color(0xFF22272A), shape = RoundedCornerShape(8.dp)) {
        Column(Modifier.fillMaxWidth().padding(9.dp)) {
            Text("$ ${entry.command}", color = Color.White, fontSize = 11.sp, fontFamily = LocalMagicanFontFamilies.current.mono)
            entry.lines.take(8).forEach { Text(it, color = Color(0xFFD7E0E2), fontSize = 10.sp, fontFamily = LocalMagicanFontFamilies.current.mono) }
            Text(if (entry.complete) "exit ${entry.exitCode ?: 0}" else "running…", color = if (entry.exitCode == 0) DTSuccess else DTWarning, fontSize = 10.sp)
        }
    }
}

@Composable
private fun ArtifactRow(artifact: DetailArtifact, taskId: String) {
    val context = LocalContext.current
    val artifactScope = rememberCoroutineScope()
    Card(colors = CardDefaults.cardColors(containerColor = Panel), border = BorderStroke(1.dp, BorderSoft)) {
        Row(Modifier.fillMaxWidth().padding(12.dp), verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.Outlined.Folder, null, tint = DTInfo, modifier = Modifier.size(20.dp)); Spacer(Modifier.width(8.dp))
            Column(Modifier.weight(1f)) {
                Text(artifact.name, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                listOfNotNull(artifact.mediaType, artifact.sizeBytes?.let(::formatBytes)).joinToString(" · ")
                    .takeIf(String::isNotBlank)?.let { Text(it, color = Muted, fontSize = 10.sp) }
                artifact.snippet?.let { Text(it, color = Muted, fontSize = 11.sp, maxLines = 2, overflow = TextOverflow.Ellipsis) }
            }
            val base = MagicianAccess.baseUrl(context)
            val url = artifact.url?.let { value ->
                if (value.startsWith("http")) value else base.trimEnd('/') + "/" + value.trimStart('/')
            } ?: TaskArtifactLinks.url(base, taskId, artifact.path)
            if (url != null) {
                IconButton(onClick = {
                    artifactScope.launch {
                        AuthenticatedArtifacts.open(context, url, artifact.path, artifact.mediaType)
                    }
                }) { Icon(Icons.AutoMirrored.Outlined.OpenInNew, "Open", tint = Coral) }
                IconButton(onClick = {
                    artifactScope.launch {
                        AuthenticatedArtifacts.share(context, url, artifact.path, artifact.mediaType)
                    }
                }) { Icon(Icons.Outlined.Share, "Share", tint = Muted) }
            }
        }
    }
}

@Composable
private fun InlineDetail(message: String, tint: Color, icon: androidx.compose.ui.graphics.vector.ImageVector) {
    Row(Modifier.fillMaxWidth().background(tint.copy(alpha = .10f), RoundedCornerShape(10.dp)).padding(10.dp), verticalAlignment = Alignment.CenterVertically) {
        Icon(icon, null, tint = tint, modifier = Modifier.size(17.dp)); Spacer(Modifier.width(7.dp)); Text(message, color = tint, fontSize = 12.sp)
    }
}

@Composable
private fun EmptyDetail(message: String) {
    Box(Modifier.fillMaxSize().padding(32.dp), contentAlignment = Alignment.Center) { Text(message, color = Muted, fontSize = 13.sp) }
}

@Composable
private fun DetailPill(label: String, tint: Color) {
    Text(label, color = tint, fontSize = 10.sp, fontWeight = FontWeight.SemiBold,
        modifier = Modifier.background(tint.copy(alpha = .12f), CircleShape).padding(horizontal = 7.dp, vertical = 3.dp))
}

private class TaskDetailProjection(private val seed: TaskV3, private val bundle: TaskDetailBundle) {
    private val taskRecord = bundle.task.obj("task") ?: bundle.details.obj("task") ?: bundle.task.orEmpty()
    private val manifest = taskRecord.obj("manifest") ?: taskRecord
    private val taskState = taskRecord.obj("state") ?: taskRecord
    private val refs = taskRecord.obj("refs").orEmpty()
    private val overview = bundle.panel.obj("overview").orEmpty()
    private val run = bundle.panel.obj("run").orEmpty()
    private val output = bundle.panel.obj("output").orEmpty()
    private val debug = bundle.panel.obj("debug").orEmpty()
    private val selected = debug.obj("selected_execution").orEmpty()
    private val plan = bundle.plan.obj("plan").orEmpty()

    val title = overview.text("title") ?: manifest.text("title") ?: seed.title.ifBlank { "Task" }
    val description = overview.text("description") ?: manifest.text("description") ?: seed.description
    val status = overview.text("status") ?: taskState.text("status") ?: seed.status
    val agentId = overview.text("active_agent_id") ?: overview.text("assigned_agent_id") ?: manifest.text("agent_id") ?: seed.agentId
    val threadId = overview.text("ui_thread_id") ?: manifest.text("ui_thread_id") ?: seed.uiThreadId
    val priority = overview.text("priority") ?: manifest.text("priority") ?: seed.priority
    val dueDate = manifest.text("due_date") ?: seed.dueDate
    val progress = overview.int("progress") ?: selected.int("progress")
    val currentStep = overview.int("current_step") ?: selected.int("current_step")
    val createdAt = overview.text("created_at") ?: manifest.text("created_at") ?: seed.createdAt.takeIf(String::isNotBlank)
    val updatedAt = overview.text("updated_at") ?: taskState.text("updated_at") ?: seed.updatedAt.takeIf(String::isNotBlank)
    val selectedExecutionId = selected.text("execution_id") ?: overview.text("execution_id") ?: seed.latestRootExecutionId
    private val rawActiveExecutionId = taskState.text("active_root_execution_id") ?: seed.activeRootExecutionId
    val activeExecutionId = rawActiveExecutionId?.takeIf {
        status.lowercase() in setOf("queued", "running", "planning", "paused", "executing")
    }
    val runSummary = run.text("summary")
    private val resultObject = output.obj("result")
    private val result = resultObject.orEmpty()
    val resultSummary = result.text("summary")?.takeIf(String::isNotBlank)
        ?: taskState.text("completion_summary")?.takeIf(String::isNotBlank)
        ?: taskRecord.text("completion_summary")?.takeIf(String::isNotBlank)
        ?: seed.completionSummary?.takeIf(String::isNotBlank)
    val resultOutcome = result.text("outcome")?.takeIf(String::isNotBlank)
        ?: taskState.text("completion_outcome")?.takeIf(String::isNotBlank)
        ?: seed.completionOutcome?.takeIf(String::isNotBlank)
    val executionError = selected.text("error_message") ?: debug.text("latest_error_message")
    val planStatus = plan.text("status") ?: seed.planStatus
    val planId = plan.text("plan_id") ?: selected.text("plan_id") ?: seed.latestPlanId
    val planMarkdown = debug.obj("taskplan")?.text("markdown") ?: plan.text("markdown")
    val observationCount = debug.array("observations").size
    val deliveries = output.array("deliveries").size
    val linkedInputCount = selected.array("linked_inputs").size
    val synthesisPending = (taskState.bool("synthesis_pending") ?: false) ||
        taskState.array("synthesis_pending_executions").isNotEmpty() || seed.synthesisPending
    val synthesisFailedExecutionId = taskState.text("synthesis_failed_execution_id")
        ?: seed.synthesisFailedExecutionId
    val synthesisFailed = !synthesisFailedExecutionId.isNullOrBlank()

    val steps: List<DetailStep> = selected.array("step_statuses").mapIndexed { index, element ->
        val row = element as? JsonObject ?: JsonObject(emptyMap())
        DetailStep(
            row.text("step_id") ?: "step-$index", row.int("number") ?: index,
            row.text("name") ?: "Step ${index + 1}",
            row.text("status") ?: "pending", row.text("progress"), row.text("capability"), row.text("delegate_agent_id"),
        )
    }

    val activity: List<DetailActivity> = (run.array("activity_log").ifEmpty { run.array("recent_activity") }.ifEmpty { debug.array("timeline") })
        .mapIndexed { index, element ->
            val row = element as? JsonObject ?: JsonObject(emptyMap())
            val metadata = row.obj("metadata").orEmpty()
            DetailActivity(
                row.text("id") ?: "activity-$index",
                activityTitle(metadata.text("event_type"), metadata.text("target") ?: metadata.text("tool_name"), row.text("title")),
                row.text("summary") ?: row.text("message"), row.text("status") ?: row.text("severity") ?: "info",
                row.text("agent_id"), row.text("created_at") ?: row.text("timestamp"), metadata.text("model"),
                metadata.long("latency_ms"), metadata.double("cost_usd") ?: metadata.double("cost"),
                executionId = metadata.text("execution_id") ?: row.text("execution_id"),
                atMillis = wireInstantMillis(row["created_at"]) ?: wireInstantMillis(row["timestamp"]),
            )
        }

    val questions: List<DetailQuestion> = (run.array("pending_questions") + plan.array("pending_questions"))
        .mapNotNull { element ->
            val row = element as? JsonObject ?: return@mapNotNull null
            val text = row.text("question_text") ?: row.text("question") ?: row.text("prompt") ?: return@mapNotNull null
            DetailQuestion(text, row.text("status") ?: "pending", row.array("options").mapNotNull { option ->
                (option as? JsonObject)?.text("label") ?: (option as? JsonObject)?.text("value")
            })
        }.distinctBy(DetailQuestion::text)

    val responsibility: DetailResponsibility? = run.obj("responsibility")?.let { row ->
        DetailResponsibility(
            row.text("active_owner_agent_id") ?: "unknown", row.text("waiting_state") ?: "unknown",
            row.text("current_stage"), row.text("current_provider"), row.array("active_children").mapIndexed { index, element ->
                val child = element as? JsonObject ?: JsonObject(emptyMap())
                DetailChild(
                    child.text("execution_id") ?: "child-$index", child.text("title") ?: "Child execution",
                    child.text("active_owner_agent_id") ?: "unknown", child.text("waiting_state") ?: "unknown",
                    child.bool("is_blocking") ?: false,
                )
            },
        )
    }

    val shellEntries: List<DetailShellEntry> = debug.array("shell_entries").mapIndexed { index, element ->
        val row = element as? JsonObject ?: JsonObject(emptyMap())
        DetailShellEntry(
            row.text("step_id") ?: "shell-$index", row.text("command") ?: "Command",
            row.array("lines").mapNotNull { (it as? JsonObject)?.text("text") }, row.int("exit_code"), row.bool("is_complete") ?: false,
        )
    }

    val artifacts: List<DetailArtifact> = buildList {
        addAll(parseArtifacts(bundle.outputs.obj("outputs")?.get("outputs")))
        addAll(parseArtifacts(refs["outputs"]))
        addAll(parseArtifacts(bundle.details.obj("task")?.obj("refs")?.get("outputs")))
    }.distinctBy(DetailArtifact::path)

    /**
     * The selected run's own files, scoped like web `selectedRunOutputs`: only
     * when the panel describes that run, and only when the backend serialized
     * the direct/delegated arrays (older payloads cannot be attributed).
     */
    private val runFiles: List<DetailArtifact> = selectedRunFiles()

    private fun selectedRunFiles(): List<DetailArtifact> {
        val panelExecution = overview.text("execution_id")
        val direct = output["selected_execution_outputs"] as? JsonArray
        val delegated = output["selected_child_outputs"] as? JsonArray
        if (panelExecution == null || direct == null || delegated == null) return emptyList()
        return buildList {
            addAll(parseArtifacts(direct, TaskOutputScope.Execution, forceScope = true))
            addAll(parseArtifacts(delegated, TaskOutputScope.Delegated, forceScope = true))
            addAll(parseArtifacts(output["selected_execution_artifacts"], TaskOutputScope.Artifact, forceScope = true))
        }.distinctBy { it.scope.wire + ":" + it.path }
    }

    val outputGroups = TaskOutputGrouping.group(artifacts + runFiles, DetailArtifact::scope)

    val delegations: List<TaskDelegationGroup> = run.array("delegations").mapNotNull { element ->
        val row = element as? JsonObject ?: return@mapNotNull null
        val id = row.text("execution_id")?.takeIf(String::isNotBlank) ?: return@mapNotNull null
        TaskDelegationGroup(
            executionId = id,
            agentId = row.text("agent_id")?.takeIf(String::isNotBlank) ?: "agent",
            status = row.text("status") ?: "unknown",
            entryCount = row.int("entry_count") ?: 0,
            parentExecutionId = row.text("parent_execution_id"),
            startedAt = row.text("started_at"),
            completedAt = row.text("completed_at"),
        )
    }

    val executions: List<DetailExecution> = parseExecutions(bundle.details?.get("executions"))
        .ifEmpty { parseRecentRuns(output["recent_runs"]) }

    private val attention: TaskAttention? = run.array("needs_attention").firstNotNullOfOrNull { element ->
        val row = element as? JsonObject ?: return@firstNotNullOfOrNull null
        val request = row.obj("hitl_request") ?: return@firstNotNullOfOrNull null
        val source = TaskAttentionSource.fromWire(request.text("source"))
            ?: return@firstNotNullOfOrNull null
        TaskAttention(
            source = source,
            summary = request.text("prompt") ?: row.text("summary"),
            raisedAtMillis = wireInstantMillis(request["at"]) ?: wireInstantMillis(row["created_at"]),
        )
    }
    private val selectedStartedAt = selected.text("started_at")
    private val selectedEndedAt = selected.text("ended_at")
    private val elapsedSeconds = runElapsedSeconds(selectedStartedAt, selectedEndedAt)
    private val currentStepLabel = currentStep?.let { index -> steps.firstOrNull { it.number == index }?.name }
    val verdict: TaskVerdictValue = TaskVerdict.derive(TaskVerdictInput(
        status = status,
        attention = attention,
        error = executionError,
        currentStep = currentStep?.plus(1),
        totalSteps = steps.takeIf(List<DetailStep>::isNotEmpty)?.size,
        currentStepLabel = currentStepLabel,
        elapsedSeconds = elapsedSeconds,
        lastProgressAtMillis = wireInstantMillis(taskState["last_progress_at"]),
    ))
    private val acts = TaskVerdict.acts(
        hasPlan = seed.hasPlan || planStatus != null || planId != null || !planMarkdown.isNullOrBlank(),
        hasRun = bundle.panel != null || selectedExecutionId != null || steps.isNotEmpty() ||
            activity.isNotEmpty() || executions.isNotEmpty(),
        hasOutput = bundle.outputs != null || artifacts.isNotEmpty() || runFiles.isNotEmpty() ||
            resultObject != null || resultSummary != null || resultOutcome != null,
    )
    val visibleTabs: List<DetailTab> = buildList {
        add(DetailTab.Overview)
        acts.forEach { add(it.toDetailTab()) }
        add(DetailTab.History)
    }
    val defaultTab: DetailTab = TaskVerdict.defaultOpenAct(verdict.state, acts, attention?.source)
        ?.toDetailTab() ?: DetailTab.Overview
    val actionTask: TaskV3 = seed.copy(
        title = title,
        description = description,
        status = status,
        agentId = agentId,
        uiThreadId = threadId,
        priority = priority,
        dueDate = dueDate,
        planStatus = planStatus,
        latestPlanId = planId,
        hasPlan = seed.hasPlan || planStatus != null || planId != null || !planMarkdown.isNullOrBlank(),
        activeRootExecutionId = activeExecutionId,
        latestRootExecutionId = selectedExecutionId ?: seed.latestRootExecutionId,
        synthesisPending = synthesisPending,
        synthesisFailedExecutionId = synthesisFailedExecutionId,
        schedule = manifest["schedule"]?.takeIf { it is JsonObject } ?: seed.schedule,
        recurringSchedule = bundle.details?.get("recurring_schedule")?.takeIf { it is JsonObject } ?: seed.recurringSchedule,
        completionSummary = resultSummary,
        completionOutcome = resultOutcome,
    )
}

private fun TaskDetailAct.toDetailTab(): DetailTab = when (this) {
    TaskDetailAct.Plan -> DetailTab.Plan
    TaskDetailAct.Run -> DetailTab.Run
    TaskDetailAct.Output -> DetailTab.Output
}

private data class DetailStep(val id: String, val number: Int, val name: String, val status: String, val progress: String?, val capability: String?, val agent: String?)
private data class DetailActivity(
    val id: String, val title: String, val body: String?, val status: String, val agent: String?,
    val timestamp: String?, val model: String?, val latency: Long?, val cost: Double?,
    val executionId: String? = null, val atMillis: Long? = null,
)
private data class DetailQuestion(val text: String, val status: String, val options: List<String>)
private data class DetailChild(val id: String, val title: String, val owner: String, val waitingState: String, val blocking: Boolean)
private data class DetailResponsibility(val owner: String, val waitingState: String, val stage: String?, val provider: String?, val children: List<DetailChild>)
private data class DetailShellEntry(val id: String, val command: String, val lines: List<String>, val exitCode: Int?, val complete: Boolean)
private data class DetailArtifact(
    val id: String, val path: String, val mediaType: String?, val role: String?, val sizeBytes: Int?,
    val snippet: String?, val url: String?, val scope: TaskOutputScope = TaskOutputScope.Task,
    val displayName: String? = null,
) {
    val name: String get() = displayName ?: path.substringAfterLast('/').ifBlank { path }
}
private data class DetailExecution(val id: String, val status: String, val startedAt: String?, val endedAt: String?, val summary: String?, val error: String?)

private fun parseArtifacts(
    value: JsonElement?,
    defaultScope: TaskOutputScope = TaskOutputScope.Task,
    forceScope: Boolean = false,
): List<DetailArtifact> = value.arrayElements().mapIndexedNotNull { index, element ->
    val row = element as? JsonObject ?: return@mapIndexedNotNull null
    val path = (row.text("relative_path") ?: row.text("artifact_path") ?: row.text("path"))
        ?.trim()?.takeIf(String::isNotEmpty) ?: return@mapIndexedNotNull null
    val scope = if (forceScope) defaultScope else row.text("scope")?.let(TaskOutputScope::fromWire) ?: defaultScope
    DetailArtifact(
        row.text("output_id") ?: row.text("artifact_id") ?: row.text("id") ?: "$path-$index", path,
        row.text("media_type") ?: row.text("content_type") ?: row.text("mime_type"), row.text("role") ?: row.text("class"),
        row.int("size_bytes"), row.text("body_snippet"), row.text("download_url") ?: row.text("serving_url"),
        scope = scope,
        displayName = row.text("display_name")?.takeIf(String::isNotBlank),
    )
}

private fun parseExecutions(value: JsonElement?): List<DetailExecution> = value.arrayElements().mapNotNull { element ->
    val root = element as? JsonObject ?: return@mapNotNull null
    val row = root.obj("state") ?: root
    val id = row.text("execution_id") ?: return@mapNotNull null
    DetailExecution(
        id, row.text("status") ?: "unknown", row.text("started_at"), row.text("completed_at") ?: row.text("ended_at"),
        row.text("completion_summary"), row.text("error_message"),
    )
}

private fun parseRecentRuns(value: JsonElement?): List<DetailExecution> = value.arrayElements().mapNotNull { element ->
    val row = element as? JsonObject ?: return@mapNotNull null
    val id = row.text("execution_id") ?: return@mapNotNull null
    DetailExecution(id, row.text("status") ?: "unknown", row.text("started_at"), row.text("ended_at"), row.text("completion_summary"), row.text("error_message"))
}

private fun activityTitle(event: String?, tool: String?, fallback: String?): String = when (event) {
    "tool.succeeded" -> tool?.let { "$it returned" } ?: fallback ?: "Tool returned"
    "tool.failed" -> tool?.let { "$it failed" } ?: fallback ?: "Tool failed"
    "tool.started", "tool.requested" -> tool?.let { "Using $it" } ?: fallback ?: "Using a tool"
    "llm.requested" -> "Thinking"
    "llm.succeeded" -> "Model responded"
    "llm.failed" -> "Model failed"
    else -> fallback ?: event?.replace('.', ' ') ?: "Activity"
}

private fun JsonObject?.obj(key: String): JsonObject? = this?.get(key) as? JsonObject
private fun JsonObject?.text(key: String): String? = (this?.get(key) as? JsonPrimitive)?.contentOrNull
private fun JsonObject?.int(key: String): Int? = (this?.get(key) as? JsonPrimitive)?.intOrNull
private fun JsonObject?.long(key: String): Long? = (this?.get(key) as? JsonPrimitive)?.contentOrNull?.toLongOrNull()
private fun JsonObject?.double(key: String): Double? = (this?.get(key) as? JsonPrimitive)?.contentOrNull?.toDoubleOrNull()
private fun JsonObject?.bool(key: String): Boolean? = (this?.get(key) as? JsonPrimitive)?.booleanOrNull
private fun JsonObject?.array(key: String): List<JsonElement> = (this?.get(key) as? JsonArray)?.toList().orEmpty()
private fun JsonElement?.arrayElements(): List<JsonElement> = (this as? JsonArray)?.toList().orEmpty()
private fun JsonObject?.orEmpty(): JsonObject = this ?: JsonObject(emptyMap())

private fun wireInstantMillis(value: JsonElement?): Long? {
    val content = (value as? JsonPrimitive)?.contentOrNull ?: return null
    runCatching { java.time.Instant.parse(content).toEpochMilli() }.getOrNull()?.let { return it }
    val raw = content.toDoubleOrNull()?.takeIf(Double::isFinite) ?: return null
    if (raw == 0.0) return null
    val millis = if (raw > 10_000_000_000.0) raw else raw * 1_000.0
    return millis.takeIf { it in Long.MIN_VALUE.toDouble()..Long.MAX_VALUE.toDouble() }?.toLong()
}

private fun runElapsedSeconds(startedAt: String?, endedAt: String?): Double? {
    val started = startedAt?.let { runCatching { java.time.Instant.parse(it) }.getOrNull() } ?: return null
    val ended = endedAt?.let { runCatching { java.time.Instant.parse(it) }.getOrNull() } ?: return null
    return (ended.toEpochMilli() - started.toEpochMilli()).toDouble() / 1_000.0
}

private fun detailStatusTint(status: String): Color = when (status.lowercase()) {
    "completed", "succeeded", "ready" -> DTSuccess
    "running", "planning", "executing" -> DTInfo
    "paused", "deferred", "pending" -> DTWarning
    "failed", "cancelled", "canceled" -> DTDanger
    else -> Muted
}

/** "Sep 23, 11:13 → 11:28 · 14m 27s"; raw text when a stamp is unreadable, no duration when an end is unknown. */
internal fun historySpanLine(
    startedAt: String?,
    endedAt: String?,
    zone: java.time.ZoneId = java.time.ZoneId.systemDefault(),
): String {
    val dayClock = java.time.format.DateTimeFormatter.ofPattern("MMM d, HH:mm", java.util.Locale.US)
    val clock = java.time.format.DateTimeFormatter.ofPattern("HH:mm", java.util.Locale.US)
    val start = TaskTimeline.parseMillis(startedAt)?.let { java.time.Instant.ofEpochMilli(it).atZone(zone) }
    val end = TaskTimeline.parseMillis(endedAt)?.let { java.time.Instant.ofEpochMilli(it).atZone(zone) }
    val startText = start?.format(dayClock) ?: startedAt
    val endText = end?.let {
        if (start != null && it.toLocalDate() == start.toLocalDate()) it.format(clock) else it.format(dayClock)
    } ?: endedAt
    val span = listOfNotNull(startText, endText).joinToString(" → ")
    val duration = TaskTimeline.executionDuration(startedAt, endedAt) ?: return span
    return if (span.isEmpty()) duration else "$span · $duration"
}

/** A timeline row's local wall clock (the panel sends epoch millis, which read as noise). */
internal fun activityClock(millis: Long, zone: java.time.ZoneId = java.time.ZoneId.systemDefault()): String =
    java.time.Instant.ofEpochMilli(millis).atZone(zone)
        .format(java.time.format.DateTimeFormatter.ofPattern("HH:mm:ss", java.util.Locale.US))

private fun shortId(id: String): String = if (id.length <= 16) id else id.take(8) + "…" + id.takeLast(5)
private fun formatBytes(bytes: Int): String = when {
    bytes >= 1_048_576 -> "%.1f MB".format(bytes / 1_048_576.0)
    bytes >= 1_024 -> "%.1f KB".format(bytes / 1_024.0)
    else -> "$bytes B"
}

// Theme palette, not fixed hex: these follow the selected Magdroid theme (light/dark).
private val DTSuccess: Color get() = activePalette.success
private val DTWarning: Color get() = activePalette.warning
private val DTDanger: Color get() = activePalette.danger
private val DTInfo: Color get() = activePalette.info
