package ai.magicbeans.magdroid.work

import kotlinx.serialization.json.*

/** Change only edited paths, retaining provider extensions and literal u64 values. */
fun JsonObject.crewAt(vararg path: String): JsonElement = path.fold(this as JsonElement) { value, key -> (value as? JsonObject)?.get(key) ?: JsonNull }
fun JsonObject.crewSet(path: List<String>, value: JsonElement?): JsonObject {
    require(path.isNotEmpty())
    val fields = toMutableMap()
    if (path.size == 1) { if (value == null) fields.remove(path[0]) else fields[path[0]] = value }
    else fields[path[0]] = ((fields[path[0]] as? JsonObject) ?: buildJsonObject {}).crewSet(path.drop(1), value)
    return JsonObject(fields)
}
fun newCrewDefinition() = buildJsonObject {
    put("agent_id", ""); put("name", ""); put("description", "")
    put("persona", "You are a practical autonomous assistant."); put("trust_level", "local"); put("kind", "worker")
    put("tools", JsonArray(emptyList())); put("excluded_tools", JsonArray(emptyList())); put("delegation_targets", JsonArray(emptyList()))
    put("constraints", buildJsonObject { put("coordination", buildJsonObject { put("max_delegation_depth", 3) }) })
}
/** A crew definition's `kind` is `personal` on the server (snake_case); older drafts may carry `Personal`. */
fun JsonObject.isPersonalCrew(): Boolean = text("kind").equals("personal", ignoreCase = true)
fun JsonObject.crewValidation(editingId: String? = null): String? {
    if (editingId != null && text("agent_id") != editingId) return "Keep the crew member identity unchanged."
    val id = text("agent_id").orEmpty()
    if (id.isNotEmpty() && !Regex("^[a-z0-9]+(?:-[a-z0-9]+)*$").matches(id)) return "Use lowercase letters, numbers and hyphens for the member ID."
    for (key in listOf("name", "persona", "trust_level")) if (text(key).isNullOrBlank()) return "${key.replace('_', ' ')} is required."
    if (text("kind")?.lowercase() !in listOf("personal", "worker")) return "Choose Personal or Worker."
    val depth = (crewAt("constraints", "coordination", "max_delegation_depth") as? JsonPrimitive)?.intOrNull ?: 3
    if (depth !in 1..10) return "Delegation depth must be between 1 and 10."
    val auto = get("autonomous_config") as? JsonObject
    if (auto != null) {
        if (!isPersonalCrew()) return "Autonomous cycles require a Personal member."
        fun cronValid(value: String?) = value?.trim()?.split(Regex("\\s+"))?.size == 5
        if (!cronValid(auto.text("schedule"))) return "Enter a five-field cron schedule for autonomous cycles."
        val areas = auto["focus_areas"] as? JsonArray
        if (areas.isNullOrEmpty()) return "Add at least one focus area."
        for (item in areas) {
            val area = item as? JsonObject ?: return "Each focus area needs a name and description."
            if (area.text("name").isNullOrBlank() || area.text("description").isNullOrBlank()) return "Each focus area needs a name and description."
            if (!area.text("schedule").isNullOrBlank() && !cronValid(area.text("schedule"))) return "Focus schedules need five cron fields."
        }
        if (((auto["max_tasks_per_cycle"] as? JsonPrimitive)?.intOrNull ?: 3) !in 1..10) return "Tasks per cycle must be between 1 and 10."
        if (((auto["max_steps_per_plan"] as? JsonPrimitive)?.intOrNull ?: 10) !in 1..30) return "Steps per plan must be between 1 and 30."
    }
    return null
}


/** RFC 7386 diff: removal in the editor must be explicit on the wire. */
fun JsonObject.crewMergePatch(original: JsonObject): JsonObject = buildJsonObject {
    for (key in this@crewMergePatch.keys + original.keys) {
        val next = this@crewMergePatch[key]
        val previous = original[key]
        if (next != previous) put(key, if (next is JsonObject && previous is JsonObject) next.crewMergePatch(previous) else next ?: JsonNull)
    }
}

/*
 * A focus area's shared program documents (`focus_areas[].shared_programs`).
 * Three states that must round-trip exactly: the key absent uses the global
 * `harness.shared_program_docs` list, `[]` gives the area none, and a
 * non-empty list replaces the global list with these program files.
 */
enum class FocusSharedDocs { Global, None, Chosen }

const val FOCUS_SHARED_PROGRAMS = "shared_programs"

/** The stored names, trimmed and de-duplicated in order; non-text entries are not names. */
fun JsonObject.focusSharedNames(): List<String> =
    (get(FOCUS_SHARED_PROGRAMS) as? JsonArray).orEmpty()
        .mapNotNull { (it as? JsonPrimitive)?.takeIf(JsonPrimitive::isString)?.content?.trim()?.takeIf(String::isNotEmpty) }
        .distinct()

/** Absent (or `null`) is the global list; an empty list is none. */
fun JsonObject.focusSharedDocs(): FocusSharedDocs = when (get(FOCUS_SHARED_PROGRAMS)) {
    is JsonArray -> if (focusSharedNames().isEmpty()) FocusSharedDocs.None else FocusSharedDocs.Chosen
    else -> FocusSharedDocs.Global
}

/**
 * Write a mode, leaving every other field alone: Global removes the key, None
 * writes `[]`, Chosen writes [names] (`[]` while nothing is checked yet, the
 * same as the web form).
 */
fun JsonObject.withFocusSharedDocs(mode: FocusSharedDocs, names: List<String> = focusSharedNames()): JsonObject = crewSet(
    listOf(FOCUS_SHARED_PROGRAMS),
    when (mode) {
        FocusSharedDocs.Global -> null
        FocusSharedDocs.None -> JsonArray(emptyList())
        FocusSharedDocs.Chosen -> JsonArray(names.map(String::trim).filter(String::isNotEmpty).distinct().map(::JsonPrimitive))
    },
)

/** Check or uncheck one document; the list keeps its order and appends new names. */
fun JsonObject.toggleFocusSharedDoc(name: String, checked: Boolean): JsonObject {
    val names = focusSharedNames().filter { it != name }
    return withFocusSharedDocs(FocusSharedDocs.Chosen, if (checked) names + name else names)
}
