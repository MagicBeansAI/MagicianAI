package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tasks.withTaskProgress
import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.chat.splitAnswerVerification
import ai.magicbeans.magdroid.work.WorkWebContext
import ai.magicbeans.magdroid.work.WorkSpecialist
import ai.magicbeans.magdroid.work.WorkSection
import ai.magicbeans.magdroid.tasks.ActCapabilities
import ai.magicbeans.magdroid.tasks.ActSummaries
import ai.magicbeans.magdroid.tasks.ExecutionControlAction
import ai.magicbeans.magdroid.tasks.LiveElapsed
import ai.magicbeans.magdroid.tasks.PlanApprovalChoice
import ai.magicbeans.magdroid.tasks.PlanInspectorRoute
import ai.magicbeans.magdroid.tasks.TaskPlanMarker
import ai.magicbeans.magdroid.tasks.PlanProgress
import ai.magicbeans.magdroid.tasks.PlanStepActivity
import ai.magicbeans.magdroid.tasks.PlanStepAttribution
import ai.magicbeans.magdroid.tasks.PlanStepMark
import ai.magicbeans.magdroid.tasks.PlanStepRow
import ai.magicbeans.magdroid.tasks.PlanSteps
import ai.magicbeans.magdroid.tasks.PlanSummaryInput
import ai.magicbeans.magdroid.tasks.PlanningEntry
import ai.magicbeans.magdroid.tasks.RunSummaryInput
import ai.magicbeans.magdroid.tasks.SynthesisWait
import ai.magicbeans.magdroid.tasks.TaskActs
import ai.magicbeans.magdroid.tasks.TaskActivityCalls
import ai.magicbeans.magdroid.tasks.TaskAttention
import ai.magicbeans.magdroid.tasks.TaskAttentionSource
import ai.magicbeans.magdroid.tasks.TaskCompletion
import ai.magicbeans.magdroid.tasks.TaskDelegationGroup
import ai.magicbeans.magdroid.tasks.TaskDetailAct
import ai.magicbeans.magdroid.tasks.TaskDetailAction
import ai.magicbeans.magdroid.tasks.TaskDetailBundle
import ai.magicbeans.magdroid.tasks.TaskDetailMenuInput
import ai.magicbeans.magdroid.tasks.TaskDetailRules
import ai.magicbeans.magdroid.tasks.TaskOutputClassification
import ai.magicbeans.magdroid.tasks.TaskOutputFile
import ai.magicbeans.magdroid.tasks.TaskOutputGrouping
import ai.magicbeans.magdroid.tasks.TaskOutputScope
import ai.magicbeans.magdroid.tasks.TaskPlanStatus
import ai.magicbeans.magdroid.tasks.TaskTimeline
import ai.magicbeans.magdroid.tasks.TaskUsageFormat
import ai.magicbeans.magdroid.tasks.TaskUsageModel
import ai.magicbeans.magdroid.tasks.TaskV3
import ai.magicbeans.magdroid.tasks.TaskVerdict
import ai.magicbeans.magdroid.tasks.TaskVerdictInput
import ai.magicbeans.magdroid.tasks.TaskVerdictState
import ai.magicbeans.magdroid.tasks.TaskVerdictValue
import ai.magicbeans.magdroid.tasks.TasksUiState
import ai.magicbeans.magdroid.tasks.TasksViewModel
import android.graphics.BitmapFactory
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.Assignment
import androidx.compose.material.icons.automirrored.outlined.Redo
import androidx.compose.material.icons.outlined.AccountTree
import androidx.compose.material.icons.outlined.Bolt
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material.icons.outlined.Inventory2
import androidx.compose.material.icons.outlined.Memory
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.RadioButtonUnchecked
import androidx.compose.material.icons.outlined.Share
import androidx.compose.material.icons.outlined.Tag
import androidx.compose.material.icons.outlined.Visibility
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.onClick
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import ai.magicbeans.magdroid.attention.askIds
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull

/*
 * The task detail screen (design §3 of
 * `docs/archive/plans/2026-10-01-mobile-lively-chat-and-task-detail-design.md`):
 * a full screen with one scroll — a header card (verdict line, title, brief,
 * id and plan chips, usage), then the Output, Run and Plan act cards in
 * `TaskActs.actOrder`, exactly one open, then Technical details. Lifecycle
 * actions live in the top bar's ⋯ menu; a compact title takes over the bar
 * once the header scrolls away. Android port of iOS `TaskDetailScreen.swift`
 * / `TaskDetailActCards.swift`; every rule comes from the bridge models
 * (`TaskDetailRules`, `TaskActs`, `ActSummaries`, `PlanSteps`,
 * `PlanProgress`, `TaskOutputGrouping`, `TaskVerdict`, `TaskUsageModel`).
 */

@Composable
internal fun TaskDetailScreen(
    state: TasksUiState,
    viewModel: TasksViewModel,
    onBack: () -> Unit = viewModel::closeTaskDetail,
    /**
     * The Plan act's "Plan graph" chip: the Plan Inspector for this task id.
     * Null opens it through [LocalOpenPlanInspector] when a host provides one,
     * else as this screen's own full-screen dialog.
     */
    onOpenPlanInspector: ((String) -> Unit)? = null,
) {
    val detailState = state.detail
    val taskId = detailState.taskId.orEmpty()
    val seed = (state.tasks + state.internalTasks).firstOrNull { it.id == taskId }
        ?: TaskV3(id = taskId, title = "Task")
    val bundle = detailState.bundle
    val nowMillis = rememberMinuteClock()
    val context = LocalContext.current
    val baseUrl = remember(context) { MagicianAccess.baseUrl(context) }
    val projection = remember(bundle, seed, detailState.selectedExecutionId, nowMillis, baseUrl) {
        bundle?.let { TaskDetailProjection(seed, it, detailState.selectedExecutionId, nowMillis, baseUrl) }
    }
    val actionTask = projection?.actionTask ?: seed
    val busy = state.mutatingTaskId != null || state.publishingTaskId != null
    // This task's asks — the panel's `needs_attention` merged with the feed
    // rows for the task (a Relentless review, a delegated child's question) —
    // answered here with the Attention form instead of only in Attention.
    val askSurface: ai.magicbeans.magdroid.attention.AskSurfaceViewModel =
        androidx.lifecycle.viewmodel.compose.viewModel()
    val askState by askSurface.state.collectAsStateWithLifecycle()
    FollowAskSurface(askSurface)
    val askConnection = remember(viewModel) { ai.magicbeans.magdroid.access.MagicianAccess.connectionSnapshot(context) }
    val panelAsks = projection?.panelAsks.orEmpty().map { it.copy(originConnection = askConnection) }
    val taskAsks = remember(askState.feed, askState.settled, panelAsks, taskId) {
        ai.magicbeans.magdroid.attention.AskSurfaces.forTask(askState.feed, taskId, panelAsks, askState.settled)
    }
    // An answer moves the run (and its verdict); re-read the panel at once.
    LaunchedEffect(askState.settled.size) {
        if (askState.settled.isNotEmpty()) viewModel.refreshTaskDetail()
    }
    // A review link that routes in-app (Work Quality) leaves this detail.
    val openReview: (String) -> Unit = { href -> if (openAskReview(context, href)) onBack() }
    val scope = rememberCoroutineScope()
    val listState = rememberLazyListState()

    var openAct by remember(taskId) { mutableStateOf<TaskDetailAct?>(null) }
    var appliedDefault by remember(taskId) { mutableStateOf(false) }
    var toast by remember { mutableStateOf<String?>(null) }
    var showSteer by remember { mutableStateOf<String?>(null) }
    var confirm by remember(taskId) { mutableStateOf<DetailConfirm?>(null) }
    var inspectorRoute by remember(taskId) { mutableStateOf<PlanInspectorRoute?>(null) }
    var workContext by remember(taskId) { mutableStateOf<WorkRoute?>(null) }
    val hostInspector = LocalOpenPlanInspector.current
    val openInspector: (String) -> Unit = onOpenPlanInspector ?: { id ->
        val route = PlanInspectorRoute(taskId = id, title = projection?.title, planDocument = projection?.planMarkdown)
        hostInspector?.invoke(route) ?: run { inspectorRoute = route }
    }

    LaunchedEffect(taskId, projection != null) {
        val loaded = projection ?: return@LaunchedEffect
        if (appliedDefault) return@LaunchedEffect
        appliedDefault = true
        openAct = TaskDetailRules.initialOpenAct(
            loaded.acts, loaded.verdict.state, loaded.attentionSource, detailState.preferredAct,
        )
    }

    // Live: poll `/execution-panel` every 4 s only while RESUMED and the task
    // is planning or running, on top of the realtime socket the view model
    // already follows. Backgrounding or leaving the screen stops it.
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    val pollStatus = projection?.status
    val pollPlan = projection?.planStatus
    LaunchedEffect(taskId, pollStatus, pollPlan, lifecycle) {
        lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) {
            if (!TaskDetailRules.shouldPoll(resumed = true, status = pollStatus, planStatus = pollPlan)) {
                return@repeatOnLifecycle
            }
            while (true) {
                delay(TaskDetailRules.POLL_INTERVAL_MILLIS)
                viewModel.refreshTaskDetail()
            }
        }
    }
    LaunchedEffect(state.actionNotice) {
        state.actionNotice?.let { toast = it; viewModel.clearNotice() }
    }

    val compactTitle by remember { derivedStateOf { listState.firstVisibleItemIndex > 0 } }
    val actOrder = projection?.let { TaskActs.actOrder(it.verdict.state, it.acts) }.orEmpty()
    val actListOffset = if (bundle?.unavailableSections.isNullOrEmpty()) 1 else 2

    fun openActAndScroll(act: TaskDetailAct) {
        openAct = act
        val index = actOrder.indexOf(act)
        if (index >= 0) scope.launch { listState.animateScrollToItem(actListOffset + index) }
    }

    Box(Modifier.fillMaxSize().themeWash()) {
        workContext?.let { route -> WorkContextDialog(route, onClose = { workContext = null }, onOpenTask = { id -> workContext = null; if (id != taskId) TaskDeepLinks.request(TaskDeepLinkTarget.Task(id)) }) }
        Column(Modifier.fillMaxSize()) {
            DetailTopBar(
                title = projection?.title ?: seed.title.ifBlank { "Task" },
                taskId = taskId,
                planned = projection?.planned ?: TaskPlanMarker.hasPlan(seed),
                verdict = projection?.verdict,
                showTitle = compactTitle,
                onBack = onBack,
            ) {
                if (projection != null) DetailActionsMenu(
                    projection = projection,
                    task = actionTask,
                    state = state,
                    viewModel = viewModel,
                    busy = busy,
                    onSteer = { showSteer = it },
                    onConfirm = { confirm = it },
                    onRefresh = viewModel::refreshTaskDetail,
                    onNotice = { toast = it },
                )
            }
            when {
                projection != null -> LazyColumn(
                    Modifier.fillMaxSize(),
                    state = listState,
                    contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 40.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    item(key = "header") {
                        DetailHeaderCard(
                            projection = projection,
                            taskId = taskId,
                            onOpenPlan = { openActAndScroll(TaskDetailAct.Plan) },
                        )
                        Row {
                            if (!projection.isExecutionOnly) TextButton(onClick = { workContext = WorkRoute("quality-context", taskId, "Quality review") }) { Text("Quality review", color = Coral) }
                            if (projection.agentId.isNotBlank()) TextButton(onClick = { workContext = WorkRoute("member", projection.agentId, "Crew member") }) { Text("Crew member", color = Coral) }
                        }
                    }
                    if (taskAsks.isNotEmpty() || askState.notice != null) item(key = "asks") {
                        TaskAsksCard(
                            asks = taskAsks,
                            state = askState,
                            viewModel = askSurface,
                            onReviewPlan = { id -> openInspector(id) },
                            onOpenReview = openReview,
                        )
                    }
                    if (bundle?.unavailableSections?.isNotEmpty() == true) item(key = "partial") {
                        DetailNotice(
                            "Some ${bundle.unavailableSections.joinToString()} details are unavailable.",
                            activePalette.warning, Icons.Outlined.ErrorOutline,
                        )
                    }
                    actOrder.forEach { act ->
                        item(key = "act-${act.name}") {
                            ActCard(
                                act = act,
                                projection = projection,
                                open = openAct == act,
                                onToggle = { openAct = TaskDetailRules.toggle(openAct, act) },
                                onOpenPlanInspector = { openInspector(taskId) },
                            ) {
                                when (act) {
                                    TaskDetailAct.Output -> OutputBody(projection)
                                    TaskDetailAct.Run -> RunBody(projection, actionTask, viewModel)
                                    TaskDetailAct.Plan -> PlanBody(projection, busy, taskAsks) { choice ->
                                        if (choice == PlanApprovalChoice.Reject) confirm = DetailConfirm.RejectPlan
                                        else viewModel.decidePlan(actionTask, choice)
                                    }
                                }
                            }
                        }
                    }
                    item(key = "technical") {
                        TechnicalCard(projection, taskId, actionTask, viewModel)
                    }
                }
                detailState.loading || detailState.error == null -> DetailPlaceholder("Loading the task…", true)
                else -> DetailPlaceholder(detailState.error ?: "Task details are unavailable.", false) {
                    viewModel.openTask(seed, detailState.selectedExecutionId, detailState.preferredAct)
                }
            }
        }
        toast?.let { message ->
            LaunchedEffect(message) {
                delay(2_400)
                if (toast == message) toast = null
            }
            Text(
                message,
                color = OnAccent,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .navigationBarsPadding()
                    .padding(start = 24.dp, end = 24.dp, bottom = 22.dp)
                    .background(AccentFill, RoundedCornerShape(50))
                    .padding(horizontal = 14.dp, vertical = 9.dp),
            )
        }
    }

    inspectorRoute?.let { route -> PlanInspectorDialog(route, onClose = { inspectorRoute = null }) }
    AskSurfaceSheet(
        viewModel = askSurface,
        state = askState,
        owner = ASK_SHEET_TASK,
        onOpenTask = { id -> if (id != taskId) TaskDeepLinks.request(TaskDeepLinkTarget.Task(id)) },
        onReviewPlan = { id -> openInspector(id) },
        onOpenReview = openReview,
    )
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
                    Text("$bytes / 4,096 bytes", color = if (bytes > 4_096) activePalette.danger else Muted, fontSize = 11.sp)
                }
            },
            dismissButton = { TextButton(onClick = { showSteer = null }) { Text("Cancel") } },
            confirmButton = {
                Button(
                    shape = MagicanButtonShape,
                    colors = magicanButtonColors(),
                    onClick = { viewModel.executionControl(executionId, ExecutionControlAction.Steer, guidance); showSteer = null },
                    enabled = guidance.isNotBlank() && bytes <= 4_096,
                ) { Text("Send guidance") }
            },
        )
    }
    confirm?.let { pending ->
        AlertDialog(
            onDismissRequest = { confirm = null },
            title = { Text(pending.title) },
            text = { Text(pending.message) },
            dismissButton = { TextButton(onClick = { confirm = null }) { Text(pending.keep) } },
            confirmButton = {
                Button(
                    shape = MagicanButtonShape,
                    enabled = !busy,
                    colors = ButtonDefaults.buttonColors(containerColor = activePalette.danger, contentColor = chatContrastingTextColor(activePalette.danger)),
                    onClick = {
                        confirm = null
                        when (pending) {
                            DetailConfirm.RejectPlan -> viewModel.decidePlan(actionTask, PlanApprovalChoice.Reject)
                            DetailConfirm.CancelTask -> viewModel.setStatus(actionTask, "cancelled")
                            is DetailConfirm.StopRun -> viewModel.executionControl(pending.executionId, ExecutionControlAction.Cancel)
                            DetailConfirm.Delete -> viewModel.deleteTask(actionTask, false) { deleted -> if (deleted) onBack() }
                        }
                    },
                ) { Text(pending.confirm) }
            },
        )
    }
}

