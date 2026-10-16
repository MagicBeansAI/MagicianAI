package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.*
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

@Composable
internal fun WorkCrewBootstrap(agentId: String) {
    val id = "bootstrap__$agentId"
    val resource = rememberWorkResource("/api/magician/v3/tasks/${WorkRepository.segment(id)}")
    val state by resource.state.collectAsStateWithLifecycle()
    val openTask = LocalOpenTaskDetail.current
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { WorkResourceStatus(state, resource); WorkLink("Refresh bootstrap", resource::refresh) }
        state.value?.let { value -> item { WorkCard {
            Text("Bootstrap task", color = Ink, fontSize = 18.sp)
            WorkRecord(value)
            TextButton(onClick = { openTask?.open(id) }, enabled = openTask != null, modifier = Modifier.heightIn(min = 48.dp)) { Text("Open task and execution details", color = Coral) }
        } } }
    }
}

@Composable
@OptIn(ExperimentalLayoutApi::class)
internal fun WorkCrewDetail(agentId: String, push: (WorkRoute) -> Unit, openEditor: Boolean = false) {
    val path = "/api/magician/v2/agents/${WorkRepository.segment(agentId)}"
    val resource = rememberWorkResource(path)
    val state by resource.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    val openTask = LocalOpenTaskDetail.current
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    LaunchedEffect(resource, lifecycle) { lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) { while (true) { delay(30_000); resource.refresh() } } }
    val member = (state.value as? JsonObject)?.let { runCatching { WorkCrewMember.decode(it) }.getOrNull() }
    var action by remember { mutableStateOf<WorkRepository.AgentAction?>(null) }
    var editing by remember { mutableStateOf(false) }
    var editingETag by remember { mutableStateOf("") }
    var editor by remember { mutableStateOf("") }
    var draft by remember { mutableStateOf(newCrewDefinition()) }
    var guided by remember { mutableStateOf(true) }
    var autoOpened by remember { mutableStateOf(false) }
    LaunchedEffect(member, openEditor) { if (openEditor && !autoOpened && member != null) { draft = member.definition; editor = Json { prettyPrint = true }.encodeToString(JsonObject.serializer(), draft); editingETag = member.etag; guided = true; editing = true; autoOpened = true } }
    LaunchedEffect(state.identity) { action = null; editing = false; editor = ""; editingETag = "" }
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { WorkResourceStatus(state, resource); WorkLink("Refresh member", resource::refresh) }
        member?.let { crew ->
            item { WorkCard {
                Text(crew.name, color = Ink, fontSize = 22.sp)
                Text(crew.kind + if (crew.isPrimary) " · Primary" else "", color = Coral, fontSize = 13.sp)
                if (crew.description.isNotBlank()) Text(crew.description, color = Ink, fontSize = 15.sp)
                Text(if (crew.disabled) "Disabled" else crew.status, color = Muted, fontSize = 12.sp)
                if (crew.kind.equals("Personal", ignoreCase = true)) FlowRow(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    TextButton(onClick = { action = WorkRepository.AgentAction.Pause }, enabled = !state.busy) { Text("Pause", color = Coral) }
                    TextButton(onClick = { action = WorkRepository.AgentAction.Resume }, enabled = !state.busy) { Text("Resume", color = Coral) }
                    TextButton(onClick = { action = WorkRepository.AgentAction.Trigger }, enabled = !state.busy) { Text("Run once", color = Coral) }
                }
                if (crew.kind.equals("Personal", ignoreCase = true) && !crew.isPrimary) WorkLink("Make primary") { action = WorkRepository.AgentAction.SetPrimary }
            } }
            item { WorkCard {
                Text("Details and controls", color = Ink, fontSize = 18.sp)
                crew.cycleTaskId?.let { id ->
                    TextButton(onClick = { openTask?.open(id, crew.executionId, false) }, enabled = openTask != null, modifier = Modifier.heightIn(min = 48.dp).testTag("work-crew-current-cycle")) { Text("Inspect current cycle", color = Coral) }
                } ?: Text("No current cycle execution.", color = Muted, fontSize = 13.sp)
                WorkLink("Member health") { push(WorkRoute("endpoint", "$path/health", "Member health")) }
                if (crew.kind.equals("Personal", ignoreCase = true)) WorkLink("Bootstrap task") { push(WorkRoute("member-bootstrap", agentId, "Bootstrap")) }
                WorkLink("Operator view") { push(WorkRoute("operator", agentId, "Operator view")) }
                listOf("Effective tools" to "/effective-tools",
                    "Runtime tool cache" to "/runtime-context-cache",
                    "Memory health" to "/memory/consolidation-health").forEach { (title, suffix) ->
                    WorkLink(title) { push(WorkRoute("endpoint", path + suffix, title)) }
                }
                WorkLink("Memory tiers") { push(WorkRoute("member-memory", agentId, "Memory tiers")) }
                WorkLink("Rules and policies") { push(WorkRoute("member-rules", agentId, "Rules and policies")) }
                WorkLink("User memory and sharing") { push(WorkRoute("user-memory", agentId, "User memory")) }
                WorkLink("Programs") { push(WorkRoute("Programs", title = "Programs")) }
                WorkLink("Quality and review") { push(WorkRoute("Quality", title = "Quality and review", agentId = agentId)) }
                WorkRelatedTools(WorkWebContext(section = WorkSection.Crew, agent = agentId), liveSurface = true)
            } }
            item { WorkCard {
                Text("Definition, rules and model pins", color = Ink, fontSize = 17.sp)
                WorkLink("Edit definition") {
                    draft = crew.definition; guided = true
                    editor = Json { prettyPrint = true }.encodeToString(JsonObject.serializer(), crew.definition); editingETag = crew.etag; editing = true
                }
                WorkRecord(crew.definition)
            } }
        }
    }
    action?.let { selected -> AlertDialog(
        onDismissRequest = { action = null }, title = { Text("Change this crew member?", color = Ink) },
        text = { Text("${selected.name} applies to ${member?.name ?: agentId} in the current connection.", color = Muted) },
        confirmButton = { TextButton(onClick = {
            val identity = state.identity; action = null
            scope.launch {
                if (resource.perform("$path/${selected.path}", identity,
                    body = if (selected == WorkRepository.AgentAction.Trigger) buildJsonObject {} else null) != null) resource.refresh()
            }
        }, enabled = !state.busy) { Text("Confirm", color = Coral) } },
        dismissButton = { TextButton(onClick = { action = null }) { Text("Cancel", color = Muted) } }, containerColor = Ground,
    ) }
    if (editing && member != null) Dialog(onDismissRequest = { if (!state.busy) editing = false }, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Column(Modifier.fillMaxSize().background(Ground).systemBarsPadding().imePadding().padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Row {
                TextButton(onClick = { editing = false }, enabled = !state.busy) { Text("Cancel", color = Muted) }
                Spacer(Modifier.weight(1f))
                TextButton(onClick = {
                    val patch = if (guided) draft else runCatching { Json.parseToJsonElement(editor) as JsonObject }.getOrNull()
                    if (patch == null) resource.reportError("Enter a valid definition object before saving.")
                    else if (patch.crewValidation(agentId) != null) resource.reportError(patch.crewValidation(agentId)!!)
                    else if ((patch.text("agent_id") ?: patch.text("id")) != agentId) resource.reportError("Keep the crew member identity unchanged.")
                    else {
                        val identity = state.identity
                        scope.launch { if (resource.perform(path, identity, WorkMethod.Patch, patch.crewMergePatch(member.definition), editingETag) != null) { editing = false; resource.refresh() } }
                    }
                }, enabled = !state.busy && editingETag.isNotBlank()) { Text("Save", color = Coral) }
            }
            LazyColumn(Modifier.fillMaxWidth().weight(1f).testTag("work-crew-editor-scroll"), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                item { FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    FilterChip(selected = guided, onClick = {
                        val parsed = if (guided) draft else runCatching { Json.parseToJsonElement(editor) as JsonObject }.getOrNull()
                        if (parsed == null) resource.reportError("Enter a valid definition before returning to guided editing.") else { draft = parsed; guided = true }
                    }, label = { Text("Guided") }, modifier = Modifier.heightIn(min = 48.dp))
                    FilterChip(selected = !guided, onClick = { if (guided) { editor = Json { prettyPrint = true }.encodeToString(JsonObject.serializer(), draft); guided = false } }, label = { Text("Advanced JSON") }, modifier = Modifier.heightIn(min = 48.dp))
                } }
                if (guided) item { WorkCrewFields(draft, { draft = it }, agentId) }
                item { Text("All fields are retained. Save checks the version you opened; concurrent edits require a refresh. Set a field to null to remove it.", color = Muted, fontSize = 13.sp) }
                if (!guided) item { OutlinedTextField(editor, { editor = it }, modifier = Modifier.fillMaxWidth().testTag("work-crew-definition-editor"), minLines = 8, maxLines = 20) }
                item { WorkResourceStatus(state, resource) }
            }
        }
    }
}

