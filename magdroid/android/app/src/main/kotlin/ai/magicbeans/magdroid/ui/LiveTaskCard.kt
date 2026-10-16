package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.chat.ArtifactKind
import ai.magicbeans.magdroid.chat.ContentBlockRecord
import ai.magicbeans.magdroid.chat.LiveTaskActivity
import ai.magicbeans.magdroid.chat.LiveTaskCardAction
import ai.magicbeans.magdroid.chat.LiveTaskCardClient
import ai.magicbeans.magdroid.chat.LiveTaskCardLoader
import ai.magicbeans.magdroid.chat.LiveTaskCardPhase
import ai.magicbeans.magdroid.chat.LiveTaskCardRules
import ai.magicbeans.magdroid.chat.LiveTaskCardState
import ai.magicbeans.magdroid.chat.LiveTaskDotTone
import ai.magicbeans.magdroid.chat.LiveTaskOutputLinks
import ai.magicbeans.magdroid.chat.LiveTaskPlanGlyph
import ai.magicbeans.magdroid.chat.LiveTaskStepBar
import ai.magicbeans.magdroid.chat.TaskStatusCard
import ai.magicbeans.magdroid.chat.TaskTitleRules
import ai.magicbeans.magdroid.chat.TaskUsageStore
import ai.magicbeans.magdroid.tasks.ExecutionControlAction
import ai.magicbeans.magdroid.tasks.ExecutionControlState
import ai.magicbeans.magdroid.tasks.PlanApprovalActions
import ai.magicbeans.magdroid.tasks.PlanApprovalOutcome
import ai.magicbeans.magdroid.tasks.PlanApprovalRequest
import ai.magicbeans.magdroid.tasks.PlanProgress
import ai.magicbeans.magdroid.tasks.PlanStepMark
import ai.magicbeans.magdroid.tasks.PlanningPhase
import ai.magicbeans.magdroid.tasks.TaskUsageFormat
import android.graphics.BitmapFactory
import android.view.HapticFeedbackConstants
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.InsertDriveFile
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.outlined.Code
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.GraphicEq
import androidx.compose.material.icons.outlined.Image
import androidx.compose.material.icons.outlined.IosShare
import androidx.compose.material.icons.outlined.Movie
import androidx.compose.material.icons.outlined.Pause
import androidx.compose.material.icons.outlined.PictureAsPdf
import androidx.compose.material.icons.outlined.PlayArrow
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.layout.SubcomposeLayout
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/*
 * The chat's live task card: one card for a task's whole life, from planning
 * through plan review to the run's end. Reads only `LiveTaskCardState` +
 * `LiveTaskCardRules` (bridge) and the loader's feed; no task logic here.
 * Android port of iOS `LiveTaskCardView.swift`; design §2 and §5 of
 * `docs/archive/plans/2026-10-01-mobile-lively-chat-and-task-detail-design.md`.
 */

/** Text column inset: the status dot's column (12) + spacing (6). */
private val CARD_TEXT_INSET = 18.dp

/** The narrowest the title may get before the actions leave its row. */
private val TITLE_MIN_WIDTH = 110.dp

private val STEP_NUMBER_WIDTH = 18.dp

/** The expand chevron's drawn box; its touch target extends without layout. */
private val CHEVRON_DP = 28.dp

private val ACTIVE_RUN_STATUSES = setOf("queued", "running", "planning", "paused", "executing")

internal fun liveTaskStatusTone(tone: LiveTaskDotTone): StatusTone = when (tone) {
    LiveTaskDotTone.NotStarted -> StatusTone.NotStarted
    LiveTaskDotTone.Running -> StatusTone.Running
    LiveTaskDotTone.Waiting -> StatusTone.Waiting
    LiveTaskDotTone.Done -> StatusTone.Done
    LiveTaskDotTone.Partial -> StatusTone.Partial
    LiveTaskDotTone.Failed -> StatusTone.Failed
    LiveTaskDotTone.Cancelled -> StatusTone.Cancelled
}

/** `**bold**` (and `__bold__`) as bold spans; everything else verbatim. The card's one-line markdown. */
internal fun liveTaskInlineMarkdown(text: String): AnnotatedString = buildAnnotatedString {
    val pattern = Regex("""\*\*(.+?)\*\*|__(.+?)__""")
    var last = 0
    for (match in pattern.findAll(text)) {
        append(text.substring(last, match.range.first))
        val inner = match.groupValues[1].ifEmpty { match.groupValues[2] }
        pushStyle(SpanStyle(fontWeight = FontWeight.Bold))
        append(inner)
        pop()
        last = match.range.last + 1
    }
    append(text.substring(last))
}

/** A human file size: `18 KB`, `1.2 MB`. */
internal fun liveTaskFileSize(bytes: Long): String = when {
    bytes < 1_000 -> "$bytes bytes"
    bytes < 1_000_000 -> "${Math.round(bytes / 1_000.0)} KB"
    bytes < 1_000_000_000 -> String.format(java.util.Locale.US, "%.1f MB", bytes / 1_000_000.0)
    else -> String.format(java.util.Locale.US, "%.1f GB", bytes / 1_000_000_000.0)
}

