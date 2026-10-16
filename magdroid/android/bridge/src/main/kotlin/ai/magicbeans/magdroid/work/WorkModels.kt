package ai.magicbeans.magdroid.work

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull

/** The same navigation choices as web and iOS, over the existing records. */
enum class WorkSection(val title: String) {
    All("All work"), NeedsYou("Needs you"), Crew("Crew"), Programs("Programs"),
    Quality("Quality and review"), Operations("Company loop"),
}

/** Raw definitions are retained so a native edit never discards unknown fields. */
data class WorkCrewMember(
    val id: String,
    val definition: JsonObject,
    val etag: String,
    val status: String,
    val cycleId: String? = null,
    val executionId: String? = null,
) {
    val cycleTaskId: String? get() = cycleId?.takeIf(String::isNotBlank)?.let { cycle -> executionId?.takeIf(String::isNotBlank)?.let { "agent-cycle:$id:$cycle" } }
    val name: String get() = definition.text("name") ?: id
    val kind: String get() = definition.text("kind").orEmpty()
    val description: String get() = definition.text("description").orEmpty()
    val disabled: Boolean get() = (definition["disabled"] as? JsonPrimitive)?.booleanOrNull ?: false
    val isPrimary: Boolean get() = (definition["is_primary"] as? JsonPrimitive)?.booleanOrNull ?: false

    companion object {
        fun decode(record: JsonObject, headerEtag: String = ""): WorkCrewMember {
            val definition = record["definition"] as? JsonObject ?: record
            val id = definition.text("agent_id") ?: definition.text("id")
            require(!id.isNullOrBlank()) { "Crew response is missing its identity." }
            return WorkCrewMember(id, definition, record.text("etag") ?: headerEtag,
                record.text("status") ?: "unknown", record.text("current_cycle_id")?.trim(), record.text("current_execution_id")?.trim())
        }
    }
}

/** Find a complete cycle through an explicit edge without recursive expansion. */
fun crewDelegationCycle(origin: String, target: String, members: List<WorkCrewMember>): List<String>? {
    val graph = members.associate { it.id to (it.definition["delegation_targets"] as? JsonArray)?.mapNotNull { (it as? JsonPrimitive)?.contentOrNull }.orEmpty() }
    if (target !in graph) return null
    val parents = mutableMapOf<String, String?>(target to null)
    val queue = java.util.ArrayDeque<String>(); queue.add(target)
    while (queue.isNotEmpty()) {
        val node = queue.removeFirst()
        if (node == origin) {
            val path = mutableListOf<String>(); var step: String? = node
            while (step != null) { path.add(step); step = parents[step] }
            return listOf(origin) + path.reversed()
        }
        graph[node].orEmpty().filter { it in graph && it !in parents }.forEach { parents[it] = node; queue.add(it) }
    }
    return null
}

data class WorkProgramSummary(val name: String, val title: String)
data class WorkProgramVersion(val snapshot: String, val modified: String)
data class WorkProgramDocument(
    val name: String, val title: String, val content: String, val history: List<WorkProgramVersion>,
) {
    companion object {
        fun decode(value: JsonObject): WorkProgramDocument = WorkProgramDocument(
            requireNotNull(value.text("name")), requireNotNull(value.text("title")),
            requireNotNull(value.text("content")),
            (value["history"] as? JsonArray)?.map { item ->
                val version = item as JsonObject
                WorkProgramVersion(requireNotNull(version.text("snapshot")), requireNotNull(version.text("modified")))
            } ?: emptyList(),
        )
    }
}

fun JsonObject.text(key: String): String? = (get(key) as? JsonPrimitive)?.contentOrNull
