package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.*
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material.icons.outlined.*
import androidx.compose.material3.*
import androidx.compose.ui.Alignment
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
    WorkPage {
        item { WorkSearchField("Find policies", search, { search = it; page = 0; requestedId = null }) }
        item { WorkGroup {
            WorkActionRow("New ${if (kind == WorkPolicyKind.Capture) "capture" else "qualification"} policy", Icons.Outlined.AddCircleOutline) { editing = null; showEditor = true }
        } }
        workStatusItem(state, resource)
        if (requestedId != null && state.value != null && rows.isEmpty() && !state.loading && !state.value.workField("pagination").workBool("has_more")) item { WorkGroup { WorkEmptyState("Requested policy is not available.", icon = Icons.Outlined.FolderOff) } }
        else if (requestedId == null && state.value != null && rows.isEmpty()) item { WorkGroup { WorkEmptyState("No ${kind.title.lowercase()} yet.", icon = Icons.Outlined.Policy) } }
        items(rows) { row ->
            val policy = kind.policy(row) ?: JsonNull
            val id = policy.workText("id").orEmpty()
            val status = row.workText("status") ?: if (policy.workField("revoked_at").let { it != null && it != JsonNull }) "revoked" else "active"
            WorkHeaderCard(policy.workField("spec").workText("name") ?: id, subtitle = id.ifBlank { null },
                chips = listOf(WorkChipSpec(workLabel(status), workStatusTone(status)))) {
                if (kind == WorkPolicyKind.Capture) {
                    WorkRow("Review successor policy", icon = Icons.Outlined.EditNote) { editing = policy; showEditor = true }
                    WorkRowDivider(); WorkRow("Captures and exact review", icon = Icons.Outlined.Inventory) { push(WorkRoute("captures", id, "Captures")) }
                    WorkRowDivider()
                }
                if (kind == WorkPolicyKind.Learning && row.workText("status") == "paused") {
                    WorkActionRow("Resume exact qualification", Icons.Outlined.PlayCircleOutline, enabled = !state.busy && !row.workBool("expired")) { confirmation = policy to "resume" }
                    WorkRowDivider()
                }
                WorkActionRow("Revoke future use", Icons.Outlined.Block, destructive = true,
                    enabled = !state.busy && row.workText("status") != "revoked" && (kind != WorkPolicyKind.Capture || policy.workField("revoked_at") == JsonNull)) { confirmation = policy to "revoke" }
                policy.workField("spec").workRows("targets").mapNotNull { it.workText("task_id") }.forEach { taskId ->
                    WorkRowDivider(); WorkRow("Task $taskId", icon = Icons.Outlined.Checklist) { onOpenTask(taskId) }
                }
                if (kind == WorkPolicyKind.Learning) {
                    val observations = row.workField("observations") as? JsonObject ?: JsonObject(emptyMap())
                    val trials = ((row.workField("reservations") as? JsonObject)?.values.orEmpty().mapNotNull { it.workText("run_id") } + observations.keys).distinct()
                    trials.forEach { runId ->
                        WorkRowDivider(); WorkRow("Trial $runId", icon = Icons.Outlined.Science) { push(WorkRoute("quality-run", runId, "Quality run")) }
                        observations[runId].workText("evidence_hash")?.let { hash -> WorkRowDivider(); WorkRow("Sealed trial evidence", icon = Icons.Outlined.Lock) { push(WorkRoute("sealed-evidence", "$root/${WorkRepository.segment(id)}/evidence/${WorkRepository.segment(runId)}", hash)) } }
                    }
                    val monitored = row.workField("monitored") as? JsonObject ?: JsonObject(emptyMap())
                    val adopted = ((row.workField("adoptions") as? JsonObject)?.values.orEmpty().mapNotNull { it.workText("run_id") } + monitored.keys).distinct()
                    adopted.forEach { runId ->
                        WorkRowDivider(); WorkRow("Adopted run $runId", icon = Icons.Outlined.Autorenew) { push(WorkRoute("quality-run", runId, "Quality run")) }
                        monitored[runId].workText("evidence_hash")?.let { hash -> WorkRowDivider(); WorkRow("Sealed monitoring evidence", icon = Icons.Outlined.Lock) { push(WorkRoute("sealed-evidence", "$root/${WorkRepository.segment(id)}/evidence/${WorkRepository.segment(runId)}", hash)) } }
                    }
                }
                WorkRowDivider(); WorkRecordRow("Policy record", row, icon = Icons.Outlined.Article)
            }
        }
        state.value?.workField("identities")?.let { identities -> item { WorkGroup { WorkRecordRow("Counterparty identities", identities, icon = Icons.Outlined.Badge) } } }
        if (state.value != null) item { WorkPager(page, state.value.workField("pagination").workBool("has_more"), { page = it }, disabled = state.loading) }
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
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
        Column(Modifier.fillMaxSize().background(Ground).systemBarsPadding().imePadding()) {
            WorkDetailBar("Review frozen policy", onBack = onClose, backLabel = "‹ Policies")
            WorkPage {
                item { WorkNote(if (kind == WorkPolicyKind.Capture) "Captures are local records. Use exact task, contract and identity references, finite caps and an expiry." else "Choose a procedural candidate, fresh evaluation and canary cases, exact recurring tasks, finite budgets and an expiry.") }
                item { WorkGroup("Policy") { WorkFieldStack { WorkLabeledField("Advanced policy", editor, { editor = it; preview = null; reviewedEditor = "" }, minLines = 16, maxLines = 40, mono = true) } } }
                workStatusItem(state, resource)
                item { Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    WorkGroup {
                        WorkActionRow("Review frozen policy", Icons.Outlined.Shield, enabled = !state.busy && !saved) {
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
                        }
                    }
                    if (saved) WorkFootnote("Policy saved. Return to refresh the policy list.", color = activePalette.success)
                } }
                preview?.takeIf { editor == reviewedEditor }?.let { reviewed -> item { WorkGroup("Frozen preview") {
                    WorkRecord(reviewed)
                    WorkRowDivider()
                    WorkActionRow("Approve exact policy", Icons.Outlined.Verified, enabled = !state.busy && !saved) {
                        val spec = runCatching { Json.parseToJsonElement(editor) as JsonObject }.getOrNull() ?: return@WorkActionRow
                        val identity = state.identity
                        val body = buildJsonObject {
                            put("spec", spec)
                            if (kind == WorkPolicyKind.Capture) put("expected_version", previous.workField("version_hash") ?: JsonNull)
                            else put("expected_version_hash", reviewed.workField("policy").workField("version_hash") ?: JsonNull)
                        }
                        scope.launch { if (resource.perform("$root/${WorkRepository.segment(policyId)}", identity, body = body) != null) saved = true }
                    }
                } } }
            }
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
    LaunchedEffect(state.identity) { reviewed.clear(); inspect = null }
    val captures = state.value.workRows("captures")
    WorkPage {
        item { WorkGroup { WorkChoiceRow("Capture status", listOf("awaiting_review", "all", "captured", "rejected").map { it to workLabel(it) }, status, { status = it; page = 0 }, icon = Icons.Outlined.FilterList) } }
        workStatusItem(state, resource)
        if (state.value != null && captures.isEmpty()) item { WorkGroup { WorkEmptyState("No captures in this view.") } }
        items(captures) { capture ->
            val binding = capture.workField("binding")
            val target = "${binding.workText("run_id")}/${binding.workText("artifact_hash")}"
            val captureStatus = capture.workText("status") ?: "unknown"
            WorkHeaderCard(capture.workText("id") ?: "Capture", subtitle = binding.workText("identity_id")?.let { "Identity $it" },
                chips = listOf(WorkChipSpec(workLabel(captureStatus), workStatusTone(captureStatus)))) {
                WorkRow("Inspect bound artifact", icon = Icons.Outlined.FindInPage, value = if (target in reviewed) "Reviewed" else null) { inspect = capture }
                if (captureStatus == "awaiting_review") listOf(true, false).forEach { approved ->
                    WorkRowDivider()
                    WorkActionRow(if (approved) "Approve exact capture" else "Reject capture", if (approved) Icons.Outlined.CheckCircle else Icons.Outlined.Cancel, destructive = !approved,
                        enabled = !state.busy && (!approved || target in reviewed)) {
                        val identity = state.identity; val id = capture.workText("id") ?: return@WorkActionRow
                        scope.launch { if (resource.perform("$root/${WorkRepository.segment(id)}/review", identity, body = buildJsonObject { put("binding_hash", capture.workField("binding_hash") ?: JsonNull); put("approved", approved) }) != null) resource.refresh() }
                    }
                }
                WorkRowDivider(); WorkRecordRow("Capture record", capture, icon = Icons.Outlined.Article)
            }
        }
        if (state.value != null) item { WorkPager(page, state.value.workField("pagination").workBool("has_more"), { page = it }) }
    }
    inspect?.let { capture ->
        val binding = capture.workField("binding"); val target = "${binding.workText("run_id")}/${binding.workText("artifact_hash")}"
        Dialog(onDismissRequest = { inspect = null }, properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
            Column(Modifier.fillMaxSize().background(Ground).systemBarsPadding()) {
                WorkDetailBar("Bound artifact", onBack = { inspect = null }, backLabel = "‹ Captures")
                WorkQualityArtifact(target, push, onReviewed = { if (target !in reviewed) reviewed.add(target) })
            }
        }
    }
}

