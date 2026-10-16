package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.*
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

@Composable
@OptIn(ExperimentalLayoutApi::class)
internal fun WorkCrewCreate(onSaved: () -> Unit) {
    val resource = rememberWorkResource("/api/magician/v2/agents")
    val state by resource.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    var draft by remember { mutableStateOf(newCrewDefinition()) }
    var editor by remember { mutableStateOf("") }
    var advanced by remember { mutableStateOf(false) }
    LaunchedEffect(state.identity) { draft = newCrewDefinition(); editor = ""; advanced = false }
    LazyColumn(Modifier.fillMaxSize().imePadding(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { WorkResourceStatus(state, resource) }
        item { FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            FilterChip(selected = !advanced, onClick = {
                if (advanced) {
                    val parsed = runCatching { Json.parseToJsonElement(editor) as JsonObject }.getOrNull()
                    if (parsed == null) resource.reportError("Enter a valid definition before returning to guided editing.") else { draft = parsed; advanced = false }
                }
            }, label = { Text("Guided") }, modifier = Modifier.heightIn(min = 48.dp))
            FilterChip(selected = advanced, onClick = { if (!advanced) { editor = Json { prettyPrint = true }.encodeToString(JsonObject.serializer(), draft); advanced = true } }, label = { Text("Advanced JSON") }, modifier = Modifier.heightIn(min = 48.dp))
        } }
        if (advanced) item { OutlinedTextField(editor, { editor = it }, label = { Text("Definition") }, modifier = Modifier.fillMaxWidth(), minLines = 8, maxLines = 20) }
        else item { WorkCrewFields(draft, { draft = it }) }
        item { Button(onClick = {
            val definition = if (advanced) runCatching { Json.parseToJsonElement(editor) as JsonObject }.getOrNull() else draft
            val error = definition?.crewValidation()
            if (definition == null) resource.reportError("Enter a valid definition before creating a member.")
            else if (error != null) resource.reportError(error)
            else { val identity = state.identity; scope.launch { if (resource.perform("/api/magician/v2/agents", identity, body = definition) != null) onSaved() } }
        }, enabled = !state.busy && state.value != null, colors = magicanButtonColors(), shape = MagicanButtonShape, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp)) { Text(if (state.busy) "Creating…" else "Create member") } }
    }
}
