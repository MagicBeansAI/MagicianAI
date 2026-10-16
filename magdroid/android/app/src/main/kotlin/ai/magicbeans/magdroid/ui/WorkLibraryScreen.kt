package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.*
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

internal data class WorkRoute(val kind: String, val id: String = "", val title: String = "", val artifactHash: String = "", val taskId: String = "", val agentId: String = "", val goalId: String = "", val tier: String = "", val previousHash: String = "")

@Composable
internal fun WorkLibraryScreen(section: WorkSection, actions: TasksScreenActions, onOpenTask: (String) -> Unit, initialRoute: WorkRoute? = null) {
    val stack = remember { mutableStateListOf<WorkRoute>().apply { initialRoute?.let(::add) } }
    val root = WorkRoute(section.name)
    val route = stack.lastOrNull() ?: root
    val push: (WorkRoute) -> Unit = { stack.add(it) }
    BackHandler(stack.isNotEmpty()) { stack.removeAt(stack.lastIndex) }
    DisposableEffect(stack.isNotEmpty()) {
        actions.showDetail(stack.isNotEmpty())
        onDispose { actions.showDetail(false) }
    }
    Column(Modifier.fillMaxSize().background(Ground)) {
        if (stack.isNotEmpty()) Row(Modifier.fillMaxWidth().statusBarsPadding()) {
            TextButton(onClick = { stack.removeAt(stack.lastIndex) }, modifier = Modifier.heightIn(min = 48.dp)) { Text("‹ Back", color = Coral) }
            Text(route.title.ifEmpty { workLabel(route.kind) }, color = Ink, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f).padding(14.dp))
            TextButton(onClick = actions::showCreate) { Text("New work", color = Coral) }
        } else Text(section.title, color = Ink, fontSize = 20.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.padding(16.dp))
        key(route) {
            when (route.kind) {
                "Crew" -> WorkCrewList(push)
                "member" -> WorkCrewDetail(route.id, push)
                "member-edit" -> WorkCrewDetail(route.id, push, openEditor = true)
                "member-rules" -> WorkCrewRules(route.id)
                "member-bootstrap" -> WorkCrewBootstrap(route.id)
                "member-memory" -> WorkCrewMemory(route.id, push)
                "user-memory" -> WorkCrewUserMemory(route.id, push)
                "memory-tier" -> WorkCrewTier(route.id, route.tier, route.goalId)
                "member-create" -> WorkCrewCreate { if (stack.isNotEmpty()) stack.removeAt(stack.lastIndex) }
                "operator" -> WorkOperatorScreen(route.id)
                "Programs" -> WorkProgramsList(push)
                "program" -> WorkProgramDetail(route.id, push)
                "Quality" -> WorkQualityScreen(push, onOpenTask, agentId = route.agentId)
                "quality-tasks" -> WorkQualityTasks(push, onOpenTask)
                "guidance-proposals" -> WorkGuidanceProposals(push)
                "quality-context" -> WorkQualityScreen(push, onOpenTask, taskId = route.id)
                "quality-run" -> WorkQualityRun(route.id, push, onOpenTask)
                "quality-proposal" -> WorkQualityProposal(route.id, push)
                "quality-campaign" -> WorkPolicies(WorkPolicyKind.Capture, push, onOpenTask, initialPolicyId = route.id)
                "quality-compose" -> WorkQualityCompose(route.id, onOpenRun = { push(WorkRoute("quality-run", it, "Quality run")) }, agentId = route.agentId)
                "artifact" -> WorkQualityArtifact(route.id, push)
                "artifact-compare" -> WorkQualityComparison(route.id, route.artifactHash, route.previousHash)
                "Operations" -> WorkOperations(push)
                "capture-policies" -> WorkPolicies(WorkPolicyKind.Capture, push, onOpenTask)
                "learning-policies" -> WorkPolicies(WorkPolicyKind.Learning, push, onOpenTask)
                "capture-bind" -> WorkCaptureBinder(route.id, route.artifactHash, route.taskId, push)
                "captures" -> WorkCaptures(route.id, push)
                "sealed-evidence" -> WorkSealedEvidence(route.id, route.title)
                "endpoint" -> WorkEndpoint(route.id)
            }
        }
    }
}

