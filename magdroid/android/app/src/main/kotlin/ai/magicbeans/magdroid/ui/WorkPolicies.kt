package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.*
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*
import java.util.UUID

internal enum class WorkPolicyKind(val endpoint: String, val rows: String, val title: String) {
    Capture("campaigns", "campaigns", "Capture policies"), Learning("guidance-policies", "policies", "Learning policies");
    fun policy(row: JsonElement) = if (this == Capture) row.workField("state").workField("campaign") else row.workField("policy")
}

/** Native advanced controls use the same preview and version checks as the web. */
@Composable
internal fun WorkPolicies(kind: WorkPolicyKind, push: (WorkRoute) -> Unit, onOpenTask: (String) -> Unit, initialPolicyId: String? = null) {
    var page by remember { mutableIntStateOf(0) }
    var search by remember { mutableStateOf(initialPolicyId.orEmpty()) }
    var requestedId by remember { mutableStateOf(initialPolicyId) }
    val root = "$WORK_QUALITY_API/${kind.endpoint}"
    val resource = rememberWorkResource("$root?limit=10&offset=${page * 10}&q=${WorkRepository.segment(search)}")
    val state by resource.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    var editing by remember { mutableStateOf<JsonElement?>(null) }
    var showEditor by remember { mutableStateOf(false) }
    var confirmation by remember { mutableStateOf<Pair<JsonElement, String>?>(null) }
    LaunchedEffect(state.identity) { editing = null; showEditor = false; confirmation = null }
    val rows = state.value.workRows(kind.rows).filter { requestedId == null || kind.policy(it).workText("id") == requestedId }
    LaunchedEffect(state.value, requestedId) {
        if (requestedId != null && state.value != null && rows.isEmpty() && state.value.workField("pagination").workBool("has_more")) page++
    }
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item {
            OutlinedTextField(search, { search = it; page = 0; requestedId = null }, label = { Text("Find policies") }, modifier = Modifier.fillMaxWidth())
            WorkLink("New ${if (kind == WorkPolicyKind.Capture) "capture" else "qualification"} policy") { editing = null; showEditor = true }
            WorkResourceStatus(state, resource); WorkLink("Refresh policies", resource::refresh)
        }
        if (requestedId != null && state.value != null && rows.isEmpty() && !state.loading && !state.value.workField("pagination").workBool("has_more")) item { Text("Requested policy is not available.", color = Muted) }
        items(rows) { row ->
            val policy = kind.policy(row) ?: JsonNull
            val id = policy.workText("id").orEmpty()
            WorkCard {
                Text(policy.workField("spec").workText("name") ?: id, color = Ink, fontSize = 18.sp)
                Text(row.workText("status") ?: "Capture policy", color = Muted, fontSize = 13.sp)
                WorkRecord(row)
                if (kind == WorkPolicyKind.Capture) {
                    WorkLink("Review successor policy") { editing = policy; showEditor = true }
                    WorkLink("Captures and exact review") { push(WorkRoute("captures", id, "Captures")) }
                }
                TextButton(onClick = { confirmation = policy to "revoke" }, enabled = !state.busy && row.workText("status") != "revoked" && (kind != WorkPolicyKind.Capture || policy.workField("revoked_at") == JsonNull)) { Text("Revoke future use", color = Danger) }
                if (kind == WorkPolicyKind.Learning && row.workText("status") == "paused") {
                    TextButton(onClick = { confirmation = policy to "resume" }, enabled = !state.busy && !row.workBool("expired")) { Text("Resume exact qualification", color = Coral) }
                }
                policy.workField("spec").workRows("targets").forEach { target -> target.workText("task_id")?.let { taskId -> WorkLink("Task $taskId") { onOpenTask(taskId) } } }
                if (kind == WorkPolicyKind.Learning) {
                    val observations = row.workField("observations") as? JsonObject ?: JsonObject(emptyMap())
                    val trials = ((row.workField("reservations") as? JsonObject)?.values.orEmpty().mapNotNull { it.workText("run_id") } + observations.keys).distinct()
                    trials.forEach { runId ->
                    WorkLink("Trial $runId") { push(WorkRoute("quality-run", runId, "Quality run")) }
                    observations[runId].workText("evidence_hash")?.let { hash -> WorkLink("Sealed trial evidence") { push(WorkRoute("sealed-evidence", "$root/${WorkRepository.segment(id)}/evidence/${WorkRepository.segment(runId)}", hash)) } }
                    }
                    val monitored = row.workField("monitored") as? JsonObject ?: JsonObject(emptyMap())
                    val adopted = ((row.workField("adoptions") as? JsonObject)?.values.orEmpty().mapNotNull { it.workText("run_id") } + monitored.keys).distinct()
                    adopted.forEach { runId ->
                        WorkLink("Adopted run $runId") { push(WorkRoute("quality-run", runId, "Quality run")) }
                        monitored[runId].workText("evidence_hash")?.let { hash -> WorkLink("Sealed monitoring evidence") { push(WorkRoute("sealed-evidence", "$root/${WorkRepository.segment(id)}/evidence/${WorkRepository.segment(runId)}", hash)) } }
                    }
                }
            }
        }
        state.value.workField("identities")?.let { identities -> item { WorkCard { Text("Counterparty identities", color = Ink, fontSize = 17.sp); WorkRecord(identities) } } }
        item { Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            TextButton(onClick = { page-- }, enabled = page > 0 && !state.loading) { Text("Previous", color = Coral) }
            Text("Page ${page + 1}", color = Muted, modifier = Modifier.padding(12.dp))
            TextButton(onClick = { page++ }, enabled = state.value.workField("pagination").workBool("has_more") && !state.loading) { Text("Next", color = Coral) }
        } }
    }
    if (showEditor) WorkPolicyEditor(kind, editing, onClose = { showEditor = false; resource.refresh() })
    confirmation?.let { (policy, action) -> AlertDialog(
        onDismissRequest = { confirmation = null }, title = { Text("Apply this policy change?", color = Ink) },
        text = { Text("$action applies to the exact version of ${policy.workField("spec").workText("name").orEmpty()}.", color = Muted) },
        confirmButton = { TextButton(onClick = {
            confirmation = null
            val identity = state.identity
            val id = policy.workText("id") ?: return@TextButton
            val path = "$root/${WorkRepository.segment(id)}" + if (kind == WorkPolicyKind.Capture) "" else "/$action"
            val body = buildJsonObject {
                put(if (kind == WorkPolicyKind.Capture) "expected_version" else "expected_version_hash", policy.workField("version_hash") ?: JsonNull)
                if (kind == WorkPolicyKind.Capture) put("spec", JsonNull)
            }
            scope.launch { if (resource.perform(path, identity, body = body) != null) resource.refresh() }
        }, enabled = !state.busy) { Text("Confirm", color = Coral) } },
        dismissButton = { TextButton(onClick = { confirmation = null }) { Text("Cancel", color = Muted) } }, containerColor = Ground,
    ) }
}

