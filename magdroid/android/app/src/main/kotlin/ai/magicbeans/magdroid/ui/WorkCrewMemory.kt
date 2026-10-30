package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.*
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.AccountCircle
import androidx.compose.material.icons.outlined.AccountTree
import androidx.compose.material.icons.outlined.Flag
import androidx.compose.material.icons.automirrored.outlined.CallMerge
import androidx.compose.material.icons.outlined.Groups
import androidx.compose.material.icons.outlined.Psychology
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

private val CREW_RULE_SECTIONS = listOf("persona", "goals", "triggers", "constraints", "memory_tiers", "memory_consolidation", "prompt_pipeline", "circuit_breaker", "feedback_loops", "notification_rules", "retention", "llm_routing", "observation", "strategy", "state_machines", "delegation_targets", "coordination")

/** Configured sections as titled cards; the rest listed once under "Not configured". */
@Composable
internal fun WorkCrewRules(agentId: String) {
    val resource = rememberWorkResource("/api/magician/v2/agents/${WorkRepository.segment(agentId)}")
    val state by resource.state.collectAsStateWithLifecycle()
    val definition = (state.value as? JsonObject)?.let { runCatching { WorkCrewMember.decode(it).definition }.getOrNull() }
    WorkPage {
        workStatusItem(state, resource)
        definition?.let { fields ->
            val configured = CREW_RULE_SECTIONS.filter { fields[it] != null && fields[it] != JsonNull }
            items(configured) { key -> WorkGroup(workLabel(key)) { WorkRecord(fields[key]) } }
            val missing = CREW_RULE_SECTIONS - configured.toSet()
            if (missing.isNotEmpty()) item { WorkGroup("Not configured") {
                missing.forEachIndexed { index, key ->
                    if (index > 0) WorkRowDivider()
                    WorkRow(workLabel(key), value = "Not configured", accessory = WorkRowAccessory.None)
                }
            } }
        }
    }
}