@Composable
internal fun WorkSealedEvidence(path: String, expectedHash: String) {
    val resource = rememberWorkResource(path)
    val state by resource.state.collectAsStateWithLifecycle()
    WorkPage {
        workStatusItem(state, resource)
        state.value?.let { evidence ->
            if (evidence.workBool("sealed") && evidence.workText("content_hash") == expectedHash) {
                item { WorkHeaderCard("Sealed evidence", subtitle = expectedHash, chips = listOf(WorkChipSpec("Sealed", activePalette.success, Icons.Filled.Lock))) }
                item { WorkGroup("Evidence") { WorkRecord(evidence) } }
            } else item { WorkGroup {
                Row(Modifier.fillMaxWidth().padding(vertical = 12.dp), verticalAlignment = Alignment.Top) {
                    Icon(Icons.Filled.Warning, null, tint = activePalette.warning, modifier = Modifier.padding(top = 2.dp).size(16.dp))
                    Spacer(Modifier.width(12.dp))
                    Text("Evidence changed or is not sealed. Refresh the recorded policy result.", color = Ink, fontSize = 15.sp, lineHeight = 20.sp)
                }
            } }
        }
    }
}

internal val workPretty = Json { prettyPrint = true }

internal fun workPolicyInitial(kind: WorkPolicyKind): JsonElement = Json.parseToJsonElement(
    if (kind == WorkPolicyKind.Capture) """{"name":"","goal_id":null,"sender_identity":"","channel":"","message_kind":"","targets":[],"allowed_contract_hashes":[],"content_contract_hash":"","content_template":null,"maximum_acts":1,"maximum_acts_per_day":1,"maximum_acts_per_recipient":1,"expires_at":"","approval":{"source_document":"","always_review":true,"important_identity_ids":[],"allow_automatic_capture":false,"clean_approvals_before_automatic_capture":null,"audit_every":1}}""" else """{"name":"","candidate_id":"","agent_id":"","ui_thread_id":"","baseline_guidance":null,"training_run_ids":[],"evaluation_cases":[],"canary_cases":[],"evaluation_requirements":{"metric":"weighted_quality","minimum_mean_gain":0,"maximum_case_regression":0,"minimum_candidate_acceptance_rate":1,"maximum_acceptance_regressions":0},"canary_requirements":{"metric":"weighted_quality","minimum_mean_gain":0,"maximum_case_regression":0,"minimum_candidate_acceptance_rate":1,"maximum_acceptance_regressions":0},"maximum_evaluation_cost_microusd":0,"maximum_canary_cost_microusd":0,"maximum_guidance_bytes":16384,"targets":[],"maximum_adopted_runs":1,"disable_after_regressions":1,"unknown_monitor_disables_adoption":true,"monitor_nonacceptance_is_regression":true,"expires_at":""}"""
)
