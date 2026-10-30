package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.*
import ai.magicbeans.magdroid.chat.ChatModelChoice
import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.*
import androidx.compose.ui.text.font.FontWeight
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
    val catalog = rememberWorkResource("/api/magician/v2/llm/routing")
    val catalogState by catalog.state.collectAsStateWithLifecycle()
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
    val modelCatalog = remember(state.value, catalogState.value) { WorkQualityModels(state.value, catalogState.value) }
    val draftModels = modelCatalog.models()
    val reviewModels = modelCatalog.family(draft.actor)?.let { modelCatalog.models(it) }.orEmpty()
    val modelsValid = modelCatalog.valid(draft.actor, draftModels) && modelCatalog.valid(draft.critic, reviewModels)
    val canAttachProposal = proposed == null || (proposed.workField("proposal").workField("native_harness") is JsonObject &&
        contextState?.value.workField("goal") == proposed.workField("proposal").workField("goal") && contextState?.value.workField("agent_id") == proposed.workField("origin").workField("agent_id"))
    val canReview = !referencesPending && proposal != null && agent.isNotBlank() && thread.isNotBlank() &&
        (!target.startsWith("proposal:") || proposed != null) &&
        (taskId.isBlank() || (contextState?.value.workText("task_id") == taskId && contextState?.value.workBool("can_start") && canAttachProposal)) &&
        (proposed != null || advanced.isNotBlank() || (draft.goal.isNotBlank() && draft.rubric.isNotBlank() && modelsValid))
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
    val crewRows = crewState.value.workRows("agents")
    var captureOpen by remember { mutableStateOf(captureTarget.isNotBlank()) }
    var advancedOpen by remember { mutableStateOf(false) }
    WorkPage(Modifier.imePadding()) {
        workStatusItem(state, resource)
        proposalState?.let { workStatusItem(it, requireNotNull(proposedResource), key = "proposal-status") }
        if (sourceTaskId.isNotBlank()) item(key = "task") { Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            WorkGroup("Task") {
                WorkSwitchRow("Use the existing task", useSourceTask, { useSourceTask = it }, enabled = !state.busy && canAttachProposal)
                contextState?.value?.let { value ->
                    WorkRowDivider()
                    WorkRow(value.workText("title") ?: taskId.ifBlank { sourceTaskId }, icon = Icons.Outlined.Checklist, detail = if (useSourceTask) "Attached to task" else "Source task", accessory = WorkRowAccessory.None)
                    value.workText("reason")?.let { WorkNote(it, color = activePalette.warning) }
                }
            }
            WorkFootnote(if (useSourceTask) "Quality uses the reviewed task context and its owning member." else "Creates new work; the proposal origin remains attached as evidence.")
        } }
        contextState?.let { contextual -> workStatusItem(contextual, requireNotNull(contextResource), key = "context-status") }
        if (proposed != null) item(key = "proposal") { WorkGroup("Owner proposal") { WorkRecord(proposed) } }
        else {
            item(key = "brief") { WorkGroup("Brief") { WorkFieldStack {
                WorkDraftField("Outcome", draft.goal) { draft = draft.copy(goal = it) }
                WorkDraftField("Source notes", draft.source, multiline = true) { draft = draft.copy(source = it) }
                WorkDraftField("Quality requirements", draft.rubric, multiline = true) { draft = draft.copy(rubric = it) }
            } } }
            item(key = "references") { WorkGroup("References") {
                WorkReferencePicker(draft.references, { draft = draft.copy(references = it) }, { referencesPending = it }, disabled = state.busy || advanced.isNotBlank())
            } }
            item(key = "models") { Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                WorkGroup("Models") {
                    if (catalogState.hasStatus) Box(Modifier.padding(top = 12.dp)) { WorkResourceStatus(catalogState, catalog) }
                    WorkQualityModelField("Draft model", draft.actor, modelCatalog, draftModels, !state.busy && advanced.isBlank()) { draft = draft.copy(actor = it) }
                    WorkRowDivider()
                    WorkQualityModelField("Independent review model", draft.critic, modelCatalog, reviewModels, !state.busy && advanced.isBlank()) { draft = draft.copy(critic = it) }
                }
                if (!modelsValid) WorkFootnote("Choose available draft and review models from different model families. Saved choices stay unchanged until you replace them.")
            } }
            item(key = "budget") { WorkGroup("Budget") {
                WorkStepperRow("Attempts", draft.attempts, 0..100, { draft = draft.copy(attempts = it) }, display = if (draft.attempts == 0) "Unlimited" else "${draft.attempts}")
                WorkRowDivider()
                WorkStepperRow("Time limit", draft.minutes, 0..1440, { draft = draft.copy(minutes = it) }, display = if (draft.minutes == 0) "Unlimited" else "${draft.minutes} min")
                WorkRowDivider()
                WorkStepperRow("Cost ceiling", draft.dollars, 0..10000, { draft = draft.copy(dollars = it) }, display = if (draft.dollars == 0) "Unlimited" else "$${draft.dollars}")
            } }
        }
        item(key = "assignment") { WorkGroup("Assignment") {
            if (crewState.hasStatus) Box(Modifier.padding(top = 12.dp)) { WorkResourceStatus(crewState, crew) }
            val members = listOf("" to "Choose a member") + listOfNotNull(agent.takeIf { it.isNotBlank() && crewRows.none { row -> row.workField("definition").workText("agent_id") == it } }?.let { it to it }) +
                crewRows.mapNotNull { row -> row.workField("definition").workText("agent_id")?.let { id -> id to (row.workField("definition").workText("name") ?: id) } }
            WorkChoiceRow("Crew member", members, agent, { agent = it }, icon = Icons.Outlined.AccountCircle, enabled = !useSourceTask,
                semanticLabel = "Crew member: ${agent.ifBlank { "Choose a member" }}")
            WorkRowDivider()
            WorkFieldStack { WorkDraftField("Thread", thread) { thread = it } }
        } }
        item(key = "options") { WorkGroup("Options") {
            WorkRow("Optional capture handoff", icon = Icons.Outlined.MoveToInbox, value = if (captureTarget.isBlank()) null else "Set",
                accessory = if (captureOpen) WorkRowAccessory.Expanded else WorkRowAccessory.Collapsed) { captureOpen = !captureOpen }
            if (captureOpen) Column {
                WorkNote("Use an exact campaign, version and identity target from a reviewed capture policy. The contract must request a capture handoff.")
                Box(Modifier.padding(bottom = 12.dp)) { WorkLabeledField("Capture target JSON", captureTarget, { captureTarget = it }, minLines = 3, maxLines = 10, mono = true) }
            }
            if (proposed == null) {
                WorkRowDivider()
                WorkRow("Advanced contract", icon = Icons.Outlined.DataObject, value = if (advanced.isBlank()) null else "Edited",
                    accessory = if (advancedOpen) WorkRowAccessory.Expanded else WorkRowAccessory.Collapsed) { advancedOpen = !advancedOpen }
                if (advancedOpen) Column {
                    WorkNote("Review a complete contract to configure references, artifact checks, task execution limits and policies.")
                    WorkActionRow("Edit complete contract", Icons.Outlined.Code) { advanced = Json { prettyPrint = true }.encodeToString(JsonObject.serializer(), proposal ?: draft.proposal()) }
                    if (advanced.isNotBlank()) Box(Modifier.padding(bottom = 12.dp)) { WorkLabeledField("Advanced contract", advanced, { advanced = it }, minLines = 8, maxLines = 24, mono = true) }
                }
            }
        } }
        item(key = "review") { WorkGroup("Review and start") {
            WorkActionRow("Review contract", Icons.Outlined.Shield, enabled = canReview && !state.busy) {
                val original = signature; val identity = state.identity; val body = proposal ?: return@WorkActionRow
                scope.launch {
                    val result = resource.perform("$WORK_QUALITY_API/preview", identity, body = body)
                    if (result != null && currentSignature == original) { preview = result; reviewedSignature = original }
                }
            }
            preview?.takeIf { signature == reviewedSignature }?.let { reviewed ->
                WorkRowDivider()
                WorkRecord(reviewed)
                WorkRowDivider()
                WorkNote("Outputs remain local. Approval does not send or submit anything.")
                if (taskId.isNotBlank()) {
                    WorkRowDivider()
                    WorkStepperRow("Maximum recurring runs", policyRuns, 1..100, { policyRuns = it; policyId = UUID.randomUUID().toString() })
                    WorkRowDivider()
                    WorkFieldStack { WorkDraftField("Policy expires (UTC)", policyExpiry) { policyExpiry = it; policyId = UUID.randomUUID().toString() } }
                    WorkRowDivider()
                    WorkActionRow("Approve bounded recurring policy", Icons.Outlined.Verified, enabled = !state.busy && contextState?.busy != true) { configurePolicy(true) }
                }
                WorkRowDivider()
                WorkActionRow("Start reviewed work", Icons.Outlined.PlayCircleOutline, enabled = canReview && !state.busy && reviewed.workBool("budget_can_admit_attempt")) {
                    val proposalBody = proposal ?: return@WorkActionRow
                    val hash = reviewed.workText("contract_hash") ?: return@WorkActionRow
                    val identity = state.identity
                    val capture = if (captureTarget.isBlank()) null else runCatching { Json.parseToJsonElement(captureTarget) as JsonObject }.getOrNull()
                    if (captureTarget.isNotBlank() && capture == null) { resource.reportError("Enter a valid capture target object."); return@WorkActionRow }
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
                }
            }
        } }
        if (useSourceTask && contextState?.value.workField("policy") != null && contextState?.value.workField("policy") != JsonNull) item(key = "policy") { WorkGroup("Current recurring policy") {
            WorkRecord(contextState?.value.workField("policy"))
            WorkRowDivider()
            WorkActionRow("Disable future quality runs", Icons.Outlined.Block, destructive = true, enabled = contextState?.busy != true) { configurePolicy(false) }
        } }
    }
}

