package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.*
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

internal const val WORK_QUALITY_API = "/api/magician/v2/relentless"

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
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item {
            Row {
                TextButton(onClick = { lane = "inbox"; page = 0; statusFilter = "all" }) { Text("Inbox", color = if (lane == "inbox") Coral else Muted) }
                TextButton(onClick = { lane = "runs"; page = 0; statusFilter = "all" }) { Text("Runs", color = if (lane == "runs") Coral else Muted) }
            }
            OutlinedTextField(search, { search = it; page = 0 }, label = { Text("Search quality work") }, modifier = Modifier.fillMaxWidth())
            WorkLink("Filters") { filters = !filters }
            if (filters) {
                WorkChoicePicker(if (lane == "runs") "Run status" else "Review type", statusFilter, if (lane == "runs") listOf("all", "active", "review", "attention", "complete", "stopped") else listOf("all", "attention", "review", "brief")) { statusFilter = it; page = 0 }
                WorkChoicePicker("Crew member", owner, listOf("", owner) + crewState.value.workRows("agents").mapNotNull { it.workField("definition").workText("agent_id") }) { owner = it; page = 0 }
                WorkResourceStatus(crewState, crew)
            }
            WorkLink("New quality work") { push(WorkRoute("quality-compose", taskId.orEmpty(), "Quality work", agentId = owner)) }
            contextState?.let { contextual ->
                WorkResourceStatus(contextual, requireNotNull(contextResource))
                WorkCard {
                    Text("Attached to task", color = Muted, fontSize = 12.sp)
                    Text(contextual.value.workText("title") ?: taskId.orEmpty(), color = Ink, fontSize = 16.sp)
                    WorkRecord(contextual.value)
                    contextual.value.workText("pending_run_id")?.let { id -> WorkLink("Open current quality run") { push(WorkRoute("quality-run", id, "Quality run")) } }
                    WorkLink("Owning task") { taskId?.let(onOpenTask) }
                }
            }
            WorkResourceStatus(state, resource)
            WorkLink("Refresh quality work", resource::refresh)
        }
        if (state.value != null && state.value.workRows("items").isEmpty()) item { Text("No quality work in this view.", color = Muted) }
        items(state.value.workRows("items")) { entry ->
            val item = entry.workField("item")
            when (entry.workText("kind")) {
                "run" -> item.workText("id")?.let { id -> Box(Modifier.clickable { push(WorkRoute("quality-run", id, "Quality run")) }) {
                    WorkCard { Text(item.workText("goal") ?: id, color = Ink, fontSize = 17.sp); Text(item.workText("label") ?: item.workText("status").orEmpty(), color = Muted, fontSize = 12.sp) }
                } }
                "proposal" -> item.workField("origin").workText("request_id")?.let { id -> Box(Modifier.clickable { push(WorkRoute("quality-proposal", id, "Proposal")) }) {
                    WorkCard { Text(item.workText("goal") ?: "Owner proposal", color = Ink, fontSize = 17.sp); Text("Review proposed work", color = Muted, fontSize = 12.sp) }
                } }
                else -> WorkCard { WorkRecord(entry) }
            }
        }
        item { Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            TextButton(onClick = { page = (page - 1).coerceAtLeast(0) }, enabled = page > 0) { Text("Previous", color = Coral) }
            Text("Page ${page + 1}", color = Muted, modifier = Modifier.padding(12.dp))
            TextButton(onClick = { page++ }, enabled = state.value.workField("pagination").workBool("has_more")) { Text("Next", color = Coral) }
        } }
        item { WorkCard { WorkRelatedTools(WorkWebContext(section = WorkSection.Quality, agent = owner, task = taskId.orEmpty()), listOf(WorkSpecialist.Analytics, WorkSpecialist.History, WorkSpecialist.Evals, WorkSpecialist.Events, WorkSpecialist.Budget)) } }
        item { WorkCard {
            WorkLink("Capture policies") { push(WorkRoute("capture-policies", title = "Capture policies")) }
            WorkLink("Learning policies") { push(WorkRoute("learning-policies", title = "Learning policies")) }
            WorkLink("Tasks and recurring quality") { push(WorkRoute("quality-tasks", title = "Recurring quality")) }
            WorkLink("Learning proposals") { push(WorkRoute("guidance-proposals", title = "Learning proposals")) }
        } }
    }
}

