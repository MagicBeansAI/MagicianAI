package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tasks.TaskDetailAct
import ai.magicbeans.magdroid.tasks.TasksViewModel
import androidx.activity.compose.BackHandler
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel

/*
 * Pushing the task detail screen over whatever is showing (design §3
 * "Navigation": a full screen pushed from the chat card, Attention,
 * notifications and the Tasks list; back returns to where you were). Android
 * counterpart of iOS `TaskDetailRoute` + `@Environment(\.openTaskDetail)`.
 *
 * The shell owns one [TaskDetailRequest] and draws [TaskDetailOverlay] for it
 * above the current tab; descendants push through [LocalOpenTaskDetail].
 */

/** One push of the task detail screen. */
data class TaskDetailRequest(
    val taskId: String,
    /** The run to inspect (a chat turn's run); null: the task's active, else latest, run. */
    val executionId: String? = null,
    /** Open the Plan act instead of the state's default act. */
    val focusPlan: Boolean = false,
    /**
     * From an OS / in-app link (`magican://task/{id}`): the id may name a
     * monitor, which is probed first and opens in the Tasks tab instead.
     */
    val fromLink: Boolean = false,
) {
    val focusAct: TaskDetailAct? get() = if (focusPlan) TaskDetailAct.Plan else null
}

/**
 * Opens the task detail screen over the current screen; back returns there.
 * `openTaskDetail(taskId, executionId, focusPlan)`.
 */
fun interface OpenTaskDetail {
    fun open(taskId: String, executionId: String?, focusPlan: Boolean)
}

/** Opens the task detail over the current screen, or null outside the app shell. */
val LocalOpenTaskDetail = staticCompositionLocalOf<OpenTaskDetail?> { null }

/** `openTaskDetail(taskId)` with the defaults, for call sites that only have an id. */
fun OpenTaskDetail.open(taskId: String) = open(taskId, null, false)

/**
 * The pushed detail screen for [request]: the activity's shared
 * [TasksViewModel] loads the task, system back and the bar's back close it.
 */
@Composable
internal fun TaskDetailOverlay(
    request: TaskDetailRequest,
    onClose: () -> Unit,
    /** A link that turned out to name a monitor: hand it to the Tasks tab. */
    onMonitor: (String) -> Unit,
    onOpenPlanInspector: ((String) -> Unit)? = null,
) {
    val viewModel: TasksViewModel = viewModel(key = "work-detail-${ai.magicbeans.magdroid.access.MagicianAccess.connectionRevision}-${request.taskId}")
    val state by viewModel.state.collectAsStateWithLifecycle()
    LaunchedEffect(request) {
        if (request.fromLink) viewModel.openTaskLink(request.taskId, request.focusAct, request.executionId)
        else viewModel.openTaskById(request.taskId, request.executionId, request.focusAct)
    }
    LaunchedEffect(state.monitorDetail.taskId) {
        if (request.fromLink && state.monitorDetail.taskId == request.taskId) onMonitor(request.taskId)
    }
    val close = {
        viewModel.closeTaskDetail()
        onClose()
    }
    BackHandler(onBack = close)
    TaskDetailScreen(state, viewModel, onBack = close, onOpenPlanInspector = onOpenPlanInspector)
    state.actionError?.let { error ->
        AlertDialog(
            onDismissRequest = viewModel::clearError,
            confirmButton = { TextButton(onClick = viewModel::clearError) { Text("OK", color = Coral) } },
            title = { Text(error.title, color = Ink) },
            text = { Text(error.message, color = Muted) },
            containerColor = Ground,
        )
    }
}