/** A destructive choice waiting on its confirmation. */
private sealed class DetailConfirm(val title: String, val message: String, val confirm: String, val keep: String) {
    data object RejectPlan : DetailConfirm("Reject this plan?", "It will not run. You can replan later.", "Reject", "Keep plan")
    data object CancelTask : DetailConfirm("Cancel this task?", "Its run stops and it will not continue.", "Cancel task", "Keep task")
    data class StopRun(val executionId: String) :
        DetailConfirm("Stop this run?", "The active execution and its running children will be cancelled.", "Stop run", "Keep running")
    data object Delete : DetailConfirm("Delete this task?", "This removes the task record and cannot be undone.", "Delete", "Keep task")
}

/** "Now", re-read every 30 s so relative times (approved 15m ago) stay current. */
@Composable
private fun rememberMinuteClock(): Long {
    var now by remember { mutableStateOf(System.currentTimeMillis()) }
    LaunchedEffect(Unit) {
        while (true) {
            delay(30_000)
            now = System.currentTimeMillis()
        }
    }
    return now
}

/** Verdict state → dot tone (iOS `TaskDetailVerdictLine.tone`). Exhaustive. */
internal fun verdictTone(state: TaskVerdictState): StatusTone = when (state) {
    TaskVerdictState.Running, TaskVerdictState.Finalizing -> StatusTone.Running
    TaskVerdictState.Waiting -> StatusTone.Waiting
    TaskVerdictState.Failed -> StatusTone.Failed
    TaskVerdictState.Stalled, TaskVerdictState.Paused, TaskVerdictState.PartlyDone -> StatusTone.Partial
    TaskVerdictState.Cancelled, TaskVerdictState.Archived -> StatusTone.Cancelled
    TaskVerdictState.Queued -> StatusTone.NotStarted
    TaskVerdictState.Finished -> StatusTone.Done
}

// Chrome

@Composable
private fun DetailTopBar(
    title: String,
    taskId: String,
    planned: Boolean,
    verdict: TaskVerdictValue?,
    showTitle: Boolean,
    onBack: () -> Unit,
    menu: @Composable () -> Unit,
) {
    Row(
        Modifier.fillMaxWidth().background(Ground).padding(horizontal = 4.dp, vertical = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Outlined.ArrowBack, "Back", tint = Coral) }
        // The compact title is the header's stand-in once it scrolls away, so
        // a tap copies the full task id like the header's id chip does.
        val flash = rememberCopyFlash()
        val target = CopyTarget.id(taskId)
        Row(
            Modifier.weight(1f).alpha(if (showTitle) 1f else 0f).heightIn(min = Lively.tapMin)
                .then(
                    if (showTitle) Modifier.clickable(onClickLabel = target.clickLabel, role = Role.Button) { flash.copy(target) }
                    else Modifier,
                ),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            verdict?.let {
                StatusDot(verdictTone(it.state), size = 7.dp)
                Spacer(Modifier.width(6.dp))
            }
            if (planned) {
                TaskPlanGlyph(size = 12.dp)
                Spacer(Modifier.width(5.dp))
            }
            CopyFlashLabel(
                title,
                copied = flash.copied,
                style = TextStyle(color = Ink, fontSize = 15.sp, fontWeight = FontWeight.SemiBold),
            )
        }
        menu()
    }
}

@Composable
private fun DetailActionsMenu(
    projection: TaskDetailProjection,
    task: TaskV3,
    state: TasksUiState,
    viewModel: TasksViewModel,
    busy: Boolean,
    onSteer: (String) -> Unit,
    onConfirm: (DetailConfirm) -> Unit,
    onRefresh: () -> Unit,
    onNotice: (String) -> Unit = {},
) {
    var expanded by remember { mutableStateOf(false) }
    val context = LocalContext.current.applicationContext
    val followed by remember(context) { ai.magicbeans.magdroid.vibedev.WorkFollows.followed(context) }.collectAsStateWithLifecycle()
    val followScope = rememberCoroutineScope()
    // Owner-visible tasks only: internal and execution-only rows are never work items.
    val followId = ai.magicbeans.magdroid.vibedev.WorkItemIds.task(task.id)
        .takeIf { !projection.isExecutionOnly && !task.isInternal }
    val runId = projection.activeExecutionId
    LaunchedEffect(runId) { runId?.let(viewModel::loadExecutionControls) }
    val controls = runId?.let { state.executionControls[it]?.state }
    val actions = if (projection.isExecutionOnly || task.isAppManaged) emptyList() else TaskDetailRules.menuActions(TaskDetailMenuInput(
        status = projection.status,
        planStatus = projection.planStatus,
        hasPlan = projection.hasPlan,
        live = runId != null,
        synthesisFailed = projection.synthesisFailed,
        synthesisPending = projection.synthesisPending,
    ))
    Box {
        IconButton(onClick = { expanded = true }, enabled = !busy, modifier = Modifier.size(Lively.tapMin)) {
            Icon(Icons.Outlined.MoreVert, "Task actions", tint = Coral)
        }
        DropdownMenu(expanded, { expanded = false }) {
            if (runId != null && controls != null) {
                if (controls.canPause) MenuRow("Pause run") { expanded = false; viewModel.executionControl(runId, ExecutionControlAction.Pause) }
                if (controls.canResume) MenuRow("Resume run") { expanded = false; viewModel.executionControl(runId, ExecutionControlAction.Resume) }
                if (controls.canSteer) MenuRow("Steer run…") { expanded = false; onSteer(runId) }
                if (controls.canCancel) MenuRow("Stop run", danger = true) { expanded = false; onConfirm(DetailConfirm.StopRun(runId)) }
                HorizontalDivider(color = BorderSoft)
            }
            actions.filter { it != TaskDetailAction.Delete && (!task.isAppManaged || it != TaskDetailAction.RetrySynthesis) }.forEach { action ->
                MenuRow(action.title, danger = action == TaskDetailAction.CancelTask) {
                    expanded = false
                    when (action) {
                        TaskDetailAction.Run -> viewModel.execute(task)
                        TaskDetailAction.Plan -> viewModel.preplan(task)
                        TaskDetailAction.ApprovePlan -> viewModel.decidePlan(task, PlanApprovalChoice.Approve)
                        TaskDetailAction.RejectPlan -> onConfirm(DetailConfirm.RejectPlan)
                        TaskDetailAction.Replan -> viewModel.replan(task)
                        TaskDetailAction.RetrySynthesis -> viewModel.retrySynthesis(task)
                        TaskDetailAction.Reset, TaskDetailAction.MarkNotDone -> viewModel.setStatus(task, "ready")
                        TaskDetailAction.MarkComplete -> viewModel.setStatus(task, "completed")
                        TaskDetailAction.CancelTask -> onConfirm(DetailConfirm.CancelTask)
                        TaskDetailAction.Delete -> onConfirm(DetailConfirm.Delete)
                    }
                }
            }
            HorizontalDivider(color = BorderSoft)
            if (task.canPublishToNotes) MenuRow("Publish to Notes") { expanded = false; viewModel.publishToNotes(task) }
            followId?.let { id ->
                val following = id in followed
                // Its own ongoing notification until it ends (at most three).
                MenuRow(if (following) "Unfollow" else "Follow") {
                    expanded = false
                    ai.magicbeans.magdroid.ui.vibedev.toggleFollow(followScope, context, id, follow = !following, onNotice)
                }
            }
            MenuRow("Refresh") { expanded = false; onRefresh() }
            if (!projection.isExecutionOnly) {
                HorizontalDivider(color = BorderSoft)
                if (!task.isAppManaged) MenuRow(TaskDetailAction.Delete.title, danger = true) { expanded = false; onConfirm(DetailConfirm.Delete) }
            }
        }
    }
}

