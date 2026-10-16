package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.*
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.serialization.json.*

@Composable
private fun CrewText(label: String, value: JsonObject, path: List<String>, change: (JsonObject) -> Unit, list: Boolean = false, multiline: Boolean = false, enabled: Boolean = true, tagPrefix: String = "") {
    val field = value.crewAt(*path.toTypedArray())
    val text = if (list) (field as? JsonArray)?.joinToString(", ") { (it as? JsonPrimitive)?.content.orEmpty() }.orEmpty() else (field as? JsonPrimitive)?.contentOrNull.orEmpty()
    OutlinedTextField(text, { input -> change(value.crewSet(path, if (list) JsonArray(input.split(',').map(String::trim).filter(String::isNotEmpty).map(::JsonPrimitive)) else JsonPrimitive(input))) },
        label = { Text(label) }, enabled = enabled, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag("work-crew-field-$tagPrefix${path.joinToString("-")}"), minLines = if (multiline) 3 else 1, maxLines = if (multiline) 8 else 2)
}

@Composable
@OptIn(ExperimentalLayoutApi::class)
private fun CrewChoice(label: String, value: JsonObject, path: List<String>, options: List<String>, fallback: String, change: (JsonObject) -> Unit) {
    Text(label, color = Muted, fontSize = 13.sp)
    val current = (value.crewAt(*path.toTypedArray()) as? JsonPrimitive)?.content ?: fallback
    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        options.forEach { option -> FilterChip(selected = current == option, onClick = {
            var next = value.crewSet(path, JsonPrimitive(option))
            if (path == listOf("kind") && option == "Worker") for (key in listOf("harness", "autonomous_config", "user_memory_isolation", "readable_agents")) next = next.crewSet(listOf(key), null)
            change(next)
        }, label = { Text(option.replace('_', ' ')) }, modifier = Modifier.heightIn(min = 48.dp)) }
    }
}

@Composable
@OptIn(ExperimentalLayoutApi::class)
private fun CrewNumber(label: String, value: JsonObject, path: List<String>, range: IntRange, fallback: Int, change: (JsonObject) -> Unit) {
    val current = (value.crewAt(*path.toTypedArray()) as? JsonPrimitive)?.intOrNull ?: fallback
    Text("$label: $current", color = Ink, fontSize = 14.sp)
    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        TextButton(onClick = { change(value.crewSet(path, JsonPrimitive(current - 1))) }, enabled = current > range.first, modifier = Modifier.heightIn(min = 48.dp)) { Text("Decrease $label", color = Coral) }
        TextButton(onClick = { change(value.crewSet(path, JsonPrimitive(current + 1))) }, enabled = current < range.last, modifier = Modifier.heightIn(min = 48.dp)) { Text("Increase $label", color = Coral) }
    }
}

@Composable
private fun CrewOptional(label: String, key: String, value: JsonObject, initial: JsonObject, change: (JsonObject) -> Unit) {
    Row(Modifier.fillMaxWidth().heightIn(min = 48.dp)) {
        Text(label, color = Ink, modifier = Modifier.weight(1f).padding(top = 12.dp))
        Switch(value[key] is JsonObject, { change(value.crewSet(listOf(key), if (it) initial else null)) }, colors = magicanSwitchColors())
    }
}

