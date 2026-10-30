package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.attention.AskSurfaceState
import ai.magicbeans.magdroid.attention.AskSurfaceViewModel
import ai.magicbeans.magdroid.attention.AskSurfaces
import ai.magicbeans.magdroid.attention.AttentionItem
import ai.magicbeans.magdroid.attention.askKey
import ai.magicbeans.magdroid.attention.openTaskId
import ai.magicbeans.magdroid.attention.request
import ai.magicbeans.magdroid.chat.HitlResponseValue
import ai.magicbeans.magdroid.tasks.PlanApprovalChoice
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.key
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle

/*
 * Answering an ask where it is shown — the composer ask dock and a task's deep
 * panel — with the same form the Attention sheet draws (`AskAnswerForm`).
 * State and posting live in bridge `AskSurfaceViewModel`; which asks a surface
 * shows is bridge `AskSurfaces`.
 */

internal const val ASK_SHEET_DOCK = "dock"
internal const val ASK_SHEET_TASK = "task"

/** Keep the full content reachable without letting it displace the other region. */
@Composable
internal fun AskScrollableSection(
    requestKey: String,
    maxHeight: Dp,
    tag: String,
    modifier: Modifier = Modifier,
    content: @Composable ColumnScope.() -> Unit,
) {
    key(requestKey) {
        Column(
            modifier.fillMaxWidth().heightIn(max = maxHeight)
                .verticalScroll(rememberScrollState()).testTag(tag),
            verticalArrangement = Arrangement.spacedBy(6.dp),
            content = content,
        )
    }
}

/** Keep [viewModel]'s asks live while the calling screen is RESUMED. */
@Composable
internal fun FollowAskSurface(viewModel: AskSurfaceViewModel) {
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    LaunchedEffect(viewModel, lifecycle) {
        lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) { viewModel.followLive() }
    }
}

/**
 * One feed ask's answer, inline. Choices, confirm / deny, a review decision,
 * text and (outside the dock) forms are answered here; secrets and grants hand
 * off to the secure sheet, exactly as Attention draws them.
 */
@Composable
internal fun InlineAskAnswer(
    item: AttentionItem,
    state: AskSurfaceState,
    compact: Boolean,
    onAnswer: (AttentionItem, HitlResponseValue) -> Unit,
    onPlanChoice: (AttentionItem, PlanApprovalChoice) -> Unit,
    onOpenSheet: (AttentionItem) -> Unit,
    onReviewPlan: ((String) -> Unit)?,
    modifier: Modifier = Modifier.fillMaxWidth(),
    onOpenReview: ((String) -> Unit)? = null,
    loadAlignmentDetail: suspend (String) -> ai.magicbeans.magdroid.attention.AlignmentDetailOutcome,
) {
    val request = item.request(item.metadata)
    val busy = state.submitting != null
    val error = state.errors[item.askKey]
    if (AskSurfaces.answersInSheet(request, compact)) {
        val context = LocalContext.current
        Column(modifier, verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                StackChip("${AskSurfaces.sheetLabel(request)} →", onClick = { onOpenSheet(item) }, enabled = !busy, prominent = true)
                request.reviewHref?.let { href ->
                    StackChip(
                        request.reviewLabel ?: "View source",
                        onClick = { onOpenReview?.invoke(href) ?: openAskReview(context, href) },
                    )
                }
            }
            error?.let { Text(it, color = Danger, fontSize = 12.sp) }
        }
        return
    }
    AskAnswerForm(
        request = request,
        submitting = busy,
        error = error,
        onSubmit = { value -> onAnswer(item, value) },
        onReviewPlan = item.openTaskId?.let { taskId -> onReviewPlan?.let { review -> { review(taskId) } } },
        onPlanChoice = { choice -> onPlanChoice(item, choice) },
        onOpenReview = onOpenReview,
        loadAlignmentDetail = loadAlignmentDetail,
        showPrompt = false,
        modifier = modifier,
    )
}

