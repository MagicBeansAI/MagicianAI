package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.WorkRepository
import ai.magicbeans.magdroid.work.WorkSection
import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Chat
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.TrackChanges
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonElement

// The All work landing's Goals and Playbooks summary (the web's compact GoalsWorkspace and
// ProgramsWorkspace over the task list), and the Quality checks lane counts.

/** One playbook document in a list: its file name and title (the name when it has none). */
internal data class WorkPlaybookSummary(val name: String, val title: String)

/** The `programs` of a `/programs` response that name a document, in the server's order. */
internal fun workPlaybookSummaries(value: JsonElement?): List<WorkPlaybookSummary> =
    value.workRows("programs").mapNotNull { row ->
        row.workText("name")?.takeIf(String::isNotBlank)?.let { name -> WorkPlaybookSummary(name, row.workText("title")?.takeIf(String::isNotBlank) ?: name) }
    }

internal fun workPlaybookCount(count: Int): String = "$count playbook${if (count == 1) "" else "s"}"

/** The Quality checks catalog's run counts, as the web's tabs read them: needing review is
 * proposals (`brief`), drafts to review and runs needing input; runs counts the active ones. */
internal data class WorkQualityCounts(val needsReview: Int, val active: Int) {
    companion object {
        /** Null when the response carries no `counts` (an older server): the lanes then show no number. */
        fun from(value: JsonElement?): WorkQualityCounts? {
            val counts = value.workField("counts") ?: return null
            fun count(key: String) = counts.workText(key)?.toIntOrNull() ?: 0
            return WorkQualityCounts(needsReview = count("brief") + count("review") + count("attention"), active = count("active"))
        }
    }
}

/** The Quality checks lane labels: "Needs review · 3" and "Runs · 2 active". */
internal fun workQualityLanes(counts: WorkQualityCounts?): List<Pair<String, String>> = listOf(
    "inbox" to (counts?.let { "Needs review · ${it.needsReview}" } ?: "Needs review"),
    "runs" to (counts?.let { "Runs · ${it.active} active" } ?: "Runs"),
)

internal object WorkLandingCopy {
    const val GOALS = "Goals"
    const val PLAYBOOKS = "Playbooks"
    const val ALL_GOALS = "All goals →"
    const val ALL_PLAYBOOKS = "All playbooks →"
    const val LOADING_PLAYBOOKS = "Loading playbooks…"
    const val NO_PLAYBOOKS = "No playbook documents in this workspace."
    /** At most this many goals and playbooks show on the landing. */
    const val GOALS_SHOWN = 3
    const val PLAYBOOKS_SHOWN = 4
}

/** Which summary sections are open; remembered on the device so the task list keeps its room. */
internal object WorkLandingMemory {
    private const val FILE = "magdroid.work"
    fun open(context: android.content.Context, section: String): Boolean =
        context.getSharedPreferences(FILE, android.content.Context.MODE_PRIVATE).getBoolean("landing_open_$section", false)
    fun setOpen(context: android.content.Context, section: String, open: Boolean) {
        context.getSharedPreferences(FILE, android.content.Context.MODE_PRIVATE).edit().putBoolean("landing_open_$section", open).apply()
    }
}

/** A load's outcome: loading until [value] or [error] arrives. */
internal data class WorkLandingLoad<T>(val value: T? = null, val error: String? = null) {
    val loading: Boolean get() = value == null && error == null
}

/**
 * What the landing read: the goals under each workspace's `Goals/` (light limits) and the playbook list.
 * It lives with the Work screen, so it loads once per visit to Work, not per lane or view change.
 */
@Stable
internal class WorkLandingState {
    /** Each workspace's open goal tree and its closed goals. */
    var goals by mutableStateOf(WorkLandingLoad<List<WorkGoalShown>>())
    var playbooks by mutableStateOf(WorkLandingLoad<List<WorkPlaybookSummary>>())
    var generation by mutableIntStateOf(0)
        private set
    fun reload() { goals = WorkLandingLoad(); playbooks = WorkLandingLoad(); generation++ }
}

/** Loads [state] once [wanted] (the summary has been on screen), again on its reload, a connection change
 * or a saved goal edit (Mark done or Reopen on the Goals screen). */