private val ArtifactKind.liveTaskIcon: ImageVector
    get() = when (this) {
        ArtifactKind.Image -> Icons.Outlined.Image
        ArtifactKind.Pdf -> Icons.Outlined.PictureAsPdf
        ArtifactKind.Html -> Icons.Outlined.Code
        ArtifactKind.Video -> Icons.Outlined.Movie
        ArtifactKind.Audio -> Icons.Outlined.GraphicEq
        ArtifactKind.Markdown, ArtifactKind.Json, ArtifactKind.Text -> Icons.Outlined.Description
        ArtifactKind.Other -> Icons.AutoMirrored.Outlined.InsertDriveFile
    }

private enum class BusyAction { Cancel, Run, ApproveAndRun }

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun LiveTaskCard(
    messageId: String,
    task: TaskStatusCard,
    /** The chat session, for session-owned output links. */
    sessionId: String?,
    /** Inspect: the task detail entry (the run when known). */
    onInspect: (taskId: String, executionId: String?) -> Unit,
    /** Review / Plan graph: the task's plan. */
    onReviewPlan: (taskId: String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    val view = LocalView.current
    val client = remember(context) { LiveTaskCardClient.shared(context) }
    val scope = rememberCoroutineScope()
    val loader = remember(messageId) { LiveTaskCardLoader(scope, client::json) }
    val feed by loader.feed.collectAsState()
    val usageRevision by TaskUsageStore.shared.revision.collectAsState()
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    val lifecycleState by lifecycle.currentStateFlow.collectAsState()
    val appActive = lifecycleState.isAtLeast(Lifecycle.State.RESUMED)

    var expanded by remember(messageId) { mutableStateOf(false) }
    var visible by remember { mutableStateOf(false) }
    var confirmCancel by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf<BusyAction?>(null) }
    var toast by remember { mutableStateOf<String?>(null) }
    var toastToken by remember { mutableIntStateOf(0) }

    // The chat's model, reconciled with the task's stored status the loader
    // fetched (the chat posts no message when a plan is approved).
    val current = feed.storeStatus?.let { LiveTaskCardRules.reconciled(task, it) } ?: task
    val input = LiveTaskCardRules.input(current, messageId)
    val state = LiveTaskCardState.of(input)
    val phase = LiveTaskCardRules.phase(state, LiveTaskCardRules.effectiveStatus(input))
    val track = PlanProgress.currentPhase(feed.planningEntries)
    val planSteps = feed.plan?.steps.orEmpty()
    val activityLines = LiveTaskActivity.recent(feed.activityLines)
    val hasActivity = phase == LiveTaskCardPhase.Running && activityLines.isNotEmpty()
    val reasonOverflows = LiveTaskCardRules.reasonOverflows(state.reason)
    val activeExecution = feed.activeRootExecutionId?.trim()?.takeIf {
        it.isNotEmpty() && (feed.storeStatus ?: current.status).lowercase() in ACTIVE_RUN_STATUSES
    }
    val expandable = state.isExpandable(
        activityHasContent = hasActivity,
        planStepCount = planSteps.size,
        reasonOverflows = reasonOverflows,
        expanded = expanded,
        liveActivityAvailable = true,
    ) || (phase == LiveTaskCardPhase.Running && activeExecution != null)
    val actions = LiveTaskCardRules.actions(
        phase = phase,
        hasTaskId = state.taskId != null,
        startable = state.startable,
        canCancel = state.canCancel,
        showsPlan = state.showsPlanPreview(planSteps.size),
    )

    fun showToast(message: String, millis: Long = 1_400) {
        toastToken += 1
        val token = toastToken
        toast = message
        view.announceForAccessibility(message)
        scope.launch {
            delay(millis)
            if (token == toastToken) toast = null
        }
    }

    DisposableEffect(messageId) {
        visible = true
        onDispose {
            visible = false
            loader.stop()
        }
    }
    LaunchedEffect(state.taskId, phase, visible, appActive) {
        loader.sync(
            taskId = state.taskId,
            phase = phase,
            visible = visible,
            appActive = appActive,
            wantsPlan = state.wantsPlan,
            terminal = state.terminal,
            usagePhase = LiveTaskCardRules.usagePhase(state),
        )
    }
    // A plan arriving while the reader looks at the chat gets a light tap.
    var lastPhase by remember(messageId) { mutableStateOf(phase) }
    LaunchedEffect(phase) {
        if (phase == LiveTaskCardPhase.PlanReady && lastPhase != LiveTaskCardPhase.PlanReady && visible && appActive) {
            view.performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP)
        }
        lastPhase = phase
    }
    var nowMillis by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(state.ticks) {
        while (state.ticks && isActive) {
            nowMillis = System.currentTimeMillis()
            delay(1_000)
        }
        nowMillis = System.currentTimeMillis()
    }

    fun inspect() {
        state.taskId?.let { onInspect(it, state.executionId) }
    }

    fun run(taskId: String) {
        busy = BusyAction.Run
        scope.launch {
            val error = client.execute(taskId)
            busy = null
            if (error == null) loader.refreshStoreStatus(taskId)
            showToast(error ?: "Run started", if (error == null) 1_400 else 3_000)
        }
    }

    fun approveAndRun(taskId: String) {
        busy = BusyAction.ApproveAndRun
        scope.launch {
            try {
                val planId = feed.plan?.planId ?: loader.fetchPlan(taskId)?.planId
                if (planId == null) {
                    showToast("The plan could not be loaded", 3_000)
                    return@launch
                }
                val outcome = PlanApprovalActions.approveAndRun(
                    PlanApprovalRequest(correlationId = planId, taskId = taskId),
                    client,
                )
                if (outcome.isApproved) loader.refreshStoreStatus(taskId)
                showToast(outcome.message, if (outcome == PlanApprovalOutcome.ApprovedAndStarted) 1_800 else 3_500)
            } finally {
                busy = null
            }
        }
    }

    fun cancel() {
        val taskId = state.taskId ?: return
        busy = BusyAction.Cancel
        scope.launch {
            val error = client.cancel(taskId, activeExecution)
            busy = null
            showToast(error ?: "Cancelling…", if (error == null) 1_400 else 3_000)
        }
    }

    Box(
        modifier
            .fillMaxWidth()
            .clip(Lively.cardShape)
            .background(activePalette.card)
            .border(1.dp, activePalette.cardBorder, Lively.cardShape)
            .semantics { contentDescription = "Task ${state.title}, ${state.headline}" },
    ) {
        Column(
            Modifier.fillMaxWidth().padding(start = 12.dp, end = 12.dp, top = 10.dp, bottom = 12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            // The header and its meta row sit close (4 dp); the sections below keep 8.
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            CardHeader(
                tone = liveTaskStatusTone(LiveTaskCardRules.dotTone(state.status)),
                title = state.title,
                taskId = state.taskId,
                planned = LiveTaskPlanGlyph.shows(current, feed),
                expandable = expandable,
                expanded = expanded,
                onToggle = { expanded = !expanded },
                actions = actions,
                chip = { action ->
                    val taskId = state.taskId.orEmpty()
                    when (action) {
                        LiveTaskCardAction.Inspect -> CardChip("Inspect", onClick = ::inspect)
                        LiveTaskCardAction.Run -> CardChip(
                            if (busy == BusyAction.Run) "Starting…" else "Run",
                            prominent = true,
                            enabled = busy == null,
                            icon = if (busy == BusyAction.Run) null else Icons.Filled.PlayArrow,
                        ) { run(taskId) }
                        LiveTaskCardAction.ApproveAndRun -> CardChip(
                            if (busy == BusyAction.ApproveAndRun) "Starting…" else "Approve & Run",
                            prominent = true,
                            enabled = busy == null,
                        ) { approveAndRun(taskId) }
                        LiveTaskCardAction.Review -> CardChip("Review") { onReviewPlan(taskId) }
                        LiveTaskCardAction.PlanGraph -> CardChip("Plan graph") { onReviewPlan(taskId) }
                        LiveTaskCardAction.Cancel -> CardChip(
                            "Cancel",
                            enabled = busy == null,
                            loading = busy == BusyAction.Cancel,
                        ) { confirmCancel = true }
                    }
                },
            )
            MetaRow(state, nowMillis, state.taskId)
            }
            LiveTaskCardRules.line(
                phase = phase,
                state = state,
                planningEntries = feed.planningEntries,
                stepRows = feed.stepRows,
                latestActivity = feed.activityLines.lastOrNull(),
            )?.let { line ->
                val emphasised = LiveTaskCardRules.lineIsEmphasised(phase, track)
                Text(
                    liveTaskInlineMarkdown(line),
                    color = if (phase == LiveTaskCardPhase.Finished) Ink else Secondary,
                    fontSize = 13.sp,
                    lineHeight = 18.sp,
                    fontWeight = if (emphasised) FontWeight.Bold else FontWeight.Normal,
                    maxLines = if (expanded) Int.MAX_VALUE else if (phase == LiveTaskCardPhase.Finished) 3 else 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.padding(start = CARD_TEXT_INSET),
                )
            }
            if (phase == LiveTaskCardPhase.Planning) PlanningTrackView(track.phase, track.failed, track.retrying)
            if (phase == LiveTaskCardPhase.Running) {
                LiveTaskStepBar.layout(feed.stepRows)?.let { StepBar(it) }
            }
            UsageLine(state.taskId, usageRevision)
            if (state.outputsInBody) {
                Deliverables(
                    outputs = current.outputs,
                    limit = if (expanded) null else LiveTaskCardState.OUTPUT_PREVIEW_LIMIT,
                    hidden = state.hiddenOutputCount(expanded),
                    expanded = expanded,
                    cardTaskId = state.taskId,
                    sessionId = sessionId,
                    onMore = { expanded = true },
                )
            }
            AnimatedVisibility(visible = expanded, enter = fadeIn(), exit = fadeOut()) {
                Column(
                    Modifier.padding(start = CARD_TEXT_INSET),
                    verticalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    if (state.showsPlanPreview(planSteps.size)) PlanPreview(planSteps)
                    if (hasActivity) {
                        Column {
                            activityLines.forEachIndexed { index, line ->
                                if (index > 0) LivelyStepDivider()
                                Text(
                                    line,
                                    style = livelyStepTitleStyle(Secondary),
                                    maxLines = 2,
                                    overflow = TextOverflow.Ellipsis,
                                    modifier = Modifier.padding(vertical = Lively.stepRowPadding),
                                )
                            }
                        }
                    }
                    if (!state.outputsInBody && current.outputs.isNotEmpty()) {
                        Deliverables(
                            outputs = current.outputs,
                            limit = null,
                            hidden = 0,
                            expanded = true,
                            cardTaskId = state.taskId,
                            sessionId = sessionId,
                            onMore = {},
                        )
                    }
                    if (phase == LiveTaskCardPhase.Running && activeExecution != null) {
                        RunControls(activeExecution, client, onToast = { showToast(it, 2_000) })
                    }
                    if (current.terminalLines.isNotEmpty()) TaskTerminalLines(current.terminalLines)
                }
            }
        }
        AnimatedVisibility(
            visible = toast != null,
            enter = fadeIn(),
            exit = fadeOut(),
            modifier = Modifier.align(Alignment.BottomCenter).padding(bottom = 8.dp),
        ) {
            Text(
                toast.orEmpty(),
                color = OnAccent,
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .clip(RoundedCornerShape(8.dp))
                    .background(AccentFill)
                    .padding(horizontal = 12.dp, vertical = 6.dp),
            )
        }
    }

    if (confirmCancel) {
        val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
        ModalBottomSheet(
            onDismissRequest = { confirmCancel = false },
            sheetState = sheetState,
            containerColor = Panel,
            shape = RoundedCornerShape(topStart = 16.dp, topEnd = 16.dp),
        ) {
            Column(
                Modifier.fillMaxWidth().padding(horizontal = 20.dp).padding(bottom = 16.dp).navigationBarsPadding(),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Text("Cancel this task?", color = Ink, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
                Text("The run stops and the task is marked cancelled.", color = Secondary, fontSize = 13.sp)
                Spacer(Modifier.height(4.dp))
                SheetButton("Cancel task", destructive = true) {
                    confirmCancel = false
                    cancel()
                }
                SheetButton("Keep running") { confirmCancel = false }
            }
        }
    }
}

