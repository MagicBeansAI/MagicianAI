package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.*
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*
import java.util.UUID

@Composable
internal fun WorkQualityCompose(target: String, onOpenRun: (String) -> Unit, agentId: String = "") {
    val resource = rememberWorkResource("$WORK_QUALITY_API/profiles")
    val state by resource.state.collectAsStateWithLifecycle()
    val proposedResource = if (target.startsWith("proposal:")) rememberWorkResource("$WORK_QUALITY_API/proposals/${WorkRepository.segment(target.removePrefix("proposal:"))}") else null
    val proposalState = proposedResource?.state?.collectAsStateWithLifecycle()?.value
    val proposed = proposalState?.value
    var useSourceTask by remember { mutableStateOf(!target.startsWith("proposal:") && target.isNotBlank()) }
    val sourceTaskId = if (target.startsWith("proposal:")) proposed.workField("origin").workText("task_id").orEmpty() else target
    val taskId = if (useSourceTask) sourceTaskId else ""
    val contextResource = if (sourceTaskId.isNotBlank()) rememberWorkResource("$WORK_QUALITY_API/task-context/${WorkRepository.segment(sourceTaskId)}") else null
    val contextState = contextResource?.state?.collectAsStateWithLifecycle()?.value
    val scope = rememberCoroutineScope()
    var draft by remember { mutableStateOf(WorkQualityDraft()) }
    val crew = rememberWorkResource("/api/magician/v2/agents")
    val crewState by crew.state.collectAsStateWithLifecycle()
    var referencesPending by remember { mutableStateOf(false) }
    var agent by remember { mutableStateOf(agentId) }
    var thread by remember { mutableStateOf("general") }
    var showCaptureTarget by remember { mutableStateOf(false) }
    var captureTarget by remember { mutableStateOf("") }
    var advanced by remember { mutableStateOf("") }
    var preview by remember { mutableStateOf<JsonElement?>(null) }
    var reviewedSignature by remember { mutableStateOf("") }
    var policyId by remember { mutableStateOf(UUID.randomUUID().toString()) }
    var policyRuns by remember { mutableIntStateOf(1) }
    var policyExpiry by remember { mutableStateOf(java.time.Instant.now().plusSeconds(7 * 86400).toString()) }
    var runId by remember { mutableStateOf(UUID.randomUUID().toString()) }
    val proposal = when {
        proposed != null -> proposed.workField("proposal") as? JsonObject
        advanced.isNotBlank() -> runCatching { Json.parseToJsonElement(advanced) as JsonObject }.getOrNull()
        else -> draft.proposal().let { if (captureTarget.isNotBlank()) JsonObject(it + ("handoff_kind" to JsonPrimitive("capture"))) else it }
    }
    val signature = "$proposal|$agent|$thread|${contextState?.value}|$captureTarget|$useSourceTask"
    val currentSignature by rememberUpdatedState(signature)
    val profiles = state.value.workRows("profiles")
    val canAttachProposal = proposed == null || (proposed.workField("proposal").workField("native_harness") is JsonObject &&
        contextState?.value.workField("goal") == proposed.workField("proposal").workField("goal") && contextState?.value.workField("agent_id") == proposed.workField("origin").workField("agent_id"))
    val canReview = !referencesPending && proposal != null && agent.isNotBlank() && thread.isNotBlank() &&
        (!target.startsWith("proposal:") || proposed != null) &&
        (taskId.isBlank() || (contextState?.value.workText("task_id") == taskId && contextState?.value.workBool("can_start") && canAttachProposal)) &&
        (proposed != null || advanced.isNotBlank() || (draft.goal.isNotBlank() && draft.rubric.isNotBlank() && draft.actor.isNotBlank() && draft.critic.isNotBlank()))
    LaunchedEffect(state.value) {
        if (draft.actor.isBlank()) draft = draft.copy(actor = state.value.workText("actor_default").orEmpty())
        if (draft.critic.isBlank()) draft = draft.copy(critic = state.value.workText("critic_default").orEmpty())
    }
    LaunchedEffect(contextState?.value, useSourceTask) {
        if (useSourceTask) {
            contextState?.value.workText("agent_id")?.let { agent = it }
            contextState?.value.workText("ui_thread_id")?.let { thread = it }
        }
        val inheritedCapture = contextState?.value.workField("policy").workField("capture_handoff")?.takeIf { it != JsonNull }
        if (useSourceTask && captureTarget.isBlank()) inheritedCapture?.let { captureTarget = workPretty.encodeToString(JsonElement.serializer(), it) }
        if (!useSourceTask && inheritedCapture != null && captureTarget == workPretty.encodeToString(JsonElement.serializer(), inheritedCapture)) captureTarget = ""
        if (draft.goal.isBlank()) draft = draft.copy(goal = contextState?.value.workText("goal").orEmpty())
    }
    fun configurePolicy(enabled: Boolean) {
        val contextual = contextState ?: return; val client = contextResource ?: return
        val contextHash = contextual.value.workText("context_hash") ?: return
        if (enabled && (preview == null || signature != reviewedSignature || proposal == null)) return
        if (enabled && captureTarget.isNotBlank() && runCatching { Json.parseToJsonElement(captureTarget) as JsonObject }.getOrNull() == null) { resource.reportError("Enter a valid capture target object."); return }
        val expiry = if (enabled) runCatching { java.time.Instant.parse(policyExpiry).toString() }.getOrNull() else java.time.Instant.now().toString()
        if (expiry == null) { resource.reportError("Enter an expiry in UTC, such as 2026-10-13T12:00:00Z."); return }
        val body = buildJsonObject {
            put("policy_id", policyId); put("expected_policy_id", contextual.value.workField("policy").workField("id") ?: JsonNull)
            put("expected_context_hash", contextHash); put("expected_contract_hash", preview.workText("contract_hash").orEmpty())
            put("proposal", if (enabled) proposal ?: JsonNull else JsonNull); put("max_runs", if (enabled) policyRuns else 0)
            put("expires_at", expiry); put("capture_handoff", if (enabled && captureTarget.isNotBlank()) runCatching { Json.parseToJsonElement(captureTarget) as JsonObject }.getOrNull() ?: JsonNull else JsonNull)
        }
        scope.launch { if (client.perform("$WORK_QUALITY_API/task-policy/${WorkRepository.segment(taskId)}", contextual.identity, body = body) != null) {
            preview = null; client.refresh(); policyId = UUID.randomUUID().toString()
        } }
    }
    LaunchedEffect(crewState.value) {
        if (agent.isBlank()) {
            val rows = crewState.value.workRows("agents")
            agent = (rows.firstOrNull { it.workField("definition").workBool("is_primary") } ?: rows.firstOrNull()).workField("definition").workText("agent_id").orEmpty()
        }
    }
    LaunchedEffect(proposed) { proposed.workField("origin").workText("agent_id")?.let { agent = it } }
    LaunchedEffect(signature) { preview = null; reviewedSignature = ""; runId = UUID.randomUUID().toString() }
    LaunchedEffect(state.identity) { preview = null; reviewedSignature = "" }
    LazyColumn(Modifier.fillMaxSize().imePadding(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { WorkResourceStatus(state, resource) }
        proposalState?.let { item { WorkResourceStatus(it, requireNotNull(proposedResource)) } }
        if (sourceTaskId.isNotBlank()) item { WorkCard {
            Row { Text("Use the existing task", color = Ink, modifier = Modifier.weight(1f).padding(top = 12.dp)); Switch(useSourceTask, { useSourceTask = it }, enabled = !state.busy && canAttachProposal, colors = magicanSwitchColors()) }
            Text(if (useSourceTask) "Quality uses the reviewed task context and its owning member." else "Creates new work; the proposal origin remains attached as evidence.", color = Muted, fontSize = 13.sp)
        } }
        contextState?.let { contextual -> item {
            WorkResourceStatus(contextual, requireNotNull(contextResource))
            WorkCard {
                Text(if (useSourceTask) "Attached to task" else "Source task", color = Muted, fontSize = 12.sp)
                Text(contextual.value.workText("title") ?: taskId, color = Ink, fontSize = 16.sp)
                contextual.value.workText("reason")?.let { Text(it, color = activePalette.warning, fontSize = 13.sp) }
            }
        } }
        if (proposed != null) item { WorkCard { Text("Owner proposal", color = Ink, fontSize = 18.sp); WorkRecord(proposed) } }
        else item { WorkCard {
            WorkDraftField("Outcome", draft.goal) { draft = draft.copy(goal = it) }
            WorkDraftField("Source notes", draft.source, multiline = true) { draft = draft.copy(source = it) }
            WorkReferencePicker(draft.references, { draft = draft.copy(references = it) }, { referencesPending = it }, disabled = state.busy || advanced.isNotBlank())
            WorkDraftField("Quality requirements", draft.rubric, multiline = true) { draft = draft.copy(rubric = it) }
            WorkProfilePicker("Actor", draft.actor, profiles) { draft = draft.copy(actor = it) }
            WorkProfilePicker("Critic", draft.critic, profiles) { draft = draft.copy(critic = it) }
            WorkNumber("Attempts", draft.attempts, 1..100) { draft = draft.copy(attempts = it) }
            WorkNumber("Time limit (minutes)", draft.minutes, 1..1440) { draft = draft.copy(minutes = it) }
            WorkNumber("Cost ceiling (USD)", draft.dollars, 1..10000) { draft = draft.copy(dollars = it) }
        } }
        item { WorkCard {
            WorkResourceStatus(crewState, crew)
            WorkProfilePicker("Crew member", agent, crewState.value.workRows("agents").map { row -> buildJsonObject { put("name", row.workField("definition").workText("agent_id").orEmpty()); put("family", row.workField("definition").workText("name").orEmpty()) } }, enabled = !useSourceTask) { agent = it }
            WorkDraftField("Thread", thread) { thread = it }
            WorkLink("Optional capture handoff") { showCaptureTarget = !showCaptureTarget }
            if (showCaptureTarget || captureTarget.isNotBlank()) WorkDraftField("Capture target JSON", captureTarget, multiline = true) { captureTarget = it }
            if (proposed == null) {
                WorkLink("Edit complete contract") { advanced = Json { prettyPrint = true }.encodeToString(JsonObject.serializer(), proposal ?: draft.proposal()) }
                if (advanced.isNotBlank()) WorkDraftField("Advanced contract", advanced, multiline = true) { advanced = it }
            }
            Button(onClick = {
                val original = signature; val identity = state.identity; val body = proposal ?: return@Button
                scope.launch {
                    val result = resource.perform("$WORK_QUALITY_API/preview", identity, body = body)
                    if (result != null && currentSignature == original) { preview = result; reviewedSignature = original }
                }
            }, enabled = canReview && !state.busy, colors = magicanButtonColors(), shape = MagicanButtonShape) { Text("Review contract") }
            preview?.takeIf { signature == reviewedSignature }?.let { reviewed ->
                WorkRecord(reviewed)
                Text("Outputs remain local. Approval does not send or submit anything.", color = Muted, fontSize = 13.sp)
                if (taskId.isNotBlank()) {
                    WorkNumber("Maximum recurring runs", policyRuns, 1..100) { policyRuns = it; policyId = UUID.randomUUID().toString() }
                    WorkDraftField("Policy expires (UTC)", policyExpiry) { policyExpiry = it; policyId = UUID.randomUUID().toString() }
                    Button(onClick = { configurePolicy(true) }, enabled = !state.busy && contextState?.busy != true, colors = magicanButtonColors(), shape = MagicanButtonShape) { Text("Approve bounded recurring policy") }
                }
                Button(onClick = {
                    val proposalBody = proposal ?: return@Button
                    val hash = reviewed.workText("contract_hash") ?: return@Button
                    val identity = state.identity
                    val capture = if (captureTarget.isBlank()) null else runCatching { Json.parseToJsonElement(captureTarget) as JsonObject }.getOrNull()
                    if (captureTarget.isNotBlank() && capture == null) { resource.reportError("Enter a valid capture target object."); return@Button }
                    val body = buildJsonObject {
                        put("run_id", (if (taskId.isNotBlank()) contextState?.value.workText("pending_run_id") else null) ?: runId)
                        put("proposal", proposalBody); put("agent_id", agent); put("ui_thread_id", thread); put("expected_contract_hash", hash)
                        if (taskId.isNotBlank()) putJsonObject("existing_task") {
                            put("task_id", taskId); put("context_hash", contextState?.value.workText("context_hash").orEmpty())
                        }
                        if (capture != null) put("capture_handoff", capture)
                        proposed.workField("origin").workText("request_id")?.let { put("proposal_request_id", it) }
                    }
                    scope.launch {
                        val run = resource.perform("$WORK_QUALITY_API/runs", identity, body = body)
                        run.workText("id")?.let { preview = null; onOpenRun(it) }
                    }
                }, enabled = canReview && !state.busy && reviewed.workBool("budget_can_admit_attempt"), colors = magicanButtonColors(), shape = MagicanButtonShape) { Text("Start reviewed work") }
            }
        } }
        if (useSourceTask && contextState?.value.workField("policy") != null && contextState?.value.workField("policy") != JsonNull) item { WorkCard {
            Text("Current recurring policy", color = Ink, fontSize = 17.sp)
            WorkRecord(contextState?.value.workField("policy"))
            TextButton(onClick = { configurePolicy(false) }, enabled = contextState?.busy != true) { Text("Disable future quality runs", color = Danger) }
        } }
    }
}

@Composable
private fun WorkDraftField(title: String, value: String, multiline: Boolean = false, onChanged: (String) -> Unit) {
    OutlinedTextField(value, onChanged, label = { Text(title) }, singleLine = !multiline, minLines = if (multiline) 3 else 1,
        modifier = Modifier.fillMaxWidth(), textStyle = LocalTextStyle.current.copy(color = Ink))
}

@Composable
private fun WorkProfilePicker(title: String, selected: String, profiles: List<JsonElement>, enabled: Boolean = true, onSelected: (String) -> Unit) {
    var open by remember { mutableStateOf(false) }
    Box {
        TextButton(onClick = { open = true }, enabled = enabled, modifier = Modifier.heightIn(min = 48.dp)) { Text("$title: ${selected.ifBlank { "Choose profile" }}", color = Coral) }
        DropdownMenu(open, { open = false }) {
            profiles.forEach { profile -> profile.workText("name")?.let { name ->
                DropdownMenuItem(text = { Text("$name · ${profile.workText("family").orEmpty()}") }, onClick = { onSelected(name); open = false })
            } }
        }
    }
}

@Composable
private fun WorkNumber(title: String, value: Int, range: IntRange, onChanged: (Int) -> Unit) {
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
        Text("$title: $value", color = Ink, modifier = Modifier.weight(1f).padding(vertical = 12.dp))
        TextButton(onClick = { onChanged((value - 1).coerceIn(range)) }, enabled = value > range.first) { Text("−", color = Coral) }
        TextButton(onClick = { onChanged((value + 1).coerceIn(range)) }, enabled = value < range.last) { Text("+", color = Coral) }
    }
}
