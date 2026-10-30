package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.WorkRepository
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.clickable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Chat
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material.icons.outlined.KeyboardArrowDown
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.ReportProblem
import androidx.compose.material.icons.outlined.TrackChanges
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * Work → More → Goals: the open goal tree from the notes under each workspace's `Goals/` folder,
 * and the Closed list (done and dropped goals), at parity with the web's `GoalsWorkspace`.
 * Mark done and Reopen change a goal's `status:` (and `closed:`) through `PATCH /notes/front-matter`;
 * other changes go through chat or the notes.
 */
@Composable
internal fun WorkGoalsScreen(
    push: (WorkRoute) -> Unit, onOpenChat: () -> Unit, limits: WorkGoalReadLimits = WorkGoalReadLimits.Standard,
    /** Where Expand all and Collapse all go: the Work bar's overflow menu. */
    actions: TasksScreenActions? = null,
    /** Opens on the Closed list (`/work?view=goals&goals=closed`) instead of the open goals. */
    closed: Boolean = false,
) {
    val context = LocalContext.current.applicationContext
    val scope = rememberCoroutineScope()
    var reload by remember { mutableIntStateOf(0) }
    var loading by remember { mutableStateOf(true) }
    var error by remember { mutableStateOf<String?>(null) }
    var workspaces by remember { mutableStateOf<List<WorkGoalShown>>(emptyList()) }
    var openNote by remember { mutableStateOf<String?>(null) }
    var showClosed by remember { mutableStateOf(closed) }
    // The note path a Mark done or Reopen is saving, and what the last one did.
    var saving by remember { mutableStateOf<String?>(null) }
    var notice by remember { mutableStateOf<String?>(null) }
    // Goal note paths whose sub-goals are folded; kept across a reload, as on the web.
    var collapsed by remember { mutableStateOf(emptySet<String>()) }
    val collapsible = remember(workspaces) { workGoalCollapsible(workspaces.map { it.tree }) }
    /** Mark done moves the goal under Closed; Reopen brings it back as active. Both reload, since closing
     * or reopening a goal can move the open goals under it; a failure keeps the screen as it is. */
    fun setStatus(goal: WorkGoal, done: Boolean) {
        if (saving != null) return
        saving = goal.path; notice = null
        scope.launch {
            val repository = try { WorkRepository.connected(context) } catch (failure: Exception) {
                notice = WorkGoalCopy.statusNotice(goal, done, WorkGoalSaveResult.Failed(failure.message ?: "Connect to your Magician in Settings.")); saving = null; return@launch
            }
            try {
                val result = withContext(Dispatchers.IO) { workUpdateGoalStatus(goal, done) { path, body -> repository.patchWithStatus(path, body) } }
                if (repository.isCurrentConnection(context)) {
                    notice = WorkGoalCopy.statusNotice(goal, done, result)
                    if (result !is WorkGoalSaveResult.Failed) reload++
                }
            } finally { repository.close(); saving = null }
        }
    }
    val markDone = remember { { goal: WorkGoal -> setStatus(goal, true) } }
    if (actions != null) {
        // Remembered actions, so an unchanged menu compares equal and the bar is not redrawn for nothing.
        val expandAll = remember { { collapsed = emptySet() } }
        val collapseAll = remember(collapsible) { { collapsed = collapsible } }
        val menu = listOf(
            WorkBarMenuItem(WorkGoalCopy.EXPAND_ALL, enabled = collapsed.any { it in collapsible }, onClick = expandAll),
            WorkBarMenuItem(WorkGoalCopy.COLLAPSE_ALL, enabled = !collapsible.all { it in collapsed }, onClick = collapseAll),
        )
        SideEffect { actions.bindMenu(menu) }
        DisposableEffect(actions) { onDispose { actions.bindMenu(null) } }
    }
    val refreshes = LocalWorkRefreshRequests.current
    LaunchedEffect(refreshes) { refreshes?.collect { reload++ } }
    // A newer load cancels this one; a connection switch discards its answer.
    LaunchedEffect(reload) {
        workspaces = emptyList(); error = null; loading = true
        val repository = try { WorkRepository.connected(context) } catch (failure: Exception) {
            error = failure.message ?: "Connect to your Magician in Settings."; loading = false; return@LaunchedEffect
        }
        try {
            val result = withContext(Dispatchers.IO) {
                WorkGoalReader.load(limits) { path -> repository.getWithStatus(path) }.map(::workGoalShown)
            }
            if (repository.isCurrentConnection(context)) { workspaces = result; loading = false }
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (failure: Exception) {
            if (repository.isCurrentConnection(context)) { error = failure.message ?: failure.toString(); loading = false }
        } finally { repository.close() }
    }
    WorkPage(Modifier.testTag("work-goals")) {
        item(key = "header") {
            WorkHeaderCard("Goals", detail = WorkGoalCopy.INTRO,
                chips = if (loading || error != null) emptyList() else listOf(WorkChipSpec(workGoalCount(workspaces.map { it.space }), Muted)))
        }
        if (!loading && error == null && workspaces.isNotEmpty()) item(key = "switch") {
            val open = workspaces.sumOf { shown -> shown.space.goals.count { !workIsClosedGoal(it) } }
            WorkSegmented(
                listOf("open" to WorkGoalCopy.open(open), "closed" to WorkGoalCopy.closed(workspaces.sumOf { it.closed.size })),
                if (showClosed) "closed" else "open", { showClosed = it == "closed" }, Modifier.testTag("work-goals-switch"),
            )
        }
        notice?.let { text ->
            item(key = "notice") {
                WorkGroup { WorkNote(text, Modifier.semantics { liveRegion = LiveRegionMode.Polite }, color = Ink) }
            }
        }
        val failure = error
        when {
            failure != null -> item(key = "error") {
                WorkGroup {
                    Row(Modifier.fillMaxWidth().padding(vertical = 12.dp), verticalAlignment = Alignment.Top) {
                        Icon(Icons.Filled.Warning, null, tint = Danger, modifier = Modifier.padding(top = 2.dp).size(with(LocalDensity.current) { 16.sp.toDp() }))
                        Spacer(Modifier.width(12.dp))
                        Text(failure, color = Ink, fontSize = 15.sp, lineHeight = 20.sp, modifier = Modifier.weight(1f).semantics { liveRegion = LiveRegionMode.Polite })
                    }
                    WorkRowDivider()
                    WorkActionRow("Retry", Icons.Outlined.Refresh) { reload++ }
                }
            }
            loading -> item(key = "loading") {
                Row(Modifier.fillMaxWidth().heightIn(min = WORK_ROW_MIN_HEIGHT_DP.dp).semantics(mergeDescendants = true) {},
                    horizontalArrangement = Arrangement.Center, verticalAlignment = Alignment.CenterVertically) {
                    CircularProgressIndicator(color = Coral, strokeWidth = 2.dp, modifier = Modifier.size(20.dp))
                    Spacer(Modifier.width(10.dp))
                    Text(WorkGoalCopy.LOADING, color = Muted, fontSize = 15.sp)
                }
            }
            workspaces.isEmpty() -> item(key = "empty") {
                WorkGroup {
                    WorkNote("${WorkGoalCopy.EMPTY_LEAD} ${WorkGoalCopy.EMPTY_LINK} ${WorkGoalCopy.EMPTY_TAIL}")
                    WorkRowDivider()
                    WorkRow(WorkGoalCopy.EMPTY_LINK, icon = Icons.AutoMirrored.Outlined.Chat, onClick = onOpenChat)
                }
            }
            showClosed -> workspaces.filter { it.space.showsInClosedList }.ifEmpty {
                item(key = "closed-empty") { WorkGroup(modifier = Modifier.testTag("work-goals-closed-empty")) { WorkNote(WorkGoalCopy.NO_CLOSED_GOALS_ANYWHERE) } }
                emptyList()
            }.forEach { shown ->
                item(key = "closed:${shown.space.path}") {
                    WorkGoalClosedSection(shown, saving, onReopen = { setStatus(it, false) }) { openNote = it }
                }
            }
            else -> workspaces.filter { it.space.showsInOpenList }.ifEmpty {
                item(key = "open-empty") {
                    WorkGroup(modifier = Modifier.testTag("work-goals-open-empty")) {
                        WorkNote(WorkGoalCopy.NO_OPEN_GOALS_ANYWHERE)
                        WorkRowDivider()
                        WorkRow(WorkGoalCopy.SEE_CLOSED, icon = Icons.Outlined.TrackChanges) { showClosed = true }
                    }
                }
                emptyList()
            }.forEach { shown ->
                item(key = "space:${shown.space.path}") {
                    WorkGoalWorkspaceSection(shown, limits, push, collapsed, { path -> collapsed = workGoalToggle(collapsed, path) },
                        saving = saving, onMarkDone = markDone) { openNote = it }
                }
            }
        }
    }
    openNote?.let { path -> WorkGoalNoteSheet(path) { openNote = null } }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun WorkGoalNoteSheet(path: String, onDismiss: () -> Unit) {
    ModalBottomSheet(onDismissRequest = onDismiss) { NotesScreen(initialPath = path) }
}

@Composable
private fun WorkGoalProblems(space: WorkGoalWorkspace) {
    if (space.problems.isEmpty()) return
    var expanded by remember(space.path) { mutableStateOf(false) }
    WorkGroup {
        WorkRow(WorkGoalCopy.problems(space.problems.size), icon = Icons.Outlined.ReportProblem,
            accessory = if (expanded) WorkRowAccessory.Expanded else WorkRowAccessory.Collapsed) { expanded = !expanded }
        if (expanded) Column(Modifier.fillMaxWidth().padding(bottom = 10.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            space.problems.forEach { SelectionContainer { Text(it, color = Muted, fontSize = 13.sp, lineHeight = 17.sp) } }
        }
    }
}

@Composable
private fun WorkGoalWorkspaceSection(
    shown: WorkGoalShown, limits: WorkGoalReadLimits, push: (WorkRoute) -> Unit,
    collapsed: Set<String>, onToggle: (String) -> Unit,
    saving: String?, onMarkDone: (WorkGoal) -> Unit, onOpenNote: (String) -> Unit,
) {
    val space = shown.space
    val tree = shown.tree
    Column(Modifier.fillMaxWidth().semantics { contentDescription = "${space.name} goals" }, verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Text(space.name, color = Ink, fontSize = 20.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.semantics { heading() })
        WorkGoalProblems(space)
        if (space.notShown > 0) Muted13(WorkGoalCopy.notShown(space.notShown, limits))
        if (space.foldersNotRead > 0) Muted13(WorkGoalCopy.foldersNotRead(space.foldersNotRead, limits))
        if (tree.roots.isNotEmpty()) Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            tree.roots.forEach { WorkGoalRow(it, tree.byKey, collapsed = collapsed, onToggle = onToggle, saving = saving, onMarkDone = onMarkDone, push = push, onOpenNote = onOpenNote) }
        } else if (tree.orphans.isEmpty()) Muted13(if (shown.closed.isNotEmpty()) WorkGoalCopy.NO_OPEN_GOALS else WorkGoalCopy.NO_GOALS)
        if (tree.orphans.isNotEmpty()) Column(
            Modifier.fillMaxWidth().padding(top = 6.dp).semantics { contentDescription = "${space.name} goals not linked" },
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(WorkGoalCopy.NOT_LINKED, color = Ink, fontSize = 17.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.semantics { heading() })
            Muted13(WorkGoalCopy.NOT_LINKED_DETAIL)
            tree.orphans.forEach { orphan -> WorkGoalRow(orphan.node, tree.byKey, note = workGoalOrphanNote(orphan), collapsed = collapsed, onToggle = onToggle, saving = saving, onMarkDone = onMarkDone, push = push, onOpenNote = onOpenNote) }
        }
    }
}

/** One workspace's done and dropped goals, most recently closed first: each opens its note and can be reopened. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun WorkGoalClosedSection(shown: WorkGoalShown, saving: String?, onReopen: (WorkGoal) -> Unit, onOpenNote: (String) -> Unit) {
    val space = shown.space
    Column(Modifier.fillMaxWidth().semantics { contentDescription = "${space.name} closed goals" }, verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Text(space.name, color = Ink, fontSize = 20.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.semantics { heading() })
        WorkGoalProblems(space)
        if (shown.closed.isEmpty()) Muted13(WorkGoalCopy.NO_CLOSED_GOALS)
        else Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            shown.closed.forEach { goal ->
                Surface(color = activePalette.card, shape = RoundedCornerShape(WORK_CARD_RADIUS_DP.dp), border = BorderStroke(1.dp, BorderSoft),
                    modifier = Modifier.fillMaxWidth().testTag("work-goal-closed")) {
                    FlowRow(
                        Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp),
                        horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(4.dp),
                    ) {
                        val centred = Modifier.align(Alignment.CenterVertically)
                        Box(centred) { GoalLink(goal.title, underline = false, label = "Open the ${goal.title} note") { onOpenNote(goal.path) } }
                        Box(centred) { GoalStatusChip(goal.status) }
                        workGoalParentTitle(goal, shown.tree.byKey)?.let { Text(WorkGoalCopy.under(it), color = Muted, fontSize = 13.sp, modifier = centred) }
                        goal.closed?.let { Text(WorkGoalCopy.closedOn(it), color = Muted, fontSize = 13.sp, modifier = centred) }
                        Box(centred) {
                            GoalLink(if (saving == goal.path) WorkGoalCopy.SAVING else WorkGoalCopy.REOPEN, underline = false,
                                enabled = saving == null, label = "Reopen ${goal.title}") { onReopen(goal) }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun Muted13(text: String) { Text(text, color = Muted, fontSize = 13.sp, lineHeight = 17.sp, modifier = Modifier.fillMaxWidth()) }

/** One goal card and, nested under it, its sub-goals (8 levels; deeper ones are counted).
 * A goal with sub-goals folds them from its title row; folded, it counts them. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun WorkGoalRow(
    node: WorkGoalNode, goals: Map<String, WorkGoal>, depth: Int = 0,
    /** Why this goal heads a "Not linked" tree. */
    note: String? = null,
    collapsed: Set<String>, onToggle: (String) -> Unit,
    /** The note path a Mark done or Reopen is saving; every Mark done waits for it. */
    saving: String?, onMarkDone: (WorkGoal) -> Unit,
    push: (WorkRoute) -> Unit, onOpenNote: (String) -> Unit,
) {
    val goal = node.goal
    val border = BorderSoft
    val hasChildren = node.children.isNotEmpty()
    val folded = workGoalCollapsed(node, collapsed)
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Surface(color = activePalette.card, shape = RoundedCornerShape(WORK_CARD_RADIUS_DP.dp), border = BorderStroke(1.dp, border), modifier = Modifier.fillMaxWidth()) {
            Column(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 10.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Row(
                    Modifier.fillMaxWidth()
                        .then(if (hasChildren) Modifier.heightIn(min = 44.dp).clickable(
                            role = Role.Button, onClickLabel = if (folded) "Show sub-goals" else "Fold sub-goals",
                        ) { onToggle(goal.path) }.semantics { stateDescription = if (folded) "Collapsed" else "Expanded" } else Modifier)
                        .testTag("work-goal-title"),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    if (hasChildren) Icon(
                        if (folded) Icons.AutoMirrored.Outlined.KeyboardArrowRight else Icons.Outlined.KeyboardArrowDown, null,
                        tint = Muted, modifier = Modifier.padding(end = 6.dp).size(20.dp),
                    )
                    FlowRow(Modifier.weight(1f), horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        Text(goal.title, color = Ink, fontSize = 16.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.align(Alignment.CenterVertically))
                        if (goal.kind == WorkGoalKind.Peer) Box(Modifier.align(Alignment.CenterVertically)) { WorkChip("Peer goal", Ink) }
                        if (folded) Box(Modifier.align(Alignment.CenterVertically)) { WorkChip(workGoalSubGoals(node.children.size), Coral) }
                    }
                }
                // A Not linked head's note already says when its parent goal is closed.
                (note ?: node.closedParent?.let(WorkGoalCopy::closedParent))?.let { Muted13(it) }
                FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    GoalStatusChip(goal.status)
                    Box(Modifier.semantics(mergeDescendants = true) { contentDescription = "${goal.visibility.key}. ${goal.visibility.meaning}" }) { WorkChip(goal.visibility.key, Muted) }
                }
                if (goal.owner != null || goal.target != null) FlowRow(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    val owner = goal.owner
                    val centred = Modifier.align(Alignment.CenterVertically)
                    if (owner == "user") Text("Owner: you", color = Muted, fontSize = 13.sp, modifier = centred)
                    else if (owner != null) Row(centred, verticalAlignment = Alignment.CenterVertically) {
                        Text("Owner:", color = Muted, fontSize = 13.sp)
                        GoalLink(owner) { push(WorkRoute("member", owner, "Crew member")) }
                    }
                    goal.target?.let { Text("Target $it", color = Muted, fontSize = 13.sp, modifier = centred) }
                }
                if (goal.peers.isNotEmpty()) FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    val centred = Modifier.align(Alignment.CenterVertically)
                    Text("Peers", color = Muted, fontSize = 13.sp, modifier = centred)
                    goal.peers.forEach { name ->
                        val found = goals[workGoalKey(name)]
                        if (found != null) GoalLink(found.title) { onOpenNote(found.path) } else Text(name, color = Ink, fontSize = 13.sp, modifier = centred)
                    }
                }
                if (goal.metrics.isNotEmpty()) Muted13("Metrics: ${goal.metrics.joinToString(", ")}")
                FlowRow(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    goal.program?.let { program -> GoalLink("Program: $program →", underline = false) { push(WorkRoute("program", workGoalProgramName(program), "Playbook")) } }
                    GoalLink("Note →", underline = false) { onOpenNote(goal.path) }
                    GoalLink(if (saving == goal.path) WorkGoalCopy.SAVING else WorkGoalCopy.MARK_DONE, underline = false,
                        enabled = saving == null, label = "Mark ${goal.title} done") { onMarkDone(goal) }
                }
            }
        }
        if (hasChildren && !folded) {
            if (depth + 1 < WorkGoalCopy.MAX_DEPTH) Column(
                Modifier.fillMaxWidth().padding(start = 8.dp)
                    .drawBehind { drawLine(border, Offset(0f, 0f), Offset(0f, size.height), strokeWidth = 1.dp.toPx()) }
                    .padding(start = 8.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                node.children.forEach { WorkGoalRow(it, goals, depth + 1, collapsed = collapsed, onToggle = onToggle, saving = saving, onMarkDone = onMarkDone, push = push, onOpenNote = onOpenNote) }
            } else Text(WorkGoalCopy.deeper(node.children.size), color = Muted, fontSize = 13.sp, lineHeight = 17.sp, modifier = Modifier.padding(start = 16.dp))
        }
    }
}