@Composable
private fun SheetButton(label: String, destructive: Boolean = false, onClick: () -> Unit) {
    Surface(
        color = if (destructive) activePalette.danger else Control,
        contentColor = if (destructive) Color.White else Ink,
        shape = RoundedCornerShape(10.dp),
        border = if (destructive) null else BorderStroke(1.dp, ControlBorder),
        modifier = Modifier
            .fillMaxWidth()
            .defaultMinSize(minHeight = Lively.tapMin)
            .clip(RoundedCornerShape(10.dp))
            .clickable(role = Role.Button, onClick = onClick),
    ) {
        Box(Modifier.padding(vertical = 13.dp), contentAlignment = Alignment.Center) {
            Text(label, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
        }
    }
}

/** One chip of the action cluster: a rounded square, 48 dp hit area. */
@Composable
private fun CardChip(
    label: String,
    prominent: Boolean = false,
    enabled: Boolean = true,
    loading: Boolean = false,
    icon: ImageVector? = null,
    onClick: () -> Unit,
) {
    SlimCardChip(onClick = onClick, prominent = prominent, enabled = enabled) {
        if (loading) {
            CircularProgressIndicator(Modifier.size(12.dp), strokeWidth = 1.5.dp, color = Secondary)
        } else {
            icon?.let { Icon(it, contentDescription = null, tint = if (prominent) OnAccent else Ink, modifier = Modifier.size(13.dp)) }
            Text(label, style = livelyChipTextStyle(prominent), maxLines = 1)
        }
    }
}

/** The card's chips are drawn this tall; the row never grows to the touch size. */
internal const val CARD_CHIP_HEIGHT_DP = 24

/**
 * A rounded-square chip drawn [CARD_CHIP_HEIGHT_DP] tall. Its layout is the
 * drawn size: Compose extends a small clickable's hit area to the 48 dp
 * minimum touch target in hit testing, without adding layout height.
 */
@Composable
private fun SlimCardChip(
    onClick: () -> Unit,
    prominent: Boolean = false,
    enabled: Boolean = true,
    content: @Composable () -> Unit,
) {
    Row(
        Modifier
            .height(CARD_CHIP_HEIGHT_DP.dp)
            .defaultMinSize(minWidth = CARD_CHIP_HEIGHT_DP.dp)
            .graphicsLayer { alpha = if (enabled) 1f else 0.45f }
            .clip(Lively.chipShape)
            .background(if (prominent) AccentFill else Control)
            .border(1.dp, if (prominent) AccentFill else ControlBorder, Lively.chipShape)
            .clickable(enabled = enabled, role = Role.Button, onClick = onClick)
            .padding(horizontal = 9.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(5.dp, Alignment.CenterHorizontally),
    ) { content() }
}

/**
 * The title row. Every task action sits in one cluster beside the title; when
 * the cluster and a readable title do not fit on one row, the whole cluster
 * moves to one row directly under the title, wrapping only when it must.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun CardHeader(
    tone: StatusTone,
    title: String,
    /** The full task id, for the short-id chip beside the title. */
    taskId: String?,
    /** A planned task: the plan glyph before the title. */
    planned: Boolean,
    expandable: Boolean,
    expanded: Boolean,
    onToggle: () -> Unit,
    actions: List<LiveTaskCardAction>,
    chip: @Composable (LiveTaskCardAction) -> Unit,
) {
    val density = LocalDensity.current
    val dot: @Composable () -> Unit = {
        Box(Modifier.width(12.dp), contentAlignment = Alignment.Center) { StatusDot(tone, size = 9.dp) }
    }
    val titleText: @Composable (Modifier) -> Unit = { mod ->
        Row(mod, verticalAlignment = Alignment.Top) {
        if (planned) {
            // Aligned to the title's first line (20 sp line height).
            TaskPlanGlyph(Modifier.padding(top = 4.dp, end = 5.dp))
        }
        // `Task · <short id>` prints as `Task` beside the chip (iOS
        // `idStandInShortId`): the id is shown once, as the copy chip.
        val standIn = TaskTitleRules.idStandInShortId(title, taskId) != null
        Text(
            TaskTitleRules.cardHeading(title, taskId),
            color = Ink,
            fontSize = 15.sp,
            lineHeight = 20.sp,
            fontWeight = FontWeight.SemiBold,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f, fill = false).then(
                if (expandable) Modifier.clickable(role = Role.Button, onClickLabel = if (expanded) "Collapse task" else "Expand task", onClick = onToggle)
                else Modifier,
            ),
        )
        taskId?.let { ShortIdChip(it, dashed = standIn) }
        }
    }
    val chevron: @Composable () -> Unit = {
        if (expandable) {
            Box(
                Modifier
                    .size(CHEVRON_DP)
                    .clickable(role = Role.Button, onClickLabel = if (expanded) "Collapse task" else "Expand task", onClick = onToggle),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    Icons.Outlined.ExpandMore,
                    contentDescription = if (expanded) "Collapse task" else "Expand task",
                    tint = Secondary,
                    modifier = Modifier.size(18.dp).rotate(if (expanded) 180f else 0f),
                )
            }
        }
    }
    SubcomposeLayout { constraints ->
        val probe = subcompose("probe") {
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) { actions.forEach { chip(it) } }
        }.map { it.measure(Constraints()) }
        val chipsWidth = probe.maxOfOrNull { it.width } ?: 0
        val fixed = with(density) {
            (12.dp + 6.dp + TITLE_MIN_WIDTH + 6.dp).roundToPx() + if (expandable) (CHEVRON_DP + 4.dp).roundToPx() else 0
        }
        val fits = fixed + chipsWidth <= constraints.maxWidth
        val placeables = subcompose("content") {
            if (fits) {
                Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                    dot()
                    Spacer(Modifier.width(6.dp))
                    titleText(Modifier.weight(1f))
                    Spacer(Modifier.width(6.dp))
                    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) { actions.forEach { chip(it) } }
                    if (expandable) Spacer(Modifier.width(4.dp))
                    chevron()
                }
            } else {
                Column(Modifier.fillMaxWidth()) {
                    Row(
                        Modifier.fillMaxWidth(),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        dot()
                        Spacer(Modifier.width(6.dp))
                        titleText(Modifier.weight(1f))
                        chevron()
                    }
                    FlowRow(
                        Modifier.padding(start = CARD_TEXT_INSET, top = 6.dp),
                        horizontalArrangement = Arrangement.spacedBy(6.dp),
                        verticalArrangement = Arrangement.spacedBy(6.dp),
                    ) { actions.forEach { chip(it) } }
                }
            }
        }.map { it.measure(constraints.copy(minHeight = 0)) }
        val height = placeables.sumOf { it.height }
        layout(constraints.maxWidth, height) {
            var y = 0
            placeables.forEach { it.placeRelative(0, y); y += it.height }
        }
    }
}

