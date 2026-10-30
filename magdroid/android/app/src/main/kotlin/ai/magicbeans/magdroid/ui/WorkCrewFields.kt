package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.*
import ai.magicbeans.magdroid.chat.ChatModelChoice
import ai.magicbeans.magdroid.chat.ChatModelSelection
import kotlinx.serialization.json.decodeFromJsonElement
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.AddCircle
import androidx.compose.material.icons.outlined.Clear
import androidx.compose.material.icons.outlined.DeleteOutline
import androidx.compose.material.icons.outlined.Groups
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.Memory
import androidx.compose.material.icons.outlined.TripOrigin
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.foundation.selection.toggleable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.serialization.json.*

/** Display labels for stored enum values; the stored values never change. */
private val TRUST_LABELS = listOf("local" to "Local", "reviewed" to "Reviewed", "builtin" to "Built-in", "untrusted" to "Untrusted")
private val PRIORITY_LABELS = listOf("high" to "High", "medium" to "Medium", "low" to "Low")
private val KIND_LABELS = listOf("worker" to "Worker", "personal" to "Personal")
private val ISOLATION_LABELS = listOf("shared" to "Shared", "fully_isolated" to "Fully isolated")

@Composable
private fun CrewText(label: String, value: JsonObject, path: List<String>, change: (JsonObject) -> Unit, list: Boolean = false, multiline: Boolean = false, enabled: Boolean = true, tagPrefix: String = "") {
    val field = value.crewAt(*path.toTypedArray())
    val text = if (list) (field as? JsonArray)?.joinToString(", ") { (it as? JsonPrimitive)?.content.orEmpty() }.orEmpty() else (field as? JsonPrimitive)?.contentOrNull.orEmpty()
    WorkLabeledField(label, text, { input -> change(value.crewSet(path, if (list) JsonArray(input.split(',').map(String::trim).filter(String::isNotEmpty).map(::JsonPrimitive)) else JsonPrimitive(input))) },
        enabled = enabled, fieldModifier = Modifier.testTag("work-crew-field-$tagPrefix${path.joinToString("-")}"), minLines = if (multiline) 3 else 1, maxLines = if (multiline) 8 else 2)
}

@Composable
private fun CrewChoice(label: String, value: JsonObject, path: List<String>, options: List<Pair<String, String>>, fallback: String, change: (JsonObject) -> Unit) {
    // The server stores enum values lowercase (`personal`); a stored value in
    // another case still selects its option.
    val current = (value.crewAt(*path.toTypedArray()) as? JsonPrimitive)?.content ?: fallback
    WorkChoiceRow(label, options, current, { option ->
        var next = value.crewSet(path, JsonPrimitive(option))
        if (path == listOf("kind") && option == "worker") for (key in listOf("harness", "autonomous_config", "user_memory_isolation", "readable_agents")) next = next.crewSet(listOf(key), null)
        change(next)
    })
}

@Composable
private fun CrewNumber(label: String, value: JsonObject, path: List<String>, range: IntRange, fallback: Int, change: (JsonObject) -> Unit) {
    val current = (value.crewAt(*path.toTypedArray()) as? JsonPrimitive)?.intOrNull ?: fallback
    WorkStepperRow(label, current, range, { change(value.crewSet(path, JsonPrimitive(it))) })
}

@Composable
private fun CrewOptional(label: String, key: String, value: JsonObject, initial: JsonObject, change: (JsonObject) -> Unit) {
    WorkSwitchRow(label, value[key] is JsonObject, { change(value.crewSet(listOf(key), if (it) initial else null)) })
}

/**
 * A focus area's shared documents: the global list (key absent), none (`[]`),
 * or chosen program files that replace the global list for this area.
 */
