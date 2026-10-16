package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tasks.PlanGraph
import ai.magicbeans.magdroid.tasks.PlanJson
import ai.magicbeans.magdroid.tasks.PlanKeyValue
import ai.magicbeans.magdroid.tasks.PlanParameter
import ai.magicbeans.magdroid.tasks.PlanStep
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.AddCircle
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material.icons.outlined.Remove
import androidx.compose.material.icons.outlined.RemoveCircle
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Slider
import androidx.compose.material3.SliderDefaults
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlin.math.roundToInt

/*
 * One step in a bottom sheet. Read-only by default; in edit mode every field
 * is editable — task, confidence, tool, parameters, expected outputs,
 * metadata, start step, and dependencies through a cycle-safe picker. Edits go
 * straight into the draft. Port of iOS `PlanStepDetailSheet.swift`.
 */

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun PlanStepSheet(controller: PlanInspectorController, stepId: String, onDismiss: () -> Unit) {
    val state = controller.read()
    val palette = activePalette
    val graph = state.shownGraph
    val step = graph?.step(stepId)
    val editing = state.isEditing
    var confirmRemove by remember { mutableStateOf(false) }
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = false),
        containerColor = palette.background,
        contentColor = palette.text,
    ) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 16.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Spacer(Modifier.widthIn(min = 72.dp))
            Text(
                if (editing) "Edit step" else "Step", style = planBodyStyle(17.sp, FontWeight.SemiBold),
                textAlign = TextAlign.Center, modifier = Modifier.weight(1f),
            )
            Box(
                Modifier
                    .widthIn(min = 72.dp)
                    .heightIn(min = Lively.tapMin)
                    .clickable(role = Role.Button, onClick = onDismiss),
                contentAlignment = Alignment.CenterEnd,
            ) { Text("Done", style = planBodyStyle(16.sp, FontWeight.SemiBold, palette.accent)) }
        }
        Column(
            Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .imePadding()
                .navigationBarsPadding()
                .padding(horizontal = 16.dp, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            if (step == null || graph == null) {
                PlanMessage("This step is no longer in the plan.")
            } else if (editing) {
                StepEditor(controller, step, graph, onRemove = { confirmRemove = true })
            } else {
                StepReadOnly(controller, step, graph)
            }
            Spacer(Modifier.height(16.dp))
        }
    }
    if (confirmRemove) ConfirmDialog(
        title = "Remove this step?",
        message = "Steps that depend on it lose that dependency. Nothing is saved until you tap Save.",
        confirm = "Remove step",
        onConfirm = {
            confirmRemove = false
            controller.mutate { editDraft { it.removeStep(stepId) } }
            onDismiss()
        },
        onDismiss = { confirmRemove = false },
    )
}

@Composable
private fun Section(title: String, footer: String? = null, content: @Composable ColumnScope.() -> Unit) {
    val palette = activePalette
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(title, style = planBodyStyle(13.sp, FontWeight.SemiBold, palette.secondaryText), modifier = Modifier.padding(start = 4.dp))
        Column(
            Modifier
                .fillMaxWidth()
                .background(palette.card, RoundedCornerShape(Lively.cardRadius))
                .padding(horizontal = 14.dp, vertical = 10.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
            content = content,
        )
        footer?.let { Text(it, style = planBodyStyle(12.sp, color = palette.secondaryText), modifier = Modifier.padding(start = 4.dp)) }
    }
}

@Composable
private fun FieldRow(label: String, value: String, color: Color? = null, mono: Boolean = false) {
    val palette = activePalette
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.Top) {
        Text(label, style = planBodyStyle(13.sp, color = palette.secondaryText))
        Spacer(Modifier.size(12.dp).weight(1f))
        SelectionContainer {
            Text(
                value,
                style = if (mono) planMonoStyle(13.sp, color = color ?: palette.text) else planBodyStyle(14.sp, color = color ?: palette.text),
                textAlign = TextAlign.End,
            )
        }
    }
}

