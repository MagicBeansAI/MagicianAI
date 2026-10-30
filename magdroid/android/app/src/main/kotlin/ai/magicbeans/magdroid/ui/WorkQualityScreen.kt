package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.*
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

internal const val WORK_QUALITY_API = "/api/magician/v2/relentless"

private val RUN_STATUSES = listOf("all", "active", "review", "attention", "complete", "stopped")
private val REVIEW_TYPES = listOf("all", "attention", "review", "brief")

@Composable
internal fun WorkQualityScreen(push: (WorkRoute) -> Unit, onOpenTask: (String) -> Unit, taskId: String? = null, agentId: String = "") {
    var owner by remember { mutableStateOf(agentId) }
    var statusFilter by remember { mutableStateOf("all") }
    var filters by remember { mutableStateOf(false) }
    val crew = rememberWorkResource("/api/magician/v2/agents")
    val crewState by crew.state.collectAsStateWithLifecycle()
    var lane by remember { mutableStateOf("inbox") }
    var search by remember { mutableStateOf("") }
    var page by remember { mutableIntStateOf(0) }
    val path = "$WORK_QUALITY_API/catalog?view=$lane&limit=20&offset=${page * 20}&q=${WorkRepository.segment(search)}&status=$statusFilter" +
        if (owner.isNotBlank()) "&agent_id=${WorkRepository.segment(owner)}" else ""
    val resource = rememberWorkResource(path)
    val state by resource.state.collectAsStateWithLifecycle()
    val contextResource = if (!taskId.isNullOrBlank()) rememberWorkResource("$WORK_QUALITY_API/task-context/${WorkRepository.segment(taskId)}") else null
    val contextState = contextResource?.state?.collectAsStateWithLifecycle()?.value
    val crewRows = crewState.value.workRows("agents")
    // The catalog counts every lane at once; the last ones read stay while the next page loads.
    var counts by remember { mutableStateOf<WorkQualityCounts?>(null) }
    LaunchedEffect(state.value) { WorkQualityCounts.from(state.value)?.let { counts = it } }
    val ownerName = if (owner.isBlank()) "All members" else crewRows.firstOrNull { it.workField("definition").workText("agent_id") == owner }?.workField("definition")?.workText("name") ?: owner
    WorkPage {
        item { WorkSearchField("Search quality work", search, { search = it; page = 0 }) }
        item { WorkSegmented(workQualityLanes(counts), lane, { lane = it; page = 0; statusFilter = "all" }, Modifier.testTag("work-quality-lanes")) }
        item { WorkGroup {
            WorkActionRow("New quality work", Icons.Outlined.AddCircleOutline) { push(WorkRoute("quality-compose", taskId.orEmpty(), "Quality work", agentId = owner)) }
            WorkRowDivider()
            WorkRow("Filters", icon = Icons.Outlined.FilterList,
                value = listOfNotNull(statusFilter.takeIf { it != "all" }?.let(::workLabel), ownerName.takeIf { owner.isNotBlank() }).joinToString(" · ").ifBlank { null },
                accessory = if (filters) WorkRowAccessory.Expanded else WorkRowAccessory.Collapsed) { filters = !filters }
            if (filters) {
                WorkRowDivider()
                WorkChoiceRow(if (lane == "runs") "Run status" else "Review type", (if (lane == "runs") RUN_STATUSES else REVIEW_TYPES).map { it to workLabel(it) }, statusFilter, { statusFilter = it; page = 0 })
                WorkRowDivider()
                val members = listOf("" to "All members") + listOfNotNull(owner.takeIf { it.isNotBlank() && crewRows.none { row -> row.workField("definition").workText("agent_id") == it } }?.let { it to it }) +
                    crewRows.mapNotNull { row -> row.workField("definition").workText("agent_id")?.let { it to (row.workField("definition").workText("name") ?: it) } }
                WorkChoiceRow("Crew member", members, owner, { owner = it; page = 0 })
                if (crewState.hasStatus) Box(Modifier.padding(bottom = 12.dp)) { WorkResourceStatus(crewState, crew) }
            }
        } }
        contextState?.let { contextual ->
            workStatusItem(contextual, requireNotNull(contextResource), key = "context-status")
            contextual.value?.let { value -> item { WorkHeaderCard(value.workText("title") ?: taskId.orEmpty(), subtitle = "Attached to task") {
                value.workText("pending_run_id")?.let { id -> WorkRow("Open current quality run", icon = Icons.Outlined.Autorenew) { push(WorkRoute("quality-run", id, "Quality run")) }; WorkRowDivider() }
                WorkRow("Owning task", icon = Icons.Outlined.Checklist) { taskId?.let(onOpenTask) }
                WorkRowDivider()
                WorkRecordRow("Task quality context", value, icon = Icons.Outlined.Verified)
            } } }
        }
        workStatusItem(state, resource)
        state.value?.let { value -> item { Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            WorkGroup(if (lane == "runs") "Runs" else "Needs review") {
                val entries = value.workRows("items")
                if (entries.isEmpty()) WorkEmptyState("No quality work in this view.", icon = Icons.Outlined.Verified)
                entries.forEachIndexed { index, entry ->
                    val item = entry.workField("item")
                    if (index > 0) WorkRowDivider()
                    val runId = item.workText("id")
                    val proposalId = item.workField("origin").workText("request_id")
                    when {
                        entry.workText("kind") == "run" && runId != null -> {
                            val status = item.workText("status") ?: "unknown"
                            WorkItemRow(item.workText("goal") ?: runId,
                                subtitle = item.workText("attempt_count")?.let { "Attempts: $it of ${item.workField("max_attempts").workDisplay().let { cap -> if (cap == "0" || cap == "—") "Unlimited" else cap }}" },
                                icon = Icons.Outlined.Autorenew,
                                chips = listOf(WorkChipSpec(item.workText("label") ?: workLabel(status), workStatusTone(status)))) { push(WorkRoute("quality-run", runId, "Quality run")) }
                        }
                        entry.workText("kind") == "proposal" && proposalId != null ->
                            WorkItemRow(item.workText("goal") ?: "Owner proposal",
                                subtitle = "Review proposed work" + (item.workField("origin").workText("agent_id")?.let { " · $it" } ?: ""),
                                icon = Icons.Outlined.Lightbulb, chips = listOf(WorkChipSpec("Proposal", MWarn))) { push(WorkRoute("quality-proposal", proposalId, "Proposal")) }
                        else -> WorkRecord(entry)
                    }
                }
            }
            WorkPager(page, value.workField("pagination").workBool("has_more"), { page = it })
        } } }
        item { WorkGroup("Policies and learning") {
            WorkRow("Tasks and recurring quality", icon = Icons.Outlined.Repeat) { push(WorkRoute("quality-tasks", title = "Recurring quality")) }
            WorkRowDivider(); WorkRow("Capture policies", icon = Icons.Outlined.MoveToInbox) { push(WorkRoute("capture-policies", title = "Capture policies")) }
            WorkRowDivider(); WorkRow("Learning policies", icon = Icons.Outlined.School) { push(WorkRoute("learning-policies", title = "Learning policies")) }
            WorkRowDivider(); WorkRow("Learning proposals", icon = Icons.Outlined.Lightbulb) { push(WorkRoute("guidance-proposals", title = "Learning proposals")) }
        } }
        item { WorkGroup("Web console") { WorkRelatedTools(WorkWebContext(section = WorkSection.Quality, agent = owner, task = taskId.orEmpty()), listOf(WorkSpecialist.Analytics, WorkSpecialist.History, WorkSpecialist.Evals, WorkSpecialist.Events, WorkSpecialist.Budget)) } }
    }
}

