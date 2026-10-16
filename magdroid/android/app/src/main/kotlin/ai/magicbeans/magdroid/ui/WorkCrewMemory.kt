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
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

@Composable
internal fun WorkCrewRules(agentId: String) {
    val resource = rememberWorkResource("/api/magician/v2/agents/${WorkRepository.segment(agentId)}")
    val state by resource.state.collectAsStateWithLifecycle()
    val definition = (state.value as? JsonObject)?.let { runCatching { WorkCrewMember.decode(it).definition }.getOrNull() }
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { WorkResourceStatus(state, resource) }
        definition?.let { fields -> items(listOf("persona", "goals", "triggers", "constraints", "memory_tiers", "memory_consolidation", "prompt_pipeline", "circuit_breaker", "feedback_loops", "notification_rules", "retention", "llm_routing", "observation", "strategy", "state_machines", "delegation_targets", "coordination")) { key ->
            WorkCard { Text(workLabel(key), color = Ink, fontSize = 17.sp); if (fields[key] == null || fields[key] == JsonNull) Text("Not configured", color = Muted) else WorkRecord(fields[key]) }
        } }
    }
}

@Composable
internal fun WorkCrewMemory(agentId: String, push: (WorkRoute) -> Unit) {
    val base = "/api/magician/v2/agents/${WorkRepository.segment(agentId)}"
    val resource = rememberWorkResource("$base/memory")
    val state by resource.state.collectAsStateWithLifecycle()
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { WorkResourceStatus(state, resource); WorkLink("Refresh memory", resource::refresh) }
        if (state.value != null && state.value.workRows("tiers").isEmpty()) item { Text("No declared memory tiers.", color = Muted) }
        items(state.value.workRows("tiers")) { tier -> tier.workText("tier_name")?.let { name -> WorkCard {
            Text(workLabel(name), color = Ink, fontSize = 18.sp)
            Text(tier.workText("description").orEmpty(), color = Muted, fontSize = 14.sp)
            Text("${tier.workText("scope").orEmpty()} · ${if (tier.workBool("shared")) "Shared" else "Private"} · ${if (tier.workBool("has_data")) "Has data" else "Empty"}", color = Muted, fontSize = 12.sp)
            WorkLink("Inspect $name") { push(WorkRoute("memory-tier", agentId, "Memory tier", tier = name, goalId = tier.workText("goal_id") ?: tier.workRows("available_goal_ids").firstOrNull()?.workDisplay().orEmpty())) }
        } } }
        item {
            WorkLink("Consolidation health") { push(WorkRoute("endpoint", "$base/memory/consolidation-health", "Consolidation health")) }
            WorkLink("User memory and sharing") { push(WorkRoute("user-memory", agentId, "User memory")) }
        }
    }
}