@Composable
@OptIn(ExperimentalLayoutApi::class)
private fun WorkCrewList(push: (WorkRoute) -> Unit) {
    val resource = rememberWorkResource("/api/magician/v2/agents")
    val state by resource.state.collectAsStateWithLifecycle()
    val health = rememberWorkResource("/api/magician/v2/agents/health")
    val healthState by health.state.collectAsStateWithLifecycle()
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    LaunchedEffect(resource, health, lifecycle) { lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) { while (true) { delay(30_000); resource.refresh(); health.refresh() } } }
    val scope = rememberCoroutineScope()
    var search by remember { mutableStateOf("") }
    var tab by remember { mutableStateOf("Members") }
    var filtersOpen by remember { mutableStateOf(false) }
    var viewMenu by remember { mutableStateOf(false) }
    var actionsMenu by remember { mutableStateOf(false) }
    var status by remember { mutableStateOf("All") }
    var kind by remember { mutableStateOf("All") }
    var reloadDefinitions by remember { mutableStateOf(false) }
    LaunchedEffect(state.identity) { reloadDefinitions = false }
    val raw = state.value.workRows("agents").ifEmpty { (state.value as? JsonArray)?.toList().orEmpty() }
    val allMembers = raw.mapNotNull { runCatching { WorkCrewMember.decode(it as JsonObject) }.getOrNull() }
    fun memberHealth(id: String) = healthState.value.workRows("agents").firstOrNull { it.workText("agent_id") == id }
    var members = allMembers.filter { member ->
        val match = search.isBlank() || "${member.name} ${member.id} ${member.description}".contains(search, ignoreCase = true)
        val statusMatch = when (status) {
            "Active" -> member.status in listOf("running", "triggered")
            "Attention" -> member.status in listOf("error", "partial") || memberHealth(member.id).workField("overall").workField("inputs").workText("state") == "needs_attention"
            "Paused" -> member.status == "paused"
            "Disabled" -> member.disabled || member.status == "disabled"
            else -> true
        }
        match && statusMatch && (kind == "All" || member.kind == kind)
    }
    members = if (tab == "Leaderboard") members.sortedWith(compareByDescending<WorkCrewMember> { memberHealth(it.id).workField("rolling_7d").workText("calls")?.toLongOrNull() ?: -1L }
        .thenByDescending { memberHealth(it.id).workField("rolling_7d").workText("spend_usd")?.toDoubleOrNull() ?: -1.0 }.thenBy { it.name.lowercase() }) else members.sortedBy { it.name.lowercase() }
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { Box {
            TextButton(onClick = { viewMenu = true }, modifier = Modifier.heightIn(min = 48.dp)) { Text("Crew view: $tab", color = Coral) }
            DropdownMenu(viewMenu, { viewMenu = false }) { listOf("Members", "Leaderboard", "Hierarchy", "Pipeline roles").forEach { label -> DropdownMenuItem(text = { Text(label) }, onClick = { tab = label; viewMenu = false }) } }
        } }
        item { OutlinedTextField(search, { search = it }, label = { Text("Find crew") }, modifier = Modifier.fillMaxWidth()) }
        if (tab != "Pipeline roles") item {
            WorkLink("Filters · $status · $kind") { filtersOpen = !filtersOpen }
            if (filtersOpen) {
            FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                listOf("All", "Active", "Attention", "Paused", "Disabled").forEach { label -> FilterChip(selected = status == label, onClick = { status = label }, label = { Text(label) }, modifier = Modifier.heightIn(min = 48.dp)) }
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                listOf("All", "Personal", "Worker").forEach { label -> FilterChip(selected = kind == label, onClick = { kind = label }, label = { Text("Kind: $label") }, modifier = Modifier.heightIn(min = 48.dp)) }
            }
            }
        }
        item {
            WorkResourceStatus(state, resource)
            if (tab == "Leaderboard" || status == "Attention") WorkResourceStatus(healthState, health)
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                WorkLink("Add member") { push(WorkRoute("member-create", title = "Add member")) }
                Box {
                    TextButton(onClick = { actionsMenu = true }, modifier = Modifier.heightIn(min = 48.dp)) { Text("Refresh crew", color = Coral) }
                    DropdownMenu(actionsMenu, { actionsMenu = false }) {
                        DropdownMenuItem(text = { Text("Refresh status and health") }, onClick = { resource.refresh(); health.refresh(); actionsMenu = false })
                        DropdownMenuItem(text = { Text("Reload definitions from disk") }, onClick = { reloadDefinitions = true; actionsMenu = false }, enabled = state.value != null && !state.busy)
                    }
                }
            }
            if (state.value != null) Text("${allMembers.size} members · ${allMembers.count { it.status in listOf("running", "triggered") }} active · ${allMembers.count { it.disabled }} disabled", color = Muted, fontSize = 12.sp)
        }
        if (tab == "Pipeline roles") {
            item { Text("Read-only runtime pipeline roles", color = Muted, fontSize = 14.sp) }
            val pipeline = state.value.workRows("system_agents").filter { search.isBlank() || it.toString().contains(search, ignoreCase = true) }
            if (state.value != null && pipeline.isEmpty()) item { Text("No matching pipeline descriptors were reported.", color = Muted) }
            items(pipeline) { role -> WorkCard {
                Text(role.workText("name") ?: role.workText("agent_id").orEmpty(), color = Ink, fontSize = 18.sp)
                Text(role.workText("agent_id").orEmpty(), color = Muted, fontSize = 12.sp)
                Text(role.workText("description").orEmpty(), color = Ink, fontSize = 14.sp)
            } }
        } else {
            if (state.value != null && members.isEmpty()) item { Text("No matching crew members in this workspace.", color = Muted) }
            items(members, key = { it.id }) { member -> WorkCard {
                WorkLink((if (tab == "Leaderboard") "${members.indexOf(member) + 1}. " else "") + member.name) { push(WorkRoute("member", member.id, member.name)) }
                Text(member.kind + (if (member.isPrimary) " · Primary" else "") + " · " + if (member.disabled) "Disabled" else member.status, color = Muted, fontSize = 12.sp)
                when (tab) {
                    "Leaderboard" -> {
                        val metrics = memberHealth(member.id)
                        if (metrics == null) Text("Health and activity have not been reported.", color = Muted, fontSize = 13.sp)
                        else {
                            val overall = metrics.workField("overall"); val rolling = metrics.workField("rolling_7d")
                            Text("Health: ${overall.workField("score").workDisplay()} · ${overall.workField("inputs").workText("state").orEmpty()}", color = Ink, fontSize = 14.sp)
                            Text("7 days · ${rolling.workField("calls").workDisplay()} calls · $${rolling.workField("spend_usd").workDisplay()} · success ${rolling.workField("success_rate").workDisplay()}", color = Ink, fontSize = 13.sp)
                            WorkRecord(metrics)
                        }
                    }
                    "Hierarchy" -> {
                        fun targets(record: WorkCrewMember) = record.definition.workRows("delegation_targets").mapNotNull { (it as? JsonPrimitive)?.contentOrNull }
                        val targets = targets(member)
                        val parents = allMembers.filter { member.id in targets(it) || "*" in targets(it) }
                        Text("Delegation depth: ${member.definition.workField("coordination").workField("max_delegation_depth").workDisplay()}", color = Muted, fontSize = 13.sp)
                        if (parents.isEmpty()) Text("No configured parent", color = Muted, fontSize = 13.sp)
                        parents.forEach { parent -> WorkLink("From: ${parent.name}") { push(WorkRoute("member", parent.id, parent.name)) } }
                        if (targets.isEmpty()) Text("No delegation targets", color = Muted, fontSize = 13.sp)
                        targets.distinct().forEach { id ->
                            val target = allMembers.firstOrNull { it.id == id }
                            when {
                                id == "*" -> Text("Can delegate to any member", color = Ink, fontSize = 13.sp)
                                target == null -> Text("Missing target: $id", color = activePalette.warning, fontSize = 13.sp)
                                else -> {
                                    WorkLink("To: ${target.name}") { push(WorkRoute("member", id, target.name)) }
                                    crewDelegationCycle(member.id, id, allMembers)?.let { cycle -> Text("Delegation cycle: ${cycle.joinToString(" → ")}", color = activePalette.warning, fontSize = 12.sp) }
                                }
                            }
                        }
                    }
                    else -> if (member.description.isNotBlank()) Text(member.description, color = Ink, fontSize = 14.sp, maxLines = 3)
                }
            } }
        }
    }
    if (reloadDefinitions) AlertDialog(onDismissRequest = { reloadDefinitions = false }, title = { Text("Reload Crew definitions from disk?") }, text = { Text("Reloads the runtime definition cache after edits made outside the app.") }, confirmButton = {
        TextButton(onClick = { val identity = state.identity; reloadDefinitions = false; scope.launch { if (resource.perform("/api/magician/v2/agents/refresh-definitions", identity) != null) { resource.refresh(); health.refresh() } } }) { Text("Reload definitions", color = Coral) }
    }, dismissButton = { TextButton(onClick = { reloadDefinitions = false }) { Text("Cancel", color = Coral) } }, containerColor = Ground)
}

