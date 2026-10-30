package ai.magicbeans.magdroid.work

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull

/** The same navigation choices as web and iOS, over the existing records. */
enum class WorkSection(val title: String) {
    All("All work"), NeedsYou("Needs you"), Crew("Crew"), Goals("Goals"), Programs("Playbooks"),
    Quality("Quality checks"), Operations("Team routines"),
}


/** Display copy; route identifiers and saved definition keys are unchanged. */
enum class WorkHelpTopic(val title: String, val explanation: String) {
    AllWork("All work", "Track tasks, scheduled checks and system activity in one place."),
    NeedsYou("Needs you", "Answer questions, review plans and unblock work waiting for your decision."),
    Crew("Crew", "Your AI team. Give each member a role, tools and responsibilities, then see what they are working on."),
    Goals("Goals", "Your goal workspaces: goals, the sub-goals under them and who owns each. Set one up from a template in chat."),
    Playbooks("Playbooks", "Reusable goals, instructions and success checks for your crew. Use them to guide ongoing work and track progress."),
    Quality("Quality checks", "Have agents review and improve an output against your criteria, then inspect the result before accepting it."),
    Operations("Team routines", "Control the shared schedule for company roles, start their sequence manually and inspect problems. Individual crew schedules have separate controls."),
    Tasks("Tasks", "Give an agent a specific outcome to deliver, once or on a schedule."),
    Monitors("Monitors", "Watch for changes on a schedule and notify you when a condition needs attention."),
    SystemWork("System work", "Background activity created by Magican, including crew cycles and supporting jobs."),
    Autopilot("Autopilot", "Let a crew member use its playbook to choose the next useful work. Permissions, budgets and approval rules still apply."),
    Cycles("Work cycles", "Scheduled check-ins where a crew member reviews its responsibilities and starts work within its task and step limits."),
    Focus("Focus areas", "A crew member’s ongoing responsibilities. Give each area a priority, instructions and an optional schedule."),
    Delegation("Delegation", "Choose which teammates this member can ask for help, and how far work can be passed along."),
    ;
    companion object {
        fun forLabel(label: String): WorkHelpTopic? {
            val text = label.lowercase()
            return when {
                "autopilot" in text -> Autopilot
                "team routine" in text -> Operations
                "playbook" in text -> Playbooks
                "focus area" in text -> Focus
                "cycle" in text || "scheduled check-in" in text -> Cycles
                "delegation" in text -> Delegation
                else -> entries.firstOrNull { it.title.lowercase() == text }
            }
        }
    }
}

val WorkSection.help: WorkHelpTopic get() = when (this) {
    WorkSection.All -> WorkHelpTopic.AllWork
    WorkSection.NeedsYou -> WorkHelpTopic.NeedsYou
    WorkSection.Crew -> WorkHelpTopic.Crew
    WorkSection.Goals -> WorkHelpTopic.Goals
    WorkSection.Programs -> WorkHelpTopic.Playbooks
    WorkSection.Quality -> WorkHelpTopic.Quality
    WorkSection.Operations -> WorkHelpTopic.Operations
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
/** [sha256] is the content hash an owner save sends back as its base; null from a server that predates editing. */
data class WorkProgramDocument(
    val name: String, val title: String, val content: String, val history: List<WorkProgramVersion>,
    val sha256: String? = null,
) {
    companion object {
        fun decode(value: JsonObject): WorkProgramDocument = WorkProgramDocument(
            requireNotNull(value.text("name")), requireNotNull(value.text("title")),
            requireNotNull(value.text("content")),
            (value["history"] as? JsonArray)?.map { item ->
                val version = item as JsonObject
                WorkProgramVersion(requireNotNull(version.text("snapshot")), requireNotNull(version.text("modified")))
            } ?: emptyList(),
            value.text("sha256")?.trim()?.takeIf(String::isNotEmpty),
        )
    }
}

/** `PUT /programs/{name}`'s result; [historySnapshot] holds the replaced version. */
data class WorkProgramSaved(
    val program: String, val created: Boolean, val unchanged: Boolean, val historySnapshot: String?, val sha256: String?,
) {
    companion object {
        fun decode(value: JsonObject): WorkProgramSaved = WorkProgramSaved(
            value.text("program").orEmpty(),
            (value["created"] as? JsonPrimitive)?.booleanOrNull ?: false,
            (value["unchanged"] as? JsonPrimitive)?.booleanOrNull ?: false,
            value.text("history_snapshot")?.takeIf(String::isNotBlank),
            value.text("sha256")?.takeIf(String::isNotBlank),
        )
    }
}

fun JsonObject.text(key: String): String? = (get(key) as? JsonPrimitive)?.contentOrNull