@Composable
private fun StepReadOnly(controller: PlanInspectorController, step: PlanStep, graph: PlanGraph) {
    val state = controller.read()
    val palette = activePalette
    Section("Task") {
        Text(step.task.ifEmpty { "—" }, style = planBodyStyle(15.sp))
        state.runMarks[step.id]?.let { mark ->
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                StatusDot(PlanStyle.tone(mark))
                Text("This run: ${PlanStyle.markLabel(mark)}", style = planBodyStyle(13.sp))
            }
        }
    }
    Section("Execution") {
        FieldRow("Confidence", PlanStyle.percent(step.confidence), PlanStyle.bandColor(step.confidence))
        val agent = step.providingAgentId
        if (step.isDelegated && agent != null) FieldRow("Delegated to", agent)
        FieldRow(
            "Tool", if (step.hasResolvedTool) step.tool.orEmpty() else "Not resolved",
            color = if (step.hasResolvedTool) null else palette.warning, mono = step.hasResolvedTool,
        )
        FieldRow("Start step", if (step.isStart) "Yes" else "No")
    }
    if (step.dependsOn.isNotEmpty()) Section("Depends on") {
        for (dep in step.dependsOn) DependencyLine(graph, dep, step.id)
    }
    if (step.parameters.isNotEmpty()) Section("Parameters") {
        for (parameter in step.parameters) FieldRow(parameter.key, PlanJson.displayText(parameter.value), mono = true)
    }
    if (step.expectedOutputs.isNotEmpty()) Section("Expected outputs") {
        for (output in step.expectedOutputs) Text(output, style = planBodyStyle(14.sp))
    }
    if (step.metadata.isNotEmpty()) Section("Metadata") {
        for (entry in step.metadata) FieldRow(entry.key, entry.value)
    }
    Section("Identity") {
        FieldRow("ID", step.id, mono = true)
        step.stepHash?.let { FieldRow("Step hash", it, mono = true) }
    }
}

@Composable
private fun DependencyLine(graph: PlanGraph, dep: String, stepId: String, modifier: Modifier = Modifier) {
    Column(modifier, verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Text(PlanStyle.title(graph, dep), style = planBodyStyle(14.sp))
        graph.reason(dep, stepId)?.takeIf { it.isNotEmpty() }?.let {
            Text(it, style = planBodyStyle(12.sp, color = activePalette.secondaryText))
        }
    }
}

@Composable
private fun StepEditor(controller: PlanInspectorController, step: PlanStep, graph: PlanGraph, onRemove: () -> Unit) {
    val palette = activePalette
    val stepId = step.id
    fun update(change: (PlanStep) -> PlanStep) = controller.mutate { editDraft { it.updateStep(stepId, change) } }

    Section("Task") {
        MagicianTextField(
            value = step.task, onValueChange = { text -> update { it.copy(task = text) } },
            placeholder = { Text("What this step does") }, minLines = 2, maxLines = 6,
            modifier = Modifier.fillMaxWidth(),
        )
    }
    Section("Confidence · ${PlanStyle.percent(step.confidence)}") {
        Slider(
            value = step.confidence.toFloat(),
            onValueChange = { value -> update { it.copy(confidence = snap(value.toDouble())) } },
            valueRange = 0f..1f,
            steps = 19,
            colors = SliderDefaults.colors(
                thumbColor = palette.accent, activeTrackColor = palette.accent, inactiveTrackColor = palette.controlBorder,
                activeTickColor = Color.Transparent, inactiveTickColor = Color.Transparent,
            ),
        )
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("Adjust by 5%", style = planBodyStyle(13.sp), modifier = Modifier.weight(1f))
            IconButton(onClick = { update { it.copy(confidence = snap(it.confidence - 0.05)) } }) {
                Icon(Icons.Outlined.Remove, "Lower confidence by 5%", tint = palette.text)
            }
            IconButton(onClick = { update { it.copy(confidence = snap(it.confidence + 0.05)) } }) {
                Icon(Icons.Outlined.Add, "Raise confidence by 5%", tint = palette.text)
            }
        }
    }
    Section("Tool", footer = "Type the tool id. Leave it empty to let the planner resolve one.") {
        MagicianTextField(
            value = step.tool.orEmpty(),
            onValueChange = { text -> update { it.copy(tool = text.ifEmpty { null }) } },
            placeholder = { Text("Tool id (empty = unresolved)") }, singleLine = true,
            textStyle = planMonoStyle(14.sp),
            keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false),
            modifier = Modifier.fillMaxWidth(),
        )
    }
    DependencyEditor(controller, step, graph)
    ParameterEditor(step, update = ::update)
    OutputEditor(step, update = ::update)
    MetadataEditor(step, update = ::update)
    Row(
        Modifier
            .fillMaxWidth()
            .background(palette.card, RoundedCornerShape(Lively.cardRadius))
            .clickable(role = Role.Button, onClick = onRemove)
            .heightIn(min = Lively.tapMin)
            .padding(horizontal = 14.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Icon(Icons.Outlined.Delete, null, tint = palette.danger)
        Text("Remove step", style = planBodyStyle(15.sp, FontWeight.SemiBold, palette.danger))
    }
}