@Composable
private fun MenuRow(label: String, danger: Boolean = false, onClick: () -> Unit) {
    DropdownMenuItem(
        text = { Text(label, color = if (danger) activePalette.danger else Ink, fontSize = 14.sp) },
        onClick = onClick,
    )
}

@Composable
private fun DetailPlaceholder(message: String, loading: Boolean, retry: (() -> Unit)? = null) {
    Box(Modifier.fillMaxSize().padding(24.dp), contentAlignment = Alignment.Center) {
        Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(12.dp)) {
            if (loading) CircularProgressIndicator(color = Coral)
            else Icon(Icons.Outlined.ErrorOutline, null, tint = activePalette.warning, modifier = Modifier.size(34.dp))
            Text(message, color = Muted, fontSize = 14.sp)
            retry?.let { LivelyChip(onClick = it, prominent = true) { Text("Try again", style = livelyChipTextStyle(true)) } }
        }
    }
}

@Composable
private fun DetailNotice(text: String, tint: Color, icon: ImageVector) {
    Row(
        Modifier.fillMaxWidth().background(tint.copy(alpha = .10f), RoundedCornerShape(10.dp)).padding(10.dp),
        verticalAlignment = Alignment.Top,
    ) {
        Icon(icon, null, tint = tint, modifier = Modifier.size(17.dp))
        Spacer(Modifier.width(8.dp))
        Text(text, color = Ink, fontSize = 13.sp, modifier = Modifier.weight(1f))
    }
}

/**
 * The Output section while the final answer is still being written
 * (`synthesis_pending`): a spinner, the label and an elapsed clock, ticking on
 * the same once-a-second clock as the running step rows. [baselineMillis] is
 * when the execution ended; a screen that has no end time counts from when it
 * first saw the flag. Past [SynthesisWait.STALE_AFTER_MILLIS] it stops spinning
 * and shows a neutral notice instead, because a client must not spin forever
 * on a flag that never clears.
 */
@Composable
private fun SynthesisProgressRow(baselineMillis: Long?) {
    val observedAt = remember { System.currentTimeMillis() }
    val start = baselineMillis ?: observedAt
    val now = rememberLiveNowMillis(start)
    if (SynthesisWait.isStale(start, now)) {
        DetailNotice(
            "The final result is taking longer than expected. It will appear here when it is ready.",
            Muted, Icons.Outlined.Description,
        )
        return
    }
    Row(
        Modifier.fillMaxWidth().background(Coral.copy(alpha = .10f), RoundedCornerShape(10.dp)).padding(10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp, color = Coral)
        Spacer(Modifier.width(8.dp))
        Text("Preparing final result", color = Ink, fontSize = 13.sp, modifier = Modifier.weight(1f))
        LiveElapsed.text(start, now)?.let {
            Text(it, color = Muted, fontSize = 12.sp, fontFamily = LocalMagicanFontFamilies.current.mono, maxLines = 1)
        }
    }
}

