package ai.magicbeans.magdroid.work

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject

/** One stage of the team sequence: a harness agent's focus area. */
data class TeamRoutineTarget(val agentId: String, val goalId: String, val label: String)

/**
 * One stage per focus area of each harness agent, read from the live
 * definitions (`GET /api/magician/v2/agents`): a hard-coded list kept
 * triggering routines retired months ago. Agents in id order; two focus areas
 * whose names slug alike are one server goal, so one stage.
 */
fun teamRoutineTargets(agents: JsonElement?): List<TeamRoutineTarget> {
    val rows = ((agents as? JsonObject)?.get("agents") as? JsonArray) ?: (agents as? JsonArray) ?: return emptyList()
    val seen = mutableSetOf<String>()
    return rows.mapNotNull { (it as? JsonObject)?.get("definition") as? JsonObject }
        .filter { it["harness"] != null && it["harness"] !is JsonNull }
        .mapNotNull { definition -> definition.text("agent_id")?.let { it to definition } }
        .sortedBy { it.first }
        .flatMap { (agentId, definition) ->
            val areas = ((definition["autonomous_config"] as? JsonObject)?.get("focus_areas") as? JsonArray).orEmpty()
            areas.mapNotNull { area ->
                val name = (area as? JsonObject)?.text("name") ?: return@mapNotNull null
                val goalId = "harness:$agentId:${harnessGoalSlug(name)}"
                if (seen.add(goalId)) TeamRoutineTarget(agentId, goalId, name) else null
            }
        }
}

/**
 * The backend's `slugify_name`: ASCII letters and digits kept, every other
 * run one `-`, trimmed, `unnamed` when empty, cut to 64 at a hyphen.
 */
fun harnessGoalSlug(name: String): String {
    val collapsed = name.lowercase()
        .map { if (it in 'a'..'z' || it in '0'..'9') it else '-' }
        .joinToString("")
        .split('-').filter { it.isNotEmpty() }.joinToString("-")
    if (collapsed.isEmpty()) return "unnamed"
    if (collapsed.length <= 64) return collapsed
    val head = collapsed.substring(0, 64)
    val cut = head.lastIndexOf('-')
    return if (cut >= 0) head.substring(0, cut) else head
}