/** Snap to the slider's 0.05 steps, inside 0–1. */
private fun snap(value: Double): Double = ((value * 20).roundToInt() / 20.0).coerceIn(0.0, 1.0)

@Composable
private fun DependencyEditor(controller: PlanInspectorController, step: PlanStep, graph: PlanGraph) {
    val palette = activePalette
    val candidates = graph.dependencyCandidates(step.id)
    var picking by remember { mutableStateOf(false) }
    Section(
        "Depends on",
        footer = if (candidates.isEmpty() && graph.steps.isNotEmpty()) "No other step can be added without making a cycle."
        else "Only steps that cannot create a cycle are offered.",
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("Start step (no dependencies)", style = planBodyStyle(14.sp), modifier = Modifier.weight(1f))
            Switch(
                checked = step.isStart,
                onCheckedChange = { on -> if (on) controller.mutate { makeStart(step.id) } },
                enabled = !step.isStart,
                colors = SwitchDefaults.colors(checkedTrackColor = palette.accentFill, checkedThumbColor = palette.onAccent),
            )
        }
        for (dep in step.dependsOn) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                DependencyLine(graph, dep, step.id, Modifier.weight(1f))
                IconButton(onClick = { controller.mutate { setDependency(step.id, dep, false) } }) {
                    Icon(Icons.Outlined.RemoveCircle, "Remove dependency on ${PlanStyle.title(graph, dep)}", tint = palette.danger)
                }
            }
        }
        Box {
            Row(
                Modifier
                    .fillMaxWidth()
                    .heightIn(min = Lively.tapMin)
                    .clickable(enabled = candidates.isNotEmpty(), role = Role.Button) { picking = true },
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                val color = if (candidates.isEmpty()) palette.secondaryText else palette.accent
                Icon(Icons.Outlined.AddCircle, null, tint = color)
                Text("Add dependency", style = planBodyStyle(14.sp, FontWeight.SemiBold, color))
            }
            DropdownMenu(picking, onDismissRequest = { picking = false }) {
                for (candidate in candidates) {
                    DropdownMenuItem(
                        text = { Text(PlanStyle.truncate(PlanStyle.title(candidate), 48)) },
                        onClick = {
                            picking = false
                            controller.mutate { setDependency(step.id, candidate.id, true) }
                        },
                    )
                }
            }
        }
    }
}

@Composable
private fun ParameterEditor(step: PlanStep, update: ((PlanStep) -> PlanStep) -> Unit) {
    val palette = activePalette
    Section("Parameters") {
        for (parameter in step.parameters) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(parameter.key, style = planMonoStyle(13.sp, color = palette.secondaryText), modifier = Modifier.width(96.dp), maxLines = 2, overflow = TextOverflow.Ellipsis)
                MagicianTextField(
                    value = PlanJson.displayText(parameter.value),
                    onValueChange = { text ->
                        // The type the server sent decides: a non-string keeps its JSON type while the text parses.
                        val original = (step.raw["parameters"] as? JsonObject)?.get(parameter.key)
                        update { current ->
                            current.copy(parameters = current.parameters.map {
                                if (it.key == parameter.key) it.copy(value = PlanParameter.valueFromText(text, original)) else it
                            })
                        }
                    },
                    singleLine = true, textStyle = planMonoStyle(13.sp), modifier = Modifier.weight(1f),
                )
                IconButton(onClick = { update { current -> current.copy(parameters = current.parameters.filter { it.key != parameter.key }) } }) {
                    Icon(Icons.Outlined.RemoveCircle, "Remove ${parameter.key}", tint = palette.danger)
                }
            }
        }
        KeyValueAdder("parameter") { key, value ->
            update { current ->
                current.copy(parameters = current.parameters.filter { it.key != key } + PlanParameter(key, JsonPrimitive(value)))
            }
        }
    }
}