/**
 * The secure sheet for an ask an inline surface handed off, drawn only by the
 * surface that opened it ([owner]). Closing a secret ask without answering is
 * an answer, as in Attention: the pending operation retires so a fresh
 * challenge can be raised instead of waiting out its window.
 */
@Composable
internal fun AskSurfaceSheet(
    viewModel: AskSurfaceViewModel,
    state: AskSurfaceState,
    owner: String,
    onOpenTask: (String) -> Unit,
    onReviewPlan: ((String) -> Unit)?,
    onOpenReview: ((String) -> Unit)? = null,
) {
    val item = state.sheet?.takeIf { state.sheetOwner == owner } ?: return
    val request = item.request(item.metadata)
    AnswerSheet(
        request = request,
        submitting = state.submitting != null,
        error = state.errors[item.askKey],
        onDismiss = {
            if (request.isSensitive && state.submitting == null) {
                viewModel.answer(item, HitlResponseValue.Aborted("dismissed"))
            } else {
                viewModel.closeSheet()
            }
        },
        onSubmit = { value -> viewModel.answer(item, value) },
        loadAlignmentDetail = viewModel::alignmentDetail,
        onOpenTask = { item.openTaskId?.let(onOpenTask) },
        onReviewPlan = item.openTaskId?.let { taskId -> onReviewPlan?.let { review -> { review(taskId) } } },
        onPlanChoice = { choice -> viewModel.answerPlan(item, choice) },
        onOpenReview = onOpenReview,
    )
}

/**
 * A task's open asks at the top of its deep panel, each answerable in place.
 * Absent when nothing is waiting, so a settled task reads as before.
 */
@Composable
internal fun TaskAsksCard(
    asks: List<AttentionItem>,
    state: AskSurfaceState,
    viewModel: AskSurfaceViewModel,
    onReviewPlan: ((String) -> Unit)?,
    onOpenReview: (String) -> Unit,
) {
    if (asks.isEmpty() && state.notice == null) return
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        state.notice?.let { notice ->
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(notice, color = Muted, fontSize = 12.sp, modifier = Modifier.weight(1f))
                TextButton(onClick = viewModel::clearNotice, contentPadding = PaddingValues(horizontal = 8.dp)) {
                    Text("OK", color = Coral, fontSize = 13.sp)
                }
            }
        }
        asks.forEachIndexed { index, item ->
            val request = item.request(item.metadata)
            androidx.compose.material3.Surface(
                color = activePalette.card,
                shape = Lively.composerShape,
                border = androidx.compose.foundation.BorderStroke(1.dp, MWarn.copy(alpha = 0.45f)),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Column(Modifier.heightIn(max = 480.dp).padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        Column(Modifier.weight(1f)) {
                            Text(
                                if (asks.size > 1) "Waiting on you · ${index + 1} of ${asks.size}" else "Waiting on you",
                                color = MWarn, fontSize = 11.sp, fontWeight = FontWeight.SemiBold,
                            )
                            request.fromLabel?.let { Text(it, color = Secondary, fontSize = 11.sp, maxLines = 1) }
                        }
                        StackChip("Review", onClick = { viewModel.openSheet(item, ASK_SHEET_TASK) })
                    }
                    AskScrollableSection(item.askKey, 160.dp, "task-ask-prompt") {
                        Text(request.headline, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                    }
                    InlineAskAnswer(
                        item = item,
                        state = state,
                        compact = false,
                        onAnswer = viewModel::answer,
                        loadAlignmentDetail = viewModel::alignmentDetail,
                        onPlanChoice = viewModel::answerPlan,
                        onOpenSheet = { viewModel.openSheet(it, ASK_SHEET_TASK) },
                        onReviewPlan = onReviewPlan,
                        onOpenReview = onOpenReview,
                        modifier = if (request.isGodMode) Modifier.fillMaxWidth().weight(1f, fill = false)
                            else Modifier.fillMaxWidth().weight(1f, fill = false).heightIn(max = 280.dp)
                                .verticalScroll(rememberScrollState()),
                    )
                }
            }
        }
    }
}