/** A rounded-square chip with an optional leading icon and a 48 dp hit area. */
@Composable
private fun IconChip(
    label: String,
    icon: ImageVector?,
    prominent: Boolean = false,
    enabled: Boolean = true,
    mono: Boolean = false,
    onClick: () -> Unit,
) {
    LivelyChip(onClick = onClick, prominent = prominent, enabled = enabled) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            icon?.let {
                Icon(it, null, tint = if (prominent) OnAccent else Ink, modifier = Modifier.size(15.dp))
                Spacer(Modifier.width(5.dp))
            }
            val style = livelyChipTextStyle(prominent)
            Text(
                label,
                style = if (mono) style.copy(fontFamily = LocalMagicanFontFamilies.current.mono, fontWeight = FontWeight.Normal) else style,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

/**
 * An [IconChip] that copies [target]: the label reads `Copied ✓` (icon a
 * check) for a moment after a tap.
 */
@Composable
private fun CopyChip(label: String, icon: ImageVector, target: CopyTarget, mono: Boolean = false) {
    val flash = rememberCopyFlash()
    LivelyChip(
        onClick = { flash.copy(target) },
        modifier = Modifier.semantics { onClick(label = target.clickLabel) { flash.copy(target); true } },
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(
                if (flash.copied) Icons.Outlined.Check else icon, null,
                tint = if (flash.copied) activePalette.success else Ink, modifier = Modifier.size(15.dp),
            )
            Spacer(Modifier.width(5.dp))
            val style = livelyChipTextStyle()
            CopyFlashLabel(
                label,
                copied = flash.copied,
                // The icon is already a check, so the label drops its own.
                copiedLabel = "Copied",
                style = if (mono && !flash.copied) {
                    style.copy(fontFamily = LocalMagicanFontFamilies.current.mono, fontWeight = FontWeight.Normal)
                } else {
                    style
                },
            )
        }
    }
}

@Composable
private fun DetailCardSurface(content: @Composable () -> Unit) {
    Surface(
        color = Panel,
        shape = Lively.cardShape,
        border = BorderStroke(1.dp, BorderSoft),
        modifier = Modifier.fillMaxWidth(),
    ) { content() }
}

// Header card

@Composable
private fun DetailHeaderCard(
    projection: TaskDetailProjection,
    taskId: String,
    onOpenPlan: () -> Unit,
) {
    DetailCardSurface {
        Column(Modifier.padding(start = 16.dp, end = 16.dp, top = 16.dp, bottom = 8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            VerdictLine(projection.verdict)
            Row(verticalAlignment = Alignment.Top) {
                if (projection.planned) {
                    TaskPlanGlyph(Modifier.padding(top = 8.dp, end = 7.dp))
                }
                Text(
                    projection.title.ifBlank { "Untitled task" },
                    color = Ink, fontSize = 22.sp, lineHeight = 27.sp, fontWeight = FontWeight.Bold,
                    maxLines = 4, overflow = TextOverflow.Ellipsis,
                )
            }
            TaskDetailRules.brief(projection.description)?.let {
                Text(it, color = Muted, fontSize = 15.sp, lineHeight = 20.sp, maxLines = 2, overflow = TextOverflow.Ellipsis)
            }
            projection.recurrence?.let {
                Text("↻ Recurring · $it", color = activePalette.info, fontSize = 12.sp, fontWeight = FontWeight.Medium)
            }
            projection.usageTotal?.let { total ->
                TaskUsageFormat.taskLine(total.usage)?.let { line ->
                    Text(
                        line,
                        color = Muted,
                        fontSize = 12.sp,
                        fontFamily = LocalMagicanFontFamilies.current.mono,
                        maxLines = 1,
                        modifier = Modifier.semantics { contentDescription = TaskUsageFormat.taskAccessibilityLabel(total) },
                    )
                }
            }
            Row(
                Modifier.horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(2.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                CopyChip(TaskDetailRules.shortId(taskId), Icons.Outlined.Tag, CopyTarget.id(taskId), mono = true)
                if (projection.hasPlan && TaskDetailAct.Plan in projection.acts) {
                    IconChip(
                        TaskDetailRules.planChipText(projection.planSummary),
                        Icons.AutoMirrored.Outlined.Assignment,
                        onClick = onOpenPlan,
                    )
                }
            }
        }
    }
}

@Composable
private fun VerdictLine(verdict: TaskVerdictValue) {
    val tone = verdictTone(verdict.state)
    Row(verticalAlignment = Alignment.Top, modifier = Modifier.semantics(mergeDescendants = true) {}) {
        Box(Modifier.padding(top = 5.dp)) { StatusDot(tone, size = 9.dp) }
        Spacer(Modifier.width(8.dp))
        Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(verdict.headline, color = statusToneColor(tone), fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
            if (verdict.detail.isNotBlank()) Text(verdict.detail, color = Muted, fontSize = 13.sp, lineHeight = 17.sp)
        }
    }
}

// Act card

@Composable
private fun ActCard(
    act: TaskDetailAct,
    projection: TaskDetailProjection,
    open: Boolean,
    onToggle: () -> Unit,
    onOpenPlanInspector: () -> Unit,
    body: @Composable () -> Unit,
) {
    val summary = projection.summary(act)
    val showsSummary = summary.isNotEmpty() && (!open || TaskDetailRules.keepsSummaryWhenOpen(act))
    val title = when (act) {
        TaskDetailAct.Output -> "Output"
        TaskDetailAct.Run -> "Run"
        TaskDetailAct.Plan -> "Plan"
    }
    val icon = when (act) {
        TaskDetailAct.Output -> Icons.Outlined.Inventory2
        TaskDetailAct.Run -> Icons.Outlined.Bolt
        TaskDetailAct.Plan -> Icons.AutoMirrored.Outlined.Assignment
    }
    val planChip = act == TaskDetailAct.Plan && (projection.hasPlanGraph || !projection.planMarkdown.isNullOrBlank())
    var planDocument by remember { mutableStateOf(false) }
    DetailCardSurface {
        Column(Modifier.padding(horizontal = 14.dp, vertical = 4.dp).padding(bottom = if (open) 10.dp else 0.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Row(
                    Modifier
                        .weight(1f)
                        .heightIn(min = Lively.tapMin)
                        .clickable(onClick = onToggle)
                        .semantics(mergeDescendants = true) {
                            contentDescription = (if (showsSummary) "$title, $summary" else title) +
                                if (open) ", expanded" else ", collapsed"
                        },
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(icon, null, tint = Coral, modifier = Modifier.size(20.dp))
                    Spacer(Modifier.width(12.dp))
                    Column(Modifier.weight(1f).padding(vertical = 6.dp)) {
                        Text(title, color = Ink, fontSize = 16.sp, fontWeight = FontWeight.Bold)
                        if (showsSummary) Text(summary, color = Muted, fontSize = 13.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    }
                    Icon(
                        Icons.Outlined.ExpandMore, null, tint = Muted,
                        modifier = Modifier.size(20.dp).rotate(if (open) 0f else -90f),
                    )
                }
                // A plan with a graph opens the Plan Inspector; a document-only plan opens its document.
                if (planChip && projection.hasPlanGraph) {
                    Spacer(Modifier.width(4.dp))
                    IconChip("Plan graph", Icons.Outlined.AccountTree, onClick = onOpenPlanInspector)
                } else if (planChip) {
                    Spacer(Modifier.width(4.dp))
                    IconChip("Plan document", Icons.Outlined.Description) { planDocument = true }
                }
            }
            if (open) {
                HorizontalDivider(color = BorderSoft, modifier = Modifier.padding(top = 2.dp, bottom = 10.dp))
                body()
            }
        }
    }
    if (planDocument) AlertDialog(
        onDismissRequest = { planDocument = false },
        title = { Text("Plan document") },
        text = {
            Box(Modifier.heightIn(max = 480.dp).verticalScroll(rememberScrollState())) {
                MarkdownText(projection.planMarkdown.orEmpty(), color = Ink, fontSize = 13.sp, lineHeight = 18.sp)
            }
        },
        confirmButton = { TextButton(onClick = { planDocument = false }) { Text("Done", color = Coral) } },
        containerColor = Ground,
    )
}

// Output

@Composable
private fun OutputBody(projection: TaskDetailProjection) {
    val files = projection.outputGroups.deliverables
    val previewed = TaskDetailRules.defaultPreview(files, { it.mediaType }, { it.relativePath })
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        projection.resultSummary?.let { TaskAnswerSummary(it) }
        if (projection.synthesisPending) {
            SynthesisProgressRow(projection.synthesisBaselineMillis)
        } else if (projection.synthesisFailed) {
            DetailNotice("Writing up the output failed. Retry it from the ⋯ menu.", activePalette.danger, Icons.Outlined.ErrorOutline)
        }
        if (files.isEmpty() && !projection.synthesisPending) {
            Text(
                if (projection.resultSummary == null) "No output yet." else "No files were delivered.",
                color = Muted, fontSize = 14.sp,
            )
        }
        files.forEachIndexed { index, file ->
            if (index > 0) HorizontalDivider(color = BorderSoft)
            OutputFileRow(file, projection.urlFor(file), showsPreview = file.id == previewed?.id)
        }
    }
}

/**
 * One file. Line 1: the name (middle-ellipsized; a tap copies the path) and
 * `kind · size`. Line 2: Open · Share · Copy path. The previewed file keeps
 * its row; its preview sits under it, small.
 */
@Composable
private fun OutputFileRow(
    file: TaskOutputFile,
    url: String?,
    showsPreview: Boolean,
    compact: Boolean = false,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val name = file.displayName.ifBlank { file.relativePath }
    val target = CopyTarget.path(file.absolutePath, file.relativePath)
    Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Row(Modifier.fillMaxWidth().heightIn(min = 36.dp), verticalAlignment = Alignment.CenterVertically) {
            // Middle-ellipsized to fit beside the meta on a phone; a tap
            // copies the full path and the name reads `Copied ✓` for a moment.
            CopyableText(
                TaskDetailRules.middleEllipsize(name, if (compact) 30 else 28),
                copyValue = target.value,
                what = target.what,
                style = TextStyle(color = Ink, fontSize = if (compact) 13.sp else 15.sp, fontWeight = FontWeight.SemiBold),
                clickLabel = target.clickLabel,
                modifier = Modifier.weight(1f),
            )
            Spacer(Modifier.width(8.dp))
            Text(TaskDetailRules.fileMeta(file.mediaType, file.relativePath, file.sizeBytes), color = Muted, fontSize = 12.sp, maxLines = 1)
        }
        Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(2.dp)) {
            IconChip("Open", Icons.Outlined.Visibility, enabled = url != null) {
                url?.let { scope.launch { AuthenticatedArtifacts.open(context, it, name, file.mediaType) } }
            }
            IconChip("Share", Icons.Outlined.Share, enabled = url != null) {
                url?.let { scope.launch { AuthenticatedArtifacts.share(context, it, name, file.mediaType) } }
            }
            CopyChip("Copy path", Icons.Outlined.ContentCopy, target)
        }
        file.detail?.let { Text(it, color = Muted, fontSize = 11.sp) }
        if (showsPreview) OutputPreview(file, url)
    }
}

/** The previewed file, small: an image inline, or the first lines of text. */
@Composable
private fun OutputPreview(file: TaskOutputFile, url: String?) {
    val context = LocalContext.current
    val image = TaskDetailRules.isImage(file.mediaType, file.relativePath)
    var text by remember(file.id, url) { mutableStateOf<String?>(null) }
    var bitmap by remember(file.id, url) { mutableStateOf<ImageBitmap?>(null) }
    var failed by remember(file.id, url) { mutableStateOf(false) }
    LaunchedEffect(file.id, url) {
        val target = url?.takeIf { isMagicianHost(context, it) }
        if (target == null) { failed = true; return@LaunchedEffect }
        val bytes = fetchPreviewBytes(context, target, if (image) 6_000_000 else 16_000)
        if (bytes == null) { failed = true; return@LaunchedEffect }
        if (image) {
            bitmap = withContext(Dispatchers.Default) {
                runCatching { BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap() }.getOrNull()
            }
            if (bitmap == null) failed = true
        } else {
            val raw = String(bytes, Charsets.UTF_8)
            // An HTML file previews as its text, not its markup.
            val html = file.mediaType?.substringBefore(';')?.trim().equals("text/html", ignoreCase = true) ||
                file.relativePath.endsWith(".html", ignoreCase = true)
            text = (if (html) taskSummaryMarkdown(raw) else raw).lines().take(PREVIEW_LINES).joinToString("\n")
        }
    }
    val shape = RoundedCornerShape(8.dp)
    if (image) {
        val shown = bitmap
        when {
            shown != null -> Image(
                shown, contentDescription = "Preview of ${file.displayName}",
                contentScale = ContentScale.FillWidth,
                modifier = Modifier.padding(top = 6.dp).fillMaxWidth().heightIn(max = 220.dp).clip(shape),
            )
            !failed -> PreviewSpinner()
        }
        return
    }
    val shown = text ?: file.bodySnippet
    if (shown == null) {
        if (!failed) PreviewSpinner()
        return
    }
    Text(
        shown,
        color = Ink,
        fontSize = 11.sp,
        lineHeight = 15.sp,
        fontFamily = LocalMagicanFontFamilies.current.mono,
        maxLines = PREVIEW_LINES,
        overflow = TextOverflow.Ellipsis,
        modifier = Modifier.padding(top = 6.dp).fillMaxWidth().background(Control, shape).padding(10.dp),
    )
}

@Composable
private fun PreviewSpinner() {
    Box(Modifier.fillMaxWidth().padding(8.dp), contentAlignment = Alignment.Center) {
        CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp, color = Coral)
    }
}

private const val PREVIEW_LINES = 6

private fun isMagicianHost(context: android.content.Context, url: String): Boolean {
    val base = MagicianAccess.baseUrl(context).trim().trimEnd('/')
    return base.isNotEmpty() && url.startsWith(base)
}

/** An authenticated GET of at most [limit] bytes; null when anything fails. */
private suspend fun fetchPreviewBytes(context: android.content.Context, url: String, limit: Int): ByteArray? =
    withContext(Dispatchers.IO) {
        runCatching {
            val connection = java.net.URL(url).openConnection() as java.net.HttpURLConnection
            try {
                connection.instanceFollowRedirects = false
                connection.connectTimeout = 15_000
                connection.readTimeout = 30_000
                MagicianAccess.headers(context).forEach(connection::setRequestProperty)
                if (connection.responseCode !in 200..299) return@runCatching null
                connection.inputStream.use { input ->
                    val out = java.io.ByteArrayOutputStream()
                    val buffer = ByteArray(8_192)
                    while (out.size() < limit) {
                        val read = input.read(buffer, 0, minOf(buffer.size, limit - out.size()))
                        if (read < 0) break
                        out.write(buffer, 0, read)
                    }
                    out.toByteArray()
                }
            } finally {
                connection.disconnect()
            }
        }.getOrNull()
    }

// Run

/** Run and Result share chat's verification popup and block Markdown layout. */
@Composable
internal fun TaskAnswerSummary(text: String) {
    val display = remember(text) { splitAnswerVerification(text) }
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        display.verification?.let { note ->
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text("Summary", color = Muted, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
                AnswerVerificationInfo(note)
            }
        }
        if (display.body.isNotEmpty()) {
            MarkdownText(taskSummaryMarkdown(display.body), color = Ink, fontSize = 14.sp, lineHeight = 20.sp)
        }
    }
}