@Composable
private fun WorkProgramsList(push: (WorkRoute) -> Unit) {
    val resource = rememberWorkResource("/api/magician/v2/programs")
    val state by resource.state.collectAsStateWithLifecycle()
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { WorkResourceStatus(state, resource); WorkLink("Refresh programs", resource::refresh) }
        if (state.value != null && state.value.workRows("programs").isEmpty()) item { Text("No scoped program documents yet.", color = Muted) }
        items(state.value.workRows("programs")) { row ->
            row.workText("name")?.let { name -> Box(Modifier.clickable { push(WorkRoute("program", name, row.workText("title") ?: name)) }) {
                WorkCard { Text(row.workText("title") ?: name, color = Ink, fontSize = 18.sp); Text(name, color = Muted, fontSize = 12.sp) }
            } }
        }
        item { WorkProgramMembers("", push) }
    }
}

@Composable
private fun WorkProgramDetail(name: String, push: (WorkRoute) -> Unit) {
    val path = "/api/magician/v2/programs/${WorkRepository.segment(name)}"
    val resource = rememberWorkResource(path)
    val state by resource.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    var restore by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(state.identity) { restore = null }
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { WorkResourceStatus(state, resource); WorkLink("Refresh program", resource::refresh) }
        state.value?.let { document ->
            item { WorkCard {
                Text(document.workText("title") ?: name, color = Ink, fontSize = 20.sp, fontWeight = FontWeight.SemiBold)
                SelectionContainer { Text(document.workText("content").orEmpty(), color = Ink, fontSize = 15.sp) }
            } }
            item { WorkProgramMembers(name, push) }
            item { WorkCard { WorkRelatedTools(WorkWebContext(section = WorkSection.Programs, program = name), listOf(WorkSpecialist.History, WorkSpecialist.Memory, WorkSpecialist.Evals, WorkSpecialist.Events)) } }
            item { Text("History", color = Ink, fontSize = 17.sp, fontWeight = FontWeight.SemiBold) }
            items(document.workRows("history")) { version ->
                version.workText("snapshot")?.let { snapshot -> WorkCard {
                    Text(version.workText("modified") ?: snapshot, color = Muted, fontSize = 13.sp)
                    Text(snapshot, color = Muted, fontSize = 12.sp)
                    TextButton(onClick = { restore = snapshot }, enabled = !state.busy) { Text("Restore version", color = Coral) }
                } }
            }
        }
    }
    restore?.let { snapshot -> AlertDialog(
        onDismissRequest = { restore = null }, title = { Text("Restore program version?", color = Ink) },
        text = { Text("Restore $snapshot through the existing program provider.", color = Muted) },
        confirmButton = { TextButton(onClick = {
            val identity = state.identity; restore = null
            scope.launch { if (resource.perform("$path/revert", identity, body = buildJsonObject { put("snapshot", snapshot) }) != null) resource.refresh() }
        }) { Text("Restore", color = Coral) } },
        dismissButton = { TextButton(onClick = { restore = null }) { Text("Keep current", color = Muted) } }, containerColor = Ground,
    ) }
}