@Composable
internal fun WorkLandingLoader(state: WorkLandingState, wanted: Boolean, connectionEpoch: Int) {
    val context = LocalContext.current.applicationContext
    val goalEdits by WorkGoalEdits.saved.collectAsState()
    LaunchedEffect(state, wanted, connectionEpoch, state.generation, goalEdits) {
        if (!wanted) return@LaunchedEffect
        state.goals = WorkLandingLoad(); state.playbooks = WorkLandingLoad()
        val repository = try { WorkRepository.connected(context) } catch (failure: Exception) {
            val message = failure.message ?: "Connect to your Magician in Settings."
            state.goals = WorkLandingLoad(error = message); state.playbooks = WorkLandingLoad(error = message)
            return@LaunchedEffect
        }
        try {
            // Playbooks are one request: show them before the goal notes finish.
            try {
                val programs = withContext(Dispatchers.IO) {
                    val (status, body) = repository.getWithStatus("/api/magician/v2/programs")
                    if (status !in 200..299) throw IllegalStateException(body.workText("message") ?: body.workText("error") ?: "Could not list playbooks ($status).")
                    workPlaybookSummaries(body)
                }
                if (repository.isCurrentConnection(context)) state.playbooks = WorkLandingLoad(programs)
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: Exception) { if (repository.isCurrentConnection(context)) state.playbooks = WorkLandingLoad(error = failure.message ?: failure.toString()) }
            try {
                val goals = withContext(Dispatchers.IO) {
                    WorkGoalReader.load(WorkGoalReadLimits.Landing) { path -> repository.getWithStatus(path) }.map(::workGoalShown)
                }
                if (repository.isCurrentConnection(context)) state.goals = WorkLandingLoad(goals)
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: Exception) { if (repository.isCurrentConnection(context)) state.goals = WorkLandingLoad(error = failure.message ?: failure.toString()) }
        } finally { repository.close() }
    }
}

/**
 * Goals and Playbooks over the All work task list, stacked and each folded to one line by default
 * (a phone needs the room for the tasks): open, a section lists its first goals or playbooks.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun WorkLandingSummary(
    state: WorkLandingState,
    onAllGoals: () -> Unit,
    onAllPlaybooks: () -> Unit,
    onOpenPlaybook: (WorkPlaybookSummary) -> Unit,
    onOpenNote: (String) -> Unit,
    onOpenChat: () -> Unit,
    modifier: Modifier = Modifier,
    /** Opens the Goals screen on its Closed list, as the web's `/work?view=goals&goals=closed`. */
    onClosedGoals: () -> Unit = { WorkDeepLinks.request(WorkLinkTarget.Library(WorkSection.Goals, WorkGoalCopy.CLOSED_ROUTE)) },
) {
    val context = LocalContext.current.applicationContext
    var goalsOpen by remember { mutableStateOf(WorkLandingMemory.open(context, "goals")) }
    var playbooksOpen by remember { mutableStateOf(WorkLandingMemory.open(context, "playbooks")) }
    val goals = state.goals
    val playbooks = state.playbooks
    val shownGoals = goals.value?.let { workGoalLandingNodes(it, WorkLandingCopy.GOALS_SHOWN) }.orEmpty()
    val goalCount = goals.value?.let { workGoalCount(it.map { shown -> shown.space }) }
    val closedCount = goals.value?.sumOf { it.closed.size } ?: 0
    Column(modifier.fillMaxWidth().padding(horizontal = WORK_SCREEN_PADDING_DP.dp).padding(bottom = 6.dp).testTag("work-landing")) {
        WorkGroup(modifier = Modifier.animateContentSize()) {
            LandingHeader(
                WorkLandingCopy.GOALS, Icons.Outlined.TrackChanges, count = goalCount,
                preview = shownGoals.joinToString(" · ") { it.goal.title }.ifBlank { null }, open = goalsOpen, tag = "work-landing-goals",
            ) { goalsOpen = !goalsOpen; WorkLandingMemory.setOpen(context, "goals", goalsOpen) }
            if (goalsOpen) {
                when {
                    goals.error != null -> LandingProblem(goals.error, state::reload)
                    goals.loading -> LandingNote(WorkGoalCopy.LOADING)
                    // Every goal read is done or dropped: say so rather than "no goals".
                    shownGoals.isEmpty() && closedCount > 0 -> {
                        LandingNote(WorkGoalCopy.ALL_CLOSED)
                        WorkRowDivider()
                        WorkRow(WorkGoalCopy.SEE_CLOSED, icon = Icons.Outlined.TrackChanges, modifier = Modifier.testTag("work-landing-closed-goals"), onClick = onClosedGoals)
                    }
                    shownGoals.isEmpty() -> {
                        LandingNote(WorkGoalCopy.EMPTY_LEAD)
                        WorkRowDivider()
                        WorkRow(WorkGoalCopy.EMPTY_LINK, icon = Icons.AutoMirrored.Outlined.Chat, onClick = onOpenChat)
                    }
                    else -> shownGoals.forEach { node -> WorkRowDivider(); LandingGoalRow(node) { onOpenNote(node.goal.path) } }
                }
                LandingFooter(WorkLandingCopy.ALL_GOALS, onAllGoals)
            }
            WorkRowDivider()
            LandingHeader(
                WorkLandingCopy.PLAYBOOKS, Icons.Outlined.Description, count = playbooks.value?.let { workPlaybookCount(it.size) },
                preview = playbooks.value?.take(WorkLandingCopy.PLAYBOOKS_SHOWN)?.joinToString(" · ") { it.title }?.ifBlank { null },
                open = playbooksOpen, tag = "work-landing-playbooks",
            ) { playbooksOpen = !playbooksOpen; WorkLandingMemory.setOpen(context, "playbooks", playbooksOpen) }
            if (playbooksOpen) {
                val rows = playbooks.value
                when {
                    playbooks.error != null -> LandingProblem(playbooks.error, state::reload)
                    rows == null -> LandingNote(WorkLandingCopy.LOADING_PLAYBOOKS)
                    rows.isEmpty() -> LandingNote(WorkLandingCopy.NO_PLAYBOOKS)
                    else -> rows.take(WorkLandingCopy.PLAYBOOKS_SHOWN).forEach { playbook ->
                        WorkRowDivider()
                        WorkRow(playbook.title, detail = playbook.name.takeIf { it != playbook.title }, modifier = Modifier.testTag("work-landing-playbook-${playbook.name}")) { onOpenPlaybook(playbook) }
                    }
                }
                LandingFooter(WorkLandingCopy.ALL_PLAYBOOKS, onAllPlaybooks)
            }
        }
    }
}