@Composable
private fun MetaRow(state: LiveTaskCardState, nowMillis: Long, taskId: String?) {
    Row(
        Modifier.padding(start = CARD_TEXT_INSET),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        state.tagText?.let { tag ->
            val accent = activePalette.secondaryAccentColor
            Text(
                tag,
                color = accent,
                fontSize = 11.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .clip(Lively.chipShape)
                    .background(accent.copy(alpha = 0.12f))
                    .padding(horizontal = 6.dp, vertical = 2.dp),
            )
        }
        Text(state.headline, color = Secondary, fontSize = 12.sp, fontWeight = FontWeight.Medium, maxLines = 1)
        state.elapsedText(nowMillis)?.let { elapsed ->
            Text(
                "· $elapsed",
                color = Secondary,
                fontSize = 12.sp,
                fontFamily = LocalMagicanFontFamilies.current.mono,
                modifier = Modifier.semantics { contentDescription = "Elapsed $elapsed" },
            )
        }
        if (state.needsYou) {
            Text("Needs you", color = MWarn, fontSize = 11.sp, fontWeight = FontWeight.SemiBold)
        }
    }
}

/**
 * The shortened task id beside the title; a tap copies the full id and
 * flashes "Copied ✓". [dashed] when the id stands in for a title (web
 * `cn-live-task__id`, iOS dashed chip): an outlined chip in the title's ink.
 */
