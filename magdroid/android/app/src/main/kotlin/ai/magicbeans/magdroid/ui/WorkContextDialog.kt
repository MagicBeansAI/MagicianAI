package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.WorkSection
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.compose.runtime.getValue
import ai.magicbeans.magdroid.tasks.TasksViewModel

/** Pushed Work context returns to the exact native task/execution underneath. */
@Composable
internal fun WorkContextDialog(route: WorkRoute, onClose: () -> Unit, onOpenTask: (String) -> Unit) {
    val actions = rememberTasksScreenActions()
    val tasks: TasksViewModel = viewModel(key = "work-context-${ai.magicbeans.magdroid.access.MagicianAccess.connectionRevision}")
    val state by tasks.state.collectAsStateWithLifecycle()
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
        Column(Modifier.fillMaxSize().background(Ground).systemBarsPadding()) {
            WorkDetailBar("", onBack = onClose, backLabel = "‹ Task")
            WorkLibraryScreen(if (route.kind == "member") WorkSection.Crew else WorkSection.Quality, actions, onOpenTask, initialRoute = route)
            if (actions.isCreateVisible) TaskCreateSheet(
                agents = state.agents, busy = state.mutatingTaskId != null,
                onDismiss = actions::hideCreate,
                onCreate = { draft -> tasks.createTask(draft) { if (it) actions.hideCreate() } },
            )
        }
    }
}