/** A section's fold row: icon, name, count chip, a one-line preview while folded, and the chevron. */
@Composable
private fun LandingHeader(title: String, icon: ImageVector, count: String?, preview: String?, open: Boolean, tag: String, onToggle: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().heightIn(min = 52.dp)
            .clickable(role = Role.Button, onClickLabel = if (open) "Fold $title" else "Show $title", onClick = onToggle)
            .semantics(mergeDescendants = true) { heading(); stateDescription = if (open) "Expanded" else "Collapsed" }
            .testTag(tag).padding(vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        WorkRowIcon(icon, Coral)
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(title, color = Ink, fontSize = 16.sp, fontWeight = FontWeight.SemiBold, maxLines = 1, softWrap = false, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false))
                count?.let { WorkChip(it, Muted) }
            }
            if (!open && preview != null) Text(preview, color = Muted, fontSize = 13.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        WorkAccessoryIcon(if (open) WorkRowAccessory.Expanded else WorkRowAccessory.Collapsed)
    }
}

/** One goal: title, then status, owner, sub-goal count and target. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun LandingGoalRow(node: WorkGoalNode, onOpen: () -> Unit) {
    val goal = node.goal
    Column(
        Modifier.fillMaxWidth().heightIn(min = WORK_ROW_MIN_HEIGHT_DP.dp)
            .clickable(role = Role.Button, onClickLabel = "Open the ${goal.title} note", onClick = onOpen)
            .padding(vertical = 10.dp).testTag("work-landing-goal"),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Text(goal.title, color = Ink, fontSize = 15.sp, fontWeight = FontWeight.SemiBold, maxLines = 2, overflow = TextOverflow.Ellipsis)
        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            val centred = Modifier.align(Alignment.CenterVertically)
            Box(centred) { GoalStatusChip(goal.status) }
            goal.owner?.let { Text(if (it == "user") "You" else it, color = Muted, fontSize = 13.sp, modifier = centred) }
            if (node.children.isNotEmpty()) Box(centred) { WorkChip(workGoalSubGoals(node.children.size), Coral) }
            goal.target?.let { Text("Target $it", color = Muted, fontSize = 13.sp, modifier = centred) }
        }
    }
}

@Composable
private fun LandingNote(text: String) { WorkRowDivider(); WorkNote(text) }

@Composable
private fun LandingProblem(message: String, onRetry: () -> Unit) {
    WorkRowDivider()
    WorkNote(message, color = Danger)
    WorkActionRow("Retry", Icons.Outlined.Refresh, onClick = onRetry)
}

@Composable
private fun LandingFooter(label: String, onClick: () -> Unit) {
    WorkRowDivider()
    Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.CenterEnd) {
        TextButton(onClick = onClick, modifier = Modifier.heightIn(min = 48.dp).semantics { contentDescription = label.removeSuffix(" →") }) {
            Text(label, color = Coral, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
        }
    }
}

/** A Work bar overflow entry a view offers (Goals' Expand all and Collapse all). */
internal data class WorkBarMenuItem(val label: String, val enabled: Boolean = true, val onClick: () -> Unit)