@Composable
private fun ShortIdChip(taskId: String, dashed: Boolean = false) {
    val shortId = TaskTitleRules.shortTaskId(taskId) ?: return
    val flash = rememberCopyFlash()
    CopyFlashLabel(
        display = shortId,
        copied = flash.copied,
        style = TextStyle(
            color = if (dashed) Ink else Secondary,
            fontSize = if (dashed) 12.sp else 10.5.sp,
            fontFamily = LocalMagicanFontFamilies.current.mono,
        ),
        modifier = Modifier
            .padding(start = 6.dp, top = 2.dp)
            .clip(Lively.chipShape)
            .then(if (dashed) Modifier.dashedChipBorder(ControlBorder) else Modifier.background(Soft))
            .semantics { contentDescription = "Task id $shortId" }
            .clickable(role = Role.Button, onClickLabel = CopyFeedbackRules.CLICK_LABEL_ID) {
                flash.copy(CopyTarget.id(taskId))
            }
            .padding(horizontal = 5.dp, vertical = 1.dp),
    )
}

/** The four-segment planning track: finished fill, the current one pulses in the running colour. */
@Composable
private fun PlanningTrackView(current: PlanningPhase?, failed: Boolean, retrying: Boolean) {
    val segments = LiveTaskCardRules.planningSegments(
        ai.magicbeans.magdroid.tasks.PlanningTrack(phase = current, retrying = retrying, failed = failed),
    )
    val moving = rememberAnimatorDurationScale() > 0f && !failed
    val transition = rememberInfiniteTransition(label = "planning-pulse")
    val pulse by transition.animateFloat(
        initialValue = 1f,
        targetValue = 0.4f,
        animationSpec = infiniteRepeatable(tween(1_100), RepeatMode.Reverse),
        label = "planning-pulse-alpha",
    )
    Column(
        Modifier
            .padding(start = CARD_TEXT_INSET)
            .semantics {
                contentDescription = "Planning: ${(current ?: PlanningPhase.Understand).title}${if (retrying) ", retrying" else ""}"
            },
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            segments.forEach { segment ->
                val fill = when (segment) {
                    LiveTaskCardRules.SegmentState.Finished -> activePalette.running
                    LiveTaskCardRules.SegmentState.Current -> if (failed) activePalette.warning else activePalette.running
                    LiveTaskCardRules.SegmentState.Ahead -> ControlBorder
                }
                val alpha = if (segment == LiveTaskCardRules.SegmentState.Current && moving) pulse else 1f
                Box(
                    Modifier
                        .weight(1f)
                        .height(5.dp)
                        .graphicsLayer { this.alpha = alpha }
                        .clip(RoundedCornerShape(3.dp))
                        .background(fill),
                )
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            PlanningPhase.entries.forEachIndexed { index, phase ->
                val isCurrent = segments[index] == LiveTaskCardRules.SegmentState.Current
                Text(
                    phase.title,
                    color = if (isCurrent) Ink else Secondary,
                    fontSize = 10.sp,
                    fontWeight = if (isCurrent) FontWeight.SemiBold else FontWeight.Normal,
                    maxLines = 1,
                    modifier = Modifier.weight(1f),
                )
            }
        }
    }
}