@Composable
private fun FocusSharedDocuments(index: Int, area: JsonObject, edit: (JsonObject) -> Unit, programState: WorkResourceState, programs: WorkResource) {
    val stored = area.focusSharedDocs()
    // "Choose documents" with nothing checked stores `[]`, which reads back as None;
    // this keeps the checklist open while the owner picks.
    var choosing by remember(index) { mutableStateOf(false) }
    var lastChosen by remember(index) { mutableStateOf(area.focusSharedNames()) }
    val mode = if (stored == FocusSharedDocs.None && choosing) FocusSharedDocs.Chosen else stored
    val selected = area.focusSharedNames()
    val tag = "work-crew-focus-$index-shared"
    val modes = listOf(FocusSharedDocs.Global to "Use global list", FocusSharedDocs.None to "None", FocusSharedDocs.Chosen to "Choose documents")
    WorkChoiceRow("Shared documents", modes.map { it.first.name to it.second }, mode.name, { name ->
        val option = FocusSharedDocs.valueOf(name)
        choosing = option == FocusSharedDocs.Chosen
        edit(area.withFocusSharedDocs(option, if (option == FocusSharedDocs.Chosen) lastChosen else selected))
    }, modifier = Modifier.testTag("$tag-mode"), optionTag = { "$tag-${it.lowercase()}" })
    Text(when (mode) {
        FocusSharedDocs.Global -> "Uses the global shared documents list from the runtime configuration."
        FocusSharedDocs.None -> "This area gets no shared documents."
        FocusSharedDocs.Chosen -> "The checked documents replace the global list for this area."
    }, color = Muted, fontSize = 12.sp, lineHeight = 16.sp, modifier = Modifier.padding(bottom = 10.dp))
    if (mode != FocusSharedDocs.Chosen) return
    val toggle: (String, Boolean) -> Unit = { name, checked ->
        val next = area.toggleFocusSharedDoc(name, checked)
        lastChosen = next.focusSharedNames(); choosing = true; edit(next)
    }
    WorkResourceStatus(programState, programs)
    val rows = programState.value.workRows("programs").mapNotNull { row -> row.workText("name")?.let { it to (row.workText("title") ?: it) } }
    val loaded = programState.value != null
    rows.forEach { (name, title) ->
        val checked = name in selected
        WorkRowDivider()
        Row(
            Modifier.fillMaxWidth().heightIn(min = WORK_ROW_MIN_HEIGHT_DP.dp)
                .toggleable(value = checked, role = Role.Checkbox, onValueChange = { toggle(name, it) })
                .testTag("$tag-doc-$name")
                .padding(vertical = WORK_ROW_PADDING_V_DP.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Checkbox(checked, onCheckedChange = null, colors = magicanCheckboxColors())
            Column(Modifier.weight(1f).padding(start = 12.dp)) {
                Text(title, color = Ink, fontSize = 15.sp)
                if (title != name) Text(name, color = Muted, fontSize = 12.sp)
            }
        }
    }
    // Names saved on the definition that the program list does not have stay visible and removable.
    selected.filter { name -> rows.none { it.first == name } }.forEach { name ->
        WorkRowDivider()
        Row(Modifier.fillMaxWidth().heightIn(min = WORK_ROW_MIN_HEIGHT_DP.dp).testTag("$tag-missing-$name"), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(name, color = Ink, fontSize = 15.sp)
                Text(if (loaded) "Not found in this workspace" else "Checking…", color = if (loaded) MWarn else Muted, fontSize = 12.sp)
            }
            TextButton(onClick = { toggle(name, false) }, modifier = Modifier.heightIn(min = 48.dp)) { Text("Remove", color = Coral) }
        }
    }
    if (loaded && rows.isEmpty()) Text("No playbook documents in this workspace yet.", color = Muted, fontSize = 13.sp, modifier = Modifier.padding(bottom = 10.dp))
}

