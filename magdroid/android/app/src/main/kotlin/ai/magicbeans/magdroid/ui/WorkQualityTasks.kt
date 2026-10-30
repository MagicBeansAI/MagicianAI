package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.*
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.*
import androidx.compose.runtime.*
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.serialization.json.*

@Composable
internal fun WorkQualityTasks(push: (WorkRoute) -> Unit, onOpenTask: (String) -> Unit) {
    var page by remember { mutableIntStateOf(0) }; var goal by remember { mutableStateOf("") }
    val resource = rememberWorkResource("$WORK_QUALITY_API/work?limit=20&offset=${page * 20}&goal_id=${WorkRepository.segment(goal)}")
    val state by resource.state.collectAsStateWithLifecycle()
    val targets = state.value.workRows("targets").filter { it.workText("task_id") != null }
    WorkPage {
        item { Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            WorkGroup { WorkFieldStack { WorkLabeledField("Goal ID (optional)", goal, { goal = it; page = 0 }, maxLines = 1) } }
            WorkFootnote("Leave empty to list every recurring task.")
        } }
        workStatusItem(state, resource)
        if (state.value != null && targets.isEmpty()) item { WorkGroup { WorkEmptyState("No recurring quality tasks.", icon = Icons.Outlined.Repeat) } }
        items(targets) { target ->
            val taskId = target.workText("task_id").orEmpty()
            WorkHeaderCard(target.workText("title") ?: taskId, subtitle = listOfNotNull(taskId, target.workText("agent_id")).joinToString(" · ")) {
                WorkRow("Task and full controls", icon = Icons.Outlined.Checklist) { onOpenTask(taskId) }
                WorkRowDivider(); WorkRow("Review task quality policy", icon = Icons.Outlined.Verified) { push(WorkRoute("quality-compose", taskId, "Quality work")) }
                target.workRows("runs").forEach { run -> run.workText("id")?.let { id -> WorkRowDivider(); WorkRow("Run $id", icon = Icons.Outlined.Autorenew) { push(WorkRoute("quality-run", id, "Quality run")) } } }
                WorkRowDivider(); WorkRecordRow("Policy and record", target, icon = Icons.Outlined.Article)
            }
        }
        if (state.value != null) item { WorkPager(page, state.value.workField("pagination").workBool("has_more"), { page = it }) }
    }
}

@Composable
internal fun WorkGuidanceProposals(push: (WorkRoute) -> Unit) {
    var page by remember { mutableIntStateOf(0) }; var prepared by remember { mutableStateOf<JsonElement?>(null) }
    val resource = rememberWorkResource("$WORK_QUALITY_API/guidance-proposals?limit=10&offset=${page * 10}")
    val state by resource.state.collectAsStateWithLifecycle()
    LaunchedEffect(state.identity) { prepared = null }
    val proposals = state.value.workRows("proposals")
    WorkPage {
        workStatusItem(state, resource)
        if (state.value != null && proposals.isEmpty()) item { WorkGroup { WorkEmptyState("No learning proposals.", icon = Icons.Outlined.Lightbulb) } }
        items(proposals) { proposal ->
            WorkHeaderCard(proposal.workField("guidance").workText("title") ?: "Guidance proposal",
                subtitle = listOfNotNull(proposal.workText("agent_id"), proposal.workText("candidate_id")).joinToString(" · "),
                detail = "Choose fresh evaluation and canary cases before approving a policy.") {
                WorkRow("Prepare qualification policy", icon = Icons.Outlined.Shield) {
                    val spec = (workPolicyInitial(WorkPolicyKind.Learning) as JsonObject).toMutableMap()
                    spec["name"] = proposal.workField("guidance").workField("title") ?: JsonPrimitive("")
                    spec["candidate_id"] = proposal.workField("candidate_id") ?: JsonPrimitive("")
                    spec["agent_id"] = proposal.workField("agent_id") ?: JsonPrimitive("")
                    spec["training_run_ids"] = proposal.workField("training_run_ids") ?: JsonArray(emptyList())
                    prepared = buildJsonObject { put("spec", JsonObject(spec)) }
                }
                proposal.workRows("training_run_ids").forEach { run -> (run as? JsonPrimitive)?.contentOrNull?.let { id -> WorkRowDivider(); WorkRow("Training run $id", icon = Icons.Outlined.Autorenew) { push(WorkRoute("quality-run", id, "Quality run")) } } }
                WorkRowDivider(); WorkRecordRow("Proposal record", proposal, icon = Icons.Outlined.Article)
            }
        }
        if (state.value != null) item { WorkPager(page, state.value.workField("pagination").workBool("has_more"), { page = it }) }
    }
    prepared?.let { WorkPolicyEditor(WorkPolicyKind.Learning, it, onClose = { prepared = null }) }
}