@Composable
private fun StepBar(layout: LiveTaskStepBar.Layout) {
    val modifier = Modifier.padding(start = CARD_TEXT_INSET).fillMaxWidth().height(5.dp)
    when (layout) {
        is LiveTaskStepBar.Layout.Segments -> Row(modifier, horizontalArrangement = Arrangement.spacedBy(3.dp)) {
            layout.marks.forEach { mark ->
                Box(Modifier.weight(1f).height(5.dp).clip(RoundedCornerShape(3.dp)).background(stepMarkColor(mark)))
            }
        }
        is LiveTaskStepBar.Layout.Continuous -> BoxWithConstraints(modifier.clip(RoundedCornerShape(3.dp)).background(ControlBorder)) {
            val width = (maxWidth * layout.fraction.toFloat()).coerceAtLeast(5.dp)
            Box(Modifier.width(width).height(5.dp).clip(RoundedCornerShape(3.dp)).background(activePalette.running))
        }
    }
}

private fun stepMarkColor(mark: PlanStepMark): Color = when (mark) {
    PlanStepMark.Done -> activePalette.success
    PlanStepMark.Failed -> activePalette.danger
    // A running task's colour is the theme's status-running, never the accent.
    PlanStepMark.InProgress -> activePalette.running
    PlanStepMark.Skipped -> activePalette.secondaryText.copy(alpha = 0.45f)
    PlanStepMark.Pending, PlanStepMark.NotReached -> ControlBorder
}

/** `3 calls · 12.3k tok · $0.04`, planning and run together; absent until a call reported usage. */
@Composable
private fun UsageLine(taskId: String?, revision: Long) {
    val total = remember(taskId, revision) { taskId?.let { TaskUsageStore.shared.total(it) } } ?: return
    val line = TaskUsageFormat.taskLine(total.usage) ?: return
    Text(
        line,
        color = Secondary,
        fontSize = 11.sp,
        fontFamily = LocalMagicanFontFamilies.current.mono,
        maxLines = 1,
        modifier = Modifier
            .padding(start = CARD_TEXT_INSET)
            .semantics { contentDescription = TaskUsageFormat.taskAccessibilityLabel(total) },
    )
}