/** The focus areas of the autonomous cycle: a row each, expanding into an inset panel. */
@Composable
private fun FocusAreas(value: JsonObject, change: (JsonObject) -> Unit) {
    val areas = (value.crewAt("autonomous_config", "focus_areas") as? JsonArray)?.toList().orEmpty()
    val programs = rememberWorkResource("/api/magician/v2/programs")
    val programState by programs.state.collectAsState()
    val expanded = remember { mutableStateMapOf<Int, Boolean>() }
    WorkGroup("Focus areas") {
        areas.forEachIndexed { index, item ->
            val area = item as? JsonObject ?: buildJsonObject {}
            val edit: (JsonObject) -> Unit = { updated -> change(value.crewSet(listOf("autonomous_config", "focus_areas"), JsonArray(areas.toMutableList().apply { this[index] = updated }))) }
            val open = expanded[index] == true
            val tagPrefix = "autonomous_config-focus_areas-$index-"
            if (index > 0) WorkRowDivider()
            val priority = (area["priority"] as? JsonPrimitive)?.contentOrNull ?: "medium"
            WorkRow(
                area.text("name")?.takeIf(String::isNotBlank) ?: "Focus area ${index + 1}",
                Modifier.testTag("work-crew-focus-$index-toggle"),
                icon = Icons.Outlined.TripOrigin,
                value = PRIORITY_LABELS.firstOrNull { it.first.equals(priority, ignoreCase = true) }?.second ?: workLabel(priority),
                accessory = if (open) WorkRowAccessory.Expanded else WorkRowAccessory.Collapsed,
            ) { expanded[index] = !open }
            if (open) WorkInsetPanel(Modifier.padding(bottom = 12.dp)) {
                WorkFieldStack {
                    CrewText("Focus name", area, listOf("name"), edit, tagPrefix = tagPrefix)
                    CrewText("Focus description", area, listOf("description"), edit, multiline = true, tagPrefix = tagPrefix)
                }
                WorkRowDivider()
                CrewChoice("Priority", area, listOf("priority"), PRIORITY_LABELS, "medium", edit)
                WorkRowDivider()
                WorkFieldStack {
                    CrewText("Schedule override (cron)", area, listOf("schedule"), edit, tagPrefix = tagPrefix)
                    CrewText("Playbook document", area, listOf("program"), edit, tagPrefix = tagPrefix)
                    CrewText("Scope (comma-separated)", area, listOf("scope"), edit, list = true, tagPrefix = tagPrefix)
                }
                WorkRowDivider()
                FocusSharedDocuments(index, area, edit, programState, programs)
                WorkRowDivider()
                WorkActionRow("Remove focus area ${index + 1}", Icons.Outlined.DeleteOutline, destructive = true) {
                    val shifted = expanded.filterKeys { it != index }.mapKeys { (key, _) -> if (key > index) key - 1 else key }
                    expanded.clear(); expanded.putAll(shifted)
                    change(value.crewSet(listOf("autonomous_config", "focus_areas"), JsonArray(areas.toMutableList().apply { removeAt(index) })))
                }
            }
        }
        if (areas.isNotEmpty()) WorkRowDivider()
        WorkActionRow("Add focus area", Icons.Outlined.AddCircle) {
            expanded[areas.size] = true
            change(value.crewSet(listOf("autonomous_config", "focus_areas"), JsonArray(areas + buildJsonObject { put("name", ""); put("description", ""); put("priority", "medium") })))
        }
    }
}

/** A row that opens its section in place, under a divider of its own. */
@Composable
private fun CrewSection(label: String, icon: androidx.compose.ui.graphics.vector.ImageVector, open: Boolean, toggle: () -> Unit, content: @Composable ColumnScope.() -> Unit) {
    WorkRow(label, icon = icon, accessory = if (open) WorkRowAccessory.Expanded else WorkRowAccessory.Collapsed, onClick = toggle)
    if (open) Column(Modifier.fillMaxWidth().padding(bottom = 4.dp), content = content)
}

/** Full guided definition editing over the same draft used by advanced JSON. */
@Composable
internal fun WorkCrewFields(value: JsonObject, change: (JsonObject) -> Unit, editingId: String? = null) {
    var toolsOpen by remember { mutableStateOf(false) }
    var memoryOpen by remember { mutableStateOf(false) }
    var modelsOpen by remember { mutableStateOf(false) }
    Column(verticalArrangement = Arrangement.spacedBy(WORK_GROUP_SPACING_DP.dp)) {
        WorkGroup("Identity") {
            WorkFieldStack {
                CrewText("Member ID (optional on creation)", value, listOf("agent_id"), change, enabled = editingId == null)
                CrewText("Name", value, listOf("name"), change)
                CrewText("Description", value, listOf("description"), change, multiline = true)
                CrewText("Persona", value, listOf("persona"), change, multiline = true)
            }
            WorkRowDivider()
            CrewChoice("Kind", value, listOf("kind"), KIND_LABELS, "worker", change)
            WorkRowDivider()
            CrewChoice("Trust", value, listOf("trust_level"), TRUST_LABELS, "local", change)
        }
        if (value.isPersonalCrew()) {
            WorkGroup("Playbooks and cycles") {
                CrewOptional("Use Autopilot", "harness", value, buildJsonObject {}, change)
                if (value["harness"] is JsonObject) {
                    CrewText("Playbook section", value, listOf("harness", "program_section"), change)
                    Spacer(Modifier.height(12.dp))
                }
                WorkRowDivider()
                CrewOptional("Scheduled check-ins", "autonomous_config", value, buildJsonObject {
                    put("schedule", "0 */4 * * *"); put("focus_areas", JsonArray(emptyList())); put("max_tasks_per_cycle", 3); put("max_steps_per_plan", 10)
                }, change)
                if (value["autonomous_config"] is JsonObject) {
                    CrewText("Cycle schedule (cron)", value, listOf("autonomous_config", "schedule"), change)
                    Spacer(Modifier.height(12.dp))
                    WorkRowDivider()
                    CrewNumber("Tasks per cycle", value, listOf("autonomous_config", "max_tasks_per_cycle"), 1..10, 3, change)
                    WorkRowDivider()
                    CrewNumber("Steps per plan", value, listOf("autonomous_config", "max_steps_per_plan"), 1..30, 10, change)
                }
            }
            if (value["autonomous_config"] is JsonObject) FocusAreas(value, change)
        }
        WorkGroup("Delegation, memory and models") {
            CrewSection("Tools and delegation", Icons.Outlined.Groups, toolsOpen, { toolsOpen = !toolsOpen }) {
                WorkFieldStack {
                    CrewText("Tools (comma-separated)", value, listOf("tools"), change, list = true)
                    CrewText("Excluded tools (comma-separated)", value, listOf("excluded_tools"), change, list = true)
                    CrewText("Delegate to (IDs or *)", value, listOf("delegation_targets"), change, list = true)
                }
                WorkRowDivider()
                CrewNumber("Delegation depth", value, listOf("constraints", "coordination", "max_delegation_depth"), 1..10, 3, change)
            }
            if (value.isPersonalCrew()) {
                WorkRowDivider()
                CrewSection("Memory boundaries", Icons.Outlined.Lock, memoryOpen, { memoryOpen = !memoryOpen }) {
                    CrewChoice("User memory", value, listOf("user_memory_isolation"), ISOLATION_LABELS, "shared", change)
                    WorkRowDivider()
                    WorkFieldStack { CrewText("Readable members (comma-separated)", value, listOf("readable_agents"), change, list = true) }
                }
            }
            WorkRowDivider()
            CrewSection("Models", Icons.Outlined.Memory, modelsOpen, { modelsOpen = !modelsOpen }) { WorkCrewModels(value, change) }
        }
    }
}