@Composable
internal fun WorkQualityProposal(id: String, push: (WorkRoute) -> Unit) {
    val resource = rememberWorkResource("$WORK_QUALITY_API/proposals/${WorkRepository.segment(id)}")
    val state by resource.state.collectAsStateWithLifecycle()
    WorkPage {
        workStatusItem(state, resource)
        state.value?.let { proposed ->
            item { WorkHeaderCard(proposed.workField("proposal").workText("goal") ?: "Proposed work",
                subtitle = proposed.workField("origin").workText("agent_id")?.let { "Proposed by $it" }, detail = proposed.workText("reason"),
                chips = listOf(WorkChipSpec("Proposal", MWarn))) {
                WorkActionRow("Review and configure", Icons.Outlined.Tune) { push(WorkRoute("quality-compose", "proposal:$id", "Quality work")) }
            } }
            item { WorkGroup("Proposal record") { WorkRecord(proposed) } }
        }
    }
}

@Composable
internal fun WorkQualityRun(id: String, push: (WorkRoute) -> Unit, onOpenTask: (String) -> Unit) {
    val path = "$WORK_QUALITY_API/runs/${WorkRepository.segment(id)}"
    val resource = rememberWorkResource(path)
    val state by resource.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    var cancel by remember { mutableStateOf(false) }
    LaunchedEffect(state.identity) { cancel = false }
    fun act(action: String, body: JsonObject = buildJsonObject {}) {
        val identity = state.identity
        scope.launch { if (resource.perform("$path/$action", identity, body = body) != null) resource.refresh() }
    }
    WorkPage {
        workStatusItem(state, resource)
        state.value?.let { run ->
            val status = run.workText("status") ?: "unknown"
            item { WorkHeaderCard(run.workField("contract").workText("goal") ?: id, subtitle = id, detail = run.workText("reason"),
                chips = listOf(WorkChipSpec(workLabel(status), workStatusTone(status)))) {
                run.workField("admission").workText("task_id")?.let { taskId -> WorkRow("Owning task", icon = Icons.Outlined.Checklist) { onOpenTask(taskId) }; WorkRowDivider() }
                if (status !in listOf("accepted", "plateau", "budget_exhausted", "cancelled", "failed")) {
                    WorkActionRow("Cancel quality run", Icons.Outlined.Cancel, destructive = true, enabled = !state.busy) { cancel = true }
                } else WorkActionRow("Reconcile available cost receipts", Icons.Outlined.Paid, enabled = !state.busy) { act("reconcile-costs") }
                if (status == "waiting_owner") {
                    run.workField("admission").workText("task_id")?.let { taskId -> WorkRowDivider(); WorkRow("Answer task question", icon = Icons.Outlined.QuestionAnswer) { onOpenTask(taskId) } }
                    run.workText("attention_id")?.let { attentionId ->
                        WorkRowDivider(); WorkNote("Answer the canonical request before resuming this run.")
                        WorkRowDivider(); WorkActionRow("Resume after answer", Icons.Outlined.PlayCircleOutline, enabled = !state.busy) { act("resume", buildJsonObject { put("attention_id", attentionId) }) }
                    }
                }
            } }
            item { WorkGroup("Budget and spend") { WorkRecord(buildJsonObject { put("spend", run.workField("spend") ?: JsonNull); put("budget", run.workField("contract").workField("budget") ?: JsonNull) }) } }
            item { WorkGroup("Attempts and exact outputs") {
                val attempts = run.workRows("attempts")
                if (attempts.isEmpty()) WorkRow("No attempts yet.", icon = Icons.Outlined.HourglassEmpty, labelColor = Muted, accessory = WorkRowAccessory.None)
                attempts.forEachIndexed { index, attempt ->
                    if (index > 0) WorkRowDivider()
                    var open by remember(attempt) { mutableStateOf(false) }
                    val hash = attempt.workField("artifact").workText("hash")
                    WorkRow("Attempt ${attempt.workField("number").workDisplay()}", icon = Icons.Outlined.Numbers, value = hash,
                        accessory = if (open) WorkRowAccessory.Expanded else WorkRowAccessory.Collapsed) { open = !open }
                    if (open) WorkInsetPanel(Modifier.padding(bottom = 12.dp)) {
                        hash?.let { WorkRow("Inspect artifact and evidence", icon = Icons.Outlined.FindInPage) { push(WorkRoute("artifact", "$id/$it", "Artifact")) }; WorkRowDivider() }
                        WorkRecord(attempt)
                    }
                }
            } }
            item { WorkGroup("Reports and evidence") {
                WorkRow("Run report", icon = Icons.Outlined.Summarize) { push(WorkRoute("endpoint", "$path/report", "Run report")) }
                if (run.workField("handoff").workText("kind") == "capture") {
                    val hash = run.workField("handoff").workText("artifact_hash")
                    val taskId = run.workField("admission").workText("task_id")
                    if (hash != null && taskId != null) { WorkRowDivider(); WorkRow("Bind exact local capture", icon = Icons.Outlined.Link) { push(WorkRoute("capture-bind", id, "Local capture", artifactHash = hash, taskId = taskId)) } }
                }
                WorkRowDivider(); WorkRow("Observed lessons", icon = Icons.Outlined.Lightbulb) { push(WorkRoute("endpoint", "$path/lessons", "Observed lessons")) }
                WorkRowDivider(); WorkRecordRow("Contract, decisions and retained work", run, icon = Icons.Outlined.Article)
            } }
        }
    }
    if (cancel) AlertDialog(onDismissRequest = { cancel = false }, title = { Text("Cancel this quality run?", color = Ink) },
        confirmButton = { TextButton(onClick = { cancel = false; act("cancel") }) { Text("Cancel run", color = Danger) } },
        dismissButton = { TextButton(onClick = { cancel = false }) { Text("Keep running", color = Muted) } }, containerColor = Ground)
}