@Composable
internal fun WorkQualityProposal(id: String, push: (WorkRoute) -> Unit) {
    val resource = rememberWorkResource("$WORK_QUALITY_API/proposals/${WorkRepository.segment(id)}")
    val state by resource.state.collectAsStateWithLifecycle()
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { WorkResourceStatus(state, resource) }
        state.value?.let { proposed -> item { WorkCard {
            Text(proposed.workField("proposal").workText("goal") ?: "Proposed work", color = Ink, fontSize = 20.sp)
            WorkRecord(proposed)
            WorkLink("Review and configure") { push(WorkRoute("quality-compose", "proposal:$id", "Quality work")) }
        } } }
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
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { WorkResourceStatus(state, resource); WorkLink("Refresh run", resource::refresh) }
        state.value?.let { run ->
            item { WorkCard {
                Text(run.workField("contract").workText("goal") ?: id, color = Ink, fontSize = 21.sp)
                Text(workLabel(run.workText("status") ?: "unknown"), color = Coral, fontSize = 16.sp)
                run.workText("reason")?.let { Text(it, color = Muted, fontSize = 14.sp) }
                WorkRecord(buildJsonObject { put("spend", run.workField("spend") ?: JsonNull); put("budget", run.workField("contract").workField("budget") ?: JsonNull) })
                run.workField("admission").workText("task_id")?.let { taskId -> WorkLink("Owning task") { onOpenTask(taskId) } }
                if (run.workText("status") !in listOf("accepted", "plateau", "budget_exhausted", "cancelled", "failed")) {
                    TextButton(onClick = { cancel = true }, enabled = !state.busy) { Text("Cancel quality run", color = Danger) }
                } else WorkLink("Reconcile available cost receipts") { act("reconcile-costs") }
                if (run.workText("status") == "waiting_owner") {
                    WorkRecord(run.workField("attention_id"))
                    run.workField("admission").workText("task_id")?.let { taskId -> WorkLink("Answer task question") { onOpenTask(taskId) } }
                    run.workText("attention_id")?.let { attentionId -> WorkLink("Resume after answer") { act("resume", buildJsonObject { put("attention_id", attentionId) }) } }
                }
            } }
            item { Text("Attempts and exact outputs", color = Ink, fontSize = 18.sp) }
            items(run.workRows("attempts")) { attempt -> WorkCard {
                Text("Attempt ${attempt.workField("number").workDisplay()}", color = Ink, fontSize = 16.sp)
                WorkRecord(attempt)
                attempt.workField("artifact").workText("hash")?.let { hash ->
                    WorkLink("Inspect artifact and evidence") { push(WorkRoute("artifact", "$id/$hash", "Artifact")) }
                }
            } }
            item { WorkCard {
                WorkLink("Run report") { push(WorkRoute("endpoint", "$path/report", "Run report")) }
                if (run.workField("handoff").workText("kind") == "capture") {
                    val hash = run.workField("handoff").workText("artifact_hash")
                    val taskId = run.workField("admission").workText("task_id")
                    if (hash != null && taskId != null) WorkLink("Bind exact local capture") { push(WorkRoute("capture-bind", id, "Local capture", artifactHash = hash, taskId = taskId)) }
                }
                WorkLink("Observed lessons") { push(WorkRoute("endpoint", "$path/lessons", "Observed lessons")) }
                Text("Contract, decisions and retained work", color = Ink, fontSize = 17.sp)
                WorkRecord(run)
            } }
        }
    }
    if (cancel) AlertDialog(onDismissRequest = { cancel = false }, title = { Text("Cancel this quality run?", color = Ink) },
        confirmButton = { TextButton(onClick = { cancel = false; act("cancel") }) { Text("Cancel run", color = Danger) } },
        dismissButton = { TextButton(onClick = { cancel = false }) { Text("Keep running", color = Muted) } }, containerColor = Ground)
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
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { Text(hash, color = Muted, fontSize = 11.sp); WorkResourceStatus(state, resource); WorkResourceStatus(runState, runResource)
            val attempts = runState.value.workRows("attempts")
            val position = attempts.indexOfFirst { it.workField("artifact").workText("hash") == hash }
            val previous = if (position < 0) null else attempts.take(position).asReversed().mapNotNull { it.workField("artifact").workText("hash") }.firstOrNull { it != hash }
            previous?.let { old -> WorkLink("Compare previous attempt") { push(WorkRoute("artifact-compare", runId, "Compare attempts", artifactHash = hash, previousHash = old)) } }
        }
        (state.value.workField("files") as? JsonObject)?.forEach { (name, content) -> item(key = name) { WorkCard {
            Text(name, color = Ink, fontSize = 17.sp)
            SelectionContainer { Text(content.workDisplay(), color = Ink, fontSize = 14.sp) }
        } } }
        item { WorkCard {
            val binaryFiles = state.value.workField("binary_files") as? JsonObject
            binaryFiles?.forEach { (name, metadata) ->
                WorkRecord(metadata)
                WorkBinaryArtifact(path, name, metadata.workText("media_type"), resource, state) { if (name !in inspectedBinaries) inspectedBinaries.add(name) }
            }
            WorkLink("Sealed evidence") { push(WorkRoute("endpoint", "$path/evidence", "Artifact evidence")) }
            if (onReviewed != null) TextButton(onClick = onReviewed, enabled = (!(state.value.workField("files") as? JsonObject).isNullOrEmpty() || !binaryFiles.isNullOrEmpty()) && binaryFiles.orEmpty().keys.all { it in inspectedBinaries }) { Text("Mark exact files reviewed", color = Coral) }
            val run = runState.value
            if (run.workText("status") == "accepted" && run.workField("routing").workText("route") == "human_review" &&
                run.workField("routing").workText("artifact_hash") == hash && run.workField("owner_approved") == JsonNull && run.workField("handoff") == JsonNull) {
                val files = state.value.workField("files") as? JsonObject
                val binaries = state.value.workField("binary_files") as? JsonObject
                Text("Review these exact files before approving handoff.", color = Muted, fontSize = 13.sp)
                TextButton(onClick = { reviewed = true }, enabled = (!files.isNullOrEmpty() || !binaries.isNullOrEmpty()) && binaries.orEmpty().keys.all { it in inspectedBinaries }) { Text("Mark files reviewed", color = Coral) }
                TextButton(onClick = {
                    val identity = state.identity
                    scope.launch {
                        if (resource.perform("$WORK_QUALITY_API/runs/${WorkRepository.segment(runId)}/review", identity,
                            body = buildJsonObject { put("artifact_hash", hash); put("approved", true) }) != null) { reviewed = false; runResource.refresh() }
                    }
                }, enabled = reviewed && !state.busy) { Text("Approve exact draft", color = Coral) }
                TextButton(onClick = {
                    val identity = state.identity
                    scope.launch {
                        if (resource.perform("$WORK_QUALITY_API/runs/${WorkRepository.segment(runId)}/review", identity,
                            body = buildJsonObject { put("artifact_hash", hash); put("approved", false) }) != null) runResource.refresh()
                    }
                }, enabled = !state.busy) { Text("Decline handoff", color = Muted) }
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
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { Text("Compare exact attempts", color = Ink, fontSize = 20.sp); WorkResourceStatus(currentState, current); WorkResourceStatus(previousState, previous) }
        if (currentState.value != null && previousState.value != null) {
            items(files.distinct()) { name -> WorkCard {
                Text(name, color = Ink, fontSize = 17.sp)
                val old = previousState.value.workField("files").workText(name); val new = currentState.value.workField("files").workText(name)
                Text(if (old == new) "Unchanged" else if (old == null) "Added" else if (new == null) "Removed" else "Changed", color = Muted, fontSize = 13.sp)
                var oldOpen by remember(name) { mutableStateOf(false) }; var newOpen by remember(name) { mutableStateOf(false) }
                WorkLink("Previous attempt · $previousHash") { oldOpen = !oldOpen }
                if (oldOpen) SelectionContainer { Text(old ?: "File absent from this attempt", color = Ink, fontSize = 14.sp) }
                WorkLink("Current attempt · $currentHash") { newOpen = !newOpen }
                if (newOpen) SelectionContainer { Text(new ?: "File absent from this attempt", color = Ink, fontSize = 14.sp) }
            } }
            item { WorkCard {
                for ((label, hash, resource, state) in listOf(QualityCompareSide("Previous binary artifacts", previousHash, previous, previousState), QualityCompareSide("Current binary artifacts", currentHash, current, currentState))) {
                    val binaries = state.value.workField("binary_files") as? JsonObject
                    if (!binaries.isNullOrEmpty()) {
                        Text(label, color = Ink, fontSize = 17.sp)
                        binaries.forEach { (name, metadata) -> WorkRecord(metadata); WorkBinaryArtifact(base + WorkRepository.segment(hash), name, metadata.workText("media_type"), resource, state) }
                    }
                }
            } }
        }
    }
}

private data class QualityCompareSide(val label: String, val hash: String, val resource: WorkResource, val state: WorkResourceState)