@Composable
internal fun WorkPolicyEditor(kind: WorkPolicyKind, previous: JsonElement?, onClose: () -> Unit) {
    val root = "$WORK_QUALITY_API/${kind.endpoint}"
    val resource = rememberWorkResource("$root?limit=1&offset=0")
    val state by resource.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    val policyId = remember { previous.workText("id") ?: UUID.randomUUID().toString() }
    var editor by remember { mutableStateOf(workPretty.encodeToString(JsonElement.serializer(), previous.workField("spec") ?: workPolicyInitial(kind))) }
    val currentEditor by rememberUpdatedState(editor)
    var reviewedEditor by remember { mutableStateOf("") }
    var preview by remember { mutableStateOf<JsonElement?>(null) }
    var saved by remember { mutableStateOf(false) }
    LaunchedEffect(state.identity) { reviewedEditor = ""; preview = null }
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        LazyColumn(Modifier.fillMaxSize().background(Ground).systemBarsPadding().imePadding(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            item { WorkLink("‹ Policies", onClose); Text("Review frozen policy", color = Ink, fontSize = 20.sp) }
            item { Text(if (kind == WorkPolicyKind.Capture) "Captures are local records. Use exact task, contract and identity references, finite caps and an expiry." else "Choose a procedural candidate, fresh evaluation and canary cases, exact recurring tasks, finite budgets and an expiry.", color = Muted, fontSize = 14.sp) }
            item { OutlinedTextField(editor, { editor = it; preview = null; reviewedEditor = "" }, modifier = Modifier.fillMaxWidth(), minLines = 16, label = { Text("Advanced policy") }) }
            item { WorkResourceStatus(state, resource) }
            item { Button(onClick = {
                val original = editor; val identity = state.identity
                val spec = runCatching { Json.parseToJsonElement(original) as JsonObject }.getOrNull()
                if (spec == null) resource.reportError("Enter a valid policy object before review.")
                else scope.launch {
                    val result = resource.perform("$root/preview", identity, body = buildJsonObject { put("spec", spec); if (kind == WorkPolicyKind.Learning) put("policy_id", policyId) })
                    if (result != null && currentEditor == original) {
                        if (kind == WorkPolicyKind.Capture) editor = workPretty.encodeToString(JsonElement.serializer(), result.workField("spec") ?: JsonNull)
                        reviewedEditor = editor; preview = result
                    }
                }
            }, enabled = !state.busy && !saved, colors = magicanButtonColors(), shape = MagicanButtonShape) { Text("Review frozen policy") } }
            preview?.takeIf { editor == reviewedEditor }?.let { reviewed -> item { WorkCard {
                WorkRecord(reviewed)
                Button(onClick = {
                    val spec = runCatching { Json.parseToJsonElement(editor) as JsonObject }.getOrNull() ?: return@Button
                    val identity = state.identity
                    val body = buildJsonObject {
                        put("spec", spec)
                        if (kind == WorkPolicyKind.Capture) put("expected_version", previous.workField("version_hash") ?: JsonNull)
                        else put("expected_version_hash", reviewed.workField("policy").workField("version_hash") ?: JsonNull)
                    }
                    scope.launch { if (resource.perform("$root/${WorkRepository.segment(policyId)}", identity, body = body) != null) saved = true }
                }, enabled = !state.busy && !saved, colors = magicanButtonColors(), shape = MagicanButtonShape) { Text("Approve exact policy") }
            } } }
            if (saved) item { Text("Policy saved. Return to refresh the policy list.", color = Ink) }
        }
    }
}

