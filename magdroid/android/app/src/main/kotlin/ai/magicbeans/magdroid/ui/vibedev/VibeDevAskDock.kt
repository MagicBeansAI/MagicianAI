package ai.magicbeans.magdroid.ui.vibedev

import ai.magicbeans.magdroid.attention.AskSurfaceViewModel
import ai.magicbeans.magdroid.attention.AskSurfaces
import ai.magicbeans.magdroid.ui.AskSurfaceSheet
import ai.magicbeans.magdroid.ui.ComposerAskDockView
import ai.magicbeans.magdroid.ui.FollowAskSurface
import ai.magicbeans.magdroid.ui.TaskDeepLinks
import ai.magicbeans.magdroid.ui.TaskDeepLinkTarget
import ai.magicbeans.magdroid.tasks.TaskDetailAct
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel

/** The chat approval card, every open ask with this run's turns first, above its controls. */
@Composable
internal fun VibeDevAskDock(taskIds: Set<String>, chatSessionId: String? = null) {
    val asks: AskSurfaceViewModel = viewModel()
    val state by asks.state.collectAsStateWithLifecycle()
    FollowAskSurface(asks)
    val pending = AskSurfaces.forVibeDev(state.feed, taskIds, chatSessionId, state.settled)
    val owner = "vibedev-approval"
    val reviewPlan: (String) -> Unit = { TaskDeepLinks.request(TaskDeepLinkTarget.Task(it, TaskDetailAct.Plan)) }
    AskSurfaceSheet(viewModel = asks, state = state, owner = owner,
        onOpenTask = { TaskDeepLinks.request(TaskDeepLinkTarget.Task(it)) }, onReviewPlan = reviewPlan)
    ComposerAskDockView(
        messages = emptyList(), enabled = true,
        onAnswer = { _, _, _, _ -> }, onOpenAttention = {},
        feedAsks = pending.items, feedHere = pending.here, askState = state,
        onAnswerFeed = asks::answer, onPlanChoice = asks::answerPlan,
        onOpenSheet = { asks.openSheet(it, owner) },
        onReviewPlan = reviewPlan,
        loadAlignmentDetail = asks::alignmentDetail,
    )
}