@Composable
private fun CrewProfilePicker(label: String, selected: String, options: List<String>, labels: Map<String, String> = emptyMap(), change: (String) -> Unit) {
    var open by remember { mutableStateOf(false) }
    var search by remember { mutableStateOf("") }
    WorkPickerRow(label, (labels[selected] ?: selected).ifBlank { "Default — global routing" }) { search = ""; open = true }
    if (open) AlertDialog(onDismissRequest = { open = false }, title = { Text(label) }, text = {
        Column(Modifier.heightIn(max = 420.dp)) {
            OutlinedTextField(search, { search = it }, label = { Text("Find a model") }, modifier = Modifier.fillMaxWidth())
            Column(Modifier.weight(1f, fill = false).verticalScroll(rememberScrollState())) {
                options.distinct().filter { it.isBlank() || (labels[it] ?: it).contains(search, ignoreCase = true) }.forEach { option ->
                    TextButton(onClick = { change(option); open = false }, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp)) {
                        Text((if (option == selected) "✓ " else "") + (labels[option] ?: option).ifBlank { "Default — global routing" }, color = Coral)
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
    val coding = rememberWorkResource("/api/magician/v2/coding/models")
    val codingState by coding.state.collectAsState()
    val lanes = listOf("planning", "evaluation", "correction_extraction", "memory_consolidation")
    val decoder = remember { Json { ignoreUnknownKeys = true } }
    val models = remember(profileState.value) { profileState.value.workRows("models").mapNotNull { runCatching { decoder.decodeFromJsonElement<ChatModelChoice>(it) }.getOrNull() } }
    val codingProfiles = remember(codingState.value) { codingState.value?.let {
        runCatching { decoder.decodeFromJsonElement<ai.magicbeans.magdroid.vibedev.VibeDevModelCatalog>(it).rows() }.getOrNull()
    }.orEmpty() }
    val codingModels = remember(codingProfiles) { codingProfiles.mapNotNull { it.modelChoice } }
    Column(Modifier.fillMaxWidth()) {
        Text("An unpinned member follows global routing. Direct and per-operation pins are preserved; edit those in the advanced definition.", color = Muted, fontSize = 13.sp, lineHeight = 17.sp, modifier = Modifier.padding(bottom = 8.dp))
        WorkResourceStatus(profileState, profiles)
        lanes.forEach { lane ->
            WorkRowDivider()
            val endpoint = value.crewAt("llm_routing", lane) as? JsonObject
            val saved = endpoint?.text("profile")?.takeIf(String::isNotBlank) ?: endpoint?.let { "direct:${it.text("provider").orEmpty()}/${it.text("model").orEmpty()}" }.orEmpty()
            val select: (String) -> Unit = { profile ->
                if (!profile.startsWith("direct:")) change(value.crewSet(listOf("llm_routing", lane), if (profile.isBlank()) null else buildJsonObject { put("profile", profile); put("provider", ""); put("model", "") }))
            }
                val selected = models.firstNotNullOfOrNull { model ->
                    (model.choices.entries.firstOrNull { it.value == saved }
                        ?: model.operationChoices?.entries?.firstOrNull { it.value == saved })
                        ?.let { ChatModelSelection(model.id, it.key) }
                } ?: profileState.value.workRows("profiles").firstOrNull { it.workText("name") == saved }?.workField("model_selection")
                    ?.let { runCatching { decoder.decodeFromJsonElement<ChatModelSelection>(it) }.getOrNull() }
                val model = models.firstOrNull { it.id == selected?.modelId }
                val labels = profileState.value.workRows("profiles").associate { it.workText("name").orEmpty() to "${it.workText("model")} · ${it.workText("provider")}" } + models.associate { it.id to "${it.label} · ${it.provider}" }
                CrewProfilePicker(workLabel(lane), model?.id ?: saved, listOf("") + listOf(saved).filter { it.isNotBlank() && model == null } + models.filter { it.installed }.map { it.id }, labels) { id ->
                    if (id.isBlank()) select("") else models.firstOrNull { it.id == id }?.let { next ->
                        val level = selected?.thinking?.takeIf { next.choices.containsKey(it) } ?: "auto"
                        (next.operationChoices ?: next.choices)[level]?.let(select)
                    }
                }
                model?.let { active -> Column(Modifier.padding(bottom = 10.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) { ModelThinkingControl(active.thinking, selected?.thinking ?: "auto") { level -> (active.operationChoices ?: active.choices)[level]?.let(select) } } }
        }
        WorkRowDivider()
        WorkResourceStatus(codingState, coding)
        val savedCoding = (value.crewAt("llm_routing", "coding_profile") as? JsonPrimitive)?.content.orEmpty()
        val setCoding: (String) -> Unit = { profile ->
            change(value.crewSet(listOf("llm_routing", "coding_profile"), if (profile.isBlank()) null else JsonPrimitive(profile)))
        }
        if (codingModels.isNotEmpty()) {
            val selected = codingProfiles.firstOrNull { it.id == savedCoding }?.modelSelection
            val model = codingModels.firstOrNull { it.id == selected?.modelId }
            val modelEngines = codingProfiles.filter { it.modelChoice != null }.map { it.engine }.toSet()
            val engines = codingProfiles.filter { it.engine !in modelEngines && it.selectable && it.id != "auto" }
            val options = (listOf("") + listOf(savedCoding).filter { it.isNotBlank() && model == null } + codingModels.map { it.id } + engines.map { it.id }).distinct()
            val labels = codingModels.associate { it.id to "${it.label} · ${it.provider}" } + codingProfiles.associate { it.id to it.label } +
                if (savedCoding.isNotBlank() && codingProfiles.none { it.id == savedCoding }) mapOf(savedCoding to "Unavailable coding model · saved") else emptyMap()
            CrewProfilePicker("Coding model or engine", model?.id ?: savedCoding, options, labels) { id ->
                val next = codingModels.firstOrNull { it.id == id }
                if (next == null) setCoding(id) else (next.choices[selected?.thinking ?: "auto"] ?: next.choices["auto"])?.let(setCoding)
            }
            model?.let { active -> Column(Modifier.padding(bottom = 10.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) { ModelThinkingControl(active.thinking, selected?.thinking ?: "auto") { level -> active.choices[level]?.let(setCoding) } } }
        } else {
            val options = (listOf("") + listOf(savedCoding).filter(String::isNotBlank) + codingProfiles.filter { it.selectable && it.id != "auto" }.map { it.id }).distinct()
            CrewProfilePicker("Coding engine model", savedCoding, options, codingProfiles.associate { it.id to it.label }, change = setCoding)
        }
        (value.crewAt("llm_routing", "operations") as? JsonObject)?.takeIf { it.isNotEmpty() }?.let {
            WorkRowDivider()
            Column(Modifier.padding(vertical = 10.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) { Text("Per-operation pins", color = Muted, fontSize = 13.sp, fontWeight = androidx.compose.ui.text.font.FontWeight.SemiBold); WorkRecord(it) }
        }
        WorkRowDivider()
        WorkActionRow("Clear lane and coding pins", Icons.Outlined.Clear, destructive = true) {
            var next = value
            for (key in lanes + "coding_profile") next = next.crewSet(listOf("llm_routing", key), null)
            if ((next["llm_routing"] as? JsonObject)?.isEmpty() == true) next = next.crewSet(listOf("llm_routing"), null)
            change(next)
        }
    }
}