@Composable
private fun RunBody(projection: TaskDetailProjection, task: TaskV3, viewModel: TasksViewModel) {
    var showAll by remember { mutableStateOf(false) }
    var openSteps by remember { mutableStateOf(emptySet<String>()) }
    var offPlanOpen by remember { mutableStateOf(false) }
    val events = projection.activity
    val shown = if (showAll) events else events.takeLast(EVENT_PREVIEW)
    // Off-plan attribution: each plan step's activity, and the work outside
    // every step. Unattributed runs group nothing and render as before.
    val groups = remember(events) { PlanStepActivity.group(events, { it.atMillis }) { it.attribution } }
    val delegatedAgents = remember(projection.delegations) { projection.delegations.associate { it.executionId to it.agentId } }
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        val latest = projection.executions.firstOrNull()?.id
        val selected = projection.selectedExecutionId
        if (selected != null && latest != null && selected != latest) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Showing run ", color = Muted, fontSize = 12.sp)
                CopyableText(
                    TaskDetailRules.shortId(selected),
                    copyValue = selected,
                    what = "run id",
                    style = TextStyle(color = Muted, fontSize = 12.sp, fontFamily = LocalMagicanFontFamilies.current.mono),
                    modifier = Modifier.weight(1f),
                )
                IconChip("Latest run", null) { viewModel.selectDetailExecution(task, latest) }
            }
        }
        projection.executionError?.takeIf(String::isNotBlank)?.let {
            DetailNotice(it, activePalette.danger, Icons.Outlined.ErrorOutline)
        }
        if (projection.stepRows.isNotEmpty()) {
            Column(Modifier.fillMaxWidth()) {
                projection.stepRows.forEachIndexed { index, row ->
                    if (index > 0) LivelyStepDivider(startInset = CHECK_TEXT_INSET)
                    val stepId = row.stepId
                    val stepRows = stepId?.let { groups.byStep[it] }.orEmpty()
                    val open = stepId != null && stepId in openSteps
                    PlanStepCheckRow(
                        row,
                        activityCount = groups.count(stepId),
                        expanded = open,
                        onToggle = stepId?.let { id -> { openSteps = if (id in openSteps) openSteps - id else openSteps + id } },
                    )
                    if (open && stepRows.isNotEmpty()) {
                        AttributedRows(stepRows, projection.timelineOrigin, delegatedAgents)
                    }
                }
                if (groups.attributed && groups.offPlan.isNotEmpty()) {
                    LivelyStepDivider()
                    OffPlanGroup(
                        rows = groups.offPlan,
                        open = offPlanOpen,
                        onToggle = { offPlanOpen = !offPlanOpen },
                        origin = projection.timelineOrigin,
                        delegatedAgents = delegatedAgents,
                    )
                }
            }
        }
        projection.runSummary?.takeIf(String::isNotBlank)?.let { TaskAnswerSummary(it) }
        if (events.isEmpty()) {
            Text(if (selected == null) "Not started yet." else "No activity recorded yet.", color = Muted, fontSize = 14.sp)
        } else {
            Text(
                listOfNotNull(
                    // With attribution the steps above hold the grouped view;
                    // this flat list is all of the run's activity.
                    if (groups.attributed) "All activity" else "Timeline",
                    "${events.size} ${if (events.size == 1) "event" else "events"}",
                    projection.runDurationText,
                )
                    .joinToString(" · "),
                color = Muted, fontSize = 12.sp, fontWeight = FontWeight.SemiBold,
            )
            Column(Modifier.fillMaxWidth()) {
                shown.forEachIndexed { index, event ->
                    RunEventRow(
                        event = event,
                        origin = projection.timelineOrigin,
                        delegatedAgent = event.executionId?.let(delegatedAgents::get),
                        first = index == 0,
                        last = index == shown.lastIndex,
                    )
                }
            }
            if (events.size > shown.size) IconChip("Show all ${events.size} events", null) { showAll = true }
        }
    }
}

private const val EVENT_PREVIEW = 30
private val CHECK_TEXT_INSET: Dp = 28.dp

/** A step's (or the off-plan group's) activity, indented under it. */
@Composable
private fun AttributedRows(rows: List<DetailActivity>, origin: Long?, delegatedAgents: Map<String, String>) {
    Column(Modifier.fillMaxWidth().padding(start = CHECK_TEXT_INSET, bottom = 4.dp)) {
        rows.forEachIndexed { index, event ->
            RunEventRow(
                event = event,
                origin = origin,
                delegatedAgent = event.executionId?.let(delegatedAgents::get),
                first = index == 0,
                last = index == rows.lastIndex,
            )
        }
    }
}

/** "Outside the plan · N": muted, folded, explained. */
@Composable
private fun OffPlanGroup(
    rows: List<DetailActivity>,
    open: Boolean,
    onToggle: () -> Unit,
    origin: Long?,
    delegatedAgents: Map<String, String>,
) {
    val label = PlanStepActivity.offPlanLabel(rows.size)
    Column(Modifier.fillMaxWidth().alpha(0.85f)) {
        Row(
            Modifier.fillMaxWidth().clickable(onClick = onToggle).padding(top = Lively.stepRowPadding)
                .semantics(mergeDescendants = true) {
                    contentDescription = "$label. ${PlanStepActivity.OFF_PLAN_EXPLANATION}"
                },
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(Modifier.width(18.dp), contentAlignment = Alignment.Center) {
                Icon(
                    if (open) Icons.Outlined.ExpandMore else Icons.AutoMirrored.Outlined.KeyboardArrowRight,
                    null, tint = Muted, modifier = Modifier.size(14.dp),
                )
            }
            Spacer(Modifier.width(10.dp))
            Text(label, style = livelyStepTitleStyle(Muted))
        }
        Text(
            PlanStepActivity.OFF_PLAN_EXPLANATION,
            style = livelyStepMetaStyle(),
            modifier = Modifier.padding(start = CHECK_TEXT_INSET, bottom = if (open) 2.dp else Lively.stepRowPadding),
        )
        if (open) AttributedRows(rows, origin, delegatedAgents)
    }
}

/**
 * One plan step in the Run checklist: ✓ done, ✕ failed, a breathing dot in
 * progress, ○ pending, ↷ skipped, and "not reached" after the run; the
 * agent's note underneath.
 */
@Composable
private fun PlanStepCheckRow(
    row: PlanStepRow,
    /** Off-plan attribution: the step's activity count; null when the run is not attributed. */
    activityCount: Int? = null,
    expanded: Boolean = false,
    onToggle: (() -> Unit)? = null,
) {
    val dimmed = row.mark in setOf(PlanStepMark.Pending, PlanStepMark.NotReached, PlanStepMark.Skipped)
    val word = when (row.mark) {
        PlanStepMark.Done -> "done"
        PlanStepMark.Failed -> "failed"
        PlanStepMark.InProgress -> "in progress"
        PlanStepMark.Pending -> "pending"
        PlanStepMark.Skipped -> "skipped"
        PlanStepMark.NotReached -> "not reached"
    }
    Row(
        Modifier.fillMaxWidth().padding(vertical = Lively.stepRowPadding)
            .semantics(mergeDescendants = true) {
                contentDescription = "${row.name}, $word" + (row.note?.let { ". $it" } ?: "") +
                    (activityCount?.let { ", $it ${if (it == 1) "activity" else "activities"}" } ?: "")
            },
        verticalAlignment = Alignment.Top,
    ) {
        Box(Modifier.width(18.dp).padding(top = 2.dp), contentAlignment = Alignment.Center) {
            when (row.mark) {
                PlanStepMark.Done -> Icon(Icons.Outlined.Check, null, tint = activePalette.success, modifier = Modifier.size(14.dp))
                PlanStepMark.Failed -> Icon(Icons.Outlined.Close, null, tint = activePalette.danger, modifier = Modifier.size(14.dp))
                PlanStepMark.InProgress -> Box(Modifier.padding(top = 2.dp)) { StatusDot(StatusTone.Running, size = 9.dp) }
                PlanStepMark.Pending -> Icon(Icons.Outlined.RadioButtonUnchecked, null, tint = Muted, modifier = Modifier.size(12.dp))
                PlanStepMark.Skipped -> Icon(Icons.AutoMirrored.Outlined.Redo, null, tint = Muted, modifier = Modifier.size(13.dp))
                PlanStepMark.NotReached -> Box(Modifier.padding(top = 2.dp).size(11.dp).border(1.dp, Muted.copy(alpha = .6f), CircleShape))
            }
        }
        Spacer(Modifier.width(10.dp))
        Column(Modifier.weight(1f)) {
            Row(verticalAlignment = Alignment.Top) {
                Text(
                    row.name,
                    style = livelyStepTitleStyle(if (dimmed) Muted else Ink),
                    fontWeight = if (row.mark == PlanStepMark.InProgress) FontWeight.SemiBold else FontWeight.Normal,
                    textDecoration = if (row.mark == PlanStepMark.Skipped) TextDecoration.LineThrough else null,
                    modifier = Modifier.weight(1f),
                )
                if (row.mark == PlanStepMark.NotReached) {
                    Text("not reached", style = livelyStepMetaStyle(), fontWeight = FontWeight.Medium)
                }
                activityCount?.let { count ->
                    Spacer(Modifier.width(6.dp))
                    val canOpen = count > 0 && onToggle != null
                    Text(
                        "$count",
                        style = livelyStepMetaStyle(),
                        color = if (expanded) Ink else Muted,
                        fontWeight = if (expanded) FontWeight.SemiBold else FontWeight.Normal,
                        modifier = Modifier
                            .alpha(if (canOpen) 1f else 0.6f)
                            .border(0.5.dp, Muted.copy(alpha = .35f), RoundedCornerShape(4.dp))
                            .then(if (canOpen) Modifier.clickable { onToggle?.invoke() } else Modifier)
                            .padding(horizontal = 5.dp),
                    )
                }
            }
            row.note?.let { Text(it, style = livelyStepMetaStyle()) }
        }
    }
}