@Composable
private fun OutputEditor(step: PlanStep, update: ((PlanStep) -> PlanStep) -> Unit) {
    val palette = activePalette
    Section("Expected outputs") {
        step.expectedOutputs.forEachIndexed { index, output ->
            Row(verticalAlignment = Alignment.CenterVertically) {
                MagicianTextField(
                    value = output,
                    onValueChange = { text ->
                        update { current ->
                            current.copy(expectedOutputs = current.expectedOutputs.mapIndexed { i, old -> if (i == index) text else old })
                        }
                    },
                    placeholder = { Text("Output") }, singleLine = true, modifier = Modifier.weight(1f),
                )
                IconButton(onClick = {
                    update { current -> current.copy(expectedOutputs = current.expectedOutputs.filterIndexed { i, _ -> i != index }) }
                }) { Icon(Icons.Outlined.RemoveCircle, "Remove output ${index + 1}", tint = palette.danger) }
            }
        }
        Row(
            Modifier
                .fillMaxWidth()
                .heightIn(min = Lively.tapMin)
                .clickable(role = Role.Button) { update { it.copy(expectedOutputs = it.expectedOutputs + "") } },
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Icon(Icons.Outlined.Add, null, tint = palette.accent)
            Text("Add output", style = planBodyStyle(14.sp, FontWeight.SemiBold, palette.accent))
        }
    }
}

@Composable
private fun MetadataEditor(step: PlanStep, update: ((PlanStep) -> PlanStep) -> Unit) {
    val palette = activePalette
    Section("Metadata") {
        for (entry in step.metadata) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(entry.key, style = planMonoStyle(13.sp, color = palette.secondaryText), modifier = Modifier.width(96.dp), maxLines = 2, overflow = TextOverflow.Ellipsis)
                MagicianTextField(
                    value = entry.value,
                    onValueChange = { text ->
                        update { current ->
                            current.copy(metadata = current.metadata.map { if (it.key == entry.key) it.copy(value = text) else it })
                        }
                    },
                    singleLine = true, modifier = Modifier.weight(1f),
                )
                IconButton(onClick = { update { current -> current.copy(metadata = current.metadata.filter { it.key != entry.key }) } }) {
                    Icon(Icons.Outlined.RemoveCircle, "Remove ${entry.key}", tint = palette.danger)
                }
            }
        }
        KeyValueAdder("key") { key, value ->
            update { current -> current.copy(metadata = current.metadata.filter { it.key != key } + PlanKeyValue(key, value)) }
        }
    }
}

@Composable
private fun KeyValueAdder(placeholder: String, add: (String, String) -> Unit) {
    val palette = activePalette
    var key by remember { mutableStateOf("") }
    var value by remember { mutableStateOf("") }
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        MagicianTextField(
            value = key, onValueChange = { key = it }, placeholder = { Text(placeholder) }, singleLine = true,
            textStyle = planMonoStyle(13.sp),
            keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false),
            modifier = Modifier.weight(1f),
        )
        MagicianTextField(
            value = value, onValueChange = { value = it }, placeholder = { Text("value") }, singleLine = true,
            modifier = Modifier.weight(1f),
        )
        val trimmed = key.trim()
        IconButton(
            onClick = {
                add(trimmed, value)
                key = ""
                value = ""
            },
            enabled = trimmed.isNotEmpty(),
        ) {
            Icon(Icons.Outlined.AddCircle, "Add $placeholder", tint = if (trimmed.isEmpty()) palette.secondaryText else palette.accent)
        }
    }
}