@Composable
private fun WorkDraftField(title: String, value: String, multiline: Boolean = false, onChanged: (String) -> Unit) {
    WorkLabeledField(title, value, onChanged, minLines = if (multiline) 3 else 1, maxLines = if (multiline) 8 else 1)
}

@Composable
private fun WorkQualityModelField(title: String, route: String, catalog: WorkQualityModels, models: List<ChatModelChoice>, enabled: Boolean, select: (String) -> Unit) {
    val selected = catalog.selection(route, models)
    val model = models.firstOrNull { it.id == selected?.modelId }
    Column(Modifier.fillMaxWidth().padding(vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(title, color = Muted, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
        CatalogChoiceDropdown(title, model?.id ?: route,
            (if (model == null && route.isNotBlank()) listOf(CatalogChoiceOption(route, "Unavailable model · saved", enabled = false)) else emptyList()) +
                models.map { CatalogChoiceOption(it.id, it.label, it.providerDisplayName, it.installed) }, enabled = enabled) { id ->
            models.firstOrNull { it.id == id }?.let { next ->
                (next.choices[selected?.thinking] ?: next.choices["auto"])?.let(select)
            }
        }
        model?.let { active -> ModelThinkingControl(active.thinking, selected?.thinking ?: "auto", enabled = enabled) { level -> active.choices[level]?.let(select) } }
    }
}