@Composable
internal fun WorkEndpoint(path: String) {
    val resource = rememberWorkResource(path)
    val state by resource.state.collectAsStateWithLifecycle()
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { WorkResourceStatus(state, resource); WorkLink("Refresh", resource::refresh) }
        state.value?.let { value -> item { WorkCard { WorkRecord(value) } } }
    }
}

@Composable
internal fun WorkProgramMembers(program: String, push: (WorkRoute) -> Unit) {
    val crew = rememberWorkResource("/api/magician/v2/agents")
    val state by crew.state.collectAsStateWithLifecycle()
    val members = state.value.workRows("agents").filter { it.workField("definition").workText("kind") == "Personal" && it.workField("definition").workField("harness") is JsonObject }
    Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text(if (program.isBlank()) "Runtime direction and progress" else "Responsible crew and progress", color = Ink, fontSize = 18.sp)
        WorkResourceStatus(state, crew)
        if (state.value != null && members.isEmpty()) Text("No program-enabled crew in this workspace.", color = Muted, fontSize = 13.sp)
        members.forEach { member -> key(member.workField("definition").workText("agent_id")) { WorkProgramProgress(member, program, push) } }
    }
}

@Composable
private fun WorkProgramProgress(record: JsonElement, program: String, push: (WorkRoute) -> Unit) {
    val definition = record.workField("definition")
    val agentId = definition.workText("agent_id").orEmpty()
    val overview = rememberWorkResource("/api/magician/v2/agents/${WorkRepository.segment(agentId)}/harness-overview")
    val state by overview.state.collectAsStateWithLifecycle()
    val openTask = LocalOpenTaskDetail.current
    val references = listOf(buildJsonObject { put("path", "program.md"); put("section", definition.workField("harness").workField("program_section") ?: JsonNull) }) +
        definition.workField("autonomous_config").workRows("focus_areas").map { area -> buildJsonObject {
            val path = area.workText("program").orEmpty().trim()
            put("path", path.ifBlank { "program.md" }); put("focus", area.workField("name") ?: JsonNull)
            put("section", if (path.isBlank()) definition.workField("harness").workField("program_section") ?: JsonNull else JsonNull)
        } }
    val active = state.value.workField("evaluation").workField("active_program_state")
    val associated = program.isBlank() || references.any { it.workText("path") == program } || state.value.workField("context").workField("program").workText("relative_path") == program
    val matching = program.isBlank() || active.workField("program").workText("relative_path") == program
    if (associated || state.loading || state.error != null) WorkCard {
        WorkLink(definition.workText("name") ?: agentId) { push(WorkRoute("member", agentId, "Crew member")) }
        Text("Definition version: ${record.workField("version").workDisplay()}", color = Muted, fontSize = 12.sp)
        references.filter { program.isBlank() || it.workText("path") == program }.forEach { reference ->
            Text(reference.workText("path").orEmpty() + (reference.workText("section")?.let { " · $it" } ?: "") + (reference.workText("focus")?.let { " · focus: $it" } ?: ""), color = Ink, fontSize = 13.sp)
        }
        WorkResourceStatus(state, overview)
        if (state.value != null) {
            if (matching && active is JsonObject && active.isNotEmpty()) {
                Text("Current source: ${active.workField("program").workField("relative_path").workDisplay()}", color = Muted, fontSize = 13.sp)
                listOf("current_phase", "current_step", "updated_at", "update_reason").forEach { key -> active.workField(key)?.takeIf { it != JsonNull }?.let { Text("${workLabel(key)}: ${it.workDisplay()}", color = Ink, fontSize = 14.sp) } }
                if (active.workRows("open_loops").isNotEmpty()) { Text("Open loops", color = Muted); WorkRecord(active.workField("open_loops")) }
                active.workField("last_run_summary")?.takeIf { it != JsonNull }?.let { Text("Latest cycle summary", color = Muted); WorkRecord(it) }
                active.workRows("update_history").takeLast(5).reversed().forEach { update ->
                    Text(update.workField("reason").workDisplay(), color = Ink, fontSize = 13.sp)
                    update.workText("task_id")?.let { id -> TextButton(onClick = { openTask?.open(id, update.workText("execution_id"), false) }, enabled = openTask != null, modifier = Modifier.heightIn(min = 48.dp)) { Text("Task and execution evidence", color = Coral) } }
                }
            } else Text("No current runtime progress for this document.", color = Muted, fontSize = 13.sp)
            val metrics = state.value.workField("evaluation").workField("program_metrics").workRows("documents").filter { program.isBlank() || it.workText("relative_path") == program }
            if (metrics.isNotEmpty()) { Text("Evaluation metrics", color = Ink); WorkRecord(JsonArray(metrics)) }
        }
        WorkLink("Focus health and operating controls") { push(WorkRoute("operator", agentId, "Operator view")) }
    }
}