/** One file's exact text in a titled card. */
@Composable
private fun WorkFileCard(name: String, content: String) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        WorkGroupTitle(name)
        WorkCard { SelectionContainer { Text(content, color = Ink, fontSize = 15.sp, lineHeight = 21.sp) } }
    }
}

@Composable
internal fun WorkQualityArtifact(target: String, push: (WorkRoute) -> Unit, onReviewed: (() -> Unit)? = null) {
    val runId = target.substringBefore('/')
    val hash = target.substringAfter('/')
    val path = "$WORK_QUALITY_API/runs/${WorkRepository.segment(runId)}/artifacts/${WorkRepository.segment(hash)}"
    val resource = rememberWorkResource(path)
    val state by resource.state.collectAsStateWithLifecycle()
    val runResource = rememberWorkResource("$WORK_QUALITY_API/runs/${WorkRepository.segment(runId)}")
    val runState by runResource.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    val inspectedBinaries = remember(hash) { mutableStateListOf<String>() }
    var reviewed by remember(hash) { mutableStateOf(false) }
    LaunchedEffect(state.identity, runState.identity) { reviewed = false; inspectedBinaries.clear() }
    val attempts = runState.value.workRows("attempts")
    val position = attempts.indexOfFirst { it.workField("artifact").workText("hash") == hash }
    val previous = if (position < 0) null else attempts.take(position).asReversed().mapNotNull { it.workField("artifact").workText("hash") }.firstOrNull { it != hash }
    val files = state.value.workField("files") as? JsonObject
    val binaryFiles = state.value.workField("binary_files") as? JsonObject
    val filesReady = (!files.isNullOrEmpty() || !binaryFiles.isNullOrEmpty()) && binaryFiles.orEmpty().keys.all { it in inspectedBinaries }
    WorkPage {
        item { WorkHeaderCard("Artifact", subtitle = hash) }
        workStatusItem(state, resource)
        workStatusItem(runState, runResource, key = "run-status")
        files?.forEach { (name, content) -> item(key = "file-$name") { WorkFileCard(name, content.workDisplay()) } }
        if (!binaryFiles.isNullOrEmpty()) item { WorkGroup("Binary artifacts") {
            binaryFiles.entries.forEachIndexed { index, (name, metadata) ->
                if (index > 0) WorkRowDivider()
                WorkRow(name, icon = Icons.Outlined.FolderZip, value = if (name in inspectedBinaries) "Reviewed" else null, accessory = WorkRowAccessory.None)
                WorkRecord(metadata)
                WorkBinaryArtifact(path, name, metadata.workText("media_type"), resource, state) { if (name !in inspectedBinaries) inspectedBinaries.add(name) }
            }
        } }
        item { WorkGroup("Review") {
            previous?.let { old -> WorkRow("Compare previous attempt", icon = Icons.Outlined.VerticalSplit) { push(WorkRoute("artifact-compare", runId, "Compare attempts", artifactHash = hash, previousHash = old)) }; WorkRowDivider() }
            WorkRow("Sealed evidence", icon = Icons.Outlined.Lock) { push(WorkRoute("endpoint", "$path/evidence", "Artifact evidence")) }
            if (onReviewed != null) { WorkRowDivider(); WorkActionRow("Mark exact files reviewed", Icons.Outlined.CheckCircle, enabled = filesReady, onClick = onReviewed) }
            val run = runState.value
            if (run.workText("status") == "accepted" && run.workField("routing").workText("route") == "human_review" &&
                run.workField("routing").workText("artifact_hash") == hash && run.workField("owner_approved") == JsonNull && run.workField("handoff") == JsonNull) {
                WorkRowDivider(); WorkNote("Review these exact files before approving handoff.")
                WorkRowDivider(); WorkActionRow("Mark files reviewed", if (reviewed) Icons.Outlined.TaskAlt else Icons.Outlined.CheckCircle, enabled = filesReady) { reviewed = true }
                WorkRowDivider()
                WorkActionRow("Approve exact draft", Icons.Outlined.Verified, enabled = reviewed && !state.busy) {
                    val identity = state.identity
                    scope.launch {
                        if (resource.perform("$WORK_QUALITY_API/runs/${WorkRepository.segment(runId)}/review", identity,
                            body = buildJsonObject { put("artifact_hash", hash); put("approved", true) }) != null) { reviewed = false; runResource.refresh() }
                    }
                }
                WorkRowDivider()
                WorkActionRow("Decline handoff", Icons.Outlined.Cancel, destructive = true, enabled = !state.busy) {
                    val identity = state.identity
                    scope.launch {
                        if (resource.perform("$WORK_QUALITY_API/runs/${WorkRepository.segment(runId)}/review", identity,
                            body = buildJsonObject { put("artifact_hash", hash); put("approved", false) }) != null) runResource.refresh()
                    }
                }
            }
        } }
    }
}