/** Full guided definition editing over the same draft used by advanced JSON. */
@Composable
internal fun WorkCrewFields(value: JsonObject, change: (JsonObject) -> Unit, editingId: String? = null) {
    Column(verticalArrangement = Arrangement.spacedBy(14.dp)) {
        WorkCard {
            Text("Identity", color = Ink, fontSize = 18.sp)
            CrewText("Member ID (optional on creation)", value, listOf("agent_id"), change, enabled = editingId == null)
            CrewText("Name", value, listOf("name"), change)
            CrewText("Description", value, listOf("description"), change, multiline = true)
            CrewText("Persona", value, listOf("persona"), change, multiline = true)
            CrewChoice("Kind", value, listOf("kind"), listOf("Worker", "Personal"), "Worker", change)
            CrewChoice("Trust", value, listOf("trust_level"), listOf("local", "reviewed", "builtin", "untrusted"), "local", change)
        }
        WorkCard {
            Text("Tools and delegation", color = Ink, fontSize = 18.sp)
            CrewText("Tools (comma-separated)", value, listOf("tools"), change, list = true)
            CrewText("Excluded tools (comma-separated)", value, listOf("excluded_tools"), change, list = true)
            CrewText("Delegate to (IDs or *)", value, listOf("delegation_targets"), change, list = true)
            CrewNumber("delegation depth", value, listOf("coordination", "max_delegation_depth"), 1..10, 3, change)
        }
        if (value.text("kind") == "Personal") {
            WorkCard {
                Text("Programs and cycles", color = Ink, fontSize = 18.sp)
                CrewOptional("Use program harness", "harness", value, buildJsonObject {}, change)
                if (value["harness"] is JsonObject) CrewText("Program section", value, listOf("harness", "program_section"), change)
                CrewOptional("Autonomous cycles", "autonomous_config", value, buildJsonObject {
                    put("schedule", "0 */4 * * *"); put("focus_areas", JsonArray(emptyList())); put("max_tasks_per_cycle", 3); put("max_steps_per_plan", 10)
                }, change)
                if (value["autonomous_config"] is JsonObject) {
                    CrewText("Cycle schedule (cron)", value, listOf("autonomous_config", "schedule"), change)
                    CrewNumber("tasks per cycle", value, listOf("autonomous_config", "max_tasks_per_cycle"), 1..10, 3, change)
                    CrewNumber("steps per plan", value, listOf("autonomous_config", "max_steps_per_plan"), 1..30, 10, change)
                    val areas = (value.crewAt("autonomous_config", "focus_areas") as? JsonArray)?.toList().orEmpty()
                    areas.forEachIndexed { index, item ->
                        val area = item as? JsonObject ?: buildJsonObject {}
                        val edit: (JsonObject) -> Unit = { updated -> change(value.crewSet(listOf("autonomous_config", "focus_areas"), JsonArray(areas.toMutableList().apply { this[index] = updated }))) }
                        HorizontalDivider(color = BorderSoft)
                        Text("Focus area ${index + 1}", color = Ink, fontSize = 16.sp)
                        CrewText("Focus name", area, listOf("name"), edit, tagPrefix = "autonomous_config-focus_areas-$index-")
                        CrewText("Focus description", area, listOf("description"), edit, multiline = true, tagPrefix = "autonomous_config-focus_areas-$index-")
                        CrewChoice("Priority", area, listOf("priority"), listOf("high", "medium", "low"), "medium", edit)
                        CrewText("Schedule override (cron)", area, listOf("schedule"), edit, tagPrefix = "autonomous_config-focus_areas-$index-")
                        CrewText("Program document", area, listOf("program"), edit, tagPrefix = "autonomous_config-focus_areas-$index-")
                        CrewText("Scope (comma-separated)", area, listOf("scope"), edit, list = true, tagPrefix = "autonomous_config-focus_areas-$index-")
                        WorkLink("Remove focus area ${index + 1}") { change(value.crewSet(listOf("autonomous_config", "focus_areas"), JsonArray(areas.toMutableList().apply { removeAt(index) }))) }
                    }
                    WorkLink("Add focus area") { change(value.crewSet(listOf("autonomous_config", "focus_areas"), JsonArray(areas + buildJsonObject { put("name", ""); put("description", ""); put("priority", "medium") }))) }
                }
            }
            WorkCard {
                Text("Memory boundaries", color = Ink, fontSize = 18.sp)
                CrewChoice("User memory", value, listOf("user_memory_isolation"), listOf("shared", "fully_isolated"), "shared", change)
                CrewText("Readable members (comma-separated)", value, listOf("readable_agents"), change, list = true)
            }
        }
        WorkCrewModels(value, change)
    }
}

