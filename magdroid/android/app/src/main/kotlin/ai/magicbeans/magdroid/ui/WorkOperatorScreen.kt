package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.*
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

/** Same harness proposal and canonical owner-response endpoints as the web. */
@Composable
internal fun WorkOperatorScreen(agentId: String) {
    val overview = rememberWorkResource("/api/magician/v2/agents/${WorkRepository.segment(agentId)}/harness-overview")
    val overviewState by overview.state.collectAsStateWithLifecycle()
    val proposals = rememberWorkResource("/api/magician/v2/proposals")
    val proposalState by proposals.state.collectAsStateWithLifecycle()
    val requests = rememberWorkResource("/api/magician/v2/user-requests?owner_agent_id=${WorkRepository.segment(agentId)}&include_history=true&history_limit=40")
    val requestState by requests.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    var confirmation by remember { mutableStateOf<JsonObject?>(null) }
    LaunchedEffect(proposalState.identity, requestState.identity) { confirmation = null }
    fun reload() { overview.refresh(); proposals.refresh(); requests.refresh() }
    val rows = proposalState.value.workRows("proposals").filter { it.workText("source") == "harness:$agentId" }
    val briefings = requestState.value.workRows("requests").filter { it.workField("context").workText("owner_agent_id") == agentId && it.workText("request_type").orEmpty().startsWith("harness.notify_owner.") }
    WorkPage {
        workStatusItem(overviewState, overview)
        overviewState.value?.let { value -> item { WorkGroup { WorkRecordRow("Playbook, state and execution evidence", value, icon = Icons.Outlined.FindInPage) } } }
        item { WorkGroupTitle("Structural proposals") }
        workStatusItem(proposalState, proposals, key = "proposal-status")
        if (proposalState.value != null && rows.isEmpty()) item { WorkGroup { WorkRow("No structural proposals.", icon = Icons.Outlined.Inbox, labelColor = Muted, accessory = WorkRowAccessory.None) } }
        items(rows) { proposal ->
            val status = proposal.workText("status") ?: "unknown"
            WorkHeaderCard(proposal.workText("title") ?: proposal.workField("id").workDisplay(), subtitle = proposal.workText("id"), detail = proposal.workText("summary"),
                chips = listOf(WorkChipSpec(workLabel(status), workStatusTone(status)))) {
                if (proposal.workText("status") == "pending" || (proposal.workText("status") == "approved" && (proposal.workText("applied_version")?.toIntOrNull() ?: 0) < 1)) {
                    listOf("approve", "defer", "reject").forEach { decision ->
                        WorkActionRow(workLabel(decision), when (decision) { "approve" -> Icons.Outlined.CheckCircle; "defer" -> Icons.Outlined.Schedule; else -> Icons.Outlined.Cancel },
                            destructive = decision == "reject", enabled = !proposalState.busy) {
                            confirmation = buildJsonObject { put("kind", "proposal"); put("id", proposal.workField("id") ?: JsonNull); put("decision", decision) }
                        }
                        WorkRowDivider()
                    }
                }
                WorkRecordRow("Proposal record", proposal, icon = Icons.Outlined.Article)
            }
        }
        item { WorkGroupTitle("Owner briefings and history") }
        workStatusItem(requestState, requests, key = "request-status")
        if (requestState.value != null && briefings.isEmpty()) item { WorkGroup { WorkRow("No owner briefings.", icon = Icons.Outlined.Inbox, labelColor = Muted, accessory = WorkRowAccessory.None) } }
        items(briefings) { request ->
            val status = request.workText("status") ?: "pending"
            WorkHeaderCard(request.workText("title") ?: workLabel(request.workField("request_type").workDisplay()), subtitle = request.workText("id"),
                chips = listOf(WorkChipSpec(workLabel(status), workStatusTone(status)))) {
                if (request.workText("status") == "pending" || request.workText("status") == null) {
                    val options = request.workRows("options").ifEmpty { listOf(buildJsonObject { put("id", "acknowledge"); put("label", "Acknowledge") }) }
                    options.forEach { option ->
                        option.workText("id")?.let { id ->
                            WorkActionRow(option.workText("label") ?: id, Icons.Outlined.CheckCircle, enabled = !requestState.busy) {
                                confirmation = buildJsonObject { put("kind", "request"); put("id", request.workField("id") ?: JsonNull); put("decision", id) }
                            }
                            WorkRowDivider()
                        }
                    }
                }
                WorkRecordRow("Briefing record", request, icon = Icons.Outlined.Article)
            }
        }
        requestState.value?.workField("history")?.let { history -> item { WorkGroup { WorkRecordRow("Previous responses", history, icon = Icons.Outlined.History) } } }
    }
    confirmation?.let { action -> AlertDialog(
        onDismissRequest = { confirmation = null }, title = { Text("Apply this reviewed decision?", color = Ink) },
        confirmButton = { TextButton(onClick = {
            confirmation = null
            val id = action.text("id") ?: return@TextButton
            val decision = action.text("decision") ?: return@TextButton
            if (action.text("kind") == "proposal") {
                val identity = proposalState.identity
                scope.launch {
                    val result = proposals.perform("/api/magician/v2/proposals/${WorkRepository.segment(id)}/resolve", identity, body = buildJsonObject { put("decision", decision); put("channel", "android") })
                    if (result != null) { reload(); result.workText("application_error")?.let(proposals::reportError) }
                }
            } else {
                val identity = requestState.identity
                scope.launch { if (requests.perform("/api/magician/v2/hitl/${WorkRepository.segment(id)}/respond", identity, body = buildJsonObject {
                    put("source", "user_request"); putJsonObject("value") { put("type", "choice"); put("selected_id", decision) }; put("channel", "android")
                }) != null) reload() }
            }
        }, enabled = !proposalState.busy && !requestState.busy) { Text("Confirm", color = Coral) } },
        dismissButton = { TextButton(onClick = { confirmation = null }) { Text("Cancel", color = Muted) } }, containerColor = Ground,
    ) }
}