@Composable
internal fun WorkQualityComparison(runId: String, currentHash: String, previousHash: String) {
    val base = "$WORK_QUALITY_API/runs/${WorkRepository.segment(runId)}/artifacts/"
    val current = rememberWorkResource(base + WorkRepository.segment(currentHash))
    val previous = rememberWorkResource(base + WorkRepository.segment(previousHash))
    val currentState by current.state.collectAsStateWithLifecycle()
    val previousState by previous.state.collectAsStateWithLifecycle()
    val files = ((currentState.value.workField("files") as? JsonObject).orEmpty().keys + (previousState.value.workField("files") as? JsonObject).orEmpty().keys).sorted()
    WorkPage {
        item { WorkHeaderCard("Compare exact attempts", subtitle = "$previousHash → $currentHash") }
        workStatusItem(currentState, current)
        workStatusItem(previousState, previous, key = "previous-status")
        if (currentState.value != null && previousState.value != null) {
            items(files.distinct()) { name -> WorkGroup(name) {
                val old = previousState.value.workField("files").workText(name); val new = currentState.value.workField("files").workText(name)
                WorkRow("Change", icon = Icons.Outlined.Difference, value = if (old == new) "Unchanged" else if (old == null) "Added" else if (new == null) "Removed" else "Changed", accessory = WorkRowAccessory.None)
                var oldOpen by remember(name) { mutableStateOf(false) }; var newOpen by remember(name) { mutableStateOf(false) }
                WorkRowDivider()
                WorkRow("Previous attempt · $previousHash", icon = Icons.Outlined.History, accessory = if (oldOpen) WorkRowAccessory.Expanded else WorkRowAccessory.Collapsed) { oldOpen = !oldOpen }
                if (oldOpen) WorkInsetPanel(Modifier.padding(bottom = 12.dp)) { SelectionContainer { Text(old ?: "File absent from this attempt", color = Ink, fontSize = 15.sp, lineHeight = 21.sp, modifier = Modifier.padding(vertical = 12.dp)) } }
                WorkRowDivider()
                WorkRow("Current attempt · $currentHash", icon = Icons.Outlined.Description, accessory = if (newOpen) WorkRowAccessory.Expanded else WorkRowAccessory.Collapsed) { newOpen = !newOpen }
                if (newOpen) WorkInsetPanel(Modifier.padding(bottom = 12.dp)) { SelectionContainer { Text(new ?: "File absent from this attempt", color = Ink, fontSize = 15.sp, lineHeight = 21.sp, modifier = Modifier.padding(vertical = 12.dp)) } }
            } }
            for ((label, hash, resource, state) in listOf(QualityCompareSide("Previous binary artifacts", previousHash, previous, previousState), QualityCompareSide("Current binary artifacts", currentHash, current, currentState))) {
                val binaries = state.value.workField("binary_files") as? JsonObject
                if (!binaries.isNullOrEmpty()) item(key = label) { WorkGroup(label) {
                    binaries.entries.forEachIndexed { index, (name, metadata) ->
                        if (index > 0) WorkRowDivider()
                        WorkRow(name, icon = Icons.Outlined.FolderZip, accessory = WorkRowAccessory.None)
                        WorkRecord(metadata)
                        WorkBinaryArtifact(base + WorkRepository.segment(hash), name, metadata.workText("media_type"), resource, state)
                    }
                } }
            }
        }
    }
}

private data class QualityCompareSide(val label: String, val hash: String, val resource: WorkResource, val state: WorkResourceState)
