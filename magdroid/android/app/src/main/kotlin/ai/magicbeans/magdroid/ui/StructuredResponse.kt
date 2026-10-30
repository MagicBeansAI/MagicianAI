package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.chat.ActivityRow
import ai.magicbeans.magdroid.chat.StructuredBlock
import ai.magicbeans.magdroid.chat.StructuredResponse
import ai.magicbeans.magdroid.chat.LiveTaskCardClient
import ai.magicbeans.magdroid.chat.TaskUsageStore
import ai.magicbeans.magdroid.chat.activityAwaitsUser
import ai.magicbeans.magdroid.chat.activityInspectTarget
import ai.magicbeans.magdroid.chat.activityLatencyText
import ai.magicbeans.magdroid.chat.activityLiveSinceMs
import ai.magicbeans.magdroid.chat.activityPreviewRows
import ai.magicbeans.magdroid.chat.activityRowDetail
import ai.magicbeans.magdroid.chat.activityStageDots
import ai.magicbeans.magdroid.chat.activityTintKey
import ai.magicbeans.magdroid.chat.activityTintSlot
import ai.magicbeans.magdroid.tasks.ExecutionControlAction
import ai.magicbeans.magdroid.tasks.TaskUsageFormat
import ai.magicbeans.magdroid.tasks.TurnUsage
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material.icons.automirrored.outlined.List
import androidx.compose.material.icons.automirrored.outlined.OpenInNew
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Description
import androidx.compose.material.icons.filled.Handyman
import androidx.compose.material.icons.filled.PanTool
import androidx.compose.material.icons.filled.PauseCircle
import androidx.compose.material.icons.filled.Psychology
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.FindInPage
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.Forum
import androidx.compose.material.icons.outlined.MoreHoriz
import androidx.compose.material.icons.outlined.NotificationsActive
import androidx.compose.material.icons.outlined.TripOrigin
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.style.TextOverflow
import kotlinx.coroutines.launch
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * Tone maps to the palette rather than Material's semantic colours, so a warning
 * here looks like a warning everywhere else in the product.
 */
internal fun structuredToneColor(tone: String?): Color = when (tone?.lowercase()) {
    "success", "ok", "positive" -> ChatSuccess
    "warning", "caution" -> MWarn
    "danger", "error", "critical" -> Danger
    // iOS uses the active accent for an informational structured response.
    "info" -> Coral
    else -> Secondary
}

/**
 * Render a backend-composed answer.
 *
 * Magician builds replies as blocks — a summary, key values, a list, artifacts.
 * Falling back to the plain `text` field would hand the owner a paragraph where
 * they were given a table, so the blocks are drawn as blocks.
 *
 * An unrecognised kind falls through to its text rather than disappearing: a new
 * block type on the backend should degrade to readable, not to blank.
 */
@Composable
fun StructuredResponseView(response: StructuredResponse) {
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        response.title?.takeIf { it.isNotBlank() }?.let {
            Text(it, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
        }
        response.summary?.takeIf { it.isNotBlank() }?.let {
            Text(
                it,
                color = Ink,
                fontSize = CHAT_BUBBLE_FONT_SP.sp,
                lineHeight = CHAT_BUBBLE_LINE_HEIGHT_SP.sp,
            )
        }
        response.blocks.forEach { Block(it) }
    }
}

