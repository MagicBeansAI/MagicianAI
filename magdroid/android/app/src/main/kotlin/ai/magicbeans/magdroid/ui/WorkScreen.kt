package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.tasks.TasksViewModel
import ai.magicbeans.magdroid.work.*
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.MoreHoriz
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.saveable.rememberSaveableStateHolder
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.viewmodel.compose.viewModel

/** Navigation over the mature task/monitor and canonical Attention screens. */
@Composable
internal fun WorkScreen(
    actions: TasksScreenActions,
    onOpenTask: (String) -> Unit,
    onReviewPlan: (String) -> Unit,
    onOpenThread: (String) -> Unit,
    onOpenSettings: () -> Unit,
) {
    var sectionName by rememberSaveable { mutableStateOf(WorkSection.All.name) }
    val section = WorkSection.valueOf(sectionName)
    var more by remember { mutableStateOf(false) }
    var connectionEpoch by remember { mutableIntStateOf(0) }
    var libraryRoute by remember { mutableStateOf<WorkRoute?>(null) }
    var libraryEpoch by remember { mutableIntStateOf(0) }
    val holder = rememberSaveableStateHolder()
    val tasks: TasksViewModel = viewModel(key = "work-tasks-${MagicianAccess.connectionRevision}")
    LaunchedEffect(actions.isCreateVisible) {
        if (actions.isCreateVisible) {
            libraryRoute = null
            if (section != WorkSection.All) tasks.selectLane(ai.magicbeans.magdroid.tasks.TaskLane.Tasks)
            sectionName = WorkSection.All.name
        }
    }
    LaunchedEffect(Unit) {
        MagicianAccess.activeMachine.collect {
            holder.removeState("tasks-$connectionEpoch"); connectionEpoch++; sectionName = WorkSection.All.name; more = false; libraryRoute = null; libraryEpoch++; WorkDeepLinks.clear()
        }
    }
    val taskState by tasks.state.collectAsState()
    val workLink by WorkDeepLinks.target.collectAsState()
    LaunchedEffect(workLink) {
        when (val target = workLink) {
            is WorkLinkTarget.Library -> { sectionName = target.section.name; libraryRoute = target.route; if (target.section == WorkSection.All) tasks.setThreadContext(null); libraryEpoch++; WorkDeepLinks.consume(target) }
            is WorkLinkTarget.Thread -> { actions.hideCreate(); sectionName = WorkSection.All.name; libraryRoute = null; tasks.setThreadContext(target.id); WorkDeepLinks.consume(target) }
            null -> Unit
        }
    }
    val deepLink by TaskDeepLinks.target.collectAsState()
    LaunchedEffect(deepLink) { if (deepLink != null) { sectionName = WorkSection.All.name; (deepLink as? TaskDeepLinkTarget.Task)?.let { tasks.setThreadContext(it.threadId) } } }
    BackHandler(section != WorkSection.All) { sectionName = WorkSection.All.name }
    var canCreate = true
    DisposableEffect(actions) { onDispose { actions.canCreate = true } }
    Column(Modifier.fillMaxSize().background(Ground)) {
        if (!actions.isDetailVisible) {
            Row(Modifier.fillMaxWidth().padding(horizontal = 6.dp), horizontalArrangement = Arrangement.spacedBy(2.dp)) {
                listOf(WorkSection.All, WorkSection.NeedsYou, WorkSection.Crew).forEach { item ->
                    TextButton(
                        onClick = { libraryRoute = null; if (item == WorkSection.All) tasks.setThreadContext(null); sectionName = item.name },
                        colors = ButtonDefaults.textButtonColors(contentColor = if (section == item) Coral else Muted),
                        modifier = Modifier.weight(1f).heightIn(min = 48.dp).testTag("work-view-${item.name}"),
                        contentPadding = PaddingValues(horizontal = 2.dp),
                    ) { Text(item.title, maxLines = 2, textAlign = TextAlign.Center) }
                }
                Box {
                    IconButton(onClick = { more = true }, modifier = Modifier.size(48.dp).testTag("work-more")) {
                        Icon(Icons.Outlined.MoreHoriz, contentDescription = "More work views", tint = Coral)
                    }
                    DropdownMenu(expanded = more, onDismissRequest = { more = false }) {
                        listOf(WorkSection.Programs, WorkSection.Quality, WorkSection.Operations).forEach { item ->
                            DropdownMenuItem(text = { Text(item.title) }, onClick = { libraryRoute = null; sectionName = item.name; more = false })
                        }
                    }
                }
            }
        }
        if (section == WorkSection.All && taskState.threadContext != null) {
            val threadId = taskState.threadContext!!
            val thread = rememberWorkResource("/api/magician/v2/ui-threads/${ai.magicbeans.magdroid.work.WorkRepository.segment(threadId)}")
            val threadState by thread.state.collectAsState()
            val resolvedThread = threadState.value?.let { value ->
                (value.workField("thread").workText("id") ?: value.workText("id"))?.lowercase() == threadId.lowercase()
            } == true
            canCreate = resolvedThread
            LaunchedEffect(actions.isCreateVisible, resolvedThread) { if (!resolvedThread && actions.isCreateVisible) actions.hideCreate() }
            Column(Modifier.fillMaxWidth().padding(horizontal = 12.dp)) {
                Text("Thread: $threadId", color = Ink)
                WorkResourceStatus(threadState, thread)
                if (!resolvedThread) Text("Resolve this thread before creating work here.", color = Muted)
                Row { TextButton(onClick = { onOpenThread(threadId) }) { Text("Chat", color = Coral) }; TextButton(onClick = { tasks.setThreadContext(null) }) { Text("All threads", color = Coral) } }
            }
        }
        when (section) {
            WorkSection.All -> holder.SaveableStateProvider("tasks-$connectionEpoch") { TasksScreen(viewModel = tasks, actions = actions) }
            WorkSection.NeedsYou -> AttentionScreen(onOpenTask, onOpenThread, onOpenSettings, onReviewPlan)
            else -> key(connectionEpoch, section, libraryEpoch) { WorkLibraryScreen(section, actions, onOpenTask, initialRoute = libraryRoute) }
        }
    }
    SideEffect { actions.canCreate = canCreate }
}