@Composable
internal fun WorkOperations(push: (WorkRoute) -> Unit) {
    val path = "/api/magician/v2/harness/runtime"
    val resource = rememberWorkResource(path)
    val state by resource.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    var confirmSequence by remember { mutableStateOf(false) }
    var sequenceRunning by remember { mutableStateOf(false) }
    var sequenceTicket by remember { mutableIntStateOf(0) }
    val sequenceResults = remember { mutableStateListOf<JsonElement>() }
    val targets = listOf("cmo" to "content-calendar", "cro" to "weekly-gtm-review", "cpo" to "backlog-grooming", "cto" to "morning-engineering-standup", "ceo" to "morning-briefing")
    DisposableEffect(Unit) { onDispose { sequenceTicket++ } }
    var change by remember { mutableStateOf<Boolean?>(null) }
    LaunchedEffect(state.identity) { change = null; confirmSequence = false; sequenceTicket++; sequenceRunning = false; sequenceResults.clear() }
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { WorkResourceStatus(state, resource); WorkLink("Refresh runtime", resource::refresh) }
        state.value?.let { runtime -> item { WorkCard {
            Text("Company loop cadence", color = Ink, fontSize = 19.sp)
            Text(if (runtime.workBool("enabled")) "Running" else "Paused", color = Ink, fontSize = 16.sp)
            Text("Controls the shared company loop. Individual crew and task controls remain separate.", color = Muted, fontSize = 13.sp)
            WorkLink(if (runtime.workBool("enabled")) "Pause company loop" else "Resume company loop") { change = !runtime.workBool("enabled") }
            WorkRecord(runtime)
        } } }
        item { WorkCard {
            Text("Manual company sequence", color = Ink, fontSize = 18.sp)
            Text("Dispatch the existing CMO, CRO, CPO, CTO and CEO focus areas in order. This does not wait for their runs to finish.", color = Muted, fontSize = 13.sp)
            TextButton(onClick = { confirmSequence = true }, enabled = !sequenceRunning && !state.busy && state.value != null) { Text(if (sequenceRunning) "Dispatching…" else "Run company sequence", color = Coral) }
            sequenceResults.forEach { WorkRecord(it) }
        } }
        item { WorkCard {
            WorkLink("Anomalies and repair evidence") { push(WorkRoute("endpoint", "/api/magician/v2/harness/anomalies", "Anomalies")) }
            WorkLink("Crew and individual cycles") { push(WorkRoute("Crew", title = "Crew")) }
            WorkLink("Programs") { push(WorkRoute("Programs", title = "Programs")) }
            WorkRelatedTools(WorkWebContext(section = WorkSection.Operations))
        } }
    }
    if (confirmSequence) AlertDialog(
        onDismissRequest = { confirmSequence = false }, title = { Text("Dispatch the five company focus areas?", color = Ink) },
        confirmButton = { TextButton(onClick = {
            confirmSequence = false; sequenceRunning = true; sequenceResults.clear(); val ticket = ++sequenceTicket; val identity = state.identity
            scope.launch {
                for ((agent, goal) in targets) {
                    if (sequenceTicket != ticket || resource.state.value.identity != identity) break
                    val result = resource.perform("/api/magician/v2/agents/$agent/trigger", identity, body = buildJsonObject { put("goal_id", "harness:$agent:$goal"); put("trigger", "manual.harness.company_loop") })
                    if (sequenceTicket != ticket || resource.state.value.identity != identity) break
                    sequenceResults.add(buildJsonObject { put("target", agent); put("result", result ?: buildJsonObject { put("status", "failed"); put("reason", resource.state.value.error ?: "Dispatch failed") }) })
                }
                if (sequenceTicket == ticket) sequenceRunning = false
            }
        }) { Text("Dispatch reviewed sequence", color = Coral) } },
        dismissButton = { TextButton(onClick = { confirmSequence = false }) { Text("Cancel", color = Muted) } }, containerColor = Ground,
    )
    change?.let { enabled -> AlertDialog(
        onDismissRequest = { change = null }, title = { Text("Change company loop cadence?", color = Ink) },
        text = { Text("Apply the existing loop policy on the current connection.", color = Muted) },
        confirmButton = { TextButton(onClick = {
            val identity = state.identity; change = null
            scope.launch {
                val result = resource.perform(path, identity, WorkMethod.Put, buildJsonObject { put("enabled", enabled) })
                if (result != null) {
                    resource.refresh()
                    val warnings = result.workRows("warnings")
                    if (warnings.isNotEmpty()) resource.reportError(warnings.joinToString("; ") { it.workDisplay() })
                }
            }
        }, enabled = !state.busy) { Text(if (enabled) "Resume" else "Pause", color = Coral) } },
        dismissButton = { TextButton(onClick = { change = null }) { Text("Cancel", color = Muted) } }, containerColor = Ground,
    ) }
}
