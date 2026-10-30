package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.tasks.TasksViewModel
import ai.magicbeans.magdroid.work.*
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.foundation.border
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.automirrored.outlined.Chat
import androidx.compose.material.icons.outlined.Autorenew
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.ClearAll
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.Forum
import androidx.compose.material.icons.outlined.KeyboardArrowDown
import androidx.compose.material.icons.outlined.TrackChanges
import androidx.compose.material.icons.outlined.Verified
import androidx.compose.ui.Alignment
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.sp
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.saveable.rememberSaveableStateHolder
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
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
    onOpenChat: () -> Unit = {},
    /** What is waiting on the owner (the Attention badge), shown on the Needs you segment. */
    needsYouCount: Int = 0,
) {
    var sectionName by rememberSaveable { mutableStateOf(WorkSection.All.name) }
    val section = WorkSection.valueOf(sectionName)
    var more by remember { mutableStateOf(false) }
    var connectionEpoch by remember { mutableIntStateOf(0) }
    var libraryRoute by remember { mutableStateOf<WorkRoute?>(null) }
    var libraryEpoch by remember { mutableIntStateOf(0) }
    val holder = rememberSaveableStateHolder()
    // The Goals and Playbooks summary over All work: read once per visit to Work, not per lane or view.
    val landing = remember { WorkLandingState() }
    var landingWanted by remember { mutableStateOf(false) }
    var landingNote by remember { mutableStateOf<String?>(null) }
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
        if (!actions.isDetailVisible) WorkViewSwitcher(
            section = section,
            moreOpen = more,
            onMore = { more = it },
            onSelect = { item -> libraryRoute = null; if (item == WorkSection.All) tasks.setThreadContext(null); sectionName = item.name; more = false },
            needsYouCount = needsYouCount,
        )
        // Hidden while composing, in a thread's work and over a task, as on the web. It scrolls with the
        // task list as the first row of the page, never in a region of its own.
        val landingVisible = section == WorkSection.All && taskState.threadContext == null && !actions.isCreateVisible && !actions.isDetailVisible
        if (landingVisible) SideEffect { landingWanted = true }
        if (section == WorkSection.All && taskState.threadContext != null) {
            val threadId = taskState.threadContext!!
            val thread = rememberWorkResource("/api/magician/v2/ui-threads/${ai.magicbeans.magdroid.work.WorkRepository.segment(threadId)}")
            val threadState by thread.state.collectAsState()
            val resolvedThread = threadState.value?.let { value ->
                (value.workField("thread").workText("id") ?: value.workText("id"))?.lowercase() == threadId.lowercase()
            } == true
            canCreate = resolvedThread
            LaunchedEffect(actions.isCreateVisible, resolvedThread) { if (!resolvedThread && actions.isCreateVisible) actions.hideCreate() }
            Column(Modifier.fillMaxWidth().padding(horizontal = WORK_SCREEN_PADDING_DP.dp, vertical = 6.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                WorkGroup {
                    WorkRow("Thread: $threadId", icon = Icons.Outlined.Forum, detail = if (!resolvedThread) "Resolve this thread before creating work here." else null, accessory = WorkRowAccessory.None)
                    WorkRowDivider()
                    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                        Box(Modifier.weight(1f)) { WorkActionRow("Chat", Icons.AutoMirrored.Outlined.Chat) { onOpenThread(threadId) } }
                        Box(Modifier.weight(1f)) { WorkActionRow("All threads", Icons.Outlined.ClearAll) { tasks.setThreadContext(null) } }
                    }
                }
                WorkResourceStatus(threadState, thread)
            }
        }
        when (section) {
            WorkSection.All -> holder.SaveableStateProvider("tasks-$connectionEpoch") { TasksScreen(
                viewModel = tasks,
                actions = actions,
                header = if (landingVisible) ({
                    WorkLandingSummary(
                        landing,
                        onAllGoals = { libraryRoute = null; sectionName = WorkSection.Goals.name },
                        onAllPlaybooks = { libraryRoute = null; sectionName = WorkSection.Programs.name },
                        onOpenPlaybook = { libraryRoute = WorkRoute("program", it.name, it.title); libraryEpoch++; sectionName = WorkSection.Programs.name },
                        onOpenNote = { landingNote = it },
                        onOpenChat = onOpenChat,
                    )
                }) else null,
            ) }
            WorkSection.NeedsYou -> {
                // The top-bar Refresh reconciles the same ledger the screen shows.
                val attention: ai.magicbeans.magdroid.attention.AttentionViewModel = viewModel()
                DisposableEffect(attention, actions) { actions.bindRefresh(attention::refresh); onDispose { actions.bindRefresh(null) } }
                AttentionScreen(onOpenTask, onOpenThread, onOpenSettings, onReviewPlan)
            }
            else -> key(connectionEpoch, section, libraryEpoch) { WorkLibraryScreen(section, actions, onOpenTask, initialRoute = libraryRoute, onOpenChat = onOpenChat) }
        }
    }
    SideEffect { actions.canCreate = canCreate }
    WorkLandingLoader(landing, landingWanted, connectionEpoch)
    landingNote?.let { path -> WorkGoalNoteSheet(path) { landingNote = null } }
}