@Composable
private fun PlanPreview(steps: List<String>) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(
            "Plan · ${steps.size} step${if (steps.size == 1) "" else "s"}",
            color = Ink,
            fontSize = 12.sp,
            fontWeight = FontWeight.SemiBold,
        )
        Column {
            steps.forEachIndexed { index, step ->
                if (index > 0) LivelyStepDivider(startInset = STEP_NUMBER_WIDTH + 6.dp)
                Row(
                    Modifier.padding(vertical = Lively.stepRowPadding),
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    Text(
                        "${index + 1}.",
                        style = livelyStepMetaStyle(),
                        fontFamily = LocalMagicanFontFamilies.current.mono,
                        textAlign = androidx.compose.ui.text.style.TextAlign.End,
                        modifier = Modifier.width(STEP_NUMBER_WIDTH).padding(top = 1.dp),
                    )
                    Text(step, style = livelyStepTitleStyle())
                }
            }
        }
    }
}

/** The task's deliverables: the first previewed, then a row per file, then "+N more". */
@Composable
private fun Deliverables(
    outputs: List<ContentBlockRecord>,
    limit: Int?,
    hidden: Int,
    expanded: Boolean,
    cardTaskId: String?,
    sessionId: String?,
    onMore: () -> Unit,
) {
    val context = LocalContext.current
    val baseUrl = remember(context) { MagicianAccess.baseUrl(context) }
    val files = limit?.let { outputs.take(it) } ?: outputs
    fun urlOf(block: ContentBlockRecord): String? = LiveTaskOutputLinks.url(
        baseUrl,
        block,
        LiveTaskCardRules.outputHome(block.source?.type, block.source?.taskId, cardTaskId, sessionId),
    )
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        files.firstOrNull()?.let { first ->
            urlOf(first)?.let { url -> DeliverablePreview(first, url) }
        }
        files.forEach { block -> DeliverableRow(block, urlOf(block)) }
        if (hidden > 0 && !expanded) {
            CardChip("+$hidden more", onClick = onMore)
        }
    }
}

/** The first deliverable, previewed by default: an image inline, the start of a text file. */
@Composable
private fun DeliverablePreview(block: ContentBlockRecord, url: String) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val kind = ArtifactKind.from(block.mimeType, block.name())
    val client = remember(context) { LiveTaskCardClient.shared(context) }
    val open: () -> Unit = { scope.launch { AuthenticatedArtifacts.open(context, url, block.name(), block.mimeType) } }
    when (kind) {
        ArtifactKind.Image -> {
            var bitmap by remember(url) { mutableStateOf<ImageBitmap?>(null) }
            LaunchedEffect(url) {
                bitmap = withContext(Dispatchers.IO) {
                    client.bytes(url)?.let { bytes -> decodeSampled(bytes, 1_080) }
                }
            }
            bitmap?.let { image ->
                Image(
                    image,
                    contentDescription = "Open ${block.name() ?: "image"}",
                    contentScale = ContentScale.Fit,
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(max = 180.dp)
                        .clip(RoundedCornerShape(10.dp))
                        .clickable(role = Role.Button, onClick = open),
                )
            }
        }
        ArtifactKind.Markdown, ArtifactKind.Json, ArtifactKind.Text -> {
            var snippet by remember(url) { mutableStateOf<String?>(null) }
            LaunchedEffect(url) { snippet = client.textSnippet(url) }
            Text(
                snippet ?: " ",
                color = if (snippet == null) Color.Transparent else Ink,
                fontSize = if (kind == ArtifactKind.Markdown) 12.sp else 11.sp,
                lineHeight = 16.sp,
                fontFamily = if (kind == ArtifactKind.Markdown) null else LocalMagicanFontFamilies.current.mono,
                maxLines = 6,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(10.dp))
                    .background(Soft)
                    .clickable(role = Role.Button, onClickLabel = "Open", onClick = open)
                    .padding(10.dp)
                    .semantics { contentDescription = "Preview of ${block.name() ?: "file"}. Open" },
            )
        }
        else -> Unit
    }
}

private fun decodeSampled(bytes: ByteArray, maxWidth: Int): ImageBitmap? {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
    if (bounds.outWidth <= 0) return null
    var sample = 1
    while (bounds.outWidth / (sample * 2) >= maxWidth) sample *= 2
    val options = BitmapFactory.Options().apply { inSampleSize = sample }
    return BitmapFactory.decodeByteArray(bytes, 0, bytes.size, options)?.asImageBitmap()
}

