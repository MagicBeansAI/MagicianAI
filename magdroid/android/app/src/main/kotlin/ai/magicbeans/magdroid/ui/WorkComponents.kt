package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.WorkResource
import ai.magicbeans.magdroid.work.WorkResourceState
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.serialization.json.*

internal val LocalWorkResourceFactory = staticCompositionLocalOf<(android.content.Context) -> WorkResource> { { WorkResource(it) } }

@Composable
internal fun rememberWorkResource(path: String): WorkResource {
    val context = LocalContext.current.applicationContext
    val factory = LocalWorkResourceFactory.current
    val resource = remember(factory) { factory(context) }
    LaunchedEffect(path) { resource.load(path) }
    DisposableEffect(resource) { onDispose { resource.close() } }
    return resource
}

internal fun workLabel(key: String): String = key.replace('_', ' ').replaceFirstChar { it.uppercase() }
internal fun JsonElement?.workDisplay(): String = when (this) {
    null, JsonNull -> "—"
    is JsonPrimitive -> content
    else -> toString()
}
internal fun JsonElement?.workField(key: String): JsonElement? = (this as? JsonObject)?.get(key)
internal fun JsonElement?.workText(key: String): String? = (workField(key) as? JsonPrimitive)?.contentOrNull
internal fun JsonElement?.workBool(key: String): Boolean = (workField(key) as? JsonPrimitive)?.booleanOrNull == true
internal fun JsonElement?.workRows(key: String): List<JsonElement> = (workField(key) as? JsonArray)?.toList().orEmpty()

@Composable
internal fun WorkCard(content: @Composable ColumnScope.() -> Unit) {
    Surface(color = activePalette.elevated, shape = RoundedCornerShape(14.dp), border = BorderStroke(1.dp, BorderSoft)) {
        Column(Modifier.fillMaxWidth().padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp), content = content)
    }
}

@Composable
internal fun WorkResourceStatus(state: WorkResourceState, resource: WorkResource) {
    if (state.loading && state.value == null) { CircularProgressIndicator(color = Coral, modifier = Modifier.size(24.dp)) }
    state.error?.let { error -> WorkCard {
        Text(error, color = Danger, fontSize = 14.sp)
        TextButton(onClick = resource::refresh, enabled = !state.busy) { Text("Retry", color = Coral) }
    } }
    state.notice?.let { Text(it, color = activePalette.success, fontSize = 12.sp) }
}

/** Scalar records first; nested operator evidence remains available on demand. */
@Composable
internal fun WorkRecord(value: JsonElement?) {
    when (value) {
        is JsonObject -> value.toSortedMap().forEach { (key, child) ->
            if (child is JsonObject || child is JsonArray) {
                var expanded by remember(child) { mutableStateOf(false) }
                TextButton(onClick = { expanded = !expanded }, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp)) {
                    Text((if (expanded) "▾ " else "▸ ") + workLabel(key), color = Coral, modifier = Modifier.fillMaxWidth())
                }
                if (expanded) Column(Modifier.padding(start = 10.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) { WorkRecord(child) }
            } else {
                Column(verticalArrangement = Arrangement.spacedBy(3.dp)) {
                    Text(workLabel(key), color = Muted, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
                    SelectionContainer { Text(child.workDisplay(), color = Ink, fontSize = 14.sp) }
                }
            }
        }
        is JsonArray -> {
            if (value.isEmpty()) Text("None", color = Muted)
            value.forEach { WorkRecord(it); HorizontalDivider(color = BorderSoft) }
        }
        else -> SelectionContainer { Text(value.workDisplay(), color = Ink, fontSize = 14.sp) }
    }
}

@Composable
internal fun WorkLink(title: String, onClick: () -> Unit) {
    TextButton(onClick, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp)) {
        Text(title, color = Coral, modifier = Modifier.fillMaxWidth())
    }
}

@Composable
internal fun WorkChoicePicker(label: String, selected: String, choices: List<String>, onSelected: (String) -> Unit) {
    var open by remember { mutableStateOf(false) }
    Box {
        TextButton(onClick = { open = true }, modifier = Modifier.heightIn(min = 48.dp)) { Text("$label: ${selected.ifBlank { "All members" }}", color = Coral) }
        DropdownMenu(open, { open = false }) {
            choices.distinct().forEach { choice -> DropdownMenuItem(text = { Text(choice.ifBlank { "All members" }) }, onClick = { onSelected(choice); open = false }) }
        }
    }
}