@Composable
internal fun WorkCrewMemory(agentId: String, push: (WorkRoute) -> Unit) {
    val base = "/api/magician/v2/agents/${WorkRepository.segment(agentId)}"
    val resource = rememberWorkResource("$base/memory")
    val state by resource.state.collectAsStateWithLifecycle()
    val tiers = state.value.workRows("tiers").mapNotNull { tier -> tier.workText("tier_name")?.let { it to tier } }
    WorkPage {
        workStatusItem(state, resource)
        if (state.value != null && tiers.isEmpty()) item { WorkGroup { WorkEmptyState("No declared memory tiers.", icon = Icons.Outlined.AccountTree) } }
        if (tiers.isNotEmpty()) item { WorkGroup("Memory tiers") {
            tiers.forEachIndexed { index, (name, tier) ->
                if (index > 0) WorkRowDivider()
                val meta = "${tier.workText("scope").orEmpty()} · ${if (tier.workBool("shared")) "Shared" else "Private"} · ${if (tier.workBool("has_data")) "Has data" else "Empty"}"
                WorkRow(workLabel(name), icon = Icons.Outlined.AccountTree,
                    detail = listOfNotNull(tier.workText("description")?.takeIf(String::isNotBlank), meta).joinToString("\n"),
                    onClickLabel = "Inspect $name") {
                    push(WorkRoute("memory-tier", agentId, "Memory tier", tier = name, goalId = tier.workText("goal_id") ?: tier.workRows("available_goal_ids").firstOrNull()?.workDisplay().orEmpty()))
                }
            }
        } }
        item { WorkGroup("Health and sharing") {
            WorkRow("Consolidation health", icon = Icons.Outlined.Psychology) { push(WorkRoute("endpoint", "$base/memory/consolidation-health", "Consolidation health")) }
            WorkRowDivider(); WorkRow("User memory and sharing", icon = Icons.Outlined.Groups) { push(WorkRoute("user-memory", agentId, "User memory")) }
        } }
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
    LaunchedEffect(goals) { if (goal.isBlank() && goals.isNotEmpty()) goal = goals.first() }
    WorkPage {
        workStatusItem(summaryState, summary, key = "summary-status")
        if (goals.isNotEmpty()) item { WorkGroup {
            WorkChoiceRow("Goal context", (listOfNotNull(goal.takeIf { it.isNotBlank() && it !in goals }) + goals).map { it to if (it in goals) it else "$it (not in current goals)" }, goal, { goal = it }, icon = Icons.Outlined.Flag)
        } }
        workStatusItem(state, resource)
        state.value?.let { value ->
            item { WorkHeaderCard(workLabel(tier), subtitle = value.workText("goal_id")?.let { "Goal: $it" }, detail = value.workField("tier").workText("description"),
                chips = listOf(
                    WorkChipSpec(workLabel(value.workField("tier").workText("scope").orEmpty().ifBlank { "unscoped" }), Muted),
                    WorkChipSpec(if (value.workBool("shared")) "Shared" else "Private", if (value.workBool("shared")) Coral else Muted),
                )) }
            item { WorkGroup("Persisted memory") { WorkRecord(value.workField("data")) } }
            item { WorkGroup("Sharing and consolidation") {
                WorkRecord(buildJsonObject { put("shared_with_agents", value.workField("shared_with_agents") ?: JsonNull); put("consolidation_rules", value.workField("consolidation_rules") ?: JsonNull) })
            } }
        }
    }
}

@Composable
internal fun WorkCrewUserMemory(agentId: String, push: (WorkRoute) -> Unit) {
    val base = "/api/magician/v2/agents/${WorkRepository.segment(agentId)}"
    val resource = rememberWorkResource("$base/memory")
    val state by resource.state.collectAsStateWithLifecycle()
    val member = rememberWorkResource(base)
    val memberState by member.state.collectAsStateWithLifecycle()
    val definition = memberState.value.workField("definition")
    val isolated = definition.workText("kind").equals("personal", ignoreCase = true) && (definition.workText("user_memory_isolation") ?: "shared") != "shared"
    val userTiers = state.value.workRows("tiers").filter { it.workText("scope") == "user" }
    val hasData = userTiers.any { it.workBool("has_data") }
    val scope = rememberCoroutineScope()
    var confirm by remember { mutableStateOf(false) }
    LaunchedEffect(state.identity) { confirm = false }
    WorkPage {
        workStatusItem(state, resource)
        workStatusItem(memberState, member, key = "member-status")
        if (state.value != null) item { WorkGroup("User memory") {
            userTiers.forEachIndexed { index, tier ->
                if (index > 0) WorkRowDivider()
                tier.workText("tier_name")?.let { name ->
                    WorkRow(workLabel(name), icon = Icons.Outlined.AccountCircle, value = if (tier.workBool("has_data")) "Has data" else "Empty",
                        detail = "${tier.workText("scope").orEmpty()} · ${if (tier.workBool("shared")) "Shared" else "Private"}") {
                        push(WorkRoute("memory-tier", agentId, "Memory tier", goalId = tier.workText("goal_id").orEmpty(), tier = name))
                    }
                }
            }
            if (userTiers.isEmpty()) WorkRow("No user memory tiers were reported.", icon = Icons.Outlined.AccountCircle, labelColor = Muted, accessory = WorkRowAccessory.None)
        } }
        if (state.value != null && isolated) item { Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            WorkGroup("Sharing") {
                WorkActionRow("Merge into shared user memory", Icons.AutoMirrored.Outlined.CallMerge, enabled = !state.busy && hasData) { confirm = true }
            }
            if (!hasData) WorkFootnote("No local user memory is available to merge.")
        } }
    }
    if (confirm) AlertDialog(onDismissRequest = { confirm = false }, title = { Text("Merge local user memory?", color = Ink) }, text = { Text("Shared user memory can be read by other eligible members in this workspace.", color = Muted) },
        confirmButton = { TextButton(onClick = { val identity = state.identity; confirm = false; scope.launch { if (resource.perform("$base/consolidate-user-memory", identity, body = null) != null) resource.refresh() } }, enabled = !state.busy) { Text("Merge reviewed memory", color = Coral) } },
        dismissButton = { TextButton(onClick = { confirm = false }) { Text("Cancel", color = Muted) } }, containerColor = Ground)
}