@Composable
internal fun WorkCaptures(campaignId: String, push: (WorkRoute) -> Unit) {
    var page by remember { mutableIntStateOf(0) }
    var status by remember { mutableStateOf("awaiting_review") }
    val root = "$WORK_QUALITY_API/campaigns/${WorkRepository.segment(campaignId)}/captures"
    val resource = rememberWorkResource("$root?limit=20&offset=${page * 20}&status=$status")
    val state by resource.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    var inspect by remember { mutableStateOf<JsonElement?>(null) }
    val reviewed = remember { mutableStateListOf<String>() }
    var statusMenu by remember { mutableStateOf(false) }
    LaunchedEffect(state.identity) { reviewed.clear(); inspect = null }
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { WorkResourceStatus(state, resource); WorkLink("Refresh captures", resource::refresh) }
        item { Box {
            WorkLink("Status: ${workLabel(status)}") { statusMenu = true }
            DropdownMenu(statusMenu, { statusMenu = false }) { listOf("awaiting_review", "all", "captured", "rejected").forEach { value ->
                DropdownMenuItem(text = { Text(workLabel(value)) }, onClick = { status = value; page = 0; statusMenu = false })
            } }
        } }
        items(state.value.workRows("captures")) { capture -> WorkCard {
            WorkRecord(capture)
            val binding = capture.workField("binding")
            val target = "${binding.workText("run_id")}/${binding.workText("artifact_hash")}"
            WorkLink("Inspect bound artifact") { inspect = capture }
            if (capture.workText("status") == "awaiting_review") Column {
                listOf(true, false).forEach { approved -> TextButton(onClick = {
                    val identity = state.identity; val id = capture.workText("id") ?: return@TextButton
                    scope.launch { if (resource.perform("$root/${WorkRepository.segment(id)}/review", identity, body = buildJsonObject { put("binding_hash", capture.workField("binding_hash") ?: JsonNull); put("approved", approved) }) != null) resource.refresh() }
                }, enabled = !state.busy && (!approved || target in reviewed)) { Text(if (approved) "Approve exact capture" else "Reject capture", color = if (approved) Coral else Danger) } }
            }
        } }
        item { Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            TextButton(onClick = { page-- }, enabled = page > 0) { Text("Previous", color = Coral) }
            Text("Page ${page + 1}", color = Muted)
            TextButton(onClick = { page++ }, enabled = state.value.workField("pagination").workBool("has_more")) { Text("Next", color = Coral) }
        } }
    }
    inspect?.let { capture ->
        val binding = capture.workField("binding"); val target = "${binding.workText("run_id")}/${binding.workText("artifact_hash")}"
        Dialog(onDismissRequest = { inspect = null }, properties = DialogProperties(usePlatformDefaultWidth = false)) {
            Column(Modifier.fillMaxSize().background(Ground).systemBarsPadding()) {
                WorkLink("‹ Captures") { inspect = null }
                WorkQualityArtifact(target, push, onReviewed = { if (target !in reviewed) reviewed.add(target) })
            }
        }
    }
}

