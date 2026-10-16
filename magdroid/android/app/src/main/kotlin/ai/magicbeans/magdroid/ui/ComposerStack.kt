package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.attention.AskSurfaceState
import ai.magicbeans.magdroid.attention.AttentionItem
import ai.magicbeans.magdroid.attention.askIds
import ai.magicbeans.magdroid.attention.request
import ai.magicbeans.magdroid.chat.ChatMessage
import ai.magicbeans.magdroid.chat.ComposerAsk
import ai.magicbeans.magdroid.chat.HitlResponseValue
import ai.magicbeans.magdroid.tasks.PlanApprovalChoice
import ai.magicbeans.magdroid.chat.ChatPresence
import ai.magicbeans.magdroid.chat.ChatReadOnly
import ai.magicbeans.magdroid.chat.ComposerAskDock
import ai.magicbeans.magdroid.chat.ComposerAskTarget
import ai.magicbeans.magdroid.chat.ComposerRows
import ai.magicbeans.magdroid.chat.EscalationOption
import ai.magicbeans.magdroid.chat.LiveTaskCardClient
import ai.magicbeans.magdroid.chat.LiveTaskPlan
import ai.magicbeans.magdroid.chat.PlannerDock
import ai.magicbeans.magdroid.chat.PlannerDockTask
import ai.magicbeans.magdroid.chat.TaskStoreRecord
import ai.magicbeans.magdroid.tasks.PlanApprovalActions
import ai.magicbeans.magdroid.tasks.PlanApprovalOutcome
import ai.magicbeans.magdroid.tasks.PlanApprovalPost
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.ArrowForward
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.GraphicEq
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/*
 * The rows stacked on and above the chat composer, web/iOS parity: the ask
 * dock ("Waiting on you"), the planner dock ("Planner needs you"), and the
 * in-card bands (queue error, auto-send, plan mode) plus the archived band
 * that replaces the composer. Rules live in bridge `chat/ComposerStack.kt`.
 * Theme tokens only, 0.5 dp hairlines, rounded squares, 48 dp targets.
 */

/** The docks share the composer card's width and left edge. */
internal val COMPOSER_DOCK_INSET = 10.dp

internal val COMPOSER_HAIRLINE = 0.5.dp

/** An outlined rounded-square chip (Lively.chipShape), the composer stack's one button. */
@Composable
internal fun StackChip(
    label: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    prominent: Boolean = false,
    destructive: Boolean = false,
    icon: ImageVector? = null,
    description: String? = null,
) {
    val tint = when {
        prominent -> OnAccent
        destructive -> Danger
        else -> Coral
    }
    Box(
        modifier
            .defaultMinSize(minWidth = Lively.tapMin, minHeight = Lively.tapMin)
            .clickable(enabled = enabled, role = Role.Button, onClick = onClick)
            .then(if (description != null) Modifier.semantics { contentDescription = description } else Modifier),
        contentAlignment = Alignment.Center,
    ) {
        Row(
            Modifier
                .graphicsLayer { alpha = if (enabled) 1f else 0.45f }
                .clip(Lively.chipShape)
                .background(if (prominent) AccentFill else Color.Transparent)
                .border(1.dp, if (prominent) AccentFill else if (destructive) Danger.copy(alpha = 0.4f) else ControlBorder, Lively.chipShape)
                .padding(horizontal = 9.dp, vertical = 5.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            icon?.let { Icon(it, contentDescription = null, tint = tint, modifier = Modifier.size(13.dp)) }
            Text(label, color = tint, fontSize = 12.sp, fontWeight = FontWeight.SemiBold, maxLines = 1)
        }
    }
}

/** A 0.5 dp rule between in-card rows. */
@Composable
internal fun ComposerHairline() {
    Box(Modifier.fillMaxWidth().height(COMPOSER_HAIRLINE).background(Muted.copy(alpha = 0.2f)))
}

// ── In-card bands ──

/** Why the last queue action failed: a danger band in the card, dismissable. */
@Composable
internal fun QueueErrorBand(message: String, onDismiss: () -> Unit) {
    Column(Modifier.fillMaxWidth()) {
        Row(
            Modifier.fillMaxWidth().background(Danger.copy(alpha = 0.08f)).padding(start = 14.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                message,
                color = Danger,
                fontSize = 12.sp,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f).padding(vertical = 6.dp),
            )
            Box(
                Modifier.size(Lively.tapMin).clickable(role = Role.Button, onClickLabel = "Dismiss", onClick = onDismiss),
                contentAlignment = Alignment.Center,
            ) {
                Icon(Icons.Outlined.Close, contentDescription = "Dismiss queue error", tint = Danger, modifier = Modifier.size(14.dp))
            }
        }
        ComposerHairline()
    }
}