/** The short name the More segment shows while one of its views is open. */
private val WorkSection.moreLabel: String get() = when (this) {
    WorkSection.Goals -> "Goals"
    WorkSection.Programs -> "Playbooks"
    WorkSection.Quality -> "Quality"
    WorkSection.Operations -> "Routines"
    else -> "More"
}

private val WorkSection.menuIcon: ImageVector get() = when (this) {
    WorkSection.Goals -> Icons.Outlined.TrackChanges
    WorkSection.Programs -> Icons.Outlined.Description
    WorkSection.Quality -> Icons.Outlined.Verified
    else -> Icons.Outlined.Autorenew
}

/**
 * One segmented track: All work · Needs you · Crew · More. More names the
 * open view (Goals, Playbooks, Quality, Routines) and opens a menu of the rest. Labels
 * wrap to two lines, and stop growing past 1.6x text, so the track always fits
 * a 360dp phone.
 */
@Composable
private fun WorkViewSwitcher(section: WorkSection, moreOpen: Boolean, onMore: (Boolean) -> Unit, onSelect: (WorkSection) -> Unit, needsYouCount: Int = 0) {
    val density = LocalDensity.current
    val labelSize = (14f * minOf(density.fontScale, 1.6f) / density.fontScale).sp
    val shape = RoundedCornerShape(14.dp)
    val inMore = section in WORK_MORE_SECTIONS
    Row(
        Modifier.fillMaxWidth().padding(horizontal = WORK_SCREEN_PADDING_DP.dp, vertical = 4.dp)
            .clip(shape).background(Control).border(1.dp, ControlBorder, shape).padding(3.dp),
        horizontalArrangement = Arrangement.spacedBy(2.dp), verticalAlignment = Alignment.CenterVertically,
    ) {
        @Composable
        fun Segment(label: String, active: Boolean, tag: String, modifier: Modifier, trailing: Boolean = false, badge: Int = 0, onClick: () -> Unit) {
            val measurer = rememberTextMeasurer()
            val base = LocalTextStyle.current
            val weight = if (active) FontWeight.SemiBold else FontWeight.Medium
            val badgeText = workBadgeText(badge)
            val badgeSize = (11f * minOf(density.fontScale, 1.3f) / density.fontScale).sp
            BoxWithConstraints(
                modifier.heightIn(min = 40.dp).clip(RoundedCornerShape(10.dp))
                    .background(if (active) Coral.copy(alpha = 0.14f) else Color.Transparent)
                    .selectable(selected = active, role = if (trailing) Role.DropdownList else Role.Tab, onClick = onClick)
                    .testTag(tag).padding(horizontal = 2.dp, vertical = 4.dp),
                contentAlignment = Alignment.Center,
            ) {
                // One line always: shrink (never below 11dp of glyph) until the label fits, then ellipsize.
                val badgeWidth = badgeText?.let { with(density) { measurer.measure(it, base.copy(fontSize = badgeSize, fontWeight = FontWeight.SemiBold)).size.width.toDp() + 13.dp } } ?: 0.dp
                val room = with(density) { (maxWidth - (if (trailing) labelSize.toDp() + 2.dp else 0.dp) - badgeWidth).roundToPx() }
                val longest = label
                var size = labelSize.value
                while (size > 11f / density.fontScale && measurer.measure(longest, base.copy(fontSize = size.sp, fontWeight = weight), maxLines = 1).size.width > room) size -= 0.5f
                Row(horizontalArrangement = Arrangement.Center, verticalAlignment = Alignment.CenterVertically) {
                    Text(label, color = if (active) Coral else Ink.copy(alpha = 0.78f), fontSize = size.sp, lineHeight = size.sp * 1.15f,
                        fontWeight = weight, textAlign = TextAlign.Center, maxLines = 1, overflow = TextOverflow.Ellipsis, softWrap = false, modifier = Modifier.weight(1f, fill = false))
                    if (trailing) Icon(Icons.Outlined.KeyboardArrowDown, null, tint = if (active) Coral else Muted, modifier = Modifier.size(with(density) { labelSize.toDp() + 2.dp }))
                    badgeText?.let {
                        Spacer(Modifier.width(3.dp))
                        Text(it, color = Color.White, fontSize = badgeSize, lineHeight = badgeSize, fontWeight = FontWeight.SemiBold, maxLines = 1,
                            modifier = Modifier.clip(RoundedCornerShape(6.dp)).background(AccentFill).padding(horizontal = 5.dp, vertical = 2.dp).testTag("$tag-count"))
                    }
                }
            }
        }
        listOf(WorkSection.All, WorkSection.NeedsYou, WorkSection.Crew).forEach { item ->
            val count = if (item == WorkSection.NeedsYou) needsYouCount else 0
            Segment(item.title, section == item, "work-view-${item.name}",
                Modifier.weight(if (count > 0) 1.5f else 1f).then(if (count > 0) Modifier.semantics { contentDescription = "${item.title}, $count waiting" } else Modifier), badge = count) { onSelect(item) }
        }
        // More carries a chevron and the longest names, so it gets a little more room.
        Box(Modifier.weight(1.2f)) {
            Segment(section.moreLabel, inMore, "work-more", Modifier.fillMaxWidth().semantics { contentDescription = "More work views" }, trailing = true) { onMore(true) }
            DropdownMenu(expanded = moreOpen, onDismissRequest = { onMore(false) }, containerColor = activePalette.elevated) {
                WORK_MORE_SECTIONS.forEach { item ->
                    val selected = item == section
                    DropdownMenuItem(
                        text = { Text(item.title, color = if (selected) Coral else Ink, fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal) },
                        leadingIcon = { Icon(item.menuIcon, null, tint = Coral) },
                        trailingIcon = if (selected) ({ Icon(Icons.Outlined.Check, "Selected", tint = Coral, modifier = Modifier.size(18.dp)) }) else null,
                        onClick = { onSelect(item) },
                        modifier = Modifier.heightIn(min = 48.dp),
                    )
                }
            }
        }
        WorkHelpButton(section.help, buttonSize = 40.dp)
    }
}

/** The Needs you count as the segment shows it: nothing at zero, `99+` past 99. */
internal fun workBadgeText(count: Int): String? = when {
    count <= 0 -> null
    count > 99 -> "99+"
    else -> "$count"
}

private val WORK_MORE_SECTIONS = listOf(WorkSection.Goals, WorkSection.Programs, WorkSection.Quality, WorkSection.Operations)
