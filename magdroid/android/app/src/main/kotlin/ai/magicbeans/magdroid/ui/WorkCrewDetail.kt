package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.*
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Star
import androidx.compose.material.icons.outlined.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.Alignment
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.DialogWindowProvider
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
    WorkPage {
        workStatusItem(state, resource)
        state.value?.let { value ->
            item { WorkHeaderCard(value.workText("title") ?: "Bootstrap task", subtitle = id,
                chips = listOfNotNull(value.workText("status")?.let { WorkChipSpec(workLabel(it), workStatusTone(it)) })) {
                WorkRow("Open task and execution details", icon = Icons.Outlined.ArrowCircleRight, enabled = openTask != null) { openTask?.open(id) }
            } }
            item { WorkGroup("Task record") { WorkRecord(value) } }
        }
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
    var definitionOpen by remember { mutableStateOf(false) }
    WorkPage {
        workStatusItem(state, resource)
        member?.let { crew ->
            val personal = crew.kind.equals("Personal", ignoreCase = true)
            val status = if (crew.disabled) "Disabled" else crew.status
            item { WorkHeaderCard(crew.name, detail = crew.description, chips = listOfNotNull(
                crew.kind.takeIf(String::isNotBlank)?.let { WorkChipSpec(workLabel(it.lowercase()), Muted) },
                status.takeIf(String::isNotBlank)?.let { WorkChipSpec(it, workStatusColor(crew.status, crew.disabled)) },
                WorkChipSpec("Primary", Coral, Icons.Filled.Star).takeIf { crew.isPrimary },
            )) {
                if (personal) {
                    WorkActionRow("Run once", Icons.Outlined.PlayArrow, enabled = !state.busy) { action = WorkRepository.AgentAction.Trigger }
                    WorkRowDivider(); WorkActionRow("Pause", Icons.Outlined.Pause, enabled = !state.busy) { action = WorkRepository.AgentAction.Pause }
                    WorkRowDivider(); WorkActionRow("Resume", Icons.Outlined.PlayCircleOutline, enabled = !state.busy) { action = WorkRepository.AgentAction.Resume }
                    if (!crew.isPrimary) { WorkRowDivider(); WorkActionRow("Make primary", Icons.Outlined.StarOutline) { action = WorkRepository.AgentAction.SetPrimary } }
                }
                if (personal) WorkRowDivider()
                WorkActionRow("Refresh member", Icons.Outlined.Refresh, enabled = !state.busy, onClick = resource::refresh)
            } }
            // One item for every group below the header: a short, fixed list that
            // composes (and is found by text) as a whole, like the single card it replaced.
            item { Column(verticalArrangement = Arrangement.spacedBy(WORK_GROUP_SPACING_DP.dp)) {
                WorkGroup("Health and runtime") {
                    crew.cycleTaskId?.let { id ->
                        WorkRow("Inspect current cycle", Modifier.testTag("work-crew-current-cycle"), icon = Icons.Outlined.Autorenew, enabled = openTask != null) { openTask?.open(id, crew.executionId, false) }
                    } ?: WorkRow("No current cycle execution.", icon = Icons.Outlined.Autorenew, labelColor = Muted, iconTint = Muted, accessory = WorkRowAccessory.None)
                    WorkRowDivider(); WorkRow("Member health", icon = Icons.Outlined.Troubleshoot) { push(WorkRoute("endpoint", "$path/health", "Member health")) }
                    WorkRowDivider(); WorkRow("Operator view", icon = Icons.Outlined.Tune) { push(WorkRoute("operator", agentId, "Operator view")) }
                    if (personal) { WorkRowDivider(); WorkRow("Bootstrap task", icon = Icons.Outlined.AddTask) { push(WorkRoute("member-bootstrap", agentId, "Bootstrap")) } }
                }
                WorkGroup("Memory") {
                    WorkRow("Memory tiers", icon = Icons.Outlined.AccountTree) { push(WorkRoute("member-memory", agentId, "Memory tiers")) }
                    WorkRowDivider(); WorkRow("Memory health", icon = Icons.Outlined.Psychology) { push(WorkRoute("endpoint", "$path/memory/consolidation-health", "Memory health")) }
                    WorkRowDivider(); WorkRow("User memory and sharing", icon = Icons.Outlined.Groups) { push(WorkRoute("user-memory", agentId, "User memory")) }
                }
                WorkGroup("Rules and tools") {
                    WorkRow("Rules and policies", icon = Icons.Outlined.Checklist) { push(WorkRoute("member-rules", agentId, "Rules and policies")) }
                    WorkRowDivider(); WorkRow("Effective tools", icon = Icons.Outlined.Handyman) { push(WorkRoute("endpoint", "$path/effective-tools", "Effective tools")) }
                    WorkRowDivider(); WorkRow("Runtime tool cache", icon = Icons.Outlined.Inventory2) { push(WorkRoute("endpoint", "$path/runtime-context-cache", "Runtime tool cache")) }
                }
                WorkGroup("Playbooks and quality") {
                    WorkRow("Playbooks", icon = Icons.Outlined.Description) { push(WorkRoute("Programs", title = "Playbooks")) }
                    WorkRowDivider(); WorkRow("Quality checks", icon = Icons.Outlined.TaskAlt) { push(WorkRoute("Quality", title = "Quality checks", agentId = agentId)) }
                }
                WorkGroup("Definition") {
                    WorkRow("Definition, rules and model pins", icon = Icons.Outlined.Code, accessory = if (definitionOpen) WorkRowAccessory.Expanded else WorkRowAccessory.Collapsed) { definitionOpen = !definitionOpen }
                    if (definitionOpen) Column(Modifier.fillMaxWidth().padding(bottom = 12.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) { WorkRecord(crew.definition) }
                    WorkRowDivider()
                    WorkActionRow("Edit definition", Icons.Outlined.Edit) {
                        draft = crew.definition; guided = true
                        editor = Json { prettyPrint = true }.encodeToString(JsonObject.serializer(), crew.definition); editingETag = crew.etag; editing = true
                    }
                }
                WorkGroup("Web console") { WorkRelatedTools(WorkWebContext(section = WorkSection.Crew, agent = agentId), liveSurface = true) }
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
    if (editing && member != null) Dialog(
        onDismissRequest = { if (!state.busy) editing = false },
        // Edge to edge, so the keyboard resizes the editor (imePadding) instead
        // of panning the whole window and its Cancel/Save bar off screen.
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
    ) {
        val window = (LocalView.current.parent as? DialogWindowProvider)?.window
        // The dialog has its own input mode; an activity's adjustResize does
        // not prevent this window from panning its action bar off screen.
        SideEffect { window?.setSoftInputMode(android.view.WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE) }
        Column(Modifier.fillMaxSize().background(Ground).systemBarsPadding().imePadding()) {
            Row(Modifier.fillMaxWidth().heightIn(min = TASK_HEADER_HEIGHT_DP.dp).padding(horizontal = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                TextButton(onClick = { editing = false }, enabled = !state.busy, modifier = Modifier.heightIn(min = 48.dp)) { Text("Cancel", color = Coral, fontSize = 16.sp) }
                Text("Edit definition", color = Ink, fontSize = TASK_HEADER_TITLE_FONT_SP.sp, fontWeight = FontWeight.SemiBold, textAlign = TextAlign.Center, maxLines = 2, modifier = Modifier.weight(1f))
                TextButton(onClick = {
                    val patch = if (guided) draft else runCatching { Json.parseToJsonElement(editor) as JsonObject }.getOrNull()
                    if (patch == null) resource.reportError("Enter a valid definition object before saving.")
                    else if (patch.crewValidation(agentId) != null) resource.reportError(patch.crewValidation(agentId)!!)
                    else if ((patch.text("agent_id") ?: patch.text("id")) != agentId) resource.reportError("Keep the crew member identity unchanged.")
                    else {
                        val identity = state.identity
                        scope.launch { if (resource.perform(path, identity, WorkMethod.Patch, patch.crewMergePatch(member.definition), editingETag) != null) { editing = false; resource.refresh() } }
                    }
                }, enabled = !state.busy && editingETag.isNotBlank(), modifier = Modifier.heightIn(min = 48.dp)) { Text("Save", color = Coral, fontSize = 16.sp, fontWeight = FontWeight.SemiBold) }
            }
            LazyColumn(Modifier.fillMaxWidth().weight(1f).testTag("work-crew-editor-scroll"),
                contentPadding = PaddingValues(start = WORK_SCREEN_PADDING_DP.dp, end = WORK_SCREEN_PADDING_DP.dp, top = 8.dp, bottom = 24.dp),
                verticalArrangement = Arrangement.spacedBy(WORK_GROUP_SPACING_DP.dp)) {
                item { WorkSegmented(listOf("guided" to "Guided", "json" to "Advanced JSON"), if (guided) "guided" else "json", { choice ->
                    if (choice == "guided" && !guided) {
                        val parsed = runCatching { Json.parseToJsonElement(editor) as JsonObject }.getOrNull()
                        if (parsed == null) resource.reportError("Enter a valid definition before returning to guided editing.") else { draft = parsed; guided = true }
                    } else if (choice == "json" && guided) { editor = Json { prettyPrint = true }.encodeToString(JsonObject.serializer(), draft); guided = false }
                }) }
                workStatusItem(state, resource)
                if (guided) item { WorkCrewFields(draft, { draft = it }, agentId) }
                if (!guided) item { WorkGroup("Definition") { WorkFieldStack {
                    WorkLabeledField("Definition JSON", editor, { editor = it }, fieldModifier = Modifier.testTag("work-crew-definition-editor"), minLines = 8, maxLines = 20, mono = true)
                } } }
                item { WorkFootnote("All fields are retained. Save checks the version you opened; concurrent edits require a refresh. Set a field to null to remove it.") }
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
    DisposableEffect(Unit) { onDispose { sequenceTicket++ } }
    var change by remember { mutableStateOf<Boolean?>(null) }
    LaunchedEffect(state.identity) { change = null; confirmSequence = false; sequenceTicket++; sequenceRunning = false; sequenceResults.clear() }
    WorkPage {
        workStatusItem(state, resource)
        state.value?.let { runtime -> item {
            val running = runtime.workBool("enabled")
            WorkHeaderCard("Team routines schedule", detail = "Controls the shared team routines. Individual crew and task controls remain separate.",
                chips = listOf(WorkChipSpec(if (running) "Running" else "Paused", if (running) activePalette.success else MWarn, if (running) Icons.Filled.PlayArrow else Icons.Filled.Pause))) {
                WorkActionRow(if (running) "Pause team routines" else "Resume team routines", if (running) Icons.Outlined.PauseCircle else Icons.Outlined.PlayCircleOutline, enabled = !state.busy) { change = !running }
                WorkRowDivider()
                WorkRecordRow("Runtime status", runtime, icon = Icons.Outlined.Info)
            }
        } }
        item { Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            WorkGroup("Run team routines now") {
                WorkActionRow(if (sequenceRunning) "Dispatching…" else "Start team sequence", Icons.Outlined.FastForward, enabled = !sequenceRunning && !state.busy && state.value != null) { confirmSequence = true }
                sequenceResults.forEach { WorkRowDivider(); WorkRecord(it) }
            }
            WorkFootnote("Dispatch each crew member's team routines (their focus areas) in order. This does not wait for their runs to finish.")
        } }
        item { WorkGroup("Inspect") {
            WorkRow("Anomalies and repair evidence", icon = Icons.Outlined.ReportProblem) { push(WorkRoute("endpoint", "/api/magician/v2/harness/anomalies", "Anomalies")) }
            WorkRowDivider(); WorkRow("Crew and individual cycles", icon = Icons.Outlined.Groups) { push(WorkRoute("Crew", title = "Crew")) }
            WorkRowDivider(); WorkRow("Playbooks", icon = Icons.Outlined.Description) { push(WorkRoute("Programs", title = "Playbooks")) }
        } }
        item { WorkGroup("Web console") { WorkRelatedTools(WorkWebContext(section = WorkSection.Operations)) } }
    }
    if (confirmSequence) AlertDialog(
        onDismissRequest = { confirmSequence = false }, title = { Text("Dispatch the team routines?", color = Ink) },
        confirmButton = { TextButton(onClick = {
            confirmSequence = false; sequenceRunning = true; sequenceResults.clear(); val ticket = ++sequenceTicket; val identity = state.identity
            scope.launch {
                // The live roster, read once: a routine retired since this screen opened must not run.
                val agents = resource.perform("/api/magician/v2/agents", identity, WorkMethod.Get, body = null, successNotice = null)
                val targets = teamRoutineTargets(agents)
                if (agents != null && targets.isEmpty() && sequenceTicket == ticket) {
                    resource.reportError("No team routines are configured: no harness crew member has a focus area.")
                }
                for (target in targets) {
                    if (sequenceTicket != ticket || resource.state.value.identity != identity) break
                    val result = resource.perform("/api/magician/v2/agents/${target.agentId}/trigger", identity, body = buildJsonObject { put("goal_id", target.goalId); put("trigger", "manual.harness.company_loop") })
                    if (sequenceTicket != ticket || resource.state.value.identity != identity) break
                    sequenceResults.add(buildJsonObject { put("target", "${target.agentId} · ${target.label}"); put("result", result ?: buildJsonObject { put("status", "failed"); put("reason", resource.state.value.error ?: "Dispatch failed") }) })
                }
                if (sequenceTicket == ticket) sequenceRunning = false
            }
        }) { Text("Dispatch reviewed sequence", color = Coral) } },
        dismissButton = { TextButton(onClick = { confirmSequence = false }) { Text("Cancel", color = Muted) } }, containerColor = Ground,
    )
    change?.let { enabled -> AlertDialog(
        onDismissRequest = { change = null }, title = { Text("Change the team routines schedule?", color = Ink) },
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