@Composable
internal fun WorkSealedEvidence(path: String, expectedHash: String) {
    val resource = rememberWorkResource(path)
    val state by resource.state.collectAsStateWithLifecycle()
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp)) {
        item { WorkResourceStatus(state, resource) }
        state.value?.let { evidence -> item {
            if (evidence.workBool("sealed") && evidence.workText("content_hash") == expectedHash) WorkRecord(evidence)
            else Text("Evidence changed or is not sealed. Refresh the recorded policy result.", color = activePalette.warning)
        } }
    }
}

internal val workPretty = Json { prettyPrint = true }

internal fun workPolicyInitial(kind: WorkPolicyKind): JsonElement = Json.parseToJsonElement(
    if (kind == WorkPolicyKind.Capture) """{"name":"","goal_id":null,"sender_identity":"","channel":"","message_kind":"","targets":[],"allowed_contract_hashes":[],"content_contract_hash":"","content_template":null,"maximum_acts":1,"maximum_acts_per_day":1,"maximum_acts_per_recipient":1,"expires_at":"","approval":{"source_document":"","always_review":true,"important_identity_ids":[],"allow_automatic_capture":false,"clean_approvals_before_automatic_capture":null,"audit_every":1}}""" else """{"name":"","candidate_id":"","agent_id":"","ui_thread_id":"","baseline_guidance":null,"training_run_ids":[],"evaluation_cases":[],"canary_cases":[],"evaluation_requirements":{"metric":"weighted_quality","minimum_mean_gain":0,"maximum_case_regression":0,"minimum_candidate_acceptance_rate":1,"maximum_acceptance_regressions":0},"canary_requirements":{"metric":"weighted_quality","minimum_mean_gain":0,"maximum_case_regression":0,"minimum_candidate_acceptance_rate":1,"maximum_acceptance_regressions":0},"maximum_evaluation_cost_microusd":0,"maximum_canary_cost_microusd":0,"maximum_guidance_bytes":16384,"targets":[],"maximum_adopted_runs":1,"disable_after_regressions":1,"unknown_monitor_disables_adoption":true,"monitor_nonacceptance_is_regression":true,"expires_at":""}"""
)