/** One run timeline event: a dot on a rail, the title, when, and the body. */
@Composable
private fun RunEventRow(event: DetailActivity, origin: Long?, delegatedAgent: String?, first: Boolean, last: Boolean) {
    val tone = when (event.status.lowercase()) {
        "running", "in_progress", "executing" -> StatusTone.Running
        "failed", "error" -> StatusTone.Failed
        "waiting", "paused", "warning" -> StatusTone.Waiting
        "done", "completed", "success", "succeeded" -> StatusTone.Done
        else -> StatusTone.NotStarted
    }
    val rail = livelyStepHairlineColor()
    Row(Modifier.fillMaxWidth().height(IntrinsicSize.Min)) {
        Column(Modifier.width(10.dp).fillMaxHeight(), horizontalAlignment = Alignment.CenterHorizontally) {
            Box(Modifier.width(1.dp).height(Lively.stepRowPadding + 4.dp).background(if (first) Color.Transparent else rail))
            StatusDot(tone, size = 7.dp)
            if (!last) Box(Modifier.width(1.dp).weight(1f).background(rail))
        }
        Spacer(Modifier.width(10.dp))
        Column(Modifier.weight(1f)) {
            if (!first) LivelyStepDivider()
            Column(Modifier.padding(vertical = Lively.stepRowPadding)) {
                Row(verticalAlignment = Alignment.Top) {
                    Text(
                        delegatedAgent?.let { "${event.title} · $it" } ?: event.title,
                        style = livelyStepTitleStyle(), fontWeight = FontWeight.Medium, modifier = Modifier.weight(1f),
                    )
                    val liveSince = event.liveSinceMillis
                    val at = event.atMillis
                    val whenText = if (at != null && origin != null && at >= origin) {
                        TaskVerdict.durationIfKnown((at - origin) / 1_000.0)?.let { "+$it" }
                    } else {
                        at?.let(::activityClock) ?: event.timestamp
                    }
                    if (liveSince != null) {
                        // A call in flight shows how long it has been running, live.
                        Spacer(Modifier.width(6.dp))
                        LiveElapsedText(
                            liveSince,
                            style = livelyStepMetaStyle(activePalette.info),
                            fontFamily = LocalMagicanFontFamilies.current.mono,
                        )
                    } else whenText?.let {
                        Spacer(Modifier.width(6.dp))
                        Text(it, style = livelyStepMetaStyle(), fontFamily = LocalMagicanFontFamilies.current.mono)
                    }
                }
                event.body?.takeIf(String::isNotBlank)?.let {
                    Text(it, style = livelyStepMetaStyle(), maxLines = 3, overflow = TextOverflow.Ellipsis)
                }
                event.model?.let { model ->
                    Text(
                        model + TaskUsageFormat.stepSuffix(event.tokens, event.cost),
                        style = livelyStepMetaStyle(Muted.copy(alpha = .85f)), maxLines = 1, overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}

// Plan

@Composable
private fun PlanBody(
    projection: TaskDetailProjection,
    busy: Boolean,
    asks: List<ai.magicbeans.magdroid.attention.AttentionItem>,
    onDecide: (PlanApprovalChoice) -> Unit,
) {
    val context = LocalContext.current
    val steps = projection.planSteps
    val live = TaskDetailRules.planningActivityIsLive(projection.planStatus)
    // A question already answerable at the top of this panel is not repeated
    // here; one the feed has not projected yet still links to Attention.
    val askIds = asks.flatMap { it.askIds }.toSet()
    val unanswerable = projection.questions.filterNot { question -> question.askId?.let(askIds::contains) == true }
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        if (unanswerable.isNotEmpty()) {
            unanswerable.forEach { question ->
                Text(question.text, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                // Answered in Attention, opened at this ask when its id is known.
                IconChip("Answer in Attention", null, prominent = true) {
                    runCatching { context.startActivity(attentionAskIntent(context, question.askId)) }
                }
            }
        }
        if (steps.isEmpty()) {
            Text(
                if (live) "The steps appear once the plan is drafted." else "This plan has no steps to show.",
                color = Muted, fontSize = 14.sp,
            )
        } else {
            Column(Modifier.fillMaxWidth()) {
                steps.forEachIndexed { index, step ->
                    if (index > 0) LivelyStepDivider(startInset = CHECK_TEXT_INSET)
                    Row(Modifier.fillMaxWidth().padding(vertical = Lively.stepRowPadding), verticalAlignment = Alignment.Top) {
                        Text(
                            "${index + 1}",
                            style = livelyStepMetaStyle(),
                            fontWeight = FontWeight.SemiBold,
                            fontFamily = LocalMagicanFontFamilies.current.mono,
                            modifier = Modifier.width(18.dp).padding(top = 1.dp),
                        )
                        Spacer(Modifier.width(10.dp))
                        Text(step, style = livelyStepTitleStyle(), modifier = Modifier.weight(1f))
                    }
                }
            }
        }
        if (TaskDetailRules.showsDraftActions(projection.planStatus, projection.status)) {
            Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(2.dp)) {
                listOf(PlanApprovalChoice.Reject, PlanApprovalChoice.Approve, PlanApprovalChoice.ApproveAndRun).forEach { choice ->
                    IconChip(
                        choice.label(),
                        null,
                        prominent = choice == PlanApprovalChoice.ApproveAndRun,
                        enabled = !busy,
                    ) { onDecide(choice) }
                }
            }
        }
        if (projection.planningEntries.isNotEmpty()) PlanningActivity(projection.planningEntries, live)
    }
}

/** `magican://attention?item=<id>`, kept in this app; no id opens Attention unselected. */
private fun attentionAskIntent(context: android.content.Context, askId: String?): android.content.Intent {
    val uri = android.net.Uri.Builder().scheme("magican").authority("attention").apply {
        askId?.trim()?.takeIf { it.isNotEmpty() }?.let { appendQueryParameter("item", it) }
    }.build()
    return android.content.Intent(android.content.Intent.ACTION_VIEW, uri).setPackage(context.packageName)
}

/**
 * Planning activity: a live list while planning (model calls as compact
 * secondary rows, warnings bold), then folded under
 * `Planning activity · N events · duration`.
 */
@Composable
private fun PlanningActivity(entries: List<PlanningEntry>, live: Boolean) {
    var expanded by remember { mutableStateOf(false) }
    val now = rememberMinuteClock()
    val suffix = TaskUsageFormat.planningSuffix(entries)
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        if (live) {
            Text("Planning activity$suffix", color = Muted, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
        } else {
            Row(
                Modifier.fillMaxWidth().heightIn(min = Lively.tapMin).clickable { expanded = !expanded },
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    "Planning activity · ${PlanProgress.activitySummary(entries)}$suffix",
                    color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f),
                )
                Icon(Icons.Outlined.ExpandMore, null, tint = Muted, modifier = Modifier.size(18.dp).rotate(if (expanded) 0f else -90f))
            }
        }
        if (live || expanded) Column(Modifier.fillMaxWidth()) {
            entries.forEachIndexed { index, entry ->
                if (index > 0) LivelyStepDivider(startInset = 18.dp)
                Row(Modifier.fillMaxWidth().padding(vertical = Lively.stepRowPadding), verticalAlignment = Alignment.Top) {
                    Box(Modifier.width(10.dp).padding(top = 4.dp), contentAlignment = Alignment.Center) {
                        if (entry.isModelCall) Icon(Icons.Outlined.Memory, null, tint = Muted, modifier = Modifier.size(10.dp))
                        else StatusDot(if (entry.warning) StatusTone.Waiting else StatusTone.Done, size = 6.dp)
                    }
                    Spacer(Modifier.width(8.dp))
                    Text(
                        planningLine(entry),
                        style = if (entry.isModelCall) livelyStepMetaStyle() else livelyStepTitleStyle(),
                        fontWeight = if (entry.warning) FontWeight.Bold else FontWeight.Normal,
                        modifier = Modifier.weight(1f),
                    )
                    Spacer(Modifier.width(6.dp))
                    Text(
                        ActSummaries.compactRelativeTime(entry.atMillis, now),
                        style = livelyStepMetaStyle(),
                        fontFamily = LocalMagicanFontFamilies.current.mono,
                    )
                }
            }
        }
    }
}

/** A model call's line with its tokens and price when the message does not already say them. */
private fun planningLine(entry: PlanningEntry): String {
    if (!entry.isModelCall) return entry.message
    val saysTokens = entry.message.contains("token", ignoreCase = true) || entry.message.contains(" tok")
    return entry.message + TaskUsageFormat.stepSuffix(if (saysTokens) null else entry.tokens, entry.costUsd)
}

// Technical details

@Composable
private fun TechnicalCard(
    projection: TaskDetailProjection,
    taskId: String,
    task: TaskV3,
    viewModel: TasksViewModel,
) {
    val detailState by viewModel.state.collectAsState()
    var expanded by remember(taskId) { mutableStateOf(false) }
    Surface(
        color = Panel.copy(alpha = Panel.alpha * 0.6f),
        shape = Lively.cardShape,
        border = BorderStroke(1.dp, BorderSoft),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(horizontal = 14.dp, vertical = 2.dp)) {
            Row(
                Modifier.fillMaxWidth().heightIn(min = Lively.tapMin).clickable { expanded = !expanded }
                    .semantics(mergeDescendants = true) {
                        contentDescription = "Technical details, " + if (expanded) "expanded" else "collapsed"
                    },
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text("Technical details", color = Ink, fontSize = 15.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
                Icon(Icons.Outlined.ExpandMore, null, tint = Muted, modifier = Modifier.size(20.dp).rotate(if (expanded) 0f else -90f))
            }
            if (expanded) Column(Modifier.padding(top = 4.dp, bottom = 12.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
                WorkRelatedTools(WorkWebContext(section = if (projection.isExecutionOnly) WorkSection.Crew else WorkSection.All, agent = task.agentId, task = if (projection.isExecutionOnly) "" else taskId, execution = projection.selectedExecutionId.orEmpty(), internal = task.isInternal, appInstallation = projection.appInstallation.orEmpty()), listOf(WorkSpecialist.Analytics, WorkSpecialist.History, WorkSpecialist.Memory, WorkSpecialist.Events, WorkSpecialist.Evals, WorkSpecialist.Budget) + if (task.isAppManaged) listOf(WorkSpecialist.Apps) else emptyList())
                projection.recurringSchedule?.let { schedule ->
                    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        Text("App schedule", color = Ink, fontWeight = FontWeight.Bold)
                        listOf("behavior_id" to "Behavior", "interval_seconds" to "Interval (seconds)", "next_due_at" to "Next due", "latest_status" to "Latest status", "waiting_for_settlement" to "Waiting for settlement").forEach { (key, label) ->
                            (schedule[key] as? JsonPrimitive)?.content?.let { value -> Text("$label: $value", color = Muted) }
                        }
                    }
                }
                projection.outputGroups.intermediates.forEach { section ->
                    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        Text(section.scope.title, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.Bold)
                        section.files.forEachIndexed { index, file ->
                            if (index > 0) HorizontalDivider(color = BorderSoft)
                            OutputFileRow(file, projection.urlFor(file), showsPreview = false, compact = true)
                        }
                    }
                }
                if (projection.executions.isNotEmpty()) Column {
                    Text("Runs", color = Ink, fontSize = 13.sp, fontWeight = FontWeight.Bold)
                    projection.executions.forEach { run ->
                        val selected = run.id == projection.selectedExecutionId
                        Row(
                            Modifier.fillMaxWidth().heightIn(min = Lively.tapMin)
                                .clickable { viewModel.selectDetailExecution(task, run.id) }
                                .semantics(mergeDescendants = true) { contentDescription = "Show run ${run.id}" },
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Box(
                                Modifier.size(14.dp).then(
                                    if (selected) Modifier.background(Coral, CircleShape)
                                    else Modifier.border(1.dp, Muted, CircleShape),
                                ),
                            )
                            Spacer(Modifier.width(10.dp))
                            Column(Modifier.weight(1f)) {
                                // The id copies (its own tap target); the rest of the row selects the run.
                                CopyableText(
                                    TaskDetailRules.shortId(run.id),
                                    copyValue = run.id,
                                    what = "run id",
                                    style = TextStyle(color = Ink, fontSize = 12.sp, fontFamily = LocalMagicanFontFamilies.current.mono),
                                )
                                Text(
                                    listOf(run.status.replace('_', ' '), historySpanLine(run.startedAt, run.endedAt))
                                        .filter(String::isNotBlank).joinToString(" · "),
                                    color = Muted, fontSize = 11.sp,
                                )
                                run.error?.let {
                                    Text(it, color = activePalette.danger, fontSize = 11.sp, maxLines = 2, overflow = TextOverflow.Ellipsis)
                                }
                            }
                        }
                    }
                }
                if (detailState.detail.historyLoading) Text("Loading older runs", color = Muted)
                detailState.detail.historyError?.let { Text(it, color = Danger) }
                if ((detailState.detail.bundle?.details?.get("next_execution_cursor") as? JsonPrimitive)?.content?.isNotBlank() == true) TextButton(onClick = viewModel::loadOlderDetailExecutions, enabled = !detailState.detail.historyLoading) { Text("Load older runs") }
                if (projection.shellEntries.isNotEmpty()) Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text("Shell", color = Ink, fontSize = 13.sp, fontWeight = FontWeight.Bold)
                    projection.shellEntries.forEach { ShellRow(it) }
                }
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    IdLine("Task", taskId, "task id")
                    projection.selectedExecutionId?.let { IdLine("Run", it, "run id") }
                    projection.planId?.let { IdLine("Plan", it, "plan id") }
                    projection.agentId.takeIf(String::isNotBlank)?.let { IdLine("Agent", it, "agent id") }
                    projection.threadId.takeIf(String::isNotBlank)?.let { IdLine("Thread", it, "thread id") }
                    task.lifecycle?.let { IdLine("Lifecycle", it) }
                }
            }
        }
    }
}