/**
 * The dictated draft sends itself: a flush ~30 dp band (accent ~12 % fill)
 * with a draining bar and a Cancel chip.
 */
@Composable
internal fun AutoSendBand(seconds: Int?, onCancel: () -> Unit) {
    AnimatedVisibility(
        visible = seconds != null,
        enter = expandVertically() + fadeIn(),
        exit = shrinkVertically() + fadeOut(),
    ) {
        val remaining = seconds ?: 0
        // Each tick drains toward the next second, linearly, so the bar moves continuously.
        val fraction by animateFloatAsState(
            targetValue = ComposerRows.autoSendFraction(remaining - 1),
            animationSpec = tween(1_000, easing = LinearEasing),
            label = "auto-send-drain",
        )
        Column(Modifier.fillMaxWidth().background(Coral.copy(alpha = 0.12f))) {
            Row(
                Modifier.fillMaxWidth().height(30.dp).padding(start = 14.dp, end = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Icon(Icons.Outlined.GraphicEq, contentDescription = null, tint = Coral, modifier = Modifier.size(14.dp))
                Text("Sending in ${remaining}s…", color = Ink, fontSize = 12.sp, fontWeight = FontWeight.Medium, modifier = Modifier.weight(1f))
                StackChip("Cancel", onClick = onCancel, description = "Cancel auto-send")
            }
            Box(Modifier.fillMaxWidth().height(2.dp)) {
                Box(Modifier.fillMaxWidth(fraction).fillMaxHeight().background(Coral))
            }
        }
    }
}

/** Plan mode: an accent-soft band with a small pulsing dot (web `plan-banner`). */
@Composable
internal fun PlanModeBand() {
    val moving = rememberAnimatorDurationScale() > 0f
    val transition = rememberInfiniteTransition(label = "plan-mode-dot")
    val pulse by transition.animateFloat(1f, 0.35f, infiniteRepeatable(tween(1_100), RepeatMode.Reverse), label = "plan-mode-dot-alpha")
    Column(Modifier.fillMaxWidth()) {
        Row(
            Modifier.fillMaxWidth().background(Coral.copy(alpha = 0.08f)).padding(horizontal = 14.dp, vertical = 7.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Box(Modifier.size(6.dp).graphicsLayer { alpha = if (moving) pulse else 1f }.background(Coral, CircleShape))
            Text(
                androidx.compose.ui.text.buildAnnotatedString {
                    pushStyle(androidx.compose.ui.text.SpanStyle(fontWeight = FontWeight.SemiBold)); append("Plan mode"); pop()
                    append(" — I'll draft a plan first. Switch to Do for direct execution.")
                },
                color = Ink,
                fontSize = 12.sp,
                lineHeight = 16.sp,
            )
        }
        ComposerHairline()
    }
}

/** In place of the composer for an archived conversation. */
@Composable
internal fun ArchivedComposerBand(onReturn: () -> Unit) {
    Surface(
        color = composerSurfaceColor(),
        shape = Lively.composerShape,
        border = BorderStroke(1.dp, Muted.copy(alpha = 0.25f)),
        modifier = Modifier.fillMaxWidth().padding(start = COMPOSER_DOCK_INSET, end = COMPOSER_DOCK_INSET, top = 2.dp, bottom = 6.dp),
    ) {
        Row(
            Modifier.fillMaxWidth().padding(start = 14.dp, end = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(ChatReadOnly.BAND_TEXT, color = Secondary, fontSize = 13.sp, modifier = Modifier.weight(1f))
            StackChip("Return to active", onClick = onReturn)
        }
    }
}

/**
 * Presence beside the chat session title (web `PresenceOrb`, iOS
 * `ChatPresenceDot`): idle in the success colour, thinking in the accent
 * (fading), working in status-running (breathing), needs-you in the warning
 * colour with a halo. Decorative: the title row carries the spoken label.
 */
@Composable
fun ChatPresenceDot(presence: ChatPresence, modifier: Modifier = Modifier, size: Dp = 7.dp) {
    when (presence) {
        ChatPresence.Working -> StatusDot(StatusTone.Running, modifier, size)
        ChatPresence.Thinking -> {
            val moving = rememberAnimatorDurationScale() > 0f
            val transition = rememberInfiniteTransition(label = "presence-thinking")
            val phase by transition.animateFloat(
                initialValue = 0f,
                targetValue = 1f,
                animationSpec = infiniteRepeatable(tween(1400), RepeatMode.Reverse),
                label = "presence-thinking-phase",
            )
            Box(
                modifier
                    .size(size)
                    .graphicsLayer { alpha = if (moving) 1f - 0.55f * phase else 1f }
                    .background(Coral, CircleShape),
            )
        }
        ChatPresence.NeedsYou -> {
            val warning = statusToneColor(StatusTone.Waiting)
            Box(
                modifier
                    .size(size)
                    .drawBehind {
                        drawCircle(warning.copy(alpha = 0.3f), radius = this.size.minDimension / 2f + 2.dp.toPx())
                    }
                    .background(warning, CircleShape),
            )
        }
        ChatPresence.Idle -> Box(modifier.size(size).background(statusToneColor(StatusTone.Done), CircleShape))
    }
}

// ── Ask dock ──

/**
 * The questions this conversation is waiting on, above the composer: a
 * "Waiting on you" badge, "1 of N" with previous / next, the prompt and its
 * answer in place. Two sources: the transcript's escalation cards, and the
 * Attention feed's asks for this conversation ([feedAsks], already scoped by
 * thread or task — Relentless and quality-controller asks arrive only this
 * way). A feed ask is answered with the same form Attention uses; secrets and
 * grants open the secure sheet. An answer that lands reads "Answered"
 * briefly, then the dock moves on or leaves.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun ComposerAskDockView(
    messages: List<ChatMessage>,
    enabled: Boolean,
    onAnswer: (messageId: String, option: EscalationOption?, text: String, selectedIds: List<String>) -> Unit,
    onOpenAttention: (String?) -> Unit,
    feedAsks: List<AttentionItem> = emptyList(),
    askState: AskSurfaceState = AskSurfaceState(),
    onAnswerFeed: (AttentionItem, HitlResponseValue) -> Unit = { _, _ -> },
    onPlanChoice: (AttentionItem, PlanApprovalChoice) -> Unit = { _, _ -> },
    onOpenSheet: (AttentionItem) -> Unit = {},
    onReviewPlan: ((String) -> Unit)? = null,
    loadAlignmentDetail: suspend (String) -> ai.magicbeans.magdroid.attention.AlignmentDetailOutcome,
) {
    val asks = remember(messages, feedAsks, askState.settled) {
        ComposerAskDock.merged(messages, feedAsks, askState.settled)
    }
    val keys = asks.map { it.key }
    var activeId by remember { mutableStateOf<String?>(null) }
    activeId = ComposerAskDock.activeKey(keys, activeId)
    val active = asks.firstOrNull { it.key == activeId }
    // "Answered" lingers briefly for the ask that just left.
    var answered by remember { mutableStateOf<ComposerAsk?>(null) }
    var lastActive by remember { mutableStateOf<ComposerAsk?>(null) }
    LaunchedEffect(active?.key) {
        val left = lastActive
        val leftAnswered = when (left) {
            is ComposerAsk.Transcript ->
                messages.firstOrNull { it.id == left.target.messageId }?.escalation?.resolved == true ||
                    left.target.card.correlationId?.let(askState.settled::contains) == true
            is ComposerAsk.Feed -> left.item.askIds.any(askState.settled::contains)
            null -> false
        }
        if (left != null && left.key != active?.key && leftAnswered) {
            answered = left
            delay(1_200)
            answered = null
        }
        lastActive = active
    }
    val shown = active ?: answered
    AnimatedVisibility(
        visible = shown != null,
        enter = slideInVertically { it / 2 } + fadeIn(),
        exit = slideOutVertically { it / 2 } + fadeOut(),
    ) {
        val target = shown ?: return@AnimatedVisibility
        val isAnswered = active == null
        val feedRequest = (target as? ComposerAsk.Feed)?.item?.let { it.request(it.metadata) }
        val question = when (target) {
            is ComposerAsk.Transcript -> target.target.card.question
            is ComposerAsk.Feed -> feedRequest?.prompt.orEmpty()
        }
        Surface(
            color = activePalette.card,
            shape = Lively.composerShape,
            border = BorderStroke(1.dp, MWarn.copy(alpha = 0.45f)),
            modifier = Modifier.fillMaxWidth().padding(start = COMPOSER_DOCK_INSET, end = COMPOSER_DOCK_INSET, bottom = 6.dp),
        ) {
            Column(Modifier.padding(horizontal = 12.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(
                        if (isAnswered) "Answered" else "Waiting on you",
                        color = Ink,
                        fontSize = 11.sp,
                        fontWeight = FontWeight.SemiBold,
                        modifier = Modifier
                            .clip(Lively.chipShape)
                            .background((if (isAnswered) ChatSuccess else MWarn).copy(alpha = 0.22f))
                            .padding(horizontal = 7.dp, vertical = 2.dp),
                    )
                    feedRequest?.fromLabel?.let { Text(it, color = Secondary, fontSize = 11.sp) }
                    Spacer(Modifier.weight(1f))
                    ComposerAskDock.positionOf(keys, activeId)?.takeIf { !isAnswered }?.let { position ->
                        IconChip(Icons.AutoMirrored.Outlined.ArrowBack, "Previous question") {
                            activeId = ComposerAskDock.stepKey(keys, activeId, -1)
                        }
                        Text(position, color = Secondary, fontSize = 11.sp)
                        IconChip(Icons.AutoMirrored.Outlined.ArrowForward, "Next question") {
                            activeId = ComposerAskDock.stepKey(keys, activeId, 1)
                        }
                    }
                }
                Text(question, color = Ink, fontSize = 14.sp, lineHeight = 19.sp, maxLines = 6, overflow = TextOverflow.Ellipsis)
                if (!isAnswered) when (target) {
                    is ComposerAsk.Transcript -> {
                        AskDockAnswer(target.target, enabled, onAnswer, onOpenAttention)
                        target.target.card.error?.let { Text(it, color = Danger, fontSize = 12.sp) }
                    }
                    is ComposerAsk.Feed -> InlineAskAnswer(
                        item = target.item,
                        state = askState,
                        compact = true,
                        onAnswer = onAnswerFeed,
                        loadAlignmentDetail = loadAlignmentDetail,
                        onPlanChoice = onPlanChoice,
                        onOpenSheet = onOpenSheet,
                        onReviewPlan = onReviewPlan,
                        modifier = Modifier.fillMaxWidth().heightIn(max = 320.dp).verticalScroll(rememberScrollState()),
                    )
                }
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun AskDockAnswer(
    target: ComposerAskTarget,
    enabled: Boolean,
    onAnswer: (String, EscalationOption?, String, List<String>) -> Unit,
    onOpenAttention: (String?) -> Unit,
) {
    val card = target.card
    val busy = card.answering != null
    val locked = !enabled || busy || card.resolved
    var typed by remember(target.messageId) { mutableStateOf("") }
    val picked = remember(target.messageId) { mutableStateListOf<String>() }
    ComposerAskDock.handoff(card)?.let { label ->
        StackChip(label, onClick = { onOpenAttention(card.correlationId) }, enabled = !card.correlationId.isNullOrBlank())
        return
    }
    when (card.inputType?.trim()?.lowercase()) {
        "text", "guidance", "file_path" -> EscalationTextAnswer(
            inputType = card.inputType,
            value = typed,
            onValue = { typed = it },
            enabled = !locked,
            onSubmit = { onAnswer(target.messageId, null, typed, emptyList()) },
            placeholder = card.placeholder,
        )
        "multi_choice" -> {
            card.options.forEach { option ->
                val on = picked.contains(option.id)
                EscalationOptionRow(option, selected = on, dimmed = false, enabled = !locked) {
                    if (on) picked.remove(option.id) else picked.add(option.id)
                }
            }
            EscalationSubmit(enabled = !locked && picked.isNotEmpty()) { onAnswer(target.messageId, null, "", picked.toList()) }
        }
        else -> {
            // Choices inline: short options as chips, longer ones as rows.
            val compact = card.options.all { it.description.isNullOrBlank() && it.label.length <= 28 }
            if (compact) {
                FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    card.options.forEach { option ->
                        StackChip(
                            if (card.answering == option.id) "Sending…" else option.label,
                            onClick = { onAnswer(target.messageId, option, typed, emptyList()) },
                            enabled = !locked,
                            prominent = ComposerAskDock.isPlanApproval(card) && option.id.contains("approve", ignoreCase = true),
                        )
                    }
                }
            } else {
                card.options.forEach { option ->
                    EscalationOptionRow(option, selected = card.answering == option.id, dimmed = false, enabled = !locked) {
                        onAnswer(target.messageId, option, typed, emptyList())
                    }
                }
            }
        }
    }
}

@Composable
private fun IconChip(icon: ImageVector, description: String, onClick: () -> Unit) {
    Box(
        Modifier.size(Lively.tapMin).clickable(role = Role.Button, onClickLabel = description, onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        Box(
            Modifier.clip(Lively.chipShape).border(1.dp, ControlBorder, Lively.chipShape).padding(5.dp),
        ) { Icon(icon, contentDescription = description, tint = Secondary, modifier = Modifier.size(13.dp)) }
    }
}

// ── Planner dock ──

/**
 * "Planner needs you": the conversation's drafted plans, each re-checked with
 * the task store, collapsible, with Review plan · Approve · Approve & Run ·
 * Replan · Reject (web `PlannerDock`).
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun PlannerDockView(
    messages: List<ChatMessage>,
    enabled: Boolean,
    onReviewPlan: (String) -> Unit,
) {
    val context = LocalContext.current
    val client = remember(context) { LiveTaskCardClient.shared(context) }
    val scope = rememberCoroutineScope()
    val candidates = remember(messages) { PlannerDock.candidates(messages) }
    // Store wins: a candidate stays only while its task still reads draft.
    val storeStatus = remember { mutableStateMapOf<String, String>() }
    LaunchedEffect(candidates.map { it.taskId }) {
        candidates.forEach { task ->
            client.json("/tasks/${LiveTaskCardClient.segment(task.taskId)}")?.let(TaskStoreRecord::parse)?.let {
                storeStatus[task.taskId] = it.status
            }
        }
    }
    val tasks = candidates.filter { storeStatus[it.taskId]?.lowercase()?.let { s -> s == "draft" } ?: false }
    var expanded by remember { mutableStateOf(false) }
    var selectedId by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf<String?>(null) }
    var message by remember { mutableStateOf<String?>(null) }
    var confirmReject by remember { mutableStateOf<PlannerDockTask?>(null) }
    val selected = tasks.firstOrNull { it.taskId == selectedId } ?: tasks.firstOrNull()

    fun act(task: PlannerDockTask, action: String) {
        busy = action
        scope.launch {
            try {
                val planId = client.json("/tasks/${LiveTaskCardClient.segment(task.taskId)}/plan")?.let(LiveTaskPlan::parse)?.planId
                val outcome: String? = when (action) {
                    "approve_run" -> PlanApprovalActions.approveAndRun(
                        taskId = task.taskId,
                        approve = {
                            client.planAction(task.taskId, "approve", planId)?.let { PlanApprovalPost.Failed(it) } ?: PlanApprovalPost.Ok
                        },
                        startRun = { id -> client.execute(id)?.let { throw IllegalStateException(it) } },
                    ).let { if (it == PlanApprovalOutcome.ApprovedAndStarted) null else it.message }
                    else -> client.planAction(task.taskId, action, planId)
                }
                message = outcome ?: when (action) {
                    "approve" -> PlanApprovalOutcome.Approved.message
                    "approve_run" -> PlanApprovalOutcome.ApprovedAndStarted.message
                    "reject" -> PlanApprovalOutcome.Rejected.message
                    else -> "Replanning…"
                }
                client.json("/tasks/${LiveTaskCardClient.segment(task.taskId)}")?.let(TaskStoreRecord::parse)?.let {
                    storeStatus[task.taskId] = it.status
                }
            } finally {
                busy = null
            }
        }
    }

    AnimatedVisibility(
        visible = selected != null,
        enter = slideInVertically { it / 2 } + fadeIn(),
        exit = slideOutVertically { it / 2 } + fadeOut(),
    ) {
        val task = selected ?: return@AnimatedVisibility
        Surface(
            color = activePalette.card,
            shape = Lively.composerShape,
            border = BorderStroke(1.dp, BorderSoft),
            modifier = Modifier.fillMaxWidth().padding(start = COMPOSER_DOCK_INSET, end = COMPOSER_DOCK_INSET, bottom = 6.dp),
        ) {
            Column {
                Row(
                    Modifier
                        .fillMaxWidth()
                        .background(Soft)
                        .defaultMinSize(minHeight = 40.dp)
                        .clickable(role = Role.Button, onClickLabel = if (expanded) "Collapse planner" else "Expand planner") { expanded = !expanded }
                        .padding(horizontal = 12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    Icon(Icons.Outlined.Description, contentDescription = null, tint = Ink, modifier = Modifier.size(15.dp))
                    Text("Planner needs you", color = Ink, fontSize = 12.sp, fontWeight = FontWeight.SemiBold, maxLines = 1)
                    task.updatedAtMs?.let { at ->
                        ai.magicbeans.magdroid.chat.ChatDateDividers.epochMillis(at.toDouble())?.let { millis ->
                            Text(ComposerRows.relative(millis, System.currentTimeMillis()), color = Muted, fontSize = 11.sp, maxLines = 1)
                        }
                    }
                    Spacer(Modifier.weight(1f))
                    if (tasks.size == 1 && !expanded) {
                        Text(task.title, color = Secondary, fontSize = 11.sp, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false))
                    }
                    Text(PlannerDock.countLabel(tasks.size, tasks.size), color = Secondary, fontSize = 11.sp, maxLines = 1)
                    Icon(Icons.Outlined.ExpandMore, contentDescription = null, tint = Secondary, modifier = Modifier.size(16.dp).rotate(if (expanded) 180f else 0f))
                }
                AnimatedVisibility(expanded, enter = expandVertically() + fadeIn(), exit = shrinkVertically() + fadeOut()) {
                    Column(Modifier.padding(horizontal = 12.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        if (tasks.size > 1) {
                            FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                                tasks.forEach { option ->
                                    StackChip(option.title.take(32), onClick = { selectedId = option.taskId }, prominent = option.taskId == task.taskId)
                                }
                            }
                        } else {
                            Text(task.title, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, maxLines = 2, overflow = TextOverflow.Ellipsis)
                        }
                        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                            Text(
                                "Draft",
                                color = MWarn,
                                fontSize = 10.sp,
                                fontWeight = FontWeight.SemiBold,
                                modifier = Modifier.clip(Lively.chipShape).background(MWarn.copy(alpha = 0.14f)).padding(horizontal = 6.dp, vertical = 2.dp),
                            )
                            Text("Plan ready for review", color = Secondary, fontSize = 12.sp)
                        }
                        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                            val locked = !enabled || busy != null
                            StackChip("Review plan", onClick = { onReviewPlan(task.taskId) }, enabled = enabled)
                            StackChip(if (busy == "approve") "Approving…" else "Approve", onClick = { act(task, "approve") }, enabled = !locked)
                            StackChip(if (busy == "approve_run") "Starting…" else "Approve & Run", onClick = { act(task, "approve_run") }, enabled = !locked, prominent = true)
                            StackChip("Replan", onClick = { act(task, "replan") }, enabled = !locked)
                            StackChip("Reject", onClick = { confirmReject = task }, enabled = !locked, destructive = true)
                        }
                        message?.let { Text(it, color = Secondary, fontSize = 12.sp) }
                    }
                }
            }
        }
    }

    confirmReject?.let { task ->
        AlertDialog(
            onDismissRequest = { confirmReject = null },
            title = { Text("Reject this plan?") },
            text = { Text("It will not run. You can replan it later.") },
            confirmButton = { TextButton(onClick = { confirmReject = null; act(task, "reject") }) { Text("Reject", color = Danger) } },
            dismissButton = { TextButton(onClick = { confirmReject = null }) { Text("Keep") } },
        )
    }
}