@Composable
private fun Block(block: StructuredBlock) {
    when (block.kind.lowercase()) {
        "markdown" -> block.text?.takeIf(String::isNotBlank)?.let { text ->
            Column {
                block.title?.takeIf(String::isNotBlank)?.let { title ->
                    Text(title, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                    Spacer(Modifier.height(3.dp))
                }
                MarkdownText(markdown = text, color = Ink, fontSize = 14.sp, lineHeight = 20.sp)
            }
        }
        "callout" -> Callout(block)
        "key_values" -> KeyValues(block)
        "list" -> BulletList(block)
        "artifacts" -> Artifacts(block)
        "metrics" -> KeyValues(block)
        "copy_text" -> CopyText(block)
        // Plain text and unknown kinds retain their literal text fallback.
        else -> block.text?.takeIf { it.isNotBlank() }?.let {
            Column {
                block.title?.takeIf { t -> t.isNotBlank() }?.let { t ->
                    Text(t, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                    Spacer(Modifier.height(3.dp))
                }
                Text(it, color = Ink, fontSize = 14.sp, lineHeight = 20.sp)
            }
        }
    }
}

@Composable
private fun Callout(block: StructuredBlock) {
    val tone = structuredToneColor(block.tone)
    Surface(
        color = tone.copy(alpha = 0.10f),
        shape = RoundedCornerShape(10.dp),
        border = BorderStroke(1.dp, tone.copy(alpha = 0.35f)),
    ) {
        Column(Modifier.padding(10.dp)) {
            block.title?.takeIf { it.isNotBlank() }?.let {
                Text(it, color = Ink, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
                Spacer(Modifier.height(2.dp))
            }
            block.text?.let { Text(it, color = Secondary, fontSize = 13.sp, lineHeight = 18.sp) }
        }
    }
}

@Composable
private fun KeyValues(block: StructuredBlock) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        block.title?.takeIf { it.isNotBlank() }?.let {
            Text(it, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
        }
        block.items.forEach { item ->
            val label = item["label"] ?: item["name"] ?: item["key"]
            val value = item["value"] ?: item["text"]
            if (!label.isNullOrBlank() && !value.isNullOrBlank()) {
                Column {
                    Text(label, color = Muted, fontSize = 11.sp)
                    Text(value, color = Ink, fontSize = 14.sp, lineHeight = 19.sp)
                }
            }
        }
    }
}

@Composable
private fun BulletList(block: StructuredBlock) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        block.title?.takeIf { it.isNotBlank() }?.let {
            Text(it, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(2.dp))
        }
        block.items.forEach { item ->
            val text = item["text"] ?: item["label"] ?: item["value"]
            if (!text.isNullOrBlank()) {
                Row(verticalAlignment = Alignment.Top) {
                    Text("•", color = Coral, fontSize = 14.sp, modifier = Modifier.width(14.dp))
                    Text(text, color = Ink, fontSize = 14.sp, lineHeight = 20.sp)
                }
            }
        }
    }
}

@Composable
private fun Artifacts(block: StructuredBlock) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(
            block.title?.takeIf { it.isNotBlank() } ?: "Files",
            color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold,
        )
        block.items.forEach { item ->
            val name = item["name"] ?: item["label"] ?: item["path"]
            if (!name.isNullOrBlank()) {
                Surface(
                    color = Ground,
                    shape = RoundedCornerShape(8.dp),
                    border = BorderStroke(1.dp, BorderSoft),
                ) {
                    Row(Modifier.padding(8.dp), verticalAlignment = Alignment.CenterVertically) {
                        Text("📄", fontSize = 13.sp)
                        Spacer(Modifier.width(6.dp))
                        Text(name, color = Ink, fontSize = 13.sp)
                    }
                }
            }
        }
    }
}

@Composable
private fun CopyText(block: StructuredBlock) {
    block.text?.takeIf { it.isNotBlank() }?.let { text ->
        Surface(
            color = Ground,
            shape = RoundedCornerShape(8.dp),
            border = BorderStroke(1.dp, BorderSoft),
        ) {
            Text(
                text,
                color = Ink,
                fontSize = 13.sp,
                fontFamily = LocalMagicanFontFamilies.current.mono,
                lineHeight = 19.sp,
                modifier = Modifier.padding(10.dp),
            )
        }
    }
}