/** Active in the accent, blocked in danger, done in success, dropped struck through. */
@Composable
internal fun GoalStatusChip(status: String) {
    val tone: Color = when (status) {
        "active" -> Coral
        "blocked" -> Danger
        "done" -> activePalette.success
        "dropped" -> Muted
        else -> Ink
    }
    Text(
        status, color = tone, fontSize = 12.sp, fontWeight = FontWeight.SemiBold,
        textDecoration = if (status == "dropped") TextDecoration.LineThrough else null,
        modifier = Modifier.clip(WorkChipShape).background(tone.copy(alpha = 0.12f)).border(1.dp, tone.copy(alpha = 0.35f), WorkChipShape)
            .padding(horizontal = 10.dp, vertical = 4.dp),
    )
}

/** [label] is what a screen reader says instead of [text], when the text alone is ambiguous. */
@Composable
private fun GoalLink(text: String, underline: Boolean = true, enabled: Boolean = true, label: String? = null, onClick: () -> Unit) {
    TextButton(
        onClick = onClick, enabled = enabled, contentPadding = PaddingValues(horizontal = 4.dp, vertical = 0.dp),
        colors = ButtonDefaults.textButtonColors(contentColor = Coral),
        modifier = if (label != null) Modifier.semantics { contentDescription = label } else Modifier,
    ) {
        Text(text, fontSize = 14.sp, fontWeight = FontWeight.Medium, textDecoration = if (underline) TextDecoration.Underline else null)
    }
}