@Composable
private fun CrewProfilePicker(label: String, selected: String, options: List<String>, change: (String) -> Unit) {
    var open by remember { mutableStateOf(false) }
    var search by remember { mutableStateOf("") }
    Text(label, color = Muted, fontSize = 13.sp)
    OutlinedButton(onClick = { search = ""; open = true }, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp), colors = ButtonDefaults.outlinedButtonColors(contentColor = Coral), shape = MagicanButtonShape) {
        Text(selected.ifBlank { "Default — global routing" })
    }
    if (open) AlertDialog(onDismissRequest = { open = false }, title = { Text(label) }, text = {
        Column(Modifier.heightIn(max = 420.dp)) {
            OutlinedTextField(search, { search = it }, label = { Text("Find a profile") }, modifier = Modifier.fillMaxWidth())
            Column(Modifier.weight(1f, fill = false).verticalScroll(rememberScrollState())) {
                options.distinct().filter { it.isBlank() || it.contains(search, ignoreCase = true) }.forEach { option ->
                    TextButton(onClick = { change(option); open = false }, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp)) {
                        Text((if (option == selected) "✓ " else "") + option.ifBlank { "Default — global routing" }, color = Coral)
                    }
                }
            }
        }
    }, confirmButton = { TextButton(onClick = { open = false }) { Text("Cancel", color = Coral) } }, containerColor = Ground)
}

@Composable
@OptIn(ExperimentalLayoutApi::class)
private fun WorkCrewModels(value: JsonObject, change: (JsonObject) -> Unit) {
    val profiles = rememberWorkResource("/api/magician/v2/llm/routing")
    val profileState by profiles.state.collectAsState()
    val coding = rememberWorkResource("/api/magician/v2/coding/profiles")
    val codingState by coding.state.collectAsState()
    val lanes = listOf("planning", "evaluation", "correction_extraction", "memory_consolidation")
    WorkCard {
        Text("Models", color = Ink, fontSize = 18.sp)
        Text("An unpinned member follows global routing. Direct and per-operation pins are preserved; edit those in the advanced definition.", color = Muted, fontSize = 13.sp)
        WorkResourceStatus(profileState, profiles)
        lanes.forEach { lane ->
            val endpoint = value.crewAt("llm_routing", lane) as? JsonObject
            val saved = endpoint?.text("profile")?.takeIf(String::isNotBlank) ?: endpoint?.let { "direct:${it.text("provider").orEmpty()}/${it.text("model").orEmpty()}" }.orEmpty()
            val choices = listOf("") + listOf(saved).filter(String::isNotBlank) + profileState.value.workRows("profiles").filter { it.workBool("selectable") }.mapNotNull { it.workText("name") }.filter { it != saved }
            CrewProfilePicker(workLabel(lane), saved, choices) { profile ->
                if (!profile.startsWith("direct:")) change(value.crewSet(listOf("llm_routing", lane), if (profile.isBlank()) null else buildJsonObject { put("profile", profile); put("provider", ""); put("model", "") }))
            }
        }
        WorkResourceStatus(codingState, coding)
        val savedCoding = (value.crewAt("llm_routing", "coding_profile") as? JsonPrimitive)?.content.orEmpty()
        val codingOptions = (listOf("") + listOf(savedCoding).filter(String::isNotBlank) + codingState.value.workRows("profiles").filter { it.workField("selectable") != JsonPrimitive(false) }.mapNotNull { it.workText("id") }.filter { it != "auto" }).distinct()
        CrewProfilePicker("Coding engine model", savedCoding, codingOptions) { profile ->
            change(value.crewSet(listOf("llm_routing", "coding_profile"), if (profile.isBlank()) null else JsonPrimitive(profile)))
        }
        (value.crewAt("llm_routing", "operations") as? JsonObject)?.takeIf { it.isNotEmpty() }?.let { Text("Per-operation pins", color = Muted); WorkRecord(it) }
        WorkLink("Clear lane and coding pins") {
            var next = value
            for (key in lanes + "coding_profile") next = next.crewSet(listOf("llm_routing", key), null)
            if ((next["llm_routing"] as? JsonObject)?.isEmpty() == true) next = next.crewSet(listOf("llm_routing"), null)
            change(next)
        }
    }
}