/**
 * The chat turn's Steps disclosure (iOS `ActivityTimelineView`).
 *
 * The steps matter when an answer is surprising and are noise the rest of the
 * time, so settled turns start collapsed. A live turn starts open, including an
 * empty `Steps 0` state, so waiting for the first activity event never looks
 * like a frozen answer. The header is one unwrapped line: the chevron, the
 * step count once settled (live, "+N" and the dots say it), with no "Steps"
 * word (the chevron and count say it, and the label
 * wrapped once dots, tokens, cost and calls crowded the row), the latest
 * [ACTIVITY_STAGE_DOT_LIMIT] dots, the turn's usage, Inspect (the turn's task
 * run) and, while live, Stop; the inline list keeps the newest five rows,
 * compact and split by hairlines. While live there is no separate spinner: a
 * thin ring circles the chevron and the latest dot breathes
 * ([activityHeaderLiveIndicators]).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ActivitySection(
    rows: List<ActivityRow>,
    onOpenAttention: () -> Unit,
    isLive: Boolean = false,
    onOpenCompleteResult: (ActivityRow) -> Unit = {},
    /** The chat session, for a step's Open file / Reveal folder on the host. */
    sessionId: String? = null,
    /** Inspect: the task detail on this turn's run. */
    onInspectRun: (taskId: String, executionId: String) -> Unit = { _, _ -> },
) {
    if (!shouldShowActivitySection(rows.size, isLive)) return
    val context = LocalContext.current
    val client = remember(context) { LiveTaskCardClient.shared(context) }
    val scope = rememberCoroutineScope()

    var expanded by remember { mutableStateOf(isLive) }
    var userToggledExpand by remember { mutableStateOf(false) }
    var showAll by remember { mutableStateOf(false) }
    var confirmStop by remember { mutableStateOf(false) }
    var stopping by remember { mutableStateOf(false) }
    var actionError by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(isLive) {
        if (!userToggledExpand) expanded = isLive
    }

    val previewRows = remember(rows) { activityPreviewRows(rows, ACTIVITY_PREVIEW_LIMIT) }
    val hiddenCount = rows.size - previewRows.size
    val inspectTarget = remember(rows) { activityInspectTarget(rows) }
    val awaitsUser = isLive && activityAwaitsUser(rows)
    val indicators = activityHeaderLiveIndicators(isLive, awaitsUser)
    val dotWindow = remember(rows) { activityStageDots(rows) }
    // The ring takes the latest dot's tint; before the first settled step, the chevron's.
    val liveTint = dotWindow.shown.lastOrNull()?.let(::stepDotColor) ?: Secondary
    val usageRevision by TaskUsageStore.shared.revision.collectAsState()
    val turnUsage = remember(rows, usageRevision) {
        TurnUsage.summary(rows) { TaskUsageStore.shared.total(it) }
    }
    // Tasks this turn started run outside its events; their stored usage is
    // read once while the turn is live and once after.
    val spawned = remember(rows) { TurnUsage.spawnedTaskIds(rows) }
    LaunchedEffect(spawned, isLive) {
        spawned.forEach { TaskUsageStore.shared.loadSnapshot(it, if (isLive) "turn-live" else "turn") }
    }
    val onPathAction: (String, Boolean) -> Unit = { path, folder ->
        val session = sessionId
        if (session.isNullOrEmpty()) {
            actionError = "This file is not attached to a chat session."
        } else {
            scope.launch {
                client.openOnHost(session, path, folder)?.let {
                    actionError = if (folder) "The folder could not be revealed." else "The file could not be opened."
                }
            }
        }
    }

    Surface(
        modifier = Modifier
            .fillMaxWidth()
            .padding(top = ACTIVITY_SECTION_TOP_GAP_DP.dp),
        // The assistant bubble's surface with a subtle edge, as on iOS: the
        // steps read as part of the reply instead of floating on the wash.
        color = ACTIVITY_SECTION_BACKGROUND,
        shape = RoundedCornerShape(8.dp),
        border = BorderStroke(1.dp, ControlBorder),
    ) {
        Column {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    // The whole header row toggles, as on web and iOS — usage
                    // text and the gap included. Its buttons (Inspect, Stop)
                    // still take their own clicks.
                    .clickable(
                        onClickLabel = activitySectionToggleLabel(expanded),
                        role = Role.Button,
                    ) {
                        userToggledExpand = true
                        expanded = !expanded
                    }
                    // Open, the header gets a little air above and below.
                    .padding(vertical = activityHeaderVerticalPaddingDp(expanded).dp)
                    .height(ACTIVITY_HEADER_HEIGHT_DP.dp)
                    .padding(start = ACTIVITY_SECTION_HORIZONTAL_PADDING_DP.dp, end = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                Row(
                    modifier = Modifier
                        .fillMaxHeight()
                        .padding(end = 2.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    // The chevron keeps its slot; while live a thin indeterminate
                    // ring circles it. The whole row stays the touch target.
                    Box(
                        Modifier
                            .padding(end = ACTIVITY_CHEVRON_END_GAP_DP.dp)
                            .size(ACTIVITY_CHEVRON_RING_DP.dp),
                        contentAlignment = Alignment.Center,
                    ) {
                        if (indicators.chevronRing) {
                            CircularProgressIndicator(
                                modifier = Modifier.matchParentSize(),
                                color = liveTint,
                                strokeWidth = 1.dp,
                            )
                        }
                        Icon(
                            imageVector = if (expanded) Icons.Outlined.ExpandMore else Icons.Outlined.ChevronRight,
                            contentDescription = null,
                            tint = Secondary,
                            modifier = Modifier.size(if (indicators.chevronRing) 12.dp else 14.dp),
                        )
                    }
                    // Live, the dots say the progress; the count returns once settled.
                    if (indicators.stepCount) Text(
                        text = rows.size.toString(),
                        color = Muted,
                        fontSize = 11.sp,
                        lineHeight = 11.sp,
                        maxLines = 1,
                        softWrap = false,
                        modifier = Modifier.semantics { contentDescription = activityStepCountDescription(rows.size) },
                    )
                    // Live only, as on web: a finished turn reads its rows, not dots.
                    if (isLive) StepDotStrip(dotWindow, breatheLatest = indicators.breathingDot)
                    if (indicators.waitingLabel) {
                        Text(
                            activityHeaderWaitingLabel(),
                            color = MWarn,
                            fontSize = 11.sp,
                            lineHeight = 11.sp,
                            fontWeight = FontWeight.SemiBold,
                            maxLines = 1,
                            softWrap = false,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
                Box(Modifier.weight(1f)) {
                    turnUsage?.let { usage ->
                        TaskUsageFormat.turnLine(usage)?.let { line ->
                            FittingUsageText(
                                full = line,
                                short = TaskUsageFormat.turnShortLine(usage),
                                description = TaskUsageFormat.turnAccessibilityLabel(usage),
                            )
                        }
                    }
                }
                inspectTarget?.let { target ->
                    StepsHeaderChip(ACTIVITY_INSPECT_LABEL, Icons.AutoMirrored.Outlined.OpenInNew, ACTIVITY_INSPECT_DESCRIPTION) {
                        onInspectRun(target.taskId, target.executionId)
                    }
                }
                if (isLive && inspectTarget != null) {
                    SlimHeaderButton(description = "Stop run", destructive = true, enabled = !stopping, onClick = { confirmStop = true }) {
                        if (stopping) {
                            CircularProgressIndicator(Modifier.size(10.dp), strokeWidth = 1.5.dp, color = Danger)
                        } else {
                            Icon(Icons.Filled.Stop, contentDescription = null, tint = Danger, modifier = Modifier.size(11.dp))
                        }
                    }
                }
            }

            if (expanded) {
                Column(
                    modifier = Modifier.padding(
                        start = ACTIVITY_SECTION_HORIZONTAL_PADDING_DP.dp,
                        end = ACTIVITY_SECTION_HORIZONTAL_PADDING_DP.dp,
                        bottom = ACTIVITY_SECTION_EXPANDED_BOTTOM_PADDING_DP.dp,
                    ),
                ) {
                    if (rows.isEmpty() && isLive) {
                        Text(
                            text = "Waiting for the first activity update…",
                            color = Muted,
                            fontSize = 11.sp,
                        )
                    }
                    previewRows.forEachIndexed { index, row ->
                        // Compact rows split by hairlines inset to the text
                        // column; none after the last row.
                        if (index > 0) LivelyStepDivider(startInset = ACTIVITY_STEP_MARK_WIDTH_DP.dp)
                        ActivityStepRow(row, onOpenAttention, onOpenCompleteResult, onPathAction, isLive)
                    }
                    // The inline list keeps the newest few; the rest are one tap away.
                    if (hiddenCount > 0) {
                        LivelyStepDivider(startInset = ACTIVITY_STEP_MARK_WIDTH_DP.dp)
                        Text(
                            activityShowAllLabel(rows.size),
                            color = Coral,
                            fontSize = 11.sp,
                            fontWeight = FontWeight.SemiBold,
                            modifier = Modifier
                                .padding(start = ACTIVITY_STEP_MARK_WIDTH_DP.dp)
                                .clickable(role = Role.Button, onClickLabel = "Show all ${rows.size} steps") { showAll = true }
                                .padding(vertical = 4.dp),
                        )
                    }
                }
            }
        }
    }

    if (showAll) {
        ModalBottomSheet(
            onDismissRequest = { showAll = false },
            sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
            containerColor = Panel,
            shape = RoundedCornerShape(topStart = 16.dp, topEnd = 16.dp),
        ) {
            Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp)) {
                Text(
                    "Activity · ${rows.size} steps",
                    color = Ink,
                    fontSize = 15.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.padding(bottom = 8.dp),
                )
                LazyColumn(Modifier.fillMaxWidth().navigationBarsPadding()) {
                    itemsIndexed(rows) { index, row ->
                        if (index > 0) LivelyStepDivider(startInset = ACTIVITY_STEP_MARK_WIDTH_DP.dp)
                        ActivityStepRow(row, onOpenAttention, onOpenCompleteResult, onPathAction, isLive)
                    }
                }
            }
        }
    }

    if (confirmStop) {
        val target = inspectTarget
        ModalBottomSheet(
            onDismissRequest = { confirmStop = false },
            sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
            containerColor = Panel,
            shape = RoundedCornerShape(topStart = 16.dp, topEnd = 16.dp),
        ) {
            Column(
                Modifier.fillMaxWidth().padding(horizontal = 20.dp).padding(bottom = 16.dp).navigationBarsPadding(),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Text("Stop this run?", color = Ink, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
                Text("The active execution and its running children will be cancelled.", color = Secondary, fontSize = 13.sp)
                Spacer(Modifier.height(4.dp))
                ConfirmSheetButton("Stop run", destructive = true) {
                    confirmStop = false
                    if (target != null) {
                        stopping = true
                        scope.launch {
                            val error = client.control(target.executionId, ExecutionControlAction.Cancel)
                            stopping = false
                            if (error != null) actionError = error
                        }
                    }
                }
                ConfirmSheetButton("Keep running") { confirmStop = false }
            }
        }
    }

    actionError?.let { message ->
        AlertDialog(
            onDismissRequest = { actionError = null },
            confirmButton = { TextButton(onClick = { actionError = null }) { Text("OK") } },
            title = { Text("Action failed") },
            text = { Text(message) },
        )
    }
}

/** The header's "Waiting on you" (an open ask in this turn). */
internal fun activityHeaderWaitingLabel(): String = "Waiting on you"

/** The turn's usage, or its short form when the header is narrow, else nothing. */
@Composable
private fun FittingUsageText(full: String, short: String?, description: String) {
    val style = TextStyle(
        fontSize = 10.sp,
        fontFamily = LocalMagicanFontFamilies.current.mono,
        color = Secondary,
    )
    val measurer = rememberTextMeasurer()
    BoxWithConstraints(Modifier.fillMaxWidth()) {
        val maxPx = constraints.maxWidth
        val shown = remember(full, short, maxPx) {
            listOfNotNull(full, short).firstOrNull { measurer.measure(it, style, maxLines = 1).size.width <= maxPx }
        }
        shown?.let {
            Text(it, style = style, maxLines = 1, softWrap = false, modifier = Modifier.semantics { contentDescription = description })
        }
    }
}

@Composable
private fun StepsHeaderChip(label: String, icon: ImageVector, description: String, onClick: () -> Unit) {
    SlimHeaderButton(description = description, onClick = onClick) {
        Icon(icon, contentDescription = null, tint = Coral, modifier = Modifier.size(11.dp))
        Text(label, color = Coral, fontSize = 10.5.sp, lineHeight = 10.5.sp, fontWeight = FontWeight.SemiBold, maxLines = 1, softWrap = false)
    }
}

/**
 * A header button drawn 24 dp tall (6 dp rounded square). Its layout stays
 * 24 dp: Compose widens a small clickable's hit area to the 48 dp minimum
 * touch target on its own, so the touch size never inflates the row.
 */
@Composable
private fun SlimHeaderButton(
    description: String,
    destructive: Boolean = false,
    enabled: Boolean = true,
    onClick: () -> Unit,
    content: @Composable () -> Unit,
) {
    Row(
        Modifier
            .height(ACTIVITY_HEADER_CHIP_HEIGHT_DP.dp)
            .defaultMinSize(minWidth = ACTIVITY_HEADER_CHIP_HEIGHT_DP.dp)
            .clip(Lively.chipShape)
            .background(Control)
            .border(1.dp, if (destructive) Danger.copy(alpha = 0.3f) else ControlBorder, Lively.chipShape)
            .clickable(enabled = enabled, role = Role.Button, onClickLabel = description, onClick = onClick)
            .semantics(mergeDescendants = true) { contentDescription = description }
            .padding(horizontal = 7.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp, Alignment.CenterHorizontally),
    ) { content() }
}

@Composable
private fun ConfirmSheetButton(label: String, destructive: Boolean = false, onClick: () -> Unit) {
    Surface(
        color = if (destructive) Danger else Control,
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

/** A row's mark (iOS `ActivityTimelineRow.iconName`): its status first, else its kind. */
internal fun activityRowIcon(row: ActivityRow): ImageVector = when (row.status?.lowercase()) {
    "failed", "error" -> Icons.Filled.Warning
    "waiting" -> Icons.Filled.PauseCircle
    "running", "in_progress" -> Icons.Outlined.TripOrigin
    else -> when (row.kind) {
        "llm" -> Icons.Filled.Psychology
        "reasoning" -> Icons.Outlined.Forum
        "tool" -> Icons.Filled.Handyman
        "artifact" -> Icons.Filled.Description
        "pause" -> Icons.Filled.PanTool
        else -> Icons.Filled.CheckCircle
    }
}

/** The mark's colour: failed red, waiting amber, done green, else by tone. */
internal fun activityRowIconColor(row: ActivityRow): Color = when (row.status?.lowercase()) {
    "failed", "error" -> Danger
    "waiting" -> MWarn
    "done" -> ChatSuccess
    else -> when (row.tone) {
        "tool", "pause" -> MWarn
        "error" -> Danger
        "reasoning" -> ChatDiscovery
        else -> Coral
    }
}

@Composable
private fun ActivityStepRow(
    row: ActivityRow,
    onOpenAttention: () -> Unit,
    onOpenCompleteResult: (ActivityRow) -> Unit,
    onPathAction: (String, Boolean) -> Unit,
    /** The turn is live: a running model or tool row shows its elapsed time, ticking. */
    live: Boolean = false,
) {
    val detail = activityRowDetail(row)?.takeIf { it.isNotBlank() }
    val liveSince = activityLiveSinceMs(row)?.takeIf { live }
    var open by remember(row.label, row.detail) { mutableStateOf(false) }
    Row(
        verticalAlignment = Alignment.Top,
        modifier = Modifier.padding(vertical = ACTIVITY_ROW_PADDING_DP.dp),
    ) {
        Box(Modifier.width(ACTIVITY_STEP_MARK_WIDTH_DP.dp).padding(top = 2.dp)) {
            Icon(activityRowIcon(row), contentDescription = row.status, tint = activityRowIconColor(row), modifier = Modifier.size(12.dp))
        }
        Column(verticalArrangement = Arrangement.spacedBy(3.dp)) {
            // One line: the title, then its detail inline in the meta style,
            // ellipsized; a tap opens the whole detail under it.
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.Bottom) {
                Text(
                    activityRowLine(row.label, detail?.takeIf { !open }),
                    style = livelyStepTitleStyle(Secondary),
                    fontWeight = FontWeight.Medium,
                    maxLines = if (open) Int.MAX_VALUE else 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier
                        .weight(1f, fill = false)
                        .then(if (detail != null) Modifier.clickable(onClickLabel = if (open) "Hide detail" else "Show detail") { open = !open } else Modifier),
                )
                if (liveSince != null) {
                    LiveElapsedText(
                        liveSince.toLong(),
                        style = livelyStepMetaStyle(),
                        fontFamily = LocalMagicanFontFamilies.current.mono,
                    )
                } else row.durationMs?.let { ms ->
                    Text(
                        activityLatencyText(ms),
                        style = livelyStepMetaStyle(),
                        fontFamily = LocalMagicanFontFamilies.current.mono,
                        maxLines = 1,
                    )
                }
            }
            row.files.forEach { file -> StepFileChip(file.label, file.absolutePath, onPathAction) }
            if (!row.resultRef.isNullOrBlank()) {
                StepLinkChip("Open complete result", Icons.Outlined.FindInPage) { onOpenCompleteResult(row) }
            }
            detail?.takeIf { open }?.let { detail ->
                MarkdownText(
                    markdown = detail,
                    color = Secondary,
                    fontSize = Lively.STEP_META_SP.sp,
                    lineHeight = 14.sp,
                )
            }
            if (row.status == "waiting") {
                StepLinkChip("Open in Attention", Icons.Outlined.NotificationsActive, onOpenAttention)
            }
        }
    }
}

@Composable
private fun StepLinkChip(label: String, icon: ImageVector, onClick: () -> Unit) {
    Row(
        Modifier
            .defaultMinSize(minHeight = ACTIVITY_HEADER_CHIP_HEIGHT_DP.dp)
            .clip(RoundedCornerShape(6.dp))
            .background(Coral.copy(alpha = 0.09f))
            .border(1.dp, Coral.copy(alpha = 0.22f), RoundedCornerShape(6.dp))
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Icon(icon, contentDescription = null, tint = Coral, modifier = Modifier.size(12.dp))
        Text(label, color = Coral, fontSize = 11.sp, fontWeight = FontWeight.SemiBold)
    }
}

/** A file a step names: tap opens it on the host; the menu also reveals its folder. */
@Composable
private fun StepFileChip(label: String, path: String, onPathAction: (String, Boolean) -> Unit) {
    var menu by remember { mutableStateOf(false) }
    // Copy path puts the absolute path on the clipboard and flashes "Copied ✓" on the label.
    val flash = rememberCopyFlash()
    Row(
        Modifier
            .clip(RoundedCornerShape(6.dp))
            .background(Coral.copy(alpha = 0.09f))
            .border(1.dp, Coral.copy(alpha = 0.22f), RoundedCornerShape(6.dp))
            .padding(start = 7.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Row(
            Modifier
                .defaultMinSize(minHeight = ACTIVITY_HEADER_CHIP_HEIGHT_DP.dp)
                .clickable(role = Role.Button, onClickLabel = "Open file") { onPathAction(path, false) },
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Icon(Icons.Outlined.Description, contentDescription = null, tint = Coral, modifier = Modifier.size(12.dp))
            CopyFlashLabel(
                display = label,
                copied = flash.copied,
                style = androidx.compose.ui.text.TextStyle(color = Coral, fontSize = 11.sp, fontWeight = FontWeight.SemiBold),
            )
        }
        Box {
            Box(
                Modifier
                    .size(width = 28.dp, height = ACTIVITY_HEADER_CHIP_HEIGHT_DP.dp)
                    .clickable(role = Role.Button, onClickLabel = "File actions for $label") { menu = true },
                contentAlignment = Alignment.Center,
            ) {
                Icon(Icons.Outlined.MoreHoriz, contentDescription = "File actions for $label", tint = Secondary, modifier = Modifier.size(14.dp))
            }
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                DropdownMenuItem(
                    text = { Text("Open file") },
                    leadingIcon = { Icon(Icons.AutoMirrored.Outlined.OpenInNew, contentDescription = null) },
                    onClick = { menu = false; onPathAction(path, false) },
                )
                DropdownMenuItem(
                    text = { Text("Reveal folder") },
                    leadingIcon = { Icon(Icons.Outlined.Folder, contentDescription = null) },
                    onClick = { menu = false; onPathAction(path, true) },
                )
                DropdownMenuItem(
                    text = { Text("Copy path") },
                    leadingIcon = { Icon(Icons.Outlined.ContentCopy, contentDescription = null) },
                    onClick = { menu = false; flash.copy(CopyTarget.path(path, path)) },
                )
            }
        }
    }
}

internal fun shouldShowActivitySection(rowCount: Int, isLive: Boolean): Boolean =
    rowCount > 0 || isLive

internal fun activitySectionToggleLabel(expanded: Boolean): String =
    if (expanded) "Hide steps" else "Show steps"

/** The inline Steps list keeps the newest five (web, iOS); "Show all N steps" opens the rest. */
internal const val ACTIVITY_PREVIEW_LIMIT = 5
/** The header's visible height, the same collapsed or expanded; its buttons are drawn 24 dp tall. */
internal const val ACTIVITY_HEADER_HEIGHT_DP = 28
internal const val ACTIVITY_HEADER_CHIP_HEIGHT_DP = 24
/** The header button that opens the turn's task run (was "Run"). */
internal const val ACTIVITY_INSPECT_LABEL = "Inspect"
internal const val ACTIVITY_INSPECT_DESCRIPTION = "Inspect run"
internal const val ACTIVITY_ROW_PADDING_DP = 4
internal const val ACTIVITY_SECTION_TOP_GAP_DP = 4

internal fun activityShowAllLabel(count: Int): String = "Show all $count steps"

/** The header's bare count, read aloud with its noun since the "Steps" label is gone. */
internal fun activityStepCountDescription(count: Int): String =
    if (count == 1) "1 step" else "$count steps"

/**
 * The five stable step tints (web `--tint-1…5`, lively order: rose, violet,
 * mint, butter, peach), mid-tone so a small dot reads on any card.
 */
internal fun stepTintColor(slot: Int, dark: Boolean): Color {
    val light = listOf(0xFFD9566B, 0xFF7A5CE0, 0xFF2F9E72, 0xFFC99412, 0xFFE0763A)
    val night = listOf(0xFFFF9AA6, 0xFFB8A6FF, 0xFF7FD9B4, 0xFFFFD36B, 0xFFFFB07F)
    return Color((if (dark) night else light)[slot.coerceIn(1, 5) - 1])
}

/** A settled step's dot: red when failed, amber when waiting, else its tool's tint. */
internal fun stepDotColor(row: ActivityRow): Color = when (row.status?.lowercase()) {
    "failed", "error" -> Danger
    "waiting" -> MWarn
    else -> stepTintColor(activityTintSlot(activityTintKey(row)), activePalette.isDark)
}

internal fun stepDotsDescription(shown: Int, hidden: Int): String =
    if (hidden > 0) "Latest $shown steps, $hidden earlier" else "$shown steps settled"

/** What the Steps header draws to say a turn is still running. */
internal data class ActivityHeaderLiveIndicators(
    /** A separate spinner beside the dots: never, it crowded the one-line header. */
    val spinner: Boolean,
    /** A thin indeterminate ring around the chevron. */
    val chevronRing: Boolean,
    /** The latest settled dot breathes (when the device allows animation). */
    val breathingDot: Boolean,
    /** "Waiting on you" for an open ask. */
    val waitingLabel: Boolean,
    /** The total step count: only once the turn settles; live, "+N" and the dots say it. */
    val stepCount: Boolean,
)

internal fun activityHeaderLiveIndicators(isLive: Boolean, awaitsUser: Boolean): ActivityHeaderLiveIndicators =
    ActivityHeaderLiveIndicators(
        spinner = false,
        chevronRing = isLive,
        breathingDot = isLive,
        waitingLabel = isLive && awaitsUser,
        stepCount = !isLive,
    )

/** Whether the latest dot pulses: a live turn, and animations not switched off (scale 0). */
internal fun stepDotBreathes(breatheLatest: Boolean, animatorDurationScale: Float): Boolean =
    breatheLatest && animatorDurationScale > 0f

/** One breath of the latest step dot, in and out (ease-in-out). */
internal const val STEP_DOT_BREATH_MS = 1400
/** The chevron's slot, which the live ring circles. */
internal const val ACTIVITY_CHEVRON_RING_DP = 16
/** Extra room right of the chevron, on top of the row's 6 dp spacing. */
internal const val ACTIVITY_CHEVRON_END_GAP_DP = 4
/** Above and below the header while the list is open; none when collapsed. */
internal const val ACTIVITY_HEADER_EXPANDED_PADDING_DP = 2

internal fun activityHeaderVerticalPaddingDp(expanded: Boolean): Int =
    if (expanded) ACTIVITY_HEADER_EXPANDED_PADDING_DP else 0

/**
 * The Steps header's dot strip (web TurnStage trail, iOS `StepDotStrip`):
 * "+N" for earlier settled steps, then one dot per latest settled step. While
 * the turn is live the latest dot breathes, unless animations are off.
 */
@Composable
private fun StepDotStrip(window: ai.magicbeans.magdroid.chat.ActivityStageDots, breatheLatest: Boolean) {
    if (window.shown.isEmpty()) return
    val breathes = stepDotBreathes(breatheLatest, rememberAnimatorDurationScale())
    val description = stepDotsDescription(window.shown.size, window.hidden)
    Row(
        modifier = Modifier.semantics(mergeDescendants = true) { contentDescription = description },
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(3.dp),
    ) {
        if (window.hidden > 0) {
            Text("+${window.hidden}", color = Muted, fontSize = 10.sp, lineHeight = 10.sp, fontWeight = FontWeight.Medium, maxLines = 1, softWrap = false)
        }
        window.shown.forEachIndexed { index, row ->
            if (breathes && index == window.shown.lastIndex) {
                BreathingStepDot(stepDotColor(row))
            } else {
                Box(Modifier.size(7.dp).clip(CircleShape).background(stepDotColor(row)))
            }
        }
    }
}

/** The latest dot while live: a gentle alpha and scale pulse. */
@Composable
private fun BreathingStepDot(color: Color) {
    val transition = rememberInfiniteTransition(label = "step-dot-breath")
    val phase by transition.animateFloat(
        initialValue = 0f,
        targetValue = 1f,
        animationSpec = infiniteRepeatable(tween(STEP_DOT_BREATH_MS, easing = FastOutSlowInEasing), RepeatMode.Reverse),
        label = "step-dot-breath-phase",
    )
    Box(
        Modifier
            .size(7.dp)
            .graphicsLayer {
                val scale = 0.85f + 0.3f * phase
                scaleX = scale
                scaleY = scale
                alpha = 0.55f + 0.45f * phase
            }
            .clip(CircleShape)
            .background(color),
    )
}

/** A step's one line: its title, then its detail (one line, meta size, regular weight). */
internal fun activityRowLine(label: String, detail: String?): androidx.compose.ui.text.AnnotatedString =
    androidx.compose.ui.text.buildAnnotatedString {
        append(label)
        detail?.let {
            pushStyle(androidx.compose.ui.text.SpanStyle(fontSize = Lively.STEP_META_SP.sp, fontWeight = FontWeight.Normal))
            append("  ")
            append(it.lineSequence().map { line -> line.trim() }.filter { line -> line.isNotEmpty() }.joinToString(" "))
            pop()
        }
    }

internal const val ACTIVITY_SECTION_HORIZONTAL_PADDING_DP = 10
internal const val ACTIVITY_SECTION_VERTICAL_PADDING_DP = 4
internal const val ACTIVITY_SECTION_EXPANDED_BOTTOM_PADDING_DP = 4
internal const val ACTIVITY_STEP_MARK_WIDTH_DP = 16
/** Steps sits on the assistant bubble's surface, as iOS draws it (`controlColor`). */
internal val ACTIVITY_SECTION_BACKGROUND: Color get() = Control