@Composable
private fun IdLine(label: String, value: String, copyAs: String? = null) {
    Row(verticalAlignment = Alignment.Top) {
        Text(label, color = Muted, fontSize = 12.sp, modifier = Modifier.width(84.dp))
        val style = TextStyle(color = Ink, fontSize = 11.sp, fontFamily = LocalMagicanFontFamilies.current.mono)
        if (copyAs == null) {
            Text(value, style = style, maxLines = 2, overflow = TextOverflow.Ellipsis)
        } else {
            CopyableText(value, copyValue = value, what = copyAs, style = style, maxLines = 2)
        }
    }
}

@Composable
private fun ShellRow(entry: DetailShellEntry) {
    Surface(color = Color(0xFF22272A), shape = RoundedCornerShape(8.dp)) {
        Column(Modifier.fillMaxWidth().padding(9.dp)) {
            Text("$ ${entry.command}", color = Color.White, fontSize = 11.sp, fontFamily = LocalMagicanFontFamilies.current.mono)
            entry.lines.take(8).forEach { Text(it, color = Color(0xFFD7E0E2), fontSize = 10.sp, fontFamily = LocalMagicanFontFamilies.current.mono) }
            Text(
                if (entry.complete) "exit ${entry.exitCode ?: 0}" else "running…",
                color = if (entry.exitCode == 0) activePalette.success else activePalette.warning, fontSize = 10.sp,
            )
        }
    }
}

// Projection