@Composable
internal fun WorkCrewTier(agentId: String, tier: String, initialGoal: String) {
    val base = "/api/magician/v2/agents/${WorkRepository.segment(agentId)}/memory"
    val summary = rememberWorkResource(base)
    val summaryState by summary.state.collectAsStateWithLifecycle()
    var goal by remember { mutableStateOf(initialGoal) }
    val path = "$base/${WorkRepository.segment(tier)}" + if (goal.isBlank()) "" else "?goal_id=${WorkRepository.segment(goal)}"
    val resource = rememberWorkResource(path)
    val state by resource.state.collectAsStateWithLifecycle()
    val entry = summaryState.value.workRows("tiers").firstOrNull { it.workText("tier_name") == tier }
    val goals = (entry.workRows("available_goal_ids") + state.value.workRows("available_goal_ids")).mapNotNull { (it as? JsonPrimitive)?.contentOrNull }.distinct()
    var choosingGoal by remember { mutableStateOf(false) }
    LaunchedEffect(goals) { if (goal.isBlank() && goals.isNotEmpty()) goal = goals.first() }
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { WorkResourceStatus(summaryState, summary); if (goals.isNotEmpty()) WorkLink("Goal context: $goal") { choosingGoal = true }; WorkResourceStatus(state, resource) }
        state.value?.let { value ->
            item { WorkCard {
                Text(workLabel(tier), color = Ink, fontSize = 20.sp)
                Text(value.workField("tier").workText("description").orEmpty(), color = Muted, fontSize = 14.sp)
                Text("Scope: ${value.workField("tier").workText("scope").orEmpty()} · ${if (value.workBool("shared")) "Shared" else "Private"}", color = Muted, fontSize = 12.sp)
                value.workText("goal_id")?.let { Text("Goal: $it", color = Ink) }
            } }
            item { WorkCard { Text("Persisted memory", color = Ink, fontSize = 18.sp); WorkRecord(value.workField("data")) } }
            item { WorkCard { Text("Sharing and consolidation", color = Ink, fontSize = 18.sp); WorkRecord(value.workField("shared_with_agents")); WorkRecord(value.workField("consolidation_rules")) } }
        }
    }
    if (choosingGoal) AlertDialog(onDismissRequest = { choosingGoal = false }, title = { Text("Goal context", color = Ink) }, text = {
        LazyColumn { items(goals) { choice -> WorkLink(choice) { goal = choice; choosingGoal = false } } }
    }, confirmButton = { TextButton(onClick = { choosingGoal = false }) { Text("Close", color = Coral) } }, containerColor = Ground)
}

@Composable
internal fun WorkCrewUserMemory(agentId: String, push: (WorkRoute) -> Unit) {
    val base = "/api/magician/v2/agents/${WorkRepository.segment(agentId)}"
    val resource = rememberWorkResource("$base/memory")
    val state by resource.state.collectAsStateWithLifecycle()
    val member = rememberWorkResource(base)
    val memberState by member.state.collectAsStateWithLifecycle()
    val definition = memberState.value.workField("definition")
    val isolated = definition.workText("kind") == "Personal" && (definition.workText("user_memory_isolation") ?: "shared") != "shared"
    val userTiers = state.value.workRows("tiers").filter { it.workText("scope") == "user" }
    val hasData = userTiers.any { it.workBool("has_data") }
    val scope = rememberCoroutineScope()
    var confirm by remember { mutableStateOf(false) }
    LaunchedEffect(state.identity) { confirm = false }
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { WorkResourceStatus(state, resource); WorkLink("Refresh user memory", resource::refresh) }
        item { WorkResourceStatus(memberState, member) }
        if (state.value != null) item { WorkCard { Text("User memory", color = Ink, fontSize = 18.sp); userTiers.forEach { tier -> tier.workText("tier_name")?.let { name -> WorkLink(workLabel(name)) { push(WorkRoute("memory-tier", agentId, "Memory tier", goalId = tier.workText("goal_id").orEmpty(), tier = name)) } }; WorkRecord(tier) }; if (userTiers.isEmpty()) Text("No user memory tiers were reported.", color = Muted) } }
        if (state.value != null && isolated) item { if (!hasData) Text("No local user memory is available to merge.", color = Muted, fontSize = 13.sp); TextButton(onClick = { confirm = true }, enabled = !state.busy && hasData, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp)) { Text("Merge into shared user memory", color = Coral) } }
    }
    if (confirm) AlertDialog(onDismissRequest = { confirm = false }, title = { Text("Merge local user memory?", color = Ink) }, text = { Text("Shared user memory can be read by other eligible members in this workspace.", color = Muted) },
        confirmButton = { TextButton(onClick = { val identity = state.identity; confirm = false; scope.launch { if (resource.perform("$base/consolidate-user-memory", identity, body = null) != null) resource.refresh() } }, enabled = !state.busy) { Text("Merge reviewed memory", color = Coral) } },
        dismissButton = { TextButton(onClick = { confirm = false }) { Text("Cancel", color = Muted) } }, containerColor = Ground)
}