@Composable
private fun DeliverableRow(block: ContentBlockRecord, url: String?) {
    // A tap on the name copies its absolute path (else the relative one) and
    // flashes "Copied ✓" in its place (shared CopyFeedback).
    val flash = rememberCopyFlash()
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val name = block.name() ?: "File"
    val kind = ArtifactKind.from(block.mimeType, name)
    val meta = listOfNotNull(kind.label.takeIf { url != null }, block.size?.let(::liveTaskFileSize))
        .joinToString(" · ")
    val copyPath = LiveTaskCardRules.copyPath(block.absolutePath, block.relativePath)
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        Icon(
            kind.liveTaskIcon,
            contentDescription = null,
            tint = Coral,
            modifier = Modifier.width(18.dp).size(16.dp),
        )
        Column(
            Modifier
                .weight(1f)
                .defaultMinSize(minHeight = Lively.tapMin)
                .clickable(enabled = copyPath != null, role = Role.Button, onClickLabel = CopyFeedbackRules.CLICK_LABEL_PATH) {
                    copyPath?.let { flash.copy(it, "path") }
                },
            verticalArrangement = Arrangement.Center,
        ) {
            val nameStyle = TextStyle(color = Ink, fontSize = 12.sp, fontWeight = FontWeight.Medium)
            if (flash.copied) {
                CopyFlashLabel(display = name, copied = true, style = nameStyle)
            } else {
                MiddleEllipsisText(name, nameStyle)
            }
            if (meta.isNotEmpty()) Text(meta, color = Secondary, fontSize = 10.sp, maxLines = 1)
        }
        if (url != null) {
            CardChip("Open") { scope.launch { AuthenticatedArtifacts.open(context, url, name, block.mimeType) } }
            SlimCardChip(onClick = { scope.launch { AuthenticatedArtifacts.share(context, url, name, block.mimeType) } }) {
                Icon(Icons.Outlined.IosShare, contentDescription = "Share $name", tint = Ink, modifier = Modifier.size(14.dp))
            }
        }
    }
}

/** One line, the middle elided (`ebike-brief-fi…-10-01.md`) so the extension stays visible. */
@Composable
private fun MiddleEllipsisText(text: String, style: TextStyle) {
    val measurer = rememberTextMeasurer()
    BoxWithConstraints(Modifier.fillMaxWidth()) {
        val maxPx = constraints.maxWidth
        val shown = remember(text, maxPx, style) {
            if (measurer.measure(text, style, maxLines = 1).size.width <= maxPx) text
            else {
                var low = 1
                var high = text.length - 1
                var best = text.take(1) + "…"
                while (low <= high) {
                    val keep = (low + high) / 2
                    val head = (keep + 1) / 2
                    val candidate = text.take(head) + "…" + text.takeLast(keep - head)
                    if (measurer.measure(candidate, style, maxLines = 1).size.width <= maxPx) {
                        best = candidate
                        low = keep + 1
                    } else {
                        high = keep - 1
                    }
                }
                best
            }
        }
        Text(shown, style = style, maxLines = 1, softWrap = false)
    }
}

/** Pause / Resume for the live run (Cancel sits in the title row). */
@Composable
private fun RunControls(executionId: String, client: LiveTaskCardClient, onToast: (String) -> Unit) {
    val scope = rememberCoroutineScope()
    var control by remember(executionId) { mutableStateOf<ExecutionControlState?>(null) }
    var busy by remember(executionId) { mutableStateOf<ExecutionControlAction?>(null) }
    LaunchedEffect(executionId) { control = client.controlState(executionId) }
    val state = control ?: return
    if (!state.canPause && !state.canResume) return
    fun perform(action: ExecutionControlAction) {
        busy = action
        scope.launch {
            val error = client.control(executionId, action)
            control = client.controlState(executionId)
            busy = null
            onToast(error ?: if (action == ExecutionControlAction.Pause) "Pausing…" else "Resuming…")
        }
    }
    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        if (state.canPause) {
            CardChip("Pause", enabled = busy == null, loading = busy == ExecutionControlAction.Pause, icon = Icons.Outlined.Pause) {
                perform(ExecutionControlAction.Pause)
            }
        }
        if (state.canResume) {
            CardChip("Resume", enabled = busy == null, loading = busy == ExecutionControlAction.Resume, icon = Icons.Outlined.PlayArrow) {
                perform(ExecutionControlAction.Resume)
            }
        }
    }
}

/** The run's streamed shell output, newest last, bounded. */
@Composable
private fun TaskTerminalLines(lines: List<String>) {
    Text(
        lines.takeLast(12).joinToString("\n") { it.ifEmpty { " " } },
        color = Color(0xFFB9C4CE),
        fontSize = 11.sp,
        lineHeight = 15.sp,
        fontFamily = LocalMagicanFontFamilies.current.mono,
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(6.dp))
            .background(Color(0xFF10141A))
            .padding(horizontal = 10.dp, vertical = 8.dp),
    )
}

/** A 1 dp dashed outline in the chip shape (iOS `StrokeStyle(dash: [3, 2])`). */
private fun Modifier.dashedChipBorder(color: Color): Modifier = drawBehind {
    val stroke = 1.dp.toPx()
    val radius = Lively.chipRadius.toPx()
    drawRoundRect(
        color = color,
        topLeft = Offset(stroke / 2, stroke / 2),
        size = Size(size.width - stroke, size.height - stroke),
        cornerRadius = CornerRadius(radius, radius),
        style = Stroke(width = stroke, pathEffect = PathEffect.dashPathEffect(floatArrayOf(3.dp.toPx(), 2.dp.toPx()))),
    )
}