/** Everything the screen shows, read once per payload from the five task endpoints. */
private class TaskDetailProjection(
    private val seed: TaskV3,
    private val bundle: TaskDetailBundle,
    pickedExecutionId: String?,
    private val nowMillis: Long,
    private val baseUrl: String,
) {
    private val taskRecord = bundle.task.obj("task") ?: bundle.details.obj("task") ?: bundle.task.orEmpty()
    private val manifest = taskRecord.obj("manifest") ?: taskRecord
    private val taskState = taskRecord.obj("state") ?: taskRecord
    private val refs = taskRecord.obj("refs").orEmpty()
    private val overview = bundle.panel.obj("overview").orEmpty()
    private val runSection = bundle.panel.obj("run").orEmpty()
    private val outputSection = bundle.panel.obj("output").orEmpty()
    private val debug = bundle.panel.obj("debug").orEmpty()
    private val selected = debug.obj("selected_execution").orEmpty()
    private val plan = bundle.plan.obj("plan") ?: bundle.plan.orEmpty()

    val title = overview.text("title") ?: manifest.text("title") ?: seed.title.ifBlank { "Task" }
    val isExecutionOnly = seed.isExecutionOnly
    val recurringSchedule = bundle.details.obj("recurring_schedule")
    val appInstallation = ai.magicbeans.magdroid.tasks.appInstallationForTask(seed.id, manifest.text("ui_thread_id") ?: seed.uiThreadId, manifest.text("created_by") ?: seed.createdBy)
    val description = overview.text("description") ?: manifest.text("description") ?: seed.description
    val status = overview.text("status") ?: taskState.text("status") ?: seed.status
    val agentId = overview.text("active_agent_id") ?: overview.text("assigned_agent_id") ?: manifest.text("agent_id") ?: seed.agentId
    val threadId = overview.text("ui_thread_id") ?: manifest.text("ui_thread_id") ?: seed.uiThreadId
    private val currentStep = overview.int("current_step") ?: selected.int("current_step")
    val selectedExecutionId = pickedExecutionId ?: selected.text("execution_id") ?: overview.text("execution_id") ?: seed.latestRootExecutionId
    private val rawActiveExecutionId = taskState.text("active_root_execution_id") ?: seed.activeRootExecutionId ?: selectedExecutionId?.takeIf { isExecutionOnly }
    val activeExecutionId = rawActiveExecutionId?.takeIf {
        status.lowercase() in setOf("queued", "running", "planning", "paused", "executing")
    }
    val runSummary = runSection.text("summary")
    private val resultObject = outputSection.obj("result")
    private val result = resultObject.orEmpty()
    val resultSummary = result.text("summary")?.takeIf(String::isNotBlank)
        ?: taskState.text("completion_summary")?.takeIf(String::isNotBlank)
        ?: taskRecord.text("completion_summary")?.takeIf(String::isNotBlank)
        ?: seed.completionSummary?.takeIf(String::isNotBlank)
    private val resultOutcome = result.text("outcome")?.takeIf(String::isNotBlank)
        ?: taskState.text("completion_outcome")?.takeIf(String::isNotBlank)
        ?: seed.completionOutcome?.takeIf(String::isNotBlank)
    /** The detail `state`'s `completion_kind` / `open_items`, else the list row's. */
    private val completionKind = (taskState["completion_kind"] ?: taskRecord["completion_kind"])
        ?.let(TaskCompletion::kind) ?: seed.completionKind
    private val openItems = (taskState["open_items"] ?: taskRecord["open_items"])
        ?.let(TaskCompletion::openItems)?.takeIf { it.isNotEmpty() } ?: seed.openItems
    val executionError = selected.text("error_message") ?: debug.text("latest_error_message")
    val planStatus = plan.text("status") ?: seed.planStatus
    val planId = plan.text("plan_id") ?: selected.text("plan_id") ?: seed.latestPlanId
    val planMarkdown = debug.obj("taskplan")?.text("markdown") ?: plan.text("markdown")
    private val planUpdatedAt = wireInstantMillis(plan["updated_at"]) ?: wireInstantMillis(plan["approved_at"])
    val planGraphSteps = TaskDetailRules.planGraphSteps(bundle.plan)
    val hasPlanGraph = planGraphSteps.isNotEmpty()
    val synthesisPending = (taskState.bool("synthesis_pending") ?: false) ||
        taskState.array("synthesis_pending_executions").isNotEmpty() || seed.synthesisPending
    private val synthesisFailedExecutionId = taskState.text("synthesis_failed_execution_id") ?: seed.synthesisFailedExecutionId
    val synthesisFailed = !synthesisFailedExecutionId.isNullOrBlank()
    val recurrence: String? = seed.recurringDescription?.takeIf { seed.isRecurring }

    private val selectedStatus = selected.text("status")
    private val selectedStartedAt = selected.text("started_at")
    private val selectedEndedAt = selected.text("ended_at")
    private val runEnded = TaskDetailRules.runHasEnded(selectedStatus, selectedEndedAt)

    /**
     * When the run whose answer is being written ended, for the "Preparing
     * final result" clock. Null (the row then counts from when it first saw the
     * flag) when the wire gave no end, or when an older run than the latest root
     * one is selected, whose end says nothing about the pending synthesis.
     */
    val synthesisBaselineMillis: Long? = TaskTimeline.parseMillis(selectedEndedAt)?.takeIf {
        val latest = taskState.text("latest_root_execution_id") ?: seed.latestRootExecutionId
        latest == null || selectedExecutionId == null || latest == selectedExecutionId
    }
    val stepRows: List<PlanStepRow> = bundle.panel?.let { PlanSteps.rows(it, runEnded) }.orEmpty()
    val planningEntries: List<PlanningEntry> = bundle.panel?.let { PlanProgress.entries(it) }.orEmpty()
    val usageTotal = bundle.panel?.let { TaskUsageModel.total(null, TaskUsageModel.usageSnapshot(it)) }

    private val runStepNames: List<String> = selected.array("step_statuses").mapIndexed { index, element ->
        (element as? JsonObject).text("name") ?: "Step ${index + 1}"
    }
    val planSteps = TaskDetailRules.planSteps(planGraphSteps, stepRows, runStepNames)

    /** Whether the selected run is still live; a running row's clock ticks only then. */
    private val runIsLive = TaskDetailRules.isActive(selectedStatus ?: status)

    val activity: List<DetailActivity> =
        (runSection.array("activity_log").ifEmpty { runSection.array("recent_activity") }.ifEmpty { debug.array("timeline") })
            .map { element -> element as? JsonObject ?: JsonObject(emptyMap()) }
            // Planning's own rows belong to the Plan act's planning activity,
            // not the run; oldest first, so offsets read +40s, +3m, …
            .filterNot { row -> (row.obj("metadata").text("execution_id") ?: row.text("execution_id") ?: "").startsWith("planexec_") }
            .sortedBy { row -> wireInstantMillis(row["created_at"]) ?: wireInstantMillis(row["timestamp"]) ?: Long.MAX_VALUE }
            // One row per call: a call's requested / routed / settled rows fold into one.
            .let(TaskActivityCalls::fold)
            .mapIndexed { index, call ->
                val row = call.row
                val metadata = row.obj("metadata").orEmpty()
                val billed = call.settled?.obj("metadata") ?: metadata
                val input = billed.double("input_tokens")
                val outputTokens = billed.double("output_tokens")
                DetailActivity(
                    id = row.text("id") ?: "activity-$index",
                    title = TaskActivityCalls.title(call),
                    body = row.text("summary") ?: row.text("message") ?: call.start?.text("summary"),
                    status = row.text("status") ?: row.text("severity") ?: "info",
                    timestamp = row.text("created_at") ?: row.text("timestamp"),
                    model = call.model,
                    cost = billed.double("cost_usd") ?: billed.double("cost"),
                    tokens = if (input == null && outputTokens == null) null else ((input ?: 0.0) + (outputTokens ?: 0.0)).toInt(),
                    executionId = metadata.text("execution_id") ?: row.text("execution_id"),
                    atMillis = wireInstantMillis(row["created_at"]) ?: wireInstantMillis(row["timestamp"]),
                    attribution = PlanStepAttribution.of(row),
                    liveSinceMillis = if (runIsLive && call.running) call.startedAtMillis else null,
                )
            }
            .sortedBy { it.atMillis ?: Long.MAX_VALUE }
    val timelineOrigin: Long? = wireInstantMillis(selected["started_at"]) ?: activity.firstNotNullOfOrNull { it.atMillis }

    val questions: List<DetailQuestion> = (runSection.array("pending_questions") + plan.array("pending_questions"))
        .mapNotNull { element ->
            val row = element as? JsonObject ?: return@mapNotNull null
            val text = row.text("question_text") ?: row.text("question") ?: row.text("prompt") ?: return@mapNotNull null
            DetailQuestion(text, askId = row.text("correlation_id") ?: row.text("pause_state_id") ?: row.text("question_id"))
        }.distinctBy(DetailQuestion::text)

    val shellEntries: List<DetailShellEntry> = debug.array("shell_entries").map { element ->
        val row = element as? JsonObject ?: JsonObject(emptyMap())
        DetailShellEntry(
            row.text("command") ?: "Command",
            row.array("lines").mapNotNull { (it as? JsonObject)?.text("text") }, row.int("exit_code"), row.bool("is_complete") ?: false,
        )
    }

    /** Server-sent download links by path, for rows that carried one. */
    private val urlsByPath = mutableMapOf<String, String>()

    private fun rows(value: JsonElement?): List<JsonObject> =
        (value as? JsonArray)?.mapNotNull { it as? JsonObject }.orEmpty().also { list ->
            list.forEach { row ->
                val path = row.text("relative_path") ?: row.text("artifact_path") ?: return@forEach
                (row.text("download_url") ?: row.text("serving_url"))?.let { urlsByPath[path] = it }
            }
        }

    /** Task rows (deliverable marks and `internal_files` sidecars), then the selected run's own files. */
    private val files: List<TaskOutputFile> = buildFiles()

    private fun buildFiles(): List<TaskOutputFile> {
        rows(bundle.outputs.obj("outputs")?.get("outputs"))
        val taskFiles = TaskOutputClassification.taskOutputFiles(bundle.outputs).ifEmpty {
            TaskOutputClassification.files(rows(refs["outputs"]))
        }
        // Scoped like web `selectedRunOutputs`: only when the panel describes
        // that run and serialized the direct/delegated arrays.
        val panelExecution = overview.text("execution_id")
        val direct = outputSection["selected_execution_outputs"] as? JsonArray
        val delegated = outputSection["selected_child_outputs"] as? JsonArray
        val runFiles = if (panelExecution == null || direct == null || delegated == null) {
            emptyList()
        } else {
            TaskOutputClassification.files(rows(direct), TaskOutputScope.Execution) +
                TaskOutputClassification.files(rows(delegated), TaskOutputScope.Delegated) +
                TaskOutputClassification.files(rows(outputSection["selected_execution_artifacts"]), TaskOutputScope.Artifact)
        }
        return (taskFiles + runFiles).distinctBy { it.scope.wire + ":" + it.relativePath }
    }

    val outputGroups = TaskOutputGrouping.group(files, { it.scope }, { it.deliverable })

    fun urlFor(file: TaskOutputFile): String? {
        val base = baseUrl.trim().trimEnd('/')
        urlsByPath[file.relativePath]?.let { value ->
            return if (value.startsWith("http")) value else base.takeIf(String::isNotEmpty)?.let { "$it/${value.trimStart('/')}" }
        }
        return if (isExecutionOnly) null else TaskArtifactLinks.url(base, seed.id, file.relativePath)
    }

    val delegations: List<TaskDelegationGroup> = runSection.array("delegations").mapNotNull { element ->
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
        .ifEmpty { parseRecentRuns(outputSection["recent_runs"]) }

    /** The panel's open asks as feed items, answerable with the Attention form. */
    val panelAsks: List<ai.magicbeans.magdroid.attention.AttentionItem> =
        ai.magicbeans.magdroid.attention.AskSurfaces.panelItems(runSection.array("needs_attention"))

    private val attention: TaskAttention? = runSection.array("needs_attention").firstNotNullOfOrNull { element ->
        val row = element as? JsonObject ?: return@firstNotNullOfOrNull null
        val request = row.obj("hitl_request") ?: return@firstNotNullOfOrNull null
        // A source this build does not know is still an ask waiting on the owner.
        val source = TaskAttentionSource.fromWire(request.text("source")) ?: TaskAttentionSource.Agentic
        TaskAttention(
            source = source,
            summary = request.text("prompt") ?: row.text("summary"),
            raisedAtMillis = wireInstantMillis(request["at"]) ?: wireInstantMillis(row["created_at"]),
        )
    }
    val attentionSource: TaskAttentionSource? get() = attention?.source

    private val elapsedSeconds: Double? = elapsed()

    private fun elapsed(): Double? {
        val started = TaskTimeline.parseMillis(selectedStartedAt) ?: return null
        val ended = TaskTimeline.parseMillis(selectedEndedAt) ?: (if (runEnded) return null else nowMillis)
        return (ended - started) / 1_000.0
    }

    val runDurationText: String? = TaskVerdict.durationIfKnown(elapsedSeconds)
    private val currentStepLabel = currentStep?.let { index -> stepRows.getOrNull(index)?.name ?: runStepNames.getOrNull(index) }
    private val totalSteps = stepRows.size.takeIf { it > 0 } ?: runStepNames.size.takeIf { it > 0 }

    val verdict: TaskVerdictValue = TaskVerdict.deriveForTaskStatus(TaskVerdictInput(
        status = status,
        attention = attention,
        error = executionError,
        currentStep = currentStep?.plus(1),
        totalSteps = totalSteps,
        currentStepLabel = currentStepLabel,
        elapsedSeconds = elapsedSeconds,
        lastProgressAtMillis = ai.magicbeans.magdroid.tasks.TaskRunEvidence.latest(
            wireInstantMillis(taskState["last_progress_at"]), bundle.withTaskProgress().observedProgressAt),
        nowMillis = nowMillis,
        completionKind = completionKind,
        openItems = openItems,
        synthesisPending = synthesisPending,
    ))

    val hasPlan = !isExecutionOnly && (seed.hasPlan || planStatus != null || planId != null || !planMarkdown.isNullOrBlank() || hasPlanGraph)

    /** The plan glyph by the title: any sign of a plan on the row, the record or the plan payload. */
    val planned = !isExecutionOnly && (hasPlan || TaskPlanMarker.hasPlan(seed) || TaskPlanMarker.hasPlan(taskRecord) ||
        TaskPlanMarker.hasPlan(bundle.plan) || stepRows.isNotEmpty())
    val acts: List<TaskDetailAct> = TaskActs.deriveActs(ActCapabilities(
        hasPlanAct = hasPlan,
        hasRunAct = bundle.panel != null || selectedExecutionId != null || activity.isNotEmpty() || executions.isNotEmpty(),
        hasOutputAct = !isExecutionOnly && (bundle.outputs != null || files.isNotEmpty() || resultObject != null ||
            resultSummary != null || resultOutcome != null),
    ))

    private val live = runIsLive

    val planSummary: String = ActSummaries.planSummary(PlanSummaryInput(
        status = TaskPlanStatus.fromWire(planStatus),
        approvedAtMillis = planUpdatedAt,
        questions = questions.map { it.text },
        nowMillis = nowMillis,
    ))

    fun summary(act: TaskDetailAct): String = when (act) {
        TaskDetailAct.Plan -> planSummary
        TaskDetailAct.Output -> ActSummaries.outputSummaryOfFiles(outputGroups.deliverables)
        TaskDetailAct.Run -> TaskDetailRules.runActSummary(
            stepRows = stepRows,
            live = live,
            input = RunSummaryInput(
                steps = runStepNames.size,
                retries = 0,
                elapsedSeconds = elapsedSeconds,
                currentStep = if (live) currentStep?.plus(1) else null,
                totalSteps = totalSteps,
                events = activity.size,
            ),
            usage = usageTotal?.run,
        )
    }

    val actionTask: TaskV3 = seed.copy(
        title = title,
        description = description,
        status = status,
        agentId = agentId,
        uiThreadId = threadId,
        lifecycle = if (isExecutionOnly) "execution_only" else manifest.text("lifecycle") ?: seed.lifecycle,
        planStatus = planStatus,
        latestPlanId = planId,
        hasPlan = hasPlan,
        activeRootExecutionId = activeExecutionId,
        latestRootExecutionId = selectedExecutionId ?: seed.latestRootExecutionId,
        synthesisPending = synthesisPending,
        synthesisFailedExecutionId = synthesisFailedExecutionId,
        completionSummary = resultSummary,
        completionOutcome = resultOutcome,
        completionKindWire = completionKind?.let { JsonPrimitive(it) } ?: seed.completionKindWire,
        openItemsWire = if (openItems.isEmpty()) seed.openItemsWire else JsonArray(openItems.map { JsonPrimitive(it) }),
    )
}

private data class DetailActivity(
    val id: String,
    val title: String,
    val body: String?,
    val status: String,
    val timestamp: String?,
    val model: String?,
    val cost: Double?,
    val tokens: Int? = null,
    val executionId: String? = null,
    val atMillis: Long? = null,
    /** Off-plan attribution: the plan step this row was recorded on, or outside the plan. */
    val attribution: PlanStepAttribution = PlanStepAttribution.NONE,
    /** When a call still running on a live run began; its row shows a ticking elapsed clock. */
    val liveSinceMillis: Long? = null,
)

private data class DetailQuestion(
    val text: String,
    /** The ask's correlation id when the row names one; Attention selects by it. */
    val askId: String? = null,
)

private data class DetailShellEntry(val command: String, val lines: List<String>, val exitCode: Int?, val complete: Boolean)
private data class DetailExecution(val id: String, val status: String, val startedAt: String?, val endedAt: String?, val error: String?)

private fun parseExecutions(value: JsonElement?): List<DetailExecution> = value.arrayElements().mapNotNull { element ->
    val root = element as? JsonObject ?: return@mapNotNull null
    val row = root.obj("state") ?: root
    val id = row.text("execution_id") ?: return@mapNotNull null
    DetailExecution(id, row.text("status") ?: "unknown", row.text("started_at"), row.text("completed_at") ?: row.text("ended_at"), row.text("error_message"))
}

private fun parseRecentRuns(value: JsonElement?): List<DetailExecution> = value.arrayElements().mapNotNull { element ->
    val row = element as? JsonObject ?: return@mapNotNull null
    val id = row.text("execution_id") ?: return@mapNotNull null
    DetailExecution(id, row.text("status") ?: "unknown", row.text("started_at"), row.text("ended_at"), row.text("error_message"))
}

private fun JsonObject?.obj(key: String): JsonObject? = this?.get(key) as? JsonObject
private fun JsonObject?.text(key: String): String? = (this?.get(key) as? JsonPrimitive)?.contentOrNull
private fun JsonObject?.int(key: String): Int? = (this?.get(key) as? JsonPrimitive)?.intOrNull
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

/** Envelope accent: running info, done success, failed danger, waiting warning. */
internal fun delegationAccent(status: String, palette: Palette): Color = when (status.lowercase()) {
    "running", "executing", "planning" -> palette.info
    "done", "completed", "succeeded" -> palette.success
    "failed", "cancelled", "canceled", "error" -> palette.danger
    "waiting", "paused", "pending", "queued" -> palette.warning
    else -> palette.secondaryText
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
