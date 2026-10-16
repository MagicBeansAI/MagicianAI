package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.*
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.serialization.json.*

@Composable
internal fun WorkQualityTasks(push: (WorkRoute) -> Unit, onOpenTask: (String) -> Unit) {
    var page by remember { mutableIntStateOf(0) }; var goal by remember { mutableStateOf("") }
    val resource = rememberWorkResource("$WORK_QUALITY_API/work?limit=20&offset=${page * 20}&goal_id=${WorkRepository.segment(goal)}")
    val state by resource.state.collectAsStateWithLifecycle()
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { OutlinedTextField(goal, { goal = it; page = 0 }, label = { Text("Goal ID (optional)") }, modifier = Modifier.fillMaxWidth()); WorkResourceStatus(state, resource); WorkLink("Refresh tasks", resource::refresh) }
        items(state.value.workRows("targets")) { target -> target.workText("task_id")?.let { taskId -> WorkCard {
            Text(target.workText("title") ?: taskId, color = Ink, fontSize = 17.sp)
            WorkRecord(target)
            WorkLink("Task and full controls") { onOpenTask(taskId) }
            WorkLink("Review task quality policy") { push(WorkRoute("quality-compose", taskId, "Quality work")) }
            target.workRows("runs").forEach { run -> run.workText("id")?.let { id -> WorkLink("Run $id") { push(WorkRoute("quality-run", id, "Quality run")) } } }
        } } }
        item { Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            TextButton(onClick = { page-- }, enabled = page > 0) { Text("Previous", color = Coral) }; Text("Page ${page + 1}", color = Muted)
            TextButton(onClick = { page++ }, enabled = state.value.workField("pagination").workBool("has_more")) { Text("Next", color = Coral) }
        } }
    }
}

@Composable
internal fun WorkGuidanceProposals(push: (WorkRoute) -> Unit) {
    var page by remember { mutableIntStateOf(0) }; var prepared by remember { mutableStateOf<JsonElement?>(null) }
    val resource = rememberWorkResource("$WORK_QUALITY_API/guidance-proposals?limit=10&offset=${page * 10}")
    val state by resource.state.collectAsStateWithLifecycle()
    LaunchedEffect(state.identity) { prepared = null }
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { WorkResourceStatus(state, resource); WorkLink("Refresh proposals", resource::refresh) }
        items(state.value.workRows("proposals")) { proposal -> WorkCard {
            Text(proposal.workField("guidance").workText("title") ?: "Guidance proposal", color = Ink, fontSize = 17.sp)
            WorkRecord(proposal)
            Text("Choose fresh evaluation and canary cases before approving a policy.", color = Muted, fontSize = 13.sp)
            WorkLink("Prepare qualification policy") {
                val spec = (workPolicyInitial(WorkPolicyKind.Learning) as JsonObject).toMutableMap()
                spec["name"] = proposal.workField("guidance").workField("title") ?: JsonPrimitive("")
                spec["candidate_id"] = proposal.workField("candidate_id") ?: JsonPrimitive("")
                spec["agent_id"] = proposal.workField("agent_id") ?: JsonPrimitive("")
                spec["training_run_ids"] = proposal.workField("training_run_ids") ?: JsonArray(emptyList())
                prepared = buildJsonObject { put("spec", JsonObject(spec)) }
            }
            proposal.workRows("training_run_ids").forEach { run -> (run as? JsonPrimitive)?.contentOrNull?.let { id -> WorkLink("Training run $id") { push(WorkRoute("quality-run", id, "Quality run")) } } }
        } }
        item { Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            TextButton(onClick = { page-- }, enabled = page > 0) { Text("Previous", color = Coral) }; Text("Page ${page + 1}", color = Muted)
            TextButton(onClick = { page++ }, enabled = state.value.workField("pagination").workBool("has_more")) { Text("Next", color = Coral) }
        } }
    }
    prepared?.let { WorkPolicyEditor(WorkPolicyKind.Learning, it, onClose = { prepared = null }) }
}
